//! Per-rule evaluation order: nested loops over the positive atoms, most-bound first, with every guard, binding and
//! negation evaluated as soon as its variables are bound. The order changes only how fast a rule is evaluated,
//! never its result.

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
            flush(lits, &mut done, &mut bound, &mut steps)?;
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

/// Plans every non-positive literal that is evaluable now, in body order, until none is.
fn flush(
    lits: &[Literal],
    done: &mut [bool],
    bound: &mut BTreeSet<VarId>,
    steps: &mut Vec<Step>,
) -> Result<(), OracleError> {
    loop {
        let mut progressed = false;
        for (i, lit) in lits.iter().enumerate() {
            if done.get(i).copied().unwrap_or(true) {
                continue;
            }
            let ready = match lit {
                Literal::Pos(_) => false,
                Literal::Neg(a) => atom_vars(a).is_subset(bound),
                Literal::Guard(e) => expr_vars(e)?.is_subset(bound),
                Literal::Bind { pat, expr } => {
                    let ready = expr_vars(expr)?.is_subset(bound);
                    if ready {
                        pattern_vars(pat, bound)?;
                    }
                    ready
                }
                Literal::Lookup { .. } => {
                    blossom_base::unimplemented_feature!("LANG-280", "lattice lookups in the oracle (WP M4.1)")
                }
                Literal::Gen { pat, src } => {
                    let ready = match src {
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
                        GenSource::Value(_) | GenSource::Lattice(_) | GenSource::TableFn { .. } => {
                            blossom_base::unimplemented_feature!(
                                "LANG-088",
                                "generators over values, lattices and table functions in the oracle"
                            )
                        }
                    };
                    if ready {
                        pattern_vars(pat, bound)?;
                    }
                    ready
                }
            };
            if ready {
                steps.push(Step::Check { lit: i });
                if let Some(d) = done.get_mut(i) {
                    *d = true;
                }
                progressed = true;
            }
        }
        if !progressed {
            return Ok(());
        }
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
    }
    Ok(())
}
