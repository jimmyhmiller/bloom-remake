//! A [`StateStore`] in one SQLite file (docs/design/STATELESS.md §4.1).
//!
//! Every process on one machine that opens the file shares the store: SQLite serializes their commits, and its WAL
//! journal lets readers go on while one commits. A commit is one `BEGIN IMMEDIATE` transaction that checks the
//! object's version, applies the writes and raises the version; with `synchronous = FULL` a commit that returned is on
//! disk. `wait` polls the object's version, every [`POLL`].
//!
//! The schema:
//!
//! ```sql
//! create table blossom_meta    (key text primary key, value integer not null);   -- format = 1
//! create table blossom_objects (object text primary key, version integer not null);
//! create table blossom_entries (object text not null, key text not null, value blob not null, primary key (object, key));
//! create table blossom_wakes   (object text not null, at integer not null, primary key (object, at));
//! create index wakes_at on blossom_wakes (at, object);
//! create table blossom_side    (key text primary key, value blob not null);
//! ```
//!
//! Text compares by bytes (SQLite's `BINARY` collation), so entries come back in the byte order of their keys.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use blossom_statestore::tables::{KEYS_TABLE, check_ident, check_table};
use blossom_statestore::{
    Commit, Owner, RowChange, Snapshot, SqlType, SqlValue, StateError, StateStore, TableDef, TableStore, Write,
    check_commit, check_name,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

/// How often `wait` reads the object's version.
pub const POLL: Duration = Duration::from_millis(20);
/// How long a statement waits for another process's commit to finish.
const BUSY: Duration = Duration::from_secs(30);
/// The schema's format; a file of another is refused.
const FORMAT: i64 = 1;
/// Connections kept for reuse.
const POOL: usize = 8;

/// A state store in an SQLite file.
pub struct SqliteStore {
    path: PathBuf,
    idle: Mutex<Vec<Connection>>,
    /// The tables' definitions this handle knows (from its `ensure_tables`, or read from the catalog).
    defs: Mutex<std::collections::BTreeMap<String, TableDef>>,
}

fn unavailable(e: rusqlite::Error) -> StateError {
    StateError::Unavailable(format!("sqlite: {e}"))
}

/// A number the database stores as `integer` (SQLite's are signed 64-bit).
fn int(what: &str, n: u64) -> Result<i64, StateError> {
    i64::try_from(n).map_err(|_| StateError::Invalid(format!("{what} {n} does not fit SQLite's integers")))
}

fn uint(what: &str, n: i64) -> Result<u64, StateError> {
    u64::try_from(n).map_err(|_| StateError::Corrupt(format!("a negative {what} ({n})")))
}

impl SqliteStore {
    /// Opens (creating it, and its schema, if need be) the store in the file `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<SqliteStore, StateError> {
        let store = SqliteStore {
            path: path.as_ref().to_path_buf(),
            idle: Mutex::new(Vec::new()),
            defs: Mutex::new(std::collections::BTreeMap::new()),
        };
        let mut conn = store.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        tx.execute_batch(
            "create table if not exists blossom_meta (key text primary key, value integer not null);
             create table if not exists blossom_objects (object text primary key, version integer not null);
             create table if not exists blossom_entries (object text not null, key text not null, value blob not null,
                 primary key (object, key));
             create table if not exists blossom_wakes (object text not null, at integer not null, primary key (object, at));
             create index if not exists wakes_at on blossom_wakes (at, object);
             create table if not exists blossom_side (key text primary key, value blob not null);
             create table if not exists blossom_catalog (key text primary key, value text not null);",
        )
        .map_err(unavailable)?;
        let format: Option<i64> = tx
            .query_row("select value from blossom_meta where key = 'format'", [], |r| r.get(0))
            .optional()
            .map_err(unavailable)?;
        match format {
            None => {
                tx.execute("insert into blossom_meta (key, value) values ('format', ?1)", [FORMAT])
                    .map_err(unavailable)?;
            }
            Some(FORMAT) => {}
            Some(other) => {
                return Err(StateError::Config(format!(
                    "{} holds a state store of format {other}; this build reads format {FORMAT}",
                    store.path.display()
                )));
            }
        }
        tx.commit().map_err(unavailable)?;
        store.release(conn);
        Ok(store)
    }

    /// Opens the store a URL names: `sqlite:PATH`.
    pub fn from_url(url: &str) -> Result<SqliteStore, StateError> {
        let path = url
            .strip_prefix("sqlite:")
            .filter(|p| !p.is_empty())
            .ok_or_else(|| StateError::Config(format!("`{url}` is not a SQLite store URL (sqlite:PATH)")))?;
        SqliteStore::open(path)
    }

    fn connect(&self) -> Result<Connection, StateError> {
        let conn = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| StateError::Unavailable(format!("sqlite: opening {}: {e}", self.path.display())))?;
        conn.busy_timeout(BUSY).map_err(unavailable)?;
        // Switching to the WAL journal can be refused as busy without the busy handler waiting (another process
        // creating the same file): tried again until the busy timeout.
        let clock = Elapsed::start();
        loop {
            match conn.execute_batch("pragma journal_mode = wal;") {
                Ok(()) => break,
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.code == rusqlite::ErrorCode::DatabaseBusy && clock.elapsed() < BUSY =>
                {
                    std::thread::sleep(POLL);
                }
                Err(e) => return Err(unavailable(e)),
            }
        }
        conn.execute_batch("pragma synchronous = full; pragma foreign_keys = off;")
            .map_err(unavailable)?;
        Ok(conn)
    }

    fn release(&self, conn: Connection) {
        if let Ok(mut idle) = self.idle.lock()
            && idle.len() < POOL
        {
            idle.push(conn);
        }
    }

    /// Runs `f` on a connection from the pool (or a new one), then returns it to the pool.
    fn with_conn<R>(&self, f: impl FnOnce(&mut Connection) -> Result<R, StateError>) -> Result<R, StateError> {
        let pooled = self
            .idle
            .lock()
            .map_err(|_| StateError::Unavailable("the SQLite pool's lock is poisoned".into()))?
            .pop();
        let mut conn = match pooled {
            Some(c) => c,
            None => self.connect()?,
        };
        let r = f(&mut conn);
        self.release(conn);
        r
    }

    fn version_on(conn: &Connection, object: &str) -> Result<u64, StateError> {
        let v: Option<i64> = conn
            .query_row("select version from blossom_objects where object = ?1", [object], |r| {
                r.get(0)
            })
            .optional()
            .map_err(unavailable)?;
        uint("version", v.unwrap_or(0))
    }
}

