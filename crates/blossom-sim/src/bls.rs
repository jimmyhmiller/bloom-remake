//! The `.bls` profile: running a compiled Blossom program in the synchronous-round world (TEST-006).
//!
//! Every node runs the program; rules placed at a role run only on that role's nodes. The world feeds the runtime
//! events: `boot()` in every node's first round (tick 0, SEM-012) and each physical timer's firings, with round `t`
//! at time `t × round` (LANGUAGE §15.2). A timer `every d` placed on a node fires once per period: firing `k` (from
//! 0) is due at `(k + 1) × d` and is delivered in the first round whose clock has reached it, as the row
//! `(k, (k + 1) × d)`. A crashed node is frozen from its crash round (CR-20).

use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{RelId, RoleId, internal_error};
use blossom_ir::core::{EventSource, Placement, RelClass};
use blossom_oracle::{Oracle, Row};
use blossom_value::Value;
use blossom_value::time::{Duration, Instant, NodeId, Tick};
use blossom_value::value::IntValue;

use crate::sync::{CrashView, FaultSchedule, SimError, SyncConfig, SyncRun, SyncWorld, now_at};

/// An input event: `row` in the root input `rel` at `node`, in round `tick`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InputEvent {
    pub node: NodeId,
    pub tick: Tick,
    pub rel: RelId,
    pub row: Row,
}

/// A compiled `.bls` program ready to run.
pub struct BlsSim<'a> {
    artifact: &'a BlsArtifact,
    oracle: Oracle,
    /// Physical timers: relation, period, and the role they are placed at.
    timers: Vec<(RelId, Duration, Option<RoleId>)>,
    boot: Option<RelId>,
}

impl<'a> BlsSim<'a> {
    /// Prepares the program (stratifies it and plans every rule) for a run seeded with `seed` (SEM-084).
    pub fn new(artifact: &'a BlsArtifact, seed: blossom_value::Seed) -> Result<BlsSim<'a>, SimError> {
        let oracle = Oracle::new(artifact.program.clone())
            .map_err(SimError::Load)?
            .with_roles(artifact.roles.clone())
            .with_seed(seed)
            .map_err(SimError::Load)?;
        let mut timers = Vec::new();
        for (id, r) in artifact.program.get().rels.iter_enumerated() {
            if let RelClass::Event(EventSource::Timer(t)) = &r.class {
                let Some(every) = t.every else {
                    return Err(blossom_base::unimplemented_error!(
                        "LANG-173",
                        "timers without a period in the simulator"
                    )
                    .into());
                };
                if t.ticks.is_some() || t.times.is_some() || t.once_after.is_some() || t.once {
                    return Err(blossom_base::unimplemented_error!(
                        "LANG-173",
                        "bounded, logical and one-shot timers in the simulator"
                    )
                    .into());
                }
                let role = match r.placement {
                    Placement::Role(role) => Some(role),
                    Placement::Shared => None,
                };
                timers.push((id, every, role));
            }
        }
        Ok(BlsSim {
            artifact,
            oracle,
            timers,
            boot: artifact.boot(),
        })
    }

    pub fn artifact(&self) -> &BlsArtifact {
        self.artifact
    }

    pub fn oracle(&self) -> &Oracle {
        &self.oracle
    }

    /// Runs rounds `0..=last` with the given input events under `faults`.
    pub fn run(
        &self,
        inputs: &[InputEvent],
        last: Tick,
        round: Duration,
        faults: &FaultSchedule,
        capture: bool,
    ) -> Result<SyncRun, SimError> {
        let n = u32::try_from(self.artifact.nodes.len()).map_err(|_| internal_error!("too many nodes"))?;
        let mut world = SyncWorld::new(&self.oracle, n);
        for (node, tick, rel, row) in self.runtime_events(last, round)? {
            world.input(node, tick, rel, row);
        }
        for e in inputs {
            world.input(e.node, e.tick, e.rel, e.row.clone());
        }
        world.run(
            &SyncConfig {
                first: Tick(0),
                last,
                crash_view: CrashView::Frozen,
                round,
                capture,
                halt: self.artifact.halt,
            },
            faults,
        )
    }

    /// `boot()` at tick 0 on every node, and the timers' firings.
    pub fn runtime_events(&self, last: Tick, round: Duration) -> Result<Vec<(NodeId, Tick, RelId, Row)>, SimError> {
        let mut out = Vec::new();
        for (i, role) in self.artifact.roles.iter().enumerate() {
            let node = NodeId(u32::try_from(i).map_err(|_| internal_error!("too many nodes"))?);
            if let Some(boot) = self.boot {
                out.push((node, Tick(0), boot, Arc::from(Vec::new())));
            }
            for (rel, every, placed) in &self.timers {
                if placed.is_some() && placed != role {
                    continue;
                }
                let period = every.as_nanos();
                if period <= 0 {
                    return Err(internal_error!("a timer with a non-positive period").into());
                }
                let mut k: i64 = 0;
                for t in 1..=last.0 {
                    let now = now_at(round, Tick(t))?;
                    loop {
                        let due = k
                            .checked_add(1)
                            .and_then(|k1| k1.checked_mul(period))
                            .ok_or_else(|| internal_error!("timer arithmetic overflows"))?;
                        if due > now.0 {
                            break;
                        }
                        let count = u64::try_from(k).map_err(|_| internal_error!("negative timer count"))?;
                        out.push((
                            node,
                            Tick(t),
                            *rel,
                            Arc::from(vec![Value::Int(IntValue::U64(count)), Value::Instant(Instant(due))]),
                        ));
                        k += 1;
                    }
                }
            }
        }
        Ok(out)
    }
}
