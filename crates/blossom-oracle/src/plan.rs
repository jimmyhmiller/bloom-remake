//! Per-rule evaluation order: nested loops over the positive atoms, most-bound first, then every other literal once
//! its variables are bound: those that cannot fail first, then fallible filters, then fallible bindings, each in body
//! order (LANGUAGE §9.14). Bindings, guards, lookups and generators run only for valuations of all the positive atoms,
//! so a rule with no such valuation raises no runtime error (BLSR004) from an expression it would never have needed,
//! and a filter protects every expression that could run after it. The order of the atoms changes only how fast a rule
//! is evaluated, never its result.

use std::collections::BTreeSet;

use blossom_base::{RelId, VarId, internal_error};
use blossom_ir::core::{Atom, Expr, GenSource, Literal, Pattern, Rule, Term};

use crate::OracleError;

/// One step of a rule's evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Loop over the rows of `rel` that match the columns `bound`, whose values are known at this point.
    Scan { lit: usize, rel: RelId, bound: Vec<usize> },
    /// Evaluate a `Bind`, a `Guard` or a negated atom whose variables are all bound.
    Check { lit: usize },
}

/// How to evaluate one rule.
#[derive(Clone, Debug)]
pub(crate) struct RulePlan {
    pub steps: Vec<Step>,
    pub nvars: usize,
}

impl RulePlan {
    pub fn new(rule: &Rule) -> Result<RulePlan, OracleError> {
        let lits = &rule.body.lits;
        let mut bound: BTreeSet<VarId> = BTreeSet::new();
        let mut done = vec![false; lits.len()];
        let mut steps = Vec::with_capacity(lits.len());
        loop {
            let best = lits
                .iter()
                .enumerate()
                .filter(|(i, _)| !done.get(*i).copied().unwrap_or(true))
                .filter_map(|(i, l)| match l {
                    Literal::Pos(a) => Some((i, a)),
                    _ => None,
                })
                .max_by_key(|(i, a)| (bound_columns(a, &bound).len(), std::cmp::Reverse(*i)));
            let Some((i, atom)) = best else { break };
            steps.push(Step::Scan {
                lit: i,
                rel: atom.rel,
                bound: bound_columns(atom, &bound),
            });
            if let Some(d) = done.get_mut(i) {
                *d = true;
            }
            bound.extend(atom_vars(atom));
        }
        flush(lits, &mut done, &mut bound, &mut steps)?;
        if let Some(i) = done.iter().position(|d| !d) {
            let lit = lits.get(i);
            return Err(internal_error!(
                "rule {}: literal {lit:?} is never evaluable (the IR validator guarantees range restriction)",
                rule.label.text
            )
            .into());
        }
        Ok(RulePlan {
            steps,
            nvars: rule.body.vars.len(),
        })
    }
}

/// Plans the non-positive literals once their variables are bound (LANGUAGE §9.14): every ready check that cannot
/// fail first, in body order; then the first ready fallible filter (a guard), or else the first ready fallible
/// binding (a `let` or a generator), in body order; then again, until none is left. So a filter protects every
/// expression it can: none runs for a valuation a filter that could run before it rejects.
fn flush(
    lits: &[Literal],
    done: &mut [bool],
    bound: &mut BTreeSet<VarId>,
    steps: &mut Vec<Step>,
) -> Result<(), OracleError> {
    let mut plan = |i: usize, bound: &mut BTreeSet<VarId>, done: &mut [bool]| -> Result<(), OracleError> {
        if let Some(lit) = lits.get(i) {
            bind(lit, bound)?;
        }
        steps.push(Step::Check { lit: i });
        if let Some(d) = done.get_mut(i) {
            *d = true;
        }
        Ok(())
    };
    loop {
        loop {
            let mut progressed = false;
            for (i, lit) in lits.iter().enumerate() {
                if done.get(i).copied().unwrap_or(true) || !lit.cannot_fail() || !ready(lit, bound)? {
                    continue;
                }
                plan(i, bound, done)?;
                progressed = true;
            }
            if !progressed {
                break;
            }
        }
        let first = |filter: bool| -> Result<Option<usize>, OracleError> {
            for (i, lit) in lits.iter().enumerate() {
                if done.get(i).copied().unwrap_or(true) || (filter && !matches!(lit, Literal::Guard(_))) {
                    continue;
                }
                if ready(lit, bound)? {
                    return Ok(Some(i));
                }
            }
            Ok(None)
        };
        let pick = match first(true)? {
            Some(i) => Some(i),
            None => first(false)?,
        };
        match pick {
            Some(i) => plan(i, bound, done)?,
            None => return Ok(()),
        }
    }
}

