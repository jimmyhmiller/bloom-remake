//! One rule: its plan, and the evaluation of one term of its delta query.
//!
//! A rule's valuations are the joins of its positive atoms, filtered and extended by its other literals — negations,
//! lookups, guards, bindings, generators — which run only for complete joins of the atoms, in the order the reference
//! semantics fixes (each as soon as the ones it depends on have run, in body order), so an earlier guard protects a
//! later expression.
//!
//! The set of valuations is a product over the literals that read relations (the *dependencies*: atoms, negations
//! and lookups), so across a tick it changes by the telescoping sum
//!
//! ```text
//! Δ V = Σ_q  V[ D_<q new, ΔD_q, D_>q old ]
//! ```
//!
//! one term per dependency that changed: the *driver*. A term enumerates the driver's change (an atom's inserted and
//! deleted rows, a negation's keys whose absence flipped, a lookup's cells whose value changed), joins the other atoms,
//! and runs the checks, reading the dependencies before the driver as they are now and those after it as they were at
//! the start of the tick. A valuation made of rows from both versions exists in neither, and cancels between terms;
//! so does any runtime error it raises, which is why errors are counted per valuation and raised only when their net
//! count is positive.
//!
//! A term runs a sequence of steps ([`Order`]), chosen per driver literal each time the rule is evaluated: the atoms
//! in a greedy join order by estimated rows (from the stores' current sizes and index fan-outs), with each check
//! hoisted to the earliest point its inputs are bound — but never past an earlier check, so the checks still run in
//! the reference order and an earlier guard still protects a later expression. A hoisted `let` feeds a later atom's
//! index probe (`let j = i - 1, log(idx: j, ..)`), and a guard `v > e` (or `<`, `>=`, `<=`) on a later atom's
//! column narrows its probe to a range of an ordered index, when no check before the guard can fail (dropping the
//! rows the guard rejects then hides nothing). A hoisted check that raises an error does not end the search: the
//! reference raises it once per complete valuation, so the remaining atoms are joined without the check's outputs
//! and the error counted for each.

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::{RelId, RuleId, VarId, internal_error};
use blossom_ir::core::{Atom, BinOp, Expr, GenSource, HeadArg, Literal, Pattern, Rule, RuleKind, Term, UnOp};
use blossom_ir::tick::{EvalError, Row};
use blossom_value::Value;

use crate::expr::{self, Ctx, ExprError, ExprResult, bug};
use crate::store::Store;

/// Which store a literal reads or a head writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum StoreKey {
    /// A relation's rows at this tick.
    Main(RelId),
    /// A channel's received tuples with their sender as a trailing column (for atoms that bind `from`).
    Sent(RelId),
    /// The rows inductive rules derive for the next tick.
    Next(RelId),
    /// The rows asynchronous rules send this tick.
    Async(RelId),
}

/// How a rule's output is kept up to date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Regime {
    /// Delta queries over the changes of its dependencies.
    Delta,
    /// Re-evaluated in full at every tick and diffed: it reads a time-varying scalar, or has no positive atom.
    Recompute,
}

/// A rule's plan.
#[derive(Clone, Debug)]
pub(crate) struct Plan {
    pub rule: RuleId,
    pub nvars: usize,
    /// The positive atoms, by literal index.
    pub atoms: Vec<usize>,
    /// The other literals in evaluation order.
    pub checks: Vec<usize>,
    /// The dependencies (atoms, then negations and lookups in body order), by literal index.
    pub deps: Vec<usize>,
    pub head: StoreKey,
    pub aggregate: bool,
    pub regime: Regime,
}

/// One end of a range probe: an expression over bound variables, and whether the end is included.
#[derive(Clone, Debug)]
pub(crate) struct RangeEnd {
    pub expr: Expr,
    pub inclusive: bool,
}

/// A range on one column of an atom, from the guards that follow it.
#[derive(Clone, Debug)]
pub(crate) struct RangeProbe {
    pub col: usize,
    pub lo: Option<RangeEnd>,
    pub hi: Option<RangeEnd>,
}

/// One step of a term.
#[derive(Clone, Debug)]
pub(crate) enum Step {
    /// Joins an atom, probing the columns whose values are known (`cols`), and a range of one more column.
    Atom {
        lit: usize,
        cols: Vec<usize>,
        range: Option<RangeProbe>,
    },
    /// Runs check `k` (an index into [`Plan::checks`]).
    Check(usize),
}

/// A term's steps: every atom other than the driver, and every check.
#[derive(Clone, Debug, Default)]
pub(crate) struct Order {
    pub steps: Vec<Step>,
}

/// The store an atom reads.
pub(crate) fn atom_store(a: &Atom) -> StoreKey {
    if a.sender.is_some() {
        StoreKey::Sent(a.rel)
    } else {
        StoreKey::Main(a.rel)
    }
}

