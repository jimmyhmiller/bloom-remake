//! What both ends of a link agree on and how they frame channel tuples (ARCHITECTURE §5.4), independent of the
//! transport: the deployment identity, the channel catalog, the `HELLO` checks, and `BATCH` encoding. The runtime's
//! TCP connections and the browser's WebSocket link (docs/design/CLIENTS.md §3) both use it.

use std::collections::BTreeMap;
use std::sync::Arc;

use blossom_base::RelId;
use blossom_ir::core::{Program, RelClass};
use blossom_ir::tick::Row;
use blossom_value::time::NodeId;

use crate::codec::{Codec, NodeEncoding, WireError, WireLimits};
use crate::frame::{Batch, ChannelSchema, Frame, Hello, KIND_PLAIN, PROTO, Peer, RejectReason};

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
    pub fn of(program: &Program) -> Result<Catalog, WireError> {
        let mut channels = Vec::new();
        for (id, r) in program.rels.iter_enumerated() {
            if !matches!(r.class, RelClass::Channel(_)) {
                continue;
            }
            let sid = u32::try_from(channels.len()).map_err(|_| WireError::Limit("channel count"))?;
            channels.push((
                id,
                ChannelSchema {
                    sid,
                    name: r.name.to_string(),
                    hash: crate::catalog::schema_hash(program, id),
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
) -> Result<Frames, WireError> {
    let (batches, oversized) = batches(codec, program, sid, rel, send_tick, rows)?;
    Ok(Frames {
        frames: batches.into_iter().map(|b| Frame::Batch(b).encode()).collect(),
        oversized,
    })
}

/// Rows of one channel as batches under [`BATCH_BODY`] each, and the number of rows too large for any frame.
pub fn batches(
    codec: &Codec<'_>,
    program: &Program,
    sid: u32,
    rel: RelId,
    send_tick: u64,
    rows: &[&Row],
) -> Result<(Vec<Batch>, u64), WireError> {
    let cols = &program
        .rels
        .get(rel)
        .ok_or_else(|| WireError::Malformed(format!("channel {rel:?} is not declared")))?
        .schema
        .cols;
    let limit = WireLimits::default().max_frame.saturating_sub(64);
    let mut out = Vec::new();
    let mut oversized = 0;
    let mut body = Vec::new();
    let mut count = 0u64;
    let flush = |body: &mut Vec<u8>, count: &mut u64, out: &mut Vec<Batch>| {
        if *count > 0 {
            out.push(Batch {
                sid,
                send_tick,
                kind: KIND_PLAIN,
                count: *count,
                body: std::mem::take(body),
            });
            *count = 0;
        }
    };
    for r in rows {
        let mut one = Vec::new();
        codec.encode_row(cols, r, &mut one)?;
        if one.len() > limit {
            oversized += 1;
            continue;
        }
        if count > 0 && body.len() + one.len() > BATCH_BODY {
            flush(&mut body, &mut count, &mut out);
        }
        body.extend_from_slice(&one);
        count += 1;
    }
    flush(&mut body, &mut count, &mut out);
    Ok((out, oversized))
}

/// Decodes a `BATCH`'s rows of channel `rel`.
pub fn batch_rows(codec: &Codec<'_>, program: &Program, rel: RelId, b: &Batch) -> Result<Vec<Row>, WireError> {
    let cols = &program
        .rels
        .get(rel)
        .ok_or_else(|| WireError::Malformed(format!("channel {rel:?} is not declared")))?
        .schema
        .cols;
    let mut input = b.body.as_slice();
    let mut out = Vec::new();
    for _ in 0..b.count {
        out.push(Arc::from(codec.decode_row(cols, &mut input)?));
    }
    if !input.is_empty() {
        return Err(WireError::Malformed("trailing bytes in a batch".into()));
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
