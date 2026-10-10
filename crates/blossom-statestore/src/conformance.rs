//! The suite every [`StateStore`] runs (docs/design/STATELESS.md §3): what a commit promises, under concurrent
//! committers too; waits, wake hints and side records; large values and keys that sort strangely; and a second handle
//! that shares nothing in memory with the first.
//!
//! An adapter's tests implement [`Harness`] (a fresh namespace in its storage, and handles onto it) and expand
//! [`statestore_conformance!`](crate::statestore_conformance), one test per case. Every case names its objects and
//! keys under its own prefix, so cases may run in parallel in one namespace.

use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use crate::{Commit, Snapshot, StateError, StateStore, Write};

/// Handles onto one namespace of an adapter's storage.
pub trait Harness: Sync {
    /// A new handle, sharing nothing in memory with the others but the storage.
    fn open(&self) -> Result<Box<dyn StateStore>, StateError>;
}

/// A case's failure.
pub type Outcome = Result<(), String>;

fn e(err: StateError) -> String {
    err.to_string()
}

fn put(k: &str, v: &[u8]) -> Write {
    Write::Put(k.to_owned(), v.to_vec())
}

fn del(k: &str) -> Write {
    Write::Delete(k.to_owned())
}

fn expect<T: PartialEq + std::fmt::Debug>(what: &str, got: T, want: T) -> Outcome {
    if got == want {
        Ok(())
    } else {
        Err(format!("{what}: got {got:?}, want {want:?}"))
    }
}

fn entries(pairs: &[(&str, &[u8])]) -> Vec<(String, Vec<u8>)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_vec())).collect()
}

/// An object never committed is at version 0 with no entries.
pub fn unknown_object(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    expect("load", s.load("unknown/o").map_err(e)?, Snapshot::default())?;
    expect("version", s.version("unknown/o").map_err(e)?, 0)
}

/// A commit at version 0 creates the object; every handle loads it, entries in key order.
pub fn commit_and_load(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    let c = s.commit("load/o", 0, &[put("b", b"2"), put("a", b"1")]).map_err(e)?;
    expect("commit", c, Commit::Done { version: 1 })?;
    let want = Snapshot {
        version: 1,
        entries: entries(&[("a", b"1"), ("b", b"2")]),
    };
    expect("load", s.load("load/o").map_err(e)?, want.clone())?;
    let other = h.open().map_err(e)?;
    expect("load through another handle", other.load("load/o").map_err(e)?, want)?;
    expect("version through another handle", other.version("load/o").map_err(e)?, 1)
}

/// A commit at a version the object is not at changes nothing and reports the version there.
pub fn version_check(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    s.commit("check/o", 0, &[put("a", b"1")]).map_err(e)?;
    expect(
        "a second creation",
        s.commit("check/o", 0, &[put("a", b"x")]).map_err(e)?,
        Commit::Conflict { current: 1 },
    )?;
    expect(
        "a commit from the future",
        s.commit("check/o", 7, &[put("a", b"y")]).map_err(e)?,
        Commit::Conflict { current: 1 },
    )?;
    expect(
        "a commit to an object never committed, at a later version",
        s.commit("check/never", 3, &[put("a", b"z")]).map_err(e)?,
        Commit::Conflict { current: 0 },
    )?;
    expect(
        "the never-committed object",
        s.load("check/never").map_err(e)?,
        Snapshot::default(),
    )?;
    expect(
        "the object",
        s.load("check/o").map_err(e)?,
        Snapshot {
            version: 1,
            entries: entries(&[("a", b"1")]),
        },
    )
}

/// Writes apply in order: a later write to a key wins, deleting a missing key does nothing, and an empty commit still
/// raises the version. Deleting every entry keeps the object's version.
pub fn writes_in_order(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    s.commit("order/o", 0, &[put("a", b"1"), put("b", b"1"), put("c", b"1")])
        .map_err(e)?;
    let c = s
        .commit(
            "order/o",
            1,
            &[
                put("a", b"2"),
                del("b"),
                del("missing"),
                put("x", b"1"),
                put("x", b"2"),
                put("y", b"1"),
                del("y"),
            ],
        )
        .map_err(e)?;
    expect("commit", c, Commit::Done { version: 2 })?;
    expect(
        "load",
        s.load("order/o").map_err(e)?,
        Snapshot {
            version: 2,
            entries: entries(&[("a", b"2"), ("c", b"1"), ("x", b"2")]),
        },
    )?;
    expect(
        "empty commit",
        s.commit("order/o", 2, &[]).map_err(e)?,
        Commit::Done { version: 3 },
    )?;
    s.commit("order/o", 3, &[del("a"), del("c"), del("x")]).map_err(e)?;
    expect(
        "emptied",
        s.load("order/o").map_err(e)?,
        Snapshot {
            version: 4,
            entries: Vec::new(),
        },
    )?;
    expect("version after emptying", s.version("order/o").map_err(e)?, 4)
}

