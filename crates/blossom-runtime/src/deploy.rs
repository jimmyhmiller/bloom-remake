//! The deployment spec (ARCHITECTURE §5.9, §12.4): `deploy.toml`, validated into [`DeploymentSpec`].
//!
//! ```toml
//! format = 1
//!
//! [deployment]
//! id      = "kvs-dev"
//! program = "kvs"                 # the program's name (`program kvs version 1;`)
//! version = 1
//! source  = "kvs.bls"             # the program root, relative to this file
//! secrets = "kvs-dev.secrets"     # `seed = "<32 hex digits>"`; mode 0600; BLOSSOM_SEED overrides
//!
//! [[node]]
//! name = "s1"; role = "Server"; addr = "127.0.0.1:7400"; client_addr = "127.0.0.1:7500"
//! principal = "spiffe://dev/kvs/Server/s1"
//! dial = { s2 = "127.0.0.1:17402" }  # optional: dial node s2 here instead of at its `addr` (a proxy, a NAT)
//!
//! [params]                        # deploy-time parameters (LANG-010): integers, bools, strings, durations
//! ELECTION_MIN = "150ms"
//!
//! [statics]                       # rows of `static` relations, by name; each row an array of column values
//! admins = [["spiffe://dev/kvs/client/admin"]]
//!
//! [security]
//! mode = "insecure-dev"           # plaintext TCP; needs `--insecure-dev`. "mtls" is DIST-060 (not in this build)
//!
//! [storage]
//! data_dir = "data"               # each node's store is <data_dir>/<node name>, relative to this file
//! checkpoint_wal_bytes = 67108864
//! ```
//!
//! The architecture's node entry has no `client_addr`: it has one client listener per node, and so does this, at
//! its own address. `source` and `[statics]` are additions: the program is compiled from source at startup, and
//! static relations take their deployment rows from the spec (LANGUAGE §7.5).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use blossom_base::RelId;
use blossom_base::TypeId;
use blossom_ir::core::{Program, RelClass};
use blossom_oracle::Row;
use blossom_value::time::NodeId;
use blossom_value::value::IntValue;
use blossom_value::{Seed, TypeDef, Value};

use crate::RuntimeError;

