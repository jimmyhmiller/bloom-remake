//! Key-proven relational fusion. A producer head determines each positive source row,
//! so each output row identifies exactly one body valuation. Substitution therefore preserves
//! set boundaries and derivation counts. Logical observations are reconstructed on demand.
use crate::{compiled::Kernel, strata};
use blossom_base::{RelId, RuleId, VarId, internal_error};
use blossom_ir::{
    ValidatedProgram,
    core::{Atom, HeadArg, HeadMode, Literal, RelClass, Rule, RuleKind, Term},
    tick::EvalError,
};
use std::collections::{BTreeMap, BTreeSet};

// Bound generated plan growth. A rejected expansion leaves its materialized producer intact.
const MAX_LITERALS: usize = 128;

fn ordinary(p: &blossom_ir::core::Program, rule: &Rule) -> bool {
    rule.construct.is_none_or(|id| {
        p.constructs
            .get(id)
            .is_some_and(|c| matches!(c.kind, blossom_ir::core::ConstructKind::ViewAlternatives { .. }))
    })
}

// A head must identify one source-row combination. Functional dependencies let us omit
// payload fields without crossing a set/distinct boundary. Inferred keys are used only
// where the inference pass established them from pure, single-writer definitions.
fn lossless(rule: &Rule, p: &blossom_ir::core::Program, keys: &BTreeMap<RelId, Vec<usize>>) -> bool {
    if rule.kind != RuleKind::Deductive || rule.head.mode != HeadMode::Insert || !Kernel::eligible(rule) {
        return false;
    }
    let mut known = BTreeSet::new();
    for arg in &rule.head.args {
        let HeadArg::Term(Term::Var(v)) = arg else { return false };
        if !known.insert(*v) {
            return false;
        }
    }
    let atoms: Vec<_> = rule
        .body
        .lits
        .iter()
        .filter_map(|l| match l {
            Literal::Pos(a) => Some(a),
            _ => None,
        })
        .collect();
    if atoms.iter().any(|a| {
        a.sender.is_some()
            || a.principal.is_some()
            || a.spec.is_some()
            || p.rels
                .get(a.rel)
                .is_none_or(|r| matches!(r.class, RelClass::Channel(_)) || !r.schema.lattice.is_empty())
    }) {
        return false;
    }
    let known_term = |t: &Term, known: &BTreeSet<VarId>| match t {
        Term::Var(v) => known.contains(v),
        Term::Const(_) => true,
        Term::Wild => false,
    };
    let mut determined = BTreeSet::new();
    loop {
        let before = determined.len();
        for (i, a) in atoms.iter().enumerate() {
            if keys.get(&a.rel).is_some_and(|cols| {
                cols.iter()
                    .all(|c| a.args.get(*c).is_some_and(|t| known_term(t, &known)))
            }) {
                determined.insert(i);
                for t in &a.args {
                    if let Term::Var(v) = t {
                        known.insert(*v);
                    }
                }
            }
        }
        if before == determined.len() {
            break;
        }
    }
    determined.len() == atoms.len()
}
fn reads(rule: &Rule, rel: RelId) -> bool {
    rule.body.lits.iter().any(|l| {
        matches!(l, Literal::Pos(a) | Literal::Neg(a) if a.rel == rel)
            || matches!(l, Literal::Lookup { rel: r, .. } if *r == rel)
    })
}
fn substitute(atom: &mut Atom, vars: &BTreeMap<VarId, Term>) -> Result<(), EvalError> {
    for term in atom
        .args
        .iter_mut()
        .chain(atom.sender.iter_mut())
        .chain(atom.principal.iter_mut())
        .chain(atom.weight.iter_mut())
    {
        if let Term::Var(v) = term {
            *term = vars
                .get(v)
                .cloned()
                .ok_or_else(|| internal_error!("fusion variable has no substitution"))?;
        }
    }
    Ok(())
}
fn expand(consumer: &Rule, producer: &Rule) -> Result<Rule, EvalError> {
    let mut out = consumer.clone();
    out.body.lits.clear();
    for lit in &consumer.body.lits {
        let Literal::Pos(a) = lit else {
            out.body.lits.push(lit.clone());
            continue;
        };
        if a.rel != producer.head.rel {
            out.body.lits.push(lit.clone());
            continue;
        }
        let mut vars = BTreeMap::new();
        for (head, arg) in producer.head.args.iter().zip(&a.args) {
            let HeadArg::Term(Term::Var(v)) = head else {
                return Err(internal_error!("non-variable lossless head").into());
            };
            let term = if matches!(arg, Term::Wild) {
                let mut decl = producer
                    .body
                    .vars
                    .get(*v)
                    .cloned()
                    .ok_or_else(|| internal_error!("fusion variable declaration missing"))?;
                decl.name = format!("$fused{}", out.body.vars.len()).as_str().into();
                Term::Var(
                    out.body
                        .vars
                        .push(decl)
                        .map_err(|e| internal_error!("fusion variables overflow: {e}"))?,
                )
            } else {
                arg.clone()
            };
            vars.insert(*v, term);
        }
        // Variables determined by the head can remain local to this expansion.
        for (v, decl) in producer.body.vars.iter_enumerated() {
            if let std::collections::btree_map::Entry::Vacant(entry) = vars.entry(v) {
                let mut decl = decl.clone();
                decl.name = format!("$fused{}", out.body.vars.len()).as_str().into();
                let id = out
                    .body
                    .vars
                    .push(decl)
                    .map_err(|e| internal_error!("fusion variables overflow: {e}"))?;
                entry.insert(Term::Var(id));
            }
        }
        for source in &producer.body.lits {
            let mut source = source.clone();
            match &mut source {
                Literal::Pos(a) | Literal::Neg(a) => substitute(a, &vars)?,
                _ => return Err(internal_error!("non-relational fusion producer").into()),
            }
            out.body.lits.push(source);
        }
    }
    Ok(out)
}

