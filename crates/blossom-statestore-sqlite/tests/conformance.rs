//! The SQLite store against the conformance suite, each case in a fresh file.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use blossom_statestore::conformance::Harness;
use blossom_statestore::{StateError, StateStore};
use blossom_statestore_sqlite::SqliteStore;

struct Sqlite(PathBuf);

impl Harness for Sqlite {
    fn open(&self) -> Result<Box<dyn StateStore>, StateError> {
        if let Some(dir) = self.0.parent() {
            std::fs::create_dir_all(dir).map_err(|e| StateError::Config(format!("{}: {e}", dir.display())))?;
        }
        Ok(Box::new(SqliteStore::open(&self.0)?))
    }
}

impl Drop for Sqlite {
    fn drop(&mut self) {
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn fresh() -> Sqlite {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "blossom-statestore-sqlite-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    Sqlite(dir.join("state.db"))
}

blossom_statestore::statestore_conformance!(fresh);

#[test]
fn a_file_of_another_format_is_refused() {
    let h = fresh();
    h.open().unwrap();
    let conn = rusqlite::Connection::open(&h.0).unwrap();
    conn.execute("update blossom_meta set value = 99 where key = 'format'", [])
        .unwrap();
    drop(conn);
    match SqliteStore::open(&h.0) {
        Err(StateError::Config(m)) => assert!(m.contains("format 99"), "{m}"),
        other => panic!("opened a store of another format: {:?}", other.err()),
    }
}

#[test]
fn urls() {
    assert!(matches!(
        SqliteStore::from_url("postgres://x"),
        Err(StateError::Config(_))
    ));
    assert!(matches!(SqliteStore::from_url("sqlite:"), Err(StateError::Config(_))));
    let h = fresh();
    h.open().unwrap();
    SqliteStore::from_url(&format!("sqlite:{}", h.0.display())).unwrap();
}

#[test]
fn many_processes_create_one_file_at_once() {
    // Handles opened at once on a file that does not exist yet (instances starting together) all open it.
    let h = fresh();
    std::fs::create_dir_all(h.0.parent().unwrap()).unwrap();
    let path = h.0.clone();
    let opened: Vec<Result<(), String>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..12)
            .map(|_| {
                let path = path.clone();
                s.spawn(move || {
                    let store = SqliteStore::open(&path).map_err(|e| e.to_string())?;
                    store.version("o").map(drop).map_err(|e| e.to_string())
                })
            })
            .collect();
        hs.into_iter().map(|j| j.join().unwrap()).collect()
    });
    for r in opened {
        r.unwrap();
    }
}

mod tables {
    use super::*;
    blossom_statestore::statestore_conformance!(tables, fresh);
}

/// What SQL readers see: the current-rows view, its typed columns holding the values committed.
#[test]
fn the_view_shows_current_rows_typed() {
    use blossom_statestore::{Owner, RowChange, SqlType, SqlValue, TableDef};
    let h = fresh();
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
    let conn = rusqlite::Connection::open(&h.0).unwrap();
    let got: Vec<(String, String, String, String, i64, String, bool)> = conn
        .prepare("select node, member, who, k, at, json_extract(doc, '$.fields[0]'), ok from log")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        got,
        vec![(
            "rooms".into(),
            "lunch".into(),
            "ada".into(),
            "18446744073709551615".into(),
            -5,
            "x".into(),
            true
        )]
    );
}
