//! What every object of a deployment shares on a stateless host: the program, compiled once per instance; the seed;
//! the page's `app.json` and client parts; and which node or member each object runs.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_value::time::MemberRef;
use blossom_value::{ExternRegistry, Seed};

use crate::RuntimeError;
use crate::deploy::DeploymentSpec;
use crate::object::{ObjectConfig, ObjectNode, Random, Target};
use crate::web::Transport;

/// The object that mints pages' tokens (docs/design/STATELESS.md §7).
pub const REGISTRY: &str = "registry";

/// Where an object's node starts from: its store's filesystem, the time, where it hibernated, and the tree its
/// database keeps its rows in (`None`: the LSM in its store).
pub(crate) struct Start {
    pub fs: Arc<dyn blossom_store::Vfs>,
    pub now: blossom_value::time::Instant,
    pub hibernation: Option<blossom_node::Hibernation>,
    pub tree: Option<Box<dyn blossom_store::tree::KeyTree>>,
}

/// What an object runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Runs {
    /// The deployment's node `node`, or, with a member, that keyed member for its role's host `node`.
    Node { node: String, member: Option<MemberRef> },
    /// The registry: no node, only its counter.
    Registry,
}

/// A deployment, as a stateless host serves it.
pub struct Deployment {
    pub spec: DeploymentSpec,
    pub artifact: Arc<BlsArtifact>,
    pub seed: Seed,
    pub externs: Arc<ExternRegistry>,
    /// The node whose pages the host serves.
    pub node: String,
    /// The keyed role the pages link to, when that node hosts one.
    pub keyed: Option<String>,
    app: String,
    clients: BTreeMap<String, Vec<u8>>,
}

impl Deployment {
    /// The deployment `spec`, its program compiled for its nodes, pages served for node `node`, their links over
    /// `transport` by default.
    pub fn new(
        spec: DeploymentSpec,
        artifact: Arc<BlsArtifact>,
        seed: Seed,
        externs: Arc<ExternRegistry>,
        node: &str,
        transport: Transport,
    ) -> Result<Deployment, RuntimeError> {
        let names = spec.names();
        if names.len() != artifact.nodes.len() || names.iter().zip(&artifact.nodes).any(|(a, b)| **a != *b.as_str()) {
            return Err(RuntimeError::Config(
                "the program was compiled for other nodes than the deployment's".into(),
            ));
        }
        let (me, _) = spec.node(node)?;
        let p = artifact.program.get();
        let keyed = artifact
            .roles
            .get(me.0 as usize)
            .copied()
            .flatten()
            .filter(|r| p.is_keyed(*r))
            .and_then(|r| p.roles.get(r))
            .map(|r| r.name.to_string());
        let clients = crate::members::project_clients(&artifact)?;
        let names: Vec<String> = clients.keys().cloned().collect();
        let app = crate::web::app_json(&spec, &names, node, transport).map_err(RuntimeError::Config)?;
        let mut desc: serde_json::Value =
            serde_json::from_str(&app).map_err(|e| RuntimeError::Config(e.to_string()))?;
        if let (Some(role), serde_json::Value::Object(fields)) = (&keyed, &mut desc) {
            // The pages link to members, each with a token the registry gave it first (docs/design/KEYED.md).
            fields.insert("keyed".into(), serde_json::Value::String(role.clone()));
            fields.insert("tokens".into(), serde_json::Value::String("/blossom/token".into()));
        }
        Ok(Deployment {
            app: serde_json::to_string(&desc).map_err(|e| RuntimeError::Config(e.to_string()))?,
            clients: clients
                .into_iter()
                .map(|(name, c)| (name, c.artifact.to_vec()))
                .collect(),
            keyed,
            spec,
            artifact,
            seed,
            externs,
            node: node.to_owned(),
        })
    }

    pub fn app_json(&self) -> &str {
        &self.app
    }

    pub fn client_part(&self, role: &str) -> Option<&[u8]> {
        self.clients.get(role).map(Vec::as_slice)
    }

    /// The object a page's link goes to: the member its URL names (`?member=KEY`) when the pages link to a keyed
    /// role's members, else the node that serves them.
    pub fn page_object(&self, member: Option<&str>) -> Result<String, String> {
        match (&self.keyed, member) {
            (Some(role), Some(key)) if !key.is_empty() => Ok(format!("member/{role}/{key}")),
            (Some(role), _) => Err(format!("a link names the member of {role} it goes to (?member=KEY)")),
            (None, _) => Ok(format!("node/{}", self.node)),
        }
    }

    /// The object a message to `t` goes to.
    pub fn object_of(t: &Target) -> String {
        match t {
            Target::Node(n) => format!("node/{n}"),
            Target::Member(m) => format!("member/{}/{}", m.role_name, m.key),
        }
    }

    /// What the object named `object` runs.
    pub fn runs(&self, object: &str) -> Result<Runs, RuntimeError> {
        if object == REGISTRY {
            return Ok(Runs::Registry);
        }
        if let Some(node) = object.strip_prefix("node/") {
            self.spec.node(node)?;
            return Ok(Runs::Node {
                node: node.to_owned(),
                member: None,
            });
        }
        if let Some((role, key)) = object.strip_prefix("member/").and_then(|r| r.split_once('/')) {
            let p = self.artifact.program.get();
            let id = p
                .keyed_role_named(role)
                .ok_or_else(|| RuntimeError::Config(format!("`{role}` is not a keyed role of the program")))?;
            let host = self
                .spec
                .nodes
                .iter()
                .zip(&self.artifact.roles)
                .find(|(_, r)| **r == Some(id))
                .map(|(n, _)| n.name.clone())
                .ok_or_else(|| {
                    RuntimeError::Config(format!(
                        "the deployment has no node of the keyed role `{role}` to run its members"
                    ))
                })?;
            return Ok(Runs::Node {
                node: host,
                member: Some(p.member(id, key)),
            });
        }
        Err(RuntimeError::Config(format!(
            "`{object}` names no object (node/NAME, member/ROLE/KEY or registry)"
        )))
    }

    /// Starts the node of `object` (the deployment's `node`, or its keyed `member` for that host) from `start`: its
    /// store (recovered, or created on its first start), or resumed from where it hibernated; its start's nonce and
    /// its tokens' secrets come from the OS.
    pub(crate) fn open_node(
        &self,
        object: &str,
        node: String,
        member: Option<MemberRef>,
        start: Start,
    ) -> Result<ObjectNode, RuntimeError> {
        let Start {
            fs,
            now,
            hibernation,
            tree,
        } = start;
        let mut nonce = [0u8; 8];
        crate::members::urandom(&mut nonce)?;
        let random: Random = Box::new(crate::members::urandom);
        ObjectNode::open(ObjectConfig {
            spec: self.spec.clone(),
            artifact: self.artifact.clone(),
            node,
            member,
            members: Arc::new(blossom_ir::members::Members::open()),
            fs,
            dir: Path::new("/").join(object),
            seed: self.seed,
            now,
            nonce: u64::from_le_bytes(nonce),
            random,
            externs: self.externs.clone(),
            hibernation,
            tree,
        })
    }
}
