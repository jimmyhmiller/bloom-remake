//! Durable relations as SQL tables (docs/design/SQL-TABLES.md): a node's database's [`KeyTree`] over a state store's
//! tables, for an object of a stateless host.
//!
//! [`TableMap`] is the deployment's tables: each durable relation's history table (its typed columns from the
//! relation's column types, §3) and current-rows view, and how a row's key becomes its typed values. [`SqlTree`] is
//! one object's tree: its reads are the store's (as of the object's committed version, so another instance's newer
//! rows never show in a request that will lose anyway), merged with the request's **pending** changes, which go to
//! SQL only with the request's commit ([`SqlTree::changes`], then [`SqlTree::committed`]).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use blossom_base::RelId;
use blossom_base::TypeId;
use blossom_ir::ValidatedProgram;
use blossom_node::durable::{DurableCodec, DurableSchema};
use blossom_statestore::tables::{KEYS_TABLE, SYSTEM_COLUMNS};
use blossom_statestore::{Owner, RowChange, SqlType, SqlValue, StateError, StateStore, TableDef};
use blossom_store::StoreError;
use blossom_store::lsm::{Flushed, Op, Page, TreeInfo};
use blossom_store::tree::KeyTree;
use blossom_value::types::{IntTy, TypeDef};
use blossom_value::value::IntValue;
use blossom_value::{TypeTable, Value};

use crate::RuntimeError;

/// How far the history floor rises before a commit prunes what fell below it.
const PRUNE_STEP: u64 = 1024;

/// A relation's table.
#[derive(Clone, Debug)]
struct RelTable {
    rel: RelId,
    name: String,
    /// The type of each column.
    types: Vec<TypeId>,
    sql: Vec<SqlType>,
}

/// The deployment's tables (docs/design/SQL-TABLES.md §2).
pub struct TableMap {
    program: ValidatedProgram,
    schema: DurableSchema,
    names: Arc<[Arc<str>]>,
    by_tag: BTreeMap<[u8; 8], RelTable>,
    defs: Vec<TableDef>,
}

