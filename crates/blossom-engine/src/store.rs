//! Relation storage that lives across ticks (ARCHITECTURE §4.2, value-level).
//!
//! A store holds one relation's rows with a count per row: how many sources support it (the carried state, the tick's
//! input, the program's facts, and every derivation of every rule). A row is present while its count is positive, so
//! deletions are exact (DBSP's counting). A lattice-valued relation (SEM-100) holds one row per cell: the join of the
//! cell's live contributions, recomputed when they change (a join has no inverse). A changed cell is joined when the
//! store is settled, not at each contribution: a tick retracts one contribution and adds another in some order, and a
//! join of the two (a point lattice's conflict, BLSR006) is a state the reference never sees.
//!
//! A store records the tick's change to its present rows (`ins`, `del`), which drives the rules reading it, and can
//! read the relation as it was at the start of the tick (`old`): the present rows, minus those inserted this tick, plus
//! those deleted. Indexes on column sets are built on first use (a probe, or a planner's estimate) and maintained with
//! every change after; they are ordered, so a probe can also take a range of one more column.
//!
//! A tiered store ([`Tiered`], docs/design/DATABASE.md §7) is a durable table whose rows live in the node's database
//! (the [`ColdTables`]): memory holds only the carry's changes the database does not hold yet, support other than
//! the carry (a program fact's), and the tick's change. Its reads probe the database and correct what they find
//! with what memory holds.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::sync::Arc;

use blossom_base::internal_error;
use blossom_ir::tick::{EvalError, Row};
use blossom_lattice::Kind;
use blossom_value::Value;

use crate::cold::{ColRange, ColdTables};
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

/// A tiered table's cold side and what memory holds of it (docs/design/DATABASE.md §7). A row is present while it is
/// carried or has other support (`Store::counts`); it is carried as the overlay says, or else as the cold side says
/// at its newest version. The overlay holds each row's newest carried membership from the ticks since the store
/// was reset, with the tick that set it: an entry the cold side has caught up with agrees with it (no later tick
/// changed the row, or the entry would be newer), and is dropped at a tick's start.
pub(crate) struct Tiered {
    pub rel: blossom_base::RelId,
    cold: Arc<dyn ColdTables>,
    /// The relation's key columns: a probe on all of them finds at most one row.
    key: Vec<usize>,
    /// Whether the table's rows can hold blobs (its in-memory store counts them).
    blobs: bool,
    overlay: BTreeMap<Row, (bool, u64)>,
    /// The overlay's rows by the tick that set them.
    by_tick: BTreeMap<u64, BTreeSet<Row>>,
    /// The change to the present rows the last carry made: the next tick's change (`Store::ins`, `Store::del`).
    staged: (BTreeSet<Row>, BTreeSet<Row>),
    /// No tick has begun since the store was made (at a reset), and whether the current tick is the first: as after
    /// any reset, the first tick shows every row as new, so the rules reading the table derive from all of it, and
    /// the table's next state is compared with what was carried at the reset (the cold side's rows).
    fresh: bool,
    first: bool,
    /// How many rows are present.
    len: usize,
}

impl std::fmt::Debug for Tiered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tiered")
            .field("rel", &self.rel)
            .field("overlay", &self.overlay.len())
            .field("len", &self.len)
            .finish()
    }
}

impl Tiered {
    /// The tiered store of `rel` over `cold`, as of the cold side's newest version.
    pub fn new(
        rel: blossom_base::RelId,
        cold: Arc<dyn ColdTables>,
        key: Vec<usize>,
        blobs: bool,
    ) -> Result<Tiered, EvalError> {
        let len = match cold.version()? {
            Some(v) => cold.count(rel, v)?,
            None => 0,
        };
        Ok(Tiered {
            rel,
            cold,
            key,
            blobs,
            overlay: BTreeMap::new(),
            by_tick: BTreeMap::new(),
            staged: (BTreeSet::new(), BTreeSet::new()),
            fresh: true,
            first: false,
            len,
        })
    }

    /// Whether the cold side holds `row` at its newest version (the carry before any tick's change).
    fn cold_carried(&self, row: &Row) -> Result<bool, EvalError> {
        match self.cold.version()? {
            Some(v) => self.cold.contains(self.rel, row, v),
            None => Ok(false),
        }
    }

    /// Whether `row` is carried.
    fn carried(&self, row: &Row) -> Result<bool, EvalError> {
        if let Some((present, _)) = self.overlay.get(row) {
            return Ok(*present);
        }
        match self.cold.version()? {
            Some(v) => self.cold.contains(self.rel, row, v),
            None => Ok(false),
        }
    }

