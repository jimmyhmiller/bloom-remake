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

use std::collections::BTreeSet;

use blossom_base::{RelId, RuleId, VarId, internal_error};
use blossom_ir::core::{Atom, BinOp, Expr, GenSource, HeadArg, Literal, Pattern, Rule, RuleKind, Term};
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

impl StoreKey {
    /// The store's place in [`Stores`]: four kinds per relation.
    fn slot(self) -> usize {
        let (rel, kind) = match self {
            StoreKey::Main(r) => (r, 0),
            StoreKey::Sent(r) => (r, 1),
            StoreKey::Next(r) => (r, 2),
            StoreKey::Async(r) => (r, 3),
        };
        rel.index() * 4 + kind
    }
}

/// Every store of an engine, found by its key in constant time (the keys are dense: four kinds per relation).
/// It knows which stores were written since the tick began (every write goes through `get_mut` or `values_mut`), so
/// a tick clears only those stores' changes and tells that a store did not change without reading it.
#[derive(Default)]
pub(crate) struct Stores {
    slots: Vec<Option<(StoreKey, Store)>>,
    /// The slots written since `clear_deltas`, and a flag per slot.
    touched: Vec<usize>,
    flags: Vec<bool>,
}

impl Stores {
    pub fn insert(&mut self, key: StoreKey, store: Store) {
        let i = key.slot();
        if self.slots.len() <= i {
            self.slots.resize_with(i + 1, || None);
        }
        if let Some(slot) = self.slots.get_mut(i) {
            *slot = Some((key, store));
        }
    }

    /// Inserts `make()` at `key` unless a store is there.
    pub fn insert_absent(&mut self, key: StoreKey, make: impl FnOnce() -> Store) {
        if self.get(&key).is_none() {
            self.insert(key, make());
        }
    }

    pub fn get(&self, key: &StoreKey) -> Option<&Store> {
        self.slots.get(key.slot()).and_then(Option::as_ref).map(|(_, s)| s)
    }

    pub fn get_mut(&mut self, key: &StoreKey) -> Option<&mut Store> {
        let i = key.slot();
        self.touch(i);
        self.slots.get_mut(i).and_then(Option::as_mut).map(|(_, s)| s)
    }

    fn touch(&mut self, i: usize) {
        if self.flags.len() <= i {
            self.flags.resize(i + 1, false);
        }
        if let Some(f) = self.flags.get_mut(i)
            && !*f
        {
            *f = true;
            self.touched.push(i);
        }
    }

    /// Whether the store at `key` changed this tick (`Store::changed`); a store not written since the tick began
    /// did not, and is not read.
    pub fn changed(&self, key: &StoreKey) -> bool {
        let i = key.slot();
        self.flags.get(i).copied().unwrap_or(false) && self.get(key).is_some_and(Store::changed)
    }

    /// Clears the tick's changes of every store written since the last clear.
    pub fn clear_deltas(&mut self) {
        for i in std::mem::take(&mut self.touched) {
            if let Some(f) = self.flags.get_mut(i) {
                *f = false;
            }
            if let Some(Some((_, s))) = self.slots.get_mut(i) {
                s.clear_delta();
            }
        }
    }

    /// Every store with its key.
    pub fn iter(&self) -> impl Iterator<Item = (StoreKey, &Store)> {
        self.slots.iter().flatten().map(|(k, s)| (*k, s))
    }

    pub fn values(&self) -> impl Iterator<Item = &Store> {
        self.slots.iter().flatten().map(|(_, s)| s)
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Store> {
        for i in 0..self.slots.len() {
            if self.slots.get(i).is_some_and(Option::is_some) {
                self.touch(i);
            }
        }
        self.slots.iter_mut().flatten().map(|(_, s)| s)
    }
}

/// Every rule's plan, found by its id in constant time, shared so that reading one copies nothing.
#[derive(Default)]
pub(crate) struct Plans {
    slots: Vec<Option<std::sync::Arc<Plan>>>,
}

impl Plans {
    pub fn insert(&mut self, id: RuleId, plan: Plan) {
        let i = id.index();
        if self.slots.len() <= i {
            self.slots.resize_with(i + 1, || None);
        }
        if let Some(slot) = self.slots.get_mut(i) {
            *slot = Some(std::sync::Arc::new(plan));
        }
    }

    pub fn get(&self, id: &RuleId) -> Option<&std::sync::Arc<Plan>> {
        self.slots.get(id.index()).and_then(Option::as_ref)
    }

    pub fn contains_key(&self, id: &RuleId) -> bool {
        self.get(id).is_some()
    }

