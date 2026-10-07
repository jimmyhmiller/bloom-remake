//! Client members' links (docs/design/CLIENTS.md §2–§3): browser tabs that are nodes of a client role, connected over
//! a WebSocket served by [`crate::web`].
//!
//! **Identity.** A member's first `HELLO` carries no token: the node admits it in its client registry
//! ([`blossom_store::ClientRegistry`]) and mints its id (`NodeId::client(me, serial)`) and a token (the serial and 16
//! random bytes). Presenting the token again resumes the identity; an unknown one gets a new identity.
//!
//! **The link.** Each direction numbers its batches (`MSG { seq }`). The member's messages become deliveries from the
//! member; the node acknowledges them (`ACK`) once the tick that took them is released, so an acknowledged message is
//! durable. What the node sends a member stays in a bounded replay buffer until the member acknowledges it. A
//! reconnect within the same incarnation of the node resumes the link when the buffer holds everything after what the
//! member took (`resumed`); otherwise, and after a restart of the node, it does not, and the link events tell the
//! program so. A receiver drops a batch it already took, by its number.
//!
//! Connection threads do the handshake and decode; the engine thread owns the links' state ([`MemberLinks`]).

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufReader, Read};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{RelId, RoleId, internal_error};
use blossom_ir::core::{EventSource, Program, RelClass};
use blossom_oracle::Row;
use blossom_store::{ClientRegistry, SECRET_LEN};
use blossom_value::Value;
use blossom_value::time::NodeId;
use blossom_wire::codec::{Codec, WireLimits};
use blossom_wire::frame::{Batch, Frame, Peer, RejectReason};

use crate::RuntimeError;
use crate::net::{Catalog, Identity};
use crate::web;

/// The frames queued for one member connection's writer; a full queue closes the connection (the member resumes).
const WRITE_QUEUE: usize = 4096;
/// The most batches, and bytes, a member's replay buffer holds.
const REPLAY_FRAMES: usize = 8192;
const REPLAY_BYTES: usize = 32 * 1024 * 1024;

/// What a member's connection thread tells the engine.
pub(crate) enum MemberEvent {
    /// A member connected: its identity, what it took and what it was acknowledged, and its connection's writer.
    Open {
        member: NodeId,
        role: RoleId,
        token: Vec<u8>,
        received: u64,
        acked: u64,
        conn: u64,
        writer: SyncSender<Vec<u8>>,
        /// The connection's socket, for the engine to close it.
        socket: TcpStream,
    },
    /// A numbered batch of the member's messages, decoded.
    Msg {
        member: NodeId,
        conn: u64,
        seq: u64,
        rel: RelId,
        rows: Vec<Row>,
    },
    /// A numbered batch on a channel whose schema differs between the ends: dropped, and taken.
    Skip { member: NodeId, conn: u64, seq: u64 },
    /// The member took everything up to `seq`.
    Ack { member: NodeId, conn: u64, seq: u64 },
    /// The connection ended.
    Closed { member: NodeId, conn: u64 },
}

/// A client role as the web listener serves it: its id, the digest of its part of the program, and that part encoded
/// (`/blossom/client/ROLE`).
pub(crate) struct ClientRole {
    pub id: RoleId,
    pub part: [u8; 16],
    pub artifact: Arc<[u8]>,
}

/// What the web listener's connection threads share.
#[derive(Clone)]
pub(crate) struct WebCtx {
    pub artifact: Arc<BlsArtifact>,
    pub id: Identity,
    pub me: NodeId,
    pub restarts: u64,
    pub nonce: u64,
    pub catalog: Arc<Catalog>,
    /// Hands a member event to the engine (false once the engine stopped).
    pub post: Arc<dyn Fn(MemberEvent) -> bool + Send + Sync>,
    pub registry: Arc<Mutex<ClientRegistry>>,
    /// `/blossom/app.json`.
    pub app: Arc<str>,
    /// The page's files.
    pub root: Option<PathBuf>,
    pub next_conn: Arc<AtomicU64>,
    /// The program's client roles, by name.
    pub client_roles: Arc<BTreeMap<String, ClientRole>>,
}

