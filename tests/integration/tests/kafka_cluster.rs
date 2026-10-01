//! Slice 8, item 6: the Kafka broker (`examples/kafka`) as a cluster of three and five brokers in the cluster
//! simulator, under crashes (some between a WAL append and its sync), downtime, splits, one-way cuts and dropped
//! connections.
//!
//! Rust clients (encoded and decoded by `kafka-protocol`) behave as Kafka's: they create a topic replicated on three
//! brokers through any broker, learn each partition's leader from Metadata, send each Produce to the partition's
//! leader, and on NOT_LEADER_OR_FOLLOWER, a timeout or a lost connection refresh their metadata and go on (an
//! idempotent producer sends the same batch again). An observer checks Raft's safety per group (`raft_safety`) all
//! along. After the faults stop and the cluster settles, every replica's rows must show:
//! - one log per partition: every replica holds the same batches, tiling its offsets to the same log end;
//! - every batch acknowledged with acks=all at the offset its answer gave, with the bytes sent; a batch acknowledged
//!   with acks=1 there or nowhere (a leader that fails before its followers copy it loses it, as in Kafka);
//! - only batches some client sent, each at most once (an idempotent producer's resends included).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_integration_tests::raft_safety::GroupSafety;
use blossom_node::durable::DurableSchema;
use blossom_sim::cluster::{Cluster, ClusterConfig, NoKvClients, StreamAction, StreamClient, StreamEvent};
use blossom_value::time::NodeId;
use blossom_value::value::IntValue;
use blossom_value::{BlobRef, Value};
use bytes::{Bytes, BytesMut};
use kafka_protocol::messages::create_topics_request::CreatableTopic;
use kafka_protocol::messages::metadata_request::MetadataRequestTopic;
use kafka_protocol::messages::produce_request::{PartitionProduceData, TopicProduceData};
use kafka_protocol::messages::{
    CreateTopicsRequest, CreateTopicsResponse, InitProducerIdRequest, InitProducerIdResponse, MetadataRequest,
    MetadataResponse, ProduceRequest, ProduceResponse, ProducerId, RequestHeader, ResponseHeader, TopicName,
};
use kafka_protocol::protocol::{Decodable, Encodable, HeaderVersion, StrBytes};
use kafka_protocol::records::{Compression, Record, RecordBatchEncoder, RecordEncodeOptions, TimestampType};

#[cfg(test)]
const TOPIC: &str = "logs";
#[cfg(test)]
const PARTITIONS: i32 = 3;
#[cfg(test)]
const REPLICATION: i16 = 3;
#[cfg(test)]
const NOT_LEADER_OR_FOLLOWER: i16 = 6;
#[cfg(test)]
const REQUEST_TIMED_OUT: i16 = 7;
#[cfg(test)]
const TOPIC_ALREADY_EXISTS: i16 = 36;
/// A client gives up on a request after this long (closing its connection), as Kafka's clients do
/// (`request.timeout.ms`).
#[cfg(test)]
const REQUEST_TIMEOUT: i64 = 1_500_000_000;

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

/// A magic-2 batch of `n` records whose values name the client and the batch. `pid` -1 is a producer without
/// idempotence; otherwise the batch carries the producer's id and epoch and starts at sequence `seq`.
#[cfg(test)]
fn batch(tag: &str, n: usize, pid: i64, epoch: i16, seq: i32) -> Vec<u8> {
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
        compression: Compression::None,
    };
    RecordBatchEncoder::encode(&mut buf, &records, &options).unwrap();
    buf.to_vec()
}

/// How many offsets a batch takes: `lastOffsetDelta + 1`.
#[cfg(test)]
fn offsets_of(b: &[u8]) -> i64 {
    i64::from(i32::from_be_bytes([b[23], b[24], b[25], b[26]])) + 1
}

/// A batch as a leader of `epoch` stores it at `base`: `baseOffset` and `partitionLeaderEpoch` written.
#[cfg(test)]
fn stamped(b: &[u8], base: i64, epoch: i32) -> Vec<u8> {
    let mut out = b.to_vec();
    out[0..8].copy_from_slice(&base.to_be_bytes());
    out[12..16].copy_from_slice(&epoch.to_be_bytes());
    out
}

