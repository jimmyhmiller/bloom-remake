//! The oracle's own stratifier (ARCHITECTURE §11.2: it shares no code with `blossom-analysis`).
//!
//! Only the deductive rules must stratify (temporal stratification, SEM-020): a relation depends on every relation
//! a deductive rule deriving it reads, strictly when the read is negated or the rule aggregates. The strongly
//! connected components of that graph, in dependency order, are the strata; a strict edge inside a component is a
//! negation or aggregation through recursion, which is rejected.

use std::collections::BTreeSet;

use blossom_base::graph::{AdjacencyList, tarjan_scc};
use blossom_base::{RelId, RuleId, internal_error};
use blossom_ir::core::{HeadArg, Literal, Program, RuleKind};

use crate::OracleError;

/// One stratum: the deductive rules deriving one strongly connected set of relations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stratum {
    /// The relations the stratum derives.
    pub rels: BTreeSet<RelId>,
    /// Its aggregate rules, which read only lower strata and run once, first.
    pub aggregates: Vec<RuleId>,
    /// Its other rules.
    pub rules: Vec<RuleId>,
    /// Whether some rule reads a relation of the same stratum, so the stratum needs iteration to its fixpoint.
    pub recursive: bool,
}

pub(crate) fn is_aggregate(rule: &blossom_ir::core::Rule) -> bool {
    rule.head.args.iter().any(|a| matches!(a, HeadArg::Agg(_)))
}

pub(crate) fn stratify(p: &Program) -> Result<Vec<Stratum>, OracleError> {
    let n = p.rels.len();
    // Edges point from a relation to the relations it depends on.
    let mut graph = AdjacencyList::new(n);
    let mut strict: Vec<(usize, usize)> = Vec::new();
    for rule in p.rules.iter() {
        if rule.kind != RuleKind::Deductive {
            continue;
        }
        let head = rule.head.rel.index();
        let agg = is_aggregate(rule);
        for lit in &rule.body.lits {
            let (atom, negated) = match lit {
                Literal::Pos(a) => (a, false),
                Literal::Neg(a) => (a, true),
                _ => continue,
            };
            let body = atom.rel.index();
            graph
                .add_edge(head, body)
                .map_err(|e| internal_error!("dependency graph: {e}"))?;
            if negated || agg {
                strict.push((head, body));
            }
        }
    }
    let sccs = tarjan_scc(&graph).map_err(|e| internal_error!("dependency graph: {e}"))?;
    for &(head, body) in &strict {
        if sccs.component_of.get(head) == sccs.component_of.get(body) {
            let name = |i: usize| {
                p.rels
                    .iter()
                    .nth(i)
                    .map(|r| r.name.to_string())
                    .unwrap_or_else(|| format!("#{i}"))
            };
            return Err(OracleError::NotStratifiable(format!(
                "`{}` depends on `{}` through negation or aggregation on a same-tick cycle",
                name(head),
                name(body)
            )));
        }
    }
    // Components come dependencies first (tarjan_scc returns them in reverse topological order of the edges).
    let mut strata = Vec::new();
    for component in &sccs.components {
        let rels: BTreeSet<RelId> = component
            .iter()
            .filter_map(|&i| u32::try_from(i).ok().map(RelId::from_raw))
            .collect();
        let mut aggregates = Vec::new();
        let mut rules = Vec::new();
        let mut recursive = false;
        for (id, rule) in p.rules.iter_enumerated() {
            if rule.kind != RuleKind::Deductive || !rels.contains(&rule.head.rel) {
                continue;
            }
            if is_aggregate(rule) {
                aggregates.push(id);
            } else {
                rules.push(id);
            }
            recursive |= rule.body.lits.iter().any(|l| match l {
                Literal::Pos(a) | Literal::Neg(a) => rels.contains(&a.rel),
                _ => false,
            });
        }
        if aggregates.is_empty() && rules.is_empty() {
            continue;
        }
        strata.push(Stratum {
            rels,
            aggregates,
            rules,
            recursive,
        });
    }
    Ok(strata)
}