/// The store a literal that reads a relation depends on.
pub(crate) fn dep_store(lit: &Literal) -> Option<StoreKey> {
    match lit {
        Literal::Pos(a) => Some(atom_store(a)),
        Literal::Neg(a) => Some(StoreKey::Main(a.rel)),
        Literal::Lookup { rel, .. } => Some(StoreKey::Main(*rel)),
        _ => None,
    }
}

fn atom_terms(a: &Atom) -> impl Iterator<Item = &Term> {
    a.args.iter().chain(a.sender.iter())
}

fn atom_vars(a: &Atom) -> BTreeSet<VarId> {
    atom_terms(a)
        .filter_map(|t| match t {
            Term::Var(v) => Some(*v),
            _ => None,
        })
        .collect()
}

fn expr_vars(e: &Expr, out: &mut BTreeSet<VarId>) {
    match e {
        Expr::Term(Term::Var(v)) => {
            out.insert(*v);
        }
        Expr::Term(_) | Expr::Param(_) | Expr::Scalar(_) => {}
        Expr::Unary { arg, .. } => expr_vars(arg, out),
        Expr::Binary { lhs, rhs, .. } => {
            expr_vars(lhs, out);
            expr_vars(rhs, out);
        }
        Expr::Call { args, .. } | Expr::Collection { elems: args, .. } | Expr::Lattice { args, .. } => {
            args.iter().for_each(|a| expr_vars(a, out));
        }
        Expr::Construct { fields, .. } => fields.iter().for_each(|a| expr_vars(a, out)),
        Expr::Field { base, .. } => expr_vars(base, out),
        Expr::If { cond, then, els } => {
            expr_vars(cond, out);
            expr_vars(then, out);
            expr_vars(els, out);
        }
        Expr::Match { scrut, arms } => {
            expr_vars(scrut, out);
            // Arm patterns bind locally; the arms read the scrutinee's scope.
            for (p, g, b) in arms {
                let mut local = BTreeSet::new();
                if let Some(g) = g {
                    expr_vars(g, &mut local);
                }
                expr_vars(b, &mut local);
                let mut bound = BTreeSet::new();
                pattern_vars(p, &mut bound);
                out.extend(local.difference(&bound));
            }
        }
        Expr::Let { value, body, pat } => {
            expr_vars(value, out);
            let mut local = BTreeSet::new();
            expr_vars(body, &mut local);
            let mut bound = BTreeSet::new();
            pattern_vars(pat, &mut bound);
            out.extend(local.difference(&bound));
        }
        Expr::Closure { body, params } => {
            let mut local = BTreeSet::new();
            expr_vars(body, &mut local);
            out.extend(local.into_iter().filter(|v| !params.contains(v)));
        }
    }
}

fn pattern_vars(p: &Pattern, out: &mut BTreeSet<VarId>) {
    match p {
        Pattern::Var(v) => {
            out.insert(*v);
        }
        Pattern::Tuple(ps) | Pattern::Variant { fields: ps, .. } => ps.iter().for_each(|x| pattern_vars(x, out)),
        Pattern::Struct { fields, .. } => fields.iter().for_each(|(_, x)| pattern_vars(x, out)),
        Pattern::Wild | Pattern::Const(_) => {}
    }
}

fn vars_of(e: &Expr) -> BTreeSet<VarId> {
    let mut s = BTreeSet::new();
    expr_vars(e, &mut s);
    s
}

