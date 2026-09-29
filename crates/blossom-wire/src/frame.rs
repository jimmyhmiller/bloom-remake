//! Frames (ARCHITECTURE §5.4): `len:u32 type:u8 body`, little-endian, `len` excluding itself.
//!
//! ```text
//! HELLO    0x01 := magic:"BLSM" proto:u16 deployment:[16] program_id:[16] program_version:u32 peer directory:[16]
//!                  restarts:u64 boot_nonce:u64 n:varint (sid:varint name:str schema_hash:[16]){n}
//!          peer := 0 node:u32 | 1 principal:str                 (a node of the deployment, or a client session)
//! HELLO_OK 0x02 := accepted_version:u32 n:varint (sid:varint){n}
//! REJECT   0x03 := reason:u8 detail:str
//! GOAWAY   0x04 := reason:u8
//! BATCH    0x10 := sid:varint send_tick:varint kind:u8 count:varint tuple{count}
//! ```
//!
//! The architecture's HELLO names only a node; a client session's HELLO carries the principal it claims, which only
//! the insecure development transport accepts unauthenticated (TLS binds it to a certificate).

use std::io::{Read, Write};

use crate::codec::{WireError, WireLimits, get_varint, put_varint, take};

pub const MAGIC: [u8; 4] = *b"BLSM";
/// The protocol version this build speaks.
pub const PROTO: u16 = 1;

const T_HELLO: u8 = 0x01;
const T_HELLO_OK: u8 = 0x02;
const T_REJECT: u8 = 0x03;
const T_GOAWAY: u8 = 0x04;
const T_BATCH: u8 = 0x10;

/// Who opened a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Peer {
    /// A node of the deployment, by its dense id.
    Node(u32),
    /// A client session, with the principal it claims.
    Client { principal: String },
}

/// One channel as the sender knows it: its schema id on this connection, name and schema hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelSchema {
    pub sid: u32,
    pub name: String,
    pub hash: [u8; 16],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub proto: u16,
    pub deployment: [u8; 16],
    pub program_id: [u8; 16],
    pub program_version: u32,
    pub peer: Peer,
    pub directory: [u8; 16],
    pub restarts: u64,
    pub boot_nonce: u64,
    pub channels: Vec<ChannelSchema>,
}

/// Why a connection was refused.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RejectReason {
    Protocol = 1,
    Deployment = 2,
    Program = 3,
    Directory = 4,
    SchemaMismatch = 5,
    NotAllowed = 6,
}

impl RejectReason {
    fn of(b: u8) -> Result<RejectReason, WireError> {
        Ok(match b {
            1 => RejectReason::Protocol,
            2 => RejectReason::Deployment,
            3 => RejectReason::Program,
            4 => RejectReason::Directory,
            5 => RejectReason::SchemaMismatch,
            6 => RejectReason::NotAllowed,
            other => return Err(WireError::Malformed(format!("reject reason {other}"))),
        })
    }
}

/// A batch of tuples of one channel, sent at one tick. `body` holds the tuples back to back (each self-delimiting).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    pub sid: u32,
    pub send_tick: u64,
    pub kind: u8,
    pub count: u64,
    pub body: Vec<u8>,
}

/// Batch kinds (DIST-006): only plain batches exist in this build.
pub const KIND_PLAIN: u8 = 0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Hello(Hello),
    HelloOk { accepted_version: u32, sids: Vec<u32> },
    Reject { reason: RejectReason, detail: String },
    GoAway { reason: u8 },
    Batch(Batch),
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn get_str(input: &mut &[u8]) -> Result<String, WireError> {
    let n = usize::try_from(get_varint(input)?).map_err(|_| WireError::Limit("string length"))?;
    let b = take(input, n, "a string")?;
    String::from_utf8(b.to_vec()).map_err(|_| WireError::Malformed("invalid UTF-8".into()))
}

fn arr16(input: &mut &[u8], what: &'static str) -> Result<[u8; 16], WireError> {
    take(input, 16, what)?.try_into().map_err(|_| WireError::Truncated(what))
}

fn le<const N: usize>(input: &mut &[u8], what: &'static str) -> Result<[u8; N], WireError> {
    take(input, N, what)?.try_into().map_err(|_| WireError::Truncated(what))
}

