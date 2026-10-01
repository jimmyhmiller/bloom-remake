//! Slice 7, item 5: Produce on the Blossom broker (`examples/kafka/produce_node.bls`) under broker crashes and
//! dropped connections, in the cluster simulator.
//!
//! Rust clients (requests encoded and responses decoded by `kafka-protocol`, decision K5) create a topic, then
//! produce batches with acks -1, 1 and 0, some corrupted and some to a partition that does not exist, at versions 10
//! to 12. At the end the broker's durable rows must show:
//! - every acknowledged batch, at the offset its answer gave, with exactly the bytes sent (its `baseOffset` and
//!   `partitionLeaderEpoch` written): an acknowledged produce is durable before its answer leaves (Invariant B);
//! - each partition's batches tiling its offsets from 0 to `log_end`, with no gap and no overlap;
//! - only batches some client sent, each at most once; nothing corrupted, nothing refused.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::durable::DurableSchema;
use blossom_sim::cluster::{Cluster, ClusterConfig, NoKvClients, StreamAction, StreamClient, StreamEvent};
use blossom_value::time::NodeId;
use blossom_value::value::IntValue;
use blossom_value::{BlobRef, Value};
use bytes::{BufMut, Bytes, BytesMut};
use kafka_protocol::messages::create_topics_request::CreatableTopic;
use kafka_protocol::messages::produce_request::{PartitionProduceData, TopicProduceData};
use kafka_protocol::messages::{
    CreateTopicsRequest, CreateTopicsResponse, InitProducerIdRequest, InitProducerIdResponse, ProduceRequest,
    ProduceResponse, ProducerId, RequestHeader, ResponseHeader, TopicName,
};
use kafka_protocol::protocol::{Decodable, Encodable, HeaderVersion, StrBytes};
use kafka_protocol::records::{Compression, Record, RecordBatchEncoder, RecordEncodeOptions, TimestampType};

#[cfg(test)]
const TOPIC: &str = "logs";
#[cfg(test)]
const PARTITIONS: i32 = 3;

/// SplitMix64.
#[cfg(test)]
struct Rng(u64);

#[cfg(test)]
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

/// A magic-2 batch of `n` records whose values name the client and the batch, "compressed" (the codec is the
/// identity: the broker never looks inside) or not. `pid` -1 is a producer without idempotence; otherwise the batch
/// carries the producer's id and epoch and starts at sequence `seq`.
#[cfg(test)]
fn batch(tag: &str, n: usize, compressed: bool, pid: i64, epoch: i16, seq: i32) -> Vec<u8> {
    let records: Vec<Record> = (0..n)
        .map(|i| Record {
            transactional: false,
            control: false,
            delete_horizon: false,
            partition_leader_epoch: -1,
            producer_id: pid,
            producer_epoch: epoch,
            timestamp_type: TimestampType::Creation,
            offset: i as i64,
            // A producer without idempotence sends base sequence -1; the encoder derives the rest from it.
            sequence: if pid == -1 { i as i32 - 1 } else { seq + i as i32 },
            timestamp: 1_700_000_000_000 + i as i64,
            key: None,
            value: Some(Bytes::from(format!("{tag}/{i}"))),
            headers: Default::default(),
        })
        .collect();
    let mut buf = BytesMut::new();
    let options = RecordEncodeOptions {
        version: 2,
        compression: if compressed {
            Compression::Gzip
        } else {
            Compression::None
        },
    };
    RecordBatchEncoder::encode_with_custom_compression(
        &mut buf,
        &records,
        &options,
        Some(|input: &mut BytesMut, out: &mut BytesMut, _c: Compression| {
            out.put_slice(input);
            Ok(())
        }),
    )
    .unwrap();
    buf.to_vec()
}

/// How many offsets a batch takes: `lastOffsetDelta + 1`.
#[cfg(test)]
fn offsets_of(b: &[u8]) -> i64 {
    i64::from(i32::from_be_bytes([b[23], b[24], b[25], b[26]])) + 1
}

