//! A Molly `.ded` program compiled for one deployment (LANGUAGE §21.1, ARCHITECTURE §8.1): what the simulator runs,
//! what the spec engine judges, and what LDFI needs to relate provenance back to Molly's relations.
//!
//! Molly's relations are global, with the location in their first column. Blossom splits a `.ded` program in two:
//!
//! - the **protocol**, a role-free per-node IR program in which the location column is implicit (every node runs
//!   every rule; `@async` heads go through generated channels; `@k` facts are input events);
//! - the **outcome spec**: `pre`, `post` and the rules that only feed them, compiled to a second IR program in which
//!   every relation keeps its location column. It is evaluated once, at EOT, over the protocol's relations at EOT,
//!   their snapshots at fixed times (`p(…)@k` atoms), and the `crash` oracle (Molly's `isGood` reads `pre` and `post`
//!   at EOT only, TEST-022).
//!
//! [`DedRel`] ties each Molly relation to its IR relations in both programs, and [`DedEdge`] records Molly's own
//! rule graph, which conservative negative support (TEST-025) reasons about.

use blossom_base::{RelId, Symbol};
use blossom_ir::ValidatedProgram;
use blossom_value::{
    Value,
    time::{NodeId, Tick},
};

/// A compiled `.ded` program. The deployment's node `nodes[i]` is `NodeId(i)`; names are sorted (canonical directory
/// order, ARCHITECTURE §5.9).
#[derive(Clone)]
pub struct DedArtifact {
    pub nodes: Vec<Symbol>,
    pub protocol: ValidatedProgram,
    /// The `@k` facts, as input events of the protocol.
    pub inputs: Vec<InputFact>,
    /// Every Molly relation, in order of first appearance.
    pub rels: Vec<DedRel>,
    /// One edge per body atom of every rule (protocol and spec).
    pub edges: Vec<DedEdge>,
    /// Present when the program defines `pre` and `post`.
    pub spec: Option<OutcomeSpec>,
}

/// An index into [`DedArtifact::rels`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DedRelIdx(pub u32);

impl DedRelIdx {
    /// The index as a `usize`.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// An input event: the fact `rel(node, row…)@tick`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InputFact {
    pub node: NodeId,
    pub tick: Tick,
    /// The protocol's input relation (an `Event(Input)` relation).
    pub rel: RelId,
    /// The row without its location column.
    pub row: Vec<Value>,
}

/// One Molly relation and the IR relations that implement it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DedRel {
    pub name: Symbol,
    /// Molly's arity, the location column included.
    pub arity: usize,
    pub kind: DedRelKind,
    /// The protocol relation holding the tuples at each node (location column dropped). `None` for spec relations
    /// and for `crash`.
    pub protocol: Option<RelId>,
    /// The generated channel that carries the relation's `@async` derivations to their destination.
    pub channel: Option<RelId>,
    /// The protocol input relation that receives the relation's `@k` facts: `protocol` itself when the relation has
    /// no rules, a generated relation otherwise.
    pub input: Option<RelId>,
    /// The spec relation: an IDB relation for a spec relation, the EOT snapshot input for a protocol relation that
    /// spec rules read, or the crash oracle input.
    pub spec: Option<RelId>,
    /// Snapshot inputs of the spec for body atoms `p(…)@k`, by time.
    pub spec_at: Vec<(Tick, RelId)>,
}

/// Which side of the program a Molly relation belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DedRelKind {
    /// Run by the nodes.
    Protocol,
    /// `pre`, `post`, or a relation that only feeds them.
    Spec,
    /// `crash(Observer, Node, Time)`, the spec's omniscient crash oracle (ANA-010).
    Crash,
}

/// A body atom of a rule: `from` is read (possibly negated) to derive `to`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DedEdge {
    pub from: DedRelIdx,
    pub to: DedRelIdx,
    pub time: EdgeTime,
    pub negated: bool,
}

/// The time relationship of an edge: that of the rule it comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeTime {
    Deductive,
    Next,
    Async,
}

/// The implicit spec of a `.ded` program: `pre` and `post` read at EOT (TEST-022).
#[derive(Clone)]
pub struct OutcomeSpec {
    /// A role-free program of deductive rules whose relations all keep their location column.
    pub program: ValidatedProgram,
    pub pre: RelId,
    pub post: RelId,
    /// What each input relation of `program` is fed with.
    pub feeds: Vec<SpecFeed>,
}

/// The contents of one input relation of the spec program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecFeed {
    /// Every node's tuples of the protocol relation at EOT, each prefixed with the node.
    AtEot { spec: RelId, rel: DedRelIdx },
    /// The same at a fixed tick (a body atom `p(…)@k`); empty when `k` is after EOT.
    AtTick { spec: RelId, rel: DedRelIdx, tick: Tick },
    /// `crash(Observer, Node, Time)`: every node observes every crash of the run.
    Crash { spec: RelId },
}

impl DedArtifact {
    /// The relation at `idx`.
    pub fn rel(&self, idx: DedRelIdx) -> Option<&DedRel> {
        self.rels.get(idx.index())
    }

    /// The Molly relation implemented by protocol relation `rel` (its own relation, channel or input).
    pub fn protocol_owner(&self, rel: RelId) -> Option<DedRelIdx> {
        self.rels
            .iter()
            .position(|r| r.protocol == Some(rel) || r.channel == Some(rel) || r.input == Some(rel))
            .and_then(|i| u32::try_from(i).ok())
            .map(DedRelIdx)
    }

    /// The Molly relation implemented by spec relation `rel`.
    pub fn spec_owner(&self, rel: RelId) -> Option<DedRelIdx> {
        self.rels
            .iter()
            .position(|r| r.spec == Some(rel) || r.spec_at.iter().any(|(_, s)| *s == rel))
            .and_then(|i| u32::try_from(i).ok())
            .map(DedRelIdx)
    }

    /// The node's name.
    pub fn node_name(&self, node: NodeId) -> Option<Symbol> {
        self.nodes.get(node.0 as usize).copied()
    }

    /// The node named `name`.
    pub fn node_id(&self, name: &str) -> Option<NodeId> {
        self.nodes
            .iter()
            .position(|n| n.as_str() == name)
            .and_then(|i| u32::try_from(i).ok())
            .map(NodeId)
    }
}
