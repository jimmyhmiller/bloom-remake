//! The production driver for one node (ARCHITECTURE §5.2), on std threads:
//!
//! - the **engine thread** owns the [`Node`]: it takes what the I/O threads admitted, runs ticks, hands each tick's
//!   WAL record to the committer, and releases ticks as the committer reports them synced (Invariant R lives in the
//!   node), sending their frames to peers and sessions;
//! - the **committer thread** owns the WAL: it takes every submitted record, appends them as one batch, syncs once,
//!   and reports the synced tick (Invariant B: batch k+1 is never written before batch k's sync returned). A failed
//!   append or sync poisons the WAL and faults the node;
//! - the **checkpoint thread** writes a checkpoint of the durable rows at a synced tick, installs it, and hands the
//!   truncation token to the committer;
//! - **listeners** accept peer and client connections; a **reader thread** per connection decodes batches, and a
//!   **writer thread** per peer and per session sends frames.
//!
//! Tick `t+1` computes while tick `t`'s fsync is in flight (pipelined group commit, ARCH-10).
//!
//! The architecture puts I/O on tokio; this build uses blocking std threads (one reader per connection), which is
//! enough for a handful of nodes and the clients of a benchmark. Admission runs on the engine thread rather than in
//! the reader: it needs the node's committed state for `principal in REL` ACLs.

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{RelId, RoleId, internal_error};
use blossom_node::acl::{AclTable, Source};
use blossom_node::durable::{DurableCodec, DurableSchema};
use blossom_node::recovery::{self, KIND_DELTA, StoreSpec};
use blossom_node::{Node, NodeConfig, NodeState, ReleasedTick};
use blossom_oracle::{Delivery, Ingress, Oracle, Row};
use blossom_store::{
    CheckpointWriter, FileCheckpoints, FileWal, MetaRecord, MetaStore, OpenMode, RealFs, StoreIdentity, StoreLock,
    SyncedTick, TruncateToken, WalRecordBuf, WalWriter,
};
use blossom_value::time::{Instant, NodeId, Tick};
use blossom_value::value::SessionId;
use blossom_value::Seed;
use blossom_wire::frame::{Frame, Peer, RejectReason};

use blossom_node::env::{Clock, Entropy};

use crate::RuntimeError;
use crate::clock::{OsEntropy, SystemClock, wall_now};
use crate::deploy::DeploymentSpec;
use crate::net::{self, Catalog, Conn, Identity};

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

/// What reaches the engine thread.
enum Inbound {
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
    SessionOpened {
        session: SessionId,
        out: SyncSender<Vec<u8>>,
    },
    SessionClosed {
        session: SessionId,
    },
    Synced(SyncedTick),
    WalFailed(String),
    CheckpointDone(Result<(), String>),
    Stop,
}

enum Commit {
    Append { tick: Tick, now: Instant, payload: Vec<u8> },
    Truncate(TruncateToken),
}

/// A running node.
pub struct Server {
    engine: Option<JoinHandle<Result<Stopped, RuntimeError>>>,
    inbound: SyncSender<Inbound>,
    stop: Arc<AtomicBool>,
    conns: Arc<Mutex<Vec<TcpStream>>>,
    threads: Vec<JoinHandle<()>>,
    pub stats: Arc<Stats>,
    pub node: NodeId,
    pub peer_addr: SocketAddr,
    pub client_addr: Option<SocketAddr>,
    /// The tick this incarnation booted at, and its restart count.
    pub boot_tick: Tick,
    pub restarts: u64,
}

