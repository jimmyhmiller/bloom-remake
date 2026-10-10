//! The objects of a deployment on a state store (docs/design/STATELESS.md §5–§9): each request loads its object (or
//! finds the node it ran last still current), runs it, and commits what it wrote under the version check, again from
//! the state that won when another commit came first. Nothing a request answers leaves before its commit.
//!
//! An object's entries hold, besides its node's store (`KvFs`'s keys) and its pages' links (`L/…`, [`LinkState`]),
//! the host's own:
//!
//! - `H/s/CONN`: a session ([`Session`]): the secret its id carries, and when its presence is checked next;
//! - `H/n`: the node's state between requests ([`blossom_node::Hibernation`], docs/design/STATELESS.md §5a): a load
//!   resumes the node from it, so its volatile tables, timers and clock carry on as if it had never stopped;
//! - `H/w`: the object's next wake (milliseconds since the epoch), the truth the store's wake hints point at;
//! - `H/xn`, `H/x/SEQ`: the outbox: messages to other objects, numbered, until they are delivered;
//! - `H/r/SENDER`: the highest outbox number taken from each sender;
//! - `H/t`: the registry's next serial.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use blossom_statestore::{Commit, StateError, StateStore, Write};
use blossom_store::{JournalKv, KvFs, KvStore};
use blossom_value::time::Instant;

use super::deployment::{Deployment, REGISTRY, Runs};
use crate::RuntimeError;
use crate::object::{Cursor, LinkState, ObjectNode, Output, Received};

/// How long a session lives with no request (CLIENTS.md §3a).
pub const LEASE: Duration = Duration::from_secs(30);
/// How long a receive waits for something to answer (CLIENTS.md §3a).
pub const POLL_WAIT: Duration = Duration::from_secs(25);
/// How often a request runs again after its commit lost.
const RETRIES: usize = 16;
/// How soon an outbox not yet delivered is tried again.
const OUTBOX_RETRY_MS: u64 = 1000;
/// The most outbox flushes one request does; the sweeper does the rest.
const FLUSHES_PER_REQUEST: usize = 64;

const WAKE_KEY: &str = "H/w";
const HIBERNATION_KEY: &str = "H/n";
const OUTBOX_NEXT_KEY: &str = "H/xn";
const SERIAL_KEY: &str = "H/t";

fn session_key(conn: u64) -> String {
    format!("H/s/{conn:016x}")
}

fn outbox_key(seq: u64) -> String {
    format!("H/x/{seq:016x}")
}

fn taken_key(sender: &str) -> String {
    format!("H/r/{sender}")
}

/// The side record of a session's presence: when a request of it last came.
pub fn presence_key(object: &str, conn: u64) -> String {
    format!("presence/{object}/{conn:016x}")
}

/// Why a request failed.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// What the request names is over or unknown (a session that ended): `410`.
    #[error("gone: {0}")]
    Gone(String),
    /// The request is malformed or names nothing the deployment has: `400`.
    #[error("bad request: {0}")]
    BadRequest(String),
    /// The store failed, or the request kept losing to other commits: `503`; the page tries again.
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

impl From<StateError> for ServeError {
    fn from(e: StateError) -> ServeError {
        ServeError::Unavailable(e.to_string())
    }
}

impl From<blossom_store::StoreError> for ServeError {
    fn from(e: blossom_store::StoreError) -> ServeError {
        ServeError::Runtime(e.into())
    }
}

fn pe(what: &str, e: postcard::Error) -> ServeError {
    ServeError::Runtime(RuntimeError::Store(blossom_store::StoreError::Invalid(format!(
        "{what}: {e}"
    ))))
}

/// A session: what a page's requests present, and when its presence is checked next.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub secret: [u8; 16],
    pub check_at: u64,
}

/// A message in an outbox.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Outgoing {
    /// The object it goes to.
    to: String,
    /// The deployment node that sent it (a node's `BATCH`; a member's `FROM_MEMBER` names its sender itself).
    from_node: Option<String>,
    frame: Vec<u8>,
}

/// An object's node and store, at a version.
struct Running {
    kv: Arc<JournalKv>,
    /// None for the registry.
    node: Option<ObjectNode>,
}

