//! Physical timers (ARCHITECTURE §5.5, LANGUAGE §15.2).
//!
//! A timer `every d` fires once per period, counted from the node's boot: firing `k` (from 0) is due at
//! `boot + (k + 1) × d` and is delivered in the first tick whose clock has reached it, as the row `(k, due)`. That is
//! the simulator's rule with the boot instant as the origin, so a program behaves the same on the network and in the
//! synchronous world. A timer placed at a role fires only on that role's nodes. Each incarnation starts counting
//! from 0 at its own boot.

use std::sync::Arc;

use blossom_base::{RelId, RoleId, internal_error};
use blossom_ir::core::{EventSource, Placement, Program, RelClass};
use blossom_oracle::Row;
use blossom_value::Value;
use blossom_value::time::Instant;
use blossom_value::value::IntValue;

use crate::NodeError;

#[derive(Clone, Debug)]
struct Timer {
    rel: RelId,
    period: i64,
    /// The next firing's count.
    next: u64,
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
    pub fn new(program: &Program, role: Option<RoleId>, boot: Instant) -> Result<TimerTable, NodeError> {
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
            });
        }
        Ok(TimerTable { boot, timers })
    }

    /// The earliest instant a timer is due.
    pub fn next_deadline(&self) -> Result<Option<Instant>, NodeError> {
        let mut best: Option<i64> = None;
        for t in &self.timers {
            let d = due_at(self.boot, t, t.next)?;
            best = Some(best.map_or(d, |b| b.min(d)));
        }
        Ok(best.map(Instant))
    }

    /// Whether a timer is due at `now`.
    pub fn any_due(&self, now: Instant) -> Result<bool, NodeError> {
        Ok(self.next_deadline()?.is_some_and(|d| d <= now))
    }

    /// Takes every firing due at `now`, in timer order then count order.
    pub fn fire(&mut self, now: Instant) -> Result<Vec<(RelId, Row)>, NodeError> {
        let mut out = Vec::new();
        let boot = self.boot;
        for t in &mut self.timers {
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

/// When firing `k` of `t` is due: `boot + (k + 1) × period`.
fn due_at(boot: Instant, t: &Timer, k: u64) -> Result<i64, NodeError> {
    let k1 = i64::try_from(k).ok().and_then(|k| k.checked_add(1));
    k1.and_then(|k1| k1.checked_mul(t.period))
        .and_then(|off| off.checked_add(boot.0))
        .ok_or_else(|| internal_error!("timer arithmetic overflows").into())
}
