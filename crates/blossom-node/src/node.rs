//! The sans-IO node (ARCHITECTURE §5.1): one node's ticks, with no threads, sockets or files.
//!
//! The driver offers the node what arrived (peer deliveries, client messages, host inputs), asks whether it is
//! [`ready`](Node::ready), and runs ticks with a clock sample. A tick's durable delta comes back as a WAL record for
//! the driver to append; the tick's sends and replies are parked until the driver reports the record synced.
//!
//! **Invariant R.** Tick `t` is released iff every tick `t' ≤ t` that produced a WAL record has been reported synced.
//! Released ticks come out in tick order. A tick with no record is released as soon as every earlier record is
//! synced. Nothing a crash can take back is ever observable outside the node.
//!
//! **When a node ticks** (SEM-009): at boot, on a message, on a due timer, on host input, and after a tick whose
//! inductive heads changed the carried state (a staged change). An idle node does not tick.
//!
//! **Tick numbers** are never reused across incarnations (SEM-001): the node boots at the tick recovery reserved,
//! runs only ticks within the reserved bound, and asks the driver to extend the bound ahead of reaching it.

use std::collections::VecDeque;
use std::sync::Arc;

use blossom_base::{RelId, RoleId, internal_error};
use blossom_ir::ValidatedProgram;
use blossom_ir::core::{EventSource, RelClass};
use blossom_oracle::{Delivery, Egress, Ingress, Instance, Row, Send, TickInput};
use blossom_value::time::{Instant, NodeId, Tick};

use crate::durable::{Delta, DurableImage, DurableSchema};
use crate::eval::Evaluator;
use crate::timers::TimerTable;
use crate::{NodeError, NodeFault};

/// How many ticks a reservation adds (ARCHITECTURE §5.6 step 5).
pub const RESERVE_STEP: u64 = 65_536;

/// A node's configuration.
#[derive(Clone, Debug)]
pub struct NodeConfig {
    pub node: NodeId,
    pub role: Option<RoleId>,
    /// The built-in `halt` output: holding at the end of a tick, it stops the node after that tick.
    pub halt: Option<RelId>,
    /// The deployment's rows of static relations (LANGUAGE §7.5), present at every tick.
    pub statics: Vec<(RelId, Row)>,
    /// The most messages (deliveries and client messages) one tick takes (CR-02: batch composition is a
    /// scheduling choice). The rest wait for the next tick.
    pub max_batch: usize,
    /// The most ticks parked awaiting their WAL sync before the node stops taking new work (backpressure).
    pub max_inflight: usize,
}

impl NodeConfig {
    pub fn new(node: NodeId, role: Option<RoleId>) -> NodeConfig {
        NodeConfig {
            node,
            role,
            halt: None,
            statics: Vec::new(),
            max_batch: 4096,
            max_inflight: 64,
        }
    }
}

/// What recovery hands the node.
#[derive(Clone, Debug)]
pub struct Boot {
    /// The durable rows as of the last synced tick.
    pub image: DurableImage,
    /// The incarnation's first tick.
    pub tick: Tick,
    /// The highest tick the incarnation may run before the driver extends the reservation.
    pub reserved: Tick,
    /// The boot instant: after every instant an earlier incarnation used.
    pub now: Instant,
}

/// Whether the node is running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeState {
    Running,
    /// The program wrote `halt`: the node runs no more ticks.
    Halted,
    /// A tick failed; nothing after it is committed or released. The driver restarts the node from durable state.
    Faulted(String),
}

/// One tick's outputs for the driver.
#[derive(Clone, Debug, Default)]
pub struct TickEffects {
    pub tick: Tick,
    pub now: Instant,
    /// The durable delta, encoded, when the tick changed durable rows: append it to the WAL.
    pub wal: Option<Delta>,
    /// Extend the tick reservation to this bound (write it to `META`, then call [`Node::reserved`]).
    pub reserve: Option<Tick>,
}

/// A released tick's externally visible effects.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReleasedTick {
    pub tick: Tick,
    pub sends: Vec<Send>,
    pub egress: Vec<Egress>,
}

/// A computed tick waiting for release.
#[derive(Clone, Debug)]
struct Parked {
    tick: Tick,
    /// Whether the tick has a WAL record.
    wal: bool,
    delta: Delta,
    sends: Vec<Send>,
    egress: Vec<Egress>,
    halts: bool,
}

/// A message waiting for a tick.
#[derive(Clone, Debug)]
enum Message {
    Deliver(Delivery),
    Ingress(Ingress),
}

pub struct Node<E: Evaluator> {
    eval: E,
    cfg: NodeConfig,
    schema: DurableSchema,
    boot_rel: Option<RelId>,
    /// The next tick to run.
    tick: Tick,
    reserved: Tick,
    reserving: bool,
    booted: bool,
    last_now: Instant,
    /// Last tick's inductive heads.
    carried: Instance,
    /// Whether the last tick staged a change (its inductive heads differ from the state it started with).
    staged: bool,
    /// The durable rows after the last computed tick.
    image: DurableImage,
    /// The durable rows after the last released tick: what a checkpoint at the synced frontier holds.
    released_image: DurableImage,
    timers: TimerTable,
    inbox: VecDeque<Message>,
    inputs: Vec<(RelId, Row)>,
    parked: VecDeque<Parked>,
    /// The latest tick whose WAL record was reported synced.
    synced: Option<Tick>,
    /// The latest released tick.
    released: Option<Tick>,
    halting: bool,
    state: NodeState,
}

