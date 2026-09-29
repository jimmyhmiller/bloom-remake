//! The provenance graph of a run (ARCHITECTURE §8.2, TEST-023): every fact that held, every distinct firing that
//! derived one, and what each firing read.
//!
//! A **goal** is a fact: a relation's tuple at a node (or, for a global spec relation, nowhere) at a tick. Its
//! [`Support`] is either a leaf (an input event, a static fact, the crash oracle: nothing can falsify it) or the
//! alternative firings that derived it; a goal holds as long as one of them survives. A **firing** holds as long as
//! every one of its premises does. A premise is another goal, the delivery of a message ([`Premise::Clock`], one
//! per sender and send tick), a negated read ([`Premise::Neg`]), or a read of the crash oracle.
//!
//! The graph is built by a frontend-specific converter (for `.ded` programs, in `blossom-ldfi`) through
//! [`ProvGraph::goal`], [`ProvGraph::set_support`] and [`ProvGraph::add_firing`].

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::Arc;

use blossom_base::{DetMap, InternalError, RelId, RuleId, internal_error};
use blossom_ir::obs::FiringKind;
use blossom_value::{
    Value,
    time::{NodeId, Tick},
};

/// Which program a goal's relation belongs to.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Space {
    /// The per-node protocol.
    Protocol,
    /// The global outcome spec.
    Spec,
}

/// A fact: a tuple of a relation at a node (`None` for a global relation) and a tick.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GoalKey {
    pub space: Space,
    pub rel: RelId,
    pub node: Option<NodeId>,
    pub tick: Tick,
    pub row: Arc<[Value]>,
}

/// A goal's index.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GoalId(pub u32);

/// A firing's index.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FiringId(pub u32);

/// How a goal is supported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Support {
    /// Not yet known: the converter has not recorded the goal's support.
    Unknown,
    /// Unfalsifiable: an input event, a static fact, a crash-oracle tuple.
    Leaf,
    /// The alternative firings that derived the goal.
    Derived(Vec<FiringId>),
}

/// A fact that held.
#[derive(Clone, Debug)]
pub struct Goal {
    pub key: GoalKey,
    /// The source-level relation the goal is a tuple of, for relations negative support reasons about; `None` for
    /// generated relations (channels, inputs), which are provenance-transparent.
    pub logical: Option<u32>,
    pub support: Support,
}

/// One distinct firing.
#[derive(Clone, Debug)]
pub struct Firing {
    pub space: Space,
    pub rule: RuleId,
    pub node: Option<NodeId>,
    pub tick: Tick,
    pub kind: FiringKind,
    pub premises: Vec<Premise>,
}

/// What a firing needs.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Premise {
    /// A positive read.
    Goal(GoalId),
    /// The delivery of what `from` sent to `to` at `send` (a message leaf, TEST-026). A node's sends to itself are
    /// not premises: nothing can falsify them.
    Clock { from: NodeId, to: NodeId, send: Tick },
    /// A negated read (ENG-113): the absence of every tuple matching [`NegRead`]'s pattern, recorded in
    /// [`ProvGraph::negation`].
    Neg(NegId),
    /// A negated read of the crash oracle, `notin crash(_, n, t)`: no node matching `node` crashed at a time
    /// matching `time` (`None` leaves the column open). Falsified by such a crash.
    CrashAbsent { node: Option<NodeId>, time: Option<Tick> },
    /// A positive read of the crash oracle's tuple `crash(_, node, time)`: falsified if `node` crashes earlier.
    CrashPresent { node: NodeId, time: Tick },
    /// An aggregate firing's group, recorded in [`ProvGraph::aggregate`]: the firing's row changes when a
    /// contributor appears (a contributor lost is one of its read premises).
    Aggregate(AggId),
}

/// An aggregate group's index.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AggId(pub u32);

/// An aggregate firing's group: `rule` evaluated at `loc` and `tick`, its head columns fixed where `key` is `Some`
/// (the grouping columns) and open where it is `None` (the aggregates).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AggGroup {
    pub space: Space,
    pub rule: RuleId,
    pub loc: Loc,
    pub tick: Tick,
    pub key: Vec<Option<Value>>,
}

/// A negated read's index.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NegId(pub u32);

/// Where a relation's tuples are.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Loc {
    /// At one node (protocol relations).
    Node(NodeId),
    /// At any node (a read that leaves the node open).
    AnyNode,
    /// Nowhere in particular (global spec relations).
    Global,
}

/// A negated read: no tuple of `rel` at `loc` and `tick` matched `pattern` (`None` for an open column).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NegRead {
    pub space: Space,
    pub rel: RelId,
    pub loc: Loc,
    pub tick: Tick,
    pub pattern: Vec<Option<Value>>,
    /// The source-level relation read, for relation-level (conservative) negative support.
    pub logical: u32,
}

