//! A node as a Durable Object hosts it (docs/design/DURABLE-OBJECTS.md): one node of a deployment, its store on any
//! [`Vfs`] (a key-value store's, [`blossom_store::KvFs`]), and its client members' links, driven by calls instead of
//! threads and sockets.
//!
//! The host calls [`ObjectNode::frame`] for each link frame a member sends, [`ObjectNode::closed`] when a connection
//! ends and [`ObjectNode::wake`] at the time [`ObjectNode::next_wake`] asked for (an alarm). Each call runs the node's
//! ticks until it is quiescent, every tick made durable before the next ([`ManualDriver`]), and queues what the
//! released ticks send: the host writes [`ObjectNode::take_output`] to its sockets after the call. So a tick's
//! messages leave after its writes, as Invariant R requires; on a Durable Object the output gate holds them until the
//! writes are committed besides. The host gives each call its wall clock; the node's time never goes back (a step
//! back runs at the node's last instant).
//!
//! An object may also run a keyed member (docs/design/KEYED.md) for the host of its role: the host hands it the
//! messages addressed to it ([`ObjectNode::deliver`]) and routes what it sends to other nodes and members
//! ([`Output::Send`]).
//!
//! What an object does not do yet: answer external sessions, or run streams. A program that needs them is refused
//! when its tick sends there.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{RelId, RoleId, internal_error};
use blossom_ir::core::Program;
use blossom_ir::members::Members;
use blossom_node::Executor;
use blossom_node::acl::{AclTable, Source};
use blossom_node::eval::{Backend, Executors};
use blossom_node::manual::ManualDriver;
use blossom_node::recovery::{self, StoreSpec};
use blossom_node::{Node, NodeConfig, ReleasedTick};
use blossom_oracle::{Delivery, Oracle, Row};
use blossom_store::{ClientRegistry, OpenMode, Vfs};
use blossom_value::time::{Instant, MemberRef, NodeId};
use blossom_value::{ExternRegistry, Seed};
use blossom_wire::codec::WireLimits;
use blossom_wire::frame::{Frame, RejectReason};

use crate::RuntimeError;
use crate::deploy::DeploymentSpec;
use crate::members::{
    AdmitError, Admitted, ClientRole, Host, LinkConn, LinkImage, MemberEvent, MemberLinks, admit_with, identify_in,
    member_event,
};
use crate::net::{Catalog, Identity};

/// What the host does after a call: write a frame to a connection, end one, or route a message the node sent to
/// another node or member (column 0 of `row` is the destination; `to` its id in the host's member table), sent by
/// the node's tick `tick`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    Frame {
        conn: u64,
        bytes: Vec<u8>,
    },
    Close {
        conn: u64,
    },
    Send {
        to: NodeId,
        rel: RelId,
        row: Row,
        tick: u64,
    },
}

/// Where a message to another node goes when objects reach each other by RPC (Durable Objects, KEYED.md §4): a node of
/// the deployment's object (by name), or a keyed member's.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    Node(String),
    Member(MemberRef),
}

/// Fills a buffer with secret random bytes.
pub type Random = Box<dyn FnMut(&mut [u8]) -> Result<(), RuntimeError> + Send>;

/// Where an object's node starts.
pub struct ObjectConfig {
    pub spec: DeploymentSpec,
    /// The deployment's program, compiled for its nodes.
    pub artifact: Arc<BlsArtifact>,
    /// The node this object runs, or, for a keyed member, the node that hosts it.
    pub node: String,
    /// The keyed member this object runs, if it runs one (docs/design/KEYED.md), and the host's member table, which
    /// gives the member and those it meets their ids.
    pub member: Option<MemberRef>,
    pub members: Arc<Members>,
    /// Its store's filesystem, and the store's directory there.
    pub fs: Arc<dyn Vfs>,
    pub dir: PathBuf,
    /// The deployment's seed, the time, and this start's nonce.
    pub seed: Seed,
    pub now: Instant,
    pub nonce: u64,
    /// Fills a buffer with secret random bytes (client members' tokens).
    pub random: Random,
    pub externs: Arc<ExternRegistry>,
    /// The node's state when it last hibernated (a stateless host, docs/design/STATELESS.md §5a): it resumes from it
    /// instead of starting again. `None`: it starts (a fresh store, or a restart).
    pub hibernation: Option<blossom_node::Hibernation>,
    /// The tree the node's database keeps its rows in, when not the LSM under its store (SQL tables,
    /// docs/design/SQL-TABLES.md).
    pub tree: Option<Box<dyn blossom_store::tree::KeyTree>>,
}

/// A connection's frames, queued for the host.
struct QueuedConn {
    conn: u64,
    out: Arc<Mutex<Vec<Output>>>,
}