/// A batch as the broker stores it at `base`: `baseOffset` written, `partitionLeaderEpoch` 0.
#[cfg(test)]
fn stamped(b: &[u8], base: i64) -> Vec<u8> {
    let mut out = b.to_vec();
    out[0..8].copy_from_slice(&base.to_be_bytes());
    out[12..16].copy_from_slice(&0i32.to_be_bytes());
    out
}

#[cfg(test)]
fn framed<M: Encodable + HeaderVersion>(key: i16, version: i16, corr: i32, body: &M) -> Vec<u8> {
    let mut buf = BytesMut::new();
    RequestHeader::default()
        .with_request_api_key(key)
        .with_request_api_version(version)
        .with_correlation_id(corr)
        .with_client_id(Some(StrBytes::from_string("produce".into())))
        .encode(&mut buf, M::header_version(version))
        .unwrap();
    body.encode(&mut buf, version).unwrap();
    let mut out = (buf.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&buf);
    out
}

/// One Produce request: its acks, and per entry the partition, the batches and whether one was corrupted.
#[cfg(test)]
#[derive(Clone, Debug)]
struct Produce {
    version: i16,
    acks: i16,
    entries: Vec<(i32, Vec<Vec<u8>>, bool)>,
}

/// A request's fate: unanswered, or each entry's (error, base offset).
#[cfg(test)]
#[derive(Clone, Debug)]
struct Sent {
    produce: Produce,
    answer: Option<Vec<(i16, i64)>>,
}

#[cfg(test)]
#[derive(Default)]
struct Shared {
    sent: Vec<Sent>,
    /// The producer ids InitProducerId handed out.
    pids: Vec<i64>,
    /// How many requests were sent again.
    resends: usize,
}

#[cfg(test)]
enum Pending {
    Create,
    InitPid,
    Produce(usize),
}

#[cfg(test)]
struct Client {
    id: u64,
    shared: Rc<RefCell<Shared>>,
    rng: Rng,
    left: u32,
    created: bool,
    connected: bool,
    open: bool,
    buf: Vec<u8>,
    pending: Option<Pending>,
    corr: i32,
    batches: u64,
    /// When set, the client sends only at multiples of this many nanoseconds, so requests arrive together and
    /// append to one partition in one tick; otherwise it thinks a random while between requests.
    grid: Option<i64>,
    /// An idempotent producer: it asks for a producer id, numbers its batches per partition, sends acks -1, and
    /// resends a request whose connection closed before its answer, as Kafka's producer does.
    idempotent: bool,
    pid: Option<(i64, i16)>,
    /// Per partition: the sequence its next batch starts at.
    next_seq: BTreeMap<i32, i32>,
    /// A request to send again (its index in the shared list).
    retry: Option<usize>,
}

#[cfg(test)]
impl Client {
    /// The client's next batch for partition `p`, starting at sequence `seq`.
    fn next_batch(&mut self, seq: i32) -> Vec<u8> {
        self.batches += 1;
        let n = 1 + self.rng.below(3) as usize;
        let compressed = self.rng.below(3) == 0;
        let (pid, epoch) = if self.idempotent {
            self.pid.unwrap_or((-1, -1))
        } else {
            (-1, -1)
        };
        batch(
            &format!("c{}b{}", self.id, self.batches),
            n,
            compressed,
            pid,
            epoch,
            seq,
        )
    }

    fn send_produce(&mut self, produce: &Produce, a: &mut StreamAction) {
        let req = ProduceRequest::default()
            .with_acks(produce.acks)
            .with_timeout_ms(30_000)
            .with_topic_data(vec![
                TopicProduceData::default()
                    .with_name(TopicName(StrBytes::from_string(TOPIC.into())))
                    .with_partition_data(
                        produce
                            .entries
                            .iter()
                            .map(|(p, bs, _)| {
                                PartitionProduceData::default()
                                    .with_index(*p)
                                    .with_records(Some(Bytes::from(bs.concat())))
                            })
                            .collect(),
                    ),
            ]);
        a.send = framed(0, produce.version, self.corr, &req);
    }