impl Plan {
    pub fn new(rule: &Rule) -> Result<Plan, EvalError> {
        let lits = &rule.body.lits;
        let atoms: Vec<usize> = lits
            .iter()
            .enumerate()
            .filter(|(_, l)| matches!(l, Literal::Pos(_)))
            .map(|(i, _)| i)
            .collect();
        let mut bound: BTreeSet<VarId> = BTreeSet::new();
        for &i in &atoms {
            if let Some(Literal::Pos(a)) = lits.get(i) {
                bound.extend(atom_vars(a));
            }
        }
        // The other literals: repeated passes in body order, each as soon as the variables it needs are bound.
        let mut done: Vec<bool> = lits.iter().map(|l| matches!(l, Literal::Pos(_))).collect();
        let mut checks = Vec::new();
        loop {
            let mut progressed = false;
            for (i, lit) in lits.iter().enumerate() {
                if done.get(i).copied().unwrap_or(true) {
                    continue;
                }
                let ready = match lit {
                    Literal::Pos(_) => false,
                    Literal::Neg(a) => atom_vars(a).is_subset(&bound),
                    Literal::Guard(e) => vars_of(e).is_subset(&bound),
                    Literal::Bind { pat, expr } => {
                        let ready = vars_of(expr).is_subset(&bound);
                        if ready {
                            pattern_vars(pat, &mut bound);
                        }
                        ready
                    }
                    Literal::Lookup { var, key, .. } => {
                        let ready = key.iter().all(|t| match t {
                            Term::Var(v) => bound.contains(v),
                            Term::Const(_) => true,
                            Term::Wild => false,
                        });
                        if ready {
                            bound.insert(*var);
                        }
                        ready
                    }
                    Literal::Gen { pat, src } => {
                        let needs = match src {
                            GenSource::Range { lo, hi, .. } => {
                                let mut v = vars_of(lo);
                                v.extend(vars_of(hi));
                                v
                            }
                            GenSource::Value(e) | GenSource::Lattice(e) => vars_of(e),
                            GenSource::TableFn { .. } => {
                                return Err(blossom_base::unimplemented_error!(
                                    "LANG-183",
                                    "table-function generators in the engine"
                                )
                                .into());
                            }
                        };
                        let ready = needs.is_subset(&bound);
                        if ready {
                            pattern_vars(pat, &mut bound);
                        }
                        ready
                    }
                };
                if ready {
                    checks.push(i);
                    if let Some(d) = done.get_mut(i) {
                        *d = true;
                    }
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
        if let Some(i) = done.iter().position(|d| !d) {
            return Err(internal_error!("rule {}: literal {i} is never evaluable", rule.label.text).into());
        }
        let mut deps = atoms.clone();
        deps.extend(
            lits.iter()
                .enumerate()
                .filter(|(_, l)| matches!(l, Literal::Neg(_) | Literal::Lookup { .. }))
                .map(|(i, _)| i),
        );
        let time_varying = lits.iter().any(|l| match l {
            Literal::Guard(e) | Literal::Bind { expr: e, .. } => expr::time_varying(e),
            Literal::Gen { src, .. } => match src {
                GenSource::Range { lo, hi, .. } => expr::time_varying(lo) || expr::time_varying(hi),
                GenSource::Value(e) | GenSource::Lattice(e) => expr::time_varying(e),
                GenSource::TableFn { .. } => true,
            },
            _ => false,
        });
        let head = match rule.kind {
            RuleKind::Deductive => StoreKey::Main(rule.head.rel),
            RuleKind::Inductive => StoreKey::Next(rule.head.rel),
            RuleKind::Async => StoreKey::Async(rule.head.rel),
        };
        let no_atoms = atoms.is_empty();
        Ok(Plan {
            rule: rule.id,
            nvars: rule.body.vars.len(),
            atoms,
            checks,
            // A rule with no positive atom has valuations even over empty relations (`not r(_)` holds there), which
            // no change announces; it is cheap to re-evaluate (it joins nothing).
            regime: if time_varying || no_atoms {
                Regime::Recompute
            } else {
                Regime::Delta
            },
            deps,
            head,
            aggregate: crate::strata::is_aggregate(rule),
        })
    }

    /// The steps of a term driven by literal `driver` (`None`: a full evaluation). `cost(lit, cols, range)` estimates
    /// the rows atom `lit` yields per probe on `cols` (narrowed by a range, if `range`).
    pub fn order_for(&self, rule: &Rule, driver: Option<usize>, cost: &dyn Fn(usize, &[usize], bool) -> usize) -> Order {
        let (skip, bound) = match driver {
            None => (None, BTreeSet::new()),
            Some(lit) => (
                matches!(rule.body.lits.get(lit), Some(Literal::Pos(_))).then_some(lit),
                driver_vars(rule, lit),
            ),
        };
        self.order(rule, skip, &bound, cost)
    }

    /// The steps of a term whose driver binds `bound` (and is atom `skip`, if an atom): repeatedly the atom with the
    /// fewest estimated rows (then the lowest literal index), each followed by the checks that become ready, in order.
    fn order(
        &self,
        rule: &Rule,
        skip: Option<usize>,
        bound: &BTreeSet<VarId>,
        cost: &dyn Fn(usize, &[usize], bool) -> usize,
    ) -> Order {
        let mut bound = bound.clone();
        let mut left: Vec<usize> = self.atoms.iter().copied().filter(|a| Some(*a) != skip).collect();
        let mut steps = Vec::new();
        let mut next = 0usize;
        self.hoist(rule, &mut next, &mut bound, &mut steps);
        while !left.is_empty() {
            struct Candidate {
                pos: usize,
                lit: usize,
                cols: Vec<usize>,
                range: Option<RangeProbe>,
                rows: usize,
            }
            let mut best: Option<Candidate> = None;
            for (pos, &lit) in left.iter().enumerate() {
                let Some(Literal::Pos(a)) = rule.body.lits.get(lit) else { continue };
                let cols = bound_columns(a, &bound);
                let range = self.range_for(rule, a, &cols, &bound, next);
                let rows = cost(lit, &cols, range.is_some());
                if best.as_ref().is_none_or(|b| rows < b.rows) {
                    best = Some(Candidate {
                        pos,
                        lit,
                        cols,
                        range,
                        rows,
                    });
                }
            }
            let Some(Candidate {
                pos,
                lit: i,
                cols,
                range,
                ..
            }) = best
            else {
                break;
            };
            left.remove(pos);
            if let Some(Literal::Pos(a)) = rule.body.lits.get(i) {
                bound.extend(atom_vars(a));
            }
            steps.push(Step::Atom { lit: i, cols, range });
            self.hoist(rule, &mut next, &mut bound, &mut steps);
        }
        // Whatever is left runs after every atom (a check whose inputs no atom binds is not evaluable; Plan::new
        // rejected that).
        while next < self.checks.len() {
            steps.push(Step::Check(next));
            next += 1;
        }
        Order { steps }
    }

    /// Appends the checks from `next` on that are ready, in order, binding what they bind.
    fn hoist(&self, rule: &Rule, next: &mut usize, bound: &mut BTreeSet<VarId>, steps: &mut Vec<Step>) {
        while let Some(&lit) = self.checks.get(*next) {
            let Some(l) = rule.body.lits.get(lit) else { return };
            if !ready(l, bound) {
                return;
            }
            bind_outputs(l, bound);
            steps.push(Step::Check(*next));
            *next += 1;
        }
    }

    /// A range on one of `a`'s unbound columns from the guards among the checks not yet run (from `next`), up to the
    /// first check that can fail: those rows the range drops would fail the guard before any error could be raised.
    fn range_for(&self, rule: &Rule, a: &Atom, cols: &[usize], bound: &BTreeSet<VarId>, next: usize) -> Option<RangeProbe> {
        for &lit in self.checks.get(next..).unwrap_or_default() {
            let l = rule.body.lits.get(lit)?;
            if let Literal::Guard(e) = l {
                let mut probe: Option<RangeProbe> = None;
                for c in conjuncts(e) {
                    if let Some((col, end, lower)) = range_conjunct(c, a, cols, bound) {
                        let p = probe.get_or_insert(RangeProbe { col, lo: None, hi: None });
                        if p.col == col {
                            if lower {
                                p.lo.get_or_insert(end);
                            } else {
                                p.hi.get_or_insert(end);
                            }
                        }
                    }
                    // A later conjunct runs only if this one held and raised nothing.
                    if !infallible(c) {
                        break;
                    }
                }
                if probe.is_some() {
                    return probe;
                }
            }
            if !check_infallible(l) {
                return None;
            }
        }
        None
    }
}

/// The variables a driver literal binds: an atom's, a negation's, a lookup's key and value.
fn driver_vars(rule: &Rule, lit: usize) -> BTreeSet<VarId> {
    let var = |t: &Term| match t {
        Term::Var(v) => Some(*v),
        _ => None,
    };
    match rule.body.lits.get(lit) {
        Some(Literal::Pos(a)) => atom_vars(a),
        Some(Literal::Neg(a)) => a.args.iter().filter_map(var).collect(),
        Some(Literal::Lookup { var: v, key, .. }) => key.iter().filter_map(var).chain(std::iter::once(*v)).collect(),
        _ => BTreeSet::new(),
    }
}

/// Whether a check's inputs are bound.
fn ready(l: &Literal, bound: &BTreeSet<VarId>) -> bool {
    match l {
        Literal::Pos(_) => false,
        Literal::Neg(a) => atom_vars(a).is_subset(bound),
        Literal::Guard(e) => vars_of(e).is_subset(bound),
        Literal::Bind { expr, .. } => vars_of(expr).is_subset(bound),
        Literal::Lookup { key, .. } => key.iter().all(|t| match t {
            Term::Var(v) => bound.contains(v),
            Term::Const(_) => true,
            Term::Wild => false,
        }),
        Literal::Gen { src, .. } => match src {
            GenSource::Range { lo, hi, .. } => vars_of(lo).is_subset(bound) && vars_of(hi).is_subset(bound),
            GenSource::Value(e) | GenSource::Lattice(e) => vars_of(e).is_subset(bound),
            GenSource::TableFn { .. } => false,
        },
    }
}

/// Adds the variables a check binds.
fn bind_outputs(l: &Literal, bound: &mut BTreeSet<VarId>) {
    match l {
        Literal::Bind { pat, .. } | Literal::Gen { pat, .. } => pattern_vars(pat, bound),
        Literal::Lookup { var, .. } => {
            bound.insert(*var);
        }
        _ => {}
    }
}

/// The conjuncts of `a && b && …`, left to right.
fn conjuncts(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::Binary {
            op: BinOp::And,
            lhs,
            rhs,
        } => {
            let mut v = conjuncts(lhs);
            v.extend(conjuncts(rhs));
            v
        }
        other => vec![other],
    }
}

/// `v op e` (or `e op v`) where `v` is the variable of one of `a`'s unbound columns and `e` is over bound variables:
/// the column, the other end, and whether it is a lower bound.
fn range_conjunct(c: &Expr, a: &Atom, cols: &[usize], bound: &BTreeSet<VarId>) -> Option<(usize, RangeEnd, bool)> {
    let Expr::Binary { op, lhs, rhs } = c else { return None };
    let column = |e: &Expr| match e {
        Expr::Term(Term::Var(v)) if !bound.contains(v) => atom_terms(a)
            .position(|t| matches!(t, Term::Var(x) if x == v))
            .filter(|c| !cols.contains(c)),
        _ => None,
    };
    let known = |e: &Expr| vars_of(e).is_subset(bound);
    // `v < e`: an upper bound; `e < v`: a lower one.
    let (col, other, var_left) = match (column(lhs), column(rhs)) {
        (Some(col), None) if known(rhs) => (col, rhs, true),
        (None, Some(col)) if known(lhs) => (col, lhs, false),
        _ => return None,
    };
    let (lower, inclusive) = match (op, var_left) {
        (BinOp::Gt, true) | (BinOp::Lt, false) => (true, false),
        (BinOp::Ge, true) | (BinOp::Le, false) => (true, true),
        (BinOp::Lt, true) | (BinOp::Gt, false) => (false, false),
        (BinOp::Le, true) | (BinOp::Ge, false) => (false, true),
        _ => return None,
    };
    Some((
        col,
        RangeEnd {
            expr: (**other).clone(),
            inclusive,
        },
        lower,
    ))
}

/// Whether evaluating `e` can raise a runtime error: only variables, constants, scalars, comparisons and boolean
/// connectives cannot.
fn infallible(e: &Expr) -> bool {
    match e {
        Expr::Term(_) | Expr::Param(_) | Expr::Scalar(_) => true,
        Expr::Unary { op: UnOp::Not, arg } => infallible(arg),
        Expr::Binary { op, lhs, rhs } => {
            matches!(
                op,
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::And | BinOp::Or
            ) && infallible(lhs)
                && infallible(rhs)
        }
        _ => false,
    }
}

/// Whether a check can raise a runtime error.
fn check_infallible(l: &Literal) -> bool {
    match l {
        Literal::Neg(_) | Literal::Lookup { .. } => true,
        Literal::Guard(e) => infallible(e),
        Literal::Bind { expr, .. } => infallible(expr),
        _ => false,
    }
}

/// The columns of `a` whose values are known (a sender is the column after the last argument).
pub(crate) fn bound_columns(a: &Atom, bound: &BTreeSet<VarId>) -> Vec<usize> {
    atom_terms(a)
        .enumerate()
        .filter(|(_, t)| match t {
            Term::Const(_) => true,
            Term::Var(v) => bound.contains(v),
            Term::Wild => false,
        })
        .map(|(i, _)| i)
        .collect()
}

/// The non-wild columns of a negated atom.
pub(crate) fn non_wild(a: &Atom) -> Vec<usize> {
    a.args
        .iter()
        .enumerate()
        .filter(|(_, t)| !matches!(t, Term::Wild))
        .map(|(i, _)| i)
        .collect()
}

/// What drives one term of a delta query.
#[derive(Clone, Debug)]
pub(crate) enum Driver {
    /// No driver: every dependency at its current version (a full evaluation).
    Full,
    /// A row of an atom's change.
    Atom { lit: usize, row: Row, sign: i64 },
    /// A key whose absence from a negated atom's relation flipped: `sign` 1 if the negation now holds.
    Neg { lit: usize, key: Vec<Value>, sign: i64 },
    /// A cell a lookup reads, with one of its values (the new one with sign 1, the old one with sign -1).
    Lookup {
        lit: usize,
        key: Vec<Value>,
        value: Value,
        sign: i64,
    },
}

impl Driver {
    pub fn lit(&self) -> Option<usize> {
        match self {
            Driver::Full => None,
            Driver::Atom { lit, .. } | Driver::Neg { lit, .. } | Driver::Lookup { lit, .. } => Some(*lit),
        }
    }

    fn sign(&self) -> i64 {
        match self {
            Driver::Full => 1,
            Driver::Atom { sign, .. } | Driver::Neg { sign, .. } | Driver::Lookup { sign, .. } => *sign,
        }
    }
}

/// One valuation found by a term.
pub(crate) struct Found<'e> {
    pub env: &'e [Option<Value>],
    pub sign: i64,
}

/// A valuation's identity, for counting its runtime errors: its atom rows and lookup values.
pub(crate) type Token = Vec<Value>;

/// Evaluates one term of `rule`'s delta query. `old(lit)` says whether dependency `lit` is read at its old version.
/// Each valuation goes to `emit`; each runtime error to `error`, with the valuation's token.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_term(
    cx: &Ctx<'_>,
    stores: &BTreeMap<StoreKey, Store>,
    rule: &Rule,
    plan: &Plan,
    order: &Order,
    driver: &Driver,
    old: &dyn Fn(usize) -> bool,
    emit: &mut dyn FnMut(Found<'_>) -> ExprResult<()>,
    error: &mut dyn FnMut(Token, ExprError, i64),
) -> Result<u64, EvalError> {
    let mut env: Vec<Option<Value>> = vec![None; plan.nvars];
    let sign = driver.sign();
    // Bind what the driver fixes.
    match driver {
        Driver::Full => {}
        Driver::Atom { lit, row, .. } => {
            let Some(Literal::Pos(a)) = rule.body.lits.get(*lit) else {
                return Err(internal_error!("an atom driver names a non-atom").into());
            };
            if !unify(cx, &mut env, atom_terms(a), row).map_err(fatal)? {
                return Ok(0);
            }
        }
        Driver::Neg { lit, key, .. } => {
            let Some(Literal::Neg(a)) = rule.body.lits.get(*lit) else {
                return Err(internal_error!("a negation driver names a non-negation").into());
            };
            let terms: Vec<&Term> = non_wild(a).iter().filter_map(|c| a.args.get(*c)).collect();
            if !unify(cx, &mut env, terms.into_iter(), key).map_err(fatal)? {
                return Ok(0);
            }
        }
        Driver::Lookup { lit, key, value, .. } => {
            let Some(Literal::Lookup { var, key: terms, .. }) = rule.body.lits.get(*lit) else {
                return Err(internal_error!("a lookup driver names a non-lookup").into());
            };
            if !unify(cx, &mut env, terms.iter(), key).map_err(fatal)? {
                return Ok(0);
            }
            match env.get_mut(var.index()) {
                Some(slot @ None) => *slot = Some(value.clone()),
                Some(Some(v)) if v == value => {}
                Some(Some(_)) => return Ok(0),
                None => return Err(internal_error!("variable {var:?} out of range").into()),
            }
        }
    }
    let mut rows: Vec<Option<Row>> = vec![None; rule.body.lits.len()];
    if let Driver::Atom { lit, row, .. } = driver
        && let Some(slot) = rows.get_mut(*lit)
    {
        *slot = Some(row.clone());
    }
    let mut search = Search {
        cx,
        stores,
        rule,
        plan,
        driver,
        old,
        steps: &order.steps,
        sign,
        emit,
        error,
        examined: 0,
    };
    search.run(0, &mut env, &mut rows, &mut Vec::new(), None)?;
    Ok(search.examined)
}

fn fatal(e: ExprError) -> EvalError {
    match e {
        ExprError::Eval(e) => e,
        other => internal_error!("an error binding a driver: {other:?}").into(),
    }
}

/// Binds the unbound variables among `terms` to `values`; false on a mismatch.
fn unify<'t>(
    cx: &Ctx<'_>,
    env: &mut [Option<Value>],
    terms: impl Iterator<Item = &'t Term>,
    values: &[Value],
) -> ExprResult<bool> {
    let mut newly = Vec::new();
    let ok = unify_tracked(cx, env, terms, values, &mut newly)?;
    if !ok {
        undo(env, &newly);
    }
    Ok(ok)
}