impl<E: Evaluator> Node<E> {
    /// Boots a node of `program` (the program the evaluator runs) from recovered state.
    pub fn boot(cfg: NodeConfig, program: &ValidatedProgram, eval: E, boot: Boot) -> Result<Node<E>, NodeError> {
        let p = program.get();
        let mut boot_rel = None;
        for (id, r) in p.rels.iter_enumerated() {
            if let RelClass::Event(src) = &r.class {
                match src {
                    EventSource::Boot => boot_rel = Some(id),
                    EventSource::Timer(_) | EventSource::Input => {}
                    other => {
                        return Err(blossom_base::unimplemented_error!(
                            "DIST-040",
                            "the runtime event `{}` ({other:?}) on a node",
                            r.name
                        )
                        .into());
                    }
                }
            }
        }
        if boot.tick > boot.reserved {
            return Err(internal_error!(
                "boot tick {} is past the reserved bound {}",
                boot.tick.0,
                boot.reserved.0
            )
            .into());
        }
        let schema = DurableSchema::of(p);
        Ok(Node {
            timers: TimerTable::new(p, cfg.role, boot.now)?,
            carried: boot.image.instance(),
            released_image: boot.image.clone(),
            image: boot.image,
            eval,
            cfg,
            schema,
            boot_rel,
            tick: boot.tick,
            reserved: boot.reserved,
            reserving: false,
            booted: false,
            last_now: boot.now,
            staged: false,
            inbox: VecDeque::new(),
            inputs: Vec::new(),
            parked: VecDeque::new(),
            synced: None,
            released: None,
            halting: false,
            state: NodeState::Running,
        })
    }

    pub fn id(&self) -> NodeId {
        self.cfg.node
    }

    pub fn state(&self) -> &NodeState {
        &self.state
    }

    pub fn schema(&self) -> &DurableSchema {
        &self.schema
    }

    /// The latest clock sample: the boot instant before the first tick. The driver's clock must not go below it.
    pub fn last_now(&self) -> Instant {
        self.last_now
    }

    /// The next tick to run.
    pub fn next_tick(&self) -> Tick {
        self.tick
    }

    /// The durable rows after the last released tick. Every tick up to [`Node::released_tick`] is released, so
    /// every WAL record up to it is synced: a checkpoint of this image covers the synced frontier.
    pub fn released_image(&self) -> &DurableImage {
        &self.released_image
    }

    pub fn released_tick(&self) -> Option<Tick> {
        self.released
    }

    /// Last tick's carried state (for inspection and admission).
    pub fn carried(&self) -> &Instance {
        &self.carried
    }

    /// The deployment's static rows.
    pub fn statics(&self) -> &[(RelId, Row)] {
        &self.cfg.statics
    }

    /// Admission by ACL (ARCHITECTURE §5.8): whether a message on `rel` from `source` is admitted. `principal in REL`
    /// reads REL's committed rows: the deployment's static rows, the program's facts (`facts`, the evaluator's
    /// static rows), the rows at the last released tick for a durable table, or else those at the last computed tick.
    pub fn admits(&self, acl: &crate::acl::AclTable, facts: &Instance, rel: RelId, source: crate::acl::Source<'_>) -> bool {
        let principal_in = |r: RelId, p: &str| {
            let is = |row: &Row| matches!(row.first(), Some(blossom_value::Value::Principal(x)) if &**x == p);
            self.cfg.statics.iter().any(|(sr, row)| *sr == r && is(row))
                || facts.rows(r).any(is)
                || match self.released_image.rows.get(&r) {
                    Some(rows) => rows.iter().any(is),
                    None => self.carried.rows(r).any(is),
                }
        };
        acl.admit(rel, source, &principal_in).is_ok()
    }

    /// A channel tuple from a peer, already admitted.
    pub fn offer_delivery(&mut self, d: Delivery) {
        self.inbox.push_back(Message::Deliver(d));
    }

    /// A client session's message, already admitted.
    pub fn offer_ingress(&mut self, m: Ingress) {
        self.inbox.push_back(Message::Ingress(m));
    }

    /// A host row for an `input` relation; it holds at the next tick (LANG-067).
    pub fn offer_input(&mut self, rel: RelId, row: Row) {
        self.inputs.push((rel, row));
    }

    /// The number of messages waiting for a tick.
    pub fn inbox_len(&self) -> usize {
        self.inbox.len()
    }

    /// The number of computed ticks waiting for release.
    pub fn parked(&self) -> usize {
        self.parked.len()
    }

    /// Whether the node wants to run a tick at `now`.
    pub fn ready(&self, now: Instant) -> Result<bool, NodeError> {
        if self.state != NodeState::Running || self.halting || self.tick > self.reserved {
            return Ok(false);
        }
        if self.parked.len() >= self.cfg.max_inflight {
            return Ok(false);
        }
        Ok(!self.booted
            || self.staged
            || !self.inbox.is_empty()
            || !self.inputs.is_empty()
            || self.timers.any_due(now)?)
    }

