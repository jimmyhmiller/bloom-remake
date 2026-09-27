//! Seeds, the keyed PRF and independent decision streams (SEM-084, LANG-174/175, DIST-032; ARCH-12, ARCH-18).
//!
//! The PRF is SipHash-1-3 over canonical fingerprints. The root seed is the run seed in simulation and the
//! deployment seed in production; the choice seed σc is shared by all nodes and each node has its own seed σn
//! (FEATURES SEM-084). Seeds are secrets: their `Debug` output is redacted. Implemented by WP M2.1.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use siphasher::sip::SipHasher13;
use std::hash::Hasher;

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
        Ok(Seeds {
            deployment: root,
            choice: derive_key(&root, "choose", b"")?,
            node: derive_key(&root, "node", node_name.as_bytes())?,
        })
    }
}

/// The keyed PRF: `PRF_key(domain, fingerprints…, words…)`.
pub fn prf(key: &Seed, domain: &str, fingerprints: &[Fingerprint], words: &[u64]) -> Result<u64, ValueError> {
    let k0 = u64::from_le_bytes(
        key.0[..8]
            .try_into()
            .map_err(|_| ValueError::InvalidValue("invalid seed".into()))?,
    );
    let k1 = u64::from_le_bytes(
        key.0[8..]
            .try_into()
            .map_err(|_| ValueError::InvalidValue("invalid seed".into()))?,
    );
    let mut h = SipHasher13::new_with_keys(k0, k1);
    h.write(&PRF_VERSION.to_le_bytes());
    h.write(&(domain.len() as u64).to_le_bytes());
    h.write(domain.as_bytes());
    h.write(&(fingerprints.len() as u64).to_le_bytes());
    for f in fingerprints {
        h.write(&f.0.to_le_bytes());
    }
    h.write(&(words.len() as u64).to_le_bytes());
    for w in words {
        h.write(&w.to_le_bytes());
    }
    Ok(h.finish())
}
fn derive_key(root: &Seed, domain: &str, identity: &[u8]) -> Result<Seed, ValueError> {
    // Length framing prevents distinct identities and domains from aliasing.
    let id_hash = xxhash_rust::xxh3::xxh3_64(identity);
    let lo = prf(root, domain, &[Fingerprint(id_hash)], &[0])?;
    let hi = prf(root, domain, &[Fingerprint(id_hash)], &[1])?;
    let mut bytes = [0; 16];
    bytes[..8].copy_from_slice(&lo.to_le_bytes());
    bytes[8..].copy_from_slice(&hi.to_le_bytes());
    Ok(Seed(bytes))
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
        let id_hash = xxhash_rust::xxh3::xxh3_64(&self.identity);
        let value = prf(&self.root, &self.purpose, &[Fingerprint(id_hash)], &[self.position])?;
        self.position = self
            .position
            .checked_add(1)
            .ok_or_else(|| ValueError::InvalidValue("PRF stream counter overflow".into()))?;
        Ok(value)
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
#[cfg(test)]
mod m2_tests {
    use super::*;
    #[test]
    fn prf_kat() {
        assert_eq!(
            prf(&Seed([0; 16]), "choose", &[Fingerprint(1), Fingerprint(2)], &[3]).unwrap(),
            0x047f018c732edc10
        );
        let seeds = Seeds::derive(Seed([1; 16]), "n1").unwrap();
        assert_eq!(
            seeds.choice.0,
            [
                0x5a, 0x57, 0x1f, 0x53, 0xd9, 0x39, 0x7e, 0xa5, 0xcf, 0x4d, 0x80, 0x95, 0x9a, 0x9f, 0x79, 0xfe
            ]
        );
        assert_eq!(
            seeds.node.0,
            [
                0x81, 0xcf, 0xf6, 0x1d, 0x9e, 0x87, 0x82, 0xd9, 0x14, 0x09, 0xdd, 0x7a, 0xd3, 0x4b, 0xf3, 0x23
            ]
        );
    }
    #[test]
    fn prf_stream_independent() {
        let root = Seed([9; 16]);
        let mut a = PrfStream::new(root, "timer", b"node-a");
        let mut b = PrfStream::new(root, "network", b"node-a");
        let first = a.next_u64().unwrap();
        let second = a.next_u64().unwrap();
        assert_ne!(first, second);
        let before = b.next_u64().unwrap();
        assert_ne!(first, before);
        let mut replay = PrfStream::new(root, "timer", b"node-a");
        assert_eq!(first, replay.next_u64().unwrap());
        assert_eq!(second, replay.next_u64().unwrap());
        assert_eq!(a.position(), 2);
    }
}