/// Objects are independent: a commit to one changes no other, whatever their names share.
pub fn objects_apart(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    s.commit("apart/o", 0, &[put("k", b"o")]).map_err(e)?;
    s.commit("apart/o/x", 0, &[put("k", b"ox")]).map_err(e)?;
    s.commit("apart/o2", 0, &[put("k", b"o2")]).map_err(e)?;
    s.commit("apart/o", 1, &[put("k", b"o!")]).map_err(e)?;
    expect(
        "apart/o/x",
        s.load("apart/o/x").map_err(e)?,
        Snapshot {
            version: 1,
            entries: entries(&[("k", b"ox")]),
        },
    )?;
    expect(
        "apart/o2",
        s.load("apart/o2").map_err(e)?,
        Snapshot {
            version: 1,
            entries: entries(&[("k", b"o2")]),
        },
    )?;
    expect(
        "apart/o",
        s.load("apart/o").map_err(e)?,
        Snapshot {
            version: 2,
            entries: entries(&[("k", b"o!")]),
        },
    )
}

/// A request with a name a store cannot hold is refused, and nothing of it is applied.
pub fn names_checked(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    let refused = |r: Result<Commit, StateError>| matches!(r, Err(StateError::Invalid(_)));
    if !refused(s.commit("", 0, &[put("a", b"1")])) {
        return Err("an empty object name was taken".into());
    }
    if !refused(s.commit("names/o", 0, &[put("a", b"1"), put("b\0c", b"1")])) {
        return Err("a key with a NUL was taken".into());
    }
    if !refused(s.commit("names/o", 0, &[put(&"k".repeat(crate::MAX_NAME + 1), b"1")])) {
        return Err("a key past the longest was taken".into());
    }
    expect("after refusals", s.load("names/o").map_err(e)?, Snapshot::default())
}

/// Keys that share prefixes, end in `/`, hold spaces, escapes and non-ASCII come back exactly, in byte order.
pub fn strange_keys(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    let long = "z".repeat(crate::MAX_NAME);
    let mut keys = vec![
        "a", "a/", "a/b", "a/b/c", "ab", "a b", "%2F", "é", "日本", "A", "_", "~", ".", "..", "a.b", &long,
    ];
    let writes: Vec<Write> = keys.iter().map(|k| put(k, k.as_bytes())).collect();
    s.commit("strange/o/é/..", 0, &writes).map_err(e)?;
    keys.sort_unstable();
    let want: Vec<(String, Vec<u8>)> = keys.iter().map(|k| (k.to_string(), k.as_bytes().to_vec())).collect();
    expect(
        "keys",
        h.open().map_err(e)?.load("strange/o/é/..").map_err(e)?.entries,
        want,
    )
}

/// Large values and many keys round-trip byte for byte.
pub fn large_values(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    let big: Vec<u8> = (0..5 * 1024 * 1024u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let mut writes = vec![put("big", &big), put("empty", b"")];
    for i in 0..300u32 {
        writes.push(put(
            &format!("k/{i:05}"),
            &i.to_le_bytes().repeat((i % 7 + 1) as usize * 100),
        ));
    }
    s.commit("large/o", 0, &writes).map_err(e)?;
    let got = h.open().map_err(e)?.load("large/o").map_err(e)?;
    expect("entries", got.entries.len(), 302)?;
    let find = |k: &str| got.entries.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    if find("big").as_deref() != Some(&big[..]) {
        return Err("the 5 MiB value came back changed".into());
    }
    expect("the empty value", find("empty"), Some(Vec::new()))?;
    for i in 0..300u32 {
        let want = i.to_le_bytes().repeat((i % 7 + 1) as usize * 100);
        if find(&format!("k/{i:05}")) != Some(want) {
            return Err(format!("k/{i:05} came back changed"));
        }
    }
    // Overwriting the large value with a small one, and back.
    s.commit("large/o", 1, &[put("big", b"small")]).map_err(e)?;
    expect("shrunk", find_in(&*s, "large/o", "big")?, Some(b"small".to_vec()))?;
    s.commit("large/o", 2, &[put("big", &big)]).map_err(e)?;
    if find_in(&*s, "large/o", "big")?.as_deref() != Some(&big[..]) {
        return Err("the value written again came back changed".into());
    }
    Ok(())
}

fn find_in(s: &dyn StateStore, object: &str, key: &str) -> Result<Option<Vec<u8>>, String> {
    Ok(s.load(object)
        .map_err(e)?
        .entries
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v))
}

