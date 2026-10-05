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
//!
//! Programs whose rounds cannot be stepped one at a time (crash-restarts, guarded timers, streams: see
//! [`steppable`]) are decided by [`enumerate`] instead: every admissible fault schedule, each run in full, fewest
//! faults first. It is as sound and complete, at one run per schedule, and it counts the schedules before it runs
//! any, so a spec too large for it fails at once.

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::internal_error;
use blossom_oracle::{Delivery, Instance, Row};
use blossom_sim::spec::{SpecSim, is_good};
use blossom_sim::{FaultSchedule, Omission};
use blossom_value::time::{NodeId, Tick};

use crate::LdfiError;
use crate::faults::FailureSpec;

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

/// One point of the search: what every node carries into the next tick, what is delivered to it, the instances at
/// the ticks the spec reads, and (under the frozen crash view) every node's last instance, which a crashed node keeps.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct State {
    carried: Vec<Instance>,
    inbox: Vec<Vec<Delivery>>,
    history: Vec<(Tick, Vec<Instance>)>,
    last: Vec<Instance>,
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

/// Whether the stepped search can decide `spec` for `sim`'s program (otherwise [`enumerate`] does).
pub fn steppable(sim: &SpecSim<'_>, spec: &FailureSpec) -> bool {
    unsteppable(sim, spec).is_none()
}

/// Why the stepped search cannot decide `spec`: its rounds are stepped one at a time, by `SpecSim::step`, which sees
/// neither a node's previous round (a guarded timer fires on it), nor its incarnations (restarts), nor the stream
/// fabric between the nodes.
fn unsteppable(sim: &SpecSim<'_>, spec: &FailureSpec) -> Option<LdfiError> {
    use blossom_ir::core::{EventSource, RelClass};
    let rels = &sim.artifact().protocol.get().rels;
    if rels
        .iter()
        .any(|r| matches!(&r.class, RelClass::Event(EventSource::Timer(t)) if t.guard.is_some()))
    {
        return Some(
            blossom_base::unimplemented_error!("LANG-172", "stepped certification of a program with a guarded timer")
                .into(),
        );
    }
    if spec.restart.is_some() {
        return Some(
            blossom_base::unimplemented_error!(
                "TEST-037",
                "stepped certification under crash-restarts (it steps nodes one round at a time, without restarts)"
            )
            .into(),
        );
    }
    if spec.delay.is_some() {
        return Some(
            blossom_base::unimplemented_error!(
                "TEST-001",
                "stepped certification under the asynchronous model (it steps nodes with a one-round inbox)"
            )
            .into(),
        );
    }
    if rels
        .iter()
        .any(|r| matches!(&r.class, RelClass::Event(EventSource::Stream(_))))
    {
        return Some(
            blossom_base::unimplemented_error!(
                "TEST-146",
                "stepped certification of a program with streams (it steps nodes without the stream fabric)"
            )
            .into(),
        );
    }
    None
}

/// The result of an enumeration.
#[derive(Clone, Debug)]
pub struct Enumeration {
    /// A schedule that violates the outcome spec, if there is one: one with the fewest faults.
    pub counterexample: Option<FaultSchedule>,
    /// The schedules run.
    pub schedules: u64,
}

/// How many fault schedules [`enumerate`] runs for `spec` (saturating): per crash schedule, every set of allowed
/// omissions within the bound and, under the asynchronous model, every way to delay at most `max_delays` of the
/// other batches (the ones a crashed sender's own downtime implies included: they are skipped, not subtracted).
pub fn schedule_count(spec: &FailureSpec) -> u128 {
    let m = omission_candidates(spec).len() as u128;
    let k = spec.max_omissions.map_or(m, |k| u128::from(k).min(m));
    let lengths = spec.delay.map_or(0, |d| u128::from(d.saturating_sub(1)));
    let mut total: u128 = 0;
    for i in 0..=k {
        // C(m, i) omission sets, then delays among the other m − i batches: Σ_j C(m − i, j)·lengths^j, j ≤ max.
        let Some(lost) = binomial(m, i) else { return u128::MAX };
        let rest = m - i;
        let mut delayed: u128 = 0;
        for j in 0..=u128::from(spec.max_delays).min(rest) {
            let Some(c) = binomial(rest, j) else { return u128::MAX };
            let Some(ways) = u32::try_from(j).ok().and_then(|j| lengths.checked_pow(j)) else {
                return u128::MAX;
            };
            delayed = delayed.saturating_add(c.saturating_mul(ways));
        }
        total = total.saturating_add(lost.saturating_mul(delayed));
    }
    (crash_schedules(spec).len() as u128).saturating_mul(total)
}

