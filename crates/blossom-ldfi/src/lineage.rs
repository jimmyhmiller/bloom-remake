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
//!
//! A lattice-valued relation holds one row per cell, the join of every value derived for it (SEM-100), so its goal
//! is supported like an aggregate row (ARCHITECTURE §8.3): by one firing that needs every contribution (the reads of
//! every contributing firing, conjunctively) and that no new contribution appears (a group premise per contributing
//! rule). A firing that read a superseded value of a cell while its stratum was still growing it is stale: the cell
//! only grew, so its contribution is below the one derived from the final value, and it is left out. A client
//! session's message (LANGUAGE §18.4) is a leaf; a reply to a session leaves the deployment and supports nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_artifact::sim::{LogicalIdx, LogicalKind, SimArtifact, SpecFeed};
use blossom_base::{InternalError, RelId, internal_error};
use blossom_ir::core::{EventSource, RelClass, RuleKind};
use blossom_ir::obs::{FiringKind, FiringRecord, NegRead};
use blossom_prov::{AggGroup, Loc, NegRead as ProvNegRead};
use blossom_prov::{By, Firing, GoalId, GoalKey, Premise, ProvGraph, RuntimeAct, Space};
use blossom_sim::spec::Outcome;
use blossom_sim::{Fate, SyncRun};
use blossom_value::{
    Value,
    time::{NodeId, Tick},
};

use crate::LdfiError;

/// A delivered message: relation, sender, receiver, send tick, tuple.
type Arrival = (RelId, NodeId, NodeId, Tick, Arc<[Value]>);

/// One firing's contribution to a lattice cell: its rule and premises.
struct Contribution {
    rule: blossom_base::RuleId,
    premises: Vec<Premise>,
}