    fn issue(&mut self, a: &mut StreamAction) {
        self.corr += 1;
        if !self.created {
            let req = CreateTopicsRequest::default()
                .with_topics(vec![
                    CreatableTopic::default()
                        .with_name(TopicName(StrBytes::from_string(TOPIC.into())))
                        .with_num_partitions(PARTITIONS)
                        .with_replication_factor(1),
                ])
                .with_timeout_ms(30_000);
            a.send = framed(19, 7, self.corr, &req);
            self.pending = Some(Pending::Create);
            return;
        }
        if self.idempotent && self.pid.is_none() {
            let req = InitProducerIdRequest::default()
                .with_transactional_id(None)
                .with_transaction_timeout_ms(60_000)
                .with_producer_id(ProducerId(-1))
                .with_producer_epoch(-1);
            a.send = framed(22, 5, self.corr, &req);
            self.pending = Some(Pending::InitPid);
            return;
        }
        if let Some(at) = self.retry.take() {
            let produce = self.shared.borrow().sent[at].produce.clone();
            self.send_produce(&produce, a);
            self.shared.borrow_mut().resends += 1;
            self.pending = Some(Pending::Produce(at));
            return;
        }
        if self.left == 0 {
            return;
        }
        self.left -= 1;
        let acks = if self.idempotent {
            -1
        } else {
            [-1i16, 1, 1, 0][self.rng.below(4) as usize]
        };
        let mut entries = Vec::new();
        let mut used = BTreeSet::new();
        for _ in 0..1 + self.rng.below(2) {
            // Mostly the topic's partitions; now and then one it does not have.
            let p = if self.rng.below(12) == 0 {
                PARTITIONS + 4
            } else {
                self.rng.below(PARTITIONS as u64) as i32
            };
            if !used.insert(p) {
                continue;
            }
            let mut seq = self.next_seq.get(&p).copied().unwrap_or(0);
            let mut bs: Vec<Vec<u8>> = Vec::new();
            // One batch per entry, as Produce v3 and later require; now and then two, which is refused.
            for _ in 0..if self.rng.below(12) == 0 { 2 } else { 1 } {
                let b = self.next_batch(seq);
                seq += offsets_of(&b) as i32;
                bs.push(b);
            }
            let corrupt = self.rng.below(10) == 0;
            if corrupt {
                let last = bs.last_mut().unwrap();
                let at = last.len() - 1;
                last[at] ^= 0x5a;
            }
            entries.push((p, bs, corrupt));
        }
        let version = 10 + self.rng.below(3) as i16;
        let produce = Produce { version, acks, entries };
        self.send_produce(&produce, a);
        let mut sh = self.shared.borrow_mut();
        sh.sent.push(Sent { produce, answer: None });
        let at = sh.sent.len() - 1;
        // acks 0 has no answer: the request is done once written.
        self.pending = if acks == 0 { None } else { Some(Pending::Produce(at)) };
    }
}

