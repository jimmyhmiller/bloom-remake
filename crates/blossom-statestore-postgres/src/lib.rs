//! A [`StateStore`] in Postgres (docs/design/STATELESS.md §4.2).
//!
//! The store's tables live in a schema of their own (`?schema=NAME`, `blossom` by default), created on first use:
//!
//! ```sql
//! create table meta    (key text primary key, value bigint not null);               -- format = 1
//! create table objects (object text collate "C" primary key, version bigint not null);
//! create table entries (object text collate "C" not null, key text collate "C" not null, value bytea not null,
//!                       primary key (object, key));
//! create table wakes   (object text collate "C" not null, at bigint not null, primary key (object, at));
//! create index wakes_at on wakes (at, object);
//! create table side    (key text collate "C" primary key, value bytea not null);
//! ```
//!
//! Text collates by bytes (`"C"`), so entries come back in the byte order of their keys.
//!
//! **A commit** is one transaction: it raises the object's version with an `update … where version = $expected` (an
//! insert for version 0), whose row lock makes a concurrent commit at the same version wait and then find the version
//! moved; then it applies the writes and `NOTIFY`s the store's channel with the object and its new version. A commit
//! whose `COMMIT` got no answer is [`StateError::Unknown`].
//!
//! **Waiting.** Each store handle keeps one connection `LISTEN`ing (started by the first `wait`), which wakes the
//! waiters of the objects notified. A notification is a hint: `wait` reads the version before it returns, and reads it
//! every [`BACKUP_POLL`] anyway, so a lost notification (a dropped listener connection) costs latency, never a change.
//!
//! **URLs**: `postgres://USER:PASS@HOST:PORT/DB?schema=NAME&sslmode=disable|prefer|require&sslrootcert=PATH`. With
//! `sslmode` other than `disable` the connection is TLS (rustls) when the server offers it (`prefer`, the default) or
//! always (`require`), the server's certificate checked against `sslrootcert`'s PEM certificates, or else the web's
//! roots (webpki-roots).

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use blossom_statestore::{Commit, Snapshot, StateError, StateStore, Write, check_commit, check_name};
use postgres::config::SslMode;
use postgres::fallible_iterator::FallibleIterator;
use postgres::{Client, Config, NoTls};
use postgres_rustls::MakeTlsConnector;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;

/// How often `wait` reads the version with no notification heard.
pub const BACKUP_POLL: Duration = Duration::from_secs(1);
/// The schema's format; a schema of another is refused.
const FORMAT: i64 = 1;
/// Connections kept for reuse.
const POOL: usize = 8;
/// How long the listener waits for a notification before checking whether it should stop.
const LISTEN_SLICE: Duration = Duration::from_millis(250);

fn unavailable(e: postgres::Error) -> StateError {
    StateError::Unavailable(format!("postgres: {e}"))
}

fn int(what: &str, n: u64) -> Result<i64, StateError> {
    i64::try_from(n).map_err(|_| StateError::Invalid(format!("{what} {n} does not fit Postgres's bigint")))
}

fn uint(what: &str, n: i64) -> Result<u64, StateError> {
    u64::try_from(n).map_err(|_| StateError::Corrupt(format!("a negative {what} ({n})")))
}

/// How connections are made.
#[derive(Clone)]
enum Tls {
    Off,
    Rustls(MakeTlsConnector),
}

/// What a connection needs: the server's configuration and the TLS to use.
#[derive(Clone)]
struct Connect {
    config: Config,
    tls: Tls,
}

impl Connect {
    fn client(&self) -> Result<Client, StateError> {
        match &self.tls {
            Tls::Off => self.config.connect(NoTls),
            Tls::Rustls(tls) => self.config.connect(tls.clone()),
        }
        .map_err(|e| StateError::Unavailable(format!("postgres: connecting: {e}")))
    }
}

/// The statements, with the schema's name in them.
struct Sql {
    version: String,
    entries: String,
    create: String,
    bump: String,
    upsert: String,
    delete: String,
    notify: String,
    schedule: String,
    due: String,
    unschedule: String,
    put_side: String,
    get_side: String,
    delete_side: String,
}

