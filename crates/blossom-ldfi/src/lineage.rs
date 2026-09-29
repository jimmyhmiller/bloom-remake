//! The provenance graph of a `.ded` run (ARCHITECTURE §8.2, TEST-023, TEST-026).
//!
//! Every tuple of every protocol relation at every node and tick is a goal; so is every tuple of the spec at EOT.
//! Their supports come from the firing logs:
//!
//! - an input event is a leaf;
//! - a deductive firing at `(n, t)` supports its head at `(n, t)`; an inductive firing supports its head at
//!   `(n, t + 1)`;
//! - an async firing at `(n, t)` supports the channel tuple it delivered at `(dest, t + 1)`, with the premise that
//!   the message `n -> dest` sent at `t` arrived (a clock premise, one per sender and send tick); a node's sends to
//!   itself need no premise;
//! - a spec firing supports its head at EOT; its reads of the spec's inputs are the protocol goals they copy (at EOT
//!   or at the fixed tick of a `p(…)@k` atom), and its reads of the crash oracle are unfalsifiable.
//!
//! A negated read becomes a premise naming the source-level relation and tick, for conservative negative support;
//! a negated read of the crash oracle names the node that must stay correct.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_artifact::ded::{DedArtifact, DedRelIdx, DedRelKind, SpecFeed};
use blossom_base::{InternalError, RelId, internal_error};
use blossom_ir::core::{RelClass, RuleKind};
use blossom_ir::obs::{FiringRecord, NegRead};
use blossom_prov::{Firing, GoalId, GoalKey, Premise, ProvGraph, Space};
use blossom_prov::{Loc, NegRead as ProvNegRead};
use blossom_sim::ded::Outcome;
use blossom_sim::{Fate, SyncRun};
use blossom_value::{
    Value,
    time::{NodeId, Tick},
};

/// A delivered message: relation, sender, receiver, send tick, tuple.
type Arrival = (RelId, NodeId, NodeId, Tick, Arc<[Value]>);

