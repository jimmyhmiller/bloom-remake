//! `blossom serve`: a deployment on a stateless host (docs/design/STATELESS.md §11). The deployment's nodes and keyed
//! members live in a state store; this process keeps nothing between requests, so any number of copies may serve one
//! deployment behind any load balancer, or on a function platform (docs/design/DEPLOY.md).
//!
//! The deployment's `security.mode` is not this command's: it secures `blossom run`'s connections between nodes,
//! and here nodes reach each other only through the store. Pages prove who they are with tokens signed by the
//! deployment's seed (STATELESS.md §7).

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use blossom_runtime::stateless::serve::ClientAddr;
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
    /// process only). A URL with a password is better given in the environment than on the command line.
    #[arg(long, value_name = "URL", env = "BLOSSOM_STORE", hide_env_values = true)]
    pub store: String,
    /// Serve the page and its links on this address (default: every interface, on the port `PORT` names, as
    /// function and container platforms set it).
    #[arg(long, value_name = "ADDR")]
    pub web: Option<std::net::SocketAddr>,
    /// The page's files: the built browser host (`index.html`, `host.js`, `pkg/`).
    #[arg(long, value_name = "DIR", default_value = "web")]
    pub web_root: PathBuf,
    /// The node whose pages this host serves (default: the deployment's first node of a keyed role, else its first).
    #[arg(long)]
    pub node: Option<String>,
    /// How often this host sweeps for objects whose time came, in milliseconds; `off` when a scheduler calls
    /// `/blossom/wake` instead (a platform that freezes idle instances).
    #[arg(long, default_value = "250", value_name = "MS|off")]
    pub sweep: String,
    /// The environment variable holding the secret a caller of `/blossom/wake` shows (`Authorization: Bearer
    /// SECRET`, at least 16 characters); `CRON_SECRET` on Vercel, whose cron jobs send it. When it is not set, the
    /// route refuses every caller.
    #[arg(long, default_value = "BLOSSOM_WAKE_SECRET", value_name = "NAME")]
    pub wake_secret_env: String,
    /// The most connections this host serves at once; another is answered 503.
    #[arg(long, default_value_t = 1024, value_name = "N")]
    pub max_connections: usize,
    /// How many page tokens one client is given a minute: every token takes one of a finite pool of page
    /// identities.
    #[arg(long, default_value_t = 60, value_name = "N")]
    pub tokens_per_minute: u32,
    /// Tell a request's client by the first address in this header (`x-real-ip`, `x-forwarded-for`) rather than
    /// by its connection. Only behind a proxy that overwrites the header: a client can write any value itself.
    #[arg(long, value_name = "HEADER")]
    pub client_ip_header: Option<String>,
    /// On SIGTERM or SIGINT, how long to let requests in flight finish before exiting, in seconds.
    #[arg(long, default_value_t = 25, value_name = "S")]
    pub drain: u64,
    /// How many objects (rooms, nodes) this host keeps in memory between requests; more are loaded again when
    /// asked for.
    #[arg(long, default_value_t = blossom_runtime::stateless::objects::DEFAULT_CACHE)]
    pub cache: usize,
}

/// The shortest wake secret taken: a short one is guessed.
const MIN_WAKE_SECRET: usize = 16;

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
    let web = match args.web {
        Some(a) => a,
        None => match std::env::var("PORT").ok().map(|p| p.parse::<u16>()) {
            Some(Ok(port)) => std::net::SocketAddr::from(([0, 0, 0, 0], port)),
            Some(Err(_)) => return fail("PORT is not a port number".into(), Exit::Usage),
            None => {
                return fail(
                    "no address to serve on: pass --web ADDR, or set PORT".into(),
                    Exit::Usage,
                );
            }
        },
    };
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
    let wake_secret = match std::env::var(&args.wake_secret_env) {
        Ok(s) if s.chars().count() < MIN_WAKE_SECRET => {
            return fail(
                format!(
                    "{} is shorter than {MIN_WAKE_SECRET} characters: a wake secret that short is guessed",
                    args.wake_secret_env
                ),
                Exit::Refused,
            );
        }
        Ok(s) => Some(s),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            return fail(format!("{} is not UTF-8", args.wake_secret_env), Exit::Refused);
        }
    };
    if sweep.is_none() && wake_secret.is_none() {
        return fail(
            format!(
                "with --sweep off only /blossom/wake runs the objects' timers, and it needs a secret: set {}",
                args.wake_secret_env
            ),
            Exit::Refused,
        );
    }
    if args.max_connections == 0 {
        return fail("--max-connections is at least 1".into(), Exit::Usage);
    }
    let seed = match spec.seed() {
        Ok(s) => s,
        Err(e) => return fail(e.to_string(), exit_of(&e)),
    };
    let externs = match crate::common::std_externs() {
        Ok(x) => x,
        Err(e) => return fail(e.to_string(), Exit::Internal),
    };
    let node = match args
        .node
        .clone()
        .or_else(|| blossom_runtime::stateless::deployment::page_node(&spec, &artifact))
    {
        Some(n) => n,
        None => return fail("the deployment has no nodes".into(), Exit::Refused),
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
    let objects = Arc::new(Objects::new(deploy, store).with_cache(args.cache));
    let serving = match blossom_runtime::stateless::serve(
        objects,
        ServeConfig {
            web,
            web_root: Some(args.web_root.clone()),
            style,
            sweep,
            report: Arc::new(|m: &str| eprintln!("blossom serve: {m}")),
            wake_secret,
            max_connections: args.max_connections,
            client: match &args.client_ip_header {
                Some(h) => ClientAddr::Header(h.to_ascii_lowercase()),
                None => ClientAddr::Peer,
            },
            tokens_per_minute: args.tokens_per_minute,
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
    let mut signals =
        match signal_hook::iterator::Signals::new([signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT]) {
            Ok(s) => s,
            Err(e) => return fail(format!("cannot listen for SIGTERM: {e}"), Exit::Internal),
        };
    // A platform scaling in sends SIGTERM: stop taking connections and let those in flight finish.
    if let Some(sig) = signals.forever().next() {
        eprintln!(
            "blossom serve: signal {sig}: draining {} connection(s), for at most {} s",
            serving.connections(),
            args.drain
        );
        if !serving.drain(Duration::from_secs(args.drain)) {
            eprintln!("blossom serve: connections still open after {} s; exiting", args.drain);
        }
    }
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
