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
    let tick = tick.unwrap_or_else(|| db.range().unwrap().1);
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
    let after_first = server.database.range().unwrap().1;
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
    server.database.flush_now().unwrap();
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
    text.push_str("\nat Server {\n    view val(v) = store(\"apple\", v);\n    view all(k) = store(k, _);\n}\n");
    let copy = dir.join("e01_query.bls");
    std::fs::write(&copy, text).unwrap();
    let nodes = [NodeSpec {
        name: "s1".into(),
        role: Some("Server".into()),
    }];
    let a = compile_file(copy.to_str().unwrap(), &nodes).0.unwrap().0;
    for (view, want) in [("val", vec![Value::str("apple")]), ("all", vec![])] {
        let (q, inputs) = a.program.query(a.rel_named(view).unwrap()).unwrap();
        assert_eq!(inputs, ["store"]);
        let store = q
            .get()
            .rels
            .iter_enumerated()
            .find(|(_, r)| r.name.to_string() == "store")
            .map(|(id, _)| id)
            .unwrap();
        assert_eq!(
            blossom_runtime::query::leading_constants(q.get(), store),
            want,
            "{view}"
        );
    }
}