/// An identifier from a Blossom name: ASCII letters, digits and `_`, every other byte `_`, not first a digit.
fn ident(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if out.is_empty() || out.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        out.insert(0, '_');
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The SQL type a column of type `ty` takes (§3).
fn sql_type(types: &TypeTable, ty: TypeId) -> SqlType {
    match types.get(ty) {
        Some(TypeDef::Bool) => SqlType::Bool,
        Some(TypeDef::Int(IntTy::U64 | IntTy::U128 | IntTy::I128)) | Some(TypeDef::Mod { .. }) => SqlType::Numeric,
        Some(TypeDef::Int(_)) | Some(TypeDef::Duration) | Some(TypeDef::Instant) => SqlType::Int,
        Some(TypeDef::F64) => SqlType::Real,
        Some(TypeDef::Str | TypeDef::Principal | TypeDef::Node(_) | TypeDef::Blob) => SqlType::Text,
        Some(TypeDef::Bytes) => SqlType::Bytes,
        _ => SqlType::Json,
    }
}

impl TableMap {
    /// The tables of `program`'s durable relations, for a deployment whose node names are `names`.
    pub fn of(program: &ValidatedProgram, names: Arc<[Arc<str>]>) -> Result<TableMap, RuntimeError> {
        let p = program.get();
        let schema = DurableSchema::of(p);
        let codec = DurableCodec::new(p, &schema, names.clone());
        let mut by_tag = BTreeMap::new();
        let mut defs = Vec::new();
        let mut views = std::collections::BTreeSet::new();
        for (rel, _, _) in &schema.rels {
            let tag = codec.rel_tag(*rel)?;
            let decl = p
                .rels
                .get(*rel)
                .ok_or_else(|| blossom_base::internal_error!("no relation {rel:?}"))?;
            // A client role's durable tables live in its pages (their storage), never at a node: no table.
            if let blossom_ir::core::Placement::Role(r) = decl.placement
                && p.roles
                    .get(r)
                    .is_some_and(|r| r.kind == blossom_ir::core::RoleKind::Client)
            {
                continue;
            }
            let base = ident(&decl.name.to_string());
            let short: String = base.chars().take(40).collect();
            let name = format!("r_{short}_{}", hex(&tag));
            let mut view = if base.starts_with("blossom_") {
                format!("rel_{base}")
            } else {
                base
            };
            if view.len() > 63 || !views.insert(view.clone()) {
                // Two relations whose names make one identifier: the second's view carries its tag.
                view = format!("{}_{}", view.chars().take(44).collect::<String>(), hex(&tag));
                views.insert(view.clone());
            }
            let mut columns: Vec<(String, SqlType)> = Vec::new();
            let mut types = Vec::new();
            let mut sql = Vec::new();
            for (i, col) in decl.schema.cols.iter().enumerate() {
                let mut c = ident(&col.name.to_string());
                while SYSTEM_COLUMNS.contains(&c.as_str()) || columns.iter().any(|(n, _)| *n == c) {
                    c.push('_');
                }
                if c.len() > 63 {
                    c = format!("c{i}");
                }
                let t = sql_type(&p.types, col.ty);
                columns.push((c, t));
                types.push(col.ty);
                sql.push(t);
            }
            defs.push(TableDef {
                name: name.clone(),
                view: Some(view),
                columns,
            });
            by_tag.insert(
                tag,
                RelTable {
                    rel: *rel,
                    name,
                    types,
                    sql,
                },
            );
        }
        drop(codec);
        Ok(TableMap {
            program: program.clone(),
            schema,
            names,
            by_tag,
            defs,
        })
    }

    /// The tables, to create.
    pub fn defs(&self) -> &[TableDef] {
        &self.defs
    }

    /// The table a key goes to: its relation's, or the internal one.
    fn route(&self, key: &[u8]) -> Option<&RelTable> {
        let tag: [u8; 8] = key.get(..8)?.try_into().ok()?;
        self.by_tag.get(&tag)
    }

    fn table_of(&self, key: &[u8]) -> &str {
        self.route(key).map_or(KEYS_TABLE, |t| t.name.as_str())
    }

    /// The tables a key range may hold keys of: one relation's, when the range is within its tag; else every table
    /// whose keys the range reaches.
    fn tables_for(&self, start: &[u8], end: Option<&[u8]>) -> Vec<&str> {
        if let Some(t) = self.route(start) {
            let tag = start.get(..8).unwrap_or_default();
            let tag_end = blossom_node::keycode::successor(tag);
            if let (Some(e), Some(te)) = (end, tag_end.as_deref())
                && e <= te
            {
                return vec![t.name.as_str()];
            }
        }
        let mut out = vec![KEYS_TABLE];
        for (tag, t) in &self.by_tag {
            let tag_end = blossom_node::keycode::successor(tag);
            let before_end = end.is_none_or(|e| tag.as_slice() < e);
            let after_start = tag_end.as_deref().is_none_or(|te| start < te);
            if before_end && after_start {
                out.push(t.name.as_str());
            }
        }
        out
    }

    /// A relation row's typed values, from its key.
    fn values(&self, t: &RelTable, key: &[u8]) -> Result<Vec<SqlValue>, RuntimeError> {
        let p = self.program.get();
        let codec = DurableCodec::new(p, &self.schema, self.names.clone());
        let row = codec.key_row(t.rel, key)?;
        let node = |n: blossom_value::time::NodeId| blossom_ir::printer::node_text(n, &self.names);
        row.iter()
            .zip(t.types.iter().zip(&t.sql))
            .map(|(v, (ty, sql))| sql_value(p, v, *ty, *sql, &node))
            .collect()
    }
}

/// A value as its column holds it (§3). A value of another shape than its column's type is a bug (the type
/// checker holds them apart), refused.
fn sql_value(
    p: &blossom_ir::core::Program,
    v: &Value,
    ty: TypeId,
    sql: SqlType,
    node: &dyn Fn(blossom_value::time::NodeId) -> String,
) -> Result<SqlValue, RuntimeError> {
    Ok(match (sql, v) {
        (SqlType::Bool, Value::Bool(b)) => SqlValue::Bool(*b),
        (SqlType::Int, Value::Int(i)) => SqlValue::Int(
            i64::try_from(int_i128(i))
                .map_err(|_| blossom_base::internal_error!("an integer of a 64-bit column that does not fit one"))?,
        ),
        (SqlType::Int, Value::Duration(d)) => SqlValue::Int(d.0),
        (SqlType::Int, Value::Instant(i)) => SqlValue::Int(i.0),
        (SqlType::Numeric, Value::Int(i)) => SqlValue::Numeric(int_text(i)),
        (SqlType::Numeric, Value::Mod(m)) => SqlValue::Numeric(m.to_string()),
        (SqlType::Real, Value::F64(x)) => SqlValue::Real(*x),
        (SqlType::Text, Value::Str(s) | Value::Principal(s)) => SqlValue::Text(s.to_string()),
        // Nodes and members as Blossom writes them: `rooms`, `Browser#3@rooms`, `Room:"lunch"`.
        (SqlType::Text, Value::Node(_) | Value::Member(_)) => {
            SqlValue::Text(blossom_ir::printer::value_text(Some(p), v, Some(ty), node))
        }
        (SqlType::Text, Value::Blob(b)) => SqlValue::Text(format!("{}:{}", b.hex(), b.len)),
        (SqlType::Bytes, Value::Bytes(b)) => SqlValue::Bytes(b.to_vec()),
        (SqlType::Json, _) => SqlValue::Json(json(p, v, Some(ty), node)),
        (sql, v) => {
            return Err(blossom_base::internal_error!("a value {v:?} in a column of SQL type {sql:?}").into());
        }
    })
}

fn int_i128(i: &IntValue) -> i128 {
    match *i {
        IntValue::U8(n) => n.into(),
        IntValue::U16(n) => n.into(),
        IntValue::U32(n) => n.into(),
        IntValue::U64(n) => n.into(),
        IntValue::U128(n) => i128::try_from(n).unwrap_or(i128::MAX),
        IntValue::I8(n) => n.into(),
        IntValue::I16(n) => n.into(),
        IntValue::I32(n) => n.into(),
        IntValue::I64(n) => n.into(),
        IntValue::I128(n) => n,
    }
}

fn int_text(i: &IntValue) -> String {
    match *i {
        IntValue::U128(n) => n.to_string(),
        _ => int_i128(i).to_string(),
    }
}

fn json_str(s: &str) -> String {
    serde_json::Value::String(s.to_owned()).to_string()
}

/// A value as JSON (deterministic): structs as objects by field name, enums as `{"variant": NAME, "fields": …}`,
/// options as `null` or their value, sequences and sets as arrays, maps as arrays of `[key, value]`, integers as
/// numbers when exact in a double (else strings), and what has no JSON shape of its own as its Blossom text.
fn json(
    p: &blossom_ir::core::Program,
    v: &Value,
    ty: Option<TypeId>,
    node: &dyn Fn(blossom_value::time::NodeId) -> String,
) -> String {
    let def = ty.and_then(|t| p.types.get(t));
    let arr = |items: Vec<String>| format!("[{}]", items.join(","));
    match v {
        Value::Unit => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => {
            let n = int_i128(i);
            if n.unsigned_abs() <= (1u128 << 53) && !matches!(i, IntValue::U128(x) if *x > i128::MAX as u128) {
                n.to_string()
            } else {
                json_str(&int_text(i))
            }
        }
        Value::F64(x) if x.is_finite() => serde_json::Value::from(*x).to_string(),
        Value::F64(x) => json_str(&x.to_string()),
        Value::Str(s) | Value::Principal(s) => json_str(s),
        Value::Bytes(b) => json_str(&hex(b)),
        Value::Duration(d) => d.0.to_string(),
        Value::Instant(i) => i.0.to_string(),
        Value::Node(_) | Value::Member(_) => json_str(&blossom_ir::printer::value_text(Some(p), v, ty, node)),
        Value::Blob(b) => json_str(&format!("{}:{}", b.hex(), b.len)),
        Value::Option(None) => "null".into(),
        Value::Option(Some(x)) => {
            let inner = match def {
                Some(TypeDef::Option(t)) => Some(*t),
                _ => None,
            };
            json(p, x, inner, node)
        }
        Value::Tuple(xs) => {
            let tys: Vec<Option<TypeId>> = match def {
                Some(TypeDef::Tuple(ts)) => ts.iter().map(|t| Some(*t)).collect(),
                _ => vec![None; xs.len()],
            };
            arr(xs
                .iter()
                .zip(tys.into_iter().chain(std::iter::repeat(None)))
                .map(|(x, t)| json(p, x, t, node))
                .collect())
        }
        Value::Struct(xs) => match def {
            Some(TypeDef::Struct(s)) if s.fields.len() == xs.len() => {
                let fields: Vec<String> = s
                    .fields
                    .iter()
                    .zip(xs.iter())
                    .map(|(f, x)| format!("{}:{}", json_str(&f.name.to_string()), json(p, x, Some(f.ty), node)))
                    .collect();
                format!("{{{}}}", fields.join(","))
            }
            _ => arr(xs.iter().map(|x| json(p, x, None, node)).collect()),
        },
        Value::Enum { variant, fields } => {
            let vd = match def {
                Some(TypeDef::Enum(e)) => e.variants.iter().find(|v| v.number == *variant),
                _ => None,
            };
            let name = vd.map_or_else(|| variant.to_string(), |v| v.name.to_string());
            let body = match vd {
                Some(v) if v.payload.len() == fields.len() => {
                    let fs: Vec<String> = v
                        .payload
                        .iter()
                        .zip(fields.iter())
                        .map(|(f, x)| format!("{}:{}", json_str(&f.name.to_string()), json(p, x, Some(f.ty), node)))
                        .collect();
                    format!("{{{}}}", fs.join(","))
                }
                _ => arr(fields.iter().map(|x| json(p, x, None, node)).collect()),
            };
            format!("{{\"variant\":{},\"fields\":{body}}}", json_str(&name))
        }
        Value::Vec(xs) => {
            let t = match def {
                Some(TypeDef::Vec(t)) => Some(*t),
                _ => None,
            };
            arr(xs.iter().map(|x| json(p, x, t, node)).collect())
        }
        Value::Set(xs) => {
            let t = match def {
                Some(TypeDef::Set(t)) => Some(*t),
                _ => None,
            };
            arr(xs.iter().map(|x| json(p, x, t, node)).collect())
        }
        Value::Map(m) => {
            let (kt, vt) = match def {
                Some(TypeDef::Map(k, v)) => (Some(*k), Some(*v)),
                _ => (None, None),
            };
            arr(m
                .iter()
                .map(|(k, x)| format!("[{},{}]", json(p, k, kt, node), json(p, x, vt, node)))
                .collect())
        }
        other => json_str(&blossom_ir::printer::value_text(Some(p), other, ty, node)),
    }
}

/// What an object's tree keeps in its entries between requests (`T/meta`).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TreeMeta {
    pub format: Option<u32>,
    /// The newest version committed, and its mark.
    pub applied: Option<(u64, u64)>,
    /// The version and mark a flush covered.
    pub flushed: (Option<u64>, u64),
    pub floor: u64,
    /// The floor the last prune went to.
    pub pruned: u64,
}