/// Builds the provenance graph of `run` and its `outcome`.
pub fn build(artifact: &SimArtifact, run: &SyncRun, outcome: &Outcome) -> Result<ProvGraph, LdfiError> {
    let protocol = artifact.protocol.get();
    // Lineage reads node values as node ids; a keyed member is a role and a key (docs/design/KEYED.md §5).
    if let Some(r) = protocol.keyed_roles().next() {
        return Err(blossom_base::unimplemented_error!(
            "TEST-020",
            "lineage over a program with a keyed role (`{}`); LDFI by enumeration runs it",
            r.name
        )
        .into());
    }
    // Lattice-valued relations and the columns that identify a cell (key and payload).
    let cells: BTreeMap<RelId, Vec<usize>> = protocol
        .rels
        .iter_enumerated()
        .filter(|(_, r)| !r.schema.lattice.is_empty())
        .map(|(id, r)| {
            let mut ident: Vec<usize> = r
                .schema
                .key
                .iter()
                .chain(&r.schema.payload)
                .map(|c| c.index())
                .collect();
            ident.sort_unstable();
            (id, ident)
        })
        .collect();
    // A node that halts runs no later tick and its state is gone; faults can make a node halt that did not, which
    // the hazard encoding does not model yet.
    if let Some(halt) = artifact.halt
        && protocol.rules.iter().any(|r| r.head.rel == halt)
    {
        return Err(blossom_base::unimplemented_error!(
            "LANG-052",
            "LDFI over a program that can `halt` (a halt that faults cause is not a modelled hazard yet)"
        )
        .into());
    }
    for rule in protocol.rules.iter() {
        if rule.kind == RuleKind::Async && cells.contains_key(&rule.head.rel) {
            return Err(blossom_base::unimplemented_error!(
                "LANG-137",
                "LDFI over a program that sends lattice values on a channel"
            )
            .into());
        }
    }
    let ident_of = |rel: RelId, row: &[Value]| -> Option<Vec<Value>> {
        cells
            .get(&rel)
            .map(|cols| cols.iter().filter_map(|c| row.get(*c).cloned()).collect())
    };
    let mut contributions: BTreeMap<(RelId, NodeId, Tick, Vec<Value>), Vec<Contribution>> = BTreeMap::new();
    let main_protocol: BTreeMap<RelId, u32> = artifact
        .rels
        .iter()
        .enumerate()
        .filter_map(|(i, r)| Some((r.protocol?, u32::try_from(i).ok()?)))
        .collect();
    let mut g = ProvGraph::new();
    // What the streams carried between nodes, round by round: where a lost message resets a connection or fails a dial.
    for c in &run.connections {
        for (from, to, tick) in &c.traffic {
            g.crossing(*from, *to, *tick);
        }
    }
    for (from, to, tick) in &run.dials {
        g.crossing(*from, *to, *tick);
    }

    // Protocol goals. A timer's firing is not a leaf: a restart starts the timer's count again, and a guarded timer
    // fires only while its guard holds.
    let mut timer_goals: Vec<(GoalId, NodeId, Tick, Option<RelId>)> = Vec::new();
    for (t, round) in run.rounds.iter().enumerate() {
        let tick = Tick(u64::try_from(t).map_err(|_| internal_error!("tick overflow"))?);
        for (n, nt) in round.iter().enumerate() {
            let node = NodeId(u32::try_from(n).map_err(|_| internal_error!("node overflow"))?);
            for (rel, rows) in &nt.instance.rels {
                let class = protocol.rels.get(*rel).map(|r| &r.class);
                let timer = match class {
                    Some(RelClass::Event(EventSource::Timer(t))) => Some(t.guard),
                    _ => None,
                };
                // A stream event comes from the host's connections ([`blossom_sim::fabric`]): supported below.
                let stream = matches!(class, Some(RelClass::Event(EventSource::Stream(_))));
                let leaf = timer.is_none() && !stream && matches!(class, Some(RelClass::Event(_) | RelClass::Static));
                for row in rows {
                    let leaf = leaf || nt.ingress.iter().any(|m| m.rel == *rel && m.row == *row);
                    let id = g.goal(
                        GoalKey {
                            space: Space::Protocol,
                            rel: *rel,
                            node: Some(node),
                            tick,
                            row: row.clone(),
                        },
                        main_protocol.get(rel).copied(),
                    )?;
                    if leaf {
                        g.set_leaf(id);
                    }
                    if let Some(guard) = timer
                        && nt.ran
                    {
                        timer_goals.push((id, node, tick, guard));
                    }
                }
            }
        }
    }

    // Timer firings: each needs the node not to have restarted by then, and a guarded one some tuple of its guard at
    // the end of the node's previous tick.
    for (goal, node, tick, guard) in timer_goals {
        // Since the start of the incarnation the firing belongs to (the run's own restart of the node, if before).
        let from = run
            .faults
            .restarts
            .get(&node)
            .copied()
            .filter(|r| *r <= tick)
            .unwrap_or(Tick(0));
        let not_restarted = Premise::NotRestarted { node, from, tick };
        let runtime = |premises: Vec<Premise>| Firing {
            space: Space::Protocol,
            by: By::Runtime(RuntimeAct::Timer),
            node: Some(node),
            tick,
            kind: FiringKind::Rule,
            premises,
        };
        match guard {
            None => {
                g.add_firing(goal, runtime(vec![not_restarted]))?;
            }
            Some(guard) => {
                let before = tick
                    .prev()
                    .ok_or_else(|| internal_error!("a guarded timer fired in the first tick"))?;
                let held: Vec<GoalId> = g.goals_at(Space::Protocol, guard, Some(node), before).to_vec();
                if held.is_empty() {
                    return Err(internal_error!("a guarded timer fired at {tick:?} without its guard before").into());
                }
                for h in held {
                    g.add_firing(goal, runtime(vec![Premise::Goal(h), not_restarted]))?;
                }
            }
        }
    }

    // A crashed node's frozen ticks (CR-20): each tuple is the one it held at the previous tick.
    if artifact.profile.frozen() {
        for (t, round) in run.rounds.iter().enumerate().skip(1) {
            let tick = Tick(u64::try_from(t).map_err(|_| internal_error!("tick overflow"))?);
            for (n, nt) in round.iter().enumerate() {
                let node = NodeId(u32::try_from(n).map_err(|_| internal_error!("node overflow"))?);
                if nt.ran || !run.faults.crashed(node, tick) {
                    continue;
                }
                for (rel, rows) in &nt.instance.rels {
                    for row in rows {
                        let key = |tick: Tick| GoalKey {
                            space: Space::Protocol,
                            rel: *rel,
                            node: Some(node),
                            tick,
                            row: row.clone(),
                        };
                        let (Some(goal), Some(prev)) = (g.find(&key(tick)), g.find(&key(Tick(tick.0 - 1)))) else {
                            return Err(
                                internal_error!("a frozen tuple without its previous tick: {:?}", key(tick)).into(),
                            );
                        };
                        g.set_frozen(goal, prev);
                    }
                }
            }
        }
    }

    // What arrived on time, between two nodes: what a delay can make arrive later.
    for m in &run.messages {
        if m.from != m.to && m.fate == Fate::Delivered(Tick(m.send.0 + 1)) {
            g.sent_on_time(m.rel, m.from, m.to, m.send, m.row.clone());
        }
    }
    // Which messages arrived, and when (the round after their send, or later when delayed).
    let delivered: BTreeMap<Arrival, Tick> = run
        .messages
        .iter()
        .filter_map(|m| match m.fate {
            Fate::Delivered(at) => Some(((m.rel, m.from, m.to, m.send, m.row.clone()), at)),
            _ => None,
        })
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
                    // A request to the host (a stream's write, close, dial, pause, resume) at the requesting node.
                    RuleKind::Async
                        if matches!(
                            protocol.rels.get(rule.head.rel).map(|r| &r.class),
                            Some(RelClass::HostOut(_))
                        ) =>
                    {
                        g.goal(
                            GoalKey {
                                space: Space::Protocol,
                                rel: rule.head.rel,
                                node: Some(node),
                                tick,
                                row: f.head.clone(),
                            },
                            main_protocol.get(&rule.head.rel).copied(),
                        )?;
                        (node, tick, None)
                    }
                    RuleKind::Async => {
                        let dest = match f.head.first() {
                            Some(Value::Node(d)) => *d,
                            // A reply to a client session supports nothing in the deployment.
                            Some(Value::Session(_)) => continue,
                            other => return Err(internal_error!("an async head's destination is {other:?}").into()),
                        };
                        let Some(at) = delivered.get(&(rule.head.rel, node, dest, tick, f.head.clone())) else {
                            continue;
                        };
                        (dest, *at, (dest != node).then_some((node, dest, tick)))
                    }
                };
                // A restarted node reloads the state its last round carried out: a durable head of an inductive firing
                // in the round before its crash holds again in its restart tick (the crash tick never ran).
                let at_tick = match (run.faults.crashes.get(&node), run.faults.restarts.get(&node)) {
                    (Some(c), Some(r))
                        if rule.kind == RuleKind::Inductive
                            && *c == at_tick
                            && protocol.rels.get(rule.head.rel).is_some_and(|x| x.durable) =>
                    {
                        *r
                    }
                    _ => at_tick,
                };
                // An inductive head at EOT + 1 lies outside the run.
                if at_tick.0 >= run.rounds.len() as u64 {
                    continue;
                }
                let cell = ident_of(rule.head.rel, &f.head);
                let head = g.find(&GoalKey {
                    space: Space::Protocol,
                    rel: rule.head.rel,
                    node: Some(at_node),
                    tick: at_tick,
                    row: f.head.clone(),
                });
                if head.is_none() && cell.is_none() {
                    // The head is not in the run: at a node frozen by a crash at the head's tick.
                    continue;
                }
                let stale = f.reads.iter().any(|r| {
                    cells.contains_key(&r.rel)
                        && g.find(&GoalKey {
                            space: Space::Protocol,
                            rel: r.rel,
                            node: Some(node),
                            tick,
                            row: r.row.clone(),
                        })
                        .is_none()
                });
                if stale {
                    continue;
                }
                let mut premises = Vec::with_capacity(f.reads.len() + f.negations.len() + 2);
                if let Some((from, to, send)) = clock {
                    premises.push(Premise::Clock { from, to, send });
                    // It arrives when it did: a delay past then (asynchronous model) falsifies the firing here.
                    premises.push(Premise::Arrives {
                        from,
                        to,
                        send,
                        via: blossom_prov::Via::Channel(rule.head.rel),
                        by: at_tick,
                    });
                }
                // Under the frozen crash view a crashed node fires nothing; its persisted state is its frozen copy, which
                // a restart keeps only for durable relations.
                if artifact.profile.frozen() && !is_frame(protocol, rule) {
                    premises.push(Premise::Alive { node, tick });
                }
                if is_frame(protocol, rule) && !protocol.rels.get(rule.head.rel).is_some_and(|r| r.durable) {
                    premises.push(Premise::NoRestart {
                        node: at_node,
                        tick: at_tick,
                    });
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
                    })?;
                    premises.push(Premise::Neg(id));
                }
                if f.kind == FiringKind::Aggregate {
                    let id =
                        g.aggregate_group(aggregate_group(Space::Protocol, rule, Loc::Node(node), tick, &f.head))?;
                    premises.push(Premise::Aggregate(id));
                }
                match (cell, head) {
                    (Some(ident), _) => contributions
                        .entry((rule.head.rel, at_node, at_tick, ident))
                        .or_default()
                        .push(Contribution { rule: f.rule, premises }),
                    (None, Some(head)) => {
                        g.add_firing(head, firing(Space::Protocol, f, Some(node), tick, premises))?;
                    }
                    (None, None) => {}
                }
            }
        }
    }

    stream_supports(&mut g, protocol, run)?;

    // Lattice cells: one firing per cell, needing every contribution and that no other appears.
    for ((rel, node, tick, ident), contribs) in &contributions {
        let row = run.node_tick(*tick, *node).and_then(|nt| {
            nt.instance
                .rows(*rel)
                .find(|r| ident_of(*rel, r).as_ref() == Some(ident))
                .cloned()
        });
        let Some(row) = row else {
            // Every contribution was ⊥ (SEM-101): the cell is absent.
            continue;
        };
        let head = g
            .find(&GoalKey {
                space: Space::Protocol,
                rel: *rel,
                node: Some(*node),
                tick: *tick,
                row: row.clone(),
            })
            .ok_or_else(|| internal_error!("a lattice cell the run holds has no goal"))?;
        let mut premises: BTreeSet<Premise> = contribs.iter().flat_map(|c| c.premises.iter().copied()).collect();
        let key: Vec<Option<Value>> = row
            .iter()
            .enumerate()
            .map(|(c, v)| cells.get(rel).is_some_and(|cols| cols.contains(&c)).then(|| v.clone()))
            .collect();
        for r in protocol.rules.iter().filter(|r| r.head.rel == *rel) {
            let at = match r.kind {
                RuleKind::Deductive => Some(*tick),
                RuleKind::Inductive => tick.0.checked_sub(1).map(Tick),
                RuleKind::Async => None,
            };
            if let Some(at) = at {
                let id = g.aggregate_group(AggGroup {
                    space: Space::Protocol,
                    rule: r.id,
                    loc: Loc::Node(*node),
                    tick: at,
                    key: key.clone(),
                })?;
                premises.insert(Premise::Aggregate(id));
            }
        }
        let rule = contribs
            .first()
            .map(|c| c.rule)
            .ok_or_else(|| internal_error!("a lattice cell without contributions"))?;
        g.add_firing(
            head,
            Firing {
                space: Space::Protocol,
                by: By::Rule(rule),
                node: Some(*node),
                tick: *tick,
                kind: FiringKind::Aggregate,
                premises: premises.into_iter().collect(),
            },
        )?;
    }

    // Spec goals and firings.
    if let Some(spec) = &artifact.spec {
        let eot = outcome.eot;
        let mut feeds: BTreeMap<RelId, SpecFeed> = BTreeMap::new();
        for feed in &spec.feeds {
            feeds.insert(feed.spec_rel(), *feed);
        }
        let main_spec: BTreeMap<RelId, u32> = artifact
            .rels
            .iter()
            .enumerate()
            .filter(|(_, r)| r.kind == LogicalKind::Spec)
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
                )?;
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
                return Err(internal_error!("a spec firing derived a tuple the spec does not hold").into());
            };
            let mut premises = Vec::with_capacity(f.reads.len() + f.negations.len());
            for r in &f.reads {
                match feeds.get(&r.rel) {
                    // crashed(n): a crashed node stays crashed in every superset of the run's faults.
                    Some(SpecFeed::Crashed { .. }) => {}
                    // crash(Observer, Node, Time): lost if the node crashes earlier.
                    Some(SpecFeed::Crash { .. }) => match (r.row.get(1), r.row.get(2)) {
                        (Some(Value::Node(node)), Some(time)) => premises.push(Premise::CrashPresent {
                            node: *node,
                            time: crash_time(time)?,
                        }),
                        other => return Err(internal_error!("a crash-oracle tuple {other:?}").into()),
                    },
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
            if f.kind == FiringKind::Aggregate {
                let id = g.aggregate_group(aggregate_group(Space::Spec, rule, Loc::Global, eot, &f.head))?;
                premises.push(Premise::Aggregate(id));
            }
            g.add_firing(head, firing(Space::Spec, f, None, eot, premises))?;
        }
    }
    Ok(g)
}

