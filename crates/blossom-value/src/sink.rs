//! What the wire decoder writes into (ARCHITECTURE §4.1, §5.4). The engine implements [`WordSink`] over its ingest
//! arena, so frames decode straight into words without building [`Value`](crate::Value)s.

use blossom_base::TypeId;
use serde::{Deserialize, Serialize};

use crate::error::ValueError;
use crate::store::RecordBuilder;
use crate::time::{NodeId, Tick};
use crate::value::SessionId;
use crate::word::{ColEncTag, Word};

/// The ingest slot of a row: which relation or channel (and which of its ingest plans) it feeds; assigned by the
/// plan's ingest table.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct IngestSlot(pub u32);

/// Who sent a row. Sender and principal are attached at admission from the connection's identity, never read from
/// the payload (LANG-241).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum RowSender {
    /// The host (an input insert, a timer, a service result).
    Host,
    /// A node of the deployment.
    Node(NodeId),
    /// An external client session.
    Session(SessionId),
}

/// The hidden metadata of a row.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct RowMeta<'a> {
    /// The sender.
    pub sender: RowSender,
    /// The sender's authenticated principal (a SPIFFE id), if any.
    pub principal: Option<&'a str>,
    /// The sender's tick at which the row was sent, for channel rows.
    pub send_tick: Option<Tick>,
}

/// A destination for decoded rows (ARCHITECTURE §4.1).
pub trait WordSink {
    /// Starts a row for `slot`.
    fn begin_row(&mut self, slot: IngestSlot, meta: RowMeta<'_>) -> Result<(), ValueError>;
    /// Appends a directly encoded column word.
    fn push_direct(&mut self, w: Word) -> Result<(), ValueError>;
    /// Appends a string or bytes column, interned or stored as bulk per `enc`.
    fn push_bytes(&mut self, ty: TypeId, enc: ColEncTag, bytes: &[u8]) -> Result<(), ValueError>;
    /// A builder for a record value of type `ty` (a tuple, struct, enum payload or collection), stored in the
    /// ingest arena. Records are built bottom-up: [`RecordBuilder::finish`] returns the record's word without
    /// appending it, so the decoder uses that word as a child of an enclosing record or appends it as a column with
    /// [`push_direct`](WordSink::push_direct).
    fn push_record(&mut self, ty: TypeId) -> Result<RecordBuilder<'_>, ValueError>;
    /// Ends the current row.
    fn end_row(&mut self) -> Result<(), ValueError>;
}
