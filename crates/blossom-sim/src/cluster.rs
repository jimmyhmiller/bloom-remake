//! A deterministic cluster simulator over real nodes: every server is a sans-IO [`Node`] with its store on a
//! [`SimFs`], driven on a virtual clock, connected by a simulated network that delays, drops and partitions
//! messages, and crashed and restarted by a nemesis. Clients run a key-value workload through a
//! [`ClientProtocol`] and record a history for the linearizability checker ([`crate::linearize`]).
//!
//! This is the network runtime's semantics in simulation: admission by ACL, durable-before-release (each tick's WAL
//! record is synced before its sends leave), recovery from the store after a crash (unsynced writes lost or torn),
//! incarnation-unique session ids, and replies to closed sessions dropped. Everything is a function of the seed, so a
//! failing run replays exactly.
//!
//! The nemesis crashes nodes (restarting them at once or after a downtime; some crashes land between a tick's WAL
//! append and its sync, so the unsynced record is lost or torn), partitions the network (one node isolated, an
//! arbitrary split, or links cut one way only), and heals it; faults overlap. [`Observer`]s check invariants over
//! every node's state after every step, and a directed test can script the faults itself ([`Cluster::step_until`],
//! [`Cluster::partition`], [`Cluster::crash`] and the rest) instead of running the nemesis.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{RelId, internal_error};
use blossom_node::acl::{AclTable, Source};
use blossom_node::durable::DurableSchema;
use blossom_node::manual::ManualDriver;
use blossom_node::recovery::{self, StoreSpec};
use blossom_node::{Backend, Executor, Executors, Node, NodeConfig, ReleasedTick};
use blossom_oracle::{Delivery, Ingress, Instance, Row};
use blossom_store::{OpenMode, SimFs, StoreIdentity, Vfs, WriteFate};
use blossom_value::Value;
use blossom_value::time::{Instant, NodeId};
use blossom_value::value::SessionId;

use crate::linearize::{KvInput, KvOutput, Operation};
use crate::sync::SimError;

mod streams;
pub use streams::{StreamAction, StreamClient, StreamEvent};

/// How a key-value workload speaks a program's client protocol.
pub trait ClientProtocol {
    /// The channel and columns (after the destination) of request `id` for `op`.
    fn request(&self, op: &KvInput, id: u64) -> Result<(RelId, Vec<Value>), String>;
    /// Reads a reply: its request id and what it says, or `None` if the row is not a reply this protocol knows.
    fn reply(&self, rel: RelId, row: &Row) -> Option<(u64, Reply)>;
}

/// What a reply says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Done(KvOutput),
    /// Not the leader: try `leader` (or another server when unknown). The request had no effect.
    Redirect(Option<NodeId>),
}

/// An invariant over the cluster's state, checked after every step it is due: `nodes[n]` is node `n`'s carried state
/// (`None` while it is down). An error is a violation: the run stops there and reports it.
pub trait Observer {
    fn observe(&mut self, now: i64, nodes: &[Option<&Instance>]) -> Result<(), String>;

    /// Whether the observer checks the state at `now`. Reading every node's state costs O(state), so an observer
    /// whose violations persist in the state may check less often than every step.
    fn due(&self, _now: i64) -> bool {
        true
    }
}

/// How a crash treats the node's unsynced writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashWrites {
    /// Each is lost, survives or is torn, at random.
    Random,
    /// Every one is lost (power loss).
    Lost,
}

/// The simulation's knobs.
#[derive(Clone, Debug)]
pub struct ClusterConfig {
    pub seed: u64,
    /// One-way message latency, uniformly in `[min, max]` nanoseconds.
    pub latency: (i64, i64),
    /// Per-message loss probability, in parts per million.
    pub loss_ppm: u32,
    pub clients: usize,
    pub keys: usize,
    /// Weights of put, get and delete.
    pub mix: (u32, u32, u32),
    /// Pause between a client's operations, uniformly in `[0, think]` nanoseconds.
    pub think: i64,
    /// A client gives up on an operation after this long: it is recorded unanswered.
    pub timeout: i64,
    /// The nemesis acts every `[nemesis/2, nemesis]` nanoseconds (0: never).
    pub nemesis: i64,
    /// Whether the nemesis may crash and restart nodes, and partition the network.
    pub crashes: bool,
    pub partitions: bool,
    /// A crashed node stays down for up to this long, in nanoseconds (0: it restarts at once).
    pub downtime: i64,
    /// How long the run lasts, in virtual nanoseconds.
    pub duration: i64,
    /// The principal clients claim.
    pub principal: String,
    /// The evaluator the nodes run. `BLOSSOM_EVALUATOR` (`engine`, `oracle`, `checked`) overrides it for a test run:
    /// `checked` runs every node's engine against the oracle at every tick.
    pub backend: Backend,
    /// How the nodes' stores certify their WAL tails.
    pub certification: blossom_store::Certification,
    /// The host functions the program's `extern fn`s call.
    pub externs: Arc<blossom_value::ExternRegistry>,
    /// The largest chunk a byte stream's bytes are split into (FOREIGN-PROTOCOLS §1.4).
    pub chunk_max: usize,
    /// Whether the nemesis also resets byte-stream connections.
    pub stream_drops: bool,
    /// Each node records its trace (`blossom trace`) into this directory: `<node>-<incarnation>.blstrace`.
    pub record: Option<std::path::PathBuf>,
}

