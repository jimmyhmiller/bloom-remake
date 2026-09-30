//! Type-directed value strategies for downstream property tests.
use crate::error::ValueError;
use crate::time::{Duration, Instant, NodeId};
use crate::types::{IntTy, TypeDef, TypeTable};
use crate::value::{BlobRef, ConnId, GroupValue, IntValue, LatValue, ModValue, SessionId, Value};
use blossom_base::TypeId;
use proptest::prelude::*;
use proptest::strategy::Union;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

fn fields(types: &TypeTable, items: &[TypeId]) -> Result<BoxedStrategy<Vec<Value>>, ValueError> {
    let mut result: BoxedStrategy<Vec<Value>> = Just(vec![]).boxed();
    for ty in items {
        let child = value_for(types, *ty)?;
        result = (result, child)
            .prop_map(|(mut all, v)| {
                all.push(v);
                all
            })
            .boxed();
    }
    Ok(result)
}
fn int_strategy(t: IntTy) -> BoxedStrategy<Value> {
    match t {
        IntTy::U8 => any::<u8>().prop_map(|v| Value::Int(IntValue::U8(v))).boxed(),
        IntTy::U16 => any::<u16>().prop_map(|v| Value::Int(IntValue::U16(v))).boxed(),
        IntTy::U32 => any::<u32>().prop_map(|v| Value::Int(IntValue::U32(v))).boxed(),
        IntTy::U64 => any::<u64>().prop_map(|v| Value::Int(IntValue::U64(v))).boxed(),
        IntTy::U128 => any::<u128>().prop_map(|v| Value::Int(IntValue::U128(v))).boxed(),
        IntTy::I8 => any::<i8>().prop_map(|v| Value::Int(IntValue::I8(v))).boxed(),
        IntTy::I16 => any::<i16>().prop_map(|v| Value::Int(IntValue::I16(v))).boxed(),
        IntTy::I32 => any::<i32>().prop_map(|v| Value::Int(IntValue::I32(v))).boxed(),
        IntTy::I64 => any::<i64>().prop_map(|v| Value::Int(IntValue::I64(v))).boxed(),
        IntTy::I128 => any::<i128>().prop_map(|v| Value::Int(IntValue::I128(v))).boxed(),
    }
}
fn mod_strategy(bits: u16) -> BoxedStrategy<Value> {
    any::<[u64; 4]>()
        .prop_filter_map("masked Mod limbs", move |mut limbs| {
            for (i, limb) in limbs.iter_mut().enumerate() {
                let low = 64 * (3 - i as u32);
                let allowed = u32::from(bits).saturating_sub(low).min(64);
                *limb &= if allowed == 64 {
                    u64::MAX
                } else if allowed == 0 {
                    0
                } else {
                    (1u64 << allowed) - 1
                };
            }
            ModValue::new(bits, limbs).ok().map(Value::Mod)
        })
        .boxed()
}
fn lattice() -> BoxedStrategy<Value> {
    let leaf = prop_oneof![
        Just(LatValue::Bottom),
        Just(LatValue::Top),
        any::<bool>().prop_map(LatValue::Bool),
        any::<i64>().prop_map(|n| LatValue::Elem(Arc::new(Value::i64(n))))
    ];
    leaf.prop_recursive(3, 48, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(|v| LatValue::Seq(v.into())),
            proptest::collection::btree_map(any::<u8>().prop_map(|n| Value::Int(IntValue::U8(n))), inner, 0..4)
                .prop_map(|m| LatValue::Map(Arc::new(m)))
        ]
    })
    .prop_map(Value::Lattice)
    .boxed()
}
fn group() -> BoxedStrategy<Value> {
    let leaf = prop_oneof![
        any::<i64>().prop_map(GroupValue::Z),
        any::<u64>().prop_map(GroupValue::Zn),
        any::<i64>().prop_map(|n| GroupValue::User(Arc::new(Value::i64(n))))
    ];
    leaf.prop_recursive(3, 48, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(|v| GroupValue::Tuple(v.into())),
            proptest::collection::btree_map(any::<u8>().prop_map(|n| Value::Int(IntValue::U8(n))), inner, 0..4)
                .prop_map(|m| GroupValue::Map(Arc::new(m)))
        ]
    })
    .prop_map(Value::Group)
    .boxed()
}
/// Generate values conforming to `ty`, including nested rows and lattice/group data.
/// The type table is acyclic (children precede parents), so strategy construction terminates.
pub fn value_for(types: &TypeTable, ty: TypeId) -> Result<BoxedStrategy<Value>, ValueError> {
    Ok(match types.def(ty)? {
        TypeDef::Bool => any::<bool>().prop_map(Value::Bool).boxed(),
        TypeDef::Int(t) => int_strategy(*t),
        TypeDef::F64 => any::<u64>().prop_map(|bits| Value::F64(f64::from_bits(bits))).boxed(),
        TypeDef::Str => "[a-zA-Z0-9é]{0,12}".prop_map(|s| Value::str(&s)).boxed(),
        TypeDef::Bytes => proptest::collection::vec(any::<u8>(), 0..16)
            .prop_map(|v| Value::Bytes(v.into()))
            .boxed(),
        TypeDef::Unit => Just(Value::Unit).boxed(),
        TypeDef::Duration => any::<i64>().prop_map(|n| Value::Duration(Duration(n))).boxed(),
        TypeDef::Instant => any::<i64>().prop_map(|n| Value::Instant(Instant(n))).boxed(),
        TypeDef::Mod { bits } => mod_strategy(*bits),
        TypeDef::Blob => (any::<[u8; 32]>(), any::<u64>())
            .prop_map(|(hash, len)| Value::Blob(BlobRef { hash, len }))
            .boxed(),
        TypeDef::Session => any::<u64>().prop_map(|n| Value::Session(SessionId(n))).boxed(),
        TypeDef::Conn => any::<u64>().prop_map(|n| Value::Conn(ConnId(n))).boxed(),
        TypeDef::Principal => "[a-z]{1,12}".prop_map(|s| Value::Principal(s.into())).boxed(),
        TypeDef::Node(_) => any::<u32>().prop_map(|n| Value::Node(NodeId(n))).boxed(),
        TypeDef::Tuple(items) => fields(types, items)?.prop_map(Value::tuple).boxed(),
        TypeDef::Struct(s) => fields(types, &s.fields.iter().map(|f| f.ty).collect::<Vec<_>>())?
            .prop_map(Value::record)
            .boxed(),
        TypeDef::Enum(e) => {
            if e.variants.is_empty() {
                return Err(ValueError::InvalidType(
                    "cannot generate a value of an empty enum".into(),
                ));
            }
            let mut variants = Vec::new();
            for v in &e.variants {
                let n = v.number;
                variants.push(
                    fields(types, &v.payload.iter().map(|f| f.ty).collect::<Vec<_>>())?
                        .prop_map(move |items| Value::variant(n, items))
                        .boxed(),
                );
            }
            Union::new(variants).boxed()
        }
        TypeDef::Vec(t) => proptest::collection::vec(value_for(types, *t)?, 0..5)
            .prop_map(Value::vec)
            .boxed(),
        TypeDef::Set(t) => proptest::collection::btree_set(value_for(types, *t)?, 0..5)
            .prop_map(|s: BTreeSet<Value>| Value::Set(Arc::new(s)))
            .boxed(),
        TypeDef::Map(k, v) => proptest::collection::btree_map(value_for(types, *k)?, value_for(types, *v)?, 0..5)
            .prop_map(|m: BTreeMap<Value, Value>| Value::Map(Arc::new(m)))
            .boxed(),
        TypeDef::Option(t) => proptest::option::of(value_for(types, *t)?)
            .prop_map(|v| Value::Option(v.map(Arc::new)))
            .boxed(),
        TypeDef::Lattice(_) => lattice(),
        TypeDef::Group(_) => group(),
        TypeDef::Extern(e) => {
            let codec = e.codec.clone();
            proptest::collection::vec(any::<u8>(), 0..16)
                .prop_map(move |bytes| Value::Extern {
                    codec: codec.clone(),
                    bytes: bytes.into(),
                })
                .boxed()
        }
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_generators_obey_type_table() {
        use proptest::strategy::ValueTree;
        use proptest::test_runner::TestRunner;
        let mut types = TypeTable::new();
        let int = types.insert(TypeDef::Int(IntTy::U64)).unwrap();
        let opt = types.insert(TypeDef::Option(int)).unwrap();
        let vec = types.insert(TypeDef::Vec(opt)).unwrap();
        let map = types.insert(TypeDef::Map(int, vec)).unwrap();
        let mut runner = TestRunner::deterministic();
        for ty in [int, opt, vec, map] {
            let strategy = value_for(&types, ty).unwrap();
            for _ in 0..64 {
                let sample = strategy.new_tree(&mut runner).unwrap().current();
                types.check_value(ty, &sample).unwrap();
            }
        }
    }
}

#[cfg(test)]
mod empty_enum_test {
    use super::*;
    use crate::types::EnumDef;
    use blossom_base::QualName;
    #[test]
    fn uninhabited_enum_strategy_is_error() {
        let mut types = TypeTable::new();
        let name = QualName(vec![blossom_base::Symbol::intern("Empty")].into());
        let ty = types
            .insert(TypeDef::Enum(EnumDef {
                name,
                variants: vec![],
                unknown: None,
                reserved: vec![],
            }))
            .unwrap();
        assert!(matches!(value_for(&types, ty), Err(ValueError::InvalidType(_))));
    }
}