pub(crate) fn compile(
    program: &ValidatedProgram,
    runs: &dyn Fn(&Rule) -> bool,
) -> Result<(ValidatedProgram, BTreeMap<RelId, RuleId>), EvalError> {
    let p = program.get();
    let strata = strata::stratify(p)?;
    let recursive = strata
        .iter()
        .filter(|s| s.recursive)
        .flat_map(|s| s.rules.iter().chain(&s.aggregates))
        .filter_map(|id| p.rules.get(*id).map(|r| r.head.rel))
        .collect();
    let running = p
        .rules
        .iter_enumerated()
        .filter(|(_, r)| runs(r))
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    let scoped = crate::engine::tick_scoped(p, &running, &recursive);
    let keys = crate::functional_keys::infer(p, &running.iter().copied().collect(), &recursive);
    let mut physical = p.clone();
    let mut virtuals = BTreeMap::new();
    let mut purity = BTreeMap::new();
    for id in strata.iter().filter(|s| !s.recursive).flat_map(|s| &s.rules) {
        let Some(producer) = physical.rules.get(*id).cloned() else {
            continue;
        };
        let rel = producer.head.rel;
        let Some(decl) = p.rels.get(rel) else { continue };
        if !ordinary(p, &producer)
            || !runs(&producer)
            || !lossless(&producer, p, &keys)
            || decl.class != RelClass::Idb
            || decl.durable
            || decl.interface.is_some()
            || decl.attrs.partition.is_some()
            || decl.attrs.materialize.is_some()
            || decl.attrs.final_output
            || !decl.schema.lattice.is_empty()
            || decl.schema.key.len() != decl.schema.cols.len()
            || p.facts.iter().any(|f| f.rel == rel)
            || p.rules.iter().filter(|r| r.head.rel == rel).count() != 1
        {
            continue;
        }
        // Consumers may compute expressions: expansion only substitutes the relational producer.
        // Keep effectful expressions and native construct boundaries materialized.
        let consumers: Vec<_> = physical.rules.iter().filter(|r| reads(r, rel)).collect();
        if consumers.is_empty()
            // Persistent shared results retain their computation reuse. A single consumer
            // can probe the same base arrangements without maintaining this extra view.
            || (!scoped.contains(&rel) && consumers.len() != 1)
            || consumers.iter().any(|r| {
                !ordinary(p, r)
                    || r.role != producer.role
                    || !crate::purity::rule_is_pure(p, r, &mut purity)
                    || r.body.lits.iter().any(|lit| match lit {
                        Literal::Pos(a) if a.rel == rel => {
                            a.sender.is_some() || a.principal.is_some() || a.spec.is_some()
                        }
                        Literal::Neg(a) if a.rel == rel => true,
                        _ => false,
                    })
            })
        {
            continue;
        }
        if consumers.iter().any(|r| {
            let occurrences = r
                .body
                .lits
                .iter()
                .filter(|l| matches!(l, Literal::Pos(a) if a.rel == rel))
                .count();
            r.body
                .lits
                .len()
                .saturating_sub(occurrences)
                .saturating_add(occurrences.saturating_mul(producer.body.lits.len()))
                > MAX_LITERALS
        }) {
            continue;
        }
        let expanded = consumers
            .iter()
            .map(|r| expand(r, &producer).map(|r| (r.id, r)))
            .collect::<Result<Vec<_>, _>>()?;
        for (id, rule) in expanded {
            *physical
                .rules
                .get_mut(id)
                .ok_or_else(|| internal_error!("fusion consumer disappeared"))? = rule;
        }
        virtuals.insert(rel, producer.id);
    }
    if virtuals.is_empty() {
        return Ok((program.clone(), virtuals));
    }
    let physical = ValidatedProgram::validate(physical)
        .map_err(|errors| internal_error!("invalid fused relational program: {errors:?}"))?;
    Ok((physical, virtuals))
}
