//! S27: an HTTP API written in Blossom (`examples/http/notes.bls`), served by a real node on a `listen` stream with
//! the engine checked against the oracle at every tick. A client creates, lists, reads and deletes notes; asks for
//! paths and methods there are none of; sends two requests in one write (answered in order on the connection, the
//! second seeing the first's note) and keeps the connection or closes it; sends a request in pieces; and sends one
//! that is not HTTP. The notes survive a restart, and new ones take ids after them.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use blossom_artifact::bls::BlsArtifact;
use blossom_front::api::NodeSpec;
use blossom_runtime::deploy::DeploymentSpec;
use blossom_runtime::server::{Server, ServerConfig};
use blossom_store::OpenMode;

#[cfg(test)]
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The example deployed as one node in `dir`, its `http` stream on a port of its own.
#[cfg(test)]
fn deployment(dir: &Path) -> (DeploymentSpec, Arc<BlsArtifact>) {
    let secrets = dir.join("s.secrets");
    std::fs::write(&secrets, "seed = \"00112233445566778899aabbccddeeff\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/http/notes.bls");
    let text = format!(
        r#"format = 1
[deployment]
id = "notes-test"
program = "notes"
version = 1
source = "{}"
secrets = "s.secrets"
[[node]]
name = "n1"
addr = "127.0.0.1:{}"
principal = "spiffe://test/notes/n1"
streams = {{ http = "127.0.0.1:0" }}
[security]
mode = "insecure-dev"
[storage]
data_dir = "data"
"#,
        source.display(),
        free_port(),
    );
    let spec = DeploymentSpec::parse(&text, dir).unwrap();
    let nodes: Vec<NodeSpec> = spec
        .nodes
        .iter()
        .map(|n| NodeSpec {
            name: n.name.clone(),
            role: n.role.clone(),
        })
        .collect();
    let (compiled, _) = blossom_driver::bls::compile_file(&spec.source.to_string_lossy(), &nodes);
    (spec, Arc::new(compiled.unwrap().0))
}

#[cfg(test)]
fn start(spec: &DeploymentSpec, artifact: &Arc<BlsArtifact>, mode: OpenMode) -> (Server, SocketAddr) {
    let server = Server::start(ServerConfig {
        spec: spec.clone(),
        artifact: artifact.clone(),
        node: "n1".into(),
        mode,
        dir: None,
        backend: blossom_node::Backend::Checked,
        externs: Arc::new(blossom_std_host::registry().unwrap()),
        record: None,
        admin: None,
        web: None,
    })
    .unwrap();
    let addr = *server.stream_addrs.get("http").unwrap();
    (server, addr)
}

/// A response: its status, headers (names lowercased) and body.
#[cfg(test)]
#[derive(Debug)]
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

#[cfg(test)]
impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

#[cfg(test)]
fn read_answer(r: &mut BufReader<TcpStream>) -> Answer {
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let status = line
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("`{line}`"));
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        if h == "\r\n" || h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':').unwrap();
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
    }
    let len: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .map(|(_, v)| v.parse().unwrap())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).unwrap();
    Answer {
        status,
        headers,
        body: String::from_utf8(body).unwrap(),
    }
}

#[cfg(test)]
fn connect(addr: SocketAddr) -> (TcpStream, BufReader<TcpStream>) {
    let s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let r = BufReader::new(s.try_clone().unwrap());
    (s, r)
}

