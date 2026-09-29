//! A closed-loop key-value workload and its history (the input of the linearizability checker).
//!
//! Each client thread runs one operation at a time: it picks put, get or delete on a random key, records the instant
//! it invoked it and the instant it saw the response. A put writes a value unique to the operation, so the checker
//! can tell writes apart. When an operation fails (timeout, connection lost: the server may be dead or partitioned)
//! it is recorded as unanswered — it may or may not have taken effect — and the client reconnects, retrying until
//! the workload ends.
//!
//! The same workload drives any store behind [`KvStore`]: a Blossom node running e01, or etcd.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use blossom_sim::linearize::{KvInput, KvOutput, Operation};

use crate::stopwatch::Stopwatch;

/// Why an operation has no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KvError {
    /// No answer (timeout, connection lost, the store refused for now): the operation may or may not have
    /// happened, which the history records.
    Unavailable(String),
    /// An answer that breaks the protocol (a malformed reply). The workload stops trusting the store: it is
    /// reported, never recorded as an unanswered operation the checker would accept.
    Protocol(String),
}

impl From<String> for KvError {
    fn from(e: String) -> KvError {
        KvError::Unavailable(e)
    }
}

/// A session with a key-value store: one operation at a time.
pub trait KvSession: Send {
    fn put(&mut self, key: &[u8], val: &[u8]) -> Result<(), KvError>;
    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, KvError>;
    fn delete(&mut self, key: &[u8]) -> Result<bool, KvError>;
}

/// Opens sessions.
pub trait KvStore: Send + Sync {
    fn connect(&self, client: usize) -> Result<Box<dyn KvSession>, String>;
}

/// The workload's shape.
#[derive(Clone, Debug)]
pub struct Workload {
    pub clients: usize,
    pub duration: Duration,
    pub keys: usize,
    /// Relative weights of put, get and delete.
    pub mix: (u32, u32, u32),
    /// Bytes per value (at least long enough to be unique).
    pub value_size: usize,
    pub seed: u64,
    /// A prefix for every key, so runs against a store that already holds data do not see each other's keys (the
    /// checker assumes every key starts absent).
    pub namespace: String,
    /// Whether to keep the full history (for the checker); latencies are always recorded.
    pub record: bool,
}

/// What the workload produced.
#[derive(Debug, Default)]
pub struct Outcome {
    pub history: Vec<Operation<KvInput, KvOutput>>,
    /// Latencies of answered operations, in nanoseconds.
    pub latencies: Vec<u64>,
    pub answered: u64,
    pub unanswered: u64,
    /// Protocol violations seen (malformed replies): any makes the run invalid.
    pub protocol_errors: Vec<String>,
    pub elapsed: Duration,
}

impl Outcome {
    /// Answered operations per second.
    pub fn throughput(&self) -> f64 {
        self.answered as f64 / self.elapsed.as_secs_f64().max(1e-9)
    }

    /// The `q`-quantile latency (0 ≤ q ≤ 1).
    pub fn latency(&self, q: f64) -> Duration {
        let mut l = self.latencies.clone();
        l.sort_unstable();
        let i = ((l.len() as f64 - 1.0) * q).round().max(0.0) as usize;
        Duration::from_nanos(l.get(i).copied().unwrap_or(0))
    }
}

/// A small deterministic generator (xorshift64*), one per client.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

/// Runs the workload against `store`. `stop` ends it early.
pub fn run(store: Arc<dyn KvStore>, w: &Workload, stop: Arc<AtomicBool>) -> Outcome {
    let epoch = Stopwatch::start();
    let out = Arc::new(Mutex::new(Outcome::default()));
    let deadline = w.duration;
    let mut threads = Vec::new();
    for c in 0..w.clients {
        let (store, out, stop, w) = (store.clone(), out.clone(), stop.clone(), w.clone());
        threads.push(std::thread::spawn(move || {
            client(c, &*store, &w, epoch, deadline, &stop, &out)
        }));
    }
    // A client thread that panicked lost the history it had not merged: that fails the run (a protocol error),
    // since a history with holes proves nothing.
    let mut panicked = Vec::new();
    for (c, t) in threads.into_iter().enumerate() {
        if t.join().is_err() {
            panicked.push(c);
        }
    }
    let mut o = match Arc::try_unwrap(out) {
        Ok(m) => m.into_inner().unwrap_or_default(),
        Err(shared) => shared.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default(),
    };
    for c in panicked {
        o.protocol_errors.push(format!("client {c} panicked; its history is lost"));
    }
    o.elapsed = epoch.elapsed();
    o
}

fn client(
    c: usize,
    store: &dyn KvStore,
    w: &Workload,
    epoch: Stopwatch,
    deadline: Duration,
    stop: &AtomicBool,
    out: &Mutex<Outcome>,
) {
    let mut rng = Rng(w.seed ^ ((c as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)) | 1);
    let mut session: Option<Box<dyn KvSession>> = None;
    let mut seq: u64 = 0;
    let mut local = Outcome::default();
    let total = u64::from(w.mix.0 + w.mix.1 + w.mix.2);
    while epoch.elapsed() < deadline && !stop.load(Ordering::Relaxed) {
        let s = match session.as_mut() {
            Some(s) => s,
            None => match store.connect(c) {
                Ok(s) => session.insert(s),
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
            },
        };
        let key = format!("{}k{}", w.namespace, rng.below(w.keys as u64)).into_bytes();
        let pick = rng.below(total);
        seq += 1;
        let input = if pick < u64::from(w.mix.0) {
            let mut val = format!("c{c}-{seq}").into_bytes();
            while val.len() < w.value_size {
                val.push(b'.');
            }
            KvInput::Put { key, val }
        } else if pick < u64::from(w.mix.0 + w.mix.1) {
            KvInput::Get { key }
        } else {
            KvInput::Delete { key }
        };
        let call = epoch.nanos();
        let result = match &input {
            KvInput::Put { key, val } => s.put(key, val).map(|()| KvOutput::PutOk),
            KvInput::Get { key } => s.get(key).map(KvOutput::Value),
            KvInput::Delete { key } => s.delete(key).map(KvOutput::Deleted),
        };
        let ret = epoch.nanos();
        match result {
            Ok(output) => {
                local.answered += 1;
                local.latencies.push(ret.saturating_sub(call));
                if w.record {
                    local.history.push(Operation {
                        call,
                        ret: Some(ret),
                        input,
                        output: Some(output),
                    });
                }
            }
            Err(KvError::Protocol(e)) => {
                local.protocol_errors.push(e);
                session = None;
            }
            Err(KvError::Unavailable(_)) => {
                local.unanswered += 1;
                if w.record {
                    local.history.push(Operation {
                        call,
                        ret: None,
                        input,
                        output: None,
                    });
                }
                session = None;
            }
        }
    }
    if let Ok(mut o) = out.lock() {
        o.history.append(&mut local.history);
        o.latencies.append(&mut local.latencies);
        o.answered += local.answered;
        o.protocol_errors.append(&mut local.protocol_errors);
        o.unanswered += local.unanswered;
    }
}