/// The deployment spec as written.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSpec {
    format: u32,
    deployment: RawDeployment,
    #[serde(rename = "node")]
    nodes: Vec<RawNode>,
    #[serde(default)]
    statics: BTreeMap<String, Vec<Vec<toml::Value>>>,
    #[serde(default)]
    params: BTreeMap<String, toml::Value>,
    security: RawSecurity,
    storage: RawStorage,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeployment {
    id: String,
    program: String,
    version: u32,
    source: PathBuf,
    secrets: Option<PathBuf>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNode {
    name: String,
    role: Option<String>,
    addr: SocketAddr,
    client_addr: Option<SocketAddr>,
    principal: String,
    #[serde(default)]
    dial: BTreeMap<String, SocketAddr>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSecurity {
    mode: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStorage {
    data_dir: PathBuf,
    checkpoint_wal_bytes: Option<u64>,
}

/// A deploy-time parameter's value as the spec writes it (LANG-010): the compiler checks it against the declared type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamValue {
    Int(i128),
    Bool(bool),
    /// A string, or a duration such as `"150ms"`.
    Text(String),
}

/// How connections are secured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecurityMode {
    /// Plaintext TCP; clients claim their principal in `HELLO`. Development only (`--insecure-dev`).
    InsecureDev,
}

/// One node of the deployment.
#[derive(Clone, Debug)]
pub struct NodeEntry {
    pub name: String,
    pub role: Option<String>,
    pub addr: SocketAddr,
    pub client_addr: Option<SocketAddr>,
    pub principal: String,
    /// Where this node dials other nodes when it is not their `addr` (a proxy, a NAT), by node name.
    pub dial: BTreeMap<String, SocketAddr>,
}

/// A validated deployment spec.
#[derive(Clone, Debug)]
pub struct DeploymentSpec {
    pub id: String,
    pub program: String,
    pub version: u32,
    /// The program root, resolved against the spec's directory.
    pub source: PathBuf,
    pub secrets: Option<PathBuf>,
    /// The nodes, sorted by name (the canonical directory order: node `i` is `NodeId(i)`).
    pub nodes: Vec<NodeEntry>,
    pub statics: BTreeMap<String, Vec<Vec<toml::Value>>>,
    /// Values of the program's deploy-time parameters.
    pub params: BTreeMap<String, ParamValue>,
    pub security: SecurityMode,
    pub data_dir: PathBuf,
    pub checkpoint_wal_bytes: u64,
}

fn invalid(key: &str, what: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Config(format!("deployment spec: `{key}`: {what}"))
}

impl DeploymentSpec {
    /// Reads and validates `path`.
    pub fn load(path: &Path) -> Result<DeploymentSpec, RuntimeError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| RuntimeError::Config(format!("cannot read {}: {e}", path.display())))?;
        let absolute = std::path::absolute(path)
            .map_err(|e| RuntimeError::Config(format!("cannot resolve {}: {e}", path.display())))?;
        let base = absolute.parent().unwrap_or(Path::new("/"));
        DeploymentSpec::parse(&text, base)
    }

    /// Parses and validates a spec whose relative paths are relative to `base`.
    pub fn parse(text: &str, base: &Path) -> Result<DeploymentSpec, RuntimeError> {
        let raw: RawSpec = toml::from_str(text).map_err(|e| RuntimeError::Config(format!("deployment spec: {e}")))?;
        if raw.format != 1 {
            return Err(invalid(
                "format",
                format!("{} is not a format this build reads (1)", raw.format),
            ));
        }
        let security = match raw.security.mode.as_str() {
            "insecure-dev" => SecurityMode::InsecureDev,
            "mtls" => {
                return Err(blossom_base::unimplemented_error!(
                    "DIST-060",
                    "mTLS transport (security.mode = \"mtls\")"
                )
                .into());
            }
            other => return Err(invalid("security.mode", format!("unknown mode {other:?}"))),
        };
        if raw.nodes.is_empty() {
            return Err(invalid("node", "the deployment has no nodes"));
        }
        let mut nodes: Vec<NodeEntry> = raw
            .nodes
            .into_iter()
            .map(|n| NodeEntry {
                name: n.name,
                role: n.role,
                addr: n.addr,
                client_addr: n.client_addr,
                principal: n.principal,
                dial: n.dial,
            })
            .collect();
        nodes.sort_by(|a, b| a.name.cmp(&b.name));
        for n in &nodes {
            for target in n.dial.keys() {
                if !nodes.iter().any(|m| m.name == *target) {
                    return Err(invalid(&format!("node.dial.{target}"), "no such node"));
                }
            }
        }
        for w in nodes.windows(2) {
            if let [a, b] = w
                && a.name == b.name
            {
                return Err(invalid("node.name", format!("`{}` is declared twice", a.name)));
            }
        }
        let resolve = |p: PathBuf| if p.is_absolute() { p } else { base.join(p) };
        let mut params = BTreeMap::new();
        for (name, v) in raw.params {
            let value = match v {
                toml::Value::Integer(n) => ParamValue::Int(i128::from(n)),
                toml::Value::Boolean(b) => ParamValue::Bool(b),
                toml::Value::String(s) => ParamValue::Text(s),
                other => return Err(invalid(&format!("params.{name}"), format!("{other} is not an integer, bool or string"))),
            };
            params.insert(name, value);
        }
        Ok(DeploymentSpec {
            id: raw.deployment.id,
            program: raw.deployment.program,
            version: raw.deployment.version,
            source: resolve(raw.deployment.source),
            secrets: raw.deployment.secrets.map(resolve),
            nodes,
            statics: raw.statics,
            params,
            security,
            data_dir: resolve(raw.storage.data_dir),
            checkpoint_wal_bytes: raw.storage.checkpoint_wal_bytes.unwrap_or(256 * 1024 * 1024),
        })
    }

    /// The node named `name`, with its id.
    pub fn node(&self, name: &str) -> Result<(NodeId, &NodeEntry), RuntimeError> {
        let i = self
            .nodes
            .iter()
            .position(|n| n.name == name)
            .ok_or_else(|| RuntimeError::Config(format!("the deployment has no node `{name}`")))?;
        let id = u32::try_from(i).map_err(|_| RuntimeError::Config("too many nodes".into()))?;
        let entry = self
            .nodes
            .get(i)
            .ok_or_else(|| blossom_base::internal_error!("node index {i} out of range"))?;
        Ok((NodeId(id), entry))
    }

    /// The node names by id.
    pub fn names(&self) -> Arc<[Arc<str>]> {
        self.nodes.iter().map(|n| Arc::from(n.name.as_str())).collect()
    }

    /// The deployment's 128-bit id.
    pub fn deployment_id(&self) -> [u8; 16] {
        digest(&[b"blossom deployment", self.id.as_bytes()])
    }

    /// The digest of the directory: every node's name, role and principal, in id order (ARCHITECTURE §5.9).
    pub fn directory_digest(&self) -> [u8; 16] {
        let mut parts: Vec<&[u8]> = vec![b"blossom directory"];
        for n in &self.nodes {
            parts.push(n.name.as_bytes());
            parts.push(n.role.as_deref().unwrap_or("").as_bytes());
            parts.push(n.principal.as_bytes());
        }
        digest(&parts)
    }

    /// The deployment seed (DIST-033): `BLOSSOM_SEED`, else the secrets file's `seed`. A secret: never logged.
    pub fn seed(&self) -> Result<Seed, RuntimeError> {
        if let Ok(hex) = std::env::var("BLOSSOM_SEED") {
            return parse_seed(&hex, "BLOSSOM_SEED");
        }
        let Some(path) = &self.secrets else {
            return Err(RuntimeError::Config(
                "no deployment seed: set BLOSSOM_SEED or `deployment.secrets` (a file with `seed = \"<32 hex digits>\"`)"
                    .into(),
            ));
        };
        check_private(path)?;
        let text = std::fs::read_to_string(path)
            .map_err(|e| RuntimeError::Config(format!("cannot read the secrets file {}: {e}", path.display())))?;
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Secrets {
            seed: String,
        }
        let s: Secrets = toml::from_str(&text)
            .map_err(|e| RuntimeError::Config(format!("the secrets file {}: {e}", path.display())))?;
        parse_seed(&s.seed, "the secrets file's `seed`")
    }

    /// The deployment's rows of the program's static relations, as tick events.
    pub fn static_rows(&self, program: &Program, names: &[Arc<str>]) -> Result<Vec<(RelId, Row)>, RuntimeError> {
        let mut out = Vec::new();
        for (name, rows) in &self.statics {
            let (rel, decl) = program
                .rels
                .iter_enumerated()
                .find(|(_, r)| r.name.to_string() == *name)
                .ok_or_else(|| invalid(&format!("statics.{name}"), "the program has no such relation"))?;
            if decl.class != RelClass::Static {
                return Err(invalid(&format!("statics.{name}"), "not a `static` relation"));
            }
            for (i, row) in rows.iter().enumerate() {
                let key = format!("statics.{name}[{i}]");
                if row.len() != decl.schema.cols.len() {
                    return Err(invalid(
                        &key,
                        format!("{} values for {} columns", row.len(), decl.schema.cols.len()),
                    ));
                }
                let mut values = Vec::with_capacity(row.len());
                for (col, v) in decl.schema.cols.iter().zip(row) {
                    values.push(value_of(program, col.ty, v, names).map_err(|e| invalid(&key, e))?);
                }
                out.push((rel, Row::from(values)));
            }
        }
        Ok(out)
    }
}

