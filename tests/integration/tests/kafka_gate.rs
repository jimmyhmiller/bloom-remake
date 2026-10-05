//! Slice 6's gate (docs/plan/notes/S6.md item 7): the Blossom Kafka broker (`examples/kafka/broker.bls`) runs as a
//! node, and stock Kafka tools talk to it over TCP.
//!
//! - `kcat -L` (librdkafka) lists the broker.
//! - `kafka-broker-api-versions.sh` (the Java client of Kafka 4.0) prints the versions it supports.
//! - franz-go (a Go client) reads the metadata.
//! - Slice 8, item 4: `kafka-cluster.sh cluster-id` reads the cluster id with DescribeCluster.
//! - Slice 7, item 4: `kafka-topics.sh` creates, lists, describes and deletes topics, and `kcat -L` shows them.
//! - Slice 7, item 8: franz-go (idempotent by default) produces to three partitions and reads them back.
//! - Slice 7, items 5 and 6: `kcat -P` produces and `kcat -C` reads it back; the Java console producer (idempotent
//!   by default) produces, the Java console consumer reads each partition from the earliest offset, and
//!   `kafka-get-offsets.sh` counts them.
//!
//! A test whose tool is not installed is skipped, and says so in its output. The tools are found on `PATH` (`kcat`,
//! `go`), under `$KAFKA_HOME/bin`, or in the repository's `.tools/kafka_*/bin` (see the notes for how to get them).

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use blossom_front::api::NodeSpec;
use blossom_runtime::deploy::DeploymentSpec;
use blossom_runtime::server::{Server, ServerConfig};
use blossom_store::OpenMode;

