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
//!
//! **Time never goes back across incarnations.** A tick is released only if its `now` is within a durable time
//! bound (`META.last_now`), which the node asks the driver to extend ahead of need, and a restart boots after the
//! bound. So no instant a released tick exposed can be sampled again after a crash, whatever the wall clock does.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use blossom_base::{RelId, RoleId, internal_error};
use blossom_ir::ValidatedProgram;
use blossom_ir::core::{EventSource, RelClass};
use blossom_ir::tick::{Delivery, Egress, HostOut, Ingress, Instance, Row, Send, StepInput};
use blossom_value::time::{Instant, NodeId, Tick};

use crate::durable::{Delta, DurableSchema};
use crate::eval::Executor;
use crate::streams::{NodeStream, Observed, StreamInbox, node_streams};
use crate::timers::TimerTable;
use crate::{NodeError, NodeFault};

/// How many ticks a reservation adds (ARCHITECTURE §5.6 step 5).
pub const RESERVE_STEP: u64 = 65_536;
/// How far ahead of the clock a time reservation reaches, in nanoseconds (one second). A restart within it boots up
/// to this far ahead of the wall clock.
pub const TIME_STEP: i64 = 1_000_000_000;

/// The durable bounds a node runs within: its highest tick and its latest releasable instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reservation {
    pub ticks: Tick,
    pub now: Instant,
}

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
    /// The most bytes of messages (estimated from their values) one tick takes; a tick always takes at least one
    /// message. It bounds a tick's input, and with it the size of its WAL record.
    pub max_batch_bytes: usize,
    /// The most ticks parked awaiting their WAL sync before the node stops taking new work (backpressure).
    pub max_inflight: usize,
    /// The most bytes one connection's `data` event carries in one tick (FOREIGN-PROTOCOLS §1.2).
    pub max_stream_bytes: usize,
    /// The most bytes of blobs the node keeps in memory before it drops those nothing references any more.
    pub blob_cache_bytes: u64,
}

impl NodeConfig {
    pub fn new(node: NodeId, role: Option<RoleId>) -> NodeConfig {
        NodeConfig {
            node,
            role,
            halt: None,
            statics: Vec::new(),
            max_batch: 4096,
            max_batch_bytes: 8 * 1024 * 1024,
            max_inflight: 64,
            max_stream_bytes: 1024 * 1024,
            blob_cache_bytes: 64 * 1024 * 1024,
        }
    }
}

/// What recovery hands the node.
#[derive(Clone, Debug)]
pub struct Boot {
    /// The node's database, holding the durable rows as of the last synced tick: the executor starts on it.
    pub database: Arc<crate::database::Database>,
    /// The incarnation's first tick.
    pub tick: Tick,
    /// The highest tick the incarnation may run before the driver extends the reservation.
    pub reserved: Tick,
    /// The latest instant a released tick may have before the driver extends the reservation.
    pub time_reserved: Instant,
    /// The boot instant: after every instant an earlier incarnation used.
    pub now: Instant,
    /// The incarnation: the store's restart count after this boot (1 on the first boot).
    pub incarnation: u64,
    /// Whether durable state was reloaded: every incarnation after the first. `recovered()` holds in the boot tick
    /// iff this is set (LANGUAGE §8.4, SEM-071).
    pub recovered: bool,
    /// The node's durable blobs (FOREIGN-PROTOCOLS §5): where recovered rows' blobs are read from.
    pub blobs: Arc<dyn blossom_value::BlobSource>,
    /// The blobs the store holds: every recovered row's, and any no row holds, which a collection may delete.
    pub stored: BTreeSet<blossom_value::BlobRef>,
    /// The catch-up of the database's durable views (DATABASE.md §8): `None` when the database started at this
    /// recovery (its views are built at the first tick).
    pub catch_up: Option<blossom_ir::tick::CatchUp>,
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
    /// Extend the reservation to these bounds (write them to `META`, then call [`Node::reserved`]).
    pub reserve: Option<Reservation>,
    /// Blobs the WAL record references that are not durable yet: make each durable before the record's sync
    /// (FOREIGN-PROTOCOLS §5), so recovery never finds a row whose blob is missing.
    pub blobs: Vec<(blossom_value::BlobRef, Arc<[u8]>)>,
}