impl Sql {
    fn of(s: &str) -> Sql {
        Sql {
            version: format!("select version from \"{s}\".objects where object = $1"),
            entries: format!("select key, value from \"{s}\".entries where object = $1 order by key"),
            create: format!("insert into \"{s}\".objects (object, version) values ($1, 1) on conflict do nothing"),
            bump: format!("update \"{s}\".objects set version = version + 1 where object = $1 and version = $2"),
            upsert: format!(
                "insert into \"{s}\".entries (object, key, value) values ($1, $2, $3)
                 on conflict (object, key) do update set value = excluded.value"
            ),
            delete: format!("delete from \"{s}\".entries where object = $1 and key = $2"),
            notify: "select pg_notify($1, $2)".into(),
            schedule: format!("insert into \"{s}\".wakes (object, at) values ($1, $2) on conflict do nothing"),
            due: format!("select object, at from \"{s}\".wakes where at <= $1 order by at, object limit $2"),
            unschedule: format!("delete from \"{s}\".wakes where object = $1 and at = $2"),
            put_side: format!(
                "insert into \"{s}\".side (key, value) values ($1, $2)
                 on conflict (key) do update set value = excluded.value"
            ),
            get_side: format!("select value from \"{s}\".side where key = $1"),
            delete_side: format!("delete from \"{s}\".side where key = $1"),
        }
    }
}

/// What the listener heard, for the objects someone waits on.
#[derive(Default)]
struct Heard {
    /// Per object waited on: how many wait, and the highest version notified.
    interest: BTreeMap<String, (usize, u64)>,
    /// Whether the listener thread runs.
    started: bool,
}

struct Listen {
    heard: Mutex<Heard>,
    notified: Condvar,
    stop: AtomicBool,
}

/// A state store in a Postgres schema.
pub struct PostgresStore {
    connect: Connect,
    schema: String,
    channel: String,
    sql: Sql,
    idle: Mutex<Vec<Client>>,
    listen: Arc<Listen>,
    listener: Mutex<Option<JoinHandle<()>>>,
}

/// A schema name the store accepts: lowercase ASCII letters, digits and `_`, not starting with a digit (so it needs
/// no quoting rules beyond the double quotes the statements put around it).
fn check_schema(name: &str) -> Result<(), StateError> {
    let ok = !name.is_empty()
        && name.len() <= 48
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !name.as_bytes().first().is_some_and(u8::is_ascii_digit);
    if ok {
        Ok(())
    } else {
        Err(StateError::Config(format!(
            "`{name}` is not a schema name the store takes (lowercase letters, digits and _, at most 48)"
        )))
    }
}