/// The supports of the run's stream events (`blossom_sim::fabric`): each needs the program requests and stream
/// events it comes from (the writes whose bytes it carries, the resume that let them through, the close it reports,
/// the dial it answers), and its connection: opened at its end, no message lost in a round traffic crossed it up to
/// the event's causes (a lost message resets it), both ends up meanwhile (a crash resets it, and clears the inbox the
/// event waits in), and for bytes the connection's previous bytes at its end (a gap holds later bytes back).
fn stream_supports(g: &mut ProvGraph, protocol: &blossom_ir::core::Program, run: &SyncRun) -> Result<(), LdfiError> {
    use blossom_ir::core::StreamEvent as Kind;
    let kind_of = |rel: RelId| match protocol.rels.get(rel).map(|r| &r.class) {
        Some(RelClass::Event(EventSource::Stream(k))) => Some(*k),
        _ => None,
    };
    let find = |g: &ProvGraph, rel: RelId, node: NodeId, tick: Tick, row: &Arc<[Value]>| {
        g.find(&GoalKey {
            space: Space::Protocol,
            rel,
            node: Some(node),
            tick,
            row: row.clone(),
        })
    };
    // The goal of each connection end's latest bytes.
    let mut last_data: BTreeMap<(NodeId, blossom_value::value::ConnId), GoalId> = BTreeMap::new();
    for (t, round) in run.rounds.iter().enumerate() {
        let tick = Tick(u64::try_from(t).map_err(|_| internal_error!("tick overflow"))?);
        for (n, nt) in round.iter().enumerate() {
            let node = NodeId(u32::try_from(n).map_err(|_| internal_error!("node overflow"))?);
            for e in &nt.streams {
                let goal = find(g, e.rel, node, tick, &e.row)
                    .ok_or_else(|| internal_error!("a stream event the run does not hold: {:?}", e.row))?;
                let mut premises = Vec::new();
                for c in &e.causes {
                    match c {
                        blossom_sim::fabric::Cause::Request { node, tick, rel, row }
                        | blossom_sim::fabric::Cause::Taken { node, tick, rel, row } => {
                            let id = find(g, *rel, *node, *tick, row).ok_or_else(|| {
                                internal_error!("a stream event's cause is not in the lineage: {row:?}")
                            })?;
                            premises.push(Premise::Goal(id));
                        }
                        // A reset or a failed dial by a fault: the fault stays in every superset of the run's faults.
                        blossom_sim::fabric::Cause::Crash { .. } | blossom_sim::fabric::Cause::Omission { .. } => {}
                    }
                }
                if let Some(conn_ref) = e.conn {
                    let c = run
                        .connections
                        .get(conn_ref.pipe)
                        .ok_or_else(|| internal_error!("a stream event on an unknown connection"))?;
                    let Some(Value::Conn(conn)) = e.row.first() else {
                        return Err(internal_error!("a stream event {:?} without its connection", e.row).into());
                    };
                    let (mine, peer) = match c.ends {
                        [a, b] if a.node == node && a.conn == *conn => (a, b),
                        [a, b] if b.node == node && b.conn == *conn => (b, a),
                        _ => return Err(internal_error!("a stream event on a connection without its end").into()),
                    };
                    let [dialer, acceptor] = c.ends;
                    let opened = c.opened;
                    if kind_of(e.rel) == Some(Kind::Opened) {
                        if dialer.node != acceptor.node {
                            premises.push(Premise::Clock {
                                from: dialer.node,
                                to: acceptor.node,
                                send: c.dialed,
                            });
                            // The dial arrives when it did (the asynchronous model: a delay opens it later).
                            premises.push(Premise::Arrives {
                                from: dialer.node,
                                to: acceptor.node,
                                send: c.dialed,
                                via: blossom_prov::Via::Streams,
                                by: tick,
                            });
                        }
                        premises.push(Premise::Up {
                            node: acceptor.node,
                            from: opened,
                            to: opened,
                        });
                    } else {
                        let opened_goal = g
                            .goals_at(Space::Protocol, mine.opened, Some(node), opened)
                            .iter()
                            .copied()
                            .find(|id| {
                                g.get(*id)
                                    .is_some_and(|goal| goal.key.row.first() == Some(&Value::Conn(*conn)))
                            })
                            .ok_or_else(|| internal_error!("connection {conn:?} has no opened event at its end"))?;
                        premises.push(Premise::Goal(opened_goal));
                        for (from, to, send) in &c.traffic {
                            if *send <= conn_ref.as_of {
                                premises.push(Premise::Clock {
                                    from: *from,
                                    to: *to,
                                    send: *send,
                                });
                            }
                            // Every flight the peer sent on the connection before the event arrives by it: one delayed
                            // past it holds back what follows (the asynchronous model; a stream keeps its order).
                            if *from == peer.node && *to == node && *send < tick {
                                premises.push(Premise::Arrives {
                                    from: *from,
                                    to: *to,
                                    send: *send,
                                    via: blossom_prov::Via::Streams,
                                    by: tick,
                                });
                            }
                        }
                        if conn_ref.as_of >= opened {
                            premises.push(Premise::Up {
                                node: peer.node,
                                from: opened,
                                to: conn_ref.as_of,
                            });
                        }
                        if let Some(before) = tick.prev()
                            && before >= opened
                        {
                            premises.push(Premise::Up {
                                node,
                                from: opened,
                                to: before,
                            });
                        }
                        if kind_of(e.rel) == Some(Kind::Data) {
                            if let Some(prev) = last_data.get(&(node, *conn)) {
                                premises.push(Premise::Goal(*prev));
                            }
                            last_data.insert((node, *conn), goal);
                        }
                    }
                }
                premises.sort();
                premises.dedup();
                g.add_firing(
                    goal,
                    Firing {
                        space: Space::Protocol,
                        by: By::Runtime(RuntimeAct::Stream),
                        node: Some(node),
                        tick,
                        kind: FiringKind::Rule,
                        premises,
                    },
                )?;
            }
        }
    }
    Ok(())
}