/// The store identity the deployment expects for a node.
pub fn store_identity(spec: &DeploymentSpec, artifact: &BlsArtifact, node: &str) -> Result<StoreIdentity, RuntimeError> {
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
        let oracle = Arc::new(
            Oracle::new(artifact.program.clone())?
                .with_roles(artifact.roles.clone())
                .with_seed(seed)?,
        );
        let nonce = OsEntropy.boot_nonce().map_err(RuntimeError::Config)?;
        let dir = cfg.dir.clone().unwrap_or_else(|| spec.data_dir.join(&entry.name));
        let opened = recovery::open(
            Arc::new(RealFs),
            &StoreSpec {
                dir,
                identity: store_identity(spec, &artifact, &cfg.node)?,
                mode: cfg.mode,
            },
            program,
            names.clone(),
            wall_now().map_err(RuntimeError::Config)?,
            nonce,
        )?;
        let mut ncfg = NodeConfig::new(me, role);
        ncfg.halt = artifact.halt;
        ncfg.statics = spec.static_rows(program, &names)?;
        let boot = opened.boot.clone();
        let node = Node::boot(ncfg, &artifact.program, oracle.clone(), boot.clone())?;
        let restarts = opened.record.restarts;

        let peer_listener = TcpListener::bind(entry.addr).map_err(|e| RuntimeError::Net(format!("bind {}: {e}", entry.addr)))?;
        let peer_addr = peer_listener.local_addr().map_err(RuntimeError::Io)?;
        let client_listener = match entry.client_addr {
            Some(a) => Some(TcpListener::bind(a).map_err(|e| RuntimeError::Net(format!("bind {a}: {e}")))?),
            None => None,
        };
        let client_addr = match &client_listener {
            Some(l) => Some(l.local_addr().map_err(RuntimeError::Io)?),
            None => None,
        };

        let stats = Arc::new(Stats::default());
        let stop = Arc::new(AtomicBool::new(false));
        let conns: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
        let (in_tx, in_rx) = mpsc::sync_channel::<Inbound>(65_536);
        let (commit_tx, commit_rx) = mpsc::channel::<Commit>();
        let (ckpt_tx, ckpt_rx) = mpsc::channel::<(blossom_store::DurableSnapshot, SyncedTick)>();
        let id = identity(spec, &artifact);
        let catalog = Arc::new(Catalog::of(program)?);
        let mut threads = Vec::new();

        let Opened { wal, checkpoints, meta, record, lock } = split(opened);
        {
            let tx = in_tx.clone();
            let stats = stats.clone();
            threads.push(spawn("committer", move || committer(wal, commit_rx, tx, stats))?);
        }
        {
            let tx = in_tx.clone();
            let commit = commit_tx.clone();
            threads.push(spawn("checkpoint", move || checkpointer(checkpoints, ckpt_rx, commit, tx))?);
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
            let (addr, id, catalog, stop) = (n.addr, id.clone(), catalog.clone(), stop.clone());
            threads.push(spawn("peer-writer", move || peer_writer(addr, id, me, restarts, nonce, catalog, rx, stop))?);
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
                inbound: in_tx.clone(),
                stop: stop.clone(),
                conns: conns.clone(),
                stats: stats.clone(),
                sessions: Arc::new(AtomicU64::new(0)),
                incarnations: Arc::new(Mutex::new(BTreeMap::new())),
                spec: Arc::new(spec.clone()),
            };
            let c = ctx.clone();
            threads.push(spawn("peer-listener", move || accept_loop(peer_listener, c, false))?);
            if let Some(l) = client_listener {
                threads.push(spawn("client-listener", move || accept_loop(l, ctx, true))?);
            }
        }
        let engine = {
            let e = Engine {
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
                sessions: BTreeMap::new(),
                commit: commit_tx,
                checkpoint: ckpt_tx,
                checkpoint_bytes: spec.checkpoint_wal_bytes,
                checkpoint_busy: false,
                last_checkpoint_lsn: 0,
                meta,
                record,
                _lock: lock,
                clock: SystemClock::anchored_at(boot.now),
                stats: stats.clone(),
            };
            std::thread::Builder::new()
                .name("engine".into())
                .spawn(move || e.run(in_rx))
                .map_err(RuntimeError::Io)?
        };
        Ok(Server {
            engine: Some(engine),
            inbound: in_tx,
            stop,
            conns,
            threads,
            stats,
            node: me,
            peer_addr,
            client_addr,
            boot_tick: boot.tick,
            restarts,
        })
    }

    /// Stops the node: the engine stops scheduling ticks, and connections close. Anything not yet released is
    /// never released (crash semantics, which are always legal).
    pub fn stop(mut self) -> Result<Stopped, RuntimeError> {
        // The engine may already have stopped (halted or faulted) and dropped its receiver.
        let _ = self.inbound.try_send(Inbound::Stop);
        self.wait_engine()
    }

    /// Waits for the engine to stop by itself (halt or fault).
    pub fn wait(mut self) -> Result<Stopped, RuntimeError> {
        self.wait_engine()
    }

    fn wait_engine(&mut self) -> Result<Stopped, RuntimeError> {
        let result = match self.engine.take() {
            Some(h) => h.join().map_err(|_| RuntimeError::Internal(internal_error!("the engine thread panicked")))?,
            None => Err(internal_error!("the engine was already joined").into()),
        };
        self.shutdown_io();
        result
    }

    fn shutdown_io(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(conns) = self.conns.lock() {
            for c in conns.iter() {
                // Closing is best-effort: a connection may already be gone.
                let _ = c.shutdown(std::net::Shutdown::Both);
            }
        }
        for t in self.threads.drain(..) {
            // A thread that panicked has already reported through its connection; joining just reaps it.
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.engine.is_some() {
            let _ = self.inbound.try_send(Inbound::Stop);
            let _ = self.wait_engine();
        }
    }
}

