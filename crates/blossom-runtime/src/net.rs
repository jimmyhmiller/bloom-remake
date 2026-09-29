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
use std::sync::Arc;

use blossom_base::RelId;
use blossom_ir::core::{Program, RelClass};
use blossom_oracle::Row;
use blossom_value::time::NodeId;
use blossom_wire::codec::{Codec, NodeEncoding, WireLimits};
use blossom_wire::frame::{Batch, ChannelSchema, Frame, Hello, KIND_PLAIN, PROTO, Peer, RejectReason};

use crate::RuntimeError;

/// The deployment facts both ends of a connection must agree on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub deployment: [u8; 16],
    pub program_id: [u8; 16],
    pub program_version: u32,
    pub directory: [u8; 16],
}

/// A program's channels with this side's schema ids: channel `i` of the list has sid `i`.
#[derive(Clone, Debug)]
pub struct Catalog {
    channels: Vec<(RelId, ChannelSchema)>,
}

impl Catalog {
    pub fn of(program: &Program) -> Result<Catalog, RuntimeError> {
        let mut channels = Vec::new();
        for (id, r) in program.rels.iter_enumerated() {
            if !matches!(r.class, RelClass::Channel(_)) {
                continue;
            }
            let sid = u32::try_from(channels.len()).map_err(|_| RuntimeError::Config("too many channels".into()))?;
            channels.push((
                id,
                ChannelSchema {
                    sid,
                    name: r.name.to_string(),
                    hash: blossom_wire::catalog::schema_hash(program, id),
                },
            ));
        }
        Ok(Catalog { channels })
    }

    pub fn schemas(&self) -> Vec<ChannelSchema> {
        self.channels.iter().map(|(_, c)| c.clone()).collect()
    }

    /// This side's sid for `rel`.
    pub fn sid(&self, rel: RelId) -> Option<u32> {
        self.channels.iter().find(|(r, _)| *r == rel).map(|(_, c)| c.sid)
    }

    /// Maps a peer's announced channels to this program's relations: only those whose name and schema hash match.
    pub fn accept(&self, theirs: &[ChannelSchema]) -> BTreeMap<u32, RelId> {
        let mut out = BTreeMap::new();
        for c in theirs {
            if let Some((rel, _)) = self
                .channels
                .iter()
                .find(|(_, mine)| mine.name == c.name && mine.hash == c.hash)
            {
                out.insert(c.sid, *rel);
            }
        }
        out
    }
}

/// A `HELLO` for this side.
pub fn hello(id: &Identity, peer: Peer, restarts: u64, boot_nonce: u64, catalog: &Catalog) -> Frame {
    Frame::Hello(Hello {
        proto: PROTO,
        deployment: id.deployment,
        program_id: id.program_id,
        program_version: id.program_version,
        peer,
        directory: id.directory,
        restarts,
        boot_nonce,
        channels: catalog.schemas(),
    })
}

/// Checks a peer's `HELLO` against this side's identity.
pub fn check_hello(h: &Hello, id: &Identity) -> Result<(), (RejectReason, String)> {
    if h.proto != PROTO {
        return Err((
            RejectReason::Protocol,
            format!("protocol {} (this node speaks {PROTO})", h.proto),
        ));
    }
    if h.deployment != id.deployment {
        return Err((RejectReason::Deployment, "another deployment".into()));
    }
    if h.program_id != id.program_id || h.program_version != id.program_version {
        return Err((
            RejectReason::Program,
            format!(
                "program version {} (this node runs {})",
                h.program_version, id.program_version
            ),
        ));
    }
    if h.directory != id.directory {
        return Err((RejectReason::Directory, "another node directory".into()));
    }
    Ok(())
}

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

/// The most bytes of tuples one `BATCH` carries: well under the receiver's frame limit.
pub const BATCH_BODY: usize = 1024 * 1024;

/// Rows of one channel encoded as `BATCH` frames: as many frames as it takes to keep each body under
/// [`BATCH_BODY`], and the number of rows too large for any frame (dropped: a message that cannot be sent is lost).
pub struct Frames {
    pub frames: Vec<Vec<u8>>,
    pub oversized: u64,
}

/// Encodes rows of one channel as `BATCH` frames.
pub fn batch_frames(
    codec: &Codec<'_>,
    program: &Program,
    sid: u32,
    rel: RelId,
    send_tick: u64,
    rows: &[&Row],
) -> Result<Frames, RuntimeError> {
    let cols = &program
        .rels
        .get(rel)
        .ok_or_else(|| blossom_base::internal_error!("channel {rel:?} is not declared"))?
        .schema
        .cols;
    let limit = WireLimits::default().max_frame.saturating_sub(64);
    let mut out = Frames {
        frames: Vec::new(),
        oversized: 0,
    };
    let mut body = Vec::new();
    let mut count = 0u64;
    let flush = |body: &mut Vec<u8>, count: &mut u64, out: &mut Frames| {
        if *count > 0 {
            out.frames.push(
                Frame::Batch(Batch {
                    sid,
                    send_tick,
                    kind: KIND_PLAIN,
                    count: *count,
                    body: std::mem::take(body),
                })
                .encode(),
            );
            *count = 0;
        }
    };
    for r in rows {
        let mut one = Vec::new();
        codec.encode_row(cols, r, &mut one)?;
        if one.len() > limit {
            out.oversized += 1;
            continue;
        }
        if count > 0 && body.len() + one.len() > BATCH_BODY {
            flush(&mut body, &mut count, &mut out);
        }
        body.extend_from_slice(&one);
        count += 1;
    }
    flush(&mut body, &mut count, &mut out);
    Ok(out)
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

/// Decodes a `BATCH`'s rows of channel `rel`.
pub fn batch_rows(codec: &Codec<'_>, program: &Program, rel: RelId, b: &Batch) -> Result<Vec<Row>, RuntimeError> {
    let cols = &program
        .rels
        .get(rel)
        .ok_or_else(|| blossom_base::internal_error!("channel {rel:?} is not declared"))?
        .schema
        .cols;
    let mut input = b.body.as_slice();
    let mut out = Vec::new();
    for _ in 0..b.count {
        out.push(Arc::from(codec.decode_row(cols, &mut input)?));
    }
    if !input.is_empty() {
        return Err(RuntimeError::Net("trailing bytes in a batch".into()));
    }
    Ok(out)
}

/// A dense-node codec for the wire.
pub fn wire_codec(program: &Program) -> Codec<'_> {
    Codec::new(program, NodeEncoding::Dense, WireLimits::default())
}

/// Whether column 0 of `row` is the node `me` (a channel tuple's destination).
pub fn addressed_to(row: &Row, me: NodeId) -> bool {
    matches!(row.first(), Some(blossom_value::Value::Node(n)) if *n == me)
}
