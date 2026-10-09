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
//! create table meta    (key text primary key, value integer not null);   -- format = 1
//! create table objects (object text primary key, version integer not null);
//! create table entries (object text not null, key text not null, value blob not null, primary key (object, key));
//! create table wakes   (object text not null, at integer not null, primary key (object, at));
//! create index wakes_at on wakes (at, object);
//! create table side    (key text primary key, value blob not null);
//! ```
//!
//! Text compares by bytes (SQLite's `BINARY` collation), so entries come back in the byte order of their keys.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use blossom_statestore::{Commit, Snapshot, StateError, StateStore, Write, check_commit, check_name};
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
        };
        let mut conn = store.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        tx.execute_batch(
            "create table if not exists meta (key text primary key, value integer not null);
             create table if not exists objects (object text primary key, version integer not null);
             create table if not exists entries (object text not null, key text not null, value blob not null,
                 primary key (object, key));
             create table if not exists wakes (object text not null, at integer not null, primary key (object, at));
             create index if not exists wakes_at on wakes (at, object);
             create table if not exists side (key text primary key, value blob not null);",
        )
        .map_err(unavailable)?;
        let format: Option<i64> = tx
            .query_row("select value from meta where key = 'format'", [], |r| r.get(0))
            .optional()
            .map_err(unavailable)?;
        match format {
            None => {
                tx.execute("insert into meta (key, value) values ('format', ?1)", [FORMAT])
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
        conn.execute_batch("pragma journal_mode = wal; pragma synchronous = full; pragma foreign_keys = off;")
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
            .query_row("select version from objects where object = ?1", [object], |r| r.get(0))
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
                    .prepare_cached("select key, value from entries where object = ?1 order by key")
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
                        "insert into entries (object, key, value) values (?1, ?2, ?3)
                         on conflict (object, key) do update set value = excluded.value",
                    )
                    .map_err(unavailable)?;
                let mut delete = tx
                    .prepare_cached("delete from entries where object = ?1 and key = ?2")
                    .map_err(unavailable)?;
                for w in writes {
                    match w {
                        Write::Put(k, v) => upsert.execute(params![object, k, v]).map(drop),
                        Write::Delete(k) => delete.execute(params![object, k]).map(drop),
                    }
                    .map_err(unavailable)?;
                }
            }
            tx.execute(
                "insert into objects (object, version) values (?1, ?2)
                 on conflict (object) do update set version = excluded.version",
                params![object, next],
            )
            .map_err(unavailable)?;
            tx.commit().map_err(unavailable)?;
            Ok(Commit::Done { version: expected + 1 })
        })
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
                "insert into wakes (object, at) values (?1, ?2) on conflict do nothing",
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
                .prepare_cached("select object, at from wakes where at <= ?1 order by at, object limit ?2")
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
            conn.execute("delete from wakes where object = ?1 and at = ?2", params![object, at])
                .map_err(unavailable)?;
            Ok(())
        })
    }

    fn put_side(&self, key: &str, value: &[u8]) -> Result<(), StateError> {
        check_name("side key", key)?;
        self.with_conn(|conn| {
            conn.execute(
                "insert into side (key, value) values (?1, ?2) on conflict (key) do update set value = excluded.value",
                params![key, value],
            )
            .map_err(unavailable)?;
            Ok(())
        })
    }

    fn get_side(&self, key: &str) -> Result<Option<Vec<u8>>, StateError> {
        check_name("side key", key)?;
        self.with_conn(|conn| {
            conn.query_row("select value from side where key = ?1", [key], |r| r.get(0))
                .optional()
                .map_err(unavailable)
        })
    }

    fn delete_side(&self, key: &str) -> Result<(), StateError> {
        check_name("side key", key)?;
        self.with_conn(|conn| {
            conn.execute("delete from side where key = ?1", [key])
                .map_err(unavailable)?;
            Ok(())
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
