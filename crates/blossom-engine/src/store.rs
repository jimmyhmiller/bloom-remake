//! Relation storage that lives across ticks (ARCHITECTURE §4.2, value-level).
//!
//! A store holds one relation's rows with a count per row: how many sources support it (the carried state, the tick's
//! input, the program's facts, and every derivation of every rule). A row is present while its count is positive, so
//! deletions are exact (DBSP's counting). A lattice-valued relation (SEM-100) holds one row per cell: the join of the
//! cell's live contributions, recomputed when they change (a join has no inverse).
//!
//! A store records the tick's change to its present rows (`ins`, `del`), which drives the rules reading it, and can
//! read the relation as it was at the start of the tick (`old`): the present rows, minus those inserted this tick, plus
//! those deleted. Indexes on column sets are maintained with every change.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_base::internal_error;
use blossom_ir::tick::{EvalError, Row};
use blossom_lattice::Kind;
use blossom_value::Value;

use crate::expr::{ExprError, ExprResult, bug};

/// How a lattice-valued relation's rows merge.
#[derive(Clone, Debug)]
pub(crate) struct CellSpec {
    /// The columns identifying a cell (key and payload), plus `extra` trailing columns (a received tuple's sender).
    pub ident: Vec<usize>,
    pub extra: usize,
    /// The lattice columns.
    pub lattice: Vec<(usize, Kind)>,
}

impl CellSpec {
    fn ident_of(&self, row: &[Value]) -> Vec<Value> {
        let n = row.len();
        self.ident
            .iter()
            .copied()
            .chain(n.saturating_sub(self.extra)..n)
            .filter_map(|c| row.get(c).cloned())
            .collect()
    }

    fn is_bottom(&self, row: &[Value]) -> ExprResult<bool> {
        for (c, k) in &self.lattice {
            match row.get(*c) {
                Some(Value::Lattice(l)) => {
                    if !k.is_bottom(l) {
                        return Ok(false);
                    }
                }
                other => return Err(bug(format!("lattice column {c} holds {other:?}"))),
            }
        }
        Ok(true)
    }

    /// The join of a cell's live contributions (at least one).
    fn join<'a>(&self, mut rows: impl Iterator<Item = &'a Row>) -> ExprResult<Row> {
        let first = rows.next().ok_or_else(|| bug("joining an empty cell".into()))?;
        let mut out = first.to_vec();
        for r in rows {
            for (c, k) in &self.lattice {
                match (out.get(*c), r.get(*c)) {
                    (Some(Value::Lattice(a)), Some(Value::Lattice(b))) => {
                        let j = k.join(a, b).map_err(ExprError::from)?;
                        if let Some(slot) = out.get_mut(*c) {
                            *slot = Value::Lattice(j);
                        }
                    }
                    (a, b) => return Err(bug(format!("merging lattice column {c}: {a:?} and {b:?}"))),
                }
            }
        }
        Ok(Arc::from(out))
    }
}

type Index = BTreeMap<Vec<Value>, BTreeSet<Row>>;

/// One relation's rows.
#[derive(Debug, Default)]
pub(crate) struct Store {
    pub cell: Option<CellSpec>,
    /// Set relation: the support of each row. Lattice relation: the support of each contribution.
    counts: BTreeMap<Row, i64>,
    /// Lattice relation: each cell's live contributions, and its merged row.
    contributions: BTreeMap<Vec<Value>, BTreeSet<Row>>,
    merged: BTreeMap<Vec<Value>, Row>,
    present: BTreeSet<Row>,
    indexes: BTreeMap<Vec<usize>, Index>,
    /// The tick's change to the present rows.
    pub ins: BTreeSet<Row>,
    pub del: BTreeSet<Row>,
}

fn key(row: &[Value], cols: &[usize]) -> Vec<Value> {
    cols.iter().filter_map(|c| row.get(*c).cloned()).collect()
}

impl Store {
    pub fn new(cell: Option<CellSpec>) -> Store {
        Store {
            cell,
            ..Store::default()
        }
    }

    pub fn present(&self) -> &BTreeSet<Row> {
        &self.present
    }

    pub fn changed(&self) -> bool {
        !self.ins.is_empty() || !self.del.is_empty()
    }

    pub fn clear_delta(&mut self) {
        self.ins.clear();
        self.del.clear();
    }

