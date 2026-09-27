//! Words, lanes and the order-preserving scalar encodings (ARCHITECTURE §4.1; ARCH-05, ARCH-22).
//!
//! A relation's rows are arrays of [`Word`]s of one [`Lane`] width. Scalars use order-preserving encodings, so word
//! order equals canonical order:
//!
//! | Type | Encoding |
//! |---|---|
//! | `bool`, `()`, C-like enum tags | 0/1, 0, variant number |
//! | `u8`…`u64`, `Mod<N≤64>` | zero-extended |
//! | `i8`…`i64`, `Duration`, `Instant` | sign-extended, then the lane's top bit flipped |
//! | `f64` | IEEE totalOrder key: `if sign { !bits } else { bits ^ 1<<63 }` |
//! | `Node` | the dense `NodeId` |
//! | `Option<T>`, T with a niche (bool, ≤ 32-bit ints, tags) | `0` = None, `enc(v)+1` = Some |
//!
//! Every other column is `Interned`, `Bulk` or a lattice slot ([`ColEncTag`]). The encode and decode functions are
//! implemented by WP M2.1.

use blossom_base::{TypeId, unimplemented_feature};
use serde::{Deserialize, Serialize};

use crate::error::ValueError;
use crate::types::{IntTy, TypeTable};
use crate::value::Value;

/// One column value in a row: a direct scalar encoding, an intern or bulk id, or an inline lattice.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Word(pub u64);

/// The width of a relation's words (ENG-020): 32 bits when every column's encoding fits, otherwise 64.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum Lane {
    /// 4-byte words.
    U32,
    /// 8-byte words.
    U64,
}

impl Lane {
    /// The width in bits.
    pub const fn bits(self) -> u32 {
        match self {
            Lane::U32 => 32,
            Lane::U64 => 64,
        }
    }

    /// The width in bytes.
    pub const fn bytes(self) -> usize {
        match self {
            Lane::U32 => 4,
            Lane::U64 => 8,
        }
    }
}

/// How a column's words represent values (ARCHITECTURE §4.1, ARCH-22).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum ColEncTag {
    /// An order-preserving scalar encoding.
    Direct,
    /// A hash-consed intern id: equality is word equality.
    Interned,
    /// An arena handle with a cached fingerprint, for payload-only columns.
    Bulk,
    /// A lattice value stored inline in the word.
    LatInline,
    /// A handle to a lattice-heap object.
    LatObj,
}

/// A word known to denote an interned or bulk `String`.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct StrWord(pub Word);

/// A word known to denote an interned or bulk `Bytes` value.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct BytesWord(pub Word);

/// A scalar type with a direct, order-preserving word encoding.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum ScalarKind {
    /// `()`.
    Unit,
    /// `bool`.
    Bool,
    /// An integer of at most 64 bits.
    Int(IntTy),
    /// `f64`.
    F64,
    /// `Duration`.
    Duration,
    /// `Instant`.
    Instant,
    /// `Mod<N>` with `N ≤ 64`.
    Mod {
        /// The width.
        bits: u16,
    },
    /// `Node`.
    Node,
    /// A C-like enum (no variant has a payload): the variant number.
    EnumTag,
    /// `Option<T>` for a `T` with a niche.
    Option(NicheScalar),
}

/// The scalars whose encoding leaves a niche for `Option` (ARCHITECTURE §4.1).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum NicheScalar {
    /// `bool`.
    Bool,
    /// An integer of at most 32 bits.
    Int(IntTy),
    /// A C-like enum tag.
    EnumTag,
}

impl ScalarKind {
    /// Whether every value of this kind encodes in 32 bits, so the kind allows [`Lane::U32`].
    pub const fn fits_u32(self) -> bool {
        match self {
            ScalarKind::Unit | ScalarKind::Bool | ScalarKind::Node | ScalarKind::EnumTag => true,
            ScalarKind::Int(t) => t.bits() <= 32,
            ScalarKind::Mod { bits } => bits <= 32,
            ScalarKind::F64 | ScalarKind::Duration | ScalarKind::Instant => false,
            // `enc(v) + 1` must still fit: a full 32-bit integer does not.
            ScalarKind::Option(NicheScalar::Bool) | ScalarKind::Option(NicheScalar::EnumTag) => true,
            ScalarKind::Option(NicheScalar::Int(t)) => t.bits() < 32,
        }
    }
}

/// The direct scalar encoding of type `ty`, or `None` if its columns are interned, bulk or lattice slots.
pub fn scalar_kind(types: &TypeTable, ty: TypeId) -> Result<Option<ScalarKind>, ValueError> {
    let _ = (types, ty);
    unimplemented_feature!("ENG-020", "scalar kinds of types (WP M2.1)")
}

/// Encodes a scalar value in the given lane.
pub fn encode_scalar(kind: ScalarKind, lane: Lane, value: &Value) -> Result<Word, ValueError> {
    let _ = (kind, lane, value);
    unimplemented_feature!("ENG-020", "order-preserving scalar encodings (WP M2.1)")
}

/// Decodes a scalar word of the given lane.
pub fn decode_scalar(kind: ScalarKind, lane: Lane, word: Word) -> Result<Value, ValueError> {
    let _ = (kind, lane, word);
    unimplemented_feature!("ENG-020", "order-preserving scalar decodings (WP M2.1)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_kind_lane_fit() {
        assert!(ScalarKind::Int(IntTy::I32).fits_u32());
        assert!(!ScalarKind::Int(IntTy::U64).fits_u32());
        assert!(ScalarKind::Mod { bits: 32 }.fits_u32() && !ScalarKind::Mod { bits: 33 }.fits_u32());
        assert!(!ScalarKind::F64.fits_u32() && !ScalarKind::Instant.fits_u32());
        assert!(ScalarKind::Option(NicheScalar::Int(IntTy::U16)).fits_u32());
        assert!(!ScalarKind::Option(NicheScalar::Int(IntTy::U32)).fits_u32());
        assert_eq!((Lane::U32.bits(), Lane::U64.bytes()), (32, 8));
    }

    #[test]
    fn encodings_are_unimplemented_until_m2_1() {
        // Works, or fails only with Unimplemented naming ENG-020 (PLAN §2.6: the test survives M2.1).
        match encode_scalar(ScalarKind::Bool, Lane::U32, &Value::Bool(true)) {
            Ok(w) => assert_eq!(
                decode_scalar(ScalarKind::Bool, Lane::U32, w).ok(),
                Some(Value::Bool(true))
            ),
            Err(ValueError::Unimplemented(u)) => assert_eq!(u.feature.as_str(), "ENG-020"),
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
}
