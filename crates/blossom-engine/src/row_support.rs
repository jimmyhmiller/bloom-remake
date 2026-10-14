//! Support indexed by schema keys, with compact scalar words and shared byte keys.
//! Compact layouts hash selected key fields. Every distinct payload retains independent support;
//! declared FD conflicts are rejected by the ordinary key check. Unsupported layouts retain full-row hashing.

use blossom_base::det::{DetMap, Entry};
use blossom_ir::{core::Schema, tick::Row};
use blossom_value::{BytesValue, IntValue, TypeDef, TypeTable, Value};
use smallvec::SmallVec;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Debug)]
pub(crate) struct Layout(Vec<(usize, Field)>);

#[derive(Clone, Debug)]
enum Field {
    Scalar(TypeDef),
    Tuple(Vec<Field>),
    Struct(Vec<Field>),
}
impl Field {
    fn of(ty: &TypeDef, types: &TypeTable) -> Option<Self> {
        match ty {
            TypeDef::Int(_)
            | TypeDef::Bool
            | TypeDef::Bytes
            | TypeDef::Duration
            | TypeDef::Instant
            | TypeDef::Conn
            | TypeDef::Session
            | TypeDef::Unit => Some(Self::Scalar(ty.clone())),
            TypeDef::Tuple(fields) => Some(Self::Tuple(
                fields
                    .iter()
                    .map(|id| Self::of(types.get(*id)?, types))
                    .collect::<Option<_>>()?,
            )),
            TypeDef::Struct(def) => Some(Self::Struct(
                def.fields
                    .iter()
                    .map(|f| Self::of(types.get(f.ty)?, types))
                    .collect::<Option<_>>()?,
            )),
            _ => None,
        }
    }

    fn pack<'a>(&self, value: &'a Value, key: &mut KeyRef<'a>) -> Option<()> {
        match (self, value) {
            (Self::Tuple(layout), Value::Tuple(fields)) | (Self::Struct(layout), Value::Struct(fields))
                if layout.len() == fields.len() =>
            {
                for (field, value) in layout.iter().zip(fields.iter()) {
                    field.pack(value, key)?;
                }
            }
            (Self::Scalar(TypeDef::Bytes), Value::Bytes(v)) => key.bytes.push(v),
            (Self::Scalar(TypeDef::Bool), Value::Bool(v)) => key.words.push(u64::from(*v)),
            (Self::Scalar(TypeDef::Duration), Value::Duration(v)) => key.words.push(v.0 as u64),
            (Self::Scalar(TypeDef::Instant), Value::Instant(v)) => key.words.push(v.0 as u64),
            (Self::Scalar(TypeDef::Conn), Value::Conn(v)) => key.words.push(v.0),
            (Self::Scalar(TypeDef::Session), Value::Session(v)) => key.words.push(v.0),
            (Self::Scalar(TypeDef::Unit), Value::Unit) => {}
            (Self::Scalar(TypeDef::Int(ty)), Value::Int(v)) if v.ty() == *ty => match v {
                IntValue::U8(v) => key.words.push(*v as u64),
                IntValue::U16(v) => key.words.push(*v as u64),
                IntValue::U32(v) => key.words.push(*v as u64),
                IntValue::U64(v) => key.words.push(*v),
                IntValue::I8(v) => key.words.push(*v as u64),
                IntValue::I16(v) => key.words.push(*v as u64),
                IntValue::I32(v) => key.words.push(*v as u64),
                IntValue::I64(v) => key.words.push(*v as u64),
                IntValue::U128(v) => key.words.extend([*v as u64, (*v >> 64) as u64]),
                IntValue::I128(v) => key.words.extend([*v as u64, (*v >> 64) as u64]),
            },
            _ => return None,
        }
        Some(())
    }
}

impl Layout {
    pub(crate) fn of(schema: &Schema, types: &TypeTable) -> Option<Self> {
        if !schema.lattice.is_empty() {
            return None;
        }
        let mut cols = Vec::new();
        for col in &schema.key {
            let ty = types.get(schema.cols.get(col.index())?.ty)?;
            if let Some(field) = Field::of(ty, types) {
                cols.push((col.index(), field));
            }
        }
        // A physical bucket need not be unique. Complex key columns stay in the canonical row,
        // and all rows sharing the compact prefix retain independent support in the ordered bucket.
        // Avoid an unselective single bucket, or specializing an all-scalar set with no omitted payload.
        if (cols.is_empty() && !schema.key.is_empty()) || (schema.payload.is_empty() && cols.len() == schema.key.len())
        {
            return None;
        }
        Some(Self(cols))
    }

