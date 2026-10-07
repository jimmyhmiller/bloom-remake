//! S23: `blossom query` (docs/design/DATABASE.md §5). `blossom run --admin` serves e01's key-value store; a client
//! puts three keys and deletes one; queries of the durable `store` see the database as of the newest released tick,
//! and as of an earlier one; a query binding the key reads by prefix; a query of a relation that is not durable is
//! refused.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use blossom_bench::blossom_kv::BlossomKvStore;
use blossom_bench::kv::KvStore;
use blossom_front::api::NodeSpec;
use blossom_runtime::deploy::DeploymentSpec;

#[cfg(test)]
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A one-node e01 deployment in a fresh directory.
#[cfg(test)]
fn deployment() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("blossom-query-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/e01_kvs.bls");
    let spec = format!(
        "format = 1\n[deployment]\nid = \"kvs-query\"\nprogram = \"kvs\"\nversion = 1\nsource = \"{}\"\n\
         secrets = \"kvs.secrets\"\n[[node]]\nname = \"s1\"\nrole = \"Server\"\naddr = \"127.0.0.1:{}\"\n\
         client_addr = \"127.0.0.1:{}\"\nprincipal = \"spiffe://test/kvs/Server/s1\"\n[statics]\n\
         admins = [[\"spiffe://test/kvs/client/admin\"]]\n[security]\nmode = \"insecure-dev\"\n[storage]\n\
         data_dir = \"data\"\n",
        source.display(),
        free_port(),
        free_port()
    );
    let path = dir.join("deploy.toml");
    std::fs::write(&path, spec).unwrap();
    let secrets = dir.join("kvs.secrets");
    std::fs::write(&secrets, "seed = \"00112233445566778899aabbccddeeff\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    path
}

#[cfg(test)]
fn start(deploy: &Path, admin: &str) -> Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_blossom"))
        .args(["run", "--deploy"])
        .arg(deploy)
        .args(["--node", "s1", "--insecure-dev", "--init-fresh", "--admin", admin])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.contains("queries on"), "the server did not come up: {line:?}");
    child
}

#[cfg(test)]
fn query(deploy: &Path, admin: &str, as_of: Option<u64>, text: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blossom"));
    cmd.args(["query", "--deploy"])
        .arg(deploy)
        .args(["--node", "s1", "--admin", admin]);
    if let Some(t) = as_of {
        cmd.args(["--as-of", &t.to_string()]);
    }
    cmd.arg(text).output().unwrap()
}

/// The first column of each row of a query's output (after its header line), and the tick it read as of.
#[cfg(test)]
fn keys(out: &Output) -> (Vec<String>, u64) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut keys: Vec<String> = stdout
        .lines()
        .skip(1)
        .map(|l| l.split('\t').next().unwrap().to_owned())
        .collect();
    keys.sort();
    let tick = stderr
        .split("as of tick ")
        .nth(1)
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or_else(|| panic!("no tick in {stderr:?}"));
    (keys, tick)
}

#[test]
fn queries_read_the_durable_store_as_of_released_ticks() {
    let deploy = deployment();
    let admin = format!("127.0.0.1:{}", free_port());
    let mut server = start(&deploy, &admin);
    let spec = DeploymentSpec::load(&deploy).unwrap();
    let nodes: Vec<NodeSpec> = spec
        .nodes
        .iter()
        .map(|n| NodeSpec {
            name: n.name.clone(),
            role: n.role.clone(),
        })
        .collect();
    let artifact = Arc::new(
        blossom_driver::bls::compile_file(&spec.source.to_string_lossy(), &nodes)
            .0
            .unwrap()
            .0,
    );
    let store = BlossomKvStore {
        addrs: spec.nodes.iter().map(|n| n.client_addr).collect(),
        id: blossom_runtime::server::identity(&spec, &artifact),
        artifact,
        principal: "spiffe://test/kvs/client/admin".into(),
        timeout: Duration::from_secs(5),
    };
    let mut kv = store.connect(0).unwrap();
    for (k, v) in [("apple", "1"), ("banana", "2"), ("cherry", "3")] {
        kv.put(k.as_bytes(), v.as_bytes()).unwrap();
    }
    let (all, before) = keys(&query(&deploy, &admin, None, "all(k, v) = store(k, v)"));
    assert_eq!(all, ["\"apple\"", "\"banana\"", "\"cherry\""]);
    assert!(kv.delete(b"banana").unwrap());
    let (after, now) = keys(&query(&deploy, &admin, None, "all(k, v) = store(k, v)"));
    assert_eq!(after, ["\"apple\"", "\"cherry\""]);
    assert!(now > before);
    // As of the tick before the delete, banana is still there.
    let (then, at) = keys(&query(&deploy, &admin, Some(before), "all(k, v) = store(k, v)"));
    assert_eq!((then.len(), at), (3, before));
    // The key bound: a prefix read; a computed column works like any view's.
    let (one, _) = keys(&query(&deploy, &admin, None, "one(k) = store(k, _), k == \"cherry\""));
    assert_eq!(one, ["\"cherry\""]);
    let (prefixed, _) = keys(&query(&deploy, &admin, None, "val(v) = store(\"apple\", v)"));
    assert_eq!(prefixed.len(), 1);
    // A relation that is not durable is not in the database.
    let refused = query(&deploy, &admin, None, "reqs(i) = put(i, _, _)");
    assert!(!refused.status.success());
    let why = String::from_utf8_lossy(&refused.stderr);
    assert!(why.contains("not durable"), "{why}");
    // A tick outside the history is refused by the node.
    let past = query(&deploy, &admin, Some(now + 1_000_000), "all(k, v) = store(k, v)");
    assert!(!past.status.success());
    let _ = server.kill();
    let _ = server.wait();
}
