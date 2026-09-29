//! `blossom-kv`: drive a key-value store with the closed-loop workload, report throughput and latency, and check
//! the recorded history for linearizability.
//!
//! `blossom-kv load --deploy deploy.toml --clients 16 --duration 10s --check` runs against the client listeners of
//! the deployment's nodes (the e01 protocol).

#![deny(unsafe_op_in_unsafe_fn)]
// A command-line tool reports on stdout and stderr.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use blossom_bench::blossom_kv::BlossomKvStore;
use blossom_bench::etcd::{EtcdStore, Routing};
use blossom_bench::kv::{self, KvStore, Outcome, Workload};
use blossom_front::api::NodeSpec;
use blossom_runtime::deploy::DeploymentSpec;
use blossom_sim::linearize::{KvModel, Verdict, check_partitioned};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "blossom-kv")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the workload against a Blossom deployment's nodes.
    Load(Load),
    /// Run the same workload against etcd (its v3 JSON gateway).
    Etcd(Etcd),
}

#[derive(Debug, clap::Args)]
struct Etcd {
    /// Client URLs as `host:port`, comma-separated.
    #[arg(long, value_delimiter = ',', default_value = "127.0.0.1:2379")]
    endpoints: Vec<std::net::SocketAddr>,
    /// `leader` (every session at the leader, as the Blossom client) or `spread` (client c at endpoint c mod n).
    #[arg(long, default_value = "leader")]
    route: String,
    #[command(flatten)]
    common: Common,
}

#[derive(Debug, clap::Args)]
struct Load {
    #[arg(long = "deploy")]
    deploy: std::path::PathBuf,
    #[command(flatten)]
    common: Common,
    /// The principal the client sessions claim.
    #[arg(long, default_value = "spiffe://dev/kvs/client/admin")]
    principal: String,
}

#[derive(Debug, clap::Args)]
struct Common {
    #[arg(long, default_value_t = 16)]
    clients: usize,
    /// How long to run, in seconds.
    #[arg(long, default_value_t = 10.0)]
    seconds: f64,
    #[arg(long, default_value_t = 1000)]
    keys: usize,
    /// Weights of put, get and delete, as `put:get:del`.
    #[arg(long, default_value = "50:50:0")]
    mix: String,
    #[arg(long, default_value_t = 16)]
    value_size: usize,
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// Reply timeout in milliseconds.
    #[arg(long, default_value_t = 2000)]
    timeout_ms: u64,
    /// Record the history and check it for linearizability.
    #[arg(long)]
    check: bool,
    /// A prefix for every key (default: unique per run, so earlier runs' data is never read).
    #[arg(long)]
    namespace: Option<String>,
}

fn workload(c: &Common) -> Result<Workload, String> {
    let parts: Vec<u32> = c
        .mix
        .split(':')
        .map(|p| p.parse::<u32>().map_err(|e| format!("--mix: {e}")))
        .collect::<Result<_, _>>()?;
    let [p, g, d] = parts.as_slice() else {
        return Err("--mix is put:get:del".into());
    };
    Ok(Workload {
        clients: c.clients,
        duration: Duration::from_secs_f64(c.seconds),
        keys: c.keys,
        mix: (*p, *g, *d),
        value_size: c.value_size,
        seed: c.seed,
        namespace: c
            .namespace
            .clone()
            .unwrap_or_else(|| format!("r{}-", std::process::id())),
        record: c.check,
    })
}