impl StateStore for SqliteStore {
    fn load(&self, object: &str) -> Result<Snapshot, StateError> {
        check_name("object name", object)?;
        self.with_conn(|conn| {
            // One read transaction: the version and the entries are of the same commit.
            let tx = conn.transaction().map_err(unavailable)?;
            let version = SqliteStore::version_on(&tx, object)?;
            let entries = {
                let mut stmt = tx
                    .prepare_cached("select key, value from blossom_entries where object = ?1 order by key")
                    .map_err(unavailable)?;
                let rows = stmt
                    .query_map([object], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))
                    .map_err(unavailable)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(unavailable)?
            };
            tx.finish().map_err(unavailable)?;
            Ok(Snapshot { version, entries })
        })
    }

    fn version(&self, object: &str) -> Result<u64, StateError> {
        check_name("object name", object)?;
        self.with_conn(|conn| SqliteStore::version_on(conn, object))
    }

    fn commit(&self, object: &str, expected: u64, writes: &[Write]) -> Result<Commit, StateError> {
        self.commit_rows(object, expected, writes, &Owner::default(), &[], None)
    }

    fn wait(&self, object: &str, since: u64, timeout: Duration) -> Result<u64, StateError> {
        check_name("object name", object)?;
        let clock = Elapsed::start();
        loop {
            let v = self.version(object)?;
            let spent = clock.elapsed();
            if v > since || spent >= timeout {
                return Ok(v);
            }
            std::thread::sleep(POLL.min(timeout.saturating_sub(spent)));
        }
    }

    fn schedule(&self, object: &str, at: u64) -> Result<(), StateError> {
        check_name("object name", object)?;
        let at = int("wake time", at)?;
        self.with_conn(|conn| {
            conn.execute(
                "insert into blossom_wakes (object, at) values (?1, ?2) on conflict do nothing",
                params![object, at],
            )
            .map_err(unavailable)?;
            Ok(())
        })
    }

    fn due(&self, now: u64, limit: usize) -> Result<Vec<(String, u64)>, StateError> {
        let now = i64::try_from(now).unwrap_or(i64::MAX);
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare_cached("select object, at from blossom_wakes where at <= ?1 order by at, object limit ?2")
                .map_err(unavailable)?;
            let rows = stmt
                .query_map(params![now, limit], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                })
                .map_err(unavailable)?;
            rows.map(|r| {
                let (o, at) = r.map_err(unavailable)?;
                Ok((o, uint("wake time", at)?))
            })
            .collect()
        })
    }

    fn unschedule(&self, object: &str, at: u64) -> Result<(), StateError> {
        check_name("object name", object)?;
        let Ok(at) = i64::try_from(at) else {
            // No hint can be at a time the table cannot hold.
            return Ok(());
        };
        self.with_conn(|conn| {
            conn.execute(
                "delete from blossom_wakes where object = ?1 and at = ?2",
                params![object, at],
            )
            .map_err(unavailable)?;
            Ok(())
        })
    }

    fn put_side(&self, key: &str, value: &[u8]) -> Result<(), StateError> {
        check_name("side key", key)?;
        self.with_conn(|conn| {
            conn.execute(
                "insert into blossom_side (key, value) values (?1, ?2) on conflict (key) do update set value = excluded.value",
                params![key, value],
            )
            .map_err(unavailable)?;
            Ok(())
        })
    }

    fn get_side(&self, key: &str) -> Result<Option<Vec<u8>>, StateError> {
        check_name("side key", key)?;
        self.with_conn(|conn| {
            conn.query_row("select value from blossom_side where key = ?1", [key], |r| r.get(0))
                .optional()
                .map_err(unavailable)
        })
    }

    fn delete_side(&self, key: &str) -> Result<(), StateError> {
        check_name("side key", key)?;
        self.with_conn(|conn| {
            conn.execute("delete from blossom_side where key = ?1", [key])
                .map_err(unavailable)?;
            Ok(())
        })
    }

    fn tables(&self) -> Option<&dyn TableStore> {
        Some(self)
    }
}

