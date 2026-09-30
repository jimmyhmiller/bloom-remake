//! Byte streams on real sockets (FOREIGN-PROTOCOLS §1): listeners, dials, a reader and a writer thread per
//! connection. What is logic — the order of stream events, the order of writes — lives in `blossom_node::streams`;
//! this module moves bytes.
//!
//! - A listener accepts a connection, allocates its [`ConnId`] (the restart count is the high half, so ids never
//!   repeat across incarnations), registers its writer, reports it opened, and only then starts its reader: the
//!   `opened` report is queued before any of its bytes.
//! - A reader queues what each read returns and, at the end (EOF or an error), reports the connection closed. It
//!   never tears the connection down: a peer that half-closed may still read the replies to what it sent.
//! - A writer writes the program's writes in `seq` order ([`SeqWriter`]). A duplicate or far-off `seq` is a located
//!   runtime error: the connection closes, the program's `closed` event carries the error, and it is counted.
//! - The host closes a connection when the program sends `close`, when the tick that delivered its `closed` event is
//!   released (after that tick's writes), or when its unsent writes pass the write limit.
//!
//! Backpressure is in bytes, at three points ([`StreamLimits`]):
//! - a reader stops reading its connection while the bytes it queued and the engine has not taken pass
//!   `read_ahead_bytes` (the peer's TCP window then fills, and the peer waits);
//! - every reader stops while the queue to the engine holds `queue_bytes`;
//! - the engine takes stream reports only while the node's undelivered stream bytes are under `backlog_bytes`.
//!
//! Stream reports have their own queue ([`StreamQueue`]), so stream bytes never hold up peers' messages or clients'
//! requests.

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use blossom_node::streams::SeqWriter;
use blossom_value::value::ConnId;

/// The byte limits of a node's streams (the deployment's `[stream_limits]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamLimits {
    /// The most bytes one connection's `data` event carries in one tick.
    pub max_stream_bytes: usize,
    /// The most bytes a connection's reader queues ahead of the engine.
    pub read_ahead_bytes: u64,
    /// The most bytes all readers together queue ahead of the engine.
    pub queue_bytes: u64,
    /// The most undelivered stream bytes the node holds before the engine stops taking more.
    pub backlog_bytes: u64,
    /// The most unsent bytes a connection's writes may hold before the connection is closed.
    pub write_queue_bytes: u64,
}

impl Default for StreamLimits {
    fn default() -> StreamLimits {
        StreamLimits {
            max_stream_bytes: 1024 * 1024,
            read_ahead_bytes: 1024 * 1024,
            queue_bytes: 64 * 1024 * 1024,
            backlog_bytes: 16 * 1024 * 1024,
            write_queue_bytes: 64 * 1024 * 1024,
        }
    }
}

/// What a stream's threads report to the engine thread (which stamps `opened` with the clock).
pub(crate) enum StreamData {
    Opened {
        stream: usize,
        conn: ConnId,
        peer: Arc<str>,
        req: Option<u64>,
    },
    Bytes {
        conn: ConnId,
        bytes: Vec<u8>,
        /// The reader's credit, returned when the engine takes these bytes.
        credit: Arc<Credit>,
    },
    Closed {
        conn: ConnId,
        reason: Arc<str>,
    },
    Failed {
        stream: usize,
        req: u64,
        reason: Arc<str>,
    },
}

/// The bytes a connection's reader has queued that the engine has not taken yet.
#[derive(Default)]
pub(crate) struct Credit {
    queued: Mutex<u64>,
    taken: Condvar,
}

impl Credit {
    /// The engine took `n` of the queued bytes.
    pub(crate) fn release(&self, n: u64) {
        if let Ok(mut q) = self.queued.lock() {
            *q = q.saturating_sub(n);
        }
        self.taken.notify_all();
    }
}

