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

use blossom_base::TypeId;
use serde::{Deserialize, Serialize};

use crate::error::ValueError;
use crate::time::{Duration, Instant, NodeId};
use crate::types::{IntTy, TypeDef, TypeTable};
use crate::value::{IntValue, ModValue, Value};

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
    /// A C-like enum tag without a declared maximum. Use a 64-bit lane so
    /// `Some(u32::MAX)` can still reserve zero for `None`.
    EnumTag,
    /// A C-like enum tag with a declared maximum, used by `scalar_kind`.
    BoundedEnumTag { max: u32 },
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
            ScalarKind::Option(NicheScalar::Bool) => true,
            ScalarKind::Option(NicheScalar::EnumTag) => false,
            ScalarKind::Option(NicheScalar::BoundedEnumTag { max }) => max < u32::MAX,
            ScalarKind::Option(NicheScalar::Int(t)) => t.bits() < 32,
        }
    }
}

/// The direct scalar encoding of type `ty`, or `None` if its columns are interned, bulk or lattice slots.
pub fn scalar_kind(types: &TypeTable, ty: TypeId) -> Result<Option<ScalarKind>, ValueError> {
    Ok(match types.def(ty)? {
        TypeDef::Unit => Some(ScalarKind::Unit),
        TypeDef::Bool => Some(ScalarKind::Bool),
        TypeDef::Int(t) if t.bits() <= 64 => Some(ScalarKind::Int(*t)),
        TypeDef::F64 => Some(ScalarKind::F64),
        TypeDef::Duration => Some(ScalarKind::Duration),
        TypeDef::Instant => Some(ScalarKind::Instant),
        TypeDef::Mod { bits } if *bits <= 64 => Some(ScalarKind::Mod { bits: *bits }),
        TypeDef::Node(_) => Some(ScalarKind::Node),
        TypeDef::Enum(e) if e.variants.iter().all(|v| v.payload.is_empty()) => Some(ScalarKind::EnumTag),
        TypeDef::Option(t) => match scalar_kind(types, *t)? {
            Some(ScalarKind::Bool) => Some(ScalarKind::Option(NicheScalar::Bool)),
            Some(ScalarKind::Int(t)) if t.bits() <= 32 => Some(ScalarKind::Option(NicheScalar::Int(t))),
            Some(ScalarKind::EnumTag) => {
                let TypeDef::Enum(e) = types.def(*t)? else {
                    return Err(ValueError::InvalidType("niche enum is not an enum".into()));
                };
                let max = e.variants.iter().map(|v| v.number).max().unwrap_or(0);
                Some(ScalarKind::Option(NicheScalar::BoundedEnumTag { max }))
            }
            _ => None,
        },
        _ => None,
    })
}
fn bad(kind: ScalarKind, value: &Value) -> ValueError {
    ValueError::InvalidValue(format!("{value:?} is not a direct scalar of kind {kind:?}"))
}
fn width_mask(bits: u32) -> u64 {
    if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 }
}
fn signed_word(n: i64, lane: Lane) -> u64 {
    ((n as u64) & width_mask(lane.bits())) ^ (1u64 << (lane.bits() - 1))
}
fn signed_value(word: u64, lane: Lane) -> i64 {
    let raw = word ^ (1u64 << (lane.bits() - 1));
    if lane == Lane::U32 {
        (raw as u32 as i32) as i64
    } else {
        raw as i64
    }
}
fn integer_value(ty: IntTy, raw: u64) -> Option<IntValue> {
    Some(match ty {
        IntTy::U8 => IntValue::U8(raw.try_into().ok()?),
        IntTy::U16 => IntValue::U16(raw.try_into().ok()?),
        IntTy::U32 => IntValue::U32(raw.try_into().ok()?),
        IntTy::U64 => IntValue::U64(raw),
        IntTy::I8 => IntValue::I8((raw as i64).try_into().ok()?),
        IntTy::I16 => IntValue::I16((raw as i64).try_into().ok()?),
        IntTy::I32 => IntValue::I32((raw as i64).try_into().ok()?),
        IntTy::I64 => IntValue::I64(raw as i64),
        IntTy::U128 | IntTy::I128 => return None,
    })
}
fn encode_inner(kind: ScalarKind, lane: Lane, value: &Value) -> Result<u64, ValueError> {
    let raw = match (kind, value) {
        (ScalarKind::Unit, Value::Unit) => 0,
        (ScalarKind::Bool, Value::Bool(b)) => u64::from(*b),
        (ScalarKind::Int(t), Value::Int(i)) if i.ty() == t && t.bits() <= 64 => {
            if t.is_signed() {
                signed_word(i.to_i128().ok_or_else(|| bad(kind, value))? as i64, lane)
            } else {
                i.to_i128().ok_or_else(|| bad(kind, value))? as u64
            }
        }
        (ScalarKind::F64, Value::F64(f)) => {
            let bits = f.to_bits();
            if bits >> 63 != 0 { !bits } else { bits ^ (1u64 << 63) }
        }
        (ScalarKind::Duration, Value::Duration(Duration(n))) => signed_word(*n, lane),
        (ScalarKind::Instant, Value::Instant(Instant(n))) => signed_word(*n, lane),
        (ScalarKind::Mod { bits }, Value::Mod(m)) if m.bits() == bits && bits <= 64 => m.limbs()[3],
        (ScalarKind::Node, Value::Node(NodeId(n))) => u64::from(*n),
        (ScalarKind::EnumTag, Value::Enum { variant, fields }) if fields.is_empty() => u64::from(*variant),
        (ScalarKind::Option(_), Value::Option(None)) => 0,
        (ScalarKind::Option(t), Value::Option(Some(v))) => {
            if let NicheScalar::BoundedEnumTag { max } = t
                && !matches!(v.as_ref(),Value::Enum{variant,fields} if *variant<=max && fields.is_empty())
            {
                return Err(bad(kind, value));
            }
            let inner = match t {
                NicheScalar::Bool => ScalarKind::Bool,
                NicheScalar::Int(t) => ScalarKind::Int(t),
                NicheScalar::EnumTag | NicheScalar::BoundedEnumTag { .. } => ScalarKind::EnumTag,
            };
            encode_inner(inner, lane, v)?
                .checked_add(1)
                .ok_or_else(|| bad(kind, value))?
        }
        _ => return Err(bad(kind, value)),
    };
    if raw > width_mask(lane.bits()) {
        return Err(bad(kind, value));
    }
    Ok(raw)
}
/// Encodes a scalar; rejects an out-of-lane value rather than truncating it.
pub fn encode_scalar(kind: ScalarKind, lane: Lane, value: &Value) -> Result<Word, ValueError> {
    if lane == Lane::U32 && !kind.fits_u32() {
        return Err(bad(kind, value));
    }
    Ok(Word(encode_inner(kind, lane, value)?))
}
/// Decodes one scalar; rejects words that are not canonical encodings of the type.
pub fn decode_scalar(kind: ScalarKind, lane: Lane, word: Word) -> Result<Value, ValueError> {
    if lane == Lane::U32 && !kind.fits_u32() || word.0 > width_mask(lane.bits()) {
        return Err(ValueError::InvalidValue(format!(
            "invalid {kind:?} word {word:?} in {lane:?}"
        )));
    }
    let v = match kind {
        ScalarKind::Unit if word.0 == 0 => Value::Unit,
        ScalarKind::Bool if word.0 <= 1 => Value::Bool(word.0 == 1),
        ScalarKind::Int(t) if t.bits() <= 64 => {
            let raw = if t.is_signed() {
                signed_value(word.0, lane) as u64
            } else {
                word.0
            };
            Value::Int(
                integer_value(t, raw)
                    .ok_or_else(|| ValueError::InvalidValue(format!("invalid {t:?} word {word:?}")))?,
            )
        }
        ScalarKind::F64 => {
            let bits = if word.0 >> 63 != 0 {
                word.0 ^ (1u64 << 63)
            } else {
                !word.0
            };
            Value::F64(f64::from_bits(bits))
        }
        ScalarKind::Duration => Value::Duration(Duration(signed_value(word.0, lane))),
        ScalarKind::Instant => Value::Instant(Instant(signed_value(word.0, lane))),
        ScalarKind::Mod { bits } if bits <= 64 => Value::Mod(ModValue::from_u64(bits, word.0)?),
        ScalarKind::Node => Value::Node(NodeId(
            word.0
                .try_into()
                .map_err(|_| ValueError::InvalidValue("node id too wide".into()))?,
        )),
        ScalarKind::EnumTag => Value::Enum {
            variant: word
                .0
                .try_into()
                .map_err(|_| ValueError::InvalidValue("enum tag too wide".into()))?,
            fields: Vec::new().into(),
        },
        ScalarKind::Option(_) if word.0 == 0 => Value::Option(None),
        ScalarKind::Option(t) => {
            let inner = match t {
                NicheScalar::Bool => ScalarKind::Bool,
                NicheScalar::Int(t) => ScalarKind::Int(t),
                NicheScalar::EnumTag | NicheScalar::BoundedEnumTag { .. } => ScalarKind::EnumTag,
            };
            let value = decode_scalar(inner, lane, Word(word.0 - 1))?;
            if let NicheScalar::BoundedEnumTag { max } = t
                && !matches!(value,Value::Enum{variant,..} if variant<=max)
            {
                return Err(ValueError::InvalidValue("enum tag exceeds declared maximum".into()));
            }
            Value::some(value)
        }
        _ => return Err(ValueError::InvalidValue(format!("invalid {kind:?} word {word:?}"))),
    };
    Ok(v)
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
        assert!(!ScalarKind::Option(NicheScalar::EnumTag).fits_u32());
        let largest = Value::some(Value::variant(u32::MAX, []));
        let word = encode_scalar(ScalarKind::Option(NicheScalar::EnumTag), Lane::U64, &largest).unwrap();
        assert_eq!(word.0, u64::from(u32::MAX) + 1);
        assert_eq!(
            decode_scalar(ScalarKind::Option(NicheScalar::EnumTag), Lane::U64, word).unwrap(),
            largest
        );
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
#[cfg(test)]
mod m2_tests {
    use super::*;
    use proptest::prelude::*;
    fn samples(kind: ScalarKind) -> Vec<Value> {
        match kind {
            ScalarKind::Unit => vec![Value::Unit],
            ScalarKind::Bool => vec![Value::Bool(false), Value::Bool(true)],
            ScalarKind::Int(IntTy::I8) => [-128, -1, 0, 1, 127]
                .into_iter()
                .map(|n| Value::Int(IntValue::I8(n)))
                .collect(),
            ScalarKind::Int(IntTy::I16) => [-32768, -1, 0, 1, 32767]
                .into_iter()
                .map(|n| Value::Int(IntValue::I16(n)))
                .collect(),
            ScalarKind::Int(IntTy::I32) => [i32::MIN, -1, 0, 1, i32::MAX]
                .into_iter()
                .map(|n| Value::Int(IntValue::I32(n)))
                .collect(),
            ScalarKind::Int(IntTy::I64) => [i64::MIN, -1, 0, 1, i64::MAX]
                .into_iter()
                .map(|n| Value::Int(IntValue::I64(n)))
                .collect(),
            ScalarKind::Int(IntTy::U8) => [0, 1, u8::MAX]
                .into_iter()
                .map(|n| Value::Int(IntValue::U8(n)))
                .collect(),
            ScalarKind::Int(IntTy::U16) => [0, 1, u16::MAX]
                .into_iter()
                .map(|n| Value::Int(IntValue::U16(n)))
                .collect(),
            ScalarKind::Int(IntTy::U32) => [0, 1, u32::MAX]
                .into_iter()
                .map(|n| Value::Int(IntValue::U32(n)))
                .collect(),
            ScalarKind::Int(IntTy::U64) => [0, 1, u64::MAX]
                .into_iter()
                .map(|n| Value::Int(IntValue::U64(n)))
                .collect(),
            ScalarKind::Int(_) => vec![],
            ScalarKind::F64 => [
                f64::NEG_INFINITY,
                -1.0,
                -0.0,
                0.0,
                1.0,
                f64::INFINITY,
                f64::from_bits(0x7ff8000000000001),
            ]
            .into_iter()
            .map(Value::F64)
            .collect(),
            ScalarKind::Duration => [i64::MIN, 0, i64::MAX]
                .into_iter()
                .map(|n| Value::Duration(Duration(n)))
                .collect(),
            ScalarKind::Instant => [i64::MIN, 0, i64::MAX]
                .into_iter()
                .map(|n| Value::Instant(Instant(n)))
                .collect(),
            ScalarKind::Mod { bits } => vec![0, 1]
                .into_iter()
                .map(|n| Value::Mod(ModValue::from_u64(bits, n).unwrap()))
                .collect(),
            ScalarKind::Node => vec![Value::Node(NodeId(0)), Value::Node(NodeId(u32::MAX))],
            ScalarKind::EnumTag => vec![Value::variant(0, []), Value::variant(1, [])],
            ScalarKind::Option(_) => vec![Value::Option(None)],
        }
    }
    #[test]
    fn encoding_order_preserving() {
        let kinds = [
            ScalarKind::Unit,
            ScalarKind::Bool,
            ScalarKind::Int(IntTy::U8),
            ScalarKind::Int(IntTy::U16),
            ScalarKind::Int(IntTy::U32),
            ScalarKind::Int(IntTy::U64),
            ScalarKind::Int(IntTy::I8),
            ScalarKind::Int(IntTy::I16),
            ScalarKind::Int(IntTy::I32),
            ScalarKind::Int(IntTy::I64),
            ScalarKind::F64,
            ScalarKind::Duration,
            ScalarKind::Instant,
            ScalarKind::Mod { bits: 32 },
            ScalarKind::Node,
            ScalarKind::EnumTag,
        ];
        for kind in kinds {
            for lane in [Lane::U32, Lane::U64] {
                if lane == Lane::U32 && !kind.fits_u32() {
                    continue;
                }
                let values = samples(kind);
                for a in &values {
                    for b in &values {
                        let aa = encode_scalar(kind, lane, a).unwrap();
                        let bb = encode_scalar(kind, lane, b).unwrap();
                        assert_eq!(a.cmp(b), aa.cmp(&bb), "{kind:?} {lane:?} {a:?} {b:?}");
                    }
                }
            }
        }
    }
    proptest! {#[test]fn encoding_roundtrip(n in any::<i64>(),u in any::<u64>(),bits in any::<u64>()) {
        for (kind,value) in [(ScalarKind::Int(IntTy::I64),Value::Int(IntValue::I64(n))),(ScalarKind::Int(IntTy::U64),Value::Int(IntValue::U64(u))),(ScalarKind::F64,Value::F64(f64::from_bits(bits))),(ScalarKind::Duration,Value::Duration(Duration(n)))] {
            let word=encode_scalar(kind,Lane::U64,&value).unwrap();
            prop_assert_eq!(decode_scalar(kind,Lane::U64,word).unwrap(),value);
        }
    }}
    #[test]
    fn option_niche_roundtrip() {
        for kind in [
            NicheScalar::Bool,
            NicheScalar::Int(IntTy::I8),
            NicheScalar::Int(IntTy::U32),
            NicheScalar::BoundedEnumTag { max: 3 },
        ] {
            let k = ScalarKind::Option(kind);
            for lane in [Lane::U32, Lane::U64] {
                if lane == Lane::U32 && !k.fits_u32() {
                    continue;
                }
                let some = match kind {
                    NicheScalar::Bool => Value::Bool(true),
                    NicheScalar::Int(IntTy::I8) => Value::Int(IntValue::I8(-128)),
                    NicheScalar::Int(IntTy::U32) => Value::Int(IntValue::U32(u32::MAX)),
                    NicheScalar::EnumTag | NicheScalar::BoundedEnumTag { .. } => Value::variant(1, []),
                    _ => continue,
                };
                for v in [Value::Option(None), Value::some(some)] {
                    let w = encode_scalar(k, lane, &v).unwrap();
                    assert_eq!(decode_scalar(k, lane, w).unwrap(), v);
                }
            }
        }
    }
}