/// What an instance knows of one object.
#[derive(Default)]
struct Slot {
    /// The version `links`, `sessions` and the running node or snapshot are of; `None`: not read yet.
    version: Option<u64>,
    links: LinkState,
    sessions: BTreeMap<u64, Session>,
    /// The entries at `version`, when no node runs from them yet.
    snapshot: Option<Vec<(String, Vec<u8>)>>,
    running: Option<Running>,
}

impl Slot {
    fn forget(&mut self) {
        *self = Slot::default();
    }
}

/// What a changing request needs: the node (none for the registry), the object's store, the time, and the store
/// itself (presence records).
pub struct Ctx<'a> {
    pub node: Option<&'a mut ObjectNode>,
    pub kv: &'a JournalKv,
    pub now: Instant,
    pub now_ms: u64,
    pub store: &'a dyn StateStore,
    pub object: &'a str,
}

impl Ctx<'_> {
    pub fn node(&mut self) -> Result<&mut ObjectNode, ServeError> {
        self.node
            .as_deref_mut()
            .ok_or_else(|| ServeError::BadRequest(format!("`{}` runs no node", self.object)))
    }

    pub fn session(&self, conn: u64) -> Result<Option<Session>, ServeError> {
        read_session(self.kv, conn)
    }

    pub fn put_session(&self, conn: u64, s: &Session) -> Result<(), ServeError> {
        let b = postcard::to_allocvec(s).map_err(|e| pe("a session", e))?;
        self.kv.put(&session_key(conn), &b)?;
        Ok(())
    }

    fn u64_entry(&self, key: &str) -> Result<u64, ServeError> {
        match self.kv.get(key)? {
            Some(b) => Ok(u64::from_le_bytes(b.as_slice().try_into().map_err(|_| {
                ServeError::Runtime(RuntimeError::Store(blossom_store::StoreError::Invalid(format!(
                    "entry {key} is not 8 bytes"
                ))))
            })?)),
            None => Ok(0),
        }
    }
}

/// What a changing request produced, once committed: its own answer and the frames the node wrote to connections.
pub struct Done<R> {
    pub value: R,
    pub frames: Vec<(u64, Vec<u8>)>,
}

/// How many objects an instance keeps what it knows of, by default.
pub const DEFAULT_CACHE: usize = 1024;

/// What an instance knows of its objects, each with when a request last used it; at most `cap` of them (an object a
/// request is using stays past it).
struct Slots {
    by_object: BTreeMap<String, (Arc<Mutex<Slot>>, u64)>,
    clock: u64,
    cap: usize,
}

impl Slots {
    /// Forgets the least recently used objects no request holds, down to the cap.
    fn evict(&mut self) {
        let over = self.by_object.len().saturating_sub(self.cap);
        if over == 0 {
            return;
        }
        let mut idle: Vec<(u64, String)> = self
            .by_object
            .iter()
            .filter(|(_, (slot, _))| Arc::strong_count(slot) == 1)
            .map(|(o, (_, used))| (*used, o.clone()))
            .collect();
        idle.sort_unstable();
        for (_, o) in idle.into_iter().take(over) {
            self.by_object.remove(&o);
        }
    }
}

/// A deployment's objects on a state store.
pub struct Objects {
    pub deploy: Arc<Deployment>,
    pub store: Arc<dyn StateStore>,
    slots: Mutex<Slots>,
}

/// Milliseconds since the epoch of an instant.
pub fn ms_of(i: Instant) -> u64 {
    u64::try_from(i.0 / 1_000_000).unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> Result<MutexGuard<'_, T>, ServeError> {
    m.lock()
        .map_err(|_| ServeError::Unavailable("an object's lock is poisoned".into()))
}

/// The wall clock, as the host gives every request its time.
pub fn now() -> Result<Instant, ServeError> {
    crate::clock::wall_now().map_err(|e| ServeError::Runtime(RuntimeError::Config(e)))
}

fn random_bytes(buf: &mut [u8]) -> Result<(), ServeError> {
    crate::members::urandom(buf).map_err(ServeError::Runtime)
}

/// Decodes the host's records and the links from an object's entries.
fn decode_slot(entries: &[(String, Vec<u8>)]) -> Result<(LinkState, BTreeMap<u64, Session>), ServeError> {
    let links = LinkState::decode(entries.iter().map(|(k, v)| (k.as_str(), v.as_slice())))?;
    let mut sessions = BTreeMap::new();
    for (k, v) in entries {
        if let Some(hex) = k.strip_prefix("H/s/") {
            let conn =
                u64::from_str_radix(hex, 16).map_err(|_| ServeError::BadRequest(format!("a session key {k}")))?;
            sessions.insert(conn, postcard::from_bytes(v).map_err(|e| pe("a session", e))?);
        }
    }
    Ok((links, sessions))
}