    /// The earliest instant a timer is due (the driver sleeps until then when nothing else arrives).
    pub fn next_deadline(&self) -> Result<Option<Instant>, NodeError> {
        self.timers.next_deadline()
    }

    /// Runs the next tick at `now`. A program error faults the node: the tick commits and releases nothing.
    pub fn run_tick(&mut self, now: Instant) -> Result<TickEffects, NodeFault> {
        match self.try_tick(now) {
            Ok(fx) => Ok(fx),
            Err(e) => {
                let fault = NodeFault {
                    tick: self.tick,
                    error: e,
                };
                self.state = NodeState::Faulted(fault.to_string());
                Err(fault)
            }
        }
    }

    fn try_tick(&mut self, now: Instant) -> Result<TickEffects, NodeError> {
        if self.state != NodeState::Running || self.halting {
            return Err(internal_error!("run_tick on a node that is not running ({:?})", self.state).into());
        }
        if self.tick > self.reserved {
            return Err(internal_error!("tick {} is past the reserved bound {}", self.tick.0, self.reserved.0).into());
        }
        if now < self.last_now {
            return Err(internal_error!("the clock went backwards: {} after {}", now.0, self.last_now.0).into());
        }
        let tick = self.tick;
        let mut events: Vec<(RelId, Row)> = self.cfg.statics.clone();
        if !self.booted
            && let Some(b) = self.boot_rel
        {
            events.push((b, Arc::from(Vec::new())));
        }
        events.extend(self.timers.fire(now)?);
        events.append(&mut self.inputs);
        let mut delivered = Vec::new();
        let mut ingress = Vec::new();
        for _ in 0..self.cfg.max_batch {
            match self.inbox.pop_front() {
                Some(Message::Deliver(d)) => delivered.push(d),
                Some(Message::Ingress(m)) => ingress.push(m),
                None => break,
            }
        }
        let out = self.eval.tick(&TickInput {
            node: self.cfg.node,
            tick,
            now,
            carried: &self.carried,
            events: &events,
            delivered: &delivered,
            ingress: &ingress,
            capture: false,
        })?;
        let next_image = DurableImage::of(&out.next, &self.schema);
        let delta = self.image.delta(&next_image);
        let halts = self.cfg.halt.is_some_and(|h| out.instance.rows(h).next().is_some());
        let wal = !delta.is_empty();
        self.staged = out.next != self.carried;
        self.carried = out.next;
        self.image = next_image;
        self.booted = true;
        self.last_now = now;
        self.halting = halts;
        self.tick = Tick(
            tick.0
                .checked_add(1)
                .ok_or_else(|| internal_error!("the tick counter overflows"))?,
        );
        self.parked.push_back(Parked {
            tick,
            wal,
            delta: delta.clone(),
            sends: out.outbox.into_iter().collect(),
            egress: out.egress.into_iter().collect(),
            halts,
        });
        let reserve = if !self.reserving && self.reserved.0.saturating_sub(tick.0) < RESERVE_STEP / 2 {
            self.reserving = true;
            Some(Tick(
                self.reserved
                    .0
                    .checked_add(RESERVE_STEP)
                    .ok_or_else(|| internal_error!("the tick reservation overflows"))?,
            ))
        } else {
            None
        };
        Ok(TickEffects {
            tick,
            now,
            wal: wal.then_some(delta),
            reserve,
        })
    }

    /// The driver made the reservation `upto` durable.
    pub fn reserved(&mut self, upto: Tick) {
        if upto > self.reserved {
            self.reserved = upto;
        }
        self.reserving = false;
    }

    /// The driver reports that every WAL record of a tick `≤ upto` is durable. Returns every tick that is now
    /// releasable, in tick order.
    pub fn wal_synced(&mut self, upto: Tick) -> Vec<ReleasedTick> {
        if self.synced.is_none_or(|s| upto > s) {
            self.synced = Some(upto);
        }
        self.release()
    }

    /// Releases the parked ticks that Invariant R allows. A faulted node still releases the ticks computed before
    /// its fault once they are synced: their records are durable, so releasing them is faithful.
    fn release(&mut self) -> Vec<ReleasedTick> {
        let mut out = Vec::new();
        while let Some(p) = self.parked.front() {
            if p.wal && self.synced.is_none_or(|s| p.tick > s) {
                break;
            }
            let Some(p) = self.parked.pop_front() else {
                break;
            };
            self.released_image.apply(&p.delta);
            self.released = Some(p.tick);
            if p.halts {
                self.state = NodeState::Halted;
            }
            out.push(ReleasedTick {
                tick: p.tick,
                sends: p.sends,
                egress: p.egress,
            });
        }
        out
    }

    /// Releases ticks without WAL records at the front of the queue (a tick with no durable change needs no sync).
    /// The driver calls this after each tick; `wal_synced` does it too.
    pub fn release_ready(&mut self) -> Vec<ReleasedTick> {
        self.release()
    }
}
