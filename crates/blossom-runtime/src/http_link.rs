//! A client member's link over plain HTTP requests (docs/design/CLIENTS.md §3a), for a page that does not hold a
//! WebSocket open. The link is the same as over a WebSocket (the same frames, numbered and acknowledged, resumed from
//! the replay buffer); requests carry its frames, and a **session** stands for the connection a WebSocket would be.
//!
//! - `POST /blossom/http/open`: the page's `HELLO`. Answered with the node's `HELLO` and `HELLO_OK` (or a `REJECT`)
//!   and, when the member is admitted, the session's id in `Blossom-Session`.
//! - `GET /blossom/http/SESSION/recv`: the node's frames for the page, in order (the `WELCOME`, then `MSG`s and
//!   `ACK`s): answered as soon as there are some, or empty after [`POLL_WAIT`]. A page keeps one outstanding, and
//!   only receives answer with frames, so they arrive in order.
//! - `POST /blossom/http/SESSION/send`: the page's frames (`MSG`, `ACK`). Answered `204`.
//! - `POST /blossom/http/SESSION/close`: the page is going (sent as a beacon when it is hidden for good).
//!
//! A body is frames, each a 4-byte big-endian length and the frame's bytes.
//!
//! A session ends, and the program hears the member's link go down, when the page closes it; when the connection of
//! its outstanding receive closes (a closed tab); when no request reached it for [`LEASE`]; when the page does not take
//! what the node sends ([`OUT_FRAMES`], [`OUT_BYTES`]) or a receive's answer cannot be written; and when the member opens
//! another (the engine closes the one it replaces). A request on an ended or unknown session is answered `410 Gone`:
//! the page opens a new session, which resumes the link as a WebSocket's reconnect does. A frame lost with a failed
//! answer is in the replay buffer: the page, which saw the failure, resumes after what it took.

use std::collections::{BTreeMap, VecDeque};
use std::io::BufReader;
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use blossom_base::{RelId, internal_error};
use blossom_value::time::NodeId;
use blossom_wire::codec::WireLimits;
use blossom_wire::frame::Frame;

use crate::RuntimeError;
use crate::clock::Stopwatch;
use crate::members::{AdmitError, Closer, MemberEvent, WebCtx};
use crate::web;

/// How long a receive waits for frames before it is answered empty.
pub const POLL_WAIT: Duration = Duration::from_secs(25);
/// A session no request has reached for this long ends (a page polls well within it).
pub const LEASE: Duration = Duration::from_secs(30);
/// How often a waiting receive checks that its connection is still open.
const CHECK: Duration = Duration::from_millis(500);
/// The most frames, and bytes, a session holds for the page: past them the page is not keeping up, and the session
/// ends (the member resumes from the replay buffer).
pub const OUT_FRAMES: usize = 4096;
pub const OUT_BYTES: usize = 32 * 1024 * 1024;
/// The most bytes of frames one receive answers with (it carries at least one frame).
const ANSWER_BYTES: usize = 4 * 1024 * 1024;
/// The most sessions a node holds: past it, an open is answered `503`.
const MAX_SESSIONS: usize = 1 << 16;
/// The engine's frames queued for a session's pump.
const PUMP_QUEUE: usize = 4096;

/// The frames of a body: each a 4-byte big-endian length and the frame's bytes.
pub fn encode_frames<'a>(frames: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    for f in frames {
        out.extend_from_slice(&(f.len() as u32).to_be_bytes());
        out.extend_from_slice(f);
    }
    out
}

/// A body's frames, each exactly one link frame.
pub fn decode_frames(mut body: &[u8]) -> Result<Vec<Frame>, RuntimeError> {
    let limits = WireLimits::default();
    let mut out = Vec::new();
    while !body.is_empty() {
        let (len, rest) = body
            .split_first_chunk::<4>()
            .ok_or_else(|| RuntimeError::Net("a truncated frame length in a link request".into()))?;
        let len = u32::from_be_bytes(*len) as usize;
        let (bytes, rest) = rest
            .split_at_checked(len)
            .ok_or_else(|| RuntimeError::Net("a truncated frame in a link request".into()))?;
        match Frame::parse(bytes, &limits)? {
            Some((f, used)) if used == bytes.len() => out.push(f),
            _ => {
                return Err(RuntimeError::Net(
                    "a link request's part that is not exactly one frame".into(),
                ));
            }
        }
        body = rest;
    }
    Ok(out)
}

