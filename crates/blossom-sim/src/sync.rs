//! The synchronous-round world (TEST-006, ARCHITECTURE §6 `SyncRoundScheduler`): every node ticks every round; a
//! message sent in round `t` arrives in round `t + 1` unless the [`FaultSchedule`] omits it; a crashed node
//! behaves as its [`CrashView`] says.
//!
//! The world is generic over the [`Evaluator`] that runs one node's tick. Until the engine exists (docs/design/
//! SLICES.md, slice 5) that is the oracle; the trait is the seam the engine will be put behind.

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::{InternalError, RelId, internal_error};
use blossom_ir::obs::FiringRecord;
use blossom_oracle::{Delivery, Instance, Oracle, OracleError, Row, TickInput, TickOutput};
use blossom_value::time::{NodeId, Tick};

/// Runs one node's tick.
pub trait Evaluator {
    fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, OracleError>;
}

impl Evaluator for Oracle {
    fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, OracleError> {
        Oracle::tick(self, input)
    }
}

/// Why a simulation stopped.
#[derive(Debug, thiserror::Error)]
pub enum SimError {
    /// A node's tick failed: a program error (BLSRnnn) or an evaluator error.
    #[error("node {} at tick {}: {error}", .node.0, .tick.0)]
    Node {
        node: NodeId,
        tick: Tick,
        error: OracleError,
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
}

impl FaultSchedule {
    /// Whether `node` has crashed by round `t` (it crashed at or before `t`).
    pub fn crashed(&self, node: NodeId, t: Tick) -> bool {
        self.crashes.get(&node).is_some_and(|c| *c <= t)
    }

    /// The number of faults.
    pub fn len(&self) -> usize {
        self.omissions.len() + self.crashes.len()
    }

    /// Whether there are no faults.
    pub fn is_empty(&self) -> bool {
        self.omissions.is_empty() && self.crashes.is_empty()
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
    /// Rounds `0..=last` run.
    pub last: Tick,
    pub crash_view: CrashView,
    /// Whether to record every node's firings.
    pub capture: bool,
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
}

impl<'a, E: Evaluator> SyncWorld<'a, E> {
    pub fn new(eval: &'a E, nodes: u32) -> SyncWorld<'a, E> {
        SyncWorld {
            eval,
            nodes,
            inputs: BTreeMap::new(),
        }
    }

    /// Schedules an input event.
    pub fn input(&mut self, node: NodeId, tick: Tick, rel: RelId, row: Row) {
        self.inputs.entry((tick, node)).or_default().push((rel, row));
    }

    /// Runs rounds `0..=config.last` under `faults`.
    pub fn run(&self, config: &SyncConfig, faults: &FaultSchedule) -> Result<SyncRun, SimError> {
        let n = self.nodes as usize;
        let mut run = SyncRun {
            rounds: Vec::new(),
            messages: Vec::new(),
            faults: faults.clone(),
        };
        let mut carried: Vec<Instance> = vec![Instance::default(); n];
        let mut inbox: Vec<Vec<Delivery>> = vec![Vec::new(); n];
        let empty: Vec<(RelId, Row)> = Vec::new();
        for t in 0..=config.last.0 {
            let tick = Tick(t);
            let mut round = Vec::with_capacity(n);
            let mut next_carried = Vec::with_capacity(n);
            let mut next_inbox: Vec<Vec<Delivery>> = vec![Vec::new(); n];
            for (i, (state, delivered)) in carried.iter().zip(inbox.iter()).enumerate() {
                let node = NodeId(u32::try_from(i).map_err(|_| internal_error!("node index overflow"))?);
                let frozen = config.crash_view == CrashView::Frozen && faults.crashed(node, tick);
                if frozen {
                    let previous = round_of(&run.rounds, t.checked_sub(1), i);
                    round.push(NodeTick {
                        instance: previous,
                        firings: Vec::new(),
                        delivered: delivered.clone(),
                        ran: false,
                    });
                    next_carried.push(state.clone());
                    continue;
                }
                let events = self.inputs.get(&(tick, node)).unwrap_or(&empty);
                let out = self
                    .eval
                    .tick(&TickInput {
                        node,
                        tick,
                        carried: state,
                        events,
                        delivered,
                        capture: config.capture,
                    })
                    .map_err(|error| SimError::Node { node, tick, error })?;
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
                    let fate = if t == config.last.0 {
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
                        Fate::Delivered(Tick(t + 1))
                    };
                    if let Fate::Delivered(_) = fate
                        && let Some(slot) = next_inbox.get_mut(send.to.0 as usize)
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
                next_carried.push(out.next);
                round.push(NodeTick {
                    instance: out.instance,
                    firings: out.firings,
                    delivered: delivered.clone(),
                    ran: true,
                });
            }
            for slot in &mut next_inbox {
                slot.sort();
                slot.dedup();
            }
            run.rounds.push(round);
            carried = next_carried;
            inbox = next_inbox;
        }
        run.messages.sort();
        Ok(run)
    }
}

fn round_of(rounds: &[Vec<NodeTick>], t: Option<u64>, node: usize) -> Instance {
    t.and_then(|t| usize::try_from(t).ok())
        .and_then(|t| rounds.get(t))
        .and_then(|r| r.get(node))
        .map(|nt| nt.instance.clone())
        .unwrap_or_default()
}