/// The stream reports waiting for the engine, in order.
pub(crate) struct StreamQueue {
    q: Mutex<VecDeque<StreamData>>,
    /// The bytes of the queued `Bytes` reports.
    bytes: AtomicU64,
    closed: AtomicBool,
    /// Whether a wake-up is already on its way to the engine.
    wake_pending: AtomicBool,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl StreamQueue {
    pub(crate) fn new(wake: Box<dyn Fn() + Send + Sync>) -> StreamQueue {
        StreamQueue {
            q: Mutex::new(VecDeque::new()),
            bytes: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            wake_pending: AtomicBool::new(false),
            wake,
        }
    }

    /// Queues a report; false once the engine has stopped.
    pub(crate) fn push(&self, d: StreamData) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        let Ok(mut q) = self.q.lock() else {
            return false;
        };
        if let StreamData::Bytes { bytes, .. } = &d {
            self.bytes.fetch_add(bytes.len() as u64, Ordering::SeqCst);
        }
        q.push_back(d);
        drop(q);
        if !self.wake_pending.swap(true, Ordering::SeqCst) {
            (self.wake)();
        }
        true
    }

    /// Takes reports in order while `budget` bytes remain (the last `Bytes` report taken may pass it by less than
    /// one read): a report that carries no bytes costs nothing.
    pub(crate) fn take(&self, budget: u64) -> Vec<StreamData> {
        self.wake_pending.store(false, Ordering::SeqCst);
        let Ok(mut q) = self.q.lock() else {
            return Vec::new();
        };
        let mut left = budget;
        let mut out = Vec::new();
        while let Some(front) = q.front() {
            let n = match front {
                StreamData::Bytes { bytes, .. } => bytes.len() as u64,
                _ => 0,
            };
            if n > 0 && left == 0 {
                break;
            }
            left = left.saturating_sub(n);
            self.bytes.fetch_sub(n, Ordering::SeqCst);
            if let Some(d) = q.pop_front() {
                out.push(d);
            }
        }
        out
    }

