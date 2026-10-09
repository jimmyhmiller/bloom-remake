//! S21, S27: client members over a WebSocket and over plain HTTP requests (docs/design/CLIENTS.md §2–§4). A real node
//! serves the chat fixture with `--web`; a test client speaks the link protocol over either transport: it is admitted
//! and given an identity, its message is delivered and durably acknowledged, the server's reply reaches it, a reconnect
//! resumes the link, and after a restart of the server the same token keeps the identity (the link does not resume, and
//! the program greets the member again). Over requests, a session ends when the page closes it, when its waiting
//! receive's connection closes, and when its lease runs out.

use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use blossom_artifact::bls::BlsArtifact;
use blossom_artifact::client::ClientArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_integration_tests::LinkClient;
use blossom_integration_tests::http_link::{self, HttpLink};
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
        admin: None,
        web: Some(WebConfig {
            addr: format!("127.0.0.1:{port}").parse().unwrap(),
            root: None,
        }),
    })
    .unwrap()
}

/// The link's transport.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
enum Via {
    WebSocket,
    Http,
}

/// A link's first frame, `HELLO` as a member of `Browser`.
#[cfg(test)]
fn member_hello(spec: &DeploymentSpec, a: &BlsArtifact, token: Option<Vec<u8>>, received: u64, acked: u64) -> Frame {
    let catalog = Catalog::of(a.program.get()).unwrap();
    let peer = Peer::Member {
        role: "Browser".into(),
        part: ClientArtifact::project(a, "Browser").unwrap().part(),
        token,
        received,
        acked,
        keyed: None,
    };
    blossom_wire::link::hello(&identity(spec, a), peer, 0, 0, &catalog)
}

