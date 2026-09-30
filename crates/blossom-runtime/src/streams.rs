//! Byte streams on real sockets (FOREIGN-PROTOCOLS §1): listeners, dials, a reader and a writer thread per
//! connection. What is logic — the order of stream events, the order of writes — lives in `blossom_node::streams`;
//! this module moves bytes.
//!
//! - A listener accepts a connection, allocates its [`ConnId`] (the restart count is the high half, so ids never
//!   repeat across incarnations), registers its writer, reports it opened, and only then starts its reader: the
//!   `opened` report is queued before any of its bytes.
//! - A reader queues what each read returns and, at the end (EOF or an error), reports the connection closed. It
//!   never tears the connection down: a peer that half-closed may still read the replies to what it sent.
//! - A writer writes the program's writes in `seq` order ([`SeqWriter`]); a duplicate or far-off `seq` closes the
//!   connection and is counted. The host closes a connection when the program sends `close`, when the tick that
//!   delivered its `closed` event is released (after that tick's writes), or when its writes back up past the queue.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use blossom_node::streams::SeqWriter;
use blossom_value::value::ConnId;

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

/// Queues a report for the engine; false once the engine has stopped.
pub(crate) type Push = Arc<dyn Fn(StreamData) -> bool + Send + Sync>;

/// The largest read a reader makes at once.
const READ_CHUNK: usize = 64 * 1024;
/// How many writes a connection's writer queues before the connection is closed for backing up.
const WRITE_QUEUE: usize = 4096;
/// How long a dial may take.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a writer waits on a peer that does not read.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Counters of what the streams did and refused.
#[derive(Debug, Default)]
pub struct StreamStats {
    pub opened: AtomicU64,
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    /// Writes and closes for connections that had already closed.
    pub dropped_closed: AtomicU64,
    /// Connections closed because their writes backed up past the queue.
    pub overflowed: AtomicU64,
    /// Connections closed for a duplicate or far-off write `seq`.
    pub seq_violations: AtomicU64,
    /// Writes held behind a `seq` gap when their connection closed.
    pub dropped_held: AtomicU64,
    pub dials_failed: AtomicU64,
    /// Why the last connection closed for a bad `seq` (the located runtime error, FOREIGN-PROTOCOLS §1.2).
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
    tx: SyncSender<WriterMsg>,
    sock: TcpStream,
}

/// The open stream connections of one node incarnation.
pub(crate) struct StreamConns {
    restarts: u64,
    next: AtomicU64,
    open: Mutex<BTreeMap<ConnId, Handle>>,
    pub stats: Arc<StreamStats>,
}

impl StreamConns {
    pub fn new(restarts: u64, stats: Arc<StreamStats>) -> StreamConns {
        StreamConns {
            restarts,
            next: AtomicU64::new(0),
            open: Mutex::new(BTreeMap::new()),
            stats,
        }
    }

    fn allocate(&self) -> ConnId {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        ConnId(self.restarts << 32 | (n & 0xffff_ffff))
    }

    /// Queues a released write; a connection that is gone counts it dropped, one whose queue is full is closed.
    pub fn write(&self, conn: ConnId, seq: u64, bytes: Vec<u8>) {
        self.send(conn, WriterMsg::Write { seq, bytes });
    }

    /// Closes `conn` after the writes already queued.
    pub fn close(&self, conn: ConnId) {
        self.send(conn, WriterMsg::Close);
        if let Ok(mut open) = self.open.lock() {
            open.remove(&conn);
        }
    }

