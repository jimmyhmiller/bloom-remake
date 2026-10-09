//! A host of keyed members on `blossom run` (docs/design/KEYED.md §3).
//!
//! A deployment's node of a keyed role runs none of that role's rules as itself: it runs the role's members that
//! rendezvous hashing gives it ([`crate::keyed::Routing`]), each an [`ObjectNode`] with its own store under
//! `<store>/members/<hash>/`, created by the first message to it and opened again (its timers resumed) when the host
//! restarts. The host is a peer like any node: other nodes send it the messages addressed to its members (column 0
//! names the member), and it sends what its members send as `FromMember` frames, so the receiver knows which member
//! sent it. A member's message to another member of this host is delivered here.
//!
//! Each member runs its ticks durably before its messages leave (the object's driver), so Invariant R holds per
//! member. The host's own store holds only its restart count, which its peers' incarnation check needs.
//!
//! Not yet (KEYED.md §4): pages connected to members (sub-slice 3), streams and external sessions at members, and
//! recording traces of members.

use std::collections::{BTreeMap, VecDeque};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{RelId, internal_error};
use blossom_ir::members::{Members, member_name};
use blossom_node::env::Entropy;
use blossom_oracle::Row;
use blossom_store::{OpenMode, RealFs};
use blossom_value::time::{Instant, MemberRef, NodeId};
use blossom_wire::frame::{Frame, Peer};

use crate::RuntimeError;
use crate::clock::{OsEntropy, wall_now};
use crate::deploy::DeploymentSpec;
use crate::keyed::{Routing, addressee, member_of};
use crate::net::{self, Catalog, Conn, Identity};
use crate::object::{ObjectConfig, ObjectNode, Output};
use crate::server::{ServerConfig, admit_peer, peer_writer, spawn};

/// How long the host's loop waits for a message before it looks at its timers and whether it should stop.
const POLL: Duration = Duration::from_millis(200);

/// Counters of what a host did and refused.
#[derive(Debug, Default)]
pub struct HostStats {
    /// Members running, and members opened since the host started.
    pub members: AtomicU64,
    pub opened: AtomicU64,
    /// Messages delivered to members, and messages members sent elsewhere.
    pub delivered: AtomicU64,
    pub sent: AtomicU64,
    /// Rows addressed to no member of this host, or from a member through another host than its own.
    pub rejected_unknown_dest: AtomicU64,
    /// Batches of a channel the peer's `HELLO` did not name.
    pub rejected_schema: AtomicU64,
    /// Frames dropped because a peer's queue was full.
    pub dropped_queue_full: AtomicU64,
    pub dropped_oversized: AtomicU64,
}

fn bump(c: &AtomicU64, n: u64) {
    c.fetch_add(n, Ordering::Relaxed);
}

/// A message for one of the host's members: who sent it (`from`, an id in the host's member table for a member),
/// through which node (`via`: the sender, or the host of a sending member), and the row (column 0 names the member).
struct Inbound {
    to: MemberRef,
    from: NodeId,
    via: NodeId,
    rel: RelId,
    row: Row,
}

/// A running host.
pub struct Hosting {
    main: Option<JoinHandle<Result<(), RuntimeError>>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    conns: Arc<Mutex<Vec<TcpStream>>>,
    pub stats: Arc<HostStats>,
    pub node: NodeId,
    pub peer_addr: SocketAddr,
    /// The host's restart count (its peers refuse an older incarnation's links).
    pub restarts: u64,
}

