//! Test-only proptest strategies for values of mixed kinds. The public `arbitrary` feature (WP M2.1) generates
//! values of a given `TypeDef`; these deliberately mix kinds to exercise the order across types too.

use std::collections::BTreeMap;
use std::sync::Arc;

use proptest::prelude::*;

use crate::time::{Duration, Instant, MemberRef, NodeId};
use crate::types::ExternCodecId;
use crate::value::{BlobRef, GroupValue, IntValue, LatValue, ModValue, SessionId, Value};

fn arb_int() -> impl Strategy<Value = IntValue> {
    prop_oneof![
        any::<u8>().prop_map(IntValue::U8),
        any::<u16>().prop_map(IntValue::U16),
        any::<u32>().prop_map(IntValue::U32),
        any::<u64>().prop_map(IntValue::U64),
        any::<u128>().prop_map(IntValue::U128),
        any::<i8>().prop_map(IntValue::I8),
        any::<i16>().prop_map(IntValue::I16),
        any::<i32>().prop_map(IntValue::I32),
        any::<i64>().prop_map(IntValue::I64),
        any::<i128>().prop_map(IntValue::I128),
        // Small values collide often, which exercises equality.
        (0i64..3).prop_map(IntValue::I64),
    ]
}

fn arb_f64() -> impl Strategy<Value = f64> {
    prop_oneof![
        any::<u64>().prop_map(f64::from_bits),
        Just(0.0),
        Just(-0.0),
        Just(f64::NAN),
        Just(f64::INFINITY),
        Just(f64::NEG_INFINITY),
        (-2i32..2).prop_map(f64::from),
    ]
}

fn arb_mod() -> impl Strategy<Value = ModValue> {
    (1u16..=256, any::<[u64; 4]>()).prop_map(|(bits, raw)| {
        // Mask each limb to the width; the result is always valid.
        let mut limbs = raw;
        for (i, limb) in limbs.iter_mut().enumerate() {
            let low = 64 * (3 - i as u32);
            let allowed = u32::from(bits).saturating_sub(low).min(64);
            *limb = if allowed >= 64 {
                *limb
            } else if allowed == 0 {
                0
            } else {
                *limb & ((1u64 << allowed) - 1)
            };
        }
        match ModValue::new(bits, limbs) {
            Ok(m) => m,
            Err(e) => panic!("masked limbs must fit: {e}"),
        }
    })
}

fn arb_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Unit),
        any::<bool>().prop_map(Value::Bool),
        arb_int().prop_map(Value::Int),
        arb_f64().prop_map(Value::F64),
        "[a-c\u{e9}]{0,3}".prop_map(|s| Value::Str(s.as_str().into())),
        proptest::collection::vec(0u8..3, 0..3).prop_map(|b| Value::Bytes(b.into())),
        any::<i64>().prop_map(|n| Value::Duration(Duration(n))),
        (-2i64..2).prop_map(|n| Value::Instant(Instant(n))),
        arb_mod().prop_map(Value::Mod),
        (0u8..2, 0u64..2).prop_map(|(h, len)| Value::Blob(BlobRef { hash: [h; 32], len })),
        (0u64..3).prop_map(|n| Value::Session(SessionId(n))),
        "[xy]{0,2}".prop_map(|s| Value::Principal(s.as_str().into())),
        (0u32..3).prop_map(|n| Value::Node(NodeId(n))),
        (0u32..2, "[gh]{0,2}").prop_map(|(r, k)| Value::Member(MemberRef::new(
            blossom_base::RoleId::from_raw(r),
            ["Game", "Room"][r as usize],
            k.as_str()
        ))),
        ("[pq]", proptest::collection::vec(0u8..2, 0..2)).prop_map(|(c, b)| Value::Extern {
            codec: ExternCodecId(c.as_str().into()),
            bytes: b.into()
        }),
        (0u32..3, 0u32..3, proptest::collection::vec(0u8..2, 0..2)).prop_map(|(v, w, p)| Value::UnknownVariant {
            variant: v,
            wire_number: w,
            payload: p.into()
        }),
    ]
}

