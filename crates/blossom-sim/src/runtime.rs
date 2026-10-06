//! The runtime events of a Blossom program in the synchronous-round world: `boot()` in every node's first tick
//! (tick 0, SEM-012) and each timer's firings, on a clock of `round` per tick (LANGUAGE §15.2).
//!
//! Every node runs one tick per round, so a timer fires by the node runtime's rule ([`blossom_ir::timers`]): firing
//! `k` of `every d` is due at `boot + (k + 1) × d` and is delivered in the first round whose clock has reached it,
//! `once` fires in the boot round, and `every n ticks` in the incarnation's round `(k + 1) × n − 1`. A timer placed at
//! a role fires only on that role's nodes. In a run the round loop makes the firings ([`crate::sync::Timer`]): a
//! guarded timer's depend on each node's state, and every timer starts counting again at a restart.

use std::sync::Arc;

use blossom_base::{RelId, RoleId, internal_error};
use blossom_ir::core::{EventSource, Placement, Program, RelClass};
use blossom_ir::timers::Schedule;
use blossom_oracle::Row;
use blossom_value::Value;
use blossom_value::time::{Duration, NodeId, Tick};
use blossom_value::value::IntValue;

use crate::sync::{SimError, Timer, now_at};

/// A program's runtime-fed relations.
#[derive(Clone, Debug, Default)]
pub struct Runtime {
    boot: Option<RelId>,
    /// Timers: relation, schedule, the role they are placed at, and the guard of a guarded one.
    timers: Vec<(RelId, Schedule, Option<RoleId>, Option<RelId>)>,
}

impl Runtime {
    /// The runtime relations of `program`.
    pub fn of(program: &Program) -> Result<Runtime, SimError> {
        let mut rt = Runtime::default();
        for (id, r) in program.rels.iter_enumerated() {
            match &r.class {
                RelClass::Event(EventSource::Boot) => rt.boot = Some(id),
                RelClass::Event(EventSource::Timer(t)) => {
                    let schedule = Schedule::of(t).map_err(timer_error)?;
                    let role = match r.placement {
                        Placement::Role(role) => Some(role),
                        Placement::Shared => None,
                    };
                    rt.timers.push((id, schedule, role, t.guard));
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
        for (rel, schedule, placed, guard) in &self.timers {
            if guard.is_some() || (placed.is_some() && *placed != node_role) {
                continue;
            }
            out.extend(firings(*rel, schedule, Tick(0), tick, round)?);
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
        for (rel, schedule, placed, guard) in &self.timers {
            let mut nodes = Vec::new();
            for (i, role) in roles.iter().enumerate() {
                if placed.is_none() || placed == role {
                    nodes.push(NodeId(u32::try_from(i).map_err(|_| internal_error!("too many nodes"))?));
                }
            }
            out.push(Timer {
                rel: *rel,
                schedule: *schedule,
                guard: *guard,
                nodes,
            });
        }
        Ok(out)
    }
}

/// Timer `rel`'s firings in round `tick` of an incarnation that booted in round `boot` (`boot <= tick`), on a clock
/// of `round` per round.
pub fn firings(
    rel: RelId,
    schedule: &Schedule,
    boot: Tick,
    tick: Tick,
    round: Duration,
) -> Result<Vec<(RelId, Row)>, SimError> {
    let local = tick
        .0
        .checked_sub(boot.0)
        .ok_or_else(|| internal_error!("timer firings asked for before the node's boot"))?;
    let before = match tick.0.checked_sub(1) {
        Some(b) if local > 0 => Some(now_at(round, Tick(b))?),
        _ => None,
    };
    let found = schedule
        .firings(now_at(round, boot)?, local, before, now_at(round, tick)?)
        .map_err(timer_error)?;
    Ok(found
        .into_iter()
        .map(|(k, at)| {
            let row: Row = Arc::from(vec![Value::Int(IntValue::U64(k)), Value::Instant(at)]);
            (rel, row)
        })
        .collect())
}

fn timer_error(e: blossom_ir::timers::TimerError) -> SimError {
    match e {
        blossom_ir::timers::TimerError::Unimplemented(u) => u.into(),
        blossom_ir::timers::TimerError::Internal(i) => i.into(),
    }
}

/// The role of node `node` in a deployment (`None` when the program has no roles).
pub fn role_of(roles: &[Option<RoleId>], node: NodeId) -> Option<RoleId> {
    roles.get(node.0 as usize).copied().flatten()
}
