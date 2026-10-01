//! Slice 7, item 7: retention on the Blossom broker (`examples/kafka/produce_node.bls`), in the cluster simulator.
//!
//! A scripted client (requests encoded and answers decoded by `kafka-protocol`) creates two topics with small
//! segments: one kept by time (`retention.ms`), one by size (`retention.bytes`), and produces batches to each. Then:
//! - by time: once every batch is older than `retention.ms`, everything is deleted, the active segment too, and the
//!   log start is the log end (Kafka's `deleteOldSegments` rolls a new active segment there);
//! - by size: the log start is where an independent model of Kafka's rule puts it (whole segments from the front,
//!   the last too unless it is empty, while the log still holds `retention.bytes` without them), with segments rolled
//!   at `segment.bytes`;
//! - a Fetch below the log start is OFFSET_OUT_OF_RANGE, and one from it reads the rest byte for byte;
//! - the broker's rows hold nothing below the log start: no batch, chunk, time index or segment row.

use std::cell::RefCell;
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
use kafka_protocol::messages::create_topics_request::{CreatableTopic, CreatableTopicConfig};
use kafka_protocol::messages::fetch_request::{FetchPartition, FetchTopic};
use kafka_protocol::messages::list_offsets_request::{ListOffsetsPartition, ListOffsetsTopic};
use kafka_protocol::messages::metadata_request::MetadataRequestTopic;
use kafka_protocol::messages::produce_request::{PartitionProduceData, TopicProduceData};
use kafka_protocol::messages::{
    BrokerId, CreateTopicsRequest, CreateTopicsResponse, FetchRequest, FetchResponse, ListOffsetsRequest,
    ListOffsetsResponse, MetadataRequest, MetadataResponse, ProduceRequest, ProduceResponse, RequestHeader,
    ResponseHeader, TopicName,
};
use kafka_protocol::protocol::{Decodable, Encodable, HeaderVersion, StrBytes};
use kafka_protocol::records::{Compression, Record, RecordBatchEncoder, RecordEncodeOptions, TimestampType};

/// The simulator's deployment epoch, in nanoseconds (`blossom_sim::cluster`): record timestamps are milliseconds
/// since it, as the broker's clock reads them.
#[cfg(test)]
const SIM_EPOCH_NS: i64 = 1_000_000_000_000_000_000;

#[cfg(test)]
const SEGMENT_BYTES: i64 = 400;
#[cfg(test)]
const RETENTION_MS: i64 = 1_000;
#[cfg(test)]
const RETENTION_BYTES: i64 = 1_000;
#[cfg(test)]
const BATCHES: usize = 16;

/// The two topics: kept by time, and by size.
#[cfg(test)]
const TOPICS: [&str; 2] = ["by-time", "by-size"];