fn tls_config(rootcert: Option<&str>) -> Result<MakeTlsConnector, StateError> {
    let mut roots = rustls::RootCertStore::empty();
    match rootcert {
        Some(path) => {
            let pem = std::fs::read(path).map_err(|e| StateError::Config(format!("sslrootcert {path}: {e}")))?;
            for cert in CertificateDer::pem_slice_iter(&pem) {
                let cert = cert.map_err(|e| StateError::Config(format!("sslrootcert {path}: {e}")))?;
                roots
                    .add(cert)
                    .map_err(|e| StateError::Config(format!("sslrootcert {path}: {e}")))?;
            }
            if roots.is_empty() {
                return Err(StateError::Config(format!("sslrootcert {path} holds no certificate")));
            }
        }
        None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| StateError::Config(format!("TLS: {e}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(MakeTlsConnector::new(Arc::new(config).into()))
}

impl PostgresStore {
    /// Opens the store a URL names (see the crate's documentation), creating its schema and tables if need be.
    pub fn from_url(url: &str) -> Result<PostgresStore, StateError> {
        if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
            return Err(StateError::Config(format!(
                "`{url}` is not a Postgres store URL (postgres://…)"
            )));
        }
        // The parameters the store reads itself come off the URL; the rest go to the client.
        let (base, query) = url.split_once('?').unwrap_or((url, ""));
        let mut schema = "blossom".to_string();
        let mut rootcert = None;
        let mut rest = Vec::new();
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            match pair.split_once('=') {
                Some(("schema", v)) => schema = v.to_string(),
                Some(("sslrootcert", v)) => rootcert = Some(v.to_string()),
                _ => rest.push(pair),
            }
        }
        check_schema(&schema)?;
        let client_url = if rest.is_empty() {
            base.to_string()
        } else {
            format!("{base}?{}", rest.join("&"))
        };
        let config = Config::from_str(&client_url).map_err(|e| StateError::Config(format!("postgres URL: {e}")))?;
        let tls = match config.get_ssl_mode() {
            SslMode::Disable => Tls::Off,
            _ => Tls::Rustls(tls_config(rootcert.as_deref())?),
        };
        PostgresStore::open(Connect { config, tls }, schema)
    }

    fn open(connect: Connect, schema: String) -> Result<PostgresStore, StateError> {
        let store = PostgresStore {
            channel: format!("blossom_{schema}"),
            sql: Sql::of(&schema),
            schema,
            connect,
            idle: Mutex::new(Vec::new()),
            listen: Arc::new(Listen {
                heard: Mutex::new(Heard::default()),
                notified: Condvar::new(),
                stop: AtomicBool::new(false),
            }),
            listener: Mutex::new(None),
        };
        store.with_client(|c| store.create_schema(c))?;
        Ok(store)
    }

    /// Creates the schema and its tables once: concurrent first starts take an advisory lock in turn.
    fn create_schema(&self, c: &mut Client) -> Result<(), StateError> {
        let s = &self.schema;
        let mut tx = c.transaction().map_err(unavailable)?;
        tx.execute("select pg_advisory_xact_lock(hashtext($1))", &[&format!("blossom schema {s}")])
            .map_err(unavailable)?;
        tx.batch_execute(&format!(
            "create schema if not exists \"{s}\";
             create table if not exists \"{s}\".meta (key text primary key, value bigint not null);
             create table if not exists \"{s}\".objects (object text collate \"C\" primary key, version bigint not null);
             create table if not exists \"{s}\".entries (object text collate \"C\" not null,
                 key text collate \"C\" not null, value bytea not null, primary key (object, key));
             create table if not exists \"{s}\".wakes (object text collate \"C\" not null, at bigint not null,
                 primary key (object, at));
             create index if not exists wakes_at on \"{s}\".wakes (at, object);
             create table if not exists \"{s}\".side (key text collate \"C\" primary key, value bytea not null);"
        ))
        .map_err(unavailable)?;
        let format: Option<i64> = tx
            .query_opt(&format!("select value from \"{s}\".meta where key = 'format'"), &[])
            .map_err(unavailable)?
            .map(|r| r.get(0));
        match format {
            None => {
                tx.execute(
                    &format!("insert into \"{s}\".meta (key, value) values ('format', $1)"),
                    &[&FORMAT],
                )
                .map_err(unavailable)?;
            }
            Some(FORMAT) => {}
            Some(other) => {
                return Err(StateError::Config(format!(
                    "schema {s} holds a state store of format {other}; this build reads format {FORMAT}"
                )));
            }
        }
        tx.commit().map_err(unavailable)
    }

    /// Runs `f` on a pooled connection (or a new one), then pools it again unless it closed.
    fn with_client<R>(&self, f: impl FnOnce(&mut Client) -> Result<R, StateError>) -> Result<R, StateError> {
        let pooled = self
            .idle
            .lock()
            .map_err(|_| StateError::Unavailable("the Postgres pool's lock is poisoned".into()))?
            .pop();
        let mut client = match pooled {
            Some(c) if !c.is_closed() => c,
            _ => self.connect.client()?,
        };
        let r = f(&mut client);
        if !client.is_closed()
            && let Ok(mut idle) = self.idle.lock()
            && idle.len() < POOL
        {
            idle.push(client);
        }
        r
    }

    fn version_on(&self, c: &mut impl postgres::GenericClient, object: &str) -> Result<u64, StateError> {
        let v: Option<i64> = c
            .query_opt(&self.sql.version, &[&object])
            .map_err(unavailable)?
            .map(|r| r.get(0));
        uint("version", v.unwrap_or(0))
    }

    /// Starts the listener thread, once.
    fn start_listener(&self) -> Result<(), StateError> {
        let mut heard = self
            .listen
            .heard
            .lock()
            .map_err(|_| StateError::Unavailable("the listener's lock is poisoned".into()))?;
        if heard.started {
            return Ok(());
        }
        heard.started = true;
        let (connect, channel, listen) = (self.connect.clone(), self.channel.clone(), self.listen.clone());
        let handle = std::thread::Builder::new()
            .name(format!("{channel}-listen"))
            .spawn(move || listener(&connect, &channel, &listen))
            .map_err(|e| StateError::Unavailable(format!("starting the listener: {e}")))?;
        if let Ok(mut slot) = self.listener.lock() {
            *slot = Some(handle);
        }
        Ok(())
    }

    /// Registers (or, with `-1`, unregisters) a waiter on `object`.
    fn interest(&self, object: &str, delta: isize) {
        if let Ok(mut heard) = self.listen.heard.lock() {
            let entry = heard.interest.entry(object.to_owned()).or_insert((0, 0));
            entry.0 = entry.0.saturating_add_signed(delta);
            if entry.0 == 0 {
                heard.interest.remove(object);
            }
        }
    }
}

