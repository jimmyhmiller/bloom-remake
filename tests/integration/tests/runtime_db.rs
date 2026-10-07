//! S23: the node's database (docs/design/DATABASE.md). A real node runs the chat fixture; a member's lines go to the
//! durable `log`, and so to the database at the ticks that wrote them. The database agrees with what was said, reads
//! as of an earlier tick see the log as it was then, and the rows survive a flush, a restart (tables and WAL replay
//! together) and a second restart after more lines.

use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_artifact::client::ClientArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_integration_tests::ws::Ws;
use blossom_runtime::deploy::DeploymentSpec;
use blossom_runtime::server::{Server, ServerConfig, WebConfig, identity};
use blossom_store::OpenMode;
use blossom_value::Value;
use blossom_value::time::NodeId;
use blossom_wire::frame::{Frame, Peer};
use blossom_wire::link::{Catalog, batches, wire_codec};

#[cfg(test)]
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[cfg(test)]
fn setup(name: &str) -> (DeploymentSpec, Arc<BlsArtifact>) {
    let dir = std::env::temp_dir().join(format!("blossom-db-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = dir.join("chat.secrets");
    std::fs::write(&secrets, "seed = \"00112233445566778899aabbccddeeff\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/clients/chat.bls");
    let text = format!(
        "format = 1\n[deployment]\nid = \"chat-db\"\nprogram = \"chat\"\nversion = 1\nsource = \"{}\"\n\
         secrets = \"chat.secrets\"\n[[node]]\nname = \"s\"\nrole = \"Server\"\naddr = \"127.0.0.1:{}\"\nprincipal = \"spiffe://test/chat/Server/s\"\n\
         [security]\nmode = \"insecure-dev\"\n[storage]\ndata_dir = \"data\"\n",
        source.display(),
        free_port(),
    );
    let spec = DeploymentSpec::parse(&text, &dir).unwrap();
    let nodes: Vec<NodeSpec> = spec
        .nodes
        .iter()
        .map(|n| NodeSpec {
            name: n.name.clone(),
            role: n.role.clone(),
        })
        .collect();
    let (compiled, _) = compile_file(&spec.source.to_string_lossy(), &nodes);
    (spec, Arc::new(compiled.unwrap().0))
}

#[cfg(test)]
fn start(spec: &DeploymentSpec, a: &Arc<BlsArtifact>, mode: OpenMode, port: u16) -> Server {
    Server::start(ServerConfig {
        spec: spec.clone(),
        artifact: a.clone(),
        node: "s".into(),
        mode,
        dir: None,
        backend: blossom_node::Backend::Engine,
        externs: Arc::new(blossom_std_host::registry().unwrap()),
        record: None,
        admin: None,
        web: Some(WebConfig {
            addr: format!("127.0.0.1:{port}").parse().unwrap(),
            root: None,
        }),
    })
    .unwrap()
}

/// A member's link: its connection, identity and token.
#[cfg(test)]
fn open(port: u16, spec: &DeploymentSpec, a: &BlsArtifact, token: Option<Vec<u8>>) -> (Ws, NodeId, Vec<u8>) {
    let mut ws = Ws::connect(port).unwrap();
    let peer = Peer::Member {
        role: "Browser".into(),
        part: ClientArtifact::project(a, "Browser").unwrap().part(),
        token,
        received: 0,
        acked: 0,
    };
    let catalog = Catalog::of(a.program.get()).unwrap();
    ws.send(&blossom_wire::link::hello(&identity(spec, a), peer, 0, 0, &catalog))
        .unwrap();
    loop {
        if let Frame::Welcome { member, token, .. } = ws.recv().unwrap() {
            return (ws, NodeId(member), token);
        }
    }
}

/// Says `text` as batch `seq`, and waits for its acknowledgement (the tick that took it is released).
#[cfg(test)]
fn say(ws: &mut Ws, a: &BlsArtifact, server: NodeId, seq: u64, text: &str) {
    let p = a.program.get();
    let rel = a.rel_named("say").unwrap();
    let sid = Catalog::of(p).unwrap().sid(rel).unwrap();
    let row: blossom_oracle::Row = Arc::from(vec![Value::Node(server), Value::str(text)]);
    let (mut bs, _) = batches(&wire_codec(p), p, sid, rel, 0, &[&row]).unwrap();
    ws.send(&Frame::Msg {
        seq,
        batch: bs.remove(0),
    })
    .unwrap();
    for _ in 0..1000 {
        match ws.recv().unwrap() {
            Frame::Ack { seq: s } if s >= seq => return,
            Frame::Msg { seq: s, .. } => ws.send(&Frame::Ack { seq: s }).unwrap(),
            _ => {}
        }
    }
    panic!("no acknowledgement of batch {seq}");
}

/// The log's texts in the database as of `tick` (the newest when `None`), sorted.
#[cfg(test)]
fn log(server: &Server, a: &BlsArtifact, tick: Option<u64>) -> Vec<String> {
    let db = &server.database;
    let tick = tick.unwrap_or_else(|| db.range().unwrap().1.unwrap());
    let rel = a.rel_named("log").unwrap();
    let mut out: Vec<String> = db
        .rows(rel, &[], tick)
        .unwrap()
        .iter()
        .map(|r| match &r[1] {
            Value::Str(s) => s.to_string(),
            other => panic!("{other:?}"),
        })
        .collect();
    out.sort();
    out
}

#[test]
fn the_database_holds_the_durable_rows_as_of_every_tick_and_survives_restarts() {
    let (spec, a) = setup("chat");
    let port = free_port();
    let server = start(&spec, &a, OpenMode::InitFresh, port);
    let s = a.node_id("s").unwrap();
    let (mut ws, me, token) = open(port, &spec, &a, None);
    say(&mut ws, &a, s, 1, "first");
    let after_first = server.database.range().unwrap().1.unwrap();
    say(&mut ws, &a, s, 2, "second");
    say(&mut ws, &a, s, 3, "third");
    assert_eq!(log(&server, &a, None), ["first", "second", "third"]);
    assert_eq!(log(&server, &a, Some(after_first)), ["first"]);
    // Prefix reads: the log rows of this member (the first column bound).
    let rel = a.rel_named("log").unwrap();
    assert_eq!(
        server
            .database
            .rows(rel, &[Value::Node(me)], after_first)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        server
            .database
            .rows(rel, &[Value::Node(NodeId(0))], after_first)
            .unwrap()
            .len(),
        0
    );
    // Flushed to a table, then more in the memtable only (the WAL holds it).
    server.database.flush().unwrap();
    say(&mut ws, &a, s, 4, "fourth");
    drop(ws);
    server.stop().unwrap();
    let server = start(&spec, &a, OpenMode::Existing, port);
    assert_eq!(log(&server, &a, None), ["first", "fourth", "second", "third"]);
    assert_eq!(log(&server, &a, Some(after_first)), ["first"]);
    let (mut ws, again, _) = open(port, &spec, &a, Some(token));
    assert_eq!(again, me);
    say(&mut ws, &a, s, 5, "fifth");
    drop(ws);
    server.stop().unwrap();
    let server = start(&spec, &a, OpenMode::Existing, port);
    assert_eq!(log(&server, &a, None), ["fifth", "first", "fourth", "second", "third"]);
    server.stop().unwrap();
}

/// A query whose reads of a durable relation all bind its leading column reads by prefix: the query program keeps the
/// constant in the atom, where the node finds it.
#[test]
fn a_query_binding_the_leading_column_reads_by_prefix() {
    let dir = std::env::temp_dir().join(format!("blossom-db-prefix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/e01_kvs.bls");
    let mut text = std::fs::read_to_string(&source).unwrap();
    text.push_str(
        "\nat Server {\n    view val(v) = store(\"apple\", v);\n    view all(k) = store(k, _);\n    \
         view mid(k) = store(k, _), k > \"b\", k <= \"d\", k > \"a\";\n    \
         view flipped(k) = store(k, _), \"m\" >= k;\n    \
         view twice(k) = store(k, v), store(v2, _), k > \"b\", v2 == \"x\";\n}\n",
    );
    let copy = dir.join("e01_query.bls");
    std::fs::write(&copy, text).unwrap();
    let nodes = [NodeSpec {
        name: "s1".into(),
        role: Some("Server".into()),
    }];
    let a = compile_file(copy.to_str().unwrap(), &nodes).0.unwrap().0;
    for (view, want) in [("val", vec![Value::str("apple")]), ("all", vec![])] {
        let server = a
            .program
            .get()
            .roles
            .iter_enumerated()
            .find(|(_, r)| r.name.to_string() == "Server")
            .map(|(id, _)| id);
        let (q, inputs) = a.program.query(a.rel_named(view).unwrap(), server).unwrap();
        assert_eq!(inputs, ["store"]);
        let store = q
            .get()
            .rels
            .iter_enumerated()
            .find(|(_, r)| r.name.to_string() == "store")
            .map(|(id, _)| id)
            .unwrap();
        assert_eq!(blossom_runtime::query::leading_constants(&q, store), want, "{view}");
    }
    // Ranges: the tightest bounds of the comparisons on the first free column, read either way round; none when the
    // relation is read twice.
    use std::ops::Bound;
    for (view, lo, hi) in [
        (
            "mid",
            Bound::Excluded(Value::str("b")),
            Bound::Included(Value::str("d")),
        ),
        ("flipped", Bound::Unbounded, Bound::Included(Value::str("m"))),
        ("twice", Bound::Unbounded, Bound::Unbounded),
    ] {
        let server = a
            .program
            .get()
            .roles
            .iter_enumerated()
            .find(|(_, r)| r.name.to_string() == "Server")
            .map(|(id, _)| id);
        let (q, _) = a.program.query(a.rel_named(view).unwrap(), server).unwrap();
        let store = q
            .get()
            .rels
            .iter_enumerated()
            .find(|(_, r)| r.name.to_string() == "store")
            .map(|(id, _)| id)
            .unwrap();
        let scan = blossom_runtime::query::scan_of(&q, store);
        assert_eq!((scan.lo, scan.hi), (lo, hi), "{view}");
    }
}

/// A node of `fixtures/db/seeded.bls` (it writes at tick 0) in a fresh directory: its deployment and program.
#[cfg(test)]
fn seeded(name: &str) -> (DeploymentSpec, Arc<BlsArtifact>) {
    let dir = std::env::temp_dir().join(format!("blossom-db-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = dir.join("s.secrets");
    std::fs::write(&secrets, "seed = \"00112233445566778899aabbccddeeff\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/db/seeded.bls");
    let text = format!(
        "format = 1\n[deployment]\nid = \"seeded\"\nprogram = \"seeded\"\nversion = 1\nsource = \"{}\"\n\
         secrets = \"s.secrets\"\n[[node]]\nname = \"k\"\nrole = \"Keeper\"\naddr = \"127.0.0.1:{}\"\n\
         principal = \"spiffe://test/seeded/Keeper/k\"\n[security]\nmode = \"insecure-dev\"\n[storage]\n\
         data_dir = \"data\"\n",
        source.display(),
        free_port(),
    );
    let spec = DeploymentSpec::parse(&text, &dir).unwrap();
    let nodes = [NodeSpec {
        name: "k".into(),
        role: Some("Keeper".into()),
    }];
    let (compiled, _) = compile_file(&spec.source.to_string_lossy(), &nodes);
    (spec, Arc::new(compiled.unwrap().0))
}

#[cfg(test)]
fn start_seeded(spec: &DeploymentSpec, a: &Arc<BlsArtifact>, mode: OpenMode) -> Server {
    Server::start(ServerConfig {
        spec: spec.clone(),
        artifact: a.clone(),
        node: "k".into(),
        mode,
        dir: None,
        backend: blossom_node::Backend::Engine,
        externs: Arc::new(blossom_std_host::registry().unwrap()),
        record: None,
        web: None,
        admin: None,
    })
    .unwrap()
}

#[cfg(test)]
fn seeded_rows(server: &Server, a: &BlsArtifact) -> usize {
    match server.database.range().unwrap().1 {
        Some(tick) => server
            .database
            .rows(a.rel_named("seeded").unwrap(), &[], tick)
            .unwrap()
            .len(),
        None => 0,
    }
}

/// Rows written at tick 0 and never flushed are applied again after a restart (a tick-0 version is not "nothing").
#[test]
fn rows_written_at_tick_zero_survive_a_restart_before_any_flush() {
    let (spec, a) = seeded("tick0");
    let server = start_seeded(&spec, &a, OpenMode::InitFresh);
    for _ in 0..100 {
        if server.database.range().unwrap().1.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Stopped before the timer's write: only tick 0's rows, never flushed.
    assert_eq!(server.database.range().unwrap().1, Some(0));
    assert_eq!(seeded_rows(&server, &a), 2);
    server.stop().unwrap();
    let server = start_seeded(&spec, &a, OpenMode::Existing);
    assert!(seeded_rows(&server, &a) >= 2);
    server.stop().unwrap();
}

/// A database rebuilt from a store's recovered rows: a bootstrap a crash cut short (no flush) leaves no manifest, so
/// the next boot starts it again; and its history begins at the rebuild.
#[test]
fn a_rebuilt_database_starts_again_after_a_crash_and_refuses_the_past() {
    let (spec, a) = seeded("rebuild");
    let server = start_seeded(&spec, &a, OpenMode::InitFresh);
    for _ in 0..200 {
        if seeded_rows(&server, &a) == 3 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(seeded_rows(&server, &a), 3, "the timer's row arrived");
    server.stop().unwrap();
    let store = spec.data_dir.join("k");
    std::fs::remove_dir_all(store.join("db")).unwrap();
    let names: Arc<[Arc<str>]> = a.nodes.iter().map(|n| Arc::from(n.as_str())).collect();
    // A bootstrap cut short: the rows reach the memtable, never a table.
    let open = || {
        blossom_runtime::db::Database::open(
            Arc::new(blossom_store::RealFs),
            &store,
            &a.program,
            names.clone(),
            blossom_store::lsm::LsmOptions::default(),
        )
        .unwrap()
    };
    let (db, fresh) = open();
    assert!(fresh);
    db.bootstrap(5, &Default::default()).unwrap();
    drop(db);
    let (db, fresh) = open();
    assert!(fresh, "a bootstrap that never flushed is started again");
    drop(db);
    // The node rebuilds it, and refuses reads before the rebuild rather than answering them empty.
    let server = start_seeded(&spec, &a, OpenMode::Existing);
    assert_eq!(seeded_rows(&server, &a), 3);
    let (floor, newest) = server.database.range().unwrap();
    assert!(
        floor > 0 && Some(floor) == newest,
        "the floor is the rebuild's tick ({floor}, {newest:?})"
    );
    assert!(
        server
            .database
            .rows(a.rel_named("seeded").unwrap(), &[], floor - 1)
            .is_err()
    );
    server.stop().unwrap();
}

/// The database over a simulated filesystem, a crash at every operation of its flushes and compactions: reopened and
/// given the WAL (every tick's delta) and the recovered rows, as a node's recovery gives them, it equals the rows as of
/// the last tick, and its watermark never claims a tick its tables lack.
#[test]
fn the_database_recovers_from_a_crash_anywhere_in_its_flushes() {
    use blossom_node::durable::{Delta, DurableImage};
    use blossom_store::{Lsn, SimFs, Vfs, WriteFate};
    use std::collections::BTreeSet;
    let (_, a) = seeded("simcrash");
    let rel = a.rel_named("seeded").unwrap();
    let names: Arc<[Arc<str>]> = a.nodes.iter().map(|n| Arc::from(n.as_str())).collect();
    let opts = blossom_store::lsm::LsmOptions {
        memtable_bytes: 300,
        block_bytes: 96,
        tier: 2,
        max_tables: 4,
        history: 6,
        ..blossom_store::lsm::LsmOptions::default()
    };
    let row = |n: u64| -> blossom_oracle::Row {
        Arc::from(vec![
            Value::Int(blossom_value::value::IntValue::U64(n)),
            Value::str("x"),
        ])
    };
    let sim = SimFs::default();
    let dir = Path::new("/store");
    let open = |fs: &SimFs| {
        blossom_runtime::db::Database::open(
            Arc::new(fs.clone()) as Arc<dyn Vfs>,
            dir,
            &a.program,
            names.clone(),
            opts,
        )
        .unwrap()
    };
    let (db, fresh) = open(&sim);
    assert!(fresh);
    db.flush().unwrap();
    // Each tick inserts its row and deletes the one three ticks before.
    let mut wal: Vec<(Lsn, u64, Delta)> = Vec::new();
    let mut present: BTreeSet<u64> = BTreeSet::new();
    let mut images: Vec<BTreeSet<u64>> = Vec::new();
    for t in 0..60u64 {
        if t == 20 {
            sim.enable_crash_recording().unwrap();
        }
        let mut changes = std::collections::BTreeMap::new();
        let deleted = if t >= 3 { vec![row(t - 3)] } else { vec![] };
        changes.insert(rel, (vec![row(t)], deleted));
        let delta = Delta { changes };
        present.insert(t);
        if t >= 3 {
            present.remove(&(t - 3));
        }
        images.push(present.clone());
        db.apply(t, &delta).unwrap();
        wal.push((Lsn(t), t, delta));
        if t % 5 == 4 {
            db.flush().unwrap();
        }
    }
    let cuts = sim.recorded_cuts().unwrap();
    assert!(cuts.len() > 20, "{} cuts", cuts.len());
    let last = 59u64;
    let image = || {
        let mut im = DurableImage::default();
        im.rows
            .insert(rel, images[last as usize].iter().map(|n| row(*n)).collect());
        im
    };
    for (i, cut) in cuts.iter().enumerate() {
        for fate in [WriteFate::Lost, WriteFate::Survive] {
            let mut crashed = cut.fork().unwrap();
            crashed.crash(&mut |_| fate).unwrap();
            let (db, fresh) = open(&crashed);
            // What recovery does (blossom-node's recovery::open): a database the store did not have starts from the
            // recovered rows; one it had takes the WAL records after its tables.
            let flushed = db.flushed().unwrap();
            let mark = flushed.map_or(0, |f| f + 1);
            if fresh {
                db.bootstrap(last, &image()).unwrap();
            } else {
                for (_, t, delta) in wal.iter().filter(|(_, t, _)| flushed.is_none_or(|f| *t > f)) {
                    db.apply(*t, delta).unwrap();
                }
            }
            let held: BTreeSet<u64> = db
                .rows(rel, &[], last)
                .unwrap()
                .iter()
                .map(|r| match &r[0] {
                    Value::Int(blossom_value::value::IntValue::U64(n)) => *n,
                    other => panic!("{other:?}"),
                })
                .collect();
            assert_eq!(held, images[last as usize], "cut {i} ({fate:?}), watermark {mark}");
            // As of any tick the history kept since the watermark's, the rows are what they were then.
            let (floor, _) = db.range().unwrap();
            for t in floor.max(mark.saturating_sub(1))..=last {
                let held: BTreeSet<u64> = db
                    .rows(rel, &[], t)
                    .unwrap()
                    .iter()
                    .filter_map(|r| match &r[0] {
                        Value::Int(blossom_value::value::IntValue::U64(n)) => Some(*n),
                        _ => None,
                    })
                    .collect();
                assert_eq!(held, images[t as usize], "cut {i} ({fate:?}), as of {t}");
            }
        }
    }
}