#[cfg(test)]
fn batch(tag: &str, n: usize, ts_ms: i64) -> Vec<u8> {
    let records: Vec<Record> = (0..n)
        .map(|i| Record {
            transactional: false,
            control: false,
            delete_horizon: false,
            partition_leader_epoch: -1,
            producer_id: -1,
            producer_epoch: -1,
            timestamp_type: TimestampType::Creation,
            offset: i as i64,
            sequence: i as i32 - 1,
            timestamp: ts_ms,
            key: None,
            value: Some(Bytes::from(format!("{tag}/{i}").repeat(4))),
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

/// How many offsets a batch takes: `lastOffsetDelta + 1`.
#[cfg(test)]
fn offsets_of(b: &[u8]) -> i64 {
    i64::from(i32::from_be_bytes([b[23], b[24], b[25], b[26]])) + 1
}

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
        .with_client_id(Some(StrBytes::from_string("retention".into())))
        .encode(&mut buf, M::header_version(version))
        .unwrap();
    body.encode(&mut buf, version).unwrap();
    let mut out = (buf.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&buf);
    out
}

#[cfg(test)]
fn topic_name(t: &str) -> TopicName {
    TopicName(StrBytes::from_string(t.into()))
}

/// Kafka's segments for batches of these sizes: each segment's first batch index and bytes (a batch that would take a
/// non-empty segment past `SEGMENT_BYTES` starts the next).
#[cfg(test)]
fn segments(sizes: &[i64]) -> Vec<(usize, i64)> {
    let mut out: Vec<(usize, i64)> = Vec::new();
    for (i, s) in sizes.iter().enumerate() {
        match out.last_mut() {
            Some(last) if last.1 == 0 || last.1 + s <= SEGMENT_BYTES => last.1 += s,
            _ => out.push((i, *s)),
        }
    }
    out
}

/// Kafka's size retention: whole segments from the front (the last too, unless it is empty) while the log still holds
/// `RETENTION_BYTES` without them; the first batch kept (the batch count if every one is deleted).
#[cfg(test)]
fn size_start(sizes: &[i64]) -> usize {
    let segs = segments(sizes);
    let mut total: i64 = sizes.iter().sum();
    let mut first = 0;
    for (k, (_, bytes)) in segs.iter().enumerate() {
        if (k + 1 == segs.len() && *bytes == 0) || total - bytes < RETENTION_BYTES {
            break;
        }
        total -= bytes;
        first = segs.get(k + 1).map(|s| s.0).unwrap_or(sizes.len());
    }
    first
}

/// What the script produced per topic: each batch's base offset and bytes; and the topics' ids.
#[cfg(test)]
#[derive(Default)]
struct Produced {
    batches: [Vec<(i64, Vec<u8>)>; 2],
    ids: [[u8; 16]; 2],
    /// Per topic: the log start ListOffsets reported, and what a Fetch from 0 and from it answered.
    earliest: [Option<i64>; 2],
    below: [Option<i16>; 2],
    rest: [Option<Vec<u8>>; 2],
}

#[cfg(test)]
struct Script {
    out: Rc<RefCell<Produced>>,
    phase: usize,
    produced: usize,
    connected: bool,
    buf: Vec<u8>,
    corr: i32,
    /// When the reads may start: long after the last batch, so every batch is older than `retention.ms`.
    read_after: i64,
    done: Rc<RefCell<bool>>,
}

#[cfg(test)]
impl Script {
    fn send(&mut self, a: &mut StreamAction, now: i64) {
        self.corr += 1;
        let corr = self.corr;
        let o = self.out.borrow();
        a.send = match self.phase {
            0 => {
                let config = |k: &str, v: i64| {
                    CreatableTopicConfig::default()
                        .with_name(StrBytes::from_string(k.into()))
                        .with_value(Some(StrBytes::from_string(v.to_string())))
                };
                let req = CreateTopicsRequest::default()
                    .with_topics(vec![
                        CreatableTopic::default()
                            .with_name(topic_name(TOPICS[0]))
                            .with_num_partitions(1)
                            .with_replication_factor(1)
                            .with_configs(vec![
                                config("segment.bytes", SEGMENT_BYTES),
                                config("retention.ms", RETENTION_MS),
                            ]),
                        CreatableTopic::default()
                            .with_name(topic_name(TOPICS[1]))
                            .with_num_partitions(1)
                            .with_replication_factor(1)
                            .with_configs(vec![
                                config("segment.bytes", SEGMENT_BYTES),
                                config("retention.bytes", RETENTION_BYTES),
                            ]),
                    ])
                    .with_timeout_ms(1000);
                framed(19, 7, corr, &req)
            }
            1 => {
                // Alternately to each topic; records stamped with the broker's clock (milliseconds since the epoch).
                let t = self.produced % 2;
                let k = self.produced / 2;
                let b = batch(&format!("{t}.{k}"), 1 + k % 3, (SIM_EPOCH_NS + now) / 1_000_000);
                drop(o);
                self.out.borrow_mut().batches[t].push((-1, b.clone()));
                let req = ProduceRequest::default()
                    .with_acks(-1)
                    .with_timeout_ms(1000)
                    .with_topic_data(vec![
                        TopicProduceData::default()
                            .with_name(topic_name(TOPICS[t]))
                            .with_partition_data(vec![
                                PartitionProduceData::default()
                                    .with_index(0)
                                    .with_records(Some(Bytes::from(b))),
                            ]),
                    ]);
                framed(0, 11, corr, &req)
            }
            2 => {
                let req = MetadataRequest::default().with_topics(Some(
                    TOPICS
                        .iter()
                        .map(|t| MetadataRequestTopic::default().with_name(Some(topic_name(t))))
                        .collect(),
                ));
                framed(3, 13, corr, &req)
            }
            3 | 5 => {
                let req = ListOffsetsRequest::default().with_replica_id(BrokerId(-1)).with_topics(
                    TOPICS
                        .iter()
                        .map(|t| {
                            ListOffsetsTopic::default()
                                .with_name(topic_name(t))
                                .with_partitions(vec![
                                    ListOffsetsPartition::default()
                                        .with_partition_index(0)
                                        .with_current_leader_epoch(-1)
                                        .with_timestamp(-2),
                                ])
                        })
                        .collect(),
                );
                framed(2, 9, corr, &req)
            }
            4 | 6 => {
                // From 0 (below the start once retention ran) and from the start.
                let from_zero = self.phase == 4;
                let req = FetchRequest::default()
                    .with_max_bytes(1 << 24)
                    .with_min_bytes(1)
                    .with_session_epoch(-1)
                    .with_topics(
                        (0..2)
                            .map(|t| {
                                let off = if from_zero { 0 } else { o.earliest[t].unwrap_or(0) };
                                FetchTopic::default()
                                    .with_topic_id(uuid::Uuid::from_bytes(o.ids[t]))
                                    .with_partitions(vec![
                                        FetchPartition::default()
                                            .with_partition(0)
                                            .with_current_leader_epoch(0)
                                            .with_fetch_offset(off)
                                            .with_partition_max_bytes(1 << 20),
                                    ])
                            })
                            .collect(),
                    );
                framed(1, 17, corr, &req)
            }
            _ => {
                drop(o);
                *self.done.borrow_mut() = true;
                return;
            }
        };
    }

    fn answer(&mut self, mut body: Bytes, now: i64, a: &mut StreamAction) -> Result<(), String> {
        match self.phase {
            0 => {
                ResponseHeader::decode(&mut body, CreateTopicsResponse::header_version(7))
                    .map_err(|e| e.to_string())?;
                let r = CreateTopicsResponse::decode(&mut body, 7).map_err(|e| e.to_string())?;
                if r.topics.iter().any(|t| t.error_code != 0) {
                    return Err(format!("creating the topics: {r:?}"));
                }
                self.phase = 1;
            }
            1 => {
                ResponseHeader::decode(&mut body, ProduceResponse::header_version(11)).map_err(|e| e.to_string())?;
                let r = ProduceResponse::decode(&mut body, 11).map_err(|e| e.to_string())?;
                let pr = &r.responses[0].partition_responses[0];
                if pr.error_code != 0 {
                    return Err(format!("produce: {pr:?}"));
                }
                let t = self.produced % 2;
                self.out.borrow_mut().batches[t].last_mut().unwrap().0 = pr.base_offset;
                self.produced += 1;
                if self.produced == 2 * BATCHES {
                    self.phase = 2;
                    self.read_after = now + 3 * RETENTION_MS * 1_000_000;
                } else {
                    // Batches 50 ms apart, so they span the retention period.
                    a.wake = Some(now + 50_000_000);
                    return Ok(());
                }
            }
            2 => {
                ResponseHeader::decode(&mut body, MetadataResponse::header_version(13)).map_err(|e| e.to_string())?;
                let r = MetadataResponse::decode(&mut body, 13).map_err(|e| e.to_string())?;
                for t in 0..2 {
                    self.out.borrow_mut().ids[t] = *r.topics[t].topic_id.as_bytes();
                }
                self.phase = 3;
                a.wake = Some(self.read_after);
                return Ok(());
            }
            3 | 5 => {
                ResponseHeader::decode(&mut body, ListOffsetsResponse::header_version(9)).map_err(|e| e.to_string())?;
                let r = ListOffsetsResponse::decode(&mut body, 9).map_err(|e| e.to_string())?;
                for t in 0..2 {
                    let p = &r.topics[t].partitions[0];
                    if p.error_code != 0 {
                        return Err(format!("list offsets: {p:?}"));
                    }
                    let mut o = self.out.borrow_mut();
                    if self.phase == 5 && o.earliest[t] != Some(p.offset) {
                        return Err(format!(
                            "the log start moved from {:?} to {} with nothing produced",
                            o.earliest[t], p.offset
                        ));
                    }
                    o.earliest[t] = Some(p.offset);
                }
                self.phase += 1;
            }
            4 | 6 => {
                ResponseHeader::decode(&mut body, FetchResponse::header_version(17)).map_err(|e| e.to_string())?;
                let r = FetchResponse::decode(&mut body, 17).map_err(|e| e.to_string())?;
                for t in 0..2 {
                    let p = &r.responses[t].partitions[0];
                    let mut o = self.out.borrow_mut();
                    if self.phase == 4 {
                        o.below[t] = Some(p.error_code);
                    } else {
                        o.rest[t] = Some(p.records.clone().unwrap_or_default().to_vec());
                    }
                }
                self.phase += 1;
                if self.phase == 5 {
                    // A second look later: nothing is produced, so the log start stays where retention left it.
                    a.wake = Some(now + 2 * RETENTION_MS * 1_000_000);
                    return Ok(());
                }
            }
            _ => return Err("an answer after the script ended".into()),
        }
        self.send(a, now);
        Ok(())
    }
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

#[cfg(test)]
impl StreamClient for Script {
    fn on(&mut self, now: i64, e: StreamEvent<'_>) -> Result<StreamAction, String> {
        let mut a = StreamAction::default();
        match e {
            StreamEvent::Wake if !self.connected => {
                self.connected = true;
                a.connect = Some((NodeId(0), Arc::from("kafka")));
            }
            StreamEvent::Wake => self.send(&mut a, now),
            StreamEvent::Opened => self.send(&mut a, now),
            StreamEvent::Received(b) => {
                self.buf.extend_from_slice(b);
                if let Some(body) = one_frame(&mut self.buf) {
                    self.answer(body, now, &mut a)?;
                }
            }
            StreamEvent::Closed(why) => return Err(format!("the connection closed: {why}")),
        }
        Ok(a)
    }
}

#[cfg(test)]
fn int(v: &Value) -> i64 {
    match v {
        Value::Int(IntValue::I32(x)) => i64::from(*x),
        Value::Int(IntValue::I64(x)) => *x,
        Value::Int(IntValue::U8(x)) => i64::from(*x),
        other => panic!("{other:?}"),
    }
}

#[test]
fn retention_deletes_whole_segments_from_the_front() {
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
    let params = [("RETENTION_CHECK".to_owned(), ParamBinding::Text("50ms".into()))]
        .into_iter()
        .collect();
    let (result, _) = blossom_driver::bls::compile_file_with(path.to_str().unwrap(), &nodes, &params);
    let artifact: BlsArtifact = result.unwrap_or_else(|e| panic!("sim_cluster.bls: {e:?}")).0;
    let schema = DurableSchema::of(artifact.program.get());
    for seed in 1..=3u64 {
        let cfg = ClusterConfig {
            seed,
            clients: 0,
            chunk_max: 256,
            duration: 10_000_000_000,
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
        let out = Rc::new(RefCell::new(Produced::default()));
        let done = Rc::new(RefCell::new(false));
        cluster.stream_client(Box::new(Script {
            out: out.clone(),
            phase: 0,
            produced: 0,
            connected: false,
            buf: Vec::new(),
            corr: 0,
            read_after: 0,
            done: done.clone(),
        }));
        cluster.run_until(10_000_000_000).unwrap();
        let run = cluster.run_so_far();
        assert!(
            run.violation.is_none(),
            "seed {seed}: {:?}\n{}",
            run.violation,
            run.log.join("\n")
        );
        assert!(*done.borrow(), "seed {seed}: the script did not finish");
        let o = out.borrow();
        for (t, name) in TOPICS.iter().enumerate() {
            let sizes: Vec<i64> = o.batches[t].iter().map(|b| b.1.len() as i64).collect();
            let segs = segments(&sizes);
            // By time, every batch expired: everything is deleted, the active segment too (a new one is rolled at
            // the log end), as Kafka deletes it. By size, the model's start.
            let first = if t == 0 { sizes.len() } else { size_start(&sizes) };
            let end = o.batches[t].last().map(|(b, bytes)| b + offsets_of(bytes)).unwrap();
            let start = o.batches[t].get(first).map(|b| b.0).unwrap_or(end);
            assert!(segs.len() > 2, "seed {seed}: only {} segments", segs.len());
            assert_eq!(o.earliest[t], Some(start), "seed {seed}: {}'s log start", name);
            assert_eq!(o.below[t], Some(1), "seed {seed}: {}'s fetch below the start", name);
            let rest: Vec<u8> = o.batches[t][first..]
                .iter()
                .flat_map(|(base, b)| stamped(b, *base))
                .collect();
            assert_eq!(
                o.rest[t].as_deref(),
                Some(&rest[..]),
                "seed {seed}: {}'s records from the start",
                name
            );
        }
        // Nothing is left below the log starts.
        let state = cluster.state(NodeId(0)).expect("the broker is up");
        let tids: Vec<Value> = TOPICS
            .iter()
            .map(|n| {
                state
                    .rows(artifact.rel_named("mtopic").unwrap())
                    .find(|r| r[0] == Value::Str((*n).into()))
                    .map(|r| r[1].clone())
                    .unwrap()
            })
            .collect();
        for (t, tid) in tids.iter().enumerate() {
            let start = o.earliest[t].unwrap();
            for (rel, col) in [("batch", 2), ("batch_chunk", 3), ("time_index", 2), ("segment", 2)] {
                let low = state
                    .rows(artifact.rel_named(rel).unwrap())
                    .filter(|r| &r[0] == tid && int(&r[col]) < start)
                    .count();
                assert_eq!(
                    low, 0,
                    "seed {seed}: {} rows of {} below the log start {start}",
                    rel, TOPICS[t]
                );
            }
            // The replication log is compacted with the data (S8 D12): no data entry below the log start is left,
            // and the snapshot point is the start.
            let in_group = |r: &[Value]| matches!(&r[0], Value::Tuple(g) if &g[0] == tid);
            let low = state
                .rows(artifact.rel_named("rlog").unwrap())
                .filter(|r| in_group(r) && int(&r[4]) == 1 && int(&r[5]) < start)
                .count();
            assert_eq!(
                low, 0,
                "seed {seed}: {} replication log entries of {} below the log start",
                low, TOPICS[t]
            );
            let snap: Vec<i64> = state
                .rows(artifact.rel_named("rsnap").unwrap())
                .filter(|r| in_group(r))
                .map(|r| int(&r[3]))
                .collect();
            assert_eq!(snap, vec![start], "seed {seed}: {}'s snapshot point", TOPICS[t]);
        }
    }
}
