//! Exhaustive certification: every admissible fault schedule, stepped tick by tick with identical states merged.
//!
//! Lineage-driven search finds counterexamples quickly, but to certify a correct protocol it must exhaust its
//! hypotheses, and when negated reads make most faults look relevant that is close to enumerating every fault set,
//! one full run each. This search decides the same question (is there an admissible fault schedule under which a
//! failure-free `post` tuple is lost while `pre` holds it?) by exploring schedules breadth-first: for each crash
//! schedule allowed by the spec, every node steps tick by tick; at each tick the messages that may be lost (between
//! two nodes, sent before EFF, by a sender that has not crashed) are lost in every combination, and successor states
//! that are equal (every node's carried state, every node's inbox, and the snapshots the spec reads) are merged, so
//! schedules that lead to the same state share their future. At EOT each state is judged by Molly's oracle. It is
//! sound and complete for the failure spec: every admissible schedule leads to some state of the last frontier.

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::internal_error;
use blossom_oracle::{Delivery, Instance, Row};
use blossom_sim::ded::{DedSim, is_good};
use blossom_sim::{FaultSchedule, Omission};
use blossom_value::time::{NodeId, Tick};

use crate::LdfiError;
use crate::faults::{FailureSpec, canonical};

/// The result of an exhaustive search.
#[derive(Clone, Debug)]
pub struct Certification {
    /// A schedule that violates the outcome spec, if there is one: the first found, with the fewest omissions
    /// among the schedules that reach its violating state.
    pub counterexample: Option<FaultSchedule>,
    /// Distinct states explored, summed over ticks and crash schedules.
    pub states: u64,
    /// Crash schedules explored.
    pub schedules: u64,
}

/// One point of the search: what every node carries into the next tick, what is delivered to it, and the
/// instances at the ticks the spec reads.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct State {
    carried: Vec<Instance>,
    inbox: Vec<Vec<Delivery>>,
    history: Vec<(Tick, Vec<Instance>)>,
}

/// The fewest omissions reaching a state, compared by count and then canonically.
fn better(a: &BTreeSet<Omission>, b: &BTreeSet<Omission>) -> bool {
    (a.len(), a) < (b.len(), b)
}

/// Every crash schedule of at most `max` nodes, each at a tick in `1..EOT`: none first, then by node set and ticks.
fn crash_schedules(spec: &FailureSpec) -> Vec<BTreeMap<NodeId, Tick>> {
    let nodes: Vec<NodeId> = (0..spec.nodes).map(NodeId).collect();
    let ticks: Vec<Tick> = spec.crash_ticks().collect();
    let mut out = vec![BTreeMap::new()];
    let mut layer: Vec<BTreeMap<NodeId, Tick>> = vec![BTreeMap::new()];
    for _ in 0..spec.max_crashes {
        let mut next = Vec::new();
        for base in &layer {
            let after = base.keys().next_back().map_or(0, |n| n.0 + 1);
            for n in nodes.iter().filter(|n| n.0 >= after) {
                for t in &ticks {
                    let mut s = base.clone();
                    s.insert(*n, *t);
                    next.push(s);
                }
            }
        }
        out.extend(next.iter().cloned());
        layer = next;
    }
    out
}

/// What stepping one state produced: its successors with the omissions that lead to each, or a violation.
enum Stepped {
    Next(Vec<(State, BTreeSet<Omission>)>),
    Violation(BTreeSet<Omission>),
    Good,
}