    /// The carried rows the cold side finds for a probe, corrected by the overlay: its absent rows removed, its
    /// present rows that `matches` added.
    fn carried_rows(
        &self,
        cols: &[usize],
        values: &[Value],
        range: Option<ColRange<'_>>,
        matches: impl Fn(&Row) -> bool,
    ) -> Result<BTreeSet<Row>, EvalError> {
        let mut out: BTreeSet<Row> = match self.cold.version()? {
            Some(v) => self.cold.probe(self.rel, cols, values, range, v)?.into_iter().collect(),
            None => BTreeSet::new(),
        };
        for (row, (present, _)) in &self.overlay {
            if *present {
                if matches(row) {
                    out.insert(row.clone());
                }
            } else {
                out.remove(row);
            }
        }
        Ok(out)
    }
}

/// One relation's rows.
#[derive(Debug, Default)]
pub(crate) struct Store {
    pub cell: Option<CellSpec>,
    /// Set relation: the support of each row. Lattice relation: the support of each contribution. Hashed: a write
    /// costs one hash and one comparison, not a row comparison per tree level; nothing reads its order (readers whose
    /// output shows an order sort, [`Store::present_sorted`]).
    counts: blossom_base::det::DetMap<Row, i64>,
    /// Lattice relation: each cell's live contributions, and its merged row.
    contributions: BTreeMap<Vec<Value>, BTreeSet<Row>>,
    merged: BTreeMap<Vec<Value>, Row>,
    /// Cells whose contributions changed since the store was last settled.
    unsettled: BTreeSet<Vec<Value>>,
    /// Counts every change to the present rows.
    generation: u64,
    /// Lattice relation: the present rows (the merged cells). A set relation's present rows are the keys of
    /// `counts` (a row is kept there only while its support is positive), so it keeps no second copy.
    merged_rows: BTreeSet<Row>,
    /// Built lazily from `&self` (a node's engine runs on one thread).
    indexes: RefCell<BTreeMap<Vec<usize>, Index>>,
    /// The tick's change to the present rows.
    pub ins: BTreeSet<Row>,
    pub del: BTreeSet<Row>,
    /// Whether any support changed this tick, even where the present rows did not.
    pub touched: bool,
    /// For a relation whose rows can hold blobs: how many present rows hold each blob, kept with every change, so
    /// the node asks whether a blob is still held without a scan (FOREIGN-PROTOCOLS §5).
    blob_refs: Option<BTreeMap<blossom_value::BlobRef, u64>>,
    /// A tiered table: its rows are the cold side's, corrected by memory (`counts` holds only support other than the
    /// carry, and no index or blob count is kept).
    tiered: Option<Box<Tiered>>,
}

fn key(row: &[Value], cols: &[usize]) -> Vec<Value> {
    cols.iter().filter_map(|c| row.get(*c).cloned()).collect()
}

/// Whether `row`'s columns `cols` hold `values` (as `key(row, cols) == values`, without building the key).
fn holds(row: &[Value], cols: &[usize], values: &[Value]) -> bool {
    cols.len() == values.len() && cols.iter().zip(values).all(|(c, v)| row.get(*c) == Some(v))
}

/// A store's present rows, in order.
pub(crate) enum Present<'a> {
    Set(blossom_base::det::Iter<'a, Row, i64>),
    Lattice(std::collections::btree_set::Iter<'a, Row>),
}

impl<'a> Iterator for Present<'a> {
    type Item = &'a Row;

    fn next(&mut self) -> Option<&'a Row> {
        match self {
            Present::Set(it) => it.next().map(|(row, _)| row),
            Present::Lattice(it) => it.next(),
        }
    }
}

impl Store {
    /// A store; `blobs` when the relation's rows can hold blobs, which it then counts.
    pub fn new(cell: Option<CellSpec>, blobs: bool) -> Store {
        Store {
            cell,
            blob_refs: blobs.then(BTreeMap::new),
            ..Store::default()
        }
    }

    /// An empty store of the same kind (a tiered store becomes the in-memory store of its table).
    pub fn emptied(&self) -> Store {
        Store::new(
            self.cell.clone(),
            self.blob_refs.is_some() || self.tiered.as_ref().is_some_and(|t| t.blobs),
        )
    }

