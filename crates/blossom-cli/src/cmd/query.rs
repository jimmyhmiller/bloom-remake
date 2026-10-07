//! `blossom query`: a Datalog query of a node's database (docs/design/DATABASE.md §5).
//!
//! The query is a view, `name(columns) = body`, over the program's durable relations. It is compiled with the
//! deployed program (at the node's role), cut down to the rules computing it (`ValidatedProgram::query`), and sent to
//! the running node's admin listener (`blossom run --admin`), which answers it from its database as of a released
//! tick: the newest, or `--as-of TICK` within the history the node keeps.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;

use blossom_driver::ded::FsLoader;
use blossom_front::api::NodeSpec;
use blossom_front::ded::LoadedFile;
use blossom_front::modules::Loader;
use blossom_runtime::deploy::DeploymentSpec;
use blossom_runtime::query::{Answer, QueryRequest};

use crate::common::Context;
use crate::exit::Exit;

/// Arguments of `blossom query`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The deployment spec (`deploy.toml`).
    #[arg(long = "deploy", value_name = "FILE")]
    pub deploy: PathBuf,
    /// The node whose database to query.
    #[arg(long)]
    pub node: String,
    /// The running node's admin listener (`blossom run --admin ADDR`).
    #[arg(
        long,
        value_name = "ADDR",
        conflicts_with = "store",
        required_unless_present = "store"
    )]
    pub admin: Option<SocketAddr>,
    /// A stopped node's store directory, read in place (it takes the store's lock).
    #[arg(long, value_name = "DIR")]
    pub store: Option<PathBuf>,
    /// Read the database as of this tick (default: the newest released).
    #[arg(long, value_name = "TICK")]
    pub as_of: Option<u64>,
    /// The query: a view, `name(columns) = body`.
    pub query: String,
}

/// What the query's view is called inside the program.
const QUERY_PREFIX: &str = "__query_";

/// The file the query's view is in, which the root file includes (at the node's role): what its diagnostics name.
const QUERY_FILE: &str = "<query>.bls";

/// The program's files from disk, the root file including the query's.
struct WithQuery {
    /// What the root file gains (the include), and the query file's text.
    include: String,
    query: String,
}

impl Loader for WithQuery {
    fn load(&mut self, from: Option<&str>, path: &str) -> Result<LoadedFile, String> {
        if path == QUERY_FILE {
            return Ok(LoadedFile {
                key: std::sync::Arc::from(QUERY_FILE),
                text: self.query.clone(),
            });
        }
        let mut f = Loader::load(&mut FsLoader, from, path)?;
        if from.is_none() {
            f.text.push_str(&self.include);
        }
        Ok(f)
    }
}

