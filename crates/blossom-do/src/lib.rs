//! A deployment's node as a Cloudflare Durable Object (docs/design/DURABLE-OBJECTS.md).
//!
//! [`Object`] is the object's node: [`blossom_runtime::object::ObjectNode`] over a [`JournalKv`], its program compiled
//! from sources in memory for the deployment. The Worker's object class (do/src/worker.js) makes one when the object
//! starts (and after every wake from hibernation), from the program's sources, the deployment and every key its
//! storage holds; it passes each WebSocket message to [`Object::frame`], each close to [`Object::closed`] and the
//! alarm to [`Object::wake`]. After each call it applies [`Object::take_writes`] to its storage and only then writes
//! [`Object::take_output`] to its sockets, all in one synchronous run of the event: the storage commits the writes
//! together, and the output gate holds the frames until they are durable (Invariant R).
//!
//! The byte encodings the Worker reads and writes, all integers little-endian:
//!
//! - storage entries (`entries`, [`Object::take_writes`]): per entry `op: u8` (0 a value, 1 a deletion), `key_len:
//!   u32`, the key (UTF-8), and for a value `len: u32` and its bytes;
//! - output ([`Object::take_output`]): per item `kind: u8`; 0 a frame (`conn: u64`, `len: u32` and its bytes), 1 a
//!   close (`conn: u64`), 2 a message to another object by RPC (the object's name and the sender's, each `len: u32`
//!   and UTF-8, then `len: u32` and the frame).
//!
//! A deployment's objects are named (docs/design/KEYED.md §4): `node/NAME` runs the deployment's node `NAME`,
//! `member/ROLE/KEY` the keyed member of `ROLE` named `KEY`, and `registry` mints its pages' tokens.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use blossom_artifact::bls::BlsArtifact;
use blossom_front::api::{BlsError, NodeSpec, ParamBinding};
use blossom_front::ded::LoadedFile;
use blossom_front::modules::Loader;
use blossom_runtime::RuntimeError;
use blossom_runtime::deploy::{DeploymentSpec, ParamValue};
use blossom_runtime::object::{ObjectConfig, ObjectNode, Output, Target};
use blossom_store::{JournalKv, KvFs, KvStore};
use blossom_value::Seed;
use blossom_value::time::Instant;

#[cfg(target_arch = "wasm32")]
mod wasm;

/// Sources in memory: `path` → text.
struct Files<'a>(&'a BTreeMap<String, String>);

impl Loader for Files<'_> {
    fn load(&mut self, _from: Option<&str>, path: &str) -> Result<LoadedFile, String> {
        let path = path.trim_start_matches("./");
        self.0
            .get(path)
            .map(|text| LoadedFile {
                key: Arc::from(path),
                text: text.clone(),
            })
            .ok_or_else(|| format!("no file `{path}`"))
    }
}

/// A deployment's program, compiled from sources in memory for its nodes.
fn compile(files: &BTreeMap<String, String>, deploy: &str) -> Result<(DeploymentSpec, BlsArtifact), String> {
    let spec = DeploymentSpec::parse(deploy, Path::new("/")).map_err(|e| e.to_string())?;
    let root = spec
        .source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("the deployment's source {} names no file", spec.source.display()))?
        .to_owned();
    let nodes: Vec<NodeSpec> = spec
        .nodes
        .iter()
        .map(|n| NodeSpec {
            name: n.name.clone(),
            role: n.role.clone(),
        })
        .collect();
    let params: BTreeMap<String, ParamBinding> = spec.params.iter().map(|(k, v)| (k.clone(), param(v))).collect();
    let (compiled, sources) = blossom_driver::bls::compile_with_loader(&root, &nodes, &params, &mut Files(files));
    let (artifact, _warnings) = compiled.map_err(|e| rendered(e, &sources))?;
    Ok((spec, artifact))
}

/// An object's name for a target of its messages.
fn object_name(t: &Target) -> String {
    match t {
        Target::Node(n) => format!("node/{n}"),
        Target::Member(m) => format!("member/{}/{}", m.role_name, m.key),
    }
}

/// What the Worker serves for the deployment itself, no object needed: `/blossom/app.json`, each client role's part
/// of the program, and the keyed role whose members the pages link to; and the registry's tokens.
pub struct Site {
    app: String,
    clients: BTreeMap<String, Vec<u8>>,
    keyed: Option<String>,
}

