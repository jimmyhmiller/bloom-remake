//! The canonical value model (ARCHITECTURE §2.2, §4.1; LANGUAGE §5).
//!
//! [`Value`] covers every [`TypeDef`](crate::TypeDef). It is used by the oracle, the REPL, the dynamic host API,
//! dumps and tests; the engine works on [`Word`](crate::Word)s and converts through a
//! [`ValueStore`](crate::ValueStore). Compound values share their payload through `Arc`, so cloning is cheap.
//!
//! Values are untyped with respect to nominal names: a struct value is its fields in declaration order, an enum
//! value its variant number and payload. The [`TypeTable`](crate::TypeTable) gives them their types and
//! [`TypeTable::check_value`](crate::TypeTable::check_value) checks a value against one.
//!
//! Lattice and group values ([`LatValue`], [`GroupValue`]) are plain data here: a structural, canonical form whose
//! operations (join, order, atomize, group arithmetic) live in `blossom-lattice`.
//!
//! Equality, ordering and hashing are the canonical ones of [`order`](crate::order) (LANGUAGE §5.5).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::ValueError;
use crate::serde_util;
use crate::time::{Duration, Instant, NodeId};
use crate::types::{ExternCodecId, IntTy};

/// A value of any Blossom type.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Value {
    /// `()`.
    Unit,
    /// `bool`.
    Bool(bool),
    /// An integer, stored with its integer type.
    Int(IntValue),
    /// `f64`, ordered by IEEE 754 `totalOrder` (serialized as its bit pattern, so every NaN payload round-trips).
    F64(#[serde(with = "serde_util::f64_bits")] f64),
    /// `String`.
    Str(Arc<str>),
    /// `Bytes`.
    Bytes(Arc<[u8]>),
    /// `Duration` (nanoseconds).
    Duration(Duration),
    /// `Instant` (nanoseconds since the deployment epoch).
    Instant(Instant),
    /// `Mod<N>` (LANG-026).
    Mod(ModValue),
    /// `Blob`: a content address (LANG-028).
    Blob(BlobRef),
    /// `Session`: an external client session (LANG-243).
    Session(SessionId),
    /// `Principal`: an authenticated identity, a SPIFFE id (LANG-240).
    Principal(Arc<str>),
    /// `Node` / `Node<R>`.
    Node(NodeId),
    /// A tuple (at least one element; `()` is [`Value::Unit`]).
    Tuple(Arc<[Value]>),
    /// A struct: its fields in declaration order.
    Struct(Arc<[Value]>),
    /// An enum value: the variant's stable number (`#n`) and its payload fields.
    Enum {
        /// The variant number.
        variant: u32,
        /// The payload, in declaration order.
        fields: Arc<[Value]>,
    },
    /// A value of a newer program version's variant, decoded to the local `#[unknown]` variant; it keeps its
    /// original number and payload bytes so it re-encodes unchanged (LANG-261, ARCHITECTURE §5.4).
    UnknownVariant {
        /// The local `#[unknown]` variant's number.
        variant: u32,
        /// The variant number on the wire.
        wire_number: u32,
        /// The payload exactly as it arrived.
        payload: Arc<[u8]>,
    },
    /// `Vec<T>`.
    Vec(Arc<[Value]>),
    /// `Set<T>`.
    Set(#[serde(with = "serde_util::set_strict")] Arc<BTreeSet<Value>>),
    /// `Map<K, V>`.
    Map(#[serde(with = "serde_util::map_pairs")] Arc<BTreeMap<Value, Value>>),
    /// `Option<T>`.
    Option(Option<Arc<Value>>),
    /// A lattice value.
    Lattice(LatValue),
    /// A group or ring value (never a lattice, CR-35).
    Group(GroupValue),
    /// An opaque host value (LANG-027): its codec and encoded bytes.
    Extern {
        /// The host type's codec.
        codec: ExternCodecId,
        /// The encoded value.
        bytes: Arc<[u8]>,
    },
}

/// An integer with its type. Integers of different types are never compared by programs (a type error); the
/// canonical order puts the type first.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum IntValue {
    /// `u8`.
    U8(u8),
    /// `u16`.
    U16(u16),
    /// `u32`.
    U32(u32),
    /// `u64`.
    U64(u64),
    /// `u128`.
    U128(u128),
    /// `i8`.
    I8(i8),
    /// `i16`.
    I16(i16),
    /// `i32`.
    I32(i32),
    /// `i64`.
    I64(i64),
    /// `i128`.
    I128(i128),
}

impl IntValue {
    /// The integer type.
    pub const fn ty(self) -> IntTy {
        match self {
            IntValue::U8(_) => IntTy::U8,
            IntValue::U16(_) => IntTy::U16,
            IntValue::U32(_) => IntTy::U32,
            IntValue::U64(_) => IntTy::U64,
            IntValue::U128(_) => IntTy::U128,
            IntValue::I8(_) => IntTy::I8,
            IntValue::I16(_) => IntTy::I16,
            IntValue::I32(_) => IntTy::I32,
            IntValue::I64(_) => IntTy::I64,
            IntValue::I128(_) => IntTy::I128,
        }
    }

    /// The value of type `ty` equal to `n`, if it is in range.
    pub fn from_i128(ty: IntTy, n: i128) -> Option<IntValue> {
        Some(match ty {
            IntTy::U8 => IntValue::U8(n.try_into().ok()?),
            IntTy::U16 => IntValue::U16(n.try_into().ok()?),
            IntTy::U32 => IntValue::U32(n.try_into().ok()?),
            IntTy::U64 => IntValue::U64(n.try_into().ok()?),
            IntTy::U128 => IntValue::U128(n.try_into().ok()?),
            IntTy::I8 => IntValue::I8(n.try_into().ok()?),
            IntTy::I16 => IntValue::I16(n.try_into().ok()?),
            IntTy::I32 => IntValue::I32(n.try_into().ok()?),
            IntTy::I64 => IntValue::I64(n.try_into().ok()?),
            IntTy::I128 => IntValue::I128(n),
        })
    }

    /// The value as an `i128`, unless it is a `u128` above `i128::MAX`.
    pub fn to_i128(self) -> Option<i128> {
        Some(match self {
            IntValue::U8(n) => n.into(),
            IntValue::U16(n) => n.into(),
            IntValue::U32(n) => n.into(),
            IntValue::U64(n) => n.into(),
            IntValue::U128(n) => n.try_into().ok()?,
            IntValue::I8(n) => n.into(),
            IntValue::I16(n) => n.into(),
            IntValue::I32(n) => n.into(),
            IntValue::I64(n) => n.into(),
            IntValue::I128(n) => n,
        })
    }
}

/// An `N`-bit modular id, `1 ≤ N ≤ 256` (LANG-026). The limbs are most significant first, so the derived order
/// is numeric within one width; the width is compared first.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(try_from = "ModRepr", into = "ModRepr")]
pub struct ModValue {
    bits: u16,
    limbs: [u64; 4],
}

/// The serialized form of [`ModValue`]; deserialization re-checks the width.
#[derive(Serialize, Deserialize)]
struct ModRepr {
    bits: u16,
    limbs: [u64; 4],
}

impl From<ModValue> for ModRepr {
    fn from(m: ModValue) -> ModRepr {
        ModRepr {
            bits: m.bits,
            limbs: m.limbs,
        }
    }
}

impl TryFrom<ModRepr> for ModValue {
    type Error = ValueError;
    fn try_from(r: ModRepr) -> Result<ModValue, ValueError> {
        ModValue::new(r.bits, r.limbs)
    }
}

impl ModValue {
    /// The widest modular type.
    pub const MAX_BITS: u16 = 256;

    /// The value with the given width and limbs (most significant first). Fails if the width is outside
    /// `1..=256` or the value does not fit in it.
    pub fn new(bits: u16, limbs: [u64; 4]) -> Result<ModValue, ValueError> {
        if bits == 0 || bits > Self::MAX_BITS {
            return Err(ValueError::InvalidValue(format!(
                "Mod<{bits}>: the width must be in 1..=256"
            )));
        }
        // Limb i (most significant first) holds bits [64·(3−i), 64·(4−i)); `allowed` is how many of them the width
        // keeps.
        for (i, limb) in limbs.iter().enumerate() {
            let low = 64 * (3 - i as u32);
            let allowed = u32::from(bits).saturating_sub(low).min(64);
            let excess = if allowed >= 64 { 0 } else { limb >> allowed };
            if excess != 0 {
                return Err(ValueError::InvalidValue(format!("a value does not fit in Mod<{bits}>")));
            }
        }
        Ok(ModValue { bits, limbs })
    }

    /// A value that fits in 64 bits.
    pub fn from_u64(bits: u16, n: u64) -> Result<ModValue, ValueError> {
        ModValue::new(bits, [0, 0, 0, n])
    }

    /// The width `N`.
    pub const fn bits(&self) -> u16 {
        self.bits
    }

    /// The limbs, most significant first.
    pub const fn limbs(&self) -> [u64; 4] {
        self.limbs
    }
}

/// A content-addressed blob handle: the BLAKE3 hash of the bytes and their length (LANG-028). Blobs order by
/// content address, never by arrival (SEM-088).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct BlobRef {
    /// BLAKE3-256 of the content.
    pub hash: [u8; 32],
    /// Length in bytes.
    pub len: u64,
}