/// The provenance graph of one run.
#[derive(Clone, Debug, Default)]
pub struct ProvGraph {
    goals: Vec<Goal>,
    negations: Vec<NegRead>,
    negation_index: DetMap<NegRead, NegId>,
    aggregates: Vec<AggGroup>,
    aggregate_index: DetMap<AggGroup, AggId>,
    /// Goals by relation, node and tick, for scans of tuples matching a pattern.
    by_place: BTreeMap<(Space, RelId, Option<NodeId>, Tick), Vec<GoalId>>,
    /// Hash iteration order is never observed: the index is only probed.
    index: DetMap<GoalKey, GoalId>,
    firings: Vec<Firing>,
    by_logical: BTreeMap<(u32, Tick), Vec<GoalId>>,
}

impl ProvGraph {
    pub fn new() -> ProvGraph {
        ProvGraph::default()
    }

    /// The goal for `key`, added (with unknown support) if it is new.
    pub fn goal(&mut self, key: GoalKey, logical: Option<u32>) -> Result<GoalId, InternalError> {
        if let Some(id) = self.index.get(&key) {
            return Ok(*id);
        }
        let id = GoalId(dense(self.goals.len())?);
        if let Some(l) = logical {
            self.by_logical.entry((l, key.tick)).or_default().push(id);
        }
        self.by_place
            .entry((key.space, key.rel, key.node, key.tick))
            .or_default()
            .push(id);
        self.index.insert(key.clone(), id);
        self.goals.push(Goal {
            key,
            logical,
            support: Support::Unknown,
        });
        Ok(id)
    }

    /// The goal for `key`, if the run has it.
    pub fn find(&self, key: &GoalKey) -> Option<GoalId> {
        self.index.get(key).copied()
    }

    /// Marks a goal a leaf.
    pub fn set_leaf(&mut self, goal: GoalId) {
        if let Some(g) = self.goals.get_mut(goal.0 as usize) {
            g.support = Support::Leaf;
        }
    }

    /// Adds a firing as an alternative derivation of `goal`.
    pub fn add_firing(&mut self, goal: GoalId, firing: Firing) -> Result<FiringId, InternalError> {
        let id = FiringId(dense(self.firings.len())?);
        self.firings.push(firing);
        if let Some(g) = self.goals.get_mut(goal.0 as usize) {
            match &mut g.support {
                Support::Derived(list) => list.push(id),
                support @ Support::Unknown => *support = Support::Derived(vec![id]),
                Support::Leaf => {}
            }
        }
        Ok(id)
    }

    pub fn get(&self, goal: GoalId) -> Option<&Goal> {
        self.goals.get(goal.0 as usize)
    }

    pub fn firing(&self, id: FiringId) -> Option<&Firing> {
        self.firings.get(id.0 as usize)
    }

    /// Records a negated read; equal reads share one id.
    pub fn negation(&mut self, read: NegRead) -> Result<NegId, InternalError> {
        if let Some(id) = self.negation_index.get(&read) {
            return Ok(*id);
        }
        let id = NegId(dense(self.negations.len())?);
        self.negation_index.insert(read.clone(), id);
        self.negations.push(read);
        Ok(id)
    }

    /// Records an aggregate group; equal groups share one id.
    pub fn aggregate_group(&mut self, group: AggGroup) -> Result<AggId, InternalError> {
        if let Some(id) = self.aggregate_index.get(&group) {
            return Ok(*id);
        }
        let id = AggId(dense(self.aggregates.len())?);
        self.aggregate_index.insert(group.clone(), id);
        self.aggregates.push(group);
        Ok(id)
    }

    pub fn aggregate(&self, id: AggId) -> Option<&AggGroup> {
        self.aggregates.get(id.0 as usize)
    }

    pub fn negated(&self, id: NegId) -> Option<&NegRead> {
        self.negations.get(id.0 as usize)
    }

    /// Every goal of `rel` at `node` (or globally) and `tick`.
    pub fn goals_at(&self, space: Space, rel: RelId, node: Option<NodeId>, tick: Tick) -> &[GoalId] {
        self.by_place.get(&(space, rel, node, tick)).map_or(&[], Vec::as_slice)
    }

    /// Every goal of source-level relation `logical` at `tick`.
    pub fn goals_of(&self, logical: u32, tick: Tick) -> &[GoalId] {
        self.by_logical.get(&(logical, tick)).map_or(&[], Vec::as_slice)
    }

    pub fn goal_count(&self) -> usize {
        self.goals.len()
    }

    pub fn firing_count(&self) -> usize {
        self.firings.len()
    }

    /// Renders the derivation tree of `goal`: each goal once with its alternatives, repeated goals as references.
    pub fn render(&self, goal: GoalId, names: &dyn Names) -> String {
        let mut out = String::new();
        let mut shown = BTreeMap::new();
        self.render_goal(goal, 0, names, &mut shown, &mut out);
        out
    }