/// Committers on several handles race on one object, each reading a counter and writing it back plus one: every
/// version has exactly one winner, no update is lost, and every winner's own writes are there.
pub fn concurrent_committers(h: &dyn Harness) -> Outcome {
    const THREADS: usize = 6;
    const EACH: u64 = 12;
    let stores: Vec<Box<dyn StateStore>> = (0..THREADS).map(|_| h.open()).collect::<Result<_, _>>().map_err(e)?;
    let start = Arc::new(Barrier::new(THREADS));
    let results: Vec<Result<u64, String>> = thread::scope(|scope| {
        let handles: Vec<_> = stores
            .iter()
            .enumerate()
            .map(|(t, s)| {
                let start = start.clone();
                scope.spawn(move || -> Result<u64, String> {
                    start.wait();
                    let mut conflicts = 0u64;
                    let mut done = 0u64;
                    while done < EACH {
                        let snap = s.load("race/o").map_err(e)?;
                        let counter = snap
                            .entries
                            .iter()
                            .find(|(k, _)| k == "counter")
                            .map(|(_, v)| {
                                <[u8; 8]>::try_from(v.as_slice())
                                    .map(u64::from_le_bytes)
                                    .map_err(|_| "a counter that is not 8 bytes".to_string())
                            })
                            .transpose()?
                            .unwrap_or(0);
                        let writes = [
                            put("counter", &(counter + 1).to_le_bytes()),
                            put(&format!("t{t}/{done:03}"), &snap.version.to_le_bytes()),
                        ];
                        match s.commit("race/o", snap.version, &writes).map_err(e)? {
                            Commit::Done { version } if version == snap.version + 1 => done += 1,
                            Commit::Done { version } => {
                                return Err(format!("a commit at {} went to version {version}", snap.version));
                            }
                            Commit::Conflict { current } if current > snap.version => conflicts += 1,
                            Commit::Conflict { current } => {
                                return Err(format!(
                                    "a conflict at {} reported version {current}, not later",
                                    snap.version
                                ));
                            }
                        }
                    }
                    Ok(conflicts)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|j| j.join().unwrap_or_else(|_| Err("a committer panicked".into())))
            .collect()
    });
    for r in results {
        r?;
    }
    let total = THREADS as u64 * EACH;
    let snap = h.open().map_err(e)?.load("race/o").map_err(e)?;
    expect("version", snap.version, total)?;
    let counter = snap
        .entries
        .iter()
        .find(|(k, _)| k == "counter")
        .map(|(_, v)| v.clone());
    expect("counter", counter, Some(total.to_le_bytes().to_vec()))?;
    // Each winner wrote its key at the version it won from: every version from 0 to total - 1, once.
    let mut from: Vec<u64> = snap
        .entries
        .iter()
        .filter(|(k, _)| k.starts_with('t'))
        .map(|(_, v)| {
            <[u8; 8]>::try_from(v.as_slice())
                .map(u64::from_le_bytes)
                .map_err(|_| "a winner's value that is not 8 bytes".to_string())
        })
        .collect::<Result<_, _>>()?;
    from.sort_unstable();
    expect(
        "the versions the winners committed from",
        from,
        (0..total).collect::<Vec<_>>(),
    )
}

/// `wait` returns at once for a version already past, when another handle commits, and after its timeout otherwise.
pub fn waits(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    s.commit("wait/o", 0, &[put("a", b"1")]).map_err(e)?;
    expect(
        "already past",
        s.wait("wait/o", 0, Duration::from_secs(30)).map_err(e)?,
        1,
    )?;
    expect(
        "a timeout with nothing new",
        s.wait("wait/o", 1, Duration::from_millis(200)).map_err(e)?,
        1,
    )?;
    let other = h.open().map_err(e)?;
    let woke = thread::scope(|scope| {
        let waiter = scope.spawn(|| s.wait("wait/o", 1, Duration::from_secs(20)));
        thread::sleep(Duration::from_millis(300));
        let c = other.commit("wait/o", 1, &[put("a", b"2")]);
        (c, waiter.join())
    });
    match woke {
        (Ok(Commit::Done { version: 2 }), Ok(Ok(2))) => Ok(()),
        (c, w) => Err(format!("a commit while waiting: commit {c:?}, wait {w:?}")),
    }
}

/// The time a waiter took to hear a commit made through another handle is bounded (a second; the S3 adapter polls).
pub fn wait_is_prompt(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    let other = h.open().map_err(e)?;
    s.commit("prompt/o", 0, &[]).map_err(e)?;
    for round in 1..=3u64 {
        let (woke, waited) = thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                let started = Stopwatch::start();
                let v = s.wait("prompt/o", round, Duration::from_secs(20));
                (v, started.elapsed())
            });
            thread::sleep(Duration::from_millis(100));
            let c = other.commit("prompt/o", round, &[put("r", &round.to_le_bytes())]);
            match waiter.join() {
                Ok((v, t)) => ((c, v), t),
                Err(_) => (
                    (c, Err(StateError::Unavailable("the waiter panicked".into()))),
                    Duration::MAX,
                ),
            }
        });
        match woke {
            (Ok(Commit::Done { .. }), Ok(v)) if v == round + 1 => {}
            other => return Err(format!("round {round}: {other:?}")),
        }
        if waited > Duration::from_millis(1100) {
            return Err(format!("round {round}: the waiter heard the commit after {waited:?}"));
        }
    }
    Ok(())
}

