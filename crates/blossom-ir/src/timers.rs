//! Timers (ARCHITECTURE §5.5, LANGUAGE §15.2): when each fires, for every host that runs a program (a node, the
//! browser host, the synchronous simulator).
//!
//! Firings are counted from the incarnation's boot, and firing `k` (from 0) is delivered as the row `(k, at)`:
//!
//! - `every d`: firing `k` is due at `boot + (k + 1) × d` and is delivered in the first tick whose clock has reached
//!   it, `at` its due time. `once after d` is `every d times 1`.
//! - `once`: firing 0 in the boot tick, `at` that tick's time (`start(0, $now) :- boot()`).
//! - `every n ticks` (logical): firing `k` in the incarnation's tick `(k + 1) × n − 1`, the boot tick being tick 0, `at`
//!   that tick's time. Until it is spent, a logical timer keeps the node ticking: its counter is a staged change.
//! - `times m` keeps the firings numbered `0` to `m − 1`; after them the timer is spent.
//!
//! That is the same rule on the network and in the synchronous world, so a program behaves the same in both. A timer
//! placed at a role fires only on that role's nodes, and each incarnation starts counting from 0 at its own boot.
//!
//! A guarded timer (`while G`) is dormant while `G` was empty at the end of the node's latest tick: it delivers
//! nothing, and a dormant physical timer wakes the node for nothing. When a tick ends with `G` holding, the timer
//! fires again from its first firing after that tick: the firings it missed are skipped, not delivered late, and they
//! count toward `times`, so a count still says where on the boot timeline a firing is. Before the first tick every
//! guarded timer is dormant (the boot tick decides).

use std::collections::BTreeMap;
use std::sync::Arc;

use blossom_base::error::{InternalError, Unimplemented};
use blossom_base::{RelId, RoleId, internal_error};
use blossom_value::Value;
use blossom_value::time::Instant;
use blossom_value::value::IntValue;

use crate::core::{EventSource, Placement, Program, RelClass, TimerClock, TimerDecl};
use crate::tick::Row;

/// A timer table's failure: a kind of timer this build does not run, or a broken invariant.
#[derive(Debug, thiserror::Error)]
pub enum TimerError {
    #[error(transparent)]
    Unimplemented(#[from] Unimplemented),
    #[error(transparent)]
    Internal(#[from] InternalError),
}

/// How a timer's firings are spaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cadence {
    /// Physical: firing `k` is due at `boot + (k + 1) × period` (nanoseconds).
    Every { period: i64 },
    /// `once`: firing 0 in the boot tick.
    Boot,
    /// Logical: firing `k` in the incarnation's tick `(k + 1) × every − 1`.
    Ticks { every: u64 },
}

/// When a timer fires: its cadence, and how many firings it has (`times m`; `once` has one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schedule {
    pub cadence: Cadence,
    pub limit: Option<u64>,
}

impl Schedule {
    /// The schedule a declaration gives (exactly one of `every`, `ticks`, `once_after` and `once`, LANGUAGE §15.2);
    /// an ill-formed one is a bug of the compiler that built it (the validator refuses it: [`Schedule::shape`]).
    pub fn of(t: &TimerDecl) -> Result<Schedule, TimerError> {
        Schedule::shape(t).ok_or_else(|| internal_error!("an ill-formed timer declaration: {t:?}").into())
    }

    /// The schedule of a well-formed declaration: one of `every d [times m]` (physical), `every n ticks [times m]`
    /// (logical), `once after d` and `once` (with no guard: it fires before a guard can hold), with positive values.
    pub fn shape(t: &TimerDecl) -> Option<Schedule> {
        let positive = |d: blossom_value::time::Duration| Some(d.as_nanos()).filter(|n| *n > 0);
        let (cadence, limit) = match (&t.clock, t.every, t.ticks, t.once_after, t.once) {
            (TimerClock::Physical, Some(d), None, None, false) => (Cadence::Every { period: positive(d)? }, t.times),
            (TimerClock::Logical, None, Some(n), None, false) if n > 0 => (Cadence::Ticks { every: n }, t.times),
            (TimerClock::Physical, None, None, Some(d), false) if t.times.is_none() => {
                (Cadence::Every { period: positive(d)? }, Some(1))
            }
            (TimerClock::Physical, None, None, None, true) if t.times.is_none() && t.guard.is_none() => {
                (Cadence::Boot, Some(1))
            }
            _ => return None,
        };
        (limit != Some(0)).then_some(Schedule { cadence, limit })
    }

    fn within(&self, k: u64) -> bool {
        self.limit.is_none_or(|m| k < m)
    }

    /// The firings `(count, at)` in the incarnation's tick numbered `local` (0 is the boot tick), run at `now` after
    /// a tick at `before` (`None` for the boot tick). A guarded timer's firings are the tick's only if its guard held
    /// at the end of the tick at `before`.
    pub fn firings(
        &self,
        boot: Instant,
        local: u64,
        before: Option<Instant>,
        now: Instant,
    ) -> Result<Vec<(u64, Instant)>, TimerError> {
        let mut out = Vec::new();
        match self.cadence {
            Cadence::Every { period } => {
                let mut k = match before {
                    Some(b) => first_after(boot, period, b)?,
                    None => 0,
                };
                while self.within(k) {
                    let due = due_at(boot, period, k)?;
                    if due > now.0 {
                        break;
                    }
                    out.push((k, Instant(due)));
                    k = k
                        .checked_add(1)
                        .ok_or_else(|| internal_error!("timer count overflows"))?;
                }
            }
            Cadence::Boot => {
                if local == 0 {
                    out.push((0, now));
                }
            }
            Cadence::Ticks { every } => {
                let ran = local
                    .checked_add(1)
                    .ok_or_else(|| internal_error!("tick count overflows"))?;
                if ran % every == 0 {
                    let k = ran / every - 1;
                    if self.within(k) {
                        out.push((k, now));
                    }
                }
            }
        }
        Ok(out)
    }