#[cfg(test)]
impl StreamClient for Client {
    fn on(&mut self, now: i64, e: StreamEvent<'_>) -> Result<StreamAction, String> {
        let mut a = StreamAction::default();
        match e {
            StreamEvent::Wake => {
                if !self.connected && (self.left > 0 || !self.created || self.retry.is_some()) {
                    self.connected = true;
                    self.open = false;
                    a.connect = Some((NodeId(0), Arc::from("kafka")));
                } else if self.open && self.pending.is_none() {
                    self.issue(&mut a);
                    if self.pending.is_none() && self.left > 0 {
                        a.wake = Some(match self.grid {
                            Some(g) => (now / g + 1) * g,
                            None => now + 5_000_000,
                        });
                    }
                }
            }
            StreamEvent::Opened => {
                self.buf.clear();
                self.open = true;
                match self.grid {
                    Some(g) if self.created => a.wake = Some((now / g + 1) * g),
                    _ => {
                        self.issue(&mut a);
                        if self.pending.is_none() {
                            a.wake = Some(now + 5_000_000);
                        }
                    }
                }
            }
            StreamEvent::Received(b) => {
                self.buf.extend_from_slice(b);
                let Some(n) = self
                    .buf
                    .get(..4)
                    .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
                else {
                    return Ok(a);
                };
                if self.buf.len() < 4 + n {
                    return Ok(a);
                }
                if self.buf.len() > 4 + n {
                    return Err("more than one response to one request".into());
                }
                let mut body = Bytes::copy_from_slice(&self.buf[4..]);
                self.buf.clear();
                match self.pending.take() {
                    Some(Pending::Create) => {
                        ResponseHeader::decode(&mut body, CreateTopicsResponse::header_version(7))
                            .map_err(|e| e.to_string())?;
                        let r = CreateTopicsResponse::decode(&mut body, 7).map_err(|e| e.to_string())?;
                        let code = r.topics.first().map(|t| t.error_code);
                        if code != Some(0) && code != Some(36) {
                            return Err(format!("creating the topic failed: {code:?}"));
                        }
                        self.created = true;
                    }
                    Some(Pending::InitPid) => {
                        ResponseHeader::decode(&mut body, InitProducerIdResponse::header_version(5))
                            .map_err(|e| e.to_string())?;
                        let r = InitProducerIdResponse::decode(&mut body, 5).map_err(|e| e.to_string())?;
                        if r.error_code != 0 || r.producer_id.0 < 0 || r.producer_epoch != 0 {
                            return Err(format!("InitProducerId answered {r:?}"));
                        }
                        self.pid = Some((r.producer_id.0, r.producer_epoch));
                        self.shared.borrow_mut().pids.push(r.producer_id.0);
                    }
                    Some(Pending::Produce(at)) => {
                        let version = 12;
                        ResponseHeader::decode(&mut body, ProduceResponse::header_version(version))
                            .map_err(|e| e.to_string())?;
                        let r = ProduceResponse::decode(&mut body, version).map_err(|e| e.to_string())?;
                        let answers: Vec<(i16, i64)> = r
                            .responses
                            .iter()
                            .flat_map(|t| t.partition_responses.iter().map(|p| (p.error_code, p.base_offset)))
                            .collect();
                        let mut sh = self.shared.borrow_mut();
                        // An acknowledged partition's next batch continues its sequence.
                        for ((p, bs, _), (code, _)) in sh.sent[at].produce.entries.iter().zip(&answers) {
                            if *code == 0 {
                                let n: i64 = bs.iter().map(|b| offsets_of(b)).sum();
                                *self.next_seq.entry(*p).or_insert(0) += n as i32;
                            }
                        }
                        sh.sent[at].answer = Some(answers);
                    }
                    None => return Err("a response with no request in flight".into()),
                }
                if !body.is_empty() {
                    return Err(format!("{} bytes after a response", body.len()));
                }
                a.wake = Some(match self.grid {
                    Some(g) => (now / g + 1) * g,
                    None => now + self.rng.below(30_000_000) as i64,
                });
            }
            StreamEvent::Closed(_) => {
                if let (true, Some(Pending::Produce(at))) = (self.idempotent, &self.pending) {
                    self.retry = Some(*at);
                }
                self.pending = None;
                self.connected = false;
                self.open = false;
                a.wake = Some(now + 15_000_000);
            }
        }
        Ok(a)
    }
}

#[cfg(test)]
fn int(v: &Value) -> i64 {
    match v {
        Value::Int(IntValue::I32(x)) => i64::from(*x),
        Value::Int(IntValue::I64(x)) => *x,
        other => panic!("{other:?}"),
    }
}

/// How a run is set up.
#[cfg(test)]
struct Setup {
    seeds: std::ops::RangeInclusive<u64>,
    crashes: bool,
    latency: (i64, i64),
    clients: u64,
    requests: u32,
    grid: Option<i64>,
    idempotent: bool,
    /// How often the nemesis acts, in nanoseconds (at most).
    nemesis: i64,
}

