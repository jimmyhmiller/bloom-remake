//! Canonical fingerprints (ENG-032; ARCHITECTURE §4.1, ARCH-18).
//!
//! A fingerprint is xxh3-64 over a value's canonical encoding; records, tuples and collections are fingerprinted
//! Merkle-style over their children's fingerprints (sets and maps over canonically sorted elements), so a
//! fingerprint costs O(arity) and never depends on intern ids: it is identical on every node and in the oracle.
//! Implemented by WP M2.1, together with the word-level Merkle helpers the interner uses.

use blossom_base::unimplemented_feature;
use serde::{Deserialize, Serialize};

use crate::error::ValueError;
use crate::value::Value;

/// The version of the canonical encoding that fingerprints and the PRF read; recorded in trace headers.
pub const ENCODING_VERSION: u16 = 1;

/// A 64-bit canonical fingerprint.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Fingerprint(pub u64);

/// The fingerprint of a value.
pub fn fingerprint(value: &Value) -> Result<Fingerprint, ValueError> {
    let _ = value;
    unimplemented_feature!("ENG-032", "canonical value fingerprints (WP M2.1)")
}

/// The fingerprint of a row (a tuple of column values).
pub fn fingerprint_row(values: &[Value]) -> Result<Fingerprint, ValueError> {
    let _ = values;
    unimplemented_feature!("ENG-032", "canonical row fingerprints (WP M2.1)")
}