/// The identity of one external client connection, allocated by the runtime per connection and recorded in traces
/// (LANG-243, DIST-065).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct SessionId(pub u64);

/// A lattice value as data, in canonical form (LANGUAGE §11.5). Which shapes are valid for which lattice type,
/// and every operation on them, is defined by `blossom-lattice`; this type only stores and orders them.
///
/// Canonical form is the lattice library's obligation (for example: no ⊥ entries in a map, sorted sets), so
/// structural equality is value equality and the derived order is LANGUAGE §5.5's "compare by canonical form".
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum LatValue {
    /// ⊥ of a lattice with an adjoined or distinguished bottom (`LMax`, `LMin`, `LPoint`, `LConflict`, `LWithBot`,
    /// `LUnit`).
    Bottom,
    /// ⊤ where the lattice has one (`LWithTop`, `LConflict`).
    Top,
    /// `LBool`.
    Bool(bool),
    /// One carried element (`LMax`, `LMin`, `LPoint`, `LConflict`).
    Elem(Arc<Value>),
    /// A set of elements (`LSet`, `LPSet`, `LUnionFind`'s classes, `LDom`'s pairs, dot sets).
    Set(#[serde(with = "serde_util::set_strict")] Arc<BTreeSet<Value>>),
    /// Keys to lattice values (`LMap`, `VClock`); ⊥ values are absent.
    Map(#[serde(with = "serde_util::map_pairs")] Arc<BTreeMap<Value, LatValue>>),
    /// Element multiplicities (`LBag`); zero counts are absent.
    Bag(#[serde(with = "serde_util::map_pairs")] Arc<BTreeMap<Value, u64>>),
    /// A fixed sequence of lattice values (products, `LPair`, `Lex`, `LVec`, tombstone and causal pairs).
    Seq(Arc<[LatValue]>),
    /// An `extern lattice` value (LANG-135): its codec and encoded bytes.
    Extern {
        /// The host type's codec.
        codec: ExternCodecId,
        /// The encoded value.
        bytes: Arc<[u8]>,
    },
}

/// A group or ring value as data (LANG-138, LANG-142). Group arithmetic lives in `blossom-lattice`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum GroupValue {
    /// `Z`: a checked `i64`.
    Z(i64),
    /// `Zn<N>`: a residue modulo `N`.
    Zn(u64),
    /// `ZSet<T>`: element weights; zero weights are absent.
    ZSet(#[serde(with = "serde_util::map_pairs")] Arc<BTreeMap<Value, i64>>),
    /// A tuple of group values.
    Tuple(Arc<[GroupValue]>),
    /// Keys to group values; zero values are absent.
    Map(#[serde(with = "serde_util::map_pairs")] Arc<BTreeMap<Value, GroupValue>>),
    /// A value of a user `impl Group`/`impl Ring` type: its carrier value.
    User(Arc<Value>),
}

/// Convenience constructors.
impl Value {
    /// A `String`.
    pub fn str(s: &str) -> Value {
        Value::Str(s.into())
    }

    /// `Bytes`.
    pub fn bytes(b: &[u8]) -> Value {
        Value::Bytes(b.into())
    }

    /// A `u64`.
    pub fn u64(n: u64) -> Value {
        Value::Int(IntValue::U64(n))
    }

    /// An `i64`.
    pub fn i64(n: i64) -> Value {
        Value::Int(IntValue::I64(n))
    }

    /// A `u32`.
    pub fn u32(n: u32) -> Value {
        Value::Int(IntValue::U32(n))
    }

    /// A tuple.
    pub fn tuple(items: impl IntoIterator<Item = Value>) -> Value {
        Value::Tuple(items.into_iter().collect())
    }

    /// A struct from its fields in declaration order.
    pub fn record(fields: impl IntoIterator<Item = Value>) -> Value {
        Value::Struct(fields.into_iter().collect())
    }

    /// An enum value.
    pub fn variant(variant: u32, fields: impl IntoIterator<Item = Value>) -> Value {
        Value::Enum {
            variant,
            fields: fields.into_iter().collect(),
        }
    }

    /// A `Vec`.
    pub fn vec(items: impl IntoIterator<Item = Value>) -> Value {
        Value::Vec(items.into_iter().collect())
    }

    /// A `Set` (duplicates collapse, as for any set).
    pub fn set(items: impl IntoIterator<Item = Value>) -> Value {
        Value::Set(Arc::new(items.into_iter().collect()))
    }

    /// A `Map`; fails on a duplicate key instead of keeping one of the values.
    pub fn map(entries: impl IntoIterator<Item = (Value, Value)>) -> Result<Value, ValueError> {
        let mut map = BTreeMap::new();
        for (k, v) in entries {
            if map.insert(k.clone(), v).is_some() {
                return Err(ValueError::InvalidValue(format!("duplicate map key {k:?}")));
            }
        }
        Ok(Value::Map(Arc::new(map)))
    }

    /// `Some(v)`.
    pub fn some(v: Value) -> Value {
        Value::Option(Some(Arc::new(v)))
    }

    /// `None`.
    pub const fn none() -> Value {
        Value::Option(None)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::testgen::arb_value;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn value_serde_roundtrip(v in arb_value()) {
            // JSON (text; map keys are not strings, so maps serialize as pairs) and postcard (compact binary).
            let json = serde_json::to_string(&v).unwrap();
            let back: Value = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(&back, &v);
            let bytes = postcard::to_allocvec(&v).unwrap();
            let back: Value = postcard::from_bytes(&bytes).unwrap();
            prop_assert_eq!(&back, &v);
        }
    }

    #[test]
    fn value_serde_roundtrip_edge_cases() {
        let cases = [
            Value::F64(-0.0),
            Value::F64(f64::from_bits(0x7ff8_0000_dead_beef)),
            Value::Int(IntValue::U128(u128::MAX)),
            Value::Int(IntValue::I128(i128::MIN)),
            Value::Mod(ModValue::new(256, [u64::MAX; 4]).unwrap()),
            Value::map([(Value::tuple([Value::u64(1)]), Value::none())]).unwrap(),
            Value::UnknownVariant {
                variant: 5,
                wire_number: 9,
                payload: [0u8, 255].into(),
            },
        ];
        for v in cases {
            let json = serde_json::to_string(&v).unwrap();
            let back: Value = serde_json::from_str(&json).unwrap();
            assert_eq!(back, v, "{json}");
            // Bit-exact, not just equal under the order.
            if let (Value::F64(a), Value::F64(b)) = (&back, &v) {
                assert_eq!(a.to_bits(), b.to_bits());
            }
            assert_eq!(
                postcard::from_bytes::<Value>(&postcard::to_allocvec(&v).unwrap()).unwrap(),
                v
            );
        }
    }

    #[test]
    fn value_serde_rejects_duplicates() {
        // A set or map with a repeated element is not a canonical encoding.
        let set = r#"{"Set":["Unit","Unit"]}"#;
        assert!(serde_json::from_str::<Value>(set).is_err());
        let map = r#"{"Map":[["Unit",{"Bool":true}],["Unit",{"Bool":false}]]}"#;
        assert!(serde_json::from_str::<Value>(map).is_err());
        assert!(serde_json::from_str::<Value>(r#"{"Map":[["Unit",{"Bool":true}]]}"#).is_ok());
    }

    #[test]
    fn int_value_ranges() {
        assert_eq!(IntValue::from_i128(IntTy::U8, 255), Some(IntValue::U8(255)));
        assert_eq!(IntValue::from_i128(IntTy::U8, 256), None);
        assert_eq!(IntValue::from_i128(IntTy::I8, -129), None);
        assert_eq!(IntValue::from_i128(IntTy::U128, -1), None);
        assert_eq!(IntValue::U128(u128::MAX).to_i128(), None);
        assert_eq!(IntValue::I16(-3).to_i128(), Some(-3));
        assert_eq!(IntValue::U32(7).ty(), IntTy::U32);
    }

    #[test]
    fn mod_value_width_checked() {
        assert!(ModValue::new(0, [0; 4]).is_err());
        assert!(ModValue::new(257, [0; 4]).is_err());
        assert!(ModValue::from_u64(8, 255).is_ok());
        assert!(ModValue::from_u64(8, 256).is_err());
        assert!(ModValue::from_u64(64, u64::MAX).is_ok());
        assert!(ModValue::new(65, [0, 0, 1, 0]).is_ok());
        assert!(ModValue::new(65, [0, 0, 2, 0]).is_err());
        assert!(ModValue::new(160, [0, 0xffff_ffff, u64::MAX, u64::MAX]).is_ok());
        assert!(ModValue::new(160, [0, 0x1_0000_0000, 0, 0]).is_err());
        assert!(ModValue::new(256, [u64::MAX; 4]).is_ok());
        let m = ModValue::from_u64(16, 9).unwrap();
        assert_eq!((m.bits(), m.limbs()), (16, [0, 0, 0, 9]));
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<ModValue>(&json).unwrap(), m);
        assert!(serde_json::from_str::<ModValue>(r#"{"bits":4,"limbs":[0,0,0,16]}"#).is_err());
    }

    #[test]
    fn map_constructor_rejects_duplicates() {
        assert!(Value::map([(Value::u64(1), Value::Unit), (Value::u64(1), Value::Unit)]).is_err());
        let m = Value::map([(Value::u64(2), Value::Unit), (Value::u64(1), Value::Unit)]).unwrap();
        let Value::Map(entries) = m else {
            panic!("expected a map")
        };
        assert_eq!(
            entries.keys().cloned().collect::<Vec<_>>(),
            vec![Value::u64(1), Value::u64(2)]
        );
    }
}
