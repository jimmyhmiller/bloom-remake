//! Peer liveness (ARCHITECTURE §5.13) across real processes: three `blossom run` brokers, every link between them
//! through a proxy the test controls, each recording what `Broker.up` / `Broker.down` tell it in a durable table the
//! test reads with `blossom query`. A killed broker is taken down after the liveness session, and comes back in its
//! next incarnation; a frozen one (SIGSTOP) is taken down and comes back when it resumes; a one-way cut takes a broker
//! down at the far end only.

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use blossom_bench::stopwatch::Stopwatch;

use crate::raft3::{Proxy, free_addr};

const PROGRAM: &str = "program live version 1;
role Broker: cluster;
at Broker {
    durable table alive(b: Node<Broker>, i: u64) key(b);
    noted_up: on Broker.up(b, i) {
        upsert alive(b, i);
    }
    noted_down: on Broker.down(b), alive(b, i), not Broker.up(b, _) {
        delete alive(b, i);
    }
}
";

#[cfg(test)]
struct Brokers {
    dir: PathBuf,
    deploy: PathBuf,
    admin: Vec<String>,
    procs: Vec<Option<Child>>,
    /// `proxies[(i, j)]`: the link from broker i to broker j (data and liveness connections alike).
    proxies: Vec<((usize, usize), Proxy)>,
}

