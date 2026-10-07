//! Connections (ARCHITECTURE §5.4, §5.8): the channel catalog, the `HELLO` handshake, and batch framing.
//!
//! The opener sends `HELLO` (who it is, the channels it will send with its schema ids). The acceptor checks the
//! protocol version, the deployment, the program and the directory, and answers with its own `HELLO` and a
//! `HELLO_OK` naming the opener's schema ids it accepts (a channel whose schema hash differs is left out: its batches
//! are dropped, `schema_mismatch`), or with `REJECT`. After that each side sends `BATCH` frames with its own schema
//! ids; the receiver maps them through the peer's `HELLO`.
//!
//! This build's transport is plaintext TCP (`--insecure-dev`): a node's or client's claimed identity is taken on
//! trust. mTLS binds it to a certificate (DIST-060).

use std::collections::BTreeMap;
use std::io::{BufReader, BufWriter, Write};
use std::net::TcpStream;

use blossom_base::RelId;
use blossom_wire::codec::WireLimits;
use blossom_wire::frame::{Frame, Hello, Peer, RejectReason};

use crate::RuntimeError;

pub use blossom_wire::link::{
    BATCH_BODY, Catalog, Frames, Identity, addressed_to, batch_frames, batch_rows, batches, check_hello, hello,
    wire_codec,
};

/// A connection's frame reader and writer.
pub struct Conn {
    pub reader: BufReader<TcpStream>,
    pub writer: BufWriter<TcpStream>,
    pub limits: WireLimits,
}

impl Conn {
    pub fn new(stream: TcpStream) -> Result<Conn, RuntimeError> {
        stream.set_nodelay(true).map_err(RuntimeError::Io)?;
        let w = stream.try_clone().map_err(RuntimeError::Io)?;
        Ok(Conn {
            reader: BufReader::with_capacity(64 * 1024, stream),
            writer: BufWriter::with_capacity(64 * 1024, w),
            limits: WireLimits::default(),
        })
    }

    pub fn read(&mut self) -> Result<Option<Frame>, RuntimeError> {
        Frame::read(&mut self.reader, &self.limits).map_err(|e| RuntimeError::Net(e.to_string()))
    }

    pub fn send(&mut self, f: &Frame) -> Result<(), RuntimeError> {
        f.write(&mut self.writer).map_err(RuntimeError::Io)?;
        self.writer.flush().map_err(RuntimeError::Io)
    }
}

/// A further check of a peer's `HELLO` (who may connect): a refusal carries the reason and its detail.
pub type Admit<'a> = &'a dyn Fn(&Hello) -> Result<(), (RejectReason, String)>;

/// What the other side said in its `HELLO`.
#[derive(Clone, Debug)]
pub struct PeerHello {
    pub peer: Peer,
    pub restarts: u64,
    pub boot_nonce: u64,
    /// Their sids → this program's relations.
    pub inbound: BTreeMap<u32, RelId>,
}

/// The acceptor's side of the handshake: reads the opener's `HELLO`, checks it with `admit` too, and answers.
pub fn accept_handshake(
    conn: &mut Conn,
    id: &Identity,
    me: Peer,
    restarts: u64,
    boot_nonce: u64,
    catalog: &Catalog,
    admit: Admit<'_>,
) -> Result<PeerHello, RuntimeError> {
    let Some(Frame::Hello(h)) = conn.read()? else {
        return Err(RuntimeError::Net("the connection did not open with HELLO".into()));
    };
    if let Err((reason, detail)) = check_hello(&h, id).and_then(|()| admit(&h)) {
        // The refusal is best-effort: the connection closes either way.
        let _ = conn.send(&Frame::Reject {
            reason,
            detail: detail.clone(),
        });
        return Err(RuntimeError::Net(format!("rejected {:?}: {detail}", h.peer)));
    }
    let inbound = catalog.accept(&h.channels);
    conn.send(&hello(id, me, restarts, boot_nonce, catalog))?;
    conn.send(&Frame::HelloOk {
        accepted_version: id.program_version,
        sids: inbound.keys().copied().collect(),
    })?;
    Ok(PeerHello {
        peer: h.peer,
        restarts: h.restarts,
        boot_nonce: h.boot_nonce,
        inbound,
    })
}

/// The opener's side: sends `HELLO`, reads the acceptor's `HELLO` and `HELLO_OK`.
pub fn open_handshake(
    conn: &mut Conn,
    id: &Identity,
    me: Peer,
    restarts: u64,
    boot_nonce: u64,
    catalog: &Catalog,
) -> Result<PeerHello, RuntimeError> {
    conn.send(&hello(id, me, restarts, boot_nonce, catalog))?;
    let h = match conn.read()? {
        Some(Frame::Hello(h)) => h,
        Some(Frame::Reject { reason, detail }) => {
            return Err(RuntimeError::Net(format!("rejected ({reason:?}): {detail}")));
        }
        other => return Err(RuntimeError::Net(format!("expected HELLO, got {other:?}"))),
    };
    check_hello(&h, id).map_err(|(r, d)| RuntimeError::Net(format!("the peer is incompatible ({r:?}): {d}")))?;
    match conn.read()? {
        Some(Frame::HelloOk { .. }) => {}
        other => return Err(RuntimeError::Net(format!("expected HELLO_OK, got {other:?}"))),
    }
    Ok(PeerHello {
        peer: h.peer,
        restarts: h.restarts,
        boot_nonce: h.boot_nonce,
        inbound: catalog.accept(&h.channels),
    })
}

/// Reads frames from a stream through a buffer, so a read timeout in the middle of a frame loses nothing: the bytes
/// read so far stay buffered for the next call.
pub struct FrameReader {
    buf: Vec<u8>,
    limits: WireLimits,
}

impl FrameReader {
    pub fn new() -> FrameReader {
        FrameReader {
            buf: Vec::new(),
            limits: WireLimits::default(),
        }
    }

    /// The next frame. `Ok(None)` when the stream's read timeout expires before a whole frame arrived.
    pub fn next(&mut self, stream: &mut dyn std::io::Read) -> Result<Option<Frame>, RuntimeError> {
        loop {
            if let Some((frame, used)) = Frame::parse(&self.buf, &self.limits)? {
                self.buf.drain(..used);
                return Ok(Some(frame));
            }
            let mut chunk = [0u8; 64 * 1024];
            match stream.read(&mut chunk) {
                Ok(0) => return Err(RuntimeError::Net("the connection closed".into())),
                Ok(n) => self.buf.extend_from_slice(chunk.get(..n).unwrap_or(&[])),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                    return Ok(None);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(RuntimeError::Io(e)),
            }
        }
    }
}

impl Default for FrameReader {
    fn default() -> FrameReader {
        FrameReader::new()
    }
}
