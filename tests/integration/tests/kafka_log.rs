//! Slice 7, item 8: the Blossom Kafka client (`examples/kafka/client_node.bls`) and a Rust client produce to one
//! topic of the Blossom broker and read it back, in the cluster simulator, under broker and client crashes and
//! dropped connections; a log checker validates every run.
//!
//! The Blossom client produces `CLIENT_BATCHES` batches (records it encodes itself, CRC32C included) and reads every
//! partition back; the Rust client (requests and answers through `kafka-protocol`, decision K5) does the same beside
//! it. Once both are done, a reader takes the final log. The checker then requires:
//! - every acknowledged batch, of either client, at its acknowledged offset with its records;
//! - every record of the log sent by some client, and none twice; offsets consecutive from 0;
//! - each client's read a prefix of the final log, partition by partition, with no gap: the observers agree.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_front::api::{NodeSpec, ParamBinding};
use blossom_node::durable::DurableSchema;
use blossom_sim::cluster::{Cluster, ClusterConfig, NoKvClients, StreamAction, StreamClient, StreamEvent};
use blossom_value::Value;
use blossom_value::time::NodeId;
use blossom_value::value::IntValue;
use bytes::{Bytes, BytesMut};
use kafka_protocol::messages::fetch_request::{FetchPartition, FetchTopic};
use kafka_protocol::messages::metadata_request::MetadataRequestTopic;
use kafka_protocol::messages::produce_request::{PartitionProduceData, TopicProduceData};
use kafka_protocol::messages::{
    FetchRequest, FetchResponse, MetadataRequest, MetadataResponse, ProduceRequest, ProduceResponse, RequestHeader,
    ResponseHeader, TopicName,
};
use kafka_protocol::protocol::{Decodable, Encodable, HeaderVersion, StrBytes};
use kafka_protocol::records::{
    Compression, Record, RecordBatchDecoder, RecordBatchEncoder, RecordEncodeOptions, TimestampType,
};

#[cfg(test)]
const TOPIC: &str = "logs";
#[cfg(test)]
const PARTITIONS: usize = 3;
#[cfg(test)]
const BLOSSOM_BATCHES: u64 = 30;
#[cfg(test)]
const RUST_BATCHES: u64 = 30;

/// A log: per partition, its records' values in offset order (offset = position).
#[cfg(test)]
type Log = Vec<Vec<Vec<u8>>>;

/// The Blossom client's values for batch `k` (`batch_values` in `client_fns.bls`).
#[cfg(test)]
fn blossom_values(k: u64) -> Vec<Vec<u8>> {
    (0..1 + k % 3)
        .map(|i| {
            let mut v = b"blossom/".to_vec();
            v.extend(k.to_be_bytes());
            v.extend(i.to_be_bytes());
            v
        })
        .collect()
}

#[cfg(test)]
fn rust_values(k: u64) -> Vec<Vec<u8>> {
    (0..1 + k % 2).map(|i| format!("rust/{k}/{i}").into_bytes()).collect()
}

#[cfg(test)]
fn batch(values: &[Vec<u8>]) -> Vec<u8> {
    let records: Vec<Record> = values
        .iter()
        .enumerate()
        .map(|(i, v)| Record {
            transactional: false,
            control: false,
            delete_horizon: false,
            partition_leader_epoch: -1,
            producer_id: -1,
            producer_epoch: -1,
            timestamp_type: TimestampType::Creation,
            offset: i as i64,
            sequence: i as i32 - 1,
            timestamp: 0,
            key: None,
            value: Some(Bytes::from(v.clone())),
            headers: Default::default(),
        })
        .collect();
    let mut buf = BytesMut::new();
    RecordBatchEncoder::encode(
        &mut buf,
        &records,
        &RecordEncodeOptions {
            version: 2,
            compression: Compression::None,
        },
    )
    .unwrap();
    buf.to_vec()
}

#[cfg(test)]
fn framed<M: Encodable + HeaderVersion>(key: i16, version: i16, corr: i32, body: &M) -> Vec<u8> {
    let mut buf = BytesMut::new();
    RequestHeader::default()
        .with_request_api_key(key)
        .with_request_api_version(version)
        .with_correlation_id(corr)
        .with_client_id(Some(StrBytes::from_string("log".into())))
        .encode(&mut buf, M::header_version(version))
        .unwrap();
    body.encode(&mut buf, version).unwrap();
    let mut out = (buf.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&buf);
    out
}

#[cfg(test)]
fn topic_name() -> TopicName {
    TopicName(StrBytes::from_string(TOPIC.into()))
}

