//! Equality probes need no key ordering. Promote to an ordered index only when a range asks for it.
//! Buckets retain canonical row order in either representation.
use crate::row_bucket::RowBucket;
use blossom_base::DetMap;
use blossom_ir::tick::Row;
use blossom_value::Value;
use std::collections::BTreeMap;

#[derive(Debug)]
pub(crate) enum Index {
    Equality(DetMap<Vec<Value>, RowBucket>),
    Ordered(BTreeMap<Vec<Value>, RowBucket>),
}

impl Index {
    pub(crate) fn new() -> Self {
        Self::Equality(DetMap::new())
    }
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Equality(m) => m.len(),
            Self::Ordered(m) => m.len(),
        }
    }
    pub(crate) fn clear(&mut self) {
        match self {
            Self::Equality(m) => m.clear(),
            Self::Ordered(m) => m.clear(),
        }
    }
    pub(crate) fn get(&self, key: &[Value]) -> Option<&RowBucket> {
        match self {
            Self::Equality(m) => m.get(key),
            Self::Ordered(m) => m.get(key),
        }
    }
    pub(crate) fn get_mut(&mut self, key: &[Value]) -> Option<&mut RowBucket> {
        match self {
            Self::Equality(m) => m.get_mut(key),
            Self::Ordered(m) => m.get_mut(key),
        }
    }
    pub(crate) fn remove(&mut self, key: &[Value]) {
        match self {
            Self::Equality(m) => {
                m.remove(key);
            }
            Self::Ordered(m) => {
                m.remove(key);
            }
        }
    }
    pub(crate) fn insert_row(&mut self, key: Vec<Value>, row: Row) {
        match self {
            Self::Equality(m) => {
                m.entry(key).or_default().insert(row);
            }
            Self::Ordered(m) => {
                m.entry(key).or_default().insert(row);
            }
        }
    }
    pub(crate) fn make_ordered(&mut self) {
        if let Self::Equality(m) = self {
            *self = Self::Ordered(std::mem::take(m).into_iter().collect());
        }
    }
    pub(crate) fn ordered(&self) -> Option<&BTreeMap<Vec<Value>, RowBucket>> {
        match self {
            Self::Ordered(m) => Some(m),
            Self::Equality(_) => None,
        }
    }
}