/// A released tick's externally visible effects.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReleasedTick {
    pub tick: Tick,
    pub sends: Vec<Send>,
    pub egress: Vec<Egress>,
    /// Requests to the host: stream writes, closes and dials (FOREIGN-PROTOCOLS §1.2).
    pub host: Vec<HostOut>,
    /// The connections whose `closed` event this tick delivered: the host closes them after the tick's writes.
    pub retired: Vec<blossom_value::value::ConnId>,
    /// How many offered messages this incarnation's ticks up to this one took: every message offered before that
    /// count was taken by a tick that is now durable (the driver acknowledges a client member's messages by it).
    pub taken: u64,
    /// The tick's change to the durable rows (empty when it changed none): the node's database applies it
    /// (docs/design/DATABASE.md §4).
    pub delta: Delta,
    /// The tick's changes to the durable views (each changed row's support before and after): the database applies
    /// them with `delta` (DATABASE.md §8). Not in the WAL: recomputed from the tables after a restart.
    pub views: BTreeMap<RelId, Vec<(Row, u64, u64)>>,
}

/// A computed tick waiting for release.
#[derive(Clone, Debug)]
struct Parked {
    tick: Tick,
    /// Whether the tick has a WAL record.
    wal: bool,
    delta: Delta,
    views: BTreeMap<RelId, Vec<(Row, u64, u64)>>,
    sends: Vec<Send>,
    egress: Vec<Egress>,
    host: Vec<HostOut>,
    retired: Vec<blossom_value::value::ConnId>,
    halts: bool,
    now: Instant,
    /// The messages taken by this tick and the ones before it.
    taken: u64,
}

/// A message waiting for a tick.
#[derive(Clone, Debug)]
enum Message {
    Deliver(Delivery),
    Ingress(Ingress),
}

pub struct Node<E: Executor> {
    exec: E,
    cfg: NodeConfig,
    schema: DurableSchema,
    boot_rel: Option<RelId>,
    recovered_rel: Option<RelId>,
    recovered: bool,
    incarnation: u64,
    /// The next tick to run.
    tick: Tick,
    reserved: Tick,
    time_reserved: Instant,
    reserving: bool,
    booted: bool,
    last_now: Instant,
    /// Whether the last tick staged a change (its inductive heads differ from the state it started with).
    staged: bool,
    /// The program's durable relations: their committed rows are the database's (the driver reads them).
    durable: BTreeSet<RelId>,
    timers: TimerTable,
    inbox: VecDeque<Message>,
    /// The messages offered and taken so far in this incarnation (`ReleasedTick::taken`).
    offered: u64,
    taken: u64,
    inputs: Vec<(RelId, Row)>,
    streams: StreamInbox,
    parked: VecDeque<Parked>,
    /// The latest tick whose WAL record was reported synced.
    synced: Option<Tick>,
    /// The latest released tick.
    released: Option<Tick>,
    halting: bool,
    state: NodeState,
    /// Blobs created by earlier ticks that are not (known) durable, with their bytes.
    blob_cache: BTreeMap<blossom_value::BlobRef, Arc<[u8]>>,
    /// Blobs made durable (recovered with the image, or handed to the driver with a WAL record).
    durable_blobs: BTreeSet<blossom_value::BlobRef>,
    /// Where durable blobs are read.
    store_blobs: Arc<dyn blossom_value::BlobSource>,
    /// The blobs the WAL records since the database's last flush reference (in their inserted rows, new or already
    /// durable), each with the latest such record's tick: a recovery from that flush may replay rows that hold them.
    recent_blobs: BTreeMap<blossom_value::BlobRef, Tick>,
    /// How many rows of the executor's carried state (every relation it carries, durable or not) hold each blob.
    carried_refs: BTreeMap<blossom_value::BlobRef, u64>,
    /// How many rows of the released durable image hold each blob.
    released_refs: BTreeMap<blossom_value::BlobRef, u64>,
    /// The durable blobs no carried row holds: the only ones a collection looks at, so its work follows the change
    /// rather than the state (FOREIGN-PROTOCOLS §5).
    candidates: BTreeSet<blossom_value::BlobRef>,
    /// The cached blobs' bytes, and the size past which the cache is next trimmed: twice what the last trim kept (at
    /// least the budget), so a trim's work is paid for by the blobs cached since the last one.
    cache_bytes: u64,
    cache_trim_at: u64,
    /// Blobs handed to the driver with a WAL record not known to be synced yet, with the record's tick: the driver
    /// writes them before the record syncs, so until then a later tick reads their bytes here.
    unsynced_blobs: BTreeMap<blossom_value::BlobRef, (Tick, Arc<[u8]>)>,
}

/// A node's blobs as its evaluator reads them: those created by earlier ticks, those handed to the driver whose
/// record may not be synced yet, then the durable ones.
#[derive(Debug)]
pub struct NodeBlobs<'a> {
    cache: &'a BTreeMap<blossom_value::BlobRef, Arc<[u8]>>,
    unsynced: &'a BTreeMap<blossom_value::BlobRef, (Tick, Arc<[u8]>)>,
    durable: &'a dyn blossom_value::BlobSource,
}

