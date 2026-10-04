//! The runtime events of a Blossom program in the synchronous-round world: `boot()` in every node's first tick
//! (tick 0, SEM-012) and each physical timer's firings, on a clock of `round` per tick (LANGUAGE §15.2).
//!
//! A timer `every d` fires once per period, counted from the node's boot: firing `k` (from 0) is due at
//! `boot + (k + 1) × d` and is delivered in the first tick whose clock has reached it, as the row `(k, due)`, the
//! node runtime's rule (`blossom_node::timers`). A timer placed at a role fires only on that role's nodes. In a run the
//! round loop makes the firings ([`crate::sync::Timer`]): a guarded timer's depend on each node's state, and every
//! timer starts counting again at a restart.

use std::sync::Arc;

use blossom_base::{RelId, RoleId, internal_error};
use blossom_ir::core::{EventSource, Placement, Program, RelClass};
use blossom_oracle::Row;
use blossom_value::Value;
use blossom_value::time::{Duration, Instant, NodeId, Tick};
use blossom_value::value::IntValue;

use crate::sync::{SimError, Timer, now_at};

/// A program's runtime-fed relations.
#[derive(Clone, Debug, Default)]
pub struct Runtime {
    boot: Option<RelId>,
    /// Physical timers: relation, period, the role they are placed at, and the guard of a guarded one.
    timers: Vec<(RelId, Duration, Option<RoleId>, Option<RelId>)>,
}

impl Runtime {
    /// The runtime relations of `program`. Timers other than `every d` fail with `Unimplemented`.
    pub fn of(program: &Program) -> Result<Runtime, SimError> {
        let mut rt = Runtime::default();
        for (id, r) in program.rels.iter_enumerated() {
            match &r.class {
                RelClass::Event(EventSource::Boot) => rt.boot = Some(id),
                RelClass::Event(EventSource::Timer(t)) => {
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
                    if every.as_nanos() <= 0 {
                        return Err(internal_error!("a timer with a non-positive period").into());
                    }
                    let role = match r.placement {
                        Placement::Role(role) => Some(role),
                        Placement::Shared => None,
                    };
                    rt.timers.push((id, every, role, t.guard));
                }
                _ => {}
            }
        }
        Ok(rt)
    }

    /// `boot()` at tick 0.
    pub fn boot_at(&self, tick: Tick) -> Option<(RelId, Row)> {
        self.boot
            .filter(|_| tick == Tick(0))
            .map(|b| (b, Arc::from(Vec::new())))
    }

    /// The unguarded timers' firings at `tick` on a first incarnation of a node whose role is `node_role` (for
    /// stepping one round at a time; a run makes the firings in its round loop).
    pub fn timer_firings_at(
        &self,
        node_role: Option<RoleId>,
        tick: Tick,
        round: Duration,
    ) -> Result<Vec<(RelId, Row)>, SimError> {
        let mut out = Vec::new();
        if tick == Tick(0) {
            return Ok(out);
        }
        let before = now_at(round, Tick(tick.0 - 1))?.0;
        let now = now_at(round, tick)?.0;
        for (rel, every, placed, guard) in &self.timers {
            if guard.is_some() || (placed.is_some() && *placed != node_role) {
                continue;
            }
            out.extend(firings(*rel, *every, 0, before, now)?);
        }
        Ok(out)
    }

    /// Whether the program has a guarded timer (whose firings depend on each node's previous round).
    pub fn has_guarded(&self) -> bool {
        self.timers.iter().any(|t| t.3.is_some())
    }

    /// The timers, with the nodes they run on (`roles` gives each node's role), for a run's round loop.
    pub fn timers(&self, roles: &[Option<RoleId>]) -> Result<Vec<Timer>, SimError> {
        let mut out = Vec::new();
        for (rel, every, placed, guard) in &self.timers {
            let mut nodes = Vec::new();
            for (i, role) in roles.iter().enumerate() {
                if placed.is_none() || placed == role {
                    nodes.push(NodeId(u32::try_from(i).map_err(|_| internal_error!("too many nodes"))?));
                }
            }
            out.push(Timer {
                rel: *rel,
                every: *every,
                guard: *guard,
                nodes,
            });
        }
        Ok(out)
    }
}

/// Timer `rel`'s firings due in `(before, now]` on the timeline of a node booted at `boot` (`boot <= before`):
/// firing `k` is due at `boot + (k + 1) × every`.
pub fn firings(rel: RelId, every: Duration, boot: i64, before: i64, now: i64) -> Result<Vec<(RelId, Row)>, SimError> {
    let period = every.as_nanos();
    if period <= 0 {
        return Err(internal_error!("a timer with a non-positive period").into());
    }
    if before < boot {
        return Err(internal_error!("timer firings asked for before the node's boot").into());
    }
    let mut out = Vec::new();
    let first = (before - boot) / period;
    let last = (now - boot) / period;
    for k1 in (first + 1)..=last {
        let count = u64::try_from(k1 - 1).map_err(|_| internal_error!("negative timer count"))?;
        let due = k1
            .checked_mul(period)
            .and_then(|d| d.checked_add(boot))
            .ok_or_else(|| internal_error!("timer arithmetic overflows"))?;
        out.push((
            rel,
            Arc::from(vec![Value::Int(IntValue::U64(count)), Value::Instant(Instant(due))]),
        ));
    }
    Ok(out)
}

/// The role of node `node` in a deployment (`None` when the program has no roles).
pub fn role_of(roles: &[Option<RoleId>], node: NodeId) -> Option<RoleId> {
    roles.get(node.0 as usize).copied().flatten()
}
