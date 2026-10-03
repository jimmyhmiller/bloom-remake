//! `blossom run`: run a node of a deployment (DIST-040).
//!
//! `blossom run --deploy deploy.toml --node s1 --insecure-dev` compiles the deployment's program, opens and recovers
//! the node's store, and serves its peers and clients until the program halts or a tick faults. It prints one line
//! when it is ready. Every reply is released only after its tick's durable writes are synced, so killing the process
//! at any moment (even with SIGKILL) loses nothing that was acknowledged; SIGTERM is the same crash, which is always
//! legal (a graceful drain is a later slice).

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use blossom_front::api::NodeSpec;
use blossom_runtime::RuntimeError;
use blossom_runtime::deploy::{DeploymentSpec, SecurityMode};
use blossom_runtime::server::{Server, ServerConfig, Stopped};
use blossom_store::OpenMode;

use crate::common::{Context, bls};
use crate::exit::Exit;

/// Arguments of `blossom run`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The deployment spec (`deploy.toml`).
    #[arg(long = "deploy", value_name = "FILE")]
    pub deploy: PathBuf,
    /// The node to run.
    #[arg(long)]
    pub node: String,
    /// Create the node's store if it has none (a new node). Without it, a missing store is refused.
    #[arg(long)]
    pub init_fresh: bool,
    /// Allow the plaintext development transport (`security.mode = "insecure-dev"`).
    #[arg(long)]
    pub insecure_dev: bool,
    /// The node's store directory (default: `<storage.data_dir>/<node>`).
    #[arg(long)]
    pub store: Option<PathBuf>,
    /// The evaluator: `engine` (incremental, the default) or `oracle` (the reference semantics, re-evaluating the
    /// whole state every tick).
    #[arg(long, default_value = "engine")]
    pub evaluator: blossom_node::Backend,
    /// Write the node's counters (ticks, WAL records, messages, …) to this file every second, one `name value` per
    /// line, replacing it whole each time (not synced: it is for watching a node, not for recovery).
    #[arg(long, value_name = "FILE")]
    pub stats: Option<PathBuf>,
    /// Record every tick's inputs to a trace in this directory (`<node>-<incarnation>.blstrace`, readable by its
    /// owner only: it holds the deployment's seed), for `blossom trace` to replay and question.
    #[arg(long, value_name = "DIR")]
    pub record: Option<PathBuf>,
}

/// The exit code for a runtime error.
pub fn exit_of(e: &RuntimeError) -> Exit {
    match e {
        RuntimeError::Config(_) | RuntimeError::Node(_) | RuntimeError::Store(_) => Exit::Refused,
        RuntimeError::Fault(_) | RuntimeError::Net(_) | RuntimeError::Io(_) | RuntimeError::Wire(_) => Exit::Fault,
        RuntimeError::Eval(blossom_oracle::OracleError::Unimplemented(_)) | RuntimeError::Unimplemented(_) => {
            Exit::Unimplemented
        }
        RuntimeError::Eval(blossom_oracle::OracleError::Program { .. }) => Exit::Fault,
        RuntimeError::Eval(_) | RuntimeError::Internal(_) => Exit::Internal,
    }
}

/// Loads the deployment spec and compiles its program for its nodes.
pub fn load(deploy: &std::path::Path) -> Result<(DeploymentSpec, Arc<blossom_artifact::bls::BlsArtifact>), ExitCode> {
    let spec = DeploymentSpec::load(deploy).map_err(|e| {
        eprintln!("{e}");
        ExitCode::from(exit_of(&e))
    })?;
    let nodes: Vec<NodeSpec> = spec
        .nodes
        .iter()
        .map(|n| NodeSpec {
            name: n.name.clone(),
            role: n.role.clone(),
        })
        .collect();
    let Some(source) = spec.source.to_str() else {
        eprintln!("the program path {} is not UTF-8", spec.source.display());
        return Err(Exit::Refused.into());
    };
    let params = spec.params.iter().map(|(k, v)| (k.clone(), param_binding(v))).collect();
    let artifact = bls::compile_with(source, &nodes, &params)?;
    Ok((spec, Arc::new(artifact)))
}

/// A deployment's parameter value for the compiler.
pub fn param_binding(v: &blossom_runtime::deploy::ParamValue) -> blossom_front::api::ParamBinding {
    use blossom_front::api::ParamBinding as B;
    use blossom_runtime::deploy::ParamValue as V;
    match v {
        V::Int(n) => B::Int(*n),
        V::Bool(b) => B::Bool(*b),
        V::Text(t) => B::Text(t.clone()),
    }
}

/// Writes `stats` to `path` every second, for the life of the process: to a temporary file, renamed over `path`, so
/// a reader sees a whole snapshot. A failed write is reported once and the writer stops (the node runs on).
fn write_stats(path: &std::path::Path, stats: &blossom_runtime::server::Stats) {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    loop {
        let text: String = stats
            .snapshot()
            .iter()
            .map(|(name, value)| format!("{name} {value}\n"))
            .collect();
        if let Err(e) = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path)) {
            eprintln!("blossom run: writing the stats to {}: {e}", path.display());
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = cx;
    let (spec, artifact) = match load(&args.deploy) {
        Ok(x) => x,
        Err(code) => return code,
    };
    if spec.security == SecurityMode::InsecureDev && !args.insecure_dev {
        eprintln!("the deployment uses the plaintext development transport; pass `--insecure-dev` to allow it");
        return Exit::Refused.into();
    }
    let externs = match crate::common::std_externs() {
        Ok(x) => x,
        Err(e) => {
            eprintln!("blossom run: {e}");
            return Exit::Internal.into();
        }
    };
    let server = match Server::start(ServerConfig {
        spec,
        artifact,
        node: args.node.clone(),
        mode: if args.init_fresh {
            OpenMode::InitFresh
        } else {
            OpenMode::Existing
        },
        dir: args.store,
        backend: args.evaluator,
        externs,
        record: args.record.clone(),
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("blossom run: {e}");
            return exit_of(&e).into();
        }
    };
    let clients = server
        .client_addr
        .map_or_else(|| "no client listener".to_string(), |a| format!("clients on {a}"));
    println!(
        "blossom: node {} ready: peers on {}, {clients} (boot tick {}, incarnation {})",
        args.node, server.peer_addr, server.boot_tick.0, server.restarts
    );
    // The readiness line is how supervisors and tests know the node serves; a closed stdout is not fatal.
    let _ = std::io::stdout().flush();
    if let Some(path) = args.stats.clone() {
        let stats = server.stats.clone();
        let spawned = std::thread::Builder::new()
            .name("stats".into())
            .spawn(move || write_stats(&path, &stats));
        if let Err(e) = spawned {
            eprintln!("blossom run: cannot start the stats writer: {e}");
            return Exit::Internal.into();
        }
    }
    match server.wait() {
        Ok(Stopped::Halted) => {
            eprintln!("blossom: node {} halted", args.node);
            Exit::Fault.into()
        }
        Ok(Stopped::Stopped) => Exit::Ok.into(),
        Err(e) => {
            eprintln!("blossom run: {e}");
            exit_of(&e).into()
        }
    }
}