#[cfg(test)]
fn framed<M: Encodable + HeaderVersion>(key: i16, version: i16, corr: i32, body: &M) -> Vec<u8> {
    let mut buf = BytesMut::new();
    RequestHeader::default()
        .with_request_api_key(key)
        .with_request_api_version(version)
        .with_correlation_id(corr)
        .with_client_id(Some(StrBytes::from_string("cluster".into())))
        .encode(&mut buf, M::header_version(version))
        .unwrap();
    body.encode(&mut buf, version).unwrap();
    let mut out = (buf.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&buf);
    out
}

/// One Produce: its acks, partition and batch.
#[cfg(test)]
#[derive(Clone, Debug)]
struct Produce {
    acks: i16,
    partition: i32,
    batch: Vec<u8>,
}

/// A produce's fate: its last answer (error, base offset), if any. An idempotent producer sends the same produce
/// again until an answer is final, so one entry stands for all its sends.
#[cfg(test)]
#[derive(Clone, Debug)]
struct Sent {
    produce: Produce,
    answer: Option<(i16, i64)>,
}

#[cfg(test)]
#[derive(Default)]
struct Shared {
    sent: Vec<Sent>,
    /// The producer ids InitProducerId handed out.
    pids: Vec<i64>,
    /// Requests sent again, and metadata refreshes after a leader moved.
    resends: usize,
    moved: usize,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
enum Want {
    Create,
    Metadata,
    InitPid,
    Produce(i32),
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
enum Pending {
    Create,
    Metadata,
    InitPid,
    Produce(usize),
}

/// A client following the partitions' leaders.
#[cfg(test)]
struct Client {
    id: u64,
    shared: Rc<RefCell<Shared>>,
    rng: Rng,
    /// Produces still to send.
    left: u32,
    /// Each broker's node, by broker id.
    brokers: BTreeMap<i32, NodeId>,
    /// Each partition's leader, by broker id, as last learned.
    leaders: BTreeMap<i32, i32>,
    created: bool,
    /// The metadata must be read again (a leader moved, or a request failed).
    stale: bool,
    /// The node connected to (or being connected to); whether it is open; whether this client closed it.
    conn: Option<NodeId>,
    open: bool,
    closing: bool,
    buf: Vec<u8>,
    corr: i32,
    batches: u64,
    pending: Option<(Pending, i64)>,
    idempotent: bool,
    pid: Option<(i64, i16)>,
    next_seq: BTreeMap<i32, i32>,
    /// A produce to send again (its index in the shared list).
    retry: Option<usize>,
    /// The partition of the next new produce.
    next_partition: i32,
}

#[cfg(test)]
impl Client {
    fn new(
        id: u64,
        seed: u64,
        shared: Rc<RefCell<Shared>>,
        brokers: BTreeMap<i32, NodeId>,
        left: u32,
        idem: bool,
    ) -> Self {
        let mut rng = Rng(seed * 1000 + id);
        let next_partition = rng.below(PARTITIONS as u64) as i32;
        Client {
            id,
            shared,
            rng,
            left,
            brokers,
            leaders: BTreeMap::new(),
            created: false,
            stale: true,
            conn: None,
            open: false,
            closing: false,
            buf: Vec::new(),
            corr: 0,
            batches: 0,
            pending: None,
            idempotent: idem,
            pid: None,
            next_seq: BTreeMap::new(),
            retry: None,
            next_partition,
        }
    }

    fn want(&self) -> Option<Want> {
        if !self.created {
            Some(Want::Create)
        } else if self.stale {
            Some(Want::Metadata)
        } else if self.idempotent && self.pid.is_none() {
            Some(Want::InitPid)
        } else if let Some(at) = self.retry {
            Some(Want::Produce(self.shared.borrow().sent[at].produce.partition))
        } else if self.left > 0 {
            Some(Want::Produce(self.next_partition))
        } else {
            None
        }
    }

    /// Where a request goes: a produce to its partition's leader, anything else to any broker.
    fn target(&mut self, w: Want) -> Option<NodeId> {
        match w {
            Want::Produce(p) => self.leaders.get(&p).and_then(|l| self.brokers.get(l)).copied(),
            _ => {
                let ids: Vec<NodeId> = self.brokers.values().copied().collect();
                ids.get(self.rng.below(ids.len() as u64) as usize).copied()
            }
        }
    }

    fn close(&mut self, a: &mut StreamAction) {
        if self.conn.is_some() && !self.closing {
            a.close = true;
            self.closing = true;
        }
    }

