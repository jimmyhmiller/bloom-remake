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

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::{RelId, RuleId, VarId, internal_error};
use blossom_ir::core::{Atom, Expr, GenSource, HeadArg, Literal, Pattern, Rule, RuleKind, Term};
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
        Ok(Plan {
            rule: rule.id,
            nvars: rule.body.vars.len(),
            atoms,
            checks,
            // A rule with no positive atom has valuations even over empty relations (`not r(_)` holds there), which
            // no change announces; it is cheap to re-evaluate (it joins nothing).
            regime: if time_varying || atoms.is_empty() {
                Regime::Recompute
            } else {
                Regime::Delta
            },
            deps,
            head,
            aggregate: crate::strata::is_aggregate(rule),
        })
    }

    /// The order to join the atoms other than `skip`, given the variables `bound` already: the atom with the most
    /// bound columns first (the lowest literal index among equals), each with the columns it probes.
    pub fn join_order(&self, rule: &Rule, skip: Option<usize>, bound: &BTreeSet<VarId>) -> Vec<(usize, Vec<usize>)> {
        let mut bound = bound.clone();
        let mut left: Vec<usize> = self.atoms.iter().copied().filter(|a| Some(*a) != skip).collect();
        let mut out = Vec::new();
        while !left.is_empty() {
            let mut best: Option<(usize, usize, Vec<usize>)> = None;
            for (pos, &i) in left.iter().enumerate() {
                let Some(Literal::Pos(a)) = rule.body.lits.get(i) else { continue };
                let cols = bound_columns(a, &bound);
                if best.as_ref().is_none_or(|(_, _, c)| cols.len() > c.len()) {
                    best = Some((pos, i, cols));
                }
            }
            let Some((pos, i, cols)) = best else { break };
            left.remove(pos);
            if let Some(Literal::Pos(a)) = rule.body.lits.get(i) {
                bound.extend(atom_vars(a));
            }
            out.push((i, cols));
        }
        out
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
    fn lit(&self) -> Option<usize> {
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
    driver: &Driver,
    old: &dyn Fn(usize) -> bool,
    emit: &mut dyn FnMut(Found<'_>) -> ExprResult<()>,
    error: &mut dyn FnMut(Token, ExprError, i64),
) -> Result<(), EvalError> {
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
                return Ok(());
            }
        }
        Driver::Neg { lit, key, .. } => {
            let Some(Literal::Neg(a)) = rule.body.lits.get(*lit) else {
                return Err(internal_error!("a negation driver names a non-negation").into());
            };
            let terms: Vec<&Term> = non_wild(a).iter().filter_map(|c| a.args.get(*c)).collect();
            if !unify(cx, &mut env, terms.into_iter(), key).map_err(fatal)? {
                return Ok(());
            }
        }
        Driver::Lookup { lit, key, value, .. } => {
            let Some(Literal::Lookup { var, key: terms, .. }) = rule.body.lits.get(*lit) else {
                return Err(internal_error!("a lookup driver names a non-lookup").into());
            };
            if !unify(cx, &mut env, terms.iter(), key).map_err(fatal)? {
                return Ok(());
            }
            match env.get_mut(var.index()) {
                Some(slot @ None) => *slot = Some(value.clone()),
                Some(Some(v)) if v == value => {}
                Some(Some(_)) => return Ok(()),
                None => return Err(internal_error!("variable {var:?} out of range").into()),
            }
        }
    }
    let bound: BTreeSet<VarId> = env
        .iter()
        .enumerate()
        .filter(|(_, v)| v.is_some())
        .filter_map(|(i, _)| u32::try_from(i).ok().map(VarId::from_raw))
        .collect();
    let skip = match driver {
        Driver::Atom { lit, .. } => Some(*lit),
        _ => None,
    };
    let order = plan.join_order(rule, skip, &bound);
    let driver_row = match driver {
        Driver::Atom { row, .. } => Some(row.clone()),
        _ => None,
    };
    let mut rows: Vec<Option<Row>> = vec![None; rule.body.lits.len()];
    if let (Some(l), Some(r)) = (skip, driver_row)
        && let Some(slot) = rows.get_mut(l)
    {
        *slot = Some(r);
    }
    let mut search = Search {
        cx,
        stores,
        rule,
        plan,
        driver,
        old,
        order: &order,
        sign,
        emit,
        error,
    };
    search.atoms(0, &mut env, &mut rows)
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
                Some(slot @ None) => *slot = Some(v.clone()),
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