    pub(crate) fn key<'a>(&self, row: &'a Row) -> Option<KeyRef<'a>> {
        self.pack(self.0.iter().map(|(col, _)| row.get(*col)))
    }

    fn pack<'a>(&self, values: impl Iterator<Item = Option<&'a Value>>) -> Option<KeyRef<'a>> {
        let mut key = KeyRef::default();
        for ((_, ty), value) in self.0.iter().zip(values) {
            ty.pack(value?, &mut key)?;
        }
        Some(key)
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Key {
    words: SmallVec<[u64; 3]>,
    bytes: SmallVec<[BytesValue; 1]>,
}

/// Queries borrow byte allocations. Read hits and occupied updates never increment/decrement their Arcs.
#[derive(Default, Hash)]
pub(crate) struct KeyRef<'a> {
    words: SmallVec<[u64; 3]>,
    bytes: SmallVec<[&'a BytesValue; 1]>,
}
impl KeyRef<'_> {
    fn owns(&self, key: &Key) -> bool {
        self.words == key.words
            && self.bytes.len() == key.bytes.len()
            && self.bytes.iter().zip(&key.bytes).all(|(a, b)| *a == b)
    }
    pub(crate) fn owned(&self) -> Key {
        Key {
            words: self.words.clone(),
            bytes: self.bytes.iter().map(|b| (*b).clone()).collect(),
        }
    }
}

#[derive(Debug)]
pub(crate) enum Bucket {
    One(Row, i64),
    Many(BTreeMap<Row, i64>),
}

impl Bucket {
    fn get(&self, row: &Row) -> Option<i64> {
        match self {
            Self::One(r, n) => (Arc::ptr_eq(r, row) || r == row).then_some(*n),
            Self::Many(rows) => rows.get(row).copied(),
        }
    }

    /// Returns the prior/new support; an empty bucket is removed by the caller.
    fn change(&mut self, row: &Row, w: i64) -> (i64, i64) {
        match self {
            Self::One(r, n) if Arc::ptr_eq(r, row) || r == row => {
                let before = *n;
                *n += w;
                (before, *n)
            }
            Self::One(r, n) => {
                *self = Self::Many(BTreeMap::from([(r.clone(), *n), (row.clone(), w)]));
                (0, w)
            }
            Self::Many(rows) => {
                let before = rows.get(row).copied().unwrap_or(0);
                let after = before + w;
                if after == 0 {
                    rows.remove(row);
                } else {
                    rows.insert(row.clone(), after);
                }
                if rows.len() == 1
                    && let Some((r, n)) = rows.first_key_value()
                {
                    *self = Self::One(r.clone(), *n);
                }
                (before, after)
            }
        }
    }

    fn iter(&self) -> BucketIter<'_> {
        match self {
            Self::One(r, n) => BucketIter::One(Some((r, n))),
            Self::Many(rows) => BucketIter::Many(rows.iter()),
        }
    }
}

pub(crate) enum BucketIter<'a> {
    One(Option<(&'a Row, &'a i64)>),
    Many(std::collections::btree_map::Iter<'a, Row, i64>),
}
impl<'a> Iterator for BucketIter<'a> {
    type Item = (&'a Row, &'a i64);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::One(r) => r.take(),
            Self::Many(rows) => rows.next(),
        }
    }
}

#[derive(Debug)]
enum Storage {
    Generic(DetMap<Row, i64>),
    Keyed {
        layout: Layout,
        rows: DetMap<Key, Bucket>,
        len: usize,
    },
}

#[derive(Debug)]
pub(crate) struct RowSupport(Storage);
impl Default for RowSupport {
    fn default() -> Self {
        Self(Storage::Generic(DetMap::default()))
    }
}

