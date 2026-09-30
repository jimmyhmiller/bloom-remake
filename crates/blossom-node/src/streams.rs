//! Byte streams at a node (FOREIGN-PROTOCOLS §1): the part of the runtime's work that is logic, not sockets, shared
//! by the real runtime and the simulators.
//!
//! - [`StreamInbox`] turns what the host observed on each connection (opened, bytes read, closed; a failed dial)
//!   into the stream events of each tick, keeping the order the language promises (§1.2a):
//!   - a connection's `opened` is in an earlier tick than any of its `data`;
//!   - at most one `data` per connection per tick, holding everything read since the last one, up to the budget;
//!   - its `closed` is in a later tick than its last `data`.
//! - [`SeqWriter`] orders one connection's writes by the program's `seq`: contiguous from 0, a gap holding back
//!   later writes, a duplicate or far-off `seq` a violation that closes the connection.
//! - [`host_request`] decodes a row of a stream's `write`, `close` or `dial`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use blossom_base::RoleId;
use blossom_base::{RelId, internal_error};
use blossom_ir::core::{HostOp, Placement, Program, StreamKind};
use blossom_ir::tick::{HostOut, Row};
use blossom_value::Value;
use blossom_value::time::Instant;
use blossom_value::value::{ConnId, IntValue};

use crate::NodeError;

/// One stream of the program placed at this node, by its relations.
#[derive(Clone, Debug)]
pub struct NodeStream {
    pub name: Arc<str>,
    pub kind: StreamKind,
    pub opened: RelId,
    pub data: RelId,
    pub closed: RelId,
    pub failed: Option<RelId>,
    pub write: RelId,
    pub close: RelId,
    pub dial: Option<RelId>,
}

/// The streams of `program` that run at a node of role `role` (shared streams run everywhere).
pub fn node_streams(program: &Program, role: Option<RoleId>) -> Vec<NodeStream> {
    program
        .streams
        .iter()
        .filter(|s| match s.placement {
            Placement::Shared => true,
            Placement::Role(r) => Some(r) == role,
        })
        .map(|s| NodeStream {
            name: Arc::from(s.name.to_string()),
            kind: s.kind,
            opened: s.opened,
            data: s.data,
            closed: s.closed,
            failed: s.failed,
            write: s.write,
            close: s.close,
            dial: s.dial,
        })
        .collect()
}

/// What the host observed on a stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observed {
    /// A connection was accepted (listen) or established (connect, answering dial `req`).
    Opened {
        stream: usize,
        conn: ConnId,
        peer: Arc<str>,
        req: Option<u64>,
        at: Instant,
    },
    /// Bytes read from `conn`.
    Bytes { conn: ConnId, bytes: Vec<u8> },
    /// `conn` ended: the peer closed it, a read or write failed, or the program closed it.
    Closed { conn: ConnId, reason: Arc<str> },
    /// Dial `req` of a connect stream did not connect.
    Failed { stream: usize, req: u64, reason: Arc<str> },
}

#[derive(Debug)]
enum Phase {
    /// Accepted; `opened` not yet delivered.
    New {
        peer: Arc<str>,
        req: Option<u64>,
        at: Instant,
    },
    /// `opened` was delivered in tick `.0`.
    Open(u64),
}

#[derive(Debug)]
struct ConnIn {
    stream: usize,
    phase: Phase,
    pending: VecDeque<u8>,
    next_seq: u64,
    closing: Option<Arc<str>>,
    /// The last tick a `data` was delivered in.
    last_data: Option<u64>,
}

/// The stream events waiting for ticks at one node.
#[derive(Debug)]
pub struct StreamInbox {
    streams: Vec<NodeStream>,
    conns: BTreeMap<ConnId, ConnIn>,
    failed: VecDeque<(usize, u64, Arc<str>)>,
    /// The most bytes one connection's `data` carries in one tick (`max_stream_bytes`).
    budget: usize,
    /// The connections whose `closed` the last [`StreamInbox::take`] delivered.
    retired: Vec<ConnId>,
}

impl StreamInbox {
    pub fn new(streams: Vec<NodeStream>, budget: usize) -> StreamInbox {
        StreamInbox {
            streams,
            conns: BTreeMap::new(),
            failed: VecDeque::new(),
            budget: budget.max(1),
            retired: Vec::new(),
        }
    }

    pub fn streams(&self) -> &[NodeStream] {
        &self.streams
    }