    pub fn values(&self) -> impl Iterator<Item = &std::sync::Arc<Plan>> {
        self.slots.iter().flatten()
    }
}

/// When a re-evaluated rule (`Regime::Recompute`) may be left alone at a tick: its output is the same as at its last
/// evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Skip {
    /// Never: it reads the tick, the incarnation, randomness, or the time otherwise than below.
    Never,
    /// While nothing it reads changes (it reads no time: re-evaluated only for having no positive atom).
    Unchanged,
    /// While nothing it reads changes and the time stays before the instant at which a comparison of `now()`
    /// against an instant, in its last evaluation, would come out the other way (`Ctx::flips_at`): its only use
    /// of the time.
    UntilFlip,
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
    /// The stores the dependencies read.
    pub dep_keys: Vec<StoreKey>,
    pub head: StoreKey,
    pub aggregate: bool,
    pub regime: Regime,
    /// When the rule only copies one atom's rows into its head: how, column by column.
    pub copy: Option<CopyPlan>,
    /// A re-evaluated rule: when it may be left alone.
    pub skip: Skip,
}

/// Whether `e` reads the time only by ordering `now()` against an expression that does not read it (and reads
/// neither the tick, the incarnation nor randomness).
fn time_only_compared(e: &Expr) -> bool {
    match e {
        Expr::Binary {
            op: BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge,
            lhs,
            rhs,
        } if expr::is_now(lhs) != expr::is_now(rhs) => {
            let other = if expr::is_now(lhs) { rhs } else { lhs };
            !expr::time_varying(other)
        }
        Expr::Scalar(_) | Expr::Term(_) | Expr::Param(_) => !expr::time_varying(e),
        Expr::Call { f, args } => !expr::draws_randomness(f) && args.iter().all(time_only_compared),
        Expr::Unary { arg, .. } => time_only_compared(arg),
        Expr::Binary { lhs, rhs, .. } => time_only_compared(lhs) && time_only_compared(rhs),
        Expr::Construct { fields, .. } => fields.iter().all(time_only_compared),
        Expr::Field { base, .. } => time_only_compared(base),
        Expr::If { cond, then, els } => time_only_compared(cond) && time_only_compared(then) && time_only_compared(els),
        Expr::Match { scrut, arms } => {
            time_only_compared(scrut)
                && arms
                    .iter()
                    .all(|(_, g, b)| g.as_ref().is_none_or(time_only_compared) && time_only_compared(b))
        }
        Expr::Collection { elems, .. } => elems.iter().all(time_only_compared),
        Expr::Lattice { args, .. } => args.iter().all(time_only_compared),
        Expr::Let { value, body, .. } => time_only_compared(value) && time_only_compared(body),
        Expr::Closure { body, .. } => time_only_compared(body),
        Expr::Typed { expr, .. } => time_only_compared(expr),
    }
}


/// A rule whose body is one positive atom and whose head is plain terms (lowering makes many: unions, renames,
/// the copies into a table's next state): a changed row of the atom maps to its head row column by column, with no
/// join, valuation or allocation but the head row.
#[derive(Clone, Debug)]
pub(crate) struct CopyPlan {
    /// What each atom column must hold.
    cols: Vec<ColCheck>,
    /// Where each head column comes from.
    head: Vec<HeadSrc>,
}

#[derive(Clone, Copy, Debug)]
enum ColCheck {
    /// Anything (a wildcard, or a variable's first column).
    Any,
    /// This constant.
    Const(blossom_base::ConstId),
    /// The value of an earlier column (a variable met again).
    Same(usize),
}

#[derive(Clone, Copy, Debug)]
enum HeadSrc {
    Col(usize),
    Const(blossom_base::ConstId),
}

impl CopyPlan {
    /// The copy plan of `rule`, if its body is one positive atom and its head plain terms over the atom's
    /// variables.
    fn of(rule: &Rule) -> Option<CopyPlan> {
        let [Literal::Pos(a)] = rule.body.lits.as_slice() else { return None };
        let mut first: std::collections::BTreeMap<VarId, usize> = std::collections::BTreeMap::new();
        let mut cols = Vec::new();
        for (i, t) in atom_terms(a).enumerate() {
            cols.push(match t {
                Term::Wild => ColCheck::Any,
                Term::Const(c) => ColCheck::Const(*c),
                Term::Var(v) => match first.get(v) {
                    Some(j) => ColCheck::Same(*j),
                    None => {
                        first.insert(*v, i);
                        ColCheck::Any
                    }
                },
            });
        }
        let mut head = Vec::new();
        for h in &rule.head.args {
            head.push(match h {
                HeadArg::Term(Term::Var(v)) => HeadSrc::Col(*first.get(v)?),
                HeadArg::Term(Term::Const(c)) => HeadSrc::Const(*c),
                _ => return None,
            });
        }
        Some(CopyPlan { cols, head })
    }

