//! Solver resource budgets intentionally measure wall time at the external solver boundary.
//! Wall time controls cancellation only and never selects a model or changes a SAT verdict.
#![allow(clippy::disallowed_methods)]
use crate::{LimitHit, SolveLimits};
use std::time::Instant;
pub(crate) struct Budget {
    start: Instant,
    limits: SolveLimits,
}
impl Budget {
    pub fn new(limits: &SolveLimits) -> Self {
        Self {
            start: Instant::now(),
            limits: limits.clone(),
        }
    }
    pub fn hit(&self, conflicts: u64) -> Option<LimitHit> {
        if self.limits.time.is_some_and(|t| self.start.elapsed() >= t) {
            Some(LimitHit::Time)
        } else if self.limits.conflicts.is_some_and(|n| conflicts >= n) {
            Some(LimitHit::Conflicts)
        } else {
            None
        }
    }
}
