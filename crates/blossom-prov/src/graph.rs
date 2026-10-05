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

use std::collections::{BTreeMap, BTreeSet};
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
    /// A crashed node's frozen state (CR-20): the tuple it held at the previous tick, which holds as long as that one
    /// does.
    Frozen(GoalId),
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

/// What made a firing: a rule of the program, or the runtime.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum By {
    Rule(RuleId),
    Runtime(RuntimeAct),
}

/// What the runtime did to make a fact hold.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuntimeAct {
    /// A physical timer fired (a guarded one because its guard held).
    Timer,
    /// The host delivered a stream event (a connection opened, bytes, a close, a failed dial).
    Stream,
}

/// One distinct firing.
#[derive(Clone, Debug)]
pub struct Firing {
    pub space: Space,
    pub by: By,
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
    /// The firing node was up (the frozen crash view, CR-20): falsified if `node` is down at `tick` (crashed at or
    /// before it, and not restarted since).
    Alive { node: NodeId, tick: Tick },
    /// `node` was up at every tick of `from..=to` (a stream event waits in its node's inbox, which a crash clears).
    Up { node: NodeId, from: Tick, to: Tick },
    /// `node` does not restart at `tick` (crash-recovery: a restart loses the node's volatile state).
    NoRestart { node: NodeId, tick: Tick },
    /// `node` has not restarted after `from` (the start of the incarnation the fact belongs to: 0, or the run's own
    /// restart) and at or before `tick` (a restart starts its timers counting again).
    NotRestarted { node: NodeId, from: Tick, tick: Tick },
    /// An aggregate firing's group, recorded in [`ProvGraph::aggregate`]: the firing's row changes when a
    /// contributor appears (a contributor lost is one of its read premises).
    Aggregate(AggId),
    /// What `from` sent `to` at `send` on `via` arrives by round `by` (the asynchronous model, S13): falsified by a
    /// delay of that batch past `by`. A stream event's flight, and every earlier flight on its connection (which, held
    /// back, holds it back).
    Arrives {
        from: NodeId,
        to: NodeId,
        send: Tick,
        via: Via,
        by: Tick,
    },
}

/// What one node sends another on a channel in a round.
type Batch = (RelId, NodeId, NodeId, Tick);

/// The path a delivery takes: a channel, or the streams between two nodes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Via {
    Channel(RelId),
    Streams,
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
    /// Goals by relation, node and tick, for scans of tuples matching a pattern (only probed, like `index`).
    by_place: DetMap<(Space, RelId, Option<NodeId>, Tick), Vec<GoalId>>,
    /// Hash iteration order is never observed: the index is only probed.
    index: DetMap<GoalKey, GoalId>,
    firings: Vec<Firing>,
    /// Goals by source-level relation and tick (only probed).
    by_logical: DetMap<(u32, Tick), Vec<GoalId>>,
    /// Every round in which a stream carried something from one node to another (bytes, a close, a dial): a lost
    /// message from the one to the other in that round resets the connection, or fails the dial.
    crossings: BTreeSet<(NodeId, NodeId, Tick)>,
    /// The rows each node sent another on each channel in each round that arrived the round after (only probed): what
    /// a delay can make arrive later (the asynchronous model).
    on_time: DetMap<Batch, Vec<Arc<[Value]>>>,
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

    /// Marks a goal a frozen copy of `prev` (a crashed node's state, CR-20).
    pub fn set_frozen(&mut self, goal: GoalId, prev: GoalId) {
        if let Some(g) = self.goals.get_mut(goal.0 as usize) {
            g.support = Support::Frozen(prev);
        }
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
                Support::Leaf | Support::Frozen(_) => {}
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

    /// Records that a stream carried something from `from` to `to` in round `tick`.
    pub fn crossing(&mut self, from: NodeId, to: NodeId, tick: Tick) {
        self.crossings.insert((from, to, tick));
    }

    /// Records that `from` sent `to` `row` on channel `rel` in round `send`, and it arrived the round after.
    pub fn sent_on_time(&mut self, rel: RelId, from: NodeId, to: NodeId, send: Tick, row: Arc<[Value]>) {
        self.on_time.entry((rel, from, to, send)).or_default().push(row);
    }

    /// The rows `from` sent `to` on channel `rel` in round `send` that arrived the round after.
    pub fn on_time(&self, rel: RelId, from: NodeId, to: NodeId, send: Tick) -> &[Arc<[Value]>] {
        self.on_time.get(&(rel, from, to, send)).map_or(&[], Vec::as_slice)
    }

    /// Whether a stream carried something from `from` to `to` in round `tick`.
    pub fn crossed(&self, from: NodeId, to: NodeId, tick: Tick) -> bool {
        self.crossings.contains(&(from, to, tick))
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
            Support::Frozen(prev) => {
                let _ = writeln!(out, "{indent}[{n}] {label}  (frozen: its node has crashed)");
                self.render_goal(*prev, depth + 1, names, shown, out);
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
                            Premise::Alive { node, tick } => {
                                let _ = writeln!(out, "{indent}    {} is up at {}", names.node(*node), tick.0);
                            }
                            Premise::Up { node, from, to } => {
                                let _ = writeln!(
                                    out,
                                    "{indent}    {} is up from {} to {}",
                                    names.node(*node),
                                    from.0,
                                    to.0
                                );
                            }
                            Premise::NoRestart { node, tick } => {
                                let _ =
                                    writeln!(out, "{indent}    {} does not restart at {}", names.node(*node), tick.0);
                            }
                            Premise::Arrives { from, to, send, by, .. } => {
                                let _ = writeln!(
                                    out,
                                    "{indent}    what {} sent {} at {} arrives by {}",
                                    names.node(*from),
                                    names.node(*to),
                                    send.0,
                                    by.0
                                );
                            }
                            Premise::NotRestarted { node, from, tick } => {
                                let _ = writeln!(
                                    out,
                                    "{indent}    {} has not restarted after {} and by {}",
                                    names.node(*node),
                                    from.0,
                                    tick.0
                                );
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
                    by: By::Rule(RuleId::from_raw(0)),
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
