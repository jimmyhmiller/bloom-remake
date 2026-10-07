//! The production driver for one node (ARCHITECTURE §5.2), on std threads:
//!
//! - the **engine thread** owns the [`Node`]: it takes what the I/O threads received, admits it, runs ticks, hands
//!   each tick's WAL record to the committer, and releases ticks as the committer reports them synced (Invariant R
//!   lives in the node), sending their frames to peers and sessions;
//! - the **committer thread** owns the WAL: it takes every submitted record, appends them as one batch, syncs once,
//!   and reports the synced tick (Invariant B: batch k+1 is never written before batch k's sync returned). A failed
//!   append or sync poisons the WAL and faults the node;
//! - the **checkpoint thread** writes a checkpoint of the durable rows at a synced tick, installs it, removes the
//!   older ones, and hands the truncation token to the committer;
//! - **listeners** accept peer and client connections; a **reader thread** per connection decodes batches, and a
//!   **writer thread** per peer and per session sends frames.
//!
//! Tick `t+1` computes while tick `t`'s fsync is in flight (pipelined group commit, ARCH-10).
//!
//! Two queues reach the engine. Control (sync reports, checkpoint results, stop) is unbounded and always drained.
//! Data (peer deliveries, client messages) is bounded: the engine takes data only while the node's inbox has room, so
//! a full queue blocks the readers and pushes back on the senders' TCP connections (DIST-008).
//!
//! The architecture puts I/O on tokio; this build uses blocking std threads (one reader per connection), which is
//! enough for a handful of nodes and the clients of a benchmark. Admission runs on the engine thread rather than in
//! the reader: it needs the node's committed state for `principal in REL` ACLs.

use std::collections::{BTreeMap, VecDeque};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{RelId, RoleId, internal_error};
use blossom_node::acl::{AclTable, Source};
use blossom_node::durable::{DurableCodec, DurableSchema};
use blossom_node::env::{Clock, Entropy};
use blossom_node::recovery::{self, StoreSpec, tick_record};
use blossom_node::{Backend, Executor, Executors, Node, NodeConfig, NodeState, ReleasedTick};
use blossom_oracle::{Delivery, Ingress, Oracle, Row};
use blossom_store::{
    CheckpointWriter, FileCheckpoints, FileWal, MetaRecord, MetaStore, OpenMode, RealFs, StoreIdentity, StoreLock,
    SyncedTick, TruncateToken, WalRecordBuf, WalWriter,
};
use blossom_value::Seed;
use blossom_value::time::{Instant, NodeId, Tick};
use blossom_value::value::SessionId;
use blossom_wire::frame::{Frame, Peer, RejectReason};

use crate::RuntimeError;
use crate::clock::{OsEntropy, Stopwatch, SystemClock, wall_now};
use crate::deploy::DeploymentSpec;
use crate::members::{Host, MemberEvent, MemberLinks, WebCtx};
use crate::net::{self, Catalog, Conn, Identity};
use crate::streams::{Env as StreamEnv, StreamConns, StreamData, StreamQueue, StreamStats};
use blossom_node::streams::{HostRequest, Observed, host_request};

/// How long a new connection has to complete its handshake, and a peer writer to write a frame.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// How to start a node.
#[derive(Clone)]
pub struct ServerConfig {
    pub spec: DeploymentSpec,
    /// The program compiled for the spec's nodes.
    pub artifact: Arc<BlsArtifact>,
    pub node: String,
    pub mode: OpenMode,
    /// Where the node's store lives (default: `<data_dir>/<node>`).
    pub dir: Option<PathBuf>,
    /// The evaluator the node runs.
    pub backend: Backend,
    /// The host functions the program's `extern fn`s call (the standard library's, for `blossom run`).
    pub externs: Arc<blossom_value::ExternRegistry>,
    /// Record every tick's inputs to a trace in this directory, `<node>-<incarnation>.blstrace` (ARCHITECTURE
    /// §6.4), for `blossom trace` to replay and question.
    pub record: Option<PathBuf>,
    /// Serve the page and client members' links (docs/design/CLIENTS.md §4).
    pub web: Option<WebConfig>,
}

/// `blossom run --web`: where to listen, and the page's files. The node makes `/blossom/app.json` and each client
/// role's artifact (docs/design/CLIENTS.md §8) itself.
#[derive(Clone, Debug)]
pub struct WebConfig {
    pub addr: SocketAddr,
    pub root: Option<PathBuf>,
}

/// Counters of what the node did and dropped.
#[derive(Debug, Default)]
pub struct Stats {
    pub ticks: AtomicU64,
    pub wal_records: AtomicU64,
    pub wal_batches: AtomicU64,
    pub released: AtomicU64,
    pub delivered: AtomicU64,
    pub ingress: AtomicU64,
    pub egress: AtomicU64,
    pub sessions: AtomicU64,
    pub checkpoints: AtomicU64,
    /// The time WAL syncs took, in total and at most, and how many took over 4, 16 and 64 ms.
    pub wal_sync_nanos: AtomicU64,
    pub wal_sync_max_nanos: AtomicU64,
    pub wal_syncs_over_4ms: AtomicU64,
    pub wal_syncs_over_16ms: AtomicU64,
    pub wal_syncs_over_64ms: AtomicU64,
    /// The time checkpoints took (written, installed, pruned, their logged blobs made files), in total.
    pub checkpoint_nanos: AtomicU64,
    /// How long messages from peers, and clients' stream data, waited between their reader queueing them and the
    /// engine taking them: at most, and how many waited over 4 ms.
    pub peer_wait_max_nanos: AtomicU64,
    pub peer_waits_over_4ms: AtomicU64,
    pub stream_wait_max_nanos: AtomicU64,
    pub stream_waits_over_4ms: AtomicU64,
    /// Blobs deleted after checkpoints (no row can reach them).
    pub blobs_collected: AtomicU64,
    /// Rejected by an ACL (an omission, SEM-090).
    pub rejected_acl: AtomicU64,
    /// Addressed to another node.
    pub rejected_unknown_dest: AtomicU64,
    /// On a channel whose schema differs between the ends.
    pub rejected_schema: AtomicU64,
    /// Replies to a session that had closed.
    pub dropped_closed_session: AtomicU64,
    /// Frames dropped because a peer's or session's queue was full (an omission).
    pub dropped_queue_full: AtomicU64,
    /// Rows too large for any frame (an omission).
    pub dropped_oversized: AtomicU64,
    /// Client members' connections (docs/design/CLIENTS.md §3).
    pub members: AtomicU64,
    /// A client member's messages addressed to another node than the one it is connected to.
    pub dropped_unroutable: AtomicU64,
    /// Web requests and member links that failed (a malformed request, a refused or broken link).
    pub web_failures: AtomicU64,
}

impl Stats {
    /// Every counter, by name, as read now (each read on its own: the counters move while they are read).
    pub fn snapshot(&self) -> Vec<(&'static str, u64)> {
        let r = |c: &AtomicU64| c.load(Ordering::Relaxed);
        vec![
            ("ticks", r(&self.ticks)),
            ("wal_records", r(&self.wal_records)),
            ("wal_batches", r(&self.wal_batches)),
            ("released", r(&self.released)),
            ("delivered", r(&self.delivered)),
            ("ingress", r(&self.ingress)),
            ("egress", r(&self.egress)),
            ("sessions", r(&self.sessions)),
            ("checkpoints", r(&self.checkpoints)),
            ("wal_sync_nanos", r(&self.wal_sync_nanos)),
            ("wal_sync_max_nanos", r(&self.wal_sync_max_nanos)),
            ("wal_syncs_over_4ms", r(&self.wal_syncs_over_4ms)),
            ("wal_syncs_over_16ms", r(&self.wal_syncs_over_16ms)),
            ("wal_syncs_over_64ms", r(&self.wal_syncs_over_64ms)),
            ("checkpoint_nanos", r(&self.checkpoint_nanos)),
            ("peer_wait_max_nanos", r(&self.peer_wait_max_nanos)),
            ("peer_waits_over_4ms", r(&self.peer_waits_over_4ms)),
            ("stream_wait_max_nanos", r(&self.stream_wait_max_nanos)),
            ("stream_waits_over_4ms", r(&self.stream_waits_over_4ms)),
            ("blobs_collected", r(&self.blobs_collected)),
            ("rejected_acl", r(&self.rejected_acl)),
            ("rejected_unknown_dest", r(&self.rejected_unknown_dest)),
            ("rejected_schema", r(&self.rejected_schema)),
            ("dropped_closed_session", r(&self.dropped_closed_session)),
            ("dropped_queue_full", r(&self.dropped_queue_full)),
            ("dropped_oversized", r(&self.dropped_oversized)),
            ("members", r(&self.members)),
            ("dropped_unroutable", r(&self.dropped_unroutable)),
            ("web_failures", r(&self.web_failures)),
        ]
    }
}

