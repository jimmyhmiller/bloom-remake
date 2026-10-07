//! Logical and physical time, node ids and incarnations (ARCHITECTURE §2.1, §5.5, §5.6).

use serde::{Deserialize, Serialize};

/// An integer as an `i128` (a `u128` above `i128::MAX` has none: it overflows any duration anyway).
fn int_wide(k: crate::value::IntValue) -> Option<i128> {
    k.to_i128()
}

/// Per-node logical time. Tick 0 is the boot tick of a fresh node (CR-13); ticks are durable and monotone across
/// incarnations (ARCHITECTURE §5.6).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Tick(pub u64);

impl Tick {
    /// The boot tick of a fresh node.
    pub const BOOT: Tick = Tick(0);

    /// The next tick; `None` on overflow.
    pub const fn next(self) -> Option<Tick> {
        match self.0.checked_add(1) {
            Some(t) => Some(Tick(t)),
            None => None,
        }
    }

    /// The previous tick; `None` at tick 0.
    pub const fn prev(self) -> Option<Tick> {
        match self.0.checked_sub(1) {
            Some(t) => Some(Tick(t)),
            None => None,
        }
    }
}

/// A dense node id, assigned in canonical directory order (ARCHITECTURE §5.9), or a client member's (CLIENTS.md §2):
/// the top bit set, then the id of the server node that admitted it (11 bits) and its serial there (20 bits).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct NodeId(pub u32);

impl NodeId {
    /// The top bit: set on a client member's id.
    pub const CLIENT: u32 = 1 << 31;
    /// The most server nodes that admit clients, and the most clients one admits.
    pub const CLIENT_SERVERS: u32 = 1 << 11;
    pub const CLIENT_SERIALS: u32 = 1 << 20;

    /// The client member `serial` admitted by server node `server`; `None` past the bounds.
    pub fn client(server: NodeId, serial: u32) -> Option<NodeId> {
        (server.0 < Self::CLIENT_SERVERS && serial < Self::CLIENT_SERIALS)
            .then_some(NodeId(Self::CLIENT | server.0 << 20 | serial))
    }

    /// Whether this is a client member's id.
    pub fn is_client(self) -> bool {
        self.0 & Self::CLIENT != 0
    }

    /// A client member's server node and serial.
    pub fn client_parts(self) -> Option<(NodeId, u32)> {
        self.is_client().then_some((
            NodeId((self.0 >> 20) & (Self::CLIENT_SERVERS - 1)),
            self.0 & (Self::CLIENT_SERIALS - 1),
        ))
    }
}

/// Nanoseconds since the deployment epoch.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Instant(pub i64);

/// A signed span of nanoseconds.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Duration(pub i64);

impl Duration {
    /// Zero.
    pub const ZERO: Duration = Duration(0);

    /// `n` nanoseconds.
    pub const fn from_nanos(n: i64) -> Duration {
        Duration(n)
    }

    /// `n` microseconds; `None` on overflow.
    pub const fn from_micros(n: i64) -> Option<Duration> {
        Self::scaled(n, 1_000)
    }

    /// `n` milliseconds; `None` on overflow.
    pub const fn from_millis(n: i64) -> Option<Duration> {
        Self::scaled(n, 1_000_000)
    }

    /// `n` seconds; `None` on overflow.
    pub const fn from_secs(n: i64) -> Option<Duration> {
        Self::scaled(n, 1_000_000_000)
    }

    const fn scaled(n: i64, unit: i64) -> Option<Duration> {
        match n.checked_mul(unit) {
            Some(ns) => Some(Duration(ns)),
            None => None,
        }
    }

    /// `self × k` for an integer of any type (LANGUAGE §5.1); `None` on overflow.
    pub fn times(self, k: crate::value::IntValue) -> Option<Duration> {
        let product = i128::from(self.0).checked_mul(int_wide(k)?)?;
        i64::try_from(product).ok().map(Duration)
    }

    /// `self ÷ k` for an integer of any type, truncated toward zero; `None` for zero or on overflow.
    pub fn divided_by(self, k: crate::value::IntValue) -> Option<Duration> {
        let quotient = i128::from(self.0).checked_div(int_wide(k)?)?;
        i64::try_from(quotient).ok().map(Duration)
    }

    /// The length in nanoseconds.
    pub const fn as_nanos(self) -> i64 {
        self.0
    }

    /// Checked addition.
    pub const fn checked_add(self, other: Duration) -> Option<Duration> {
        match self.0.checked_add(other.0) {
            Some(ns) => Some(Duration(ns)),
            None => None,
        }
    }

    /// Checked subtraction.
    pub const fn checked_sub(self, other: Duration) -> Option<Duration> {
        match self.0.checked_sub(other.0) {
            Some(ns) => Some(Duration(ns)),
            None => None,
        }
    }
}

impl Instant {
    /// `self + d`; `None` on overflow (LANGUAGE §5.1: `Instant ± Duration = Instant`).
    pub const fn checked_add(self, d: Duration) -> Option<Instant> {
        match self.0.checked_add(d.0) {
            Some(ns) => Some(Instant(ns)),
            None => None,
        }
    }

    /// `self - d`; `None` on overflow.
    pub const fn checked_sub(self, d: Duration) -> Option<Instant> {
        match self.0.checked_sub(d.0) {
            Some(ns) => Some(Instant(ns)),
            None => None,
        }
    }

    /// `self - earlier` (LANGUAGE §5.1: `Instant - Instant = Duration`); `None` on overflow.
    pub const fn checked_since(self, earlier: Instant) -> Option<Duration> {
        match self.0.checked_sub(earlier.0) {
            Some(ns) => Some(Duration(ns)),
            None => None,
        }
    }
}

/// A node incarnation: the durable restart counter and the boot nonce drawn at start (SEM-084, DIST-033).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Incarnation {
    /// How many times the node restarted from durable state.
    pub restarts: u64,
    /// OS entropy drawn at boot, recorded in the WAL header and the trace.
    pub boot_nonce: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_arithmetic_is_checked() {
        assert_eq!(Tick::BOOT.next(), Some(Tick(1)));
        assert_eq!(Tick(u64::MAX).next(), None);
        assert_eq!(Tick::BOOT.prev(), None);
        assert_eq!(Duration::from_secs(2), Some(Duration(2_000_000_000)));
        assert_eq!(Duration::from_millis(3).map(Duration::as_nanos), Some(3_000_000));
        assert_eq!(Duration::from_micros(4), Some(Duration(4_000)));
        assert_eq!(Duration::from_secs(i64::MAX), None);
        assert_eq!(Duration(1).checked_add(Duration(i64::MAX)), None);
        assert_eq!(Duration(1).checked_sub(Duration(3)), Some(Duration(-2)));
        let t = Instant(100);
        assert_eq!(t.checked_add(Duration(5)), Some(Instant(105)));
        assert_eq!(t.checked_sub(Duration(5)), Some(Instant(95)));
        assert_eq!(Instant(105).checked_since(t), Some(Duration(5)));
        assert_eq!(Instant(i64::MIN).checked_since(Instant(1)), None);
    }

    #[test]
    fn time_serde_roundtrip() {
        let inc = Incarnation {
            restarts: 3,
            boot_nonce: 0xdead_beef,
        };
        let json = serde_json::to_string(&(Tick(7), NodeId(2), Instant(-5), Duration(9), inc)).unwrap();
        let back: (Tick, NodeId, Instant, Duration, Incarnation) = serde_json::from_str(&json).unwrap();
        assert_eq!(back, (Tick(7), NodeId(2), Instant(-5), Duration(9), inc));
    }
}
