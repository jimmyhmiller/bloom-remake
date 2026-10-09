//! Frames (ARCHITECTURE §5.4): `len:u32 type:u8 body`, little-endian, `len` excluding itself.
//!
//! ```text
//! HELLO    0x01 := magic:"BLSM" proto:u16 deployment:[16] program_id:[16] program_version:u32 peer directory:[16]
//!                  restarts:u64 boot_nonce:u64 n:varint (sid:varint name:str schema_hash:[16]){n}
//!          peer := 0 node:u32 | 1 principal:str                 (a node of the deployment, or a client session)
//!                | 2 role:str token:bytes received:u64 acked:u64  (a client member, CLIENTS.md §3; empty token: new)
//!                | 3 role:str token:bytes received:u64 acked:u64 keyed_role:str key:str
//!                                                     (a client member linking to a keyed member, KEYED.md)
//! HELLO_OK 0x02 := accepted_version:u32 n:varint (sid:varint){n}
//! REJECT   0x03 := reason:u8 detail:str
//! GOAWAY   0x04 := reason:u8
//! WELCOME  0x05 := member:u32 token:bytes resumed:u8 floor:u64 seed:[16]  (to a client member, after HELLO_OK)
//! BATCH    0x10 := sid:varint send_tick:varint kind:u8 count:varint tuple{count}
//! MSG      0x11 := seq:varint BATCH-body                         (a batch on a member link, numbered)
//! ACK      0x12 := seq:varint                                    (everything up to seq was taken)
//! MEMBER   0x13 := role:u32 key:str BATCH-body                    (a batch a keyed member sent, from its host)
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
const T_WELCOME: u8 = 0x05;
const T_BATCH: u8 = 0x10;
const T_MSG: u8 = 0x11;
const T_ACK: u8 = 0x12;
const T_MEMBER: u8 = 0x13;

/// Who opened a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Peer {
    /// A node of the deployment, by its dense id.
    Node(u32),
    /// A client session, with the principal it claims.
    Client { principal: String },
    /// A member of a client role (docs/design/CLIENTS.md §3): its role, the digest of the part of the program it runs
    /// (§8: a server refuses a page built from another program), its token (`None` the first time), the sequence
    /// number of the last message it took from the server, and of the last of its own the server acknowledged; and
    /// the keyed member it links to, if its server is one (docs/design/KEYED.md): the role's name and the key.
    Member {
        role: String,
        part: [u8; 16],
        token: Option<Vec<u8>>,
        received: u64,
        acked: u64,
        keyed: Option<(String, String)>,
    },
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
    HelloOk {
        accepted_version: u32,
        sids: Vec<u32>,
    },
    Reject {
        reason: RejectReason,
        detail: String,
    },
    GoAway {
        reason: u8,
    },
    Batch(Batch),
    /// The server's answer to a member's `HELLO` (after `HELLO_OK`): its identity, whether the link takes up where
    /// the last one left off, the last of the member's messages the server holds (the member resends the rest), and
    /// the member's own root seed (derived from the deployment's, which a member never sees).
    Welcome {
        member: u32,
        token: Vec<u8>,
        resumed: bool,
        floor: u64,
        seed: [u8; 16],
    },
    /// A batch on a member link, with its sequence number on that direction.
    Msg {
        seq: u64,
        batch: Batch,
    },
    /// Everything up to `seq` on that direction of a member link was taken.
    Ack {
        seq: u64,
    },
    /// A batch a keyed member sent (docs/design/KEYED.md), on its host's link: the member's role (its id in the
    /// program both ends checked) and key.
    FromMember {
        role: u32,
        key: String,
        batch: Batch,
    },
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn get_bytes(input: &mut &[u8]) -> Result<Vec<u8>, WireError> {
    let n = usize::try_from(get_varint(input)?).map_err(|_| WireError::Limit("byte string length"))?;
    Ok(take(input, n, "a byte string")?.to_vec())
}

fn put_batch(out: &mut Vec<u8>, b: &Batch) {
    put_varint(out, u64::from(b.sid));
    put_varint(out, b.send_tick);
    out.push(b.kind);
    put_varint(out, b.count);
    out.extend_from_slice(&b.body);
}

