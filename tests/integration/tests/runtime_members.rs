//! S21: client members over a WebSocket (docs/design/CLIENTS.md §2–§4). A real node serves the chat fixture with
//! `--web`; a test client speaks the link protocol over a WebSocket of its own: it is admitted and given an identity,
//! its message is delivered and durably acknowledged, the server's reply reaches it, a reconnect resumes the link, and
//! after a restart of the server the same token keeps the identity (the link does not resume, and the program greets
//! the member again).

use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

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
use blossom_wire::link::{Catalog, batch_rows, batches, wire_codec};

#[cfg(test)]
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A one-server chat deployment in a fresh directory, and its program.
#[cfg(test)]
fn setup(name: &str) -> (DeploymentSpec, Arc<BlsArtifact>) {
    let dir = std::env::temp_dir().join(format!("blossom-members-{name}-{}", std::process::id()));
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
        r#"format = 1
[deployment]
id = "chat-rt"
program = "chat"
version = 1
source = "{}"
secrets = "chat.secrets"
[[node]]
name = "s"
role = "Server"
addr = "127.0.0.1:{}"
principal = "spiffe://test/chat/Server/s"
[security]
mode = "insecure-dev"
[storage]
data_dir = "data"
"#,
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
fn start(spec: &DeploymentSpec, artifact: &Arc<BlsArtifact>, mode: OpenMode, port: u16) -> Server {
    Server::start(ServerConfig {
        spec: spec.clone(),
        artifact: artifact.clone(),
        node: "s".into(),
        mode,
        dir: None,
        backend: blossom_node::Backend::Engine,
        externs: Arc::new(blossom_std_host::registry().unwrap()),
        record: None,
        web: Some(WebConfig {
            addr: format!("127.0.0.1:{port}").parse().unwrap(),
            root: None,
        }),
    })
    .unwrap()
}

/// Opens a link as a member of `Browser`: its identity, token, whether it resumed, the server's floor.
#[cfg(test)]
fn open(
    port: u16,
    spec: &DeploymentSpec,
    a: &BlsArtifact,
    token: Option<Vec<u8>>,
    received: u64,
    acked: u64,
) -> (Ws, NodeId, Vec<u8>, bool, u64) {
    let mut ws = Ws::connect(port).unwrap();
    let catalog = Catalog::of(a.program.get()).unwrap();
    let peer = Peer::Member {
        role: "Browser".into(),
        part: ClientArtifact::project(a, "Browser").unwrap().part(),
        token,
        received,
        acked,
    };
    ws.send(&blossom_wire::link::hello(&identity(spec, a), peer, 0, 0, &catalog))
        .unwrap();
    assert!(matches!(ws.recv().unwrap(), Frame::Hello(_)));
    assert!(matches!(ws.recv().unwrap(), Frame::HelloOk { .. }));
    match ws.recv().unwrap() {
        Frame::Welcome {
            member,
            token,
            resumed,
            floor,
            ..
        } => (ws, NodeId(member), token, resumed, floor),
        other => panic!("expected WELCOME, got {other:?}"),
    }
}

/// The rows of `heard` the next frames carry, acknowledging each batch, and the acknowledgements seen, until `want`
/// rows arrived.
#[cfg(test)]
fn hear(ws: &mut Ws, a: &BlsArtifact, want: usize) -> (Vec<Vec<Value>>, Vec<u64>, u64) {
    let p = a.program.get();
    let codec = wire_codec(p);
    let heard = a.rel_named("heard").unwrap();
    let sid = Catalog::of(p).unwrap().sid(heard).unwrap();
    let (mut rows, mut acks, mut last) = (Vec::new(), Vec::new(), 0);
    // Each read waits at most the client's timeout; a bounded number of frames keeps a chatty server from looping.
    for _ in 0..1000 {
        if rows.len() >= want {
            break;
        }
        match ws.recv().unwrap() {
            Frame::Msg { seq, batch } => {
                assert_eq!(batch.sid, sid);
                for r in batch_rows(&codec, p, heard, &batch).unwrap() {
                    rows.push(r.to_vec());
                }
                last = seq;
                ws.send(&Frame::Ack { seq }).unwrap();
            }
            Frame::Ack { seq } => acks.push(seq),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(rows.len() >= want, "only {rows:?} arrived");
    (rows, acks, last)
}

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
}

#[test]
fn a_member_is_admitted_heard_acknowledged_and_resumed() {
    let (spec, a) = setup("chat");
    let port = free_port();
    let server = start(&spec, &a, OpenMode::InitFresh, port);
    let s = a.node_id("s").unwrap();
    let (mut ws, me, token, resumed, floor) = open(port, &spec, &a, None, 0, 0);
    assert!(me.is_client(), "{me:?}");
    assert_eq!(me.client_parts(), Some((s, 0)), "the first member this node admits");
    assert!(!resumed, "a new member's link starts over");
    assert_eq!(floor, 0);
    // The member's line comes back to it (it is online), and its batch is acknowledged once durable.
    say(&mut ws, &a, s, 1, "hi");
    let (rows, mut acks, last) = hear(&mut ws, &a, 1);
    assert_eq!(rows, vec![vec![Value::Node(me), Value::Node(me), Value::str("hi")]]);
    // The acknowledgement may follow the reply.
    for _ in 0..1000 {
        if acks.contains(&1) {
            break;
        }
        if let Frame::Ack { seq } = ws.recv().unwrap() {
            acks.push(seq);
        }
    }
    assert!(acks.contains(&1), "no acknowledgement of the member's batch");
    drop(ws);
    // A reconnect with the token is the same member, and the link resumes: the server holds the member's batch 1.
    std::thread::sleep(Duration::from_millis(200));
    let (ws, again, token2, resumed, floor) = open(port, &spec, &a, Some(token.clone()), last, 1);
    assert_eq!((again, resumed, floor), (me, true, 1));
    assert_eq!(token2, token);
    drop(ws);
    // A token this node never gave out is a new member.
    let mut forged = token.clone();
    if let Some(b) = forged.last_mut() {
        *b ^= 1;
    }
    let (ws, other, _, resumed, _) = open(port, &spec, &a, Some(forged), 0, 0);
    assert_ne!(other, me);
    assert!(!resumed);
    drop(ws);
    server.stop().unwrap();
    // After a restart the same token keeps the identity; the link does not resume, so the program greets the member
    // with the log, which survived the restart.
    let server = start(&spec, &a, OpenMode::Existing, port);
    let (mut ws, after, _, resumed, floor) = open(port, &spec, &a, Some(token), last, 1);
    assert_eq!((after, resumed, floor), (me, false, 1));
    let (rows, _, _) = hear(&mut ws, &a, 1);
    assert_eq!(rows, vec![vec![Value::Node(me), Value::Node(me), Value::str("hi")]]);
    drop(ws);
    server.stop().unwrap();
}

/// A member that reconnects while its old connection is still open (a tab whose network changed): the node closes the
/// old connection, and the new one carries the link.
#[test]
fn a_reconnect_closes_the_connection_it_replaces() {
    let (spec, a) = setup("replace");
    let port = free_port();
    let server = start(&spec, &a, OpenMode::InitFresh, port);
    let s = a.node_id("s").unwrap();
    let (mut old, me, token, _, _) = open(port, &spec, &a, None, 0, 0);
    let (mut new, again, _, resumed, _) = open(port, &spec, &a, Some(token), 0, 0);
    assert_eq!((again, resumed), (me, true));
    // The old connection is closed by the node: reading from it ends (a close frame, or the connection's end).
    let ended = (0..100).any(|_| old.recv_bytes_within(Duration::from_millis(100)).is_err());
    assert!(ended, "the replaced connection is still open");
    // The new one works: the member's line comes back to it, as a member still online.
    say(&mut new, &a, s, 1, "still here");
    let (rows, _, _) = hear(&mut new, &a, 1);
    assert_eq!(
        rows,
        vec![vec![Value::Node(me), Value::Node(me), Value::str("still here")]]
    );
    drop(new);
    server.stop().unwrap();
}

/// A page built from another version of the role's part of the program is refused (CLIENTS.md §8): it must load again.
#[test]
fn a_page_built_from_another_program_is_refused() {
    let (spec, a) = setup("stale");
    let port = free_port();
    let server = start(&spec, &a, OpenMode::InitFresh, port);
    let mut ws = Ws::connect(port).unwrap();
    let catalog = Catalog::of(a.program.get()).unwrap();
    let mut part = ClientArtifact::project(&a, "Browser").unwrap().part();
    part[0] ^= 1;
    let peer = Peer::Member {
        role: "Browser".into(),
        part,
        token: None,
        received: 0,
        acked: 0,
    };
    ws.send(&blossom_wire::link::hello(&identity(&spec, &a), peer, 0, 0, &catalog))
        .unwrap();
    match ws.recv().unwrap() {
        Frame::Reject { reason, .. } => assert_eq!(reason, blossom_wire::frame::RejectReason::Program),
        other => panic!("expected REJECT, got {other:?}"),
    }
    server.stop().unwrap();
}
