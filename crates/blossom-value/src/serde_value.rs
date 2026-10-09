//! Deserialization mirrors for the three recursive value enums. Nested fields call back into
//! their public types' guarded `Deserialize` implementation.
use crate::bounded::DepthGuard;
use crate::serde_util;
use crate::time::{Duration, Instant, MemberRef, NodeId};
use crate::types::ExternCodecId;
use crate::value::{BlobRef, ConnId, GroupValue, IntValue, LatValue, ModValue, SessionId, Value};
use serde::{Deserialize, Deserializer};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Deserialize)]
enum ValueRepr {
    Unit,
    Bool(bool),
    Int(IntValue),
    F64(#[serde(with = "serde_util::f64_bits")] f64),
    Str(Arc<str>),
    Bytes(Arc<[u8]>),
    Duration(Duration),
    Instant(Instant),
    Mod(ModValue),
    Blob(BlobRef),
    Session(SessionId),
    Principal(Arc<str>),
    Node(NodeId),
    Tuple(#[serde(deserialize_with = "serde_util::nonempty_tuple::deserialize")] Arc<[Value]>),
    Struct(Arc<[Value]>),
    Enum {
        variant: u32,
        fields: Arc<[Value]>,
    },
    UnknownVariant {
        variant: u32,
        wire_number: u32,
        payload: Arc<[u8]>,
    },
    Vec(Arc<[Value]>),
    Set(#[serde(with = "serde_util::set_strict")] Arc<BTreeSet<Value>>),
    Map(#[serde(with = "serde_util::map_pairs")] Arc<BTreeMap<Value, Value>>),
    Option(Option<Arc<Value>>),
    Lattice(LatValue),
    Group(GroupValue),
    Extern {
        codec: ExternCodecId,
        bytes: Arc<[u8]>,
    },
    Conn(ConnId),
    Member(MemberRef),
}
impl From<ValueRepr> for Value {
    fn from(v: ValueRepr) -> Self {
        match v {
            ValueRepr::Unit => Self::Unit,
            ValueRepr::Bool(v) => Self::Bool(v),
            ValueRepr::Int(v) => Self::Int(v),
            ValueRepr::F64(v) => Self::F64(v),
            ValueRepr::Str(v) => Self::Str(v),
            ValueRepr::Bytes(v) => Self::Bytes(v),
            ValueRepr::Duration(v) => Self::Duration(v),
            ValueRepr::Instant(v) => Self::Instant(v),
            ValueRepr::Mod(v) => Self::Mod(v),
            ValueRepr::Blob(v) => Self::Blob(v),
            ValueRepr::Session(v) => Self::Session(v),
            ValueRepr::Conn(v) => Self::Conn(v),
            ValueRepr::Member(v) => Self::Member(v),
            ValueRepr::Principal(v) => Self::Principal(v),
            ValueRepr::Node(v) => Self::Node(v),
            ValueRepr::Tuple(v) => Self::Tuple(v),
            ValueRepr::Struct(v) => Self::Struct(v),
            ValueRepr::Enum { variant, fields } => Self::Enum { variant, fields },
            ValueRepr::UnknownVariant {
                variant,
                wire_number,
                payload,
            } => Self::UnknownVariant {
                variant,
                wire_number,
                payload,
            },
            ValueRepr::Vec(v) => Self::Vec(v),
            ValueRepr::Set(v) => Self::Set(v),
            ValueRepr::Map(v) => Self::Map(v),
            ValueRepr::Option(v) => Self::Option(v),
            ValueRepr::Lattice(v) => Self::Lattice(v),
            ValueRepr::Group(v) => Self::Group(v),
            ValueRepr::Extern { codec, bytes } => Self::Extern { codec, bytes },
        }
    }
}
impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let _depth = DepthGuard::enter(|| serde::de::Error::custom("value nesting exceeds 128"))?;
        ValueRepr::deserialize(d).map(Into::into)
    }
}
#[derive(Deserialize)]
enum LatRepr {
    Bottom,
    Top,
    Bool(bool),
    Elem(Arc<Value>),
    Set(#[serde(with = "serde_util::set_strict")] Arc<BTreeSet<Value>>),
    Map(#[serde(with = "serde_util::map_pairs")] Arc<BTreeMap<Value, LatValue>>),
    Bag(#[serde(with = "serde_util::map_pairs")] Arc<BTreeMap<Value, u64>>),
    Seq(Arc<[LatValue]>),
    Extern { codec: ExternCodecId, bytes: Arc<[u8]> },
}
impl From<LatRepr> for LatValue {
    fn from(v: LatRepr) -> Self {
        match v {
            LatRepr::Bottom => Self::Bottom,
            LatRepr::Top => Self::Top,
            LatRepr::Bool(v) => Self::Bool(v),
            LatRepr::Elem(v) => Self::Elem(v),
            LatRepr::Set(v) => Self::Set(v),
            LatRepr::Map(v) => Self::Map(v),
            LatRepr::Bag(v) => Self::Bag(v),
            LatRepr::Seq(v) => Self::Seq(v),
            LatRepr::Extern { codec, bytes } => Self::Extern { codec, bytes },
        }
    }
}
impl<'de> Deserialize<'de> for LatValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let _depth = DepthGuard::enter(|| serde::de::Error::custom("value nesting exceeds 128"))?;
        LatRepr::deserialize(d).map(Into::into)
    }
}
#[derive(Deserialize)]
enum GroupRepr {
    Z(i64),
    Zn(u64),
    ZSet(#[serde(with = "serde_util::map_pairs")] Arc<BTreeMap<Value, i64>>),
    Tuple(Arc<[GroupValue]>),
    Map(#[serde(with = "serde_util::map_pairs")] Arc<BTreeMap<Value, GroupValue>>),
    User(Arc<Value>),
}
impl From<GroupRepr> for GroupValue {
    fn from(v: GroupRepr) -> Self {
        match v {
            GroupRepr::Z(v) => Self::Z(v),
            GroupRepr::Zn(v) => Self::Zn(v),
            GroupRepr::ZSet(v) => Self::ZSet(v),
            GroupRepr::Tuple(v) => Self::Tuple(v),
            GroupRepr::Map(v) => Self::Map(v),
            GroupRepr::User(v) => Self::User(v),
        }
    }
}
impl<'de> Deserialize<'de> for GroupValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let _depth = DepthGuard::enter(|| serde::de::Error::custom("value nesting exceeds 128"))?;
        GroupRepr::deserialize(d).map(Into::into)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recursive_decode_is_bounded() {
        let mut bytes = Vec::new();
        for _ in 0..200 {
            bytes.extend_from_slice(&[20, 1]);
        }
        bytes.push(0);
        let result = postcard::from_bytes::<Value>(&bytes);
        assert!(result.is_err());
        let value = Value::some(Value::Lattice(LatValue::Seq(vec![LatValue::Bottom].into())));
        let b = postcard::to_allocvec(&value).unwrap();
        assert_eq!(postcard::from_bytes::<Value>(&b).unwrap(), value);
    }
}