impl blossom_value::BlobSource for NodeBlobs<'_> {
    fn get(&self, b: &blossom_value::BlobRef) -> Option<Arc<[u8]>> {
        self.cache
            .get(b)
            .cloned()
            .or_else(|| self.unsynced.get(b).map(|x| x.1.clone()))
            .or_else(|| self.durable.get(b))
    }
}

/// The blobs the rows hold.
fn row_blobs<'r>(rows: impl IntoIterator<Item = &'r Row>, out: &mut BTreeSet<blossom_value::BlobRef>) {
    for r in rows {
        for v in r.iter() {
            blossom_value::blobs_in(v, out);
        }
    }
}

/// Counts, in `refs`, each blob of each of `rows` once per row: up for rows added, down for rows removed. Returns
/// the blobs whose count went from 0 to 1 (`added`) or from 1 to 0 (removed).
fn count_blobs<'r>(
    refs: &mut BTreeMap<blossom_value::BlobRef, u64>,
    rows: impl IntoIterator<Item = &'r Row>,
    added: bool,
) -> Result<Vec<blossom_value::BlobRef>, NodeError> {
    let mut changed = Vec::new();
    for r in rows {
        let mut bs = BTreeSet::new();
        row_blobs(std::iter::once(r), &mut bs);
        for b in bs {
            if added {
                let n = refs.entry(b).or_insert(0);
                *n += 1;
                if *n == 1 {
                    changed.push(b);
                }
            } else {
                match refs.get_mut(&b) {
                    Some(n) if *n > 1 => *n -= 1,
                    Some(_) => {
                        refs.remove(&b);
                        changed.push(b);
                    }
                    None => {
                        return Err(internal_error!("a removed row holds blob {}, which no row held", b.hex()).into());
                    }
                }
            }
        }
    }
    Ok(changed)
}

/// The durable views' rows a tick's changes brought and took (their support crossing zero).
fn view_presence(views: &BTreeMap<RelId, Vec<(Row, u64, u64)>>) -> (Vec<Row>, Vec<Row>) {
    let (mut came, mut went) = (Vec::new(), Vec::new());
    for (row, before, after) in views.values().flatten() {
        match (*before > 0, *after > 0) {
            (false, true) => came.push(row.clone()),
            (true, false) => went.push(row.clone()),
            _ => {}
        }
    }
    (came, went)
}

/// A node's state between two requests of a stateless host (docs/design/STATELESS.md §5a): what a restart loses and
/// a hibernation keeps. Its durable rows are not in it (they are in its store): its carried volatile rows, the bytes
/// of the blobs they hold that are not durable, its timers, and its clock.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Hibernation {
    pub carried: Vec<(RelId, Vec<Row>)>,
    pub blobs: Vec<(blossom_value::BlobRef, Vec<u8>)>,
    pub timers: crate::timers::TimerImage,
    pub last_now: Instant,
}

impl<E: Executor> Node<E> {
    /// Boots a node of `program` (the program the executor runs) from recovered state: the executor starts from the
    /// durable rows (volatile state does not survive a restart).
    pub fn boot(cfg: NodeConfig, program: &ValidatedProgram, exec: E, boot: Boot) -> Result<Node<E>, NodeError> {
        Node::boot_from(cfg, program, exec, boot, None)
    }

    /// Resumes a node that hibernated (docs/design/STATELESS.md §5a): from its store's state and `h`, as if it had
    /// never stopped. Its boot tick ran before it hibernated, so neither `boot` nor `recovered` holds again.
    pub fn resume(
        cfg: NodeConfig,
        program: &ValidatedProgram,
        exec: E,
        boot: Boot,
        h: Hibernation,
    ) -> Result<Node<E>, NodeError> {
        Node::boot_from(cfg, program, exec, boot, Some(h))
    }

    /// The node's state between requests: taken when it is quiescent (every message taken, nothing staged).
    pub fn hibernate(&self) -> Result<Hibernation, NodeError> {
        if !self.booted
            || self.staged
            || !self.inbox.is_empty()
            || !self.inputs.is_empty()
            || !self.parked.is_empty()
            || self.state != NodeState::Running
        {
            return Err(internal_error!("a node hibernates only when it is running and quiescent").into());
        }
        let carried: Vec<(RelId, Vec<Row>)> = self
            .exec
            .carried()?
            .rels
            .into_iter()
            .filter(|(r, rows)| !self.durable.contains(r) && !rows.is_empty())
            .map(|(r, rows)| (r, rows.into_iter().collect()))
            .collect();
        let mut held = BTreeSet::new();
        for (_, rows) in &carried {
            row_blobs(rows, &mut held);
        }
        let mut blobs = Vec::new();
        for b in held.into_iter().filter(|b| !self.durable_blobs.contains(b)) {
            let bytes = self
                .blob_cache
                .get(&b)
                .ok_or_else(|| internal_error!("a carried row holds blob {}, whose bytes are gone", b.hex()))?;
            blobs.push((b, bytes.to_vec()));
        }
        Ok(Hibernation {
            carried,
            blobs,
            timers: self.timers.image(),
            last_now: self.last_now,
        })
    }