/// Builds the provenance graph of `run` and its `outcome`.
pub fn build(artifact: &DedArtifact, run: &SyncRun, outcome: &Outcome) -> Result<ProvGraph, InternalError> {
    let protocol = artifact.protocol.get();
    let main_protocol: BTreeMap<RelId, u32> = artifact
        .rels
        .iter()
        .enumerate()
        .filter_map(|(i, r)| Some((r.protocol?, u32::try_from(i).ok()?)))
        .collect();
    let mut g = ProvGraph::new();

    // Protocol goals.
    for (t, round) in run.rounds.iter().enumerate() {
        let tick = Tick(u64::try_from(t).map_err(|_| internal_error!("tick overflow"))?);
        for (n, nt) in round.iter().enumerate() {
            let node = NodeId(u32::try_from(n).map_err(|_| internal_error!("node overflow"))?);
            for (rel, rows) in &nt.instance.rels {
                let leaf = matches!(
                    protocol.rels.get(*rel).map(|r| &r.class),
                    Some(RelClass::Event(_) | RelClass::Static)
                );
                for row in rows {
                    let id = g.goal(
                        GoalKey {
                            space: Space::Protocol,
                            rel: *rel,
                            node: Some(node),
                            tick,
                            row: row.clone(),
                        },
                        main_protocol.get(rel).copied(),
                    );
                    if leaf {
                        g.set_leaf(id);
                    }
                }
            }
        }
    }

    // Which messages arrived.
    let delivered: BTreeSet<Arrival> = run
        .messages
        .iter()
        .filter(|m| matches!(m.fate, Fate::Delivered(_)))
        .map(|m| (m.rel, m.from, m.to, m.send, m.row.clone()))
        .collect();

    // Protocol firings.
    for (t, round) in run.rounds.iter().enumerate() {
        let tick = Tick(u64::try_from(t).map_err(|_| internal_error!("tick overflow"))?);
        for (n, nt) in round.iter().enumerate() {
            let node = NodeId(u32::try_from(n).map_err(|_| internal_error!("node overflow"))?);
            for f in &nt.firings {
                let rule = protocol.rules.get_or_bug(f.rule)?;
                let (at_node, at_tick, clock) = match rule.kind {
                    RuleKind::Deductive => (node, tick, None),
                    RuleKind::Inductive => (node, Tick(tick.0 + 1), None),
                    RuleKind::Async => {
                        let dest = match f.head.first() {
                            Some(Value::Node(d)) => *d,
                            other => return Err(internal_error!("an async head's destination is {other:?}")),
                        };
                        if !delivered.contains(&(rule.head.rel, node, dest, tick, f.head.clone())) {
                            continue;
                        }
                        (dest, Tick(tick.0 + 1), (dest != node).then_some((node, dest, tick)))
                    }
                };
                let Some(head) = g.find(&GoalKey {
                    space: Space::Protocol,
                    rel: rule.head.rel,
                    node: Some(at_node),
                    tick: at_tick,
                    row: f.head.clone(),
                }) else {
                    // An inductive head at EOT + 1 lies outside the run.
                    continue;
                };
                let mut premises = Vec::with_capacity(f.reads.len() + f.negations.len() + 1);
                if let Some((from, to, send)) = clock {
                    premises.push(Premise::Clock { from, to, send });
                }
                for r in &f.reads {
                    let key = GoalKey {
                        space: Space::Protocol,
                        rel: r.rel,
                        node: Some(node),
                        tick,
                        row: r.row.clone(),
                    };
                    let id = g
                        .find(&key)
                        .ok_or_else(|| internal_error!("a firing read a tuple the run does not hold: {key:?}"))?;
                    premises.push(Premise::Goal(id));
                }
                for neg in &f.negations {
                    let logical = protocol_logical(artifact, neg.rel)?;
                    let id = g.negation(ProvNegRead {
                        space: Space::Protocol,
                        rel: neg.rel,
                        loc: Loc::Node(node),
                        tick,
                        pattern: neg.pattern.clone(),
                        logical,
                    });
                    premises.push(Premise::Neg(id));
                }
                g.add_firing(head, firing(Space::Protocol, f, Some(node), tick, premises));
            }
        }
    }

    // Spec goals and firings.
    if let Some(spec) = &artifact.spec {
        let eot = outcome.eot;
        let mut feeds: BTreeMap<RelId, SpecFeed> = BTreeMap::new();
        for feed in &spec.feeds {
            let rel = match *feed {
                SpecFeed::AtEot { spec, .. } | SpecFeed::AtTick { spec, .. } | SpecFeed::Crash { spec } => spec,
            };
            feeds.insert(rel, *feed);
        }
        let main_spec: BTreeMap<RelId, u32> = artifact
            .rels
            .iter()
            .enumerate()
            .filter(|(_, r)| r.kind == DedRelKind::Spec)
            .filter_map(|(i, r)| Some((r.spec?, u32::try_from(i).ok()?)))
            .collect();
        for (rel, rows) in &outcome.instance.rels {
            if feeds.contains_key(rel) {
                continue;
            }
            for row in rows {
                g.goal(
                    GoalKey {
                        space: Space::Spec,
                        rel: *rel,
                        node: None,
                        tick: eot,
                        row: row.clone(),
                    },
                    main_spec.get(rel).copied(),
                );
            }
        }
        let spec_program = spec.program.get();
        for f in &outcome.firings {
            let rule = spec_program.rules.get_or_bug(f.rule)?;
            let Some(head) = g.find(&GoalKey {
                space: Space::Spec,
                rel: rule.head.rel,
                node: None,
                tick: eot,
                row: f.head.clone(),
            }) else {
                return Err(internal_error!("a spec firing derived a tuple the spec does not hold"));
            };
            let mut premises = Vec::with_capacity(f.reads.len() + f.negations.len());
            for r in &f.reads {
                match feeds.get(&r.rel) {
                    Some(SpecFeed::Crash { .. }) => {}
                    Some(SpecFeed::AtEot { rel, .. }) => {
                        premises.push(Premise::Goal(snapshot_goal(&g, artifact, *rel, eot, &r.row)?))
                    }
                    Some(SpecFeed::AtTick { rel, tick, .. }) => {
                        premises.push(Premise::Goal(snapshot_goal(&g, artifact, *rel, *tick, &r.row)?))
                    }
                    None => {
                        let key = GoalKey {
                            space: Space::Spec,
                            rel: r.rel,
                            node: None,
                            tick: eot,
                            row: r.row.clone(),
                        };
                        let id = g
                            .find(&key)
                            .ok_or_else(|| internal_error!("a spec firing read a tuple the spec does not hold"))?;
                        premises.push(Premise::Goal(id));
                    }
                }
            }
            for neg in &f.negations {
                premises.push(spec_negation(&mut g, artifact, &feeds, &main_spec, neg, eot)?);
            }
            g.add_firing(head, firing(Space::Spec, f, None, eot, premises));
        }
    }
    Ok(g)
}

fn firing(space: Space, f: &FiringRecord, node: Option<NodeId>, tick: Tick, premises: Vec<Premise>) -> Firing {
    Firing {
        space,
        rule: f.rule,
        node,
        tick,
        kind: f.kind,
        premises,
    }
}

/// The source-level relation a negated protocol read names.
fn protocol_logical(artifact: &DedArtifact, rel: RelId) -> Result<u32, InternalError> {
    artifact
        .protocol_owner(rel)
        .map(|DedRelIdx(i)| i)
        .ok_or_else(|| internal_error!("a negated read of protocol relation {rel:?}, which no Molly relation owns"))
}

/// The protocol goal a spec input tuple copies: `row` is `[node, …]`.
fn snapshot_goal(
    g: &ProvGraph,
    artifact: &DedArtifact,
    ded: DedRelIdx,
    tick: Tick,
    row: &[Value],
) -> Result<GoalId, InternalError> {
    let rel = artifact
        .rel(ded)
        .and_then(|r| r.protocol)
        .ok_or_else(|| internal_error!("a spec feed without a protocol relation"))?;
    let (Some(Value::Node(node)), Some(rest)) = (row.first(), row.get(1..)) else {
        return Err(internal_error!("a spec input row without its node: {row:?}"));
    };
    let key = GoalKey {
        space: Space::Protocol,
        rel,
        node: Some(*node),
        tick,
        row: Arc::from(rest.to_vec()),
    };
    g.find(&key)
        .ok_or_else(|| internal_error!("a spec input copies a tuple the run does not hold: {key:?}"))
}

