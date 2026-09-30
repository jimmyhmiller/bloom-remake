//! Slice 6: byte streams on real TCP (FOREIGN-PROTOCOLS §1). Blossom servers — a line echo and a length-prefixed
//! framing server — run as nodes; Rust clients connect to their `listen` streams and send bytes in arbitrary chunks.
//! The replies must be exactly what the protocol defines, in order, per connection, with several connections at
//! once; a half-closed client still gets its replies, then the server closes the connection.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
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

/// SplitMix64.
#[cfg(test)]
struct Rng(u64);

#[cfg(test)]
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

/// A one-node deployment of fixture `file` (program `program`, stream `stream`) in a fresh directory, started.
#[cfg(test)]
fn start(file: &str, program: &str, stream: &str, streams: Option<String>) -> Result<Server, String> {
    start_with(file, program, stream, streams, &[])
}

/// [`start`] with deploy-time parameters (strings).
#[cfg(test)]
fn start_with(
    file: &str,
    program: &str,
    stream: &str,
    streams: Option<String>,
    params: &[(&str, String)],
) -> Result<Server, String> {
    let dir = std::env::temp_dir().join(format!("blossom-st-{program}-{}-{}", std::process::id(), free_port()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = dir.join("s.secrets");
    std::fs::write(&secrets, "seed = \"00112233445566778899aabbccddeeff\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/streams")
        .join(file);
    let streams = streams.unwrap_or_else(|| format!("streams = {{ {stream} = \"127.0.0.1:0\" }}"));
    let text = format!(
        r#"format = 1
[deployment]
id = "{program}-st"
program = "{program}"
version = 1
source = "{}"
secrets = "s.secrets"
[[node]]
name = "n1"
addr = "127.0.0.1:{}"
principal = "spiffe://test/{program}/n1"
{streams}
[security]
mode = "insecure-dev"
[storage]
data_dir = "data"
"#,
        source.display(),
        free_port(),
    );
    let spec = DeploymentSpec::parse(&text, &dir).map_err(|e| e.to_string())?;
    let nodes: Vec<NodeSpec> = spec
        .nodes
        .iter()
        .map(|n| NodeSpec {
            name: n.name.clone(),
            role: n.role.clone(),
        })
        .collect();
    let bindings: std::collections::BTreeMap<String, blossom_front::api::ParamBinding> = params
        .iter()
        .map(|(k, v)| (k.to_string(), blossom_front::api::ParamBinding::Text(v.clone())))
        .collect();
    let (compiled, _) = blossom_driver::bls::compile_file_with(&spec.source.to_string_lossy(), &nodes, &bindings);
    let artifact: Arc<BlsArtifact> = Arc::new(compiled.map_err(|e| format!("{e:?}"))?.0);
    Server::start(ServerConfig {
        spec,
        artifact,
        node: "n1".into(),
        mode: OpenMode::InitFresh,
        dir: None,
        backend: blossom_node::Backend::Engine,
        externs: Arc::new(blossom_std_host::registry().unwrap()),
    })
    .map_err(|e| e.to_string())
}

/// Reads until `want` bytes arrived (or the deadline passes).
#[cfg(test)]
fn read_exactly(s: &mut TcpStream, want: usize) -> Vec<u8> {
    // At most 200 reads of up to 100 ms each: 20 seconds.
    s.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    let mut attempts = 0;
    while got.len() < want && attempts < 200 {
        attempts += 1;
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => panic!("read: {e}"),
        }
    }
    got
}

/// Sends `bytes` in random chunks with random pauses.
#[cfg(test)]
fn send_chunked(s: &mut TcpStream, bytes: &[u8], rng: &mut Rng) {
    let mut rest = bytes;
    while !rest.is_empty() {
        let n = (1 + rng.below(7) as usize).min(rest.len());
        s.write_all(&rest[..n]).unwrap();
        s.flush().unwrap();
        rest = &rest[n..];
        if rng.below(3) == 0 {
            std::thread::sleep(Duration::from_millis(rng.below(4)));
        }
    }
}

#[cfg(test)]
fn addr(server: &Server, stream: &str) -> SocketAddr {
    *server.stream_addrs.get(stream).unwrap()
}

#[test]
fn the_echo_server_echoes_lines_to_many_connections_over_tcp() {
    let server = start("echo.bls", "echo", "echo", None).unwrap();
    let at = addr(&server, "echo");
    let handles: Vec<_> = (0..6u64)
        .map(|k| {
            std::thread::spawn(move || {
                let mut rng = Rng(k + 1);
                let mut s = TcpStream::connect(at).unwrap();
                let mut text = Vec::new();
                for i in 0..20 {
                    text.extend(format!("conn {k} line {i} {}\n", "x".repeat(rng.below(40) as usize)).bytes());
                }
                // A trailing partial line is never echoed.
                text.extend(b"partial");
                let complete = text.len() - b"partial".len();
                send_chunked(&mut s, &text, &mut rng);
                let got = read_exactly(&mut s, complete);
                assert_eq!(
                    String::from_utf8_lossy(&got),
                    String::from_utf8_lossy(&text[..complete]),
                    "conn {k}"
                );
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(
        server
            .stream_stats
            .seq_violations
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    server.stop().unwrap();
}

#[test]
fn a_half_closed_client_gets_its_replies_then_the_server_closes() {
    let server = start("echo.bls", "echo", "echo", None).unwrap();
    let mut s = TcpStream::connect(addr(&server, "echo")).unwrap();
    s.write_all(b"one\ntwo\nthr").unwrap();
    s.shutdown(std::net::Shutdown::Write).unwrap();
    // The replies arrive, and then EOF: the server closed once the `closed` event's tick was released.
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut got = Vec::new();
    s.read_to_end(&mut got).unwrap();
    assert_eq!(got, b"one\ntwo\n");
    server.stop().unwrap();
}

#[test]
fn the_framing_server_answers_every_frame_whatever_the_chunking() {
    let server = start("frames.bls", "frames", "frames", None).unwrap();
    let at = addr(&server, "frames");
    for seed in 0..4u64 {
        let mut rng = Rng(100 + seed);
        let mut s = TcpStream::connect(at).unwrap();
        let mut request = Vec::new();
        let mut expected = Vec::new();
        for _ in 0..30 {
            let payload: Vec<u8> = (0..rng.below(50)).map(|_| rng.next() as u8).collect();
            request.extend((payload.len() as u32).to_be_bytes());
            request.extend(&payload);
            let reply: Vec<u8> = payload.iter().rev().copied().collect();
            expected.extend((reply.len() as u32).to_be_bytes());
            expected.extend(reply);
        }
        send_chunked(&mut s, &request, &mut rng);
        let got = read_exactly(&mut s, expected.len());
        assert_eq!(got, expected, "seed {seed}");
    }
    server.stop().unwrap();
}

#[test]
fn a_listen_stream_without_an_address_refuses_to_start() {
    let e = start("echo.bls", "echo", "echo", Some(String::new())).err().unwrap();
    assert!(e.contains("has no address"), "{e}");
    let e = start(
        "echo.bls",
        "echo",
        "echo",
        Some("streams = { echo = \"127.0.0.1:0\", nope = \"127.0.0.1:0\" }".into()),
    )
    .err()
    .unwrap();
    assert!(e.contains("not a listen stream"), "{e}");
}

/// Sends `bytes` and waits until the server has surely taken it as its own chunk.
#[cfg(test)]
fn send_alone(s: &mut TcpStream, bytes: &[u8]) {
    s.write_all(bytes).unwrap();
    s.flush().unwrap();
    std::thread::sleep(Duration::from_millis(150));
}

#[test]
fn writes_out_of_seq_order_are_held_and_sent_in_order() {
    let server = start("order.bls", "order", "s", None).unwrap();
    let mut s = TcpStream::connect(addr(&server, "s")).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    // Chunk 0 is written as seq 1: held until chunk 1 (seq 0) arrives.
    send_alone(&mut s, b"A");
    send_alone(&mut s, b"B");
    send_alone(&mut s, b"C");
    send_alone(&mut s, b"D");
    assert_eq!(read_exactly(&mut s, 4), b"BADC");
    server.stop().unwrap();
}

#[test]
fn a_duplicate_write_seq_closes_the_connection() {
    let server = start("dup.bls", "dup", "s", None).unwrap();
    let mut s = TcpStream::connect(addr(&server, "s")).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    send_alone(&mut s, b"first");
    send_alone(&mut s, b"second");
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut got = Vec::new();
    // The first write arrives; the duplicate closes the connection, so the read ends.
    let _ = s.read_to_end(&mut got);
    assert_eq!(got, b"first");
    assert_eq!(
        server
            .stream_stats
            .seq_violations
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    let why = server
        .stream_stats
        .last_violation
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default();
    assert!(why.contains("already written"), "{why}");
    server.stop().unwrap();
}

#[test]
fn a_connect_stream_dials_falls_back_on_failure_and_talks() {
    // TARGET refuses connections (a port nothing listens on); FALLBACK is this test's listener.
    let refused = format!("127.0.0.1:{}", free_port());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let fallback = listener.local_addr().unwrap().to_string();
    let server = start_with(
        "client.bls",
        "client",
        "up",
        Some(String::new()),
        &[("TARGET", refused), ("FALLBACK", fallback)],
    )
    .unwrap();
    listener.set_nonblocking(false).unwrap();
    let (mut s, _) = listener.accept().unwrap();
    assert_eq!(read_exactly(&mut s, 6), b"hello\n");
    s.write_all(b"ping").unwrap();
    assert_eq!(read_exactly(&mut s, 4), b"ping");
    assert_eq!(
        server
            .stream_stats
            .dials_failed
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    server.stop().unwrap();
}

#[test]
fn a_write_through_the_wrong_stream_is_refused_and_counted() {
    let server = start(
        "wrong.bls",
        "wrong",
        "a",
        Some("streams = { a = \"127.0.0.1:0\", b = \"127.0.0.1:0\" }".to_owned()),
    )
    .unwrap();
    let mut s = TcpStream::connect(addr(&server, "a")).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    send_alone(&mut s, b"secret");
    // Only the right write arrives.
    assert_eq!(read_exactly(&mut s, 3), b"ok\n");
    assert_eq!(
        server
            .stream_stats
            .wrong_stream
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    let why = server
        .stream_stats
        .last_violation
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default();
    assert!(why.contains("through stream 1"), "{why}");
    server.stop().unwrap();
}

#[test]
fn a_dial_tries_every_address_its_name_resolves_to() {
    // `localhost` may resolve to `::1` before `127.0.0.1`; the listener is on the IPv4 address only.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let target = format!("localhost:{port}");
    let server = start_with(
        "client.bls",
        "client",
        "up",
        Some(String::new()),
        &[("TARGET", target.clone()), ("FALLBACK", target)],
    )
    .unwrap();
    listener.set_nonblocking(false).unwrap();
    let (mut s, _) = listener.accept().unwrap();
    assert_eq!(read_exactly(&mut s, 6), b"hello\n");
    assert_eq!(
        server
            .stream_stats
            .dials_failed
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    server.stop().unwrap();
}