#[cfg(test)]
fn one_frame(buf: &mut Vec<u8>) -> Option<Bytes> {
    let n = buf
        .get(..4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)?;
    if buf.len() < 4 + n {
        return None;
    }
    let body = Bytes::copy_from_slice(&buf[4..4 + n]);
    buf.drain(..4 + n);
    Some(body)
}

/// What the Rust client did: its acknowledged batches (partition, base, values), everything it sent, and its read.
#[cfg(test)]
#[derive(Default)]
struct Seen {
    acked: Vec<(usize, i64, Vec<Vec<u8>>)>,
    sent: Vec<Vec<u8>>,
    read: Option<Log>,
    done: bool,
}

/// A Rust client: learns the topic (auto-creating it), produces `batches` (skipping one whose connection died
/// unanswered), then reads every partition from 0 to its high watermark. With `batches` 0 it only reads, once `go`.
#[cfg(test)]
struct RustClient {
    seen: Rc<RefCell<Seen>>,
    go: Rc<dyn Fn() -> bool>,
    batches: u64,
    k: u64,
    topic_id: Option<[u8; 16]>,
    read: Log,
    part: usize,
    phase: u8,
    connected: bool,
    buf: Vec<u8>,
    corr: i32,
}

#[cfg(test)]
impl RustClient {
    fn send(&mut self, a: &mut StreamAction) {
        self.corr += 1;
        a.send = match self.phase {
            0 => {
                let req = MetadataRequest::default()
                    .with_topics(Some(vec![
                        MetadataRequestTopic::default().with_name(Some(topic_name())),
                    ]))
                    .with_allow_auto_topic_creation(true);
                framed(3, 13, self.corr, &req)
            }
            1 => {
                let values = rust_values(self.k);
                self.seen.borrow_mut().sent.extend(values.clone());
                let req = ProduceRequest::default()
                    .with_acks(-1)
                    .with_timeout_ms(1000)
                    .with_topic_data(vec![
                        TopicProduceData::default()
                            .with_name(topic_name())
                            .with_partition_data(vec![
                                PartitionProduceData::default()
                                    .with_index((self.k % PARTITIONS as u64) as i32)
                                    .with_records(Some(Bytes::from(batch(&values)))),
                            ]),
                    ]);
                framed(0, 12, self.corr, &req)
            }
            2 => {
                let off = self.read[self.part].len() as i64;
                let req = FetchRequest::default()
                    .with_max_bytes(1 << 20)
                    .with_session_epoch(-1)
                    .with_topics(vec![
                        FetchTopic::default()
                            .with_topic_id(uuid::Uuid::from_bytes(self.topic_id.unwrap_or([0; 16])))
                            .with_partitions(vec![
                                FetchPartition::default()
                                    .with_partition(self.part as i32)
                                    .with_current_leader_epoch(-1)
                                    .with_fetch_offset(off)
                                    .with_partition_max_bytes(1 << 20),
                            ]),
                    ]);
                framed(1, 17, self.corr, &req)
            }
            _ => Vec::new(),
        };
    }

    fn answer(&mut self, mut body: Bytes) -> Result<(), String> {
        match self.phase {
            0 => {
                ResponseHeader::decode(&mut body, MetadataResponse::header_version(13)).map_err(|e| e.to_string())?;
                let r = MetadataResponse::decode(&mut body, 13).map_err(|e| e.to_string())?;
                let t = &r.topics[0];
                if t.error_code == 0 && t.partitions.len() == PARTITIONS {
                    self.topic_id = Some(*t.topic_id.as_bytes());
                    self.phase = if self.k < self.batches { 1 } else { 2 };
                }
            }
            1 => {
                ResponseHeader::decode(&mut body, ProduceResponse::header_version(12)).map_err(|e| e.to_string())?;
                let r = ProduceResponse::decode(&mut body, 12).map_err(|e| e.to_string())?;
                let pr = &r.responses[0].partition_responses[0];
                // The partition's leader is not elected yet (its broker just restarted): nothing was appended (with
                // one broker, a leader that appended loses its leadership only by crashing, which also closes this
                // connection), so the same batch goes again, as a client retries.
                if pr.error_code == NOT_LEADER_OR_FOLLOWER {
                    return Ok(());
                }
                if pr.error_code != 0 {
                    return Err(format!("a produce was refused: {pr:?}"));
                }
                self.seen.borrow_mut().acked.push((
                    (self.k % PARTITIONS as u64) as usize,
                    pr.base_offset,
                    rust_values(self.k),
                ));
                self.k += 1;
                if self.k == self.batches {
                    self.phase = 2;
                }
            }
            2 => {
                ResponseHeader::decode(&mut body, FetchResponse::header_version(17)).map_err(|e| e.to_string())?;
                let r = FetchResponse::decode(&mut body, 17).map_err(|e| e.to_string())?;
                let p = &r.responses[0].partitions[0];
                if p.error_code == NOT_LEADER_OR_FOLLOWER {
                    return Ok(());
                }
                if p.error_code != 0 {
                    return Err(format!("a fetch failed: {p:?}"));
                }
                let mut recs = p.records.clone().unwrap_or_default();
                if !recs.is_empty() {
                    for rec in RecordBatchDecoder::decode(&mut recs)
                        .map_err(|e| e.to_string())?
                        .records
                    {
                        let have = self.read[self.part].len() as i64;
                        if rec.offset == have {
                            self.read[self.part].push(rec.value.unwrap_or_default().to_vec());
                        } else if rec.offset > have {
                            return Err(format!("a gap before offset {} in partition {}", rec.offset, self.part));
                        }
                    }
                }
                if self.read[self.part].len() as i64 >= p.high_watermark {
                    self.part += 1;
                    if self.part == PARTITIONS {
                        self.phase = 3;
                        let mut s = self.seen.borrow_mut();
                        s.read = Some(self.read.clone());
                        s.done = true;
                    }
                }
            }
            _ => return Err("an answer after the client finished".into()),
        }
        Ok(())
    }
}