impl LinkConn for QueuedConn {
    fn write(&self, frame: Vec<u8>) -> bool {
        match self.out.lock() {
            Ok(mut out) => {
                out.push(Output::Frame {
                    conn: self.conn,
                    bytes: frame,
                });
                true
            }
            Err(_) => false,
        }
    }

    fn close(&self) {
        if let Ok(mut out) = self.out.lock() {
            out.push(Output::Close { conn: self.conn });
        }
    }
}

/// A connection, before and after its `HELLO`.
enum ConnState {
    Opening,
    Member {
        member: NodeId,
        inbound: BTreeMap<u32, RelId>,
    },
}

/// Counters of what an object delivered, dropped or refused.
#[derive(Default, Debug)]
pub struct ObjectStats {
    pub delivered: AtomicU64,
    pub rejected_acl: AtomicU64,
    pub dropped_unroutable: AtomicU64,
    pub dropped_closed_session: AtomicU64,
    pub rejected_schema: AtomicU64,
    pub link_failures: AtomicU64,
}

fn bump(c: &AtomicU64, n: u64) {
    c.fetch_add(n, Ordering::Relaxed);
}

/// A connection as it is kept between requests (docs/design/STATELESS.md §6.2): the member whose link it carries,
/// and the channels' numbers its `HELLO` agreed.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConnImage {
    pub member: u32,
    pub inbound: Vec<(u32, u32)>,
}

/// One write to an object's entries: a value, or `None` to delete the key.
pub type EntryWrite = (String, Option<Vec<u8>>);

/// The prefix of the links' entries among an object's entries.
pub const LINKS_PREFIX: &str = "L/";
const NEXT_CONN_KEY: &str = "L/n";

fn page_key(id: NodeId) -> String {
    format!("L/p/{:08x}", id.0)
}

fn conn_key(conn: u64) -> String {
    format!("L/c/{conn:016x}")
}

fn hex_of<T: TryFrom<u64>>(key: &str, prefix: &str) -> Result<T, RuntimeError> {
    key.strip_prefix(prefix)
        .and_then(|h| u64::from_str_radix(h, 16).ok())
        .and_then(|n| T::try_from(n).ok())
        .ok_or_else(|| {
            RuntimeError::Store(blossom_store::StoreError::Invalid(format!(
                "a malformed link entry {key}"
            )))
        })
}

/// The pages' links and their connections, as an object's entries keep them between requests (docs/design/
/// STATELESS.md §6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkState {
    pub pages: BTreeMap<NodeId, LinkImage>,
    pub conns: BTreeMap<u64, ConnImage>,
    /// The number of the next connection.
    pub next_conn: u64,
}

/// Where a page's receives are (docs/design/STATELESS.md §6.3): the number of the last batch, and the last
/// acknowledgement, it was given.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub msg: u64,
    pub ack: u64,
}

impl Cursor {
    /// The cursor as the page carries it (opaque to the page).
    pub fn text(&self) -> String {
        format!("{}.{}", self.msg, self.ack)
    }

    pub fn parse(s: &str) -> Option<Cursor> {
        let (m, a) = s.split_once('.')?;
        Some(Cursor {
            msg: m.parse().ok()?,
            ack: a.parse().ok()?,
        })
    }
}

/// What a receive answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Received {
    /// Frames for the page (none: nothing new), and where it is then.
    Frames { frames: Vec<Vec<u8>>, next: Cursor },
    /// The connection's link is over (why): the page opens another and resumes.
    Gone(&'static str),
}

