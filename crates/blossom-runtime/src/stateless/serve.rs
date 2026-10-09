//! `blossom serve` (docs/design/STATELESS.md §11): a stateless host's HTTP front. It serves the page, `app.json`,
//! the client parts and the stylesheet; mints pages' tokens; runs pages' links over plain requests (CLIENTS.md §3a,
//! with the cursor of STATELESS.md §6.3) and over WebSockets (§6.5); answers `POST /blossom/wake` with a sweep; and
//! sweeps on its own every [`ServeConfig::sweep`]. It keeps nothing a request needs from an earlier one: every
//! request goes to [`Objects`].
//!
//! The HTTP server is the hand-written one of `blossom run` (crate::web): HTTP/1.1 with keep-alive, a thread per
//! connection.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use super::objects::{Objects, POLL_WAIT, ServeError, parse_session, session_id};
use crate::RuntimeError;
use crate::object::Cursor;
use crate::web::{self, Message, Request, Response};

/// How long a kept connection may sit between requests.
const IDLE: Duration = Duration::from_secs(75);
/// How long a WebSocket's pusher waits for the object before it records the session's presence again.
const SOCKET_PRESENCE: Duration = Duration::from_secs(10);
/// The most frames, and bytes, a request's body carries.
const MAX_BODY: usize = 32 * 1024 * 1024;
/// How many objects one sweep wakes.
const SWEEP_LIMIT: usize = 256;

/// What a host reports that no request is there to hear: a sweep's failure, a connection's.
pub type Report = Arc<dyn Fn(&str) + Send + Sync>;

/// Where and how a host serves.
pub struct ServeConfig {
    pub web: SocketAddr,
    /// The page's files (the built browser host).
    pub web_root: Option<PathBuf>,
    /// The deployment's stylesheet (`[web] style`).
    pub style: Option<PathBuf>,
    /// How often the host sweeps on its own; `None`: only when `POST /blossom/wake` asks.
    pub sweep: Option<Duration>,
    pub report: Report,
}

/// Counters of what a host did.
#[derive(Default, Debug)]
pub struct ServeStats {
    pub requests: AtomicU64,
    pub failed: AtomicU64,
    pub swept: AtomicU64,
}

/// A host serving: its address, and what stops it.
pub struct Serving {
    pub addr: SocketAddr,
    pub stats: Arc<ServeStats>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Serving {
    /// Stops accepting and sweeping (connections in flight finish on their own).
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wakes the accept loop with a connection of its own.
        let _ = TcpStream::connect(self.addr);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }

    /// Serves until the process ends.
    pub fn wait(mut self) {
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

struct Ctx {
    objects: Arc<Objects>,
    root: Option<PathBuf>,
    style: Option<PathBuf>,
    report: Report,
    stats: Arc<ServeStats>,
}

/// Starts serving `objects` as `cfg` says.
pub fn serve(objects: Arc<Objects>, cfg: ServeConfig) -> Result<Serving, RuntimeError> {
    let listener = TcpListener::bind(cfg.web).map_err(|e| RuntimeError::Net(format!("binding {}: {e}", cfg.web)))?;
    let addr = listener.local_addr().map_err(RuntimeError::Io)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(ServeStats::default());
    let ctx = Arc::new(Ctx {
        objects: objects.clone(),
        root: cfg.web_root,
        style: cfg.style,
        report: cfg.report.clone(),
        stats: stats.clone(),
    });
    let mut threads = Vec::new();
    {
        let (stop, ctx) = (stop.clone(), ctx.clone());
        threads.push(
            std::thread::Builder::new()
                .name("serve-accept".into())
                .spawn(move || accept_loop(&listener, &stop, &ctx))
                .map_err(RuntimeError::Io)?,
        );
    }
    if let Some(every) = cfg.sweep {
        let (stop, ctx) = (stop.clone(), ctx.clone());
        threads.push(
            std::thread::Builder::new()
                .name("serve-sweep".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        match ctx.objects.sweep(SWEEP_LIMIT) {
                            Ok(woken) => {
                                ctx.stats.swept.fetch_add(woken.len() as u64, Ordering::Relaxed);
                            }
                            Err(e) => (ctx.report)(&format!("sweep: {e}")),
                        }
                        std::thread::sleep(every);
                    }
                })
                .map_err(RuntimeError::Io)?,
        );
    }
    Ok(Serving {
        addr,
        stats,
        stop,
        threads,
    })
}

