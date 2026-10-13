//! Peer liveness (ARCHITECTURE §5.13): each node's view of whether each peer is alive, made by the runtime outside the
//! node's ticks, which a program reads as `R.up(n, incarnation)` and `R.down(n)`.
//!
//! A node keeps a liveness connection to each peer, beside its data connection, that carries nothing but `ALIVE`
//! frames: one every `period`, sent by the connection's own thread, never behind data, a tick or a WAL sync. It sends
//! none while its engine has been inside one tick for longer than `stuck` (a live process with a stuck engine is not
//! serving; a tick that only runs long under load is not stuck, so `stuck` is far longer than any such tick). The receiving side stamps each frame as it arrives: a peer is up from its first frame (in an incarnation,
//! the `restarts` its `HELLO` gave), and down once no frame came for `session`. Only changes reach the engine, through
//! its control channel (not behind queued data), as inputs of its next tick: recorded in its trace, so replay sees the
//! same view. A loaded node is slower, not dead: its frames still leave and arrive on time.

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use blossom_value::time::NodeId;
use blossom_wire::frame::{Frame, Peer};

use crate::RuntimeError;
use crate::clock::Stopwatch;
use crate::net::{self, Catalog, Conn, Identity};

/// How often a node says it is alive, how long a peer counts as alive after its last word, and how long one tick of its
/// engine may run before it stops saying so (`[liveness]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LivenessConfig {
    pub period: Duration,
    pub session: Duration,
    pub stuck: Duration,
}

impl Default for LivenessConfig {
    fn default() -> LivenessConfig {
        LivenessConfig {
            period: Duration::from_millis(50),
            session: Duration::from_millis(500),
            stuck: Duration::from_secs(10),
        }
    }
}

/// A change in a node's view of a peer: up in an incarnation, or down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerChange {
    pub peer: NodeId,
    pub up: Option<u64>,
}

/// Whether the engine is making progress: when the tick it is in began (none between ticks).
pub struct EngineProgress {
    origin: Stopwatch,
    /// The nanoseconds since `origin` when the current tick began, plus one; 0 between ticks.
    tick_began: AtomicU64,
}

impl EngineProgress {
    pub fn new() -> EngineProgress {
        EngineProgress {
            origin: Stopwatch::start(),
            tick_began: AtomicU64::new(0),
        }
    }

    pub fn tick_begins(&self) {
        self.tick_began
            .store(self.origin.nanos().saturating_add(1), Ordering::SeqCst);
    }

    pub fn tick_ends(&self) {
        self.tick_began.store(0, Ordering::SeqCst);
    }

    /// Whether the engine is between ticks, or in one that began less than `limit` ago.
    pub fn responsive(&self, limit: Duration) -> bool {
        let began = self.tick_began.load(Ordering::SeqCst);
        began == 0 || self.origin.nanos().saturating_sub(began - 1) < limit.as_nanos() as u64
    }
}

impl Default for EngineProgress {
    fn default() -> EngineProgress {
        EngineProgress::new()
    }
}

/// What a node has heard of each peer.
struct View {
    incarnation: u64,
    /// When its last frame arrived, in nanoseconds since the views' origin.
    last: u64,
    up: bool,
}

/// A node's views of its peers, fed by its liveness connections' readers and checked for silence.
pub struct PeerViews {
    origin: Stopwatch,
    session: Duration,
    views: Mutex<BTreeMap<NodeId, View>>,
    changes: Box<dyn Fn(PeerChange) + Send + Sync>,
}

impl PeerViews {
    /// Views that report each change to `changes`.
    pub fn new(session: Duration, changes: Box<dyn Fn(PeerChange) + Send + Sync>) -> PeerViews {
        PeerViews {
            origin: Stopwatch::start(),
            session,
            views: Mutex::new(BTreeMap::new()),
            changes,
        }
    }

    /// A frame from `peer`, in incarnation `incarnation`, arrived now.
    pub fn heard(&self, peer: NodeId, incarnation: u64) {
        let now = self.origin.nanos();
        let changed = {
            let Ok(mut views) = self.views.lock() else { return };
            let v = views.entry(peer).or_insert(View {
                incarnation,
                last: now,
                up: false,
            });
            if incarnation < v.incarnation {
                // An older incarnation's connection, not yet closed: it says nothing of the newer one.
                return;
            }
            v.last = now;
            let changed = !v.up || v.incarnation != incarnation;
            v.up = true;
            v.incarnation = incarnation;
            changed
        };
        if changed {
            (self.changes)(PeerChange {
                peer,
                up: Some(incarnation),
            });
        }
    }