fn arb_lattice(inner: BoxedStrategy<Value>) -> impl Strategy<Value = LatValue> {
    let leaf = prop_oneof![
        Just(LatValue::Bottom),
        Just(LatValue::Top),
        any::<bool>().prop_map(LatValue::Bool),
        inner.clone().prop_map(|v| LatValue::Elem(Arc::new(v))),
        proptest::collection::btree_set(inner.clone(), 0..3).prop_map(|s| LatValue::Set(Arc::new(s))),
        proptest::collection::btree_map(inner.clone(), 1u64..3, 0..3).prop_map(|m| LatValue::Bag(Arc::new(m))),
        ("[pq]", proptest::collection::vec(0u8..2, 0..2)).prop_map(|(c, b)| LatValue::Extern {
            codec: ExternCodecId(c.as_str().into()),
            bytes: b.into()
        }),
    ];
    leaf.prop_recursive(2, 8, 3, move |lat| {
        prop_oneof![
            proptest::collection::vec(lat.clone(), 0..3).prop_map(|v| LatValue::Seq(v.into())),
            proptest::collection::btree_map(inner.clone(), lat, 0..3).prop_map(|m| LatValue::Map(Arc::new(m))),
        ]
    })
}

fn arb_group(inner: BoxedStrategy<Value>) -> impl Strategy<Value = GroupValue> {
    let leaf = prop_oneof![
        any::<i64>().prop_map(GroupValue::Z),
        (0u64..5).prop_map(GroupValue::Zn),
        proptest::collection::btree_map(inner.clone(), -2i64..3, 0..3).prop_map(|m| GroupValue::ZSet(Arc::new(m))),
        inner.clone().prop_map(|v| GroupValue::User(Arc::new(v))),
    ];
    leaf.prop_recursive(2, 8, 3, move |g| {
        prop_oneof![
            proptest::collection::vec(g.clone(), 0..3).prop_map(|v| GroupValue::Tuple(v.into())),
            proptest::collection::btree_map(inner.clone(), g, 0..3).prop_map(|m| GroupValue::Map(Arc::new(m))),
        ]
    })
}

/// Values of every kind, nested up to a few levels.
pub(crate) fn arb_value() -> BoxedStrategy<Value> {
    arb_leaf()
        .prop_recursive(3, 24, 4, |inner| {
            let lat_inner = inner.clone();
            let group_inner = inner.clone();
            prop_oneof![
                proptest::collection::vec(inner.clone(), 1..4).prop_map(|v| Value::Tuple(v.into())),
                proptest::collection::vec(inner.clone(), 0..4).prop_map(|v| Value::Struct(v.into())),
                (0u32..3, proptest::collection::vec(inner.clone(), 0..3)).prop_map(|(variant, v)| Value::Enum {
                    variant,
                    fields: v.into()
                }),
                proptest::collection::vec(inner.clone(), 0..4).prop_map(|v| Value::Vec(v.into())),
                proptest::collection::btree_set(inner.clone(), 0..4).prop_map(|s| Value::Set(Arc::new(s))),
                proptest::collection::btree_map(inner.clone(), inner.clone(), 0..3)
                    .prop_map(|m: BTreeMap<Value, Value>| Value::Map(Arc::new(m))),
                proptest::option::of(inner.clone()).prop_map(|o| Value::Option(o.map(Arc::new))),
                arb_lattice(lat_inner).prop_map(Value::Lattice),
                arb_group(group_inner).prop_map(Value::Group),
            ]
        })
        .boxed()
}

/// Values over tiny domains, so equal and nearly equal values of the same kind are common: pairs and triples drawn
/// from a handful of these exercise the same-kind branches of the canonical order (nested collections, enums,
/// signed zeros and NaNs) instead of the cross-kind rank.
pub(crate) fn arb_dense_value() -> BoxedStrategy<Value> {
    let leaf = prop_oneof![
        (0i64..3).prop_map(Value::i64),
        any::<bool>().prop_map(Value::Bool),
        prop_oneof![Just(-0.0), Just(0.0), Just(1.0), Just(f64::NAN), Just(-f64::NAN)].prop_map(Value::F64),
        "[ab]{0,2}".prop_map(|s| Value::Str(s.as_str().into())),
    ];
    leaf.prop_recursive(3, 16, 3, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 1..3).prop_map(|v| Value::Tuple(v.into())),
            proptest::collection::vec(inner.clone(), 0..3).prop_map(|v| Value::Vec(v.into())),
            proptest::collection::btree_set(inner.clone(), 0..3).prop_map(|s| Value::Set(Arc::new(s))),
            proptest::collection::btree_map(inner.clone(), inner.clone(), 0..2)
                .prop_map(|m: BTreeMap<Value, Value>| Value::Map(Arc::new(m))),
            proptest::option::of(inner.clone()).prop_map(|o| Value::Option(o.map(Arc::new))),
            (0u32..2, proptest::collection::vec(inner, 0..2)).prop_map(|(variant, v)| Value::Enum {
                variant,
                fields: v.into()
            }),
        ]
    })
    .boxed()
}