fn accept_loop(listener: &TcpListener, stop: &AtomicBool, ctx: &Arc<Ctx>) {
    for stream in listener.incoming() {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(stream) = stream else { continue };
        let conn_ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("serve-conn".into()).spawn(move || {
            if let Err(e) = connection(stream, &conn_ctx)
                && !quiet(&e)
            {
                conn_ctx.stats.failed.fetch_add(1, Ordering::Relaxed);
                (conn_ctx.report)(&format!("connection: {e}"));
            }
        });
        if spawned.is_err() {
            (ctx.report)("could not start a connection's thread");
        }
    }
}

/// A connection's ordinary end: the client closed it.
fn quiet(e: &RuntimeError) -> bool {
    match e {
        RuntimeError::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::WouldBlock
                | std::io::ErrorKind::TimedOut
        ),
        RuntimeError::Net(m) => m.contains("closed inside a request"),
        _ => false,
    }
}

fn connection(stream: TcpStream, ctx: &Ctx) -> Result<(), RuntimeError> {
    stream.set_read_timeout(Some(IDLE)).map_err(RuntimeError::Io)?;
    let _ = stream.set_nodelay(true);
    let mut reader = BufReader::new(stream.try_clone().map_err(RuntimeError::Io)?);
    let mut w = stream;
    let mut first = true;
    loop {
        if !first && reader.fill_buf().map_or(true, |b| b.is_empty()) {
            return Ok(());
        }
        first = false;
        let req = web::read_request(&mut reader)?;
        ctx.stats.requests.fetch_add(1, Ordering::Relaxed);
        if req.path == "/blossom/link" {
            return socket(&req, reader, w, ctx);
        }
        let keep = req.keep_alive();
        let body = web::read_body(&mut reader, &req, MAX_BODY)?;
        request(&req, &body, &mut w, ctx, keep)?;
        if !keep {
            return Ok(());
        }
    }
}

fn status_of(e: &ServeError) -> (u16, &'static str) {
    match e {
        ServeError::Gone(_) => (410, "Gone"),
        ServeError::BadRequest(_) => (400, "Bad Request"),
        ServeError::Unavailable(_) => (503, "Service Unavailable"),
        ServeError::Runtime(_) => (500, "Internal Server Error"),
    }
}

/// Answers a request that failed: its status, and the reason as text. A failure of the host itself is reported.
fn failed(e: &ServeError, w: &mut TcpStream, ctx: &Ctx, keep: bool) -> Result<(), RuntimeError> {
    let (status, reason) = status_of(e);
    if status == 500 {
        ctx.stats.failed.fetch_add(1, Ordering::Relaxed);
        (ctx.report)(&format!("a request failed: {e}"));
    }
    Response::new(status, reason, "text/plain; charset=utf-8", e.to_string().as_bytes()).write(w, keep)
}

/// Splits a body of frames, each a 4-byte big-endian length and its bytes (CLIENTS.md §3a).
fn split_frames(mut body: &[u8]) -> Result<Vec<Vec<u8>>, ServeError> {
    let mut out = Vec::new();
    while !body.is_empty() {
        let (len, rest) = body
            .split_at_checked(4)
            .ok_or_else(|| ServeError::BadRequest("a frame length cut short".into()))?;
        let len = u32::from_be_bytes(
            len.try_into()
                .map_err(|_| ServeError::BadRequest("a frame length cut short".into()))?,
        ) as usize;
        let (frame, rest) = rest
            .split_at_checked(len)
            .ok_or_else(|| ServeError::BadRequest("a frame cut short".into()))?;
        out.push(frame.to_vec());
        body = rest;
    }
    Ok(out)
}