/// The entry an object keeps its tree's meta in.
pub const TREE_META_KEY: &str = "T/meta";

#[derive(Default)]
struct TreeState {
    committed: TreeMeta,
    applied: Option<(u64, u64)>,
    flushed: Flushed,
    floor: u64,
    format: Option<u32>,
    /// The request's changes, not committed: key → version → present.
    pending: BTreeMap<Vec<u8>, BTreeMap<u64, bool>>,
}

/// One object's tree over a state store's tables (see the module's documentation). Clones share it: the host keeps
/// one to commit what the node's database applied to another.
#[derive(Clone)]
pub struct SqlTree {
    inner: Arc<TreeInner>,
}

struct TreeInner {
    store: Arc<dyn StateStore>,
    owner: Owner,
    map: Arc<TableMap>,
    given_format: u32,
    state: Mutex<TreeState>,
}

fn store_err(e: StateError) -> StoreError {
    StoreError::Invalid(format!("the state store's tables: {e}"))
}

fn poisoned() -> StoreError {
    StoreError::Invalid("the SQL tree's lock is poisoned".into())
}

impl SqlTree {
    /// The tree of the object `owner` names, as its entries' `meta` left it; keys of format `format` from its first
    /// version.
    pub fn new(store: Arc<dyn StateStore>, owner: Owner, map: Arc<TableMap>, meta: TreeMeta, format: u32) -> SqlTree {
        let state = TreeState {
            applied: meta.applied,
            flushed: Flushed::at(meta.flushed.0, meta.flushed.1),
            floor: meta.floor,
            format: meta.format,
            committed: meta,
            pending: BTreeMap::new(),
        };
        SqlTree {
            inner: Arc::new(TreeInner {
                store,
                owner,
                map,
                given_format: format,
                state: Mutex::new(state),
            }),
        }
    }

