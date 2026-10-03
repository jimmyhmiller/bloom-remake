//! One node's input trace (ARCHITECTURE §6.4, TEST-010): everything one incarnation of a node read, tick by tick,
//! so that a run of a real `blossom run` process can be replayed exactly, in-process, and questioned (`blossom
//! trace`, `why`, `whynot`).
//!
//! A tick is a pure function of its [`StepInput`](blossom_ir::tick::StepInput), the carried state it starts from,
//! the deployment's seed (every `rand` draw is a PRF of the seed, the node, the incarnation and the tick) and the
//! bytes of the blobs it reads. An incarnation starts from its durable rows. So a trace holds:
//! - a header naming the program (by digest), the deployment's nodes, the node, its incarnation and the seed;
//! - [`NodeRecord::Boot`]: the durable rows the incarnation started from;
//! - per tick, [`NodeRecord::Tick`] (its inputs, written before the tick runs, so a tick that fails is in the trace),
//!   then a [`NodeRecord::Blob`] for each blob it read that the trace does not hold yet and no earlier tick of the
//!   incarnation created, then [`NodeRecord::Outcome`] (a digest of what the tick changed and sent, which a replay
//!   must reproduce) or [`NodeRecord::Failed`].
//!
//! The file holds the deployment's seed: it is created readable by its owner only.
//!
//! Format: the 8 bytes `BLSTRACE`, then length-prefixed records (a little-endian `u32` length, then the postcard
//! encoding): the header first. A process killed mid-write leaves a torn last record, which a reader reports as the
//! end of the trace ([`TraceReader::torn`]).

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::sync::Arc;

use blossom_base::RelId;
use blossom_ir::tick::{Changes, Delivery, Egress, HostOut, Ingress, Row, Send};
use blossom_value::BlobRef;
use blossom_value::time::{Instant, NodeId, Tick};
use serde::{Deserialize, Serialize};

/// The trace format's version: a reader refuses any other.
pub const FORMAT: u16 = 1;

const MAGIC: [u8; 8] = *b"BLSTRACE";

/// The largest record a reader accepts (a tick's inputs, or a blob: the runtime's blobs are at most 1 GiB).
const MAX_RECORD: u32 = 1 << 30;

/// What a trace is of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeTraceHeader {
    pub format: u16,
    /// The program's name, version and digest ([`ValidatedProgram::digest`](blossom_ir::ValidatedProgram::digest)):
    /// a replay of another program is refused.
    pub program: Arc<str>,
    pub version: u32,
    pub digest: [u8; 32],
    /// The deployment's nodes, in their dense order, and the node traced.
    pub nodes: Vec<Arc<str>>,
    pub node: NodeId,
    pub incarnation: u64,
    /// The deployment's root seed.
    pub seed: [u8; 16],
}

/// One record after the header.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeRecord {
    /// The carried state the incarnation started from (its recovered durable rows), by relation.
    Boot { image: Vec<(RelId, Vec<Row>)> },
    /// A tick's inputs.
    Tick {
        tick: Tick,
        now: Instant,
        events: Vec<(RelId, Row)>,
        delivered: Vec<Delivery>,
        ingress: Vec<Ingress>,
    },
    /// The bytes of a blob the tick before read.
    Blob { blob: BlobRef, bytes: Vec<u8> },
    /// What the tick before changed and sent, as [`outcome_digest`].
    Outcome { tick: Tick, digest: [u8; 32] },
    /// The tick before failed with this error (the node faulted).
    Failed { tick: Tick, error: String },
}

/// A trace that cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("reading the trace: {0}")]
    Io(#[from] io::Error),
    #[error("not a Blossom trace (no `BLSTRACE` at its start)")]
    NotATrace,
    #[error("the trace is of format {found}; this build reads format {FORMAT}")]
    Format { found: u16 },
    #[error("a record of {0} bytes, more than a trace holds")]
    TooLarge(u32),
    #[error("a malformed record: {0}")]
    Malformed(String),
}

/// Writes a trace.
pub struct TraceWriter<W: Write> {
    out: W,
}

impl<W: Write> TraceWriter<W> {
    /// Starts a trace on `out` with `header`.
    pub fn new(mut out: W, header: &NodeTraceHeader) -> io::Result<TraceWriter<W>> {
        out.write_all(&MAGIC)?;
        let mut w = TraceWriter { out };
        w.frame(header)?;
        Ok(w)
    }

    pub fn record(&mut self, r: &NodeRecord) -> io::Result<()> {
        self.frame(r)
    }