/// The view's name: what comes before its columns.
fn view_name(query: &str) -> Option<&str> {
    let (head, _) = query.split_once('(')?;
    let name = head.trim();
    let ok = name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    (ok && query.contains('=')).then_some(name)
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = cx;
    let Some(name) = view_name(&args.query) else {
        eprintln!(
            "blossom query: a query is a view, `name(columns) = body` (got `{}`)",
            args.query
        );
        return Exit::Usage.into();
    };
    let spec = match DeploymentSpec::load(&args.deploy) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("blossom query: {e}");
            return Exit::Refused.into();
        }
    };
    let Some(entry) = spec.nodes.iter().find(|n| n.name == args.node) else {
        eprintln!("blossom query: the deployment has no node `{}`", args.node);
        return Exit::Usage.into();
    };
    // The view lives in the program's namespace: a name of its own keeps the query's name from clashing with the
    // program's (`total` is a common view name).
    let internal = format!("{QUERY_PREFIX}{name}");
    let Some(rest) = args.query.trim().trim_end_matches(';').strip_prefix(name) else {
        eprintln!(
            "blossom query: a query is a view, `name(columns) = body` (got `{}`)",
            args.query
        );
        return Exit::Usage.into();
    };
    let query = format!("view {internal}{rest};\n");
    let include = match &entry.role {
        Some(role) => format!("\n\nat {role} {{\n    include \"{QUERY_FILE}\";\n}}\n"),
        None => format!("\n\ninclude \"{QUERY_FILE}\";\n"),
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
        .map(|(k, v)| (k.clone(), crate::cmd::run::param_binding(v)))
        .collect();
    let Some(root) = spec.source.to_str() else {
        eprintln!("blossom query: the program path {} is not UTF-8", spec.source.display());
        return Exit::Refused.into();
    };
    let (compiled, sources) =
        blossom_driver::bls::compile_with_loader(root, &nodes, &params, &mut WithQuery { include, query });
    let artifact = match compiled {
        Ok((a, _)) => a,
        Err(blossom_front::api::BlsError::Rejected(found)) => {
            for d in found.iter() {
                eprint!("{}", blossom_driver::render::render(d, &sources));
            }
            return Exit::UserError.into();
        }
        Err(e) => {
            eprintln!("blossom query: {e}");
            return Exit::UserError.into();
        }
    };
    let Some(rel) = artifact.rel_named(&internal) else {
        eprintln!("blossom query: the compiled program has no view `{name}`");
        return Exit::Internal.into();
    };
    let program = match artifact.program.query(rel) {
        Ok((p, _)) => p,
        Err(e) => {
            eprintln!("blossom query: {e}");
            return Exit::UserError.into();
        }
    };
    let req = QueryRequest {
        program: program.get().clone(),
        view: internal.clone(),
        as_of: args.as_of,
    };
    let result = match (&args.admin, &args.store) {
        (Some(addr), _) => ask(*addr, &req),
        (None, Some(dir)) => offline(dir, &artifact, req),
        (None, None) => Err("a query needs --admin ADDR or --store DIR".into()),
    };
    let answer = match result {
        Ok(a) => a,
        Err(e) => {
            eprintln!("blossom query: {e}");
            return Exit::UserError.into();
        }
    };
    println!("{}", answer.columns.join("\t"));
    for row in &answer.rows {
        println!("{}", row.join("\t"));
    }
    eprintln!(
        "{} row{} as of tick {}",
        answer.rows.len(),
        if answer.rows.len() == 1 { "" } else { "s" },
        answer.tick
    );
    ExitCode::SUCCESS
}

/// Answers the query from the stopped node's store `dir`.
fn offline(
    dir: &std::path::Path,
    artifact: &blossom_artifact::bls::BlsArtifact,
    req: QueryRequest,
) -> Result<Answer, String> {
    let names: std::sync::Arc<[std::sync::Arc<str>]> = artifact
        .nodes
        .iter()
        .map(|n| std::sync::Arc::from(n.as_str()))
        .collect();
    let (db, _lock) =
        blossom_runtime::db::Database::open_offline(dir, std::sync::Arc::new(artifact.clone()), names.clone())
            .map_err(|e| e.to_string())?;
    let externs = crate::common::std_externs().map_err(|e| e.to_string())?;
    let now = blossom_runtime::clock::wall_now()?;
    blossom_runtime::query::answer(req, &db, &artifact.program, &names, externs, now).map_err(|e| e.to_string())
}

/// Sends the query to the admin listener at `addr`: its answer, or why not.
fn ask(addr: SocketAddr, req: &QueryRequest) -> Result<Answer, String> {
    let body = req.encode().map_err(|e| e.to_string())?;
    let mut s = TcpStream::connect(addr).map_err(|e| format!("cannot reach the admin listener at {addr}: {e}"))?;
    write!(
        s,
        "POST /query HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .and_then(|()| s.write_all(&body))
    .map_err(|e| format!("sending the query: {e}"))?;
    let mut all = Vec::new();
    s.read_to_end(&mut all)
        .map_err(|e| format!("reading the answer: {e}"))?;
    let at = all
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("a malformed answer (no header end)")?;
    let (head, body) = all.split_at(at);
    let head = String::from_utf8_lossy(head);
    let body = body.get(4..).unwrap_or_default();
    if !head.starts_with("HTTP/1.1 200") {
        return Err(String::from_utf8_lossy(body).into_owned());
    }
    serde_json::from_slice(body).map_err(|e| format!("a malformed answer: {e}"))
}