/// One request on a connection of its own.
#[cfg(test)]
fn ask(addr: SocketAddr, method: &str, path: &str, body: &str) -> Answer {
    let (mut w, mut r) = connect(addr);
    write!(
        w,
        "{method} {path} HTTP/1.1\r\nHost: test\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    read_answer(&mut r)
}

#[cfg(test)]
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("blossom-http-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_notes_api_in_blossom_answers_http() {
    let dir = scratch("notes");
    let (spec, artifact) = deployment(&dir);
    let (server, addr) = start(&spec, &artifact, OpenMode::InitFresh);
    // An empty list, then two notes (one with characters JSON escapes).
    let empty = ask(addr, "GET", "/notes", "");
    assert_eq!((empty.status, empty.body.as_str()), (200, "[]"));
    assert_eq!(empty.header("content-type"), Some("application/json"));
    let one = ask(addr, "POST", "/notes", "buy milk");
    assert_eq!((one.status, one.body.as_str()), (201, "{\"id\": 1}"));
    let two = ask(addr, "POST", "/notes", "walk the \"dog\"\nlater ✓");
    assert_eq!((two.status, two.body.as_str()), (201, "{\"id\": 2}"));
    let all = ask(addr, "GET", "/notes", "");
    assert_eq!(
        all.body,
        "[{\"id\": 1, \"text\": \"buy milk\"}, {\"id\": 2, \"text\": \"walk the \\\"dog\\\"\\nlater ✓\"}]"
    );
    assert_eq!(
        ask(addr, "GET", "/notes/1", "").body,
        "{\"id\": 1, \"text\": \"buy milk\"}"
    );
    // Deleting, and what there is none of.
    assert_eq!(ask(addr, "DELETE", "/notes/1", "").status, 204);
    assert_eq!(ask(addr, "GET", "/notes/1", "").status, 404);
    assert_eq!(ask(addr, "DELETE", "/notes/1", "").status, 404);
    assert_eq!(ask(addr, "GET", "/elsewhere", "").status, 404);
    assert_eq!(ask(addr, "GET", "/notes/x", "").status, 404);
    assert_eq!(ask(addr, "PUT", "/notes", "").status, 405);
    // Two requests in one write, on a kept connection: answered in order, the second seeing the first's note; then a
    // third on the same connection that asks to close it.
    let (mut w, mut r) = connect(addr);
    w.write_all(
        b"POST /notes HTTP/1.1\r\nHost: t\r\nContent-Length: 5\r\n\r\nthirdGET /notes/3 HTTP/1.1\r\nHost: t\r\n\r\n",
    )
    .unwrap();
    let created = read_answer(&mut r);
    assert_eq!((created.status, created.body.as_str()), (201, "{\"id\": 3}"));
    let read = read_answer(&mut r);
    assert_eq!(read.body, "{\"id\": 3, \"text\": \"third\"}");
    assert_eq!(read.header("connection"), None, "a kept connection");
    w.write_all(b"GET /notes/2 HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .unwrap();
    let last = read_answer(&mut r);
    assert_eq!((last.status, last.header("connection")), (200, Some("close")));
    let mut rest = Vec::new();
    assert_eq!(r.read_to_end(&mut rest).unwrap(), 0, "the node closed the connection");
    // A request in pieces, its body split mid-character.
    let (mut w, mut r) = connect(addr);
    let body = "naïve";
    let request = format!(
        "POST /notes HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let bytes = request.as_bytes();
    let cut = bytes.len() - 3;
    for piece in [&bytes[..10], &bytes[10..cut], &bytes[cut..]] {
        w.write_all(piece).unwrap();
        w.flush().unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(read_answer(&mut r).body, "{\"id\": 4}");
    assert_eq!(
        ask(addr, "GET", "/notes/4", "").body,
        "{\"id\": 4, \"text\": \"naïve\"}"
    );
    // Not HTTP: a 400, and the connection closes.
    let (mut w, mut r) = connect(addr);
    w.write_all(b"HELLO there\r\n\r\n").unwrap();
    let bad = read_answer(&mut r);
    assert_eq!((bad.status, bad.header("connection")), (400, Some("close")));
    server.stop().unwrap();
    // After a restart the notes are there, and a new one takes the next id.
    let (server, addr) = start(&spec, &artifact, OpenMode::Existing);
    assert_eq!(
        ask(addr, "GET", "/notes", "").body,
        "[{\"id\": 2, \"text\": \"walk the \\\"dog\\\"\\nlater ✓\"}, {\"id\": 3, \"text\": \"third\"}, {\"id\": 4, \
         \"text\": \"naïve\"}]"
    );
    assert_eq!(ask(addr, "POST", "/notes", "after").body, "{\"id\": 5}");
    server.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
