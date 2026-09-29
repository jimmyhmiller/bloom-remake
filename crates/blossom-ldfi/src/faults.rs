//! Failure specs and fault sets (ARCHITECTURE §8.1, §8.3).

use std::collections::BTreeSet;

use blossom_sim::{FaultSchedule, Omission};
use blossom_value::time::{NodeId, Tick};

use crate::LdfiError;

/// What faults LDFI may inject (TEST-020, TEST-021): omissions of messages sent before EFF, and up to
/// `max_crashes` crashes, over a run of ticks `1..=eot`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureSpec {
    pub eot: Tick,
    pub eff: Tick,
    pub max_crashes: u32,
    /// The number of nodes of the deployment.
    pub nodes: u32,
}

impl FailureSpec {
    pub fn new(eot: u64, eff: u64, max_crashes: u32, nodes: u32) -> Result<FailureSpec, LdfiError> {
        if eot < 1 {
            return Err(LdfiError::Spec("EOT must be at least 1".into()));
        }
        if eff >= eot {
            return Err(LdfiError::Spec(format!(
                "EFF ({eff}) must be before EOT ({eot}) (TEST-020)"
            )));
        }
        if max_crashes > nodes {
            return Err(LdfiError::Spec(format!(
                "{max_crashes} crashes of {nodes} node(s) (TEST-020)"
            )));
        }
        Ok(FailureSpec {
            eot: Tick(eot),
            eff: Tick(eff),
            max_crashes,
            nodes,
        })
    }

    /// Whether the message `from -> to` sent at `send` may be omitted (CR-21, TEST-021).
    pub fn omission_allowed(&self, from: NodeId, to: NodeId, send: Tick) -> bool {
        from != to && send.0 >= 1 && send < self.eff && from.0 < self.nodes && to.0 < self.nodes
    }

    /// The ticks a node may crash at: `1..EOT` (Molly's hypothesis space).
    pub fn crash_ticks(&self) -> impl Iterator<Item = Tick> {
        (1..self.eot.0).map(Tick)
    }

    /// Whether `faults` lies within the spec.
    pub fn admits(&self, faults: &FaultSchedule) -> bool {
        u32::try_from(faults.crashes.len()).is_ok_and(|n| n <= self.max_crashes)
            && faults
                .crashes
                .iter()
                .all(|(n, t)| n.0 < self.nodes && t.0 >= 1 && *t < self.eot)
            && faults
                .omissions
                .iter()
                .all(|o| self.omission_allowed(o.from, o.to, o.send))
    }

    /// The clock facts a fault set removes (ARCHITECTURE §8.3, LDFI Appendix B): an omission removes its own; a crash
    /// of `n` at `c` removes every clock `n -> x` with `x != n` from `c` to EOT.
    pub fn removed_clocks(&self, faults: &FaultSchedule) -> BTreeSet<Omission> {
        let mut out: BTreeSet<Omission> = faults.omissions.clone();
        for (&n, &c) in &faults.crashes {
            for x in (0..self.nodes).map(NodeId) {
                if x == n {
                    continue;
                }
                for s in c.0..=self.eot.0 {
                    out.insert(Omission {
                        from: n,
                        to: x,
                        send: Tick(s),
                    });
                }
            }
        }
        out
    }
}

/// The canonical form of a fault set: omissions that a crash of their sender at or before the send tick already
/// implies are dropped.
pub fn canonical(mut faults: FaultSchedule) -> FaultSchedule {
    let crashes = faults.crashes.clone();
    faults
        .omissions
        .retain(|o| crashes.get(&o.from).is_none_or(|c| *c > o.send));
    faults
}

/// `O(from,to,send)` and `C(node,tick)` labels, sorted (the corpus's notation).
pub fn labels(faults: &FaultSchedule, node: &dyn Fn(NodeId) -> String) -> Vec<String> {
    let mut out: Vec<String> = faults
        .crashes
        .iter()
        .map(|(n, t)| format!("C({},{})", node(*n), t.0))
        .chain(
            faults
                .omissions
                .iter()
                .map(|o| format!("O({},{},{})", node(o.from), node(o.to), o.send.0)),
        )
        .collect();
    out.sort();
    out
}

/// The order hypotheses are tried in: fewer faults first, then fewer removed clock facts (so a crash is tried at its
/// latest useful tick), then the canonical order of the fault sets.
pub fn order_key(spec: &FailureSpec, faults: &FaultSchedule) -> (usize, usize, FaultSchedule) {
    (faults.len(), spec.removed_clocks(faults).len(), faults.clone())
}