#[cfg(test)]
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The repository root (this crate is `tests/integration`).
#[cfg(test)]
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The broker, started as node `b1`, with its `kafka` stream on a free port it also advertises.
#[cfg(test)]
fn start_broker() -> (Server, u16) {
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("blossom-kafka-{}-{port}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = dir.join("k.secrets");
    std::fs::write(&secrets, "seed = \"00112233445566778899aabbccddeeff\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let source = repo().join("examples/kafka/broker.bls");
    let text = format!(
        r#"format = 1
[deployment]
id = "kafka-gate"
program = "kafka"
version = 1
source = "{}"
secrets = "k.secrets"
[[node]]
name = "b1"
role = "Broker"
addr = "127.0.0.1:{}"
principal = "spiffe://test/kafka/b1"
streams = {{ kafka = "127.0.0.1:{port}" }}
[statics]
broker = [["b1", 1, "127.0.0.1", {port}]]
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
    let params = std::collections::BTreeMap::new();
    let (compiled, sources) = blossom_driver::bls::compile_file_with(&spec.source.to_string_lossy(), &nodes, &params);
    let artifact = match compiled {
        Ok((a, _)) => Arc::new(a),
        Err(blossom_front::api::BlsError::Rejected(d)) => {
            let text: Vec<String> = d.iter().map(|x| blossom_driver::render::render(x, &sources)).collect();
            panic!("broker.bls does not compile:\n{}", text.join(""))
        }
        Err(e) => panic!("{e}"),
    };
    let server = Server::start(ServerConfig {
        spec,
        artifact,
        node: "b1".into(),
        mode: OpenMode::InitFresh,
        dir: None,
        backend: blossom_node::Backend::Engine,
        externs: Arc::new(blossom_std_host::registry().unwrap()),
        record: None,
    })
    .unwrap();
    (server, port)
}

/// Reports a test skipped because its tool is missing: the gate's tests say so in their output (S6 notes).
#[cfg(test)]
#[allow(clippy::print_stdout)] // The notice is this function's purpose: a silent skip would look like a pass.
fn skipped(why: &str) {
    println!("SKIPPED: {why}");
}

/// A tool on `PATH`, or `None` (the test then says it is skipped).
#[cfg(test)]
fn on_path(tool: &str) -> Option<PathBuf> {
    let out = Command::new("which").arg(tool).output().ok()?;
    out.status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// Kafka's `bin` directory: `$KAFKA_HOME/bin`, or the repository's `.tools/kafka_*/bin`.
#[cfg(test)]
fn kafka_bin() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("KAFKA_HOME") {
        let bin = PathBuf::from(home).join("bin");
        if bin.join("kafka-broker-api-versions.sh").exists() {
            return Some(bin);
        }
    }
    // The main checkout's `.tools`, which worktrees under `.worktrees/` share.
    for base in [repo(), repo().join("../..")] {
        let tools = base.join(".tools");
        if let Ok(entries) = std::fs::read_dir(&tools) {
            for e in entries.flatten() {
                let bin = e.path().join("bin");
                if e.file_name().to_string_lossy().starts_with("kafka_")
                    && bin.join("kafka-broker-api-versions.sh").exists()
                {
                    return Some(bin);
                }
            }
        }
    }
    None
}

#[test]
fn kcat_lists_the_blossom_broker() {
    let Some(kcat) = kcat() else {
        return;
    };
    let (server, port) = start_broker();
    let out = Command::new(kcat)
        .args(["-L", "-b", &format!("127.0.0.1:{port}"), "-m", "10"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "kcat failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("1 brokers:"), "{stdout}");
    assert!(
        stdout.contains(&format!("broker 1 at 127.0.0.1:{port} (controller)")),
        "{stdout}"
    );
    assert!(stdout.contains("0 topics:"), "{stdout}");
    server.stop().unwrap();
}

#[test]
fn kafka_broker_api_versions_prints_the_supported_versions() {
    let Some(bin) = kafka_bin() else {
        skipped("no Kafka distribution (set KAFKA_HOME, or unpack one into .tools/)");
        return;
    };
    let (server, port) = start_broker();
    let out = Command::new(bin.join("kafka-broker-api-versions.sh"))
        .args(["--bootstrap-server", &format!("127.0.0.1:{port}")])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "the tool failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains(&format!("127.0.0.1:{port} (id: 1 rack: null")),
        "{stdout}"
    );
    assert!(stdout.contains("Metadata(3): 13 [usable: 13]"), "{stdout}");
    assert!(stdout.contains("ApiVersions(18): 3 to 4 [usable: 4]"), "{stdout}");
    server.stop().unwrap();
}

/// S8 item 4: `kafka-cluster.sh cluster-id` asks with DescribeCluster (60).
#[test]
fn kafka_cluster_prints_the_cluster_id() {
    let Some(bin) = kafka_bin() else {
        skipped("no Kafka distribution (set KAFKA_HOME, or unpack one into .tools/)");
        return;
    };
    let (server, port) = start_broker();
    // The subcommand comes before its options, so not through `kafka_tool`.
    let out = Command::new(bin.join("kafka-cluster.sh"))
        .args(["cluster-id", "--bootstrap-server", &format!("127.0.0.1:{port}")])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "the tool failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Cluster ID: blossom-kafka"), "{stdout}");
    server.stop().unwrap();
}

#[test]
fn franz_go_reads_the_metadata() {
    let Some(go) = on_path("go") else {
        skipped("Go is not installed");
        return;
    };
    let (server, port) = start_broker();
    let dir = repo().join("tests/integration/fixtures/kafka/franz");
    let out = Command::new(go)
        .args(["run", ".", &format!("127.0.0.1:{port}")])
        .current_dir(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "franz-go failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains(&format!("broker 1 at 127.0.0.1:{port}")), "{stdout}");
    assert!(stdout.contains("0 topics"), "{stdout}");
    server.stop().unwrap();
}

/// Runs a Kafka tool against the broker and returns its standard output, failing the test if it fails.
#[cfg(test)]
fn kafka_tool(bin: &Path, tool: &str, port: u16, args: &[&str]) -> String {
    let out = Command::new(bin.join(tool))
        .args(["--bootstrap-server", &format!("127.0.0.1:{port}")])
        .args(args)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{tool} {args:?} failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

#[test]
fn kafka_topics_creates_describes_and_deletes_topics() {
    let Some(bin) = kafka_bin() else {
        skipped("no Kafka distribution (set KAFKA_HOME, or unpack one into .tools/)");
        return;
    };
    let (server, port) = start_broker();
    let created = kafka_tool(
        &bin,
        "kafka-topics.sh",
        port,
        &[
            "--create",
            "--topic",
            "orders",
            "--partitions",
            "3",
            "--config",
            "retention.ms=60000",
        ],
    );
    assert!(created.contains("Created topic orders."), "{created}");
    kafka_tool(&bin, "kafka-topics.sh", port, &["--create", "--topic", "audit"]);
    // A second creation is refused, as Kafka refuses it.
    let again = Command::new(bin.join("kafka-topics.sh"))
        .args([
            "--bootstrap-server",
            &format!("127.0.0.1:{port}"),
            "--create",
            "--topic",
            "orders",
        ])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&again.stdout),
        String::from_utf8_lossy(&again.stderr)
    );
    assert!(text.contains("already exists"), "{text}");
    let listed = kafka_tool(&bin, "kafka-topics.sh", port, &["--list"]);
    assert_eq!(listed.lines().collect::<Vec<_>>(), ["audit", "orders"], "{listed}");
    let described = kafka_tool(&bin, "kafka-topics.sh", port, &["--describe", "--topic", "orders"]);
    assert!(described.contains("Topic: orders"), "{described}");
    assert!(described.contains("PartitionCount: 3"), "{described}");
    assert!(described.contains("ReplicationFactor: 1"), "{described}");
    assert!(described.contains("Configs: retention.ms=60000"), "{described}");
    for p in 0..3 {
        assert!(
            described.contains(&format!("Partition: {p}\tLeader: 1\tReplicas: 1\tIsr: 1")),
            "{described}"
        );
    }
    let configs = kafka_tool(
        &bin,
        "kafka-configs.sh",
        port,
        &["--describe", "--entity-type", "topics", "--entity-name", "orders"],
    );
    assert!(configs.contains("retention.ms=60000"), "{configs}");
    if let Some(kcat) = kcat() {
        let out = Command::new(kcat)
            .args(["-L", "-b", &format!("127.0.0.1:{port}"), "-m", "10"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("2 topics:"), "{stdout}");
        assert!(stdout.contains("topic \"orders\" with 3 partitions:"), "{stdout}");
        assert!(stdout.contains("topic \"audit\" with 1 partitions:"), "{stdout}");
    }
    kafka_tool(&bin, "kafka-topics.sh", port, &["--delete", "--topic", "orders"]);
    let listed = kafka_tool(&bin, "kafka-topics.sh", port, &["--list"]);
    assert_eq!(listed.lines().collect::<Vec<_>>(), ["audit"], "{listed}");
    server.stop().unwrap();
}

/// Runs `cmd` with `lines` on its standard input and returns its standard output, failing the test if it fails.
#[cfg(test)]
fn with_input(cmd: &mut Command, lines: &[String]) -> String {
    use std::io::Write;
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for l in lines {
            writeln!(stdin, "{l}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{cmd:?} failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

#[test]
fn kcat_produces_and_consumes() {
    let Some(kcat) = kcat() else {
        return;
    };
    let (server, port) = start_broker();
    let broker = format!("127.0.0.1:{port}");
    let messages: Vec<String> = (0..50).map(|i| format!("message {i}")).collect();
    // The topic is created by the producer's Metadata request (auto-creation).
    with_input(
        Command::new(&kcat).args([
            "-P",
            "-b",
            &broker,
            "-t",
            "events",
            "-p",
            "0",
            "-X",
            "topic.request.required.acks=-1",
        ]),
        &messages,
    );
    let out = Command::new(&kcat)
        .args([
            "-C",
            "-b",
            &broker,
            "-t",
            "events",
            "-p",
            "0",
            "-o",
            "beginning",
            "-e",
            "-q",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "kcat -C failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        messages.iter().map(String::as_str).collect::<Vec<_>>()
    );
    // Reading from an offset inside the log.
    let out = Command::new(&kcat)
        .args(["-C", "-b", &broker, "-t", "events", "-p", "0", "-o", "45", "-e", "-q"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        messages[45..].iter().map(String::as_str).collect::<Vec<_>>()
    );
    server.stop().unwrap();
}

#[test]
fn the_java_console_tools_produce_and_consume() {
    let Some(bin) = kafka_bin() else {
        skipped("no Kafka distribution (set KAFKA_HOME, or unpack one into .tools/)");
        return;
    };
    let (server, port) = start_broker();
    let broker = format!("127.0.0.1:{port}");
    kafka_tool(
        &bin,
        "kafka-topics.sh",
        port,
        &["--create", "--topic", "orders", "--partitions", "2"],
    );
    let messages: Vec<String> = (0..20).map(|i| format!("order {i}")).collect();
    // The console producer is idempotent by default (InitProducerId, sequence numbers).
    let out = with_input(
        Command::new(bin.join("kafka-console-producer.sh")).args(["--bootstrap-server", &broker, "--topic", "orders"]),
        &messages,
    );
    assert!(!out.contains("ERROR"), "{out}");
    let offsets = kafka_tool(&bin, "kafka-get-offsets.sh", port, &["--topic", "orders"]);
    let total: i64 = offsets
        .lines()
        .map(|l| l.rsplit(':').next().unwrap().trim().parse::<i64>().unwrap())
        .sum();
    assert_eq!(total, 20, "{offsets}");
    // Each partition read back from the earliest offset: together, every message once.
    let mut read = Vec::new();
    for p in ["0", "1"] {
        let out = Command::new(bin.join("kafka-console-consumer.sh"))
            .args([
                "--bootstrap-server",
                &broker,
                "--topic",
                "orders",
                "--partition",
                p,
                "--offset",
                "earliest",
                "--timeout-ms",
                "5000",
            ])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        read.extend(stdout.lines().map(str::to_owned));
    }
    read.sort();
    let mut want = messages.clone();
    want.sort();
    assert_eq!(read, want);
    server.stop().unwrap();
}

#[test]
fn franz_go_produces_and_consumes() {
    let Some(go) = on_path("go") else {
        skipped("Go is not installed");
        return;
    };
    let (server, port) = start_broker();
    let dir = repo().join("tests/integration/fixtures/kafka/franz");
    let out = Command::new(go)
        .args(["run", ".", "produce-consume", &format!("127.0.0.1:{port}")])
        .current_dir(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "franz-go failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("ok 300 records"), "{stdout}");
    server.stop().unwrap();
}

/// kcat, when it is installed and its librdkafka can talk to the broker, else `None` (and the test says it skips).
/// The broker answers Metadata v13 only (with Produce 10+ and Fetch 16+, Kafka 4.0's newest), and librdkafka 2.11.0
/// is the first release that asks for v13: an older one finds no Metadata version in common and fails every request
/// ("Required feature not supported by broker").
#[cfg(test)]
fn kcat() -> Option<PathBuf> {
    let Some(path) = on_path("kcat") else {
        skipped("kcat is not installed (brew install kcat)");
        return None;
    };
    let out = Command::new(&path).arg("-V").output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let version: Vec<u32> = text
        .split("librdkafka ")
        .nth(1)
        .and_then(|rest| rest.split([' ', ')']).next())
        .map(|v| v.split('.').filter_map(|x| x.parse().ok()).collect())
        .unwrap_or_else(|| panic!("no librdkafka version in `kcat -V`: {text}"));
    if version.as_slice() < [2, 11, 0].as_slice() {
        skipped(&format!(
            "kcat's librdkafka {} is older than 2.11.0, the first that speaks Metadata v13, the broker's only version",
            version.iter().map(u32::to_string).collect::<Vec<_>>().join(".")
        ));
        return None;
    }
    Some(path)
}