/// Wake hints are a set; `due` lists those due, earliest first, at most `limit`; `unschedule` removes one.
pub fn wakes(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    let mine = |v: Vec<(String, u64)>| -> Vec<(String, u64)> {
        v.into_iter().filter(|(o, _)| o.starts_with("wakes/")).collect()
    };
    s.schedule("wakes/a", 1_000_100).map_err(e)?;
    s.schedule("wakes/b", 1_000_050).map_err(e)?;
    s.schedule("wakes/a", 1_000_100).map_err(e)?;
    s.schedule("wakes/a", 1_000_300).map_err(e)?;
    s.schedule("wakes/c/é", 1_000_100).map_err(e)?;
    let other = h.open().map_err(e)?;
    expect(
        "due at 1_000_100",
        mine(other.due(1_000_100, 1000).map_err(e)?),
        vec![
            ("wakes/b".to_string(), 1_000_050),
            ("wakes/a".to_string(), 1_000_100),
            ("wakes/c/é".to_string(), 1_000_100),
        ],
    )?;
    expect(
        "nothing due earlier",
        mine(other.due(1_000_049, 1000).map_err(e)?),
        Vec::new(),
    )?;
    other.unschedule("wakes/a", 1_000_100).map_err(e)?;
    other.unschedule("wakes/a", 999).map_err(e)?;
    expect(
        "due at 1_000_300",
        mine(s.due(1_000_300, 1000).map_err(e)?),
        vec![
            ("wakes/b".to_string(), 1_000_050),
            ("wakes/c/é".to_string(), 1_000_100),
            ("wakes/a".to_string(), 1_000_300),
        ],
    )?;
    // A limit takes the earliest; the hints of other cases may come first, so it is checked on the whole answer.
    let limited = s.due(1_000_300, 2).map_err(e)?;
    expect("a limit of 2", limited.len().min(2), limited.len())?;
    let all = s.due(1_000_300, 1000).map_err(e)?;
    if !all.windows(2).all(|w| w.first().map(|a| a.1) <= w.get(1).map(|b| b.1)) {
        return Err(format!("due not in time order: {all:?}"));
    }
    for (o, at) in mine(all) {
        s.unschedule(&o, at).map_err(e)?;
    }
    expect("all removed", mine(s.due(u64::MAX, 1000).map_err(e)?), Vec::new())
}