impl RowSupport {
    pub(crate) fn with_layout(layout: Layout) -> Self {
        Self(Storage::Keyed {
            layout,
            rows: DetMap::default(),
            len: 0,
        })
    }
    pub(crate) fn emptied(&self) -> Self {
        match &self.0 {
            Storage::Generic(_) => Self::default(),
            Storage::Keyed { layout, .. } => Self::with_layout(layout.clone()),
        }
    }
    pub(crate) fn len(&self) -> usize {
        match &self.0 {
            Storage::Generic(rows) => rows.len(),
            Storage::Keyed { len, .. } => *len,
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub(crate) fn contains_key(&self, row: &Row) -> bool {
        match &self.0 {
            Storage::Generic(rows) => rows.contains_key(row),
            Storage::Keyed { layout, rows, .. } => layout
                .key(row)
                .and_then(|key| rows.get_by(&key, |k, q| q.owns(k)))
                .and_then(|b| b.get(row))
                .is_some(),
        }
    }
    pub(crate) fn has_key(&self, cols: &[usize]) -> bool {
        matches!(&self.0, Storage::Keyed {layout,..}
            if layout.0.len() == cols.len() && layout.0.iter().zip(cols).all(|((col,_), c)| col == c))
    }
    pub(crate) fn key_count(&self, cols: &[usize]) -> Option<usize> {
        if !self.has_key(cols) {
            return None;
        }
        match &self.0 {
            Storage::Keyed { rows, .. } => Some(rows.len()),
            _ => None,
        }
    }
    pub(crate) fn probe(&self, cols: &[usize], values: &[Value]) -> Option<BucketIter<'_>> {
        if !self.has_key(cols) || cols.len() != values.len() {
            return None;
        }
        let Storage::Keyed { layout, rows, .. } = &self.0 else {
            return None;
        };
        let key = layout.pack(values.iter().map(Some))?;
        Some(
            rows.get_by(&key, |k, q| q.owns(k))
                .map_or(BucketIter::One(None), Bucket::iter),
        )
    }
    pub(crate) fn change(&mut self, row: &Row, w: i64) -> Option<(i64, i64)> {
        match &mut self.0 {
            Storage::Generic(rows) => Some(match rows.entry(row.clone()) {
                Entry::Occupied(mut entry) => {
                    let before = *entry.get();
                    let after = before + w;
                    if after == 0 {
                        entry.remove();
                    } else {
                        *entry.get_mut() = after;
                    }
                    (before, after)
                }
                Entry::Vacant(entry) => {
                    entry.insert(w);
                    (0, w)
                }
            }),
            Storage::Keyed { layout, rows, len } => {
                let key = layout.key(row)?;
                let (before, after) = match rows.entry_by(&key, |k, q| q.owns(k), || key.owned()) {
                    Entry::Vacant(entry) => {
                        entry.insert(Bucket::One(row.clone(), w));
                        (0, w)
                    }
                    Entry::Occupied(mut entry) => {
                        let change = entry.get_mut().change(row, w);
                        if matches!(entry.get(), Bucket::One(_, 0)) {
                            entry.remove();
                        }
                        change
                    }
                };
                if before == 0 && after != 0 {
                    *len += 1;
                }
                if before != 0 && after == 0 {
                    *len -= 1;
                }
                Some((before, after))
            }
        }
    }
    pub(crate) fn iter(&self) -> Iter<'_> {
        match &self.0 {
            Storage::Generic(rows) => Iter::Generic(rows.iter()),
            Storage::Keyed { rows, .. } => Iter::Keyed {
                rows: rows.iter(),
                bucket: BucketIter::One(None),
            },
        }
    }
    pub(crate) fn keys(&self) -> impl Iterator<Item = &Row> {
        self.iter().map(|(r, _)| r)
    }
    pub(crate) fn drain(&mut self) -> Drain<'_> {
        match &mut self.0 {
            Storage::Generic(rows) => Drain::Generic(rows.drain()),
            Storage::Keyed { rows, len, .. } => {
                *len = 0;
                Drain::Keyed {
                    rows: rows.drain(),
                    bucket: BucketDrain::One(None),
                }
            }
        }
    }
}

