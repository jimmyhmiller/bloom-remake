//! The hazard circuit (S12): the hazards of a run's lineage as a monotone circuit over fault atoms, independent of
//! any solver.
//!
//! A node is immutable and shared (`Arc`), so the hazards one run encodes can be reused by later runs, and by every
//! worker of a search, without re-deriving them; a solver gets the nodes it needs through its own
//! [`crate::hazard::FaultVars`], one auxiliary variable per node with implications in one direction
//! (Plaisted–Greenbaum). Constants are folded as nodes are built, so a node is never constant by construction.
//!
//! [`Atoms`] builds the fault atoms and the fault predicates over them that the encoding uses (a node down at a tick,
//! restarted in a range), by the failure spec's crash model.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use blossom_base::DetMap;
use blossom_sim::Omission;
use blossom_value::time::{NodeId, Tick};

use crate::faults::FailureSpec;

/// A fault atom: a lost message, or a crash variable of a node at a tick: under crash-stop `K(n,t)`, "crashed at or
/// before `t`"; under crash-restart `X(n,t)`, "crashes at `t`".
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Atom {
    Omission(Omission),
    Crash(NodeId, Tick),
}

/// A circuit node: an atom, or a conjunction or disjunction of nodes.
#[derive(Debug)]
pub struct Node {
    id: u64,
    kind: Kind,
}

#[derive(Debug)]
pub enum Kind {
    Atom(Atom),
    And(Vec<Arc<Node>>),
    Or(Vec<Arc<Node>>),
}

/// Node ids are unique in the process: a solver maps each node it was given to its literal by id.
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

impl Node {
    fn new(kind: Kind) -> Arc<Node> {
        Arc::new(Node {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            kind,
        })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn kind(&self) -> &Kind {
        &self.kind
    }
}

/// A hazard: a constant, or a circuit node.
#[derive(Clone, Debug)]
pub enum Hazard {
    /// Nothing the spec allows falsifies it.
    False,
    /// Already falsified.
    True,
    Node(Arc<Node>),
}

impl PartialEq for Hazard {
    fn eq(&self, other: &Hazard) -> bool {
        match (self, other) {
            (Hazard::False, Hazard::False) | (Hazard::True, Hazard::True) => true,
            (Hazard::Node(a), Hazard::Node(b)) => a.id == b.id,
            _ => false,
        }
    }
}

impl Eq for Hazard {}

/// An atom's node.
pub fn atom(a: Atom) -> Hazard {
    Hazard::Node(Node::new(Kind::Atom(a)))
}

/// The conjunction of `children`, folded: false if any is, true if all are.
pub fn and(children: Vec<Hazard>) -> Hazard {
    let mut nodes = Vec::with_capacity(children.len());
    for c in children {
        match c {
            Hazard::False => return Hazard::False,
            Hazard::True => {}
            Hazard::Node(n) => nodes.push(n),
        }
    }
    match nodes.len() {
        0 => Hazard::True,
        1 => nodes.pop().map_or(Hazard::True, Hazard::Node),
        _ => Hazard::Node(Node::new(Kind::And(nodes))),
    }
}

/// The disjunction of `children`, folded (true if any is, false if none is a node) and without repeated nodes.
pub fn or(children: Vec<Hazard>) -> Hazard {
    let mut nodes = Vec::with_capacity(children.len());
    for c in children {
        match c {
            Hazard::True => return Hazard::True,
            Hazard::False => {}
            Hazard::Node(n) => nodes.push(n),
        }
    }
    nodes.sort_by_key(|n| n.id);
    nodes.dedup_by_key(|n| n.id);
    match nodes.len() {
        0 => Hazard::False,
        1 => nodes.pop().map_or(Hazard::False, Hazard::Node),
        _ => Hazard::Node(Node::new(Kind::Or(nodes))),
    }
}

/// The fault atoms of one failure spec and the predicates over them, each built once.
pub struct Atoms<'s> {
    spec: &'s FailureSpec,
    omissions: DetMap<Omission, Hazard>,
    crashes: DetMap<(NodeId, Tick), Hazard>,
    /// Disjunctions of a node's crash-time atoms over a tick range (crash-restart).
    ranges: DetMap<(NodeId, u64, u64), Hazard>,
}