fn report(name: &str, w: &Workload, o: &Outcome, check: bool) -> bool {
    println!(
        "{name}: {} clients, {:.1}s: {} ops answered, {} unanswered, {:.0} ops/s; latency p50 {:?} p99 {:?} p99.9 {:?} max {:?}",
        w.clients,
        o.elapsed.as_secs_f64(),
        o.answered,
        o.unanswered,
        o.throughput(),
        o.latency(0.5),
        o.latency(0.99),
        o.latency(0.999),
        o.latency(1.0)
    );
    if !o.protocol_errors.is_empty() {
        println!(
            "PROTOCOL ERRORS ({}): first: {}",
            o.protocol_errors.len(),
            o.protocol_errors.first().map_or("", |e| e.as_str())
        );
        return false;
    }
    if !check {
        return true;
    }
    let (verdict, key) = check_partitioned(&KvModel, &o.history, |i| i.key().to_vec(), 50_000_000);
    let key = key.map(|k| String::from_utf8_lossy(&k).into_owned());
    match verdict {
        Verdict::Linearizable => {
            println!("linearizable: {} operations checked", o.history.len());
            true
        }
        Verdict::NotLinearizable { longest } => {
            println!(
                "NOT linearizable at key {key:?}: longest linearizable prefix {} ops",
                longest.len()
            );
            let mut ops: Vec<_> = o
                .history
                .iter()
                .filter(|op| Some(String::from_utf8_lossy(op.input.key()).into_owned()) == key)
                .collect();
            ops.sort_by_key(|op| op.call);
            for op in ops {
                println!(
                    "  [{:>12} .. {:>12}] {:?} -> {:?}",
                    op.call,
                    op.ret.map_or("-".to_string(), |r| r.to_string()),
                    op.input,
                    op.output
                );
            }
            false
        }
        Verdict::Unknown => {
            println!("linearizability unknown at key {key:?}: the search exceeded its budget");
            false
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Load(args) => load(args),
        Command::Etcd(args) => etcd(args),
    }
}

fn run_and_report(name: &str, store: Arc<dyn KvStore>, common: &Common) -> ExitCode {
    let w = match workload(common) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let o = kv::run(store, &w, Arc::new(AtomicBool::new(false)));
    if report(name, &w, &o, common.check) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(3)
    }
}

fn etcd(args: Etcd) -> ExitCode {
    let routing = match args.route.as_str() {
        "leader" => Routing::Leader,
        "spread" => Routing::Spread,
        other => {
            eprintln!("--route: unknown routing {other:?} (leader or spread)");
            return ExitCode::from(2);
        }
    };
    let store: Arc<dyn KvStore> = Arc::new(EtcdStore {
        endpoints: args.endpoints,
        timeout: Duration::from_millis(args.common.timeout_ms),
        routing,
    });
    run_and_report("etcd", store, &args.common)
}

fn load(args: Load) -> ExitCode {
    let spec = match DeploymentSpec::load(&args.deploy) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(5);
        }
    };
    let nodes: Vec<NodeSpec> = spec
        .nodes
        .iter()
        .map(|n| NodeSpec {
            name: n.name.clone(),
            role: n.role.clone(),
        })
        .collect();
    let params = spec
        .params
        .iter()
        .map(|(k, v)| {
            use blossom_front::api::ParamBinding as B;
            use blossom_runtime::deploy::ParamValue as V;
            let b = match v {
                V::Int(n) => B::Int(*n),
                V::Bool(b) => B::Bool(*b),
                V::Text(t) => B::Text(t.clone()),
            };
            (k.clone(), b)
        })
        .collect();
    let (result, _) = blossom_driver::bls::compile_file_with(&spec.source.to_string_lossy(), &nodes, &params);
    let artifact = match result {
        Ok((a, _)) => Arc::new(a),
        Err(e) => {
            eprintln!("compiling {}: {e:?}", spec.source.display());
            return ExitCode::from(1);
        }
    };
    let store: Arc<dyn KvStore> = Arc::new(BlossomKvStore {
        addrs: spec.nodes.iter().map(|n| n.client_addr).collect(),
        id: blossom_runtime::server::identity(&spec, &artifact),
        artifact,
        principal: args.principal,
        timeout: Duration::from_millis(args.common.timeout_ms),
    });
    run_and_report("blossom", store, &args.common)
}
