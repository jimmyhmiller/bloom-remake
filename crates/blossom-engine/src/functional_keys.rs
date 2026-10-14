//! Conservative functional keys for derived relations. These are physical hints, never new
//! language constraints: compact buckets still retain every conflicting full row independently.
use std::collections::{BTreeMap, BTreeSet};

use blossom_base::{RelId, RuleId, VarId};
use blossom_ir::core::{Expr, HeadArg, HeadMode, Literal, Pattern, Program, RelClass, Rule, RuleKind, Term};

type Keys = BTreeMap<RelId, Vec<usize>>;

pub(crate) fn infer(p: &Program, running: &BTreeSet<RuleId>, recursive: &BTreeSet<RelId>) -> Keys {
    let mut keys: Keys = p
        .rels
        .iter_enumerated()
        .map(|(id, r)| (id, r.schema.key.iter().map(|c| c.index()).collect()))
        .collect();
    let mut purity = BTreeMap::new();
    let rules: Vec<&Rule> = p
        .rules
        .iter_enumerated()
        .filter_map(|(id, rule)| {
            let r = p.rels.get(rule.head.rel)?;
            (running.contains(&id)
                && rule.kind == RuleKind::Deductive
                && rule.head.mode == HeadMode::Insert
                && !recursive.contains(&rule.head.rel)
                && r.class == RelClass::Idb
                && !r.durable
                && r.schema.lattice.is_empty()
                && !p.facts.iter().any(|f| f.rel == rule.head.rel)
                && p.rules
                    .iter_enumerated()
                    .filter(|(id, r)| running.contains(id) && r.head.rel == rule.head.rel)
                    .count()
                    == 1
                && crate::purity::rule_is_pure(p, rule, &mut purity))
            .then_some(rule)
        })
        .collect();
    loop {
        let mut changed = false;
        for rule in &rules {
            // Starting with every head term is always a set key. Remove columns only when the
            // surviving columns determine every head term through the body's functional dependencies.
            let mut candidate: Vec<usize> = (0..rule.head.args.len()).collect();
            for col in (0..rule.head.args.len()).rev() {
                let trial: Vec<usize> = candidate.iter().copied().filter(|c| *c != col).collect();
                if determines_head(rule, &trial, &keys, p) {
                    candidate = trial;
                }
            }
            if let Some(current) = keys.get_mut(&rule.head.rel)
                && candidate.len() < current.len()
            {
                *current = candidate;
                changed = true;
            }
        }
        if !changed {
            return keys;
        }
    }
}

fn term_known(t: &Term, known: &BTreeSet<VarId>) -> bool {
    match t {
        Term::Var(v) => known.contains(v),
        Term::Const(_) => true,
        Term::Wild => false,
    }
}
fn add_term(t: &Term, known: &mut BTreeSet<VarId>) {
    if let Term::Var(v) = t {
        known.insert(*v);
    }
}
fn pattern_vars(p: &Pattern, known: &mut BTreeSet<VarId>) {
    match p {
        Pattern::Var(v) => {
            known.insert(*v);
        }
        Pattern::Tuple(ps) | Pattern::Variant { fields: ps, .. } => {
            for p in ps {
                pattern_vars(p, known);
            }
        }
        Pattern::Struct { fields, .. } => {
            for (_, p) in fields {
                pattern_vars(p, known);
            }
        }
        Pattern::Wild | Pattern::Const(_) => {}
    }
}
fn pattern_known(p: &Pattern, known: &BTreeSet<VarId>) -> bool {
    match p {
        Pattern::Var(v) => known.contains(v),
        Pattern::Const(_) => true,
        Pattern::Wild => false,
        Pattern::Tuple(ps) | Pattern::Variant { fields: ps, .. } => ps.iter().all(|p| pattern_known(p, known)),
        // A partial struct pattern does not determine the omitted fields.
        Pattern::Struct { .. } => false,
    }
}
fn expr_known(e: &Expr, known: &BTreeSet<VarId>) -> bool {
    match e {
        Expr::Term(t) => term_known(t, known),
        Expr::Param(_) | Expr::Scalar(_) => true,
        Expr::Unary { arg, .. } | Expr::Field { base: arg, .. } | Expr::Typed { expr: arg, .. } => {
            expr_known(arg, known)
        }
        Expr::Binary { lhs, rhs, .. } => expr_known(lhs, known) && expr_known(rhs, known),
        Expr::Call { args, .. } | Expr::Lattice { args, .. } => args.iter().all(|e| expr_known(e, known)),
        Expr::Construct { fields, .. } => fields.iter().all(|e| expr_known(e, known)),
        Expr::Collection { elems, .. } => elems.iter().all(|e| expr_known(e, known)),
        Expr::If { cond, then, els } => [cond, then, els].iter().all(|e| expr_known(e, known)),
        Expr::Let { pat, value, body } => {
            let mut local = known.clone();
            pattern_vars(pat, &mut local);
            expr_known(value, known) && expr_known(body, &local)
        }
        Expr::Closure { params, body } => {
            let mut local = known.clone();
            local.extend(params);
            expr_known(body, &local)
        }
        Expr::Match { scrut, arms } => {
            expr_known(scrut, known)
                && arms.iter().all(|(pat, guard, body)| {
                    let mut local = known.clone();
                    pattern_vars(pat, &mut local);
                    guard.as_ref().is_none_or(|g| expr_known(g, &local)) && expr_known(body, &local)
                })
        }
    }
}
/// A known constructed value determines its constituent expressions, only through injective constructors.
fn invert(e: &Expr, known: &mut BTreeSet<VarId>) {
    match e {
        Expr::Term(t) => add_term(t, known),
        Expr::Construct { fields, .. } => {
            for e in fields {
                invert(e, known);
            }
        }
        Expr::Typed { expr, .. } => invert(expr, known),
        _ => {}
    }
}
fn determines_head(rule: &Rule, cols: &[usize], keys: &Keys, p: &Program) -> bool {
    let mut known = BTreeSet::new();
    for c in cols {
        match rule.head.args.get(*c) {
            Some(HeadArg::Term(t)) => add_term(t, &mut known),
            _ => return false,
        }
    }
    loop {
        let before = known.len();
        for lit in &rule.body.lits {
            match lit {
                Literal::Pos(a)
                    if p.rels
                        .get(a.rel)
                        .is_some_and(|r| !matches!(r.class, RelClass::Channel(_)))
                        && a.sender.is_none()
                        && a.principal.is_none()
                        && a.weight.is_none()
                        && a.spec.is_none() =>
                {
                    if keys.get(&a.rel).is_some_and(|key| {
                        key.iter()
                            .all(|c| a.args.get(*c).is_some_and(|t| term_known(t, &known)))
                    }) {
                        for t in &a.args {
                            add_term(t, &mut known);
                        }
                    }
                }
                Literal::Bind { pat, expr } => {
                    if expr_known(expr, &known) {
                        pattern_vars(pat, &mut known);
                    }
                    if pattern_known(pat, &known) {
                        invert(expr, &mut known);
                    }
                }
                Literal::Lookup { var, key, .. } if key.iter().all(|t| term_known(t, &known)) => {
                    known.insert(*var);
                }
                // A known generator source does not determine which member of the collection was chosen.
                _ => {}
            }
        }
        if known.len() == before {
            break;
        }
    }
    rule.head.args.iter().all(|a| match a {
        HeadArg::Term(t) => term_known(t, &known),
        HeadArg::Agg(_) => false,
    })
}
