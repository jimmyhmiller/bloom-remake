//! The synchronous-round world (TEST-006, ARCHITECTURE §6 `SyncRoundScheduler`): every node ticks every round; a
//! message sent in round `t` arrives in round `t + 1` unless the [`FaultSchedule`] omits it; a crashed node
//! behaves as its [`CrashView`] says, and one that restarts (crash-recovery, TEST-037) comes back with its durable
//! relations only.
//!
//! The world is generic over the [`Evaluator`] that runs one node's tick. Until the engine exists (docs/design/
//! SLICES.md, slice 5) that is the oracle; the trait is the seam the engine will be put behind.

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::{InternalError, RelId, internal_error};
use blossom_ir::obs::FiringRecord;
use blossom_oracle::{Delivery, Egress, Ingress, Instance, OracleError, Row, TickInput};
use blossom_value::time::{Duration, Instant, NodeId, Tick};

/// Runs one node's tick: the node's seam, shared with the network runtime.
pub use blossom_node::Evaluator;

/// Why a simulation stopped.
#[derive(Debug, thiserror::Error)]
pub enum SimError {
    /// The program cannot be prepared for evaluation (for example, it does not stratify).
    #[error("the program cannot run: {0}")]
    Load(OracleError),
    /// A node's tick failed: a program error (BLSRnnn) or an evaluator error.
    #[error("node {} at tick {}: {error}", .node.0, .tick.0)]
    Node {
        node: NodeId,
        tick: Tick,
        error: OracleError,
    },
    /// A node stayed ready (its state changing) for `ticks` ticks at one instant: the program never quiesces, so
    /// simulated time cannot advance. `changed` names the relations that kept changing.
    #[error("node {} livelocks: still ready after {ticks} ticks at one instant, changing {changed:?}", .node.0)]
    Livelock {
        node: NodeId,
        ticks: u64,
        changed: Vec<String>,
    },
    /// A node sent to a node outside the deployment.
    #[error("node {} sent to node {}, which is not in the deployment of {nodes} node(s)", .from.0, .to.0)]
    UnknownDestination { from: NodeId, to: NodeId, nodes: u32 },
    #[error(transparent)]
    Unimplemented(#[from] blossom_base::Unimplemented),
    #[error(transparent)]
    Internal(#[from] InternalError),
}

/// A lost message: everything `from` sends to `to` in round `send`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Omission {
    pub from: NodeId,
    pub to: NodeId,
    pub send: Tick,
}

/// The faults of one run.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FaultSchedule {
    pub omissions: BTreeSet<Omission>,
    /// Each crashed node and its crash round.
    pub crashes: BTreeMap<NodeId, Tick>,
    /// Each crashed node that restarts, and the round it runs again in (after its crash round; crash-recovery,
    /// TEST-037, under [`CrashView::Frozen`] only).
    pub restarts: BTreeMap<NodeId, Tick>,
    /// Each delayed batch (named like an omission: everything `from` sends `to` in round `send`) and the number of
    /// rounds after its send it arrives in, at least 2 (the asynchronous model, S13). A stream's delayed flight holds
    /// back the ones after it on its connection.
    pub delays: BTreeMap<Omission, u64>,
}

impl FaultSchedule {
    /// Whether `node` is down at round `t`: it crashed at or before `t` and has not restarted by `t`.
    pub fn crashed(&self, node: NodeId, t: Tick) -> bool {
        self.crashes.get(&node).is_some_and(|c| *c <= t) && self.restarts.get(&node).is_none_or(|r| t < *r)
    }

    /// Whether `node` restarts at round `t`.
    pub fn restarts_at(&self, node: NodeId, t: Tick) -> bool {
        self.restarts.get(&node) == Some(&t)
    }

    /// The number of faults.
    pub fn len(&self) -> usize {
        self.omissions.len() + self.crashes.len() + self.delays.len()
    }

    /// Whether there are no faults.
    pub fn is_empty(&self) -> bool {
        self.omissions.is_empty() && self.crashes.is_empty() && self.delays.is_empty()
    }

    /// The rounds after its send the batch `from -> to` sent in round `send` arrives in: 1, unless delayed.
    pub fn delay(&self, from: NodeId, to: NodeId, send: Tick) -> u64 {
        if from == to {
            return 1;
        }
        self.delays.get(&Omission { from, to, send }).copied().unwrap_or(1)
    }
}

