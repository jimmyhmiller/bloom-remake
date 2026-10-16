//! A page's requests, whatever server carries them (docs/design/DEPLOY.md §2): `blossom serve`'s own HTTP server and
//! a function platform's handler (crates/blossom-vercel) answer them here, the same way. The routes: `app.json`, the
//! client parts, the stylesheet, `/blossom/token` (within each client's allowance), `/blossom/health`, and the link
//! over plain requests (`/blossom/http/open`, then `SESSION/recv`, `SESSION/send` and `SESSION/close`; CLIENTS.md §3a,
//! with the cursor of STATELESS.md §6.3). Every answer comes from [`Objects`]: nothing is kept between requests but
//! the allowances.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::objects::{Objects, POLL_WAIT, ServeError, parse_session, session_id};
use crate::object::Cursor;
use crate::web::{self, Request};

/// The most clients whose token allowance a host remembers; past it, a new client is refused a token until some
/// client's allowance is whole again (a pool of page serials is finite: docs/design/DEPLOY.md §2).
const MAX_CLIENTS: usize = 65_536;

/// Where a request's client is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientAddr {
    /// The connection's peer: the host faces its clients.
    Peer,
    /// The first address in this header (lower case), which the proxy in front of the host sets; the peer when it
    /// is missing. Only behind a proxy that overwrites the header: a client could write any value itself.
    Header(String),
}

/// An answer to a request.
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub reason: &'static str,
    pub content_type: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
    /// A failure of the host itself (a 500), for the host to report: nobody else hears of it.
    pub fault: Option<String>,
}

impl Answer {
    pub fn new(status: u16, reason: &'static str, content_type: &'static str, body: impl Into<Vec<u8>>) -> Answer {
        Answer {
            status,
            reason,
            content_type,
            headers: Vec::new(),
            body: body.into(),
            fault: None,
        }
    }

    fn with(mut self, name: &'static str, value: String) -> Answer {
        self.headers.push((name, value));
        self
    }

    /// The answer to a request that failed: its status, and the reason as text.
    pub fn failed(e: &ServeError) -> Answer {
        let (status, reason) = match e {
            ServeError::Gone(_) => (410, "Gone"),
            ServeError::BadRequest(_) => (400, "Bad Request"),
            ServeError::Unavailable(_) => (503, "Service Unavailable"),
            ServeError::Runtime(_) => (500, "Internal Server Error"),
        };
        let mut a = Answer::new(status, reason, "text/plain; charset=utf-8", e.to_string());
        if status == 500 {
            a.fault = Some(e.to_string());
        }
        a
    }
}

/// A page's requests over a deployment's objects.
pub struct Front {
    objects: Arc<Objects>,
    /// The deployment's stylesheet (`[web] style`).
    style: Option<PathBuf>,
    client: ClientAddr,
    tokens: Allowances,
    tokens_refused: AtomicU64,
}

impl Front {
    /// `objects`' pages' requests; each client is given `tokens_per_minute` page tokens a minute.
    pub fn new(objects: Arc<Objects>, style: Option<PathBuf>, client: ClientAddr, tokens_per_minute: u32) -> Front {
        Front {
            objects,
            style,
            client,
            tokens: Allowances {
                per_minute: tokens_per_minute,
                buckets: Mutex::new(BTreeMap::new()),
            },
            tokens_refused: AtomicU64::new(0),
        }
    }

    pub fn objects(&self) -> &Arc<Objects> {
        &self.objects
    }

    /// How many page tokens were refused because their client had used its allowance.
    pub fn tokens_refused(&self) -> u64 {
        self.tokens_refused.load(Ordering::Relaxed)
    }