impl Default for ClusterConfig {
    fn default() -> ClusterConfig {
        ClusterConfig {
            seed: 1,
            latency: (200_000, 2_000_000),
            loss_ppm: 0,
            clients: 4,
            keys: 4,
            mix: (45, 40, 15),
            think: 5_000_000,
            timeout: 500_000_000,
            nemesis: 0,
            crashes: false,
            partitions: false,
            downtime: 0,
            duration: 5_000_000_000,
            principal: "spiffe://sim/client".into(),
            backend: Backend::default(),
            certification: blossom_store::Certification::default(),
            externs: Arc::new(blossom_value::ExternRegistry::new()),
            chunk_max: 16,
            stream_drops: false,
            record: None,
        }
    }
}

/// What a run produced.
#[derive(Debug, Default)]
pub struct ClusterRun {
    pub history: Vec<Operation<KvInput, KvOutput>>,
    pub crashes: u64,
    pub partitions: u64,
    pub messages: u64,
    pub dropped: u64,
    pub ticks: u64,
    /// The nemesis's actions, for a failure report.
    pub log: Vec<String>,
    /// The first invariant an observer found violated (the run stopped there).
    pub violation: Option<String>,
    /// The nodes' join work, in rows examined, when their executors measure it.
    pub rows_examined: Option<u64>,
    /// Byte-stream connections accepted, bytes carried, connections reset, and bad write `seq`s.
    pub stream_connections: u64,
    pub stream_bytes: u64,
    pub stream_resets: u64,
    pub stream_violations: u64,
    /// Chunks and closes held for a paused node end, delivered when it resumed (or dropped by a reset).
    pub stream_held: u64,
}

/// How many ticks a node may run at one instant before the simulator calls it a livelock.
const LIVELOCK_TICKS: u64 = 10_000;

/// SplitMix64: the simulation's only randomness.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        let span = u64::try_from(hi.saturating_sub(lo)).unwrap_or(0);
        lo.saturating_add(i64::try_from(self.below(span.saturating_add(1))).unwrap_or(0))
    }
    fn chance_ppm(&mut self, ppm: u32) -> bool {
        self.below(1_000_000) < u64::from(ppm)
    }
}

enum Envelope {
    Peer {
        from: NodeId,
        to: NodeId,
        rel: RelId,
        row: Row,
    },
    FromClient {
        client: usize,
        to: NodeId,
        rel: RelId,
        row: Row,
    },
    ToClient {
        client: usize,
        rel: RelId,
        row: Row,
    },
    /// A byte-stream connection attempt reaches its node.
    PipeConnect {
        pipe: usize,
    },
    /// The connecting end learns its connection is established.
    PipeOpened {
        pipe: usize,
    },
    PipeBytes {
        pipe: usize,
        to: usize,
        bytes: Vec<u8>,
    },
    PipeClosed {
        pipe: usize,
        to: usize,
        reason: Arc<str>,
    },
    /// A node's dial that could not even start (no such address, or a partition) is reported failed: after a delay,
    /// as a runtime learns it from the attempt, and only to the incarnation that dialed.
    DialFailed {
        node: NodeId,
        restarts: u64,
        stream: usize,
        req: u64,
        reason: Arc<str>,
    },
}

struct Pending {
    op: KvInput,
    call: i64,
    req: u64,
    /// When the client gives up.
    deadline: i64,
}

struct Client {
    /// The node it currently talks to.
    target: NodeId,
    /// Its session on each node, by node incarnation.
    sessions: BTreeMap<NodeId, (u64, SessionId)>,
    pending: Option<Pending>,
    next_req: u64,
    /// When it next acts (a new operation, or a retry after a redirect).
    wake: i64,
    retry: Option<NodeId>,
}

struct SimNode<'p> {
    fs: SimFs,
    driver: Option<ManualDriver<'p, Box<dyn Executor>>>,
    restarts: u64,
    /// How far this incarnation's clock is ahead of virtual time: a restart boots after every instant the previous
    /// incarnation may have exposed, which can be ahead of the virtual clock (the real clock anchors the same way).
    offset: i64,
    next_session: u64,
    next_conn: u64,
    /// Open sessions: which client each is.
    sessions: BTreeMap<SessionId, usize>,
    /// While the node is down: when it restarts (`i64::MAX`: when a script restarts it).
    down_until: Option<i64>,
}

/// The protocol of a cluster without key-value clients (`clients = 0`), for runs driven by stream clients or a
/// script: it refuses every request, so a misconfigured run fails loudly.
pub struct NoKvClients;

impl ClientProtocol for NoKvClients {
    fn request(&self, _op: &KvInput, _id: u64) -> Result<(RelId, Vec<Value>), String> {
        Err("this cluster has no key-value clients (NoKvClients)".into())
    }

    fn reply(&self, _rel: RelId, _row: &Row) -> Option<(u64, Reply)> {
        None
    }
}

/// A simulated cluster of one program's nodes.
pub struct Cluster<'p> {
    artifact: &'p BlsArtifact,
    schema: &'p DurableSchema,
    executors: Executors,
    /// The program's seed (for a recorded trace's header).
    program_seed: blossom_value::Seed,
    acl: AclTable,
    names: Arc<[Arc<str>]>,
    statics: Vec<(RelId, Row)>,
    nodes: Vec<SimNode<'p>>,
    clients: Vec<Client>,
    net: BTreeMap<(i64, u64), Envelope>,
    seq: u64,
    blocked: BTreeSet<(NodeId, NodeId)>,
    now: i64,
    rng: Rng,
    cfg: ClusterConfig,
    run: ClusterRun,
    protocol: Box<dyn ClientProtocol + 'p>,
    observers: Vec<Box<dyn Observer + 'p>>,
    /// Whether clients hold off starting new operations (operations in flight continue).
    clients_paused: bool,
    nemesis_at: i64,
    /// Byte-stream connections, stream clients, and each node connection's pipe and end.
    pipes: Vec<streams::Pipe>,
    stream_clients: Vec<streams::ClientSlot<'p>>,
    node_ends: BTreeMap<(NodeId, blossom_value::value::ConnId), (usize, usize)>,
}