    /// Whose rows it holds.
    pub fn owner(&self) -> &Owner {
        &self.inner.owner
    }

    fn state(&self) -> Result<MutexGuard<'_, TreeState>, StoreError> {
        self.inner.state.lock().map_err(|_| poisoned())
    }

    fn tables(&self) -> Result<&dyn blossom_statestore::TableStore, StoreError> {
        self.inner
            .store
            .tables()
            .ok_or_else(|| StoreError::Invalid("the state store has no tables".into()))
    }

    /// The request's changes as row changes for its commit, and the floor to prune below (when it rose far enough
    /// since the last prune); and the meta the tree has once they commit.
    pub fn changes(&self) -> Result<(Vec<RowChange>, Option<u64>, TreeMeta), RuntimeError> {
        let s = self.state()?;
        let base = s.committed.applied.map(|(v, _)| v);
        let tables = self.tables()?;
        let mut rows = Vec::new();
        for (key, ops) in &s.pending {
            let table = self.inner.map.table_of(key).to_owned();
            let mut present = match base {
                Some(b) => tables.has_key(&table, &self.inner.owner, key, b).map_err(store_err)?,
                None => false,
            };
            for (version, put) in ops {
                if *put == present {
                    continue;
                }
                present = *put;
                if *put {
                    let values = match self.inner.map.route(key) {
                        Some(t) => self.inner.map.values(t, key)?,
                        None => Vec::new(),
                    };
                    rows.push(RowChange::Open {
                        table: table.clone(),
                        key: key.clone(),
                        from: *version,
                        values,
                    });
                } else {
                    rows.push(RowChange::Close {
                        table: table.clone(),
                        key: key.clone(),
                        at: *version,
                    });
                }
            }
        }
        let prune = (s.floor >= s.committed.pruned.saturating_add(PRUNE_STEP)).then_some(s.floor);
        let meta = TreeMeta {
            format: s.format,
            applied: s.applied,
            flushed: (s.flushed.version(), s.flushed.mark()),
            floor: s.floor,
            pruned: prune.unwrap_or(s.committed.pruned),
        };
        Ok((rows, prune, meta))
    }

    /// The request committed with `meta` (what [`SqlTree::changes`] gave): its changes are the store's now.
    pub fn committed(&self, meta: TreeMeta) -> Result<(), RuntimeError> {
        let mut s = self.state()?;
        s.pending.clear();
        s.committed = meta;
        Ok(())
    }

    /// The version the store's rows are read at for a read at `as_of`: no newer than what this object committed.
    fn read_at(s: &TreeState, as_of: u64) -> Option<u64> {
        s.committed.applied.map(|(v, _)| v.min(as_of))
    }

    fn readable(s: &TreeState, as_of: u64) -> Result<(), StoreError> {
        let newest = s.applied.map(|(v, _)| v);
        if as_of < s.floor || newest.is_some_and(|n| as_of > n) {
            return Err(StoreError::Invalid(format!(
                "version {as_of} is outside the history kept (from version {})",
                s.floor
            )));
        }
        Ok(())
    }

    /// The pending say about `key` as of `as_of`, if it says anything.
    fn pending_at(s: &TreeState, key: &[u8], as_of: u64) -> Option<bool> {
        s.pending
            .get(key)
            .and_then(|ops| ops.range(..=as_of).next_back().map(|(_, p)| *p))
    }
}

