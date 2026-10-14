use super::*;
use blossom_base::RelId;
use std::sync::atomic::{AtomicUsize, Ordering};
struct Cold {
    rows: Vec<Row>,
    version: Option<u64>,
    full: AtomicUsize,
    materialized: AtomicUsize,
}
impl Cold {
    fn new(n: usize) -> Arc<Self> {
        Arc::new(Self {
            rows: (0..n)
                .map(|i| Arc::from([Value::u64(0), Value::u64(i as u64)]))
                .collect(),
            version: Some(0),
            full: AtomicUsize::new(0),
            materialized: AtomicUsize::new(0),
        })
    }
}
impl ColdTables for Cold {
    fn version(&self) -> Result<Option<u64>, EvalError> {
        Ok(self.version)
    }
    fn tables(&self) -> Vec<RelId> {
        vec![RelId::from_raw(0)]
    }
    fn contains(&self, _: RelId, row: &Row, _: u64) -> Result<bool, EvalError> {
        Ok(self.rows.contains(row))
    }
    fn open_views(&self, _: &[(RelId, [u8; 32])], _: bool) -> Result<bool, EvalError> {
        Ok(true)
    }
    fn count(&self, _: RelId, _: u64) -> Result<usize, EvalError> {
        Ok(self.rows.len())
    }
    fn probe(
        &self,
        _: RelId,
        cols: &[usize],
        values: &[Value],
        _: Option<ColRange<'_>>,
        _: u64,
    ) -> Result<Vec<Row>, EvalError> {
        self.full.fetch_add(1, Ordering::Relaxed);
        Ok(self.rows.iter().filter(|r| holds(r, cols, values)).cloned().collect())
    }
    fn probe_some(
        &self,
        _: RelId,
        cols: &[usize],
        values: &[Value],
        _: u64,
        n: usize,
    ) -> Result<(Vec<Row>, bool), EvalError> {
        let mut iter = self.rows.iter().filter(|r| holds(r, cols, values));
        let rows: Vec<Row> = iter.by_ref().take(n).cloned().collect();
        self.materialized.fetch_add(rows.len(), Ordering::Relaxed);
        Ok((rows, iter.next().is_some()))
    }
    fn probe_at_most(
        &self,
        _: RelId,
        cols: &[usize],
        values: &[Value],
        _: u64,
        max: usize,
    ) -> Result<Option<Vec<Row>>, EvalError> {
        let rows: Vec<_> = self
            .rows
            .iter()
            .filter(|r| holds(r, cols, values))
            .take(max + 1)
            .cloned()
            .collect();
        Ok((rows.len() <= max).then_some(rows))
    }
}
fn tiered(cold: Arc<Cold>) -> Tiered {
    Tiered::new(RelId::from_raw(0), cold, vec![], false, 65536, false, false).unwrap()
}
#[test]
fn first_existence_miss_reads_one_witness_without_corrupting_complete_probes() {
    let cold = Cold::new(1024);
    let t = tiered(cold.clone());
    let cols = [0];
    let values = [Value::u64(0)];
    assert!(t.any_carried(&cols, &values, 0, |_| true).unwrap());
    assert!(t.any_carried(&cols, &values, 0, |_| true).unwrap());
    assert_eq!(cold.full.load(Ordering::Relaxed), 0);
    assert_eq!(cold.materialized.load(Ordering::Relaxed), 1);
    assert!(t.hot.borrow_mut().probe(&cols, &values).is_none());
    assert_eq!(t.cold_rows(&cols, &values, None, &|_| true).unwrap().len(), 1024);
}
#[test]
fn existence_respects_overlay_removals_old_exclusions_and_reinsertions() {
    let cold = Cold::new(3);
    let mut t = tiered(cold.clone());
    let cols = [0];
    let values = [Value::u64(0)];
    assert!(t.any_carried(&cols, &values, 0, |_| true).unwrap());
    t.set_overlay(&cold.rows[0], 0, 1);
    assert!(t.any_carried(&cols, &values, 1, |r| r[1] != Value::u64(1)).unwrap());
    for row in &cold.rows {
        t.set_overlay(row, 0, 1);
    }
    assert!(!t.any_carried(&cols, &values, 0, |_| true).unwrap());
    t.set_overlay(&cold.rows[2], 1, 2);
    assert!(t.any_carried(&cols, &values, 0, |_| true).unwrap());
    assert!(!t.any_carried(&cols, &values, 1, |r| r[1] != Value::u64(2)).unwrap());
}
#[test]
fn a_witness_caught_up_as_deleted_is_discarded_not_cached_as_absence() {
    let cold = Cold::new(4);
    let t = tiered(cold.clone());
    assert!(t.any_carried(&[0], &[Value::u64(0)], 0, |_| true).unwrap());
    let mut hot = t.hot.borrow_mut();
    let before = (hot.rows, hot.entries);
    hot.caught_up(&cold.rows[1], 1);
    assert_eq!((hot.rows, hot.entries), before);
    hot.caught_up(&cold.rows[0], 0);
    assert!(hot.witness(&[0], &[Value::u64(0)]).is_none());
    assert!(hot.probe(&[0], &[Value::u64(0)]).is_none());
    assert_eq!((hot.rows, hot.entries), (0, 0));
}
#[test]
fn overlays_are_visible_before_the_database_has_a_version() {
    let cold = Arc::new(Cold {
        rows: vec![],
        version: None,
        full: AtomicUsize::new(0),
        materialized: AtomicUsize::new(0),
    });
    let mut t = tiered(cold);
    let row = Arc::from([Value::u64(0), Value::u64(7)]);
    t.set_overlay(&row, 1, 1);
    assert!(t.any_carried(&[0], &[Value::u64(0)], 0, |_| true).unwrap());
    assert!(!t.any_carried(&[0], &[Value::u64(0)], 1, |_| false).unwrap());
}
#[test]
fn exclusion_search_keeps_a_witness_instead_of_an_oversized_answer() {
    let cold = Cold::new(1024);
    let t = tiered(cold);
    assert!(
        t.any_carried(&[0], &[Value::u64(0)], 1023, |r| r[1] == Value::u64(1023))
            .unwrap()
    );
    let mut hot = t.hot.borrow_mut();
    assert!(hot.probe(&[0], &[Value::u64(0)]).is_none());
    assert!(hot.rows <= 1);
    assert_eq!(hot.witness(&[0], &[Value::u64(0)]).unwrap()[1], Value::u64(1023));
    hot.shrink(0);
    assert_eq!((hot.rows, hot.entries), (0, 0));
}