/// [`unify`], recording the variables it bound in `newly` (the caller undoes them, on a mismatch too).
fn unify_tracked<'t>(
    cx: &Ctx<'_>,
    env: &mut [Option<Value>],
    terms: impl Iterator<Item = &'t Term>,
    values: &[Value],
    newly: &mut Vec<usize>,
) -> ExprResult<bool> {
    let mut n = 0;
    for (t, v) in terms.zip(values) {
        n += 1;
        match t {
            Term::Wild => {}
            Term::Const(_) => {
                if expr::term(cx, env, t)? != *v {
                    return Ok(false);
                }
            }
            Term::Var(var) => match env.get_mut(var.index()) {
                Some(slot @ None) => {
                    *slot = Some(v.clone());
                    newly.push(var.index());
                }
                Some(Some(existing)) => {
                    if existing != v {
                        return Ok(false);
                    }
                }
                None => return Err(bug(format!("variable {var:?} out of range"))),
            },
        }
    }
    if n != values.len() {
        return Err(bug(format!("{n} terms against {} values", values.len())));
    }
    Ok(true)
}

/// Unbinds the variables a step bound.
fn undo(env: &mut [Option<Value>], newly: &[usize]) {
    for &i in newly {
        if let Some(slot) = env.get_mut(i) {
            *slot = None;
        }
    }
}

