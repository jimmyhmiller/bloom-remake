//! A Blossom deployment as a Vercel function (docs/design/DEPLOY.md §3). Vercel's Rust runtime hands the function
//! each request under `/blossom/`; the function answers it from the state store through the stateless engine
//! ([`Front`] over [`Objects`], the same as `blossom serve`), and nothing runs between requests. The page's files are
//! Vercel's static files.
//!
//! Its configuration is the environment:
//!
//! - `BLOSSOM_DEPLOY`: the deployment spec, a path in the function's bundle (its program and stylesheet beside it);
//! - `BLOSSOM_STORE`: the state store, `postgres://…` or `s3://…` (a function's disk is gone after its request);
//! - `BLOSSOM_SEED`: the deployment's seed, which pages' tokens are signed with;
//! - `BLOSSOM_TOKENS_PER_MINUTE`: each client's page tokens a minute (default 60).
//!
//! What a function cannot do is refused when it starts, by name: a program whose timers fire as time passes (nothing
//! runs between requests to fire them), and pages linked over WebSockets (a function holds no socket).

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use blossom_artifact::bls::BlsArtifact;
use blossom_front::api::{NodeSpec, ParamBinding};
use blossom_runtime::deploy::{DeploymentSpec, ParamValue};
use blossom_runtime::stateless::front::{Answer, ClientAddr, Front};
use blossom_runtime::stateless::{Deployment, Objects};
use blossom_runtime::web::{Request, Transport};
use blossom_statestore::StateStore;
use blossom_value::Seed;

/// The most bytes a request's body carries (Vercel refuses more than about 4.5 MB itself).
pub const MAX_BODY: usize = 4 * 1024 * 1024;
/// How often an instance does the store's housekeeping, after a request it answered.
const HOUSEKEEPING: Duration = Duration::from_secs(60);

/// A deployment loaded for the function: its requests' front, and when its housekeeping ran last.
pub struct App {
    pub front: Front,
    housekept: Mutex<Option<std::time::Instant>>,
}

/// The state store `url` names: Postgres or S3.
fn open_store(url: &str) -> Result<Arc<dyn StateStore>, String> {
    if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        Ok(Arc::new(
            blossom_statestore_postgres::PostgresStore::from_url(url).map_err(|e| e.to_string())?,
        ))
    } else if url.starts_with("s3://") {
        Ok(Arc::new(
            blossom_statestore_s3::S3Store::from_url(url).map_err(|e| e.to_string())?,
        ))
    } else {
        Err(
            "BLOSSOM_STORE is postgres://… or s3://…: a function's disk does not outlive its request, so sqlite: and \
             memory: cannot hold a deployment's state here"
                .into(),
        )
    }
}

/// A deployment's parameter value for the compiler.
fn param_binding(v: &ParamValue) -> ParamBinding {
    match v {
        ParamValue::Int(n) => ParamBinding::Int(*n),
        ParamValue::Bool(b) => ParamBinding::Bool(*b),
        ParamValue::Text(t) => ParamBinding::Text(t.clone()),
    }
}

/// Compiles the deployment's program for its nodes, as `blossom serve` does: the program is refused for an error,
/// or for a class claim its lattice's methods break (BLS0704).
fn compile(spec: &DeploymentSpec, externs: Arc<blossom_value::ExternRegistry>) -> Result<Arc<BlsArtifact>, String> {
    let nodes: Vec<NodeSpec> = spec
        .nodes
        .iter()
        .map(|n| NodeSpec {
            name: n.name.clone(),
            role: n.role.clone(),
        })
        .collect();
    let source = spec
        .source
        .to_str()
        .ok_or_else(|| format!("the program path {} is not UTF-8", spec.source.display()))?;
    let params = spec.params.iter().map(|(k, v)| (k.clone(), param_binding(v))).collect();
    let (result, sources) = blossom_driver::bls::compile_deployed(source, &nodes, &params);
    let render = |ds: &blossom_base::Diagnostics| {
        ds.iter()
            .map(|d| blossom_driver::render::render(d, &sources))
            .collect::<String>()
    };
    let artifact = match result {
        Ok((a, _warnings)) => a,
        Err(blossom_front::api::BlsError::Rejected(diags)) => return Err(render(&diags)),
        Err(blossom_front::api::BlsError::Internal(e)) => return Err(e.to_string()),
    };
    let report = blossom_verify::laws::check(&artifact, externs).map_err(|e| format!("the law harness: {e}"))?;
    let refuted = blossom_verify::laws::diagnostics(&report, &artifact);
    if !refuted.is_empty() {
        return Err(render(&refuted));
    }
    Ok(Arc::new(artifact))
}