/// A column's SQLite type.
fn sqlite_type(t: SqlType) -> &'static str {
    match t {
        SqlType::Bool | SqlType::Int => "integer",
        SqlType::Numeric | SqlType::Text | SqlType::Json => "text",
        SqlType::Real => "real",
        SqlType::Bytes => "blob",
    }
}

fn bind(v: &SqlValue) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match v {
        SqlValue::Null => V::Null,
        SqlValue::Bool(b) => V::Integer(i64::from(*b)),
        SqlValue::Int(n) => V::Integer(*n),
        SqlValue::Numeric(t) | SqlValue::Text(t) | SqlValue::Json(t) => V::Text(t.clone()),
        SqlValue::Real(x) => V::Real(*x),
        SqlValue::Bytes(b) => V::Blob(b.clone()),
    }
}

fn keys_def() -> TableDef {
    TableDef {
        name: KEYS_TABLE.into(),
        view: None,
        columns: Vec::new(),
    }
}

fn catalog_lock() -> StateError {
    StateError::Unavailable("the SQLite catalog lock is poisoned".into())
}

impl SqliteStore {
    /// A table's definition: known to this handle, or read from the catalog (another instance created it).
    fn def_of(&self, conn: &Connection, table: &str) -> Result<TableDef, StateError> {
        if let Some(d) = self.defs.lock().map_err(|_| catalog_lock())?.get(table) {
            return Ok(d.clone());
        }
        let text: Option<String> = conn
            .query_row(
                "select value from blossom_catalog where key = ?1",
                [format!("table:{table}")],
                |r| r.get(0),
            )
            .optional()
            .map_err(unavailable)?;
        let text = text.ok_or_else(|| StateError::Invalid(format!("no table {table}")))?;
        let d: TableDef = serde_json::from_str(&text)
            .map_err(|e| StateError::Corrupt(format!("the catalog's entry for {table}: {e}")))?;
        self.defs
            .lock()
            .map_err(|_| catalog_lock())?
            .insert(table.to_owned(), d.clone());
        Ok(d)
    }
}