    /// Takes down each peer up whose last frame is older than the session.
    pub fn check(&self) {
        let now = self.origin.nanos();
        let session = self.session.as_nanos() as u64;
        let mut down = Vec::new();
        {
            let Ok(mut views) = self.views.lock() else { return };
            for (peer, v) in views.iter_mut() {
                if v.up && now.saturating_sub(v.last) >= session {
                    v.up = false;
                    down.push(*peer);
                }
            }
        }
        for peer in down {
            (self.changes)(PeerChange { peer, up: None });
        }
    }
}

/// The checker: takes silent peers down, a few times a session, until `stop`.
pub fn checker(views: Arc<PeerViews>, cfg: LivenessConfig, stop: Arc<AtomicBool>) {
    let every = (cfg.session / 10).max(Duration::from_millis(1));
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(every);
        views.check();
    }
}

/// A liveness connection to the peer at `addr`: connects (retrying), says `HELLO`, then sends `ALIVE` every period
/// while the engine is responsive, until `stop`.
#[allow(clippy::too_many_arguments)]
pub fn sender(
    addr: SocketAddr,
    id: Identity,
    me: NodeId,
    restarts: u64,
    nonce: u64,
    catalog: Arc<Catalog>,
    progress: Arc<EngineProgress>,
    cfg: LivenessConfig,
    stop: Arc<AtomicBool>,
) {
    let mut backoff = Duration::from_millis(20);
    while !stop.load(Ordering::SeqCst) {
        let conn = connect(addr, &id, me, restarts, nonce, &catalog, cfg.period);
        let mut conn = match conn {
            Ok(c) => {
                backoff = Duration::from_millis(20);
                c
            }
            Err(_) => {
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(cfg.period.max(Duration::from_millis(20)));
                continue;
            }
        };
        while !stop.load(Ordering::SeqCst) {
            if progress.responsive(cfg.stuck) && conn.send(&Frame::Alive).is_err() {
                break;
            }
            std::thread::sleep(cfg.period);
        }
    }
}

fn connect(
    addr: SocketAddr,
    id: &Identity,
    me: NodeId,
    restarts: u64,
    nonce: u64,
    catalog: &Catalog,
    period: Duration,
) -> Result<Conn, RuntimeError> {
    let s = TcpStream::connect_timeout(&addr, Duration::from_secs(1)).map_err(RuntimeError::Io)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(RuntimeError::Io)?;
    // A frame that cannot leave within a period is late anyway: the connection is dropped and made again.
    s.set_write_timeout(Some(period.max(Duration::from_millis(10))))
        .map_err(RuntimeError::Io)?;
    let mut c = Conn::new(s)?;
    net::open_handshake(&mut c, id, Peer::Node(me.0), restarts, nonce, catalog)?;
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_peer_is_up_from_its_first_frame_and_down_after_a_silent_session() {
        let seen: Arc<Mutex<Vec<PeerChange>>> = Arc::default();
        let views = {
            let seen = seen.clone();
            PeerViews::new(
                Duration::from_millis(30),
                Box::new(move |c| seen.lock().unwrap().push(c)),
            )
        };
        let (a, b) = (NodeId(1), NodeId(2));
        views.heard(a, 0);
        views.heard(a, 0);
        views.heard(b, 3);
        views.check();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![PeerChange { peer: a, up: Some(0) }, PeerChange { peer: b, up: Some(3) }]
        );
        std::thread::sleep(Duration::from_millis(40));
        views.heard(b, 3);
        views.check();
        // A restarted peer is up in its new incarnation; an older incarnation's late frame changes nothing.
        views.heard(b, 4);
        views.heard(b, 3);
        assert_eq!(
            seen.lock().unwrap()[2..],
            [PeerChange { peer: a, up: None }, PeerChange { peer: b, up: Some(4) }]
        );
    }

    #[test]
    fn an_engine_inside_a_long_tick_is_not_responsive() {
        let p = EngineProgress::new();
        assert!(p.responsive(Duration::from_millis(10)));
        p.tick_begins();
        assert!(p.responsive(Duration::from_millis(1000)));
        std::thread::sleep(Duration::from_millis(15));
        assert!(!p.responsive(Duration::from_millis(10)));
        p.tick_ends();
        assert!(p.responsive(Duration::from_millis(10)));
    }
}
