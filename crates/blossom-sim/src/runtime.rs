//! The runtime events of a Blossom program in the synchronous-round world: `boot()` in every node's first tick
//! (tick 0, SEM-012) and each physical timer's firings, on a clock of `round` per tick (LANGUAGE §15.2).
//!
//! A timer `every d` fires once per period: firing `k` (counted from 0) is due at `(k + 1) × d` and is delivered in
//! the first tick whose clock has reached it, as the row `(k, (k + 1) × d)`. A timer placed at a role fires only on
//! that role's nodes.

use std::sync::Arc;

use blossom_base::{RelId, RoleId, internal_error};
use blossom_ir::core::{EventSource, Placement, Program, RelClass};
use blossom_oracle::Row;
use blossom_value::Value;
use blossom_value::time::{Duration, Instant, NodeId, Tick};
use blossom_value::value::IntValue;

use crate::sync::{SimError, now_at};

/// A program's runtime-fed relations.
#[derive(Clone, Debug, Default)]
pub struct Runtime {
    boot: Option<RelId>,
    /// Physical timers: relation, period, and the role they are placed at.
    timers: Vec<(RelId, Duration, Option<RoleId>)>,
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
                    rt.timers.push((id, every, role));
                }
                _ => {}
            }
        }
        Ok(rt)
    }

    /// The runtime events of `node` (whose role is `role`) at `tick`.
    pub fn events_at(
        &self,
        node_role: Option<RoleId>,
        tick: Tick,
        round: Duration,
    ) -> Result<Vec<(RelId, Row)>, SimError> {
        let mut out = Vec::new();
        if tick == Tick(0)
            && let Some(boot) = self.boot
        {
            out.push((boot, Arc::from(Vec::new())));
        }
        if tick == Tick(0) {
            return Ok(out);
        }
        let before = now_at(round, Tick(tick.0 - 1))?.0;
        let now = now_at(round, tick)?.0;
        for (rel, every, placed) in &self.timers {
            if placed.is_some() && *placed != node_role {
                continue;
            }
            let period = every.as_nanos();
            // Firings k with (k + 1) × period in (before, now].
            let first = before / period;
            let last = now / period;
            for k1 in (first + 1)..=last {
                let count = u64::try_from(k1 - 1).map_err(|_| internal_error!("negative timer count"))?;
                let due = k1
                    .checked_mul(period)
                    .ok_or_else(|| internal_error!("timer arithmetic overflows"))?;
                out.push((
                    *rel,
                    Arc::from(vec![Value::Int(IntValue::U64(count)), Value::Instant(Instant(due))]),
                ));
            }
        }
        Ok(out)
    }
}

/// The role of node `node` in a deployment (`None` when the program has no roles).
pub fn role_of(roles: &[Option<RoleId>], node: NodeId) -> Option<RoleId> {
    roles.get(node.0 as usize).copied().flatten()
}
