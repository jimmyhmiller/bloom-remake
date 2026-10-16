//! Bounded per-primary-input output bags for pure differential maps and joins.
//! Eviction only changes work: a missing retraction is evaluated normally.
use crate::row_support::{Key, KeyRef, Layout};
use blossom_base::{DetMap, RuleId};
use blossom_ir::tick::Row;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    sync::Arc,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MapCacheReport {
    pub row_budget: usize,
    pub retained_rows: usize,
    pub peak_retained_rows: usize,
    pub retraction_hits: u64,
    pub retraction_misses: u64,
    pub evictions: u64,
}

#[derive(Clone)]
struct CacheKey {
    rule: RuleId,
    input: Row,
    packed: Option<Key>,
}
struct CacheQuery<'a> {
    rule: RuleId,
    input: &'a Row,
    packed: Option<KeyRef<'a>>,
}
impl PartialEq for CacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.rule == other.rule && (Arc::ptr_eq(&self.input, &other.input) || self.input == other.input)
    }
}
impl Eq for CacheKey {}
impl Hash for CacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.rule.hash(state);
        self.packed.is_some().hash(state);
        match &self.packed {
            Some(k) => k.hash(state),
            None => self.input.hash(state),
        }
    }
}
impl Hash for CacheQuery<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.rule.hash(state);
        self.packed.is_some().hash(state);
        match &self.packed {
            Some(k) => k.hash(state),
            None => self.input.hash(state),
        }
    }
}
impl CacheQuery<'_> {
    fn owns(&self, key: &CacheKey) -> bool {
        self.rule == key.rule && (Arc::ptr_eq(self.input, &key.input) || self.input == &key.input)
    }
    fn owned(&self) -> CacheKey {
        CacheKey {
            rule: self.rule,
            input: self.input.clone(),
            packed: self.packed.as_ref().map(KeyRef::owned),
        }
    }
}

struct Entry {
    order: u64,
    outputs: BTreeMap<Row, i64>,
}
pub(crate) struct MapCache {
    entries: DetMap<CacheKey, Entry>,
    layouts: BTreeMap<RuleId, Layout>,
    order: BTreeMap<u64, CacheKey>,
    serial: u64,
    stats: MapCacheReport,
}
impl MapCache {
    pub fn new(budget: usize) -> Self {
        Self {
            entries: DetMap::default(),
            layouts: BTreeMap::new(),
            order: BTreeMap::new(),
            serial: 0,
            stats: MapCacheReport {
                row_budget: budget,
                ..Default::default()
            },
        }
    }
    pub fn with_layouts(mut self, layouts: BTreeMap<RuleId, Layout>) -> Self {
        self.layouts = layouts;
        self
    }
    pub fn report(&self) -> MapCacheReport {
        self.stats.clone()
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.serial = 0;
        self.stats.retained_rows = 0;
    }
    fn remove(&mut self, key: &CacheKey) -> Option<BTreeMap<Row, i64>> {
        let entry = self.entries.remove(key)?;
        self.order.remove(&entry.order);
        self.stats.retained_rows -= 1 + entry.outputs.len();
        Some(entry.outputs)
    }
    pub fn retract(&mut self, rule: RuleId, row: &Row) -> Option<BTreeMap<Row, i64>> {
        let query = CacheQuery {
            rule,
            input: row,
            packed: self.layouts.get(&rule).and_then(|layout| layout.key(row)),
        };
        let result = self.entries.remove_by(&query, |key, q| q.owns(key)).map(|entry| {
            self.order.remove(&entry.order);
            self.stats.retained_rows -= 1 + entry.outputs.len();
            entry.outputs
        });
        if result.is_some() {
            self.stats.retraction_hits += 1;
        } else {
            self.stats.retraction_misses += 1;
        }
        result
    }
    /// Incorporate a non-primary dependency's signed valuation into a retained primary input's output bag.
    /// Absent/evicted inputs stay absent: a partial delta cannot reconstruct their complete output.
    pub fn update(&mut self, rule: RuleId, input: &Row, output: &Row, sign: i64) {
        let query = CacheQuery {
            rule,
            input,
            packed: self.layouts.get(&rule).and_then(|layout| layout.key(input)),
        };
        let Some(entry) = self.entries.get_mut_by(&query, |key, q| q.owns(key)) else {
            return;
        };
        let before = entry.outputs.len();
        let count = entry.outputs.entry(output.clone()).or_insert(0);
        *count += sign;
        if *count == 0 {
            entry.outputs.remove(output);
        }
        let after = entry.outputs.len();
        self.stats.retained_rows = self.stats.retained_rows - before + after;
        while self.stats.retained_rows > self.stats.row_budget {
            let Some((_, oldest)) = self.order.first_key_value() else {
                break;
            };
            let oldest = oldest.clone();
            self.remove(&oldest);
            self.stats.evictions += 1;
        }
        self.stats.peak_retained_rows = self.stats.peak_retained_rows.max(self.stats.retained_rows);
    }
    pub fn insert(&mut self, rule: RuleId, row: Row, outputs: Vec<(Row, i64)>) {
        let outputs: BTreeMap<Row, i64> = outputs.into_iter().filter(|(_, n)| *n != 0).collect();
        let weight = 1 + outputs.len();
        if weight > self.stats.row_budget {
            return;
        }
        let key = CacheQuery {
            rule,
            input: &row,
            packed: self.layouts.get(&rule).and_then(|layout| layout.key(&row)),
        }
        .owned();
        self.remove(&key);
        while self.stats.retained_rows > self.stats.row_budget - weight {
            let Some((_, oldest)) = self.order.first_key_value() else {
                break;
            };
            let oldest = oldest.clone();
            self.remove(&oldest);
            self.stats.evictions += 1;
        }
        // Overflow is an optimization reset, never a semantic failure.
        let Some(serial) = self.serial.checked_add(1) else {
            self.clear();
            return;
        };
        self.serial = serial;
        self.order.insert(serial, key.clone());
        self.entries.insert(key, Entry { order: serial, outputs });
        self.stats.retained_rows += weight;
        self.stats.peak_retained_rows = self.stats.peak_retained_rows.max(self.stats.retained_rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blossom_value::Value;
    use std::sync::Arc;
    fn row(n: u64) -> Row {
        Arc::from([Value::u64(n)])
    }
    #[test]
    fn global_budget_eviction_retraction_and_empty_outputs() {
        let mut c = MapCache::new(3);
        let rule = RuleId::from_raw(1);
        c.insert(rule, row(1), vec![(row(2), 2)]);
        c.insert(rule, row(3), vec![]);
        assert_eq!(c.report().retained_rows, 3);
        c.insert(rule, row(4), vec![(row(5), 1)]);
        assert!(c.retract(rule, &row(1)).is_none());
        assert_eq!(
            c.retract(rule, &row(4)).unwrap().into_iter().collect::<Vec<_>>(),
            vec![(row(5), 1)]
        );
        assert!(c.retract(rule, &row(3)).unwrap().is_empty());
        assert_eq!(c.report().retained_rows, 0);
        assert_eq!(c.report().evictions, 1);
        c.insert(rule, row(6), vec![(row(7), 1), (row(8), 1), (row(9), 1)]);
        assert_eq!(c.report().retained_rows, 0); // Oversized expansion never retained.
        c.insert(rule, row(6), vec![]);
        c.clear();
        assert!(c.retract(rule, &row(6)).is_none());
    }
}