/// Serves one HTTP connection: a file, `app.json`, or a member's link.
pub(crate) fn web_conn(stream: TcpStream, ctx: &WebCtx) -> Result<(), RuntimeError> {
    let mut reader = BufReader::new(stream.try_clone().map_err(RuntimeError::Io)?);
    let req = web::read_request(&mut reader)?;
    let mut w = stream;
    if req.method != "GET" {
        return web::respond(&mut w, 405, "Method Not Allowed", "text/plain", b"GET only");
    }
    match req.path.as_str() {
        "/blossom/app.json" => web::respond(&mut w, 200, "OK", "application/json", ctx.app.as_bytes()),
        path if path.starts_with("/blossom/client/") => {
            match path
                .strip_prefix("/blossom/client/")
                .and_then(|r| ctx.client_roles.get(r))
            {
                Some(role) => web::respond(&mut w, 200, "OK", "application/octet-stream", &role.artifact),
                None => web::respond(&mut w, 404, "Not Found", "text/plain", b"no such client role"),
            }
        }
        "/blossom/link" => {
            web::upgrade(&mut w, &req)?;
            reader.get_ref().set_read_timeout(None).map_err(RuntimeError::Io)?;
            link(reader, w, ctx)
        }
        path => match ctx.root.as_ref().and_then(|r| web::file_of(r, path)) {
            Some(file) => {
                let body = std::fs::read(&file).map_err(RuntimeError::Io)?;
                web::respond(&mut w, 200, "OK", web::content_type(&file), &body)
            }
            None => web::respond(&mut w, 404, "Not Found", "text/plain", b"not found"),
        },
    }
}

/// Reads one link frame: a binary message holding exactly one frame. Pings are answered on the way.
fn next_frame(r: &mut impl Read, w: &Mutex<TcpStream>) -> Result<Option<Frame>, RuntimeError> {
    loop {
        match web::read_message(r)? {
            web::Message::Binary(b) => {
                let limits = WireLimits::default();
                return match Frame::parse(&b, &limits)? {
                    Some((f, used)) if used == b.len() => Ok(Some(f)),
                    _ => Err(RuntimeError::Net("a link message that is not exactly one frame".into())),
                };
            }
            web::Message::Ping(p) => {
                let mut s = w
                    .lock()
                    .map_err(|_| internal_error!("a link writer's lock is poisoned"))?;
                web::write_pong(&mut *s, &p).map_err(RuntimeError::Io)?;
            }
            web::Message::Pong => {}
            web::Message::Close => return Ok(None),
            web::Message::Text(_) => return Err(RuntimeError::Net("a text message on a link".into())),
        }
    }
}

fn send_now(w: &Mutex<TcpStream>, f: &Frame) -> Result<(), RuntimeError> {
    let mut s = w
        .lock()
        .map_err(|_| internal_error!("a link writer's lock is poisoned"))?;
    web::write_binary(&mut *s, &f.encode()).map_err(RuntimeError::Io)
}

/// A member's link: the handshake, then its messages until the connection ends.
fn link(mut r: BufReader<TcpStream>, w: TcpStream, ctx: &WebCtx) -> Result<(), RuntimeError> {
    let socket = w.try_clone().map_err(RuntimeError::Io)?;
    let w = Arc::new(Mutex::new(w));
    let Some(Frame::Hello(h)) = next_frame(&mut r, &w)? else {
        return Err(RuntimeError::Net("a link that did not open with HELLO".into()));
    };
    let refuse = |reason: RejectReason, detail: String| -> Result<(), RuntimeError> {
        // The refusal is best-effort: the connection closes either way.
        let _ = send_now(
            &w,
            &Frame::Reject {
                reason,
                detail: detail.clone(),
            },
        );
        Err(RuntimeError::Net(format!("refused a member: {detail}")))
    };
    if let Err((reason, detail)) = crate::net::check_hello(&h, &ctx.id) {
        return refuse(reason, detail);
    }
    let Peer::Member {
        role,
        part,
        token,
        received,
        acked,
    } = h.peer
    else {
        return refuse(RejectReason::NotAllowed, "only client members connect here".into());
    };
    let Some(client) = ctx.client_roles.get(&role) else {
        return refuse(
            RejectReason::NotAllowed,
            format!("`{role}` is not a client role of the program"),
        );
    };
    if part != client.part {
        return refuse(
            RejectReason::Program,
            format!("the page runs another version of `{role}`'s part of the program: load it again"),
        );
    }
    let role_id = client.id;
    let inbound = ctx.catalog.accept(&h.channels);
    let (member, token) = identify(ctx, &role, token)?;
    let conn = ctx.next_conn.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(WRITE_QUEUE);
    let writer = {
        let w = w.clone();
        std::thread::Builder::new()
            .name("member-writer".into())
            .spawn(move || {
                for frame in rx {
                    let Ok(mut s) = w.lock() else { break };
                    if web::write_binary(&mut *s, &frame).is_err() {
                        break;
                    }
                }
                if let Ok(mut s) = w.lock() {
                    // The member is gone or the link was replaced: the close is best-effort.
                    let _ = web::write_close(&mut *s);
                    let _ = s.shutdown(std::net::Shutdown::Both);
                }
            })
            .map_err(RuntimeError::Io)?
    };
    let hello = crate::net::hello(&ctx.id, Peer::Node(ctx.me.0), ctx.restarts, ctx.nonce, &ctx.catalog);
    let ok = Frame::HelloOk {
        accepted_version: ctx.id.program_version,
        sids: inbound.keys().copied().collect(),
    };
    let opened = tx.send(hello.encode()).is_ok()
        && tx.send(ok.encode()).is_ok()
        && (ctx.post)(MemberEvent::Open {
            member,
            role: role_id,
            token,
            received,
            acked,
            conn,
            writer: tx.clone(),
            socket,
        });
    let result = if opened {
        read_messages(&mut r, &w, ctx, member, conn, &inbound)
    } else {
        Ok(())
    };
    (ctx.post)(MemberEvent::Closed { member, conn });
    drop(tx);
    let _ = writer.join();
    result
}

