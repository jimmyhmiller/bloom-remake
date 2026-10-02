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
use blossom_front::api::NodeSpec;
use blossom_integration_tests::raft_safety::GroupSafety;
use blossom_node::durable::DurableSchema;
use blossom_sim::cluster::{Cluster, ClusterConfig, CrashWrites, NoKvClients, StreamAction, StreamClient, StreamEvent};
use blossom_value::time::NodeId;
use blossom_value::value::IntValue;
use blossom_value::{BlobRef, Value};
use bytes::{Bytes, BytesMut};
use kafka_protocol::messages::alter_partition_reassignments_request::{ReassignablePartition, ReassignableTopic};
use kafka_protocol::messages::create_topics_request::{CreatableTopic, CreatableTopicConfig};
use kafka_protocol::messages::fetch_request::{FetchPartition, FetchTopic};
use kafka_protocol::messages::metadata_request::MetadataRequestTopic;
use kafka_protocol::messages::produce_request::{PartitionProduceData, TopicProduceData};
use kafka_protocol::messages::{
    AlterPartitionReassignmentsRequest, AlterPartitionReassignmentsResponse, BrokerId, CreateTopicsRequest,
    CreateTopicsResponse, FetchRequest, FetchResponse, InitProducerIdRequest, InitProducerIdResponse,
    ListPartitionReassignmentsRequest, ListPartitionReassignmentsResponse, MetadataRequest, MetadataResponse,
    ProduceRequest, ProduceResponse, ProducerId, RequestHeader, ResponseHeader, TopicName,
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
#[cfg(test)]
const NO_REASSIGNMENT_IN_PROGRESS: i16 = 85;
#[cfg(test)]
const FENCED_LEADER_EPOCH: i16 = 74;
#[cfg(test)]
const UNKNOWN_LEADER_EPOCH: i16 = 75;
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

/// What a client knows of a partition's leader: its broker id (-1 when it must find out) and the leader epoch, which
/// never goes back: Metadata with an older epoch is stale (a broker cut off from the rest still believes it leads),
/// and is ignored, as Kafka's clients do (KIP-320).
#[cfg(test)]
type Leaders = BTreeMap<i32, (i32, i32)>;

#[cfg(test)]
fn learn_leader(leaders: &mut Leaders, p: i32, leader: i32, epoch: i32) {
    if leader >= 0 && epoch >= leaders.get(&p).map_or(-1, |x| x.1) {
        leaders.insert(p, (leader, epoch));
    }
}

/// The leader refused (it no longer leads that epoch, or its epoch is newer than ours): the next one known must
/// have a newer epoch than the one that refused.
#[cfg(test)]
fn leader_refused(leaders: &mut Leaders, p: i32) {
    if let Some(x) = leaders.get_mut(&p) {
        *x = (-1, x.1 + 1);
    }
}

#[cfg(test)]
fn every_leader_known(leaders: &Leaders) -> bool {
    (0..PARTITIONS).all(|p| leaders.get(&p).is_some_and(|x| x.0 >= 0))
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
    /// What the readers fetched: (partition, base offset, the batch as stored), and fetches refused for a leader
    /// epoch the reader had wrong.
    reads: Vec<(i64, i64, BlobRef)>,
    fenced: usize,
    /// The reader's last state, for a failure's report: its next offset per partition and what it knows of leaders.
    reader_state: String,
    /// When the last produce was answered and the reader last read (virtual nanoseconds), for a failure's report.
    last_answer: i64,
    last_read: i64,
    /// Fetches right after an `acks=1` batch (see `Client::probing`).
    probes: usize,
    /// The replicas the admin client last reassigned each partition to, once that reassignment was done.
    reassigned: BTreeMap<i32, Vec<i32>>,
    /// Reassignment waves finished.
    waves: usize,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
enum Want {
    Create,
    Metadata,
    InitPid,
    Produce(i32),
    Probe(i32),
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
enum Pending {
    Create,
    Metadata,
    InitPid,
    Produce(usize),
    Probe,
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
    leaders: Leaders,
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
    /// With `probing`, after each `acks=1` acknowledgement the client fetches right after its batch at the leader
    /// (`probe`): past the high watermark until the followers copy it, but within the leader's log, which Kafka
    /// answers with no records, never OFFSET_OUT_OF_RANGE. (Only without faults: a failover may lose the batch.)
    probing: bool,
    probe: Option<(i32, i64)>,
    topic_id: Option<[u8; 16]>,
    /// The topic's configurations, when this client creates it.
    topic_configs: Vec<(&'static str, &'static str)>,
    /// The time of the event being handled.
    now: i64,
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
            probing: false,
            probe: None,
            topic_id: None,
            topic_configs: Vec::new(),
            now: 0,
        }
    }

    fn want(&self) -> Option<Want> {
        if !self.created {
            Some(Want::Create)
        } else if self.stale {
            Some(Want::Metadata)
        } else if self.idempotent && self.pid.is_none() {
            Some(Want::InitPid)
        } else if let Some((p, _)) = self.probe {
            Some(Want::Probe(p))
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
            Want::Produce(p) | Want::Probe(p) => self.leaders.get(&p).and_then(|l| self.brokers.get(&l.0)).copied(),
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
            Some(c) if matches!(w, Want::Produce(_) | Want::Probe(_)) && !self.stale && c != target => self.close(a),
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
                            .with_replication_factor(REPLICATION)
                            .with_configs(
                                self.topic_configs
                                    .iter()
                                    .map(|(k, v)| {
                                        CreatableTopicConfig::default()
                                            .with_name(StrBytes::from_static_str(k))
                                            .with_value(Some(StrBytes::from_static_str(v)))
                                    })
                                    .collect(),
                            ),
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
            Want::Probe(p) => {
                let off = self.probe.take().map_or(0, |x| x.1);
                let req = FetchRequest::default()
                    .with_max_bytes(1 << 16)
                    .with_min_bytes(0)
                    .with_max_wait_ms(0)
                    .with_session_epoch(-1)
                    .with_topics(vec![
                        FetchTopic::default()
                            .with_topic_id(uuid::Uuid::from_bytes(self.topic_id.unwrap_or([0; 16])))
                            .with_partitions(vec![
                                FetchPartition::default()
                                    .with_partition(p)
                                    .with_current_leader_epoch(-1)
                                    .with_fetch_offset(off)
                                    .with_partition_max_bytes(1 << 16),
                            ]),
                    ]);
                a.send = framed(1, 17, self.corr, &req);
                self.pending = Some((Pending::Probe, deadline));
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
                    self.topic_id = Some(*t.topic_id.as_bytes());
                    if t.partitions.len() != PARTITIONS as usize {
                        return Err(format!("the topic has {} partitions", t.partitions.len()));
                    }
                    for part in &t.partitions {
                        // During a reassignment the replicas shown are the target's and those it removes.
                        if part.replica_nodes.len() < REPLICATION as usize {
                            return Err(format!(
                                "partition {} has replicas {:?}",
                                part.partition_index, part.replica_nodes
                            ));
                        }
                        if part.leader_id.0 >= 0
                            && (!part.replica_nodes.contains(&part.leader_id)
                                || !part.isr_nodes.contains(&part.leader_id))
                        {
                            return Err(format!("a leader that is no in-sync replica: {part:?}"));
                        }
                        learn_leader(
                            &mut self.leaders,
                            part.partition_index,
                            part.leader_id.0,
                            part.leader_epoch,
                        );
                    }
                    self.stale = !every_leader_known(&self.leaders);
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
            Pending::Probe => {
                ResponseHeader::decode(&mut body, FetchResponse::header_version(17)).map_err(|x| x.to_string())?;
                let r = FetchResponse::decode(&mut body, 17).map_err(|x| x.to_string())?;
                let code = r
                    .responses
                    .first()
                    .and_then(|t| t.partitions.first())
                    .map(|x| x.error_code);
                if code != Some(0) {
                    return Err(format!("a fetch right after an acks=1 batch answered {code:?}: {r:?}"));
                }
                self.shared.borrow_mut().probes += 1;
            }
            Pending::Produce(at) => {
                ResponseHeader::decode(&mut body, ProduceResponse::header_version(12)).map_err(|x| x.to_string())?;
                let r = ProduceResponse::decode(&mut body, 12).map_err(|x| x.to_string())?;
                let Some(pr) = r.responses.first().and_then(|t| t.partition_responses.first()) else {
                    return Err(format!("a Produce answer with no partition: {r:?}"));
                };
                let (code, base) = (pr.error_code, pr.base_offset);
                {
                    let mut sh = self.shared.borrow_mut();
                    sh.sent[at].answer = Some((code, base));
                    sh.last_answer = self.now;
                }
                match code {
                    0 => {
                        let (acks, partition, n) = {
                            let sh = self.shared.borrow();
                            let x = &sh.sent[at].produce;
                            (x.acks, x.partition, offsets_of(&x.batch))
                        };
                        if self.probing && acks == 1 {
                            self.probe = Some((partition, base + n));
                        }
                        if self.idempotent {
                            let sh = self.shared.borrow();
                            let produce = &sh.sent[at].produce;
                            *self.next_seq.entry(produce.partition).or_insert(0) += offsets_of(&produce.batch) as i32;
                        }
                    }
                    NOT_LEADER_OR_FOLLOWER => {
                        self.shared.borrow_mut().moved += 1;
                        let p = self.shared.borrow().sent[at].produce.partition;
                        leader_refused(&mut self.leaders, p);
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
        self.now = now;
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
                // A connection lost (or refused: the broker is down) sends the client back to the metadata, as
                // Kafka's clients refresh it when a leader's connection fails.
                self.stale = true;
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

/// A consumer: it reads every partition from offset 0 with Fetch at the partition's leader, as a Kafka consumer does
/// (with the leader epoch Metadata gave, so a stale leader is fenced), across leader changes. What it reads must come
/// in order, with no gap, and be in the final log at the same offset (it read only what was committed).
#[cfg(test)]
struct Reader {
    shared: Rc<RefCell<Shared>>,
    rng: Rng,
    brokers: BTreeMap<i32, NodeId>,
    /// Each partition's leader, and the topic id, as last learned.
    leaders: Leaders,
    topic_id: Option<[u8; 16]>,
    stale: bool,
    conn: Option<NodeId>,
    open: bool,
    closing: bool,
    buf: Vec<u8>,
    corr: i32,
    pending: Option<(bool, i64)>,
    /// The partition read next, and each partition's next offset.
    part: i32,
    next: BTreeMap<i32, i64>,
    /// Stop reading at this time.
    until: i64,
    now: i64,
}

#[cfg(test)]
impl Reader {
    fn new(seed: u64, shared: Rc<RefCell<Shared>>, brokers: BTreeMap<i32, NodeId>, until: i64) -> Self {
        Reader {
            shared,
            rng: Rng(seed * 7919 + 13),
            brokers,
            leaders: BTreeMap::new(),
            topic_id: None,
            stale: true,
            conn: None,
            open: false,
            closing: false,
            buf: Vec::new(),
            corr: 0,
            pending: None,
            part: 0,
            next: BTreeMap::new(),
            until,
            now: 0,
        }
    }

    fn close(&mut self, a: &mut StreamAction) {
        if self.conn.is_some() && !self.closing {
            a.close = true;
            self.closing = true;
        }
    }

    fn step(&mut self, now: i64, a: &mut StreamAction) {
        if self.closing || self.pending.is_some() {
            return;
        }
        if now >= self.until {
            self.close(a);
            return;
        }
        let target = if self.stale {
            let ids: Vec<NodeId> = self.brokers.values().copied().collect();
            ids.get(self.rng.below(ids.len() as u64) as usize).copied()
        } else {
            self.leaders
                .get(&self.part)
                .and_then(|(l, _)| self.brokers.get(l))
                .copied()
        };
        let Some(target) = target else {
            self.stale = true;
            return;
        };
        match self.conn {
            None => {
                self.conn = Some(target);
                self.open = false;
                a.connect = Some((target, Arc::from("kafka")));
            }
            Some(_) if !self.open => {}
            Some(c) if !self.stale && c != target => self.close(a),
            Some(_) => {
                self.corr += 1;
                if self.stale {
                    let req = MetadataRequest::default()
                        .with_topics(Some(vec![
                            MetadataRequestTopic::default()
                                .with_name(Some(TopicName(StrBytes::from_string(TOPIC.into())))),
                        ]))
                        .with_allow_auto_topic_creation(false);
                    a.send = framed(3, 13, self.corr, &req);
                    self.pending = Some((true, now + REQUEST_TIMEOUT));
                } else {
                    let epoch = self.leaders.get(&self.part).map_or(-1, |x| x.1);
                    let req = FetchRequest::default()
                        .with_max_bytes(1 << 16)
                        .with_min_bytes(1)
                        .with_max_wait_ms(50)
                        .with_session_epoch(-1)
                        .with_topics(vec![
                            FetchTopic::default()
                                .with_topic_id(uuid::Uuid::from_bytes(self.topic_id.unwrap_or([0; 16])))
                                .with_partitions(vec![
                                    FetchPartition::default()
                                        .with_partition(self.part)
                                        .with_current_leader_epoch(epoch)
                                        .with_fetch_offset(self.next.get(&self.part).copied().unwrap_or(0))
                                        .with_partition_max_bytes(1 << 16),
                                ]),
                        ]);
                    a.send = framed(1, 17, self.corr, &req);
                    self.pending = Some((false, now + REQUEST_TIMEOUT));
                }
            }
        }
    }

    fn answered(&mut self, metadata: bool, mut body: Bytes, a: &mut StreamAction) -> Result<(), String> {
        if metadata {
            ResponseHeader::decode(&mut body, MetadataResponse::header_version(13)).map_err(|x| x.to_string())?;
            let r = MetadataResponse::decode(&mut body, 13).map_err(|x| x.to_string())?;
            if let Some(t) = r.topics.first()
                && t.error_code == 0
            {
                self.topic_id = Some(*t.topic_id.as_bytes());
                for part in &t.partitions {
                    learn_leader(
                        &mut self.leaders,
                        part.partition_index,
                        part.leader_id.0,
                        part.leader_epoch,
                    );
                }
                self.stale = !every_leader_known(&self.leaders);
            }
            return Ok(());
        }
        ResponseHeader::decode(&mut body, FetchResponse::header_version(17)).map_err(|x| x.to_string())?;
        let r = FetchResponse::decode(&mut body, 17).map_err(|x| x.to_string())?;
        let Some(pr) = r.responses.first().and_then(|t| t.partitions.first()) else {
            return Err(format!("a Fetch answer with no partition: {r:?}"));
        };
        match pr.error_code {
            0 => {
                let records = pr.records.clone().unwrap_or_default();
                let mut at = 0usize;
                let mut sh = self.shared.borrow_mut();
                // Whole batches only (a trimmed last batch is read again from its start).
                while records.len() >= at + 12 {
                    let len = u32::from_be_bytes([records[at + 8], records[at + 9], records[at + 10], records[at + 11]])
                        as usize;
                    let Some(b) = records.get(at..at + 12 + len) else { break };
                    let base = i64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
                    let next = self.next.get(&self.part).copied().unwrap_or(0);
                    if base != next {
                        return Err(format!(
                            "partition {}: read a batch at {base}, expected {next}",
                            self.part
                        ));
                    }
                    sh.reads.push((i64::from(self.part), base, BlobRef::of(b)));
                    self.next.insert(self.part, base + offsets_of(b));
                    sh.reader_state = format!("next {:?} leaders {:?}", self.next, self.leaders);
                    sh.last_read = self.now;
                    at += 12 + len;
                }
                if at == 0 {
                    self.part = (self.part + 1) % PARTITIONS;
                }
            }
            NOT_LEADER_OR_FOLLOWER | FENCED_LEADER_EPOCH => {
                if pr.error_code == FENCED_LEADER_EPOCH {
                    self.shared.borrow_mut().fenced += 1;
                }
                leader_refused(&mut self.leaders, self.part);
                self.stale = true;
                self.close(a);
            }
            // The broker is behind the epoch this reader knows: ask again (the broker will learn, or another lead).
            UNKNOWN_LEADER_EPOCH => {
                self.stale = true;
                self.close(a);
            }
            other => return Err(format!("a fetch answered {other}: {pr:?}")),
        }
        Ok(())
    }
}

#[cfg(test)]
impl StreamClient for Reader {
    fn on(&mut self, now: i64, e: StreamEvent<'_>) -> Result<StreamAction, String> {
        self.now = now;
        let mut a = StreamAction::default();
        match e {
            StreamEvent::Wake => {
                if let Some((_, deadline)) = self.pending
                    && now >= deadline
                {
                    self.pending = None;
                    self.stale = true;
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
                let Some((metadata, _)) = self.pending.take() else {
                    return Err("a response with no request in flight".into());
                };
                self.answered(metadata, body, &mut a)?;
                if !a.close {
                    a.wake = Some(now + if self.stale { 30_000_000 } else { 1_000_000 });
                }
            }
            StreamEvent::Closed(_) => {
                // A close this reader asked for, to move to another partition's leader, keeps what it knows of the
                // leaders (it once asked a random broker for metadata, and closed again unless that broker led the
                // partition: a run could spend its last half second so); any other close makes it ask again.
                if !self.closing || self.pending.is_some() {
                    self.stale = true;
                }
                self.pending = None;
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

/// An administrator: once the topic exists, it reassigns every partition to new replicas (AlterPartitionReassignments
/// at any broker), then asks ListPartitionReassignments until none is in progress, for each wave in turn (item 5).
#[cfg(test)]
struct Admin {
    shared: Rc<RefCell<Shared>>,
    rng: Rng,
    brokers: BTreeMap<i32, NodeId>,
    /// Each wave: per partition, its target replicas.
    waves: Vec<Vec<(i32, Vec<i32>)>>,
    wave: usize,
    /// 0 waiting to start; 1 asking for the wave; 2 polling until it is done; 3 finished.
    phase: u8,
    /// When it may start, and polls in a row that found nothing in progress.
    start: i64,
    quiet: u32,
    conn: Option<NodeId>,
    open: bool,
    buf: Vec<u8>,
    corr: i32,
    pending: Option<i64>,
}

#[cfg(test)]
impl Admin {
    fn send(&mut self, now: i64, a: &mut StreamAction) {
        self.corr += 1;
        a.send = if self.phase == 1 {
            let mut by_topic = Vec::new();
            for (p, target) in &self.waves[self.wave] {
                by_topic.push(
                    ReassignablePartition::default()
                        .with_partition_index(*p)
                        .with_replicas(Some(target.iter().map(|x| BrokerId(*x)).collect())),
                );
            }
            let req = AlterPartitionReassignmentsRequest::default()
                .with_timeout_ms(1_000)
                .with_allow_replication_factor_change(true)
                .with_topics(vec![
                    ReassignableTopic::default()
                        .with_name(TopicName(StrBytes::from_string(TOPIC.into())))
                        .with_partitions(by_topic),
                ]);
            framed(45, 1, self.corr, &req)
        } else {
            framed(
                46,
                0,
                self.corr,
                &ListPartitionReassignmentsRequest::default().with_timeout_ms(1_000),
            )
        };
        self.pending = Some(now + REQUEST_TIMEOUT);
    }

    fn answered(&mut self, now: i64, mut body: Bytes) -> Result<(), String> {
        if self.phase == 1 {
            ResponseHeader::decode(&mut body, AlterPartitionReassignmentsResponse::header_version(1))
                .map_err(|x| x.to_string())?;
            let r = AlterPartitionReassignmentsResponse::decode(&mut body, 1).map_err(|x| x.to_string())?;
            let codes: Vec<i16> = r
                .responses
                .iter()
                .flat_map(|t| t.partitions.iter().map(|p| p.error_code))
                .collect();
            if r.error_code != 0 || codes.len() != self.waves[self.wave].len() {
                return Err(format!("a reassignment answered {r:?}"));
            }
            if codes.iter().all(|c| *c == 0) {
                self.phase = 2;
                self.quiet = 0;
            } else if !codes.iter().all(|c| *c == 0 || *c == REQUEST_TIMED_OUT) {
                return Err(format!("a reassignment was refused: {r:?}"));
            }
            // Timed out (the controller was unavailable): asked again (the same targets).
        } else {
            ResponseHeader::decode(&mut body, ListPartitionReassignmentsResponse::header_version(0))
                .map_err(|x| x.to_string())?;
            let r = ListPartitionReassignmentsResponse::decode(&mut body, 0).map_err(|x| x.to_string())?;
            if r.error_code != 0 {
                return Err(format!("listing reassignments answered {r:?}"));
            }
            for t in &r.topics {
                for p in &t.partitions {
                    let want: Vec<i32> = self.waves[self.wave]
                        .iter()
                        .find(|x| x.0 == p.partition_index)
                        .map(|x| x.1.clone())
                        .unwrap_or_default();
                    let adding: Vec<i32> = p.adding_replicas.iter().map(|b| b.0).collect();
                    if !adding.iter().all(|b| want.contains(b)) {
                        return Err(format!(
                            "partition {} adds {adding:?}, not in its target {want:?}",
                            p.partition_index
                        ));
                    }
                }
            }
            // Three answers in a row with nothing in progress (from brokers that may lag the controller): done.
            self.quiet = if r.topics.is_empty() { self.quiet + 1 } else { 0 };
            if self.quiet >= 3 {
                let mut sh = self.shared.borrow_mut();
                for (p, target) in &self.waves[self.wave] {
                    sh.reassigned.insert(*p, target.clone());
                }
                sh.waves += 1;
                self.wave += 1;
                self.phase = if self.wave < self.waves.len() { 1 } else { 3 };
                self.start = now + 300_000_000;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
impl StreamClient for Admin {
    fn on(&mut self, now: i64, e: StreamEvent<'_>) -> Result<StreamAction, String> {
        let mut a = StreamAction::default();
        match e {
            StreamEvent::Wake => {
                if self.pending.is_some_and(|d| now >= d) {
                    self.pending = None;
                    a.close = true;
                } else if self.phase == 0 && now >= self.start {
                    self.phase = 1;
                }
                if self.phase != 0 && self.phase != 3 && self.pending.is_none() && now >= self.start && !a.close {
                    match self.conn {
                        None => {
                            let ids: Vec<NodeId> = self.brokers.values().copied().collect();
                            let n = ids[self.rng.below(ids.len() as u64) as usize];
                            self.conn = Some(n);
                            self.open = false;
                            a.connect = Some((n, Arc::from("kafka")));
                        }
                        Some(_) if self.open => self.send(now, &mut a),
                        Some(_) => {}
                    }
                }
                if self.phase != 3 {
                    a.wake = Some(now + 50_000_000);
                }
            }
            StreamEvent::Opened => {
                self.open = true;
                self.buf.clear();
                if self.phase != 0 && self.phase != 3 && self.pending.is_none() {
                    self.send(now, &mut a);
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
                let body = Bytes::copy_from_slice(&self.buf[4..4 + n]);
                self.buf.drain(..4 + n);
                if self.pending.take().is_none() {
                    return Err("a response with no request in flight".into());
                }
                self.answered(now, body)?;
                // Each poll goes to another broker, a while later.
                a.close = true;
                a.wake = Some(now + 100_000_000);
            }
            StreamEvent::Closed(_) => {
                self.pending = None;
                self.conn = None;
                self.open = false;
                self.buf.clear();
                a.wake = Some(now + 20_000_000);
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
    /// Reassignment waves the admin client runs (none: no admin client).
    waves: Vec<Vec<(i32, Vec<i32>)>>,
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
    read: usize,
    fenced: usize,
    probes: usize,
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
    // A follower out of sync leaves the ISR after half a second here (Kafka's default is 30 s): acks=all waits for
    // every in-sync replica, and a run lasts seconds.
    let params = [(
        "REPLICA_LAG_MAX".to_owned(),
        blossom_front::api::ParamBinding::Text("500ms".into()),
    )]
    .into_iter()
    .collect();
    let (result, _) = blossom_driver::bls::compile_file_with(path.to_str().unwrap(), &nodes, &params);
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
            duration: 14_000_000_000,
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
        cluster.stream_client(Box::new(Reader::new(
            seed,
            shared.clone(),
            brokers.clone(),
            13_500_000_000,
        )));
        if !setup.waves.is_empty() {
            cluster.stream_client(Box::new(Admin {
                shared: shared.clone(),
                rng: Rng(seed * 31 + 7),
                brokers: brokers.clone(),
                waves: setup.waves.clone(),
                wave: 0,
                phase: 0,
                start: 800_000_000,
                quiet: 0,
                conn: None,
                open: false,
                buf: Vec::new(),
                corr: 0,
                pending: None,
            }));
        }
        for c in 0..setup.clients {
            let mut client = Client::new(
                c,
                seed,
                shared.clone(),
                brokers.clone(),
                setup.requests,
                setup.idempotent,
            );
            client.probing = !setup.faults;
            cluster.stream_client(Box::new(client));
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
        cluster.step_until(14_000_000_000).unwrap();
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
        // Every wave finished, and each partition is on the replicas the last one moved it to; no reassignment is
        // left, and the brokers it left hold nothing of it.
        if !setup.waves.is_empty() {
            let sh = shared.borrow();
            assert_eq!(
                sh.waves,
                setup.waves.len(),
                "{}",
                fail(&cluster, "the reassignments did not finish")
            );
            for (p, target) in &sh.reassigned {
                assert_eq!(
                    replicas.get(&i64::from(*p)),
                    Some(target),
                    "seed {seed}: partition {p}'s replicas"
                );
            }
            for s in &states {
                assert_eq!(
                    s.rows(rel("mreassign")).count(),
                    0,
                    "seed {seed}: a reassignment is left"
                );
            }
            for (p, rs) in &replicas {
                for (id, n) in &brokers {
                    if rs.contains(id) {
                        continue;
                    }
                    let s = &states[n.0 as usize];
                    let held = s
                        .rows(rel("log_start"))
                        .filter(|r| r[0] == tid && int(&r[1]) == *p)
                        .count()
                        + s.rows(rel("batch")).filter(|r| r[0] == tid && int(&r[1]) == *p).count()
                        + s.rows(rel("rlog"))
                            .filter(|r| matches!(&r[0], Value::Tuple(g) if g[0] == tid && int(&g[1]) == *p))
                            .count();
                    assert_eq!(
                        held, 0,
                        "seed {seed}: broker {id} still holds rows of partition {p} it left"
                    );
                }
            }
        }
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
        // What the reader read is in the log, where it read it.
        for (p, base, blob) in &sh.reads {
            assert_eq!(
                at.get(&(*p, *base)),
                Some(blob),
                "{}",
                fail(
                    &cluster,
                    &format!("the reader read a batch at {p}/{base} the log does not hold there")
                )
            );
        }
        // Reading on after the faults stopped, it read every partition to its end.
        let ends: BTreeMap<i64, i64> = logs.iter().map(|(p, l)| (*p, l.2)).collect();
        assert_eq!(
            sh.reads.len(),
            at.len(),
            "{}",
            fail(
                &cluster,
                &format!(
                    "the reader did not read every batch: {} ends {ends:?}; last answer at {} ns, last read at {} ns",
                    sh.reader_state, sh.last_answer, sh.last_read
                )
            )
        );
        totals.read += sh.reads.len();
        totals.probes += sh.probes;
        totals.fenced += sh.fenced;
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
        waves: Vec::new(),
        clients: 3,
        requests: 40,
        idempotent: false,
    });
    assert_eq!(t.ambiguous, 0, "{t:?}");
    assert!(t.probes > 30, "{t:?}");
    assert!(t.acked_all + t.acked_one == 240, "{t:?}");
}

/// Three brokers under crashes, downtime, splits, one-way cuts and dropped connections.
#[test]
fn three_brokers_keep_acknowledged_records_under_faults() {
    let t = check_runs(&Setup {
        brokers: 3,
        seeds: 1..=6,
        faults: true,
        waves: Vec::new(),
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
        waves: Vec::new(),
        clients: 3,
        requests: 50,
        idempotent: true,
    });
    assert!(t.acked_all > 300, "{t:?}");
    assert!(t.resends > 0, "{t:?}");
}

/// D12: retention compacts the replication log, so a follower that was down while its leader deleted old segments
/// cannot be sent the entries it lacks: it gets the leader's snapshot point, empties its partition log, starts again
/// at the leader's log start and catches up from there (as a Kafka follower that fetched below its leader's log start
/// does). Afterwards it holds what the leader holds from its new start, to the same end.
#[test]
fn a_follower_behind_its_leaders_log_start_catches_up_from_a_snapshot() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/kafka/sim_cluster.bls");
    let mut nodes: Vec<NodeSpec> = (1..=3)
        .map(|i| NodeSpec {
            name: format!("b{i}"),
            role: Some("Broker".to_owned()),
        })
        .collect();
    nodes.push(NodeSpec {
        name: "c1".to_owned(),
        role: Some("Client".to_owned()),
    });
    let params = [
        (
            "RETENTION_CHECK".to_owned(),
            blossom_front::api::ParamBinding::Text("50ms".into()),
        ),
        (
            "REPLICA_LAG_MAX".to_owned(),
            blossom_front::api::ParamBinding::Text("500ms".into()),
        ),
    ]
    .into_iter()
    .collect();
    let (result, _) = blossom_driver::bls::compile_file_with(path.to_str().unwrap(), &nodes, &params);
    let artifact: BlsArtifact = result.unwrap_or_else(|e| panic!("sim_cluster.bls: {e:?}")).0;
    let schema = DurableSchema::of(artifact.program.get());
    let rel = |n: &str| artifact.rel_named(n).unwrap();
    let brokers: BTreeMap<i32, NodeId> = (1..=3).map(|i| (i, NodeId(i as u32 - 1))).collect();
    for seed in 1..=2u64 {
        let cfg = ClusterConfig {
            seed,
            clients: 0,
            duration: 12_000_000_000,
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
        let (_safety, observer) = GroupSafety::of(&artifact).unwrap().shared();
        cluster.observe(observer);
        let shared = Rc::new(RefCell::new(Shared::default()));
        for c in 0..2 {
            let mut client = Client::new(c, seed, shared.clone(), brokers.clone(), 120, false);
            client.topic_configs = vec![("retention.bytes", "1000"), ("segment.bytes", "300")];
            cluster.stream_client(Box::new(client));
        }
        let fail = |cluster: &Cluster<'_>, what: &str| -> String {
            format!("seed {seed}: {what}\n{}", cluster.run_so_far().log.join("\n"))
        };
        // Broker 3 goes down once the topic exists, and stays down while the others produce and delete.
        cluster.run_until(300_000_000).unwrap();
        let lagging = NodeId(2);
        // Per partition group: the last index of the returning broker's log when it went down (at most this: a crash
        // may lose its unsynced writes).
        let group_of = |r: &[Value]| match &r[0] {
            Value::Tuple(g) if g.len() == 2 && int(&g[1]) >= 0 => Some(r[0].clone()),
            _ => None,
        };
        let mut held: BTreeMap<Value, i64> = BTreeMap::new();
        for r in cluster.state(lagging).unwrap().rows(rel("rlog")) {
            if let Some(g) = group_of(r) {
                let e = held.entry(g).or_insert(0);
                *e = (*e).max(int(&r[1]));
            }
        }
        cluster.crash(lagging, CrashWrites::Random).unwrap();
        cluster.run_until(3_000_000_000).unwrap();
        assert!(
            cluster.violation().is_none(),
            "{}",
            fail(&cluster, cluster.violation().unwrap_or(""))
        );
        // Both others compacted some partition past where it stopped: whichever leads it can only send it a
        // snapshot point.
        let mut least: BTreeMap<Value, i64> = BTreeMap::new();
        let survivors: Vec<NodeId> = brokers.values().copied().filter(|n| *n != lagging).collect();
        for (k, n) in survivors.iter().enumerate() {
            let points: BTreeMap<Value, i64> = cluster
                .state(*n)
                .unwrap()
                .rows(rel("rsnap"))
                .map(|r| (r[0].clone(), int(&r[1])))
                .collect();
            if k == 0 {
                least = points;
            } else {
                least = least
                    .into_iter()
                    .filter_map(|(g, i)| points.get(&g).map(|j| (g, i.min(*j))))
                    .collect();
            }
        }
        let past = least
            .iter()
            .filter(|(g, i)| **i > held.get(*g).copied().unwrap_or(0))
            .count();
        assert!(
            past > 0,
            "{}",
            fail(&cluster, "no leader compacted past the stopped broker")
        );
        cluster.restart(lagging).unwrap();
        cluster.run_until(8_000_000_000).unwrap();
        cluster.step_until(9_000_000_000).unwrap();
        assert!(
            cluster.violation().is_none(),
            "{}",
            fail(&cluster, cluster.violation().unwrap_or(""))
        );
        assert!(
            shared.borrow().sent.len() == 240,
            "{}",
            fail(
                &cluster,
                &format!("the producers sent {} of 240", shared.borrow().sent.len())
            )
        );
        let states: Vec<_> = brokers.values().map(|n| cluster.state(*n).unwrap()).collect();
        let tid = states[0]
            .rows(rel("mtopic"))
            .find(|r| r[0] == Value::Str(TOPIC.into()))
            .map(|r| r[1].clone())
            .unwrap();
        let in_topic = |r: &[Value]| r[0] == tid;
        let mut snapshots = 0;
        // Each partition's replicas' log starts and batches, for the acknowledged batches' check.
        let mut held_logs: Vec<(i64, i64, BTreeMap<i64, BlobRef>)> = Vec::new();
        for p in 0..i64::from(PARTITIONS) {
            // Each replica's (log start, log end, batches by base).
            let logs: Vec<(i64, i64, BTreeMap<i64, BlobRef>)> = states
                .iter()
                .map(|s| {
                    let one = |name: &str| {
                        s.rows(rel(name))
                            .find(|r| in_topic(r) && int(&r[1]) == p)
                            .map(|r| int(&r[2]))
                            .unwrap_or(-1)
                    };
                    let batches = s
                        .rows(rel("batch"))
                        .filter(|r| in_topic(r) && int(&r[1]) == p)
                        .map(|r| {
                            let Value::Blob(b) = &r[5] else { panic!() };
                            (int(&r[2]), *b)
                        })
                        .collect();
                    (one("log_start"), one("log_end"), batches)
                })
                .collect();
            let end = logs[0].1;
            held_logs.extend(logs.iter().map(|(s, _, b)| (p, *s, b.clone())));
            for (k, (start, e, batches)) in logs.iter().enumerate() {
                assert_eq!(
                    *e,
                    end,
                    "{}",
                    fail(
                        &cluster,
                        &format!("partition {p}: broker {} ends at {e}, not {end}", k + 1)
                    )
                );
                assert!(
                    *start > 0,
                    "seed {seed}: partition {p}: broker {} deleted nothing",
                    k + 1
                );
                // Where two replicas both hold data, they hold the same batches.
                for (other, (s2, _, b2)) in logs.iter().enumerate() {
                    let from = (*start).max(*s2);
                    let mine: Vec<_> = batches.range(from..).collect();
                    let theirs: Vec<_> = b2.range(from..).collect();
                    assert_eq!(
                        mine,
                        theirs,
                        "seed {seed}: partition {p}: brokers {} and {} differ from {from}",
                        k + 1,
                        other + 1
                    );
                }
            }
            // The returning broker took a snapshot point: its log starts at one, past what it held when it went down.
            let snap = states[2]
                .rows(rel("rsnap"))
                .find(|r| matches!(&r[0], Value::Tuple(g) if g[0] == tid && int(&g[1]) == p))
                .map(|r| int(&r[3]));
            if snap.is_some() {
                snapshots += 1;
            }
        }
        assert!(
            snapshots > 0,
            "{}",
            fail(&cluster, "the returning broker took no snapshot point")
        );
        // Every acknowledged acks=all batch at or past a replica's start is there, on every replica.
        let sh = shared.borrow();
        let max_term = states
            .iter()
            .flat_map(|s| s.rows(rel("rterm")).map(|r| int(&r[1])))
            .max()
            .unwrap_or(0);
        let mut acked = 0;
        for x in sh.sent.iter().filter(|x| x.produce.acks == -1) {
            let Some((0, base)) = x.answer else { continue };
            acked += 1;
            let p = i64::from(x.produce.partition);
            for (_, start, batches) in held_logs.iter().filter(|l| l.0 == p && base >= l.1) {
                assert!(
                    batches
                        .get(&base)
                        .is_some_and(|blob| (0..max_term).any(|e| BlobRef::of(&stamped(
                            &x.produce.batch,
                            base,
                            e as i32
                        )) == *blob)),
                    "{}",
                    fail(
                        &cluster,
                        &format!("the acks=all batch at {p}/{base} is missing from a replica starting at {start}")
                    )
                );
            }
        }
        assert!(acked > 60, "seed {seed}: only {acked} acknowledged");
    }
}

/// Item 5: five brokers, each partition moved to new replicas twice while producers and a reader run (the second
/// wave moves every partition off replicas the first put it on, and some off their leader), without and with
/// faults. Each change of a partition's members goes through its log, one replica at a time.
#[test]
fn reassignments_move_partitions_under_load() {
    let waves = vec![
        vec![(0, vec![4, 5, 1]), (1, vec![5, 1, 2]), (2, vec![1, 2, 3])],
        vec![(0, vec![2, 3, 4]), (1, vec![3, 4, 5]), (2, vec![4, 5, 1])],
    ];
    for faults in [false, true] {
        let t = check_runs(&Setup {
            brokers: 5,
            seeds: 1..=2,
            faults,
            waves: waves.clone(),
            clients: 3,
            requests: 60,
            idempotent: true,
        });
        assert!(t.acked_all > 200, "faults {faults}: {t:?}");
    }
}

/// One step of a scripted admin client: the request, and what to do with its answer (`Ok(true)`: next step;
/// `Ok(false)`: ask again a while later; `Err`: the run fails).
#[cfg(test)]
type Check = Box<dyn Fn(Bytes) -> Result<bool, String>>;

/// A scripted client: each step's request at a random broker, again until its check passes.
#[cfg(test)]
struct Script {
    rng: Rng,
    brokers: Vec<NodeId>,
    steps: Vec<(Vec<u8>, Check)>,
    at: usize,
    conn: Option<NodeId>,
    open: bool,
    buf: Vec<u8>,
    pending: Option<i64>,
    done: Rc<RefCell<usize>>,
    /// How long a request may go unanswered before the connection is dropped.
    patience: i64,
}

#[cfg(test)]
impl StreamClient for Script {
    fn on(&mut self, now: i64, e: StreamEvent<'_>) -> Result<StreamAction, String> {
        let mut a = StreamAction::default();
        match e {
            StreamEvent::Wake => {
                if self.pending.is_some_and(|d| now >= d) {
                    self.pending = None;
                    a.close = true;
                } else if self.at < self.steps.len() && self.pending.is_none() {
                    match self.conn {
                        None => {
                            let n = self.brokers[self.rng.below(self.brokers.len() as u64) as usize];
                            self.conn = Some(n);
                            self.open = false;
                            a.connect = Some((n, Arc::from("kafka")));
                        }
                        Some(_) if self.open => {
                            a.send = self.steps[self.at].0.clone();
                            self.pending = Some(now + self.patience);
                        }
                        Some(_) => {}
                    }
                }
                if self.at < self.steps.len() {
                    a.wake = Some(now + 50_000_000);
                }
            }
            StreamEvent::Opened => {
                self.open = true;
                self.buf.clear();
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
                let body = Bytes::copy_from_slice(&self.buf[4..4 + n]);
                self.buf.drain(..4 + n);
                self.pending = None;
                if (self.steps[self.at].1)(body)? {
                    self.at += 1;
                    *self.done.borrow_mut() = self.at;
                }
                a.close = true;
                a.wake = Some(now + 100_000_000);
            }
            StreamEvent::Closed(_) => {
                self.pending = None;
                self.conn = None;
                self.open = false;
                self.buf.clear();
                a.wake = Some(now + 20_000_000);
            }
        }
        Ok(a)
    }
}

#[cfg(test)]
fn alter_frame(topic: &str, parts: &[(i32, Option<Vec<i32>>)]) -> Vec<u8> {
    let req = AlterPartitionReassignmentsRequest::default()
        .with_timeout_ms(1_000)
        .with_allow_replication_factor_change(true)
        .with_topics(vec![
            ReassignableTopic::default()
                .with_name(TopicName(StrBytes::from_string(topic.into())))
                .with_partitions(
                    parts
                        .iter()
                        .map(|(p, rs)| {
                            ReassignablePartition::default()
                                .with_partition_index(*p)
                                .with_replicas(rs.as_ref().map(|v| v.iter().map(|x| BrokerId(*x)).collect()))
                        })
                        .collect(),
                ),
        ]);
    framed(45, 1, 1, &req)
}

/// The error per partition entry of an AlterPartitionReassignments answer; `want` each one's expected code (a
/// timeout asks again).
#[cfg(test)]
fn alter_check(want: Vec<i16>) -> Check {
    alter_check_any(vec![want])
}

/// As `alter_check`, with any of several expected answers.
#[cfg(test)]
fn alter_check_any(wants: Vec<Vec<i16>>) -> Check {
    Box::new(move |mut body: Bytes| {
        ResponseHeader::decode(&mut body, AlterPartitionReassignmentsResponse::header_version(1))
            .map_err(|x| x.to_string())?;
        let r = AlterPartitionReassignmentsResponse::decode(&mut body, 1).map_err(|x| x.to_string())?;
        let codes: Vec<i16> = r
            .responses
            .iter()
            .flat_map(|t| t.partitions.iter().map(|p| p.error_code))
            .collect();
        if codes.contains(&REQUEST_TIMED_OUT) {
            return Ok(false);
        }
        if !wants.contains(&codes) {
            return Err(format!("a reassignment answered {codes:?}, not one of {wants:?}"));
        }
        Ok(true)
    })
}

/// Asks ListPartitionReassignments until it lists none (three answers in a row, from brokers that may lag).
#[cfg(test)]
fn until_no_reassignment() -> (Vec<u8>, Check) {
    let quiet = Rc::new(RefCell::new(0));
    let frame = framed(
        46,
        0,
        1,
        &ListPartitionReassignmentsRequest::default().with_timeout_ms(1_000),
    );
    let check: Check = Box::new(move |mut body: Bytes| {
        ResponseHeader::decode(&mut body, ListPartitionReassignmentsResponse::header_version(0))
            .map_err(|x| x.to_string())?;
        let r = ListPartitionReassignmentsResponse::decode(&mut body, 0).map_err(|x| x.to_string())?;
        let mut q = quiet.borrow_mut();
        *q = if r.topics.is_empty() { *q + 1 } else { 0 };
        Ok(*q >= 3)
    });
    (frame, check)
}

#[cfg(test)]
fn create_frame(topic: &str, rf: i16) -> Vec<u8> {
    create_frame_within(topic, rf, 1_000)
}

#[cfg(test)]
fn create_frame_within(topic: &str, rf: i16, timeout_ms: i32) -> Vec<u8> {
    let req = CreateTopicsRequest::default()
        .with_topics(vec![
            CreatableTopic::default()
                .with_name(TopicName(StrBytes::from_string(topic.into())))
                .with_num_partitions(2)
                .with_replication_factor(rf),
        ])
        .with_timeout_ms(timeout_ms);
    framed(19, 7, 1, &req)
}

#[cfg(test)]
fn create_check() -> Check {
    Box::new(|mut body: Bytes| {
        ResponseHeader::decode(&mut body, CreateTopicsResponse::header_version(7)).map_err(|x| x.to_string())?;
        let r = CreateTopicsResponse::decode(&mut body, 7).map_err(|x| x.to_string())?;
        match r.topics[0].error_code {
            0 | TOPIC_ALREADY_EXISTS => Ok(true),
            REQUEST_TIMED_OUT => Ok(false),
            other => Err(format!("creating answered {other}")),
        }
    })
}

#[cfg(test)]
fn delete_frame(topic: &str) -> Vec<u8> {
    let req = kafka_protocol::messages::DeleteTopicsRequest::default()
        .with_topics(vec![
            kafka_protocol::messages::delete_topics_request::DeleteTopicState::default()
                .with_name(Some(TopicName(StrBytes::from_string(topic.into())))),
        ])
        .with_timeout_ms(1_000);
    framed(20, 6, 1, &req)
}

#[cfg(test)]
fn delete_check() -> Check {
    Box::new(|mut body: Bytes| {
        ResponseHeader::decode(
            &mut body,
            kafka_protocol::messages::DeleteTopicsResponse::header_version(6),
        )
        .map_err(|x| x.to_string())?;
        let r = kafka_protocol::messages::DeleteTopicsResponse::decode(&mut body, 6).map_err(|x| x.to_string())?;
        match r.responses[0].error_code {
            0 => Ok(true),
            REQUEST_TIMED_OUT => Ok(false),
            other => Err(format!("deleting answered {other}")),
        }
    })
}

/// The review's controller findings, directed (S8 item 8), on five brokers:
/// - a reassignment naming a partition twice is refused for both entries (it once halted every broker);
/// - a reassignment replaced while in progress, cancelled, and set again to an earlier target finishes (its
///   completion report once collided with the earlier one's and was never sent);
/// - a topic deleted while a reassignment of it is in progress leaves nothing behind (its reassignment once stayed,
///   and a late completion report brought a partition of the deleted topic back);
///
/// And through it all, no broker fails and the cluster settles.
#[test]
fn reassignments_replaced_cancelled_or_deleted_settle() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/kafka/sim_cluster.bls");
    let mut nodes: Vec<NodeSpec> = (1..=5)
        .map(|i| NodeSpec {
            name: format!("b{i}"),
            role: Some("Broker".to_owned()),
        })
        .collect();
    nodes.push(NodeSpec {
        name: "c1".to_owned(),
        role: Some("Client".to_owned()),
    });
    let params = [(
        "REPLICA_LAG_MAX".to_owned(),
        blossom_front::api::ParamBinding::Text("500ms".into()),
    )]
    .into_iter()
    .collect();
    let (result, _) = blossom_driver::bls::compile_file_with(path.to_str().unwrap(), &nodes, &params);
    let artifact: BlsArtifact = result.unwrap_or_else(|e| panic!("sim_cluster.bls: {e:?}")).0;
    let schema = DurableSchema::of(artifact.program.get());
    let rel = |n: &str| artifact.rel_named(n).unwrap();
    let brokers: Vec<NodeId> = (0..5).map(NodeId).collect();
    for seed in 1..=2u64 {
        let cfg = ClusterConfig {
            seed,
            clients: 0,
            duration: 20_000_000_000,
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
        let (_safety, observer) = GroupSafety::of(&artifact).unwrap().shared();
        cluster.observe(observer);
        let steps: Vec<(Vec<u8>, Check)> = vec![
            (create_frame("moving", 3), create_check()),
            (create_frame("doomed", 3), create_check()),
            // A partition twice (and one with a cancellation and a target): every entry naming it is refused.
            (
                alter_frame(
                    "moving",
                    &[
                        (0, Some(vec![1, 2, 4])),
                        (1, Some(vec![2, 3, 4])),
                        (0, Some(vec![3, 4, 5])),
                        (1, None),
                    ],
                ),
                alter_check(vec![42, 42, 42, 42]),
            ),
            // Replaced in flight, cancelled, set again: A, then B, then back (cancelled), then A again.
            (alter_frame("moving", &[(0, Some(vec![4, 5, 1]))]), alter_check(vec![0])),
            (alter_frame("moving", &[(0, Some(vec![5, 1, 2]))]), alter_check(vec![0])),
            // (The second target may already be reached: nothing to cancel then.)
            (
                alter_frame("moving", &[(0, None)]),
                alter_check_any(vec![vec![0], vec![NO_REASSIGNMENT_IN_PROGRESS]]),
            ),
            (alter_frame("moving", &[(0, Some(vec![4, 5, 1]))]), alter_check(vec![0])),
            until_no_reassignment(),
            (alter_frame("moving", &[(0, Some(vec![2, 3, 4]))]), alter_check(vec![0])),
            until_no_reassignment(),
            (alter_frame("moving", &[(0, Some(vec![4, 5, 1]))]), alter_check(vec![0])),
            until_no_reassignment(),
            // A topic deleted while it is being moved (broker 5 is down meanwhile, so the moves cannot finish).
            (
                alter_frame("doomed", &[(0, Some(vec![1, 4, 5])), (1, Some(vec![2, 4, 5]))]),
                alter_check(vec![0, 0]),
            ),
            (delete_frame("doomed"), delete_check()),
            until_no_reassignment(),
        ];
        let total = steps.len();
        // The steps of the doomed topic's reassignment and deletion (broker 5 is down from the first to the second).
        let (doomed_move, doomed_delete) = (total - 3, total - 2);
        let done = Rc::new(RefCell::new(0));
        cluster.stream_client(Box::new(Script {
            rng: Rng(seed * 13 + 1),
            brokers: brokers[..4].to_vec(),
            steps,
            at: 0,
            conn: None,
            open: false,
            buf: Vec::new(),
            pending: None,
            done: done.clone(),
            patience: REQUEST_TIMEOUT,
        }));
        let fail = |cluster: &Cluster<'_>, what: &str| -> String {
            format!("seed {seed}: {what}\n{}", cluster.run_so_far().log.join("\n"))
        };
        let mut b5_down = false;
        while cluster.now() < 18_000_000_000 && *done.borrow() < total {
            cluster.run_until(cluster.now() + 20_000_000).unwrap();
            assert!(
                cluster.violation().is_none(),
                "{}",
                fail(&cluster, cluster.violation().unwrap_or(""))
            );
            let at = *done.borrow();
            if at == doomed_move && !b5_down && cluster.state(brokers[4]).is_some() {
                cluster.crash(brokers[4], CrashWrites::Random).unwrap();
                b5_down = true;
            }
            if at > doomed_delete && b5_down && cluster.state(brokers[4]).is_none() {
                cluster.restart(brokers[4]).unwrap();
            }
        }
        assert_eq!(*done.borrow(), total, "{}", fail(&cluster, "the script did not finish"));
        if cluster.state(brokers[4]).is_none() {
            cluster.restart(brokers[4]).unwrap();
        }
        cluster.step_until(20_000_000_000).unwrap();
        for n in &brokers {
            let s = cluster.state(*n).unwrap();
            assert_eq!(
                s.rows(rel("mreassign")).count(),
                0,
                "seed {seed}: broker {n:?} keeps a reassignment"
            );
            let topics: Vec<String> = s
                .rows(rel("mtopic"))
                .map(|r| match &r[0] {
                    Value::Str(x) => x.to_string(),
                    other => panic!("{other:?}"),
                })
                .collect();
            assert_eq!(topics, vec!["moving".to_owned()], "seed {seed}: broker {n:?}'s topics");
            let tid = s.rows(rel("mtopic")).next().map(|r| r[1].clone()).unwrap();
            let p0: Vec<i64> = s
                .rows(rel("massign"))
                .filter(|r| r[0] == tid && int(&r[1]) == 0)
                .flat_map(|r| match &r[2] {
                    Value::Vec(v) => v.iter().map(int).collect::<Vec<_>>(),
                    other => panic!("{other:?}"),
                })
                .collect();
            assert_eq!(p0, vec![4, 5, 1], "seed {seed}: moving/0's replicas at broker {n:?}");
            // Only the controller's and moving's groups have Raft state.
            for name in ["rterm", "rlog", "rsnap", "massign"] {
                let foreign = s
                    .rows(rel(name))
                    .filter(|r| match &r[0] {
                        Value::Tuple(g) => {
                            g[0] != tid
                                && int(&g[1]) >= 0
                                && g[0] != Value::Bytes(vec![0u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1].into())
                        }
                        Value::Bytes(_) => r[0] != tid,
                        _ => false,
                    })
                    .count();
                assert_eq!(
                    foreign, 0,
                    "seed {seed}: broker {n:?} keeps {name} rows of the deleted topic"
                );
            }
        }
    }
}

/// HD item 2: a command its origin sends again after its `done` row has gone (`DONE_WINDOW` entries after it was
/// applied) is stale, and skipped. The controller leader's messages to one broker are cut (its own still arrive, and
/// it is slow to call an election), so it does not learn its creation was applied; meanwhile another broker's client
/// deletes the topic and creates more, past the window. The late copy must not bring the deleted topic back.
///
/// Here the cut broker sends its creation again every 50ms, so copies reach the leader during the cut.
#[test]
fn a_command_sent_again_past_the_done_window_is_skipped() {
    late_copies(false);
}

/// As `a_command_sent_again_past_the_done_window_is_skipped`, but the cut broker sends its creation again only every
/// 8s: the cut heals first, it catches up from the leader's applied metadata (its creation's `done` row already gone
/// from it), and only then sends its creation again. The copy keeps the index it was first made at, so it is stale.
#[test]
fn a_command_sent_again_after_catching_up_from_a_snapshot_is_skipped() {
    late_copies(true);
}

#[cfg(test)]
fn late_copies(slow: bool) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/kafka/sim_cluster.bls");
    let mut nodes: Vec<NodeSpec> = (1..=3)
        .map(|i| NodeSpec {
            name: format!("b{i}"),
            role: Some("Broker".to_owned()),
        })
        .collect();
    nodes.push(NodeSpec {
        name: "c1".to_owned(),
        role: Some("Client".to_owned()),
    });
    let text = |v: &str| blossom_front::api::ParamBinding::Text(v.into());
    let params = [
        ("DONE_WINDOW".to_owned(), blossom_front::api::ParamBinding::Int(4)),
        ("CONTROLLER_KEEP".to_owned(), blossom_front::api::ParamBinding::Int(2)),
        ("CMD_RESEND".to_owned(), text(if slow { "8s" } else { "50ms" })),
        ("RAFT_ELECTION_MIN".to_owned(), text("20s")),
        ("RAFT_ELECTION_MAX".to_owned(), text("25s")),
    ]
    .into_iter()
    .collect();
    let (result, _) = blossom_driver::bls::compile_file_with(path.to_str().unwrap(), &nodes, &params);
    let artifact: BlsArtifact = result.unwrap_or_else(|e| panic!("sim_cluster.bls: {e:?}")).0;
    let schema = DurableSchema::of(artifact.program.get());
    let rel = |n: &str| artifact.rel_named(n).unwrap();
    let ctl = {
        let mut id = vec![0u8; 16];
        id[15] = 1;
        Value::Tuple(vec![Value::Bytes(id.into()), Value::Int(IntValue::I32(0))].into())
    };
    let named = |cluster: &Cluster<'_>, n: NodeId, topic: &str| {
        cluster
            .state(n)
            .unwrap()
            .rows(rel("mtopic"))
            .any(|r| r[0] == Value::Str(topic.into()))
    };
    for seed in 1..=2u64 {
        let cfg = ClusterConfig {
            seed,
            clients: 0,
            duration: 15_000_000_000,
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
        let fail = |cluster: &Cluster<'_>, what: &str| -> String {
            format!("seed {seed}: {what}\n{}", cluster.run_so_far().log.join("\n"))
        };
        cluster.run_until(500_000_000).unwrap();
        // The controller's leader (the newest term's), the broker cut from it, and the third.
        let leader = cluster
            .state(NodeId(0))
            .unwrap()
            .rows(rel("leader_of"))
            .filter(|r| r[0] == ctl)
            .max_by_key(|r| int(&r[1]))
            .map(|r| match &r[2] {
                Value::Node(n) => *n,
                other => panic!("{other:?}"),
            })
            .unwrap_or_else(|| panic!("seed {seed}: no controller leader"));
        let others: Vec<NodeId> = (0..3).map(NodeId).filter(|n| *n != leader).collect();
        let (origin, third) = (others[0], others[1]);
        cluster.cut(leader, origin).unwrap();
        let creator = Rc::new(RefCell::new(0));
        cluster.stream_client(Box::new(Script {
            rng: Rng(seed),
            brokers: vec![origin],
            steps: vec![(create_frame_within("gone", 3, 30_000), Box::new(|_| Ok(true)))],
            at: 0,
            conn: None,
            open: false,
            buf: Vec::new(),
            pending: None,
            done: creator.clone(),
            patience: 30_000_000_000,
        }));
        while !named(&cluster, leader, "gone") {
            assert!(
                cluster.now() < 3_000_000_000,
                "{}",
                fail(&cluster, "the creation was not applied")
            );
            cluster.run_until(cluster.now() + 10_000_000).unwrap();
        }
        let mut steps: Vec<(Vec<u8>, Check)> = vec![(delete_frame("gone"), delete_check())];
        steps.extend((0..5).map(|k| (create_frame(&format!("after{k}"), 3), create_check())));
        let total = steps.len();
        let deleter = Rc::new(RefCell::new(0));
        cluster.stream_client(Box::new(Script {
            rng: Rng(seed + 100),
            brokers: vec![third],
            steps,
            at: 0,
            conn: None,
            open: false,
            buf: Vec::new(),
            pending: None,
            done: deleter.clone(),
            patience: REQUEST_TIMEOUT,
        }));
        while *deleter.borrow() < total {
            assert!(
                cluster.now() < 10_000_000_000,
                "{}",
                fail(&cluster, "the deletions did not finish")
            );
            cluster.run_until(cluster.now() + 20_000_000).unwrap();
        }
        // The cut broker still waits on its creation.
        assert_eq!(*creator.borrow(), 0, "{}", fail(&cluster, "the creation was answered"));
        if slow {
            // It catches up from a snapshot (the leader compacted past its log) before it sends its creation again.
            cluster.heal();
            while !named(&cluster, origin, "after4") {
                assert!(
                    cluster.now() < 7_500_000_000,
                    "{}",
                    fail(&cluster, "the cut broker did not catch up before sending again")
                );
                cluster.run_until(cluster.now() + 10_000_000).unwrap();
            }
            let point = cluster
                .state(origin)
                .unwrap()
                .rows(rel("rsnap"))
                .find(|r| r[0] == ctl)
                .map(|r| int(&r[1]))
                .unwrap_or(0);
            assert!(point > 0, "{}", fail(&cluster, "the cut broker took no snapshot point"));
            cluster.step_until(10_000_000_000).unwrap();
        } else {
            cluster.run_until(cluster.now() + 1_000_000_000).unwrap();
            cluster.heal();
            cluster.step_until(cluster.now() + 2_000_000_000).unwrap();
        }
        assert!(
            cluster.violation().is_none(),
            "{}",
            fail(&cluster, cluster.violation().unwrap_or(""))
        );
        for n in (0..3).map(NodeId) {
            assert!(
                !named(&cluster, n, "gone"),
                "{}",
                fail(&cluster, &format!("the deleted topic is back on broker {n:?}"))
            );
            assert!(
                named(&cluster, n, "after4"),
                "{}",
                fail(&cluster, &format!("broker {n:?} is behind"))
            );
        }
    }
}