    /// Records what the host observed. A connection the inbox does not know (bytes or a close after it forgot it)
    /// is a bug of the host.
    pub fn observe(&mut self, o: Observed) -> Result<(), NodeError> {
        match o {
            Observed::Opened {
                stream,
                conn,
                peer,
                req,
                at,
            } => {
                let Some(st) = self.streams.get(stream) else {
                    return Err(
                        internal_error!("a connection of stream {stream}, which this node does not run").into(),
                    );
                };
                if (st.kind == StreamKind::Connect) != req.is_some() {
                    return Err(internal_error!("a connection of stream {} without its dial request", st.name).into());
                }
                if self.conns.contains_key(&conn) {
                    return Err(internal_error!("connection {conn:?} opened twice").into());
                }
                self.conns.insert(
                    conn,
                    ConnIn {
                        stream,
                        phase: Phase::New { peer, req, at },
                        pending: VecDeque::new(),
                        next_seq: 0,
                        closing: None,
                        last_data: None,
                    },
                );
            }
            Observed::Bytes { conn, bytes } => {
                let c = self
                    .conns
                    .get_mut(&conn)
                    .ok_or_else(|| internal_error!("bytes of unknown connection {conn:?}"))?;
                if c.closing.is_some() {
                    return Err(internal_error!("bytes of connection {conn:?} after it closed").into());
                }
                c.pending.extend(bytes);
            }
            Observed::Closed { conn, reason } => {
                let c = self
                    .conns
                    .get_mut(&conn)
                    .ok_or_else(|| internal_error!("the close of unknown connection {conn:?}"))?;
                c.closing.get_or_insert(reason);
            }
            Observed::Failed { stream, req, reason } => {
                match self.streams.get(stream) {
                    Some(s) if s.failed.is_some() => {}
                    _ => return Err(internal_error!("a failed dial of stream {stream}, not a connect stream").into()),
                }
                self.failed.push_back((stream, req, reason));
            }
        }
        Ok(())
    }

    /// Whether a tick would take a stream event now.
    pub fn has_events(&self, tick: u64) -> bool {
        !self.failed.is_empty()
            || self.conns.values().any(|c| match c.phase {
                Phase::New { .. } => true,
                Phase::Open(t) => t < tick && (!c.pending.is_empty() || c.closing.is_some()),
            })
    }

    /// Whether any event is waiting, now or for a later tick.
    pub fn has_pending(&self) -> bool {
        !self.failed.is_empty()
            || self
                .conns
                .values()
                .any(|c| matches!(c.phase, Phase::New { .. }) || !c.pending.is_empty() || c.closing.is_some())
    }

    /// The bytes read but not yet delivered, over every connection (the host stops reading past a limit).
    pub fn backlog(&self) -> usize {
        self.conns.values().map(|c| c.pending.len()).sum()
    }

    /// The stream events of tick `tick`, in connection order.
    pub fn take(&mut self, tick: u64) -> Result<Vec<(RelId, Row)>, NodeError> {
        let mut out = Vec::new();
        while let Some((stream, req, reason)) = self.failed.pop_front() {
            let rel = self
                .streams
                .get(stream)
                .and_then(|s| s.failed)
                .ok_or_else(|| internal_error!("a failed dial of a stream without `failed`"))?;
            out.push((rel, row(vec![u64v(req), Value::Str(reason)])));
        }
        let mut gone = Vec::new();
        for (conn, c) in self.conns.iter_mut() {
            let st = self
                .streams
                .get(c.stream)
                .ok_or_else(|| internal_error!("a connection of an unknown stream"))?;
            match &c.phase {
                Phase::New { peer, req, at } => {
                    let mut cols = vec![Value::Conn(*conn)];
                    if let Some(r) = req {
                        cols.push(u64v(*r));
                    }
                    cols.push(Value::Str(peer.clone()));
                    cols.push(Value::Instant(*at));
                    out.push((st.opened, row(cols)));
                    c.phase = Phase::Open(tick);
                }
                Phase::Open(opened) if *opened < tick => {
                    if !c.pending.is_empty() {
                        let n = c.pending.len().min(self.budget);
                        let chunk: Vec<u8> = c.pending.drain(..n).collect();
                        out.push((
                            st.data,
                            row(vec![Value::Conn(*conn), u64v(c.next_seq), Value::Bytes(chunk.into())]),
                        ));
                        c.next_seq += 1;
                        c.last_data = Some(tick);
                    } else if let Some(reason) = &c.closing
                        && c.last_data.is_none_or(|t| t < tick)
                    {
                        out.push((st.closed, row(vec![Value::Conn(*conn), Value::Str(reason.clone())])));
                        gone.push(*conn);
                    }
                }
                Phase::Open(_) => {}
            }
        }
        for c in &gone {
            self.conns.remove(c);
        }
        self.retired = gone;
        Ok(out)
    }