/// A range end's value, if it evaluates to an ordered scalar (integers, durations, instants: their value order is
/// their numeric order). Anything else, an error included, means no range: the guard itself then decides.
fn range_end(cx: &Ctx<'_>, env: &[Option<Value>], end: &Option<RangeEnd>) -> Option<std::ops::Bound<Value>> {
    use std::ops::Bound;
    let Some(end) = end else { return Some(Bound::Unbounded) };
    match expr::eval(cx, env, &end.expr) {
        Ok(v @ (Value::Int(_) | Value::Duration(_) | Value::Instant(_))) => Some(if end.inclusive {
            Bound::Included(v)
        } else {
            Bound::Excluded(v)
        }),
        _ => None,
    }
}

struct Search<'a, 'b> {
    cx: &'a Ctx<'a>,
    stores: &'a BTreeMap<StoreKey, Store>,
    rule: &'a Rule,
    plan: &'a Plan,
    driver: &'a Driver,
    old: &'a dyn Fn(usize) -> bool,
    steps: &'a [Step],
    sign: i64,
    emit: &'b mut dyn FnMut(Found<'_>) -> ExprResult<()>,
    error: &'b mut dyn FnMut(Token, ExprError, i64),
    examined: u64,
}

impl Search<'_, '_> {
    fn store(&self, key: StoreKey) -> Result<&Store, EvalError> {
        self.stores
            .get(&key)
            .ok_or_else(|| internal_error!("no store for {key:?}").into())
    }