    fn boot_from(
        cfg: NodeConfig,
        program: &ValidatedProgram,
        mut exec: E,
        boot: Boot,
        hibernation: Option<Hibernation>,
    ) -> Result<Node<E>, NodeError> {
        let p = program.get();
        let mut boot_rel = None;
        let mut recovered_rel = None;
        for (id, r) in p.rels.iter_enumerated() {
            if let RelClass::Event(src) = &r.class {
                match src {
                    EventSource::Boot => boot_rel = Some(id),
                    EventSource::Recovered => recovered_rel = Some(id),
                    // A client link's events are offered by the driver as inputs (docs/design/CLIENTS.md §3).
                    EventSource::Timer(_) | EventSource::Input | EventSource::Stream(_) | EventSource::Link { .. } => {}
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
        let streams = StreamInbox::new(node_streams(p, cfg.role), cfg.max_stream_bytes);
        let mut carried = Instance::default();
        if let Some(h) = &hibernation {
            for (rel, rows) in &h.carried {
                carried.rels.insert(*rel, rows.iter().cloned().collect());
            }
        }
        exec.reset_on(
            carried,
            boot.database.clone(),
            blossom_engine::Resume {
                statics: cfg.statics.clone(),
                catch_up: boot.catch_up.clone(),
            },
        )?;
        // The executor starts on the recovered rows, which are also the released ones. Every blob the store holds is
        // durable; those no row holds are candidates for collection.
        let mut carried_refs = boot.database.blob_counts()?;
        let released_refs = carried_refs.clone();
        if let Some(h) = &hibernation {
            for (_, rows) in &h.carried {
                count_blobs(&mut carried_refs, rows, true)?;
            }
        }
        let candidates = boot
            .stored
            .iter()
            .filter(|b| !carried_refs.contains_key(*b))
            .copied()
            .collect();
        let mut node = Node {
            blob_cache: BTreeMap::new(),
            recent_blobs: BTreeMap::new(),
            carried_refs,
            released_refs,
            candidates,
            cache_bytes: 0,
            cache_trim_at: cfg.blob_cache_bytes,
            unsynced_blobs: BTreeMap::new(),
            store_blobs: boot.blobs.clone(),
            durable_blobs: boot.stored,
            timers: TimerTable::new(p, cfg.role, boot.now)?,
            durable: schema.rels.iter().map(|(r, _, _)| *r).collect(),
            exec,
            cfg,
            schema,
            boot_rel,
            recovered_rel,
            recovered: boot.recovered,
            incarnation: boot.incarnation,
            tick: boot.tick,
            reserved: boot.reserved,
            time_reserved: boot.time_reserved,
            reserving: false,
            booted: false,
            last_now: boot.now,
            staged: false,
            inbox: VecDeque::new(),
            offered: 0,
            taken: 0,
            inputs: Vec::new(),
            streams,
            parked: VecDeque::new(),
            synced: None,
            released: None,
            halting: false,
            state: NodeState::Running,
        };
        if let Some(h) = hibernation {
            node.timers.restore(&h.timers)?;
            node.booted = true;
            node.recovered = false;
            node.last_now = node.last_now.max(h.last_now);
            for (b, bytes) in h.blobs {
                node.cache_bytes += bytes.len() as u64;
                node.blob_cache.insert(b, Arc::from(bytes));
            }
        }
        Ok(node)
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

    pub fn released_tick(&self) -> Option<Tick> {
        self.released
    }

    /// The carried rows of `rel` at the last computed tick.
    pub fn carried_rows(&self, rel: RelId) -> Result<Vec<Row>, NodeError> {
        Ok(self.exec.carried_rows(rel)?)
    }

    /// The executor's join work so far, in rows examined, if it measures it.
    pub fn rows_examined(&self) -> Option<u64> {
        self.exec.rows_examined()
    }

    /// The rows the executor holds in memory, if it counts them.
    pub fn resident_rows(&self) -> Option<usize> {
        self.exec.resident_rows()
    }

    /// The rows each of the executor's stores holds in memory, largest first, if it counts them.
    pub fn resident_by_store(&self) -> Option<Vec<(RelId, &'static str, usize)>> {
        self.exec.resident_by_store()
    }

    /// The work of each rule in the last tick, if the executor measures it.
    pub fn last_tick_work(&self) -> Option<BTreeMap<blossom_base::RuleId, blossom_ir::tick::RuleWork>> {
        self.exec.last_tick_work()
    }

    /// Whether this incarnation booted from durable state (`recovered()` holds in its boot tick).
    pub fn recovered(&self) -> bool {
        self.recovered
    }

    /// The whole carried state at the last computed tick, for inspection (O(state): tests and tools, not the hot
    /// path).
    pub fn carried(&self) -> Result<Instance, NodeError> {
        Ok(self.exec.carried()?)
    }

    /// The deployment's static rows.
    pub fn statics(&self) -> &[(RelId, Row)] {
        &self.cfg.statics
    }

    /// Admission by ACL (ARCHITECTURE §5.8): whether a message on `rel` from `source` is admitted. `principal in REL`
    /// reads REL's committed rows: the deployment's static rows, the program's facts (`facts`, the evaluator's
    /// static rows), for a durable table the rows at the last released tick (`committed(REL, principal)`: whether the
    /// node's database holds a row of REL led by the principal), or else the rows at the last computed tick.
    pub fn admits(
        &self,
        acl: &crate::acl::AclTable,
        facts: &Instance,
        rel: RelId,
        source: crate::acl::Source<'_>,
        committed: &dyn Fn(RelId, &str) -> Result<bool, NodeError>,
    ) -> Result<bool, NodeError> {
        let failed = std::cell::RefCell::new(None);
        let principal_in = |r: RelId, p: &str| {
            let is = |row: &Row| matches!(row.first(), Some(blossom_value::Value::Principal(x)) if &**x == p);
            self.cfg.statics.iter().any(|(sr, row)| *sr == r && is(row))
                || facts.rows(r).any(is)
                || if self.durable.contains(&r) {
                    committed(r, p).unwrap_or_else(|e| {
                        failed.borrow_mut().get_or_insert(e);
                        false
                    })
                } else {
                    match self.exec.carried_rows(r) {
                        Ok(rows) => rows.iter().any(is),
                        Err(e) => {
                            failed.borrow_mut().get_or_insert(e.into());
                            false
                        }
                    }
                }
        };
        let admitted = acl.admit(rel, source, &principal_in).is_ok();
        match failed.into_inner() {
            Some(e) => Err(e),
            None => Ok(admitted),
        }
    }

    /// A channel tuple from a peer, already admitted. Returns its number among the messages offered to this
    /// incarnation (`ReleasedTick::taken` counts them).
    pub fn offer_delivery(&mut self, d: Delivery) -> u64 {
        self.inbox.push_back(Message::Deliver(d));
        self.offered += 1;
        self.offered - 1
    }

    /// A client session's message, already admitted. Returns its number among the messages offered.
    pub fn offer_ingress(&mut self, m: Ingress) -> u64 {
        self.inbox.push_back(Message::Ingress(m));
        self.offered += 1;
        self.offered - 1
    }

    /// A host row for an `input` relation; it holds at the next tick (LANG-067).
    pub fn offer_input(&mut self, rel: RelId, row: Row) {
        self.inputs.push((rel, row));
    }

    /// What the host observed on one of the node's streams; it becomes stream events of later ticks.
    pub fn observe_stream(&mut self, o: Observed) -> Result<(), NodeError> {
        self.streams.observe(o)
    }

    /// The node's streams (their relations, by index).
    pub fn streams(&self) -> &[NodeStream] {
        self.streams.streams()
    }

    /// The bytes read from connections but not yet delivered (the host stops reading past a limit).
    pub fn stream_backlog(&self) -> usize {
        self.streams.backlog()
    }

    /// The number of messages waiting for a tick.
    pub fn inbox_len(&self) -> usize {
        self.inbox.len()
    }

    /// The number of computed ticks waiting for release.
    pub fn parked(&self) -> usize {
        self.parked.len()
    }

    /// Whether the node cannot run a tick until the driver reports progress (a sync, a reservation) or it stopped:
    /// a due timer or new input does not make it ready.
    pub fn waiting(&self) -> bool {
        self.state != NodeState::Running
            || self.halting
            || self.tick > self.reserved
            || self.parked.len() >= self.cfg.max_inflight
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
            || self.streams.has_events(self.tick.0)
            || self.timers.any_due(now)?)
    }

    /// The earliest instant a timer is due (the driver sleeps until then when nothing else arrives).
    pub fn next_deadline(&self) -> Result<Option<Instant>, NodeError> {
        Ok(self.timers.next_deadline()?)
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
        if !self.booted {
            if let Some(b) = self.boot_rel {
                events.push((b, Arc::from(Vec::new())));
            }
            if self.recovered
                && let Some(r) = self.recovered_rel
            {
                events.push((r, Arc::from(Vec::new())));
            }
        }
        events.extend(self.timers.fire(now)?);
        events.append(&mut self.inputs);
        events.extend(self.streams.take(tick.0)?);
        let retired = self.streams.take_retired();
        let mut delivered = Vec::new();
        let mut ingress = Vec::new();
        let mut bytes = 0usize;
        for taken in 0..self.cfg.max_batch {
            let size = match self.inbox.front() {
                Some(Message::Deliver(d)) => row_size(&d.row),
                Some(Message::Ingress(m)) => row_size(&m.row),
                None => break,
            };
            if taken > 0 && bytes.saturating_add(size) > self.cfg.max_batch_bytes {
                break;
            }
            bytes = bytes.saturating_add(size);
            match self.inbox.pop_front() {
                Some(Message::Deliver(d)) => delivered.push(d),
                Some(Message::Ingress(m)) => ingress.push(m),
                None => break,
            }
            self.taken += 1;
        }
        // The `halt` output and the timers' guards are read at the end of the tick.
        let observe: Vec<RelId> = self.cfg.halt.into_iter().chain(self.timers.guards()).collect();
        let blobs = NodeBlobs {
            cache: &self.blob_cache,
            unsynced: &self.unsynced_blobs,
            durable: self.store_blobs.as_ref(),
        };
        let mut out = self.exec.step(
            &StepInput {
                node: self.cfg.node,
                incarnation: self.incarnation,
                tick,
                now,
                events: &events,
                delivered: &delivered,
                ingress: &ingress,
                blobs: &blobs,
            },
            &observe,
        )?;
        for (b, bytes) in std::mem::take(&mut out.blobs) {
            if !self.durable_blobs.contains(&b)
                && let std::collections::btree_map::Entry::Vacant(e) = self.blob_cache.entry(b)
            {
                self.cache_bytes += bytes.len() as u64;
                e.insert(bytes);
            }
        }
        // The carried rows' blobs: a durable blob no carried row holds may be garbage; one a row holds again is not.
        for rows in out.changes.inserted.values() {
            for b in count_blobs(&mut self.carried_refs, rows, true)? {
                self.candidates.remove(&b);
            }
        }
        for rows in out.changes.deleted.values() {
            for b in count_blobs(&mut self.carried_refs, rows, false)? {
                if self.durable_blobs.contains(&b) {
                    self.candidates.insert(b);
                }
            }
        }
        // A durable view's rows are kept as the carried ones are (DATABASE.md §8): their blobs count the same.
        let (came, went) = view_presence(&out.views);
        for b in count_blobs(&mut self.carried_refs, &came, true)? {
            self.candidates.remove(&b);
        }
        for b in count_blobs(&mut self.carried_refs, &went, false)? {
            if self.durable_blobs.contains(&b) {
                self.candidates.insert(b);
            }
        }
        // The durable delta is the change to the durable relations.
        let mut delta = Delta::default();
        for (rel, rows) in &out.changes.inserted {
            if self.schema.contains(*rel) && !rows.is_empty() {
                delta.changes.entry(*rel).or_default().0.extend(rows.iter().cloned());
            }
        }
        for (rel, rows) in &out.changes.deleted {
            if self.schema.contains(*rel) && !rows.is_empty() {
                delta.changes.entry(*rel).or_default().1.extend(rows.iter().cloned());
            }
        }
        // How the durable views' sources' rows at the tick differ from the carried ones: logged with the change, for
        // a restart's catch-up of the views (DATABASE.md §8).
        for (rel, rows) in std::mem::take(&mut out.written.inserted) {
            if self.schema.contains(rel) && !rows.is_empty() {
                delta.written.entry(rel).or_default().0 = rows;
            }
        }
        for (rel, rows) in std::mem::take(&mut out.written.deleted) {
            if self.schema.contains(rel) && !rows.is_empty() {
                delta.written.entry(rel).or_default().1 = rows;
            }
        }
        let halts = self
            .cfg
            .halt
            .is_some_and(|h| out.observed.get(&h).is_some_and(|rows| !rows.is_empty()));
        self.timers.observe(&out.observed)?;
        // A new node's first boot tick always leaves a WAL record, even an empty one: it marks the store as holding
        // a boot that happened, so a restart knows it recovers (`recovered()`). Until that record is durable the
        // boot did not happen: nothing of it is released, and a crash before the sync boots fresh again.
        let wal = !delta.is_quiet() || (!self.booted && !self.recovered);
        // The blobs the record's new rows reference and that are not durable yet: the driver makes them durable
        // before the record syncs.
        let mut referenced = BTreeSet::new();
        for (inserted, _) in delta.changes.values() {
            row_blobs(inserted, &mut referenced);
        }
        let mut new_blobs = Vec::new();
        for b in referenced {
            // Every blob the record references is a root while a recovery may replay it, new or not.
            self.recent_blobs.insert(b, tick);
            if self.durable_blobs.contains(&b) {
                continue;
            }
            let bytes = self.blob_cache.remove(&b).ok_or_else(|| {
                internal_error!(
                    "tick {} writes a durable row with blob {}, whose bytes are gone",
                    tick.0,
                    b.hex()
                )
            })?;
            self.cache_bytes = self.cache_bytes.saturating_sub(bytes.len() as u64);
            self.durable_blobs.insert(b);
            self.unsynced_blobs.insert(b, (tick, bytes.clone()));
            new_blobs.push((b, bytes));
        }
        self.staged = !out.changes.is_empty();
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
            views: std::mem::take(&mut out.views),
            sends: out.outbox.into_iter().collect(),
            egress: out.egress.into_iter().collect(),
            host: out.host.into_iter().collect(),
            retired,
            halts,
            now,
            taken: self.taken,
        });
        let ticks_low = self.reserved.0.saturating_sub(tick.0) < RESERVE_STEP / 2;
        let time_low = self.time_reserved.0.saturating_sub(now.0) < TIME_STEP / 2;
        let reserve = if !self.reserving && (ticks_low || time_low) {
            self.reserving = true;
            Some(Reservation {
                ticks: if ticks_low {
                    Tick(
                        self.reserved
                            .0
                            .checked_add(RESERVE_STEP)
                            .ok_or_else(|| internal_error!("the tick reservation overflows"))?,
                    )
                } else {
                    self.reserved
                },
                now: Instant(self.time_reserved.0.max(now.0).saturating_add(TIME_STEP)),
            })
        } else {
            None
        };
        self.collect_blobs();
        Ok(TickEffects {
            tick,
            now,
            wal: wal.then_some(delta),
            reserve,
            blobs: new_blobs,
        })
    }

    /// The node's blobs, as its evaluator reads them (for a driver resolving a stream write's `Part::Blob`).
    pub fn blobs(&self) -> NodeBlobs<'_> {
        NodeBlobs {
            cache: &self.blob_cache,
            unsynced: &self.unsynced_blobs,
            durable: self.store_blobs.as_ref(),
        }
    }