    fn render_goal(
        &self,
        goal: GoalId,
        depth: usize,
        names: &dyn Names,
        shown: &mut BTreeMap<GoalId, usize>,
        out: &mut String,
    ) {
        let indent = "  ".repeat(depth);
        let Some(g) = self.get(goal) else {
            return;
        };
        let label = names.goal(&g.key);
        if let Some(n) = shown.get(&goal) {
            let _ = writeln!(out, "{indent}{label}  (see [{n}])");
            return;
        }
        let n = shown.len() + 1;
        shown.insert(goal, n);
        match &g.support {
            Support::Leaf => {
                let _ = writeln!(out, "{indent}[{n}] {label}  (input)");
            }
            Support::Unknown => {
                let _ = writeln!(out, "{indent}[{n}] {label}  (no recorded derivation)");
            }
            Support::Derived(firings) => {
                let _ = writeln!(out, "{indent}[{n}] {label}");
                let alternatives = firings.len();
                for (i, f) in firings.iter().enumerate() {
                    let Some(firing) = self.firing(*f) else { continue };
                    let alt = if alternatives > 1 {
                        format!("  (alternative {} of {alternatives})", i + 1)
                    } else {
                        String::new()
                    };
                    let _ = writeln!(out, "{indent}  <- {}{alt}", names.rule(firing));
                    for p in &firing.premises {
                        match p {
                            Premise::Goal(pg) => self.render_goal(*pg, depth + 2, names, shown, out),
                            Premise::Clock { from, to, send } => {
                                let _ = writeln!(
                                    out,
                                    "{indent}    the message {} -> {} sent at {}",
                                    names.node(*from),
                                    names.node(*to),
                                    send.0
                                );
                            }
                            Premise::Neg(id) => {
                                if let Some(n) = self.negated(*id) {
                                    let _ = writeln!(
                                        out,
                                        "{indent}    the absence of a matching `{}` tuple at {}",
                                        names.logical(n.logical),
                                        n.tick.0
                                    );
                                }
                            }
                            Premise::CrashAbsent { node, time } => {
                                let who = node.map_or_else(
                                    || "no node crashes".to_owned(),
                                    |n| format!("{} does not crash", names.node(n)),
                                );
                                let _ = match time {
                                    Some(t) => writeln!(out, "{indent}    {who} at {}", t.0),
                                    None => writeln!(out, "{indent}    {who}"),
                                };
                            }
                            Premise::CrashPresent { node, time } => {
                                let _ = writeln!(
                                    out,
                                    "{indent}    {} crashes at {}, not earlier",
                                    names.node(*node),
                                    time.0
                                );
                            }
                            Premise::Aggregate(_) => {
                                let _ = writeln!(out, "{indent}    no new contributor joins the group");
                            }
                        }
                    }
                }
            }
        }
    }
}

/// How to show a graph's goals, firings, nodes and relations.
pub trait Names {
    fn goal(&self, key: &GoalKey) -> String;
    fn rule(&self, firing: &Firing) -> String;
    fn node(&self, node: NodeId) -> String;
    fn logical(&self, logical: u32) -> String;
}

/// A table position as a dense `u32` id.
fn dense(len: usize) -> Result<u32, InternalError> {
    u32::try_from(len).map_err(|_| internal_error!("more than u32::MAX provenance records"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use blossom_value::value::IntValue;

    use super::*;

    fn key(rel: u32, node: u32, tick: u64, v: i64) -> GoalKey {
        GoalKey {
            space: Space::Protocol,
            rel: RelId::from_raw(rel),
            node: Some(NodeId(node)),
            tick: Tick(tick),
            row: Arc::from(vec![Value::Int(IntValue::I64(v))]),
        }
    }

    #[test]
    fn goals_negations_and_groups_are_shared_and_indexed() {
        let mut g = ProvGraph::new();
        let a = g.goal(key(1, 0, 2, 7), Some(9)).unwrap();
        assert_eq!(g.goal(key(1, 0, 2, 7), Some(9)).unwrap(), a, "one goal per fact");
        let b = g.goal(key(1, 0, 2, 8), Some(9)).unwrap();
        g.goal(key(1, 1, 2, 7), Some(9)).unwrap();
        assert_eq!(
            g.goals_at(Space::Protocol, RelId::from_raw(1), Some(NodeId(0)), Tick(2)),
            [a, b]
        );
        assert_eq!(g.goals_of(9, Tick(2)).len(), 3);
        assert_eq!(g.find(&key(1, 0, 2, 8)), Some(b));
        assert_eq!(g.find(&key(1, 0, 3, 8)), None);
        let read = NegRead {
            space: Space::Protocol,
            rel: RelId::from_raw(2),
            loc: Loc::Node(NodeId(0)),
            tick: Tick(2),
            pattern: vec![None],
            logical: 3,
        };
        let n = g.negation(read.clone()).unwrap();
        assert_eq!(g.negation(read).unwrap(), n, "equal reads share one id");
        let f = g
            .add_firing(
                a,
                Firing {
                    space: Space::Protocol,
                    rule: RuleId::from_raw(0),
                    node: Some(NodeId(0)),
                    tick: Tick(2),
                    kind: FiringKind::Rule,
                    premises: vec![Premise::Goal(b), Premise::Neg(n)],
                },
            )
            .unwrap();
        assert!(matches!(&g.get(a).unwrap().support, Support::Derived(list) if list == &[f]));
        g.set_leaf(b);
        assert_eq!(g.get(b).unwrap().support, Support::Leaf);
    }
}