/// Whether `rule` is a table's frame rule or a lattice's identity rule (the persistence of state), and not another
/// rule of the same construct (a `while` table's guard).
fn is_frame(program: &blossom_ir::core::Program, rule: &blossom_ir::core::Rule) -> bool {
    use blossom_ir::core::Persistence;
    program.rels.get(rule.head.rel).is_some_and(|r| match r.persistence {
        Persistence::Frame { rule: frame, .. } | Persistence::Identity { rule: frame } => frame == rule.id,
        _ => false,
    })
}

fn firing(space: Space, f: &FiringRecord, node: Option<NodeId>, tick: Tick, premises: Vec<Premise>) -> Firing {
    Firing {
        space,
        by: By::Rule(f.rule),
        node,
        tick,
        kind: f.kind,
        premises,
    }
}

/// The source-level relation a negated protocol read names.
fn protocol_logical(artifact: &SimArtifact, rel: RelId) -> Result<u32, InternalError> {
    artifact
        .protocol_owner(rel)
        .map(|LogicalIdx(i)| i)
        .ok_or_else(|| internal_error!("a negated read of protocol relation {rel:?}, which no Molly relation owns"))
}

/// The protocol goal a spec input tuple copies: `row` is `[node, …]`.
fn snapshot_goal(
    g: &ProvGraph,
    artifact: &SimArtifact,
    ded: LogicalIdx,
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
    if let Some(id) = g.find(&key) {
        return Ok(id);
    }
    // A blob column holds the blob's reference in the spec's copy.
    g.goals_at(Space::Protocol, rel, Some(*node), tick)
        .iter()
        .copied()
        .find(|id| {
            g.get(*id).is_some_and(|goal| {
                goal.key.row.len() == rest.len()
                    && goal
                        .key
                        .row
                        .iter()
                        .zip(rest)
                        .all(|(v, w)| blossom_sim::spec::trace_value(v) == *w)
            })
        })
        .ok_or_else(|| internal_error!("a spec input copies a tuple the run does not hold: {key:?}"))
}

