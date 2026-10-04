//! How long something the command does takes: the replay of a tick (`trace replay --slow`), the phases of an LDFI
//! search (`ldfi --progress`).
//!
//! Exempt from the ambient-time rule (clippy.toml): it measures the command's own process; nothing it reads reaches
//! a tick, a replay or a search's results.
#![allow(clippy::disallowed_methods)]

use std::time::Instant;

pub struct Stopwatch(Instant);

impl Stopwatch {
    pub fn start() -> Stopwatch {
        Stopwatch(Instant::now())
    }

    /// Nanoseconds since the start.
    pub fn nanos(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}
