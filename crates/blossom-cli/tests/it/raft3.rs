//! Slice 4's demo: the Raft key-value store (`examples/e11_raft_kv.bls`) as three `blossom run` processes over TCP.
//!
//! Every directed link between servers goes through a proxy the test controls, so the network can be partitioned
//! for real: a node's peer connections to the others are cut and refused. A nemesis kills servers with SIGKILL
//! (leaders included) and restarts them, and isolates servers, while clients run a put/get/delete workload that
//! follows redirects to the leader. The whole history, including operations in flight at a kill or a partition,
//! must be linearizable.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use blossom_bench::blossom_kv::BlossomKvStore;
use blossom_bench::kv::{self, KvStore, Workload};
use blossom_bench::stopwatch::Stopwatch;
use blossom_front::api::NodeSpec;
use blossom_runtime::deploy::DeploymentSpec;
use blossom_sim::linearize::{KvModel, Verdict, check_partitioned};

#[cfg(test)]
fn free_addr() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap()
}

/// A TCP proxy for one directed link: forwards connections to `upstream` unless blocked. Blocking cuts the open
/// connections and refuses new ones.
#[cfg(test)]
struct Proxy {
    addr: SocketAddr,
    blocked: Arc<AtomicBool>,
    open: Arc<Mutex<Vec<TcpStream>>>,
}

