//! Schema-compiled equality arrangements. Packed keys preserve exact equality, not canonical order.
//! A range request promotes once to the ordinary value-ordered index using the retained source rows.
use crate::{
    row_bucket::RowBucket,
    row_support::{Key, Layout},
};
use blossom_ir::tick::Row;
use blossom_value::Value;
use std::collections::BTreeMap;

#[derive(Debug)]
pub(crate) enum Index {
    Packed {
        layout: Layout,
        cols: Vec<usize>,
        rows: BTreeMap<Key, RowBucket>,
    },
    Ordered(BTreeMap<Vec<Value>, RowBucket>),
}
impl Index {
    pub(crate) fn new(cols: &[usize], layout: Option<Layout>) -> Self {
        match layout {
            Some(layout) => Self::Packed {
                layout,
                cols: cols.to_vec(),
                rows: BTreeMap::new(),
            },
            None => Self::Ordered(BTreeMap::new()),
        }
    }
    pub(crate) fn is_packed(&self) -> bool {
        matches!(self, Self::Packed { .. })
    }
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Packed { rows, .. } => rows.len(),
            Self::Ordered(rows) => rows.len(),
        }
    }
    pub(crate) fn clear(&mut self) {
        match self {
            Self::Packed { rows, .. } => rows.clear(),
            Self::Ordered(rows) => rows.clear(),
        }
    }
    pub(crate) fn get(&self, values: &[Value]) -> Option<&RowBucket> {
        match self {
            Self::Packed { layout, rows, .. } => rows.get(&layout.probe_key(values)?.owned()),
            Self::Ordered(rows) => rows.get(values),
        }
    }
    pub(crate) fn get_mut(&mut self, values: &[Value]) -> Option<&mut RowBucket> {
        match self {
            Self::Packed { layout, rows, .. } => rows.get_mut(&layout.probe_key(values)?.owned()),
            Self::Ordered(rows) => rows.get_mut(values),
        }
    }
    pub(crate) fn remove(&mut self, values: &[Value]) {
        match self {
            Self::Packed { layout, rows, .. } => {
                if let Some(key) = layout.probe_key(values) {
                    rows.remove(&key.owned());
                }
            }
            Self::Ordered(rows) => {
                rows.remove(values);
            }
        }
    }
    pub(crate) fn insert_row(&mut self, cols: &[usize], row: Row) {
        if let Self::Packed { layout, rows, .. } = self
            && let Some(key) = layout.key(&row).map(|key| key.owned())
        {
            rows.entry(key).or_default().insert(row);
            return;
        }
        let values = cols.iter().filter_map(|c| row.get(*c).cloned()).collect();
        self.insert(values, row);
    }
    pub(crate) fn insert(&mut self, values: Vec<Value>, row: Row) {
        match self {
            Self::Packed { layout, rows, .. } => {
                if let Some(key) = layout.probe_key(&values) {
                    rows.entry(key.owned()).or_default().insert(row);
                    return;
                }
                // Preserve exact data even if an internal caller supplies a value outside the schema.
                self.make_ordered();
                self.insert(values, row);
            }
            Self::Ordered(rows) => {
                rows.entry(values).or_default().insert(row);
            }
        }
    }
    pub(crate) fn make_ordered(&mut self) {
        if let Self::Packed { cols, rows, .. } = self {
            let mut ordered = BTreeMap::<Vec<Value>, RowBucket>::new();
            for bucket in std::mem::take(rows).into_values() {
                for row in bucket.iter() {
                    let values = cols.iter().filter_map(|c| row.get(*c).cloned()).collect();
                    ordered.entry(values).or_default().insert(row.clone());
                }
            }
            *self = Self::Ordered(ordered);
        }
    }
    pub(crate) fn ordered(&self) -> Option<&BTreeMap<Vec<Value>, RowBucket>> {
        match self {
            Self::Ordered(rows) => Some(rows),
            _ => None,
        }
    }
}
