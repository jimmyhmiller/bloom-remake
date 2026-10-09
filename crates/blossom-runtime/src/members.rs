//! Client members' links (docs/design/CLIENTS.md §2–§3): browser tabs that are nodes of a client role, connected over
//! a WebSocket served by [`crate::web`], or over plain requests ([`crate::http_link`]).
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
//! Connection threads do the handshake and decode; the engine thread owns the links' state ([`MemberLinks`]). A link's
//! connection is a WebSocket's socket or an HTTP session ([`Closer`]); the engine sees the same events either way.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Read};
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
use blossom_value::time::{MemberRef, NodeId};
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

impl MemberEvent {
    /// The connection the event is of.
    pub(crate) fn conn(&self) -> u64 {
        match self {
            MemberEvent::Open { conn, .. }
            | MemberEvent::Msg { conn, .. }
            | MemberEvent::Skip { conn, .. }
            | MemberEvent::Ack { conn, .. }
            | MemberEvent::Closed { conn, .. } => *conn,
        }
    }
}

/// What a member's connection thread tells the engine.
pub(crate) enum MemberEvent {
    /// A member connected: its identity, what it took and what it was acknowledged, and its connection's writer; and
    /// the keyed member it links to, at a host (docs/design/KEYED.md).
    Open {
        member: NodeId,
        role: RoleId,
        token: Vec<u8>,
        received: u64,
        acked: u64,
        keyed: Option<MemberRef>,
        conn: u64,
        /// The connection, as the engine writes to it and ends it.
        link: Box<dyn LinkConn>,
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

/// Each client role's part of the program, projected once: what its pages run, and the digest their links present. A
/// projection that would show a page anything placed at another role is refused.
pub(crate) fn project_clients(artifact: &BlsArtifact) -> Result<BTreeMap<String, ClientRole>, RuntimeError> {
    let program = artifact.program.get();
    let mut client_roles = BTreeMap::new();
    for (role, r) in program.roles.iter_enumerated() {
        if r.kind != blossom_ir::core::RoleKind::Client {
            continue;
        }
        let name = r.name.to_string();
        let client = blossom_artifact::client::ClientArtifact::project(artifact, &name)
            .map_err(|e| RuntimeError::Config(e.to_string()))?;
        let leaks = client.leaks(artifact);
        if !leaks.is_empty() {
            return Err(RuntimeError::Config(format!(
                "the part of the program `{name}`'s pages run would show them: {}",
                leaks.join("; ")
            )));
        }
        let bytes = client.encode().map_err(|e| RuntimeError::Config(e.to_string()))?;
        client_roles.insert(
            name,
            ClientRole {
                id: role,
                part: client.part(),
                artifact: Arc::from(bytes),
            },
        );
    }
    Ok(client_roles)
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
    /// The deployment's stylesheet (`[web] style`), read when asked for, so an edit shows on the next load.
    pub style: Option<PathBuf>,
    pub next_conn: Arc<AtomicU64>,
    /// The program's client roles, by name.
    pub client_roles: Arc<BTreeMap<String, ClientRole>>,
    /// The links over plain requests.
    pub sessions: Arc<crate::http_link::Sessions>,
    /// The keyed member a `HELLO` may name (docs/design/KEYED.md): resolved and checked by the node.
    pub keyed: Arc<KeyedTarget>,
}

/// Resolves the keyed member a member's `HELLO` names (its role's name and its key): the member, none (a link to the
/// node itself), or the refusal to send.
pub(crate) type KeyedTarget =
    dyn Fn(Option<&(String, String)>) -> Result<Option<MemberRef>, (RejectReason, String)> + Send + Sync;

/// A node that runs no keyed members: links to it name none.
pub(crate) fn no_keyed(target: Option<&(String, String)>) -> Result<Option<MemberRef>, (RejectReason, String)> {
    match target {
        None => Ok(None),
        Some((role, key)) => Err((
            RejectReason::NotAllowed,
            format!("this node runs no keyed members: no `{role}` `{key}` here"),
        )),
    }
}

/// A connection carrying a member's link, as the engine writes to it: a connection thread's queue here, a socket of
/// the host elsewhere ([`crate::object`]).
pub(crate) trait LinkConn: Send {
    /// Writes a frame; `false` when the connection cannot take it (gone, or not keeping up).
    fn write(&self, frame: Vec<u8>) -> bool;
    /// Ends the connection; the member reconnects.
    fn close(&self);
}

/// A connection served by a thread of this runtime: frames go to its writer's queue.
pub(crate) struct ThreadConn {
    pub writer: SyncSender<Vec<u8>>,
    pub closer: Closer,
}

impl LinkConn for ThreadConn {
    fn write(&self, frame: Vec<u8>) -> bool {
        self.writer.try_send(frame).is_ok()
    }
    fn close(&self) {
        self.closer.close();
    }
}

/// How the engine ends a link's connection: a WebSocket's socket, or an HTTP session.
pub(crate) enum Closer {
    Socket(TcpStream),
    Session(Arc<crate::http_link::Session>),
}

impl Closer {
    /// Ends the connection: its thread (or the session's next request) then ends, and the member reconnects. The
    /// engine, which closes it, already knows: nothing is posted back.
    pub(crate) fn close(&self) {
        match self {
            // The socket may be closed already; either way the connection is over.
            Closer::Socket(s) => {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
            Closer::Session(s) => s.end(false),
        }
    }
}

/// Fills `buf` from the operating system's entropy.
pub(crate) fn urandom(buf: &mut [u8]) -> Result<(), RuntimeError> {
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(buf))
        .map_err(|e| RuntimeError::Config(format!("reading /dev/urandom: {e}")))
}

/// Serves one HTTP connection: its requests (a file, `app.json`, a client role's part, a request of a link over HTTP)
/// while the client keeps it, or a member's WebSocket link.
pub(crate) fn web_conn(stream: TcpStream, ctx: &WebCtx) -> Result<(), RuntimeError> {
    let mut reader = BufReader::new(stream.try_clone().map_err(RuntimeError::Io)?);
    let mut w = stream;
    let mut first = true;
    loop {
        // Between requests on a kept connection, its end (or no request within the read timeout) ends it: no failure.
        if !first && reader.fill_buf().map_or(true, |b| b.is_empty()) {
            return Ok(());
        }
        first = false;
        let req = web::read_request(&mut reader)?;
        if req.path == "/blossom/link" {
            web::upgrade(&mut w, &req)?;
            reader.get_ref().set_read_timeout(None).map_err(RuntimeError::Io)?;
            return link(reader, w, ctx);
        }
        // A request with a body this listener does not read closes its connection (the body is not taken as the next
        // request); a link's requests read theirs.
        let link_request = req.path.starts_with("/blossom/http/");
        let bodied =
            req.header("content-length").is_some_and(|l| l != "0") || req.header("transfer-encoding").is_some();
        let keep = req.keep_alive() && (link_request || !bodied);
        if let Some(rest) = req.path.strip_prefix("/blossom/http/") {
            let rest = rest.to_owned();
            crate::http_link::serve(&rest, &req, &mut reader, &mut w, ctx, keep)?;
        } else {
            serve_get(&req, &mut w, ctx, keep)?;
        }
        if !keep {
            return Ok(());
        }
    }
}

/// A `GET` for a file, `app.json` or a client role's part.
fn serve_get(req: &web::Request, w: &mut TcpStream, ctx: &WebCtx, keep: bool) -> Result<(), RuntimeError> {
    let r = web::Response::new;
    if req.method != "GET" {
        return r(405, "Method Not Allowed", "text/plain", b"GET only").write(w, keep);
    }
    match req.path.as_str() {
        "/blossom/app.json" => r(200, "OK", "application/json", ctx.app.as_bytes()).write(w, keep),
        web::STYLE_PATH if let Some(file) = &ctx.style => {
            let body = std::fs::read(file).map_err(RuntimeError::Io)?;
            r(200, "OK", "text/css; charset=utf-8", &body).write(w, keep)
        }
        path if path.starts_with("/blossom/client/") => {
            match path
                .strip_prefix("/blossom/client/")
                .and_then(|r| ctx.client_roles.get(r))
            {
                Some(role) => r(200, "OK", "application/octet-stream", &role.artifact).write(w, keep),
                None => r(404, "Not Found", "text/plain", b"no such client role").write(w, keep),
            }
        }
        path => match ctx.root.as_ref().and_then(|root| web::file_of(root, path)) {
            Some(file) => {
                let body = std::fs::read(&file).map_err(RuntimeError::Io)?;
                r(200, "OK", web::content_type(&file), &body).write(w, keep)
            }
            None => r(404, "Not Found", "text/plain", b"not found").write(w, keep),
        },
    }
}

/// A member admitted by its `HELLO`: who it is, and the link as it sees it.
pub(crate) struct Admitted {
    pub member: NodeId,
    pub role: RoleId,
    pub token: Vec<u8>,
    pub received: u64,
    pub acked: u64,
    /// The member's channels this node takes, by the member's sid.
    pub inbound: BTreeMap<u32, RelId>,
    /// The keyed member it links to, at a host.
    pub keyed: Option<MemberRef>,
}

/// Checks a member's `HELLO` and finds (or mints) its identity; the refusal to send it otherwise.
pub(crate) fn admit(ctx: &WebCtx, h: blossom_wire::frame::Hello) -> Result<Admitted, AdmitError> {
    admit_with(
        &ctx.id,
        &ctx.catalog,
        &ctx.client_roles,
        &*ctx.keyed,
        h,
        &mut |role, token| identify(ctx, role, token),
    )
}

/// Identifies a member of a client role by its token (or none): its node id and its token.
pub(crate) type Identify<'a> = dyn FnMut(&str, Option<Vec<u8>>) -> Result<(NodeId, Vec<u8>), RuntimeError> + 'a;

/// [`admit`], with what it needs given: the node's identity, its channel catalog and client roles, and how a member
/// is identified (`identify(role, token)`).
pub(crate) fn admit_with(
    id: &Identity,
    catalog: &Catalog,
    client_roles: &BTreeMap<String, ClientRole>,
    keyed: &KeyedTarget,
    h: blossom_wire::frame::Hello,
    identify: &mut Identify<'_>,
) -> Result<Admitted, AdmitError> {
    if let Err((reason, detail)) = crate::net::check_hello(&h, id) {
        return Err(AdmitError::Refused(reason, detail));
    }
    let Peer::Member {
        role,
        part,
        token,
        received,
        acked,
        keyed: target,
    } = h.peer
    else {
        return Err(AdmitError::Refused(
            RejectReason::NotAllowed,
            "only client members connect here".into(),
        ));
    };
    let Some(client) = client_roles.get(&role) else {
        return Err(AdmitError::Refused(
            RejectReason::NotAllowed,
            format!("`{role}` is not a client role of the program"),
        ));
    };
    if part != client.part {
        return Err(AdmitError::Refused(
            RejectReason::Program,
            format!("the page runs another version of `{role}`'s part of the program: load it again"),
        ));
    }
    let keyed = keyed(target.as_ref()).map_err(|(reason, detail)| AdmitError::Refused(reason, detail))?;
    let inbound = catalog.accept(&h.channels);
    let (member, token) = identify(&role, token).map_err(AdmitError::Failed)?;
    Ok(Admitted {
        member,
        role: client.id,
        token,
        received,
        acked,
        inbound,
        keyed,
    })
}

/// Why a member was not admitted: a refusal it is told of, or this node's failure.
pub(crate) enum AdmitError {
    Refused(RejectReason, String),
    Failed(RuntimeError),
}

/// The node's frames that answer an admitted member's `HELLO`: its own `HELLO` and `HELLO_OK`.
pub(crate) fn hello_answer(ctx: &WebCtx, a: &Admitted) -> [Frame; 2] {
    [
        crate::net::hello(&ctx.id, Peer::Node(ctx.me.0), ctx.restarts, ctx.nonce, &ctx.catalog),
        Frame::HelloOk {
            accepted_version: ctx.id.program_version,
            sids: a.inbound.keys().copied().collect(),
        },
    ]
}

/// A member's batch or acknowledgement, as the engine takes it; `None` for a frame a member does not send on its link.
pub(crate) fn member_event(
    codec: &Codec<'_>,
    p: &Program,
    a: (NodeId, u64),
    inbound: &BTreeMap<u32, RelId>,
    frame: Frame,
) -> Result<Option<MemberEvent>, RuntimeError> {
    let (member, conn) = a;
    Ok(Some(match frame {
        Frame::Msg { seq, batch } => match inbound.get(&batch.sid).copied() {
            Some(rel) => MemberEvent::Msg {
                member,
                conn,
                seq,
                rel,
                rows: decode(codec, p, rel, &batch)?,
            },
            // A channel whose schema differs between the ends: its batch is dropped, and taken (acknowledged).
            None => MemberEvent::Skip { member, conn, seq },
        },
        Frame::Ack { seq } => MemberEvent::Ack { member, conn, seq },
        _ => return Ok(None),
    }))
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
    let admitted = match admit(ctx, h) {
        Ok(a) => a,
        Err(AdmitError::Refused(reason, detail)) => return refuse(reason, detail),
        Err(AdmitError::Failed(e)) => return Err(e),
    };
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
    let [hello, ok] = hello_answer(ctx, &admitted);
    let Admitted {
        member,
        role,
        token,
        received,
        acked,
        inbound,
        keyed,
    } = admitted;
    let opened = tx.send(hello.encode()).is_ok()
        && tx.send(ok.encode()).is_ok()
        && (ctx.post)(MemberEvent::Open {
            member,
            role,
            token,
            received,
            acked,
            keyed,
            conn,
            link: Box::new(ThreadConn {
                writer: tx.clone(),
                closer: Closer::Socket(socket),
            }),
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
        let shown = format!("{frame:?}");
        let Some(event) = member_event(&codec, p, (member, conn), inbound, frame)? else {
            return Err(RuntimeError::Net(format!("a member sent {shown} on its link")));
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
    identify_in(&mut reg, ctx.me, role, token, &mut urandom)
}

/// [`identify`] in a registry, a new token's secret drawn from `random`.
pub(crate) fn identify_in(
    reg: &mut ClientRegistry,
    me: NodeId,
    role: &str,
    token: Option<Vec<u8>>,
    random: &mut dyn FnMut(&mut [u8]) -> Result<(), RuntimeError>,
) -> Result<(NodeId, Vec<u8>), RuntimeError> {
    if let Some(t) = token
        && let (Some(serial), Some(secret)) = (t.get(..4), t.get(4..))
        && let (Ok(serial), Ok(secret)) = (<[u8; 4]>::try_from(serial), <[u8; SECRET_LEN]>::try_from(secret))
    {
        let serial = u32::from_le_bytes(serial);
        if reg.check(serial, &secret) == Some(role)
            && let Some(id) = NodeId::client(me, serial)
        {
            return Ok((id, t));
        }
    }
    let mut secret = [0u8; SECRET_LEN];
    random(&mut secret)?;
    let serial = reg
        .admit(role, &secret, NodeId::CLIENT_SERIALS)?
        .ok_or_else(|| RuntimeError::Net("this node admitted as many client members as it can".into()))?;
    let id = NodeId::client(me, serial)
        .ok_or_else(|| RuntimeError::Config(format!("node {} has too high an id to admit client members", me.0)))?;
    let mut token = serial.to_le_bytes().to_vec();
    token.extend_from_slice(&secret);
    Ok((id, token))
}

/// The connection carrying a member's link.
struct Conn {
    id: u64,
    link: Box<dyn LinkConn>,
}

impl Conn {
    /// Writes a frame; `false` when the connection cannot take it (gone, or not keeping up).
    fn write(&self, frame: Vec<u8>) -> bool {
        self.link.write(frame)
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
    fn offer(&mut self, from: NodeId, role: RoleId, rel: RelId, row: Row) -> Result<Option<u64>, RuntimeError>;
    /// Offers a link event, an input of the node's next tick.
    fn event(&mut self, rel: RelId, row: Row);
    fn me(&self) -> NodeId;
    /// This node as the rows addressed to it name it: its id, or, for a keyed member, its member value.
    fn me_value(&self) -> Value {
        Value::Node(self.me())
    }
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
    c.link.close();
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

    pub(crate) fn handle(&mut self, e: MemberEvent, host: &mut dyn Host) -> Result<(), RuntimeError> {
        match e {
            MemberEvent::Open {
                member,
                role,
                token,
                received,
                acked,
                conn,
                link,
                ..
            } => self.open(member, role, token, (received, acked), Conn { id: conn, link }, host),
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
                )?;
            }
            MemberEvent::Msg {
                member,
                conn,
                seq,
                rel,
                rows,
            } => {
                let me = host.me_value();
                let Some(m) = self.members.get_mut(&member) else {
                    return Ok(());
                };
                if m.conn.as_ref().is_none_or(|c| c.id != conn) {
                    // A message on a connection that was replaced: the member resends it on the new one.
                    return Ok(());
                }
                if seq <= m.in_floor {
                    m.pending.push_back((None, seq));
                } else {
                    m.in_floor = seq;
                    let mut last = None;
                    for row in rows {
                        if row.first() != Some(&me) {
                            host.dropped_unroutable(1);
                            continue;
                        }
                        if let Some(n) = host.offer(member, m.role, rel, row)? {
                            last = Some(n);
                        }
                    }
                    m.pending.push_back((last, seq));
                }
                self.acknowledge(host);
            }
            MemberEvent::Ack { member, conn, seq } => {
                let Some(m) = self.members.get_mut(&member) else {
                    return Ok(());
                };
                if m.conn.as_ref().is_some_and(|c| c.id == conn) {
                    while m.replay.front().is_some_and(|(s, _)| *s <= seq) {
                        if let Some((_, f)) = m.replay.pop_front() {
                            m.replay_bytes = m.replay_bytes.saturating_sub(f.len());
                        }
                    }
                }
            }
            MemberEvent::Closed { member, conn } => {
                let Some(m) = self.members.get_mut(&member) else {
                    return Ok(());
                };
                if m.conn.as_ref().is_some_and(|c| c.id == conn) {
                    drop_link(member, m, &self.links, host);
                }
            }
        }
        Ok(())
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
            conn.link.close();
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
            conn.link.close();
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