fn request(req: &Request, body: &[u8], w: &mut TcpStream, ctx: &Ctx, keep: bool) -> Result<(), RuntimeError> {
    let r = Response::new;
    let path = req.path.as_str();
    if let Some(rest) = path.strip_prefix("/blossom/http/") {
        return match http_link(rest, req, body, w, ctx, keep) {
            Ok(()) => Ok(()),
            Err(e) => failed(&e, w, ctx, keep),
        };
    }
    match (req.method.as_str(), path) {
        ("GET", "/blossom/app.json") => {
            r(200, "OK", "application/json", ctx.objects.deploy.app_json().as_bytes()).write(w, keep)
        }
        ("GET", web::STYLE_PATH) if ctx.style.is_some() => {
            let file = ctx
                .style
                .as_ref()
                .ok_or_else(|| RuntimeError::Config("no stylesheet".into()))?;
            let css = std::fs::read(file).map_err(RuntimeError::Io)?;
            r(200, "OK", "text/css; charset=utf-8", &css).write(w, keep)
        }
        ("GET", p) if p.starts_with("/blossom/client/") => {
            match p
                .strip_prefix("/blossom/client/")
                .and_then(|role| ctx.objects.deploy.client_part(role))
            {
                Some(part) => r(200, "OK", "application/octet-stream", part).write(w, keep),
                None => r(404, "Not Found", "text/plain", b"no such client role").write(w, keep),
            }
        }
        ("POST", "/blossom/token") => {
            let Some(role) = req.param("role") else {
                return r(
                    400,
                    "Bad Request",
                    "text/plain",
                    b"a token is for a client role (?role=)",
                )
                .write(w, keep);
            };
            match ctx.objects.mint(&role) {
                Ok(token) => r(200, "OK", "application/octet-stream", &token).write(w, keep),
                Err(e) => failed(&e, w, ctx, keep),
            }
        }
        ("POST", "/blossom/wake") => match ctx.objects.sweep(SWEEP_LIMIT) {
            Ok(woken) => {
                ctx.stats.swept.fetch_add(woken.len() as u64, Ordering::Relaxed);
                let json = serde_json::to_vec(&woken).map_err(|e| RuntimeError::Config(e.to_string()))?;
                r(200, "OK", "application/json", &json).write(w, keep)
            }
            Err(e) => failed(&e, w, ctx, keep),
        },
        ("GET", p) => match ctx.root.as_ref().and_then(|root| web::file_of(root, p)) {
            Some(file) => {
                let bytes = std::fs::read(&file).map_err(RuntimeError::Io)?;
                r(200, "OK", web::content_type(&file), &bytes).write(w, keep)
            }
            None => r(404, "Not Found", "text/plain", b"not found").write(w, keep),
        },
        _ => r(405, "Method Not Allowed", "text/plain", b"not here").write(w, keep),
    }
}

/// The link over plain requests: `open`, then `SESSION/recv`, `SESSION/send` and `SESSION/close`.
fn http_link(
    rest: &str,
    req: &Request,
    body: &[u8],
    w: &mut TcpStream,
    ctx: &Ctx,
    keep: bool,
) -> Result<(), ServeError> {
    let objects = &ctx.objects;
    if rest == "open" {
        if req.method != "POST" {
            return Err(ServeError::BadRequest("open is a POST".into()));
        }
        let object = objects
            .deploy
            .page_object(req.param("member").as_deref())
            .map_err(ServeError::BadRequest)?;
        let frames = split_frames(body)?;
        let [hello] = frames.as_slice() else {
            return Err(ServeError::BadRequest("open carries the page's HELLO, alone".into()));
        };
        let opened = objects.open(&object, hello)?;
        let answer = crate::http_link::encode_frames(opened.frames.iter().map(Vec::as_slice));
        let session = opened
            .session
            .map(|(conn, secret, cursor)| (session_id(&object, conn, &secret), cursor.text()));
        let headers: Vec<(&str, &str)> = match &session {
            Some((id, cursor)) => vec![("Blossom-Session", id.as_str()), ("Blossom-Cursor", cursor.as_str())],
            None => Vec::new(),
        };
        return Response {
            status: 200,
            reason: "OK",
            content_type: "application/octet-stream",
            headers: &headers,
            body: &answer,
        }
        .write(w, keep)
        .map_err(ServeError::Runtime);
    }
    let (id, verb) = rest
        .rsplit_once('/')
        .ok_or_else(|| ServeError::BadRequest(format!("no link request `{rest}`")))?;
    let (object, conn, secret) = parse_session(id).ok_or_else(|| ServeError::Gone("no such session".into()))?;
    match (req.method.as_str(), verb) {
        ("GET", "recv") => {
            let cursor = req
                .param("cursor")
                .and_then(|c| Cursor::parse(&c))
                .ok_or_else(|| ServeError::BadRequest("a receive names its cursor (?cursor=)".into()))?;
            objects.touch(&object, conn)?;
            let (frames, next) = objects.receive(&object, conn, &secret, cursor, POLL_WAIT)?;
            let answer = crate::http_link::encode_frames(frames.iter().map(Vec::as_slice));
            let next = next.text();
            Response {
                status: 200,
                reason: "OK",
                content_type: "application/octet-stream",
                headers: &[("Blossom-Cursor", next.as_str())],
                body: &answer,
            }
            .write(w, keep)
            .map_err(ServeError::Runtime)
        }
        ("POST", "send") => {
            objects.send(&object, conn, &secret, &split_frames(body)?)?;
            Response::new(204, "No Content", "text/plain", b"")
                .write(w, keep)
                .map_err(ServeError::Runtime)
        }
        ("POST", "close") => {
            objects.close(&object, conn, &secret)?;
            Response::new(204, "No Content", "text/plain", b"")
                .write(w, keep)
                .map_err(ServeError::Runtime)
        }
        _ => Err(ServeError::BadRequest(format!("no link request `{verb}`"))),
    }
}