impl LinkState {
    /// The links' state among an object's entries (those under [`LINKS_PREFIX`]; others are ignored).
    pub fn decode<'a>(entries: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Result<LinkState, RuntimeError> {
        let bad = |k: &str, e: postcard::Error| {
            RuntimeError::Store(blossom_store::StoreError::Invalid(format!("link entry {k}: {e}")))
        };
        let mut state = LinkState::default();
        for (k, v) in entries {
            if k == NEXT_CONN_KEY {
                state.next_conn = postcard::from_bytes(v).map_err(|e| bad(k, e))?;
            } else if k.starts_with("L/p/") {
                state.pages.insert(
                    NodeId(hex_of(k, "L/p/")?),
                    postcard::from_bytes(v).map_err(|e| bad(k, e))?,
                );
            } else if k.starts_with("L/c/") {
                state
                    .conns
                    .insert(hex_of(k, "L/c/")?, postcard::from_bytes(v).map_err(|e| bad(k, e))?);
            } else if k.starts_with(LINKS_PREFIX) {
                return Err(RuntimeError::Store(blossom_store::StoreError::Invalid(format!(
                    "an unknown link entry {k}"
                ))));
            }
        }
        Ok(state)
    }

    fn encode(&self) -> Result<BTreeMap<String, Vec<u8>>, RuntimeError> {
        let enc = |e: postcard::Error| internal_error!("encoding a link entry: {e}");
        let mut out = BTreeMap::new();
        out.insert(
            NEXT_CONN_KEY.to_owned(),
            postcard::to_allocvec(&self.next_conn).map_err(enc)?,
        );
        for (id, p) in &self.pages {
            out.insert(page_key(*id), postcard::to_allocvec(p).map_err(enc)?);
        }
        for (c, i) in &self.conns {
            out.insert(conn_key(*c), postcard::to_allocvec(i).map_err(enc)?);
        }
        Ok(out)
    }

    /// The member a connection carries the link of, while it does.
    pub fn member_of(&self, conn: u64) -> Option<NodeId> {
        let c = self.conns.get(&conn)?;
        let member = NodeId(c.member);
        (self.pages.get(&member)?.conn == Some(conn)).then_some(member)
    }

    /// Where a connection just opened is: its open's answer gave its `WELCOME`, the replay after what the page took,
    /// and what the open's ticks sent it.
    pub fn opened(&self, conn: u64) -> Option<Cursor> {
        let p = self.pages.get(&self.member_of(conn)?)?;
        Some(Cursor {
            msg: p.out_next.saturating_sub(1),
            ack: p.in_floor,
        })
    }

    /// What a receive on `conn` from `at` answers (docs/design/STATELESS.md §6.3): the batches after the cursor, and
    /// an `ACK` if the node took more of the page's since.
    pub fn receive(&self, conn: u64, at: Cursor) -> Received {
        let Some(member) = self.member_of(conn) else {
            return Received::Gone("the session ended");
        };
        let Some(p) = self.pages.get(&member) else {
            return Received::Gone("the session ended");
        };
        if p.lost_upto > at.msg {
            return Received::Gone("batches the page had not received left the replay buffer");
        }
        let mut next = at;
        let mut frames = Vec::new();
        for (seq, f) in &p.replay {
            if *seq > at.msg {
                frames.push(f.clone());
                next.msg = *seq;
            }
        }
        if p.in_floor > at.ack {
            frames.push(Frame::Ack { seq: p.in_floor }.encode());
            next.ack = p.in_floor;
        }
        Received::Frames { frames, next }
    }
}

/// One node, run by its host's calls.
pub struct ObjectNode {
    driver: ManualDriver<Box<dyn Executor>>,
    members: MemberLinks,
    registry: ClientRegistry,
    conns: BTreeMap<u64, ConnState>,
    out: Arc<Mutex<Vec<Output>>>,
    artifact: Arc<BlsArtifact>,
    id: Identity,
    catalog: Catalog,
    acl: AclTable,
    oracle: Arc<Oracle>,
    client_roles: BTreeMap<String, ClientRole>,
    me: NodeId,
    names: Arc<[Arc<str>]>,
    seed: Seed,
    restarts: u64,
    nonce: u64,
    app: String,
    random: Random,
    next_conn: u64,
    /// The host's member table (a member's role, for admission).
    members_table: Arc<Members>,
    /// The keyed member this object runs, if it runs one, and this node as rows addressed to it name it.
    member: Option<MemberRef>,
    me_value: blossom_value::Value,
    /// The deployment's node this object is, or hosts its member for (a member's pages' ids are that node's).
    host: NodeId,
    /// Each deployment node's principal, by id.
    principals: Vec<String>,
    pub stats: ObjectStats,
    /// The links' entries as last restored or exported (docs/design/STATELESS.md §6.1), to write only what changed.
    persisted: BTreeMap<String, Vec<u8>>,
}