/// Notes how long a queued message waited: the largest wait, and the count over 4 ms.
fn note_wait(max: &AtomicU64, over: &AtomicU64, nanos: u64) {
    max.fetch_max(nanos, Ordering::Relaxed);
    if nanos > 4_000_000 {
        over.fetch_add(1, Ordering::Relaxed);
    }
}

fn bump(c: &AtomicU64, n: u64) {
    c.fetch_add(n, Ordering::Relaxed);
}

/// Why the engine stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    /// `stop()` was called.
    Stopped,
    /// The program wrote `halt`.
    Halted,
}

/// A message received from the network, before admission.
enum Data {
    Deliver {
        from: NodeId,
        rel: RelId,
        row: Row,
    },
    Ingress {
        session: SessionId,
        principal: Arc<str>,
        rel: RelId,
        row: Row,
    },
    /// A client member's link (docs/design/CLIENTS.md §3).
    Member(MemberEvent),
}

/// What else reaches the engine thread.
enum Control {
    Synced(SyncedTick),
    WalFailed(String),
    /// A checkpoint was installed; the shape of the chain it ends.
    CheckpointDone(Result<Option<blossom_store::ChainInfo>, String>),
    /// Data was queued.
    Wake,
    Stop,
}

/// What the checkpoint thread writes (FOREIGN-PROTOCOLS §6): a delta layer (encoded on the engine thread: its cost
/// follows the change), or a full image, encoded on the checkpoint thread so the engine only copies row handles.
enum CheckpointJob {
    Layer(Vec<u8>),
    Full(blossom_node::durable::DurableImage),
}

enum Commit {
    Append {
        tick: Tick,
        now: Instant,
        payload: Vec<u8>,
        /// The blobs the record references that are not durable yet: made durable before the record syncs.
        blobs: Vec<(blossom_value::BlobRef, Arc<[u8]>)>,
    },
    Truncate(TruncateToken),
}

/// The bounded data queue between the readers and the engine.
struct DataQueue {
    /// Each message with how long it has waited.
    q: Mutex<VecDeque<(Stopwatch, Data)>>,
    not_full: Condvar,
    cap: usize,
    closed: AtomicBool,
    /// Whether a `Wake` is already on its way to the engine.
    wake_pending: AtomicBool,
    control: Sender<Control>,
    stats: Arc<Stats>,
}

impl DataQueue {
    /// Queues `d`, waiting while the queue is full. False once the engine has stopped.
    fn push(&self, d: Data) -> bool {
        let Ok(mut q) = self.q.lock() else {
            return false;
        };
        while q.len() >= self.cap && !self.closed.load(Ordering::SeqCst) {
            q = match self.not_full.wait_timeout(q, Duration::from_millis(100)) {
                Ok((q, _)) => q,
                Err(_) => return false,
            };
        }
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        q.push_back((Stopwatch::start(), d));
        drop(q);
        if !self.wake_pending.swap(true, Ordering::SeqCst) {
            // The engine may be gone; the closed flag reports that on the next push.
            let _ = self.control.send(Control::Wake);
        }
        true
    }

    /// Takes up to `max` messages.
    fn take(&self, max: usize) -> Vec<Data> {
        self.wake_pending.store(false, Ordering::SeqCst);
        let Ok(mut q) = self.q.lock() else {
            return Vec::new();
        };
        let n = max.min(q.len());
        let out: Vec<Data> = q
            .drain(..n)
            .map(|(waited, d)| {
                note_wait(
                    &self.stats.peer_wait_max_nanos,
                    &self.stats.peer_waits_over_4ms,
                    waited.nanos(),
                );
                d
            })
            .collect();
        self.not_full.notify_all();
        out
    }

    fn len(&self) -> usize {
        self.q.lock().map(|q| q.len()).unwrap_or(0)
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.not_full.notify_all();
    }
}

/// The open connections, so that stopping can close them; each connection's thread removes its own on exit.
#[derive(Default)]
struct Conns {
    next: AtomicU64,
    open: Mutex<BTreeMap<u64, TcpStream>>,
}

