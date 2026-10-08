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

/// Rows by the values of some of their columns, hashed.
type RowsBy = blossom_base::det::DetMap<Vec<Value>, BTreeSet<Row>>;

/// A probe's rows kept in the hot tier (sorted), and when they were last used.
type Kept = (Arc<Vec<Row>>, u64);

/// How many rows an engine's tiered stores keep of their recent probes, together, by default
/// (`EngineConfig::hot_rows`).
pub(crate) const HOT_ROWS: usize = 1 << 16;

/// The most rows a hot tier keeps of one probe (Kafka at 300000 records: 1024 cost an eighth of the throughput).
const PROBE_ROWS: usize = 256;

/// A tiered table's hot tier: the cold side's answers to its recent probes (the rows with given values in given
/// columns, sorted, and whether a row is there), kept equal to the cold side's newest version as it moves: each
/// overlay entry the cold side catches up with is applied to them rather than dropping them, so a probe asked again
/// costs no read of the cold side. A range probe is answered from its prefix's rows when they are few enough to
/// keep. Bounded by `budget` rows: past it, the least recently used half goes.
#[derive(Default)]
struct Hot {
    /// By the probe's columns, then their values: the rows, and when they were last used.
    probes: BTreeMap<Vec<usize>, blossom_base::det::DetMap<Vec<Value>, Kept>>,
    /// Probes known to find more rows than one may keep: a range probe of one reads the cold side's range.
    large: BTreeMap<Vec<usize>, blossom_base::det::DetMap<Vec<Value>, u64>>,
    /// Rows asked for: their support on the cold side (a table's 0 or 1), and when last used.
    contains: blossom_base::det::DetMap<Row, (u64, u64)>,
    /// Range probes of large prefixes, by the probe's columns and the range's column, then their values: each range
    /// asked (its bounds), its rows (sorted) and when they were last used. A range asked again (a follower fetching
    /// from where it stands) costs no read of the cold side.
    ranges: BTreeMap<(Vec<usize>, usize), RangesBy>,
    /// The rows kept: every probe's and range's, and one per membership.
    rows: usize,
    clock: u64,
    budget: usize,
}

/// A range probe's rows kept in the hot tier: its bounds, its rows (sorted) and when they were last used.
struct KeptRange {
    lo: Bound<Value>,
    hi: Bound<Value>,
    rows: Arc<Vec<Row>>,
    used: u64,
}

/// A prefix's kept ranges by its values.
type RangesBy = blossom_base::det::DetMap<Vec<Value>, Vec<KeptRange>>;

/// The most ranges kept for one prefix: past it, the least recently used goes.
const RANGES_PER_PREFIX: usize = 8;

/// Whether `v` lies within `lo` and `hi`.
fn within_bounds(v: &Value, lo: &Bound<Value>, hi: &Bound<Value>) -> bool {
    (match lo {
        Bound::Included(l) => v >= l,
        Bound::Excluded(l) => v > l,
        Bound::Unbounded => true,
    }) && (match hi {
        Bound::Included(h) => v <= h,
        Bound::Excluded(h) => v < h,
        Bound::Unbounded => true,
    })
}

impl Hot {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// The most rows one probe may keep: a small share of the budget, so the probes kept are many, and never more
    /// than [`PROBE_ROWS`]: a kept answer is read whole and kept current row by row, and a prefix not known yet is
    /// read up to it, so a larger prefix's ranges and first rows are read from the cold side instead (their cost is
    /// what they read, not the prefix).
    fn largest(&self) -> usize {
        (self.budget / 64).clamp(2, PROBE_ROWS)
    }

    fn probe(&mut self, cols: &[usize], values: &[Value]) -> Option<Arc<Vec<Row>>> {
        let now = self.tick();
        let (rows, used) = self.probes.get_mut(cols)?.get_mut(values)?;
        *used = now;
        Some(rows.clone())
    }

    fn is_large(&mut self, cols: &[usize], values: &[Value]) -> bool {
        let now = self.tick();
        match self.large.get_mut(cols).and_then(|m| m.get_mut(values)) {
            Some(used) => {
                *used = now;
                true
            }
            None => false,
        }
    }