impl Drop for PostgresStore {
    fn drop(&mut self) {
        self.listen.stop.store(true, Ordering::Relaxed);
        if let Ok(mut slot) = self.listener.lock()
            && let Some(h) = slot.take()
        {
            let _ = h.join();
        }
    }
}

/// The listener thread: `LISTEN` on the store's channel, and wake the waiters of each object notified. On a failure
/// it connects again (waiters poll meanwhile).
fn listener(connect: &Connect, channel: &str, listen: &Listen) {
    let mut backoff = Duration::from_millis(100);
    while !listen.stop.load(Ordering::Relaxed) {
        let mut client = match connect.client() {
            Ok(c) => c,
            Err(_) => {
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(5));
                continue;
            }
        };
        if client.batch_execute(&format!("listen \"{channel}\"")).is_err() {
            std::thread::sleep(backoff);
            continue;
        }
        backoff = Duration::from_millis(100);
        while !listen.stop.load(Ordering::Relaxed) {
            let mut failed = false;
            let mut notes = client.notifications();
            let mut it = notes.timeout_iter(LISTEN_SLICE);
            loop {
                match it.next() {
                    Ok(Some(n)) => {
                        let Some((v, object)) = n.payload().split_once(' ') else { continue };
                        let Ok(v) = v.parse::<u64>() else { continue };
                        if let Ok(mut heard) = listen.heard.lock()
                            && let Some(entry) = heard.interest.get_mut(object)
                        {
                            entry.1 = entry.1.max(v);
                            listen.notified.notify_all();
                        }
                    }
                    Ok(None) => break,
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            drop(it);
            drop(notes);
            if failed || client.is_closed() {
                break;
            }
        }
    }
}

impl StateStore for PostgresStore {
    fn load(&self, object: &str) -> Result<Snapshot, StateError> {
        check_name("object name", object)?;
        self.with_client(|c| {
            // One repeatable-read transaction: the version and the entries are of the same commit.
            let mut tx = c
                .build_transaction()
                .isolation_level(postgres::IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .map_err(unavailable)?;
            let version = self.version_on(&mut tx, object)?;
            let entries = tx
                .query(&self.sql.entries, &[&object])
                .map_err(unavailable)?
                .into_iter()
                .map(|r| (r.get::<_, String>(0), r.get::<_, Vec<u8>>(1)))
                .collect();
            tx.commit().map_err(unavailable)?;
            Ok(Snapshot { version, entries })
        })
    }

    fn version(&self, object: &str) -> Result<u64, StateError> {
        check_name("object name", object)?;
        self.with_client(|c| self.version_on(c, object))
    }

    fn commit(&self, object: &str, expected: u64, writes: &[Write]) -> Result<Commit, StateError> {
        check_commit(object, writes)?;
        let exp = int("version", expected)?;
        let next = expected.saturating_add(1);
        int("version", next)?;
        self.with_client(|c| {
            let mut tx = c.transaction().map_err(unavailable)?;
            let won = if expected == 0 {
                tx.execute(&self.sql.create, &[&object]).map_err(unavailable)?
            } else {
                tx.execute(&self.sql.bump, &[&object, &exp]).map_err(unavailable)?
            };
            if won != 1 {
                let current = self.version_on(&mut tx, object)?;
                tx.rollback().map_err(unavailable)?;
                return Ok(Commit::Conflict { current });
            }
            {
                let upsert = tx.prepare(&self.sql.upsert).map_err(unavailable)?;
                let delete = tx.prepare(&self.sql.delete).map_err(unavailable)?;
                for w in writes {
                    match w {
                        Write::Put(k, v) => tx.execute(&upsert, &[&object, k, v]),
                        Write::Delete(k) => tx.execute(&delete, &[&object, k]),
                    }
                    .map_err(unavailable)?;
                }
            }
            tx.execute(&self.sql.notify, &[&self.channel, &format!("{next} {object}")])
                .map_err(unavailable)?;
            match tx.commit() {
                Ok(()) => Ok(Commit::Done { version: next }),
                // The server refused the commit: it rolled back.
                Err(e) if e.as_db_error().is_some() => Err(unavailable(e)),
                // No answer: it may have committed.
                Err(e) => Err(StateError::Unknown {
                    object: object.to_owned(),
                    reason: format!("postgres: {e}"),
                }),
            }
        })
    }

    fn wait(&self, object: &str, since: u64, timeout: Duration) -> Result<u64, StateError> {
        check_name("object name", object)?;
        self.interest(object, 1);
        let r = self.wait_registered(object, since, timeout);
        self.interest(object, -1);
        r
    }

    fn schedule(&self, object: &str, at: u64) -> Result<(), StateError> {
        check_name("object name", object)?;
        let at = int("wake time", at)?;
        self.with_client(|c| c.execute(&self.sql.schedule, &[&object, &at]).map(drop).map_err(unavailable))
    }

    fn due(&self, now: u64, limit: usize) -> Result<Vec<(String, u64)>, StateError> {
        let now = i64::try_from(now).unwrap_or(i64::MAX);
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.with_client(|c| {
            c.query(&self.sql.due, &[&now, &limit])
                .map_err(unavailable)?
                .into_iter()
                .map(|r| Ok((r.get::<_, String>(0), uint("wake time", r.get::<_, i64>(1))?)))
                .collect()
        })
    }

    fn unschedule(&self, object: &str, at: u64) -> Result<(), StateError> {
        check_name("object name", object)?;
        let Ok(at) = i64::try_from(at) else {
            // No hint can be at a time the table cannot hold.
            return Ok(());
        };
        self.with_client(|c| c.execute(&self.sql.unschedule, &[&object, &at]).map(drop).map_err(unavailable))
    }

    fn put_side(&self, key: &str, value: &[u8]) -> Result<(), StateError> {
        check_name("side key", key)?;
        self.with_client(|c| c.execute(&self.sql.put_side, &[&key, &value]).map(drop).map_err(unavailable))
    }

    fn get_side(&self, key: &str) -> Result<Option<Vec<u8>>, StateError> {
        check_name("side key", key)?;
        self.with_client(|c| {
            Ok(c.query_opt(&self.sql.get_side, &[&key])
                .map_err(unavailable)?
                .map(|r| r.get::<_, Vec<u8>>(0)))
        })
    }

    fn delete_side(&self, key: &str) -> Result<(), StateError> {
        check_name("side key", key)?;
        self.with_client(|c| c.execute(&self.sql.delete_side, &[&key]).map(drop).map_err(unavailable))
    }
}

impl PostgresStore {
    fn wait_registered(&self, object: &str, since: u64, timeout: Duration) -> Result<u64, StateError> {
        self.start_listener()?;
        let clock = Elapsed::start();
        loop {
            let v = self.version(object)?;
            let spent = clock.elapsed();
            if v > since || spent >= timeout {
                return Ok(v);
            }
            let slice = BACKUP_POLL.min(timeout.saturating_sub(spent));
            let heard = self
                .listen
                .heard
                .lock()
                .map_err(|_| StateError::Unavailable("the listener's lock is poisoned".into()))?;
            let _ = self
                .listen
                .notified
                .wait_timeout_while(heard, slice, |h| {
                    h.interest.get(object).is_none_or(|(_, noted)| *noted <= since)
                })
                .map_err(|_| StateError::Unavailable("the listener's lock is poisoned".into()))?;
        }
    }
}

/// Elapsed time for `wait`'s deadline: a host's I/O, not a node's clock.
struct Elapsed(std::time::Instant);

impl Elapsed {
    #[allow(clippy::disallowed_methods)] // a store's wait deadline, not a node's time
    fn start() -> Elapsed {
        Elapsed(std::time::Instant::now())
    }
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}
