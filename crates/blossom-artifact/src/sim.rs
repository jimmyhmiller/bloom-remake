//! A program compiled for one deployment, as the simulator runs it, the spec engine judges it and LDFI explains it
//! (ARCHITECTURE §8.1). Both frontends produce it: the Molly `.ded` frontend (LANGUAGE §21.1) under the Molly
//! [`Profile`], and the Blossom frontend with a spec (LANGUAGE §17) under the Blossom profile.
//!
//! It has two programs:
//!
//! - the **protocol**, the per-node IR program in which the location column is implicit (rules placed at a role
//!   run only on that role's nodes);
//! - the **outcome spec**: `pre`, `post` and the rules that only feed them, compiled to a second IR program in which
//!   every relation keeps its location column. It is evaluated once, at EOT, over the protocol's relations at EOT,
//!   their snapshots at fixed times, and the crash oracle (`isGood` reads `pre` and `post` at EOT only, TEST-022).
//!
//! [`LogicalRel`] ties each source-level relation to its IR relations in both programs, and [`LogicalEdge`] records
//! the source-level rule graph, which conservative negative support (TEST-025) reasons about. A Molly relation owns
//! its generated channel and input; a Blossom program's logical relations are its IR relations one for one.

use blossom_base::{RelId, RoleId, Symbol};
use blossom_ir::ValidatedProgram;
use blossom_value::{
    Value,
    time::{Duration, NodeId, Tick},
};

/// A compiled program for one deployment. The deployment's node `nodes[i]` is `NodeId(i)`; names are sorted
/// (canonical directory order, ARCHITECTURE §5.9).
#[derive(Clone)]
pub struct SimArtifact {
    pub nodes: Vec<Symbol>,
    /// Each node's role (empty for a role-free program).
    pub roles: Vec<Option<RoleId>>,
    pub profile: Profile,
    pub protocol: ValidatedProgram,
    /// Scheduled input events of the protocol (`.ded` `@k` facts, spec facts `@ n at tick k`).
    pub inputs: Vec<InputFact>,
    /// Scheduled messages of client sessions.
    pub ingress: Vec<IngressFact>,
    /// Rows of static relations that hold at one node only (spec facts `@ n` into a static relation, and deployment
    /// configuration, LANGUAGE §7.5): present at every tick of that node.
    pub statics: Vec<NodeStatic>,
    /// The built-in `halt` output, which stops a node at the end of its tick.
    pub halt: Option<RelId>,
    /// Every Molly relation, in order of first appearance.
    pub rels: Vec<LogicalRel>,
    /// One edge per body atom of every rule (protocol and spec).
    pub edges: Vec<LogicalEdge>,
    /// Present when the program defines `pre` and `post`.
    pub spec: Option<OutcomeSpec>,
    /// The run's root seed (seeded choices and resolution policies draw from it, SEM-084).
    pub seed: blossom_value::Seed,
}

/// An index into [`SimArtifact::rels`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogicalIdx(pub u32);

impl LogicalIdx {
    /// The index as a `usize`.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// How runs of the program go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// Molly's: round `k` is tick `k` and no node runs at tick 0 (CR-13); a crashed node keeps running its local
    /// rules and sends nothing (the `.ded` crash view).
    Molly,
    /// Blossom's: tick 0 is every node's boot tick (SEM-012), physical timers fire on a clock of `round` per tick
    /// (LANGUAGE §15.2), and a crashed node is frozen from its crash tick (CR-20).
    Blossom { round: Duration },
}

/// The round duration of the Molly profile; Molly's programs never read the clock.
pub const MOLLY_ROUND: Duration = Duration::from_nanos(1_000_000);

impl Profile {
    /// The first tick a node runs.
    pub const fn first_tick(self) -> Tick {
        match self {
            Profile::Molly => Tick(1),
            Profile::Blossom { .. } => Tick(0),
        }
    }

    /// Whether a crashed node is frozen (CR-20) rather than running on without sending (Molly's view).
    pub const fn frozen(self) -> bool {
        matches!(self, Profile::Blossom { .. })
    }

    /// The clock advance per tick.
    pub const fn round(self) -> Duration {
        match self {
            Profile::Molly => MOLLY_ROUND,
            Profile::Blossom { round } => round,
        }
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

/// A client session's message (LANGUAGE §18.4): spec facts `fact c(…) @ n from s at tick k` on a channel whose
/// source role is `external`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct IngressFact {
    pub node: NodeId,
    pub tick: Tick,
    /// The protocol's channel.
    pub rel: RelId,
    pub session: blossom_value::value::SessionId,
    /// The whole row: column 0 is the destination, `node`.
    pub row: Vec<Value>,
}

/// A static row at one node: `rel(row…)` holds at every tick of `node`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeStatic {
    pub node: NodeId,
    pub rel: RelId,
    pub row: Vec<Value>,
}

/// One source-level relation and the IR relations that implement it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogicalRel {
    pub name: Symbol,
    /// Molly's arity, the location column included.
    pub arity: usize,
    pub kind: LogicalKind,
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
pub enum LogicalKind {
    /// Run by the nodes.
    Protocol,
    /// `pre`, `post`, or a relation that only feeds them.
    Spec,
    /// `crash(Observer, Node, Time)`, the spec's omniscient crash oracle (ANA-010).
    Crash,
}

/// A body atom of a rule: `from` is read (possibly negated) to derive `to`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LogicalEdge {
    pub from: LogicalIdx,
    pub to: LogicalIdx,
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
    AtEot { spec: RelId, rel: LogicalIdx },
    /// The same at a fixed tick (a body atom `p(…)@k`); empty when `k` is after EOT.
    AtTick { spec: RelId, rel: LogicalIdx, tick: Tick },
    /// `crash(Observer, Node, Time)`: every node observes every crash of the run.
    Crash { spec: RelId },
    /// `crashed(n)` (a Blossom spec's oracle, LANGUAGE §17.3): every node that crashed during the run.
    Crashed { spec: RelId },
}

impl SpecFeed {
    /// The spec input relation the feed fills.
    pub const fn spec_rel(self) -> RelId {
        match self {
            SpecFeed::AtEot { spec, .. }
            | SpecFeed::AtTick { spec, .. }
            | SpecFeed::Crash { spec }
            | SpecFeed::Crashed { spec } => spec,
        }
    }
}

impl SimArtifact {
    /// The relation at `idx`.
    pub fn rel(&self, idx: LogicalIdx) -> Option<&LogicalRel> {
        self.rels.get(idx.index())
    }

    /// The Molly relation implemented by protocol relation `rel` (its own relation, channel or input).
    pub fn protocol_owner(&self, rel: RelId) -> Option<LogicalIdx> {
        self.rels
            .iter()
            .position(|r| r.protocol == Some(rel) || r.channel == Some(rel) || r.input == Some(rel))
            .and_then(|i| u32::try_from(i).ok())
            .map(LogicalIdx)
    }

    /// The Molly relation implemented by spec relation `rel`.
    pub fn spec_owner(&self, rel: RelId) -> Option<LogicalIdx> {
        self.rels
            .iter()
            .position(|r| r.spec == Some(rel) || r.spec_at.iter().any(|(_, s)| *s == rel))
            .and_then(|i| u32::try_from(i).ok())
            .map(LogicalIdx)
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
