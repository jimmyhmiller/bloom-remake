//! The real clock and entropy (ARCHITECTURE §5.5).
//!
//! This module is one of the two places allowed to read ambient time (clippy.toml): the node sees time only through
//! [`Clock`], sampled once per tick by the driver.
#![allow(clippy::disallowed_methods)]

use std::io::Read;

use blossom_node::env::{Clock, Entropy};
use blossom_value::time::Instant;

/// The wall clock now, in nanoseconds since the Unix epoch (the deployment epoch).
pub fn wall_now() -> Result<Instant, String> {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("the wall clock is before 1970: {e}"))?;
    i64::try_from(d.as_nanos())
        .map(Instant)
        .map_err(|_| "the wall clock overflows".to_string())
}

/// Anchored at an instant (the boot instant, which is after every instant an earlier incarnation used) and
/// advancing with the monotonic clock, so it never goes backwards within an incarnation.
pub struct SystemClock {
    anchor: Instant,
    mono: std::time::Instant,
}

impl SystemClock {
    pub fn anchored_at(anchor: Instant) -> SystemClock {
        SystemClock {
            anchor,
            mono: std::time::Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        let elapsed = i64::try_from(self.mono.elapsed().as_nanos()).unwrap_or(i64::MAX);
        Instant(self.anchor.0.saturating_add(elapsed))
    }
}

/// How long the runtime's own operations take (a WAL sync, a checkpoint), for its counters: never read by a node.
pub struct Stopwatch(std::time::Instant);

impl Stopwatch {
    pub fn start() -> Stopwatch {
        Stopwatch(std::time::Instant::now())
    }

    /// Nanoseconds since the start.
    pub fn nanos(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// The operating system's entropy.
pub struct OsEntropy;

impl Entropy for OsEntropy {
    fn boot_nonce(&mut self) -> Result<u64, String> {
        let mut b = [0u8; 8];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut b))
            .map_err(|e| format!("reading /dev/urandom: {e}"))?;
        Ok(u64::from_le_bytes(b))
    }
}
