//! Slice 6's gate (docs/plan/notes/S6.md item 7): the Blossom Kafka broker (`examples/kafka/broker.bls`) runs as a
//! node, and stock Kafka tools talk to it over TCP.
//!
//! - `kcat -L` (librdkafka) lists the broker.
//! - `kafka-broker-api-versions.sh` (the Java client of Kafka 4.0) prints the versions it supports.
//! - franz-go (a Go client) reads the metadata.
//! - Slice 7, item 4: `kafka-topics.sh` creates, lists, describes and deletes topics, and `kcat -L` shows them.
//!
//! A test whose tool is not installed is skipped, and says so in its output. The tools are found on `PATH` (`kcat`,
//! `go`), under `$KAFKA_HOME/bin`, or in the repository's `.tools/kafka_*/bin` (see the notes for how to get them).

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use blossom_front::api::{NodeSpec, ParamBinding};
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
addr = "127.0.0.1:{}"
principal = "spiffe://test/kafka/b1"
streams = {{ kafka = "127.0.0.1:{port}" }}
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
    let params = [
        ("ADVERTISED_HOST".to_owned(), ParamBinding::Text("127.0.0.1".into())),
        ("ADVERTISED_PORT".to_owned(), ParamBinding::Int(i128::from(port))),
    ]
    .into_iter()
    .collect();
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
    let Some(kcat) = on_path("kcat") else {
        skipped("kcat is not installed (brew install kcat)");
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
        &["--create", "--topic", "orders", "--partitions", "3", "--config", "retention.ms=60000"],
    );
    assert!(created.contains("Created topic orders."), "{created}");
    kafka_tool(&bin, "kafka-topics.sh", port, &["--create", "--topic", "audit"]);
    // A second creation is refused, as Kafka refuses it.
    let again = Command::new(bin.join("kafka-topics.sh"))
        .args(["--bootstrap-server", &format!("127.0.0.1:{port}"), "--create", "--topic", "orders"])
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&again.stdout), String::from_utf8_lossy(&again.stderr));
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
    if let Some(kcat) = on_path("kcat") {
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
