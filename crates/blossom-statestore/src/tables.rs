//! Durable relations as tables (docs/design/SQL-TABLES.md §2, §5): what a store with SQL tables offers a node's
//! database. Every table is a **history table** of keys: each row a key (bytes), its owner (the object's `node` and
//! `member`), the ticks it was present from and until, and typed columns for SQL readers. A table may have a **view**
//! of its current rows. The store knows nothing of Blossom's values: it works in keys, ticks and [`SqlValue`]s.

use crate::{Commit, StateError, StateStore, Write};

/// A column's SQL type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SqlType {
    Bool,
    /// A 64-bit signed integer.
    Int,
    /// An exact decimal (a `u64`, `u128`, `i128`, `Mod<N>`).
    Numeric,
    Real,
    Text,
    Bytes,
    Json,
}

/// A column's value as SQL holds it.
#[derive(Clone, Debug, PartialEq)]
pub enum SqlValue {
    Null,
    Bool(bool),
    Int(i64),
    /// Decimal digits, with a leading `-` when negative.
    Numeric(String),
    Real(f64),
    Text(String),
    Bytes(Vec<u8>),
    /// A JSON document's text.
    Json(String),
}

/// A table: its name, the name of the view of its current rows (none for an internal table), and its typed columns.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TableDef {
    pub name: String,
    pub view: Option<String>,
    pub columns: Vec<(String, SqlType)>,
}

/// The columns every history table has; a typed column of one of these names is refused ([`check_table`]).
pub const SYSTEM_COLUMNS: [&str; 5] = ["node", "member", "key", "from_tick", "to_tick"];

/// The names the stores keep for themselves: a table or view may not take one, or start with `blossom_` unless it
/// is the internal keyspace table.
pub const KEYS_TABLE: &str = "blossom_keys";

/// Whether `name` is an identifier the stores take: ASCII letters, digits and `_`, not starting with a digit, at most
/// 63 bytes (Postgres's limit).
pub fn check_ident(what: &str, name: &str) -> Result<(), StateError> {
    let ok = !name.is_empty()
        && name.len() <= 63
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !name.as_bytes().first().is_some_and(u8::is_ascii_digit);
    if ok {
        Ok(())
    } else {
        Err(StateError::Invalid(format!(
            "`{name}` cannot be a {what} (ASCII letters, digits and _, not first a digit, at most 63 bytes)"
        )))
    }
}

/// Checks a table's names: identifiers, no column named as a system column, no name a store keeps for itself.
pub fn check_table(t: &TableDef) -> Result<(), StateError> {
    check_ident("table name", &t.name)?;
    if t.name.starts_with("blossom_") && t.name != KEYS_TABLE {
        return Err(StateError::Invalid(format!(
            "the table name `{}` is the stores' own",
            t.name
        )));
    }
    if let Some(v) = &t.view {
        check_ident("view name", v)?;
        if v.starts_with("blossom_") {
            return Err(StateError::Invalid(format!("the view name `{v}` is the stores' own")));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for (c, _) in &t.columns {
        check_ident("column name", c)?;
        if SYSTEM_COLUMNS.contains(&c.as_str()) {
            return Err(StateError::Invalid(format!(
                "the column `{c}` of `{}` is a system column's name",
                t.name
            )));
        }
        if !seen.insert(c.as_str()) {
            return Err(StateError::Invalid(format!("the column `{c}` twice in `{}`", t.name)));
        }
    }
    Ok(())
}

/// Whose rows: an object's node, and its keyed member's key (empty for a node's own rows).
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Owner {
    pub node: String,
    pub member: String,
}

/// One change to a history table, by a commit of the owner's object.
#[derive(Clone, Debug, PartialEq)]
pub enum RowChange {
    /// `key` is present from tick `from` (a new row), with its typed columns' values in the table's order.
    Open {
        table: String,
        key: Vec<u8>,
        from: u64,
        values: Vec<SqlValue>,
    },
    /// The open row of `key` ends at tick `at` (it is absent from then).
    Close { table: String, key: Vec<u8>, at: u64 },
}

/// A store whose objects' durable relations are tables (docs/design/SQL-TABLES.md §5).
pub trait TableStore: StateStore {
    /// Creates what is missing of `tables` (and [`KEYS_TABLE`]), their views (replaced: a view follows the deployed
    /// program) and the catalog, for the deployment `deployment`. A store serves one deployment: another's tables
    /// are refused.
    fn ensure_tables(&self, deployment: &str, tables: &[TableDef]) -> Result<(), StateError>;
    /// The keys of `owner` in `table` from `lo` (inclusive) to `hi` (exclusive; `None`: to the end) present at tick
    /// `at`, in byte order, at most `limit`.
    fn scan_keys(
        &self,
        table: &str,
        owner: &Owner,
        lo: &[u8],
        hi: Option<&[u8]>,
        at: u64,
        limit: usize,
    ) -> Result<Vec<Vec<u8>>, StateError>;
    /// Whether `key` of `owner` is present in `table` at tick `at`.
    fn has_key(&self, table: &str, owner: &Owner, key: &[u8], at: u64) -> Result<bool, StateError>;
    /// [`StateStore::commit`] with `rows` (all `owner`'s) applied in the same transaction, in order; and, with
    /// `prune_below`, `owner`'s rows that ended at or before that tick deleted (the history kept starts there).
    fn commit_rows(
        &self,
        object: &str,
        expected: u64,
        writes: &[Write],
        owner: &Owner,
        rows: &[RowChange],
        prune_below: Option<u64>,
    ) -> Result<Commit, StateError>;
}
