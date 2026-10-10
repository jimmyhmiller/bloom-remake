//! `blossom serve` (docs/design/STATELESS.md §11): a stateless host's HTTP front. It serves the page, `app.json`,
//! the client parts and the stylesheet; mints pages' tokens; runs pages' links over plain requests (CLIENTS.md §3a,
//! with the cursor of STATELESS.md §6.3) and over WebSockets (§6.5); answers `/blossom/wake` with a sweep, for a
//! caller with the wake secret; and sweeps on its own every [`ServeConfig::sweep`]. It keeps nothing a request needs
//! from an earlier one: every request goes to [`Objects`].
//!
//! On the open internet (docs/design/DEPLOY.md §2): it serves at most [`ServeConfig::max_connections`] at once,
//! gives each client at most [`ServeConfig::tokens_per_minute`] page tokens, answers `GET /blossom/health` from the
//! store, and [`Serving::drain`]s on a platform's `SIGTERM`.
//!
//! The HTTP server is the hand-written one of `blossom run` (crate::web): HTTP/1.1 with keep-alive, a thread per
//! connection.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::objects::{Objects, POLL_WAIT, ServeError, parse_session, session_id};
use crate::RuntimeError;
use crate::object::Cursor;
use crate::web::{self, Message, Request, Response};

/// How long a kept connection may sit between requests.
const IDLE: Duration = Duration::from_secs(75);
/// How often a connection waiting for its next request looks whether the host drains.
const DRAIN_CHECK: Duration = Duration::from_millis(250);
/// How long a WebSocket's pusher waits for the object before it records the session's presence again.
const SOCKET_PRESENCE: Duration = Duration::from_secs(10);
/// The most bytes a request's body carries (a page's frames; function platforms refuse more than about 4.5 MB).
const MAX_BODY: usize = 4 * 1024 * 1024;
/// The most clients whose token allowance a host remembers; past it, a new client is refused a token until some
/// client's allowance is whole again (a pool of page serials is finite: docs/design/DEPLOY.md §2).
const MAX_CLIENTS: usize = 65_536;
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
    /// How often the host sweeps on its own; `None`: only when `/blossom/wake` asks.
    pub sweep: Option<Duration>,
    pub report: Report,
    /// The secret a caller of `/blossom/wake` shows (`Authorization: Bearer SECRET`); `None`: the route refuses
    /// every caller.
    pub wake_secret: Option<String>,
    /// The most connections served at once; another is answered `503` and closed.
    pub max_connections: usize,
    /// Where a request's client is told: its peer, or a header the proxy in front sets.
    pub client: ClientAddr,
    /// How many page tokens one client is given a minute (and at once, after a quiet minute).
    pub tokens_per_minute: u32,
}

/// Where a request's client is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientAddr {
    /// The connection's peer: the host faces its clients.
    Peer,
    /// The first address in this header (lower case), which the proxy in front of the host sets; the peer when it
    /// is missing. Only behind a proxy that overwrites the header: a client could write any value itself.
    Header(String),
}

/// Counters of what a host did.
#[derive(Default, Debug)]
pub struct ServeStats {
    pub requests: AtomicU64,
    pub failed: AtomicU64,
    pub swept: AtomicU64,
    /// Connections answered `503` because [`ServeConfig::max_connections`] were open.
    pub turned_away: AtomicU64,
    /// Page tokens refused because their client had used its allowance.
    pub tokens_refused: AtomicU64,
}

/// A host serving: its address, and what stops it.
pub struct Serving {
    pub addr: SocketAddr,
    pub stats: Arc<ServeStats>,
    stop: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    threads: Vec<JoinHandle<()>>,
}

