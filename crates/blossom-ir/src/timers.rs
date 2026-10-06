//! Physical timers (ARCHITECTURE §5.5, LANGUAGE §15.2): when each fires, for any host that runs a program on a clock
//! (a node, the browser host).
//!
//! A timer `every d` fires once per period, counted from the node's boot: firing `k` (from 0) is due at
//! `boot + (k + 1) × d` and is delivered in the first tick whose clock has reached it, as the row `(k, due)`. That is
//! the simulator's rule with the boot instant as the origin, so a program behaves the same on the network and in the
//! synchronous world. A timer placed at a role fires only on that role's nodes. Each incarnation starts counting
//! from 0 at its own boot.
//!
//! A guarded timer (`every d while G`) is dormant while `G` was empty at the end of the node's latest tick: it is not
//! due and wakes the node for nothing. When a tick ends with `G` holding, it fires again from its first firing after
//! that tick (the firings it missed are skipped, not delivered late), so its count still says where on the boot
//! timeline a firing is. Before the first tick every guarded timer is dormant (the boot tick decides).

use std::collections::BTreeMap;
use std::sync::Arc;

use blossom_base::error::{InternalError, Unimplemented};
use blossom_base::{RelId, RoleId, internal_error};
use blossom_value::Value;
use blossom_value::time::Instant;
use blossom_value::value::IntValue;

use crate::core::{EventSource, Placement, Program, RelClass};
use crate::tick::Row;

/// A timer table's failure: a kind of timer this build does not run, or a broken invariant.
#[derive(Debug, thiserror::Error)]
pub enum TimerError {
    #[error(transparent)]
    Unimplemented(#[from] Unimplemented),
    #[error(transparent)]
    Internal(#[from] InternalError),
}

#[derive(Clone, Debug)]
struct Timer {
    rel: RelId,
    period: i64,
    /// The next firing's count.
    next: u64,
    /// `while G`: the guard, and whether it held at the end of the latest tick.
    guard: Option<RelId>,
    held: bool,
}

impl Timer {
    fn active(&self) -> bool {
        self.guard.is_none() || self.held
    }
}

/// The node's physical timers.
#[derive(Clone, Debug)]
pub struct TimerTable {
    boot: Instant,
    timers: Vec<Timer>,
}

impl TimerTable {
    /// The timers of `program` that run on a node of `role`, anchored at `boot`. Timers other than `every d` fail
    /// with `Unimplemented`.
    pub fn new(program: &Program, role: Option<RoleId>, boot: Instant) -> Result<TimerTable, TimerError> {
        let mut timers = Vec::new();
        for (id, r) in program.rels.iter_enumerated() {
            let RelClass::Event(EventSource::Timer(t)) = &r.class else {
                continue;
            };
            if let Placement::Role(placed) = r.placement
                && Some(placed) != role
            {
                continue;
            }
            let Some(every) = t.every else {
                return Err(blossom_base::unimplemented_error!("LANG-173", "timers without a period on a node").into());
            };
            if t.ticks.is_some() || t.times.is_some() || t.once_after.is_some() || t.once {
                return Err(blossom_base::unimplemented_error!(
                    "LANG-173",
                    "bounded, logical and one-shot timers on a node"
                )
                .into());
            }
            let period = every.as_nanos();
            if period <= 0 {
                return Err(internal_error!("a timer with a non-positive period").into());
            }
            timers.push(Timer {
                rel: id,
                period,
                next: 0,
                guard: t.guard,
                held: false,
            });
        }
        Ok(TimerTable { boot, timers })
    }

    /// The guards the node must observe at the end of each tick (`observe`).
    pub fn guards(&self) -> impl Iterator<Item = RelId> + '_ {
        self.timers.iter().filter_map(|t| t.guard)
    }

    /// Records, after a tick at `now`, whether each guard holds (`observed` has every guard's final rows). A guard
    /// that comes to hold resumes its timer at its first firing after `now`.
    pub fn observe(&mut self, now: Instant, observed: &BTreeMap<RelId, Vec<Row>>) -> Result<(), TimerError> {
        let boot = self.boot;
        for t in &mut self.timers {
            let Some(g) = t.guard else { continue };
            let holds = observed
                .get(&g)
                .ok_or_else(|| internal_error!("a timer guard was not observed"))?
                .iter()
                .next()
                .is_some();
            if holds && !t.held {
                t.next = first_after(boot, t, now)?.max(t.next);
            }
            t.held = holds;
        }
        Ok(())
    }

    /// The earliest instant an active timer is due.
    pub fn next_deadline(&self) -> Result<Option<Instant>, TimerError> {
        let mut best: Option<i64> = None;
        for t in self.timers.iter().filter(|t| t.active()) {
            let d = due_at(self.boot, t, t.next)?;
            best = Some(best.map_or(d, |b| b.min(d)));
        }
        Ok(best.map(Instant))
    }

    /// Whether a timer is due at `now`.
    pub fn any_due(&self, now: Instant) -> Result<bool, TimerError> {
        Ok(self.next_deadline()?.is_some_and(|d| d <= now))
    }

    /// Takes every firing of an active timer due at `now`, in timer order then count order.
    pub fn fire(&mut self, now: Instant) -> Result<Vec<(RelId, Row)>, TimerError> {
        let mut out = Vec::new();
        let boot = self.boot;
        for t in self.timers.iter_mut().filter(|t| t.active()) {
            loop {
                let due = due_at(boot, t, t.next)?;
                if due > now.0 {
                    break;
                }
                out.push((
                    t.rel,
                    Arc::from(vec![Value::Int(IntValue::U64(t.next)), Value::Instant(Instant(due))]),
                ));
                t.next = t
                    .next
                    .checked_add(1)
                    .ok_or_else(|| internal_error!("timer count overflows"))?;
            }
        }
        Ok(out)
    }
}

/// The count of `t`'s first firing due after `now`: the least `k` with `boot + (k + 1) × period > now`.
fn first_after(boot: Instant, t: &Timer, now: Instant) -> Result<u64, TimerError> {
    let since = now.0.saturating_sub(boot.0).max(0);
    u64::try_from(since / t.period).map_err(|_| internal_error!("timer arithmetic overflows").into())
}

/// When firing `k` of `t` is due: `boot + (k + 1) × period`.
fn due_at(boot: Instant, t: &Timer, k: u64) -> Result<i64, TimerError> {
    let k1 = i64::try_from(k).ok().and_then(|k| k.checked_add(1));
    k1.and_then(|k1| k1.checked_mul(t.period))
        .and_then(|off| off.checked_add(boot.0))
        .ok_or_else(|| internal_error!("timer arithmetic overflows").into())
}