#[cfg(test)]
impl Proxy {
    fn start(upstream: SocketAddr) -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let blocked = Arc::new(AtomicBool::new(false));
        let open: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
        let (b, o) = (blocked.clone(), open.clone());
        std::thread::spawn(move || {
            for inbound in listener.incoming() {
                let Ok(inbound) = inbound else { continue };
                if b.load(Ordering::SeqCst) {
                    drop(inbound);
                    continue;
                }
                let Ok(outbound) = TcpStream::connect(upstream) else { continue };
                let mut reg = o.lock().unwrap();
                reg.push(inbound.try_clone().unwrap());
                reg.push(outbound.try_clone().unwrap());
                drop(reg);
                let pipe = |mut from: TcpStream, mut to: TcpStream| {
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 64 * 1024];
                        loop {
                            match from.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    if to.write_all(&buf[..n]).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        let _ = to.shutdown(std::net::Shutdown::Both);
                    });
                };
                pipe(inbound.try_clone().unwrap(), outbound.try_clone().unwrap());
                pipe(outbound, inbound);
            }
        });
        Proxy { addr, blocked, open }
    }

    fn block(&self) {
        self.blocked.store(true, Ordering::SeqCst);
        for c in self.open.lock().unwrap().drain(..) {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
    }

    fn heal(&self) {
        self.blocked.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
struct Cluster {
    dir: PathBuf,
    deploy: PathBuf,
    names: Vec<String>,
    procs: Vec<Option<Child>>,
    /// `proxies[(i, j)]`: the link from node i to node j.
    proxies: Vec<((usize, usize), Proxy)>,
}

#[cfg(test)]
impl Cluster {
    fn new() -> Cluster {
        let dir = std::env::temp_dir().join(format!("blossom-raft3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let names: Vec<String> = (1..=3).map(|i| format!("s{i}")).collect();
        let peer: Vec<SocketAddr> = names.iter().map(|_| free_addr()).collect();
        let client: Vec<SocketAddr> = names.iter().map(|_| free_addr()).collect();
        let mut proxies = Vec::new();
        for i in 0..3 {
            for (j, upstream) in peer.iter().enumerate() {
                if i != j {
                    proxies.push(((i, j), Proxy::start(*upstream)));
                }
            }
        }
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/e11_raft_kv.bls");
        let mut spec = format!(
            "format = 1\n\n[deployment]\nid = \"raft3\"\nprogram = \"raft_kv\"\nversion = 1\nsource = \"{}\"\nsecrets = \"raft.secrets\"\n",
            source.display()
        );
        for (i, n) in names.iter().enumerate() {
            let dial: Vec<String> = proxies
                .iter()
                .filter(|((a, _), _)| *a == i)
                .map(|((_, b), p)| format!("{} = \"{}\"", names[*b], p.addr))
                .collect();
            spec.push_str(&format!(
                "\n[[node]]\nname = \"{n}\"\nrole = \"Server\"\naddr = \"{}\"\nclient_addr = \"{}\"\nprincipal = \"spiffe://test/raft/Server/{n}\"\ndial = {{ {} }}\n",
                peer[i],
                client[i],
                dial.join(", ")
            ));
        }
        spec.push_str("\n[security]\nmode = \"insecure-dev\"\n\n[storage]\ndata_dir = \"data\"\ncheckpoint_wal_bytes = 262144\n");
        let deploy = dir.join("deploy.toml");
        std::fs::write(&deploy, spec).unwrap();
        let secrets = dir.join("raft.secrets");
        std::fs::write(&secrets, "seed = \"0f0e0d0c0b0a09080706050403020100\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut c = Cluster {
            dir,
            deploy,
            names,
            procs: vec![None, None, None],
            proxies,
        };
        for i in 0..3 {
            c.start(i, true);
        }
        c
    }

    fn start(&mut self, i: usize, fresh: bool) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blossom"));
        cmd.args(["run", "--deploy"])
            .arg(&self.deploy)
            .args(["--node", &self.names[i], "--insecure-dev"]);
        if fresh {
            cmd.arg("--init-fresh");
        }
        let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        assert!(line.contains("ready"), "{} did not come up: {line:?}", self.names[i]);
        self.procs[i] = Some(child);
    }

    fn kill(&mut self, i: usize) {
        if let Some(mut c) = self.procs[i].take() {
            c.kill().unwrap(); // SIGKILL
            c.wait().unwrap();
        }
    }

    fn isolate(&self, i: usize) {
        for ((a, b), p) in &self.proxies {
            if *a == i || *b == i {
                p.block();
            }
        }
    }

    fn heal(&self) {
        for (_, p) in &self.proxies {
            p.heal();
        }
    }
}

#[cfg(test)]
impl Drop for Cluster {
    fn drop(&mut self) {
        for i in 0..3 {
            self.kill(i);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn three_processes_stay_linearizable_under_kill_9_and_partitions() {
    let mut cluster = Cluster::new();
    let spec = DeploymentSpec::load(&cluster.deploy).unwrap();
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
    let store: Arc<dyn KvStore> = Arc::new(BlossomKvStore {
        addrs: spec.nodes.iter().map(|n| n.client_addr).collect(),
        id: blossom_runtime::server::identity(&spec, &artifact),
        artifact,
        principal: "spiffe://test/raft/client".into(),
        timeout: Duration::from_millis(1500),
    });
    let workload = Workload {
        clients: 6,
        duration: Duration::from_secs(24),
        keys: 8,
        mix: (45, 40, 15),
        value_size: 16,
        seed: 7,
        namespace: String::new(),
        record: true,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let run = {
        let (store, w, stop) = (store.clone(), workload.clone(), stop.clone());
        std::thread::spawn(move || kv::run(store, &w, stop))
    };
    // Let a leader emerge, then alternate kills and partitions.
    std::thread::sleep(Duration::from_secs(3));
    let clock = Stopwatch::start();
    let (mut kills, mut partitions) = (0, 0);
    let mut rng: u64 = 0x5eed_1234;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    // Kills and partitions alternate. Kills pick a random server; partitions isolate each server in turn, so the
    // leader is isolated too.
    let mut isolate_next = 0usize;
    while clock.elapsed() < Duration::from_secs(17) {
        if (kills + partitions) % 2 == 0 {
            let victim = (next() % 3) as usize;
            cluster.kill(victim);
            kills += 1;
            std::thread::sleep(Duration::from_millis(300 + next() % 700));
            cluster.start(victim, false);
        } else {
            let victim = isolate_next % 3;
            isolate_next += 1;
            cluster.isolate(victim);
            partitions += 1;
            std::thread::sleep(Duration::from_millis(1000 + next() % 1500));
            cluster.heal();
        }
        std::thread::sleep(Duration::from_millis(800 + next() % 1200));
    }
    let outcome = run.join().unwrap();
    assert!(outcome.protocol_errors.is_empty(), "protocol errors: {:?}", outcome.protocol_errors);
    assert!(kills >= 2 && partitions >= 3, "{kills} kills, {partitions} partitions");
    assert!(outcome.answered > 100, "only {} operations answered", outcome.answered);
    let (verdict, key) = check_partitioned(&KvModel, &outcome.history, |i| i.key().to_vec(), 50_000_000);
    let summary = match &verdict {
        Verdict::Linearizable => "linearizable".to_string(),
        Verdict::NotLinearizable { longest } => format!("not linearizable after {} operations", longest.len()),
        Verdict::Unknown => "unknown (search budget exceeded)".to_string(),
    };
    assert!(
        verdict == Verdict::Linearizable,
        "{summary} at key {:?}; {} answered, {} unanswered, {kills} kills, {partitions} partitions",
        key.map(|k| String::from_utf8_lossy(&k).into_owned()),
        outcome.answered,
        outcome.unanswered
    );
}