/// Runs each seed and checks the broker's rows against what the clients sent and were told; returns the partition
/// entries acknowledged, left unanswered, and refused, and the requests sent again.
#[cfg(test)]
fn check_runs(setup: &Setup) -> (usize, usize, usize, usize) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/kafka/sim_cluster.bls");
    let nodes = [
        NodeSpec {
            name: "b1".to_owned(),
            role: Some("Broker".to_owned()),
        },
        NodeSpec {
            name: "c1".to_owned(),
            role: Some("Client".to_owned()),
        },
    ];
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    let artifact: BlsArtifact = result.unwrap_or_else(|e| panic!("sim_cluster.bls: {e:?}")).0;
    let schema = DurableSchema::of(artifact.program.get());
    let (mut acked, mut unanswered, mut refused, mut resends) = (0, 0, 0, 0);
    for seed in setup.seeds.clone() {
        let cfg = ClusterConfig {
            seed,
            clients: 0,
            latency: setup.latency,
            chunk_max: 64 + (seed as usize % 5) * 200,
            nemesis: setup.nemesis,
            crashes: setup.crashes,
            downtime: 40_000_000,
            stream_drops: setup.crashes,
            duration: 3_000_000_000,
            externs: Arc::new(blossom_std_host::registry().unwrap()),
            ..ClusterConfig::default()
        };
        let mut cluster = Cluster::new(
            &artifact,
            &schema,
            blossom_value::Seed::from_u64(seed),
            Vec::new(),
            Box::new(NoKvClients),
            cfg,
        )
        .unwrap();
        let shared = Rc::new(RefCell::new(Shared::default()));
        for c in 0..setup.clients {
            cluster.stream_client(Box::new(Client {
                id: c,
                shared: shared.clone(),
                rng: Rng(seed * 1000 + c),
                left: setup.requests,
                created: false,
                connected: false,
                open: false,
                buf: Vec::new(),
                pending: None,
                corr: 0,
                batches: 0,
                grid: setup.grid,
                idempotent: setup.idempotent,
                pid: None,
                next_seq: BTreeMap::new(),
                retry: None,
            }));
        }
        cluster.run_until(3_000_000_000).unwrap();
        let run = cluster.run_so_far();
        assert!(
            run.violation.is_none(),
            "seed {seed}: {:?}\n{}",
            run.violation,
            run.log.join("\n")
        );
        cluster.step_until(3_500_000_000).unwrap();
        let state = cluster.state(NodeId(0)).expect("the broker is up");
        let tid = state
            .rows(artifact.rel_named("topic").unwrap())
            .find(|r| r[0] == Value::Str(TOPIC.into()))
            .map(|r| r[1].clone())
            .expect("the topic exists");
        // The stored batches, by partition and base offset: (last offset, blob).
        let mut stored: BTreeMap<(i64, i64), (i64, BlobRef)> = BTreeMap::new();
        for r in state.rows(artifact.rel_named("batch").unwrap()) {
            assert_eq!(r[0], tid, "seed {seed}: a batch of another topic");
            let Value::Blob(b) = &r[5] else { panic!("{:?}", r[5]) };
            stored.insert((int(&r[1]), int(&r[2])), (int(&r[3]), *b));
        }
        let ends: BTreeMap<i64, i64> = state
            .rows(artifact.rel_named("log_end").unwrap())
            .map(|r| (int(&r[1]), int(&r[2])))
            .collect();
        // Each partition's batches tile [0, log_end).
        for p in 0..i64::from(PARTITIONS) {
            let mut next = 0;
            for ((_, base), (last, _)) in stored.range((p, i64::MIN)..=(p, i64::MAX)) {
                assert_eq!(
                    *base, next,
                    "seed {seed}: partition {p} has a gap or an overlap at {base}"
                );
                next = last + 1;
            }
            assert_eq!(
                ends.get(&p).copied(),
                Some(next),
                "seed {seed}: partition {p}'s log end"
            );
        }
        // Every acknowledged batch is stored at its offset; every stored batch was sent once and not refused.
        let sent = shared.borrow().sent.clone();
        resends += shared.borrow().resends;
        // Producer ids are never handed out twice, across crashes too.
        let pids = shared.borrow().pids.clone();
        assert_eq!(
            pids.iter().collect::<BTreeSet<_>>().len(),
            pids.len(),
            "seed {seed}: a producer id twice: {pids:?}"
        );
        let mut placed: BTreeMap<(i64, i64), BlobRef> = BTreeMap::new();
        let mut candidates: Vec<(i64, Vec<u8>, bool)> = Vec::new();
        for s in &sent {
            for (i, (p, bs, corrupt)) in s.produce.entries.iter().enumerate() {
                for b in bs {
                    candidates.push((i64::from(*p), b.clone(), *corrupt));
                }
                match &s.answer {
                    None if s.produce.acks == 0 => {}
                    None => unanswered += 1,
                    Some(answer) => {
                        let (code, base) = answer[i];
                        let want = if *p >= PARTITIONS {
                            3
                        } else if bs.len() != 1 {
                            87
                        } else if *corrupt {
                            2
                        } else {
                            0
                        };
                        assert_eq!(code, want, "seed {seed}: the answer to {:?}", s.produce);
                        if code != 0 {
                            refused += 1;
                            continue;
                        }
                        acked += 1;
                        let mut at = base;
                        for b in bs {
                            placed.insert((i64::from(*p), at), BlobRef::of(&stamped(b, at)));
                            at += offsets_of(b);
                        }
                    }
                }
            }
        }
        for (key, blob) in &placed {
            assert_eq!(
                stored.get(key).map(|x| x.1),
                Some(*blob),
                "seed {seed}: the acknowledged batch at {key:?} is not stored as sent"
            );
        }
        let mut seen = BTreeSet::new();
        for ((p, base), (_, blob)) in &stored {
            let from: Vec<&(i64, Vec<u8>, bool)> = candidates
                .iter()
                .filter(|(cp, b, _)| cp == p && BlobRef::of(&stamped(b, *base)) == *blob)
                .collect();
            assert_eq!(
                from.len(),
                1,
                "seed {seed}: the batch at {p}/{base} matches {} sent batches",
                from.len()
            );
            assert!(
                !from[0].2,
                "seed {seed}: a batch of a corrupted entry was stored at {p}/{base}"
            );
            assert!(seen.insert(from[0].1.clone()), "seed {seed}: a batch is stored twice");
        }
    }
    (acked, unanswered, refused, resends)
}

