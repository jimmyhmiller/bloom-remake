//! Digests (ARCHITECTURE §4.11, ARCH-18; ENG-120).
//!
//! - [`Digest256`]: BLAKE3-256, for program, schema and plan digests and checkpoint files;
//! - [`Digest128`]: an incremental set hash — the sum modulo 2¹²⁸ of one [`SetElement`] per member — so it is
//!   independent of order and maintained in O(1) per insertion or removal.
//!
//! The element and BLAKE3 hash functions (with their domain separation and known-answer vectors) are implemented
//! by WP M2.1; the set arithmetic is exact and implemented here.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::ValueError;
use crate::fp::Fingerprint;

/// An order-independent incremental set digest.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Digest128(pub u128);

/// The contribution of one member to a [`Digest128`].
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct SetElement(pub u128);

impl Digest128 {
    /// The digest of the empty set.
    pub const EMPTY: Digest128 = Digest128(0);

    /// Adds a member.
    pub fn add(&mut self, element: SetElement) {
        self.0 = self.0.wrapping_add(element.0);
    }

    /// Removes a member that was added.
    pub fn remove(&mut self, element: SetElement) {
        self.0 = self.0.wrapping_sub(element.0);
    }

    /// The digest of the union of two disjoint sets.
    pub fn combine(self, other: Digest128) -> Digest128 {
        Digest128(self.0.wrapping_add(other.0))
    }
}

/// The set element for a member identified by fingerprints, in a digest domain (a relation, the outbox, …).
pub fn set_element(domain: &str, fingerprints: &[Fingerprint]) -> Result<SetElement, ValueError> {
    let mut h = blake3::Hasher::new();
    h.update(b"blossom-set-element-v1");
    h.update(&(domain.len() as u64).to_le_bytes());
    h.update(domain.as_bytes());
    h.update(&(fingerprints.len() as u64).to_le_bytes());
    for fp in fingerprints {
        h.update(&fp.0.to_le_bytes());
    }
    let bytes = h.finalize();
    let arr: [u8; 16] = bytes.as_bytes()[..16]
        .try_into()
        .map_err(|_| ValueError::InvalidValue("digest length".into()))?;
    Ok(SetElement(u128::from_le_bytes(arr)))
}

/// A BLAKE3-256 digest.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Digest256(pub [u8; 32]);

impl fmt::Debug for Digest256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Digest256(")?;
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        f.write_str(")")
    }
}

/// BLAKE3-256 of `bytes` in the given domain: program digests (recorded in trace headers, TEST-010), schema and plan
/// digests, and checkpoint files (ARCH-18).
pub fn digest256(domain: &str, bytes: &[u8]) -> Result<Digest256, ValueError> {
    let mut h = blake3::Hasher::new();
    h.update(b"blossom-digest-v1");
    h.update(&(domain.len() as u64).to_le_bytes());
    h.update(domain.as_bytes());
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
    Ok(Digest256(*h.finalize().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest128_order_independent() {
        let elements = [SetElement(u128::MAX), SetElement(3), SetElement(1 << 100)];
        let mut forward = Digest128::EMPTY;
        elements.iter().for_each(|e| forward.add(*e));
        let mut backward = Digest128::EMPTY;
        elements.iter().rev().for_each(|e| backward.add(*e));
        assert_eq!(forward, backward);
        forward.remove(SetElement(3));
        let mut two = Digest128::EMPTY;
        two.add(SetElement(u128::MAX));
        two.add(SetElement(1 << 100));
        assert_eq!(forward, two);
        assert_eq!(Digest128(5).combine(Digest128(u128::MAX)), Digest128(4));
        assert_eq!(format!("{:?}", Digest256([0xab; 32])).len(), "Digest256()".len() + 64);
    }

    #[test]
    fn digest_functions_work_or_are_unimplemented_until_m2_1() {
        // Works, or fails only with Unimplemented naming the feature (PLAN §2.6: the test survives M2.1).
        match digest256("program", b"abc") {
            Ok(d) => assert_eq!(digest256("program", b"abc").ok(), Some(d)),
            Err(ValueError::Unimplemented(u)) => assert_eq!(u.feature.as_str(), "TEST-010"),
            Err(other) => panic!("unexpected error: {other}"),
        }
        match set_element("rel", &[Fingerprint(1)]) {
            Ok(e) => assert_eq!(set_element("rel", &[Fingerprint(1)]).ok(), Some(e)),
            Err(ValueError::Unimplemented(u)) => assert_eq!(u.feature.as_str(), "ENG-120"),
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
}