    fn mark_large(&mut self, cols: &[usize], values: &[Value]) {
        let now = self.tick();
        self.large
            .entry(cols.to_vec())
            .or_default()
            .insert(values.to_vec(), now);
        // The markers are a key each, not rows: past the budget's count, the least recently used half goes.
        if self.large.values().map(|m| m.len()).sum::<usize>() > self.budget {
            let mut uses: Vec<u64> = self.large.values().flat_map(|m| m.values().copied()).collect();
            uses.sort_unstable();
            let cut = uses.get(uses.len() / 2).copied().unwrap_or(0);
            for by_values in self.large.values_mut() {
                by_values.retain(|_, u| *u > cut);
            }
            self.large.retain(|_, m| !m.is_empty());
        }
    }

    /// Keeps a probe's rows (sorted).
    fn keep_probe(&mut self, cols: &[usize], values: &[Value], rows: Arc<Vec<Row>>) {
        let now = self.tick();
        self.rows += rows.len();
        if let Some((old, _)) = self
            .probes
            .entry(cols.to_vec())
            .or_default()
            .insert(values.to_vec(), (rows, now))
        {
            self.rows -= old.len();
        }
        self.trim();
    }

    fn range(&mut self, cols: &[usize], values: &[Value], range: &ColRange<'_>) -> Option<Arc<Vec<Row>>> {
        let now = self.tick();
        let (col, lo, hi) = range;
        let kept = self.ranges.get_mut(&(cols.to_vec(), *col))?.get_mut(values)?;
        let entry = kept.iter_mut().find(|k| k.lo.as_ref() == *lo && k.hi.as_ref() == *hi)?;
        entry.used = now;
        Some(entry.rows.clone())
    }

    /// Keeps a range probe's rows (sorted).
    fn keep_range(&mut self, cols: &[usize], values: &[Value], range: &ColRange<'_>, rows: Arc<Vec<Row>>) {
        if rows.len() > self.largest() {
            return;
        }
        let now = self.tick();
        let (col, lo, hi) = range;
        let kept = self
            .ranges
            .entry((cols.to_vec(), *col))
            .or_default()
            .entry(values.to_vec())
            .or_default();
        self.rows += rows.len();
        if let Some(at) = kept.iter().position(|k| k.lo.as_ref() == *lo && k.hi.as_ref() == *hi) {
            let old = kept.remove(at);
            self.rows -= old.rows.len();
        }
        if kept.len() == RANGES_PER_PREFIX
            && let Some((at, _)) = kept.iter().enumerate().min_by_key(|(_, k)| k.used)
        {
            let old = kept.remove(at);
            self.rows -= old.rows.len();
        }
        kept.push(KeptRange {
            lo: lo.cloned(),
            hi: hi.cloned(),
            rows,
            used: now,
        });
        self.trim();
    }

    fn support(&mut self, row: &Row) -> Option<u64> {
        let now = self.tick();
        let (support, used) = self.contains.get_mut(row)?;
        *used = now;
        Some(*support)
    }

    fn keep_support(&mut self, row: &Row, support: u64) {
        let now = self.tick();
        if self.contains.insert(row.clone(), (support, now)).is_none() {
            self.rows += 1;
        }
        self.trim();
    }

    /// The cold side now holds `row` with `support` (absent at 0): every kept answer it touches follows (a probe that
    /// outgrows what one may keep goes, and is known large).
    fn caught_up(&mut self, row: &Row, support: u64) {
        let present = support > 0;
        let largest = self.largest();
        for (cols, by_values) in self.probes.iter_mut() {
            let values = key(row, cols);
            let Some((rows, used)) = by_values.get_mut(&values) else {
                continue;
            };
            match (rows.binary_search(row), present) {
                (Err(at), true) => {
                    Arc::make_mut(rows).insert(at, row.clone());
                    self.rows += 1;
                    if rows.len() > largest {
                        let used = *used;
                        if let Some((gone, _)) = by_values.remove(&values) {
                            self.rows -= gone.len();
                        }
                        self.large.entry(cols.clone()).or_default().insert(values, used);
                    }
                }
                (Ok(at), false) => {
                    Arc::make_mut(rows).remove(at);
                    self.rows -= 1;
                }
                _ => {}
            }
        }
        for ((cols, col), by_values) in self.ranges.iter_mut() {
            let values = key(row, cols);
            let Some(kept) = by_values.get_mut(&values) else {
                continue;
            };
            let Some(v) = row.get(*col) else { continue };
            kept.retain_mut(|k| {
                if !within_bounds(v, &k.lo, &k.hi) {
                    return true;
                }
                match (k.rows.binary_search(row), present) {
                    (Err(at), true) => {
                        Arc::make_mut(&mut k.rows).insert(at, row.clone());
                        self.rows += 1;
                    }
                    (Ok(at), false) => {
                        Arc::make_mut(&mut k.rows).remove(at);
                        self.rows -= 1;
                    }
                    _ => {}
                }
                // A range that outgrows what one may keep goes.
                if k.rows.len() > largest {
                    self.rows -= k.rows.len();
                    return false;
                }
                true
            });
        }
        if let Some((held, _)) = self.contains.get_mut(row) {
            *held = support;
        }
    }