fn spec_negation(
    g: &mut ProvGraph,
    artifact: &SimArtifact,
    feeds: &BTreeMap<RelId, SpecFeed>,
    main_spec: &BTreeMap<RelId, u32>,
    neg: &NegRead,
    eot: Tick,
) -> Result<Premise, InternalError> {
    let logical = match feeds.get(&neg.rel) {
        Some(SpecFeed::Crashed { .. }) => {
            let node = match neg.pattern.first() {
                Some(Some(Value::Node(n))) => Some(*n),
                Some(None) => None,
                other => return Err(internal_error!("a crashed-oracle read with node column {other:?}")),
            };
            return Ok(Premise::CrashAbsent { node, time: None });
        }
        Some(SpecFeed::Crash { .. }) => {
            let node = match neg.pattern.get(1) {
                Some(Some(Value::Node(n))) => Some(*n),
                Some(None) => None,
                other => return Err(internal_error!("a crash-oracle read with node column {other:?}")),
            };
            let time = match neg.pattern.get(2) {
                Some(Some(t)) => Some(crash_time(t)?),
                Some(None) => None,
                other => return Err(internal_error!("a crash-oracle read with time column {other:?}")),
            };
            return Ok(Premise::CrashAbsent { node, time });
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
    })?;
    Ok(Premise::Neg(id))
}

