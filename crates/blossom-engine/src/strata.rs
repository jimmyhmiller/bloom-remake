//! The engine's stratification of the deductive rules (SEM-020), independent of the oracle's: a relation depends on
//! every relation a deductive rule deriving it reads, strictly when the read is negated, the rule aggregates, or a
//! lattice read is not monotone (SEM-102). Strongly connected components in dependency order are the strata; a strict
//! edge inside one is rejected.

use std::collections::BTreeSet;

use blossom_base::graph::{AdjacencyList, tarjan_scc};
use blossom_base::{RelId, RuleId, internal_error};
use blossom_ir::core::{HeadArg, Literal, Program, Rule, RuleKind};
use blossom_ir::tick::EvalError;

/// One stratum.
#[derive(Clone, Debug)]
pub(crate) struct Stratum {
    /// Aggregate rules, which read only lower strata.
    pub aggregates: Vec<RuleId>,
    pub rules: Vec<RuleId>,
    /// Whether a rule reads a relation of its own stratum.
    pub recursive: bool,
}

pub(crate) fn is_aggregate(rule: &Rule) -> bool {
    rule.head.args.iter().any(|a| matches!(a, HeadArg::Agg(_)))
}

/// The relations a rule's body reads.
pub(crate) fn reads(rule: &Rule) -> impl Iterator<Item = RelId> + '_ {
    rule.body.lits.iter().filter_map(|l| match l {
        Literal::Pos(a) | Literal::Neg(a) => Some(a.rel),
        Literal::Lookup { rel, .. } => Some(*rel),
        _ => None,
    })
}

pub(crate) fn stratify(p: &Program) -> Result<Vec<Stratum>, EvalError> {
    let mut graph = AdjacencyList::new(p.rels.len());
    let mut strict: Vec<(usize, usize)> = Vec::new();
    let exact = blossom_ir::polarity::non_monotone_reads(p);
    for rule in p.rules.iter() {
        if rule.kind != RuleKind::Deductive {
            continue;
        }
        let head = rule.head.rel.index();
        let agg = is_aggregate(rule);
        for (i, lit) in rule.body.lits.iter().enumerate() {
            let (rel, negated) = match lit {
                Literal::Pos(a) => (a.rel, false),
                Literal::Neg(a) => (a.rel, true),
                Literal::Lookup { rel, .. } => (*rel, false),
                _ => continue,
            };
            graph
                .add_edge(head, rel.index())
                .map_err(|e| internal_error!("the dependency graph: {e}"))?;
            if negated || agg || exact.contains(&(rule.id, i)) {
                strict.push((head, rel.index()));
            }
        }
    }
    let sccs = tarjan_scc(&graph).map_err(|e| internal_error!("the dependency graph: {e}"))?;
    for &(head, body) in &strict {
        if sccs.component_of.get(head) == sccs.component_of.get(body) {
            let name = |i: usize| {
                p.rels
                    .iter()
                    .nth(i)
                    .map_or_else(|| format!("#{i}"), |r| r.name.to_string())
            };
            return Err(EvalError::NotStratifiable(format!(
                "`{}` depends on `{}` through negation or aggregation on a same-tick cycle",
                name(head),
                name(body)
            )));
        }
    }
    let mut out = Vec::new();
    for component in &sccs.components {
        let rels: BTreeSet<RelId> = component
            .iter()
            .filter_map(|&i| u32::try_from(i).ok().map(RelId::from_raw))
            .collect();
        let (mut aggregates, mut rules, mut recursive) = (Vec::new(), Vec::new(), false);
        for (id, rule) in p.rules.iter_enumerated() {
            if rule.kind != RuleKind::Deductive || !rels.contains(&rule.head.rel) {
                continue;
            }
            if is_aggregate(rule) {
                aggregates.push(id);
            } else {
                rules.push(id);
            }
            recursive |= reads(rule).any(|r| rels.contains(&r));
        }
        if aggregates.is_empty() && rules.is_empty() {
            continue;
        }
        out.push(Stratum {
            aggregates,
            rules,
            recursive,
        });
    }
    Ok(out)
}
