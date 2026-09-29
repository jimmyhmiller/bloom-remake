//! Slice 3's demo: the e01 key-value store as a server that never loses an acknowledged write when killed with
//! `kill -9`.
//!
//! `blossom run` serves e01 while clients run a put/get/delete workload. The test SIGKILLs the server process at
//! random moments and restarts it from its store, over and over. Every client operation is recorded (unanswered ones
//! as "may or may not have happened"), and the whole history must be linearizable: an acknowledged put that a crash
//! lost, a read of a value that was never durable, or a delete reported twice would all break it.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use blossom_bench::blossom_kv::E01Store;
use blossom_bench::kv::{self, KvStore, Workload};
use blossom_front::api::NodeSpec;
use blossom_runtime::deploy::DeploymentSpec;
use blossom_sim::linearize::{KvModel, Verdict, check_partitioned};

/// A port nobody is listening on right now.
#[cfg(test)]
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[cfg(test)]
fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("blossom-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes a one-node e01 deployment into `dir`.
#[cfg(test)]
fn deployment(dir: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/e01_kvs.bls");
    let spec = format!(
        r#"format = 1

[deployment]
id = "kvs-kill9"
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
checkpoint_wal_bytes = 65536
"#,
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

/// Starts `blossom run` and waits for its readiness line.
#[cfg(test)]
fn start(deploy: &Path, fresh: bool) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blossom"));
    cmd.args(["run", "--deploy"]).arg(deploy).args(["--node", "s1", "--insecure-dev"]);
    if fresh {
        cmd.arg("--init-fresh");
    }
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    assert!(line.contains("ready"), "the server did not come up: {line:?}");
    child
}

#[test]
fn kill_9_never_loses_an_acknowledged_write() {
    let dir = scratch_dir("kill9");
    let deploy = deployment(&dir);
    let spec = DeploymentSpec::load(&deploy).unwrap();
    let nodes: Vec<NodeSpec> = spec
        .nodes
        .iter()
        .map(|n| NodeSpec {
            name: n.name.clone(),
            role: n.role.clone(),
        })
        .collect();
    let (compiled, _) = blossom_driver::bls::compile_file(&spec.source.to_string_lossy(), &nodes);
    let artifact = Arc::new(compiled.unwrap().0);

    let mut server = start(&deploy, true);
    let store: Arc<dyn KvStore> = Arc::new(E01Store {
        addrs: spec.nodes.iter().filter_map(|n| n.client_addr).collect(),
        id: blossom_runtime::server::identity(&spec, &artifact),
        artifact,
        principal: "spiffe://test/kvs/client/admin".into(),
        timeout: Duration::from_millis(500),
    });
    let workload = Workload {
        clients: 8,
        duration: Duration::from_secs(14),
        keys: 12,
        mix: (45, 40, 15),
        value_size: 24,
        seed: 42,
        namespace: String::new(),
        record: true,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let run = {
        let (store, w, stop) = (store.clone(), workload.clone(), stop.clone());
        std::thread::spawn(move || kv::run(store, &w, stop))
    };
    // Kill and restart the server at irregular intervals while the workload runs.
    let begin = blossom_bench::stopwatch::Stopwatch::start();
    let mut kills = 0;
    let mut rng: u64 = 0x5eed;
    while begin.elapsed() < Duration::from_secs(11) {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        std::thread::sleep(Duration::from_millis(400 + rng % 1200));
        server.kill().unwrap(); // SIGKILL
        server.wait().unwrap();
        kills += 1;
        server = start(&deploy, false);
    }
    let outcome = run.join().unwrap();
    server.kill().unwrap();
    server.wait().unwrap();

    assert!(kills >= 5, "only {kills} kills");
    assert!(outcome.answered > 200, "only {} operations answered", outcome.answered);
    assert!(outcome.unanswered > 0, "no operation was in flight at a kill: the test did not test anything");
    let (verdict, key) = check_partitioned(&KvModel, &outcome.history, |i| i.key().to_vec(), 50_000_000);
    let summary = match &verdict {
        Verdict::Linearizable => "linearizable".to_string(),
        Verdict::NotLinearizable { longest } => format!("not linearizable after {} operations", longest.len()),
        Verdict::Unknown => "unknown (search budget exceeded)".to_string(),
    };
    assert!(
        verdict == Verdict::Linearizable,
        "{summary} at key {:?}; {} answered, {} unanswered, {kills} kills",
        key.map(|k| String::from_utf8_lossy(&k).into_owned()),
        outcome.answered,
        outcome.unanswered
    );
}
