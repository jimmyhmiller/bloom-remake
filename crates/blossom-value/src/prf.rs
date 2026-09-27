//! Seeds, the keyed PRF and independent decision streams (SEM-084, LANG-174/175, DIST-032; ARCH-12, ARCH-18).
//!
//! The PRF is SipHash-1-3 over canonical fingerprints. The root seed is the run seed in simulation and the
//! deployment seed in production; the choice seed σc is shared by all nodes and each node has its own seed σn
//! (FEATURES SEM-084). Seeds are secrets: their `Debug` output is redacted. Implemented by WP M2.1.

use std::fmt;
use std::sync::Arc;

use blossom_base::unimplemented_feature;
use serde::{Deserialize, Serialize};

use crate::error::ValueError;
use crate::fp::Fingerprint;

/// The version of the PRF and of seed derivation; recorded in trace headers.
pub const PRF_VERSION: u16 = 1;

/// A 128-bit PRF key.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Seed(pub [u8; 16]);

impl fmt::Debug for Seed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Seed(<redacted>)")
    }
}

/// The seeds of one node (TickHeader::seeds, ARCHITECTURE §4.7).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Seeds {
    /// The root seed ρ (deployment or run seed).
    pub deployment: Seed,
    /// The choice seed σc = PRF(ρ, "choose"), shared by every node.
    pub choice: Seed,
    /// The node seed σn.
    pub node: Seed,
}

impl Seeds {
    /// Derives a node's seeds from the root seed and the node's stable name.
    pub fn derive(root: Seed, node_name: &str) -> Result<Seeds, ValueError> {
        let _ = (root, node_name);
        unimplemented_feature!("SEM-084", "seed derivation (WP M2.1)")
    }
}

/// The keyed PRF: `PRF_key(domain, fingerprints…, words…)`.
pub fn prf(key: &Seed, domain: &str, fingerprints: &[Fingerprint], words: &[u64]) -> Result<u64, ValueError> {
    let _ = (key, domain, fingerprints, words);
    unimplemented_feature!("SEM-084", "the SipHash-1-3 PRF (WP M2.1)")
}

/// An independent stream of pseudo-random decisions for one purpose, e.g. one simulator decision kind
/// (ARCH-12): streams with different purposes or identities never influence each other.
#[derive(Clone, Debug)]
pub struct PrfStream {
    root: Seed,
    purpose: Arc<str>,
    identity: Arc<[u8]>,
    position: u64,
}

impl PrfStream {
    /// A stream keyed by `root`, for `purpose`, identified by `identity` (for example a node name).
    pub fn new(root: Seed, purpose: &str, identity: &[u8]) -> PrfStream {
        PrfStream {
            root,
            purpose: purpose.into(),
            identity: identity.into(),
            position: 0,
        }
    }

    /// The purpose.
    pub fn purpose(&self) -> &str {
        &self.purpose
    }

    /// How many values have been drawn.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// The next value.
    pub fn next_u64(&mut self) -> Result<u64, ValueError> {
        let _ = (&self.root, &self.identity);
        unimplemented_feature!("DIST-032", "PRF decision streams (WP M2.1)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_are_redacted_in_debug() {
        let s = Seeds {
            deployment: Seed([1; 16]),
            choice: Seed([2; 16]),
            node: Seed([3; 16]),
        };
        let text = format!("{s:?}");
        assert!(
            !text.contains('1') && !text.contains('2') && !text.contains('3'),
            "{text}"
        );
        assert!(text.contains("<redacted>"));
    }

    #[test]
    fn prf_works_or_is_unimplemented_until_m2_1() {
        let mut stream = PrfStream::new(Seed([7; 16]), "sched", b"n1");
        assert_eq!((stream.purpose(), stream.position()), ("sched", 0));
        match stream.next_u64() {
            Ok(_) => {}
            Err(ValueError::Unimplemented(u)) => assert_eq!(u.feature.as_str(), "DIST-032"),
            Err(other) => panic!("unexpected error: {other}"),
        }
        match prf(&Seed([0; 16]), "choose", &[], &[]) {
            Ok(_) => {}
            Err(ValueError::Unimplemented(u)) => assert_eq!(u.feature.as_str(), "SEM-084"),
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
}
