//! The e01 key-value store's client protocol (`examples/e01_kvs.bls`): `put(id, key, val)` answered by
//! `put_ok(id)`, `get(id, key)` by `get_resp(id, key, val)`, `del(id, key)` by `del_ok(id, existed)`. Keys are
//! strings and values bytes; a session numbers its requests and matches replies by id.

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

/// Sessions to the client listeners of e01 nodes; client `c` uses node `c mod n`.
pub struct E01Store {
    pub addrs: Vec<SocketAddr>,
    pub artifact: Arc<BlsArtifact>,
    pub id: Identity,
    pub principal: String,
    /// How long to wait for a reply before counting the operation unanswered.
    pub timeout: Duration,
}

struct Rels {
    put: RelId,
    put_ok: RelId,
    get: RelId,
    get_resp: RelId,
    del: RelId,
    del_ok: RelId,
}

struct E01Session {
    client: Client,
    rels: Rels,
    next: u64,
    timeout: Duration,
}

impl KvStore for E01Store {
    fn connect(&self, client: usize) -> Result<Box<dyn KvSession>, String> {
        let addr = self
            .addrs
            .get(client % self.addrs.len().max(1))
            .ok_or_else(|| "no node addresses".to_string())?;
        let c = Client::connect(*addr, self.artifact.clone(), &self.id, &self.principal, self.timeout)
            .map_err(|e| e.to_string())?;
        let rel = |n: &str| c.rel(n).map_err(|e| e.to_string());
        let rels = Rels {
            put: rel("put")?,
            put_ok: rel("put_ok")?,
            get: rel("get")?,
            get_resp: rel("get_resp")?,
            del: rel("del")?,
            del_ok: rel("del_ok")?,
        };
        Ok(Box::new(E01Session {
            client: c,
            rels,
            next: 0,
            timeout: self.timeout,
        }))
    }
}

fn key_value(key: &[u8]) -> Result<Value, KvError> {
    std::str::from_utf8(key)
        .map(|k| Value::Str(k.into()))
        .map_err(|_| KvError::Protocol("e01 keys are strings".to_string()))
}

impl E01Session {
    /// Sends one request and waits for the reply on `reply` with the same id; returns the reply's columns after
    /// the session column.
    fn call(&mut self, rel: RelId, reply: RelId, fields: Vec<Value>) -> Result<Vec<Value>, KvError> {
        self.next += 1;
        let id = self.next;
        let mut row = vec![Value::Int(IntValue::U64(id))];
        row.extend(fields);
        self.client
            .send(rel, &[row])
            .map_err(|e| KvError::Unavailable(e.to_string()))?;
        let clock = Stopwatch::start();
        loop {
            let left = self.timeout.saturating_sub(clock.elapsed());
            if left.is_zero() {
                return Err(KvError::Unavailable("timed out".into()));
            }
            match self
                .client
                .recv(Some(left))
                .map_err(|e| KvError::Unavailable(e.to_string()))?
            {
                None => return Err(KvError::Unavailable("timed out".into())),
                Some((r, row)) => {
                    let matches = r == reply && matches!(row.get(1), Some(Value::Int(IntValue::U64(x))) if *x == id);
                    if matches {
                        return Ok(row.iter().skip(1).cloned().collect());
                    }
                }
            }
        }
    }
}

impl KvSession for E01Session {
    fn put(&mut self, key: &[u8], val: &[u8]) -> Result<(), KvError> {
        let (put, put_ok) = (self.rels.put, self.rels.put_ok);
        self.call(put, put_ok, vec![key_value(key)?, Value::Bytes(val.into())])
            .map(|_| ())
    }

    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, KvError> {
        let (get, get_resp) = (self.rels.get, self.rels.get_resp);
        let r = self.call(get, get_resp, vec![key_value(key)?])?;
        match r.get(2) {
            Some(Value::Option(None)) => Ok(None),
            Some(Value::Option(Some(v))) => match &**v {
                Value::Bytes(b) => Ok(Some(b.to_vec())),
                other => Err(KvError::Protocol(format!("get_resp value {other:?}"))),
            },
            other => Err(KvError::Protocol(format!("get_resp value {other:?}"))),
        }
    }

    fn delete(&mut self, key: &[u8]) -> Result<bool, KvError> {
        let (del, del_ok) = (self.rels.del, self.rels.del_ok);
        match self.call(del, del_ok, vec![key_value(key)?])?.get(1) {
            Some(Value::Bool(b)) => Ok(*b),
            other => Err(KvError::Protocol(format!("del_ok existed {other:?}"))),
        }
    }
}