impl TableStore for SqliteStore {
    fn ensure_tables(&self, deployment: &str, tables: &[TableDef]) -> Result<(), StateError> {
        for t in tables {
            check_table(t)?;
        }
        let keys = keys_def();
        self.with_conn(|conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(unavailable)?;
            let held: Option<String> = tx
                .query_row("select value from blossom_catalog where key = 'deployment'", [], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(unavailable)?;
            match held {
                Some(d) if d != deployment => {
                    return Err(StateError::Config(format!(
                        "the store holds the tables of deployment {d}, not {deployment}"
                    )));
                }
                Some(_) => {}
                None => {
                    tx.execute(
                        "insert into blossom_catalog (key, value) values ('deployment', ?1)",
                        [deployment],
                    )
                    .map_err(unavailable)?;
                }
            }
            for t in tables.iter().chain(std::iter::once(&keys)) {
                let json = serde_json::to_string(t).map_err(|e| StateError::Invalid(e.to_string()))?;
                let held: Option<String> = tx
                    .query_row(
                        "select value from blossom_catalog where key = ?1",
                        [format!("table:{}", t.name)],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(unavailable)?;
                match held {
                    Some(h) if h != json => {
                        return Err(StateError::Config(format!(
                            "table {} exists with another definition than the program's",
                            t.name
                        )));
                    }
                    Some(_) => {}
                    None => {
                        let cols: String = t
                            .columns
                            .iter()
                            .map(|(c, ty)| format!(", \"{c}\" {}", sqlite_type(*ty)))
                            .collect();
                        tx.execute_batch(&format!(
                            "create table \"{n}\" (node text not null, member text not null, key blob not null,
                                 from_tick integer not null, to_tick integer{cols},
                                 primary key (node, member, key, from_tick));
                             create index \"{n}__open\" on \"{n}\" (node, member, key) where to_tick is null;",
                            n = t.name
                        ))
                        .map_err(unavailable)?;
                        tx.execute(
                            "insert into blossom_catalog (key, value) values (?1, ?2)",
                            params![format!("table:{}", t.name), json],
                        )
                        .map_err(unavailable)?;
                    }
                }
                if let Some(v) = &t.view {
                    let cols: String = t.columns.iter().map(|(c, _)| format!(", \"{c}\"")).collect();
                    tx.execute_batch(&format!(
                        "drop view if exists \"{v}\";
                         create view \"{v}\" as select node, member{cols} from \"{n}\" where to_tick is null;",
                        n = t.name
                    ))
                    .map_err(unavailable)?;
                }
            }
            tx.commit().map_err(unavailable)?;
            let mut defs = self.defs.lock().map_err(|_| catalog_lock())?;
            for t in tables.iter().chain(std::iter::once(&keys)) {
                defs.insert(t.name.clone(), t.clone());
            }
            Ok(())
        })
    }