    /// The blobs the parked ticks' output holds (what a released tick still sends or writes).
    fn parked_blobs(&self) -> BTreeSet<blossom_value::BlobRef> {
        let mut out = BTreeSet::new();
        for p in &self.parked {
            row_blobs(p.host.iter().map(|h| &h.row), &mut out);
            row_blobs(p.sends.iter().map(|s| &s.row), &mut out);
            row_blobs(p.egress.iter().map(|e| &e.row), &mut out);
        }
        out
    }

    /// Drops the cached blobs nothing references any more, once the cache passes its trim size: a blob a carried
    /// row, a row the executor derived, or a parked output holds may still be written durably or sent, so it stays.
    fn collect_blobs(&mut self) {
        if self.cache_bytes <= self.cache_trim_at {
            return;
        }
        let parked = self.parked_blobs();
        let (refs, exec) = (&self.carried_refs, &self.exec);
        let mut kept = 0u64;
        self.blob_cache.retain(|b, bytes| {
            let live = refs.contains_key(b) || exec.holds_blob(b) || parked.contains(b);
            if live {
                kept += bytes.len() as u64;
            }
            live
        });
        self.cache_bytes = kept;
        self.cache_trim_at = kept.saturating_mul(2).max(self.cfg.blob_cache_bytes);
    }