/// A crash oracle's time column as a tick (Molly's INT, an `i64`).
fn crash_time(v: &Value) -> Result<Tick, InternalError> {
    match v {
        Value::Int(blossom_value::value::IntValue::I64(t)) => u64::try_from(*t)
            .map(Tick)
            .map_err(|_| internal_error!("a negative crash time {t}")),
        other => Err(internal_error!("a crash time {other:?}")),
    }
}

/// An aggregate firing's group: its head with the aggregate columns open.
fn aggregate_group(space: Space, rule: &blossom_ir::core::Rule, loc: Loc, tick: Tick, head: &[Value]) -> AggGroup {
    let key = rule
        .head
        .args
        .iter()
        .zip(head)
        .map(|(arg, v)| match arg {
            blossom_ir::core::HeadArg::Agg(_) => None,
            blossom_ir::core::HeadArg::Term(_) => Some(v.clone()),
        })
        .collect();
    AggGroup {
        space,
        rule: rule.id,
        loc,
        tick,
        key,
    }
}

/// How the tuples of a `.ded` program's relations come about, for tuple-level negative support.
pub struct ArtifactRules<'a> {
    artifact: &'a SimArtifact,
    nodes: u32,
    /// Per space, dense by relation: what the encoder asks about a relation for every tuple it reasons about, so
    /// computed once.
    protocol: Vec<RelFacts<'a>>,
    spec: Vec<RelFacts<'a>>,
}