impl ObjectNode {
    /// Opens the node's store (recovering it, or creating it on the first start) and boots the node.
    pub fn open(cfg: ObjectConfig) -> Result<ObjectNode, RuntimeError> {
        let spec = &cfg.spec;
        let artifact = cfg.artifact.clone();
        let program = artifact.program.get();
        let names = spec.names();
        if names.len() != artifact.nodes.len() || names.iter().zip(&artifact.nodes).any(|(a, b)| **a != *b.as_str()) {
            return Err(internal_error!("the program was compiled for other nodes than the deployment's").into());
        }
        let (host, entry) = spec.node(&cfg.node)?;
        let host_role = artifact.roles.get(host.0 as usize).copied().flatten();
        let (me, role, identity) = match &cfg.member {
            Some(m) => {
                if host_role != Some(m.role) {
                    return Err(RuntimeError::Config(format!(
                        "node {} does not host the role of {}",
                        cfg.node,
                        blossom_ir::members::member_name(m)
                    )));
                }
                let me = cfg
                    .members
                    .id(m)
                    .ok_or_else(|| RuntimeError::Config("the host has given every member id".into()))?;
                let mut identity = crate::server::store_identity(spec, &artifact, &cfg.node)?;
                identity.node_name = blossom_ir::members::member_name(m).into();
                (me, Some(m.role), identity)
            }
            None if host_role.is_some_and(|r| program.is_keyed(r)) => {
                return Err(RuntimeError::Config(format!(
                    "node {} hosts the keyed role `{}`: it runs that role's members, each an object of its own",
                    entry.name,
                    host_role
                        .and_then(|r| program.roles.get(r))
                        .map(|r| r.name.to_string())
                        .unwrap_or_default()
                )));
            }
            None => (
                host,
                host_role,
                crate::server::store_identity(spec, &artifact, &cfg.node)?,
            ),
        };
        let members = cfg.members.clone();
        let executors = Executors::new(
            Backend::Engine,
            artifact.program.clone(),
            artifact.roles.clone(),
            names.to_vec(),
            cfg.seed,
            cfg.externs.clone(),
            members.clone(),
        )?
        .tiered(spec.tiered);
        let oracle = executors.oracle().clone();
        // A store that does not exist yet is created: an object's first start.
        let fresh = cfg.fs.list(&cfg.dir).map_or(true, |entries| entries.is_empty());
        cfg.fs.create_dir_all(&cfg.dir)?;
        let opened = recovery::open_with(
            cfg.fs.clone(),
            &StoreSpec {
                dir: cfg.dir.clone(),
                identity,
                mode: if fresh { OpenMode::InitFresh } else { OpenMode::Existing },
                certification: spec.tail_certification,
                database: blossom_store::lsm::LsmOptions {
                    history: spec.history_ticks,
                    ..blossom_store::lsm::LsmOptions::default()
                },
            },
            cfg.tree,
            &artifact.program,
            names.clone(),
            cfg.now,
            cfg.nonce,
        )?;
        let restarts = opened.record.restarts;
        let mut ncfg = NodeConfig::new(me, role);
        ncfg.halt = artifact.halt;
        ncfg.max_stream_bytes = spec.stream_limits.max_stream_bytes;
        ncfg.statics = spec.static_rows(program, &names)?;
        let exec = executors.make(me)?;
        let node = match cfg.hibernation {
            Some(h) => Node::resume(ncfg, &artifact.program, exec, opened.boot.clone(), h)?,
            None => Node::boot(ncfg, &artifact.program, exec, opened.boot.clone())?,
        };
        if !node.streams().is_empty() {
            return Err(blossom_base::unimplemented_error!(
                "DIST-001",
                "streams at a node an object runs (docs/design/DURABLE-OBJECTS.md)"
            )
            .into());
        }
        let schema = blossom_node::durable::DurableSchema::of(program);
        let registry = ClientRegistry::open(cfg.fs.clone(), &cfg.dir)?;
        let driver = ManualDriver::new(node, &artifact.program, &schema, names.clone(), opened);
        let client_roles = crate::members::project_clients(&artifact)?;
        let client_names: Vec<String> = client_roles.keys().cloned().collect();
        let app = crate::web::app_json(spec, &client_names, &cfg.node, crate::web::Transport::WebSocket)
            .map_err(RuntimeError::Config)?;
        Ok(ObjectNode {
            members: MemberLinks::of(program),
            driver,
            registry,
            conns: BTreeMap::new(),
            out: Arc::new(Mutex::new(Vec::new())),
            id: crate::server::identity(spec, &artifact),
            catalog: Catalog::of(program)?,
            acl: AclTable::of(program),
            oracle,
            client_roles,
            me,
            names,
            seed: cfg.seed,
            restarts,
            nonce: cfg.nonce,
            app,
            random: cfg.random,
            next_conn: 0,
            me_value: members.value(me),
            host,
            principals: spec.nodes.iter().map(|n| n.principal.clone()).collect(),
            member: cfg.member.clone(),
            members_table: members,
            stats: ObjectStats::default(),
            persisted: BTreeMap::new(),
            artifact,
        })
    }

    /// `/blossom/app.json` for the pages this object serves (their link is a WebSocket).
    pub fn app_json(&self) -> &str {
        &self.app
    }

    /// A client role's part of the program, as `/blossom/client/ROLE` serves it.
    pub fn client_part(&self, role: &str) -> Option<&[u8]> {
        self.client_roles.get(role).map(|r| &*r.artifact)
    }