    /// The candidate blobs the released durable rows do not hold: the ones a database flush of them may let go. Taken
    /// when the flush is asked for (every released tick applied to the database); its work is the candidates', not the
    /// image's.
    pub fn collection_candidates(&self) -> BTreeSet<blossom_value::BlobRef> {
        self.candidates
            .iter()
            .filter(|b| !self.released_refs.contains_key(*b))
            .copied()
            .collect()
    }

    /// The blobs the store may delete once the database's tables cover tick `flushed`, of `outside` (the
    /// [`Node::collection_candidates`] taken when that flush was asked for): those no recovery can reach (the database
    /// does not hold them, no WAL record after the flush references them) and the running node may not write or send
    /// (no carried row, derived row, parked output or unsynced record holds them). They stop being durable here: a row
    /// that needs one later writes it again.
    pub fn blob_garbage(
        &mut self,
        flushed: Tick,
        outside: &BTreeSet<blossom_value::BlobRef>,
    ) -> Vec<blossom_value::BlobRef> {
        // Recovery starts from the flushed tables now: only the records after them are replayed.
        self.recent_blobs.retain(|_, t| *t > flushed);
        let parked = self.parked_blobs();
        let mut gone = Vec::new();
        for b in outside {
            let held = self.carried_refs.contains_key(b)
                || self.exec.holds_blob(b)
                || self.recent_blobs.contains_key(b)
                || self.unsynced_blobs.contains_key(b)
                || parked.contains(b);
            if !held && self.candidates.remove(b) {
                self.durable_blobs.remove(b);
                gone.push(*b);
            }
        }
        gone
    }