    /// A tiered store: a set table's rows on the cold side.
    pub fn tiered(tiered: Tiered) -> Store {
        Store {
            tiered: Some(Box::new(tiered)),
            ..Store::default()
        }
    }

    /// Whether a present row holds `b`. A tiered table answers for none: after a tick's end its rows are exactly the
    /// carried ones, whose blobs the node counts itself (`Engine::holds_blob`).
    pub fn holds_blob(&self, b: &blossom_value::BlobRef) -> bool {
        self.blob_refs.as_ref().is_some_and(|m| m.contains_key(b))
    }

    fn count_blobs(&mut self, row: &Row, shown: bool) {
        let Some(refs) = self.blob_refs.as_mut() else {
            return;
        };
        let mut bs = BTreeSet::new();
        row.iter().for_each(|v| blossom_value::blobs_in(v, &mut bs));
        for b in bs {
            if shown {
                *refs.entry(b).or_insert(0) += 1;
            } else if let Some(n) = refs.get_mut(&b) {
                *n -= 1;
                if *n == 0 {
                    refs.remove(&b);
                }
            }
        }
    }

    /// The present rows, in no particular order (a set relation's are hashed). A tiered store's are read by probes
    /// ([`Store::rows`]).
    pub fn present(&self) -> Result<Present<'_>, EvalError> {
        if self.tiered.is_some() {
            return Err(internal_error!("a tiered table's rows are read by probes, not held").into());
        }
        Ok(if self.cell.is_some() {
            Present::Lattice(self.merged_rows.iter())
        } else {
            Present::Set(self.counts.iter())
        })
    }

    /// The present rows, in order.
    pub fn present_sorted(&self) -> Result<Vec<Row>, EvalError> {
        let mut rows: Vec<Row> = match &self.tiered {
            Some(_) => self.new_rows(&[], &[])?,
            None => self.present()?.cloned().collect(),
        };
        rows.sort_unstable();
        Ok(rows)
    }

    /// How many rows are present.
    pub fn present_len(&self) -> usize {
        if let Some(t) = &self.tiered {
            t.len
        } else if self.cell.is_some() {
            self.merged_rows.len()
        } else {
            self.counts.len()
        }
    }

    /// How many rows memory holds: the present rows, or a tiered store's overlay and other support.
    pub fn resident_len(&self) -> usize {
        match &self.tiered {
            Some(t) => t.overlay.len() + self.counts.len(),
            None => self.present_len(),
        }
    }

    pub fn changed(&self) -> bool {
        !self.ins.is_empty() || !self.del.is_empty()
    }

    pub fn clear_delta(&mut self) {
        self.ins.clear();
        self.del.clear();
        self.touched = false;
    }

    /// Adds `w` (non-zero) to the support of `row`.
    pub fn add(&mut self, row: Row, w: i64) -> ExprResult<()> {
        if w == 0 {
            return Ok(());
        }
        self.touched = true;
        if self.tiered.is_some() {
            return self.add_tiered(row, w);
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
        if live.is_empty() {
            self.contributions.remove(&id);
        }
        self.unsettled.insert(id);
        Ok(())
    }

    /// A tiered store's support other than the carry (a program fact's).
    fn add_tiered(&mut self, row: Row, w: i64) -> ExprResult<()> {
        let was = self.contains(&row).map_err(ExprError::Eval)?;
        let count = self.counts.entry(row.clone()).or_insert(0);
        *count += w;
        let after = *count;
        if after == 0 {
            self.counts.remove(&row);
        }
        if after < 0 {
            return Err(bug(format!("the support of {row:?} went negative")));
        }
        let is = self.contains(&row).map_err(ExprError::Eval)?;
        match (was, is) {
            (false, true) => self.show(row),
            (true, false) => self.hide(&row),
            _ => {}
        }
        Ok(())
    }

    /// A tiered store takes tick `tick`'s change to its carry (the change to the table's next state, a net one: a row
    /// it inserts was not carried, one it deletes was), once the tick's outputs are built. Its present rows are then
    /// the next tick's; the change shows as the next tick's change ([`Store::begin_tick`]).
    pub fn carry(&mut self, row: &Row, present: bool, tick: u64) -> Result<(), EvalError> {
        let other = self.counts.contains_key(row);
        let t = self
            .tiered
            .as_deref_mut()
            .ok_or_else(|| internal_error!("a carry into a store that is not tiered"))?;
        if let Some((_, old)) = t.overlay.insert(row.clone(), (present, tick))
            && let Some(rows) = t.by_tick.get_mut(&old)
        {
            rows.remove(row);
            if rows.is_empty() {
                t.by_tick.remove(&old);
            }
        }
        t.by_tick.entry(tick).or_default().insert(row.clone());
        if other {
            return Ok(());
        }
        let (ins, del) = &mut t.staged;
        if present {
            t.len += 1;
            if !del.remove(row) {
                ins.insert(row.clone());
            }
        } else {
            t.len = t
                .len
                .checked_sub(1)
                .ok_or_else(|| internal_error!("a tiered table lost a row it did not hold"))?;
            if !ins.remove(row) {
                del.insert(row.clone());
            }
        }
        Ok(())
    }

    /// A tiered store at a tick's start (after its change was cleared): the last carry's change becomes the tick's,
    /// and the overlay's entries the cold side has caught up with go.
    pub fn begin_tick(&mut self) -> Result<(), EvalError> {
        let Some(t) = self.tiered.as_deref_mut() else {
            return Ok(());
        };
        t.first = std::mem::take(&mut t.fresh);
        if t.first {
            // Every row is new to the rules (their stores start empty after a reset): read whole, this once.
            let mut ins = t.carried_rows(&[], &[], None, |_| true)?;
            ins.extend(self.counts.keys().cloned());
            self.touched = true;
            self.generation = self.generation.wrapping_add(1);
            self.ins = ins;
            self.del = BTreeSet::new();
            return Ok(());
        }
        let (ins, del) = std::mem::take(&mut t.staged);
        if let Some(v) = t.cold.version()? {
            let kept = t.by_tick.split_off(&v.saturating_add(1));
            for rows in std::mem::replace(&mut t.by_tick, kept).into_values() {
                for row in rows {
                    t.overlay.remove(&row);
                }
            }
        }
        if !ins.is_empty() || !del.is_empty() {
            self.touched = true;
            self.generation = self.generation.wrapping_add(1);
        }
        self.ins = ins;
        self.del = del;
        Ok(())
    }

    /// Joins every changed cell's live contributions into its row.
    pub fn settle(&mut self) -> ExprResult<()> {
        let Some(spec) = self.cell.clone() else {
            return Ok(());
        };
        for id in std::mem::take(&mut self.unsettled) {
            let new = match self.contributions.get(&id) {
                Some(live) if !live.is_empty() => Some(spec.join(live.iter())?),
                _ => None,
            };
            let old = self.merged.get(&id).cloned();
            if old == new {
                continue;
            }
            if let Some(o) = old {
                self.hide(&o);
                self.merged.remove(&id);
            }
            if let Some(n) = new {
                self.merged.insert(id, n.clone());
                self.show(n);
            }
        }
        Ok(())
    }

    /// A read must see settled cells.
    fn settled(&self) -> Result<(), EvalError> {
        if self.unsettled.is_empty() {
            Ok(())
        } else {
            Err(internal_error!("a read of a lattice store with unsettled cells").into())
        }
    }

    /// A counter that moves with every change to the present rows.
    /// Whether `row` is present.
    /// Whether `row` was present at the start of the tick.
    pub fn contained(&self, row: &Row) -> Result<bool, EvalError> {
        Ok((self.contains(row)? && !self.ins.contains(row)) || self.del.contains(row))
    }

    /// A tiered store in the first tick after its reset: whether `row` was carried at the reset (the table's next
    /// state before the tick). `None`: not such a store, or not its first tick.
    pub fn carried_at_reset(&self, row: &Row) -> Result<Option<bool>, EvalError> {
        match &self.tiered {
            Some(t) if t.first => t.cold_carried(row).map(Some),
            _ => Ok(None),
        }
    }

    pub fn contains(&self, row: &Row) -> Result<bool, EvalError> {
        if let Some(t) = &self.tiered {
            return Ok(self.counts.contains_key(row) || t.carried(row)?);
        }
        Ok(if self.cell.is_some() {
            self.merged_rows.contains(row)
        } else {
            self.counts.contains_key(row)
        })
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Retracts every row, with all its support (a tick-scoped relation at the start of a tick: its rows came from the
    /// last tick's events). The change shows in the deltas like any other.
    pub fn retract_all(&mut self) -> ExprResult<()> {
        if self.cell.is_some() || self.tiered.is_some() {
            return Err(bug("a lattice or tiered store is never tick-scoped".into()));
        }
        let rows: Vec<(Row, i64)> = self.counts.iter().map(|(r, c)| (r.clone(), *c)).collect();
        for (row, count) in rows {
            self.add(row, -count)?;
        }
        Ok(())
    }

    fn show(&mut self, row: Row) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(t) = self.tiered.as_deref_mut() {
            t.len += 1;
        }
        self.count_blobs(&row, true);
        for (cols, index) in self.indexes.get_mut() {
            index.entry(key(&row, cols)).or_default().insert(row.clone());
        }
        if self.cell.is_some() {
            self.merged_rows.insert(row.clone());
        }
        if !self.del.remove(&row) {
            self.ins.insert(row);
        }
    }

    fn hide(&mut self, row: &Row) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(t) = self.tiered.as_deref_mut() {
            t.len = t.len.saturating_sub(1);
        }
        self.count_blobs(row, false);
        for (cols, index) in self.indexes.get_mut() {
            let k = key(row, cols);
            if let Some(bucket) = index.get_mut(&k) {
                bucket.remove(row);
                if bucket.is_empty() {
                    index.remove(&k);
                }
            }
        }
        if self.cell.is_some() {
            self.merged_rows.remove(row);
        }
        if !self.ins.remove(row) {
            self.del.insert(row.clone());
        }
    }

    /// Builds the index on `cols` if there is none.
    /// (A tiered store keeps none: the cold side keeps its own.)
    pub fn ensure_index(&self, cols: &[usize]) {
        if cols.is_empty() || self.tiered.is_some() || self.indexes.borrow().contains_key(cols) {
            return;
        }
        let mut index = Index::new();
        let rows = if self.cell.is_some() {
            Present::Lattice(self.merged_rows.iter())
        } else {
            Present::Set(self.counts.iter())
        };
        for row in rows {
            index.entry(key(row, cols)).or_default().insert(row.clone());
        }
        self.indexes.borrow_mut().insert(cols.to_vec(), index);
    }

    /// About how many present rows match a probe on `cols`: all of them, or the average bucket of the index. A
    /// tiered store guesses without reading the cold side: one row for a probe on the key, else an eighth.
    pub fn estimate(&self, cols: &[usize]) -> usize {
        let n = self.present_len();
        if cols.is_empty() {
            return n;
        }
        if let Some(t) = &self.tiered {
            if !t.key.is_empty() && t.key.iter().all(|k| cols.contains(k)) {
                return 1;
            }
            return n.div_ceil(8).max(1);
        }
        self.ensure_index(cols);
        let keys = self.indexes.borrow().get(cols).map_or(1, BTreeMap::len).max(1);
        n.div_ceil(keys)
    }

    /// The present rows whose columns `cols` hold `values`.
    pub fn new_rows(&self, cols: &[usize], values: &[Value]) -> Result<Vec<Row>, EvalError> {
        self.settled()?;
        if let Some(t) = &self.tiered {
            let matches = |r: &Row| holds(r, cols, values);
            let mut out = t.carried_rows(cols, values, None, matches)?;
            out.extend(self.counts.keys().filter(|r| matches(r)).cloned());
            return Ok(out.into_iter().collect());
        }
        if cols.is_empty() {
            return Ok(self.present()?.cloned().collect());
        }
        self.ensure_index(cols);
        let indexes = self.indexes.borrow();
        let index = indexes
            .get(cols)
            .ok_or_else(|| internal_error!("a probe on columns {cols:?} without an index"))?;
        Ok(index
            .get(values)
            .map(|b| b.iter().cloned().collect())
            .unwrap_or_default())
    }

    /// Whether a row of one version has columns `cols` holding `values` (without collecting them).
    pub fn any(&self, old: bool, cols: &[usize], values: &[Value]) -> Result<bool, EvalError> {
        self.settled()?;
        if self.tiered.is_some() {
            return Ok(!self.rows(old, cols, values)?.is_empty());
        }
        let matches = |r: &Row| holds(r, cols, values);
        if old && self.del.iter().any(matches) {
            return Ok(true);
        }
        let live = |r: &Row| !old || !self.ins.contains(r);
        if cols.is_empty() {
            return Ok(self.present()?.any(live));
        }
        self.ensure_index(cols);
        let indexes = self.indexes.borrow();
        let index = indexes
            .get(cols)
            .ok_or_else(|| internal_error!("a probe on columns {cols:?} without an index"))?;
        Ok(index.get(values).is_some_and(|b| b.iter().any(live)))
    }

    /// The rows that were present at the start of the tick with columns `cols` holding `values`.
    pub fn old_rows(&self, cols: &[usize], values: &[Value]) -> Result<Vec<Row>, EvalError> {
        let mut out: Vec<Row> = self
            .new_rows(cols, values)?
            .into_iter()
            .filter(|r| !self.ins.contains(r))
            .collect();
        out.extend(self.del.iter().filter(|r| holds(r, cols, values)).cloned());
        Ok(out)
    }

    /// The rows of one version whose columns `cols` hold `values` and whose column `col` lies between `lo` and
    /// `hi`, from the ordered index on `cols` then `col`.
    pub fn range_rows(
        &self,
        old: bool,
        cols: &[usize],
        values: &[Value],
        col: usize,
        lo: Bound<Value>,
        hi: Bound<Value>,
    ) -> Result<Vec<Row>, EvalError> {
        self.settled()?;
        if let Some(t) = &self.tiered {
            return self.tiered_range(t, old, cols, values, col, lo, hi);
        }
        let mut key_cols = cols.to_vec();
        key_cols.push(col);
        self.ensure_index(&key_cols);
        let indexes = self.indexes.borrow();
        let index = indexes
            .get(&key_cols)
            .ok_or_else(|| internal_error!("a range probe on columns {key_cols:?} without an index"))?;
        let within = |v: &Value| {
            (match &lo {
                Bound::Included(l) => v >= l,
                Bound::Excluded(l) => v > l,
                Bound::Unbounded => true,
            }) && (match &hi {
                Bound::Included(h) => v <= h,
                Bound::Excluded(h) => v < h,
                Bound::Unbounded => true,
            })
        };
        // Keys are `values` then the range column: start at the lower end, stop past the prefix or the upper end.
        let mut start = values.to_vec();
        let lower = match &lo {
            Bound::Included(l) | Bound::Excluded(l) => {
                start.push(l.clone());
                Bound::Included(start)
            }
            Bound::Unbounded => Bound::Included(start),
        };
        let mut out: Vec<Row> = Vec::new();
        for (k, bucket) in index.range((lower, Bound::Unbounded)) {
            let (prefix, last) = k.split_at(k.len().saturating_sub(1));
            if prefix != values {
                break;
            }
            let Some(v) = last.first() else { continue };
            if !within(v) {
                if matches!(&hi, Bound::Included(h) | Bound::Excluded(h) if v >= h) {
                    break;
                }
                continue;
            }
            out.extend(bucket.iter().filter(|r| !old || !self.ins.contains(*r)).cloned());
        }
        if old {
            out.extend(
                self.del
                    .iter()
                    .filter(|r| holds(r, cols, values) && r.get(col).is_some_and(within))
                    .cloned(),
            );
        }
        Ok(out)
    }

    /// [`Store::range_rows`] of a tiered store: the cold side's range, corrected by memory.
    #[allow(clippy::too_many_arguments)]
    fn tiered_range(
        &self,
        t: &Tiered,
        old: bool,
        cols: &[usize],
        values: &[Value],
        col: usize,
        lo: Bound<Value>,
        hi: Bound<Value>,
    ) -> Result<Vec<Row>, EvalError> {
        let within = |r: &Row| {
            holds(r, cols, values)
                && r.get(col).is_some_and(|v| {
                    (match &lo {
                        Bound::Included(l) => v >= l,
                        Bound::Excluded(l) => v > l,
                        Bound::Unbounded => true,
                    }) && (match &hi {
                        Bound::Included(h) => v <= h,
                        Bound::Excluded(h) => v < h,
                        Bound::Unbounded => true,
                    })
                })
        };
        let mut out = t.carried_rows(cols, values, Some((col, lo.as_ref(), hi.as_ref())), within)?;
        out.extend(self.counts.keys().filter(|r| within(r)).cloned());
        if old {
            out.retain(|r| !self.ins.contains(r));
            out.extend(self.del.iter().filter(|r| within(r)).cloned());
        }
        Ok(out.into_iter().collect())
    }

    /// The rows of one version whose columns `cols` hold `values`.
    pub fn rows(&self, old: bool, cols: &[usize], values: &[Value]) -> Result<Vec<Row>, EvalError> {
        if old {
            self.old_rows(cols, values)
        } else {
            self.new_rows(cols, values)
        }
    }

    /// The tick's change: inserted rows with weight 1, deleted rows with weight -1.
    pub fn delta(&self) -> impl Iterator<Item = (&Row, i64)> {
        self.ins.iter().map(|r| (r, 1)).chain(self.del.iter().map(|r| (r, -1)))
    }
}
