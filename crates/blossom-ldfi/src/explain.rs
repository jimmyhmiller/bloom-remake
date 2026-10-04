//! Why a goal of a run's lineage counts as lost under a fault schedule (a diagnostic for incomplete lineage): the
//! lineage is evaluated under the concrete faults, the way the hazard encoding reads it, and one derivation path is
//! shown down to the premises the faults falsify.
//!
//! A run's own `post` goals hold in that run, so their lineage must not count them as lost under the run's own faults;
//! when it does ([`crate::hazard::Extensions::incomplete`]), the lineage misses how the run derived them, or a premise
//! claims more than the faults do. Negated reads and aggregate groups depend on what faults can make appear, which
//! only the encoder decides: here they count as holding, and are shown as such.

use std::collections::BTreeMap;
use std::fmt::Write;

use blossom_prov::{GoalId, Names, Premise, ProvGraph, Support};
use blossom_sim::{FaultSchedule, Omission};
use blossom_value::time::{NodeId, Tick};

use crate::faults::FailureSpec;

/// Whether `goal` counts as lost under `faults`, and if so, why: one derivation path to falsified premises.
pub fn lost_under(
    graph: &ProvGraph,
    spec: &FailureSpec,
    faults: &FaultSchedule,
    goal: GoalId,
    names: &dyn Names,
) -> Option<String> {
    let mut e = Eval {
        graph,
        spec,
        faults,
        memo: BTreeMap::new(),
        path: Vec::new(),
    };
    if !e.goal(goal) {
        return None;
    }
    let mut out = String::new();
    e.render(goal, 0, names, &mut out, &mut Vec::new());
    Some(out)
}

struct Eval<'a> {
    graph: &'a ProvGraph,
    spec: &'a FailureSpec,
    faults: &'a FaultSchedule,
    memo: BTreeMap<GoalId, bool>,
    path: Vec<GoalId>,
}

impl Eval<'_> {
    fn down(&self, node: NodeId, t: Tick) -> bool {
        self.faults.crashes.get(&node).is_some_and(|c| self.spec.down(*c, t))
    }

    /// Whether the faults falsify a premise (negated reads and aggregate groups: never, here).
    fn premise(&mut self, p: &Premise) -> bool {
        match *p {
            Premise::Goal(g) => self.goal(g),
            Premise::Clock { from, to, send } => {
                self.faults.omissions.contains(&Omission { from, to, send }) || self.down(from, send)
            }
            Premise::Alive { node, tick } => self.down(node, tick),
            Premise::Up { node, from, to } => (from.0..=to.0).any(|t| self.down(node, Tick(t))),
            Premise::NoRestart { node, tick } => self.faults.restarts.get(&node) == Some(&tick),
            Premise::NotRestarted { node, from, tick } => {
                self.faults.restarts.get(&node).is_some_and(|r| from < *r && *r <= tick)
            }
            Premise::CrashAbsent { node, time } => self.faults.crashes.iter().any(|(n, c)| {
                node.is_none_or(|m| m == *n)
                    && match time {
                        Some(t) => *c == t,
                        None => self.down(*n, self.spec.eot),
                    }
            }),
            Premise::CrashPresent { .. } | Premise::Neg(_) | Premise::Aggregate(_) => false,
        }
    }

    /// Whether every derivation of `goal` has a falsified premise (a goal met again on the path counts as lost, as in
    /// the encoder: a derivation that needs itself is no derivation).
    fn goal(&mut self, goal: GoalId) -> bool {
        if let Some(l) = self.memo.get(&goal) {
            return *l;
        }
        if self.path.contains(&goal) {
            return true;
        }
        self.path.push(goal);
        let lost = match self.graph.get(goal).map(|g| g.support.clone()) {
            None | Some(Support::Leaf) | Some(Support::Unknown) => false,
            Some(Support::Frozen(prev)) => self.goal(prev),
            Some(Support::Derived(firings)) => firings.iter().all(|f| {
                let premises = self.graph.firing(*f).map(|f| f.premises.clone()).unwrap_or_default();
                premises.iter().any(|p| self.premise(p))
            }),
        };
        self.path.pop();
        self.memo.insert(goal, lost);
        lost
    }

    /// A lost goal and, per firing, its first falsified premise (goals recursively, each shown once).
    fn render(&mut self, goal: GoalId, depth: usize, names: &dyn Names, out: &mut String, shown: &mut Vec<GoalId>) {
        let indent = "  ".repeat(depth);
        let Some(g) = self.graph.get(goal) else { return };
        let label = names.goal(&g.key);
        if shown.contains(&goal) {
            let _ = writeln!(out, "{indent}{label}  (shown above)");
            return;
        }
        shown.push(goal);
        match g.support.clone() {
            Support::Frozen(prev) => {
                let _ = writeln!(out, "{indent}{label}  (frozen copy)");
                self.render(prev, depth + 1, names, out, shown);
            }
            Support::Derived(firings) => {
                let _ = writeln!(out, "{indent}{label}  lost: every derivation fails");
                for f in firings {
                    let Some(firing) = self.graph.firing(f).cloned() else {
                        continue;
                    };
                    let _ = writeln!(out, "{indent}  <- {}", names.rule(&firing));
                    let Some(p) = firing.premises.iter().find(|p| self.premise(p)) else {
                        let _ = writeln!(out, "{indent}     (no falsified premise: a cycle on the path)");
                        continue;
                    };
                    match p {
                        Premise::Goal(pg) => self.render(*pg, depth + 2, names, out, shown),
                        other => {
                            let _ = writeln!(out, "{indent}     falsified: {}", premise_text(other, names));
                        }
                    }
                }
            }
            Support::Leaf | Support::Unknown => {
                let _ = writeln!(out, "{indent}{label}  (a leaf: not lost)");
            }
        }
    }
}

fn premise_text(p: &Premise, names: &dyn Names) -> String {
    match *p {
        Premise::Clock { from, to, send } => {
            format!(
                "the message {} -> {} sent at {}",
                names.node(from),
                names.node(to),
                send.0
            )
        }
        Premise::Alive { node, tick } => format!("{} up at {}", names.node(node), tick.0),
        Premise::Up { node, from, to } => format!("{} up from {} to {}", names.node(node), from.0, to.0),
        Premise::NoRestart { node, tick } => format!("{} does not restart at {}", names.node(node), tick.0),
        Premise::NotRestarted { node, from, tick } => {
            format!(
                "{} has not restarted after {} and by {}",
                names.node(node),
                from.0,
                tick.0
            )
        }
        Premise::CrashAbsent { node, .. } => match node {
            Some(n) => format!("{} has not crashed", names.node(n)),
            None => "no node has crashed".to_owned(),
        },
        Premise::Goal(_) | Premise::CrashPresent { .. } | Premise::Neg(_) | Premise::Aggregate(_) => format!("{p:?}"),
    }
}