    /// The valuation's token: its atom rows, then the values its lookups read.
    fn token(&self, rows: &[Option<Row>], looked: &[Value]) -> Token {
        let mut t = Vec::new();
        for &i in &self.plan.atoms {
            if let Some(Some(r)) = rows.get(i) {
                t.push(Value::Tuple(r.clone()));
            }
        }
        t.extend(looked.iter().cloned());
        t
    }

    /// Runs the steps from `pc`. `failed` is the error a hoisted check raised: the remaining atoms are joined without
    /// that check's outputs (and the checks skipped), and the error is counted for each complete valuation.
    fn run(
        &mut self,
        pc: usize,
        env: &mut Vec<Option<Value>>,
        rows: &mut Vec<Option<Row>>,
        looked: &mut Vec<Value>,
        failed: Option<&ExprError>,
    ) -> Result<(), EvalError> {
        let Some(step) = self.steps.get(pc) else {
            let sign = self.sign;
            if let Some(e) = failed {
                let token = self.token(rows, looked);
                (self.error)(token, e.duplicate(), sign);
                return Ok(());
            }
            return match (self.emit)(Found { env, sign }) {
                Ok(()) => Ok(()),
                Err(ExprError::Eval(e)) => Err(e),
                Err(e) => {
                    let token = self.token(rows, looked);
                    (self.error)(token, e, sign);
                    Ok(())
                }
            };
        };
        match step {
            Step::Atom { lit, cols, range } => self.atom(pc, *lit, cols, range.as_ref(), env, rows, looked, failed),
            Step::Check(_) if failed.is_some() => self.run(pc + 1, env, rows, looked, failed),
            Step::Check(k) => {
                let lit = *self
                    .plan
                    .checks
                    .get(*k)
                    .ok_or_else(|| internal_error!("check {k} out of range"))?;
                self.check(pc, lit, env, rows, looked)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn atom(
        &mut self,
        pc: usize,
        lit: usize,
        cols: &[usize],
        range: Option<&RangeProbe>,
        env: &mut Vec<Option<Value>>,
        rows: &mut Vec<Option<Row>>,
        looked: &mut Vec<Value>,
        failed: Option<&ExprError>,
    ) -> Result<(), EvalError> {
        let Some(Literal::Pos(a)) = self.rule.body.lits.get(lit) else {
            return Err(internal_error!("a join step names a non-atom").into());
        };
        let terms: Vec<&Term> = atom_terms(a).collect();
        // The probe's values; after a failed check a planned column may be unbound, and the atom is scanned.
        let mut values = Vec::with_capacity(cols.len());
        let mut probe = true;
        for c in cols {
            let t = terms
                .get(*c)
                .ok_or_else(|| internal_error!("a bound column is out of range"))?;
            match expr::term(self.cx, env, t) {
                Ok(v) => values.push(v),
                Err(_) if failed.is_some() => {
                    probe = false;
                    break;
                }
                Err(e) => return Err(fatal(e)),
            }
        }
        let store = self.store(atom_store(a))?;
        let old = (self.old)(lit);
        let candidates = if !probe {
            store.rows(old, &[], &[])?
        } else {
            let bounds = match range {
                Some(r) if failed.is_none() => match (range_end(self.cx, env, &r.lo), range_end(self.cx, env, &r.hi)) {
                    (Some(lo), Some(hi)) => Some((r.col, lo, hi)),
                    _ => None,
                },
                _ => None,
            };
            match bounds {
                Some((col, lo, hi)) => store.range_rows(old, cols, &values, col, lo, hi)?,
                None => store.rows(old, cols, &values)?,
            }
        };
        self.examined += candidates.len() as u64;
        let mut newly = Vec::new();
        for row in candidates {
            newly.clear();
            let ok = unify_tracked(self.cx, env, terms.iter().copied(), &row, &mut newly).map_err(fatal)?;
            if ok {
                if let Some(slot) = rows.get_mut(lit) {
                    *slot = Some(row);
                }
                self.run(pc + 1, env, rows, looked, failed)?;
                if let Some(slot) = rows.get_mut(lit) {
                    *slot = None;
                }
            }
            undo(env, &newly);
        }
        Ok(())
    }

    /// Runs check `lit`, then the steps after `pc` for each way it holds.
    fn check(
        &mut self,
        pc: usize,
        lit: usize,
        env: &mut Vec<Option<Value>>,
        rows: &mut Vec<Option<Row>>,
        looked: &mut Vec<Value>,
    ) -> Result<(), EvalError> {
        let is_driver = self.driver.lit() == Some(lit);
        let result: ExprResult<bool> = match self.rule.body.lits.get(lit) {
            Some(Literal::Neg(a)) => {
                if is_driver {
                    // The driver's flip is the term's sign.
                    Ok(true)
                } else {
                    let cols = non_wild(a);
                    let mut values = Vec::with_capacity(cols.len());
                    for c in &cols {
                        let t = a.args.get(*c).ok_or_else(|| internal_error!("a negated column is out of range"))?;
                        values.push(expr::term(self.cx, env, t).map_err(fatal)?);
                    }
                    let present = self.store(StoreKey::Main(a.rel))?.any((self.old)(lit), &cols, &values)?;
                    Ok(!present)
                }
            }
            Some(Literal::Guard(e)) => expr::eval(self.cx, env, e).and_then(|v| expr::truth(&v)),
            Some(Literal::Bind { pat, expr: e }) => match expr::eval(self.cx, env, e) {
                Ok(v) => {
                    let mut newly = Vec::new();
                    let r = expr::matches(self.cx, env, pat, &v, &mut newly);
                    let out = match r {
                        Ok(true) => self.run(pc + 1, env, rows, looked, None),
                        Ok(false) => Ok(()),
                        Err(ExprError::Eval(e)) => Err(e),
                        Err(e) => self.fail(pc, e, env, rows, looked),
                    };
                    undo(env, &newly);
                    return out;
                }
                Err(e) => Err(e),
            },
            Some(Literal::Lookup { var, rel, key }) => {
                let value = if is_driver {
                    env.get(var.index())
                        .cloned()
                        .flatten()
                        .ok_or_else(|| internal_error!("a lookup driver left its variable unbound"))?
                } else {
                    let mut values = Vec::with_capacity(key.len());
                    for t in key {
                        values.push(expr::term(self.cx, env, t).map_err(fatal)?);
                    }
                    let store = self.store(StoreKey::Main(*rel))?;
                    let (cols, col, bottom) = lookup_shape(self.cx, *rel)?;
                    let found = store.rows((self.old)(lit), &cols, &values)?;
                    match found.first() {
                        Some(r) => r
                            .get(col)
                            .cloned()
                            .ok_or_else(|| internal_error!("a cell row without its value"))?,
                        None => bottom,
                    }
                };
                let slot_was_empty = env.get(var.index()).is_some_and(Option::is_none);
                if let Some(slot) = env.get_mut(var.index()) {
                    *slot = Some(value.clone());
                }
                looked.push(value);
                let out = self.run(pc + 1, env, rows, looked, None);
                looked.pop();
                if slot_was_empty {
                    undo(env, &[var.index()]);
                }
                return out;
            }
            Some(Literal::Gen { pat, src }) => match expr::generate(self.cx, env, src) {
                Ok(values) => {
                    for v in values {
                        let mut newly = Vec::new();
                        let r = match expr::matches(self.cx, env, pat, &v, &mut newly) {
                            Ok(true) => self.run(pc + 1, env, rows, looked, None),
                            Ok(false) => Ok(()),
                            Err(ExprError::Eval(e)) => Err(e),
                            Err(e) => self.fail(pc, e, env, rows, looked),
                        };
                        undo(env, &newly);
                        r?;
                    }
                    return Ok(());
                }
                Err(e) => Err(e),
            },
            other => return Err(internal_error!("a check step names {other:?}").into()),
        };
        match result {
            Ok(true) => self.run(pc + 1, env, rows, looked, None),
            Ok(false) => Ok(()),
            Err(ExprError::Eval(e)) => Err(e),
            Err(e) => self.fail(pc, e, env, rows, looked),
        }
    }

    /// A check at `pc` raised `e`: the valuation fails, once per completion of the atoms still to join.
    fn fail(
        &mut self,
        pc: usize,
        e: ExprError,
        env: &mut Vec<Option<Value>>,
        rows: &mut Vec<Option<Row>>,
        looked: &mut Vec<Value>,
    ) -> Result<(), EvalError> {
        self.run(pc + 1, env, rows, looked, Some(&e))
    }
}

/// A lookup's key columns, value column and ⊥.
pub(crate) fn lookup_shape(cx: &Ctx<'_>, rel: RelId) -> Result<(Vec<usize>, usize, Value), EvalError> {
    let decl = cx
        .program
        .rels
        .get(rel)
        .ok_or_else(|| internal_error!("a lookup on an undeclared relation"))?;
    let cols: Vec<usize> = decl.schema.key.iter().map(|c| c.index()).collect();
    let [(col, lat)] = decl.schema.lattice.as_slice() else {
        return Err(internal_error!("a lookup needs exactly one lattice column").into());
    };
    let kind = cx
        .shared
        .kinds
        .get(lat.index())
        .and_then(Option::as_ref)
        .ok_or_else(|| blossom_base::unimplemented_error!("LANG-124", "lattice {lat:?} in the engine"))?;
    Ok((cols, col.index(), Value::Lattice(kind.bottom())))
}

/// The head row of a non-aggregate rule for a valuation.
pub(crate) fn head_row(cx: &Ctx<'_>, rule: &Rule, env: &[Option<Value>]) -> ExprResult<Row> {
    let mut row = Vec::with_capacity(rule.head.args.len());
    for a in &rule.head.args {
        match a {
            HeadArg::Term(t) => row.push(expr::term(cx, env, t)?),
            HeadArg::Agg(_) => return Err(bug("an aggregate head column in a plain rule".into())),
        }
    }
    Ok(Row::from(row))
}