impl<'s> Atoms<'s> {
    pub fn new(spec: &'s FailureSpec) -> Atoms<'s> {
        Atoms {
            spec,
            omissions: DetMap::default(),
            crashes: DetMap::default(),
            ranges: DetMap::default(),
        }
    }

    /// `O(o)`: the message is lost.
    pub fn omission(&mut self, o: Omission) -> Hazard {
        self.omissions
            .entry(o)
            .or_insert_with(|| atom(Atom::Omission(o)))
            .clone()
    }

    /// The crash atom of `node` at `t` (false outside the ticks a node may crash at, or when no node may crash).
    fn crash(&mut self, node: NodeId, t: Tick) -> Hazard {
        if self.spec.max_crashes == 0 || t.0 < 1 || t >= self.spec.eot {
            return Hazard::False;
        }
        self.crashes
            .entry((node, t))
            .or_insert_with(|| atom(Atom::Crash(node, t)))
            .clone()
    }

    /// `K(n,t)` (crash-stop): `node` crashed at or before `t`.
    pub fn k(&mut self, node: NodeId, t: Tick) -> Hazard {
        self.crash(node, t)
    }

    /// Under crash-restart: `node` crashes at some tick of `lo..=hi`.
    pub fn crash_in(&mut self, node: NodeId, lo: u64, hi: u64) -> Hazard {
        if self.spec.max_crashes == 0 {
            return Hazard::False;
        }
        let lo = lo.max(1);
        let hi = hi.min(self.spec.eot.0.saturating_sub(1));
        if lo > hi {
            return Hazard::False;
        }
        if let Some(h) = self.ranges.get(&(node, lo, hi)) {
            return h.clone();
        }
        let atoms: Vec<Hazard> = (lo..=hi).map(|t| self.crash(node, Tick(t))).collect();
        let h = or(atoms);
        self.ranges.insert((node, lo, hi), h.clone());
        h
    }

    /// `node` is down at `t`: crashed at or before it, and (crash-restart) not restarted since.
    pub fn down(&mut self, node: NodeId, t: Tick) -> Hazard {
        match self.spec.restart {
            None => self.k(node, t),
            Some(d) => self.crash_in(node, (t.0 + 1).saturating_sub(d), t.0),
        }
    }

    /// `node` is down at some tick of `from..=to`.
    pub fn down_during(&mut self, node: NodeId, from: Tick, to: Tick) -> Hazard {
        match self.spec.restart {
            None => self.k(node, to),
            Some(d) => self.crash_in(node, (from.0 + 1).saturating_sub(d), to.0),
        }
    }

    /// `node` restarts at `t` (never under crash-stop).
    pub fn restart_at(&mut self, node: NodeId, t: Tick) -> Hazard {
        match self.spec.restart {
            Some(d) if t.0 > d => self.crash_in(node, t.0 - d, t.0 - d),
            _ => Hazard::False,
        }
    }

    /// `node` has restarted at or before `t` (never under crash-stop).
    pub fn restarted_by(&mut self, node: NodeId, t: Tick) -> Hazard {
        self.restarted_between(node, Tick(0), t)
    }

    /// `node` restarts at some tick after `from` and at or before `to` (never under crash-stop).
    pub fn restarted_between(&mut self, node: NodeId, from: Tick, to: Tick) -> Hazard {
        match self.spec.restart {
            Some(d) if to.0 > d => self.crash_in(node, (from.0 + 1).saturating_sub(d), to.0 - d),
            _ => Hazard::False,
        }
    }

    /// `node` crashes at `c` (under crash-stop `K(n,c)`, which a crash at `c` implies: the encoding is monotone).
    pub fn crash_at(&mut self, node: NodeId, c: Tick) -> Hazard {
        match self.spec.restart {
            None => self.k(node, c),
            Some(_) => self.crash_in(node, c.0, c.0),
        }
    }
}