struct Opened {
    wal: FileWal,
    checkpoints: FileCheckpoints,
    meta: MetaStore,
    record: MetaRecord,
    lock: StoreLock,
}

fn split(o: recovery::Opened) -> Opened {
    Opened {
        wal: o.wal,
        checkpoints: o.checkpoints,
        meta: o.meta,
        record: o.record,
        lock: o.lock,
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, RuntimeError> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map_err(RuntimeError::Io)
}

/// The committer (Invariant B): append everything submitted as one batch, sync once, report.
fn committer(mut wal: FileWal, rx: Receiver<Commit>, tx: SyncSender<Inbound>, stats: Arc<Stats>) {
    let mut batch: u64 = 0;
    let mut truncates: Vec<TruncateToken> = Vec::new();
    while let Ok(first) = rx.recv() {
        let mut work = vec![first];
        while let Ok(more) = rx.try_recv() {
            work.push(more);
        }
        batch = batch.saturating_add(1);
        let mut appended = 0u64;
        let mut result: Result<(), String> = Ok(());
        for w in work {
            match w {
                Commit::Append { tick, now, payload } => {
                    let rec = WalRecordBuf {
                        batch,
                        tick: tick.0,
                        now: now.0,
                        kind: KIND_DELTA,
                        payload,
                    };
                    if let Err(e) = wal.append(&rec) {
                        result = Err(format!("WAL append failed: {e}"));
                        break;
                    }
                    appended += 1;
                }
                Commit::Truncate(t) => truncates.push(t),
            }
        }
        if let Err(e) = result {
            let _ = tx.send(Inbound::WalFailed(e));
            return;
        }
        if appended > 0 {
            match wal.sync() {
                Ok(synced) => {
                    bump(&stats.wal_records, appended);
                    bump(&stats.wal_batches, 1);
                    if let Some(t) = synced.synced_tick()
                        && tx.send(Inbound::Synced(t)).is_err()
                    {
                        return;
                    }
                }
                Err(e) => {
                    // A failed sync must never be retried: the WAL is poisoned for the incarnation.
                    let _ = tx.send(Inbound::WalFailed(format!("WAL sync failed: {e}")));
                    return;
                }
            }
        }
        // Truncation happens at a batch boundary, after the sync.
        for t in truncates.drain(..) {
            if let Err(e) = wal.truncate_through(t) {
                let _ = tx.send(Inbound::WalFailed(format!("WAL truncation failed: {e}")));
                return;
            }
        }
    }
}

/// The checkpoint thread: write, install, hand the truncation token to the committer.
fn checkpointer(
    mut ckpt: FileCheckpoints,
    rx: Receiver<(blossom_store::DurableSnapshot, SyncedTick)>,
    commit: Sender<Commit>,
    tx: SyncSender<Inbound>,
) {
    while let Ok((snap, covers)) = rx.recv() {
        let result = ckpt
            .write(snap, covers)
            .and_then(|id| ckpt.install(id))
            .map_err(|e| e.to_string())
            .and_then(|token| commit.send(Commit::Truncate(token)).map_err(|e| e.to_string()));
        if tx.send(Inbound::CheckpointDone(result)).is_err() {
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
    inbound: SyncSender<Inbound>,
    stop: Arc<AtomicBool>,
    conns: Arc<Mutex<Vec<TcpStream>>>,
    stats: Arc<Stats>,
    sessions: Arc<AtomicU64>,
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
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                if let (Ok(mut conns), Ok(c)) = (ctx.conns.lock(), stream.try_clone()) {
                    conns.push(c);
                }
                let ctx = ctx.clone();
                let name = if clients { "session" } else { "peer-reader" };
                // A connection whose thread cannot start is dropped, which closes it.
                let _ = spawn(name, move || {
                    let _ = if clients { session(stream, ctx) } else { peer_reader(stream, ctx) };
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn peer_reader(stream: TcpStream, ctx: Accept) -> Result<(), RuntimeError> {
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
                    return Err((RejectReason::NotAllowed, format!("stale incarnation {} of node {n}", h.restarts)));
                }
                *newest = h.restarts;
                Ok(())
            }
            _ => Err((RejectReason::NotAllowed, format!("{:?} is not a peer of this node", h.peer))),
        },
    )?;
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
            if ctx.inbound.send(Inbound::Deliver { from, rel, row }).is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn session(stream: TcpStream, ctx: Accept) -> Result<(), RuntimeError> {
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
    let Peer::Client { principal } = hello.peer else {
        return Err(internal_error!("a client HELLO without a principal").into());
    };
    let principal: Arc<str> = principal.into();
    // Session ids never repeat across incarnations: the restart count is the high half.
    let n = ctx.sessions.fetch_add(1, Ordering::SeqCst);
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
    if ctx.inbound.send(Inbound::SessionOpened { session, out: tx }).is_err() {
        return Ok(());
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
                let m = Inbound::Ingress {
                    session,
                    principal: principal.clone(),
                    rel,
                    row,
                };
                if ctx.inbound.send(m).is_err() {
                    return Ok(());
                }
            }
        }
        Ok(())
    })();
    let _ = ctx.inbound.send(Inbound::SessionClosed { session });
    let _ = w.join();
    result
}