    /// The head row `row` maps to, if it matches the atom.
    pub(crate) fn row(&self, cx: &Ctx<'_>, row: &[Value]) -> ExprResult<Option<Row>> {
        if row.len() != self.cols.len() {
            return Err(bug(format!("{} terms against {} values", self.cols.len(), row.len())));
        }
        let konst = |c: &blossom_base::ConstId| -> ExprResult<&Value> {
            cx.program
                .consts
                .get(*c)
                .ok_or_else(|| bug(format!("unknown constant {c:?}")))
        };
        for (i, check) in self.cols.iter().enumerate() {
            let held = row.get(i).ok_or_else(|| bug("a copied column out of range".into()))?;
            let ok = match check {
                ColCheck::Any => true,
                ColCheck::Const(c) => konst(c)? == held,
                ColCheck::Same(j) => row.get(*j).is_some_and(|x| x == held),
            };
            if !ok {
                return Ok(None);
            }
        }
        let mut out = Vec::with_capacity(self.head.len());
        for src in &self.head {
            out.push(match src {
                HeadSrc::Col(i) => row
                    .get(*i)
                    .cloned()
                    .ok_or_else(|| bug("a copied column out of range".into()))?,
                HeadSrc::Const(c) => konst(c)?.clone(),
            });
        }
        Ok(Some(Row::from(out)))
    }
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

/// A probe of an atom on more columns than the bound ones: those a later binding `let x = e` gives, where `x` is the
/// column's variable and `e` reads only bound variables. The binding still runs at its place in the plan (it is the
/// equality that keeps a row); its value is only computed early, to find the rows it can keep. The rows the probe
/// skips would have failed that binding, and every check before it in the plan cannot fail, so none of them would
/// have raised an error either. If the early value fails to evaluate, the atom is probed as if there were no key, and
/// the binding raises its error at its place. The early value is computed for valuations the plan might never have
/// evaluated the binding for, so its expression may only compute from values ([`keyable`]): no call (a function's
/// cost, or a blob's bytes), only operators, construction and field access.
#[derive(Clone, Debug)]
pub(crate) struct KeyProbe {
    /// The probe's columns: the atom's bound ones, then the keyed ones.
    pub cols: Vec<usize>,
    /// The keyed columns' bindings, by literal index, in the order of their columns in `cols`.
    pub from: Vec<usize>,
}

/// One step of a term.
#[derive(Clone, Debug)]
pub(crate) enum Step {
    /// Joins an atom, probing the columns whose values are known (`cols`), and a range of one more column; or, when
    /// `key` is given and its values evaluate, the key's columns.
    Atom {
        lit: usize,
        cols: Vec<usize>,
        range: Option<RangeProbe>,
        key: Option<Box<KeyProbe>>,
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
        Expr::Typed { expr, .. } => expr_vars(expr, out),
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
        // The other literals, once the variables they need are bound (LANGUAGE §9.14): every ready check that cannot
        // fail, in body order; then the first ready fallible guard, or else the first ready fallible binding; again.
        let mut done: Vec<bool> = lits.iter().map(|l| matches!(l, Literal::Pos(_))).collect();
        let mut checks = Vec::new();
        loop {
            loop {
                let mut progressed = false;
                for (i, lit) in lits.iter().enumerate() {
                    if done.get(i).copied().unwrap_or(true) || !lit.cannot_fail() || !check_ready(lit, &bound)? {
                        continue;
                    }
                    bind_outputs(lit, &mut bound);
                    checks.push(i);
                    if let Some(d) = done.get_mut(i) {
                        *d = true;
                    }
                    progressed = true;
                }
                if !progressed {
                    break;
                }
            }
            let first = |filter: bool| -> Result<Option<usize>, EvalError> {
                for (i, lit) in lits.iter().enumerate() {
                    if done.get(i).copied().unwrap_or(true) || (filter && !matches!(lit, Literal::Guard(_))) {
                        continue;
                    }
                    if check_ready(lit, &bound)? {
                        return Ok(Some(i));
                    }
                }
                Ok(None)
            };
            let pick = match first(true)? {
                Some(i) => Some(i),
                None => first(false)?,
            };
            let Some(i) = pick else { break };
            if let Some(lit) = lits.get(i) {
                bind_outputs(lit, &mut bound);
            }
            checks.push(i);
            if let Some(d) = done.get_mut(i) {
                *d = true;
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
        let dep_keys = deps.iter().filter_map(|l| rule.body.lits.get(*l).and_then(dep_store)).collect();
        let aggregate = crate::strata::is_aggregate(rule);
        let copy = if aggregate { None } else { CopyPlan::of(rule) };
        let compared_only = lits.iter().all(|l| match l {
            Literal::Guard(e) | Literal::Bind { expr: e, .. } => time_only_compared(e),
            Literal::Gen { src, .. } => match src {
                GenSource::Range { lo, hi, .. } => time_only_compared(lo) && time_only_compared(hi),
                GenSource::Value(e) | GenSource::Lattice(e) => time_only_compared(e),
                GenSource::TableFn { .. } => false,
            },
            _ => true,
        });
        let skip = match (time_varying, compared_only) {
            (false, _) => Skip::Unchanged,
            (true, true) => Skip::UntilFlip,
            (true, false) => Skip::Never,
        };
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
            dep_keys,
            head,
            aggregate,
            copy,
            skip,
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
                key: Option<KeyProbe>,
                rows: usize,
            }
            let mut best: Option<Candidate> = None;
            for (pos, &lit) in left.iter().enumerate() {
                let Some(Literal::Pos(a)) = rule.body.lits.get(lit) else { continue };
                let cols = bound_columns(a, &bound);
                let range = self.range_for(rule, a, &cols, &bound, next);
                let key = self.key_for(rule, a, &cols, &bound, next);
                let rows = match &key {
                    Some(k) => cost(lit, &k.cols, false),
                    None => cost(lit, &cols, range.is_some()),
                };
                if best.as_ref().is_none_or(|b| rows < b.rows) {
                    best = Some(Candidate {
                        pos,
                        lit,
                        cols,
                        range,
                        key,
                        rows,
                    });
                }
            }
            let Some(Candidate {
                pos,
                lit: i,
                cols,
                range,
                key,
                ..
            }) = best
            else {
                break;
            };
            left.remove(pos);
            if let Some(Literal::Pos(a)) = rule.body.lits.get(i) {
                bound.extend(atom_vars(a));
            }
            steps.push(Step::Atom {
                lit: i,
                cols,
                range,
                key: key.map(Box::new),
            });
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

    /// A key for `a` from the bindings not yet run (from `next`): each unbound column whose variable a binding
    /// `let x = e` gives, with `e` over bound variables, where every check before that binding in the plan cannot fail
    /// ([`KeyProbe`]).
    fn key_for(&self, rule: &Rule, a: &Atom, cols: &[usize], bound: &BTreeSet<VarId>, next: usize) -> Option<KeyProbe> {
        let pending = self.checks.get(next..).unwrap_or_default();
        let mut key = KeyProbe {
            cols: cols.to_vec(),
            from: Vec::new(),
        };
        for (c, t) in a.args.iter().enumerate() {
            let Term::Var(v) = t else { continue };
            if cols.contains(&c) || bound.contains(v) {
                continue;
            }
            for &lit in pending {
                let Some(l) = rule.body.lits.get(lit) else { break };
                if let Literal::Bind { pat: Pattern::Var(x), expr } = l
                    && x == v
                {
                    if vars_of(expr).is_subset(bound) && keyable(expr) {
                        key.cols.push(c);
                        key.from.push(lit);
                    }
                    break;
                }
                if !l.cannot_fail() {
                    break;
                }
            }
        }
        (!key.from.is_empty()).then_some(key)
    }

    /// A range on one of `a`'s unbound columns from the guards among the checks not yet run (from `next`), up to and
    /// including the first check that can fail: the rows the range drops fail a guard that runs before any check
    /// that could raise an error on them (LANGUAGE §9.14). A guard's conjuncts count up to its first fallible one. An
    /// end that fails to evaluate is no bound (`range_end`): the guard then raises its error on the rows it reaches.
    fn range_for(&self, rule: &Rule, a: &Atom, cols: &[usize], bound: &BTreeSet<VarId>, next: usize) -> Option<RangeProbe> {
        let mut probe: Option<RangeProbe> = None;
        'checks: for &lit in self.checks.get(next..).unwrap_or_default() {
            let l = rule.body.lits.get(lit)?;
            if let Literal::Guard(e) = l {
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
                    if !c.cannot_fail() {
                        break 'checks;
                    }
                }
            }
            if !l.cannot_fail() {
                break;
            }
        }
        probe
    }
}

/// Whether an expression only computes from values, so that computing it early, for a probe key, reads and costs
/// nothing the plan would not: variables, constants, parameters, the tick's scalars, operators, construction, field
/// access and conditionals over such expressions.
fn keyable(e: &Expr) -> bool {
    match e {
        Expr::Term(_) | Expr::Param(_) | Expr::Scalar(_) => true,
        Expr::Unary { arg, .. } => keyable(arg),
        Expr::Binary { lhs, rhs, .. } => keyable(lhs) && keyable(rhs),
        Expr::Construct { fields, .. } => fields.iter().all(keyable),
        Expr::Field { base, .. } => keyable(base),
        Expr::Typed { expr, .. } => keyable(expr),
        Expr::If { cond, then, els } => keyable(cond) && keyable(then) && keyable(els),
        Expr::Call { .. }
        | Expr::Match { .. }
        | Expr::Collection { .. }
        | Expr::Lattice { .. }
        | Expr::Let { .. }
        | Expr::Closure { .. } => false,
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

/// Whether a check's inputs are bound, for planning (a table-function generator is not implemented).
fn check_ready(l: &Literal, bound: &BTreeSet<VarId>) -> Result<bool, EvalError> {
    if let Literal::Gen {
        src: GenSource::TableFn { .. },
        ..
    } = l
    {
        return Err(blossom_base::unimplemented_error!("LANG-183", "table-function generators in the engine").into());
    }
    Ok(ready(l, bound))
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
    stores: &Stores,
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
    // A negation or lookup driver is a check: an error raised by a check before it does not depend on its change.
    let driver_check = match driver {
        Driver::Neg { lit, .. } | Driver::Lookup { lit, .. } => plan.checks.iter().position(|c| c == lit),
        _ => None,
    };
    let mut search = Search {
        cx,
        stores,
        rule,
        plan,
        driver,
        driver_check,
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
    stores: &'a Stores,
    rule: &'a Rule,
    plan: &'a Plan,
    driver: &'a Driver,
    /// The driver's position among the checks, when it is a negation or a lookup.
    driver_check: Option<usize>,
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
            Step::Atom { lit, cols, range, key } => {
                self.atom(pc, *lit, cols, range.as_ref(), key.as_deref(), env, rows, looked, failed)
            }
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
        key: Option<&KeyProbe>,
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
            // Only after a failed check can a planned column's variable be unbound (the check would have bound it).
            let unbound = matches!(t, Term::Var(v) if env.get(v.index()).is_none_or(Option::is_none));
            if unbound && failed.is_some() {
                probe = false;
                break;
            }
            values.push(expr::term(self.cx, env, t).map_err(fatal)?);
        }
        let store = self.store(atom_store(a))?;
        let old = (self.old)(lit);
        let keyed = match key {
            Some(k) if probe && failed.is_none() => self.key_values(k, &values, env)?,
            _ => None,
        };
        let candidates = if let (Some(k), Some(kv)) = (key, &keyed) {
            store.rows(old, &k.cols, kv)?
        } else if !probe {
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

    /// A key probe's values: the bound columns' `values`, then each keyed binding's value; `None` when one fails to
    /// evaluate (the binding raises it at its place).
    fn key_values(
        &self,
        key: &KeyProbe,
        values: &[Value],
        env: &[Option<Value>],
    ) -> Result<Option<Vec<Value>>, EvalError> {
        let mut out = values.to_vec();
        for lit in &key.from {
            let Some(Literal::Bind { expr: e, .. }) = self.rule.body.lits.get(*lit) else {
                return Err(internal_error!("a key probe names a non-binding").into());
            };
            match expr::eval(self.cx, env, e) {
                Ok(v) => out.push(v),
                Err(_) => return Ok(None),
            }
        }
        Ok(Some(out))
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
    ///
    /// The reference raises a check's error for every valuation of the atoms and the checks before it, whatever the
    /// checks after it say. So in a term driven by a negation's or a lookup's change, an error from a check before
    /// the driver is not this term's: the set of erring valuations does not depend on the driver, and the terms
    /// driven by the atoms count it.
    fn fail(
        &mut self,
        pc: usize,
        e: ExprError,
        env: &mut Vec<Option<Value>>,
        rows: &mut Vec<Option<Row>>,
        looked: &mut Vec<Value>,
    ) -> Result<(), EvalError> {
        if let (Some(Step::Check(k)), Some(d)) = (self.steps.get(pc), self.driver_check)
            && *k < d
        {
            return Ok(());
        }
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