/// What a crashed node does (ARCHITECTURE §8.1).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CrashView {
    /// CR-20, the default for `.bls` programs: from its crash round a crashed node fires no rules and sends
    /// nothing; its state stays as it was.
    Frozen,
    /// The `.ded` profile (Molly's): a crashed node keeps receiving and running its deductive and inductive rules,
    /// and from its crash round sends nothing to other nodes (it still delivers to itself).
    MollyContinue,
}

/// How to run.
#[derive(Clone, Debug)]
pub struct SyncConfig {
    /// Rounds before `first` are empty: no node runs (the `.ded` profile starts at Molly's round 1).
    pub first: Tick,
    /// Rounds `first..=last` run.
    pub last: Tick,
    pub crash_view: CrashView,
    /// The duration of a round: round `t` samples `now = t × round` (LANGUAGE §15.2: under LDFI, physical time is
    /// mapped to rounds).
    pub round: Duration,
    /// Whether to record every node's firings.
    pub capture: bool,
    /// A relation that, holding at the end of a node's tick, stops the node: it runs no later tick (`halt`,
    /// LANGUAGE §7.15).
    pub halt: Option<RelId>,
    /// The physical timers, whose firings the round loop makes.
    pub timers: Vec<Timer>,
    /// What a restart keeps and raises (crash-recovery): the durable relations, and the `boot()` and
    /// `recovered()` events of the restart round.
    pub durable: BTreeSet<RelId>,
    pub boot: Option<RelId>,
    pub recovered: Option<RelId>,
    /// The deployment's byte streams, when the world connects them ([`crate::fabric`]); `None` leaves stream events
    /// to the scheduled inputs and drops the requests to the host.
    pub streams: Option<crate::fabric::StreamsConfig>,
}

/// A physical timer (`every d`, LANGUAGE §15.2) in the synchronous world: firing `k` of a node's incarnation is due
/// at `boot + (k + 1) × d`, `boot` the clock of the incarnation's first round, and reaches the node in the first round
/// whose clock has reached it ([`crate::runtime::firings`]). A guarded timer's (`every d while G`) reach it only if
/// `G` held at the end of the node's previous tick. (A guarded timer that resumes fires from its first firing after
/// that tick, which is exactly the round's.)
#[derive(Clone, Debug)]
pub struct Timer {
    pub rel: RelId,
    pub every: Duration,
    pub guard: Option<RelId>,
    /// The nodes it runs on (those of the role it is placed at).
    pub nodes: Vec<NodeId>,
}

/// A message and what became of it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MessageRecord {
    pub rel: RelId,
    pub from: NodeId,
    pub to: NodeId,
    pub send: Tick,
    pub row: Row,
    pub fate: Fate,
}

/// Whether a message arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fate {
    /// Delivered in this round.
    Delivered(Tick),
    /// Omitted by the fault schedule.
    Lost,
    /// Sent in the last round: it would arrive after the run ends.
    AfterEnd,
}

/// One node's round.
#[derive(Clone, Debug, Default)]
pub struct NodeTick {
    /// Every relation's contents at the end of the round.
    pub instance: Instance,
    /// The round's firings (when capturing).
    pub firings: Vec<FiringRecord>,
    /// The channel tuples delivered at the start of the round.
    pub delivered: Vec<Delivery>,
    /// The client sessions' messages of the round.
    pub ingress: Vec<Ingress>,
    /// The replies to client sessions the node sent in the round.
    pub egress: Vec<Egress>,
    /// The requests to the host (stream writes, closes, dials) the node made in the round.
    pub host: Vec<blossom_ir::tick::HostOut>,
    /// The stream events the node took in the round, with their causes (when the world connects streams).
    pub streams: Vec<crate::fabric::StreamEvent>,
    /// Whether the node ran this round (a crashed node under [`CrashView::Frozen`] does not).
    pub ran: bool,
}

/// A whole run: `rounds[t][n]` is node `n`'s round `t`.
#[derive(Clone, Debug, Default)]
pub struct SyncRun {
    pub rounds: Vec<Vec<NodeTick>>,
    /// Every message sent to another node or to the sender itself, in canonical order.
    pub messages: Vec<MessageRecord>,
    pub faults: FaultSchedule,
    /// The requests the nodes' hosts refused (located runtime errors of the program).
    pub stream_violations: Vec<crate::fabric::StreamViolation>,
    /// The stream connections made, by the index stream events refer to them by.
    pub connections: Vec<crate::fabric::Connection>,
    /// Every dial to a node of the deployment: its node, the node it dialed, and its round.
    pub dials: BTreeSet<(NodeId, NodeId, Tick)>,
}