    fn scan_keys(
        &self,
        table: &str,
        owner: &Owner,
        lo: &[u8],
        hi: Option<&[u8]>,
        at: u64,
        limit: usize,
    ) -> Result<Vec<Vec<u8>>, StateError> {
        check_ident("table name", table)?;
        let at = int("tick", at)?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare_cached(&format!(
                    "select key from \"{table}\" where node = ?1 and member = ?2 and key >= ?3
                       and (?4 is null or key < ?4) and from_tick <= ?5 and (to_tick is null or to_tick > ?5)
                     order by key limit ?6"
                ))
                .map_err(unavailable)?;
            let rows = stmt
                .query_map(params![owner.node, owner.member, lo, hi, at, limit], |r| {
                    r.get::<_, Vec<u8>>(0)
                })
                .map_err(unavailable)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(unavailable)
        })
    }

    fn has_key(&self, table: &str, owner: &Owner, key: &[u8], at: u64) -> Result<bool, StateError> {
        check_ident("table name", table)?;
        let at = int("tick", at)?;
        self.with_conn(|conn| {
            conn.query_row(
                &format!(
                    "select exists (select 1 from \"{table}\" where node = ?1 and member = ?2 and key = ?3
                       and from_tick <= ?4 and (to_tick is null or to_tick > ?4))"
                ),
                params![owner.node, owner.member, key, at],
                |r| r.get::<_, bool>(0),
            )
            .map_err(unavailable)
        })
    }

    fn commit_rows(
        &self,
        object: &str,
        expected: u64,
        writes: &[Write],
        owner: &Owner,
        rows: &[RowChange],
        prune_below: Option<u64>,
    ) -> Result<Commit, StateError> {
        check_commit(object, writes)?;
        let next = int("version", expected.saturating_add(1))?;
        self.with_conn(|conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(unavailable)?;
            let current = SqliteStore::version_on(&tx, object)?;
            if current != expected {
                tx.rollback().map_err(unavailable)?;
                return Ok(Commit::Conflict { current });
            }
            {
                let mut upsert = tx
                    .prepare_cached(
                        "insert into blossom_entries (object, key, value) values (?1, ?2, ?3)
                         on conflict (object, key) do update set value = excluded.value",
                    )
                    .map_err(unavailable)?;
                let mut delete = tx
                    .prepare_cached("delete from blossom_entries where object = ?1 and key = ?2")
                    .map_err(unavailable)?;
                for w in writes {
                    match w {
                        Write::Put(k, v) => upsert.execute(params![object, k, v]).map(drop),
                        Write::Delete(k) => delete.execute(params![object, k]).map(drop),
                    }
                    .map_err(unavailable)?;
                }
            }
            for change in rows {
                match change {
                    RowChange::Open {
                        table,
                        key,
                        from,
                        values,
                    } => {
                        let def = self.def_of(&tx, table)?;
                        if values.len() != def.columns.len() {
                            return Err(StateError::Invalid(format!(
                                "{} values for the {} columns of {table}",
                                values.len(),
                                def.columns.len()
                            )));
                        }
                        let open: bool = tx
                            .query_row(
                                &format!(
                                    "select exists (select 1 from \"{table}\" where node = ?1 and member = ?2
                                       and key = ?3 and to_tick is null)"
                                ),
                                params![owner.node, owner.member, key],
                                |r| r.get(0),
                            )
                            .map_err(unavailable)?;
                        if open {
                            return Err(StateError::Corrupt(format!("{table}: a key opened while open")));
                        }
                        let names: String = def.columns.iter().map(|(c, _)| format!(", \"{c}\"")).collect();
                        let marks: String = (0..def.columns.len()).map(|i| format!(", ?{}", i + 5)).collect();
                        let mut args: Vec<rusqlite::types::Value> = vec![
                            owner.node.clone().into(),
                            owner.member.clone().into(),
                            key.clone().into(),
                            int("tick", *from)?.into(),
                        ];
                        args.extend(values.iter().map(bind));
                        tx.execute(
                            &format!(
                                "insert into \"{table}\" (node, member, key, from_tick{names})
                                 values (?1, ?2, ?3, ?4{marks})"
                            ),
                            rusqlite::params_from_iter(args),
                        )
                        .map_err(unavailable)?;
                    }
                    RowChange::Close { table, key, at } => {
                        check_ident("table name", table)?;
                        let n = tx
                            .execute(
                                &format!(
                                    "update \"{table}\" set to_tick = ?4 where node = ?1 and member = ?2
                                       and key = ?3 and to_tick is null"
                                ),
                                params![owner.node, owner.member, key, int("tick", *at)?],
                            )
                            .map_err(unavailable)?;
                        if n != 1 {
                            return Err(StateError::Corrupt(format!("{table}: a key closed while not open")));
                        }
                    }
                }
            }
            if let Some(floor) = prune_below {
                let floor = int("tick", floor)?;
                let names: Vec<String> = {
                    let mut stmt = tx
                        .prepare_cached("select key from blossom_catalog where key like 'table:%'")
                        .map_err(unavailable)?;
                    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(unavailable)?;
                    rows.collect::<Result<Vec<_>, _>>().map_err(unavailable)?
                };
                for k in names {
                    let table = k.trim_start_matches("table:");
                    check_ident("table name", table)?;
                    tx.execute(
                        &format!(
                            "delete from \"{table}\" where node = ?1 and member = ?2 and to_tick is not null
                               and to_tick <= ?3"
                        ),
                        params![owner.node, owner.member, floor],
                    )
                    .map_err(unavailable)?;
                }
            }
            tx.execute(
                "insert into blossom_objects (object, version) values (?1, ?2)
                 on conflict (object) do update set version = excluded.version",
                params![object, next],
            )
            .map_err(unavailable)?;
            tx.commit().map_err(unavailable)?;
            Ok(Commit::Done { version: expected + 1 })
        })
    }
}

/// Elapsed time for `wait`'s deadline: a host's I/O, not a node's clock.
struct Elapsed(std::time::Instant);

impl Elapsed {
    #[allow(clippy::disallowed_methods)] // a store's poll deadline, not a node's time
    fn start() -> Elapsed {
        Elapsed(std::time::Instant::now())
    }
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}