    /// Decides what to do next: connect where the next request goes, send it, or close a connection to the wrong
    /// broker first.
    fn step(&mut self, now: i64, a: &mut StreamAction) {
        if self.closing || self.pending.is_some() {
            return;
        }
        let Some(w) = self.want() else {
            self.close(a);
            return;
        };
        let target = match self.target(w) {
            Some(t) => t,
            None => {
                // No leader known for the partition: read the metadata first.
                self.stale = true;
                let w = Want::Metadata;
                match self.target(w) {
                    Some(t) => t,
                    None => return,
                }
            }
        };
        match self.conn {
            None => {
                self.conn = Some(target);
                self.open = false;
                a.connect = Some((target, Arc::from("kafka")));
            }
            Some(c) if !self.open => {
                let _ = c;
            }
            Some(c) if matches!(w, Want::Produce(_)) && !self.stale && c != target => self.close(a),
            Some(_) => self.issue(now, a),
        }
    }

    fn issue(&mut self, now: i64, a: &mut StreamAction) {
        let Some(w) = self.want() else { return };
        self.corr += 1;
        let deadline = now + REQUEST_TIMEOUT;
        match w {
            Want::Create => {
                let req = CreateTopicsRequest::default()
                    .with_topics(vec![
                        CreatableTopic::default()
                            .with_name(TopicName(StrBytes::from_string(TOPIC.into())))
                            .with_num_partitions(PARTITIONS)
                            .with_replication_factor(REPLICATION),
                    ])
                    .with_timeout_ms(1_000);
                a.send = framed(19, 7, self.corr, &req);
                self.pending = Some((Pending::Create, deadline));
            }
            Want::Metadata => {
                let req = MetadataRequest::default()
                    .with_topics(Some(vec![
                        MetadataRequestTopic::default().with_name(Some(TopicName(StrBytes::from_string(TOPIC.into())))),
                    ]))
                    .with_allow_auto_topic_creation(false);
                a.send = framed(3, 13, self.corr, &req);
                self.pending = Some((Pending::Metadata, deadline));
            }
            Want::InitPid => {
                let req = InitProducerIdRequest::default()
                    .with_transactional_id(None)
                    .with_transaction_timeout_ms(60_000)
                    .with_producer_id(ProducerId(-1))
                    .with_producer_epoch(-1);
                a.send = framed(22, 5, self.corr, &req);
                self.pending = Some((Pending::InitPid, deadline));
            }
            Want::Produce(p) => {
                let at = match self.retry.take() {
                    Some(at) => {
                        self.shared.borrow_mut().resends += 1;
                        at
                    }
                    None => {
                        self.left -= 1;
                        self.batches += 1;
                        let seq = self.next_seq.get(&p).copied().unwrap_or(0);
                        let (pid, epoch) = if self.idempotent {
                            self.pid.unwrap_or((-1, -1))
                        } else {
                            (-1, -1)
                        };
                        let n = 1 + self.rng.below(3) as usize;
                        let b = batch(&format!("c{}b{}", self.id, self.batches), n, pid, epoch, seq);
                        let acks = if self.idempotent {
                            -1
                        } else {
                            [-1i16, -1, 1][self.rng.below(3) as usize]
                        };
                        let mut sh = self.shared.borrow_mut();
                        sh.sent.push(Sent {
                            produce: Produce {
                                acks,
                                partition: p,
                                batch: b,
                            },
                            answer: None,
                        });
                        self.next_partition = self.rng.below(PARTITIONS as u64) as i32;
                        sh.sent.len() - 1
                    }
                };
                let produce = self.shared.borrow().sent[at].produce.clone();
                let req = ProduceRequest::default()
                    .with_acks(produce.acks)
                    .with_timeout_ms(1_000)
                    .with_topic_data(vec![
                        TopicProduceData::default()
                            .with_name(TopicName(StrBytes::from_string(TOPIC.into())))
                            .with_partition_data(vec![
                                PartitionProduceData::default()
                                    .with_index(produce.partition)
                                    .with_records(Some(Bytes::from(produce.batch.clone()))),
                            ]),
                    ]);
                a.send = framed(0, 12, self.corr, &req);
                self.pending = Some((Pending::Produce(at), deadline));
            }
        }
    }