    /// Whether a report waits that `budget` lets the engine take.
    pub(crate) fn takeable(&self, budget: u64) -> bool {
        self.q.lock().is_ok_and(|q| match q.front() {
            Some(StreamData::Bytes { .. }) => budget > 0,
            Some(_) => true,
            None => false,
        })
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

/// How long a dial may take.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a writer waits on a peer that does not read.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// The largest read a reader makes at once.
const READ_CHUNK: usize = 64 * 1024;

/// Counters of what the streams did and refused.
#[derive(Debug, Default)]
pub struct StreamStats {
    pub opened: AtomicU64,
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    /// Writes and closes for connections that had already closed.
    pub dropped_closed: AtomicU64,
    /// Connections closed because their unsent writes passed the write limit.
    pub overflowed: AtomicU64,
    /// Connections closed for a duplicate or far-off write `seq`.
    pub seq_violations: AtomicU64,
    /// Writes and closes a program sent through a stream the connection does not belong to (refused, not done).
    pub wrong_stream: AtomicU64,
    /// Connections closed for a write the host refused (a blob range outside the blob).
    pub refused_writes: AtomicU64,
    /// Writes held behind a `seq` gap when their connection closed.
    pub dropped_held: AtomicU64,
    pub dials_failed: AtomicU64,
    /// The last located runtime error of a stream request (FOREIGN-PROTOCOLS §1.2): a bad `seq`, or a connection
    /// used through the wrong stream.
    pub last_violation: Mutex<Option<String>>,
}

fn bump(c: &AtomicU64, n: u64) {
    c.fetch_add(n, Ordering::Relaxed);
}

enum WriterMsg {
    Write { seq: u64, bytes: Vec<u8> },
    Close,
}

struct Handle {
    stream: usize,
    tx: Sender<WriterMsg>,
    sock: TcpStream,
    /// The bytes queued for the writer and not yet written (held writes included).
    unsent: Arc<AtomicU64>,
    /// Why the connection closed, when the host closed it for a located runtime error: the reader reports this
    /// rather than what the socket says.
    why: Arc<Mutex<Option<Arc<str>>>>,
}

/// The open stream connections of one node incarnation.
pub(crate) struct StreamConns {
    restarts: u64,
    next: AtomicU64,
    open: Mutex<BTreeMap<ConnId, Handle>>,
    limits: StreamLimits,
    pub stats: Arc<StreamStats>,
}

impl StreamConns {
    pub fn new(restarts: u64, limits: StreamLimits, stats: Arc<StreamStats>) -> StreamConns {
        StreamConns {
            restarts,
            next: AtomicU64::new(0),
            open: Mutex::new(BTreeMap::new()),
            limits,
            stats,
        }
    }

    fn allocate(&self) -> ConnId {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        ConnId(self.restarts << 32 | (n & 0xffff_ffff))
    }

    fn violation(&self, why: String) {
        if let Ok(mut last) = self.stats.last_violation.lock() {
            *last = Some(why);
        }
    }

    /// Queues a released write through `stream`. A connection that is gone counts it dropped; one of another
    /// stream refuses it; one whose unsent writes would pass the limit is closed.
    pub fn write(&self, stream: usize, conn: ConnId, seq: u64, bytes: Vec<u8>) {
        let Ok(mut open) = self.open.lock() else {
            return;
        };
        let Some(h) = open.get(&conn) else {
            bump(&self.stats.dropped_closed, 1);
            return;
        };
        if h.stream != stream {
            bump(&self.stats.wrong_stream, 1);
            let belongs = h.stream;
            drop(open);
            self.violation(format!(
                "a write through stream {stream} to connection {} of stream {belongs}",
                conn.0
            ));
            return;
        }
        let n = bytes.len() as u64;
        if h.unsent.load(Ordering::SeqCst).saturating_add(n) > self.limits.write_queue_bytes {
            bump(&self.stats.overflowed, 1);
            if let Ok(mut why) = h.why.lock() {
                *why = Some(Arc::from("its unsent writes passed the write limit"));
            }
            // Shutting down wakes the writer and the reader; the reader reports the close.
            let _ = h.sock.shutdown(std::net::Shutdown::Both);
            open.remove(&conn);
            return;
        }
        h.unsent.fetch_add(n, Ordering::SeqCst);
        if h.tx.send(WriterMsg::Write { seq, bytes }).is_err() {
            bump(&self.stats.dropped_closed, 1);
            open.remove(&conn);
        }
    }

    /// Closes `conn` after the writes already queued (a program's `close` through `stream`).
    pub fn close(&self, stream: usize, conn: ConnId) {
        let Ok(mut open) = self.open.lock() else {
            return;
        };
        match open.get(&conn) {
            None => bump(&self.stats.dropped_closed, 1),
            Some(h) if h.stream != stream => {
                bump(&self.stats.wrong_stream, 1);
                let belongs = h.stream;
                drop(open);
                self.violation(format!(
                    "a close through stream {stream} of connection {} of stream {belongs}",
                    conn.0
                ));
            }
            Some(h) => {
                let _ = h.tx.send(WriterMsg::Close);
                open.remove(&conn);
            }
        }
    }

    /// Closes `conn` for a located runtime error of the program (a write the host refuses): its `closed` event carries
    /// `why`, which is also recorded, as for a bad `seq`.
    pub fn refuse(&self, stream: usize, conn: ConnId, why: String) {
        let Ok(mut open) = self.open.lock() else {
            return;
        };
        match open.get(&conn) {
            None => bump(&self.stats.dropped_closed, 1),
            Some(h) if h.stream != stream => {
                bump(&self.stats.wrong_stream, 1);
            }
            Some(h) => {
                bump(&self.stats.refused_writes, 1);
                if let Ok(mut w) = h.why.lock() {
                    *w = Some(Arc::from(why.as_str()));
                }
                let _ = h.sock.shutdown(std::net::Shutdown::Both);
                open.remove(&conn);
                drop(open);
                self.violation(why);
            }
        }
    }

    /// Closes a connection whose `closed` event a released tick delivered, if the program has not already.
    pub fn retire(&self, conn: ConnId) {
        if let Ok(mut open) = self.open.lock()
            && let Some(h) = open.remove(&conn)
        {
            let _ = h.tx.send(WriterMsg::Close);
        }
    }

    /// Closes every connection (the node stops).
    pub fn close_all(&self) {
        if let Ok(mut open) = self.open.lock() {
            for h in open.values() {
                let _ = h.sock.shutdown(std::net::Shutdown::Both);
            }
            open.clear();
        }
    }
}

/// What a connection's threads share with the node's streams.
#[derive(Clone)]
pub(crate) struct Env {
    pub conns: Arc<StreamConns>,
    pub queue: Arc<StreamQueue>,
    pub stop: Arc<AtomicBool>,
}

/// Starts a connected socket: registers its writer, reports it opened, then starts its reader.
fn start(sock: TcpStream, stream: usize, req: Option<u64>, env: &Env) -> std::io::Result<()> {
    sock.set_nodelay(true)?;
    sock.set_write_timeout(Some(WRITE_TIMEOUT))?;
    sock.set_read_timeout(Some(Duration::from_millis(200)))?;
    let peer: Arc<str> = sock
        .peer_addr()
        .map_or_else(|_| Arc::from("?"), |a| Arc::from(a.to_string()));
    let writer_sock = sock.try_clone()?;
    let reader_sock = sock.try_clone()?;
    let conn = env.conns.allocate();
    let (tx, rx) = mpsc::channel::<WriterMsg>();
    let unsent = Arc::new(AtomicU64::new(0));
    let why: Arc<Mutex<Option<Arc<str>>>> = Arc::new(Mutex::new(None));
    env.conns
        .open
        .lock()
        .map_err(|_| std::io::Error::other("the stream table's lock is poisoned"))?
        .insert(
            conn,
            Handle {
                stream,
                tx,
                sock,
                unsent: unsent.clone(),
                why: why.clone(),
            },
        );
    bump(&env.conns.stats.opened, 1);
    if !env.queue.push(StreamData::Opened {
        stream,
        conn,
        peer,
        req,
    }) {
        // The engine stopped: nobody will serve or close this connection, so close it now.
        env.conns.retire(conn);
        return Ok(());
    }
    // From here the connection is the program's: a thread that cannot start closes it, and the reader's report (or
    // this one) tells the program.
    let stats = env.conns.stats.clone();
    let writer_why = why.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("stream-writer".into())
        .spawn(move || writer(writer_sock, rx, &unsent, &writer_why, &stats))
    {
        let _ = reader_sock.shutdown(std::net::Shutdown::Both);
        env.conns.retire(conn);
        env.queue.push(StreamData::Closed {
            conn,
            reason: Arc::from(format!("cannot start the writer: {e}")),
        });
        return Ok(());
    }
    let env2 = env.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("stream-reader".into())
        .spawn(move || reader(reader_sock, conn, &why, &env2))
    {
        env.conns.retire(conn);
        env.queue.push(StreamData::Closed {
            conn,
            reason: Arc::from(format!("cannot start the reader: {e}")),
        });
    }
    Ok(())
}

/// Accepts connections of `listen` stream `stream` until the node stops.
pub(crate) fn listen_loop(listener: TcpListener, stream: usize, env: Env) {
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    while !env.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((sock, _)) => {
                // A connection that cannot be set up is dropped, which closes it.
                if sock.set_nonblocking(false).is_ok() {
                    let _ = start(sock, stream, None, &env);
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Dials `addr` for connect stream `stream`, answering request `req` with an `opened` or a `failed` report. Every
/// address the name resolves to is tried in turn, as a TCP client does.
pub(crate) fn dial(stream: usize, req: u64, addr: Arc<str>, env: Env) {
    let spawned = std::thread::Builder::new().name("stream-dial".into()).spawn({
        let env = env.clone();
        let addr = addr.clone();
        move || {
            let result = connect_any(&addr).and_then(|s| start(s, stream, Some(req), &env));
            if let Err(e) = result {
                bump(&env.conns.stats.dials_failed, 1);
                env.queue.push(StreamData::Failed {
                    stream,
                    req,
                    reason: Arc::from(e.to_string()),
                });
            }
        }
    });
    if let Err(e) = spawned {
        env.queue.push(StreamData::Failed {
            stream,
            req,
            reason: Arc::from(format!("cannot start the dial: {e}")),
        });
    }
}

/// A connection to the first address of `addr` that accepts; the last error when none does.
fn connect_any(addr: &str) -> std::io::Result<TcpStream> {
    let targets: Vec<SocketAddr> = addr
        .to_socket_addrs()
        .map_err(|e| std::io::Error::other(format!("`{addr}` does not resolve: {e}")))?
        .collect();
    let mut last = std::io::Error::other(format!("`{addr}` resolves to no address"));
    for a in targets {
        match TcpStream::connect_timeout(&a, DIAL_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = std::io::Error::new(e.kind(), format!("{a}: {e}")),
        }
    }
    Err(last)
}

fn reader(mut sock: TcpStream, conn: ConnId, why: &Mutex<Option<Arc<str>>>, env: &Env) {
    let limits = env.conns.limits;
    let credit = Arc::new(Credit::default());
    let mut buf = vec![0u8; READ_CHUNK];
    let reason: Arc<str> = 'conn: loop {
        // Backpressure: wait while this connection, or all of them, queued enough ahead of the engine.
        loop {
            if env.stop.load(Ordering::SeqCst) {
                break 'conn Arc::from("the node stopped");
            }
            let Ok(queued) = credit.queued.lock() else {
                break 'conn Arc::from("the stream table's lock is poisoned");
            };
            if *queued < limits.read_ahead_bytes && env.queue.bytes.load(Ordering::SeqCst) < limits.queue_bytes {
                break;
            }
            // Woken when the engine takes this connection's bytes; the timeout re-checks the shared limit and stop.
            let _ = credit.taken.wait_timeout(queued, Duration::from_millis(50));
        }
        match sock.read(&mut buf) {
            Ok(0) => break Arc::from("eof"),
            Ok(n) => {
                bump(&env.conns.stats.bytes_in, n as u64);
                let bytes = buf.get(..n).map(<[u8]>::to_vec).unwrap_or_default();
                if let Ok(mut q) = credit.queued.lock() {
                    *q += n as u64;
                }
                if !env.queue.push(StreamData::Bytes {
                    conn,
                    bytes,
                    credit: credit.clone(),
                }) {
                    return;
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => break Arc::from(e.to_string()),
        }
    };
    let reason = why.lock().ok().and_then(|w| w.clone()).unwrap_or(reason);
    env.queue.push(StreamData::Closed { conn, reason });
}

fn writer(
    mut sock: TcpStream,
    rx: Receiver<WriterMsg>,
    unsent: &AtomicU64,
    why: &Mutex<Option<Arc<str>>>,
    stats: &StreamStats,
) {
    let mut order = SeqWriter::default();
    while let Ok(m) = rx.recv() {
        match m {
            WriterMsg::Write { seq, bytes } => match order.accept(seq, bytes) {
                Ok(ready) => {
                    for b in ready {
                        if sock.write_all(&b).is_err() {
                            let _ = sock.shutdown(std::net::Shutdown::Both);
                            return;
                        }
                        unsent.fetch_sub(b.len() as u64, Ordering::SeqCst);
                        bump(&stats.bytes_out, b.len() as u64);
                    }
                }
                Err(e) => {
                    // A located runtime error: the program wrote a `seq` it may not (FOREIGN-PROTOCOLS §1.2). The
                    // program's `closed` event carries it.
                    if let Ok(mut last) = stats.last_violation.lock() {
                        *last = Some(e.clone());
                    }
                    if let Ok(mut w) = why.lock() {
                        *w = Some(Arc::from(e));
                    }
                    bump(&stats.seq_violations, 1);
                    let _ = sock.shutdown(std::net::Shutdown::Both);
                    return;
                }
            },
            WriterMsg::Close => break,
        }
    }
    bump(&stats.dropped_held, order.held() as u64);
    let _ = sock.flush();
    let _ = sock.shutdown(std::net::Shutdown::Both);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connected pair: the client's socket, and the node's side started as connection 0 of stream 0.
    fn pair(limits: StreamLimits) -> (TcpStream, Env, ConnId) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        let env = Env {
            conns: Arc::new(StreamConns::new(0, limits, Arc::new(StreamStats::default()))),
            queue: Arc::new(StreamQueue::new(Box::new(|| {}))),
            stop: Arc::new(AtomicBool::new(false)),
        };
        start(server, 0, None, &env).unwrap();
        let conn = match env.queue.take(0).as_slice() {
            [StreamData::Opened { conn, .. }] => *conn,
            _ => panic!("the connection did not report opened first"),
        };
        (client, env, conn)
    }

    /// Waits (at most about 5 s) until `done` holds.
    fn eventually(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..500 {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_reader_stops_at_its_read_ahead_until_the_engine_takes_its_bytes() {
        let limits = StreamLimits {
            read_ahead_bytes: 64 * 1024,
            ..StreamLimits::default()
        };
        let (mut client, env, _) = pair(limits);
        let total = 8 * 1024 * 1024;
        let writer = std::thread::spawn(move || {
            let chunk = vec![7u8; 64 * 1024];
            let mut sent = 0;
            while sent < total {
                if client.write_all(&chunk).is_err() {
                    break;
                }
                sent += chunk.len();
            }
            client
        });
        // The reader queues its read-ahead (plus at most one read) and stops: the rest waits in TCP.
        assert!(eventually(
            || env.queue.bytes.load(Ordering::SeqCst) >= limits.read_ahead_bytes
        ));
        std::thread::sleep(Duration::from_millis(200));
        let queued = env.queue.bytes.load(Ordering::SeqCst);
        assert!(
            queued <= limits.read_ahead_bytes + READ_CHUNK as u64,
            "{queued} bytes queued past a read-ahead of {}",
            limits.read_ahead_bytes
        );
        // The engine takes bytes (returning their credit): the reader reads on, until everything arrived.
        let mut got = 0u64;
        assert!(eventually(|| {
            for d in env.queue.take(u64::MAX) {
                if let StreamData::Bytes { bytes, credit, .. } = d {
                    got += bytes.len() as u64;
                    credit.release(bytes.len() as u64);
                }
            }
            got == total as u64
        }));
        let _client = writer.join().unwrap();
        env.stop.store(true, Ordering::SeqCst);
        env.conns.close_all();
    }

    #[test]
    fn a_duplicate_seq_closes_the_connection_and_its_closed_event_says_why() {
        let (mut client, env, conn) = pair(StreamLimits::default());
        env.conns.write(0, conn, 0, b"first".to_vec());
        env.conns.write(0, conn, 0, b"again".to_vec());
        let mut got = Vec::new();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let _ = client.read_to_end(&mut got);
        assert_eq!(got, b"first");
        let mut why = None;
        assert!(eventually(|| {
            for d in env.queue.take(u64::MAX) {
                if let StreamData::Closed { reason, .. } = d {
                    why = Some(reason);
                }
            }
            why.is_some()
        }));
        let why = why.unwrap_or_else(|| Arc::from(""));
        assert!(why.contains("already written"), "{why}");
        env.stop.store(true, Ordering::SeqCst);
    }

    #[test]
    fn writes_past_the_write_limit_close_the_connection() {
        let limits = StreamLimits {
            write_queue_bytes: 1024,
            ..StreamLimits::default()
        };
        let (_client, env, conn) = pair(limits);
        // A write held behind a gap (seq 1 before seq 0) stays unsent; the next one passes the limit.
        env.conns.write(0, conn, 1, vec![0; 800]);
        env.conns.write(0, conn, 2, vec![0; 800]);
        assert_eq!(env.conns.stats.overflowed.load(Ordering::SeqCst), 1);
        env.conns.write(0, conn, 0, vec![0; 1]);
        assert_eq!(env.conns.stats.dropped_closed.load(Ordering::SeqCst), 1);
        env.stop.store(true, Ordering::SeqCst);
    }
}