/// Side records: last write wins, delete removes, a missing one is `None`.
pub fn side_records(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    expect("missing", s.get_side("side/a").map_err(e)?, None)?;
    s.put_side("side/a", b"1").map_err(e)?;
    s.put_side("side/a", b"2").map_err(e)?;
    s.put_side("side/b/é", b"").map_err(e)?;
    let other = h.open().map_err(e)?;
    expect("overwritten", other.get_side("side/a").map_err(e)?, Some(b"2".to_vec()))?;
    expect("empty", other.get_side("side/b/é").map_err(e)?, Some(Vec::new()))?;
    other.delete_side("side/a").map_err(e)?;
    other.delete_side("side/never").map_err(e)?;
    expect("deleted", s.get_side("side/a").map_err(e)?, None)
}

// ---------------------------------------------------------------------------------------------------- tables

use crate::tables::{Owner, RowChange, SqlType, SqlValue, TableDef, TableStore};

fn tables_of(s: &dyn StateStore) -> Result<&dyn TableStore, String> {
    s.tables().ok_or_else(|| "the store has no tables".to_string())
}

fn def(name: &str) -> TableDef {
    TableDef {
        name: name.into(),
        view: Some(format!("{name}_now")),
        columns: vec![
            ("who".into(), SqlType::Text),
            ("n".into(), SqlType::Int),
            ("big".into(), SqlType::Numeric),
            ("x".into(), SqlType::Real),
            ("ok".into(), SqlType::Bool),
            ("raw".into(), SqlType::Bytes),
            ("doc".into(), SqlType::Json),
        ],
    }
}

fn values(i: i64) -> Vec<SqlValue> {
    vec![
        SqlValue::Text(format!("who {i}")),
        SqlValue::Int(i),
        SqlValue::Numeric(format!("1844674407370955161{i}")),
        SqlValue::Real(i as f64 / 2.0),
        SqlValue::Bool(i % 2 == 0),
        SqlValue::Bytes(vec![0, 0xff, i as u8]),
        SqlValue::Json(format!("{{\"variant\": {i}, \"fields\": []}}")),
    ]
}

fn open(table: &str, key: &[u8], from: u64, i: i64) -> RowChange {
    RowChange::Open {
        table: table.into(),
        key: key.to_vec(),
        from,
        values: values(i),
    }
}

fn close(table: &str, key: &[u8], at: u64) -> RowChange {
    RowChange::Close {
        table: table.into(),
        key: key.to_vec(),
        at,
    }
}

fn owner(node: &str, member: &str) -> Owner {
    Owner {
        node: node.into(),
        member: member.into(),
    }
}

/// Tables are created once (again: nothing changes), with their views; another deployment's are refused, and so are
/// names a store cannot hold.
pub fn tables_created(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    let t = tables_of(&*s)?;
    t.ensure_tables("dep-a", &[def("r_log_00aa"), def("r_seen_00bb")])
        .map_err(e)?;
    t.ensure_tables("dep-a", &[def("r_log_00aa"), def("r_seen_00bb")])
        .map_err(e)?;
    if t.ensure_tables("dep-b", &[def("r_log_00aa")]).is_ok() {
        return Err("a second deployment's tables were taken".into());
    }
    let mut bad = def("r_bad");
    bad.columns.push(("key".into(), SqlType::Text));
    if t.ensure_tables("dep-a", &[bad]).is_ok() {
        return Err("a column named as a system column was taken".into());
    }
    for name in ["blossom_objects", "1abc", "a-b", "a\"b"] {
        let mut d = def("r_ok");
        d.name = name.into();
        if t.ensure_tables("dep-a", &[d]).is_ok() {
            return Err(format!("the table name `{name}` was taken"));
        }
    }
    let other = h.open().map_err(e)?;
    tables_of(&*other)?
        .ensure_tables("dep-a", &[def("r_log_00aa")])
        .map_err(e)?;
    Ok(())
}