    /// Adds `w` (non-zero) to the support of `row`.
    pub fn add(&mut self, row: Row, w: i64) -> ExprResult<()> {
        if w == 0 {
            return Ok(());
        }
        let Some(spec) = self.cell.clone() else {
            let count = self.counts.entry(row.clone()).or_insert(0);
            let before = *count;
            *count += w;
            let after = *count;
            if after == 0 {
                self.counts.remove(&row);
            }
            if after < 0 {
                return Err(bug(format!("the support of {row:?} went negative")));
            }
            match (before > 0, after > 0) {
                (false, true) => self.show(row),
                (true, false) => self.hide(&row),
                _ => {}
            }
            return Ok(());
        };
        // A contribution whose lattice values are all ⊥ is no contribution (SEM-101).
        if spec.is_bottom(&row)? {
            return Ok(());
        }
        let count = self.counts.entry(row.clone()).or_insert(0);
        let before = *count;
        *count += w;
        let after = *count;
        if after == 0 {
            self.counts.remove(&row);
        }
        if after < 0 {
            return Err(bug(format!("the support of {row:?} went negative")));
        }
        if (before > 0) == (after > 0) {
            return Ok(());
        }
        let id = spec.ident_of(&row);
        let live = self.contributions.entry(id.clone()).or_default();
        if after > 0 {
            live.insert(row);
        } else {
            live.remove(&row);
        }
        let new = if live.is_empty() {
            self.contributions.remove(&id);
            None
        } else {
            Some(spec.join(live.iter())?)
        };
        let old = self.merged.get(&id).cloned();
        if old == new {
            return Ok(());
        }
        if let Some(o) = old {
            self.hide(&o);
            self.merged.remove(&id);
        }
        if let Some(n) = new {
            self.merged.insert(id, n.clone());
            self.show(n);
        }
        Ok(())
    }

    fn show(&mut self, row: Row) {
        for (cols, index) in &mut self.indexes {
            index.entry(key(&row, cols)).or_default().insert(row.clone());
        }
        if !self.del.remove(&row) {
            self.ins.insert(row.clone());
        }
        self.present.insert(row);
    }

    fn hide(&mut self, row: &Row) {
        for (cols, index) in &mut self.indexes {
            let k = key(row, cols);
            if let Some(bucket) = index.get_mut(&k) {
                bucket.remove(row);
                if bucket.is_empty() {
                    index.remove(&k);
                }
            }
        }
        if !self.ins.remove(row) {
            self.del.insert(row.clone());
        }
        self.present.remove(row);
    }

    pub fn ensure_index(&mut self, cols: &[usize]) {
        if cols.is_empty() || self.indexes.contains_key(cols) {
            return;
        }
        let mut index = Index::new();
        for row in &self.present {
            index.entry(key(row, cols)).or_default().insert(row.clone());
        }
        self.indexes.insert(cols.to_vec(), index);
    }

    /// The present rows whose columns `cols` hold `values`.
    pub fn new_rows<'a>(&'a self, cols: &[usize], values: &[Value]) -> Result<Vec<&'a Row>, EvalError> {
        if cols.is_empty() {
            return Ok(self.present.iter().collect());
        }
        let index = self
            .indexes
            .get(cols)
            .ok_or_else(|| internal_error!("a probe on columns {cols:?} without an index"))?;
        Ok(index.get(values).map(|b| b.iter().collect()).unwrap_or_default())
    }

    /// The rows that were present at the start of the tick with columns `cols` holding `values`.
    pub fn old_rows(&self, cols: &[usize], values: &[Value]) -> Result<Vec<Row>, EvalError> {
        let mut out: Vec<Row> = self
            .new_rows(cols, values)?
            .into_iter()
            .filter(|r| !self.ins.contains(*r))
            .cloned()
            .collect();
        out.extend(self.del.iter().filter(|r| key(r, cols) == values).cloned());
        Ok(out)
    }

    /// The rows of one version whose columns `cols` hold `values`.
    pub fn rows(&self, old: bool, cols: &[usize], values: &[Value]) -> Result<Vec<Row>, EvalError> {
        if old {
            self.old_rows(cols, values)
        } else {
            Ok(self.new_rows(cols, values)?.into_iter().cloned().collect())
        }
    }

    /// The tick's change: inserted rows with weight 1, deleted rows with weight -1.
    pub fn delta(&self) -> impl Iterator<Item = (&Row, i64)> {
        self.ins.iter().map(|r| (r, 1)).chain(self.del.iter().map(|r| (r, -1)))
    }
}