impl Site {
    /// The site of the deployment `deploy` (its pages served by node `node`).
    pub fn open(files: &BTreeMap<String, String>, deploy: &str, node: &str) -> Result<Site, String> {
        let (spec, artifact) = compile(files, deploy)?;
        let p = artifact.program.get();
        let (me, _) = spec.node(node).map_err(|e| e.to_string())?;
        let keyed = artifact
            .roles
            .get(me.0 as usize)
            .copied()
            .flatten()
            .filter(|r| p.is_keyed(*r))
            .and_then(|r| p.roles.get(r))
            .map(|r| r.name.to_string());
        let clients = blossom_runtime::members::project_clients(&artifact).map_err(|e| e.to_string())?;
        let names: Vec<String> = clients.keys().cloned().collect();
        let app = blossom_runtime::web::app_json(&spec, &names, node, blossom_runtime::web::Transport::WebSocket)?;
        let mut desc: serde_json::Value = serde_json::from_str(&app).map_err(|e| e.to_string())?;
        if let (Some(role), serde_json::Value::Object(fields)) = (&keyed, &mut desc) {
            // The pages link to members, each with a token the registry gave it first.
            fields.insert("keyed".into(), serde_json::Value::String(role.clone()));
            fields.insert("tokens".into(), serde_json::Value::String("/blossom/token".into()));
        }
        Ok(Site {
            app: serde_json::to_string(&desc).map_err(|e| e.to_string())?,
            clients: clients
                .into_iter()
                .map(|(name, c)| (name, c.artifact.to_vec()))
                .collect(),
            keyed,
        })
    }

    pub fn app_json(&self) -> &str {
        &self.app
    }

    pub fn client_part(&self, role: &str) -> Option<&[u8]> {
        self.clients.get(role).map(Vec::as_slice)
    }

    /// The keyed role whose members the pages link to, when the node serving them hosts one.
    pub fn keyed_role(&self) -> Option<&str> {
        self.keyed.as_deref()
    }
}

/// A page's token for client role `role`, serial `serial`, signed for the deployment seeded `seed` (the registry
/// object's mint).
pub fn mint_token(seed: [u8; 16], role: &str, serial: u32) -> Vec<u8> {
    blossom_runtime::members::signed_token(Seed(seed), role, serial)
}

/// A Durable Object's node.
pub struct Object {
    node: ObjectNode,
    kv: Arc<JournalKv>,
    /// Secret random bytes the Worker gives with each call (`crypto.getRandomValues`), drawn for members' tokens.
    entropy: Arc<Mutex<Vec<u8>>>,
}

fn param(v: &ParamValue) -> ParamBinding {
    match v {
        ParamValue::Int(n) => ParamBinding::Int(*n),
        ParamValue::Bool(b) => ParamBinding::Bool(*b),
        ParamValue::Text(t) => ParamBinding::Text(t.clone()),
    }
}

