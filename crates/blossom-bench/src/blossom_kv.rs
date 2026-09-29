//! The client protocol of the Blossom key-value stores (`examples/e01_kvs.bls`, `examples/e11_raft_kv.bls`):
//! `put(id, key, val)` answered by `put_ok(id)`, `get(id, key)` by `get_resp(id, key, val)`, `del(id, key)` by
//! `del_ok(id, existed)`. Keys are strings and values bytes; a session numbers its requests and matches replies by id.
//!
//! A replicated store may answer `redirect(id, leader)` instead: the server does not lead, and the request had no
//! effect. The session then reconnects to the leader it names (or to the next server when it names none) and sends
//! the same operation again, until the operation's timeout.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::RelId;
use blossom_runtime::client::Client;
use blossom_runtime::net::Identity;
use blossom_value::Value;
use blossom_value::value::IntValue;

use crate::kv::{KvError, KvSession, KvStore};
use crate::stopwatch::Stopwatch;

/// Sessions to the client listeners of a Blossom KV deployment's nodes; client `c` starts at node `c mod n`.
pub struct BlossomKvStore {
    /// Each node's client address, by node id (`None` for a node without a client listener).
    pub addrs: Vec<Option<SocketAddr>>,
    pub artifact: Arc<BlsArtifact>,
    pub id: Identity,
    pub principal: String,
    /// How long an operation may take, redirects included, before it counts as unanswered.
    pub timeout: Duration,
}

#[derive(Clone, Copy)]
struct Rels {
    put: RelId,
    put_ok: RelId,
    get: RelId,
    get_resp: RelId,
    del: RelId,
    del_ok: RelId,
    redirect: Option<RelId>,
}

struct BlossomSession {
    store: Arc<Shared>,
    /// The node this session is connected to, and the connection.
    at: usize,
    client: Option<Client>,
    rels: Rels,
    next: u64,
}

/// What every session of a store shares.
struct Shared {
    addrs: Vec<Option<SocketAddr>>,
    artifact: Arc<BlsArtifact>,
    id: Identity,
    principal: String,
    timeout: Duration,
}

impl KvStore for BlossomKvStore {
    fn connect(&self, client: usize) -> Result<Box<dyn KvSession>, String> {
        let rel = |n: &str| {
            self.artifact
                .rel_named(n)
                .ok_or_else(|| format!("the program has no channel `{n}`"))
        };
        let rels = Rels {
            put: rel("put")?,
            put_ok: rel("put_ok")?,
            get: rel("get")?,
            get_resp: rel("get_resp")?,
            del: rel("del")?,
            del_ok: rel("del_ok")?,
            redirect: self.artifact.rel_named("redirect"),
        };
        let n = self.addrs.len().max(1);
        let mut s = BlossomSession {
            store: Arc::new(Shared {
                addrs: self.addrs.clone(),
                artifact: self.artifact.clone(),
                id: self.id.clone(),
                principal: self.principal.clone(),
                timeout: self.timeout,
            }),
            at: client % n,
            client: None,
            rels,
            next: 0,
        };
        s.reconnect(client % n).map_err(|e| format!("{e:?}"))?;
        Ok(Box::new(s))
    }
}

fn key_value(key: &[u8]) -> Result<Value, KvError> {
    std::str::from_utf8(key)
        .map(|k| Value::Str(k.into()))
        .map_err(|_| KvError::Protocol("keys are strings".to_string()))
}

impl BlossomSession {
    fn reconnect(&mut self, node: usize) -> Result<(), KvError> {
        self.client = None;
        self.at = node;
        let addr = self
            .store
            .addrs
            .get(node)
            .copied()
            .flatten()
            .ok_or_else(|| KvError::Unavailable(format!("node {node} has no client address")))?;
        let s = &self.store;
        let c = Client::connect(addr, s.artifact.clone(), &s.id, &s.principal, s.timeout)
            .map_err(|e| KvError::Unavailable(e.to_string()))?;
        self.client = Some(c);
        Ok(())
    }