/// What the encoder asks about one relation.
struct RelFacts<'a> {
    from: From,
    /// The rules that derive it: deductive, inductive and asynchronous.
    rules: [Vec<&'a blossom_ir::core::Rule>; 3],
    lattice: Vec<usize>,
    arity: usize,
    durable: bool,
}

/// Where a relation's tuples come from ([`crate::hazard::Origin`] without the rules it borrows).
#[derive(Clone, Copy)]
enum From {
    Rules,
    Input,
    Snapshot {
        protocol: RelId,
        tick: Option<Tick>,
    },
    Crash,
    Crashed,
    Restart,
    Timer {
        guard: Option<RelId>,
    },
    Stream,
    /// A spec copy of a relation without a protocol relation (reported when asked).
    NoProtocol,
}

impl<'a> ArtifactRules<'a> {
    pub fn new(artifact: &'a SimArtifact) -> Result<ArtifactRules<'a>, InternalError> {
        let nodes = u32::try_from(artifact.nodes.len()).map_err(|_| internal_error!("too many nodes"))?;
        let (mut protocol, mut spec_rels) = (Vec::new(), Vec::new());
        let programs = std::iter::once((Space::Protocol, artifact.protocol.get()))
            .chain(artifact.spec.as_ref().map(|s| (Space::Spec, s.program.get())));
        for (space, program) in programs {
            let facts = match space {
                Space::Protocol => &mut protocol,
                Space::Spec => &mut spec_rels,
            };
            for rel in program.rels.iter() {
                let from = match &rel.class {
                    RelClass::Idb | RelClass::Channel(_) => From::Rules,
                    RelClass::Event(EventSource::Boot | EventSource::Recovered) if space == Space::Protocol => {
                        From::Restart
                    }
                    RelClass::Event(EventSource::Timer(t)) if space == Space::Protocol => {
                        From::Timer { guard: t.guard }
                    }
                    RelClass::Event(EventSource::Stream(_)) if space == Space::Protocol => From::Stream,
                    _ => From::Input,
                };
                facts.push(RelFacts {
                    from,
                    rules: Default::default(),
                    lattice: rel.schema.lattice.iter().map(|(c, _)| c.index()).collect(),
                    arity: rel.schema.cols.len(),
                    durable: space == Space::Protocol && rel.durable,
                });
            }
            for rule in program.rules.iter() {
                let Some(f) = facts.get_mut(rule.head.rel.index()) else {
                    return Err(internal_error!("a rule's head {:?} is not a relation", rule.head.rel));
                };
                let [deductive, inductive, asynchronous] = &mut f.rules;
                match rule.kind {
                    RuleKind::Deductive => deductive.push(rule),
                    RuleKind::Inductive => inductive.push(rule),
                    RuleKind::Async => asynchronous.push(rule),
                }
            }
        }
        if let Some(spec) = &artifact.spec {
            let snapshot_of = |ded: LogicalIdx| match artifact.rel(ded).and_then(|r| r.protocol) {
                Some(protocol) => From::Snapshot { protocol, tick: None },
                None => From::NoProtocol,
            };
            for feed in &spec.feeds {
                let from = match *feed {
                    SpecFeed::Crash { .. } => From::Crash,
                    SpecFeed::Crashed { .. } => From::Crashed,
                    SpecFeed::AtEot { rel, .. } => snapshot_of(rel),
                    SpecFeed::AtTick { rel, tick, .. } => match snapshot_of(rel) {
                        From::Snapshot { protocol, .. } => From::Snapshot {
                            protocol,
                            tick: Some(tick),
                        },
                        other => other,
                    },
                };
                let Some(f) = spec_rels.get_mut(feed.spec_rel().index()) else {
                    return Err(internal_error!(
                        "a spec feed's relation {:?} is not a relation",
                        feed.spec_rel()
                    ));
                };
                f.from = from;
            }
        }
        Ok(ArtifactRules {
            artifact,
            nodes,
            protocol,
            spec: spec_rels,
        })
    }

    fn facts(&self, space: Space, rel: RelId) -> Option<&RelFacts<'a>> {
        match space {
            Space::Protocol => self.protocol.get(rel.index()),
            Space::Spec => self.spec.get(rel.index()),
        }
    }
}

