//! The cold side of the engine's tiered tables (docs/design/DATABASE.md §7): a durable table's rows as of a version,
//! kept by the node's database rather than in the engine's memory. The node implements [`ColdTables`] over its
//! database; [`crate::Engine::reset_on`] hands it to the engine, whose tiered stores keep in memory only what the
//! database does not hold yet.

use std::ops::Bound;

use blossom_base::RelId;
use blossom_ir::tick::{EvalError, Row};
use blossom_value::Value;

/// A probe's range on one more column: the column, and its bounds.
pub type ColRange<'a> = (usize, Bound<&'a Value>, Bound<&'a Value>);

/// Durable tables' rows by version. Every read names its version, which is at most [`ColdTables::version`] and no
/// older than the history the source keeps.
pub trait ColdTables: Send + Sync {
    /// The newest version the rows are at (`None`: none yet, every table empty).
    fn version(&self) -> Result<Option<u64>, EvalError>;
    /// The tables it holds: the program's durable relations.
    fn tables(&self) -> Vec<RelId>;
    /// Whether `rel` holds `row` as of `at`.
    fn contains(&self, rel: RelId, row: &Row, at: u64) -> Result<bool, EvalError>;
    /// `row`'s support in `rel` as of `at`: for a table, 1 if it holds the row, else 0; for a durable view
    /// (docs/design/DATABASE.md §8), its count of derivations.
    fn support(&self, rel: RelId, row: &Row, at: u64) -> Result<u64, EvalError> {
        Ok(u64::from(self.contains(rel, row, at)?))
    }
    /// Opens the keyspaces of the program's durable views (each with its definition's hash) for this run: `true` if
    /// every one is complete there (its rows as of the views' version, DATABASE.md §8) and `resume` (the caller can
    /// catch them up); else every view starts empty in a keyspace of its own, and is complete once the first tick
    /// writes it.
    fn open_views(&self, views: &[(RelId, [u8; 32])], resume: bool) -> Result<bool, EvalError>;
    /// The rows of `rel` as of `at` whose columns `cols` hold `values` and, with `range`, whose column lies within
    /// its bounds. No columns and no range: every row.
    fn probe(
        &self,
        rel: RelId,
        cols: &[usize],
        values: &[Value],
        range: Option<ColRange<'_>>,
        at: u64,
    ) -> Result<Vec<Row>, EvalError>;
    /// How many rows `rel` holds as of `at`.
    fn count(&self, rel: RelId, at: u64) -> Result<usize, EvalError>;
    /// The first `n` rows (in the source's order) of `rel` as of `at` whose columns `cols` hold `values`, and whether
    /// there are more: an existence check that reads no more than it needs.
    fn probe_some(
        &self,
        rel: RelId,
        cols: &[usize],
        values: &[Value],
        at: u64,
        n: usize,
    ) -> Result<(Vec<Row>, bool), EvalError> {
        let mut rows = self.probe(rel, cols, values, None, at)?;
        let more = rows.len() > n;
        rows.truncate(n);
        Ok((rows, more))
    }
    /// The rows of `rel` as of `at` whose columns `cols` hold `values`, if there are at most `max` (`None`: more,
    /// found without reading them all).
    fn probe_at_most(
        &self,
        rel: RelId,
        cols: &[usize],
        values: &[Value],
        at: u64,
        max: usize,
    ) -> Result<Option<Vec<Row>>, EvalError> {
        let rows = self.probe(rel, cols, values, None, at)?;
        Ok((rows.len() <= max).then_some(rows))
    }
}

/// How an engine resumes on the cold side ([`crate::Engine::reset_on`]): the deployment's static rows (fed as each
/// tick's events; they name the views' definitions with the program's), and the catch-up of the durable views if
/// the caller has one (the WAL's ticks since the database's views, docs/design/DATABASE.md §8; `None`: the views are
/// built again).
#[derive(Clone, Debug, Default)]
pub struct Resume {
    pub statics: Vec<(RelId, Row)>,
    pub catch_up: Option<blossom_ir::tick::CatchUp>,
}
