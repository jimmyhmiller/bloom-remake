//! The Postgres store against the conformance suite, each case in a fresh schema, over a plain connection and over
//! TLS. The server is `scripts/test-services.sh start`'s (`eval "$(scripts/test-services.sh env)"` sets the URLs); the
//! fast tier ignores these tests, and with the variables unset they fail, naming them.

use std::sync::atomic::{AtomicU64, Ordering};

use blossom_statestore::conformance::Harness;
use blossom_statestore::{StateError, StateStore};
use blossom_statestore_postgres::PostgresStore;

const WHY: &str = "needs Postgres: scripts/test-services.sh start, then eval \"$(scripts/test-services.sh env)\"";

/// A schema of its own, dropped when the case ends.
struct Pg {
    /// The server's URL, or why there is none.
    url: Result<String, String>,
    schema: String,
}

fn base(var: &str) -> Result<String, String> {
    match std::env::var(var) {
        Ok(url) if !url.is_empty() => Ok(url),
        _ => Err(format!("{var} is not set; {WHY}")),
    }
}

fn with_schema(url: &str, schema: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}schema={schema}")
}

fn fresh(var: &str) -> Pg {
    static N: AtomicU64 = AtomicU64::new(0);
    let url = base(var);
    let schema = format!("t_{}_{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
    if let Ok(u) = &url {
        drop_schema(u, &schema);
    }
    Pg { url, schema }
}

fn drop_schema(url: &str, schema: &str) {
    let plain = url.split('?').next().unwrap_or(url);
    if let Ok(mut c) = postgres::Client::connect(&format!("{plain}?sslmode=disable"), postgres::NoTls) {
        let _ = c.batch_execute(&format!("drop schema if exists \"{schema}\" cascade"));
    }
}

impl Harness for Pg {
    fn open(&self) -> Result<Box<dyn StateStore>, StateError> {
        let url = self.url.as_ref().map_err(|why| StateError::Config(why.clone()))?;
        Ok(Box::new(PostgresStore::from_url(&with_schema(url, &self.schema))?))
    }
}

impl Drop for Pg {
    fn drop(&mut self) {
        if let Ok(url) = &self.url {
            drop_schema(url, &self.schema);
        }
    }
}

mod plain {
    use super::*;
    blossom_statestore::statestore_conformance!(ignore = "needs Postgres: scripts/test-services.sh", || fresh(
        "BLOSSOM_TEST_POSTGRES"
    ));
}

mod tls {
    use super::*;
    blossom_statestore::statestore_conformance!(ignore = "needs Postgres: scripts/test-services.sh", || fresh(
        "BLOSSOM_TEST_POSTGRES_TLS"
    ));
}

#[test]
#[ignore = "needs Postgres: scripts/test-services.sh"]
fn a_schema_of_another_format_is_refused() {
    let h = fresh("BLOSSOM_TEST_POSTGRES");
    h.open().unwrap();
    let url = h.url.clone().unwrap();
    let plain = url.split('?').next().unwrap().to_string();
    let mut c = postgres::Client::connect(&format!("{plain}?sslmode=disable"), postgres::NoTls).unwrap();
    c.execute(
        &format!(
            "update \"{}\".blossom_meta set value = 99 where key = 'format'",
            h.schema
        ),
        &[],
    )
    .unwrap();
    match h.open() {
        Err(StateError::Config(m)) => assert!(m.contains("format 99"), "{m}"),
        other => panic!("opened a store of another format: {:?}", other.err()),
    }
}

#[test]
#[ignore = "needs Postgres: scripts/test-services.sh"]
fn tls_is_required_when_asked_and_checked_against_the_roots() {
    // The test server's certificate is signed by a CA of its own: with the web's roots it is refused.
    let url = base("BLOSSOM_TEST_POSTGRES_TLS").unwrap();
    let without_root: String = url
        .split('&')
        .filter(|p| !p.starts_with("sslrootcert="))
        .collect::<Vec<_>>()
        .join("&");
    match PostgresStore::from_url(&without_root) {
        Err(StateError::Unavailable(m)) => assert!(m.contains("connect"), "{m}"),
        other => panic!("a server certificate no root signed was taken: {:?}", other.err()),
    }
}

#[test]
fn urls_and_schemas() {
    assert!(matches!(
        PostgresStore::from_url("sqlite:x"),
        Err(StateError::Config(_))
    ));
    for bad in ["Upper", "1abc", "a-b", "a\"b", ""] {
        let r = PostgresStore::from_url(&format!("postgres://u@127.0.0.1:1/db?schema={bad}"));
        assert!(matches!(r, Err(StateError::Config(_))), "schema `{bad}` was taken");
    }
}

/// A waiter hears another handle's commit by its notification, well before the backup poll would find it.
#[test]
#[ignore = "needs Postgres: scripts/test-services.sh"]
fn a_waiter_hears_a_commit_by_its_notification() {
    use blossom_statestore::{Commit, Write};
    use std::time::Duration;
    let h = fresh("BLOSSOM_TEST_POSTGRES");
    let (waiter, committer) = (h.open().unwrap(), h.open().unwrap());
    committer.commit("notify/o", 0, &[]).unwrap();
    // The first wait starts the listener; later ones find it running.
    for round in 1..=5u64 {
        let heard = std::thread::scope(|s| {
            let w = s.spawn(|| {
                #[allow(clippy::disallowed_methods)] // measures the store's latency
                let t = std::time::Instant::now();
                let v = waiter.wait("notify/o", round, Duration::from_secs(10)).unwrap();
                (v, t.elapsed())
            });
            std::thread::sleep(Duration::from_millis(150));
            let c = committer
                .commit(
                    "notify/o",
                    round,
                    &[Write::Put("r".into(), round.to_le_bytes().to_vec())],
                )
                .unwrap();
            assert_eq!(c, Commit::Done { version: round + 1 });
            w.join().unwrap()
        });
        assert_eq!(heard.0, round + 1);
        assert!(
            heard.1 < Duration::from_millis(550),
            "round {round}: heard after {:?}; the backup poll is 1 s, so the notification was missed",
            heard.1
        );
    }
}

mod tables {
    use super::*;
    blossom_statestore::statestore_conformance!(tables, ignore = "needs Postgres: scripts/test-services.sh", || fresh(
        "BLOSSOM_TEST_POSTGRES"
    ));
}

/// What SQL readers see: the current-rows view, its typed columns holding the values committed.
#[test]
#[ignore = "needs Postgres: scripts/test-services.sh"]
fn the_view_shows_current_rows_typed() {
    use blossom_statestore::{Owner, RowChange, SqlType, SqlValue, TableDef};
    let h = fresh("BLOSSOM_TEST_POSTGRES");
    let s = h.open().unwrap();
    let t = s.tables().unwrap();
    let def = TableDef {
        name: "r_log_0123abcd".into(),
        view: Some("log".into()),
        columns: vec![
            ("who".into(), SqlType::Text),
            ("k".into(), SqlType::Numeric),
            ("at".into(), SqlType::Int),
            ("doc".into(), SqlType::Json),
            ("ok".into(), SqlType::Bool),
        ],
    };
    t.ensure_tables("dep", std::slice::from_ref(&def)).unwrap();
    let owner = Owner {
        node: "rooms".into(),
        member: "lunch".into(),
    };
    let row = |k: &str, who: &str| RowChange::Open {
        table: def.name.clone(),
        key: k.as_bytes().to_vec(),
        from: 3,
        values: vec![
            SqlValue::Text(who.into()),
            SqlValue::Numeric("18446744073709551615".into()),
            SqlValue::Int(-5),
            SqlValue::Json(r#"{"variant": 1, "fields": ["x"]}"#.into()),
            SqlValue::Bool(true),
        ],
    };
    t.commit_rows("o", 0, &[], &owner, &[row("a", "ada"), row("b", "bob")], None)
        .unwrap();
    t.commit_rows(
        "o",
        1,
        &[],
        &owner,
        &[RowChange::Close {
            table: def.name.clone(),
            key: b"b".to_vec(),
            at: 4,
        }],
        None,
    )
    .unwrap();
    let url = h.url.clone().unwrap();
    let plain = url.split('?').next().unwrap().to_string();
    let mut c = postgres::Client::connect(&format!("{plain}?sslmode=disable"), postgres::NoTls).unwrap();
    let rows = c
        .query(
            &format!(
                "select node, member, who, k::text, at, doc->'fields'->>0, ok from \"{}\".log",
                h.schema
            ),
            &[],
        )
        .unwrap();
    assert_eq!(rows.len(), 1, "only the current row");
    let r = &rows[0];
    assert_eq!(r.get::<_, String>(0), "rooms");
    assert_eq!(r.get::<_, String>(1), "lunch");
    assert_eq!(r.get::<_, String>(2), "ada");
    assert_eq!(r.get::<_, String>(3), "18446744073709551615");
    assert_eq!(r.get::<_, i64>(4), -5);
    assert_eq!(r.get::<_, String>(5), "x");
    assert!(r.get::<_, bool>(6));
}