    /// Past the budget, the least recently used half goes.
    fn trim(&mut self) {
        if self.rows > self.budget {
            self.halve();
        }
    }

    /// Down to `target` rows, the least recently used half at a time.
    fn shrink(&mut self, target: usize) {
        while self.rows > target {
            let before = self.rows;
            self.halve();
            if self.rows >= before {
                return;
            }
        }
    }

    /// The least recently used half of what is kept goes.
    fn halve(&mut self) {
        let mut uses: Vec<u64> = self
            .probes
            .values()
            .flat_map(|m| m.values().map(|(_, u)| *u))
            .chain(self.contains.values().map(|(_, u)| *u))
            .chain(
                self.ranges
                    .values()
                    .flat_map(|m| m.values().flat_map(|ks| ks.iter().map(|k| k.used))),
            )
            .collect();
        uses.sort_unstable();
        let cut = uses.get(uses.len() / 2).copied().unwrap_or(0);
        let mut rows = 0usize;
        for by_values in self.probes.values_mut() {
            by_values.retain(|_, (r, u)| {
                let keep = *u > cut;
                if keep {
                    rows += r.len();
                }
                keep
            });
        }
        self.probes.retain(|_, m| !m.is_empty());
        for by_values in self.ranges.values_mut() {
            by_values.retain(|_, kept| {
                kept.retain(|k| {
                    let keep = k.used > cut;
                    if keep {
                        rows += k.rows.len();
                    }
                    keep
                });
                !kept.is_empty()
            });
        }
        self.ranges.retain(|_, m| !m.is_empty());
        self.contains.retain(|_, (_, u)| *u > cut);
        self.rows = rows + self.contains.len();
    }
}

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
    overlay: blossom_base::det::DetMap<Row, (u64, u64)>,
    /// A durable view (docs/design/DATABASE.md §8): its rows have support counts, which the overlay and the cold side
    /// hold (a table's are 0 or 1, its carry), and its rules write it within a tick.
    counted: bool,
    /// The tick whose changes the store takes now (a durable view's label for its overlay entries).
    now: u64,
    /// A durable view's rows whose support changed since the changes were last taken, with their support before.
    changed: BTreeMap<Row, u64>,
    /// The overlay's rows by the tick that set them.
    by_tick: BTreeMap<u64, BTreeSet<Row>>,
    /// The overlay's rows by the values of the columns a probe asks for: built for a column list on its first probe
    /// and kept with every overlay change, so a probe's correction costs its rows, not the overlay's.
    overlay_by: RefCell<BTreeMap<Vec<usize>, RowsBy>>,
    /// The hot tier: recent probes' answers, kept with the cold side.
    hot: RefCell<Hot>,
    /// What probes found, for the planner's estimates (the cold side keeps no statistics), as running averages in
    /// sixteenths of a row: by the probe's columns, the rows a prefix holds (`false`: an index's average bucket, as an
    /// in-memory store knows it), and the rows a range of a prefix gave (`true`).
    seen: RefCell<BTreeMap<(Vec<usize>, bool), u64>>,
    /// Moves whenever an average crosses a power of two: a join order planned on the old ones is planned again.
    epoch: std::cell::Cell<u64>,
    /// The change to the present rows the last carry made: the next tick's change (`Store::ins`, `Store::del`).
    staged: (BTreeSet<Row>, BTreeSet<Row>),
    /// No tick has begun since the store was made (at a reset), and whether the current tick is the first: as after
    /// any reset, the first tick shows every row as new, so the rules reading the table derive from all of it, and
    /// the table's next state is compared with what was carried at the reset (the cold side's rows).
    fresh: bool,
    first: bool,
    /// How many rows are present.
    len: usize,
    /// The rows present this tick that were not carried into it (a rule of the tick wrote them): what the tick's
    /// rows hold beyond the table's previous version (docs/design/DATABASE.md §8).
    uncarried: BTreeSet<Row>,
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
        hot_rows: usize,
        counted: bool,
        fresh: bool,
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
            overlay: blossom_base::det::DetMap::new(),
            counted,
            now: 0,
            changed: BTreeMap::new(),
            by_tick: BTreeMap::new(),
            overlay_by: RefCell::new(BTreeMap::new()),
            hot: RefCell::new(Hot {
                budget: hot_rows,
                ..Hot::default()
            }),
            seen: RefCell::new(BTreeMap::new()),
            epoch: std::cell::Cell::new(0),
            staged: (BTreeSet::new(), BTreeSet::new()),
            fresh,
            first: false,
            len,
            uncarried: BTreeSet::new(),
        })
    }

    /// `row`'s support on the cold side at its newest version (a table's: whether it is carried before any tick's
    /// change).
    fn cold_support(&self, row: &Row) -> Result<u64, EvalError> {
        if let Some(support) = self.hot.borrow_mut().support(row) {
            return Ok(support);
        }
        let support = match self.cold.version()? {
            Some(v) => self.cold.support(self.rel, row, v)?,
            None => 0,
        };
        self.hot.borrow_mut().keep_support(row, support);
        Ok(support)
    }

    /// Whether the cold side holds `row` at its newest version.
    fn cold_carried(&self, row: &Row) -> Result<bool, EvalError> {
        Ok(self.cold_support(row)? > 0)
    }

    /// `row`'s support: as the overlay says, else as the cold side does.
    fn support(&self, row: &Row) -> Result<u64, EvalError> {
        match self.overlay.get(row) {
            Some((support, _)) => Ok(*support),
            None => self.cold_support(row),
        }
    }

    /// Whether `row` is carried (a durable view's: has support).
    fn carried(&self, row: &Row) -> Result<bool, EvalError> {
        Ok(self.support(row)? > 0)
    }

    /// Sets `row`'s support in the overlay, as tick `tick` left it.
    fn set_overlay(&mut self, row: &Row, support: u64, tick: u64) {
        match self.overlay.insert(row.clone(), (support, tick)) {
            Some((_, old)) => {
                if old != tick
                    && let Some(rows) = self.by_tick.get_mut(&old)
                {
                    rows.remove(row);
                    if rows.is_empty() {
                        self.by_tick.remove(&old);
                    }
                }
            }
            None => self.index_overlay(row, true),
        }
        self.by_tick.entry(tick).or_default().insert(row.clone());
    }

    /// The cold side's rows for a probe on `cols` holding `values` (and, with `range`, the rows of those that
    /// `matches`), sorted: from the hot tier, else read and kept there when few enough.
    fn cold_rows(
        &self,
        cols: &[usize],
        values: &[Value],
        range: Option<ColRange<'_>>,
        matches: &impl Fn(&Row) -> bool,
    ) -> Result<Arc<Vec<Row>>, EvalError> {
        let rows = self.read_cold(cols, values, range, matches)?;
        if range.is_some() {
            self.observe(cols, true, rows.len());
        }
        Ok(rows)
    }

    /// A prefix of `cols` holds `rows` rows (`range`: a range of one gave them): the running average moves an eighth
    /// of the way.
    fn observe(&self, cols: &[usize], range: bool, rows: usize) {
        let x = (rows as u64).saturating_mul(16);
        let mut seen = self.seen.borrow_mut();
        let crossed = match seen.get_mut(&(cols.to_vec(), range)) {
            Some(avg) => {
                let old = *avg;
                *avg = (old.saturating_mul(7).saturating_add(x)) / 8;
                old.max(1).ilog2() != (*avg).max(1).ilog2()
            }
            None => {
                seen.insert((cols.to_vec(), range), x);
                true
            }
        };
        if crossed {
            self.epoch.set(self.epoch.get().wrapping_add(1));
        }
    }

    /// About how many rows a probe on `cols` finds, with a range on one more column or not. Its prefix's rows: all
    /// of them for no columns, one for the key, else as prefixes were found to hold, or one for columns not seen yet
    /// (the probe is tried, then known). A range: a small fraction of its prefix's rows, as an in-memory store
    /// reckons, or what ranges of that shape gave if more (a range read from the cold side costs what it gives).
    fn estimate(&self, cols: &[usize], range: bool) -> usize {
        let seen = self.seen.borrow();
        let avg = |r: bool| seen.get(&(cols.to_vec(), r)).map(|a| (*a / 16).max(1) as usize);
        let prefix = if cols.is_empty() {
            self.len
        } else if !self.key.is_empty() && self.key.iter().all(|k| cols.contains(k)) {
            1
        } else {
            avg(false).unwrap_or(1)
        };
        if range {
            (prefix / 16 + 1).max(avg(true).unwrap_or(0))
        } else {
            prefix
        }
    }

    fn read_cold(
        &self,
        cols: &[usize],
        values: &[Value],
        range: Option<ColRange<'_>>,
        matches: &impl Fn(&Row) -> bool,
    ) -> Result<Arc<Vec<Row>>, EvalError> {
        let Some(v) = self.cold.version()? else {
            return Ok(Arc::new(Vec::new()));
        };
        let ranged = |all: Arc<Vec<Row>>| match range {
            None => all,
            // A range on the column after a leading run: the rows (sorted, one prefix) are in that column's order.
            Some((col, lo, hi)) if col == cols.len() && cols.iter().enumerate().all(|(i, c)| i == *c) => {
                fn at(r: &Row, col: usize) -> Option<&Value> {
                    r.get(col)
                }
                let from = all.partition_point(|r| match lo {
                    Bound::Included(l) => at(r, col).is_none_or(|v| v < l),
                    Bound::Excluded(l) => at(r, col).is_none_or(|v| v <= l),
                    Bound::Unbounded => false,
                });
                let to = all.partition_point(|r| match hi {
                    Bound::Included(h) => at(r, col).is_none_or(|v| v <= h),
                    Bound::Excluded(h) => at(r, col).is_none_or(|v| v < h),
                    Bound::Unbounded => true,
                });
                Arc::new(all.get(from..to.max(from)).unwrap_or_default().to_vec())
            }
            Some(_) => Arc::new(all.iter().filter(|r| matches(r)).cloned().collect()),
        };
        let (large, max) = {
            let mut hot = self.hot.borrow_mut();
            if let Some(all) = hot.probe(cols, values) {
                drop(hot);
                self.observe(cols, false, all.len());
                return Ok(ranged(all));
            }
            (hot.is_large(cols, values), hot.largest())
        };
        if !large {
            match self.cold.probe_at_most(self.rel, cols, values, v, max)? {
                Some(mut all) => {
                    all.sort_unstable();
                    self.observe(cols, false, all.len());
                    let all = Arc::new(all);
                    self.hot.borrow_mut().keep_probe(cols, values, all.clone());
                    return Ok(ranged(all));
                }
                None => {
                    self.hot.borrow_mut().mark_large(cols, values);
                    // Its rows are not counted: more than a probe may keep, so large for the planner.
                    self.observe(cols, false, max.saturating_mul(16));
                }
            }
        }
        // A large prefix: its range from the hot tier when asked before, else from the cold side (kept if small).
        if let Some(r) = &range
            && let Some(rows) = self.hot.borrow_mut().range(cols, values, r)
        {
            return Ok(rows);
        }
        let mut rows = self.cold.probe(self.rel, cols, values, range, v)?;
        rows.sort_unstable();
        if range.is_none() {
            self.observe(cols, false, rows.len());
        }
        let rows = Arc::new(rows);
        if let Some(r) = &range {
            self.hot.borrow_mut().keep_range(cols, values, r, rows.clone());
        }
        Ok(rows)
    }

    /// The overlay's corrections to the cold side's rows for a probe on `cols` holding `values` (sorted, `cold`):
    /// the positions of the rows it holds absent, and its present rows with those values that `matches` the cold
    /// side lacks.
    fn corrections(
        &self,
        cols: &[usize],
        values: &[Value],
        cold: &[Row],
        matches: impl Fn(&Row) -> bool,
    ) -> (BTreeSet<usize>, Vec<Row>) {
        let (mut gone, mut added) = (BTreeSet::new(), Vec::new());
        if self.overlay.is_empty() {
            return (gone, added);
        }
        let mut by = self.overlay_by.borrow_mut();
        let index = by.entry(cols.to_vec()).or_insert_with(|| {
            let mut index = blossom_base::det::DetMap::new();
            for row in self.overlay.keys() {
                index
                    .entry(key(row, cols))
                    .or_insert_with(BTreeSet::new)
                    .insert(row.clone());
            }
            index
        });
        for row in index.get(values).into_iter().flatten() {
            let present = self.overlay.get(row).is_some_and(|(support, _)| *support > 0);
            match (cold.binary_search(row), present) {
                (Ok(at), false) => {
                    gone.insert(at);
                }
                (Err(_), true) if matches(row) => added.push(row.clone()),
                _ => {}
            }
        }
        (gone, added)
    }

    /// The carried rows for a probe on `cols` holding `values` (and, with `range`, those that `matches`): the cold
    /// side's, corrected by the overlay. No row twice.
    fn carried_rows(
        &self,
        cols: &[usize],
        values: &[Value],
        range: Option<ColRange<'_>>,
        matches: impl Fn(&Row) -> bool,
    ) -> Result<Vec<Row>, EvalError> {
        let cold = self.cold_rows(cols, values, range, &matches)?;
        let (gone, added) = self.corrections(cols, values, &cold, matches);
        let mut out: Vec<Row> = if gone.is_empty() {
            cold.to_vec()
        } else {
            cold.iter()
                .enumerate()
                .filter(|(i, _)| !gone.contains(i))
                .map(|(_, r)| r.clone())
                .collect()
        };
        out.extend(added);
        Ok(out)
    }

    /// Whether a carried row has columns `cols` holding `values` and is `live` (at most `excluded` rows of the
    /// cold side's are not). A prefix too large to keep is asked for its first rows only: one more than the overlay
    /// holds absent and `excluded` together, of which one at least counts if there are that many.
    fn any_carried(
        &self,
        cols: &[usize],
        values: &[Value],
        excluded: usize,
        live: impl Fn(&Row) -> bool,
    ) -> Result<bool, EvalError> {
        let large = {
            let mut hot = self.hot.borrow_mut();
            hot.probe(cols, values).is_none() && hot.is_large(cols, values)
        };
        if !large {
            let cold = self.cold_rows(cols, values, None, &|_| true)?;
            let (gone, added) = self.corrections(cols, values, &cold, |_| true);
            return Ok(cold.iter().enumerate().any(|(i, r)| !gone.contains(&i) && live(r)) || added.iter().any(live));
        }
        let Some(v) = self.cold.version()? else {
            return Ok(false);
        };
        let mut n = self.absent_with(cols, values) + excluded + 1;
        loop {
            let (mut first, more) = self.cold.probe_some(self.rel, cols, values, v, n)?;
            first.sort_unstable();
            let (gone, added) = self.corrections(cols, values, &first, |_| true);
            if first.iter().enumerate().any(|(i, r)| !gone.contains(&i) && live(r)) || added.iter().any(&live) {
                return Ok(true);
            }
            if !more {
                return Ok(false);
            }
            n = n.saturating_mul(2);
        }
    }

    /// How many rows the overlay holds absent with columns `cols` holding `values`.
    fn absent_with(&self, cols: &[usize], values: &[Value]) -> usize {
        if self.overlay.is_empty() {
            return 0;
        }
        let mut by = self.overlay_by.borrow_mut();
        let index = by.entry(cols.to_vec()).or_insert_with(|| {
            let mut index = blossom_base::det::DetMap::new();
            for row in self.overlay.keys() {
                index
                    .entry(key(row, cols))
                    .or_insert_with(BTreeSet::new)
                    .insert(row.clone());
            }
            index
        });
        index
            .get(values)
            .into_iter()
            .flatten()
            .filter(|r| self.overlay.get(*r).is_some_and(|(support, _)| *support == 0))
            .count()
    }

    /// Keeps the overlay's indexes with a row the overlay gained (`true`) or lost.
    fn index_overlay(&mut self, row: &Row, gained: bool) {
        for (cols, index) in self.overlay_by.get_mut().iter_mut() {
            let k = key(row, cols);
            if gained {
                index.entry(k).or_insert_with(BTreeSet::new).insert(row.clone());
            } else if let Some(rows) = index.get_mut(&k) {
                rows.remove(row);
                if rows.is_empty() {
                    index.remove(&k);
                }
            }
        }
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

    /// Whether a present row holds `b`. A tiered store answers for its rows of support kept in memory only (a mixed
    /// relation's other rules'): its other rows are carried or kept with the database's, whose blobs the node counts
    /// itself (`Engine::holds_blob`).
    pub fn holds_blob(&self, b: &blossom_value::BlobRef) -> bool {
        if self.tiered.is_some() {
            return self.counts.keys().any(|row| {
                let mut bs = BTreeSet::new();
                row.iter().for_each(|v| blossom_value::blobs_in(v, &mut bs));
                bs.contains(b)
            });
        }
        self.blob_refs.as_ref().is_some_and(|m| m.contains_key(b))
    }

    /// Adds `w` to `row`'s support, as [`Store::add`] does; `volatile`: support kept in memory only, never with the
    /// database's (a mixed relation's rules that are not durable, DATABASE.md §8).
    pub fn add_by(&mut self, row: Row, w: i64, volatile: bool) -> ExprResult<()> {
        if volatile && self.tiered.is_some() {
            if w == 0 {
                return Ok(());
            }
            self.touched = true;
            return self.add_tiered(row, w);
        }
        self.add(row, w)
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

    /// How many rows a tiered store's hot tier keeps.
    pub fn hot_rows(&self) -> usize {
        self.tiered.as_ref().map_or(0, |t| t.hot.borrow().rows)
    }

    /// Shrinks a tiered store's hot tier to `target` rows, the least recently used first.
    pub fn shrink_hot(&mut self, target: usize) {
        if let Some(t) = self.tiered.as_deref_mut() {
            t.hot.get_mut().shrink(target);
        }
    }

    /// How many rows memory holds: the present rows, or a tiered store's overlay and other support.
    pub fn resident_len(&self) -> usize {
        match &self.tiered {
            Some(t) => t.overlay.len() + self.counts.len() + t.hot.borrow().rows,
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
        match self.tiered.as_deref() {
            Some(t) if t.counted => return self.add_counted(row, w),
            Some(_) => return self.add_tiered(row, w),
            None => {}
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

    /// A durable view's support (docs/design/DATABASE.md §8): its count as the overlay or the cold side holds it,
    /// moved by `w`; the row shows or hides when it crosses zero.
    fn add_counted(&mut self, row: Row, w: i64) -> ExprResult<()> {
        let t = self
            .tiered
            .as_deref_mut()
            .ok_or_else(|| bug("a counted add to a store that is not tiered".into()))?;
        let before = t.support(&row).map_err(ExprError::Eval)?;
        let after = i64::try_from(before)
            .ok()
            .and_then(|b| b.checked_add(w))
            .and_then(|a| u64::try_from(a).ok())
            .ok_or_else(|| bug(format!("the support of {row:?} went negative")))?;
        t.changed.entry(row.clone()).or_insert(before);
        let now = t.now;
        t.set_overlay(&row, after, now);
        // Support kept in memory only (a mixed relation's other rules') keeps the row present either way.
        let other = self.counts.contains_key(&row);
        match (before > 0 || other, after > 0 || other) {
            (false, true) => self.show(row),
            (true, false) => self.hide(&row),
            _ => {}
        }
        Ok(())
    }

    /// A tiered table's row as an earlier version held it (`present`), in the overlay, until a carry replaces the
    /// entry: the restart's catch-up reads the tables as of the tick before the last one (DATABASE.md §8).
    pub fn rewind(&mut self, row: &Row, present: bool) -> Result<(), EvalError> {
        let t = self
            .tiered
            .as_deref_mut()
            .ok_or_else(|| internal_error!("a rewind of a store that is not tiered"))?;
        let was = t.carried(row)?;
        t.set_overlay(row, u64::from(present), u64::MAX);
        match (was, present) {
            (false, true) => t.len += 1,
            (true, false) => t.len = t.len.saturating_sub(1),
            _ => {}
        }
        Ok(())
    }

    /// A durable view's rows whose support changed since they were last taken: each with its support before and now.
    pub fn take_changes(&mut self) -> Result<Vec<(Row, u64, u64)>, EvalError> {
        let Some(t) = self.tiered.as_deref_mut() else {
            return Ok(Vec::new());
        };
        let changed = std::mem::take(&mut t.changed);
        let mut out = Vec::with_capacity(changed.len());
        for (row, before) in changed {
            let now = t.support(&row)?;
            if now != before {
                out.push((row, before, now));
            }
        }
        Ok(out)
    }

    /// Sets the store's rows and change directly (the restart's catch-up: a tiered table's net change since the
    /// views' version, DATABASE.md §8).
    pub fn set_change(&mut self, ins: BTreeSet<Row>, del: BTreeSet<Row>) {
        self.touched = !ins.is_empty() || !del.is_empty();
        self.generation = self.generation.wrapping_add(1);
        self.ins = ins;
        self.del = del;
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
        // A row that shows or hides here is not carried (a carried row is present either way).
        if let Some(t) = self.tiered.as_deref_mut() {
            match (was, is) {
                (false, true) => {
                    t.uncarried.insert(row.clone());
                }
                (true, false) => {
                    t.uncarried.remove(&row);
                }
                _ => {}
            }
        }
        match (was, is) {
            (false, true) => self.show(row),
            (true, false) => self.hide(&row),
            _ => {}
        }
        Ok(())
    }

    /// A tiered table's rows present this tick that were not carried into it (DATABASE.md §8); none for another
    /// store.
    pub fn uncarried(&self) -> Vec<Row> {
        self.tiered
            .as_deref()
            .map(|t| t.uncarried.iter().cloned().collect())
            .unwrap_or_default()
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
        t.set_overlay(row, u64::from(present), tick);
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
    pub fn begin_tick(&mut self, tick: u64) -> Result<(), EvalError> {
        let Some(t) = self.tiered.as_deref_mut() else {
            return Ok(());
        };
        t.now = tick;
        t.uncarried.clear();
        t.first = std::mem::take(&mut t.fresh);
        if t.first {
            // Every row is new to the rules (their stores start empty after a reset): read whole, this once.
            let mut ins: BTreeSet<Row> = t.carried_rows(&[], &[], None, |_| true)?.into_iter().collect();
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
                    if let Some((support, _)) = t.overlay.remove(&row) {
                        t.hot.get_mut().caught_up(&row, support);
                    }
                    t.index_overlay(&row, false);
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

    /// A tiered table: whether `row` is carried into this tick, its next state as the last tick left it (a tiered
    /// store takes each tick's change at that tick's end, and nothing changes its carry within a tick). In the first
    /// tick after a reset that showed all its rows as new, the cold side's. `None`: not a tiered table.
    pub fn carried_in(&self, row: &Row) -> Result<Option<bool>, EvalError> {
        match &self.tiered {
            Some(t) if t.first => t.cold_carried(row).map(Some),
            Some(t) if !t.counted => t.carried(row).map(Some),
            _ => Ok(None),
        }
    }

    pub fn contains(&self, row: &Row) -> Result<bool, EvalError> {
        if let Some(t) = &self.tiered {
            return Ok((!self.counts.is_empty() && self.counts.contains_key(row)) || t.carried(row)?);
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

    /// About how many rows a probe on `cols` finds, with a range on one more column (a small fraction of the probe's
    /// rows, assumed) or not. A tiered store answers from what its probes found ([`Tiered`]).
    pub fn estimate_probe(&self, cols: &[usize], range: bool) -> usize {
        if let Some(t) = &self.tiered {
            return t.estimate(cols, range);
        }
        let rows = self.estimate(cols);
        if range { rows / 16 + 1 } else { rows }
    }

    /// The store's size as the planner's cached join orders see it: the power of two of its rows (stores below
    /// `small` rows count as one size), and for a tiered store the epoch of its estimates.
    pub fn size_class(&self, small: usize) -> u64 {
        let bits = u64::from((usize::BITS - self.present_len().leading_zeros()).max(small.trailing_zeros()));
        match &self.tiered {
            Some(t) => bits | (t.epoch.get() << 8),
            None => bits,
        }
    }

    /// About how many present rows match a probe on `cols`: all of them, or the average bucket of the index.
    pub fn estimate(&self, cols: &[usize]) -> usize {
        let n = self.present_len();
        if cols.is_empty() {
            return n;
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
            // Support other than the carry (rare): its rows, those not carried too.
            if !self.counts.is_empty() {
                let carried: BTreeSet<Row> = out.iter().cloned().collect();
                out.extend(
                    self.counts
                        .keys()
                        .filter(|r| matches(r) && !carried.contains(*r))
                        .cloned(),
                );
            }
            return Ok(out);
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
        if let Some(t) = &self.tiered {
            let matches = |r: &Row| holds(r, cols, values);
            let live = |r: &Row| !old || !self.ins.contains(r);
            let excluded = if old { self.ins.len() } else { 0 };
            return Ok((old && self.del.iter().any(matches))
                || self.counts.keys().any(|r| matches(r) && live(r))
                || t.any_carried(cols, values, excluded, live)?);
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
        // Support other than the carry (rare): its rows, those not carried too.
        if !self.counts.is_empty() {
            let carried: BTreeSet<Row> = out.iter().cloned().collect();
            out.extend(
                self.counts
                    .keys()
                    .filter(|r| within(r) && !carried.contains(*r))
                    .cloned(),
            );
        }
        // At the start of the tick: without the rows the tick inserted, with those it deleted (present before, and
        // so neither carried nor supported now).
        if old {
            if !self.ins.is_empty() {
                out.retain(|r| !self.ins.contains(r));
            }
            out.extend(self.del.iter().filter(|r| within(r)).cloned());
        }
        Ok(out)
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