struct Search<'a, 'b> {
    cx: &'a Ctx<'a>,
    stores: &'a BTreeMap<StoreKey, Store>,
    rule: &'a Rule,
    plan: &'a Plan,
    driver: &'a Driver,
    old: &'a dyn Fn(usize) -> bool,
    order: &'a [(usize, Vec<usize>)],
    sign: i64,
    emit: &'b mut dyn FnMut(Found<'_>) -> ExprResult<()>,
    error: &'b mut dyn FnMut(Token, ExprError, i64),
}

impl Search<'_, '_> {
    fn store(&self, key: StoreKey) -> Result<&Store, EvalError> {
        self.stores
            .get(&key)
            .ok_or_else(|| internal_error!("no store for {key:?}").into())
    }

    fn atoms(&mut self, step: usize, env: &mut Vec<Option<Value>>, rows: &mut Vec<Option<Row>>) -> Result<(), EvalError> {
        let Some((lit, cols)) = self.order.get(step) else {
            return self.checks(0, env, rows, &mut Vec::new());
        };
        let Some(Literal::Pos(a)) = self.rule.body.lits.get(*lit) else {
            return Err(internal_error!("a join step names a non-atom").into());
        };
        let terms: Vec<&Term> = atom_terms(a).collect();
        let mut values = Vec::with_capacity(cols.len());
        for c in cols {
            let t = terms
                .get(*c)
                .ok_or_else(|| internal_error!("a bound column is out of range"))?;
            values.push(expr::term(self.cx, env, t).map_err(fatal)?);
        }
        let candidates = self.store(atom_store(a))?.rows((self.old)(*lit), cols, &values)?;
        for row in candidates {
            let saved = env.clone();
            if unify(self.cx, env, terms.iter().copied(), &row).map_err(fatal)? {
                if let Some(slot) = rows.get_mut(*lit) {
                    *slot = Some(row.clone());
                }
                self.atoms(step + 1, env, rows)?;
                if let Some(slot) = rows.get_mut(*lit) {
                    *slot = None;
                }
            }
            *env = saved;
        }
        Ok(())
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

    fn checks(
        &mut self,
        step: usize,
        env: &mut Vec<Option<Value>>,
        rows: &mut Vec<Option<Row>>,
        looked: &mut Vec<Value>,
    ) -> Result<(), EvalError> {
        let Some(&lit) = self.plan.checks.get(step) else {
            let sign = self.sign;
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
                    let present = !self.store(StoreKey::Main(a.rel))?.rows((self.old)(lit), &cols, &values)?.is_empty();
                    Ok(!present)
                }
            }
            Some(Literal::Guard(e)) => expr::eval(self.cx, env, e).and_then(|v| expr::truth(&v)),
            Some(Literal::Bind { pat, expr: e }) => match expr::eval(self.cx, env, e) {
                Ok(v) => {
                    let saved = env.clone();
                    let r = expr::matches(self.cx, env, pat, &v, &mut Vec::new());
                    match r {
                        Ok(true) => {
                            self.checks(step + 1, env, rows, looked)?;
                            *env = saved;
                            return Ok(());
                        }
                        Ok(false) => {
                            *env = saved;
                            Ok(false)
                        }
                        Err(e) => Err(e),
                    }
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
                let saved = env.clone();
                if let Some(slot) = env.get_mut(var.index()) {
                    *slot = Some(value.clone());
                }
                looked.push(value);
                self.checks(step + 1, env, rows, looked)?;
                looked.pop();
                *env = saved;
                return Ok(());
            }
            Some(Literal::Gen { pat, src }) => match expr::generate(self.cx, env, src) {
                Ok(values) => {
                    for v in values {
                        let saved = env.clone();
                        match expr::matches(self.cx, env, pat, &v, &mut Vec::new()) {
                            Ok(true) => self.checks(step + 1, env, rows, looked)?,
                            Ok(false) => {}
                            Err(ExprError::Eval(e)) => return Err(e),
                            Err(e) => {
                                let token = self.token(rows, looked);
                                (self.error)(token, e, self.sign);
                            }
                        }
                        *env = saved;
                    }
                    return Ok(());
                }
                Err(e) => Err(e),
            },
            other => return Err(internal_error!("a check step names {other:?}").into()),
        };
        match result {
            Ok(true) => self.checks(step + 1, env, rows, looked),
            Ok(false) => Ok(()),
            Err(ExprError::Eval(e)) => Err(e),
            Err(e) => {
                let token = self.token(rows, looked);
                (self.error)(token, e, self.sign);
                Ok(())
            }
        }
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