/// A member's link over requests: the connection a WebSocket would be.
pub struct Session {
    member: NodeId,
    conn: u64,
    /// The member's channels this node takes, by the member's sid.
    inbound: BTreeMap<u32, RelId>,
    state: Mutex<State>,
    ready: Condvar,
    post: Arc<dyn Fn(MemberEvent) -> bool + Send + Sync>,
}

struct State {
    /// The node's frames the page has not received yet.
    out: VecDeque<Vec<u8>>,
    out_bytes: usize,
    ended: bool,
    /// A receive is waiting.
    receiving: bool,
    /// Since the last request on the session ended (or the session began).
    idle: Stopwatch,
}

impl Session {
    fn lock(&self) -> Result<MutexGuard<'_, State>, RuntimeError> {
        self.state
            .lock()
            .map_err(|_| internal_error!("an HTTP link session's lock is poisoned").into())
    }

    /// Ends the session; `tell` posts the link's end to the engine (not when the engine itself ends it: it knows, and
    /// its thread must not wait on its own queue).
    pub fn end(&self, tell: bool) {
        let Ok(mut s) = self.state.lock() else { return };
        if s.ended {
            return;
        }
        s.ended = true;
        s.out.clear();
        s.out_bytes = 0;
        drop(s);
        self.ready.notify_all();
        if tell {
            (self.post)(MemberEvent::Closed {
                member: self.member,
                conn: self.conn,
            });
        }
    }

    /// Queues one of the node's frames for the page; false when the session has ended or the page is not keeping up.
    fn push(&self, frame: Vec<u8>) -> bool {
        let Ok(mut s) = self.state.lock() else { return false };
        if s.ended || s.out.len() >= OUT_FRAMES || s.out_bytes + frame.len() > OUT_BYTES {
            return false;
        }
        s.out_bytes += frame.len();
        s.out.push_back(frame);
        drop(s);
        self.ready.notify_all();
        true
    }

    /// Whether no request has reached the session for [`LEASE`] (a waiting receive keeps it).
    fn expired(&self) -> bool {
        self.state
            .lock()
            .is_ok_and(|s| !s.ended && !s.receiving && s.idle.nanos() > LEASE.as_nanos() as u64)
    }

    fn ended(&self) -> bool {
        self.state.lock().map_or(true, |s| s.ended)
    }

    /// A request reached the session: its lease starts over. False when the session has ended.
    fn touch(&self) -> Result<bool, RuntimeError> {
        let mut s = self.lock()?;
        s.idle = Stopwatch::start();
        Ok(!s.ended)
    }

    /// The node's next frames for the page (as a body), waiting up to [`POLL_WAIT`] for some; `None` when the session
    /// ended, or `gone` says the page's connection closed while it waited (the session then ends).
    fn receive(&self, gone: &dyn Fn() -> bool) -> Result<Option<Vec<u8>>, RuntimeError> {
        let mut s = self.lock()?;
        if s.ended {
            return Ok(None);
        }
        if s.receiving {
            // A page keeps one receive outstanding: a second one is not that page's (or it lost track): the session
            // ends, and the page opens another.
            drop(s);
            self.end(true);
            return Ok(None);
        }
        s.receiving = true;
        let mut waited = Duration::ZERO;
        while s.out.is_empty() && !s.ended && waited < POLL_WAIT {
            s = self
                .ready
                .wait_timeout(s, CHECK)
                .map_err(|_| internal_error!("an HTTP link session's lock is poisoned"))?
                .0;
            waited += CHECK;
            if s.out.is_empty() && !s.ended {
                drop(s);
                if gone() {
                    if let Ok(mut s) = self.state.lock() {
                        s.receiving = false;
                    }
                    self.end(true);
                    return Ok(None);
                }
                s = self.lock()?;
            }
        }
        s.receiving = false;
        s.idle = Stopwatch::start();
        if s.ended {
            return Ok(None);
        }
        let mut frames = Vec::new();
        let mut bytes = 0;
        while let Some(f) = s.out.front() {
            if !frames.is_empty() && bytes + f.len() > ANSWER_BYTES {
                break;
            }
            bytes += f.len();
            if let Some(f) = s.out.pop_front() {
                frames.push(f);
            }
        }
        s.out_bytes = s.out_bytes.saturating_sub(bytes);
        Ok(Some(encode_frames(frames.iter().map(Vec::as_slice))))
    }
}

