//! Wall-clock measurement for benchmarks and client workloads.
//!
//! Exempt from the ambient-time rule (clippy.toml): a client measuring latency and a workload running for a fixed
//! time need a stopwatch. Nothing here feeds a node.
#![allow(clippy::disallowed_methods)]

use std::time::{Duration, Instant};

/// Time since the stopwatch started.
#[derive(Clone, Copy, Debug)]
pub struct Stopwatch(Instant);

impl Stopwatch {
    pub fn start() -> Stopwatch {
        Stopwatch(Instant::now())
    }

    pub fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }

    /// Nanoseconds since the start.
    pub fn nanos(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}