    /// The incarnation's restart count (each wake of a hibernated object is one).
    pub fn restarts(&self) -> u64 {
        self.restarts
    }

    /// A new connection (a socket the host accepted); its link starts with the member's `HELLO`.
    pub fn connect(&mut self) -> u64 {
        let conn = self.next_conn;
        self.next_conn += 1;
        self.conns.insert(conn, ConnState::Opening);
        conn
    }

    /// One frame from a connection (a WebSocket's binary message), then the ticks it makes ready.
    pub fn frame(&mut self, conn: u64, bytes: &[u8], now: Instant) -> Result<(), RuntimeError> {
        let frame = match Frame::parse(bytes, &WireLimits::default())? {
            Some((f, used)) if used == bytes.len() => f,
            _ => {
                self.close(conn);
                return Ok(());
            }
        };
        match self.conns.get(&conn) {
            None => return Ok(()),
            Some(ConnState::Opening) => {
                let Frame::Hello(h) = frame else {
                    self.close(conn);
                    return Ok(());
                };
                self.hello(conn, h)?;
            }
            Some(ConnState::Member { member, inbound }) => {
                let p = self.artifact.program.get();
                let codec = crate::net::wire_codec(p);
                match member_event(&codec, p, (*member, conn), inbound, frame)? {
                    Some(event) => self.handle(event)?,
                    // A frame a member does not send on its link ends it.
                    None => self.close(conn),
                }
            }
        }
        self.run(now)
    }

    /// An event of a page's link that the host's web listener admitted for the keyed member this object runs
    /// (crate::hosting), then the ticks it makes ready.
    pub(crate) fn link_event(&mut self, event: MemberEvent, now: Instant) -> Result<(), RuntimeError> {
        self.handle(event)?;
        self.run(now)
    }

    /// The connection ended (the member closed it, or the socket failed).
    pub fn closed(&mut self, conn: u64, now: Instant) -> Result<(), RuntimeError> {
        if let Some(ConnState::Member { member, .. }) = self.conns.remove(&conn) {
            self.handle(MemberEvent::Closed { member, conn })?;
        }
        self.run(now)
    }

    /// The time came (an alarm): the timers due run.
    pub fn wake(&mut self, now: Instant) -> Result<(), RuntimeError> {
        self.run(now)
    }

    /// When the node next needs to run with nothing arriving: its earliest timer.
    pub fn next_wake(&self) -> Result<Option<Instant>, RuntimeError> {
        Ok(self.driver.node.next_deadline()?)
    }

    /// What to write to the connections, in order.
    pub fn take_output(&mut self) -> Vec<Output> {
        match self.out.lock() {
            Ok(mut out) => std::mem::take(&mut *out),
            Err(_) => Vec::new(),
        }
    }

    /// The node's state to resume from at the next request (docs/design/STATELESS.md §5a).
    pub fn hibernate(&self) -> Result<blossom_node::Hibernation, RuntimeError> {
        Ok(self.driver.node.hibernate()?)
    }

    /// The pages' links and their connections now. Only a connection that still carries its member's link is
    /// kept: one a reconnect replaced, or one still in its handshake, ended with the request.
    pub fn link_state(&self) -> Result<LinkState, RuntimeError> {
        let pages = self.members.images()?;
        let conns = self
            .conns
            .iter()
            .filter_map(|(c, s)| match s {
                ConnState::Member { member, inbound } if pages.get(member).is_some_and(|p| p.conn == Some(*c)) => {
                    Some((
                        *c,
                        ConnImage {
                            member: member.0,
                            inbound: inbound.iter().map(|(sid, rel)| (*sid, rel.raw())).collect(),
                        },
                    ))
                }
                _ => None,
            })
            .collect();
        Ok(LinkState {
            pages,
            conns,
            next_conn: self.next_conn,
        })
    }

    /// Takes up the links an object's entries kept (a node loaded for a request, docs/design/STATELESS.md §6.1).
    pub fn restore_links(&mut self, state: LinkState) -> Result<(), RuntimeError> {
        self.persisted = state.encode()?;
        self.next_conn = state.next_conn;
        self.conns = state
            .conns
            .iter()
            .map(|(c, i)| {
                (
                    *c,
                    ConnState::Member {
                        member: NodeId(i.member),
                        inbound: i
                            .inbound
                            .iter()
                            .map(|(sid, rel)| (*sid, RelId::from_raw(*rel)))
                            .collect(),
                    },
                )
            })
            .collect();
        let out = self.out.clone();
        self.members
            .restore(state.pages, &mut |conn| Box::new(QueuedConn { conn, out: out.clone() }));
        Ok(())
    }