const EPOCH: i64 = 1_000_000_000_000_000_000;

fn node_id(i: usize) -> Result<NodeId, SimError> {
    u32::try_from(i)
        .map(NodeId)
        .map_err(|_| internal_error!("too many nodes").into())
}

fn identity(names: &[Arc<str>], n: NodeId) -> StoreIdentity {
    StoreIdentity {
        store_uuid: [0; 16],
        deployment_id: [7; 16],
        program_id: [0; 16],
        node_name: names.get(n.0 as usize).cloned().unwrap_or_else(|| Arc::from("?")),
        principal: format!("spiffe://sim/node/{}", n.0).into(),
        format: recovery::FORMAT,
        directory_digest: [0; 16],
    }
}

impl<'p> Cluster<'p> {
    /// A cluster of every node of `artifact`, each with a fresh store. `seed` seeds the program (SEM-084);
    /// `statics` are the deployment's static rows.
    pub fn new(
        artifact: &'p BlsArtifact,
        schema: &'p DurableSchema,
        program_seed: blossom_value::Seed,
        statics: Vec<(RelId, Row)>,
        protocol: Box<dyn ClientProtocol + 'p>,
        cfg: ClusterConfig,
    ) -> Result<Cluster<'p>, SimError> {
        let backend = match std::env::var_os("BLOSSOM_EVALUATOR") {
            Some(_) => Backend::from_env().map_err(|e| SimError::Internal(internal_error!("{e}")))?,
            None => cfg.backend,
        };
        let executors = Executors::new(
            backend,
            artifact.program.clone(),
            artifact.roles.clone(),
            artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
            program_seed,
            cfg.externs.clone(),
        )
        .map_err(SimError::Load)?;
        let names: Arc<[Arc<str>]> = artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect();
        let mut c = Cluster {
            program_seed,
            artifact,
            schema,
            acl: AclTable::of(artifact.program.get()),
            executors,
            names,
            statics,
            nodes: Vec::new(),
            clients: Vec::new(),
            net: BTreeMap::new(),
            seq: 0,
            blocked: BTreeSet::new(),
            now: EPOCH,
            rng: Rng(cfg.seed),
            run: ClusterRun::default(),
            protocol,
            observers: Vec::new(),
            clients_paused: false,
            nemesis_at: i64::MAX,
            pipes: Vec::new(),
            stream_clients: Vec::new(),
            node_ends: BTreeMap::new(),
            cfg,
        };
        if c.cfg.nemesis > 0 {
            c.nemesis_at = c.now + c.rng.range(c.cfg.nemesis / 2, c.cfg.nemesis);
        }
        for i in 0..artifact.nodes.len() {
            c.nodes.push(SimNode {
                fs: SimFs::default(),
                driver: None,
                restarts: 0,
                offset: 0,
                next_session: 0,
                next_conn: 0,
                sessions: BTreeMap::new(),
                down_until: None,
            });
            c.boot(node_id(i)?, true)?;
        }
        for i in 0..c.cfg.clients {
            let target = node_id(i % c.nodes.len().max(1))?;
            let wake = c.now + c.rng.range(0, c.cfg.think);
            c.clients.push(Client {
                target,
                sessions: BTreeMap::new(),
                pending: None,
                next_req: 0,
                wake,
                retry: None,
            });
        }
        Ok(c)
    }

    fn boot(&mut self, n: NodeId, fresh: bool) -> Result<(), SimError> {
        let names = self.names.clone();
        let now = self.now;
        let certification = self.cfg.certification;
        let artifact = self.artifact;
        let statics = self.statics.clone();
        let schema = self.schema;
        let slot = self
            .nodes
            .get_mut(n.0 as usize)
            .ok_or_else(|| internal_error!("no node {}", n.0))?;
        let fs: Arc<dyn Vfs> = Arc::new(slot.fs.clone());
        let opened = recovery::open(
            fs,
            &StoreSpec {
                dir: PathBuf::from(format!("/node{}", n.0)),
                identity: identity(&names, n),
                mode: if fresh { OpenMode::InitFresh } else { OpenMode::Existing },
                certification,
                // Small, so a run's flushes and compactions (and crashes inside them) are many.
                database: blossom_store::lsm::LsmOptions {
                    memtable_bytes: 4 << 10,
                    block_bytes: 512,
                    tier: 3,
                    max_tables: 8,
                    history: 4096,
                    ..blossom_store::lsm::LsmOptions::default()
                },
            },
            &artifact.program,
            names.clone(),
            Instant(now),
            u64::from(n.0) ^ slot.restarts,
        )
        .map_err(|e| SimError::Internal(internal_error!("node {} cannot recover: {e}", n.0)))?;
        slot.restarts = opened.record.restarts;
        slot.offset = opened.boot.now.0.saturating_sub(now).max(0);
        slot.next_session = 0;
        slot.next_conn = 0;
        slot.sessions.clear();
        slot.down_until = None;
        let mut cfg = NodeConfig::new(n, artifact.roles.get(n.0 as usize).copied().flatten());
        cfg.halt = artifact.halt;
        cfg.statics = statics;
        let exec = self
            .executors
            .make(n)
            .map_err(|e| SimError::Internal(internal_error!("node {} cannot start its evaluator: {e}", n.0)))?;
        let exec = match &self.cfg.record {
            None => exec,
            Some(dir) => {
                let program = artifact.program.get();
                let header = blossom_trace::node::NodeTraceHeader {
                    format: blossom_trace::node::FORMAT,
                    program: program.meta.name.as_str().into(),
                    version: program.meta.version,
                    digest: artifact.program.digest().0,
                    nodes: names.to_vec(),
                    node: n,
                    incarnation: opened.boot.incarnation,
                    seed: self.program_seed.0,
                };
                let name = names
                    .get(n.0 as usize)
                    .map_or_else(|| format!("node{}", n.0), |s| s.to_string());
                let path = dir.join(format!("{name}-{}.blstrace", opened.boot.incarnation));
                let file = std::fs::create_dir_all(dir)
                    .and_then(|()| blossom_trace::node::create_trace_file(&path))
                    .map_err(|e| SimError::Internal(internal_error!("the trace {}: {e}", path.display())))?;
                let sink: Box<dyn std::io::Write + Send> = Box::new(std::io::BufWriter::new(file));
                Box::new(
                    blossom_node::record::Recording::new(sink, &header, exec)
                        .map_err(|e| SimError::Internal(internal_error!("the trace {}: {e}", path.display())))?,
                )
            }
        };
        let node = Node::boot(cfg, &artifact.program, exec, opened.boot.clone())
            .map_err(|e| SimError::Internal(internal_error!("node {} cannot boot: {e}", n.0)))?;
        slot.driver = Some(ManualDriver::new(node, artifact.program.get(), schema, names, opened));
        Ok(())
    }

    fn schedule(&mut self, delay: i64, e: Envelope) {
        self.schedule_at(self.now + delay.max(1), e);
    }

    fn schedule_at(&mut self, at: i64, e: Envelope) {
        self.seq += 1;
        self.run.messages += 1;
        self.net.insert((at.max(self.now + 1), self.seq), e);
    }

    /// Adds an invariant checked after every step.
    pub fn observe(&mut self, o: Box<dyn Observer + 'p>) {
        self.observers.push(o);
    }

    /// Runs the workload, with the nemesis if configured, to the end of the configured duration.
    pub fn run(mut self) -> Result<ClusterRun, SimError> {
        let end = EPOCH + self.cfg.duration;
        self.advance(end, true)?;
        Ok(self.finish())
    }

    /// The run so far; operations still in flight never got an answer.
    pub fn finish(mut self) -> ClusterRun {
        for i in 0..self.nodes.len() {
            self.count_work(i);
        }
        for c in &mut self.clients {
            if let Some(p) = c.pending.take() {
                self.run.history.push(Operation {
                    call: u64::try_from(p.call - EPOCH).unwrap_or(0),
                    ret: None,
                    input: p.op,
                    output: None,
                });
            }
        }
        self.run
    }

    /// Runs the cluster with the nemesis (if configured) until virtual time `at` (nanoseconds since the start), or
    /// until an invariant is violated; the cluster stays available for inspection ([`Cluster::state`]).
    pub fn run_until(&mut self, at: i64) -> Result<(), SimError> {
        self.advance(EPOCH.saturating_add(at), true)
    }

    /// The counters and log of the run so far.
    pub fn run_so_far(&self) -> &ClusterRun {
        &self.run
    }

    /// Runs the cluster without the nemesis until virtual time `at` (nanoseconds since the start), or until an
    /// invariant is violated ([`Cluster::violation`]).
    pub fn step_until(&mut self, at: i64) -> Result<(), SimError> {
        self.advance(EPOCH.saturating_add(at), false)
    }

    /// Virtual time, in nanoseconds since the start.
    pub fn now(&self) -> i64 {
        self.now - EPOCH
    }

    pub fn violation(&self) -> Option<&str> {
        self.run.violation.as_deref()
    }

    /// Node `n`'s carried state, or `None` while it is down (O(state)).
    pub fn state(&self, n: NodeId) -> Result<Option<Instance>, SimError> {
        self.nodes
            .get(n.0 as usize)
            .and_then(|s| s.driver.as_ref())
            .map(|d| {
                d.node
                    .carried()
                    .map_err(|e| SimError::Internal(internal_error!("node {}'s state: {e}", n.0)))
            })
            .transpose()
    }

    /// Partitions the network into `groups`: messages cross no group boundary (a node in no group is isolated).
    /// Replaces any earlier partition.
    pub fn partition(&mut self, groups: &[&[NodeId]]) -> Result<(), SimError> {
        let group_of = |n: NodeId| groups.iter().position(|g| g.contains(&n));
        self.blocked.clear();
        for a in 0..self.nodes.len() {
            for b in 0..self.nodes.len() {
                let (a, b) = (node_id(a)?, node_id(b)?);
                if a != b && (group_of(a).is_none() || group_of(a) != group_of(b)) {
                    self.blocked.insert((a, b));
                }
            }
        }
        self.note(format!("partition {groups:?}"));
        self.streams_partitioned()
    }

    /// Cuts the link from `from` to `to` (one way). A byte-stream connection between them resets.
    pub fn cut(&mut self, from: NodeId, to: NodeId) -> Result<(), SimError> {
        self.blocked.insert((from, to));
        self.note(format!("cut {} -> {}", from.0, to.0));
        self.streams_partitioned()
    }

    pub fn heal(&mut self) {
        self.blocked.clear();
        self.note("heal".into());
    }

    /// Crashes node `n`, applying `writes` to its unsynced writes; it stays down until [`Cluster::restart`].
    pub fn crash(&mut self, n: NodeId, writes: CrashWrites) -> Result<(), SimError> {
        self.crash_node(n, writes, i64::MAX, false)
    }

    /// Restarts a crashed node from its store.
    pub fn restart(&mut self, n: NodeId) -> Result<(), SimError> {
        if self.nodes.get(n.0 as usize).is_none_or(|s| s.driver.is_some()) {
            return Err(internal_error!("node {} is not down", n.0).into());
        }
        self.note(format!("restart node {}", n.0));
        self.boot(n, false)
    }

    /// Offers `row` to node `n`'s input relation `rel`, for its next tick; a node that is down never sees it. A
    /// directed test drives a program through its inputs this way.
    pub fn input(&mut self, n: NodeId, rel: RelId, row: Row) -> Result<(), SimError> {
        let slot = self
            .nodes
            .get_mut(n.0 as usize)
            .ok_or_else(|| SimError::Internal(internal_error!("no node {}", n.0)))?;
        if let Some(d) = slot.driver.as_mut() {
            d.node.offer_input(rel, row);
        }
        Ok(())
    }

    /// Holds clients off new operations (`true`), or lets them go on.
    pub fn pause_clients(&mut self, paused: bool) {
        self.clients_paused = paused;
    }

    /// Points every client at node `n` for its next operation.
    pub fn route_clients(&mut self, n: NodeId) {
        for c in &mut self.clients {
            c.target = n;
            if c.pending.is_none() {
                c.retry = None;
            }
        }
    }

    /// Adds node `i`'s join work to the run's (before its executor goes).
    fn count_work(&mut self, i: usize) {
        if let Some(n) = self
            .nodes
            .get(i)
            .and_then(|s| s.driver.as_ref())
            .and_then(|d| d.node.rows_examined())
        {
            *self.run.rows_examined.get_or_insert(0) += n;
        }
    }

    fn note(&mut self, what: String) {
        self.run.log.push(format!("{}: {what}", self.now - EPOCH));
    }

    fn advance(&mut self, end: i64, nemesis: bool) -> Result<(), SimError> {
        while self.now < end && self.run.violation.is_none() {
            // Run every node that is ready now, then check the invariants.
            for i in 0..self.nodes.len() {
                self.step_node(node_id(i)?)?;
            }
            self.check()?;
            if self.run.violation.is_some() {
                break;
            }
            // The next event.
            let mut next = if nemesis { end.min(self.nemesis_at) } else { end };
            if let Some(((t, _), _)) = self.net.first_key_value() {
                next = next.min(*t);
            }
            for c in &self.clients {
                if !(self.clients_paused && c.pending.is_none()) {
                    next = next.min(c.wake);
                }
                if let Some(p) = &c.pending {
                    next = next.min(p.deadline);
                }
            }
            if let Some(w) = self.stream_wake() {
                next = next.min(w);
            }
            for n in &self.nodes {
                if let Some(t) = n.down_until {
                    next = next.min(t);
                }
                if let Some(d) = &n.driver
                    && let Some(t) = d.node.next_deadline().map_err(|e| internal_error!("{e}"))?
                {
                    next = next.min(t.0.saturating_sub(n.offset));
                }
            }
            self.now = next.max(self.now + 1);
            // Deliver what arrives by now.
            while let Some(entry) = self.net.first_entry() {
                if entry.key().0 > self.now {
                    break;
                }
                let e = entry.remove();
                self.deliver(e)?;
            }
            // Restart the nodes whose downtime is over.
            for i in 0..self.nodes.len() {
                if self
                    .nodes
                    .get(i)
                    .and_then(|s| s.down_until)
                    .is_some_and(|t| t <= self.now)
                {
                    let n = node_id(i)?;
                    self.note(format!("restart node {}", n.0));
                    self.boot(n, false)?;
                }
            }
            if nemesis && self.now >= self.nemesis_at {
                self.nemesis()?;
                self.nemesis_at = self.now + self.rng.range(self.cfg.nemesis / 2, self.cfg.nemesis);
            }
            for c in 0..self.clients.len() {
                self.client_step(c)?;
            }
            self.stream_clients_step()?;
        }
        Ok(())
    }

    fn check(&mut self) -> Result<(), SimError> {
        let now = self.now - EPOCH;
        if !self.observers.iter().any(|o| o.due(now)) {
            return Ok(());
        }
        let owned: Vec<Option<Instance>> = self
            .nodes
            .iter()
            .map(|s| s.driver.as_ref().map(|d| d.node.carried()).transpose())
            .collect::<Result<_, _>>()
            .map_err(|e| SimError::Internal(internal_error!("a node's state: {e}")))?;
        let states: Vec<Option<&Instance>> = owned.iter().map(Option::as_ref).collect();
        let mut violation = None;
        for o in &mut self.observers {
            if !o.due(now) {
                continue;
            }
            if let Err(e) = o.observe(now, &states) {
                violation = Some(format!("{now}: {e}"));
                break;
            }
        }
        if let Some(v) = violation {
            self.run.log.push(format!("violation: {v}"));
            self.run.violation = Some(v);
        }
        Ok(())
    }

    /// Crashes node `n`. With `mid_tick`, a ready node first runs one tick up to its WAL append, so the crash lands
    /// between the append and the sync.
    fn crash_node(&mut self, n: NodeId, writes: CrashWrites, down_until: i64, mid_tick: bool) -> Result<(), SimError> {
        self.count_work(n.0 as usize);
        let seed = self.rng.next();
        let now = self.now;
        let slot = self
            .nodes
            .get_mut(n.0 as usize)
            .ok_or_else(|| internal_error!("no node {}", n.0))?;
        let mut interrupted = false;
        if let Some(d) = slot.driver.take()
            && mid_tick
        {
            let at = Instant(now.saturating_add(slot.offset));
            if d.node
                .ready(at)
                .map_err(|e| internal_error!("node {} failed: {e}", n.0))?
            {
                let tick = d.node.next_tick();
                d.crash_before_sync(at).map_err(|e| node_failure(n, tick, e))?;
                interrupted = true;
            }
        }
        let mut rng = Rng(seed);
        slot.fs
            .crash(&mut |_| match writes {
                CrashWrites::Lost => WriteFate::Lost,
                CrashWrites::Random => match rng.below(3) {
                    0 => WriteFate::Lost,
                    1 => WriteFate::Survive,
                    _ => WriteFate::Torn { sectors: 1 },
                },
            })
            .map_err(|e| internal_error!("crash: {e}"))?;
        slot.down_until = Some(down_until);
        self.run.crashes += 1;
        self.streams_node_down(n)?;
        self.note(format!(
            "crash node {}{}",
            n.0,
            if interrupted {
                " between a WAL append and its sync"
            } else {
                ""
            }
        ));
        Ok(())
    }

    fn step_node(&mut self, n: NodeId) -> Result<(), SimError> {
        let Some(slot) = self.nodes.get_mut(n.0 as usize) else {
            return Err(internal_error!("no node {}", n.0).into());
        };
        let now = Instant(self.now.saturating_add(slot.offset));
        if slot.driver.is_none() {
            return Ok(());
        }
        let sessions = slot.sessions.clone();
        // One tick at a time, each released tick's stream writes resolved before the next tick runs, as the runtime
        // dispatches them on release: a later tick may drop a blob from the cache, or a checkpoint collect it.
        let mut released: Vec<ReleasedTick> = Vec::new();
        let mut at_once = 0u64;
        let mut watched: Option<blossom_ir::tick::Instance> = None;
        loop {
            let Some(driver) = self.nodes.get_mut(n.0 as usize).and_then(|s| s.driver.as_mut()) else {
                return Ok(());
            };
            if !driver
                .node
                .ready(now)
                .map_err(|e| node_failure(n, driver.node.next_tick(), e))?
            {
                break;
            }
            // A node that is still ready after `LIVELOCK_TICKS` ticks at one instant never quiesces: a rule changes
            // state at every tick. The run stops with the relations the last tick changed.
            at_once += 1;
            if at_once == LIVELOCK_TICKS {
                watched = Some(
                    driver
                        .node
                        .carried()
                        .map_err(|e| node_failure(n, driver.node.next_tick(), e))?,
                );
            }
            if at_once > LIVELOCK_TICKS {
                let now_state = driver
                    .node
                    .carried()
                    .map_err(|e| node_failure(n, driver.node.next_tick(), e))?;
                let p = self.artifact.program.get();
                let name = |r: &RelId| p.rels.get(*r).map_or_else(|| format!("{r:?}"), |d| d.name.to_string());
                let before = watched.take().unwrap_or_default();
                let mut changed: Vec<String> = now_state
                    .rels
                    .iter()
                    .filter(|(r, rows)| before.rels.get(r) != Some(rows))
                    .map(|(r, _)| name(r))
                    .collect();
                changed.extend(before.rels.keys().filter(|r| !now_state.rels.contains_key(r)).map(name));
                return Err(SimError::Livelock {
                    node: n,
                    ticks: LIVELOCK_TICKS,
                    changed,
                });
            }
            let ticks = driver
                .run_one(now)
                .map_err(|e| node_failure(n, driver.node.next_tick(), e))?;
            self.run.ticks += 1;
            for mut t in ticks {
                let host = std::mem::take(&mut t.host);
                let retired = std::mem::take(&mut t.retired);
                self.stream_released(n, &host, &retired)?;
                released.push(t);
            }
        }
        // Occasionally flush the database (beside the flushes its size makes), to exercise recovery from its tables.
        if self.rng.below(50) == 0
            && let Some(driver) = self.nodes.get_mut(n.0 as usize).and_then(|s| s.driver.as_mut())
        {
            driver
                .flush()
                .map_err(|e| SimError::Internal(internal_error!("node {} database flush failed: {e}", n.0)))?;
        }
        let mut local: Vec<Delivery> = Vec::new();
        for t in released {
            for s in t.sends {
                if s.to == n {
                    local.push(Delivery {
                        rel: s.rel,
                        from: n,
                        row: s.row,
                    });
                    continue;
                }
                if self.blocked.contains(&(n, s.to)) || self.rng.chance_ppm(self.cfg.loss_ppm) {
                    self.run.dropped += 1;
                    continue;
                }
                let delay = self.rng.range(self.cfg.latency.0, self.cfg.latency.1);
                self.schedule(
                    delay,
                    Envelope::Peer {
                        from: n,
                        to: s.to,
                        rel: s.rel,
                        row: s.row,
                    },
                );
            }
            for e in t.egress {
                let Some(&client) = sessions.get(&e.session) else {
                    self.run.dropped += 1;
                    continue;
                };
                let delay = self.rng.range(self.cfg.latency.0, self.cfg.latency.1);
                self.schedule(
                    delay,
                    Envelope::ToClient {
                        client,
                        rel: e.rel,
                        row: e.row,
                    },
                );
            }
        }
        if let Some(d) = self.nodes.get_mut(n.0 as usize).and_then(|s| s.driver.as_mut()) {
            for l in local {
                d.node.offer_delivery(l);
            }
        }
        Ok(())
    }

    fn deliver(&mut self, e: Envelope) -> Result<(), SimError> {
        match e {
            Envelope::Peer { from, to, rel, row } => {
                if self.blocked.contains(&(from, to)) {
                    self.run.dropped += 1;
                    return Ok(());
                }
                let principal = format!("spiffe://sim/node/{}", from.0);
                let role = self.artifact.roles.get(from.0 as usize).copied().flatten();
                let facts = self.executors.oracle().static_facts();
                let Some(d) = self.nodes.get_mut(to.0 as usize).and_then(|s| s.driver.as_mut()) else {
                    self.run.dropped += 1;
                    return Ok(());
                };
                let admitted = d
                    .admits(
                        &self.acl,
                        facts,
                        rel,
                        Source::Node {
                            role,
                            principal: &principal,
                        },
                    )
                    .map_err(|e| SimError::Internal(internal_error!("node {} admission failed: {e}", to.0)))?;
                if admitted {
                    d.node.offer_delivery(Delivery { rel, from, row });
                } else {
                    self.run.dropped += 1;
                }
            }
            Envelope::FromClient { client, to, rel, row } => {
                let facts = self.executors.oracle().static_facts();
                let principal = self.cfg.principal.clone();
                let Some(slot) = self.nodes.get_mut(to.0 as usize) else {
                    return Err(internal_error!("no node {}", to.0).into());
                };
                let Some(d) = slot.driver.as_mut() else {
                    self.run.dropped += 1;
                    return Ok(());
                };
                // The client's session on this incarnation of the node (opened by its first message).
                let c = self
                    .clients
                    .get_mut(client)
                    .ok_or_else(|| internal_error!("no client {client}"))?;
                let session = match c.sessions.get(&to) {
                    Some((inc, s)) if *inc == slot.restarts => *s,
                    _ => {
                        let s = SessionId(slot.restarts << 32 | slot.next_session);
                        slot.next_session += 1;
                        slot.sessions.insert(s, client);
                        c.sessions.insert(to, (slot.restarts, s));
                        s
                    }
                };
                let admitted = d
                    .admits(&self.acl, facts, rel, Source::Session { principal: &principal })
                    .map_err(|e| SimError::Internal(internal_error!("node {} admission failed: {e}", to.0)))?;
                if admitted {
                    d.node.offer_ingress(Ingress { rel, session, row });
                } else {
                    self.run.dropped += 1;
                }
            }
            Envelope::ToClient { client, rel, row } => self.client_reply(client, rel, &row)?,
            Envelope::PipeConnect { pipe } => self.pipe_connect(pipe)?,
            Envelope::PipeOpened { pipe } => self.pipe_opened(pipe)?,
            Envelope::PipeBytes { pipe, to, bytes } => self.pipe_bytes(pipe, to, bytes)?,
            Envelope::PipeClosed { pipe, to, reason } => self.pipe_closed(pipe, to, &reason)?,
            Envelope::DialFailed {
                node,
                restarts,
                stream,
                req,
                reason,
            } => self.dial_failed_later(node, restarts, stream, req, &reason)?,
        }
        Ok(())
    }

    fn client_reply(&mut self, client: usize, rel: RelId, row: &Row) -> Result<(), SimError> {
        let protocol = self.protocol()?;
        let Some((req, reply)) = protocol.reply(rel, row) else {
            return Ok(());
        };
        let now = self.now;
        let think = self.cfg.think;
        let wake = now + self.rng.range(0, think);
        let retry_wake = now + self.rng.range(1_000_000, 20_000_000);
        let nodes = self.nodes.len();
        let pick = node_id(usize::try_from(self.rng.below(nodes as u64)).unwrap_or(0))?;
        let c = self
            .clients
            .get_mut(client)
            .ok_or_else(|| internal_error!("no client {client}"))?;
        let Some(p) = &c.pending else {
            return Ok(());
        };
        if p.req != req {
            return Ok(()); // a late reply to an operation already given up on
        }
        match reply {
            Reply::Done(output) => {
                let Some(p) = c.pending.take() else {
                    return Ok(());
                };
                self.run.history.push(Operation {
                    call: u64::try_from(p.call - EPOCH).unwrap_or(0),
                    ret: Some(u64::try_from(now - EPOCH).unwrap_or(0)),
                    input: p.op,
                    output: Some(output),
                });
                c.wake = wake;
            }
            Reply::Redirect(leader) => {
                // The request had no effect: resend the same operation (same call time) elsewhere.
                c.retry = Some(leader.unwrap_or(pick));
                c.wake = retry_wake;
            }
        }
        Ok(())
    }

    fn protocol(&self) -> Result<&dyn ClientProtocol, SimError> {
        Ok(&*self.protocol)
    }

    fn client_step(&mut self, client: usize) -> Result<(), SimError> {
        let now = self.now;
        let Some(c) = self.clients.get(client) else {
            return Err(internal_error!("no client {client}").into());
        };
        // Give up on an operation that took too long: it may or may not have happened.
        if let Some(p) = &c.pending
            && now >= p.deadline
        {
            let wake = now + self.rng.range(0, self.cfg.think);
            let nodes = self.nodes.len() as u64;
            let target = node_id(usize::try_from(self.rng.below(nodes)).unwrap_or(0))?;
            let c = self
                .clients
                .get_mut(client)
                .ok_or_else(|| internal_error!("no client"))?;
            if let Some(p) = c.pending.take() {
                self.run.history.push(Operation {
                    call: u64::try_from(p.call - EPOCH).unwrap_or(0),
                    ret: None,
                    input: p.op,
                    output: None,
                });
            }
            c.target = target;
            c.retry = None;
            c.wake = wake;
            return Ok(());
        }
        if now < c.wake || (self.clients_paused && c.pending.is_none()) {
            return Ok(());
        }
        // A redirected operation is resent; otherwise start a new one.
        let (op, req, to) = match (&c.pending, c.retry) {
            (Some(p), Some(to)) => (p.op.clone(), p.req, to),
            (Some(_), None) => return Ok(()),
            (None, _) => {
                let key = format!("k{}", self.rng.below(self.cfg.keys as u64)).into_bytes();
                let total = u64::from(self.cfg.mix.0 + self.cfg.mix.1 + self.cfg.mix.2);
                let pick = self.rng.below(total);
                let c = self
                    .clients
                    .get_mut(client)
                    .ok_or_else(|| internal_error!("no client"))?;
                c.next_req += 1;
                let req = c.next_req;
                let op = if pick < u64::from(self.cfg.mix.0) {
                    KvInput::Put {
                        key,
                        val: format!("c{client}-{req}").into_bytes(),
                    }
                } else if pick < u64::from(self.cfg.mix.0 + self.cfg.mix.1) {
                    KvInput::Get { key }
                } else {
                    KvInput::Delete { key }
                };
                c.pending = Some(Pending {
                    op: op.clone(),
                    call: now,
                    req,
                    deadline: now + self.cfg.timeout,
                });
                (op, req, c.target)
            }
        };
        let (rel, fields) = self
            .protocol()?
            .request(&op, req)
            .map_err(|e| internal_error!("client request: {e}"))?;
        let mut row = vec![Value::Node(to)];
        row.extend(fields);
        let delay = self.rng.range(self.cfg.latency.0, self.cfg.latency.1);
        let c = self
            .clients
            .get_mut(client)
            .ok_or_else(|| internal_error!("no client"))?;
        c.target = to;
        c.retry = None;
        c.wake = i64::MAX;
        self.schedule(
            delay,
            Envelope::FromClient {
                client,
                to,
                rel,
                row: Row::from(row),
            },
        );
        Ok(())
    }

    fn nemesis(&mut self) -> Result<(), SimError> {
        let n = self.nodes.len();
        #[derive(Clone, Copy)]
        enum Action {
            Crash,
            Isolate,
            Split,
            OneWay,
            Heal,
            DropStream,
        }
        let mut actions: Vec<Action> = Vec::new();
        if self.cfg.crashes {
            actions.push(Action::Crash);
        }
        if self.cfg.partitions {
            actions.extend([Action::Isolate, Action::Split, Action::OneWay, Action::Heal]);
        }
        if self.cfg.stream_drops {
            actions.push(Action::DropStream);
        }
        if actions.is_empty() || n == 0 {
            return Ok(());
        }
        let pick = actions
            .get(usize::try_from(self.rng.below(actions.len() as u64)).unwrap_or(0))
            .copied()
            .unwrap_or(Action::Heal);
        let victim = node_id(usize::try_from(self.rng.below(n as u64)).unwrap_or(0))?;
        match pick {
            Action::Crash => {
                // A kill -9 (or power loss), sometimes between a tick's WAL append and its sync; the node restarts
                // at once (a supervisor) or after a downtime. A down node is left alone.
                if self.nodes.get(victim.0 as usize).is_some_and(|s| s.driver.is_none()) {
                    return Ok(());
                }
                let mid_tick = self.rng.below(2) == 0;
                let down = if self.cfg.downtime > 0 && self.rng.below(2) == 0 {
                    self.now + self.rng.range(1, self.cfg.downtime)
                } else {
                    self.now
                };
                self.crash_node(victim, CrashWrites::Random, down, mid_tick)?;
                if down <= self.now {
                    self.note(format!("restart node {}", victim.0));
                    self.boot(victim, false)?;
                }
            }
            Action::Isolate => {
                let others: Vec<NodeId> = (0..n)
                    .filter_map(|o| node_id(o).ok())
                    .filter(|o| *o != victim)
                    .collect();
                self.partition(&[&[victim], &others])?;
                self.run.partitions += 1;
            }
            Action::Split => {
                // Every node lands on one side at random (either side may hold a majority, or everyone).
                let mut sides: [Vec<NodeId>; 2] = [Vec::new(), Vec::new()];
                for o in 0..n {
                    let side = usize::from(self.rng.below(2) == 1);
                    if let Some(g) = sides.get_mut(side) {
                        g.push(node_id(o)?);
                    }
                }
                let [a, b] = &sides;
                self.partition(&[a, b])?;
                self.run.partitions += 1;
            }
            Action::OneWay => {
                // Cuts a few links one way, on top of what is already cut.
                let cuts = 1 + self.rng.below(n as u64);
                for _ in 0..cuts {
                    let from = node_id(usize::try_from(self.rng.below(n as u64)).unwrap_or(0))?;
                    let to = node_id(usize::try_from(self.rng.below(n as u64)).unwrap_or(0))?;
                    if from != to {
                        self.cut(from, to)?;
                    }
                }
                self.run.partitions += 1;
            }
            Action::Heal => self.heal(),
            Action::DropStream => self.stream_drop()?,
        }
        Ok(())
    }
}

/// A node's failed tick as the run's error: the program's own error (BLSRnnn) is reported as the node's, like the
/// runtime halting the node; a missing feature as unimplemented; anything else (the evaluator's or the node's
/// machinery failing) is a bug in Blossom.
fn node_failure(n: NodeId, tick: blossom_value::time::Tick, e: blossom_node::NodeError) -> SimError {
    use blossom_ir::tick::EvalError;
    match e {
        blossom_node::NodeError::Eval(error @ EvalError::Program { .. }) => SimError::Node {
            node: n,
            tick,
            error,
            also: Vec::new(),
        },
        blossom_node::NodeError::Eval(EvalError::Unimplemented(u)) | blossom_node::NodeError::Unimplemented(u) => {
            SimError::Unimplemented(u)
        }
        other => SimError::Internal(internal_error!("node {} failed: {other}", n.0)),
    }
}
