//! One node's tick, as every evaluator sees it (ARCHITECTURE §5.1): what a tick reads ([`TickInput`]) and what it
//! produces ([`TickOutput`]). The reference oracle and the engine both implement it, and neither depends on the
//! other (ARCH-16).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_base::RelId;
use blossom_value::{
    Value,
    time::{Instant, NodeId, Tick},
    value::SessionId,
};

use blossom_base::{InternalError, Unimplemented};

use crate::obs::{FiringRecord, ProgramErrorRecord};

/// A tuple.
pub type Row = Arc<[Value]>;

/// Relation contents: every non-empty relation's rows.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instance {
    pub rels: BTreeMap<RelId, BTreeSet<Row>>,
}

impl Instance {
    /// The rows of `rel` (empty when it has none).
    pub fn rows(&self, rel: RelId) -> impl Iterator<Item = &Row> {
        self.rels.get(&rel).into_iter().flatten()
    }

    /// Whether `rel` holds `row`.
    pub fn contains(&self, rel: RelId, row: &[Value]) -> bool {
        self.rels.get(&rel).is_some_and(|rows| rows.contains(row))
    }

    /// Adds a row; whether it was new.
    pub fn insert(&mut self, rel: RelId, row: Row) -> bool {
        self.rels.entry(rel).or_default().insert(row)
    }

    /// Whether no relation has a row.
    pub fn is_empty(&self) -> bool {
        self.rels.values().all(BTreeSet::is_empty)
    }
}

/// A channel tuple delivered to the node this tick. Column 0 of `row` is the destination, the node itself.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Delivery {
    pub rel: RelId,
    pub from: NodeId,
    pub row: Row,
}

/// A channel tuple the node sends: column 0 of `row` is the destination.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Send {
    pub rel: RelId,
    pub to: NodeId,
    pub row: Row,
}

/// A message from an external client session on a channel whose source role is `external` (LANGUAGE §18.4).
/// Column 0 of `row` is the destination, the node itself.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ingress {
    pub rel: RelId,
    pub session: SessionId,
    pub row: Row,
}

/// A reply to an external client session: column 0 of `row` is the session.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Egress {
    pub rel: RelId,
    pub session: SessionId,
    pub row: Row,
}

/// Everything one tick of one node reads.
#[derive(Clone, Debug)]
pub struct TickInput<'a> {
    /// `$self`.
    pub node: NodeId,
    /// The node's incarnation (its restart count; 1 on the first boot): `rand` draws differ across incarnations.
    pub incarnation: u64,
    pub tick: Tick,
    /// `$now`: the tick's clock sample.
    pub now: Instant,
    /// Last tick's `@next` heads.
    pub carried: &'a Instance,
    /// The tick's input events.
    pub events: &'a [(RelId, Row)],
    /// The channel tuples delivered this tick.
    pub delivered: &'a [Delivery],
    /// The messages client sessions sent this tick.
    pub ingress: &'a [Ingress],
    /// Whether to report the tick's firings.
    pub capture: bool,
}

/// Everything one tick of one node produces.
#[derive(Clone, Debug, Default)]
pub struct TickOutput {
    /// The tick's final instance: every relation's contents at this tick.
    pub instance: Instance,
    /// The inductive heads: next tick's carried state.
    pub next: Instance,
    /// The async heads to nodes.
    pub outbox: BTreeSet<Send>,
    /// The async heads to client sessions.
    pub egress: BTreeSet<Egress>,
    /// The distinct firings of the tick, in evaluation order (deterministic); empty unless capture was requested.
    pub firings: Vec<FiringRecord>,
}

/// Why an evaluator (the oracle or the engine) could not evaluate a tick.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// The deductive rules do not stratify: a negated or aggregated read on a same-tick cycle (SEM-020).
    #[error("the program does not stratify: {0}")]
    NotStratifiable(String),
    /// A deploy-time parameter without a default that the deployment does not bind.
    #[error("the deployment does not bind the parameter `{0}`, which has no default")]
    Unbound(String),
    /// A runtime hard error of the program at this tick (BLSRnnn, ARCHITECTURE §6.6).
    #[error("{} at tick {}: {}", .error.code, .tick.0, .error.detail)]
    Program { tick: Tick, error: ProgramErrorRecord },
    #[error(transparent)]
    Unimplemented(#[from] Unimplemented),
    #[error(transparent)]
    Internal(#[from] InternalError),
}