struct Engine {
    node: Node<Arc<Oracle>>,
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
    sessions: BTreeMap<SessionId, SyncSender<Vec<u8>>>,
    commit: Sender<Commit>,
    checkpoint: Sender<(blossom_store::DurableSnapshot, SyncedTick)>,
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
    fn run(mut self, rx: Receiver<Inbound>) -> Result<Stopped, RuntimeError> {
        let program = self.artifact.program.clone();
        let codec = net::wire_codec(program.get());
        let schema = self.schema.clone();
        let durable = DurableCodec::new(program.get(), &schema, self.names.clone());
        loop {
            // Wait for something to do: a message, or the next timer.
            let now = self.clock.now();
            let first = if self.node.ready(now)? {
                match rx.try_recv() {
                    Ok(m) => Some(m),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => return Err(internal_error!("the engine's inbox closed").into()),
                }
            } else {
                let wait = match self.node.next_deadline()? {
                    Some(d) => Duration::from_nanos(u64::try_from(d.0.saturating_sub(now.0)).unwrap_or(0)),
                    None => Duration::from_secs(1),
                }
                .min(Duration::from_secs(1));
                match rx.recv_timeout(wait) {
                    Ok(m) => Some(m),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return Err(internal_error!("the engine's inbox closed").into()),
                }
            };
            let mut next = first;
            while let Some(m) = next {
                if let Some(stopped) = self.handle(m, &codec, &durable)? {
                    return Ok(stopped);
                }
                next = rx.try_recv().ok();
            }
            if *self.node.state() == NodeState::Halted {
                return Ok(Stopped::Halted);
            }
            // Run ticks while ready (bounded by the node's in-flight limit).
            while self.node.ready(self.clock.now())? {
                let now = self.clock.now();
                let fx = self.node.run_tick(now).map_err(|f| RuntimeError::Fault(f.to_string()))?;
                bump(&self.stats.ticks, 1);
                if let Some(upto) = fx.reserve {
                    self.record.reserved_tick = upto.0;
                    self.record.last_now = self.record.last_now.max(now.0);
                    self.meta.write(&self.record)?;
                    self.node.reserved(upto);
                }
                match fx.wal {
                    Some(delta) => {
                        let payload = durable.encode_delta(&delta)?;
                        self.commit
                            .send(Commit::Append {
                                tick: fx.tick,
                                now: fx.now,
                                payload,
                            })
                            .map_err(|_| RuntimeError::Fault("the committer stopped".into()))?;
                    }
                    None => {
                        let released = self.node.release_ready();
                        self.dispatch(released, &codec)?;
                    }
                }
                // Take newly arrived messages into the next tick's batch.
                while let Ok(m) = rx.try_recv() {
                    if let Some(stopped) = self.handle(m, &codec, &durable)? {
                        return Ok(stopped);
                    }
                }
            }
        }
    }