impl crate::hazard::Rules for ArtifactRules<'_> {
    fn nodes(&self) -> u32 {
        self.nodes
    }

    fn rule(&self, space: Space, id: blossom_base::RuleId) -> Option<&blossom_ir::core::Rule> {
        match space {
            Space::Protocol => self.artifact.protocol.get().rules.get(id),
            Space::Spec => self.artifact.spec.as_ref().and_then(|s| s.program.get().rules.get(id)),
        }
    }

    fn logical(&self, space: Space, rel: RelId) -> Option<u32> {
        match space {
            Space::Protocol => self.artifact.protocol_owner(rel).map(|i| i.0),
            Space::Spec => self.artifact.spec_owner(rel).map(|i| i.0),
        }
    }

    fn constant(&self, space: Space, id: blossom_base::ConstId) -> Option<&Value> {
        match space {
            Space::Protocol => self.artifact.protocol.get().consts.get(id),
            Space::Spec => self.artifact.spec.as_ref().and_then(|s| s.program.get().consts.get(id)),
        }
    }

    fn lattice_cols(&self, space: Space, rel: RelId) -> &[usize] {
        self.facts(space, rel).map_or(&[], |f| f.lattice.as_slice())
    }

    fn durable(&self, space: Space, rel: RelId) -> bool {
        self.facts(space, rel).is_some_and(|f| f.durable)
    }

    fn host_requests(&self) -> Vec<RelId> {
        self.artifact
            .protocol
            .get()
            .rels
            .iter_enumerated()
            .filter(|(_, r)| matches!(r.class, RelClass::HostOut(_)))
            .map(|(id, _)| id)
            .collect()
    }

    fn stream_events(&self) -> Vec<RelId> {
        self.artifact
            .protocol
            .get()
            .rels
            .iter_enumerated()
            .filter(|(_, r)| matches!(r.class, RelClass::Event(EventSource::Stream(_))))
            .map(|(id, _)| id)
            .collect()
    }

    fn arity(&self, space: Space, rel: RelId) -> usize {
        self.facts(space, rel).map_or(0, |f| f.arity)
    }

    fn origin(&self, space: Space, rel: RelId) -> Result<crate::hazard::Origin<'_>, InternalError> {
        use crate::hazard::Origin;
        let Some(f) = self.facts(space, rel) else {
            return Err(internal_error!("unknown relation {rel:?} of the {space:?} program"));
        };
        Ok(match f.from {
            From::Rules => {
                let [deductive, inductive, asynchronous] = &f.rules;
                Origin::Rules {
                    deductive,
                    inductive,
                    asynchronous,
                }
            }
            From::Input => Origin::Input,
            From::Snapshot { protocol, tick } => Origin::Snapshot { protocol, tick },
            From::Crash => Origin::Crash,
            From::Crashed => Origin::Crashed,
            From::Restart => Origin::Restart,
            From::Timer { guard } => Origin::Timer { guard },
            From::Stream => Origin::Stream,
            From::NoProtocol => {
                return Err(internal_error!(
                    "a spec snapshot of a relation without a protocol relation"
                ));
            }
        })
    }
}
