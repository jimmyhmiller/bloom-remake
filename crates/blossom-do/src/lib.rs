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
//! - output ([`Object::take_output`]): per item `kind: u8` (0 a frame, 1 a close), `conn: u64`, and for a frame `len:
//!   u32` and its bytes.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use blossom_front::api::{BlsError, NodeSpec, ParamBinding};
use blossom_front::ded::LoadedFile;
use blossom_front::modules::Loader;
use blossom_runtime::RuntimeError;
use blossom_runtime::deploy::{DeploymentSpec, ParamValue};
use blossom_runtime::object::{ObjectConfig, ObjectNode, Output};
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
    /// Starts the node `node` of the deployment `deploy` (a deployment spec's text; its `source` names the root among
    /// `files`), its store the one `entries` hold (none on the object's first start).
    pub fn open(
        files: &BTreeMap<String, String>,
        deploy: &str,
        node: &str,
        seed: [u8; 16],
        entries: &[u8],
        now: Instant,
        nonce: u64,
    ) -> Result<Object, String> {
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
        let kv = Arc::new(JournalKv::load(decode_entries(entries)?));
        let fs = KvFs::open(kv.clone() as Arc<dyn KvStore>).map_err(|e| e.to_string())?;
        let entropy: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let pool = entropy.clone();
        let node = ObjectNode::open(ObjectConfig {
            spec,
            artifact: Arc::new(artifact),
            node: node.to_owned(),
            member: None,
            members: Arc::new(blossom_ir::members::Members::open()),
            fs: Arc::new(fs),
            dir: Path::new("/node").join(node),
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
        for o in self.node.take_output() {
            match o {
                Output::Frame { conn, bytes } => {
                    out.push(0);
                    out.extend_from_slice(&conn.to_le_bytes());
                    put_bytes(&mut out, &bytes)?;
                }
                Output::Close { conn } => {
                    out.push(1);
                    out.extend_from_slice(&conn.to_le_bytes());
                }
                // Objects reach each other by RPC, which this prototype does not do yet.
                Output::Send { .. } => {
                    return Err(
                        "a send to another node or a keyed member: an object reaches others by RPC, which \
                                this prototype does not do yet (docs/design/KEYED.md §4, sub-slice 4)"
                            .into(),
                    );
                }
            }
        }
        Ok(out)
    }
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