    /// The writes that bring the links' entries up to date: what changed since the last restore or export.
    pub fn export_links(&mut self) -> Result<Vec<EntryWrite>, RuntimeError> {
        let now = self.link_state()?.encode()?;
        let mut writes = Vec::new();
        for (k, v) in &now {
            if self.persisted.get(k) != Some(v) {
                writes.push((k.clone(), Some(v.clone())));
            }
        }
        for k in self.persisted.keys() {
            if !now.contains_key(k) {
                writes.push((k.clone(), None));
            }
        }
        self.persisted = now;
        Ok(writes)
    }

    /// The node's committed rows of a durable relation, by its name (for tests and tools).
    pub fn rows(&self, rel: &str) -> Result<Vec<Row>, RuntimeError> {
        let id = self
            .artifact
            .rel_named(rel)
            .ok_or_else(|| RuntimeError::Config(format!("no relation `{rel}`")))?;
        let image = self.driver.released_image()?;
        Ok(image.rows.get(&id).cloned().unwrap_or_default().into_iter().collect())
    }

    fn push(&self, o: Output) {
        if let Ok(mut out) = self.out.lock() {
            out.push(o);
        }
    }

    /// A message to this node from another node or member of the deployment (`from` its id in the host's member
    /// table), admitted by its channel's ACL as from `principal`, then the ticks it makes ready.
    pub fn deliver(
        &mut self,
        from: NodeId,
        principal: &str,
        rel: RelId,
        row: Row,
        now: Instant,
    ) -> Result<(), RuntimeError> {
        let role = match self.members_table.get(from) {
            Some(m) => Some(m.role),
            None => self.artifact.roles.get(from.0 as usize).copied().flatten(),
        };
        let source = Source::Node { role, principal };
        if self.driver.admits(&self.acl, self.oracle.static_facts(), rel, source)? {
            bump(&self.stats.delivered, 1);
            self.driver.node.offer_delivery(Delivery { rel, from, row });
        } else {
            bump(&self.stats.rejected_acl, 1);
        }
        self.run(now)
    }

    /// The keyed member this object runs, if it runs one.
    pub fn member(&self) -> Option<&MemberRef> {
        self.member.as_ref()
    }

    /// The deployment node this object is, or hosts its member for.
    pub fn name(&self) -> &str {
        self.names.get(self.host.0 as usize).map_or("", |n| n)
    }

    /// This node's id: a deployment node's, or a keyed member's in the host's table.
    pub fn me(&self) -> NodeId {
        self.me
    }

    /// Frames for what the node sent other nodes and members (`Output::Send`s), when objects reach each other by
    /// RPC: per destination object and channel, `BATCH`es from a deployment node (the RPC names the sender) or
    /// `FROM_MEMBER`s from a keyed member.
    pub fn rpc_frames(&self, sends: &[(NodeId, RelId, Row, u64)]) -> Result<Vec<(Target, Vec<u8>)>, RuntimeError> {
        let p = self.artifact.program.get();
        let codec = crate::net::wire_codec(p);
        let mut by: BTreeMap<(Target, RelId, u64), Vec<&Row>> = BTreeMap::new();
        for (to, rel, row, tick) in sends {
            let target =
                match self.members_table.get(*to) {
                    Some(m) => Target::Member(m),
                    None => Target::Node(self.names.get(to.0 as usize).map(|n| n.to_string()).ok_or_else(|| {
                        RuntimeError::Fault(format!("a send to node {}, not in the deployment", to.0))
                    })?),
                };
            by.entry((target, *rel, *tick)).or_default().push(row);
        }
        let mut out = Vec::new();
        for ((target, rel, tick), rows) in by {
            let sid = self
                .catalog
                .sid(rel)
                .ok_or_else(|| internal_error!("{rel:?} is not a channel"))?;
            let (batches, oversized) = crate::net::batches(&codec, p, sid, rel, tick, &rows)?;
            bump(&self.stats.dropped_unroutable, oversized);
            for batch in batches {
                let frame = match &self.member {
                    Some(m) => Frame::FromMember {
                        role: m.role.raw(),
                        key: m.key.to_string(),
                        batch,
                    },
                    None => Frame::Batch(batch),
                };
                out.push((target.clone(), frame.encode()));
            }
        }
        Ok(out)
    }