impl Serving {
    /// Stops accepting and sweeping (connections in flight finish on their own).
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wakes the accept loop with a connection of its own.
        let _ = TcpStream::connect(self.addr);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }

    /// Stops accepting and sweeping, then waits up to `grace` for the connections in flight: each ends after the
    /// request it is in (a receive waits at most [`POLL_WAIT`]); a WebSocket closes at its next presence (its page
    /// reconnects, to another instance). Whether every connection ended in time. Nothing is lost when one did not:
    /// every request's effect is committed before it is answered.
    pub fn drain(mut self, grace: Duration) -> bool {
        self.halt();
        let until = Instant::now() + grace;
        while self.active.load(Ordering::Relaxed) > 0 {
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        true
    }

    /// How many connections are open.
    pub fn connections(&self) -> usize {
        self.active.load(Ordering::Relaxed)
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
    /// The wake secret's hash: comparing hashes takes the same time however much of a guess matched, and whatever
    /// its length.
    wake_secret: Option<blake3::Hash>,
    max_connections: usize,
    client: ClientAddr,
    tokens: Allowances,
    stop: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
}

/// Each client's allowance of page tokens: a bucket of [`ServeConfig::tokens_per_minute`] that fills again at that
/// rate.
struct Allowances {
    per_minute: u32,
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
}

impl Allowances {
    /// Takes a token from `client`'s allowance at `now`; when it has none, how long until it has one.
    fn take(&self, client: &str, now: Instant) -> Result<(), Duration> {
        let full = f64::from(self.per_minute);
        let rate = full / 60.0;
        let level =
            |(left, at): (f64, Instant)| (left + now.saturating_duration_since(at).as_secs_f64() * rate).min(full);
        let refused = Duration::from_secs(60);
        if self.per_minute == 0 {
            return Err(refused);
        }
        let Ok(mut b) = self.buckets.lock() else {
            return Err(refused);
        };
        if !b.contains_key(client) && b.len() >= MAX_CLIENTS {
            // A whole allowance is as good as none remembered.
            b.retain(|_, v| level(*v) < full);
            if b.len() >= MAX_CLIENTS {
                return Err(refused);
            }
        }
        let have = b.get(client).map_or(full, |v| level(*v));
        if have >= 1.0 {
            b.insert(client.to_owned(), (have - 1.0, now));
            Ok(())
        } else {
            b.insert(client.to_owned(), (have, now));
            Err(Duration::from_secs_f64((1.0 - have) / rate))
        }
    }
}

/// Counts a connection while it is open.
struct Open(Arc<AtomicUsize>);

impl Drop for Open {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Starts serving `objects` as `cfg` says.
pub fn serve(objects: Arc<Objects>, cfg: ServeConfig) -> Result<Serving, RuntimeError> {
    let listener = TcpListener::bind(cfg.web).map_err(|e| RuntimeError::Net(format!("binding {}: {e}", cfg.web)))?;
    let addr = listener.local_addr().map_err(RuntimeError::Io)?;
    let stop = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicUsize::new(0));
    let stats = Arc::new(ServeStats::default());
    let ctx = Arc::new(Ctx {
        objects: objects.clone(),
        root: cfg.web_root,
        style: cfg.style,
        report: cfg.report.clone(),
        stats: stats.clone(),
        wake_secret: cfg.wake_secret.as_deref().map(|s| blake3::hash(s.as_bytes())),
        max_connections: cfg.max_connections,
        client: cfg.client,
        tokens: Allowances {
            per_minute: cfg.tokens_per_minute,
            buckets: Mutex::new(HashMap::new()),
        },
        stop: stop.clone(),
        active: active.clone(),
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
        active,
        threads,
    })
}

fn accept_loop(listener: &TcpListener, stop: &AtomicBool, ctx: &Arc<Ctx>) {
    for stream in listener.incoming() {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut stream) = stream else { continue };
        if ctx.active.fetch_add(1, Ordering::Relaxed) >= ctx.max_connections {
            ctx.active.fetch_sub(1, Ordering::Relaxed);
            ctx.stats.turned_away.fetch_add(1, Ordering::Relaxed);
            // A fresh socket's buffer takes the answer; the timeout only guards a peer that stops reading at once.
            let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
            let _ = web::respond(
                &mut stream,
                503,
                "Service Unavailable",
                "text/plain",
                b"too many connections",
            );
            continue;
        }
        let open = Open(ctx.active.clone());
        let conn_ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("serve-conn".into()).spawn(move || {
            let _open = open;
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

/// Waits for a connection's next request: whether one is coming. A connection idle for [`IDLE`], or idle while the
/// host drains, is closed; one whose request has begun is served.
fn next_request(reader: &mut BufReader<TcpStream>, ctx: &Ctx) -> Result<bool, RuntimeError> {
    let until = Instant::now() + IDLE;
    reader
        .get_ref()
        .set_read_timeout(Some(DRAIN_CHECK))
        .map_err(RuntimeError::Io)?;
    let coming = loop {
        if ctx.stop.load(Ordering::Relaxed) {
            break false;
        }
        match reader.fill_buf() {
            Ok(b) => break !b.is_empty(),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                if Instant::now() >= until {
                    break false;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(RuntimeError::Io(e)),
        }
    };
    // The request itself may take its time between bytes, up to IDLE.
    reader.get_ref().set_read_timeout(Some(IDLE)).map_err(RuntimeError::Io)?;
    Ok(coming)
}

fn connection(stream: TcpStream, ctx: &Ctx) -> Result<(), RuntimeError> {
    let _ = stream.set_nodelay(true);
    let peer = stream.peer_addr().map_err(RuntimeError::Io)?;
    let mut reader = BufReader::new(stream.try_clone().map_err(RuntimeError::Io)?);
    let mut w = stream;
    loop {
        if !next_request(&mut reader, ctx)? {
            return Ok(());
        }
        let req = web::read_request(&mut reader)?;
        ctx.stats.requests.fetch_add(1, Ordering::Relaxed);
        if req.path == "/blossom/link" {
            return socket(&req, reader, w, ctx);
        }
        // A draining host answers this request and closes: the client's next one goes to another instance.
        let keep = req.keep_alive() && !ctx.stop.load(Ordering::Relaxed);
        let body = web::read_body(&mut reader, &req, MAX_BODY)?;
        request(&req, &body, &mut w, ctx, keep, peer)?;
        if !keep || ctx.stop.load(Ordering::Relaxed) {
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

/// The client a request comes from, as [`ServeConfig::client`] says to tell it.
fn client_of(req: &Request, peer: SocketAddr, how: &ClientAddr) -> String {
    let told = match how {
        ClientAddr::Peer => None,
        ClientAddr::Header(name) => req
            .header(name)
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .filter(|v| !v.is_empty()),
    };
    told.map_or_else(|| peer.ip().to_string(), str::to_owned)
}

/// Whether a request shows the wake secret (`Authorization: Bearer SECRET`).
fn may_wake(req: &Request, secret: &blake3::Hash) -> bool {
    req.header("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|shown| blake3::hash(shown.as_bytes()) == *secret)
}

fn request(
    req: &Request,
    body: &[u8],
    w: &mut TcpStream,
    ctx: &Ctx,
    keep: bool,
    peer: SocketAddr,
) -> Result<(), RuntimeError> {
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
            if let Err(wait) = ctx.tokens.take(&client_of(req, peer, &ctx.client), Instant::now()) {
                ctx.stats.tokens_refused.fetch_add(1, Ordering::Relaxed);
                let after = wait.as_secs().max(1).to_string();
                return Response {
                    status: 429,
                    reason: "Too Many Requests",
                    content_type: "text/plain",
                    headers: &[("Retry-After", after.as_str())],
                    body: b"this client was given its page tokens for now",
                }
                .write(w, keep);
            }
            match ctx.objects.mint(&role) {
                Ok(token) => r(200, "OK", "application/octet-stream", &token).write(w, keep),
                Err(e) => failed(&e, w, ctx, keep),
            }
        }
        // A scheduler's call (Vercel's cron jobs make a GET).
        ("GET" | "POST", "/blossom/wake") => match &ctx.wake_secret {
            None => r(
                403,
                "Forbidden",
                "text/plain",
                b"this host has no wake secret (blossom serve --wake-secret-env)",
            )
            .write(w, keep),
            Some(secret) if !may_wake(req, secret) => {
                r(401, "Unauthorized", "text/plain", b"wake needs the secret").write(w, keep)
            }
            Some(_) => match ctx.objects.sweep(SWEEP_LIMIT) {
                Ok(woken) => {
                    ctx.stats.swept.fetch_add(woken.len() as u64, Ordering::Relaxed);
                    let json = serde_json::to_vec(&woken).map_err(|e| RuntimeError::Config(e.to_string()))?;
                    r(200, "OK", "application/json", &json).write(w, keep)
                }
                Err(e) => failed(&e, w, ctx, keep),
            },
        },
        // Whether this host can serve: its store answers.
        ("GET", "/blossom/health") => match ctx.objects.store.version(crate::stateless::REGISTRY) {
            Ok(_) => r(200, "OK", "text/plain", b"ok").write(w, keep),
            Err(e) => r(
                503,
                "Service Unavailable",
                "text/plain; charset=utf-8",
                format!("the state store: {e}").as_bytes(),
            )
            .write(w, keep),
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
        let draining = ctx.stop.clone();
        std::thread::Builder::new()
            .name("serve-push".into())
            .spawn(move || {
                let mut at = cursor;
                // A draining host closes the socket at its next presence: the page reconnects elsewhere.
                while !done.load(Ordering::Relaxed) && !draining.load(Ordering::Relaxed) {
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