/// The partition's leader is not this broker (yet): the client asks again.
#[cfg(test)]
const NOT_LEADER_OR_FOLLOWER: i16 = 6;

#[cfg(test)]
impl StreamClient for RustClient {
    fn on(&mut self, now: i64, e: StreamEvent<'_>) -> Result<StreamAction, String> {
        let mut a = StreamAction::default();
        match e {
            StreamEvent::Wake if !(self.go)() => a.wake = Some(now + 20_000_000),
            StreamEvent::Wake if !self.connected && self.phase < 3 => {
                self.connected = true;
                a.connect = Some((NodeId(0), Arc::from("kafka")));
            }
            StreamEvent::Wake => {
                if self.phase < 3 {
                    self.send(&mut a);
                }
            }
            StreamEvent::Opened => {
                self.buf.clear();
                // Learn the topic again on every connection (it may not exist yet).
                self.phase = if self.topic_id.is_none() { 0 } else { self.phase };
                self.send(&mut a);
            }
            StreamEvent::Received(b) => {
                self.buf.extend_from_slice(b);
                if let Some(body) = one_frame(&mut self.buf) {
                    self.answer(body)?;
                    if self.phase == 0 {
                        // The topic is not there yet: ask again a little later.
                        a.wake = Some(now + 20_000_000);
                    } else if self.phase < 3 {
                        self.send(&mut a);
                    }
                }
            }
            StreamEvent::Closed(_) => {
                // A produce in flight may or may not have landed: go on with the next batch.
                if self.phase == 1 {
                    self.k += 1;
                    if self.k == self.batches {
                        self.phase = 2;
                    }
                }
                self.connected = false;
                a.wake = Some(now + 15_000_000);
            }
        }
        Ok(a)
    }
}

#[cfg(test)]
fn u64_of(v: &Value) -> u64 {
    match v {
        Value::Int(IntValue::U64(x)) => *x,
        other => panic!("{other:?}"),
    }
}

#[cfg(test)]
fn int(v: &Value) -> i64 {
    match v {
        Value::Int(IntValue::I16(x)) => i64::from(*x),
        Value::Int(IntValue::I32(x)) => i64::from(*x),
        Value::Int(IntValue::I64(x)) => *x,
        other => panic!("{other:?}"),
    }
}

#[cfg(test)]
fn bytes_of(v: &Value) -> Vec<u8> {
    match v {
        Value::Bytes(b) => b.to_vec(),
        other => panic!("{other:?}"),
    }
}