fn digest(parts: &[&[u8]]) -> [u8; 16] {
    let mut h = blake3::Hasher::new();
    for p in parts {
        h.update(&(p.len() as u64).to_le_bytes());
        h.update(p);
    }
    let hash = h.finalize();
    let mut out = [0u8; 16];
    for (o, b) in out.iter_mut().zip(hash.as_bytes()) {
        *o = *b;
    }
    out
}

fn parse_seed(hex: &str, what: &str) -> Result<Seed, RuntimeError> {
    let bad = || RuntimeError::Config(format!("{what} must be 32 hex digits"));
    let digits: Vec<u8> = hex
        .trim()
        .chars()
        .map(|c| c.to_digit(16).and_then(|d| u8::try_from(d).ok()))
        .collect::<Option<Vec<u8>>>()
        .ok_or_else(bad)?;
    if digits.len() != 32 {
        return Err(bad());
    }
    let mut out = [0u8; 16];
    for (o, pair) in out.iter_mut().zip(digits.chunks(2)) {
        if let [hi, lo] = pair {
            *o = (hi << 4) | lo;
        }
    }
    Ok(Seed(out))
}

/// Refuses a secrets file that is group- or world-readable (ARCHITECTURE §5.8).
#[cfg(unix)]
fn check_private(path: &Path) -> Result<(), RuntimeError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path)
        .map_err(|e| RuntimeError::Config(format!("cannot read the secrets file {}: {e}", path.display())))?;
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(RuntimeError::Config(format!(
            "the secrets file {} is readable by group or others; chmod 600 it",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_private(_path: &Path) -> Result<(), RuntimeError> {
    Err(blossom_base::unimplemented_error!("DIST-060", "secrets file permission checks on this platform").into())
}

/// A TOML value as a value of type `ty`.
fn value_of(program: &Program, ty: TypeId, v: &toml::Value, names: &[Arc<str>]) -> Result<Value, String> {
    let def = program.types.def(ty).map_err(|e| e.to_string())?;
    Ok(match (def, v) {
        (TypeDef::Bool, toml::Value::Boolean(b)) => Value::Bool(*b),
        (TypeDef::Int(t), toml::Value::Integer(n)) => {
            Value::Int(IntValue::from_i128(*t, i128::from(*n)).ok_or_else(|| format!("{n} is out of range for {t:?}"))?)
        }
        (TypeDef::Str, toml::Value::String(s)) => Value::Str(s.as_str().into()),
        (TypeDef::Principal, toml::Value::String(s)) => Value::Principal(s.as_str().into()),
        (TypeDef::Bytes, toml::Value::String(s)) => Value::Bytes(s.as_bytes().into()),
        (TypeDef::Node(_), toml::Value::String(s)) => {
            let i = names
                .iter()
                .position(|n| **n == **s)
                .ok_or_else(|| format!("no node named `{s}`"))?;
            Value::Node(NodeId(u32::try_from(i).map_err(|_| "too many nodes".to_string())?))
        }
        (def, v) => {
            return Err(format!(
                "cannot read {v} as a {def:?} (this build reads bool, integers, strings, principals, bytes and nodes)"
            ));
        }
    })
}
