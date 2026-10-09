//! The canonical total order on values (LANGUAGE §5.5; LANG-024, SEM-088).
//!
//! Every type has one total order, used by `<` on values, by `index!`, `top!`, `collect!`, `percentile!`,
//! tie-breaking in `choose*!`, host callbacks, `stdout` and dumps:
//!
//! - numbers compare numerically; integers of different types (a type error upstream) order by [`IntTy`] first;
//! - `f64` by IEEE 754 `totalOrder`: `-NaN < -∞ < … < -0.0 < +0.0 < … < +∞ < +NaN`, NaNs by payload;
//! - `false < true`; `String`, `Bytes` and `Principal` lexicographically by bytes;
//! - tuples and structs field by field; enums by variant number, then payload (an unknown variant after a known
//!   one with the same number, then by wire number and bytes); `None < Some(x)`;
//! - `Vec` lexicographically; `Set` and `Map` as their sorted sequences;
//! - `Node` by node id, then keyed members by role name and key; `Duration` and `Instant` numerically; `Mod` by width, then numerically;
//! - `Blob` by content address; lattice and group values by their canonical form (for deduplication and ties
//!   only: this is not the lattice order);
//! - values of different types (a type error upstream) by a fixed rank of their kind.
//!
//! The order never uses intern ids, hashes, pointers or arrival order. Equality and hashing are consistent with
//! it: `a == b` exactly when `a.cmp(b) == Equal` (so `-0.0 != +0.0` and a NaN equals itself), and equal values
//! hash equally.
//!
//! [`IntTy`]: crate::IntTy

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::value::Value;

impl Value {
    /// The rank of the value's kind: the order between values of different types.
    fn rank(&self) -> u8 {
        match self {
            Value::Unit => 0,
            Value::Bool(_) => 1,
            Value::Int(_) => 2,
            Value::F64(_) => 3,
            Value::Str(_) => 4,
            Value::Bytes(_) => 5,
            Value::Duration(_) => 6,
            Value::Instant(_) => 7,
            Value::Mod(_) => 8,
            Value::Blob(_) => 9,
            Value::Session(_) => 10,
            Value::Principal(_) => 11,
            // A keyed member orders after every node id (see `cmp`).
            Value::Node(_) | Value::Member(_) => 12,
            Value::Tuple(_) => 13,
            Value::Struct(_) => 14,
            // Known and unknown variants of one enum type interleave by variant number.
            Value::Enum { .. } | Value::UnknownVariant { .. } => 15,
            Value::Vec(_) => 16,
            Value::Set(_) => 17,
            Value::Map(_) => 18,
            Value::Option(_) => 19,
            Value::Lattice(_) => 20,
            Value::Group(_) => 21,
            Value::Extern { .. } => 22,
            Value::Conn(_) => 23,
        }
    }

    /// The canonical order (LANGUAGE §5.5); the same as `Ord::cmp`.
    pub fn canonical_cmp(&self, other: &Value) -> Ordering {
        self.cmp(other)
    }
}

/// Compares two shared payloads, short-cutting when both are the same allocation.
fn cmp_shared<T: Ord + ?Sized>(a: &Arc<T>, b: &Arc<T>) -> Ordering {
    if Arc::ptr_eq(a, b) {
        Ordering::Equal
    } else {
        (**a).cmp(&**b)
    }
}

