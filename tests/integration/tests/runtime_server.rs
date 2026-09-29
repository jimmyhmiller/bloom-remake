//! Slice 3: the network runtime in-process (regressions from the S3 review): closed sessions release their
//! descriptors, and replies larger than a frame are split into frames under the limit.

use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_runtime::client::Client;
use blossom_runtime::deploy::DeploymentSpec;
use blossom_runtime::server::{Server, ServerConfig, identity};
use blossom_store::OpenMode;
use blossom_value::Value;
use blossom_value::value::IntValue;

#[cfg(test)]
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A one-node e01 deployment in a fresh directory, and its program.
#[cfg(test)]
fn setup(name: &str) -> (DeploymentSpec, Arc<BlsArtifact>) {
    let dir = std::env::temp_dir().join(format!("blossom-rt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = dir.join("kvs.secrets");
    std::fs::write(&secrets, "seed = \"00112233445566778899aabbccddeeff\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/e01_kvs.bls");
    let text = format!(
        r#"format = 1
[deployment]
id = "kvs-rt"
program = "kvs"
version = 1
source = "{}"
secrets = "kvs.secrets"
[[node]]
name = "s1"
role = "Server"
addr = "127.0.0.1:{}"
client_addr = "127.0.0.1:{}"
principal = "spiffe://test/kvs/Server/s1"
[statics]
admins = [["spiffe://test/kvs/client/admin"]]
[security]
mode = "insecure-dev"
[storage]
data_dir = "data"
"#,
        source.display(),
        free_port(),
        free_port()
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
fn start(spec: &DeploymentSpec, artifact: &Arc<BlsArtifact>) -> Server {
    Server::start(ServerConfig {
        spec: spec.clone(),
        artifact: artifact.clone(),
        node: "s1".into(),
        mode: OpenMode::InitFresh,
        dir: None,
    })
    .unwrap()
}

#[cfg(test)]
#[cfg(unix)]
fn open_fds() -> usize {
    std::fs::read_dir("/dev/fd").unwrap().count()
}

#[cfg(unix)]
#[test]
fn closed_sessions_release_their_descriptors() {
    let (spec, artifact) = setup("fds");
    let server = start(&spec, &artifact);
    let addr = server.client_addr.unwrap();
    let id = identity(&spec, &artifact);
    std::thread::sleep(Duration::from_millis(200));
    let before = open_fds();
    for _ in 0..200 {
        drop(Client::connect(addr, artifact.clone(), &id, "p", Duration::from_secs(2)).unwrap());
    }
    std::thread::sleep(Duration::from_millis(1500));
    let after = open_fds();
    assert!(after < before + 20, "{before} descriptors before 200 closed sessions, {after} after");
    server.stop().unwrap();
}

#[test]
fn replies_larger_than_a_frame_are_split() {
    let (spec, artifact) = setup("bigframe");
    let server = start(&spec, &artifact);
    let id = identity(&spec, &artifact);
    let mut c = Client::connect(server.client_addr.unwrap(), artifact.clone(), &id, "p", Duration::from_secs(5)).unwrap();
    let put = c.rel("put").unwrap();
    let get = c.rel("get").unwrap();
    let key = Value::Str("k".into());
    c.send(
        put,
        &[vec![Value::Int(IntValue::U64(1)), key.clone(), Value::Bytes(vec![7u8; 1 << 20].into())]],
    )
    .unwrap();
    assert!(c.recv(Some(Duration::from_secs(10))).unwrap().is_some(), "the put is acknowledged");
    let gets: Vec<Vec<Value>> = (0..20u64).map(|i| vec![Value::Int(IntValue::U64(100 + i)), key.clone()]).collect();
    c.send(get, &gets).unwrap();
    for got in 0..20 {
        let r = c.recv(Some(Duration::from_secs(10))).unwrap();
        assert!(r.is_some(), "timed out after {got} of 20 replies");
    }
    server.stop().unwrap();
}

/// e01's `#[accept(external, principal in admins)]` on `del`: a session whose principal is not in `admins` is
/// refused at admission (its delete is dropped, an omission), and an admin's is answered.
#[test]
fn only_admins_may_delete() {
    use std::sync::atomic::Ordering;
    let (spec, artifact) = setup("acl");
    let server = start(&spec, &artifact);
    let id = identity(&spec, &artifact);
    let addr = server.client_addr.unwrap();
    let del_row = |id: u64| vec![Value::Int(IntValue::U64(id)), Value::Str("k".into())];
    let mut other = Client::connect(addr, artifact.clone(), &id, "spiffe://test/kvs/client/other", Duration::from_secs(5)).unwrap();
    let del = other.rel("del").unwrap();
    other.send(del, &[del_row(1)]).unwrap();
    assert!(other.recv(Some(Duration::from_millis(800))).unwrap().is_none(), "a non-admin's delete is answered");
    assert!(server.stats.rejected_acl.load(Ordering::Relaxed) >= 1);
    let mut admin = Client::connect(addr, artifact.clone(), &id, "spiffe://test/kvs/client/admin", Duration::from_secs(5)).unwrap();
    admin.send(del, &[del_row(2)]).unwrap();
    let (rel, row) = admin.recv(Some(Duration::from_secs(5))).unwrap().expect("the admin's delete is answered");
    assert_eq!(rel, admin.rel("del_ok").unwrap());
    assert_eq!(row.get(2), Some(&Value::Bool(false)));
    server.stop().unwrap();
}