fn var(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set (docs/design/DEPLOY.md §3)"))
}

/// What the function is configured with: the environment's variables ([`Config::from_env`]).
pub struct Config {
    /// The deployment spec (`BLOSSOM_DEPLOY`).
    pub deploy: PathBuf,
    /// The state store's URL (`BLOSSOM_STORE`).
    pub store: String,
    /// The deployment's seed (`BLOSSOM_SEED`).
    pub seed: Seed,
    /// Each client's page tokens a minute (`BLOSSOM_TOKENS_PER_MINUTE`, default 60).
    pub tokens_per_minute: u32,
}

impl Config {
    pub fn from_env() -> Result<Config, String> {
        Ok(Config {
            deploy: PathBuf::from(var("BLOSSOM_DEPLOY")?),
            store: var("BLOSSOM_STORE")?,
            seed: blossom_runtime::deploy::parse_seed(&var("BLOSSOM_SEED")?, "BLOSSOM_SEED")
                .map_err(|e| e.to_string())?,
            tokens_per_minute: match std::env::var("BLOSSOM_TOKENS_PER_MINUTE") {
                Ok(n) => n
                    .parse()
                    .map_err(|_| format!("BLOSSOM_TOKENS_PER_MINUTE is a number, not `{n}`"))?,
                Err(_) => 60,
            },
        })
    }
}

impl App {
    /// The deployment the environment names, loaded.
    pub fn from_env() -> Result<App, String> {
        App::load(&Config::from_env()?)
    }

    /// The deployment `cfg` names, loaded: refused, with the reason, when it cannot run as a function.
    pub fn load(cfg: &Config) -> Result<App, String> {
        let spec = DeploymentSpec::load(&cfg.deploy).map_err(|e| e.to_string())?;
        if spec.web_link == Transport::WebSocket {
            return Err(
                "the deployment links its pages over WebSockets ([web] link = \"websocket\"), and a function holds \
                 no socket: link them over plain requests ([web] link = \"http\")"
                    .into(),
            );
        }
        let seed = cfg.seed;
        let externs = Arc::new(blossom_std_host::registry().map_err(|e| format!("the standard host functions: {e}"))?);
        let artifact = compile(&spec, externs.clone())?;
        let node = blossom_runtime::stateless::deployment::page_node(&spec, &artifact)
            .ok_or_else(|| "the deployment has no nodes".to_string())?;
        let style = spec.web_style.clone();
        let deploy =
            Deployment::new(spec, artifact, seed, externs, &node, Transport::Http).map_err(|e| e.to_string())?;
        let clocked = deploy.clocked_timers();
        if !clocked.is_empty() {
            return Err(format!(
                "the program's timers {} fire as time passes, and a function runs only while it answers a request: \
                 nothing would fire them. Host it with `blossom serve`, whose sweeper does",
                clocked.join(", ")
            ));
        }
        let store = open_store(&cfg.store)?;
        let objects = Arc::new(Objects::new(Arc::new(deploy), store));
        Ok(App {
            // Vercel's proxy sets x-real-ip to the client's address, whatever the client sent.
            front: Front::new(
                objects,
                style,
                ClientAddr::Header("x-real-ip".into()),
                cfg.tokens_per_minute,
            ),
            housekept: Mutex::new(None),
        })
    }

    /// The answer to a request: a page's, or a 404 for anything else under `/blossom/`.
    pub fn answer(&self, req: &Request, body: &[u8]) -> Answer {
        // The client's address comes from x-real-ip; a function has no peer of its own to fall back on.
        let peer = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        self.front
            .answer(req, body, peer)
            .unwrap_or_else(|| Answer::new(404, "Not Found", "text/plain", "not a page's request"))
    }

    /// Whether this instance's housekeeping is due (at most every minute), marking it done when it is.
    pub fn housekeeping_due(&self) -> bool {
        let now = blossom_runtime::clock::host_monotonic();
        let Ok(mut last) = self.housekept.lock() else {
            return false;
        };
        if last.is_some_and(|t| now.saturating_duration_since(t) < HOUSEKEEPING) {
            return false;
        }
        *last = Some(now);
        true
    }

    /// The store's housekeeping (StateStore::maintain): old history and unreferenced blobs go.
    pub fn housekeep(&self) -> Result<(), String> {
        self.front.objects().maintain().map_err(|e| e.to_string())
    }
}