impl Objects {
    pub fn new(deploy: Arc<Deployment>, store: Arc<dyn StateStore>) -> Objects {
        Objects {
            deploy,
            store,
            slots: Mutex::new(Slots {
                by_object: BTreeMap::new(),
                clock: 0,
                cap: DEFAULT_CACHE,
            }),
        }
    }

    /// Keeps at most `cap` objects' nodes and links in memory (`blossom serve --cache`).
    pub fn with_cache(self, cap: usize) -> Objects {
        if let Ok(mut slots) = self.slots.lock() {
            slots.cap = cap.max(1);
        }
        self
    }

    /// How many objects this instance keeps in memory now.
    pub fn cached(&self) -> Result<usize, ServeError> {
        Ok(lock(&self.slots)?.by_object.len())
    }

    fn slot(&self, object: &str) -> Result<Arc<Mutex<Slot>>, ServeError> {
        let mut slots = lock(&self.slots)?;
        slots.clock += 1;
        let now = slots.clock;
        let slot = {
            let entry = slots
                .by_object
                .entry(object.to_owned())
                .or_insert_with(|| (Arc::default(), now));
            entry.1 = now;
            entry.0.clone()
        };
        slots.evict();
        Ok(slot)
    }

    /// Brings a slot's links and sessions to the store's version (reading the entries if it moved).
    fn refresh(&self, object: &str, s: &mut Slot) -> Result<u64, ServeError> {
        let v = self.store.version(object)?;
        if s.version != Some(v) {
            let snap = self.store.load(object)?;
            let (links, sessions) = decode_slot(&snap.entries)?;
            *s = Slot {
                version: Some(snap.version),
                links,
                sessions,
                snapshot: Some(snap.entries),
                running: None,
            };
        }
        Ok(s.version.unwrap_or(0))
    }

    /// Brings a slot to the store's version with its node running.
    fn running(&self, object: &str, s: &mut Slot, now: Instant) -> Result<(), ServeError> {
        self.refresh(object, s)?;
        if s.running.is_some() {
            return Ok(());
        }
        let entries = s.snapshot.take().unwrap_or_default();
        let kv = Arc::new(JournalKv::load(entries));
        let node = match self.deploy.runs(object)? {
            Runs::Registry => None,
            Runs::Node { node, member } => {
                let hibernation = match kv.get(HIBERNATION_KEY)? {
                    Some(b) => Some(postcard::from_bytes(&b).map_err(|e| pe("a node's hibernation", e))?),
                    None => None,
                };
                let fs = KvFs::open(kv.clone() as Arc<dyn KvStore>)?;
                let mut node = self.deploy.open_node(
                    object,
                    node,
                    member,
                    super::deployment::Start {
                        fs: Arc::new(fs),
                        now,
                        hibernation,
                        tree: None,
                    },
                )?;
                node.restore_links(s.links.clone())?;
                Some(node)
            }
        };
        s.running = Some(Running { kv, node });
        Ok(())
    }