/// The node's sessions, by id.
#[derive(Default)]
pub struct Sessions {
    by_id: Mutex<BTreeMap<String, Arc<Session>>>,
}

impl Sessions {
    fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.by_id.lock().ok()?.get(id).cloned()
    }

    /// Ends the sessions whose lease ran out, and forgets the ended ones.
    pub(crate) fn reap(&self) {
        let Ok(mut by_id) = self.by_id.lock() else { return };
        let expired: Vec<Arc<Session>> = by_id.values().filter(|s| s.expired()).cloned().collect();
        by_id.retain(|_, s| !s.ended());
        drop(by_id);
        for s in expired {
            s.end(true);
        }
    }
}

/// Ends expired sessions about once a second until `stop`.
pub(crate) fn reap_loop(sessions: Arc<Sessions>, stop: Arc<std::sync::atomic::AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        sessions.reap();
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Serves a request under `/blossom/http/` (`rest` is the path after it).
pub(crate) fn serve(
    rest: &str,
    req: &web::Request,
    r: &mut BufReader<TcpStream>,
    w: &mut TcpStream,
    ctx: &WebCtx,
    keep: bool,
) -> Result<(), RuntimeError> {
    let resp = web::Response::new;
    let body = web::read_body(r, req, web::MAX_MESSAGE)?;
    let (id, verb) = match rest.split_once('/') {
        Some((id, verb)) => (id, verb),
        None => ("", rest),
    };
    match (req.method.as_str(), id, verb) {
        ("POST", "", "open") => open(&body, w, ctx, keep),
        (method, id, verb) if !id.is_empty() => {
            let Some(session) = ctx.sessions.get(id) else {
                return resp(410, "Gone", "text/plain", b"no such session: open another").write(w, keep);
            };
            match (method, verb) {
                ("GET", "recv") => receive(&session, r, w, keep),
                ("POST", "send") => send(&session, &body, w, ctx, keep),
                ("POST", "close") => {
                    session.end(true);
                    resp(204, "No Content", "text/plain", b"").write(w, keep)
                }
                _ => resp(404, "Not Found", "text/plain", b"no such request on a session").write(w, keep),
            }
        }
        _ => resp(404, "Not Found", "text/plain", b"no such request").write(w, keep),
    }
}

/// `POST /blossom/http/open`: admits the member, and starts its session.
fn open(body: &[u8], w: &mut TcpStream, ctx: &WebCtx, keep: bool) -> Result<(), RuntimeError> {
    const FRAMES: &str = "application/octet-stream";
    let mut frames = decode_frames(body)?;
    let (Some(Frame::Hello(h)), true) = (frames.pop(), frames.is_empty()) else {
        web::Response::new(400, "Bad Request", "text/plain", b"an open carries one HELLO").write(w, false)?;
        return Err(RuntimeError::Net("an HTTP link opened without one HELLO".into()));
    };
    let admitted = match crate::members::admit(ctx, h) {
        Ok(a) => a,
        Err(AdmitError::Refused(reason, detail)) => {
            let reject = Frame::Reject { reason, detail }.encode();
            return web::Response::new(200, "OK", FRAMES, &encode_frames([reject.as_slice()])).write(w, keep);
        }
        Err(AdmitError::Failed(e)) => return Err(e),
    };
    if ctx.sessions.by_id.lock().map_or(true, |m| m.len() >= MAX_SESSIONS) {
        return web::Response::new(503, "Service Unavailable", "text/plain", b"too many sessions").write(w, keep);
    }
    let answer = crate::members::hello_answer(ctx, &admitted);
    let mut raw = [0u8; 16];
    crate::members::urandom(&mut raw)?;
    let id: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let conn = ctx.next_conn.fetch_add(1, Ordering::SeqCst);
    let session = Arc::new(Session {
        member: admitted.member,
        conn,
        inbound: admitted.inbound,
        state: Mutex::new(State {
            out: VecDeque::new(),
            out_bytes: 0,
            ended: false,
            receiving: false,
            idle: Stopwatch::start(),
        }),
        ready: Condvar::new(),
        post: ctx.post.clone(),
    });
    // The engine writes the link's frames to `tx`; the pump queues them for the page's receives, and ends the session
    // when the engine lets go of the link or the page does not keep up.
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(PUMP_QUEUE);
    {
        let session = session.clone();
        std::thread::Builder::new()
            .name("member-session".into())
            .spawn(move || {
                for frame in rx {
                    if !session.push(frame) {
                        break;
                    }
                }
                session.end(true);
            })
            .map_err(RuntimeError::Io)?;
    }
    ctx.sessions
        .by_id
        .lock()
        .map_err(|_| internal_error!("the HTTP link sessions' lock is poisoned"))?
        .insert(id.clone(), session.clone());
    let opened = (ctx.post)(MemberEvent::Open {
        member: admitted.member,
        role: admitted.role,
        token: admitted.token,
        received: admitted.received,
        acked: admitted.acked,
        conn,
        link: Box::new(crate::members::ThreadConn {
            writer: tx,
            closer: Closer::Session(session.clone()),
        }),
    });
    if !opened {
        session.end(false);
        return web::Response::new(503, "Service Unavailable", "text/plain", b"the node is stopping").write(w, keep);
    }
    let encoded: Vec<Vec<u8>> = answer.iter().map(Frame::encode).collect();
    web::Response {
        headers: &[("Blossom-Session", &id)],
        ..web::Response::new(200, "OK", FRAMES, &encode_frames(encoded.iter().map(Vec::as_slice)))
    }
    .write(w, keep)
}

/// `GET /blossom/http/SESSION/recv`: the node's next frames for the page.
fn receive(session: &Session, r: &mut BufReader<TcpStream>, w: &mut TcpStream, keep: bool) -> Result<(), RuntimeError> {
    let socket = r.get_ref();
    let gone = || connection_closed(socket);
    match session.receive(&gone)? {
        Some(body) => {
            let written = web::Response::new(200, "OK", "application/octet-stream", &body).write(w, keep);
            if written.is_err() {
                // The page did not get these frames: the session ends, and the page resumes after what it took.
                session.end(true);
            }
            written
        }
        None => web::Response::new(410, "Gone", "text/plain", b"the session ended: open another").write(w, keep),
    }
}

/// `POST /blossom/http/SESSION/send`: the page's frames, to the engine.
fn send(session: &Session, body: &[u8], w: &mut TcpStream, ctx: &WebCtx, keep: bool) -> Result<(), RuntimeError> {
    if !session.touch()? {
        return web::Response::new(410, "Gone", "text/plain", b"the session ended: open another").write(w, keep);
    }
    let program = ctx.artifact.program.clone();
    let p = program.get();
    let codec = crate::net::wire_codec(p);
    let refuse = |w: &mut TcpStream, why: String| -> Result<(), RuntimeError> {
        session.end(true);
        web::Response::new(400, "Bad Request", "text/plain", why.as_bytes()).write(w, false)?;
        Err(RuntimeError::Net(why))
    };
    let frames = match decode_frames(body) {
        Ok(f) => f,
        Err(e) => return refuse(w, e.to_string()),
    };
    for frame in frames {
        let shown = format!("{frame:?}");
        let event =
            match crate::members::member_event(&codec, p, (session.member, session.conn), &session.inbound, frame) {
                Ok(Some(e)) => e,
                Ok(None) => return refuse(w, format!("a member sent {shown} on its link")),
                Err(e) => return refuse(w, e.to_string()),
            };
        if !(session.post)(event) {
            return web::Response::new(503, "Service Unavailable", "text/plain", b"the node is stopping")
                .write(w, keep);
        }
    }
    web::Response::new(204, "No Content", "text/plain", b"").write(w, keep)
}

/// Whether the client closed `socket` (a waiting receive's page went away): its end read, without taking a byte.
fn connection_closed(socket: &TcpStream) -> bool {
    let before = socket.read_timeout().ok().flatten();
    if socket.set_read_timeout(Some(Duration::from_millis(1))).is_err() {
        return true;
    }
    let mut byte = [0u8; 1];
    let closed = match socket.peek(&mut byte) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => !matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted
        ),
    };
    // The timeout of the requests that follow on the connection.
    let _ = socket.set_read_timeout(before);
    closed
}