    fn send(&self, conn: ConnId, m: WriterMsg) {
        let Ok(mut open) = self.open.lock() else {
            return;
        };
        let Some(h) = open.get(&conn) else {
            bump(&self.stats.dropped_closed, 1);
            return;
        };
        match h.tx.try_send(m) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                bump(&self.stats.overflowed, 1);
                // Shutting down wakes the writer and the reader; the reader reports the close.
                let _ = h.sock.shutdown(std::net::Shutdown::Both);
                open.remove(&conn);
            }
            Err(TrySendError::Disconnected(_)) => {
                bump(&self.stats.dropped_closed, 1);
                open.remove(&conn);
            }
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

/// Starts a connected socket: registers its writer, reports it opened, then starts its reader.
fn start(
    sock: TcpStream,
    stream: usize,
    req: Option<u64>,
    conns: &Arc<StreamConns>,
    push: &Push,
    stop: &Arc<AtomicBool>,
) -> std::io::Result<()> {
    sock.set_nodelay(true)?;
    sock.set_write_timeout(Some(WRITE_TIMEOUT))?;
    sock.set_read_timeout(Some(Duration::from_millis(200)))?;
    let peer: Arc<str> = sock
        .peer_addr()
        .map_or_else(|_| Arc::from("?"), |a| Arc::from(a.to_string()));
    let conn = conns.allocate();
    let (tx, rx) = mpsc::sync_channel::<WriterMsg>(WRITE_QUEUE);
    let writer_sock = sock.try_clone()?;
    let reader_sock = sock.try_clone()?;
    conns
        .open
        .lock()
        .map_err(|_| std::io::Error::other("the stream table's lock is poisoned"))?
        .insert(conn, Handle { tx, sock });
    bump(&conns.stats.opened, 1);
    if !push(StreamData::Opened {
        stream,
        conn,
        peer,
        req,
    }) {
        return Ok(());
    }
    let stats = conns.stats.clone();
    std::thread::Builder::new()
        .name("stream-writer".into())
        .spawn(move || writer(writer_sock, rx, &stats))?;
    let (push, stop, stats) = (push.clone(), stop.clone(), conns.stats.clone());
    std::thread::Builder::new()
        .name("stream-reader".into())
        .spawn(move || reader(reader_sock, conn, &push, &stop, &stats))?;
    Ok(())
}

/// Accepts connections of `listen` stream `stream` until the node stops.
pub(crate) fn listen_loop(
    listener: TcpListener,
    stream: usize,
    conns: Arc<StreamConns>,
    push: Push,
    stop: Arc<AtomicBool>,
) {
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((sock, _)) => {
                // A connection that cannot be set up is dropped, which closes it.
                if sock.set_nonblocking(false).is_ok() {
                    let _ = start(sock, stream, None, &conns, &push, &stop);
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Dials `addr` for connect stream `stream`, answering request `req` with an `opened` or a `failed` report.
pub(crate) fn dial(
    stream: usize,
    req: u64,
    addr: Arc<str>,
    conns: Arc<StreamConns>,
    push: Push,
    stop: Arc<AtomicBool>,
) {
    let spawned = std::thread::Builder::new().name("stream-dial".into()).spawn({
        let push = push.clone();
        let addr = addr.clone();
        move || {
            let target: Option<SocketAddr> = addr.to_socket_addrs().ok().and_then(|mut a| a.next());
            let result = match target {
                Some(a) => TcpStream::connect_timeout(&a, DIAL_TIMEOUT)
                    .and_then(|s| start(s, stream, Some(req), &conns, &push, &stop)),
                None => Err(std::io::Error::other(format!("`{addr}` is not a socket address"))),
            };
            if let Err(e) = result {
                bump(&conns.stats.dials_failed, 1);
                push(StreamData::Failed {
                    stream,
                    req,
                    reason: Arc::from(e.to_string()),
                });
            }
        }
    });
    if let Err(e) = spawned {
        push(StreamData::Failed {
            stream,
            req,
            reason: Arc::from(format!("cannot start the dial: {e}")),
        });
    }
}

fn reader(mut sock: TcpStream, conn: ConnId, push: &Push, stop: &AtomicBool, stats: &StreamStats) {
    let mut buf = vec![0u8; READ_CHUNK];
    let reason: Arc<str> = loop {
        if stop.load(Ordering::SeqCst) {
            break Arc::from("the node stopped");
        }
        match sock.read(&mut buf) {
            Ok(0) => break Arc::from("eof"),
            Ok(n) => {
                bump(&stats.bytes_in, n as u64);
                let bytes = buf.get(..n).map(<[u8]>::to_vec).unwrap_or_default();
                if !push(StreamData::Bytes { conn, bytes }) {
                    return;
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => break Arc::from(e.to_string()),
        }
    };
    push(StreamData::Closed { conn, reason });
}

fn writer(mut sock: TcpStream, rx: Receiver<WriterMsg>, stats: &StreamStats) {
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
                        bump(&stats.bytes_out, b.len() as u64);
                    }
                }
                Err(why) => {
                    // A located runtime error: the program wrote a `seq` it may not (FOREIGN-PROTOCOLS §1.2).
                    if let Ok(mut last) = stats.last_violation.lock() {
                        *last = Some(why);
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