    /// A request's change, then the deliveries of the outboxes it filled (and those they fill in turn).
    fn request<R>(
        &self,
        object: &str,
        f: impl FnMut(&mut Ctx<'_>) -> Result<R, ServeError>,
    ) -> Result<Done<R>, ServeError> {
        let done = self.change(object, f)?;
        self.flush_from(object)?;
        Ok(done)
    }

    /// Runs a changing request on `object` (docs/design/STATELESS.md §5): `f` on its node, then the host's
    /// bookkeeping, then the commit; again from the state that won when another commit came first. `f` must be
    /// runnable again (it is, for a request: its input is the request). It delivers no outbox ([`Objects::request`]
    /// does).
    pub fn change<R>(
        &self,
        object: &str,
        mut f: impl FnMut(&mut Ctx<'_>) -> Result<R, ServeError>,
    ) -> Result<Done<R>, ServeError> {
        let slot = self.slot(object)?;
        let mut backoff = Duration::from_millis(2);
        for _ in 0..RETRIES {
            let now = now()?;
            let now_ms = ms_of(now);
            let mut s = lock(&slot)?;
            match self.attempt(object, &mut s, now, now_ms, &mut f) {
                Ok(Some(done)) => return Ok(done),
                Ok(None) => {
                    s.forget();
                    drop(s);
                    std::thread::sleep(backoff.mul_f64(0.5 + jitter()));
                    backoff = (backoff * 2).min(Duration::from_millis(200));
                }
                Err(e) => {
                    s.forget();
                    return Err(e);
                }
            }
        }
        Err(ServeError::Unavailable(format!(
            "a request on `{object}` lost to other commits {RETRIES} times"
        )))
    }

    /// One attempt: `Some` when it committed (or had nothing to commit), `None` when another commit came first.
    fn attempt<R>(
        &self,
        object: &str,
        s: &mut Slot,
        now: Instant,
        now_ms: u64,
        f: &mut impl FnMut(&mut Ctx<'_>) -> Result<R, ServeError>,
    ) -> Result<Option<Done<R>>, ServeError> {
        self.running(object, s, now)?;
        let version = s.version.unwrap_or(0);
        let run = s
            .running
            .as_mut()
            .ok_or_else(|| ServeError::Unavailable("an object's node did not start".into()))?;
        let mut ctx = Ctx {
            node: run.node.as_mut(),
            kv: &run.kv,
            now,
            now_ms,
            store: &*self.store,
            object,
        };
        let value = f(&mut ctx)?;
        let frames = self.bookkeep(object, &mut ctx)?;
        let writes: Vec<Write> = run
            .kv
            .take_writes()?
            .into_iter()
            .map(|(k, v)| match v {
                Some(v) => Write::Put(k, v),
                None => Write::Delete(k),
            })
            .collect();
        if writes.is_empty() {
            return Ok(Some(Done { value, frames }));
        }
        match self.store.commit(object, version, &writes)? {
            Commit::Done { version } => {
                s.version = Some(version);
                if let Some(run) = &s.running {
                    if let Some(node) = &run.node {
                        s.links = node.link_state()?;
                    }
                    let entries: Vec<(String, Vec<u8>)> = run
                        .kv
                        .list("H/s/")?
                        .into_iter()
                        .map(|k| Ok((k.clone(), run.kv.get(&k)?.unwrap_or_default())))
                        .collect::<Result<_, ServeError>>()?;
                    s.sessions = decode_slot(&entries)?.1;
                }
                Ok(Some(Done { value, frames }))
            }
            Commit::Conflict { .. } => Ok(None),
        }
    }

    /// The host's part of a request, after the node ran: the outbox from what the node sent, the sessions whose
    /// links ended, and the next wake (its hint added before the commit). Returns the frames the node wrote.
    fn bookkeep(&self, object: &str, ctx: &mut Ctx<'_>) -> Result<Vec<(u64, Vec<u8>)>, ServeError> {
        let mut frames = Vec::new();
        let mut next_wake: Option<u64> = None;
        let kv = ctx.kv;
        if let Some(node) = ctx.node.as_deref_mut() {
            let mut sends = Vec::new();
            for o in node.take_output() {
                match o {
                    Output::Frame { conn, bytes } => frames.push((conn, bytes)),
                    Output::Close { .. } => {}
                    Output::Send { to, rel, row, tick } => sends.push((to, rel, row, tick)),
                }
            }
            if !sends.is_empty() {
                let from_node = match node.member() {
                    Some(_) => None,
                    None => Some(node.name().to_owned()),
                };
                let mut seq = ctx_u64(kv, OUTBOX_NEXT_KEY)?;
                for (target, frame) in node.rpc_frames(&sends)? {
                    seq += 1;
                    let out = Outgoing {
                        to: Deployment::object_of(&target),
                        from_node: from_node.clone(),
                        frame,
                    };
                    kv.put(
                        &outbox_key(seq),
                        &postcard::to_allocvec(&out).map_err(|e| pe("an outbox entry", e))?,
                    )?;
                }
                kv.put(OUTBOX_NEXT_KEY, &seq.to_le_bytes())?;
            }
            let h = postcard::to_allocvec(&node.hibernate()?).map_err(|e| pe("a node's hibernation", e))?;
            if kv.get(HIBERNATION_KEY)?.as_deref() != Some(h.as_slice()) {
                kv.put(HIBERNATION_KEY, &h)?;
            }
            for (k, v) in node.export_links()? {
                match v {
                    Some(v) => kv.put(&k, &v)?,
                    None => kv.delete(&k)?,
                }
            }
            // Sessions whose links are over go, with their presence.
            let links = node.link_state()?;
            for k in kv.list("H/s/")? {
                let conn = k
                    .strip_prefix("H/s/")
                    .and_then(|h| u64::from_str_radix(h, 16).ok())
                    .ok_or_else(|| ServeError::BadRequest(format!("a session key {k}")))?;
                if links.member_of(conn).is_none() {
                    kv.delete(&k)?;
                    let _ = ctx.store.delete_side(&presence_key(object, conn));
                } else if let Some(s) = read_session(kv, conn)? {
                    next_wake = Some(next_wake.map_or(s.check_at, |w| w.min(s.check_at)));
                }
            }
            if let Some(t) = node.next_wake()? {
                let t = ms_of(t);
                next_wake = Some(next_wake.map_or(t, |w| w.min(t)));
            }
        }
        if !kv.list("H/x/")?.is_empty() {
            let t = ctx.now_ms + OUTBOX_RETRY_MS;
            next_wake = Some(next_wake.map_or(t, |w| w.min(t)));
        }
        let stored = kv
            .get(WAKE_KEY)?
            .map(|b| b.as_slice().try_into().map(u64::from_le_bytes));
        let stored = match stored {
            Some(Ok(v)) => Some(v),
            Some(Err(_)) => return Err(ServeError::BadRequest("a malformed wake entry".into())),
            None => None,
        };
        if next_wake != stored {
            match next_wake {
                Some(t) => {
                    // The hint first: a crash before the commit leaves an extra one, never a missing one.
                    ctx.store.schedule(object, t)?;
                    kv.put(WAKE_KEY, &t.to_le_bytes())?;
                }
                None => kv.delete(WAKE_KEY)?,
            }
        }
        Ok(frames)
    }

    /// The links and sessions of `object` at its current version (no node started).
    fn current(&self, object: &str) -> Result<(u64, Arc<Mutex<Slot>>), ServeError> {
        let slot = self.slot(object)?;
        let v = {
            let mut s = lock(&slot)?;
            self.refresh(object, &mut s)?
        };
        Ok((v, slot))
    }

    /// Checks a session's secret; the session's record if it holds.
    fn check(&self, s: &Slot, conn: u64, secret: &[u8; 16]) -> Result<Session, ServeError> {
        let rec = s
            .sessions
            .get(&conn)
            .ok_or_else(|| ServeError::Gone("the session ended".into()))?;
        // Compared whole: how much of a guessed secret matched must not show.
        if rec.secret.iter().zip(secret).fold(0u8, |acc, (a, b)| acc | (a ^ b)) != 0 {
            return Err(ServeError::Gone("no such session".into()));
        }
        Ok(rec.clone())
    }

    /// A receive (docs/design/STATELESS.md §6.3): what the session has not been given since `at`, waiting up to
    /// `wait` for something when there is nothing yet. Writes nothing to the object.
    pub fn receive(
        &self,
        object: &str,
        conn: u64,
        secret: &[u8; 16],
        at: Cursor,
        wait: Duration,
    ) -> Result<(Vec<Vec<u8>>, Cursor), ServeError> {
        let mut waited = Duration::ZERO;
        loop {
            let (version, slot) = self.current(object)?;
            let received = {
                let s = lock(&slot)?;
                self.check(&s, conn, secret)?;
                s.links.receive(conn, at)
            };
            match received {
                Received::Gone(why) => return Err(ServeError::Gone(why.into())),
                Received::Frames { frames, next } if !frames.is_empty() || waited >= wait => {
                    return Ok((frames, next));
                }
                Received::Frames { .. } => {}
            }
            let slice = (wait - waited).min(Duration::from_secs(5));
            self.store.wait(object, version, slice)?;
            waited += slice;
        }
    }

    /// Records that a session made a request now (its presence, docs/design/STATELESS.md §6.4).
    pub fn touch(&self, object: &str, conn: u64) -> Result<(), ServeError> {
        let now = ms_of(now()?);
        self.store.put_side(&presence_key(object, conn), &now.to_le_bytes())?;
        Ok(())
    }

    /// Opens a page's link (its `HELLO`): the frames that answer it (the node's `HELLO` and `HELLO_OK`, the
    /// `WELCOME` and the replay; or a `REJECT`), and, when the page was admitted, its session's connection, secret
    /// and cursor.
    pub fn open(&self, object: &str, hello: &[u8]) -> Result<Opened, ServeError> {
        let done = self.request(object, |ctx| {
            let now = ctx.now;
            let now_ms = ctx.now_ms;
            let node = ctx.node()?;
            let conn = node.connect();
            node.frame(conn, hello, now)?;
            let links = node.link_state()?;
            let admitted = links.member_of(conn).is_some();
            let cursor = links.opened(conn);
            let session = if admitted {
                let mut secret = [0u8; 16];
                random_bytes(&mut secret)?;
                let s = Session {
                    secret,
                    check_at: now_ms + LEASE.as_millis() as u64,
                };
                ctx.put_session(conn, &s)?;
                Some(s)
            } else {
                None
            };
            Ok((conn, session, cursor))
        })?;
        let (conn, session, cursor) = done.value;
        let frames = done
            .frames
            .into_iter()
            .filter(|(c, _)| *c == conn)
            .map(|(_, f)| f)
            .collect();
        if session.is_some() {
            self.touch(object, conn)?;
        }
        Ok(Opened {
            frames,
            session: session.map(|s| (conn, s.secret, cursor.unwrap_or_default())),
        })
    }

    /// A page's frames (`MSG`s and `ACK`s) on its session.
    pub fn send(&self, object: &str, conn: u64, secret: &[u8; 16], frames: &[Vec<u8>]) -> Result<(), ServeError> {
        {
            let (_, slot) = self.current(object)?;
            let s = lock(&slot)?;
            self.check(&s, conn, secret)?;
        }
        self.request(object, |ctx| {
            if ctx.session(conn)?.is_none_or(|s| s.secret != *secret) {
                return Err(ServeError::Gone("the session ended".into()));
            }
            let now = ctx.now;
            let node = ctx.node()?;
            for f in frames {
                node.frame(conn, f, now)?;
            }
            Ok(())
        })?;
        self.touch(object, conn)
    }

    /// The page is going: its session ends now.
    pub fn close(&self, object: &str, conn: u64, secret: &[u8; 16]) -> Result<(), ServeError> {
        {
            let (_, slot) = self.current(object)?;
            let s = lock(&slot)?;
            self.check(&s, conn, secret)?;
        }
        self.request(object, |ctx| {
            let now = ctx.now;
            ctx.node()?.closed(conn, now)?;
            Ok(())
        })?;
        Ok(())
    }

    /// The object's time came (a wake hint): sessions not seen for [`LEASE`] end, and the node's timers due run.
    pub fn wake(&self, object: &str) -> Result<(), ServeError> {
        self.check_sessions(object, false)
    }

    /// Checks every session's presence now, not only those whose check is due (operators and tests).
    pub fn expire_sessions_now(&self, object: &str) -> Result<(), ServeError> {
        self.check_sessions(object, true)
    }

    fn check_sessions(&self, object: &str, every: bool) -> Result<(), ServeError> {
        if object == REGISTRY {
            return Ok(());
        }
        self.request(object, |ctx| {
            let now = ctx.now;
            let now_ms = ctx.now_ms;
            let lease = LEASE.as_millis() as u64;
            let due: Vec<(u64, Session)> = ctx
                .kv
                .list("H/s/")?
                .into_iter()
                .filter_map(|k| k.strip_prefix("H/s/").and_then(|h| u64::from_str_radix(h, 16).ok()))
                .map(|c| Ok((c, ctx.session(c)?)))
                .collect::<Result<Vec<_>, ServeError>>()?
                .into_iter()
                .filter_map(|(c, s)| s.map(|s| (c, s)))
                .filter(|(_, s)| every || s.check_at <= now_ms)
                .collect();
            for (conn, mut s) in due {
                let seen = ctx
                    .store
                    .get_side(&presence_key(ctx.object, conn))?
                    .and_then(|b| b.as_slice().try_into().ok().map(u64::from_le_bytes))
                    .unwrap_or(0);
                if now_ms.saturating_sub(seen) >= lease {
                    ctx.node()?.closed(conn, now)?;
                } else {
                    s.check_at = seen + lease;
                    ctx.put_session(conn, &s)?;
                }
            }
            ctx.node()?.wake(now)?;
            Ok(())
        })?;
        Ok(())
    }

    /// The registry's next serial, for a page's token (docs/design/STATELESS.md §7).
    pub fn mint(&self, role: &str) -> Result<Vec<u8>, ServeError> {
        if !self
            .deploy
            .artifact
            .program
            .get()
            .roles
            .iter()
            .any(|r| r.name.to_string() == role)
        {
            return Err(ServeError::BadRequest(format!("`{role}` is not a role of the program")));
        }
        let done = self.change(REGISTRY, |ctx| {
            let n = ctx.u64_entry(SERIAL_KEY)?;
            ctx.kv.put(SERIAL_KEY, &(n + 1).to_le_bytes())?;
            u32::try_from(n).map_err(|_| ServeError::Unavailable("the registry gave every serial".into()))
        })?;
        Ok(crate::members::signed_token(self.deploy.seed, role, done.value))
    }

    /// Delivers the outboxes of `sender` and of every object its deliveries made send (at most
    /// [`FLUSHES_PER_REQUEST`]); what is left waits for the sweeper (each such object's wake is a second ahead).
    fn flush_from(&self, sender: &str) -> Result<(), ServeError> {
        let mut queue: VecDeque<String> = VecDeque::from([sender.to_owned()]);
        let mut flushes = 0;
        while let Some(from) = queue.pop_front() {
            if flushes >= FLUSHES_PER_REQUEST {
                break;
            }
            flushes += 1;
            let pending = self.outbox(&from)?;
            if pending.is_empty() {
                continue;
            }
            let mut delivered = 0;
            for (seq, out) in &pending {
                // In order, and stopping at the first that fails: a later one never overtakes it.
                let taken = self.change(&out.to, |ctx| {
                    let last = ctx.u64_entry(&taken_key(&from))?;
                    if *seq <= last {
                        return Ok(());
                    }
                    let now = ctx.now;
                    ctx.node()?.rpc_frame(out.from_node.as_deref(), &out.frame, now)?;
                    ctx.kv.put(&taken_key(&from), &seq.to_le_bytes())?;
                    Ok(())
                });
                if taken.is_err() {
                    break;
                }
                delivered = *seq;
                if !queue.contains(&out.to) {
                    queue.push_back(out.to.clone());
                }
            }
            if delivered > 0 {
                let _ = self.change(&from, |ctx| {
                    for k in ctx.kv.list("H/x/")? {
                        let seq = k.strip_prefix("H/x/").and_then(|h| u64::from_str_radix(h, 16).ok());
                        if seq.is_some_and(|s| s <= delivered) {
                            ctx.kv.delete(&k)?;
                        }
                    }
                    Ok(())
                });
            }
        }
        Ok(())
    }

    /// An object's undelivered messages, in order.
    fn outbox(&self, object: &str) -> Result<Vec<(u64, Outgoing)>, ServeError> {
        let (_, slot) = self.current(object)?;
        let s = lock(&slot)?;
        let entries: Vec<(String, Vec<u8>)> = match (&s.running, &s.snapshot) {
            (Some(run), _) => run
                .kv
                .list("H/x/")?
                .into_iter()
                .map(|k| Ok((k.clone(), run.kv.get(&k)?.unwrap_or_default())))
                .collect::<Result<_, ServeError>>()?,
            (None, Some(snap)) => snap.iter().filter(|(k, _)| k.starts_with("H/x/")).cloned().collect(),
            (None, None) => Vec::new(),
        };
        entries
            .into_iter()
            .map(|(k, v)| {
                let seq = k
                    .strip_prefix("H/x/")
                    .and_then(|h| u64::from_str_radix(h, 16).ok())
                    .ok_or_else(|| ServeError::BadRequest(format!("an outbox key {k}")))?;
                Ok((seq, postcard::from_bytes(&v).map_err(|e| pe("an outbox entry", e))?))
            })
            .collect()
    }

    /// One sweep (docs/design/STATELESS.md §9): wakes the objects due, removes the hints it handled, and runs the
    /// store's housekeeping. Returns the objects woken.
    pub fn sweep(&self, limit: usize) -> Result<Vec<String>, ServeError> {
        let now_ms = ms_of(now()?);
        let mut woken = Vec::new();
        for (object, at) in self.store.due(now_ms, limit)? {
            // The object's own next wake says whether this hint is still wanted; waking it brings that up to date.
            if self.wake(&object).is_ok() {
                self.store.unschedule(&object, at)?;
                woken.push(object);
            }
        }
        self.store.maintain(now_ms)?;
        Ok(woken)
    }

    /// The committed rows of a node's durable relation (tests and tools).
    pub fn rows(&self, object: &str, rel: &str) -> Result<Vec<Vec<blossom_value::Value>>, ServeError> {
        let slot = self.slot(object)?;
        let mut s = lock(&slot)?;
        self.running(object, &mut s, now()?)?;
        let node = s
            .running
            .as_ref()
            .and_then(|r| r.node.as_ref())
            .ok_or_else(|| ServeError::BadRequest(format!("`{object}` runs no node")))?;
        Ok(node.rows(rel)?.into_iter().map(|r| r.to_vec()).collect())
    }

    /// Forgets everything this instance knows of its objects (tests: an instance that restarts).
    pub fn forget_all(&self) -> Result<(), ServeError> {
        lock(&self.slots)?.by_object.clear();
        Ok(())
    }
}

fn read_session(kv: &JournalKv, conn: u64) -> Result<Option<Session>, ServeError> {
    match kv.get(&session_key(conn))? {
        Some(b) => Ok(Some(postcard::from_bytes(&b).map_err(|e| pe("a session", e))?)),
        None => Ok(None),
    }
}

fn ctx_u64(kv: &JournalKv, key: &str) -> Result<u64, ServeError> {
    match kv.get(key)? {
        Some(b) => {
            Ok(u64::from_le_bytes(b.as_slice().try_into().map_err(|_| {
                ServeError::BadRequest(format!("entry {key} is not 8 bytes"))
            })?))
        }
        None => Ok(0),
    }
}

/// A factor in [0, 1) for a retry's backoff, from the OS.
fn jitter() -> f64 {
    let mut b = [0u8; 2];
    match crate::members::urandom(&mut b) {
        Ok(()) => f64::from(u16::from_le_bytes(b)) / 65536.0,
        Err(_) => 0.5,
    }
}

/// What opening a link answered.
pub struct Opened {
    /// The frames for the page, in order.
    pub frames: Vec<Vec<u8>>,
    /// The session's connection, secret and first cursor, when the page was admitted.
    pub session: Option<(u64, [u8; 16], Cursor)>,
}

/// A session's id as a page carries it: the object, the connection and the secret (`OBJECT~CONN~SECRET`, the
/// object's name percent-encoded so the id is one URL path segment).
pub fn session_id(object: &str, conn: u64, secret: &[u8; 16]) -> String {
    let mut enc = String::new();
    for &b in object.as_bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' {
            enc.push(char::from(b));
        } else {
            enc.push_str(&format!("%{b:02X}"));
        }
    }
    let hex: String = secret.iter().map(|b| format!("{b:02x}")).collect();
    format!("{enc}~{conn:x}~{hex}")
}

/// A session's id read back: the object, the connection and the secret.
pub fn parse_session(id: &str) -> Option<(String, u64, [u8; 16])> {
    let mut parts = id.split('~');
    let (obj, conn, secret) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let mut bytes = Vec::new();
    let raw = obj.as_bytes();
    let mut i = 0;
    while let Some(&b) = raw.get(i) {
        if b == b'%' {
            bytes.push(u8::from_str_radix(obj.get(i + 1..i + 3)?, 16).ok()?);
            i += 3;
        } else {
            bytes.push(b);
            i += 1;
        }
    }
    let conn = u64::from_str_radix(conn, 16).ok()?;
    if secret.len() != 32 {
        return None;
    }
    let mut s = [0u8; 16];
    for (k, slot) in s.iter_mut().enumerate() {
        *slot = u8::from_str_radix(secret.get(2 * k..2 * k + 2)?, 16).ok()?;
    }
    Some((String::from_utf8(bytes).ok()?, conn, s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_round_trip() {
        let secret = [0xab; 16];
        for object in ["member/Room/lunch", "node/s", "member/Room/a b~c%/é"] {
            let id = session_id(object, 0x1f, &secret);
            assert!(!id.contains('/') && !id.contains(' '), "{id}");
            assert_eq!(parse_session(&id), Some((object.to_string(), 0x1f, secret)));
        }
        assert_eq!(parse_session("x~1"), None);
        assert_eq!(parse_session("x~zz~00"), None);
    }
}