// FEATURE: LANG-024
impl Ord for Value {
    fn cmp(&self, other: &Value) -> Ordering {
        use Value as V;
        match (self, other) {
            (V::Unit, V::Unit) => Ordering::Equal,
            (V::Bool(a), V::Bool(b)) => a.cmp(b),
            (V::Int(a), V::Int(b)) => a.cmp(b),
            (V::F64(a), V::F64(b)) => a.total_cmp(b),
            (V::Str(a), V::Str(b)) => cmp_shared(a, b),
            (V::Bytes(a), V::Bytes(b)) => cmp_shared(a, b),
            (V::Duration(a), V::Duration(b)) => a.cmp(b),
            (V::Instant(a), V::Instant(b)) => a.cmp(b),
            (V::Mod(a), V::Mod(b)) => a.cmp(b),
            (V::Blob(a), V::Blob(b)) => a.cmp(b),
            (V::Session(a), V::Session(b)) => a.cmp(b),
            (V::Conn(a), V::Conn(b)) => a.cmp(b),
            (V::Principal(a), V::Principal(b)) => cmp_shared(a, b),
            (V::Node(a), V::Node(b)) => a.cmp(b),
            (V::Member(a), V::Member(b)) => a.cmp(b),
            (V::Node(_), V::Member(_)) => Ordering::Less,
            (V::Member(_), V::Node(_)) => Ordering::Greater,
            (V::Tuple(a), V::Tuple(b)) | (V::Struct(a), V::Struct(b)) | (V::Vec(a), V::Vec(b)) => cmp_shared(a, b),
            (
                V::Enum {
                    variant: va,
                    fields: fa,
                },
                V::Enum {
                    variant: vb,
                    fields: fb,
                },
            ) => va.cmp(vb).then_with(|| cmp_shared(fa, fb)),
            (V::Enum { variant: va, .. }, V::UnknownVariant { variant: vb, .. }) => va.cmp(vb).then(Ordering::Less),
            (V::UnknownVariant { variant: va, .. }, V::Enum { variant: vb, .. }) => va.cmp(vb).then(Ordering::Greater),
            (
                V::UnknownVariant {
                    variant: va,
                    wire_number: wa,
                    payload: pa,
                },
                V::UnknownVariant {
                    variant: vb,
                    wire_number: wb,
                    payload: pb,
                },
            ) => va.cmp(vb).then_with(|| wa.cmp(wb)).then_with(|| cmp_shared(pa, pb)),
            (V::Set(a), V::Set(b)) => cmp_shared(a, b),
            (V::Map(a), V::Map(b)) => cmp_shared(a, b),
            (V::Option(a), V::Option(b)) => match (a, b) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Less,
                (Some(_), None) => Ordering::Greater,
                (Some(a), Some(b)) => cmp_shared(a, b),
            },
            (V::Lattice(a), V::Lattice(b)) => a.cmp(b),
            (V::Group(a), V::Group(b)) => a.cmp(b),
            (V::Extern { codec: ca, bytes: ba }, V::Extern { codec: cb, bytes: bb }) => {
                ca.cmp(cb).then_with(|| cmp_shared(ba, bb))
            }
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Value) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Value {}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.rank().hash(state);
        match self {
            Value::Unit => {}
            Value::Bool(b) => b.hash(state),
            Value::Int(i) => i.hash(state),
            // Equal floats have identical bits (totalOrder), so hashing the bits is consistent with Eq.
            Value::F64(f) => f.to_bits().hash(state),
            Value::Str(s) | Value::Principal(s) => s.hash(state),
            Value::Bytes(b) => b.hash(state),
            Value::Duration(d) => d.hash(state),
            Value::Instant(t) => t.hash(state),
            Value::Mod(m) => m.hash(state),
            Value::Blob(b) => b.hash(state),
            Value::Session(s) => s.hash(state),
            Value::Conn(c) => c.hash(state),
            Value::Node(n) => n.hash(state),
            Value::Member(m) => m.hash(state),
            Value::Tuple(items) | Value::Struct(items) | Value::Vec(items) => items.hash(state),
            Value::Enum { variant, fields } => {
                variant.hash(state);
                0u8.hash(state);
                fields.hash(state);
            }
            Value::UnknownVariant {
                variant,
                wire_number,
                payload,
            } => {
                variant.hash(state);
                1u8.hash(state);
                wire_number.hash(state);
                payload.hash(state);
            }
            Value::Set(s) => s.hash(state),
            Value::Map(m) => m.hash(state),
            Value::Option(o) => o.hash(state),
            Value::Lattice(l) => l.hash(state),
            Value::Group(g) => g.hash(state),
            Value::Extern { codec, bytes } => {
                codec.hash(state);
                bytes.hash(state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::hash::BuildHasher;

    use blossom_base::DetState;
    use proptest::prelude::*;

    use super::*;
    use crate::testgen::{arb_dense_value, arb_value};
    use crate::time::{Duration, Instant, NodeId};
    use crate::types::ExternCodecId;
    use crate::value::{BlobRef, GroupValue, IntValue, LatValue, ModValue, SessionId};

    fn hash_of(v: &Value) -> u64 {
        DetState::fixed().hash_one(v)
    }

    /// Asserts that `values` is strictly increasing under the canonical order, pairwise.
    fn assert_strictly_increasing(values: &[Value]) {
        for (i, a) in values.iter().enumerate() {
            for (j, b) in values.iter().enumerate() {
                assert_eq!(a.cmp(b), i.cmp(&j), "{a:?} vs {b:?}");
                assert_eq!(a == b, i == j);
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn canonical_order_total(a in arb_value(), b in arb_value(), c in arb_value()) {
            // Antisymmetry (and totality: cmp always answers).
            prop_assert_eq!(a.cmp(&b), b.cmp(&a).reverse());
            // Consistency with Eq, and of Hash with Eq.
            prop_assert_eq!(a == b, a.cmp(&b) == Ordering::Equal);
            if a == b {
                prop_assert_eq!(hash_of(&a), hash_of(&b));
            }
            // Reflexivity, also across separate allocations.
            let deep = deep_copy(&a);
            prop_assert_eq!(a.cmp(&deep), Ordering::Equal);
            prop_assert_eq!(hash_of(&a), hash_of(&deep));
            // Transitivity.
            if a <= b && b <= c {
                prop_assert!(a <= c, "{:?} <= {:?} <= {:?}", a, b, c);
            }
            if a < b && b < c {
                prop_assert!(a < c);
            }
        }

        #[test]
        fn canonical_order_total_dense(values in proptest::collection::vec(arb_dense_value(), 2..14)) {
            // Every pair and triple of a small sample over tiny domains: most share a kind, many are equal.
            for a in &values {
                for b in &values {
                    prop_assert_eq!(a.cmp(b), b.cmp(a).reverse());
                    prop_assert_eq!(a == b, a.cmp(b) == Ordering::Equal);
                    if a == b {
                        prop_assert_eq!(hash_of(a), hash_of(b));
                    }
                    for c in &values {
                        if a <= b && b <= c {
                            prop_assert!(a <= c, "{:?} <= {:?} <= {:?}", a, b, c);
                        }
                        if a < b && b < c {
                            prop_assert!(a < c, "{:?} < {:?} < {:?}", a, b, c);
                        }
                    }
                }
            }
        }

        #[test]
        fn canonical_order_total_sorting_is_consistent(mut values in proptest::collection::vec(arb_value(), 0..24)) {
            values.sort();
            for w in values.windows(2) {
                prop_assert!(w[0] <= w[1]);
            }
            // Every pair respects the sorted positions (transitivity over the whole sample).
            for i in 0..values.len() {
                for j in i..values.len() {
                    prop_assert!(values[i] <= values[j]);
                }
            }
        }
    }

    /// A structurally equal copy with no shared allocations.
    fn deep_copy(v: &Value) -> Value {
        use Value as V;
        let copy_all = |items: &Arc<[Value]>| items.iter().map(deep_copy).collect::<Arc<[Value]>>();
        match v {
            V::Str(s) => V::Str(Arc::from(&**s)),
            V::Principal(s) => V::Principal(Arc::from(&**s)),
            V::Bytes(b) => V::Bytes(Arc::from(&**b)),
            V::Tuple(items) => V::Tuple(copy_all(items)),
            V::Struct(items) => V::Struct(copy_all(items)),
            V::Vec(items) => V::Vec(copy_all(items)),
            V::Enum { variant, fields } => V::Enum {
                variant: *variant,
                fields: copy_all(fields),
            },
            V::UnknownVariant {
                variant,
                wire_number,
                payload,
            } => V::UnknownVariant {
                variant: *variant,
                wire_number: *wire_number,
                payload: Arc::from(&**payload),
            },
            V::Set(s) => V::Set(Arc::new(s.iter().map(deep_copy).collect())),
            V::Map(m) => V::Map(Arc::new(m.iter().map(|(k, v)| (deep_copy(k), deep_copy(v))).collect())),
            V::Option(o) => V::Option(o.as_ref().map(|x| Arc::new(deep_copy(x)))),
            V::Extern { codec, bytes } => V::Extern {
                codec: codec.clone(),
                bytes: Arc::from(&**bytes),
            },
            other => other.clone(),
        }
    }

    #[test]
    fn canonical_order_f64() {
        let neg_nan_big = f64::from_bits(0xfff8_0000_0000_0001);
        let neg_nan = f64::from_bits(0xfff8_0000_0000_0000);
        let pos_nan = f64::from_bits(0x7ff8_0000_0000_0000);
        let pos_nan_big = f64::from_bits(0x7ff8_0000_0000_0001);
        let order: Vec<Value> = [
            neg_nan_big,
            neg_nan,
            f64::NEG_INFINITY,
            -1.5,
            -f64::MIN_POSITIVE,
            -0.0,
            0.0,
            f64::MIN_POSITIVE,
            1.5,
            f64::INFINITY,
            pos_nan,
            pos_nan_big,
        ]
        .into_iter()
        .map(Value::F64)
        .collect();
        assert_strictly_increasing(&order);
        // -0.0 and +0.0 are distinct values; a NaN equals itself.
        assert_ne!(Value::F64(-0.0), Value::F64(0.0));
        assert_eq!(Value::F64(pos_nan), Value::F64(pos_nan));
        assert_eq!(hash_of(&Value::F64(pos_nan)), hash_of(&Value::F64(pos_nan)));
    }

    #[test]
    fn canonical_order_scalars() {
        assert_strictly_increasing(&[Value::Bool(false), Value::Bool(true)]);
        assert_strictly_increasing(&[Value::i64(-5), Value::i64(0), Value::i64(7)]);
        // Integers of different types order by type first (never compared by programs).
        assert_strictly_increasing(&[
            Value::Int(IntValue::U8(200)),
            Value::Int(IntValue::U64(1)),
            Value::Int(IntValue::I8(-100)),
            Value::Int(IntValue::I64(-100)),
        ]);
        // Strings and bytes by bytes: "B" < "a" < "z" < "é".
        assert_strictly_increasing(&[
            Value::str(""),
            Value::str("B"),
            Value::str("a"),
            Value::str("z"),
            Value::str("é"),
        ]);
        assert_strictly_increasing(&[
            Value::bytes(b""),
            Value::bytes(&[0]),
            Value::bytes(&[0, 0]),
            Value::bytes(&[1]),
        ]);
        // Keyed members after every node id (a client member's too), by role name, then key; the role's id in a
        // program does not count.
        let member = |r: u32, name: &str, k: &str| {
            Value::Member(crate::time::MemberRef::new(blossom_base::RoleId::from_raw(r), name, k))
        };
        assert_strictly_increasing(&[
            Value::Node(NodeId(0)),
            Value::Node(NodeId(3)),
            Value::Node(NodeId(u32::MAX)),
            member(1, "Game", "b"),
            member(1, "Game", "game-1"),
            member(0, "Room", "a"),
        ]);
        assert_eq!(member(0, "Game", "a"), member(7, "Game", "a"));
        assert_strictly_increasing(&[Value::Duration(Duration(-1)), Value::Duration(Duration(2))]);
        assert_strictly_increasing(&[Value::Instant(Instant(-1)), Value::Instant(Instant(2))]);
        assert_strictly_increasing(&[
            Value::Mod(ModValue::from_u64(8, 200).unwrap()),
            Value::Mod(ModValue::from_u64(64, 3).unwrap()),
            Value::Mod(ModValue::new(64, [0, 0, 0, u64::MAX]).unwrap()),
            Value::Mod(ModValue::new(160, [0, 0, 1, 0]).unwrap()),
            Value::Mod(ModValue::new(160, [0, 1, 0, 0]).unwrap()),
        ]);
        let blob = |h: u8, len| Value::Blob(BlobRef { hash: [h; 32], len });
        assert_strictly_increasing(&[blob(1, 9), blob(2, 0), blob(2, 5)]);
        assert_strictly_increasing(&[Value::Session(SessionId(1)), Value::Session(SessionId(2))]);
    }

    #[test]
    fn canonical_order_nested() {
        // None < Some(x), and Some compares its payload.
        assert_strictly_increasing(&[Value::none(), Value::some(Value::i64(-1)), Value::some(Value::i64(4))]);
        // Vec is lexicographic: [] < [1] < [1, 0] < [2].
        let v = |xs: &[i64]| Value::vec(xs.iter().map(|x| Value::i64(*x)));
        assert_strictly_increasing(&[v(&[]), v(&[1]), v(&[1, 0]), v(&[2])]);
        // Tuples and structs field by field.
        let t = |a: i64, b: &str| Value::tuple([Value::i64(a), Value::str(b)]);
        assert_strictly_increasing(&[t(1, "z"), t(2, "a"), t(2, "b")]);
        let s = |a: bool, b: i64| Value::record([Value::Bool(a), Value::i64(b)]);
        assert_strictly_increasing(&[s(false, 9), s(true, 0)]);
        // Sets compare as their sorted sequences: {1,2} < {1,3} < {2}.
        let set = |xs: &[i64]| Value::set(xs.iter().map(|x| Value::i64(*x)));
        assert_strictly_increasing(&[set(&[]), set(&[2, 1]), set(&[3, 1]), set(&[2])]);
        assert_eq!(set(&[2, 1, 2]), set(&[1, 2]));
        // Maps compare as sorted (key, value) sequences.
        let map = |xs: &[(i64, i64)]| Value::map(xs.iter().map(|(k, v)| (Value::i64(*k), Value::i64(*v)))).unwrap();
        assert_strictly_increasing(&[
            map(&[]),
            map(&[(1, 6), (0, 9)]),
            map(&[(1, 5)]),
            map(&[(1, 6)]),
            map(&[(2, 0)]),
        ]);
        // Enums by variant number, then payload; an unknown variant after the known value with its number.
        let e = |n: u32, xs: &[i64]| Value::variant(n, xs.iter().map(|x| Value::i64(*x)));
        let unknown = |wire: u32, bytes: &[u8]| Value::UnknownVariant {
            variant: 5,
            wire_number: wire,
            payload: bytes.into(),
        };
        assert_strictly_increasing(&[
            e(1, &[9]),
            e(2, &[]),
            e(2, &[0]),
            e(5, &[]),
            unknown(7, b""),
            unknown(7, b"a"),
            unknown(9, b""),
            e(6, &[]),
        ]);
        // Deep nesting.
        let deep = |x: i64| Value::vec([Value::some(Value::tuple([set(&[x]), map(&[(x, x)])]))]);
        assert_strictly_increasing(&[deep(1), deep(2), deep(3)]);
        // Values of different kinds order by kind rank (type errors upstream, but the order stays total).
        assert_strictly_increasing(&[
            Value::Unit,
            Value::Bool(false),
            Value::i64(0),
            Value::F64(0.0),
            Value::str(""),
        ]);
    }

    #[test]
    fn canonical_order_lattice_and_group_values() {
        let lset = |xs: &[i64]| LatValue::Set(Arc::new(xs.iter().map(|x| Value::i64(*x)).collect::<BTreeSet<_>>()));
        assert_strictly_increasing(&[
            Value::Lattice(LatValue::Bottom),
            Value::Lattice(LatValue::Elem(Arc::new(Value::i64(3)))),
            Value::Lattice(lset(&[1, 2])),
            Value::Lattice(lset(&[1, 3])),
        ]);
        let zset = |xs: &[(i64, i64)]| {
            GroupValue::ZSet(Arc::new(
                xs.iter().map(|(k, w)| (Value::i64(*k), *w)).collect::<BTreeMap<_, _>>(),
            ))
        };
        assert_strictly_increasing(&[
            Value::Group(GroupValue::Z(-1)),
            Value::Group(zset(&[(1, -1)])),
            Value::Group(zset(&[(1, 2)])),
        ]);
        let ext = |c: &str, b: &[u8]| Value::Extern {
            codec: ExternCodecId(c.into()),
            bytes: b.into(),
        };
        assert_strictly_increasing(&[ext("a", b"z"), ext("b", b""), ext("b", b"a")]);
    }
}