pub(crate) enum Iter<'a> {
    Generic(blossom_base::det::Iter<'a, Row, i64>),
    Keyed {
        rows: blossom_base::det::Iter<'a, Key, Bucket>,
        bucket: BucketIter<'a>,
    },
}
impl<'a> Iterator for Iter<'a> {
    type Item = (&'a Row, &'a i64);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Generic(rows) => rows.next(),
            Self::Keyed { rows, bucket } => loop {
                if let Some(row) = bucket.next() {
                    return Some(row);
                }
                *bucket = rows.next()?.1.iter();
            },
        }
    }
}

pub(crate) enum Drain<'a> {
    Generic(blossom_base::det::Drain<'a, Row, i64>),
    Keyed {
        rows: blossom_base::det::Drain<'a, Key, Bucket>,
        bucket: BucketDrain,
    },
}
pub(crate) enum BucketDrain {
    One(Option<(Row, i64)>),
    Many(std::collections::btree_map::IntoIter<Row, i64>),
}
impl Iterator for BucketDrain {
    type Item = (Row, i64);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::One(r) => r.take(),
            Self::Many(rows) => rows.next(),
        }
    }
}
impl Iterator for Drain<'_> {
    type Item = (Row, i64);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Generic(rows) => rows.next(),
            Self::Keyed { rows, bucket } => loop {
                if let Some(row) = bucket.next() {
                    return Some(row);
                }
                *bucket = match rows.next()?.1 {
                    Bucket::One(r, n) => BucketDrain::One(Some((r, n))),
                    Bucket::Many(rows) => BucketDrain::Many(rows.into_iter()),
                };
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blossom_value::IntTy;

    fn row(key: u64, payload: u64) -> Row {
        Arc::from(vec![
            Value::Int(IntValue::U64(key)),
            Value::Vec(Arc::from(vec![Value::Int(IntValue::U64(payload)); 32])),
        ])
    }
    #[test]
    fn support_conflicts_demotion_and_drains_match_full_row_counts() {
        let mut support = RowSupport::with_layout(Layout(vec![(0, Field::Scalar(TypeDef::Int(IntTy::U64)))]));
        let mut oracle = BTreeMap::<Row, i64>::new();
        let mut random = 42u64;
        for step in 0..5000 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let r = row((random >> 32) % 17, (random >> 40) % 5);
            let before = oracle.get(&r).copied().unwrap_or(0);
            let w = if before > 0 && random & 4 == 0 { -1 } else { 1 };
            assert_eq!(support.change(&r, w), Some((before, before + w)));
            if before + w == 0 {
                oracle.remove(&r);
            } else {
                oracle.insert(r.clone(), before + w);
            }
            assert_eq!(support.len(), oracle.len());
            assert_eq!(support.contains_key(&r), oracle.contains_key(&r));
            assert_eq!(
                support.iter().map(|(r, n)| (r.clone(), *n)).collect::<BTreeMap<_, _>>(),
                oracle
            );
            let k = r[0].clone();
            let rows: Vec<_> = support
                .probe(&[0], std::slice::from_ref(&k))
                .unwrap()
                .map(|(r, _)| r.clone())
                .collect();
            assert_eq!(rows, oracle.keys().filter(|r| r[0] == k).cloned().collect::<Vec<_>>());
            if step % 401 == 0 {
                assert_eq!(support.drain().collect::<BTreeMap<_, _>>(), oracle);
                oracle.clear();
                assert!(support.is_empty());
            }
        }
        let mut empty = support.emptied();
        assert!(empty.is_empty());
        assert!(empty.has_key(&[0]));
        assert_eq!(empty.change(&row(1, 2), 1), Some((0, 1)));
    }
    #[test]
    fn compact_keys_preserve_all_integer_bits_and_shared_bytes() {
        let layout = Layout(vec![
            (2, Field::Scalar(TypeDef::Bytes)),
            (0, Field::Scalar(TypeDef::Int(IntTy::I128))),
            (1, Field::Scalar(TypeDef::Int(IntTy::U64))),
        ]);
        let mut support = RowSupport::with_layout(layout);
        for (i, n) in [i128::MIN, -1, 0, i128::MAX].into_iter().enumerate() {
            let r: Row = Arc::from(vec![
                Value::Int(IntValue::I128(n)),
                Value::Int(IntValue::U64(u64::MAX)),
                Value::bytes(b"topic"),
                Value::Bool(i % 2 == 0),
            ]);
            assert_eq!(support.change(&r, 1), Some((0, 1)));
            assert!(support.contains_key(&r));
            let values = [Value::bytes(b"topic"), r[0].clone(), r[1].clone()];
            assert_eq!(support.probe(&[2, 0, 1], &values).unwrap().next().unwrap().0, &r);
        }
        assert_eq!(support.len(), 4);
        assert!(support.probe(&[0, 1, 2], &[]).is_none());
        let wrong: Row = Arc::from(vec![
            Value::Int(IntValue::U128(0)),
            Value::Int(IntValue::U64(0)),
            Value::bytes(b"topic"),
        ]);
        assert_eq!(support.change(&wrong, 1), None);
    }
    #[test]
    fn empty_key_keeps_conflicting_payloads_distinct() {
        let mut support = RowSupport::with_layout(Layout(vec![]));
        assert_eq!(support.change(&row(1, 2), 1), Some((0, 1)));
        assert_eq!(support.change(&row(3, 4), 1), Some((0, 1)));
        assert_eq!(support.probe(&[], &[]).unwrap().count(), 2);
        assert_eq!(support.change(&row(1, 2), -1), Some((1, 0)));
        assert_eq!(support.change(&row(3, 4), -1), Some((1, 0)));
        assert!(support.is_empty());
    }
}