    /// Hands what was written to the operating system (so it survives the process being killed).
    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    fn frame<T: Serialize>(&mut self, value: &T) -> io::Result<()> {
        let bytes = postcard::to_allocvec(value).map_err(|e| io::Error::other(format!("encoding a record: {e}")))?;
        let len = u32::try_from(bytes.len())
            .ok()
            .filter(|n| *n <= MAX_RECORD)
            .ok_or_else(|| io::Error::other(format!("a record of {} bytes is too large for a trace", bytes.len())))?;
        self.out.write_all(&len.to_le_bytes())?;
        self.out.write_all(&bytes)
    }
}

/// Reads a trace.
pub struct TraceReader<R: Read> {
    inp: R,
    header: NodeTraceHeader,
    torn: bool,
}

impl<R: Read> TraceReader<R> {
    /// Opens a trace: checks its magic and format and reads its header.
    pub fn open(mut inp: R) -> Result<TraceReader<R>, TraceError> {
        let mut magic = [0u8; 8];
        if read_full(&mut inp, &mut magic)? != magic.len() || magic != MAGIC {
            return Err(TraceError::NotATrace);
        }
        let header: NodeTraceHeader = match frame(&mut inp)? {
            Frame::Record(bytes) => postcard::from_bytes(&bytes).map_err(|e| TraceError::Malformed(e.to_string()))?,
            Frame::End | Frame::Torn => return Err(TraceError::Malformed("a trace without its header".into())),
        };
        if header.format != FORMAT {
            return Err(TraceError::Format { found: header.format });
        }
        Ok(TraceReader {
            inp,
            header,
            torn: false,
        })
    }

    pub fn header(&self) -> &NodeTraceHeader {
        &self.header
    }

    /// The next record, or `None` at the end of the trace.
    pub fn next_record(&mut self) -> Result<Option<NodeRecord>, TraceError> {
        if self.torn {
            return Ok(None);
        }
        match frame(&mut self.inp)? {
            Frame::Record(bytes) => postcard::from_bytes(&bytes)
                .map(Some)
                .map_err(|e| TraceError::Malformed(e.to_string())),
            Frame::End => Ok(None),
            Frame::Torn => {
                self.torn = true;
                Ok(None)
            }
        }
    }

    /// Whether the trace ended inside a record (the process that wrote it was killed mid-write).
    pub fn torn(&self) -> bool {
        self.torn
    }
}

enum Frame {
    Record(Vec<u8>),
    End,
    Torn,
}

fn frame<R: Read>(inp: &mut R) -> Result<Frame, TraceError> {
    let mut len = [0u8; 4];
    match read_full(inp, &mut len)? {
        0 => return Ok(Frame::End),
        4 => {}
        _ => return Ok(Frame::Torn),
    }
    let n = u32::from_le_bytes(len);
    if n > MAX_RECORD {
        return Err(TraceError::TooLarge(n));
    }
    let mut bytes = vec![0u8; n as usize];
    if read_full(inp, &mut bytes)? != bytes.len() {
        return Ok(Frame::Torn);
    }
    Ok(Frame::Record(bytes))
}

