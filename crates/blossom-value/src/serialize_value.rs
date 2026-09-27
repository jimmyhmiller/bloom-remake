//! Guarded serialization of recursive values; the wire shape matches the derived enum shape.
use crate::bounded::DepthGuard;
use crate::serde_util;
use crate::time::{Duration, Instant, NodeId};
use crate::types::ExternCodecId;
use crate::value::{BlobRef, GroupValue, IntValue, LatValue, ModValue, SessionId, Value};
use serde::{Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
#[derive(Serialize)]
enum ValueRef<'a> {
    Unit,
    Bool(&'a bool),
    Int(&'a IntValue),
    F64(u64),
    Str(&'a Arc<str>),
    Bytes(&'a Arc<[u8]>),
    Duration(&'a Duration),
    Instant(&'a Instant),
    Mod(&'a ModValue),
    Blob(&'a BlobRef),
    Session(&'a SessionId),
    Principal(&'a Arc<str>),
    Node(&'a NodeId),
    Tuple(&'a Arc<[Value]>),
    Struct(&'a Arc<[Value]>),
    Enum {
        variant: &'a u32,
        fields: &'a Arc<[Value]>,
    },
    UnknownVariant {
        variant: &'a u32,
        wire_number: &'a u32,
        payload: &'a Arc<[u8]>,
    },
    Vec(&'a Arc<[Value]>),
    Set(#[serde(with = "set_ref")] &'a Arc<BTreeSet<Value>>),
    Map(#[serde(with = "map_ref")] &'a Arc<BTreeMap<Value, Value>>),
    Option(&'a Option<Arc<Value>>),
    Lattice(&'a LatValue),
    Group(&'a GroupValue),
    Extern {
        codec: &'a ExternCodecId,
        bytes: &'a Arc<[u8]>,
    },
}
mod set_ref {
    use super::*;
    pub fn serialize<S: Serializer>(v: &&Arc<BTreeSet<Value>>, s: S) -> Result<S::Ok, S::Error> {
        serde_util::set_strict::serialize(v, s)
    }
}
mod map_ref {
    use super::*;
    pub fn serialize<S: Serializer>(v: &&Arc<BTreeMap<Value, Value>>, s: S) -> Result<S::Ok, S::Error> {
        serde_util::map_pairs::serialize(v, s)
    }
}
impl<'a> From<&'a Value> for ValueRef<'a> {
    fn from(v: &'a Value) -> Self {
        match v {
            Value::Unit => Self::Unit,
            Value::Bool(v) => Self::Bool(v),
            Value::Int(v) => Self::Int(v),
            Value::F64(v) => Self::F64(v.to_bits()),
            Value::Str(v) => Self::Str(v),
            Value::Bytes(v) => Self::Bytes(v),
            Value::Duration(v) => Self::Duration(v),
            Value::Instant(v) => Self::Instant(v),
            Value::Mod(v) => Self::Mod(v),
            Value::Blob(v) => Self::Blob(v),
            Value::Session(v) => Self::Session(v),
            Value::Principal(v) => Self::Principal(v),
            Value::Node(v) => Self::Node(v),
            Value::Tuple(v) => Self::Tuple(v),
            Value::Struct(v) => Self::Struct(v),
            Value::Enum { variant, fields } => Self::Enum { variant, fields },
            Value::UnknownVariant {
                variant,
                wire_number,
                payload,
            } => Self::UnknownVariant {
                variant,
                wire_number,
                payload,
            },
            Value::Vec(v) => Self::Vec(v),
            Value::Set(v) => Self::Set(v),
            Value::Map(v) => Self::Map(v),
            Value::Option(v) => Self::Option(v),
            Value::Lattice(v) => Self::Lattice(v),
            Value::Group(v) => Self::Group(v),
            Value::Extern { codec, bytes } => Self::Extern { codec, bytes },
        }
    }
}
impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let _depth = DepthGuard::enter(|| serde::ser::Error::custom("value nesting exceeds 128"))?;
        ValueRef::from(self).serialize(s)
    }
}
#[derive(Serialize)]
enum LatRef<'a> {
    Bottom,
    Top,
    Bool(&'a bool),
    Elem(&'a Arc<Value>),
    Set(#[serde(with = "set_ref")] &'a Arc<BTreeSet<Value>>),
    Map(#[serde(with = "lat_map_ref")] &'a Arc<BTreeMap<Value, LatValue>>),
    Bag(#[serde(with = "bag_ref")] &'a Arc<BTreeMap<Value, u64>>),
    Seq(&'a Arc<[LatValue]>),
    Extern {
        codec: &'a ExternCodecId,
        bytes: &'a Arc<[u8]>,
    },
}
mod lat_map_ref {
    use super::*;
    pub fn serialize<S: Serializer>(v: &&Arc<BTreeMap<Value, LatValue>>, s: S) -> Result<S::Ok, S::Error> {
        serde_util::map_pairs::serialize(v, s)
    }
}
mod bag_ref {
    use super::*;
    pub fn serialize<S: Serializer>(v: &&Arc<BTreeMap<Value, u64>>, s: S) -> Result<S::Ok, S::Error> {
        serde_util::map_pairs::serialize(v, s)
    }
}
impl<'a> From<&'a LatValue> for LatRef<'a> {
    fn from(v: &'a LatValue) -> Self {
        match v {
            LatValue::Bottom => Self::Bottom,
            LatValue::Top => Self::Top,
            LatValue::Bool(v) => Self::Bool(v),
            LatValue::Elem(v) => Self::Elem(v),
            LatValue::Set(v) => Self::Set(v),
            LatValue::Map(v) => Self::Map(v),
            LatValue::Bag(v) => Self::Bag(v),
            LatValue::Seq(v) => Self::Seq(v),
            LatValue::Extern { codec, bytes } => Self::Extern { codec, bytes },
        }
    }
}
impl Serialize for LatValue {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let _depth = DepthGuard::enter(|| serde::ser::Error::custom("value nesting exceeds 128"))?;
        LatRef::from(self).serialize(s)
    }
}
#[derive(Serialize)]
enum GroupRef<'a> {
    Z(&'a i64),
    Zn(&'a u64),
    ZSet(#[serde(with = "zset_ref")] &'a Arc<BTreeMap<Value, i64>>),
    Tuple(&'a Arc<[GroupValue]>),
    Map(#[serde(with = "group_map_ref")] &'a Arc<BTreeMap<Value, GroupValue>>),
    User(&'a Arc<Value>),
}
mod zset_ref {
    use super::*;
    pub fn serialize<S: Serializer>(v: &&Arc<BTreeMap<Value, i64>>, s: S) -> Result<S::Ok, S::Error> {
        serde_util::map_pairs::serialize(v, s)
    }
}
mod group_map_ref {
    use super::*;
    pub fn serialize<S: Serializer>(v: &&Arc<BTreeMap<Value, GroupValue>>, s: S) -> Result<S::Ok, S::Error> {
        serde_util::map_pairs::serialize(v, s)
    }
}
impl<'a> From<&'a GroupValue> for GroupRef<'a> {
    fn from(v: &'a GroupValue) -> Self {
        match v {
            GroupValue::Z(v) => Self::Z(v),
            GroupValue::Zn(v) => Self::Zn(v),
            GroupValue::ZSet(v) => Self::ZSet(v),
            GroupValue::Tuple(v) => Self::Tuple(v),
            GroupValue::Map(v) => Self::Map(v),
            GroupValue::User(v) => Self::User(v),
        }
    }
}
impl Serialize for GroupValue {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let _depth = DepthGuard::enter(|| serde::ser::Error::custom("value nesting exceeds 128"))?;
        GroupRef::from(self).serialize(s)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recursive_serialize_is_bounded() {
        let mut v = Value::Unit;
        for _ in 0..200 {
            v = Value::some(v);
        }
        assert!(postcard::to_allocvec(&v).is_err());
        let mut l = LatValue::Bottom;
        for _ in 0..200 {
            l = LatValue::Seq(vec![l].into());
        }
        assert!(postcard::to_allocvec(&l).is_err());
    }
}