    fn handle(
        &mut self,
        m: Inbound,
        codec: &blossom_wire::codec::Codec<'_>,
        durable: &DurableCodec<'_>,
    ) -> Result<Option<Stopped>, RuntimeError> {
        match m {
            Inbound::Deliver { from, rel, row } => {
                let principal = self.principals.get(from.0 as usize).cloned().unwrap_or_else(|| Arc::from(""));
                let source = Source::Node {
                    role: self.roles.get(from.0 as usize).copied().flatten(),
                    principal: &principal,
                };
                if self.admit(rel, source) {
                    bump(&self.stats.delivered, 1);
                    self.node.offer_delivery(Delivery { rel, from, row });
                }
            }
            Inbound::Ingress {
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
            Inbound::SessionOpened { session, out } => {
                self.sessions.insert(session, out);
            }
            Inbound::SessionClosed { session } => {
                self.sessions.remove(&session);
            }
            Inbound::Synced(t) => {
                let released = self.node.wal_synced(Tick(t.tick()));
                self.dispatch(released, codec)?;
                self.maybe_checkpoint(t, durable)?;
            }
            Inbound::WalFailed(e) => return Err(RuntimeError::Fault(e)),
            Inbound::CheckpointDone(r) => {
                self.checkpoint_busy = false;
                r.map_err(|e| RuntimeError::Fault(format!("checkpoint failed: {e}")))?;
                bump(&self.stats.checkpoints, 1);
            }
            Inbound::Stop => return Ok(Some(Stopped::Stopped)),
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
    /// channel); a send to this node itself is delivered locally.
    fn dispatch(&mut self, released: Vec<ReleasedTick>, codec: &blossom_wire::codec::Codec<'_>) -> Result<(), RuntimeError> {
        let program = self.artifact.program.clone();
        let p = program.get();
        for t in released {
            bump(&self.stats.released, 1);
            let mut to_peers: BTreeMap<(NodeId, RelId), Vec<&Row>> = BTreeMap::new();
            for s in &t.sends {
                if s.to == self.me {
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
                    return Err(RuntimeError::Fault(format!("a send to node {}, which is not in the deployment", to.0)));
                };
                let sid = self.catalog.sid(rel).ok_or_else(|| internal_error!("{rel:?} is not a channel"))?;
                let frame = net::batch_frame(codec, p, sid, rel, t.tick.0, &rows)?;
                if let Err(TrySendError::Full(_)) = q.try_send(frame) {
                    bump(&self.stats.dropped_queue_full, rows.len() as u64);
                }
            }
            let mut to_sessions: BTreeMap<(SessionId, RelId), Vec<&Row>> = BTreeMap::new();
            for e in &t.egress {
                to_sessions.entry((e.session, e.rel)).or_default().push(&e.row);
            }
            for ((session, rel), rows) in to_sessions {
                bump(&self.stats.egress, rows.len() as u64);
                let Some(q) = self.sessions.get(&session) else {
                    bump(&self.stats.dropped_closed_session, rows.len() as u64);
                    continue;
                };
                let sid = self.catalog.sid(rel).ok_or_else(|| internal_error!("{rel:?} is not a channel"))?;
                let frame = net::batch_frame(codec, p, sid, rel, t.tick.0, &rows)?;
                match q.try_send(frame) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => bump(&self.stats.dropped_queue_full, rows.len() as u64),
                    Err(TrySendError::Disconnected(_)) => bump(&self.stats.dropped_closed_session, rows.len() as u64),
                }
            }
        }
        Ok(())
    }

    /// Starts a checkpoint of the durable rows at the synced tick `t` when the WAL has grown past the threshold.
    /// Every tick up to `t` has just been released, so the released image is the image at `t`.
    fn maybe_checkpoint(&mut self, t: SyncedTick, durable: &DurableCodec<'_>) -> Result<(), RuntimeError> {
        if self.checkpoint_busy || t.lsn().0.saturating_sub(self.last_checkpoint_lsn) < self.checkpoint_bytes {
            return Ok(());
        }
        if self.node.released_tick() < Some(Tick(t.tick())) {
            return Err(internal_error!("a synced tick is not released").into());
        }
        let snap = durable.encode_image(self.node.released_image())?;
        self.checkpoint_busy = true;
        self.last_checkpoint_lsn = t.lsn().0;
        self.checkpoint
            .send((snap, t))
            .map_err(|_| RuntimeError::Fault("the checkpoint thread stopped".into()))
    }
}