fn get_batch(input: &mut &[u8], limits: &WireLimits) -> Result<Batch, WireError> {
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
    Ok(Batch {
        sid,
        send_tick,
        kind,
        count,
        body,
    })
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
    take(input, 16, what)?
        .try_into()
        .map_err(|_| WireError::Truncated(what))
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
                    Peer::Member {
                        role,
                        part,
                        token,
                        received,
                        acked,
                        keyed,
                    } => {
                        body.push(if keyed.is_some() { 3 } else { 2 });
                        put_str(&mut body, role);
                        body.extend_from_slice(part);
                        put_bytes(&mut body, token.as_deref().unwrap_or(&[]));
                        body.extend_from_slice(&received.to_le_bytes());
                        body.extend_from_slice(&acked.to_le_bytes());
                        if let Some((r, k)) = keyed {
                            put_str(&mut body, r);
                            put_str(&mut body, k);
                        }
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
                put_batch(&mut body, b);
                T_BATCH
            }
            Frame::Welcome {
                member,
                token,
                resumed,
                floor,
                seed,
            } => {
                body.extend_from_slice(&member.to_le_bytes());
                put_bytes(&mut body, token);
                body.push(u8::from(*resumed));
                body.extend_from_slice(&floor.to_le_bytes());
                body.extend_from_slice(seed);
                T_WELCOME
            }
            Frame::Msg { seq, batch } => {
                put_varint(&mut body, *seq);
                put_batch(&mut body, batch);
                T_MSG
            }
            Frame::Ack { seq } => {
                put_varint(&mut body, *seq);
                T_ACK
            }
            Frame::FromMember { role, key, batch } => {
                body.extend_from_slice(&role.to_le_bytes());
                put_str(&mut body, key);
                put_batch(&mut body, batch);
                T_MEMBER
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
                    kind @ (2 | 3) => {
                        let role = get_str(input)?;
                        let part = arr16(input, "program part")?;
                        let token = get_bytes(input)?;
                        let received = u64::from_le_bytes(le(input, "received")?);
                        let acked = u64::from_le_bytes(le(input, "acked")?);
                        let keyed = if kind == 3 {
                            Some((get_str(input)?, get_str(input)?))
                        } else {
                            None
                        };
                        Peer::Member {
                            role,
                            part,
                            token: (!token.is_empty()).then_some(token),
                            received,
                            acked,
                            keyed,
                        }
                    }
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
            T_BATCH => Frame::Batch(get_batch(input, limits)?),
            T_WELCOME => {
                let member = u32::from_le_bytes(le(input, "member")?);
                let token = get_bytes(input)?;
                let (&resumed, rest) = input.split_first().ok_or(WireError::Truncated("resumed"))?;
                *input = rest;
                Frame::Welcome {
                    member,
                    token,
                    resumed: resumed != 0,
                    floor: u64::from_le_bytes(le(input, "floor")?),
                    seed: arr16(input, "seed")?,
                }
            }
            T_MSG => {
                let seq = get_varint(input)?;
                Frame::Msg {
                    seq,
                    batch: get_batch(input, limits)?,
                }
            }
            T_ACK => Frame::Ack {
                seq: get_varint(input)?,
            },
            T_MEMBER => {
                let role = u32::from_le_bytes(le(input, "a member's role")?);
                let key = get_str(input)?;
                Frame::FromMember {
                    role,
                    key,
                    batch: get_batch(input, limits)?,
                }
            }
            other => return Err(WireError::Malformed(format!("frame type {other:#x}"))),
        };
        if !input.is_empty() {
            return Err(WireError::Malformed(format!(
                "{} trailing bytes in a frame",
                input.len()
            )));
        }
        Ok(frame)
    }

    /// Reads one frame from a byte stream (blocking). `Ok(None)` at a clean end of stream.
    pub fn read(r: &mut dyn Read, limits: &WireLimits) -> Result<Option<Frame>, FrameIoError> {
        let mut len = [0u8; 4];
        // A clean end of stream is one before the first byte of a frame; an end inside a frame is an error.
        let mut got = 0;
        while got < len.len() {
            let Some(rest) = len.get_mut(got..) else {
                break;
            };
            match r.read(rest) {
                Ok(0) if got == 0 => return Ok(None),
                Ok(0) => return Err(FrameIoError::Io(std::io::ErrorKind::UnexpectedEof.into())),
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(FrameIoError::Io(e)),
            }
        }
        let len = u32::from_le_bytes(len) as usize;
        if len == 0 || len > limits.max_frame {
            return Err(FrameIoError::Wire(WireError::Limit("frame length")));
        }
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf).map_err(FrameIoError::Io)?;
        let (&t, body) = buf
            .split_first()
            .ok_or(FrameIoError::Wire(WireError::Truncated("frame type")))?;
        Frame::decode(t, body, limits).map(Some).map_err(FrameIoError::Wire)
    }

    /// Decodes the first whole frame of `buf`, if it holds one: the frame and the bytes it used.
    pub fn parse(buf: &[u8], limits: &WireLimits) -> Result<Option<(Frame, usize)>, WireError> {
        let Some(len) = buf.get(..4) else {
            return Ok(None);
        };
        let len = u32::from_le_bytes(len.try_into().map_err(|_| WireError::Truncated("frame length"))?) as usize;
        if len == 0 || len > limits.max_frame {
            return Err(WireError::Limit("frame length"));
        }
        let Some(frame) = buf.get(4..4 + len) else {
            return Ok(None);
        };
        let (&t, body) = frame.split_first().ok_or(WireError::Truncated("frame type"))?;
        Ok(Some((Frame::decode(t, body, limits)?, 4 + len)))
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