    /// The answer to a page's request from `peer`, or `None` when it is not one (the host's own routes and files).
    pub fn answer(&self, req: &Request, body: &[u8], peer: IpAddr) -> Option<Answer> {
        let path = req.path.as_str();
        if let Some(rest) = path.strip_prefix("/blossom/http/") {
            return Some(self.link(rest, req, body).unwrap_or_else(|e| Answer::failed(&e)));
        }
        Some(match (req.method.as_str(), path) {
            ("GET", "/blossom/app.json") => Answer::new(
                200,
                "OK",
                "application/json",
                self.objects.deploy.app_json().as_bytes().to_vec(),
            ),
            ("GET", web::STYLE_PATH) => match &self.style {
                Some(file) => match std::fs::read(file) {
                    Ok(css) => Answer::new(200, "OK", "text/css; charset=utf-8", css),
                    Err(e) => Answer::failed(&ServeError::Runtime(crate::RuntimeError::Config(format!(
                        "the stylesheet {}: {e}",
                        file.display()
                    )))),
                },
                None => Answer::new(404, "Not Found", "text/plain", "this deployment has no stylesheet"),
            },
            ("GET", p) if p.starts_with("/blossom/client/") => match p
                .strip_prefix("/blossom/client/")
                .and_then(|role| self.objects.deploy.client_part(role))
            {
                Some(part) => Answer::new(200, "OK", "application/octet-stream", part.to_vec()),
                None => Answer::new(404, "Not Found", "text/plain", "no such client role"),
            },
            ("POST", "/blossom/token") => self.token(req, peer),
            // Whether this host can serve: its store answers.
            ("GET", "/blossom/health") => match self.objects.store.version(super::REGISTRY) {
                Ok(_) => Answer::new(200, "OK", "text/plain", "ok"),
                Err(e) => Answer::new(
                    503,
                    "Service Unavailable",
                    "text/plain; charset=utf-8",
                    format!("the state store: {e}"),
                ),
            },
            _ => return None,
        })
    }

    fn token(&self, req: &Request, peer: IpAddr) -> Answer {
        let Some(role) = req.param("role") else {
            return Answer::new(
                400,
                "Bad Request",
                "text/plain",
                "a token is for a client role (?role=)",
            );
        };
        if let Err(wait) = self
            .tokens
            .take(&self.client_of(req, peer), crate::clock::host_monotonic())
        {
            self.tokens_refused.fetch_add(1, Ordering::Relaxed);
            return Answer::new(
                429,
                "Too Many Requests",
                "text/plain",
                "this client was given its page tokens for now",
            )
            .with("Retry-After", wait.as_secs().max(1).to_string());
        }
        match self.objects.mint(&role) {
            Ok(token) => Answer::new(200, "OK", "application/octet-stream", token),
            Err(e) => Answer::failed(&e),
        }
    }

    /// The client a request comes from, as [`ClientAddr`] says to tell it.
    fn client_of(&self, req: &Request, peer: IpAddr) -> String {
        let told = match &self.client {
            ClientAddr::Peer => None,
            ClientAddr::Header(name) => req
                .header(name)
                .and_then(|v| v.split(',').next())
                .map(str::trim)
                .filter(|v| !v.is_empty()),
        };
        told.map_or_else(|| peer.to_string(), str::to_owned)
    }

    /// The link over plain requests: `open`, then `SESSION/recv`, `SESSION/send` and `SESSION/close`.
    fn link(&self, rest: &str, req: &Request, body: &[u8]) -> Result<Answer, ServeError> {
        let objects = &self.objects;
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
            let mut a = Answer::new(200, "OK", "application/octet-stream", answer);
            if let Some((conn, secret, cursor)) = opened.session {
                a = a
                    .with("Blossom-Session", session_id(&object, conn, &secret))
                    .with("Blossom-Cursor", cursor.text());
            }
            return Ok(a);
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
                Ok(Answer::new(200, "OK", "application/octet-stream", answer).with("Blossom-Cursor", next.text()))
            }
            ("POST", "send") => {
                objects.send(&object, conn, &secret, &split_frames(body)?)?;
                Ok(Answer::new(204, "No Content", "text/plain", Vec::new()))
            }
            ("POST", "close") => {
                objects.close(&object, conn, &secret)?;
                Ok(Answer::new(204, "No Content", "text/plain", Vec::new()))
            }
            _ => Err(ServeError::BadRequest(format!("no link request `{verb}`"))),
        }
    }
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

/// Each client's allowance of page tokens: a bucket of `per_minute` that fills again at that rate.
struct Allowances {
    per_minute: u32,
    buckets: Mutex<BTreeMap<String, (f64, Instant)>>,
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