    /// When the timer is next due, after `ran` ticks of the incarnation, the latest at `last` (`None` before the
    /// boot tick): the instant of its next physical firing, `last` (or `boot`) for a logical timer, which is due in
    /// every tick until it is spent, and `None` once it is spent.
    pub fn next_due(&self, boot: Instant, ran: u64, last: Option<Instant>) -> Result<Option<Instant>, TimerError> {
        Ok(match self.cadence {
            Cadence::Every { period } => {
                let k = match last {
                    Some(l) => first_after(boot, period, l)?,
                    None => 0,
                };
                if self.within(k) {
                    Some(Instant(due_at(boot, period, k)?))
                } else {
                    None
                }
            }
            Cadence::Boot => (ran == 0).then_some(boot),
            // The next firing is the one of the first tick `t ≥ ran` with `(t + 1) % every == 0`: number `ran / every`.
            Cadence::Ticks { every } => self.within(ran / every).then(|| last.unwrap_or(boot)),
        })
    }
}

#[derive(Clone, Debug)]
struct Timer {
    rel: RelId,
    schedule: Schedule,
    /// `while G`: the guard, and whether it held at the end of the latest tick.
    guard: Option<RelId>,
    held: bool,
}

impl Timer {
    fn dormant(&self) -> bool {
        self.guard.is_some() && !self.held
    }
}

/// A node's timers, for a host that runs its ticks one at a time: [`TimerTable::fire`] is called once in every tick,
/// before it runs, and [`TimerTable::observe`] after it.
#[derive(Clone, Debug)]
pub struct TimerTable {
    boot: Instant,
    /// The ticks run so far (the next tick's number).
    ran: u64,
    /// The latest tick's clock.
    last: Option<Instant>,
    timers: Vec<Timer>,
}

impl TimerTable {
    /// The timers of `program` that run on a node of `role`, anchored at `boot`.
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
            timers.push(Timer {
                rel: id,
                schedule: Schedule::of(t)?,
                guard: t.guard,
                held: false,
            });
        }
        Ok(TimerTable {
            boot,
            ran: 0,
            last: None,
            timers,
        })
    }

    /// The guards the node must observe at the end of each tick (`observe`).
    pub fn guards(&self) -> impl Iterator<Item = RelId> + '_ {
        self.timers.iter().filter_map(|t| t.guard)
    }

    /// Records, after a tick, whether each guard holds (`observed` has every guard's final rows). A guard that comes
    /// to hold resumes its timer at its first firing after that tick.
    pub fn observe(&mut self, observed: &BTreeMap<RelId, Vec<Row>>) -> Result<(), TimerError> {
        for t in &mut self.timers {
            let Some(g) = t.guard else { continue };
            t.held = observed
                .get(&g)
                .ok_or_else(|| internal_error!("a timer guard was not observed"))?
                .iter()
                .next()
                .is_some();
        }
        Ok(())
    }

    /// The earliest instant a timer is due: a physical timer's next firing, or, while a logical timer is not spent,
    /// the latest tick's clock (the node ticks again at once). A dormant physical timer is never due.
    pub fn next_deadline(&self) -> Result<Option<Instant>, TimerError> {
        let mut best: Option<Instant> = None;
        for t in &self.timers {
            // A logical timer's counter runs while it is dormant: only its delivery waits for the guard.
            if t.dormant() && !matches!(t.schedule.cadence, Cadence::Ticks { .. }) {
                continue;
            }
            if let Some(d) = t.schedule.next_due(self.boot, self.ran, self.last)? {
                best = Some(best.map_or(d, |b| b.min(d)));
            }
        }
        Ok(best)
    }

    /// Whether a timer is due at `now`.
    pub fn any_due(&self, now: Instant) -> Result<bool, TimerError> {
        Ok(self.next_deadline()?.is_some_and(|d| d <= now))
    }

    /// The firings of the tick run at `now`, in timer order then count order; counts the tick.
    pub fn fire(&mut self, now: Instant) -> Result<Vec<(RelId, Row)>, TimerError> {
        if self.last.is_some_and(|l| now < l) {
            return Err(internal_error!("the timer clock went backwards").into());
        }
        let mut out = Vec::new();
        for t in self.timers.iter().filter(|t| !t.dormant()) {
            for (k, at) in t.schedule.firings(self.boot, self.ran, self.last, now)? {
                out.push((t.rel, Arc::from(vec![Value::Int(IntValue::U64(k)), Value::Instant(at)])));
            }
        }
        self.ran = self
            .ran
            .checked_add(1)
            .ok_or_else(|| internal_error!("tick count overflows"))?;
        self.last = Some(now);
        Ok(out)
    }
}

/// The count of the first firing due after `now`: the least `k` with `boot + (k + 1) × period > now`.
fn first_after(boot: Instant, period: i64, now: Instant) -> Result<u64, TimerError> {
    let since = now.0.saturating_sub(boot.0).max(0);
    u64::try_from(since / period).map_err(|_| internal_error!("timer arithmetic overflows").into())
}

/// When firing `k` is due: `boot + (k + 1) × period`.
fn due_at(boot: Instant, period: i64, k: u64) -> Result<i64, TimerError> {
    let k1 = i64::try_from(k).ok().and_then(|k| k.checked_add(1));
    k1.and_then(|k1| k1.checked_mul(period))
        .and_then(|off| off.checked_add(boot.0))
        .ok_or_else(|| internal_error!("timer arithmetic overflows").into())
}