/// Opens a link as a member of `Browser` over `via`: the link, the member's identity, token, whether it resumed, and
/// the server's floor.
#[cfg(test)]
fn open(
    via: Via,
    port: u16,
    spec: &DeploymentSpec,
    a: &BlsArtifact,
    token: Option<Vec<u8>>,
    received: u64,
    acked: u64,
) -> (Box<dyn LinkClient>, NodeId, Vec<u8>, bool, u64) {
    let hello = member_hello(spec, a, token, received, acked);
    let mut ws: Box<dyn LinkClient> = match via {
        Via::WebSocket => {
            let mut ws = Ws::connect(port).unwrap();
            ws.send(&hello).unwrap();
            Box::new(ws)
        }
        Via::Http => Box::new(HttpLink::open(port, &hello).unwrap()),
    };
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
fn hear(ws: &mut dyn LinkClient, a: &BlsArtifact, want: usize) -> (Vec<Vec<Value>>, Vec<u64>, u64) {
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
fn say(ws: &mut dyn LinkClient, a: &BlsArtifact, server: NodeId, seq: u64, text: &str) {
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
    admitted_heard_acknowledged_and_resumed(Via::WebSocket);
}

#[test]
fn a_member_over_http_is_admitted_heard_acknowledged_and_resumed() {
    admitted_heard_acknowledged_and_resumed(Via::Http);
}

#[cfg(test)]
fn admitted_heard_acknowledged_and_resumed(via: Via) {
    let (spec, a) = setup(&format!("chat-{via:?}"));
    let port = free_port();
    let server = start(&spec, &a, OpenMode::InitFresh, port);
    let s = a.node_id("s").unwrap();
    let (mut ws, me, token, resumed, floor) = open(via, port, &spec, &a, None, 0, 0);
    assert!(me.is_client(), "{me:?}");
    assert_eq!(me.client_parts(), Some((s, 0)), "the first member this node admits");
    assert!(!resumed, "a new member's link starts over");
    assert_eq!(floor, 0);
    // The member's line comes back to it (it is online), and its batch is acknowledged once durable.
    say(&mut *ws, &a, s, 1, "hi");
    let (rows, mut acks, last) = hear(&mut *ws, &a, 1);
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
    let (ws, again, token2, resumed, floor) = open(via, port, &spec, &a, Some(token.clone()), last, 1);
    assert_eq!((again, resumed, floor), (me, true, 1));
    assert_eq!(token2, token);
    drop(ws);
    // A token this node never gave out is a new member.
    let mut forged = token.clone();
    if let Some(b) = forged.last_mut() {
        *b ^= 1;
    }
    let (ws, other, _, resumed, _) = open(via, port, &spec, &a, Some(forged), 0, 0);
    assert_ne!(other, me);
    assert!(!resumed);
    drop(ws);
    server.stop().unwrap();
    // After a restart the same token keeps the identity; the link does not resume, so the program greets the member
    // with the log, which survived the restart.
    let server = start(&spec, &a, OpenMode::Existing, port);
    let (mut ws, after, _, resumed, floor) = open(via, port, &spec, &a, Some(token), last, 1);
    assert_eq!((after, resumed, floor), (me, false, 1));
    let (rows, _, _) = hear(&mut *ws, &a, 1);
    assert_eq!(rows, vec![vec![Value::Node(me), Value::Node(me), Value::str("hi")]]);
    drop(ws);
    server.stop().unwrap();
}

/// A member that reconnects while its old connection is still open (a tab whose network changed): the node closes the
/// old connection, and the new one carries the link.
#[test]
fn a_reconnect_closes_the_connection_it_replaces() {
    reconnect_closes_what_it_replaces(Via::WebSocket);
}

#[test]
fn an_http_session_is_ended_by_the_one_that_replaces_it() {
    reconnect_closes_what_it_replaces(Via::Http);
}

#[cfg(test)]
fn reconnect_closes_what_it_replaces(via: Via) {
    let (spec, a) = setup(&format!("replace-{via:?}"));
    let port = free_port();
    let server = start(&spec, &a, OpenMode::InitFresh, port);
    let s = a.node_id("s").unwrap();
    let (mut old, me, token, _, _) = open(via, port, &spec, &a, None, 0, 0);
    let (mut new, again, _, resumed, _) = open(via, port, &spec, &a, Some(token), 0, 0);
    assert_eq!((again, resumed), (me, true));
    // The old connection is closed by the node: reading from it ends (a close frame, the connection's end, or `410`).
    assert!(
        old.ended_within(Duration::from_secs(10)),
        "the replaced connection is still open"
    );
    // The new one works: the member's line comes back to it, as a member still online.
    say(&mut *new, &a, s, 1, "still here");
    let (rows, _, _) = hear(&mut *new, &a, 1);
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
    let catalog = Catalog::of(a.program.get()).unwrap();
    let mut part = ClientArtifact::project(&a, "Browser").unwrap().part();
    part[0] ^= 1;
    let peer = Peer::Member {
        role: "Browser".into(),
        part,
        token: None,
        received: 0,
        acked: 0,
        keyed: None,
    };
    let hello = blossom_wire::link::hello(&identity(&spec, &a), peer, 0, 0, &catalog);
    let mut ws = Ws::connect(port).unwrap();
    ws.send(&hello).unwrap();
    let mut over_http = HttpLink::open(port, &hello).unwrap();
    // A refused open makes no session.
    assert_eq!(over_http.session, None);
    for frame in [ws.recv().unwrap(), over_http.recv().unwrap()] {
        match frame {
            Frame::Reject { reason, .. } => assert_eq!(reason, blossom_wire::frame::RejectReason::Program),
            other => panic!("expected REJECT, got {other:?}"),
        }
    }
    server.stop().unwrap();
}

/// A session ends when the page closes it (its beacon), and when the connection of its waiting receive closes (a
/// closed tab): a request on it is then answered `410`. Another member's session goes on.
#[test]
fn an_http_session_ends_when_closed_or_when_its_waiting_receive_goes() {
    let (spec, a) = setup("http-end");
    let port = free_port();
    let server = start(&spec, &a, OpenMode::InitFresh, port);
    let s = a.node_id("s").unwrap();
    // Closed by the page.
    let (mut one, _, _, _, _) = open(Via::Http, port, &spec, &a, None, 0, 0);
    let mut raw = HttpLink::open(port, &member_hello(&spec, &a, None, 0, 0)).unwrap();
    assert_eq!(raw.close().unwrap(), 204);
    assert_eq!(raw.send_status(&Frame::Ack { seq: 0 }).unwrap(), 410);
    // A receive left waiting, whose connection then closes.
    let mut gone = HttpLink::open(port, &member_hello(&spec, &a, None, 0, 0)).unwrap();
    while gone.recv().is_ok_and(|f| !matches!(f, Frame::Welcome { .. })) {}
    let session = gone.session.clone().unwrap();
    {
        use std::io::Write;
        let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            c,
            "GET /blossom/http/{session}/recv HTTP/1.1
Host: localhost

"
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
    }
    assert!(
        gone.ended_within(Duration::from_secs(5)),
        "a session whose waiting receive went is still open"
    );
    say(&mut *one, &a, s, 1, "who is here");
    let (rows, _, _) = hear(&mut *one, &a, 1);
    assert_eq!(rows.len(), 1);
    // A session no one knows (ended and forgotten, or never made) is gone too.
    let a410 = http_link::request(port, "GET", "/blossom/http/00/recv", b"").unwrap();
    assert_eq!(a410.status, 410);
    server.stop().unwrap();
}

/// A session no request reaches for the lease's length ends.
#[test]
#[ignore = "full tier"]
fn an_http_session_without_requests_ends_with_its_lease() {
    let (spec, a) = setup("http-lease");
    let port = free_port();
    let server = start(&spec, &a, OpenMode::InitFresh, port);
    let mut quiet = HttpLink::open(port, &member_hello(&spec, &a, None, 0, 0)).unwrap();
    std::thread::sleep(blossom_runtime::http_link::LEASE + Duration::from_secs(3));
    assert_eq!(quiet.send_status(&Frame::Ack { seq: 0 }).unwrap(), 410);
    server.stop().unwrap();
}

/// A client may ask several things on one connection (HTTP/1.1 keeps it by default).
#[test]
fn a_connection_carries_several_requests() {
    use std::io::{BufReader, Write};
    let (spec, a) = setup("keep-alive");
    let port = free_port();
    let server = start(&spec, &a, OpenMode::InitFresh, port);
    let c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut w = c.try_clone().unwrap();
    let mut r = BufReader::new(c);
    for _ in 0..3 {
        write!(
            w,
            "GET /blossom/app.json HTTP/1.1
Host: localhost

"
        )
        .unwrap();
        let answer = http_link::read_answer(&mut r).unwrap();
        assert_eq!(answer.status, 200);
        assert_eq!(answer.headers.get("connection").map(String::as_str), Some("keep-alive"));
        let app: serde_json::Value = serde_json::from_slice(&answer.body).unwrap();
        assert_eq!(app["http"], "/blossom/http");
        // Plain requests, the deployment not saying otherwise.
        assert_eq!(app["transport"], "http");
    }
    server.stop().unwrap();
}