/// A page's link over a WebSocket (docs/design/STATELESS.md §6.5): the socket is all this host holds. Its first
/// message is the page's `HELLO`; each later one a request; a pusher writes what a receive would answer, waiting on
/// the object, and records the session's presence while the socket is open.
fn socket(req: &Request, mut reader: BufReader<TcpStream>, mut w: TcpStream, ctx: &Ctx) -> Result<(), RuntimeError> {
    let object = match ctx.objects.deploy.page_object(req.param("member").as_deref()) {
        Ok(o) => o,
        Err(m) => return web::respond(&mut w, 400, "Bad Request", "text/plain", m.as_bytes()),
    };
    web::upgrade(&mut w, req)?;
    reader.get_ref().set_read_timeout(None).map_err(RuntimeError::Io)?;
    let hello = loop {
        match web::read_message(&mut reader)? {
            Message::Binary(b) => break b,
            Message::Ping(p) => web::write_pong(&mut w, &p).map_err(RuntimeError::Io)?,
            Message::Pong => {}
            Message::Text(_) | Message::Close => return web::write_close(&mut w).map_err(RuntimeError::Io),
        }
    };
    let opened = match ctx.objects.open(&object, &hello) {
        Ok(o) => o,
        Err(_) => return web::write_close(&mut w).map_err(RuntimeError::Io),
    };
    for f in &opened.frames {
        web::write_binary(&mut w, f).map_err(RuntimeError::Io)?;
    }
    let Some((conn, secret, cursor)) = opened.session else {
        return web::write_close(&mut w).map_err(RuntimeError::Io);
    };
    let writer = Arc::new(Mutex::new(w.try_clone().map_err(RuntimeError::Io)?));
    let done = Arc::new(AtomicBool::new(false));
    let pusher = {
        let (writer, done, objects, object) = (writer.clone(), done.clone(), ctx.objects.clone(), object.clone());
        std::thread::Builder::new()
            .name("serve-push".into())
            .spawn(move || {
                let mut at = cursor;
                while !done.load(Ordering::Relaxed) {
                    if objects.touch(&object, conn).is_err() {
                        break;
                    }
                    match objects.receive(&object, conn, &secret, at, SOCKET_PRESENCE) {
                        Ok((frames, next)) => {
                            at = next;
                            let Ok(mut w) = writer.lock() else { break };
                            if frames.iter().any(|f| web::write_binary(&mut *w, f).is_err()) {
                                break;
                            }
                        }
                        // The session ended or the store failed: the page reconnects and resumes.
                        Err(_) => break,
                    }
                }
                done.store(true, Ordering::Relaxed);
                if let Ok(mut w) = writer.lock() {
                    let _ = web::write_close(&mut *w);
                    let _ = w.shutdown(std::net::Shutdown::Both);
                }
            })
            .map_err(RuntimeError::Io)?
    };
    let result = loop {
        if done.load(Ordering::Relaxed) {
            break Ok(());
        }
        match web::read_message(&mut reader) {
            Ok(Message::Binary(f)) => {
                if ctx.objects.send(&object, conn, &secret, &[f]).is_err() {
                    break Ok(());
                }
            }
            Ok(Message::Ping(p)) => {
                if let Ok(mut w) = writer.lock() {
                    web::write_pong(&mut *w, &p).map_err(RuntimeError::Io)?;
                }
            }
            Ok(Message::Pong) => {}
            Ok(Message::Text(_)) | Ok(Message::Close) => break Ok(()),
            Err(e) => break if quiet(&e) { Ok(()) } else { Err(e) },
        }
    };
    let ended = !done.swap(true, Ordering::Relaxed);
    if ended {
        // The page closed the socket: its session ends now (if this fails, its presence lapses and it ends then).
        let _ = ctx.objects.close(&object, conn, &secret);
    }
    if let Ok(mut w) = writer.lock() {
        let _ = w.flush();
        let _ = w.shutdown(std::net::Shutdown::Both);
    }
    let _ = pusher.join();
    result
}