impl Hosting {
    /// Starts the host `cfg.node`, a node of a keyed role: its peer links, and every member its store holds.
    pub fn start(cfg: ServerConfig) -> Result<Hosting, RuntimeError> {
        let spec = cfg.spec.clone();
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
        let routing = Arc::new(Routing::of(&spec, &artifact)?);
        if !routing.is_host(me) {
            return Err(RuntimeError::Config(format!("node {} hosts no keyed role", cfg.node)));
        }
        for (what, set) in [
            (
                "pages (`--web`) at a host of keyed members (KEYED.md §4, sub-slice 3)",
                cfg.web.is_some(),
            ),
            ("queries (`--admin`) at a host of keyed members", cfg.admin.is_some()),
            ("recording traces of keyed members", cfg.record.is_some()),
            (
                "external sessions at a host of keyed members",
                entry.client_addr.is_some(),
            ),
            ("streams at a host of keyed members", !entry.streams.is_empty()),
        ] {
            if set {
                return Err(blossom_base::unimplemented_error!("LANG-153", "{what}").into());
            }
        }
        let dir = cfg.dir.clone().unwrap_or_else(|| spec.data_dir.join(&entry.name));
        let restarts = next_incarnation(&dir, cfg.mode)?;
        let nonce = OsEntropy.boot_nonce().map_err(RuntimeError::Config)?;
        let id = crate::server::identity(&spec, &artifact);
        let catalog = Arc::new(Catalog::of(program)?);
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(HostStats::default());
        let conns: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
        let members = Arc::new(Members::open());
        let mut threads = Vec::new();

        let listener =
            TcpListener::bind(entry.addr).map_err(|e| RuntimeError::Net(format!("bind {}: {e}", entry.addr)))?;
        let peer_addr = listener.local_addr().map_err(RuntimeError::Io)?;
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
        let (inbox, rx) = mpsc::sync_channel::<Inbound>(4096);
        {
            let ctx = Readers {
                artifact: artifact.clone(),
                id: id.clone(),
                me,
                restarts,
                nonce,
                catalog: catalog.clone(),
                nodes: spec.nodes.len(),
                incarnations: Arc::new(Mutex::new(BTreeMap::new())),
                members: members.clone(),
                routing: routing.clone(),
                inbox,
                stop: stop.clone(),
                stats: stats.clone(),
                conns: conns.clone(),
            };
            threads.push(spawn("peer-listener", move || accept_loop(listener, ctx))?);
        }
        let mut host = Host {
            spec,
            artifact,
            name: cfg.node.clone(),
            me,
            dir,
            members,
            routing,
            objects: BTreeMap::new(),
            catalog,
            peers,
            local: VecDeque::new(),
            externs: cfg.externs.clone(),
            stats: stats.clone(),
        };
        host.open_stored()?;
        let main = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("hosting".into())
                .stack_size(blossom_ir::depth::EVAL_STACK_BYTES)
                .spawn(move || host.run(&rx, &stop))
                .map_err(RuntimeError::Io)?
        };
        Ok(Hosting {
            main: Some(main),
            stop,
            threads,
            conns,
            stats,
            node: me,
            peer_addr,
            restarts,
        })
    }

    /// Stops the host: its members stop taking messages, and its connections close.
    pub fn stop(mut self) -> Result<(), RuntimeError> {
        self.stop.store(true, Ordering::SeqCst);
        self.join()
    }

    /// Waits for the host to stop by itself (a fault).
    pub fn wait(mut self) -> Result<(), RuntimeError> {
        self.join()
    }

    fn join(&mut self) -> Result<(), RuntimeError> {
        let result = match self.main.take() {
            Some(h) => h
                .join()
                .map_err(|_| RuntimeError::Internal(internal_error!("the hosting thread panicked")))?,
            None => Ok(()),
        };
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(conns) = self.conns.lock() {
            for c in conns.iter() {
                let _ = c.shutdown(std::net::Shutdown::Both);
            }
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        result
    }
}

impl Drop for Hosting {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.join();
    }
}

/// The host's restart count: one more than the last start's, kept in `<dir>/host` (created by `InitFresh`).
fn next_incarnation(dir: &Path, mode: OpenMode) -> Result<u64, RuntimeError> {
    let path = dir.join("host");
    let last = match (std::fs::read_to_string(&path), mode) {
        (Ok(_), OpenMode::InitFresh) => {
            return Err(RuntimeError::Config(format!(
                "{} already holds a host's store (start it without --init-fresh)",
                dir.display()
            )));
        }
        (Ok(text), OpenMode::Existing) => text
            .trim()
            .parse::<u64>()
            .map_err(|_| RuntimeError::Config(format!("{} is not a restart count", path.display())))?,
        (Err(e), OpenMode::Existing) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(RuntimeError::Config(format!(
                "{} holds no host's store (start it with --init-fresh)",
                dir.display()
            )));
        }
        (Err(e), OpenMode::InitFresh) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir).map_err(RuntimeError::Io)?;
            0
        }
        (Err(e), _) => return Err(RuntimeError::Io(e)),
    };
    let next = last + 1;
    // Written whole and synced before the host links to anyone: a peer never sees this count twice.
    let tmp = dir.join("host.tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp).map_err(RuntimeError::Io)?;
        f.write_all(next.to_string().as_bytes()).map_err(RuntimeError::Io)?;
        f.sync_all().map_err(RuntimeError::Io)?;
    }
    std::fs::rename(&tmp, &path).map_err(RuntimeError::Io)?;
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(RuntimeError::Io)?;
    Ok(next)
}