fn rendered(e: BlsError, sources: &blossom_base::SourceDb) -> String {
    match e {
        BlsError::Rejected(diags) => diags
            .iter()
            .map(|d| blossom_driver::render::render(d, sources))
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// Milliseconds since the epoch (JavaScript's `Date.now()`) as an instant.
pub fn instant_of_ms(ms: f64) -> Instant {
    Instant((ms as i64).saturating_mul(1_000_000))
}

impl Object {
    /// Starts the object named `name` (`node/NAME` or `member/ROLE/KEY`) of the deployment `deploy` (a deployment
    /// spec's text; its `source` names the root among `files`), its store the one `entries` hold (none on the
    /// object's first start). A member's object runs it for the deployment's node of its role (its host).
    pub fn open(
        files: &BTreeMap<String, String>,
        deploy: &str,
        name: &str,
        seed: [u8; 16],
        entries: &[u8],
        now: Instant,
        nonce: u64,
    ) -> Result<Object, String> {
        let (spec, artifact) = compile(files, deploy)?;
        let (node, member) = if let Some(node) = name.strip_prefix("node/") {
            (node.to_owned(), None)
        } else if let Some((role, key)) = name.strip_prefix("member/").and_then(|r| r.split_once('/')) {
            let p = artifact.program.get();
            let id = p
                .keyed_role_named(role)
                .ok_or_else(|| format!("`{role}` is not a keyed role of the program"))?;
            let host = spec
                .nodes
                .iter()
                .zip(&artifact.roles)
                .find(|(_, r)| **r == Some(id))
                .map(|(n, _)| n.name.clone())
                .ok_or_else(|| format!("the deployment has no node of the keyed role `{role}` to run its members"))?;
            (host, Some(p.member(id, key)))
        } else {
            return Err(format!("`{name}` names no object (node/NAME or member/ROLE/KEY)"));
        };
        let kv = Arc::new(JournalKv::load(decode_entries(entries)?));
        let fs = KvFs::open(kv.clone() as Arc<dyn KvStore>).map_err(|e| e.to_string())?;
        let entropy: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let pool = entropy.clone();
        let node = ObjectNode::open(ObjectConfig {
            spec,
            artifact: Arc::new(artifact),
            node,
            member,
            members: Arc::new(blossom_ir::members::Members::open()),
            fs: Arc::new(fs),
            dir: Path::new("/").join(name),
            seed: Seed(seed),
            now,
            nonce,
            random: Box::new(move |buf| {
                let mut pool = pool
                    .lock()
                    .map_err(|_| RuntimeError::Config("the entropy pool's lock is poisoned".into()))?;
                if pool.len() < buf.len() {
                    return Err(RuntimeError::Config(format!(
                        "the host gave {} random bytes, {} are needed",
                        pool.len(),
                        buf.len()
                    )));
                }
                let rest = pool.split_off(buf.len());
                buf.copy_from_slice(&pool);
                *pool = rest;
                Ok(())
            }),
            externs: Arc::new(blossom_std_host::registry().map_err(|e| e.to_string())?),
            hibernation: None,
            tree: None,
        })
        .map_err(|e| e.to_string())?;
        Ok(Object { node, kv, entropy })
    }

    pub fn app_json(&self) -> &str {
        self.node.app_json()
    }

    pub fn client_part(&self, role: &str) -> Option<&[u8]> {
        self.node.client_part(role)
    }

    pub fn connect(&mut self) -> u64 {
        self.node.connect()
    }

    /// A WebSocket message on connection `conn`, with fresh random bytes for any token it mints.
    pub fn frame(&mut self, conn: u64, bytes: &[u8], now: Instant, entropy: &[u8]) -> Result<(), String> {
        self.entropy
            .lock()
            .map_err(|_| "the entropy pool's lock is poisoned".to_string())?
            .extend_from_slice(entropy);
        self.node.frame(conn, bytes, now).map_err(|e| e.to_string())
    }

    pub fn closed(&mut self, conn: u64, now: Instant) -> Result<(), String> {
        self.node.closed(conn, now).map_err(|e| e.to_string())
    }

    /// The node's committed rows of a durable relation, by its name (for tests and tools).
    pub fn rows(&self, rel: &str) -> Result<Vec<Vec<blossom_value::Value>>, String> {
        Ok(self
            .node
            .rows(rel)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|r| r.to_vec())
            .collect())
    }

    /// A frame another object sent this one by RPC (`sender`: the deployment node that sent a `BATCH`).
    pub fn rpc(&mut self, sender: Option<&str>, bytes: &[u8], now: Instant) -> Result<(), String> {
        self.node.rpc_frame(sender, bytes, now).map_err(|e| e.to_string())
    }

    pub fn wake(&mut self, now: Instant) -> Result<(), String> {
        self.node.wake(now).map_err(|e| e.to_string())
    }

    /// When the alarm should ring (milliseconds since the epoch), if a timer waits.
    pub fn next_wake_ms(&self) -> Result<Option<f64>, String> {
        Ok(self
            .node
            .next_wake()
            .map_err(|e| e.to_string())?
            .map(|i| (i.0 / 1_000_000) as f64))
    }

    /// The storage writes since the last call (see the crate's documentation for the encoding).
    pub fn take_writes(&mut self) -> Result<Vec<u8>, String> {
        encode_writes(self.kv.take_writes().map_err(|e| e.to_string())?)
    }

    /// The frames to write and the connections to close (see the crate's documentation for the encoding).
    pub fn take_output(&mut self) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        let mut sends = Vec::new();
        for o in self.node.take_output() {
            match o {
                Output::Send { to, rel, row, tick } => sends.push((to, rel, row, tick)),
                Output::Frame { conn, bytes } => {
                    out.push(0);
                    out.extend_from_slice(&conn.to_le_bytes());
                    put_bytes(&mut out, &bytes)?;
                }
                Output::Close { conn } => {
                    out.push(1);
                    out.extend_from_slice(&conn.to_le_bytes());
                }
            }
        }
        // Messages to other objects: the node's and members' (a member's frames name it; a node's RPC names it).
        let sender = match self.node.member() {
            Some(_) => String::new(),
            None => self.node.name().to_owned(),
        };
        for (target, frame) in self.node.rpc_frames(&sends).map_err(|e| e.to_string())? {
            out.push(2);
            put_bytes(&mut out, object_name(&target).as_bytes())?;
            put_bytes(&mut out, sender.as_bytes())?;
            put_bytes(&mut out, &frame)?;
        }
        Ok(out)
    }
}