    /// Sends one request and waits for its reply (following redirects); returns the reply's columns after the session
    /// column.
    fn call(&mut self, rel: RelId, reply: RelId, fields: Vec<Value>) -> Result<Vec<Value>, KvError> {
        let clock = Stopwatch::start();
        let timeout = self.store.timeout;
        let nodes = self.store.addrs.len().max(1);
        loop {
            if clock.elapsed() >= timeout {
                return Err(KvError::Unavailable("timed out".into()));
            }
            if self.client.is_none() {
                let at = self.at;
                if self.reconnect(at).is_err() {
                    // The node is down: try the next one after a moment.
                    std::thread::sleep(Duration::from_millis(20));
                    self.at = (self.at + 1) % nodes;
                    continue;
                }
            }
            self.next += 1;
            let id = self.next;
            let mut row = vec![Value::Int(IntValue::U64(id))];
            row.extend(fields.iter().cloned());
            let client = self
                .client
                .as_mut()
                .ok_or_else(|| KvError::Unavailable("no connection".into()))?;
            if client.send(rel, &[row]).is_err() {
                self.client = None;
                return Err(KvError::Unavailable("the connection broke while sending".into()));
            }
            // Wait for this request's answer.
            let answer = loop {
                let left = timeout.saturating_sub(clock.elapsed());
                if left.is_zero() {
                    return Err(KvError::Unavailable("timed out".into()));
                }
                let got = match self.client.as_mut() {
                    Some(c) => c.recv(Some(left)),
                    None => return Err(KvError::Unavailable("no connection".into())),
                };
                match got {
                    Err(e) => {
                        // The request may have taken effect: unanswered.
                        self.client = None;
                        return Err(KvError::Unavailable(e.to_string()));
                    }
                    Ok(None) => return Err(KvError::Unavailable("timed out".into())),
                    Ok(Some((r, row))) => {
                        let same = matches!(row.get(1), Some(Value::Int(IntValue::U64(x))) if *x == id);
                        if same && (r == reply || Some(r) == self.rels.redirect) {
                            break (r, row);
                        }
                    }
                }
            };
            let (r, row) = answer;
            if r == reply {
                return Ok(row.iter().skip(1).cloned().collect());
            }
            // A redirect: the request had no effect. Go to the named leader, or the next server.
            let target = match row.get(2) {
                Some(Value::Option(Some(v))) => match &**v {
                    Value::Node(n) => n.0 as usize,
                    other => return Err(KvError::Protocol(format!("redirect to {other:?}"))),
                },
                Some(Value::Option(None)) => {
                    std::thread::sleep(Duration::from_millis(20));
                    (self.at + 1) % nodes
                }
                other => return Err(KvError::Protocol(format!("redirect {other:?}"))),
            };
            if target != self.at {
                let _ = self.reconnect(target);
            }
        }
    }
}

impl KvSession for BlossomSession {
    fn put(&mut self, key: &[u8], val: &[u8]) -> Result<(), KvError> {
        let r = self.rels;
        self.call(r.put, r.put_ok, vec![key_value(key)?, Value::Bytes(val.into())]).map(|_| ())
    }

    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, KvError> {
        let r = self.rels;
        let row = self.call(r.get, r.get_resp, vec![key_value(key)?])?;
        match row.get(2) {
            Some(Value::Option(None)) => Ok(None),
            Some(Value::Option(Some(v))) => match &**v {
                Value::Bytes(b) => Ok(Some(b.to_vec())),
                other => Err(KvError::Protocol(format!("get_resp value {other:?}"))),
            },
            other => Err(KvError::Protocol(format!("get_resp value {other:?}"))),
        }
    }

    fn delete(&mut self, key: &[u8]) -> Result<bool, KvError> {
        let r = self.rels;
        match self.call(r.del, r.del_ok, vec![key_value(key)?])?.get(1) {
            Some(Value::Bool(b)) => Ok(*b),
            other => Err(KvError::Protocol(format!("del_ok existed {other:?}"))),
        }
    }
}
