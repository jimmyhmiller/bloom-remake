//! Bounded accumulation of signed relational changes in sorted runs.
//!
//! Consolidate small runs before merging them, so repeated valuations do not retain an unbounded
//! list of duplicate rows. Each level holds at most one run; merging equal levels bounds how often
//! a surviving row moves. Ordering and equality are always the key's semantic order.

use std::cmp::Ordering;

const RUN_ROWS: usize = 256;

type Run<K> = Vec<(K, i64)>;

pub(crate) struct ChangeBatch<K> {
    pending: Run<K>,
    levels: Vec<Option<Run<K>>>,
}

impl<K> Default for ChangeBatch<K> {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            levels: Vec::new(),
        }
    }
}

impl<K: Ord> ChangeBatch<K> {
    /// Adopts an already consolidated run produced by `into_sorted`.
    pub fn from_sorted(run: Run<K>) -> Self {
        Self {
            pending: Vec::new(),
            levels: vec![Some(run)],
        }
    }

    pub fn push(&mut self, key: K, weight: i64) {
        if weight == 0 {
            return;
        }
        self.pending.push((key, weight));
        if self.pending.len() == RUN_ROWS {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let mut run = std::mem::take(&mut self.pending);
        run.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        run.dedup_by(|a, b| {
            if a.0 == b.0 {
                b.1 += a.1;
                true
            } else {
                false
            }
        });
        run.retain(|(_, w)| *w != 0);
        self.insert_run(0, run);
    }

    fn insert_run(&mut self, mut level: usize, mut run: Run<K>) {
        while !run.is_empty() {
            if level == self.levels.len() {
                self.levels.push(Some(run));
                return;
            }
            let Some(previous) = self.levels.get_mut(level).and_then(Option::take) else {
                if let Some(slot) = self.levels.get_mut(level) {
                    *slot = Some(run);
                }
                return;
            };
            run = merge(previous, run);
            level += 1;
        }
    }

    pub fn extend(&mut self, other: Self) {
        // A level describes the amount of input represented, not the remaining distinct row count.
        // Transfer runs at their original levels instead of flattening and sorting them again.
        for (level, run) in other.levels.into_iter().enumerate() {
            if let Some(run) = run {
                while self.levels.len() < level {
                    self.levels.push(None);
                }
                self.insert_run(level, run);
            }
        }
        for (key, weight) in other.pending {
            self.push(key, weight);
        }
    }

    pub fn into_sorted(mut self) -> Run<K> {
        self.flush();
        self.levels.into_iter().flatten().fold(Vec::new(), merge)
    }
}

fn merge<K: Ord>(left: Run<K>, right: Run<K>) -> Run<K> {
    if left.is_empty() {
        return right;
    }
    if right.is_empty() {
        return left;
    }
    let mut merged = Vec::with_capacity(left.len() + right.len());
    let mut left = left.into_iter().peekable();
    let mut right = right.into_iter().peekable();
    while let (Some(a), Some(b)) = (left.peek(), right.peek()) {
        match a.0.cmp(&b.0) {
            Ordering::Less => merged.extend(left.next()),
            Ordering::Greater => merged.extend(right.next()),
            Ordering::Equal => {
                if let (Some((key, a)), Some((_, b))) = (left.next(), right.next()) {
                    let weight = a + b;
                    if weight != 0 {
                        merged.push((key, weight));
                    }
                }
            }
        }
    }
    merged.extend(left);
    merged.extend(right);
    merged
}

impl<K: Ord> FromIterator<(K, i64)> for ChangeBatch<K> {
    fn from_iter<T: IntoIterator<Item = (K, i64)>>(iter: T) -> Self {
        let mut batch = Self::default();
        for (key, weight) in iter {
            batch.push(key, weight);
        }
        batch
    }
}

impl<K: Ord> IntoIterator for ChangeBatch<K> {
    type Item = (K, i64);
    type IntoIter = std::vec::IntoIter<Self::Item>;
    fn into_iter(self) -> Self::IntoIter {
        self.into_sorted().into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn merged_chunks_match_counted_reference() {
        for chunk_size in [1, 17, 255, 256, 257, 4096] {
            let mut reference = BTreeMap::new();
            let mut batch = ChangeBatch::default();
            let mut chunk = ChangeBatch::default();
            let mut random = 42u64;
            for i in 0..30_000 {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                let key = (random >> 32) % 1021;
                let weight = if random & 4 == 0 { -1 } else { 1 };
                *reference.entry(key).or_insert(0) += weight;
                chunk.push(key, weight);
                if i % chunk_size == 0 {
                    batch.extend(std::mem::take(&mut chunk));
                }
            }
            batch.extend(chunk);
            reference.retain(|_, w| *w != 0);
            assert_eq!(batch.into_sorted(), reference.into_iter().collect::<Vec<_>>());
        }
    }

    #[test]
    fn duplicates_are_bounded_and_opposite_runs_cancel() {
        let mut a = ChangeBatch::default();
        let mut b = ChangeBatch::default();
        for _ in 0..100_000 {
            a.push(7, 1);
            b.push(7, -1);
        }
        let retained = a.pending.len() + a.levels.iter().flatten().map(Vec::len).sum::<usize>();
        assert!(retained < RUN_ROWS + 20);
        a.extend(b);
        assert!(a.into_sorted().is_empty());
    }
}