/// A member's messages, posted to the engine until the connection ends.
fn read_messages(
    r: &mut BufReader<TcpStream>,
    w: &Mutex<TcpStream>,
    ctx: &WebCtx,
    member: NodeId,
    conn: u64,
    inbound: &BTreeMap<u32, RelId>,
) -> Result<(), RuntimeError> {
    let program = ctx.artifact.program.clone();
    let p = program.get();
    let codec = crate::net::wire_codec(p);
    loop {
        let frame = match next_frame(r, w) {
            Ok(Some(f)) => f,
            // A closed or failed connection ends the link; the member reconnects.
            Ok(None) | Err(_) => return Ok(()),
        };
        let event = match frame {
            Frame::Msg { seq, batch } => match inbound.get(&batch.sid).copied() {
                Some(rel) => MemberEvent::Msg {
                    member,
                    conn,
                    seq,
                    rel,
                    rows: decode(&codec, p, rel, &batch)?,
                },
                // A channel whose schema differs between the ends: its batch is dropped, and taken (acknowledged).
                None => MemberEvent::Skip { member, conn, seq },
            },
            Frame::Ack { seq } => MemberEvent::Ack { member, conn, seq },
            other => {
                return Err(RuntimeError::Net(format!("a member sent {other:?} on its link")));
            }
        };
        if !(ctx.post)(event) {
            return Ok(());
        }
    }
}

fn decode(codec: &Codec<'_>, p: &Program, rel: RelId, b: &Batch) -> Result<Vec<Row>, RuntimeError> {
    Ok(crate::net::batch_rows(codec, p, rel, b)?)
}

/// A member's identity: the one its token names (if this node gave it out, for the same role), or a new one.
fn identify(ctx: &WebCtx, role: &str, token: Option<Vec<u8>>) -> Result<(NodeId, Vec<u8>), RuntimeError> {
    let mut reg = ctx
        .registry
        .lock()
        .map_err(|_| internal_error!("the client registry's lock is poisoned"))?;
    if let Some(t) = token
        && let (Some(serial), Some(secret)) = (t.get(..4), t.get(4..))
        && let (Ok(serial), Ok(secret)) = (<[u8; 4]>::try_from(serial), <[u8; SECRET_LEN]>::try_from(secret))
    {
        let serial = u32::from_le_bytes(serial);
        if reg.check(serial, &secret) == Some(role)
            && let Some(id) = NodeId::client(ctx.me, serial)
        {
            return Ok((id, t));
        }
    }
    let mut secret = [0u8; SECRET_LEN];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut secret))
        .map_err(|e| RuntimeError::Config(format!("reading /dev/urandom: {e}")))?;
    let serial = reg
        .admit(role, &secret, NodeId::CLIENT_SERIALS)?
        .ok_or_else(|| RuntimeError::Net("this node admitted as many client members as it can".into()))?;
    let id = NodeId::client(ctx.me, serial)
        .ok_or_else(|| RuntimeError::Config(format!("node {} has too high an id to admit client members", ctx.me.0)))?;
    let mut token = serial.to_le_bytes().to_vec();
    token.extend_from_slice(&secret);
    Ok((id, token))
}

/// The connection carrying a member's link.
struct Conn {
    id: u64,
    writer: SyncSender<Vec<u8>>,
    socket: TcpStream,
}

impl Conn {
    /// Writes a frame; `false` when the connection cannot take it (gone, or not keeping up).
    fn write(&self, frame: Vec<u8>) -> bool {
        self.writer.try_send(frame).is_ok()
    }
}