/// `C(n, k)`, or `None` past `u128`.
fn binomial(n: u128, k: u128) -> Option<u128> {
    if k > n {
        return Some(0);
    }
    let k = k.min(n - k);
    let mut c: u128 = 1;
    for j in 0..k {
        // C(n, j+1) = C(n, j)·(n−j)/(j+1), exactly.
        c = c.checked_mul(n - j)? / (j + 1);
    }
    Some(c)
}

/// Every omission the spec allows, in order.
fn omission_candidates(spec: &FailureSpec) -> Vec<Omission> {
    let mut out = Vec::new();
    for send in (1..spec.eff.0).map(Tick) {
        for from in (0..spec.nodes).map(NodeId) {
            for to in (0..spec.nodes).map(NodeId) {
                if spec.omission_allowed(from, to, send) {
                    out.push(Omission { from, to, send });
                }
            }
        }
    }
    out
}

/// Every admissible fault schedule of `spec`, each run in full and judged against `ff_post`, the failure-free run's
/// `post`, fewest faults first; stops at the first violation. Fails with a budget error, before running any, when
/// there are more than `max_schedules`.
pub fn enumerate(
    sim: &SpecSim<'_>,
    spec: &FailureSpec,
    ff_post: &BTreeSet<Row>,
    workers: usize,
    max_schedules: u64,
) -> Result<Enumeration, LdfiError> {
    if schedule_count(spec) > u128::from(max_schedules) {
        return Err(LdfiError::ScheduleBudget(max_schedules));
    }
    let candidates = omission_candidates(spec);
    let k = spec
        .max_omissions
        .map_or(candidates.len(), |k| (k as usize).min(candidates.len()));
    let mut schedules: Vec<FaultSchedule> = Vec::new();
    for crashes in crash_schedules(spec) {
        let base = spec.with_restarts(FaultSchedule {
            crashes,
            ..FaultSchedule::default()
        });
        subsets(&candidates, k, &mut Vec::new(), 0, &mut |omissions| {
            let lost: BTreeSet<Omission> = omissions.iter().copied().collect();
            let others: Vec<Omission> = candidates.iter().filter(|o| !lost.contains(o)).copied().collect();
            delay_sets(spec, &others, &mut Vec::new(), 0, &mut |delays| {
                let faults = FaultSchedule {
                    omissions: lost.clone(),
                    delays: delays.iter().copied().collect(),
                    ..base.clone()
                };
                // An omission or delay whose sender is down at the send is implied by the crash: the smaller schedule
                // covers it.
                if spec.canonical(faults.clone()) == faults {
                    schedules.push(faults);
                }
            });
        });
    }
    schedules.sort_by_cached_key(|f| crate::faults::order_key(spec, f));
    let mut result = Enumeration {
        counterexample: None,
        schedules: 0,
    };
    let judge = |faults: &FaultSchedule| -> Result<bool, LdfiError> {
        if !spec.admits(faults) {
            return Err(internal_error!("enumerated a schedule the spec does not admit: {faults:?}").into());
        }
        // A run that ends in a program error is a counterexample.
        let run = match sim.run(spec.eot, faults, false) {
            Ok(run) => run,
            Err(e) if crate::driver::program_failure(&e).is_some() => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        Ok(is_good(ff_post, &sim.outcome(&run, spec.eot, false)?))
    };
    // Judge in order, a batch at a time in parallel: the first violation in order is the result.
    let batch = workers.max(1) * 8;
    for part in schedules.chunks(batch) {
        let goods = in_parallel(part, workers, &judge)?;
        for (faults, good) in part.iter().zip(goods) {
            result.schedules += 1;
            if !good {
                result.counterexample = Some(faults.clone());
                return Ok(result);
            }
        }
    }
    Ok(result)
}

/// A batch and the rounds after its send it arrives in.
type Delayed = (Omission, u64);

/// Calls `f` with every way to delay at most `spec.max_delays` of `batches[from..]` (each by 2 to `spec.delay` rounds)
/// added to `chosen`; under the synchronous model, only with `chosen`.
fn delay_sets(
    spec: &FailureSpec,
    batches: &[Omission],
    chosen: &mut Vec<Delayed>,
    from: usize,
    f: &mut dyn FnMut(&[Delayed]),
) {
    f(chosen);
    let Some(max) = spec.delay else { return };
    if chosen.len() >= spec.max_delays as usize {
        return;
    }
    for i in from..batches.len() {
        if let Some(batch) = batches.get(i) {
            for rounds in 2..=max {
                chosen.push((*batch, rounds));
                delay_sets(spec, batches, chosen, i + 1, f);
                chosen.pop();
            }
        }
    }
}

/// Calls `f` with every subset of at most `k` of `items[from..]` added to `chosen`.
fn subsets<T: Copy>(items: &[T], k: usize, chosen: &mut Vec<T>, from: usize, f: &mut dyn FnMut(&[T])) {
    f(chosen);
    if chosen.len() == k {
        return;
    }
    for i in from..items.len() {
        if let Some(item) = items.get(i) {
            chosen.push(*item);
            subsets(items, k, chosen, i + 1, f);
            chosen.pop();
        }
    }
}

/// `f` over `items` on up to `workers` threads; results in `items` order.
fn in_parallel<T: Sync, R: Send>(
    items: &[T],
    workers: usize,
    f: &(dyn Fn(&T) -> Result<R, LdfiError> + Sync),
) -> Result<Vec<R>, LdfiError> {
    if workers <= 1 || items.len() < 2 {
        return items.iter().map(f).collect();
    }
    let chunk = items.len().div_ceil(workers);
    let results: Vec<Result<Vec<R>, LdfiError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|part| {
                std::thread::Builder::new()
                    .stack_size(blossom_ir::depth::EVAL_STACK_BYTES)
                    .spawn_scoped(scope, move || part.iter().map(f).collect::<Result<Vec<_>, _>>())
            })
            .collect();
        handles
            .into_iter()
            .map(|h| match h {
                Ok(h) => h
                    .join()
                    .unwrap_or_else(|_| Err(internal_error!("an enumeration worker panicked").into())),
                Err(e) => Err(internal_error!("an enumeration worker could not start: {e}").into()),
            })
            .collect()
    });
    let mut out = Vec::with_capacity(items.len());
    for r in results {
        out.extend(r?);
    }
    Ok(out)
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
    sim: &SpecSim<'_>,
    spec: &FailureSpec,
    ff_post: &BTreeSet<Row>,
    workers: usize,
    max_states: u64,
) -> Result<Certification, LdfiError> {
    if let Some(e) = unsteppable(sim, spec) {
        return Err(e);
    }
    if sim.artifact().halt.is_some() {
        return Err(blossom_base::unimplemented_error!(
            "TEST-029",
            "exhaustive certification of a program that writes `halt`"
        )
        .into());
    }
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
                last: if sim.artifact().profile.frozen() {
                    vec![Instance::default(); n]
                } else {
                    Vec::new()
                },
            },
            BTreeSet::new(),
        );
        // The Molly profile starts at tick 1 (tick 0 is the empty initial state); the Blossom profile at tick 0.
        for t in sim.artifact().profile.first_tick().0..=spec.eot.0 {
            let tick = Tick(t);
            let items: Vec<(State, BTreeSet<Omission>)> = std::mem::take(&mut frontier).into_iter().collect();
            result.states += items.len() as u64;
            if result.states > max_states {
                return Err(LdfiError::StateBudget(max_states));
            }
            // Step the frontier in bounded batches, merging each batch before the next, so the successors in memory
            // at once stay proportional to one batch.
            let batch = workers.max(1) * 64;
            for part in items.chunks(batch) {
                let stepped = step_all(sim, spec, &crashes, &snapshot_ticks, ff_post, tick, part, workers)?;
                for s in stepped {
                    match s {
                        Stepped::Violation(oms) => {
                            result.counterexample = Some(spec.canonical(FaultSchedule {
                                omissions: oms,
                                crashes: crashes.clone(),
                                ..FaultSchedule::default()
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
                                            return Err(LdfiError::StateBudget(max_states));
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
    sim: &SpecSim<'_>,
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
            .map(|part| {
                std::thread::Builder::new()
                    .stack_size(blossom_ir::depth::EVAL_STACK_BYTES)
                    .spawn_scoped(scope, move || part.iter().map(one).collect::<Result<Vec<_>, _>>())
            })
            .collect();
        handles
            .into_iter()
            .map(|h| match h {
                Ok(h) => h
                    .join()
                    .unwrap_or_else(|_| Err(internal_error!("a certification worker panicked").into())),
                Err(e) => Err(internal_error!("a certification worker could not start: {e}").into()),
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
    sim: &SpecSim<'_>,
    spec: &FailureSpec,
    crashes: &BTreeMap<NodeId, Tick>,
    snapshot_ticks: &BTreeSet<Tick>,
    ff_post: &BTreeSet<Row>,
    tick: Tick,
    state: &State,
    oms: &BTreeSet<Omission>,
) -> Result<Stepped, LdfiError> {
    let frozen_view = sim.artifact().profile.frozen();
    let crashed = |node: NodeId| crashes.get(&node).is_some_and(|c| *c <= tick);
    let mut outs = Vec::with_capacity(state.carried.len());
    for (i, (carried, inbox)) in state.carried.iter().zip(&state.inbox).enumerate() {
        let node = NodeId(u32::try_from(i).map_err(|_| internal_error!("node index overflow"))?);
        if frozen_view && crashed(node) {
            // A crashed node runs no tick and keeps its state (CR-20).
            outs.push(blossom_oracle::TickOutput {
                instance: state.last.get(i).cloned().unwrap_or_default(),
                next: carried.clone(),
                outbox: BTreeSet::new(),
                egress: BTreeSet::new(),
                host: BTreeSet::new(),
                firings: Vec::new(),
                blobs: std::collections::BTreeMap::new(),
            });
            continue;
        }
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
        let faults = FaultSchedule {
            crashes: crashes.clone(),
            ..FaultSchedule::default()
        };
        let outcome = sim.outcome_of(tick, &at, &faults, false)?;
        return Ok(if is_good(ff_post, &outcome) {
            Stepped::Good
        } else {
            Stepped::Violation(oms.clone())
        });
    }
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
    // A node's last instance matters only once it is frozen: keep it for nodes that crash by the next tick, so that
    // states differing only in other nodes' tick-local contents still merge.
    let last: Vec<Instance> = if frozen_view {
        outs.iter()
            .enumerate()
            .map(|(i, o)| {
                let freezes = u32::try_from(i)
                    .ok()
                    .and_then(|i| crashes.get(&NodeId(i)))
                    .is_some_and(|c| c.0 <= tick.0 + 1);
                if freezes {
                    o.instance.clone()
                } else {
                    Instance::default()
                }
            })
            .collect()
    } else {
        Vec::new()
    };
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
                last: last.clone(),
            },
            cand,
        ));
    }
    Ok(Stepped::Next(successors))
}