/// Reads until `buf` is full or the input ends; the bytes read.
fn read_full<R: Read>(inp: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        let Some(rest) = buf.get_mut(got..) else { break };
        match inp.read(rest) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

/// The digest of what a tick did: its changes to the carried state, its sends, its replies to sessions and its
/// requests to the host, each in canonical order (so two evaluators that agree on the tick agree on the digest,
/// whatever order they produce them in).
pub fn outcome_digest<'a>(
    changes: &Changes,
    outbox: impl IntoIterator<Item = &'a Send>,
    egress: impl IntoIterator<Item = &'a Egress>,
    host: impl IntoIterator<Item = &'a HostOut>,
) -> [u8; 32] {
    let sorted = |m: &BTreeMap<RelId, Vec<Row>>| -> Vec<(RelId, Vec<Row>)> {
        m.iter()
            .filter(|(_, rows)| !rows.is_empty())
            .map(|(r, rows)| {
                let mut rows = rows.clone();
                rows.sort();
                (*r, rows)
            })
            .collect()
    };
    let mut sends: Vec<(RelId, NodeId, Row)> = outbox.into_iter().map(|s| (s.rel, s.to, s.row.clone())).collect();
    sends.sort();
    let mut replies: Vec<(RelId, u64, Row)> = egress
        .into_iter()
        .map(|e| (e.rel, e.session.0, e.row.clone()))
        .collect();
    replies.sort();
    let mut requests: Vec<(RelId, Row)> = host.into_iter().map(|h| (h.rel, h.row.clone())).collect();
    requests.sort();
    let canonical = (
        sorted(&changes.inserted),
        sorted(&changes.deleted),
        sends,
        replies,
        requests,
    );
    let mut hasher = blake3::Hasher::new();
    // Encoding canonical values cannot fail; if it ever did, the digest would differ from any recorded one and the
    // replay would report the tick, never pass silently.
    match postcard::to_allocvec(&canonical) {
        Ok(bytes) => {
            hasher.update(&bytes);
        }
        Err(e) => {
            hasher.update(b"unencodable outcome: ");
            hasher.update(e.to_string().as_bytes());
        }
    }
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use blossom_value::Value;

    fn header() -> NodeTraceHeader {
        NodeTraceHeader {
            format: FORMAT,
            program: "p".into(),
            version: 1,
            digest: [7; 32],
            nodes: vec!["a".into(), "b".into()],
            node: NodeId(1),
            incarnation: 3,
            seed: [9; 16],
        }
    }

    fn tick(t: u64) -> NodeRecord {
        NodeRecord::Tick {
            tick: Tick(t),
            now: Instant(t as i64 * 10),
            events: vec![(RelId::from_raw(2), Arc::from(vec![Value::Bool(true)]))],
            delivered: vec![],
            ingress: vec![],
        }
    }

    #[test]
    fn a_trace_reads_back_what_was_written() {
        let mut w = TraceWriter::new(Vec::new(), &header()).unwrap();
        let records = vec![
            NodeRecord::Boot { image: vec![] },
            tick(1),
            NodeRecord::Blob {
                blob: BlobRef::of(b"x"),
                bytes: b"x".to_vec(),
            },
            NodeRecord::Outcome {
                tick: Tick(1),
                digest: [1; 32],
            },
        ];
        for r in &records {
            w.record(r).unwrap();
        }
        let bytes = w.out;
        let mut r = TraceReader::open(&bytes[..]).unwrap();
        assert_eq!(r.header(), &header());
        let mut got = Vec::new();
        while let Some(x) = r.next_record().unwrap() {
            got.push(x);
        }
        assert_eq!(got, records);
        assert!(!r.torn());
    }

    /// A process killed mid-write leaves a torn last record: the reader stops before it, and says so.
    #[test]
    fn a_torn_tail_ends_the_trace() {
        let mut w = TraceWriter::new(Vec::new(), &header()).unwrap();
        w.record(&tick(1)).unwrap();
        w.record(&tick(2)).unwrap();
        let mut bytes = w.out;
        bytes.truncate(bytes.len() - 3);
        let mut r = TraceReader::open(&bytes[..]).unwrap();
        assert_eq!(r.next_record().unwrap(), Some(tick(1)));
        assert_eq!(r.next_record().unwrap(), None);
        assert!(r.torn());
    }

    #[test]
    fn another_format_or_file_is_refused() {
        assert!(matches!(
            TraceReader::open(&b"NOTATRACE"[..]),
            Err(TraceError::NotATrace)
        ));
        let mut h = header();
        h.format = FORMAT + 1;
        let w = TraceWriter::new(Vec::new(), &h).unwrap();
        assert!(matches!(TraceReader::open(&w.out[..]), Err(TraceError::Format { .. })));
    }

    /// The outcome digest does not depend on the order an evaluator produced sends and rows in.
    #[test]
    fn the_outcome_digest_is_canonical() {
        let row = |x: i64| -> Row { Arc::from(vec![Value::Int(blossom_value::value::IntValue::I64(x))]) };
        let rel = RelId::from_raw(4);
        let a = Changes {
            inserted: [(rel, vec![row(1), row(2)])].into_iter().collect(),
            deleted: BTreeMap::new(),
        };
        let b = Changes {
            inserted: [(rel, vec![row(2), row(1)])].into_iter().collect(),
            deleted: [(rel, vec![])].into_iter().collect(),
        };
        let s = |x| Send {
            rel,
            to: NodeId(0),
            row: row(x),
        };
        assert_eq!(
            outcome_digest(&a, &[s(1), s(2)], &[], &[]),
            outcome_digest(&b, &[s(2), s(1)], &[], &[])
        );
        assert_ne!(
            outcome_digest(&a, &[s(1)], &[], &[]),
            outcome_digest(&a, &[s(2)], &[], &[])
        );
    }
}