    /// The driver made the reservation `r` durable. Returns the ticks that it makes releasable.
    pub fn reserved(&mut self, r: Reservation) -> Result<Vec<ReleasedTick>, NodeError> {
        if r.ticks > self.reserved {
            self.reserved = r.ticks;
        }
        if r.now > self.time_reserved {
            self.time_reserved = r.now;
        }
        self.reserving = false;
        self.release()
    }

    /// The driver reports that every WAL record of a tick `≤ upto` is durable. Returns every tick that is now
    /// releasable, in tick order.
    pub fn wal_synced(&mut self, upto: Tick) -> Result<Vec<ReleasedTick>, NodeError> {
        if self.synced.is_none_or(|s| upto > s) {
            self.synced = Some(upto);
        }
        // The blobs of the synced records are in the store now (written before their records synced).
        self.unsynced_blobs.retain(|_, (t, _)| *t > upto);
        self.release()
    }

    /// Releases the parked ticks that Invariant R allows. A faulted node still releases the ticks computed before
    /// its fault once they are synced: their records are durable, so releasing them is faithful.
    fn release(&mut self) -> Result<Vec<ReleasedTick>, NodeError> {
        let mut out = Vec::new();
        while let Some(p) = self.parked.front() {
            if p.wal && self.synced.is_none_or(|s| p.tick > s) {
                break;
            }
            // Its instant is not yet covered by the durable time bound.
            if p.now > self.time_reserved {
                break;
            }
            let Some(p) = self.parked.pop_front() else {
                break;
            };
            for (inserted, deleted) in p.delta.changes.values() {
                count_blobs(&mut self.released_refs, inserted, true)?;
                count_blobs(&mut self.released_refs, deleted, false)?;
            }
            let (came, went) = view_presence(&p.views);
            count_blobs(&mut self.released_refs, &came, true)?;
            count_blobs(&mut self.released_refs, &went, false)?;
            self.released = Some(p.tick);
            if p.halts {
                self.state = NodeState::Halted;
            }
            out.push(ReleasedTick {
                tick: p.tick,
                sends: p.sends,
                egress: p.egress,
                host: p.host,
                retired: p.retired,
                taken: p.taken,
                delta: p.delta,
                views: p.views,
            });
        }
        Ok(out)
    }