impl Frame {
    /// The frame's bytes, length prefix included.
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        let t = match self {
            Frame::Hello(h) => {
                body.extend_from_slice(&MAGIC);
                body.extend_from_slice(&h.proto.to_le_bytes());
                body.extend_from_slice(&h.deployment);
                body.extend_from_slice(&h.program_id);
                body.extend_from_slice(&h.program_version.to_le_bytes());
                match &h.peer {
                    Peer::Node(n) => {
                        body.push(0);
                        body.extend_from_slice(&n.to_le_bytes());
                    }
                    Peer::Client { principal } => {
                        body.push(1);
                        put_str(&mut body, principal);
                    }
                }
                body.extend_from_slice(&h.directory);
                body.extend_from_slice(&h.restarts.to_le_bytes());
                body.extend_from_slice(&h.boot_nonce.to_le_bytes());
                put_varint(&mut body, h.channels.len() as u64);
                for c in &h.channels {
                    put_varint(&mut body, u64::from(c.sid));
                    put_str(&mut body, &c.name);
                    body.extend_from_slice(&c.hash);
                }
                T_HELLO
            }
            Frame::HelloOk { accepted_version, sids } => {
                body.extend_from_slice(&accepted_version.to_le_bytes());
                put_varint(&mut body, sids.len() as u64);
                for s in sids {
                    put_varint(&mut body, u64::from(*s));
                }
                T_HELLO_OK
            }
            Frame::Reject { reason, detail } => {
                body.push(*reason as u8);
                put_str(&mut body, detail);
                T_REJECT
            }
            Frame::GoAway { reason } => {
                body.push(*reason);
                T_GOAWAY
            }
            Frame::Batch(b) => {
                put_varint(&mut body, u64::from(b.sid));
                put_varint(&mut body, b.send_tick);
                body.push(b.kind);
                put_varint(&mut body, b.count);
                body.extend_from_slice(&b.body);
                T_BATCH
            }
        };
        let mut out = Vec::with_capacity(body.len() + 5);
        let len = (body.len() + 1) as u32;
        out.extend_from_slice(&len.to_le_bytes());
        out.push(t);
        out.extend_from_slice(&body);
        out
    }

    /// Decodes a frame from its type byte and body.
    pub fn decode(t: u8, mut body: &[u8], limits: &WireLimits) -> Result<Frame, WireError> {
        let input = &mut body;
        let frame = match t {
            T_HELLO => {
                if take(input, 4, "magic")? != MAGIC {
                    return Err(WireError::Malformed("bad magic".into()));
                }
                let proto = u16::from_le_bytes(le(input, "proto")?);
                let deployment = arr16(input, "deployment")?;
                let program_id = arr16(input, "program id")?;
                let program_version = u32::from_le_bytes(le(input, "program version")?);
                let (&kind, rest) = input.split_first().ok_or(WireError::Truncated("peer kind"))?;
                *input = rest;
                let peer = match kind {
                    0 => Peer::Node(u32::from_le_bytes(le(input, "node")?)),
                    1 => Peer::Client {
                        principal: get_str(input)?,
                    },
                    other => return Err(WireError::Malformed(format!("peer kind {other}"))),
                };
                let directory = arr16(input, "directory digest")?;
                let restarts = u64::from_le_bytes(le(input, "restarts")?);
                let boot_nonce = u64::from_le_bytes(le(input, "boot nonce")?);
                let n = get_varint(input)?;
                if n > input.len() as u64 {
                    return Err(WireError::Limit("channel count"));
                }
                let mut channels = Vec::new();
                for _ in 0..n {
                    let sid = u32::try_from(get_varint(input)?).map_err(|_| WireError::Malformed("sid".into()))?;
                    let name = get_str(input)?;
                    let hash = arr16(input, "schema hash")?;
                    channels.push(ChannelSchema { sid, name, hash });
                }
                Frame::Hello(Hello {
                    proto,
                    deployment,
                    program_id,
                    program_version,
                    peer,
                    directory,
                    restarts,
                    boot_nonce,
                    channels,
                })
            }
            T_HELLO_OK => {
                let accepted_version = u32::from_le_bytes(le(input, "accepted version")?);
                let n = get_varint(input)?;
                if n > input.len() as u64 {
                    return Err(WireError::Limit("sid count"));
                }
                let mut sids = Vec::new();
                for _ in 0..n {
                    sids.push(u32::try_from(get_varint(input)?).map_err(|_| WireError::Malformed("sid".into()))?);
                }
                Frame::HelloOk { accepted_version, sids }
            }
            T_REJECT => {
                let (&r, rest) = input.split_first().ok_or(WireError::Truncated("reason"))?;
                *input = rest;
                Frame::Reject {
                    reason: RejectReason::of(r)?,
                    detail: get_str(input)?,
                }
            }
            T_GOAWAY => {
                let (&reason, rest) = input.split_first().ok_or(WireError::Truncated("reason"))?;
                *input = rest;
                Frame::GoAway { reason }
            }
            T_BATCH => {
                let sid = u32::try_from(get_varint(input)?).map_err(|_| WireError::Malformed("sid".into()))?;
                let send_tick = get_varint(input)?;
                let (&kind, rest) = input.split_first().ok_or(WireError::Truncated("batch kind"))?;
                *input = rest;
                if kind != KIND_PLAIN {
                    return Err(WireError::Unsupported(format!("batch kind {kind}")));
                }
                let count = get_varint(input)?;
                if count > limits.max_tuples_per_batch as u64 || count > input.len() as u64 {
                    return Err(WireError::Limit("tuples per batch"));
                }
                let body = std::mem::take(input).to_vec();
                Frame::Batch(Batch {
                    sid,
                    send_tick,
                    kind,
                    count,
                    body,
                })
            }
            other => return Err(WireError::Malformed(format!("frame type {other:#x}"))),
        };
        if !input.is_empty() {
            return Err(WireError::Malformed(format!("{} trailing bytes in a frame", input.len())));
        }
        Ok(frame)
    }

    /// Reads one frame from a byte stream (blocking). `Ok(None)` at a clean end of stream.
    pub fn read(r: &mut dyn Read, limits: &WireLimits) -> Result<Option<Frame>, FrameIoError> {
        let mut len = [0u8; 4];
        match r.read_exact(&mut len) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(FrameIoError::Io(e)),
        }
        let len = u32::from_le_bytes(len) as usize;
        if len == 0 || len > limits.max_frame {
            return Err(FrameIoError::Wire(WireError::Limit("frame length")));
        }
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf).map_err(FrameIoError::Io)?;
        let (&t, body) = buf.split_first().ok_or(FrameIoError::Wire(WireError::Truncated("frame type")))?;
        Frame::decode(t, body, limits).map(Some).map_err(FrameIoError::Wire)
    }

    /// Writes one frame.
    pub fn write(&self, w: &mut dyn Write) -> std::io::Result<()> {
        w.write_all(&self.encode())
    }
}

/// Reading a frame from a stream failed.
#[derive(Debug, thiserror::Error)]
pub enum FrameIoError {
    #[error("i/o: {0}")]
    Io(std::io::Error),
    #[error(transparent)]
    Wire(WireError),
}