/// Rows open and close at ticks, in the commit of their object: present from the tick they opened until the one
/// they closed; reads as of every tick, by range, with a limit, apart per owner.
pub fn rows_in_commits(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    let t = tables_of(&*s)?;
    t.ensure_tables("dep-a", &[def("r_log_00aa")]).map_err(e)?;
    let (lunch, dinner) = (owner("rooms", "lunch"), owner("rooms", "dinner"));
    let tbl = "r_log_00aa";
    let c = t
        .commit_rows(
            "tables/lunch",
            0,
            &[put("e", b"1")],
            &lunch,
            &[
                open(tbl, b"\x01a", 5, 1),
                open(tbl, b"\x01b", 5, 2),
                open(tbl, b"\x01c", 6, 3),
            ],
            None,
        )
        .map_err(e)?;
    expect("first commit", c, Commit::Done { version: 1 })?;
    t.commit_rows("tables/dinner", 0, &[], &dinner, &[open(tbl, b"\x01a", 2, 9)], None)
        .map_err(e)?;
    // In one commit: b closes at 7, opens again at 9; a closes at 8.
    t.commit_rows(
        "tables/lunch",
        1,
        &[],
        &lunch,
        &[
            close(tbl, b"\x01b", 7),
            close(tbl, b"\x01a", 8),
            open(tbl, b"\x01b", 9, 4),
        ],
        None,
    )
    .map_err(e)?;
    let other = h.open().map_err(e)?;
    let o = tables_of(&*other)?;
    let at = |tick: u64| o.scan_keys(tbl, &lunch, b"", None, tick, 100).map_err(e);
    expect("as of 4", at(4)?, Vec::<Vec<u8>>::new())?;
    expect("as of 5", at(5)?, vec![b"\x01a".to_vec(), b"\x01b".to_vec()])?;
    expect(
        "as of 6",
        at(6)?,
        vec![b"\x01a".to_vec(), b"\x01b".to_vec(), b"\x01c".to_vec()],
    )?;
    expect("as of 7", at(7)?, vec![b"\x01a".to_vec(), b"\x01c".to_vec()])?;
    expect("as of 8", at(8)?, vec![b"\x01c".to_vec()])?;
    expect("as of 9", at(9)?, vec![b"\x01b".to_vec(), b"\x01c".to_vec()])?;
    expect(
        "a range",
        o.scan_keys(tbl, &lunch, b"\x01b", Some(b"\x01c"), 9, 100).map_err(e)?,
        vec![b"\x01b".to_vec()],
    )?;
    expect(
        "a limit",
        o.scan_keys(tbl, &lunch, b"", None, 6, 2).map_err(e)?,
        vec![b"\x01a".to_vec(), b"\x01b".to_vec()],
    )?;
    expect(
        "dinner apart",
        o.scan_keys(tbl, &dinner, b"", None, 9, 100).map_err(e)?,
        vec![b"\x01a".to_vec()],
    )?;
    expect("has b at 7", o.has_key(tbl, &lunch, b"\x01b", 7).map_err(e)?, false)?;
    expect("has b at 9", o.has_key(tbl, &lunch, b"\x01b", 9).map_err(e)?, true)?;
    expect(
        "has a for another member",
        o.has_key(tbl, &owner("rooms", "x"), b"\x01a", 9).map_err(e)?,
        false,
    )?;
    // The object's entries went with its rows.
    expect("the entries", other.load("tables/lunch").map_err(e)?.version, 2)
}

/// A commit at the wrong version applies none of its rows; one whose row change cannot apply (closing a key that
/// is not open) applies nothing at all.
pub fn rows_atomic(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    let t = tables_of(&*s)?;
    t.ensure_tables("dep-a", &[def("r_log_00aa")]).map_err(e)?;
    let me = owner("n", "");
    let tbl = "r_log_00aa";
    t.commit_rows("tables/atomic", 0, &[], &me, &[open(tbl, b"k1", 1, 1)], None)
        .map_err(e)?;
    expect(
        "a stale commit",
        t.commit_rows(
            "tables/atomic",
            0,
            &[put("x", b"1")],
            &me,
            &[open(tbl, b"k2", 2, 2)],
            None,
        )
        .map_err(e)?,
        Commit::Conflict { current: 1 },
    )?;
    let bad = t.commit_rows(
        "tables/atomic",
        1,
        &[put("x", b"1")],
        &me,
        &[open(tbl, b"k3", 2, 3), close(tbl, b"never", 2)],
        None,
    );
    if bad.is_ok() {
        return Err("closing a key that is not open was taken".into());
    }
    expect(
        "after both",
        t.scan_keys(tbl, &me, b"", None, 5, 10).map_err(e)?,
        vec![b"k1".to_vec()],
    )?;
    expect("the version", s.version("tables/atomic").map_err(e)?, 1)?;
    expect("the entries", s.load("tables/atomic").map_err(e)?.entries, Vec::new())
}