    /// The connections whose `closed` event the last [`StreamInbox::take`] delivered: once that tick is released,
    /// after its writes, the host closes them.
    pub fn take_retired(&mut self) -> Vec<ConnId> {
        std::mem::take(&mut self.retired)
    }

    /// Forgets every connection (a crash closes them all; a restarted node begins with none).
    pub fn clear(&mut self) {
        self.conns.clear();
        self.failed.clear();
    }
}

fn row(cols: Vec<Value>) -> Row {
    Arc::from(cols)
}

fn u64v(n: u64) -> Value {
    Value::Int(IntValue::U64(n))
}

/// The greatest distance ahead of the next expected `seq` a write may be held at.
pub const MAX_SEQ_GAP: u64 = 4096;

/// One connection's writes in `seq` order.
#[derive(Debug, Default)]
pub struct SeqWriter {
    next: u64,
    held: BTreeMap<u64, Vec<u8>>,
}

impl SeqWriter {
    /// Accepts write `seq`: the bytes that may now be written, in order, or why the connection must close (a
    /// duplicate `seq`, or one beyond [`MAX_SEQ_GAP`]).
    pub fn accept(&mut self, seq: u64, bytes: Vec<u8>) -> Result<Vec<Vec<u8>>, String> {
        if seq < self.next || self.held.contains_key(&seq) {
            return Err(format!("write seq {seq} was already written"));
        }
        if seq - self.next > MAX_SEQ_GAP {
            return Err(format!(
                "write seq {seq} is more than {MAX_SEQ_GAP} past the next expected, {}",
                self.next
            ));
        }
        self.held.insert(seq, bytes);
        let mut ready = Vec::new();
        while let Some(b) = self.held.remove(&self.next) {
            ready.push(b);
            self.next += 1;
        }
        Ok(ready)
    }

    /// The writes held back by a gap.
    pub fn held(&self) -> usize {
        self.held.len()
    }
}

/// A request to the host, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostRequest {
    /// A write, through the node's stream `stream` (the host refuses it if `conn` is another stream's).
    Write {
        stream: usize,
        conn: ConnId,
        seq: u64,
        bytes: Vec<u8>,
    },
    /// A close, through the node's stream `stream`.
    Close {
        stream: usize,
        conn: ConnId,
    },
    /// A write the host must refuse, a located runtime error of the program (a blob range outside the blob): the
    /// connection closes and its `closed` event carries `why`, as for a bad `seq`.
    Refused {
        stream: usize,
        conn: ConnId,
        why: String,
    },
    Dial {
        stream: usize,
        req: u64,
        addr: Arc<str>,
    },
}