/// Whether a non-positive literal's inputs are bound.
fn ready(lit: &Literal, bound: &BTreeSet<VarId>) -> Result<bool, OracleError> {
    Ok(match lit {
        Literal::Pos(_) => false,
        Literal::Neg(a) => atom_vars(a).is_subset(bound),
        Literal::Guard(e) | Literal::Bind { expr: e, .. } => expr_vars(e)?.is_subset(bound),
        Literal::Lookup { key, .. } => key.iter().all(|t| match t {
            Term::Var(v) => bound.contains(v),
            Term::Const(_) => true,
            Term::Wild => false,
        }),
        Literal::Gen { src, .. } => match src {
            GenSource::Range {
                lo,
                hi,
                ring_bits: None,
                ..
            } => {
                let mut vs = expr_vars(lo)?;
                vs.extend(expr_vars(hi)?);
                vs.is_subset(bound)
            }
            GenSource::Range { ring_bits: Some(_), .. } => {
                blossom_base::unimplemented_feature!("LANG-026", "ring-interval generators in the oracle")
            }
            GenSource::Value(e) | GenSource::Lattice(e) => expr_vars(e)?.is_subset(bound),
            GenSource::TableFn { .. } => {
                blossom_base::unimplemented_feature!("LANG-183", "table-function generators in the oracle")
            }
        },
    })
}

/// Adds the variables a planned literal binds.
fn bind(lit: &Literal, bound: &mut BTreeSet<VarId>) -> Result<(), OracleError> {
    match lit {
        Literal::Bind { pat, .. } | Literal::Gen { pat, .. } => pattern_vars(pat, bound),
        Literal::Lookup { var, .. } => {
            bound.insert(*var);
            Ok(())
        }
        Literal::Pos(_) | Literal::Neg(_) | Literal::Guard(_) => Ok(()),
    }
}

/// The columns of `a` whose values are known; a bound sender is the column after the last (the scan then ranges
/// over rows extended with their sender).
fn bound_columns(a: &Atom, bound: &BTreeSet<VarId>) -> Vec<usize> {
    let known = |t: &Term| match t {
        Term::Const(_) => true,
        Term::Var(v) => bound.contains(v),
        Term::Wild => false,
    };
    let mut cols: Vec<usize> = a
        .args
        .iter()
        .enumerate()
        .filter(|(_, t)| known(t))
        .map(|(i, _)| i)
        .collect();
    if a.sender.as_ref().is_some_and(known) {
        cols.push(a.args.len());
    }
    cols
}

pub(crate) fn atom_vars(a: &Atom) -> BTreeSet<VarId> {
    a.args
        .iter()
        .chain(a.sender.iter())
        .filter_map(|t| match t {
            Term::Var(v) => Some(*v),
            _ => None,
        })
        .collect()
}

/// Adds the variables a binding pattern binds.
fn pattern_vars(p: &Pattern, bound: &mut BTreeSet<VarId>) -> Result<(), OracleError> {
    match p {
        Pattern::Var(v) => {
            bound.insert(*v);
            Ok(())
        }
        Pattern::Wild | Pattern::Const(_) => Ok(()),
        Pattern::Tuple(ps) | Pattern::Variant { fields: ps, .. } => {
            for x in ps {
                pattern_vars(x, bound)?;
            }
            Ok(())
        }
        Pattern::Struct { fields, .. } => {
            for (_, x) in fields {
                pattern_vars(x, bound)?;
            }
            Ok(())
        }
    }
}

/// The variables an expression reads, for the expressions the oracle evaluates.
pub(crate) fn expr_vars(e: &Expr) -> Result<BTreeSet<VarId>, OracleError> {
    let mut out = BTreeSet::new();
    collect(e, &mut out)?;
    Ok(out)
}

fn collect(e: &Expr, out: &mut BTreeSet<VarId>) -> Result<(), OracleError> {
    match e {
        Expr::Term(Term::Var(v)) => {
            out.insert(*v);
        }
        Expr::Term(_) | Expr::Scalar(_) | Expr::Param(_) => {}
        Expr::Unary { arg, .. } => collect(arg, out)?,
        Expr::Binary { lhs, rhs, .. } => {
            collect(lhs, out)?;
            collect(rhs, out)?;
        }
        Expr::If { cond, then, els } => {
            collect(cond, out)?;
            collect(then, out)?;
            collect(els, out)?;
        }
        Expr::Call { args, .. } | Expr::Construct { fields: args, .. } | Expr::Collection { elems: args, .. } => {
            for a in args {
                collect(a, out)?;
            }
        }
        Expr::Field { base, .. } => collect(base, out)?,
        Expr::Match { scrut, arms } => {
            collect(scrut, out)?;
            for (pat, guard, body) in arms {
                // Variables the arm's pattern binds are not read from outside.
                let mut own = BTreeSet::new();
                pattern_vars(pat, &mut own)?;
                let mut inner = BTreeSet::new();
                if let Some(g) = guard {
                    collect(g, &mut inner)?;
                }
                collect(body, &mut inner)?;
                out.extend(inner.difference(&own).copied());
            }
        }
        Expr::Lattice { args, .. } => {
            for a in args {
                collect(a, out)?;
            }
        }
        Expr::Let { value, body, .. } => {
            collect(value, out)?;
            collect(body, out)?;
        }
        Expr::Closure { body, .. } => collect(body, out)?,
        Expr::Typed { expr, .. } => collect(expr, out)?,
    }
    Ok(())
}
