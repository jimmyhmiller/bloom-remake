//! `blossom serve`: a deployment on a stateless host (docs/design/STATELESS.md §11). The deployment's nodes and keyed
//! members live in a state store; this process keeps nothing between requests, so any number of copies may serve one
//! deployment behind any load balancer.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use blossom_runtime::stateless::{Deployment, Objects, ServeConfig};
use blossom_statestore::StateStore;

use crate::cmd::run::{exit_of, load};
use crate::common::Context;
use crate::exit::Exit;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// The deployment spec (`deploy.toml`).
    #[arg(long = "deploy", value_name = "FILE")]
    pub deploy: PathBuf,
    /// The state store: `sqlite:PATH`, `postgres://…`, `s3://BUCKET/PREFIX?endpoint=…`, or `memory:` (this
    /// process only).
    #[arg(long, value_name = "URL")]
    pub store: String,
    /// Serve the page and its links on this address.
    #[arg(long, value_name = "ADDR")]
    pub web: std::net::SocketAddr,
    /// The page's files: the built browser host (`index.html`, `host.js`, `pkg/`).
    #[arg(long, value_name = "DIR", default_value = "web")]
    pub web_root: PathBuf,
    /// The node whose pages this host serves (default: the deployment's first node of a keyed role, else its first).
    #[arg(long)]
    pub node: Option<String>,
    /// How often this host sweeps for objects whose time came, in milliseconds; `off` when a scheduler calls
    /// `POST /blossom/wake` instead (a platform that freezes idle instances).
    #[arg(long, default_value = "250", value_name = "MS|off")]
    pub sweep: String,
    /// Allow the plaintext development transport (`security.mode = "insecure-dev"`).
    #[arg(long)]
    pub insecure_dev: bool,
}

/// Opens the state store a URL names.
pub fn open_store(url: &str) -> Result<Arc<dyn StateStore>, blossom_statestore::StateError> {
    Ok(if url.starts_with("sqlite:") {
        Arc::new(blossom_statestore_sqlite::SqliteStore::from_url(url)?)
    } else if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        Arc::new(blossom_statestore_postgres::PostgresStore::from_url(url)?)
    } else if url.starts_with("s3://") {
        Arc::new(blossom_statestore_s3::S3Store::from_url(url)?)
    } else if url == "memory:" {
        Arc::new(blossom_statestore::MemStore::new())
    } else {
        return Err(blossom_statestore::StateError::Config(format!(
            "`{url}` names no state store (sqlite:PATH, postgres://…, s3://…, memory:)"
        )));
    })
}

pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = cx;
    let fail = |m: String, code: Exit| {
        eprintln!("blossom serve: {m}");
        ExitCode::from(code)
    };
    let (spec, artifact) = match load(&args.deploy) {
        Ok(x) => x,
        Err(code) => return code,
    };
    if spec.security == blossom_runtime::deploy::SecurityMode::InsecureDev && !args.insecure_dev {
        return fail(
            "the deployment uses the plaintext development transport; pass `--insecure-dev` to allow it".into(),
            Exit::Refused,
        );
    }
    if !args.web_root.join("index.html").is_file() {
        return fail(
            format!(
                "the page is served from {}, which has no index.html (build it with scripts/build-web.sh, or pass \
                 --web-root)",
                args.web_root.display()
            ),
            Exit::Refused,
        );
    }
    let sweep = match args.sweep.as_str() {
        "off" => None,
        ms => match ms.parse::<u64>() {
            Ok(ms) if ms > 0 => Some(Duration::from_millis(ms)),
            _ => return fail(format!("--sweep is milliseconds or `off`, not `{ms}`"), Exit::Usage),
        },
    };
    let seed = match spec.seed() {
        Ok(s) => s,
        Err(e) => return fail(e.to_string(), exit_of(&e)),
    };
    let externs = match crate::common::std_externs() {
        Ok(x) => x,
        Err(e) => return fail(e.to_string(), Exit::Internal),
    };
    // The pages are a keyed role's members' when the deployment has one; else they link to its first node.
    let node = match &args.node {
        Some(n) => n.clone(),
        None => {
            let p = artifact.program.get();
            let keyed = spec
                .nodes
                .iter()
                .zip(&artifact.roles)
                .find(|(_, r)| r.is_some_and(|r| p.is_keyed(r)))
                .map(|(n, _)| n.name.clone());
            match keyed.or_else(|| spec.nodes.first().map(|n| n.name.clone())) {
                Some(n) => n,
                None => return fail("the deployment has no nodes".into(), Exit::Refused),
            }
        }
    };
    let store = match open_store(&args.store) {
        Ok(s) => s,
        Err(e) => return fail(e.to_string(), Exit::Refused),
    };
    let style = spec.web_style.clone();
    let transport = spec.web_link;
    let deploy = match Deployment::new(spec, artifact, seed, externs, &node, transport) {
        Ok(d) => Arc::new(d),
        Err(e) => return fail(e.to_string(), exit_of(&e)),
    };
    let objects = Arc::new(Objects::new(deploy, store));
    let serving = match blossom_runtime::stateless::serve(
        objects,
        ServeConfig {
            web: args.web,
            web_root: Some(args.web_root.clone()),
            style,
            sweep,
            report: Arc::new(|m: &str| eprintln!("blossom serve: {m}")),
        },
    ) {
        Ok(s) => s,
        Err(e) => return fail(e.to_string(), exit_of(&e)),
    };
    println!(
        "blossom: serving the page on http://{}/ (stateless; store {})",
        serving.addr,
        redact(&args.store)
    );
    // The readiness line is how supervisors and tests know the host serves; a closed stdout is not fatal.
    let _ = std::io::Write::flush(&mut std::io::stdout());
    serving.wait();
    Exit::Ok.into()
}

/// A store URL without its password.
fn redact(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    match rest.split_once('@') {
        Some((creds, host)) => match creds.split_once(':') {
            Some((user, _)) => format!("{scheme}://{user}:***@{host}"),
            None => url.to_owned(),
        },
        None => url.to_owned(),
    }
}