    /// A frame another object of the deployment sent this one by RPC: a `BATCH` from the deployment's node `sender`
    /// (the RPC names it), or a `FROM_MEMBER` from a keyed member; then the ticks it makes ready. A frame that is
    /// neither, or names no sender, is refused (counted), not a fault.
    pub fn rpc_frame(&mut self, sender: Option<&str>, bytes: &[u8], now: Instant) -> Result<(), RuntimeError> {
        let p = self.artifact.program.clone();
        let program = p.get();
        let frame = match Frame::parse(bytes, &WireLimits::default()) {
            Ok(Some((f, used))) if used == bytes.len() => f,
            _ => {
                bump(&self.stats.rejected_schema, 1);
                return Ok(());
            }
        };
        let sender_node = |name: &str| self.names.iter().position(|n| **n == *name).map(|i| NodeId(i as u32));
        let (from, via, batch) = match (frame, sender) {
            (Frame::Batch(b), Some(name)) => match sender_node(name) {
                Some(n) => (n, n, b),
                None => {
                    bump(&self.stats.dropped_unroutable, b.count);
                    return Ok(());
                }
            },
            (Frame::FromMember { role, key, batch }, _) => {
                let m = crate::keyed::member_of(program, role, key)?;
                let id = self
                    .members_table
                    .id(&m)
                    .ok_or_else(|| RuntimeError::Fault("this object has given every keyed member id".into()))?;
                // A member's principal is its role's host's: the deployment's first node of the role.
                let host = self
                    .artifact
                    .roles
                    .iter()
                    .position(|r| *r == Some(m.role))
                    .map_or(id, |i| NodeId(i as u32));
                (id, host, batch)
            }
            _ => {
                bump(&self.stats.rejected_schema, 1);
                return Ok(());
            }
        };
        let inbound = self.catalog.accept(&self.catalog.schemas());
        let Some(&rel) = inbound.get(&batch.sid) else {
            bump(&self.stats.rejected_schema, batch.count);
            return Ok(());
        };
        let codec = crate::net::wire_codec(program);
        let principal = self.principals.get(via.0 as usize).cloned().unwrap_or_default();
        for row in crate::net::batch_rows(&codec, program, rel, &batch)? {
            if row.first() != Some(&self.me_value) {
                bump(&self.stats.dropped_unroutable, 1);
                continue;
            }
            let role = match self.members_table.get(from) {
                Some(m) => Some(m.role),
                None => self.artifact.roles.get(from.0 as usize).copied().flatten(),
            };
            if self.driver.admits(
                &self.acl,
                self.oracle.static_facts(),
                rel,
                Source::Node {
                    role,
                    principal: &principal,
                },
            )? {
                bump(&self.stats.delivered, 1);
                self.driver.node.offer_delivery(Delivery { rel, from, row });
            } else {
                bump(&self.stats.rejected_acl, 1);
            }
        }
        self.run(now)
    }

    fn close(&mut self, conn: u64) {
        QueuedConn {
            conn,
            out: self.out.clone(),
        }
        .close();
    }

    fn hello(&mut self, conn: u64, h: blossom_wire::frame::Hello) -> Result<(), RuntimeError> {
        let (registry, me, random) = (&mut self.registry, self.me, &mut self.random);
        let admitted = match &self.member {
            // A keyed member's pages bring tokens the deployment signed (one registry object minted them, KEYED.md):
            // their ids are the host node's, so every member names a page alike.
            Some(own) => {
                let (seed, host) = (self.seed, self.host);
                let own = own.clone();
                let keyed = move |t: Option<&(String, String)>| match t {
                    Some((role, key)) if *role == *own.role_name && *key == *own.key => Ok(Some(own.clone())),
                    _ => Err((
                        RejectReason::NotAllowed,
                        format!("this object runs {}", blossom_ir::members::member_name(&own)),
                    )),
                };
                admit_with(
                    &self.id,
                    &self.catalog,
                    &self.client_roles,
                    &keyed,
                    h,
                    &mut |role, token| crate::members::identify_signed(seed, host, role, token),
                )
            }
            None => admit_with(
                &self.id,
                &self.catalog,
                &self.client_roles,
                &crate::members::no_keyed,
                h,
                &mut |role, token| identify_in(registry, me, role, token, random).map_err(AdmitError::Failed),
            ),
        };
        let a: Admitted = match admitted {
            Ok(a) => a,
            Err(AdmitError::Refused(reason, detail)) => {
                self.refuse(conn, reason, detail);
                return Ok(());
            }
            Err(AdmitError::Failed(e)) => return Err(e),
        };
        let link = QueuedConn {
            conn,
            out: self.out.clone(),
        };
        let hello = crate::net::hello(
            &self.id,
            blossom_wire::frame::Peer::Node(self.me.0),
            self.restarts,
            self.nonce,
            &self.catalog,
        );
        let ok = Frame::HelloOk {
            accepted_version: self.id.program_version,
            sids: a.inbound.keys().copied().collect(),
        };
        link.write(hello.encode());
        link.write(ok.encode());
        self.conns.insert(
            conn,
            ConnState::Member {
                member: a.member,
                inbound: a.inbound,
            },
        );
        self.handle(MemberEvent::Open {
            member: a.member,
            role: a.role,
            token: a.token,
            received: a.received,
            acked: a.acked,
            keyed: a.keyed,
            conn,
            link: Box::new(link),
        })
    }