impl KeyTree for SqlTree {
    fn key_format(&self) -> Result<Option<u32>, StoreError> {
        Ok(self.state()?.format)
    }

    fn applied(&self) -> Result<Option<u64>, StoreError> {
        Ok(self.state()?.applied.map(|(v, _)| v))
    }

    fn apply(&self, version: u64, mark: u64, changes: Vec<(Vec<u8>, Op)>) -> Result<(), StoreError> {
        let mut s = self.state()?;
        if let Some((a, _)) = s.applied
            && version <= a
        {
            return Err(StoreError::Invalid(format!(
                "version {version} applied after version {a}"
            )));
        }
        for (k, op) in changes {
            s.pending.entry(k).or_default().insert(version, op == Op::Put);
        }
        s.applied = Some((version, mark));
        if s.format.is_none() {
            s.format = Some(self.inner.given_format);
        }
        Ok(())
    }

    fn amend(&self, changes: Vec<(Vec<u8>, Op)>) -> Result<(), StoreError> {
        let mut s = self.state()?;
        let (version, _) = s
            .applied
            .ok_or_else(|| StoreError::Invalid("an amendment of a tree with no version applied".into()))?;
        for (k, op) in changes {
            s.pending.entry(k).or_default().insert(version, op == Op::Put);
        }
        Ok(())
    }