/// Produces in flight across broker crashes and dropped connections.
#[test]
fn acknowledged_produces_are_durable_at_their_offsets() {
    let (acked, unanswered, refused, _) = check_runs(&Setup {
        seeds: 1..=8,
        crashes: true,
        latency: ClusterConfig::default().latency,
        clients: 3,
        requests: 40,
        grid: None,
        idempotent: false,
        nemesis: 150_000_000,
    });
    assert!(acked > 300, "only {acked} partition entries were acknowledged");
    assert!(
        unanswered > 0,
        "no produce was left unanswered: the faults did not bite"
    );
    assert!(refused > 10, "only {refused} partition entries were refused");
}

/// Produces that arrive together append to the same partitions in one tick: each gets the offsets after the ones
/// before it.
#[test]
fn produces_in_one_tick_take_consecutive_offsets() {
    let (acked, _, _, _) = check_runs(&Setup {
        seeds: 1..=6,
        crashes: false,
        latency: (1_000_000, 1_000_000),
        clients: 5,
        requests: 30,
        grid: Some(10_000_000),
        idempotent: false,
        nemesis: 150_000_000,
    });
    assert!(acked > 600, "only {acked} partition entries were acknowledged");
}

/// Idempotent producers resend what a dropped connection or a crash left unanswered: nothing is stored twice, a
/// resent batch the partition holds is answered with its original offset, and producer ids stay unique.
#[test]
fn idempotent_producers_resend_without_duplicates() {
    let (acked, unanswered, _, resends) = check_runs(&Setup {
        seeds: 1..=8,
        crashes: true,
        latency: ClusterConfig::default().latency,
        clients: 3,
        requests: 40,
        grid: None,
        idempotent: true,
        nemesis: 40_000_000,
    });
    assert!(acked > 300, "only {acked} partition entries were acknowledged");
    assert_eq!(unanswered, 0, "an idempotent producer resends until it is answered");
    assert!(
        resends > 10,
        "only {resends} requests were resent: the faults did not bite"
    );
}