fn spec_negation(
    g: &mut ProvGraph,
    artifact: &DedArtifact,
    feeds: &BTreeMap<RelId, SpecFeed>,
    main_spec: &BTreeMap<RelId, u32>,
    neg: &NegRead,
    eot: Tick,
) -> Result<Premise, InternalError> {
    let logical = match feeds.get(&neg.rel) {
        Some(SpecFeed::Crash { .. }) => {
            let node = match neg.pattern.get(1) {
                Some(Some(Value::Node(n))) => Some(*n),
                Some(None) => None,
                other => return Err(internal_error!("a crash-oracle read with node column {other:?}")),
            };
            return Ok(Premise::CrashOracle { node });
        }
        Some(SpecFeed::AtEot { rel, .. } | SpecFeed::AtTick { rel, .. }) => rel.0,
        None => main_spec
            .get(&neg.rel)
            .copied()
            .or_else(|| artifact.spec_owner(neg.rel).map(|i| i.0))
            .ok_or_else(|| {
                internal_error!(
                    "a negated read of spec relation {:?}, which no Molly relation owns",
                    neg.rel
                )
            })?,
    };
    // Relation-level support reads a snapshot input at its own tick; the read itself is the spec's, at EOT.
    let tick = match feeds.get(&neg.rel) {
        Some(SpecFeed::AtTick { tick, .. }) => *tick,
        _ => eot,
    };
    let id = g.negation(ProvNegRead {
        space: Space::Spec,
        rel: neg.rel,
        loc: Loc::Global,
        tick,
        pattern: neg.pattern.clone(),
        logical,
    });
    Ok(Premise::Neg(id))
}

/// How the tuples of a `.ded` program's relations come about, for tuple-level negative support.
pub struct DedRules<'a> {
    artifact: &'a DedArtifact,
    feeds: BTreeMap<RelId, SpecFeed>,
}

impl<'a> DedRules<'a> {
    pub fn new(artifact: &'a DedArtifact) -> DedRules<'a> {
        let mut feeds = BTreeMap::new();
        if let Some(spec) = &artifact.spec {
            for feed in &spec.feeds {
                let rel = match *feed {
                    SpecFeed::AtEot { spec, .. } | SpecFeed::AtTick { spec, .. } | SpecFeed::Crash { spec } => spec,
                };
                feeds.insert(rel, *feed);
            }
        }
        DedRules { artifact, feeds }
    }
}

impl crate::hazard::Rules for DedRules<'_> {
    fn nodes(&self) -> u32 {
        u32::try_from(self.artifact.nodes.len()).unwrap_or(u32::MAX)
    }

    fn constant(&self, space: Space, id: blossom_base::ConstId) -> Option<&Value> {
        match space {
            Space::Protocol => self.artifact.protocol.get().consts.get(id),
            Space::Spec => self.artifact.spec.as_ref().and_then(|s| s.program.get().consts.get(id)),
        }
    }

    fn origin(&self, space: Space, rel: RelId) -> crate::hazard::Origin<'_> {
        use crate::hazard::Origin;
        let program = match space {
            Space::Protocol => self.artifact.protocol.get(),
            Space::Spec => match self.feeds.get(&rel) {
                Some(SpecFeed::Crash { .. }) => return Origin::Crash,
                Some(SpecFeed::AtEot { rel: ded, .. }) => {
                    return match self.artifact.rel(*ded).and_then(|r| r.protocol) {
                        Some(protocol) => Origin::Snapshot { protocol, tick: None },
                        None => Origin::Input,
                    };
                }
                Some(SpecFeed::AtTick { rel: ded, tick, .. }) => {
                    return match self.artifact.rel(*ded).and_then(|r| r.protocol) {
                        Some(protocol) => Origin::Snapshot {
                            protocol,
                            tick: Some(*tick),
                        },
                        None => Origin::Input,
                    };
                }
                None => match &self.artifact.spec {
                    Some(s) => s.program.get(),
                    None => return Origin::Input,
                },
            },
        };
        match program.rels.get(rel).map(|r| &r.class) {
            Some(RelClass::Idb | RelClass::Channel(_)) => {}
            _ => return Origin::Input,
        }
        let mut deductive = Vec::new();
        let mut inductive = Vec::new();
        let mut asynchronous = Vec::new();
        for rule in program.rules.iter().filter(|r| r.head.rel == rel) {
            match rule.kind {
                RuleKind::Deductive => deductive.push(rule),
                RuleKind::Inductive => inductive.push(rule),
                RuleKind::Async => asynchronous.push(rule),
            }
        }
        Origin::Rules {
            deductive,
            inductive,
            asynchronous,
        }
    }
}