impl Conns {
    fn add(&self, s: &TcpStream) -> Option<u64> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let clone = s.try_clone().ok()?;
        self.open.lock().ok()?.insert(id, clone);
        Some(id)
    }

    fn remove(&self, id: Option<u64>) {
        if let (Some(id), Ok(mut open)) = (id, self.open.lock()) {
            open.remove(&id);
        }
    }

    fn close_all(&self) {
        if let Ok(open) = self.open.lock() {
            for c in open.values() {
                // Closing is best-effort: a connection may already be gone.
                let _ = c.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

type Sessions = Arc<Mutex<BTreeMap<SessionId, SyncSender<Vec<u8>>>>>;

/// A running node.
pub struct Server {
    engine: Option<JoinHandle<Result<Stopped, RuntimeError>>>,
    control: Sender<Control>,
    data: Arc<DataQueue>,
    stop: Arc<AtomicBool>,
    conns: Arc<Conns>,
    threads: Vec<JoinHandle<()>>,
    pub stats: Arc<Stats>,
    pub node: NodeId,
    pub peer_addr: SocketAddr,
    pub client_addr: Option<SocketAddr>,
    /// Where `--web` serves the page and client links.
    pub web_addr: Option<SocketAddr>,
    /// The address each `listen` stream accepts on, by stream name.
    pub stream_addrs: BTreeMap<String, SocketAddr>,
    pub stream_stats: Arc<StreamStats>,
    streams: Arc<StreamConns>,
    stream_queue: Arc<StreamQueue>,
    /// The tick this incarnation booted at, and its restart count.
    pub boot_tick: Tick,
    pub restarts: u64,
}

/// The store identity the deployment expects for a node.
pub fn store_identity(
    spec: &DeploymentSpec,
    artifact: &BlsArtifact,
    node: &str,
) -> Result<StoreIdentity, RuntimeError> {
    let (_, entry) = spec.node(node)?;
    Ok(StoreIdentity {
        store_uuid: [0; 16],
        deployment_id: spec.deployment_id(),
        program_id: artifact.program.get().meta.program_id,
        node_name: entry.name.as_str().into(),
        principal: entry.principal.as_str().into(),
        format: recovery::FORMAT,
        directory_digest: spec.directory_digest(),
    })
}

/// The connection identity of a deployment's program.
pub fn identity(spec: &DeploymentSpec, artifact: &BlsArtifact) -> Identity {
    Identity {
        deployment: spec.deployment_id(),
        program_id: artifact.program.get().meta.program_id,
        program_version: artifact.program.get().meta.version,
        directory: spec.directory_digest(),
    }
}

/// Errors after startup are faults (restart from durable state), whatever layer they came from; only bugs and
/// missing features keep their own kind.
fn as_fault(e: RuntimeError) -> RuntimeError {
    match e {
        RuntimeError::Fault(_) | RuntimeError::Internal(_) | RuntimeError::Unimplemented(_) => e,
        other => RuntimeError::Fault(other.to_string()),
    }
}

impl Server {
    /// Opens the node's store (recovering it), binds its listeners and starts its threads.
    pub fn start(cfg: ServerConfig) -> Result<Server, RuntimeError> {
        let spec = &cfg.spec;
        let artifact = cfg.artifact.clone();
        let program = artifact.program.get();
        if spec.program != program.meta.name.as_str() || spec.version != program.meta.version {
            return Err(RuntimeError::Config(format!(
                "the deployment runs program {} version {}, the source is {} version {}",
                spec.program, spec.version, program.meta.name, program.meta.version
            )));
        }
        let names = spec.names();
        if names.len() != artifact.nodes.len() || names.iter().zip(&artifact.nodes).any(|(a, b)| **a != *b.as_str()) {
            return Err(internal_error!("the program was compiled for other nodes than the deployment's").into());
        }
        let (me, entry) = spec.node(&cfg.node)?;
        let role = artifact.roles.get(me.0 as usize).copied().flatten();
        let seed: Seed = spec.seed()?;
        let executors = Executors::new(
            cfg.backend,
            artifact.program.clone(),
            artifact.roles.clone(),
            names.to_vec(),
            seed,
            cfg.externs.clone(),
        )?;
        let oracle = executors.oracle().clone();
        let nonce = OsEntropy.boot_nonce().map_err(RuntimeError::Config)?;
        let dir = cfg.dir.clone().unwrap_or_else(|| spec.data_dir.join(&entry.name));
        let opened = recovery::open(
            Arc::new(RealFs),
            &StoreSpec {
                dir: dir.clone(),
                identity: store_identity(spec, &artifact, &cfg.node)?,
                mode: cfg.mode,
                certification: spec.tail_certification,
            },
            program,
            names.clone(),
            wall_now().map_err(RuntimeError::Config)?,
            nonce,
        )?;
        // The node's durable blobs (FOREIGN-PROTOCOLS §5), opened by recovery.
        let blob_store = opened.blobs.clone();
        let mut ncfg = NodeConfig::new(me, role);
        ncfg.halt = artifact.halt;
        ncfg.max_stream_bytes = spec.stream_limits.max_stream_bytes;
        ncfg.statics = spec.static_rows(program, &names)?;
        let inbox_cap = ncfg.max_batch.saturating_mul(4);
        let boot = opened.boot.clone();
        let exec = executors.make(me)?;
        let exec: Box<dyn blossom_node::Executor> = match &cfg.record {
            None => exec,
            Some(dir) => {
                std::fs::create_dir_all(dir)
                    .map_err(|e| RuntimeError::Config(format!("the trace directory {}: {e}", dir.display())))?;
                let path = dir.join(format!("{}-{}.blstrace", entry.name, boot.incarnation));
                let header = blossom_trace::node::NodeTraceHeader {
                    format: blossom_trace::node::FORMAT,
                    program: program.meta.name.as_str().into(),
                    version: program.meta.version,
                    digest: artifact.program.digest().0,
                    nodes: names.to_vec(),
                    node: me,
                    incarnation: boot.incarnation,
                    seed: seed.0,
                };
                let file = blossom_trace::node::create_trace_file(&path)
                    .map_err(|e| RuntimeError::Config(format!("the trace {}: {e}", path.display())))?;
                let sink: Box<dyn std::io::Write + Send> = Box::new(std::io::BufWriter::new(file));
                Box::new(blossom_node::record::Recording::new(sink, &header, exec)?)
            }
        };
        let node = Node::boot(ncfg, &artifact.program, exec, boot.clone())?;
        let restarts = opened.record.restarts;
        let last_checkpoint_lsn = opened.checkpoint.map_or(0, |c| c.lsn.0);
        let opened_chain = opened.checkpoints.chain()?;

        let peer_listener =
            TcpListener::bind(entry.addr).map_err(|e| RuntimeError::Net(format!("bind {}: {e}", entry.addr)))?;
        let peer_addr = peer_listener.local_addr().map_err(RuntimeError::Io)?;
        let client_listener = match entry.client_addr {
            Some(a) => Some(TcpListener::bind(a).map_err(|e| RuntimeError::Net(format!("bind {a}: {e}")))?),
            None => None,
        };
        let client_addr = match &client_listener {
            Some(l) => Some(l.local_addr().map_err(RuntimeError::Io)?),
            None => None,
        };
        let web_listener = match &cfg.web {
            Some(w) => Some(TcpListener::bind(w.addr).map_err(|e| RuntimeError::Net(format!("bind {}: {e}", w.addr)))?),
            None => None,
        };
        let web_addr = match &web_listener {
            Some(l) => Some(l.local_addr().map_err(RuntimeError::Io)?),
            None => None,
        };

        // Every `listen` stream at this node has an address in the deployment, and every address names one.
        let mut stream_listeners = Vec::new();
        let mut stream_addrs = BTreeMap::new();
        for (i, st) in node.streams().iter().enumerate() {
            if st.kind != blossom_ir::core::StreamKind::Listen {
                continue;
            }
            let Some(addr) = entry.streams.get(&*st.name) else {
                return Err(RuntimeError::Config(format!(
                    "node {}: the listen stream `{}` has no address (`streams = {{ {} = \"host:port\" }}`)",
                    entry.name, st.name, st.name
                )));
            };
            let l = TcpListener::bind(addr).map_err(|e| RuntimeError::Net(format!("bind {addr}: {e}")))?;
            stream_addrs.insert(st.name.to_string(), l.local_addr().map_err(RuntimeError::Io)?);
            stream_listeners.push((i, l));
        }
        if let Some(name) = entry.streams.keys().find(|n| !stream_addrs.contains_key(*n)) {
            return Err(RuntimeError::Config(format!(
                "node {}: `streams` names `{name}`, which is not a listen stream of this node",
                entry.name
            )));
        }

        let stats = Arc::new(Stats::default());
        let stop = Arc::new(AtomicBool::new(false));
        let conns = Arc::new(Conns::default());
        let sessions: Sessions = Arc::new(Mutex::new(BTreeMap::new()));
        let (ctl_tx, ctl_rx) = mpsc::channel::<Control>();
        let data = Arc::new(DataQueue {
            q: Mutex::new(VecDeque::new()),
            not_full: Condvar::new(),
            cap: inbox_cap,
            closed: AtomicBool::new(false),
            wake_pending: AtomicBool::new(false),
            control: ctl_tx.clone(),
            stats: stats.clone(),
        });
        let stream_stats = Arc::new(StreamStats::default());
        let streams = Arc::new(StreamConns::new(restarts, spec.stream_limits, stream_stats.clone()));
        let stream_queue = Arc::new(StreamQueue::new(
            {
                let control = ctl_tx.clone();
                // The engine may be gone; the closed queue reports that on the next push.
                Box::new(move || {
                    let _ = control.send(Control::Wake);
                })
            },
            {
                let stats = stats.clone();
                Box::new(move |nanos| note_wait(&stats.stream_wait_max_nanos, &stats.stream_waits_over_4ms, nanos))
            },
        ));
        let stream_env = StreamEnv {
            conns: streams.clone(),
            queue: stream_queue.clone(),
            stop: stop.clone(),
        };
        let (commit_tx, commit_rx) = mpsc::channel::<Commit>();
        let (ckpt_tx, ckpt_rx) = mpsc::channel::<(CheckpointJob, SyncedTick)>();
        let id = identity(spec, &artifact);
        let catalog = Arc::new(Catalog::of(program)?);
        let mut threads = Vec::new();

        let recovery::Opened {
            wal,
            checkpoints,
            meta,
            record,
            lock,
            ..
        } = opened;
        {
            let (tx, stats) = (ctl_tx.clone(), stats.clone());
            let blobs = blob_store.clone();
            threads.push(spawn("committer", move || {
                committer(wal, &blobs, commit_rx, tx, stats)
            })?);
        }
        {
            let (tx, commit, stats) = (ctl_tx.clone(), commit_tx.clone(), stats.clone());
            let (artifact, names, blobs) = (artifact.clone(), names.clone(), blob_store.clone());
            threads.push(spawn("checkpoint", move || {
                checkpointer(checkpoints, &artifact, names, &blobs, ckpt_rx, commit, tx, &stats)
            })?);
        }
        // Peer writers: one per other node.
        let mut peers: BTreeMap<NodeId, SyncSender<Vec<u8>>> = BTreeMap::new();
        for (i, n) in spec.nodes.iter().enumerate() {
            let to = NodeId(u32::try_from(i).map_err(|_| RuntimeError::Config("too many nodes".into()))?);
            if to == me {
                continue;
            }
            let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(4096);
            peers.insert(to, tx);
            let addr = entry.dial.get(&n.name).copied().unwrap_or(n.addr);
            let (id, catalog, stop) = (id.clone(), catalog.clone(), stop.clone());
            threads.push(spawn("peer-writer", move || {
                peer_writer(addr, id, me, restarts, nonce, catalog, rx, stop)
            })?);
        }
        // Listeners.
        {
            let ctx = Accept {
                artifact: artifact.clone(),
                id: id.clone(),
                me,
                restarts,
                nonce,
                catalog: catalog.clone(),
                data: data.clone(),
                stop: stop.clone(),
                conns: conns.clone(),
                stats: stats.clone(),
                sessions: sessions.clone(),
                next_session: Arc::new(AtomicU64::new(0)),
                incarnations: Arc::new(Mutex::new(BTreeMap::new())),
                spec: Arc::new(spec.clone()),
            };
            let c = ctx.clone();
            threads.push(spawn("peer-listener", move || accept_loop(peer_listener, c, false))?);
            if let Some(l) = client_listener {
                threads.push(spawn("client-listener", move || accept_loop(l, ctx, true))?);
            }
        }
        if let (Some(l), Some(w)) = (web_listener, &cfg.web) {
            // Each client role's part of the program, projected once: what its pages run, and the digest their links
            // present. A projection that would show a page anything placed at another role is refused.
            let mut client_roles = BTreeMap::new();
            for (role, r) in program.roles.iter_enumerated() {
                if r.kind != blossom_ir::core::RoleKind::Client {
                    continue;
                }
                let name = r.name.to_string();
                let client = blossom_artifact::client::ClientArtifact::project(&artifact, &name)
                    .map_err(|e| RuntimeError::Config(e.to_string()))?;
                let leaks = client.leaks(&artifact);
                if !leaks.is_empty() {
                    return Err(RuntimeError::Config(format!(
                        "the part of the program `{name}`'s pages run would show them: {}",
                        leaks.join("; ")
                    )));
                }
                let bytes = client.encode().map_err(|e| RuntimeError::Config(e.to_string()))?;
                client_roles.insert(
                    name,
                    crate::members::ClientRole {
                        id: role,
                        part: client.part(),
                        artifact: Arc::from(bytes),
                    },
                );
            }
            let names: Vec<String> = client_roles.keys().cloned().collect();
            let app = crate::web::app_json(spec, &names, &cfg.node).map_err(RuntimeError::Config)?;
            let registry = blossom_store::ClientRegistry::open(Arc::new(RealFs), &dir)?;
            let queue = data.clone();
            let ctx = WebCtx {
                artifact: artifact.clone(),
                id: id.clone(),
                me,
                restarts,
                nonce,
                catalog: catalog.clone(),
                post: Arc::new(move |e| queue.push(Data::Member(e))),
                registry: Arc::new(Mutex::new(registry)),
                app: Arc::from(app.as_str()),
                root: w.root.clone(),
                next_conn: Arc::new(AtomicU64::new(0)),
                client_roles: Arc::new(client_roles),
            };
            let (stop, conns, stats) = (stop.clone(), conns.clone(), stats.clone());
            threads.push(spawn("web-listener", move || {
                web_accept_loop(l, ctx, stop, conns, stats)
            })?);
        }
        for (i, l) in stream_listeners {
            let env = stream_env.clone();
            threads.push(spawn("stream-listener", move || {
                crate::streams::listen_loop(l, i, env)
            })?);
        }
        let engine = {
            let e = Engine {
                streams: stream_env.clone(),
                backlog_bytes: spec.stream_limits.backlog_bytes,
                blob_store: blob_store.clone(),
                checkpoint_blobs: None,
                node,
                artifact: artifact.clone(),
                schema: DurableSchema::of(program),
                names,
                oracle,
                acl: AclTable::of(program),
                catalog,
                me,
                roles: artifact.roles.clone(),
                principals: spec.nodes.iter().map(|n| Arc::from(n.principal.as_str())).collect(),
                peers,
                sessions,
                members: MemberLinks::of(program),
                seed,
                data: data.clone(),
                inbox_cap,
                commit: commit_tx,
                checkpoint: ckpt_tx,
                checkpoint_bytes: spec.checkpoint_wal_bytes,
                checkpoint_busy: false,
                last_checkpoint_lsn,
                chain: opened_chain,
                meta,
                record,
                _lock: lock,
                clock: SystemClock::anchored_at(boot.now),
                stats: stats.clone(),
            };
            let (data, env) = (data.clone(), stream_env.clone());
            std::thread::Builder::new()
                .name("engine".into())
                .stack_size(blossom_ir::depth::EVAL_STACK_BYTES)
                .spawn(move || {
                    let r = e.run(ctl_rx).map_err(as_fault);
                    // Readers blocked on a full queue give up, and the streams stop taking connections: a halted or
                    // faulted node serves none.
                    data.close();
                    env.queue.close();
                    env.stop.store(true, Ordering::SeqCst);
                    env.conns.close_all();
                    r
                })
                .map_err(RuntimeError::Io)?
        };
        Ok(Server {
            engine: Some(engine),
            control: ctl_tx,
            data,
            stop,
            conns,
            threads,
            stats,
            node: me,
            peer_addr,
            client_addr,
            web_addr,
            stream_addrs,
            stream_stats,
            streams,
            stream_queue,
            boot_tick: boot.tick,
            restarts,
        })
    }

    /// Stops the node: the engine stops scheduling ticks, and connections close. Anything not yet released is
    /// never released (crash semantics, which are always legal).
    pub fn stop(mut self) -> Result<Stopped, RuntimeError> {
        // The engine may already have stopped (halted or faulted) and dropped its receiver.
        let _ = self.control.send(Control::Stop);
        self.wait_engine()
    }

    /// Waits for the engine to stop by itself (halt or fault).
    pub fn wait(mut self) -> Result<Stopped, RuntimeError> {
        self.wait_engine()
    }

    fn wait_engine(&mut self) -> Result<Stopped, RuntimeError> {
        let result = match self.engine.take() {
            Some(h) => h
                .join()
                .map_err(|_| RuntimeError::Internal(internal_error!("the engine thread panicked")))?,
            None => Err(internal_error!("the engine was already joined").into()),
        };
        self.shutdown_io();
        result
    }

    fn shutdown_io(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.data.close();
        self.stream_queue.close();
        self.conns.close_all();
        self.streams.close_all();
        for t in self.threads.drain(..) {
            // A thread that panicked has already reported through its connection; joining just reaps it.
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.engine.is_some() {
            let _ = self.control.send(Control::Stop);
            let _ = self.wait_engine();
        }
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, RuntimeError> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map_err(RuntimeError::Io)
}

/// The committer (Invariant B): append everything submitted as one batch, sync once, report.
fn committer(
    mut wal: FileWal,
    blob_store: &blossom_store::BlobStore,
    rx: Receiver<Commit>,
    tx: Sender<Control>,
    stats: Arc<Stats>,
) {
    let mut batch: u64 = 0;
    let mut truncates: Vec<TruncateToken> = Vec::new();
    while let Ok(first) = rx.recv() {
        let mut work = vec![first];
        while let Ok(more) = rx.try_recv() {
            work.push(more);
        }
        let mut appended = 0u64;
        for w in work {
            match w {
                Commit::Append {
                    tick,
                    now,
                    payload,
                    blobs,
                } => {
                    // Its blobs are durable once the record syncs: logged in it, or made durable as files first.
                    let record = match tick_record(blob_store, &blobs, payload) {
                        Ok(r) => r,
                        Err(e) => {
                            let _ = tx.send(Control::WalFailed(format!("the blobs of tick {} failed: {e}", tick.0)));
                            return;
                        }
                    };
                    if appended == 0 {
                        batch = batch.saturating_add(1);
                    }
                    let rec = WalRecordBuf {
                        batch,
                        tick: tick.0,
                        now: now.0,
                        kind: record.kind,
                        payload: record.payload,
                    };
                    let lsn = match wal.append(&rec) {
                        Ok(lsn) => lsn,
                        Err(e) => {
                            let _ = tx.send(Control::WalFailed(format!("WAL append of tick {} failed: {e}", tick.0)));
                            return;
                        }
                    };
                    if let Err(e) = blob_store.write_logged(&record.logged, lsn) {
                        let _ = tx.send(Control::WalFailed(format!("the blobs of tick {} failed: {e}", tick.0)));
                        return;
                    }
                    appended += 1;
                }
                Commit::Truncate(t) => truncates.push(t),
            }
        }
        if appended > 0 {
            let clock = Stopwatch::start();
            let result = wal.sync();
            let took = clock.nanos();
            bump(&stats.wal_sync_nanos, took);
            stats.wal_sync_max_nanos.fetch_max(took, Ordering::Relaxed);
            for (over, c) in [
                (4_000_000, &stats.wal_syncs_over_4ms),
                (16_000_000, &stats.wal_syncs_over_16ms),
                (64_000_000, &stats.wal_syncs_over_64ms),
            ] {
                if took > over {
                    bump(c, 1);
                }
            }
            match result {
                Ok(synced) => {
                    bump(&stats.wal_records, appended);
                    bump(&stats.wal_batches, 1);
                    if let Some(t) = synced.synced_tick()
                        && tx.send(Control::Synced(t)).is_err()
                    {
                        return;
                    }
                }
                Err(e) => {
                    // A failed sync must never be retried: the WAL is poisoned for the incarnation.
                    let _ = tx.send(Control::WalFailed(format!("WAL sync failed: {e}")));
                    return;
                }
            }
        }
        // Truncation happens at a batch boundary, after the sync.
        for t in truncates.drain(..) {
            if let Err(e) = wal.truncate_through(t) {
                let _ = tx.send(Control::WalFailed(format!("WAL truncation failed: {e}")));
                return;
            }
        }
    }
}

/// The checkpoint thread: write, install, remove the older checkpoints, hand the truncation token to the committer.
#[allow(clippy::too_many_arguments)]
fn checkpointer(
    mut ckpt: FileCheckpoints,
    artifact: &BlsArtifact,
    names: Arc<[Arc<str>]>,
    blobs: &blossom_store::BlobStore,
    rx: Receiver<(CheckpointJob, SyncedTick)>,
    commit: Sender<Commit>,
    tx: Sender<Control>,
    stats: &Stats,
) {
    let program = artifact.program.get();
    let schema = DurableSchema::of(program);
    let codec = DurableCodec::new(program, &schema, names);
    while let Ok((job, covers)) = rx.recv() {
        let clock = Stopwatch::start();
        let written = match job {
            CheckpointJob::Layer(delta) => ckpt.write_layer(&delta, covers).map_err(|e| e.to_string()),
            CheckpointJob::Full(image) => codec
                .encode_image(&image)
                .map_err(|e| e.to_string())
                .and_then(|snap| ckpt.write(snap, covers).map_err(|e| e.to_string())),
        };
        let result = written
            .and_then(|id| ckpt.install(id).map_err(|e| e.to_string()))
            .and_then(|token| ckpt.prune().map(|_| token).map_err(|e| e.to_string()))
            // The blobs logged in the WAL about to go are made durable as files first (here, off the tick path).
            .and_then(|token| {
                blobs
                    .sync_logged_below(token.lsn())
                    .map(|_| token)
                    .map_err(|e| e.to_string())
            })
            .and_then(|token| commit.send(Commit::Truncate(token)).map_err(|e| e.to_string()))
            .and_then(|()| ckpt.chain().map_err(|e| e.to_string()));
        bump(&stats.checkpoint_nanos, clock.nanos());
        if tx.send(Control::CheckpointDone(result)).is_err() {
            return;
        }
    }
}

/// A peer writer: connect (retrying), handshake, then send frames until the connection breaks, and reconnect.
/// Frames wait in the queue meanwhile (a delay, which channels allow); a full queue drops (an omission).
#[allow(clippy::too_many_arguments)]
fn peer_writer(
    addr: SocketAddr,
    id: Identity,
    me: NodeId,
    restarts: u64,
    nonce: u64,
    catalog: Arc<Catalog>,
    rx: Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
) {
    let mut backoff = Duration::from_millis(20);
    let mut pending: Option<Vec<u8>> = None;
    while !stop.load(Ordering::SeqCst) {
        let conn = TcpStream::connect_timeout(&addr, Duration::from_secs(1))
            .and_then(|s| {
                s.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
                s.set_write_timeout(Some(WRITE_TIMEOUT))?;
                Ok(s)
            })
            .map_err(RuntimeError::Io)
            .and_then(Conn::new)
            .and_then(|mut c| net::open_handshake(&mut c, &id, Peer::Node(me.0), restarts, nonce, &catalog).map(|_| c));
        let mut conn = match conn {
            Ok(c) => {
                backoff = Duration::from_millis(20);
                c
            }
            Err(_) => {
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(1));
                continue;
            }
        };
        loop {
            let frame = match pending.take() {
                Some(f) => f,
                None => match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(f) => f,
                    Err(RecvTimeoutError::Timeout) => {
                        if stop.load(Ordering::SeqCst) {
                            return;
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                },
            };
            use std::io::Write;
            let mut ok = conn.writer.write_all(&frame).is_ok();
            // Coalesce whatever else is queued into the same flush.
            while ok {
                match rx.try_recv() {
                    Ok(f) => ok = conn.writer.write_all(&f).is_ok(),
                    Err(_) => break,
                }
            }
            if !ok || conn.writer.flush().is_err() {
                // The frames written into the broken connection may be lost: an omission.
                break;
            }
        }
    }
}

#[derive(Clone)]
struct Accept {
    artifact: Arc<BlsArtifact>,
    spec: Arc<DeploymentSpec>,
    id: Identity,
    me: NodeId,
    restarts: u64,
    nonce: u64,
    catalog: Arc<Catalog>,
    data: Arc<DataQueue>,
    stop: Arc<AtomicBool>,
    conns: Arc<Conns>,
    stats: Arc<Stats>,
    sessions: Sessions,
    next_session: Arc<AtomicU64>,
    /// The newest incarnation (restart count) seen of each peer (ARCHITECTURE §5.8).
    incarnations: Arc<Mutex<BTreeMap<u32, u64>>>,
}

fn accept_loop(listener: TcpListener, ctx: Accept, clients: bool) {
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    while !ctx.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let configured = stream
                    .set_nonblocking(false)
                    .and_then(|()| stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)))
                    .and_then(|()| stream.set_write_timeout(Some(WRITE_TIMEOUT)));
                if configured.is_err() {
                    continue;
                }
                let registered = ctx.conns.add(&stream);
                let ctx = ctx.clone();
                let name = if clients { "session" } else { "peer-reader" };
                // A connection whose thread cannot start is dropped, which closes it.
                let _ = spawn(name, move || {
                    let _ = if clients {
                        session(stream, &ctx)
                    } else {
                        peer_reader(stream, &ctx)
                    };
                    ctx.conns.remove(registered);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn peer_reader(stream: TcpStream, ctx: &Accept) -> Result<(), RuntimeError> {
    let mut conn = Conn::new(stream)?;
    let nodes = ctx.spec.nodes.len();
    let hello = net::accept_handshake(
        &mut conn,
        &ctx.id,
        Peer::Node(ctx.me.0),
        ctx.restarts,
        ctx.nonce,
        &ctx.catalog,
        &|h| match h.peer {
            Peer::Node(n) if (n as usize) < nodes && n != ctx.me.0 => {
                // A HELLO from an older incarnation than one already seen is refused (`stale_incarnation`).
                let mut seen = ctx
                    .incarnations
                    .lock()
                    .map_err(|_| (RejectReason::Protocol, "internal: poisoned lock".to_string()))?;
                let newest = seen.entry(n).or_insert(h.restarts);
                if h.restarts < *newest {
                    return Err((
                        RejectReason::NotAllowed,
                        format!("stale incarnation {} of node {n}", h.restarts),
                    ));
                }
                *newest = h.restarts;
                Ok(())
            }
            _ => Err((
                RejectReason::NotAllowed,
                format!("{:?} is not a peer of this node", h.peer),
            )),
        },
    )?;
    conn.reader.get_ref().set_read_timeout(None).map_err(RuntimeError::Io)?;
    let Peer::Node(from) = hello.peer else {
        return Err(internal_error!("a peer HELLO without a node").into());
    };
    let from = NodeId(from);
    let program = ctx.artifact.program.get();
    let codec = net::wire_codec(program);
    while let Some(frame) = conn.read()? {
        let Frame::Batch(b) = frame else {
            continue;
        };
        let Some(&rel) = hello.inbound.get(&b.sid) else {
            bump(&ctx.stats.rejected_schema, b.count);
            continue;
        };
        for row in net::batch_rows(&codec, program, rel, &b)? {
            if !net::addressed_to(&row, ctx.me) {
                bump(&ctx.stats.rejected_unknown_dest, 1);
                continue;
            }
            if !ctx.data.push(Data::Deliver { from, rel, row }) {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn session(stream: TcpStream, ctx: &Accept) -> Result<(), RuntimeError> {
    let mut conn = Conn::new(stream)?;
    let hello = net::accept_handshake(
        &mut conn,
        &ctx.id,
        Peer::Node(ctx.me.0),
        ctx.restarts,
        ctx.nonce,
        &ctx.catalog,
        &|h| match &h.peer {
            Peer::Client { .. } => Ok(()),
            other => Err((RejectReason::NotAllowed, format!("{other:?} on the client listener"))),
        },
    )?;
    conn.reader.get_ref().set_read_timeout(None).map_err(RuntimeError::Io)?;
    let Peer::Client { principal } = hello.peer else {
        return Err(internal_error!("a client HELLO without a principal").into());
    };
    let principal: Arc<str> = principal.into();
    // Session ids never repeat across incarnations: the restart count is the high half.
    let n = ctx.next_session.fetch_add(1, Ordering::SeqCst);
    let session = SessionId(ctx.restarts << 32 | (n & 0xffff_ffff));
    bump(&ctx.stats.sessions, 1);
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(4096);
    let mut writer = conn.writer;
    let w = spawn("session-writer", move || {
        use std::io::Write;
        while let Ok(f) = rx.recv() {
            let mut ok = writer.write_all(&f).is_ok();
            while ok {
                match rx.try_recv() {
                    Ok(f) => ok = writer.write_all(&f).is_ok(),
                    Err(_) => break,
                }
            }
            if !ok || writer.flush().is_err() {
                return;
            }
        }
    })?;
    // Registered before any of its messages is queued, so a reply always finds it while it is open.
    if let Ok(mut s) = ctx.sessions.lock() {
        s.insert(session, tx);
    }
    let program = ctx.artifact.program.get();
    let codec = net::wire_codec(program);
    let mut reader = conn.reader;
    let result = (|| -> Result<(), RuntimeError> {
        while let Some(frame) = Frame::read(&mut reader, &conn.limits).map_err(|e| RuntimeError::Net(e.to_string()))? {
            let Frame::Batch(b) = frame else {
                continue;
            };
            let Some(&rel) = hello.inbound.get(&b.sid) else {
                bump(&ctx.stats.rejected_schema, b.count);
                continue;
            };
            for row in net::batch_rows(&codec, program, rel, &b)? {
                if !net::addressed_to(&row, ctx.me) {
                    bump(&ctx.stats.rejected_unknown_dest, 1);
                    continue;
                }
                let m = Data::Ingress {
                    session,
                    principal: principal.clone(),
                    rel,
                    row,
                };
                if !ctx.data.push(m) {
                    return Ok(());
                }
            }
        }
        Ok(())
    })();
    // Closing the session drops its sender, which ends the writer.
    if let Ok(mut s) = ctx.sessions.lock() {
        s.remove(&session);
    }
    let _ = w.join();
    result
}

/// The node and counters as a member link sees them.
struct MemberHost<'a> {
    node: &'a mut Node<Box<dyn Executor>>,
    acl: &'a AclTable,
    oracle: &'a Arc<Oracle>,
    stats: &'a Stats,
    me: NodeId,
    names: &'a [Arc<str>],
    seed: Seed,
}

impl Host for MemberHost<'_> {
    fn offer(&mut self, from: NodeId, role: RoleId, rel: RelId, row: Row) -> Option<u64> {
        let source = Source::Node {
            role: Some(role),
            principal: "",
        };
        if !self.node.admits(self.acl, self.oracle.static_facts(), rel, source) {
            bump(&self.stats.rejected_acl, 1);
            return None;
        }
        bump(&self.stats.delivered, 1);
        Some(self.node.offer_delivery(Delivery { rel, from, row }))
    }

    fn event(&mut self, rel: RelId, row: Row) {
        self.node.offer_input(rel, row);
    }

    fn me(&self) -> NodeId {
        self.me
    }

    fn dropped_unroutable(&self, n: u64) {
        bump(&self.stats.dropped_unroutable, n);
    }

    fn dropped_closed(&self, n: u64) {
        bump(&self.stats.dropped_closed_session, n);
    }

    fn rejected_schema(&self, n: u64) {
        bump(&self.stats.rejected_schema, n);
    }

    fn member_seed(&self, member: NodeId) -> Result<[u8; 16], String> {
        let name = format!("client {}", blossom_ir::printer::node_text(member, self.names));
        blossom_value::Seeds::derive(self.seed, &name)
            .map(|s| s.node.0)
            .map_err(|e| format!("deriving the seed of {name}: {e}"))
    }

    fn link_failed(&self) {
        bump(&self.stats.web_failures, 1);
    }
}

/// Accepts the web listener's connections, each served on a thread of its own ([`crate::members::web_conn`]), until
/// the node stops.
fn web_accept_loop(listener: TcpListener, ctx: WebCtx, stop: Arc<AtomicBool>, conns: Arc<Conns>, stats: Arc<Stats>) {
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let configured = stream
                    .set_nonblocking(false)
                    .and_then(|()| stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)))
                    .and_then(|()| stream.set_write_timeout(Some(WRITE_TIMEOUT)));
                if configured.is_err() {
                    continue;
                }
                let registered = conns.add(&stream);
                let (ctx, conns, stats) = (ctx.clone(), conns.clone(), stats.clone());
                // A connection whose thread cannot start is dropped, which closes it.
                let _ = spawn("web", move || {
                    // A failed request or link closes its connection and is counted.
                    if crate::members::web_conn(stream, &ctx).is_err() {
                        bump(&stats.web_failures, 1);
                    }
                    conns.remove(registered);
                });
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

struct Engine {
    node: Node<Box<dyn Executor>>,
    blob_store: Arc<blossom_store::BlobStore>,
    /// The checkpoint in progress: its tick and the blobs its rows hold.
    checkpoint_blobs: Option<(Tick, std::collections::BTreeSet<blossom_value::BlobRef>)>,
    streams: StreamEnv,
    /// The undelivered stream bytes past which the engine takes no more stream reports.
    backlog_bytes: u64,
    artifact: Arc<BlsArtifact>,
    schema: DurableSchema,
    names: Arc<[Arc<str>]>,
    oracle: Arc<Oracle>,
    acl: AclTable,
    catalog: Arc<Catalog>,
    me: NodeId,
    roles: Vec<Option<RoleId>>,
    principals: Vec<Arc<str>>,
    peers: BTreeMap<NodeId, SyncSender<Vec<u8>>>,
    sessions: Sessions,
    /// Client members' links (docs/design/CLIENTS.md §3).
    members: MemberLinks,
    /// The deployment's root seed (members' seeds derive from it).
    seed: Seed,
    data: Arc<DataQueue>,
    /// The most messages the node's inbox holds before the engine stops taking data.
    inbox_cap: usize,
    commit: Sender<Commit>,
    checkpoint: Sender<(CheckpointJob, SyncedTick)>,
    /// The shape of the installed checkpoint chain (`None` without one), from the last checkpoint.
    chain: Option<blossom_store::ChainInfo>,
    checkpoint_bytes: u64,
    checkpoint_busy: bool,
    last_checkpoint_lsn: u64,
    meta: MetaStore,
    record: MetaRecord,
    _lock: StoreLock,
    clock: SystemClock,
    stats: Arc<Stats>,
}

impl Engine {
    fn run(mut self, ctl: Receiver<Control>) -> Result<Stopped, RuntimeError> {
        let program = self.artifact.program.clone();
        let codec = net::wire_codec(program.get());
        let schema = self.schema.clone();
        let durable = DurableCodec::new(program.get(), &schema, self.names.clone());
        loop {
            // Wait for something to do: control, data the inbox has room for, or the next timer.
            let now = self.clock.now();
            let room = self.data_room();
            let wait = if self.node.ready(now)?
                || (room > 0 && self.data.len() > 0)
                || self.streams.queue.takeable(self.stream_budget())
            {
                Duration::ZERO
            } else if self.node.waiting() {
                // Only a sync report or a stop can help; a due timer cannot.
                Duration::from_secs(1)
            } else {
                match self.node.next_deadline()? {
                    Some(d) => Duration::from_nanos(u64::try_from(d.0.saturating_sub(now.0)).unwrap_or(0)),
                    None => Duration::from_secs(1),
                }
                .min(Duration::from_secs(1))
            };
            match ctl.recv_timeout(wait) {
                Ok(m) => {
                    if let Some(stopped) = self.control(m, &codec, &durable)? {
                        return Ok(stopped);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(internal_error!("the engine's inbox closed").into()),
            }
            if let Some(stopped) = self.drain(&ctl, &codec, &durable)? {
                return Ok(stopped);
            }
            if *self.node.state() == NodeState::Halted {
                return Ok(Stopped::Halted);
            }
            // Run ticks while ready (bounded by the node's in-flight limit).
            while self.node.ready(self.clock.now())? {
                let now = self.clock.now();
                let fx = self
                    .node
                    .run_tick(now)
                    .map_err(|f| RuntimeError::Fault(f.to_string()))?;
                bump(&self.stats.ticks, 1);
                if let Some(r) = fx.reserve {
                    self.record.reserved_tick = r.ticks.0;
                    self.record.last_now = self.record.last_now.max(r.now.0);
                    self.meta.write(&self.record)?;
                    let released = self.node.reserved(r)?;
                    self.dispatch(released, &codec)?;
                }
                match fx.wal {
                    Some(delta) => {
                        let payload = durable.encode_delta(&delta)?;
                        self.commit
                            .send(Commit::Append {
                                tick: fx.tick,
                                now: fx.now,
                                payload,
                                blobs: fx.blobs,
                            })
                            .map_err(|_| RuntimeError::Fault("the committer stopped".into()))?;
                    }
                    None => {
                        let released = self.node.release_ready()?;
                        self.dispatch(released, &codec)?;
                    }
                }
                if let Some(stopped) = self.drain(&ctl, &codec, &durable)? {
                    return Ok(stopped);
                }
            }
        }
    }

    /// Handles every queued control message, then takes the data the inbox has room for.
    fn drain(
        &mut self,
        ctl: &Receiver<Control>,
        codec: &blossom_wire::codec::Codec<'_>,
        durable: &DurableCodec<'_>,
    ) -> Result<Option<Stopped>, RuntimeError> {
        while let Ok(m) = ctl.try_recv() {
            if let Some(stopped) = self.control(m, codec, durable)? {
                return Ok(Some(stopped));
            }
        }
        let room = self.data_room();
        for d in self.data.take(room) {
            self.admit_and_offer(d)?;
        }
        for sd in self.streams.queue.take(self.stream_budget()) {
            let o = match sd {
                StreamData::Opened {
                    stream,
                    conn,
                    peer,
                    req,
                } => Observed::Opened {
                    stream,
                    conn,
                    peer,
                    req,
                    at: self.clock.now(),
                },
                StreamData::Bytes { conn, bytes, credit } => {
                    // The reader may read more of this connection now.
                    credit.release(bytes.len() as u64);
                    Observed::Bytes { conn, bytes }
                }
                StreamData::Closed { conn, reason } => Observed::Closed { conn, reason },
                StreamData::Failed { stream, req, reason } => Observed::Failed { stream, req, reason },
            };
            self.node
                .observe_stream(o)
                .map_err(|e| RuntimeError::Fault(e.to_string()))?;
        }
        Ok(None)
    }

    /// How many messages the engine takes now: what the node's inbox has room for.
    fn data_room(&self) -> usize {
        self.inbox_cap.saturating_sub(self.node.inbox_len())
    }

    /// How many stream bytes the engine takes now: none while the node holds `backlog_bytes` it has not delivered
    /// (the readers then stop, pushing back on the peers' TCP).
    fn stream_budget(&self) -> u64 {
        self.backlog_bytes.saturating_sub(self.node.stream_backlog() as u64)
    }

    fn admit_and_offer(&mut self, d: Data) -> Result<(), RuntimeError> {
        match d {
            Data::Deliver { from, rel, row } => {
                let principal = self
                    .principals
                    .get(from.0 as usize)
                    .cloned()
                    .unwrap_or_else(|| Arc::from(""));
                let source = Source::Node {
                    role: self.roles.get(from.0 as usize).copied().flatten(),
                    principal: &principal,
                };
                if self.admit(rel, source) {
                    bump(&self.stats.delivered, 1);
                    self.node.offer_delivery(Delivery { rel, from, row });
                }
            }
            Data::Ingress {
                session,
                principal,
                rel,
                row,
            } => {
                if self.admit(rel, Source::Session { principal: &principal }) {
                    bump(&self.stats.ingress, 1);
                    self.node.offer_ingress(Ingress { rel, session, row });
                }
            }
            Data::Member(e) => {
                if let MemberEvent::Open { .. } = e {
                    bump(&self.stats.members, 1);
                }
                let mut host = MemberHost {
                    node: &mut self.node,
                    acl: &self.acl,
                    oracle: &self.oracle,
                    stats: &self.stats,
                    me: self.me,
                    names: &self.names,
                    seed: self.seed,
                };
                self.members.handle(e, &mut host);
            }
        }
        Ok(())
    }

    fn control(
        &mut self,
        m: Control,
        codec: &blossom_wire::codec::Codec<'_>,
        durable: &DurableCodec<'_>,
    ) -> Result<Option<Stopped>, RuntimeError> {
        match m {
            Control::Synced(t) => {
                let released = self.node.wal_synced(Tick(t.tick()))?;
                self.dispatch(released, codec)?;
                self.maybe_checkpoint(t, durable)?;
            }
            Control::WalFailed(e) => return Err(RuntimeError::Fault(e)),
            Control::CheckpointDone(r) => {
                self.checkpoint_busy = false;
                self.chain = r.map_err(|e| RuntimeError::Fault(format!("checkpoint failed: {e}")))?;
                bump(&self.stats.checkpoints, 1);
                // The installed checkpoint is where recovery starts now: the blobs no recovery and no running rule
                // can reach go (FOREIGN-PROTOCOLS §5).
                if let Some((tick, outside)) = self.checkpoint_blobs.take() {
                    let gone = self.node.blob_garbage(tick, &outside);
                    let deleted = self.blob_store.delete(&gone)?;
                    bump(&self.stats.blobs_collected, deleted as u64);
                }
            }
            Control::Wake => {}
            Control::Stop => return Ok(Some(Stopped::Stopped)),
        }
        Ok(None)
    }

    /// Admission by ACL (the node reads the committed rows `principal in REL` needs), counting rejections.
    fn admit(&self, rel: RelId, source: Source<'_>) -> bool {
        let admitted = self.node.admits(&self.acl, self.oracle.static_facts(), rel, source);
        if !admitted {
            bump(&self.stats.rejected_acl, 1);
        }
        admitted
    }

    /// Sends a released tick's frames: to peers merged per (destination, channel), to sessions per (session,
    /// channel), split into frames under the size limit; a send to this node itself is delivered locally.
    fn dispatch(
        &mut self,
        released: Vec<ReleasedTick>,
        codec: &blossom_wire::codec::Codec<'_>,
    ) -> Result<(), RuntimeError> {
        let program = self.artifact.program.clone();
        let p = program.get();
        for t in released {
            bump(&self.stats.released, 1);
            let mut to_peers: BTreeMap<(NodeId, RelId), Vec<&Row>> = BTreeMap::new();
            let mut to_members: BTreeMap<(NodeId, RelId), Vec<&Row>> = BTreeMap::new();
            for s in &t.sends {
                if s.to.is_client() {
                    to_members.entry((s.to, s.rel)).or_default().push(&s.row);
                } else if s.to == self.me {
                    self.node.offer_delivery(Delivery {
                        rel: s.rel,
                        from: self.me,
                        row: s.row.clone(),
                    });
                } else {
                    to_peers.entry((s.to, s.rel)).or_default().push(&s.row);
                }
            }
            for ((to, rel), rows) in to_peers {
                let Some(q) = self.peers.get(&to) else {
                    return Err(RuntimeError::Fault(format!(
                        "a send to node {}, which is not in the deployment",
                        to.0
                    )));
                };
                let sid = self
                    .catalog
                    .sid(rel)
                    .ok_or_else(|| internal_error!("{rel:?} is not a channel"))?;
                let out = net::batch_frames(codec, p, sid, rel, t.tick.0, &rows)?;
                bump(&self.stats.dropped_oversized, out.oversized);
                for f in out.frames {
                    if let Err(TrySendError::Full(_)) = q.try_send(f) {
                        bump(&self.stats.dropped_queue_full, 1);
                    }
                }
            }
            for ((member, rel), rows) in to_members {
                let sid = self
                    .catalog
                    .sid(rel)
                    .ok_or_else(|| internal_error!("{rel:?} is not a channel"))?;
                let (batches, oversized) = net::batches(codec, p, sid, rel, t.tick.0, &rows)?;
                bump(&self.stats.dropped_oversized, oversized);
                let mut host = MemberHost {
                    node: &mut self.node,
                    acl: &self.acl,
                    oracle: &self.oracle,
                    stats: &self.stats,
                    me: self.me,
                    names: &self.names,
                    seed: self.seed,
                };
                for b in batches {
                    self.members.send(member, b, &mut host);
                }
            }
            // The messages this tick took are durable now: their members' batches are acknowledged.
            let mut host = MemberHost {
                node: &mut self.node,
                acl: &self.acl,
                oracle: &self.oracle,
                stats: &self.stats,
                me: self.me,
                names: &self.names,
                seed: self.seed,
            };
            self.members.released(t.taken, &mut host);
            self.dispatch_streams(&t)?;
            let mut to_sessions: BTreeMap<(SessionId, RelId), Vec<&Row>> = BTreeMap::new();
            for e in &t.egress {
                to_sessions.entry((e.session, e.rel)).or_default().push(&e.row);
            }
            if to_sessions.is_empty() {
                continue;
            }
            let sessions = self
                .sessions
                .lock()
                .map_err(|_| internal_error!("the session table's lock is poisoned"))?;
            for ((session, rel), rows) in to_sessions {
                bump(&self.stats.egress, rows.len() as u64);
                let Some(q) = sessions.get(&session) else {
                    bump(&self.stats.dropped_closed_session, rows.len() as u64);
                    continue;
                };
                let sid = self
                    .catalog
                    .sid(rel)
                    .ok_or_else(|| internal_error!("{rel:?} is not a channel"))?;
                let out = net::batch_frames(codec, p, sid, rel, t.tick.0, &rows)?;
                bump(&self.stats.dropped_oversized, out.oversized);
                for f in out.frames {
                    match q.try_send(f) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => bump(&self.stats.dropped_queue_full, 1),
                        Err(TrySendError::Disconnected(_)) => bump(&self.stats.dropped_closed_session, 1),
                    }
                }
            }
        }
        Ok(())
    }

    /// A released tick's requests to the host (FOREIGN-PROTOCOLS §1.2): its writes, in connection and `seq` order,
    /// then its pauses, then its resumes, then its closes, then the connections whose `closed` event it delivered,
    /// then its dials.
    fn dispatch_streams(&mut self, t: &ReleasedTick) -> Result<(), RuntimeError> {
        let mut writes = Vec::new();
        let mut closes = Vec::new();
        let mut pauses = Vec::new();
        let mut dials = Vec::new();
        let mut refused = Vec::new();
        let requests: Vec<HostRequest> = {
            let blobs = self.node.blobs();
            t.host
                .iter()
                .map(|h| host_request(self.node.streams(), h, &blobs))
                .collect::<Result<_, _>>()
                .map_err(|e| RuntimeError::Fault(e.to_string()))?
        };
        for r in requests {
            match r {
                HostRequest::Write {
                    stream,
                    conn,
                    seq,
                    bytes,
                } => writes.push((conn, seq, stream, bytes)),
                HostRequest::Close { stream, conn } => closes.push((stream, conn)),
                HostRequest::Pause { stream, conn, paused } => pauses.push((!paused, conn, stream)),
                HostRequest::Refused { stream, conn, why } => refused.push((stream, conn, why)),
                HostRequest::Dial { stream, req, addr } => dials.push((stream, req, addr)),
            }
        }
        for (stream, conn, why) in refused {
            self.streams.conns.refuse(stream, conn, why);
        }
        writes.sort_by_key(|w| (w.0, w.1));
        for (conn, seq, stream, bytes) in writes {
            self.streams.conns.write(stream, conn, seq, bytes);
        }
        // Pauses before resumes: a tick that asks both reads the connection.
        pauses.sort();
        for (resume, conn, stream) in pauses {
            self.streams.conns.pause(stream, conn, !resume);
        }
        for (stream, conn) in closes {
            self.streams.conns.close(stream, conn);
        }
        for conn in &t.retired {
            self.streams.conns.retire(*conn);
        }
        for (stream, req, addr) in dials {
            crate::streams::dial(stream, req, addr, self.streams.clone());
        }
        Ok(())
    }

    /// Starts a checkpoint of the durable rows at the synced tick `t` when the WAL has grown past the threshold
    /// since the last one. Every tick up to `t` has just been released, so the released image is the image at `t`.
    fn maybe_checkpoint(&mut self, t: SyncedTick, durable: &DurableCodec<'_>) -> Result<(), RuntimeError> {
        if self.checkpoint_busy || t.lsn().0.saturating_sub(self.last_checkpoint_lsn) < self.checkpoint_bytes {
            return Ok(());
        }
        if self.node.released_tick() < Some(Tick(t.tick())) {
            return Err(internal_error!("a synced tick is not released").into());
        }
        // A delta layer when the change since the installed checkpoint is known and the chain has room; otherwise a
        // full image.
        let job = match self.node.take_checkpoint_delta() {
            Some(d) if blossom_node::durable::layer_fits(self.chain) => CheckpointJob::Layer(durable.encode_delta(&d)?),
            _ => CheckpointJob::Full(self.node.released_image().clone()),
        };
        self.checkpoint_blobs = Some((Tick(t.tick()), self.node.checkpoint_candidates()));
        self.checkpoint_busy = true;
        self.last_checkpoint_lsn = t.lsn().0;
        self.checkpoint
            .send((job, t))
            .map_err(|_| RuntimeError::Fault("the checkpoint thread stopped".into()))
    }
}