/// One member's link as the engine keeps it.
struct Member {
    role: RoleId,
    /// The connection carrying the link.
    conn: Option<Conn>,
    /// The next number of a batch to the member.
    out_next: u64,
    /// The batches to the member it has not acknowledged, by number.
    replay: VecDeque<(u64, Vec<u8>)>,
    replay_bytes: usize,
    /// The highest number of a batch dropped from the replay buffer unacknowledged (0: none).
    lost_upto: u64,
    /// The highest number of the member's batches taken (offered to the node, or dropped).
    in_floor: u64,
    /// The member's batches waiting for their acknowledgement: the number of the last message each offered to the
    /// node (`None`: it offered none), and its batch number.
    pending: VecDeque<(Option<u64>, u64)>,
}

/// The links of every member this incarnation saw (engine thread).
pub(crate) struct MemberLinks {
    members: BTreeMap<NodeId, Member>,
    /// The link relations: `(peer role, up)`.
    links: BTreeMap<(RoleId, bool), RelId>,
    /// How many messages the node's released ticks took.
    taken: u64,
}

/// What the engine needs from the node and its counters while handling member events.
pub(crate) trait Host {
    /// Offers a delivery to the node; its message number, or `None` when the ACL refused it.
    fn offer(&mut self, from: NodeId, role: RoleId, rel: RelId, row: Row) -> Option<u64>;
    /// Offers a link event, an input of the node's next tick.
    fn event(&mut self, rel: RelId, row: Row);
    fn me(&self) -> NodeId;
    fn dropped_unroutable(&self, n: u64);
    fn dropped_closed(&self, n: u64);
    fn rejected_schema(&self, n: u64);
    /// A member's root seed: derived from the deployment's and its name, so members cannot predict other nodes'
    /// draws.
    fn member_seed(&self, member: NodeId) -> Result<[u8; 16], String>;
    /// A link the engine could not open (counted; the member reconnects).
    fn link_failed(&self);
}

/// Ends `member`'s link on its current connection: closes the socket (its connection thread then ends, and the member
/// reconnects) and tells the program the link is down.
fn drop_link(member: NodeId, m: &mut Member, links: &BTreeMap<(RoleId, bool), RelId>, host: &mut dyn Host) {
    let Some(c) = m.conn.take() else { return };
    // The socket may be closed already; either way the connection is over.
    let _ = c.socket.shutdown(std::net::Shutdown::Both);
    if let Some(rel) = links.get(&(m.role, false)) {
        host.event(*rel, Row::from(vec![Value::Node(member)]));
    }
}

impl MemberLinks {
    pub(crate) fn of(program: &Program) -> MemberLinks {
        let mut links = BTreeMap::new();
        for (id, r) in program.rels.iter_enumerated() {
            if let RelClass::Event(EventSource::Link { peer, up }) = &r.class {
                links.insert((*peer, *up), id);
            }
        }
        MemberLinks {
            members: BTreeMap::new(),
            links,
            taken: 0,
        }
    }

    pub(crate) fn handle(&mut self, e: MemberEvent, host: &mut dyn Host) {
        match e {
            MemberEvent::Open {
                member,
                role,
                token,
                received,
                acked,
                conn,
                writer,
                socket,
            } => self.open(
                member,
                role,
                token,
                (received, acked),
                Conn {
                    id: conn,
                    writer,
                    socket,
                },
                host,
            ),
            MemberEvent::Skip { member, conn, seq } => {
                host.rejected_schema(1);
                self.handle(
                    MemberEvent::Msg {
                        member,
                        conn,
                        seq,
                        rel: RelId::from_raw(0),
                        rows: Vec::new(),
                    },
                    host,
                );
            }
            MemberEvent::Msg {
                member,
                conn,
                seq,
                rel,
                rows,
            } => {
                let me = host.me();
                let Some(m) = self.members.get_mut(&member) else { return };
                if m.conn.as_ref().is_none_or(|c| c.id != conn) {
                    // A message on a connection that was replaced: the member resends it on the new one.
                    return;
                }
                if seq <= m.in_floor {
                    m.pending.push_back((None, seq));
                } else {
                    m.in_floor = seq;
                    let mut last = None;
                    for row in rows {
                        if !crate::net::addressed_to(&row, me) {
                            host.dropped_unroutable(1);
                            continue;
                        }
                        if let Some(n) = host.offer(member, m.role, rel, row) {
                            last = Some(n);
                        }
                    }
                    m.pending.push_back((last, seq));
                }
                self.acknowledge(host);
            }
            MemberEvent::Ack { member, conn, seq } => {
                let Some(m) = self.members.get_mut(&member) else { return };
                if m.conn.as_ref().is_some_and(|c| c.id == conn) {
                    while m.replay.front().is_some_and(|(s, _)| *s <= seq) {
                        if let Some((_, f)) = m.replay.pop_front() {
                            m.replay_bytes = m.replay_bytes.saturating_sub(f.len());
                        }
                    }
                }
            }
            MemberEvent::Closed { member, conn } => {
                let Some(m) = self.members.get_mut(&member) else { return };
                if m.conn.as_ref().is_some_and(|c| c.id == conn) {
                    drop_link(member, m, &self.links, host);
                }
            }
        }
    }