    /// Releases ticks without WAL records at the front of the queue (a tick with no durable change needs no sync).
    /// The driver calls this after each tick; `wal_synced` does it too.
    pub fn release_ready(&mut self) -> Result<Vec<ReleasedTick>, NodeError> {
        self.release()
    }
}

/// An estimate of a row's encoded size, for batching.
fn row_size(row: &Row) -> usize {
    row.iter().map(value_size).sum()
}

fn value_size(v: &blossom_value::Value) -> usize {
    use blossom_value::Value as V;
    match v {
        V::Str(s) | V::Principal(s) => s.len() + 2,
        V::Bytes(b) => b.len() + 2,
        V::Tuple(xs) | V::Struct(xs) | V::Vec(xs) => xs.iter().map(value_size).sum::<usize>() + 2,
        V::Enum { fields, .. } => fields.iter().map(value_size).sum::<usize>() + 2,
        V::Set(xs) => xs.iter().map(value_size).sum::<usize>() + 2,
        V::Map(m) => m.iter().map(|(k, v)| value_size(k) + value_size(v)).sum::<usize>() + 2,
        V::Option(Some(x)) => value_size(x) + 1,
        V::UnknownVariant { payload, .. } | V::Extern { bytes: payload, .. } => payload.len() + 4,
        _ => 9,
    }
}