/// What the peer readers share.
struct Readers {
    artifact: Arc<BlsArtifact>,
    id: Identity,
    me: NodeId,
    restarts: u64,
    nonce: u64,
    catalog: Arc<Catalog>,
    nodes: usize,
    incarnations: Arc<Mutex<BTreeMap<u32, u64>>>,
    members: Arc<Members>,
    routing: Arc<Routing>,
    inbox: SyncSender<Inbound>,
    stop: Arc<AtomicBool>,
    stats: Arc<HostStats>,
    conns: Arc<Mutex<Vec<TcpStream>>>,
}

fn accept_loop(listener: TcpListener, ctx: Readers) {
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    let ctx = Arc::new(ctx);
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
                // A connection whose thread cannot start is dropped, which closes it.
                let _ = spawn("peer-reader", move || {
                    let _ = read_peer(stream, &ctx);
                });
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// One peer's link: the messages it carries for this host's members.
fn read_peer(stream: TcpStream, ctx: &Readers) -> Result<(), RuntimeError> {
    let mut conn = Conn::new(stream)?;
    let hello = net::accept_handshake(
        &mut conn,
        &ctx.id,
        Peer::Node(ctx.me.0),
        ctx.restarts,
        ctx.nonce,
        &ctx.catalog,
        &|h| admit_peer(h, ctx.me, ctx.nodes, &ctx.incarnations),
    )?;
    let Peer::Node(peer) = hello.peer else {
        return Err(internal_error!("a peer HELLO without a node").into());
    };
    let peer = NodeId(peer);
    let program = ctx.artifact.program.get();
    let codec = net::wire_codec(program);
    while let Some(frame) = conn.read()? {
        let (from, b) = match frame {
            Frame::Batch(b) => (peer, b),
            // A member another host runs: a sender only through that host.
            Frame::FromMember { role, key, batch } => {
                let m = member_of(program, role, key)?;
                if ctx.routing.host_of(&m)? != peer {
                    bump(&ctx.stats.rejected_unknown_dest, batch.count);
                    continue;
                }
                let id = ctx
                    .members
                    .id(&m)
                    .ok_or_else(|| RuntimeError::Fault("this host has given every keyed member id".into()))?;
                (id, batch)
            }
            _ => continue,
        };
        let Some(&rel) = hello.inbound.get(&b.sid) else {
            bump(&ctx.stats.rejected_schema, b.count);
            continue;
        };
        for row in net::batch_rows(&codec, program, rel, &b)? {
            let to = match addressee(&row) {
                Some(m) if ctx.routing.host_of(m)? == ctx.me => m.clone(),
                _ => {
                    bump(&ctx.stats.rejected_unknown_dest, 1);
                    continue;
                }
            };
            let msg = Inbound {
                to,
                from,
                via: peer,
                rel,
                row,
            };
            if ctx.inbox.send(msg).is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// The hosting thread's state: the members running here, and where their messages go.
struct Host {
    spec: DeploymentSpec,
    artifact: Arc<BlsArtifact>,
    name: String,
    me: NodeId,
    dir: PathBuf,
    members: Arc<Members>,
    routing: Arc<Routing>,
    objects: BTreeMap<MemberRef, ObjectNode>,
    catalog: Arc<Catalog>,
    peers: BTreeMap<NodeId, SyncSender<Vec<u8>>>,
    /// Messages from one of this host's members to another, delivered before the next from a peer.
    local: VecDeque<Inbound>,
    externs: Arc<blossom_value::ExternRegistry>,
    stats: Arc<HostStats>,
}

impl Host {
    fn run(mut self, rx: &Receiver<Inbound>, stop: &AtomicBool) -> Result<(), RuntimeError> {
        let r = self.serve(rx, stop);
        stop.store(true, Ordering::SeqCst);
        r
    }

    fn serve(&mut self, rx: &Receiver<Inbound>, stop: &AtomicBool) -> Result<(), RuntimeError> {
        while !stop.load(Ordering::SeqCst) {
            let now = wall_now().map_err(RuntimeError::Config)?;
            self.wake_due(now)?;
            while let Some(m) = self.local.pop_front() {
                self.deliver(m)?;
            }
            let wait = self.next_wake()?.map_or(POLL, |at| {
                let nanos = u64::try_from(at.0.saturating_sub(now.0)).unwrap_or(0);
                Duration::from_nanos(nanos).min(POLL)
            });
            match rx.recv_timeout(wait) {
                Ok(m) => self.deliver(m)?,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
        Ok(())
    }

    /// The directory of member `m`'s store, and the file naming the member in it.
    fn member_dir(&self, m: &MemberRef) -> PathBuf {
        let name = member_name(self.artifact.program.get(), m);
        let digest = blake3::hash(name.as_bytes());
        let hex: String = digest.as_bytes().iter().take(16).map(|b| format!("{b:02x}")).collect();
        self.dir.join("members").join(hex)
    }

    /// Opens every member the host's store holds (after a restart): their timers run again.
    fn open_stored(&mut self) -> Result<(), RuntimeError> {
        let root = self.dir.join("members");
        let entries = match std::fs::read_dir(&root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(RuntimeError::Io(e)),
        };
        let mut stored = Vec::new();
        for e in entries {
            let path = e.map_err(RuntimeError::Io)?.path().join("member");
            let text = std::fs::read_to_string(&path).map_err(RuntimeError::Io)?;
            stored.push(self.parse_member(&text, &path)?);
        }
        let now = wall_now().map_err(RuntimeError::Config)?;
        for m in stored {
            self.object(&m)?;
            if let Some(o) = self.objects.get_mut(&m) {
                o.wake(now)?;
            }
            self.route(&m)?;
        }
        Ok(())
    }

    /// A member as its store's `member` file names it: its role's name, a newline, its key.
    fn parse_member(&self, text: &str, path: &Path) -> Result<MemberRef, RuntimeError> {
        let (role, key) = text
            .split_once('\n')
            .ok_or_else(|| RuntimeError::Config(format!("{} names no member", path.display())))?;
        let p = self.artifact.program.get();
        let role = p
            .keyed_roles()
            .find(|r| r.name.to_string() == role)
            .map(|r| r.id)
            .ok_or_else(|| RuntimeError::Config(format!("{}: `{role}` is not a keyed role", path.display())))?;
        Ok(MemberRef {
            role,
            key: Arc::from(key),
        })
    }

    /// Member `m`'s object, opened (its store created on its first message) if it is not running.
    fn object(&mut self, m: &MemberRef) -> Result<&mut ObjectNode, RuntimeError> {
        if !self.objects.contains_key(m) {
            let dir = self.member_dir(m);
            let file = dir.join("member");
            let p = self.artifact.program.get();
            let role = p
                .roles
                .get(m.role)
                .map(|r| r.name.to_string())
                .ok_or_else(|| internal_error!("an unknown role"))?;
            let naming = format!("{role}\n{}", m.key);
            match std::fs::read_to_string(&file) {
                Ok(text) if text == naming => {}
                Ok(_) => {
                    return Err(RuntimeError::Config(format!(
                        "{} holds another member than {}",
                        dir.display(),
                        member_name(p, m)
                    )));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir_all(&dir).map_err(RuntimeError::Io)?;
                    let tmp = dir.join("member.tmp");
                    std::fs::write(&tmp, naming.as_bytes()).map_err(RuntimeError::Io)?;
                    std::fs::File::open(&tmp)
                        .and_then(|f| f.sync_all())
                        .map_err(RuntimeError::Io)?;
                    std::fs::rename(&tmp, &file).map_err(RuntimeError::Io)?;
                }
                Err(e) => return Err(RuntimeError::Io(e)),
            }
            let object = ObjectNode::open(ObjectConfig {
                spec: self.spec.clone(),
                artifact: self.artifact.clone(),
                node: self.name.clone(),
                member: Some(m.clone()),
                members: self.members.clone(),
                fs: Arc::new(RealFs),
                dir: dir.join("store"),
                seed: self.spec.seed()?,
                now: wall_now().map_err(RuntimeError::Config)?,
                nonce: OsEntropy.boot_nonce().map_err(RuntimeError::Config)?,
                random: Box::new(|_| {
                    Err(blossom_base::unimplemented_error!(
                        "LANG-153",
                        "pages connected to a keyed member (KEYED.md §4, sub-slice 3)"
                    )
                    .into())
                }),
                externs: self.externs.clone(),
            })?;
            self.objects.insert(m.clone(), object);
            bump(&self.stats.opened, 1);
            self.stats.members.store(self.objects.len() as u64, Ordering::Relaxed);
        }
        self.objects
            .get_mut(m)
            .ok_or_else(|| internal_error!("a member opened is not running").into())
    }

    fn deliver(&mut self, msg: Inbound) -> Result<(), RuntimeError> {
        let principal = self
            .spec
            .nodes
            .get(msg.via.0 as usize)
            .map(|n| n.principal.clone())
            .unwrap_or_default();
        let now = wall_now().map_err(RuntimeError::Config)?;
        self.object(&msg.to)?
            .deliver(msg.from, &principal, msg.rel, msg.row, now)?;
        bump(&self.stats.delivered, 1);
        self.route(&msg.to)
    }

    fn wake_due(&mut self, now: Instant) -> Result<(), RuntimeError> {
        let mut due = Vec::new();
        for (m, o) in &self.objects {
            if o.next_wake()?.is_some_and(|at| at <= now) {
                due.push(m.clone());
            }
        }
        for m in due {
            if let Some(o) = self.objects.get_mut(&m) {
                o.wake(now)?;
            }
            self.route(&m)?;
        }
        Ok(())
    }

    fn next_wake(&self) -> Result<Option<Instant>, RuntimeError> {
        let mut next: Option<Instant> = None;
        for o in self.objects.values() {
            if let Some(at) = o.next_wake()? {
                next = Some(next.map_or(at, |n| n.min(at)));
            }
        }
        Ok(next)
    }

    /// Sends what member `m`'s released ticks sent: to a member of this host, here; to another host's member, to that
    /// host; to a node, to it. Each leaves as `FromMember`, so its receiver knows `m` sent it.
    fn route(&mut self, m: &MemberRef) -> Result<(), RuntimeError> {
        let (sender, out) = match self.objects.get_mut(m) {
            Some(o) => (o.me(), o.take_output()),
            None => return Ok(()),
        };
        let mut remote: BTreeMap<(NodeId, RelId, u64), Vec<Row>> = BTreeMap::new();
        for o in out {
            let Output::Send { to, rel, row, tick } = o else {
                return Err(internal_error!("a keyed member wrote to a connection; members have none yet").into());
            };
            if to.is_client() {
                return Err(blossom_base::unimplemented_error!(
                    "LANG-153",
                    "a keyed member's message to a page (KEYED.md §4, sub-slice 3)"
                )
                .into());
            }
            let dest = match self.members.get(to) {
                Some(target) => {
                    let host = self.routing.host_of(&target)?;
                    if host == self.me {
                        self.local.push_back(Inbound {
                            to: target,
                            from: sender,
                            via: self.me,
                            rel,
                            row,
                        });
                        continue;
                    }
                    host
                }
                None if to == self.me => {
                    return Err(internal_error!("a keyed member sent to its host, which runs no rules").into());
                }
                None => to,
            };
            remote.entry((dest, rel, tick)).or_default().push(row);
        }
        let program = self.artifact.program.clone();
        let p = program.get();
        let codec = net::wire_codec(p);
        for ((dest, rel, tick), rows) in remote {
            let Some(q) = self.peers.get(&dest) else {
                return Err(RuntimeError::Fault(format!(
                    "a send to node {}, which is not in the deployment",
                    dest.0
                )));
            };
            let sid = self
                .catalog
                .sid(rel)
                .ok_or_else(|| internal_error!("{rel:?} is not a channel"))?;
            let refs: Vec<&Row> = rows.iter().collect();
            let (batches, oversized) = net::batches(&codec, p, sid, rel, tick, &refs)?;
            bump(&self.stats.dropped_oversized, oversized);
            for batch in batches {
                bump(&self.stats.sent, batch.count);
                let f = Frame::FromMember {
                    role: m.role.raw(),
                    key: m.key.to_string(),
                    batch,
                };
                if let Err(TrySendError::Full(_)) = q.try_send(f.encode()) {
                    bump(&self.stats.dropped_queue_full, 1);
                }
            }
        }
        Ok(())
    }
}