    /// A member's new connection, `(received, acked)` its view of the link.
    fn open(
        &mut self,
        member: NodeId,
        role: RoleId,
        token: Vec<u8>,
        (received, acked): (u64, u64),
        conn: Conn,
        host: &mut dyn Host,
    ) {
        let Ok(seed) = host.member_seed(member) else {
            host.link_failed();
            // The socket may be closed already; either way the connection is over.
            let _ = conn.socket.shutdown(std::net::Shutdown::Both);
            return;
        };
        let fresh = !self.members.contains_key(&member);
        let m = self.members.entry(member).or_insert_with(|| Member {
            role,
            conn: None,
            // A link this incarnation never saw continues the member's numbering.
            out_next: received.saturating_add(1),
            replay: VecDeque::new(),
            replay_bytes: 0,
            lost_upto: 0,
            in_floor: acked,
            pending: VecDeque::new(),
        });
        // A connection the member left behind (it reconnected before this one was seen to close) ends: the program
        // hears the link go down before it comes back up.
        drop_link(member, m, &self.links, host);
        // Resumed when nothing after what the member took is missing from the replay buffer.
        let resumed = !fresh && m.lost_upto <= received && received < m.out_next;
        let welcome = Frame::Welcome {
            member: member.0,
            token,
            resumed,
            floor: m.in_floor,
            seed,
        };
        let mut ok = conn.write(welcome.encode());
        if resumed {
            for (seq, f) in &m.replay {
                if *seq > received && ok {
                    ok = conn.write(f.clone());
                }
            }
        }
        if !ok {
            // It could not take its handshake: closed before the program hears of it; the member reconnects.
            let _ = conn.socket.shutdown(std::net::Shutdown::Both);
            return;
        }
        m.conn = Some(conn);
        if let Some(rel) = self.links.get(&(role, true)) {
            host.event(*rel, Row::from(vec![Value::Node(member), Value::Bool(resumed)]));
        }
    }

    /// The node's released ticks took `taken` messages: acknowledges every member batch whose messages they took.
    pub(crate) fn released(&mut self, taken: u64, host: &mut dyn Host) {
        self.taken = self.taken.max(taken);
        self.acknowledge(host);
    }

    fn acknowledge(&mut self, host: &mut dyn Host) {
        for (member, m) in self.members.iter_mut() {
            let mut upto = None;
            while m.pending.front().is_some_and(|(n, _)| n.is_none_or(|n| n < self.taken)) {
                upto = m.pending.pop_front().map(|(_, s)| s);
            }
            if let (Some(seq), Some(c)) = (upto, m.conn.as_ref())
                && !c.write(Frame::Ack { seq }.encode())
            {
                drop_link(*member, m, &self.links, host);
            }
        }
    }

    /// Sends a released tick's batches to a member: numbered, kept for replay, and written while it is connected.
    /// `false` when the member is not known here (the batch is dropped).
    pub(crate) fn send(&mut self, member: NodeId, batch: Batch, host: &mut dyn Host) -> bool {
        let Some(m) = self.members.get_mut(&member) else {
            host.dropped_closed(batch.count);
            return false;
        };
        let seq = m.out_next;
        m.out_next += 1;
        let frame = Frame::Msg { seq, batch }.encode();
        // A connection that cannot keep up is closed; the member resumes from the replay buffer.
        if m.conn.as_ref().is_some_and(|c| !c.write(frame.clone())) {
            drop_link(member, m, &self.links, host);
        }
        m.replay_bytes += frame.len();
        m.replay.push_back((seq, frame));
        while m.replay.len() > REPLAY_FRAMES || m.replay_bytes > REPLAY_BYTES {
            let Some((s, f)) = m.replay.pop_front() else { break };
            m.replay_bytes = m.replay_bytes.saturating_sub(f.len());
            m.lost_upto = m.lost_upto.max(s);
        }
        true
    }
}
