//! `blossom serve` (docs/design/STATELESS.md §11): a stateless host's HTTP front. It serves the page, `app.json`,
//! the client parts and the stylesheet; mints pages' tokens; runs pages' links over plain requests (CLIENTS.md §3a,
//! with the cursor of STATELESS.md §6.3) and over WebSockets (§6.5); answers `/blossom/wake` with a sweep, for a
//! caller with the wake secret; and sweeps on its own every [`ServeConfig::sweep`]. It keeps nothing a request needs
//! from an earlier one: every request goes to [`Objects`].
//!
//! A page's requests are answered by [`Front`], which a function platform's handler shares; this module adds what a
//! server of its own does: the page's files, WebSockets, the sweeper and `/blossom/wake`, and, on the open internet
//! (docs/design/DEPLOY.md §2), at most [`ServeConfig::max_connections`] at once and a [`Serving::drain`] on
//! `SIGTERM`.
//!
//! The HTTP server is the hand-written one of `blossom run` (crate::web): HTTP/1.1 with keep-alive, a thread per
//! connection.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub use super::front::ClientAddr;
use super::front::{Answer, Front};
use super::objects::Objects;
use crate::RuntimeError;
use crate::web::{self, Message, Request, Response};

/// How long a kept connection may sit between requests.
const IDLE: Duration = Duration::from_secs(75);
/// How often a connection waiting for its next request looks whether the host drains.
const DRAIN_CHECK: Duration = Duration::from_millis(250);
/// How long a WebSocket's pusher waits for the object before it records the session's presence again.
const SOCKET_PRESENCE: Duration = Duration::from_secs(10);
/// The most bytes a request's body carries (a page's frames; function platforms refuse more than about 4.5 MB).
const MAX_BODY: usize = 4 * 1024 * 1024;
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

/// Counters of what a host did.
#[derive(Default, Debug)]
pub struct ServeStats {
    pub requests: AtomicU64,
    pub failed: AtomicU64,
    pub swept: AtomicU64,
    /// Connections answered `503` because [`ServeConfig::max_connections`] were open.
    pub turned_away: AtomicU64,
}

/// A host serving: its address, and what stops it.
pub struct Serving {
    pub addr: SocketAddr,
    pub stats: Arc<ServeStats>,
    pub front: Arc<Front>,
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
    /// request it is in (a receive waits at most [`super::POLL_WAIT`]); a WebSocket closes at its next presence (its page
    /// reconnects, to another instance). Whether every connection ended in time. Nothing is lost when one did not:
    /// every request's effect is committed before it is answered.
    pub fn drain(mut self, grace: Duration) -> bool {
        self.halt();
        let until = crate::clock::host_monotonic() + grace;
        while self.active.load(Ordering::Relaxed) > 0 {
            if crate::clock::host_monotonic() >= until {
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
    front: Arc<Front>,
    root: Option<PathBuf>,
    report: Report,
    stats: Arc<ServeStats>,
    /// The wake secret's hash: comparing hashes takes the same time however much of a guess matched, and whatever
    /// its length.
    wake_secret: Option<blake3::Hash>,
    max_connections: usize,
    stop: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
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
    let front = Arc::new(Front::new(
        objects.clone(),
        cfg.style,
        cfg.client,
        cfg.tokens_per_minute,
    ));
    let ctx = Arc::new(Ctx {
        objects: objects.clone(),
        front: front.clone(),
        root: cfg.web_root,
        report: cfg.report.clone(),
        stats: stats.clone(),
        wake_secret: cfg.wake_secret.as_deref().map(|s| blake3::hash(s.as_bytes())),
        max_connections: cfg.max_connections,
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
        front,
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
    let until = crate::clock::host_monotonic() + IDLE;
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
                if crate::clock::host_monotonic() >= until {
                    break false;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(RuntimeError::Io(e)),
        }
    };
    // The request itself may take its time between bytes, up to IDLE.
    reader
        .get_ref()
        .set_read_timeout(Some(IDLE))
        .map_err(RuntimeError::Io)?;
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

/// Whether a request shows the wake secret (`Authorization: Bearer SECRET`).
fn may_wake(req: &Request, secret: &blake3::Hash) -> bool {
    req.header("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|shown| blake3::hash(shown.as_bytes()) == *secret)
}

/// Writes an answer.
fn write(a: &Answer, w: &mut TcpStream, keep: bool) -> Result<(), RuntimeError> {
    let headers: Vec<(&str, &str)> = a.headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
    Response {
        status: a.status,
        reason: a.reason,
        content_type: a.content_type,
        headers: &headers,
        body: &a.body,
    }
    .write(w, keep)
}

fn request(
    req: &Request,
    body: &[u8],
    w: &mut TcpStream,
    ctx: &Ctx,
    keep: bool,
    peer: SocketAddr,
) -> Result<(), RuntimeError> {
    if let Some(a) = ctx.front.answer(req, body, peer.ip()) {
        if let Some(fault) = &a.fault {
            ctx.stats.failed.fetch_add(1, Ordering::Relaxed);
            (ctx.report)(&format!("a request failed: {fault}"));
        }
        return write(&a, w, keep);
    }
    let r = Response::new;
    match (req.method.as_str(), req.path.as_str()) {
        // A scheduler's call.
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
                Err(e) => {
                    let a = Answer::failed(&e);
                    if let Some(fault) = &a.fault {
                        (ctx.report)(&format!("a wake failed: {fault}"));
                    }
                    write(&a, w, keep)
                }
            },
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
