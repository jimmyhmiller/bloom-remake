//! `blossom serve` on the open internet (docs/design/DEPLOY.md §2), over real connections: `/blossom/wake` only for
//! a caller with the secret, `/blossom/health` from the store, each client's allowance of page tokens (by its
//! connection, or by the header a proxy sets), the cap on connections, and a drain that closes idle connections at
//! once and stops taking new ones.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use blossom_driver::bls::compile_deployed;
use blossom_front::api::NodeSpec;
use blossom_runtime::deploy::DeploymentSpec;
use blossom_runtime::stateless::serve::ClientAddr;
use blossom_runtime::stateless::{Deployment, Objects, ServeConfig, Serving, serve};
use blossom_runtime::web::Transport;
use blossom_statestore::{Commit, MemStore, Snapshot, StateError, StateStore, Write as StoreWrite};

const SECRET: &str = "a-wake-secret-of-some-length";

#[cfg(test)]
fn deployment() -> Arc<Deployment> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/web/keyed_chat.deploy.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let spec = DeploymentSpec::parse(&text, path.parent().unwrap()).unwrap();
    let nodes: Vec<NodeSpec> = spec
        .nodes
        .iter()
        .map(|n| NodeSpec {
            name: n.name.clone(),
            role: n.role.clone(),
        })
        .collect();
    let (compiled, _) = compile_deployed(&spec.source.to_string_lossy(), &nodes, &Default::default());
    Arc::new(
        Deployment::new(
            spec,
            Arc::new(compiled.unwrap().0),
            blossom_value::Seed([9; 16]),
            Arc::new(blossom_std_host::registry().unwrap()),
            "rooms",
            Transport::Http,
        )
        .unwrap(),
    )
}

#[cfg(test)]
fn config() -> ServeConfig {
    ServeConfig {
        web: "127.0.0.1:0".parse().unwrap(),
        web_root: None,
        style: None,
        sweep: None,
        // What a host reports goes nowhere: each test reads the answers it gets.
        report: Arc::new(|_: &str| {}),
        wake_secret: Some(SECRET.into()),
        max_connections: 64,
        client: ClientAddr::Peer,
        tokens_per_minute: 60,
    }
}

#[cfg(test)]
fn host(store: Arc<dyn StateStore>, cfg: ServeConfig) -> Serving {
    serve(Arc::new(Objects::new(deployment(), store)), cfg).unwrap()
}

/// An answer: its status, headers (names in lower case) and body.
#[cfg(test)]
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[cfg(test)]
impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// Reads one answer from a connection (its body by `Content-Length`).
#[cfg(test)]
fn read_answer(r: &mut BufReader<TcpStream>) -> Answer {
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let status = line.split(' ').nth(1).unwrap().parse().unwrap();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':').unwrap();
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
    }
    let len: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .map_or(0, |(_, v)| v.parse().unwrap());
    let mut body = vec![0; len];
    r.read_exact(&mut body).unwrap();
    Answer { status, headers, body }
}

/// One request on a connection of its own.
#[cfg(test)]
fn call(addr: SocketAddr, method: &str, path: &str, headers: &[(&str, &str)]) -> Answer {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\nContent-Length: 0\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    read_answer(&mut BufReader::new(s))
}

/// A connection kept open after one request it made (so the host has surely taken it).
#[cfg(test)]
fn kept(addr: SocketAddr) -> BufReader<TcpStream> {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    s.write_all(b"GET /blossom/health HTTP/1.1\r\nHost: test\r\n\r\n")
        .unwrap();
    let mut r = BufReader::new(s);
    assert_eq!(read_answer(&mut r).status, 200);
    r
}

/// Waits until `f` holds, for at most `limit`.
#[cfg(test)]
#[allow(clippy::disallowed_methods)] // the test's own deadlines
fn until(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    f()
}

#[test]
fn wake_is_only_for_a_caller_with_the_secret() {
    let h = host(Arc::new(MemStore::new()), config());
    assert_eq!(call(h.addr, "GET", "/blossom/wake", &[]).status, 401);
    assert_eq!(
        call(
            h.addr,
            "GET",
            "/blossom/wake",
            &[("Authorization", "Bearer not-the-secret")]
        )
        .status,
        401
    );
    // All but its last character: a guess that matches almost all of it.
    let prefix = SECRET.strip_suffix('h').unwrap();
    let shown = format!("Bearer {prefix}");
    assert_eq!(
        call(h.addr, "GET", "/blossom/wake", &[("Authorization", &shown)]).status,
        401
    );
    let shown = format!("Bearer {SECRET}");
    for method in ["GET", "POST"] {
        let a = call(h.addr, method, "/blossom/wake", &[("Authorization", &shown)]);
        assert_eq!(a.status, 200, "{method}");
        let woken: Vec<String> = serde_json::from_slice(&a.body).unwrap();
        assert!(woken.is_empty(), "nothing was due: {woken:?}");
    }
    h.stop();

    // A host without a secret refuses everyone, whatever they show.
    let h = host(
        Arc::new(MemStore::new()),
        ServeConfig {
            wake_secret: None,
            ..config()
        },
    );
    assert_eq!(
        call(h.addr, "POST", "/blossom/wake", &[("Authorization", &shown)]).status,
        403
    );
    h.stop();
}

/// A store that is down: every call fails.
#[cfg(test)]
struct Down;