    /// A tree with pending changes wants a flush at once: its rows are as durable as the request's commit, and the
    /// WAL truncates behind it within the request.
    fn needs_flush(&self) -> Result<bool, StoreError> {
        let s = self.state()?;
        Ok(s.applied.map(|(v, _)| v) != s.flushed.version())
    }

    fn flush(&self) -> Result<Flushed, StoreError> {
        let mut s = self.state()?;
        let f = match s.applied {
            Some((v, m)) => Flushed::at(Some(v), m),
            None => Flushed::at(None, 0),
        };
        s.flushed = f;
        Ok(f)
    }

    fn flushed(&self) -> Result<Flushed, StoreError> {
        Ok(self.state()?.flushed)
    }

    fn compact(&self) -> Result<bool, StoreError> {
        Ok(false)
    }

    fn floor(&self) -> Result<u64, StoreError> {
        Ok(self.state()?.floor)
    }

    fn raise_floor(&self, version: u64) -> Result<(), StoreError> {
        let mut s = self.state()?;
        s.floor = s.floor.max(version);
        Ok(())
    }

    fn get(&self, key: &[u8], as_of: u64) -> Result<bool, StoreError> {
        let s = self.state()?;
        Self::readable(&s, as_of)?;
        if let Some(p) = Self::pending_at(&s, key, as_of) {
            return Ok(p);
        }
        match Self::read_at(&s, as_of) {
            Some(at) => self
                .tables()?
                .has_key(self.inner.map.table_of(key), &self.inner.owner, key, at)
                .map_err(store_err),
            None => Ok(false),
        }
    }

    fn scan_page(&self, start: &[u8], end: Option<&[u8]>, as_of: u64, keys: usize) -> Result<Page, StoreError> {
        let n = keys.max(1);
        let s = self.state()?;
        Self::readable(&s, as_of)?;
        // The store's keys, from each table the range reaches: complete up to `bound` (the smallest last key of a
        // table that gave a whole page), and the pending changes merged over them up to there.
        let mut found: Vec<Vec<u8>> = Vec::new();
        let mut bound: Option<Vec<u8>> = None;
        if let Some(at) = Self::read_at(&s, as_of) {
            let tables = self.tables()?;
            for table in self.inner.map.tables_for(start, end) {
                let got = tables
                    .scan_keys(table, &self.inner.owner, start, end, at, n)
                    .map_err(store_err)?;
                if got.len() == n
                    && let Some(last) = got.last()
                    && bound.as_ref().is_none_or(|b| last < b)
                {
                    bound = Some(last.clone());
                }
                found.extend(got);
            }
        }
        let within =
            |k: &[u8]| k >= start && end.is_none_or(|e| k < e) && bound.as_ref().is_none_or(|b| k <= b.as_slice());
        let mut set: std::collections::BTreeSet<Vec<u8>> = found.into_iter().filter(|k| within(k)).collect();
        for (k, ops) in s.pending.range(start.to_vec()..) {
            if !within(k) {
                if end.is_some_and(|e| k.as_slice() >= e) || bound.as_ref().is_some_and(|b| k > b) {
                    break;
                }
                continue;
            }
            match ops.range(..=as_of).next_back() {
                Some((_, true)) => {
                    set.insert(k.clone());
                }
                Some((_, false)) => {
                    set.remove(k);
                }
                None => {}
            }
        }
        let mut all: Vec<Vec<u8>> = set.into_iter().collect();
        if all.len() > n {
            let next = all.get(n).cloned();
            all.truncate(n);
            return Ok(Page { keys: all, next });
        }
        // The page ends at the bound: the next starts just after it.
        let next = bound.map(|mut b| {
            b.push(0);
            b
        });
        Ok(Page { keys: all, next })
    }

    fn info(&self) -> Result<TreeInfo, StoreError> {
        let s = self.state()?;
        Ok(TreeInfo {
            tables: Vec::new(),
            flushed: s.flushed.version(),
            applied: s.applied.map(|(v, _)| v),
            floor: s.floor,
            key_format: s.format.unwrap_or(self.inner.given_format),
            memtable_entries: s.pending.values().map(BTreeMap::len).sum(),
        })
    }
}