impl SyncRun {
    /// Node `node`'s round `t`.
    pub fn node_tick(&self, t: Tick, node: NodeId) -> Option<&NodeTick> {
        usize::try_from(t.0)
            .ok()
            .and_then(|t| self.rounds.get(t))
            .and_then(|r| r.get(node.0 as usize))
    }

    /// The last round.
    pub fn last(&self) -> Option<Tick> {
        self.rounds
            .len()
            .checked_sub(1)
            .and_then(|t| u64::try_from(t).ok())
            .map(Tick)
    }
}

/// A deployment of `nodes` nodes running one evaluator, with input events per node and round.
pub struct SyncWorld<'a, E: Evaluator> {
    eval: &'a E,
    nodes: u32,
    inputs: BTreeMap<(Tick, NodeId), Vec<(RelId, Row)>>,
    ingress: BTreeMap<(Tick, NodeId), Vec<Ingress>>,
}

impl<'a, E: Evaluator> SyncWorld<'a, E> {
    pub fn new(eval: &'a E, nodes: u32) -> SyncWorld<'a, E> {
        SyncWorld {
            eval,
            nodes,
            inputs: BTreeMap::new(),
            ingress: BTreeMap::new(),
        }
    }

    /// Schedules a client session's message to `node` (LANGUAGE §18.4).
    pub fn ingress(&mut self, node: NodeId, tick: Tick, message: Ingress) {
        self.ingress.entry((tick, node)).or_default().push(message);
    }

    /// Schedules an input event.
    pub fn input(&mut self, node: NodeId, tick: Tick, rel: RelId, row: Row) {
        self.inputs.entry((tick, node)).or_default().push((rel, row));
    }

    /// Runs rounds `config.first..=config.last` under `faults` (earlier rounds are empty).
    pub fn run(&self, config: &SyncConfig, faults: &FaultSchedule) -> Result<SyncRun, SimError> {
        let n = self.nodes as usize;
        if !faults.restarts.is_empty() && config.crash_view != CrashView::Frozen {
            return Err(blossom_base::unimplemented_error!(
                "TEST-037",
                "restarts under the `.ded` crash view (Molly's model has crash-stop failures only)"
            )
            .into());
        }
        for (node, r) in &faults.restarts {
            if faults.crashes.get(node).is_none_or(|c| c >= r) {
                return Err(internal_error!("node {} restarts at {} without crashing before it", node.0, r.0).into());
            }
        }
        if config.streams.is_some() && config.crash_view != CrashView::Frozen {
            return Err(internal_error!("byte streams under the `.ded` crash view").into());
        }
        let mut fabric = config.streams.as_ref().map(crate::fabric::Fabric::new);
        // Each node's incarnation (1, and one more at each restart) and the round it booted in.
        let mut incarnation = vec![1u64; n];
        let mut booted = vec![Tick(0); n];
        let mut run = SyncRun {
            rounds: Vec::new(),
            messages: Vec::new(),
            faults: faults.clone(),
            stream_violations: Vec::new(),
            connections: Vec::new(),
            dials: BTreeSet::new(),
        };
        let mut carried: Vec<Instance> = vec![Instance::default(); n];
        let mut halted = vec![false; n];
        // Per node: the guards that held at the end of its latest tick.
        let mut held: Vec<std::collections::BTreeSet<RelId>> = vec![std::collections::BTreeSet::new(); n];
        // Each node's blobs, kept for the whole run (a simulation is short): what its later ticks read.
        let mut blobs: Vec<blossom_value::BlobMap> = vec![blossom_value::BlobMap::default(); n];
        // What arrives in each later round, per node: a batch arrives a round after its send, or later when delayed.
        let mut arriving: BTreeMap<Tick, Vec<Vec<Delivery>>> = BTreeMap::new();
        for (batch, d) in &faults.delays {
            if *d < 2 {
                return Err(internal_error!(
                    "a delay of {d} round(s) for {batch:?}: a delayed batch arrives 2 or more rounds after its send"
                )
                .into());
            }
        }
        let empty: Vec<(RelId, Row)> = Vec::new();
        let no_ingress: Vec<Ingress> = Vec::new();
        for t in 0..=config.last.0 {
            let tick = Tick(t);
            if tick < config.first {
                run.rounds.push(vec![NodeTick::default(); n]);
                continue;
            }
            if let Some(f) = fabric.as_mut() {
                for (node, c) in &faults.crashes {
                    if *c == tick {
                        f.node_down(*node, tick)?;
                    }
                }
            }
            // Each node's requests to the host and the connections whose `closed` it took: released after the round.
            let mut released: Vec<(
                NodeId,
                Vec<blossom_ir::tick::HostOut>,
                Vec<blossom_value::value::ConnId>,
            )> = Vec::new();
            let mut round = Vec::with_capacity(n);
            let mut next_carried = Vec::with_capacity(n);
            let mut inbox = arriving.remove(&tick).unwrap_or_else(|| vec![Vec::new(); n]);
            for slot in &mut inbox {
                slot.sort();
                slot.dedup();
            }
            for (i, (state, delivered)) in carried.iter().zip(inbox.iter()).enumerate() {
                let node = NodeId(u32::try_from(i).map_err(|_| internal_error!("node index overflow"))?);
                if halted.get(i).copied().unwrap_or(false) {
                    // A halted node runs no tick and holds nothing.
                    round.push(NodeTick::default());
                    next_carried.push(Instance::default());
                    continue;
                }
                let frozen = config.crash_view == CrashView::Frozen && faults.crashed(node, tick);
                if frozen {
                    let previous = round_of(&run.rounds, t.checked_sub(1), i);
                    round.push(NodeTick {
                        instance: previous,
                        firings: Vec::new(),
                        delivered: delivered.clone(),
                        ingress: Vec::new(),
                        egress: Vec::new(),
                        host: Vec::new(),
                        streams: Vec::new(),
                        ran: false,
                    });
                    next_carried.push(state.clone());
                    continue;
                }
                // A node that restarts this round starts from its durable relations as they were after its last
                // round (a crash loses the round it lands in), with every volatile relation empty.
                let restarting = faults.restarts_at(node, tick);
                let restored;
                let state = if restarting {
                    restored = Instance {
                        rels: state
                            .rels
                            .iter()
                            .filter(|(r, _)| config.durable.contains(r))
                            .map(|(r, rows)| (*r, rows.clone()))
                            .collect(),
                    };
                    if let Some(slot) = incarnation.get_mut(i) {
                        *slot += 1;
                    }
                    if let Some(slot) = booted.get_mut(i) {
                        *slot = tick;
                    }
                    if let Some(h) = held.get_mut(i) {
                        h.clear();
                    }
                    &restored
                } else {
                    state
                };
                let scheduled = self.inputs.get(&(tick, node)).unwrap_or(&empty);
                let restart_events: Vec<(RelId, Row)>;
                let scheduled = if restarting {
                    restart_events = scheduled
                        .iter()
                        .cloned()
                        .chain(
                            config
                                .boot
                                .into_iter()
                                .chain(config.recovered)
                                .map(|r| (r, Row::from(Vec::new()))),
                        )
                        .collect();
                    &restart_events
                } else {
                    scheduled
                };
                let mut timer_firings = Vec::new();
                let boot = booted.get(i).copied().unwrap_or(Tick(0));
                if tick > boot {
                    let origin = now_at(config.round, boot)?.0;
                    let (before, now) = (now_at(config.round, Tick(t - 1))?.0, now_at(config.round, tick)?.0);
                    for timer in config.timers.iter().filter(|timer| timer.nodes.contains(&node)) {
                        if timer.guard.is_none_or(|g| held.get(i).is_some_and(|h| h.contains(&g))) {
                            timer_firings.extend(crate::runtime::firings(timer.rel, timer.every, origin, before, now)?);
                        }
                    }
                }
                let (stream_events, retired) = match fabric.as_mut() {
                    Some(f) => f.take(node, tick)?,
                    None => (Vec::new(), Vec::new()),
                };
                let with_more: Vec<(RelId, Row)>;
                let events = if timer_firings.is_empty() && stream_events.is_empty() {
                    scheduled
                } else {
                    with_more = scheduled
                        .iter()
                        .cloned()
                        .chain(timer_firings)
                        .chain(stream_events.iter().map(|e| (e.rel, e.row.clone())))
                        .collect();
                    &with_more
                };
                let ingress = self.ingress.get(&(tick, node)).unwrap_or(&no_ingress);
                let node_blobs = blobs
                    .get(i)
                    .ok_or_else(|| internal_error!("node {i} has no blob map"))?;
                let out = self
                    .eval
                    .tick(&TickInput {
                        incarnation: incarnation.get(i).copied().unwrap_or(1),
                        node,
                        tick,
                        now: now_at(config.round, tick)?,
                        carried: state,
                        events,
                        delivered,
                        ingress,
                        capture: config.capture,
                        blobs: node_blobs,
                    })
                    .map_err(|error| SimError::Node { node, tick, error })?;
                if let Some(b) = blobs.get_mut(i) {
                    b.0.extend(out.blobs.iter().map(|(k, v)| (*k, v.clone())));
                }
                for send in &out.outbox {
                    if send.to.0 >= self.nodes {
                        return Err(SimError::UnknownDestination {
                            from: node,
                            to: send.to,
                            nodes: self.nodes,
                        });
                    }
                    let own = send.to == node;
                    if !own && faults.crashed(node, tick) {
                        // A crashed node sends nothing to other nodes (both crash views).
                        continue;
                    }
                    let arrival = Tick(t + faults.delay(node, send.to, tick));
                    let fate = if arrival > config.last {
                        Fate::AfterEnd
                    } else if !own
                        && faults.omissions.contains(&Omission {
                            from: node,
                            to: send.to,
                            send: tick,
                        })
                    {
                        Fate::Lost
                    } else {
                        Fate::Delivered(arrival)
                    };
                    if let Fate::Delivered(at) = fate
                        && let Some(slot) = arriving
                            .entry(at)
                            .or_insert_with(|| vec![Vec::new(); n])
                            .get_mut(send.to.0 as usize)
                    {
                        slot.push(Delivery {
                            rel: send.rel,
                            from: node,
                            row: send.row.clone(),
                        });
                    }
                    run.messages.push(MessageRecord {
                        rel: send.rel,
                        from: node,
                        to: send.to,
                        send: tick,
                        row: send.row.clone(),
                        fate,
                    });
                }
                if config.halt.is_some_and(|h| out.instance.rows(h).next().is_some())
                    && let Some(slot) = halted.get_mut(i)
                {
                    *slot = true;
                }
                if let Some(h) = held.get_mut(i) {
                    for g in config
                        .timers
                        .iter()
                        .filter(|t| t.nodes.contains(&node))
                        .filter_map(|t| t.guard)
                    {
                        if out.instance.rows(g).next().is_some() {
                            h.insert(g);
                        } else {
                            h.remove(&g);
                        }
                    }
                }
                next_carried.push(out.next);
                // A crashed node's replies and host requests are lost like its messages.
                let (egress, host) = if faults.crashed(node, tick) {
                    (Vec::new(), Vec::new())
                } else {
                    (out.egress.into_iter().collect(), out.host.into_iter().collect())
                };
                if fabric.is_some() {
                    released.push((node, host.clone(), retired));
                }
                round.push(NodeTick {
                    instance: out.instance,
                    firings: out.firings,
                    delivered: delivered.clone(),
                    ingress: ingress.clone(),
                    egress,
                    host,
                    streams: stream_events,
                    ran: true,
                });
            }
            if let Some(f) = fabric.as_mut() {
                let next = Tick(t + 1);
                let at = now_at(config.round, next)?;
                let up_next = |m: NodeId| {
                    next <= config.last
                        && !faults.crashed(m, next)
                        && !halted.get(m.0 as usize).copied().unwrap_or(true)
                };
                for (node, host, retired) in &released {
                    let node_blobs = blobs
                        .get(node.0 as usize)
                        .ok_or_else(|| internal_error!("node {} has no blob map", node.0))?;
                    f.release(*node, tick, host, retired, node_blobs)?;
                }
                let lost = |from: NodeId, to: NodeId| faults.omissions.contains(&Omission { from, to, send: tick });
                let delay = |from: NodeId, to: NodeId| faults.delay(from, to, tick);
                f.deliver(tick, &up_next, &lost, &delay, at)?;
            }
            run.rounds.push(round);
            carried = next_carried;
        }
        run.messages.sort();
        if let Some(f) = fabric {
            run.stream_violations = f.violations;
            run.connections = f.connections;
            run.dials = f.dials;
        }
        Ok(run)
    }
}

/// What a restart keeps and raises in `program`: its durable relations, and its `boot()` and `recovered()` events
/// (for [`SyncConfig`]).
pub fn restart_shape(program: &blossom_ir::core::Program) -> (BTreeSet<RelId>, Option<RelId>, Option<RelId>) {
    use blossom_ir::core::{EventSource, RelClass};
    let mut durable = BTreeSet::new();
    let (mut boot, mut recovered) = (None, None);
    for (id, r) in program.rels.iter_enumerated() {
        if r.durable {
            durable.insert(id);
        }
        match r.class {
            RelClass::Event(EventSource::Boot) => boot = Some(id),
            RelClass::Event(EventSource::Recovered) => recovered = Some(id),
            _ => {}
        }
    }
    (durable, boot, recovered)
}

/// The clock sample of round `t`: `t × round` after the deployment epoch.
pub fn now_at(round: Duration, t: Tick) -> Result<Instant, SimError> {
    i64::try_from(t.0)
        .ok()
        .and_then(|t| round.as_nanos().checked_mul(t))
        .map(Instant)
        .ok_or_else(|| internal_error!("the clock overflows at round {}", t.0).into())
}

/// The round duration of the `.ded` profile: Molly's programs never read the clock.
pub const DED_ROUND: Duration = Duration::from_nanos(1_000_000);

fn round_of(rounds: &[Vec<NodeTick>], t: Option<u64>, node: usize) -> Instance {
    t.and_then(|t| usize::try_from(t).ok())
        .and_then(|t| rounds.get(t))
        .and_then(|r| r.get(node))
        .map(|nt| nt.instance.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use blossom_base::RelId;
    use blossom_oracle::{Send, TickOutput};
    use blossom_value::Value;

    use super::*;

    /// Every node sends `ping(n)` to every other node and to itself at every tick it runs.
    struct Pinger {
        nodes: u32,
    }

    const PING: RelId = RelId::from_raw(0);

    impl Evaluator for Pinger {
        fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, OracleError> {
            let mut out = TickOutput::default();
            for d in input.delivered {
                out.instance.insert(PING, d.row.clone());
            }
            for to in (0..self.nodes).map(NodeId) {
                out.outbox.insert(Send {
                    rel: PING,
                    to,
                    row: Arc::from(vec![Value::Node(to), Value::Node(input.node)]),
                });
            }
            Ok(out)
        }
    }

    fn senders(run: &SyncRun, t: u64, node: u32) -> Vec<u32> {
        run.node_tick(Tick(t), NodeId(node))
            .unwrap()
            .delivered
            .iter()
            .map(|d| d.from.0)
            .collect()
    }

    fn config(view: CrashView) -> SyncConfig {
        SyncConfig {
            first: Tick(1),
            last: Tick(3),
            crash_view: view,
            round: DED_ROUND,
            capture: false,
            halt: None,
            timers: Vec::new(),
            durable: BTreeSet::new(),
            boot: None,
            recovered: None,
            streams: None,
        }
    }

    #[test]
    fn messages_arrive_one_tick_later_unless_omitted() {
        let eval = Pinger { nodes: 2 };
        let world = SyncWorld::new(&eval, 2);
        let mut faults = FaultSchedule::default();
        faults.omissions.insert(Omission {
            from: NodeId(0),
            to: NodeId(1),
            send: Tick(1),
        });
        let run = world.run(&config(CrashView::MollyContinue), &faults).unwrap();
        assert!(
            run.node_tick(Tick(0), NodeId(0)).is_some_and(|nt| !nt.ran),
            "nothing runs before the first tick"
        );
        assert_eq!(
            senders(&run, 1, 1),
            Vec::<u32>::new(),
            "nothing is delivered at the first tick"
        );
        assert_eq!(
            senders(&run, 2, 1),
            [1],
            "0's message sent at 1 was lost; 1's own arrives"
        );
        assert_eq!(senders(&run, 3, 1), [0, 1]);
        assert!(
            run.messages
                .iter()
                .any(|m| m.from == NodeId(0) && m.to == NodeId(1) && m.fate == Fate::Lost)
        );
        assert!(
            run.messages
                .iter()
                .filter(|m| m.send == Tick(3))
                .all(|m| m.fate == Fate::AfterEnd)
        );
    }

    #[test]
    fn a_delayed_batch_arrives_later_and_a_later_one_overtakes_it() {
        let eval = Pinger { nodes: 2 };
        let world = SyncWorld::new(&eval, 2);
        let mut faults = FaultSchedule::default();
        let batch = |send| Omission {
            from: NodeId(0),
            to: NodeId(1),
            send: Tick(send),
        };
        // 0's batch to 1 sent at 1 arrives at 4, after the one sent at 2 (at 3); the one sent at 4 would arrive at 7.
        faults.delays.insert(batch(1), 3);
        faults.delays.insert(batch(4), 3);
        let mut cfg = config(CrashView::MollyContinue);
        cfg.last = Tick(5);
        let run = world.run(&cfg, &faults).unwrap();
        assert_eq!(senders(&run, 2, 1), [1], "0's batch of 1 is delayed; 1's own arrives");
        assert_eq!(senders(&run, 3, 1), [0, 1], "0's batch of 2 overtook it");
        assert_eq!(
            senders(&run, 4, 1),
            [0, 1],
            "0's batches of 1 and 3 (the same row, once)"
        );
        assert_eq!(senders(&run, 5, 1), [1], "0's batch of 4 is delayed past the run");
        let fate = |send| {
            run.messages
                .iter()
                .find(|m| m.from == NodeId(0) && m.to == NodeId(1) && m.send == Tick(send))
                .map(|m| m.fate)
        };
        assert_eq!(fate(1), Some(Fate::Delivered(Tick(4))));
        assert_eq!(fate(2), Some(Fate::Delivered(Tick(3))));
        assert_eq!(fate(4), Some(Fate::AfterEnd));
    }

    /// Node 0 carries a durable row and a volatile row per tick it ran, and records what it saw: the incarnation,
    /// `boot()`, `recovered()` and every ping delivered. Every node pings every node.
    struct Keeper {
        nodes: u32,
    }

    const DUR: RelId = RelId::from_raw(1);
    const VOL: RelId = RelId::from_raw(2);
    const BOOT: RelId = RelId::from_raw(3);
    const RECOVERED: RelId = RelId::from_raw(4);
    const SAW: RelId = RelId::from_raw(5);

    impl Evaluator for Keeper {
        fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, OracleError> {
            let mut out = TickOutput::default();
            let u = |x: u64| Value::Int(blossom_value::value::IntValue::U64(x));
            out.next = input.carried.clone();
            out.next.insert(DUR, Arc::from(vec![u(input.tick.0)]));
            out.next.insert(VOL, Arc::from(vec![u(input.tick.0)]));
            out.instance = input.carried.clone();
            out.instance.insert(SAW, Arc::from(vec![u(input.incarnation)]));
            for (rel, _) in input.events {
                out.instance.insert(*rel, Arc::from(vec![]));
            }
            for d in input.delivered {
                out.instance.insert(PING, d.row.clone());
            }
            for to in (0..self.nodes).map(NodeId) {
                out.outbox.insert(Send {
                    rel: PING,
                    to,
                    row: Arc::from(vec![Value::Node(to), Value::Node(input.node)]),
                });
            }
            Ok(out)
        }
    }

    #[test]
    fn a_restarted_node_keeps_only_its_durable_relations_and_boots_again() {
        let eval = Keeper { nodes: 2 };
        let world = SyncWorld::new(&eval, 2);
        let mut faults = FaultSchedule::default();
        faults.crashes.insert(NodeId(0), Tick(3));
        faults.restarts.insert(NodeId(0), Tick(5));
        let mut cfg = config(CrashView::Frozen);
        cfg.last = Tick(6);
        cfg.durable = BTreeSet::from([DUR]);
        cfg.boot = Some(BOOT);
        cfg.recovered = Some(RECOVERED);
        let run = world.run(&cfg, &faults).unwrap();
        let at = |t: u64| run.node_tick(Tick(t), NodeId(0)).unwrap();
        let ticks = |t: u64, rel: RelId| -> Vec<u64> {
            at(t)
                .instance
                .rows(rel)
                .filter_map(|r| match r.first() {
                    Some(Value::Int(blossom_value::value::IntValue::U64(x))) => Some(*x),
                    _ => None,
                })
                .collect()
        };
        assert!(
            at(2).ran && !at(3).ran && !at(4).ran && at(5).ran,
            "down for rounds 3 and 4"
        );
        assert_eq!(ticks(5, DUR), [1, 2], "the durable rows of the rounds before the crash");
        assert_eq!(ticks(5, VOL), Vec::<u64>::new(), "no volatile row survives the restart");
        assert_eq!(ticks(2, SAW), [1]);
        assert_eq!(ticks(5, SAW), [2], "a restart is a new incarnation");
        assert!(at(5).instance.rows(BOOT).next().is_some() && at(5).instance.rows(RECOVERED).next().is_some());
        assert!(
            at(6).instance.rows(BOOT).next().is_none(),
            "boot() holds in the restart round only"
        );
        assert_eq!(senders(&run, 5, 1), [1], "nothing from 0 while it was down (sent at 4)");
        assert_eq!(senders(&run, 6, 1), [0, 1], "0 sends again from its restart");
        assert!(
            !run.messages
                .iter()
                .any(|m| m.from == NodeId(0) && (m.send == Tick(3) || m.send == Tick(4))),
            "a down node sends nothing"
        );
    }

    /// Records every timer firing it gets as a row of `TIMER`.
    struct Ticker;

    const TIMER: RelId = RelId::from_raw(6);

    impl Evaluator for Ticker {
        fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, OracleError> {
            let mut out = TickOutput::default();
            for (rel, row) in input.events {
                if *rel == TIMER {
                    out.instance.insert(TIMER, row.clone());
                }
            }
            Ok(out)
        }
    }

    #[test]
    fn a_restart_starts_the_timers_counting_again_from_its_round() {
        let world = SyncWorld::new(&Ticker, 1);
        let mut faults = FaultSchedule::default();
        faults.crashes.insert(NodeId(0), Tick(3));
        faults.restarts.insert(NodeId(0), Tick(5));
        let mut cfg = config(CrashView::Frozen);
        cfg.first = Tick(0);
        cfg.last = Tick(9);
        cfg.round = Duration::from_nanos(10);
        cfg.timers = vec![Timer {
            rel: TIMER,
            every: Duration::from_nanos(20),
            guard: None,
            nodes: vec![NodeId(0)],
        }];
        let run = world.run(&cfg, &faults).unwrap();
        let firings: Vec<(u64, u64, i64)> = (0..=9)
            .flat_map(|t| {
                let nt = run.node_tick(Tick(t), NodeId(0)).unwrap();
                let rows: Vec<_> = if nt.ran {
                    nt.instance.rows(TIMER).collect()
                } else {
                    Vec::new()
                };
                rows.into_iter()
                    .map(|r| match (&r[0], &r[1]) {
                        (Value::Int(blossom_value::value::IntValue::U64(k)), Value::Instant(due)) => (t, *k, due.0),
                        other => panic!("not a timer row: {other:?}"),
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(
            firings,
            [(2, 0, 20), (7, 0, 70), (9, 1, 90)],
            "counted from boot (0), then from the restart's clock (50)"
        );
    }

    #[test]
    fn restarts_are_refused_under_the_ded_crash_view() {
        let eval = Pinger { nodes: 2 };
        let world = SyncWorld::new(&eval, 2);
        let mut faults = FaultSchedule::default();
        faults.crashes.insert(NodeId(0), Tick(2));
        faults.restarts.insert(NodeId(0), Tick(3));
        assert!(matches!(
            world.run(&config(CrashView::MollyContinue), &faults),
            Err(SimError::Unimplemented(_))
        ));
    }

    #[test]
    fn a_crashed_node_stops_sending_to_others_but_keeps_its_own_messages() {
        let eval = Pinger { nodes: 2 };
        let world = SyncWorld::new(&eval, 2);
        let mut faults = FaultSchedule::default();
        faults.crashes.insert(NodeId(0), Tick(2));
        let run = world.run(&config(CrashView::MollyContinue), &faults).unwrap();
        assert_eq!(senders(&run, 2, 1), [0, 1], "sent at 1, before the crash");
        assert_eq!(senders(&run, 3, 1), [1], "0 crashed at 2: nothing more from it");
        assert_eq!(
            senders(&run, 3, 0),
            [0, 1],
            "a crashed node keeps receiving and its own messages"
        );
        let frozen = world.run(&config(CrashView::Frozen), &faults).unwrap();
        assert!(
            frozen.node_tick(Tick(2), NodeId(0)).is_some_and(|nt| !nt.ran),
            "a frozen node does not run"
        );
    }
}