    fn refuse(&mut self, conn: u64, reason: RejectReason, detail: String) {
        let link = QueuedConn {
            conn,
            out: self.out.clone(),
        };
        link.write(Frame::Reject { reason, detail }.encode());
        link.close();
        self.conns.remove(&conn);
    }

    fn handle(&mut self, event: MemberEvent) -> Result<(), RuntimeError> {
        let mut host = ObjectHost {
            driver: &mut self.driver,
            me_value: &self.me_value,
            acl: &self.acl,
            oracle: &self.oracle,
            stats: &self.stats,
            me: self.me,
            names: &self.names,
            seed: self.seed,
        };
        self.members.handle(event, &mut host)
    }

    /// Runs ticks while the node is ready, each durable before the next, and dispatches what they release. The
    /// host's time never takes the node back: a wall clock that steps back (or a call with an older time) runs at the
    /// node's last instant, as the runtime's clock is anchored after every instant an earlier incarnation used.
    fn run(&mut self, now: Instant) -> Result<(), RuntimeError> {
        let now = now.max(self.driver.node.last_now());
        loop {
            let released = self.driver.run_until_quiescent(now)?;
            if released.is_empty() {
                return Ok(());
            }
            for t in &released {
                self.dispatch(t)?;
            }
        }
    }

    fn dispatch(&mut self, t: &ReleasedTick) -> Result<(), RuntimeError> {
        let program = self.artifact.program.clone();
        let p: &Program = program.get();
        let codec = crate::net::wire_codec(p);
        let mut to_members: BTreeMap<(NodeId, RelId), Vec<&Row>> = BTreeMap::new();
        for s in &t.sends {
            if s.to.is_client() {
                to_members.entry((s.to, s.rel)).or_default().push(&s.row);
            } else if s.to == self.me {
                self.driver.node.offer_delivery(Delivery {
                    rel: s.rel,
                    from: self.me,
                    row: s.row.clone(),
                });
            } else {
                // Another node or member: the host routes it, after this tick's writes (Invariant R).
                self.push(Output::Send {
                    to: s.to,
                    rel: s.rel,
                    row: s.row.clone(),
                    tick: t.tick.0,
                });
            }
        }
        if !t.egress.is_empty() {
            return Err(blossom_base::unimplemented_error!(
                "LANG-243",
                "replies to external sessions from a node an object runs"
            )
            .into());
        }
        for ((member, rel), rows) in to_members {
            let sid = self
                .catalog
                .sid(rel)
                .ok_or_else(|| internal_error!("{rel:?} is not a channel"))?;
            let (batches, _oversized) = crate::net::batches(&codec, p, sid, rel, t.tick.0, &rows)?;
            let mut host = ObjectHost {
                driver: &mut self.driver,
                me_value: &self.me_value,
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
        let mut host = ObjectHost {
            driver: &mut self.driver,
            me_value: &self.me_value,
            acl: &self.acl,
            oracle: &self.oracle,
            stats: &self.stats,
            me: self.me,
            names: &self.names,
            seed: self.seed,
        };
        self.members.released(t.taken, &mut host);
        Ok(())
    }
}

/// The node and counters, as the member links see them.
struct ObjectHost<'a> {
    driver: &'a mut ManualDriver<Box<dyn Executor>>,
    me_value: &'a blossom_value::Value,
    acl: &'a AclTable,
    oracle: &'a Oracle,
    stats: &'a ObjectStats,
    me: NodeId,
    names: &'a [Arc<str>],
    seed: Seed,
}

impl Host for ObjectHost<'_> {
    fn offer(&mut self, from: NodeId, role: RoleId, rel: RelId, row: Row) -> Result<Option<u64>, RuntimeError> {
        let source = Source::Node {
            role: Some(role),
            principal: "",
        };
        if !self.driver.admits(self.acl, self.oracle.static_facts(), rel, source)? {
            bump(&self.stats.rejected_acl, 1);
            return Ok(None);
        }
        bump(&self.stats.delivered, 1);
        Ok(Some(self.driver.node.offer_delivery(Delivery { rel, from, row })))
    }

    fn event(&mut self, rel: RelId, row: Row) {
        self.driver.node.offer_input(rel, row);
    }

    fn me(&self) -> NodeId {
        self.me
    }

    fn me_value(&self) -> blossom_value::Value {
        self.me_value.clone()
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
        bump(&self.stats.link_failures, 1);
    }
}
