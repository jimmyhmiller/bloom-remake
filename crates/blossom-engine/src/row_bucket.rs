//! Ordered index buckets without a tree allocation for the common single-row case.
//! A second distinct row promotes the bucket to a tree; removing all but one demotes it again.
//! No schema uniqueness assumption is made: every distinct row remains present and canonically ordered.

use blossom_ir::tick::Row;
use std::collections::{BTreeSet, btree_set};

#[derive(Debug, Default)]
pub(crate) enum RowBucket {
    #[default]
    Empty,
    One(Row),
    Many(BTreeSet<Row>),
}

impl RowBucket {
    pub(crate) fn insert(&mut self, row: Row) -> bool {
        match self {
            Self::Empty => {
                *self = Self::One(row);
                true
            }
            Self::One(current) if *current == row => false,
            Self::One(current) => {
                *self = Self::Many(BTreeSet::from([current.clone(), row]));
                true
            }
            Self::Many(rows) => rows.insert(row),
        }
    }

    pub(crate) fn remove(&mut self, row: &Row) -> bool {
        match self {
            Self::Empty => false,
            Self::One(current) if current == row => {
                *self = Self::Empty;
                true
            }
            Self::One(_) => false,
            Self::Many(rows) => {
                let removed = rows.remove(row);
                if rows.len() == 1
                    && let Some(last) = rows.first().cloned()
                {
                    *self = Self::One(last);
                }
                removed
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    pub(crate) fn iter(&self) -> Iter<'_> {
        match self {
            Self::Empty => Iter::One(None),
            Self::One(row) => Iter::One(Some(row)),
            Self::Many(rows) => Iter::Many(rows.iter()),
        }
    }
}

pub(crate) enum Iter<'a> {
    One(Option<&'a Row>),
    Many(btree_set::Iter<'a, Row>),
}

impl<'a> Iterator for Iter<'a> {
    type Item = &'a Row;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::One(row) => row.take(),
            Self::Many(rows) => rows.next(),
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::One(row) => {
                let n = usize::from(row.is_some());
                (n, Some(n))
            }
            Self::Many(rows) => rows.size_hint(),
        }
    }
}

impl ExactSizeIterator for Iter<'_> {}

impl<'a> IntoIterator for &'a RowBucket {
    type Item = &'a Row;
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blossom_value::{IntValue, Value};
    use std::sync::Arc;

    fn row(n: u64) -> Row {
        Arc::from(vec![Value::Int(IntValue::U64(n)), Value::bytes(&vec![n as u8; 1024])])
    }

    #[test]
    fn mixed_updates_match_an_ordered_set_including_equal_distinct_allocations() {
        let mut bucket = RowBucket::default();
        let mut reference = BTreeSet::new();
        let mut random = 42u64;
        for _ in 0..10_000 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let r = row((random >> 32) % 31);
            if random & 4 == 0 {
                assert_eq!(bucket.insert(r.clone()), reference.insert(r));
            } else {
                assert_eq!(bucket.remove(&r), reference.remove(&r));
            }
            assert_eq!(bucket.is_empty(), reference.is_empty());
            assert_eq!(bucket.iter().len(), reference.len());
            assert!(bucket.iter().eq(reference.iter()));
        }
    }

    #[test]
    fn promotion_demotion_and_last_row_removal_preserve_all_rows() {
        let mut bucket = RowBucket::default();
        let first = row(9);
        let weak = Arc::downgrade(&first);
        assert!(bucket.insert(first));
        assert!(matches!(bucket, RowBucket::One(_)));
        assert!(!bucket.insert(row(9)), "independent equal allocations are one row");
        assert!(bucket.insert(row(1)));
        assert!(matches!(bucket, RowBucket::Many(_)));
        assert!(!bucket.remove(&row(5)));
        assert!(bucket.remove(&row(9)));
        assert!(weak.upgrade().is_none(), "removed payload is released on demotion");
        assert!(matches!(bucket, RowBucket::One(_)));
        assert_eq!(bucket.iter().next(), Some(&row(1)));
        assert!(bucket.remove(&row(1)));
        assert!(bucket.is_empty());
        assert!(bucket.insert(row(3)));
        assert!(bucket.iter().eq([row(3)].iter()));
    }
}