#[cfg(test)]
mod borrowed_tests {
    use super::*;
    use std::hash::BuildHasher;
    #[test]
    fn borrowed_and_owned_keys_hash_identically_and_hits_keep_byte_references_borrowed() {
        let small = BytesValue::from(b"topic".as_slice());
        let large = BytesValue::from(vec![7u8; 1024]);
        for n in [0, 3, 9] {
            let query = KeyRef {
                words: (0..n).collect(),
                bytes: SmallVec::from_slice(&[&small, &large]),
            };
            let owned = query.owned();
            for state in [blossom_base::DetState::fixed(), blossom_base::DetState::from_nonce(42)] {
                assert_eq!(state.hash_one(&query), state.hash_one(&owned));
            }
            let mut rows = DetMap::default();
            rows.insert(owned, 3u64);
            let before = Arc::strong_count(small.shared());
            assert_eq!(rows.get_by(&query, |k, q| q.owns(k)), Some(&3));
            match rows.entry_by(&query, |k, q| q.owns(k), || panic!("a hit must not build an owned key")) {
                Entry::Occupied(mut e) => *e.get_mut() += 1,
                Entry::Vacant(_) => panic!("the equivalent key vanished"),
            }
            assert_eq!(Arc::strong_count(small.shared()), before);
            assert_eq!(rows.get_by(&query, |k, q| q.owns(k)), Some(&4));
        }
    }
    #[test]
    fn composite_partition_keys_preserve_shapes_fields_and_all_payload_versions() {
        let layout = Layout(vec![(
            0,
            Field::Tuple(vec![
                Field::Scalar(TypeDef::Bytes),
                Field::Scalar(TypeDef::Int(blossom_value::IntTy::I32)),
            ]),
        )]);
        let mut support = RowSupport::with_layout(layout);
        for partition in [-1, 0, 1] {
            for payload in [0, 1] {
                let r: Row = Arc::from(vec![
                    Value::Tuple(Arc::from(vec![
                        Value::bytes(b"topic"),
                        Value::Int(IntValue::I32(partition)),
                    ])),
                    Value::Int(IntValue::U64(payload)),
                ]);
                assert_eq!(support.change(&r, 1), Some((0, 1)));
                assert!(support.contains_key(&r));
            }
        }
        let group = Value::Tuple(Arc::from(vec![Value::bytes(b"topic"), Value::Int(IntValue::I32(-1))]));
        assert_eq!(support.probe(&[0], &[group]).unwrap().count(), 2);
        let wrong: Row = Arc::from(vec![
            Value::Struct(Arc::from(vec![Value::bytes(b"topic"), Value::Int(IntValue::I32(-1))])),
            Value::Int(IntValue::U64(0)),
        ]);
        assert_eq!(support.change(&wrong, 1), None);
    }
}