#[cfg(test)]
impl Brokers {
    fn new() -> Brokers {
        let dir = std::env::temp_dir().join(format!("blossom-liveness-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("live.bls"), PROGRAM).unwrap();
        let peer: Vec<SocketAddr> = (0..3).map(|_| free_addr()).collect();
        let mut proxies = Vec::new();
        for i in 0..3 {
            for (j, upstream) in peer.iter().enumerate() {
                if i != j {
                    proxies.push(((i, j), Proxy::start(*upstream)));
                }
            }
        }
        let mut spec = "format = 1\n\n[deployment]\nid = \"live\"\nprogram = \"live\"\nversion = 1\n\
                        source = \"live.bls\"\nsecrets = \"live.secrets\"\n"
            .to_owned();
        for (i, addr) in peer.iter().enumerate() {
            let dial: Vec<String> = proxies
                .iter()
                .filter(|((a, _), _)| *a == i)
                .map(|((_, b), p)| format!("b{} = \"{}\"", b + 1, p.addr))
                .collect();
            spec.push_str(&format!(
                "\n[[node]]\nname = \"b{n}\"\nrole = \"Broker\"\naddr = \"{}\"\nprincipal = \"spiffe://test/live/b{n}\"\n\
                 dial = {{ {} }}\n",
                addr,
                dial.join(", "),
                n = i + 1
            ));
        }
        spec.push_str(
            "\n[security]\nmode = \"insecure-dev\"\n\n[storage]\ndata_dir = \"data\"\n\n\
             [liveness]\nperiod_ms = 50\nsession_ms = 500\n",
        );
        let deploy = dir.join("deploy.toml");
        std::fs::write(&deploy, spec).unwrap();
        let secrets = dir.join("live.secrets");
        std::fs::write(&secrets, "seed = \"0f0e0d0c0b0a09080706050403020100\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut b = Brokers {
            dir,
            deploy,
            admin: (0..3).map(|_| free_addr().to_string()).collect(),
            procs: vec![None, None, None],
            proxies,
        };
        for i in 0..3 {
            b.start(i, true);
        }
        b
    }

    fn start(&mut self, i: usize, fresh: bool) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blossom"));
        cmd.args(["run", "--deploy"]).arg(&self.deploy).args([
            "--node",
            &format!("b{}", i + 1),
            "--insecure-dev",
            "--admin",
            &self.admin[i],
        ]);
        if fresh {
            cmd.arg("--init-fresh");
        }
        let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(line.contains("ready"), "b{} did not come up: {line:?}", i + 1);
        self.procs[i] = Some(child);
    }

    fn kill(&mut self, i: usize) {
        if let Some(mut c) = self.procs[i].take() {
            c.kill().unwrap(); // SIGKILL
            c.wait().unwrap();
        }
    }

    fn signal(&self, i: usize, sig: &str) {
        let pid = self.procs[i].as_ref().unwrap().id().to_string();
        let st = Command::new("kill").args([sig, &pid]).status().unwrap();
        assert!(st.success(), "kill {sig} {pid}");
    }

    fn cut(&self, a: usize, b: usize, cut: bool) {
        for ((x, y), p) in &self.proxies {
            if (*x, *y) == (a, b) {
                if cut {
                    p.block();
                } else {
                    p.heal();
                }
            }
        }
    }

    /// The brokers broker `i` holds alive, with their incarnations, as `query` prints them.
    fn view(&self, i: usize) -> Vec<String> {
        let out = Command::new(env!("CARGO_BIN_EXE_blossom"))
            .args(["query", "--deploy"])
            .arg(&self.deploy)
            .args(["--node", &format!("b{}", i + 1), "--admin", &self.admin[i]])
            .arg("a(b, i) = alive(b, i)")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let mut rows: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .skip(1)
            .map(str::to_owned)
            .collect();
        rows.sort();
        rows
    }

    fn holds(&self, i: usize, peer: usize) -> bool {
        let name = format!("b{}", peer + 1);
        self.view(i)
            .iter()
            .any(|r| r.split('\t').next().is_some_and(|c| c.contains(&name)))
    }

    /// Waits until `cond` holds of broker `i`'s view; returns how long that took.
    fn until(&self, i: usize, within: Duration, what: &str, cond: impl Fn(&[String]) -> bool) -> Duration {
        let clock = Stopwatch::start();
        loop {
            let v = self.view(i);
            if cond(&v) {
                return clock.elapsed();
            }
            assert!(
                clock.elapsed() < within,
                "{what}: not within {within:?}; b{} holds {v:?}",
                i + 1
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
impl Drop for Brokers {
    fn drop(&mut self) {
        for i in 0..3 {
            if self.procs[i].is_some() {
                let _ = Command::new("kill")
                    .args(["-CONT", &self.procs[i].as_ref().unwrap().id().to_string()])
                    .status();
            }
            self.kill(i);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn brokers_see_each_other_go_down_and_come_back() {
    let _one = crate::one_cluster();
    let mut b = Brokers::new();
    let has = |v: &[String], n: usize| v.iter().any(|r| r.contains(&format!("b{n}")));
    // Each broker holds all three alive, itself included, in their first incarnation.
    for i in 0..3 {
        b.until(i, Duration::from_secs(10), "all up", |v| v.len() == 3);
    }
    // Incarnations count from 1 (a store's first boot).
    assert!(b.view(0).iter().all(|r| r.ends_with("\t1")), "{:?}", b.view(0));

    // Killed: down at the others after the session (500 ms), not at once (a closed connection decides nothing).
    b.kill(2);
    let took = b.until(0, Duration::from_secs(3), "b3 down at b1", |v| !has(v, 3));
    assert!(
        took >= Duration::from_millis(300),
        "b3 was taken down after only {took:?}"
    );
    // Restarted: up again, in its next incarnation.
    b.start(2, false);
    b.until(0, Duration::from_secs(10), "b3 back at b1", |v| {
        v.iter().any(|r| r.contains("b3") && r.ends_with("\t2"))
    });

    // Frozen (SIGSTOP, as a long pause): down after the session; resumed (SIGCONT): up again, the same incarnation.
    b.signal(1, "-STOP");
    let took = b.until(0, Duration::from_secs(3), "frozen b2 down at b1", |v| !has(v, 2));
    assert!(
        took >= Duration::from_millis(300),
        "b2 was taken down after only {took:?}"
    );
    b.signal(1, "-CONT");
    b.until(0, Duration::from_secs(5), "resumed b2 back at b1", |v| {
        v.iter().any(|r| r.contains("b2") && r.ends_with("\t1"))
    });

    // A one-way cut (b3 to b1): b1 takes b3 down; b3 still hears b1.
    b.cut(2, 0, true);
    b.until(0, Duration::from_secs(3), "cut b3 down at b1", |v| !has(v, 3));
    assert!(b.holds(2, 0), "b3 lost b1 though only b3 -> b1 is cut: {:?}", b.view(2));
    b.cut(2, 0, false);
    b.until(0, Duration::from_secs(5), "healed b3 back at b1", |v| has(v, 3));
}