/// Decodes a released host row against the node's streams.
pub fn host_request(
    streams: &[NodeStream],
    h: &HostOut,
    blobs: &dyn blossom_value::BlobSource,
) -> Result<HostRequest, NodeError> {
    let (i, st) = streams
        .iter()
        .enumerate()
        .find(|(_, s)| s.write == h.rel || s.close == h.rel || s.dial == Some(h.rel))
        .ok_or_else(|| internal_error!("a host request {:?} of no stream of this node", h.rel))?;
    let op = if st.write == h.rel {
        HostOp::Write
    } else if st.close == h.rel {
        HostOp::Close
    } else {
        HostOp::Dial
    };
    let bad = || internal_error!("a malformed {op:?} row {:?}", h.row);
    Ok(match op {
        HostOp::Write => {
            let [Value::Conn(conn), Value::Int(IntValue::U64(seq)), Value::Vec(parts)] = &*h.row else {
                return Err(bad().into());
            };
            let mut bytes = Vec::new();
            for p in parts.iter() {
                match p {
                    // `Part::Bytes(b)` (variant 0 of the built-in `Part`).
                    Value::Enum { variant: 0, fields } => match &**fields {
                        [Value::Bytes(b)] => bytes.extend_from_slice(b),
                        _ => return Err(bad().into()),
                    },
                    // `Part::Blob(b, lo, hi)`: bytes of a stored blob. A range outside it is the program's error,
                    // refused like a bad `seq`; a missing blob is a host bug.
                    Value::Enum { variant: 1, fields } => match &**fields {
                        [Value::Blob(r), Value::Int(IntValue::U64(lo)), Value::Int(IntValue::U64(hi))] => {
                            let b = blobs
                                .get(r)
                                .ok_or_else(|| internal_error!("the bytes of blob {} are not available", r.hex()))?;
                            let part = usize::try_from(*lo)
                                .ok()
                                .zip(usize::try_from(*hi).ok())
                                .filter(|(lo, hi)| lo <= hi)
                                .and_then(|(lo, hi)| b.get(lo..hi));
                            match part {
                                Some(p) => bytes.extend_from_slice(p),
                                None => {
                                    return Ok(HostRequest::Refused {
                                        stream: i,
                                        conn: *conn,
                                        why: format!("a write of bytes {lo}..{hi} of a {}-byte blob", b.len()),
                                    });
                                }
                            }
                        }
                        _ => return Err(bad().into()),
                    },
                    _ => return Err(bad().into()),
                }
            }
            HostRequest::Write {
                stream: i,
                conn: *conn,
                seq: *seq,
                bytes,
            }
        }
        HostOp::Close => {
            let [Value::Conn(conn)] = &*h.row else {
                return Err(bad().into());
            };
            HostRequest::Close { stream: i, conn: *conn }
        }
        HostOp::Dial => {
            let [Value::Int(IntValue::U64(req)), Value::Str(addr)] = &*h.row else {
                return Err(bad().into());
            };
            HostRequest::Dial {
                stream: i,
                req: *req,
                addr: addr.clone(),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream() -> NodeStream {
        NodeStream {
            name: "s".into(),
            kind: StreamKind::Listen,
            opened: RelId::from_raw(0),
            data: RelId::from_raw(1),
            closed: RelId::from_raw(2),
            failed: None,
            write: RelId::from_raw(3),
            close: RelId::from_raw(4),
            dial: None,
        }
    }

    fn open(i: &mut StreamInbox, c: u64) {
        i.observe(Observed::Opened {
            stream: 0,
            conn: ConnId(c),
            peer: "p".into(),
            req: None,
            at: Instant(0),
        })
        .unwrap();
    }

    fn rels(events: &[(RelId, Row)]) -> Vec<u32> {
        events.iter().map(|(r, _)| r.raw()).collect()
    }

    #[test]
    fn opened_data_and_closed_come_in_separate_ticks_in_order() {
        let mut i = StreamInbox::new(vec![stream()], 4);
        open(&mut i, 1);
        i.observe(Observed::Bytes {
            conn: ConnId(1),
            bytes: b"abcdef".to_vec(),
        })
        .unwrap();
        i.observe(Observed::Closed {
            conn: ConnId(1),
            reason: "eof".into(),
        })
        .unwrap();
        assert!(i.has_events(1));
        assert_eq!(rels(&i.take(1).unwrap()), vec![0], "opened alone");
        // The same tick again delivers nothing: data waits for a later tick.
        assert!(!i.has_events(1));
        let t2 = i.take(2).unwrap();
        assert_eq!(rels(&t2), vec![1]);
        assert_eq!(
            t2[0].1[2],
            Value::Bytes(b"abcd".to_vec().into()),
            "the budget caps a chunk"
        );
        let t3 = i.take(3).unwrap();
        assert_eq!(
            (t3[0].1[1].clone(), t3[0].1[2].clone()),
            (u64v(1), Value::Bytes(b"ef".to_vec().into()))
        );
        assert_eq!(rels(&i.take(4).unwrap()), vec![2], "closed after the last data");
        assert!(!i.has_pending());
        assert!(i.take(5).unwrap().is_empty());
    }

    #[test]
    fn bytes_read_between_ticks_coalesce_into_one_chunk() {
        let mut i = StreamInbox::new(vec![stream()], 1 << 20);
        open(&mut i, 9);
        i.take(1).unwrap();
        for b in [b"he".as_slice(), b"ll", b"o"] {
            i.observe(Observed::Bytes {
                conn: ConnId(9),
                bytes: b.to_vec(),
            })
            .unwrap();
        }
        let t = i.take(2).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].1[2], Value::Bytes(b"hello".to_vec().into()));
        assert_eq!(i.backlog(), 0);
    }

    #[test]
    fn the_seq_writer_orders_holds_gaps_and_refuses_duplicates() {
        let mut w = SeqWriter::default();
        assert_eq!(
            w.accept(1, b"b".to_vec()).unwrap(),
            Vec::<Vec<u8>>::new(),
            "held behind the gap"
        );
        assert_eq!(w.held(), 1);
        assert_eq!(w.accept(0, b"a".to_vec()).unwrap(), vec![b"a".to_vec(), b"b".to_vec()]);
        assert!(w.accept(1, b"x".to_vec()).is_err(), "a duplicate");
        assert!(w.accept(3 + MAX_SEQ_GAP, vec![]).is_err(), "too far ahead");
        assert_eq!(w.accept(2, b"c".to_vec()).unwrap(), vec![b"c".to_vec()]);
    }
}