/// One item of [`Object::take_output`], as the Worker reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputItem {
    Frame {
        conn: u64,
        bytes: Vec<u8>,
    },
    Close {
        conn: u64,
    },
    /// A message to the object named `to`; `from` names the deployment node that sent it (empty for a member).
    Rpc {
        to: String,
        from: String,
        frame: Vec<u8>,
    },
}

/// Reads [`Object::take_output`]'s bytes.
pub fn decode_output(mut b: &[u8]) -> Result<Vec<OutputItem>, String> {
    let mut out = Vec::new();
    let text = |b: &mut &[u8]| -> Result<String, String> {
        let n = take_len(b)?;
        String::from_utf8(take(b, n)?.to_vec()).map_err(|_| "a name that is not UTF-8".to_string())
    };
    while let Some((&kind, rest)) = b.split_first() {
        b = rest;
        if kind == 2 {
            let to = text(&mut b)?;
            let from = text(&mut b)?;
            let n = take_len(&mut b)?;
            out.push(OutputItem::Rpc {
                to,
                from,
                frame: take(&mut b, n)?.to_vec(),
            });
            continue;
        }
        let conn = u64::from_le_bytes(
            take(&mut b, 8)?
                .try_into()
                .map_err(|_| "a short connection id".to_string())?,
        );
        if kind == 0 {
            let n = take_len(&mut b)?;
            out.push(OutputItem::Frame {
                conn,
                bytes: take(&mut b, n)?.to_vec(),
            });
        } else {
            out.push(OutputItem::Close { conn });
        }
    }
    Ok(out)
}

/// Storage writes as the Worker reads them: values, or `None` for deletions.
pub fn encode_writes(writes: Vec<(String, Option<Vec<u8>>)>) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for (key, value) in writes {
        out.push(u8::from(value.is_none()));
        put_bytes(&mut out, key.as_bytes())?;
        if let Some(v) = value {
            put_bytes(&mut out, &v)?;
        }
    }
    Ok(out)
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) -> Result<(), String> {
    let len = u32::try_from(b.len()).map_err(|_| format!("{} bytes do not fit a length", b.len()))?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(b);
    Ok(())
}

fn take<'a>(b: &mut &'a [u8], n: usize) -> Result<&'a [u8], String> {
    if b.len() < n {
        return Err("storage entries end in the middle of one".into());
    }
    let (head, rest) = b.split_at(n);
    *b = rest;
    Ok(head)
}

fn take_len(b: &mut &[u8]) -> Result<usize, String> {
    let raw = take(b, 4)?;
    let len = u32::from_le_bytes(raw.try_into().map_err(|_| "a short length".to_string())?);
    Ok(len as usize)
}

/// The storage's entries as the Worker encodes them: values only (a deletion is refused).
pub fn decode_entries(mut b: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = Vec::new();
    while !b.is_empty() {
        let (&op, rest) = b.split_first().ok_or("storage entries end in the middle of one")?;
        b = rest;
        if op != 0 {
            return Err(format!("a stored entry with operation {op}, not a value"));
        }
        let n = take_len(&mut b)?;
        let key = String::from_utf8(take(&mut b, n)?.to_vec()).map_err(|_| "a key that is not UTF-8".to_string())?;
        let n = take_len(&mut b)?;
        out.push((key, take(&mut b, n)?.to_vec()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_encode_as_the_worker_reads_them() {
        let kv = JournalKv::default();
        kv.put("a", b"xyz").unwrap();
        kv.put("b", b"").unwrap();
        kv.delete("b").unwrap();
        let out = encode_writes(kv.take_writes().unwrap()).unwrap();
        assert_eq!(
            out,
            [0, 1, 0, 0, 0, b'a', 3, 0, 0, 0, b'x', b'y', b'z', 1, 1, 0, 0, 0, b'b']
        );
        // What the storage then holds loads back.
        let stored = [0, 1, 0, 0, 0, b'a', 3, 0, 0, 0, b'x', b'y', b'z'];
        assert_eq!(decode_entries(&stored).unwrap(), [("a".to_string(), b"xyz".to_vec())]);
        assert!(decode_entries(&[0, 9, 0, 0, 0]).is_err());
    }
}