/// Checks a run: the acknowledged batches (partition, base, values) against the final log; everything in the log
/// sent, once; each read a prefix of it.
#[cfg(test)]
fn check(
    final_log: &Log,
    acked: &[(usize, i64, Vec<Vec<u8>>)],
    sent: &BTreeSet<Vec<u8>>,
    reads: &[(&str, &Log)],
) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for (p, part) in final_log.iter().enumerate() {
        for (off, v) in part.iter().enumerate() {
            if !sent.contains(v) {
                return Err(format!("partition {p} offset {off}: a record nobody sent"));
            }
            if !seen.insert(v.clone()) {
                return Err(format!("partition {p} offset {off}: a record twice"));
            }
        }
    }
    for (p, base, values) in acked {
        for (i, v) in values.iter().enumerate() {
            let at = *base as usize + i;
            if final_log[*p].get(at) != Some(v) {
                return Err(format!("partition {p}: an acknowledged record is not at offset {at}"));
            }
        }
    }
    for (who, read) in reads {
        for p in 0..PARTITIONS {
            if read[p].len() > final_log[p].len() || read[p][..] != final_log[p][..read[p].len()] {
                return Err(format!(
                    "{who}'s read of partition {p} is not a prefix of the final log"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn both_clients_see_one_log_and_every_acknowledged_record_once() {
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
    let params = [
        (
            "CLIENT_BATCHES".to_owned(),
            ParamBinding::Int(i128::from(BLOSSOM_BATCHES)),
        ),
        ("DEFAULT_PARTITIONS".to_owned(), ParamBinding::Int(PARTITIONS as i128)),
    ]
    .into_iter()
    .collect();
    let (result, _) = blossom_driver::bls::compile_file_with(path.to_str().unwrap(), &nodes, &params);
    let artifact: BlsArtifact = result.unwrap_or_else(|e| panic!("sim_cluster.bls: {e:?}")).0;
    let schema = DurableSchema::of(artifact.program.get());
    let rel = |n: &str| artifact.rel_named(n).unwrap();
    for seed in 1..=6u64 {
        let cfg = ClusterConfig {
            seed,
            clients: 0,
            chunk_max: 64 + (seed as usize % 4) * 300,
            nemesis: 200_000_000,
            crashes: true,
            downtime: 50_000_000,
            stream_drops: true,
            duration: 6_000_000_000,
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
        let writer = Rc::new(RefCell::new(Seen::default()));
        let last = Rc::new(RefCell::new(Seen::default()));
        let client = |seen: Rc<RefCell<Seen>>, batches: u64, go: Rc<dyn Fn() -> bool>| RustClient {
            seen,
            go,
            batches,
            k: 0,
            topic_id: None,
            read: vec![Vec::new(); PARTITIONS],
            part: 0,
            phase: 0,
            connected: false,
            buf: Vec::new(),
            corr: 0,
        };
        cluster.stream_client(Box::new(client(writer.clone(), RUST_BATCHES, Rc::new(|| true))));
        cluster.run_until(5_000_000_000).unwrap();
        let run = cluster.run_so_far();
        assert!(
            run.violation.is_none(),
            "seed {seed}: {:?}\n{}",
            run.violation,
            run.log.join("\n")
        );
        assert!(
            run.crashes > 0 && run.stream_resets > 0,
            "seed {seed}: the faults did not happen"
        );
        assert!(writer.borrow().done, "seed {seed}: the Rust client did not finish");
        // Settle without faults until the Blossom client is done, then read the final log.
        cluster.step_until(6_000_000_000).unwrap();
        let state = cluster.state(NodeId(1)).expect("the client node is up");
        assert!(
            state.rows(rel("read_done")).next().is_some(),
            "seed {seed}: the Blossom client did not finish reading"
        );
        cluster.stream_client(Box::new(client(last.clone(), 0, Rc::new(|| true))));
        cluster.step_until(7_000_000_000).unwrap();
        let final_log = last.borrow().read.clone().expect("the final read finished");

        // The Blossom client's acknowledged batches and its read, from its durable rows.
        let mut acked: Vec<(usize, i64, Vec<Vec<u8>>)> = writer.borrow().acked.clone();
        let mut blossom_acked = 0;
        for r in state.rows(rel("acked")) {
            assert_eq!(int(&r[3]), 0, "seed {seed}: the broker refused a Blossom produce");
            let k = u64_of(&r[0]);
            acked.push((int(&r[1]) as usize, int(&r[2]), blossom_values(k)));
            blossom_acked += 1;
        }
        let mut blossom_read: Log = vec![Vec::new(); PARTITIONS];
        let mut consumed: BTreeMap<(usize, i64), Vec<u8>> = BTreeMap::new();
        for r in state.rows(rel("consumed")) {
            consumed.insert((int(&r[0]) as usize, int(&r[1])), bytes_of(&r[2]));
        }
        for ((p, off), v) in consumed {
            assert_eq!(
                off as usize,
                blossom_read[p].len(),
                "seed {seed}: a gap in the Blossom client's read of {p}"
            );
            blossom_read[p].push(v);
        }
        let sent: BTreeSet<Vec<u8>> = writer
            .borrow()
            .sent
            .iter()
            .cloned()
            .chain((0..BLOSSOM_BATCHES).flat_map(blossom_values))
            .collect();
        let rust_read = writer.borrow().read.clone().unwrap();
        check(
            &final_log,
            &acked,
            &sent,
            &[("the Rust client", &rust_read), ("the Blossom client", &blossom_read)],
        )
        .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert!(
            blossom_acked > 10,
            "seed {seed}: only {blossom_acked} Blossom produces were acknowledged"
        );
        assert!(
            blossom_read.iter().any(|p| !p.is_empty()),
            "seed {seed}: the Blossom client read nothing"
        );
    }
}