    /// A request that failed or went unanswered: an idempotent producer sends the same produce again.
    fn failed(&mut self, p: Pending) {
        if let (true, Pending::Produce(at)) = (self.idempotent, p) {
            self.retry = Some(at);
        }
        self.stale = true;
    }

    fn answered(&mut self, p: Pending, mut body: Bytes, a: &mut StreamAction) -> Result<(), String> {
        match p {
            Pending::Create => {
                ResponseHeader::decode(&mut body, CreateTopicsResponse::header_version(7))
                    .map_err(|x| x.to_string())?;
                let r = CreateTopicsResponse::decode(&mut body, 7).map_err(|x| x.to_string())?;
                match r.topics.first().map(|t| t.error_code) {
                    Some(0 | TOPIC_ALREADY_EXISTS) => self.created = true,
                    // The controller did not apply it in time: ask again.
                    Some(REQUEST_TIMED_OUT) => {}
                    other => return Err(format!("creating the topic answered {other:?}: {r:?}")),
                }
            }
            Pending::Metadata => {
                ResponseHeader::decode(&mut body, MetadataResponse::header_version(13)).map_err(|x| x.to_string())?;
                let r = MetadataResponse::decode(&mut body, 13).map_err(|x| x.to_string())?;
                let known: BTreeSet<i32> = r.brokers.iter().map(|b| b.node_id.0).collect();
                if known != self.brokers.keys().copied().collect() {
                    return Err(format!("Metadata lists brokers {known:?}"));
                }
                if let Some(t) = r.topics.first()
                    && t.error_code == 0
                {
                    if t.partitions.len() != PARTITIONS as usize {
                        return Err(format!("the topic has {} partitions", t.partitions.len()));
                    }
                    for part in &t.partitions {
                        if part.replica_nodes.len() != REPLICATION as usize {
                            return Err(format!(
                                "partition {} has replicas {:?}",
                                part.partition_index, part.replica_nodes
                            ));
                        }
                        if part.leader_id.0 >= 0 {
                            if !part.replica_nodes.contains(&part.leader_id)
                                || !part.isr_nodes.contains(&part.leader_id)
                            {
                                return Err(format!("a leader that is no in-sync replica: {part:?}"));
                            }
                            self.leaders.insert(part.partition_index, part.leader_id.0);
                        } else {
                            self.leaders.remove(&part.partition_index);
                        }
                    }
                    self.stale = self.leaders.len() < PARTITIONS as usize;
                }
                // A broker that has not applied the creation yet does not know the topic: ask again.
            }
            Pending::InitPid => {
                ResponseHeader::decode(&mut body, InitProducerIdResponse::header_version(5))
                    .map_err(|x| x.to_string())?;
                let r = InitProducerIdResponse::decode(&mut body, 5).map_err(|x| x.to_string())?;
                if r.error_code != 0 || r.producer_id.0 < 0 || r.producer_epoch != 0 {
                    return Err(format!("InitProducerId answered {r:?}"));
                }
                self.pid = Some((r.producer_id.0, r.producer_epoch));
                self.shared.borrow_mut().pids.push(r.producer_id.0);
            }
            Pending::Produce(at) => {
                ResponseHeader::decode(&mut body, ProduceResponse::header_version(12)).map_err(|x| x.to_string())?;
                let r = ProduceResponse::decode(&mut body, 12).map_err(|x| x.to_string())?;
                let Some(pr) = r.responses.first().and_then(|t| t.partition_responses.first()) else {
                    return Err(format!("a Produce answer with no partition: {r:?}"));
                };
                let (code, base) = (pr.error_code, pr.base_offset);
                self.shared.borrow_mut().sent[at].answer = Some((code, base));
                match code {
                    0 => {
                        if self.idempotent {
                            let sh = self.shared.borrow();
                            let produce = &sh.sent[at].produce;
                            *self.next_seq.entry(produce.partition).or_insert(0) += offsets_of(&produce.batch) as i32;
                        }
                    }
                    NOT_LEADER_OR_FOLLOWER => {
                        self.shared.borrow_mut().moved += 1;
                        self.failed(Pending::Produce(at));
                        self.close(a);
                    }
                    REQUEST_TIMED_OUT => self.failed(Pending::Produce(at)),
                    other => return Err(format!("a produce answered {other}: {pr:?}")),
                }
            }
        }
        if !body.is_empty() {
            return Err(format!("{} bytes after a response", body.len()));
        }
        Ok(())
    }
}

#[cfg(test)]
impl StreamClient for Client {
    fn on(&mut self, now: i64, e: StreamEvent<'_>) -> Result<StreamAction, String> {
        let mut a = StreamAction::default();
        match e {
            StreamEvent::Wake => {
                if let Some((p, deadline)) = self.pending
                    && now >= deadline
                {
                    self.pending = None;
                    self.failed(p);
                    self.close(&mut a);
                }
                self.step(now, &mut a);
            }
            StreamEvent::Opened => {
                self.buf.clear();
                self.open = true;
                self.step(now, &mut a);
            }
            StreamEvent::Received(b) => {
                if self.closing {
                    return Ok(a);
                }
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
                let body = Bytes::copy_from_slice(&self.buf[4..]);
                self.buf.clear();
                let Some((p, _)) = self.pending.take() else {
                    return Err("a response with no request in flight".into());
                };
                self.answered(p, body, &mut a)?;
                if !a.close {
                    // Think a little before the next request; a failed one waits for things to settle.
                    a.wake = Some(
                        now + if self.stale {
                            30_000_000
                        } else {
                            self.rng.below(10_000_000) as i64
                        },
                    );
                }
            }
            StreamEvent::Closed(_) => {
                if let Some((p, _)) = self.pending.take() {
                    self.failed(p);
                }
                self.conn = None;
                self.open = false;
                self.closing = false;
                self.buf.clear();
                a.wake = Some(now + 20_000_000);
            }
        }
        if self.pending.is_some() && a.wake.is_none() {
            a.wake = Some(now + 100_000_000);
        }
        Ok(a)
    }
}

#[cfg(test)]
fn int(v: &Value) -> i64 {
    match v {
        Value::Int(IntValue::I32(x)) => i64::from(*x),
        Value::Int(IntValue::I64(x)) => *x,
        Value::Int(IntValue::U64(x)) => *x as i64,
        Value::Int(IntValue::U8(x)) => i64::from(*x),
        other => panic!("{other:?}"),
    }
}

/// How a run is set up.
#[cfg(test)]
struct Setup {
    brokers: u32,
    seeds: std::ops::RangeInclusive<u64>,
    faults: bool,
    clients: u64,
    requests: u32,
    idempotent: bool,
}

/// What the runs added up to.
#[cfg(test)]
#[derive(Debug, Default)]
struct Totals {
    acked_all: usize,
    acked_one: usize,
    lost_one: usize,
    ambiguous: usize,
    resends: usize,
    moved: usize,
    terms: u64,
}

/// One replica's partition log: (base, last, blob) per batch, and its log start and end.
#[cfg(test)]
type Replica = (Vec<(i64, i64, BlobRef)>, i64, i64);

#[cfg(test)]
fn check_runs(setup: &Setup) -> Totals {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/kafka/sim_cluster.bls");
    // The brokers, then the fixture's Blossom client (which only reads the metadata here).
    let mut nodes: Vec<NodeSpec> = (1..=setup.brokers)
        .map(|i| NodeSpec {
            name: format!("b{i}"),
            role: Some("Broker".to_owned()),
        })
        .collect();
    nodes.push(NodeSpec {
        name: "c1".to_owned(),
        role: Some("Client".to_owned()),
    });
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    let artifact: BlsArtifact = result.unwrap_or_else(|e| panic!("sim_cluster.bls: {e:?}")).0;
    let schema = DurableSchema::of(artifact.program.get());
    let rel = |n: &str| artifact.rel_named(n).unwrap();
    let brokers: BTreeMap<i32, NodeId> = (1..=setup.brokers).map(|i| (i as i32, NodeId(i - 1))).collect();
    let mut totals = Totals::default();
    for seed in setup.seeds.clone() {
        let cfg = ClusterConfig {
            seed,
            clients: 0,
            nemesis: if setup.faults { 300_000_000 } else { 0 },
            crashes: setup.faults,
            partitions: setup.faults,
            stream_drops: setup.faults,
            downtime: 400_000_000,
            duration: 12_000_000_000,
            chunk_max: 64 + (seed as usize % 5) * 200,
            externs: Arc::new(blossom_std_host::registry().unwrap()),
            ..ClusterConfig::default()
        };
        let mut cluster = Cluster::new(
            &artifact,
            &schema,
            blossom_value::Seed::from_u64(seed),
            blossom_integration_tests::kafka_brokers(&artifact).unwrap(),
            Box::new(NoKvClients),
            cfg,
        )
        .unwrap();
        let (safety, observer) = GroupSafety::of(&artifact).unwrap().shared();
        cluster.observe(observer);
        let shared = Rc::new(RefCell::new(Shared::default()));
        for c in 0..setup.clients {
            cluster.stream_client(Box::new(Client::new(
                c,
                seed,
                shared.clone(),
                brokers.clone(),
                setup.requests,
                setup.idempotent,
            )));
        }
        let fail = |cluster: &Cluster<'_>, what: &str| -> String {
            format!("seed {seed}: {what}\n{}", cluster.run_so_far().log.join("\n"))
        };
        cluster.run_until(8_000_000_000).unwrap();
        assert!(
            cluster.violation().is_none(),
            "{}",
            fail(&cluster, cluster.violation().unwrap_or(""))
        );
        // The faults stop; the cluster settles (every broker back up, every replica caught up).
        cluster.heal();
        cluster.step_until(12_000_000_000).unwrap();
        assert!(
            cluster.violation().is_none(),
            "{}",
            fail(&cluster, cluster.violation().unwrap_or(""))
        );
        let states: Vec<_> = brokers
            .values()
            .map(|n| {
                cluster
                    .state(*n)
                    .unwrap_or_else(|| panic!("{}", fail(&cluster, &format!("broker {n:?} is down"))))
            })
            .collect();

        // The topic, its placement, and the highest term any partition reached (the leader epochs a batch may carry).
        let tid = states[0]
            .rows(rel("mtopic"))
            .find(|r| r[0] == Value::Str(TOPIC.into()))
            .map(|r| r[1].clone())
            .unwrap_or_else(|| panic!("{}", fail(&cluster, "the topic does not exist")));
        let mut replicas: BTreeMap<i64, Vec<i32>> = BTreeMap::new();
        for r in states[0].rows(rel("massign")).filter(|r| r[0] == tid) {
            let Value::Vec(rs) = &r[2] else { panic!() };
            replicas.insert(int(&r[1]), rs.iter().map(|x| int(x) as i32).collect());
        }
        assert_eq!(replicas.len(), PARTITIONS as usize, "seed {seed}: {replicas:?}");
        let mut max_term = 0;
        for s in &states {
            for r in s.rows(rel("rterm")) {
                max_term = max_term.max(int(&r[1]));
            }
        }
        totals.terms += max_term as u64;
        let stored_as = |b: &[u8], base: i64, blob: BlobRef| {
            (0..max_term).any(|e| BlobRef::of(&stamped(b, base, e as i32)) == blob)
        };

        // One log per partition: every replica holds the same batches, tiling [start, end).
        let mut logs: BTreeMap<i64, Replica> = BTreeMap::new();
        for (p, rs) in &replicas {
            let mut seen: Option<(i32, Replica)> = None;
            for id in rs {
                let s = &states[(*id - 1) as usize];
                let mut batches: Vec<(i64, i64, BlobRef)> = s
                    .rows(rel("batch"))
                    .filter(|r| r[0] == tid && int(&r[1]) == *p)
                    .map(|r| {
                        let Value::Blob(b) = &r[5] else { panic!("{:?}", r[5]) };
                        (int(&r[2]), int(&r[3]), *b)
                    })
                    .collect();
                batches.sort();
                let one = |name: &str| {
                    s.rows(rel(name))
                        .find(|r| r[0] == tid && int(&r[1]) == *p)
                        .map(|r| int(&r[2]))
                        .unwrap_or(0)
                };
                let (start, end) = (one("log_start"), one("log_end"));
                let mut next = start;
                for (base, last, _) in &batches {
                    assert_eq!(
                        *base, next,
                        "seed {seed}: broker {id}'s partition {p} has a gap or an overlap at {base}"
                    );
                    next = last + 1;
                }
                assert_eq!(
                    next, end,
                    "seed {seed}: broker {id}'s partition {p} ends at {next}, its log end is {end}"
                );
                let mine = (batches, start, end);
                match &seen {
                    None => seen = Some((*id, mine)),
                    Some((other, theirs)) => assert!(
                        *theirs == mine,
                        "{}",
                        fail(
                            &cluster,
                            &format!(
                                "partition {p}: brokers {other} and {id} hold different logs: {:?} / {:?}",
                                (theirs.1, theirs.2, theirs.0.len()),
                                (mine.1, mine.2, mine.0.len())
                            )
                        )
                    ),
                }
            }
            if let Some((_, log)) = seen {
                logs.insert(*p, log);
            }
        }

        // What the clients were told, against the log.
        let sh = shared.borrow();
        let pids = &sh.pids;
        assert_eq!(
            pids.iter().collect::<BTreeSet<_>>().len(),
            pids.len(),
            "seed {seed}: a producer id twice: {pids:?}"
        );
        totals.resends += sh.resends;
        totals.moved += sh.moved;
        let at: BTreeMap<(i64, i64), BlobRef> = logs
            .iter()
            .flat_map(|(p, l)| l.0.iter().map(move |(base, _, blob)| ((*p, *base), *blob)))
            .collect();
        for s in &sh.sent {
            let p = i64::from(s.produce.partition);
            let b = &s.produce.batch;
            match s.answer {
                Some((0, base)) => {
                    let here = at.get(&(p, base)).is_some_and(|blob| stored_as(b, base, *blob));
                    if s.produce.acks == -1 {
                        totals.acked_all += 1;
                        assert!(
                            here,
                            "{}",
                            fail(
                                &cluster,
                                &format!("the acks=all batch acknowledged at {p}/{base} is not there")
                            )
                        );
                    } else {
                        totals.acked_one += 1;
                        let anywhere = at.iter().any(|((q, o), blob)| *q == p && stored_as(b, *o, *blob));
                        assert!(
                            here || !anywhere,
                            "{}",
                            fail(
                                &cluster,
                                &format!("the acks=1 batch acknowledged at {p}/{base} is elsewhere")
                            )
                        );
                        if !here {
                            totals.lost_one += 1;
                        }
                    }
                }
                _ => totals.ambiguous += 1,
            }
        }
        // Only batches sent, each once.
        for ((p, base), blob) in &at {
            let from: Vec<&Sent> = sh
                .sent
                .iter()
                .filter(|s| i64::from(s.produce.partition) == *p && stored_as(&s.produce.batch, *base, *blob))
                .collect();
            assert_eq!(
                from.len(),
                1,
                "{}",
                fail(
                    &cluster,
                    &format!("the batch at {p}/{base} matches {} sent batches", from.len())
                )
            );
        }
        let mut once = BTreeSet::new();
        for (_, blob) in at.iter() {
            assert!(once.insert(*blob), "seed {seed}: a batch is stored twice");
        }
        // Every partition made progress, and the observer saw it.
        let progress = safety.borrow().progress.clone();
        assert!(
            progress.len() > PARTITIONS as usize,
            "seed {seed}: groups committed: {progress:?}"
        );
    }
    totals
}

/// Three brokers, no faults: every produce is acknowledged and every replica holds the same log.
#[test]
fn three_brokers_replicate_every_partition() {
    let t = check_runs(&Setup {
        brokers: 3,
        seeds: 1..=2,
        faults: false,
        clients: 3,
        requests: 40,
        idempotent: false,
    });
    assert_eq!(t.ambiguous, 0, "{t:?}");
    assert!(t.acked_all + t.acked_one == 240, "{t:?}");
}

/// Three brokers under crashes, downtime, splits, one-way cuts and dropped connections.
#[test]
fn three_brokers_keep_acknowledged_records_under_faults() {
    let t = check_runs(&Setup {
        brokers: 3,
        seeds: 1..=6,
        faults: true,
        clients: 3,
        requests: 60,
        idempotent: false,
    });
    assert!(t.acked_all > 300, "{t:?}");
    assert!(t.moved > 0 && t.terms > 6 * 3, "the faults did not move leaders: {t:?}");
}

/// Five brokers, each partition on three of them, so most brokers lead or follow only some partitions; idempotent
/// producers resend across leader changes without duplicates.
#[test]
fn five_brokers_with_idempotent_producers_store_each_batch_once() {
    let t = check_runs(&Setup {
        brokers: 5,
        seeds: 1..=4,
        faults: true,
        clients: 3,
        requests: 50,
        idempotent: true,
    });
    assert!(t.acked_all > 300, "{t:?}");
    assert!(t.resends > 0, "{t:?}");
}