#[cfg(test)]
impl StateStore for Down {
    fn load(&self, _: &str) -> Result<Snapshot, StateError> {
        Err(StateError::Unavailable("down".into()))
    }
    fn version(&self, _: &str) -> Result<u64, StateError> {
        Err(StateError::Unavailable("down".into()))
    }
    fn commit(&self, _: &str, _: u64, _: &[StoreWrite]) -> Result<Commit, StateError> {
        Err(StateError::Unavailable("down".into()))
    }
    fn wait(&self, _: &str, _: u64, _: Duration) -> Result<u64, StateError> {
        Err(StateError::Unavailable("down".into()))
    }
    fn schedule(&self, _: &str, _: u64) -> Result<(), StateError> {
        Err(StateError::Unavailable("down".into()))
    }
    fn due(&self, _: u64, _: usize) -> Result<Vec<(String, u64)>, StateError> {
        Err(StateError::Unavailable("down".into()))
    }
    fn unschedule(&self, _: &str, _: u64) -> Result<(), StateError> {
        Err(StateError::Unavailable("down".into()))
    }
    fn put_side(&self, _: &str, _: &[u8]) -> Result<(), StateError> {
        Err(StateError::Unavailable("down".into()))
    }
    fn get_side(&self, _: &str) -> Result<Option<Vec<u8>>, StateError> {
        Err(StateError::Unavailable("down".into()))
    }
    fn delete_side(&self, _: &str) -> Result<(), StateError> {
        Err(StateError::Unavailable("down".into()))
    }
}

#[test]
fn health_is_the_stores() {
    let h = host(Arc::new(MemStore::new()), config());
    let a = call(h.addr, "GET", "/blossom/health", &[]);
    assert_eq!((a.status, a.body.as_slice()), (200, b"ok".as_slice()));
    h.stop();
    let h = host(Arc::new(Down), config());
    let a = call(h.addr, "GET", "/blossom/health", &[]);
    assert_eq!(a.status, 503);
    assert!(String::from_utf8_lossy(&a.body).contains("down"));
    h.stop();
}

#[test]
fn each_client_is_given_its_allowance_of_page_tokens() {
    let token = "/blossom/token?role=Browser";
    // By connection: what a client writes in a header is nothing to this host.
    let h = host(
        Arc::new(MemStore::new()),
        ServeConfig {
            tokens_per_minute: 3,
            ..config()
        },
    );
    for k in 0..3 {
        let a = call(h.addr, "POST", token, &[("X-Real-IP", &format!("10.0.0.{k}"))]);
        assert_eq!(a.status, 200, "token {k}");
    }
    let a = call(h.addr, "POST", token, &[("X-Real-IP", "10.0.0.9")]);
    assert_eq!(a.status, 429);
    let after: u64 = a.header("retry-after").unwrap().parse().unwrap();
    assert!((1..=20).contains(&after), "a token comes back every 20 s: {after}");
    assert_eq!(h.stats.tokens_refused.load(std::sync::atomic::Ordering::Relaxed), 1);
    h.stop();

    // Behind a proxy: by the header it sets.
    let h = host(
        Arc::new(MemStore::new()),
        ServeConfig {
            tokens_per_minute: 3,
            client: ClientAddr::Header("x-real-ip".into()),
            ..config()
        },
    );
    for _ in 0..3 {
        assert_eq!(call(h.addr, "POST", token, &[("X-Real-IP", "10.0.0.1")]).status, 200);
    }
    assert_eq!(call(h.addr, "POST", token, &[("X-Real-IP", "10.0.0.1")]).status, 429);
    assert_eq!(
        call(h.addr, "POST", token, &[("X-Real-IP", "10.0.0.2, 10.9.9.9")]).status,
        200
    );
    // No header: the connection's address.
    for _ in 0..3 {
        assert_eq!(call(h.addr, "POST", token, &[]).status, 200);
    }
    assert_eq!(call(h.addr, "POST", token, &[]).status, 429);
    h.stop();
}

#[test]
fn a_host_serves_at_most_its_connections_at_once() {
    let h = host(
        Arc::new(MemStore::new()),
        ServeConfig {
            max_connections: 2,
            ..config()
        },
    );
    let a = kept(h.addr);
    let _b = kept(h.addr);
    let third = call(h.addr, "GET", "/blossom/health", &[]);
    assert_eq!(
        (third.status, third.body.as_slice()),
        (503, b"too many connections".as_slice())
    );
    assert_eq!(h.stats.turned_away.load(std::sync::atomic::Ordering::Relaxed), 1);
    drop(a);
    assert!(until(Duration::from_secs(5), || h.connections() == 1));
    assert_eq!(call(h.addr, "GET", "/blossom/health", &[]).status, 200);
    h.stop();
}

#[test]
#[allow(clippy::disallowed_methods)] // the test's own deadlines
fn a_drain_closes_idle_connections_at_once_and_takes_no_new_ones() {
    let h = host(Arc::new(MemStore::new()), config());
    let addr = h.addr;
    let mut idle = kept(addr);
    let started = Instant::now();
    assert!(h.drain(Duration::from_secs(20)), "every connection ended");
    // Well before the connection's idle limit (75 s): at the host's next look.
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    let mut rest = Vec::new();
    assert_eq!(
        idle.read_to_end(&mut rest).unwrap(),
        0,
        "the idle connection was closed"
    );
    assert!(TcpStream::connect(addr).is_err(), "a drained host takes no connections");
}