/// Pruning deletes the owner's rows that ended at or before the floor, and nothing else.
pub fn rows_pruned(h: &dyn Harness) -> Outcome {
    let s = h.open().map_err(e)?;
    let t = tables_of(&*s)?;
    t.ensure_tables("dep-a", &[def("r_log_00aa")]).map_err(e)?;
    let (me, them) = (owner("n", "me"), owner("n", "them"));
    let tbl = "r_log_00aa";
    t.commit_rows(
        "tables/prune-me",
        0,
        &[],
        &me,
        &[
            open(tbl, b"a", 1, 1),
            open(tbl, b"b", 1, 2),
            close(tbl, b"a", 3),
            close(tbl, b"b", 8),
        ],
        None,
    )
    .map_err(e)?;
    t.commit_rows(
        "tables/prune-them",
        0,
        &[],
        &them,
        &[open(tbl, b"a", 1, 1), close(tbl, b"a", 2)],
        None,
    )
    .map_err(e)?;
    t.commit_rows("tables/prune-me", 1, &[], &me, &[open(tbl, b"c", 9, 3)], Some(5))
        .map_err(e)?;
    // As of 2 the pruned row is gone for me (history before the floor is not kept), not for them.
    expect(
        "mine as of 2",
        t.scan_keys(tbl, &me, b"", None, 2, 10).map_err(e)?,
        vec![b"b".to_vec()],
    )?;
    expect(
        "theirs as of 1",
        t.scan_keys(tbl, &them, b"", None, 1, 10).map_err(e)?,
        vec![b"a".to_vec()],
    )?;
    expect(
        "mine as of 9",
        t.scan_keys(tbl, &me, b"", None, 9, 10).map_err(e)?,
        vec![b"c".to_vec()],
    )
}

/// Elapsed time for the promptness check (the case measures a host's I/O, not a node's time).
struct Stopwatch(std::time::Instant);

impl Stopwatch {
    #[allow(clippy::disallowed_methods)] // measures the store's latency, not a node's clock
    fn start() -> Stopwatch {
        Stopwatch(std::time::Instant::now())
    }
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}

/// One test per conformance case, each on a fresh harness: `statestore_conformance!(|| make_harness())`. A suite
/// that needs a service the fast tier does not start is ignored there:
/// `statestore_conformance!(ignore = "needs Postgres: scripts/test-services.sh", || make_harness())`.
#[macro_export]
macro_rules! statestore_conformance {
    (ignore = $why:literal, $make:expr) => {
        $crate::statestore_conformance!(@all [ignore = $why] $make);
    };
    ($make:expr) => {
        $crate::statestore_conformance!(@all [] $make);
    };
    (@all [$($attr:meta)*] $make:expr) => {
        $crate::statestore_conformance!(@cases [$($attr)*] $make;
            unknown_object, commit_and_load, version_check, writes_in_order, objects_apart, names_checked,
            strange_keys, large_values, concurrent_committers, waits, wait_is_prompt, wakes, side_records);
    };
    (tables, ignore = $why:literal, $make:expr) => {
        $crate::statestore_conformance!(@cases [ignore = $why] $make;
            tables_created, rows_in_commits, rows_atomic, rows_pruned);
    };
    (tables, $make:expr) => {
        $crate::statestore_conformance!(@cases [] $make;
            tables_created, rows_in_commits, rows_atomic, rows_pruned);
    };
    (@cases $attrs:tt $make:expr; $($case:ident),*) => {
        $( $crate::statestore_conformance!(@one $attrs $make; $case); )*
    };
    (@one [$($attr:meta)*] $make:expr; $case:ident) => {
        #[test]
        $(#[$attr])*
        fn $case() {
            let harness = ($make)();
            if let Err(err) = $crate::conformance::$case(&harness) {
                panic!("{}: {err}", stringify!($case));
            }
        }
    };
}
