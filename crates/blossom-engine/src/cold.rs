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
}