/// Searches every admissible schedule of `spec` for a violation of the outcome spec against `ff_post`, the
/// failure-free run's `post`. Stops at the first violation. Fails with a budget error beyond `max_states` states.
pub fn exhaustive(
    sim: &DedSim<'_>,
    spec: &FailureSpec,
    ff_post: &BTreeSet<Row>,
    workers: usize,
    max_states: u64,
) -> Result<Certification, LdfiError> {
    let snapshot_ticks = sim.snapshot_ticks();
    let n = spec.nodes as usize;
    let mut result = Certification {
        counterexample: None,
        states: 0,
        schedules: 0,
    };
    for crashes in crash_schedules(spec) {
        result.schedules += 1;
        let mut frontier: BTreeMap<State, BTreeSet<Omission>> = BTreeMap::new();
        frontier.insert(
            State {
                carried: vec![Instance::default(); n],
                inbox: vec![Vec::new(); n],
                history: Vec::new(),
            },
            BTreeSet::new(),
        );
        for t in 0..=spec.eot.0 {
            let tick = Tick(t);
            let items: Vec<(State, BTreeSet<Omission>)> = std::mem::take(&mut frontier).into_iter().collect();
            result.states += items.len() as u64;
            if result.states > max_states {
                return Err(LdfiError::Budget(max_states));
            }
            // Step the frontier in bounded batches, merging each batch before the next, so the successors in memory
            // at once stay proportional to one batch.
            let batch = workers.max(1) * 64;
            for part in items.chunks(batch) {
                let stepped = step_all(sim, spec, &crashes, &snapshot_ticks, ff_post, tick, part, workers)?;
                for s in stepped {
                    match s {
                        Stepped::Violation(oms) => {
                            result.counterexample = Some(canonical(FaultSchedule {
                                omissions: oms,
                                crashes: crashes.clone(),
                            }));
                            return Ok(result);
                        }
                        Stepped::Good => {}
                        Stepped::Next(successors) => {
                            for (state, oms) in successors {
                                match frontier.get_mut(&state) {
                                    Some(best) => {
                                        if better(&oms, best) {
                                            *best = oms;
                                        }
                                    }
                                    None => {
                                        frontier.insert(state, oms);
                                        if result.states.saturating_add(frontier.len() as u64) > max_states {
                                            return Err(LdfiError::Budget(max_states));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(result)
}

/// Steps every state of a frontier, in parallel when `workers > 1`; results come back in frontier order.
#[allow(clippy::too_many_arguments)]
fn step_all(
    sim: &DedSim<'_>,
    spec: &FailureSpec,
    crashes: &BTreeMap<NodeId, Tick>,
    snapshot_ticks: &BTreeSet<Tick>,
    ff_post: &BTreeSet<Row>,
    tick: Tick,
    items: &[(State, BTreeSet<Omission>)],
    workers: usize,
) -> Result<Vec<Stepped>, LdfiError> {
    let one = |(state, oms): &(State, BTreeSet<Omission>)| {
        step_state(sim, spec, crashes, snapshot_ticks, ff_post, tick, state, oms)
    };
    if workers <= 1 || items.len() < 2 {
        return items.iter().map(one).collect();
    }
    let chunk = items.len().div_ceil(workers);
    let results: Vec<Result<Vec<Stepped>, LdfiError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|part| scope.spawn(move || part.iter().map(one).collect::<Result<Vec<_>, _>>()))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(internal_error!("a certification worker panicked").into()))
            })
            .collect()
    });
    let mut out = Vec::with_capacity(items.len());
    for r in results {
        out.extend(r?);
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn step_state(
    sim: &DedSim<'_>,
    spec: &FailureSpec,
    crashes: &BTreeMap<NodeId, Tick>,
    snapshot_ticks: &BTreeSet<Tick>,
    ff_post: &BTreeSet<Row>,
    tick: Tick,
    state: &State,
    oms: &BTreeSet<Omission>,
) -> Result<Stepped, LdfiError> {
    let mut outs = Vec::with_capacity(state.carried.len());
    for (i, (carried, inbox)) in state.carried.iter().zip(&state.inbox).enumerate() {
        let node = NodeId(u32::try_from(i).map_err(|_| internal_error!("node index overflow"))?);
        outs.push(sim.step(node, tick, carried, inbox)?);
    }
    let mut history = state.history.clone();
    if snapshot_ticks.contains(&tick) {
        history.push((tick, outs.iter().map(|o| o.instance.clone()).collect()));
    }
    if tick == spec.eot {
        let at = |t: Tick| -> Option<Vec<&Instance>> {
            if t == tick {
                Some(outs.iter().map(|o| &o.instance).collect())
            } else {
                history
                    .iter()
                    .find(|(k, _)| *k == t)
                    .map(|(_, inst)| inst.iter().collect())
            }
        };
        let outcome = sim.outcome_of(tick, &at, crashes, false)?;
        return Ok(if is_good(ff_post, &outcome) {
            Stepped::Good
        } else {
            Stepped::Violation(oms.clone())
        });
    }
    let crashed = |node: NodeId| crashes.get(&node).is_some_and(|c| *c <= tick);
    let mut always: Vec<Vec<Delivery>> = vec![Vec::new(); outs.len()];
    let mut droppable: BTreeMap<(NodeId, NodeId), Vec<Delivery>> = BTreeMap::new();
    for (i, out) in outs.iter().enumerate() {
        let from = NodeId(u32::try_from(i).map_err(|_| internal_error!("node index overflow"))?);
        for send in &out.outbox {
            let own = send.to == from;
            if !own && crashed(from) {
                continue;
            }
            let delivery = Delivery {
                rel: send.rel,
                from,
                row: send.row.clone(),
            };
            if !own && spec.omission_allowed(from, send.to, tick) {
                droppable.entry((from, send.to)).or_default().push(delivery);
            } else if let Some(slot) = always.get_mut(send.to.0 as usize) {
                slot.push(delivery);
            } else {
                return Err(internal_error!("a message to a node outside the deployment").into());
            }
        }
    }
    let channels: Vec<(&(NodeId, NodeId), &Vec<Delivery>)> = droppable.iter().collect();
    if channels.len() >= 63 {
        return Err(internal_error!("{} droppable channels in one tick", channels.len()).into());
    }
    let carried: Vec<Instance> = outs.into_iter().map(|o| o.next).collect();
    let mut successors = Vec::with_capacity(1usize << channels.len());
    for mask in 0u64..(1u64 << channels.len()) {
        let mut inbox = always.clone();
        let mut cand = oms.clone();
        for (bit, ((from, to), deliveries)) in channels.iter().enumerate() {
            if mask & (1 << bit) != 0 {
                cand.insert(Omission {
                    from: *from,
                    to: *to,
                    send: tick,
                });
            } else if let Some(slot) = inbox.get_mut(to.0 as usize) {
                slot.extend(deliveries.iter().cloned());
            }
        }
        for slot in &mut inbox {
            slot.sort();
            slot.dedup();
        }
        successors.push((
            State {
                carried: carried.clone(),
                inbox,
                history: history.clone(),
            },
            cand,
        ));
    }
    Ok(Stepped::Next(successors))
}
