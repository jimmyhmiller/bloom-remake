//! Slice 7, item 6: Fetch and ListOffsets on the Blossom broker (`examples/kafka/fetch_node.bls`), in the cluster
//! simulator, with requests encoded and answers decoded by `kafka-protocol` (decision K5).
//!
//! A scripted client creates a topic and produces batches in every codec Kafka uses (none, gzip, snappy raw and in
//! the Java client's xerial framing, LZ4 frames, zstd), then checks, against what it produced:
//! - Fetch: a whole partition read back byte for byte, a fetch offset inside a batch, the byte limits (and the first
//!   batch kept whatever its size), a fetch at the log end that waits `max_wait_ms` and returns nothing, and the
//!   errors (offset out of range, unknown topic id and partition, leader epochs, fetch sessions);
//! - ListOffsets: earliest, latest, the greatest timestamp and a search by timestamp (record by record, inside
//!   compressed batches), unknown partitions and duplicate entries.
//!
//! A second client's long poll at the log end is answered as soon as a third client's batch lands, well before its
//! `max_wait_ms`.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::durable::DurableSchema;
use blossom_sim::cluster::{Cluster, ClusterConfig, NoKvClients, StreamAction, StreamClient, StreamEvent};
use blossom_value::Value;
use blossom_value::externs::ExternRegistry;
use blossom_value::time::NodeId;
use bytes::{BufMut, Bytes, BytesMut};
use kafka_protocol::messages::create_topics_request::CreatableTopic;
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

#[cfg(test)]
const TOPIC: &str = "f";

/// The codecs the batches are written in: Kafka's attribute bits and a name.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
enum Codec {
    None,
    Gzip,
    SnappyRaw,
    SnappyXerial,
    Lz4,
    Zstd,
}

#[cfg(test)]
const CODECS: [Codec; 6] = [
    Codec::None,
    Codec::Gzip,
    Codec::SnappyRaw,
    Codec::SnappyXerial,
    Codec::Lz4,
    Codec::Zstd,
];

/// Compresses with the standard library's host functions (the broker decompresses with the same codecs).
#[cfg(test)]
fn compress(reg: &ExternRegistry, codec: Codec, input: &[u8]) -> Vec<u8> {
    let call = |path: &str, args: Vec<Value>| -> Vec<u8> {
        match reg.lookup_fn(path).unwrap().call(&args).unwrap() {
            Value::Bytes(b) => b.to_vec(),
            other => panic!("{other:?}"),
        }
    };
    let b = Value::Bytes(Arc::from(input));
    match codec {
        Codec::None => input.to_vec(),
        Codec::Gzip => call(
            "blossom_std::compress::gzip_compress",
            vec![b, Value::Int(blossom_value::value::IntValue::U8(6))],
        ),
        Codec::SnappyRaw => call("blossom_std::compress::snappy_compress", vec![b]),
        Codec::SnappyXerial => {
            // The xerial framing: magic, version and compatibility words, then chunks of a length and a raw block
            // (two chunks here, so the framing's chunks are really walked).
            let mid = input.len() / 2;
            let mut out = b"\x82SNAPPY\x00".to_vec();
            out.extend(1i32.to_be_bytes());
            out.extend(1i32.to_be_bytes());
            for part in [&input[..mid], &input[mid..]] {
                let block = call(
                    "blossom_std::compress::snappy_compress",
                    vec![Value::Bytes(Arc::from(part))],
                );
                out.extend((block.len() as u32).to_be_bytes());
                out.extend(block);
            }
            out
        }
        Codec::Lz4 => call("blossom_std::compress::lz4_compress", vec![b]),
        Codec::Zstd => call("blossom_std::compress::zstd_compress", vec![b]),
    }
}

/// A magic-2 batch whose records have timestamps `ts` (base 1_700_000_000_000 + each), in `codec`.
#[cfg(test)]
fn batch(reg: &ExternRegistry, codec: Codec, tag: &str, ts: &[i64]) -> Vec<u8> {
    let records: Vec<Record> = ts
        .iter()
        .enumerate()
        .map(|(i, t)| Record {
            transactional: false,
            control: false,
            delete_horizon: false,
            partition_leader_epoch: -1,
            producer_id: -1,
            producer_epoch: -1,
            timestamp_type: TimestampType::Creation,
            offset: i as i64,
            sequence: i as i32 - 1,
            timestamp: 1_700_000_000_000 + t,
            key: Some(Bytes::from(format!("k{i}"))),
            value: Some(Bytes::from(format!("{tag}/{i}").repeat(1 + i % 3))),
            headers: Default::default(),
        })
        .collect();
    let compression = match codec {
        Codec::None => Compression::None,
        Codec::Gzip => Compression::Gzip,
        Codec::SnappyRaw | Codec::SnappyXerial => Compression::Snappy,
        Codec::Lz4 => Compression::Lz4,
        Codec::Zstd => Compression::Zstd,
    };
    let mut buf = BytesMut::new();
    RecordBatchEncoder::encode_with_custom_compression(
        &mut buf,
        &records,
        &RecordEncodeOptions {
            version: 2,
            compression,
        },
        Some(|input: &mut BytesMut, out: &mut BytesMut, _c: Compression| {
            out.put_slice(&compress(reg, codec, input));
            Ok(())
        }),
    )
    .unwrap();
    buf.to_vec()
}

/// A batch as stored at `base`: `baseOffset` written, `partitionLeaderEpoch` 0.
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
        .with_client_id(Some(StrBytes::from_string("fetch".into())))
        .encode(&mut buf, M::header_version(version))
        .unwrap();
    body.encode(&mut buf, version).unwrap();
    let mut out = (buf.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&buf);
    out
}

#[cfg(test)]
fn name() -> TopicName {
    TopicName(StrBytes::from_string(TOPIC.into()))
}

/// A produced batch: its base offset, its bytes and its records (offset, timestamp).
#[cfg(test)]
type Produced = (i64, Vec<u8>, Vec<(i64, i64)>);

/// A Fetch answer's partition: error, high watermark, log start, records.
#[cfg(test)]
type FetchedPartition = (i16, i64, i64, Vec<u8>);

/// When a long poll was sent, what it read, and when it was answered.
#[cfg(test)]
type Polled = Rc<RefCell<Option<(i64, Vec<u8>, i64)>>>;

/// A late batch's bytes, base offset, and when it was sent.
#[cfg(test)]
type Late = Rc<RefCell<Option<(Vec<u8>, i64, i64)>>>;

/// A step's request, given what was produced and a correlation id.
#[cfg(test)]
type Request = Box<dyn Fn(&Log, i32) -> Vec<u8>>;

/// What the script produced: per partition, each batch's base offset, bytes and records (offset, timestamp).
#[cfg(test)]
#[derive(Default, Clone)]
struct Log {
    batches: Vec<Vec<Produced>>,
    topic_id: [u8; 16],
}

#[cfg(test)]
impl Log {
    fn end(&self, p: usize) -> i64 {
        self.batches[p].last().map(|b| b.2.last().unwrap().0 + 1).unwrap_or(0)
    }
    /// The partition's bytes from the batch holding `offset` on.
    fn from(&self, p: usize, offset: i64) -> Vec<u8> {
        self.batches[p]
            .iter()
            .filter(|b| b.2.last().unwrap().0 >= offset)
            .flat_map(|b| stamped(&b.1, b.0))
            .collect()
    }
}

/// One scripted step: the request to send (given what was produced), and the check of its answer.
#[cfg(test)]
type Check = Box<dyn Fn(&Log, Bytes, i64, i64) -> Result<(), String>>;

#[cfg(test)]
struct Step {
    request: Request,
    check: Check,
}

#[cfg(test)]
fn fetch_request(
    log: &Log,
    corr: i32,
    parts: &[(i32, i64, i32)],
    max_bytes: i32,
    min_bytes: i32,
    wait: i32,
) -> Vec<u8> {
    let req = FetchRequest::default()
        .with_max_wait_ms(wait)
        .with_min_bytes(min_bytes)
        .with_max_bytes(max_bytes)
        .with_session_epoch(-1)
        .with_topics(vec![
            FetchTopic::default()
                .with_topic_id(uuid::Uuid::from_bytes(log.topic_id))
                .with_partitions(
                    parts
                        .iter()
                        .map(|(p, off, pmax)| {
                            FetchPartition::default()
                                .with_partition(*p)
                                .with_current_leader_epoch(0)
                                .with_fetch_offset(*off)
                                .with_partition_max_bytes(*pmax)
                        })
                        .collect(),
                ),
        ]);
    framed(1, 17, corr, &req)
}

/// A Fetch answer's partitions: (error, high watermark, log start, records).
#[cfg(test)]
fn fetch_answer(mut body: Bytes) -> Result<(i16, Vec<FetchedPartition>), String> {
    ResponseHeader::decode(&mut body, FetchResponse::header_version(17)).map_err(|e| e.to_string())?;
    let r = FetchResponse::decode(&mut body, 17).map_err(|e| e.to_string())?;
    if !body.is_empty() {
        return Err("bytes after a Fetch answer".into());
    }
    Ok((
        r.error_code,
        r.responses
            .iter()
            .flat_map(|t| t.partitions.iter())
            .map(|p| {
                (
                    p.error_code,
                    p.high_watermark,
                    p.log_start_offset,
                    p.records.clone().unwrap_or_default().to_vec(),
                )
            })
            .collect(),
    ))
}

#[cfg(test)]
fn list_request(corr: i32, topic: &str, parts: &[(i32, i64)]) -> Vec<u8> {
    let req = ListOffsetsRequest::default()
        .with_replica_id(BrokerId(-1))
        .with_topics(vec![
            ListOffsetsTopic::default()
                .with_name(TopicName(StrBytes::from_string(topic.into())))
                .with_partitions(
                    parts
                        .iter()
                        .map(|(p, t)| {
                            ListOffsetsPartition::default()
                                .with_partition_index(*p)
                                .with_current_leader_epoch(-1)
                                .with_timestamp(*t)
                        })
                        .collect(),
                ),
        ])
        .with_timeout_ms(1000);
    framed(2, 10, corr, &req)
}

/// A ListOffsets answer's partitions: (error, timestamp, offset).
#[cfg(test)]
fn list_answer(mut body: Bytes) -> Result<Vec<(i16, i64, i64)>, String> {
    ResponseHeader::decode(&mut body, ListOffsetsResponse::header_version(10)).map_err(|e| e.to_string())?;
    let r = ListOffsetsResponse::decode(&mut body, 10).map_err(|e| e.to_string())?;
    Ok(r.topics
        .iter()
        .flat_map(|t| t.partitions.iter())
        .map(|p| (p.error_code, p.timestamp, p.offset))
        .collect())
}

#[cfg(test)]
fn expect<T: PartialEq + std::fmt::Debug>(what: &str, got: T, want: T) -> Result<(), String> {
    if got == want {
        Ok(())
    } else {
        Err(format!("{what}: got {got:?}, want {want:?}"))
    }
}

/// The script's steps after the topic exists and its batches are produced.
#[cfg(test)]
fn reads() -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    let t0 = 1_700_000_000_000i64;
    // Each partition read whole, byte for byte.
    for p in 0..2i32 {
        steps.push(Step {
            request: Box::new(move |log, corr| fetch_request(log, corr, &[(p, 0, 1 << 20)], 1 << 24, 1, 0)),
            check: Box::new(move |log, b, _, _| {
                let (e, parts) = fetch_answer(b)?;
                let pu = p as usize;
                expect("error", e, 0)?;
                expect("partition", parts[0].clone(), (0, log.end(pu), 0, log.from(pu, 0)))
            }),
        });
    }
    // A fetch offset inside a batch returns from the batch holding it.
    // (The last record of the last batch with several: an offset that starts no batch.)
    let inside = |log: &Log| -> i64 {
        let b = log.batches[0].iter().rev().find(|b| b.2.len() > 1).unwrap();
        b.2.last().unwrap().0
    };
    steps.push(Step {
        request: Box::new(move |log, corr| fetch_request(log, corr, &[(0, inside(log), 1 << 20)], 1 << 24, 1, 0)),
        check: Box::new(move |log, b, _, _| {
            let (_, parts) = fetch_answer(b)?;
            let off = inside(log);
            if log.batches[0].iter().any(|b| b.0 == off) {
                return Err(format!("offset {off} starts a batch"));
            }
            expect("records from inside a batch", parts[0].3.clone(), log.from(0, off))
        }),
    });
    // Byte limits: one byte per partition keeps only the response's first batch; a response limit of one byte too.
    steps.push(Step {
        request: Box::new(|log, corr| fetch_request(log, corr, &[(0, 0, 1), (1, 0, 1)], 1 << 24, 1, 0)),
        check: Box::new(|log, b, _, _| {
            let (_, parts) = fetch_answer(b)?;
            let first = stamped(&log.batches[0][0].1, log.batches[0][0].0);
            expect("first partition", parts[0].3.clone(), first)?;
            expect("second partition", parts[1].3.clone(), Vec::new())
        }),
    });
    steps.push(Step {
        request: Box::new(|log, corr| fetch_request(log, corr, &[(1, 0, 1 << 20), (0, 0, 1 << 20)], 1, 1, 0)),
        check: Box::new(|log, b, _, _| {
            let (_, parts) = fetch_answer(b)?;
            let first = stamped(&log.batches[1][0].1, log.batches[1][0].0);
            expect("first partition", parts[0].3.clone(), first)?;
            expect("second partition", parts[1].3.clone(), Vec::new())
        }),
    });
    // At the log end with min_bytes 1: nothing arrives, so the answer is empty after max_wait_ms.
    steps.push(Step {
        request: Box::new(|log, corr| fetch_request(log, corr, &[(0, log.end(0), 1 << 20)], 1 << 24, 1, 200)),
        check: Box::new(|log, b, sent, got| {
            let (_, parts) = fetch_answer(b)?;
            expect("waited answer", parts[0].clone(), (0, log.end(0), 0, Vec::new()))?;
            if got - sent < 200_000_000 {
                return Err(format!(
                    "the long poll answered after {} ns, before max_wait_ms",
                    got - sent
                ));
            }
            Ok(())
        }),
    });
    // Errors: out of range, unknown partition, leader epochs, unknown topic id, a session this broker never made.
    steps.push(Step {
        request: Box::new(|log, corr| {
            fetch_request(log, corr, &[(0, log.end(0) + 1, 100), (9, 0, 100)], 1 << 24, 1, 0)
        }),
        check: Box::new(|log, b, _, _| {
            let (_, parts) = fetch_answer(b)?;
            expect("out of range", (parts[0].0, parts[0].1), (1, log.end(0)))?;
            expect("unknown partition", parts[1].0, 3)
        }),
    });
    steps.push(Step {
        request: Box::new(|log, corr| {
            let req = FetchRequest::default()
                .with_max_bytes(1000)
                .with_session_epoch(-1)
                .with_topics(vec![
                    FetchTopic::default()
                        .with_topic_id(uuid::Uuid::from_bytes(log.topic_id))
                        .with_partitions(vec![
                            FetchPartition::default()
                                .with_partition(0)
                                .with_current_leader_epoch(5)
                                .with_partition_max_bytes(10),
                            FetchPartition::default()
                                .with_partition(1)
                                .with_current_leader_epoch(-3)
                                .with_partition_max_bytes(10),
                        ]),
                    FetchTopic::default()
                        .with_topic_id(uuid::Uuid::from_u128(77))
                        .with_partitions(vec![
                            FetchPartition::default()
                                .with_partition(0)
                                .with_current_leader_epoch(-1)
                                .with_partition_max_bytes(10),
                        ]),
                ]);
            framed(1, 16, corr, &req)
        }),
        check: Box::new(|_, b, _, _| {
            let (_, parts) = fetch_answer(b)?;
            expect(
                "errors",
                parts.iter().map(|p| p.0).collect::<Vec<_>>(),
                vec![75, 74, 100],
            )
        }),
    });
    steps.push(Step {
        request: Box::new(|log, corr| {
            let req = FetchRequest::default()
                .with_session_id(7)
                .with_session_epoch(3)
                .with_topics(vec![
                    FetchTopic::default().with_topic_id(uuid::Uuid::from_bytes(log.topic_id)),
                ]);
            framed(1, 17, corr, &req)
        }),
        check: Box::new(|_, b, _, _| {
            let (e, parts) = fetch_answer(b)?;
            expect("session error", (e, parts.len()), (70, 0))
        }),
    });
    // ListOffsets, one query per request (a request naming a partition twice is refused, below): earliest,
    // latest, earliest local, latest tiered, and the greatest timestamp.
    let ends: [(i32, i64); 5] = [(0, -2), (0, -1), (0, -4), (0, -5), (1, -3)];
    for (p, t) in ends {
        steps.push(Step {
            request: Box::new(move |_, corr| list_request(corr, TOPIC, &[(p, t)])),
            check: Box::new(move |log, b, _, _| {
                let got = list_answer(b)?;
                let (mts, moff) = log.batches[p as usize]
                    .iter()
                    .flat_map(|b| b.2.iter().copied())
                    .fold((i64::MIN, -1), |acc, (o, ts)| if ts > acc.0 { (ts, o) } else { acc });
                let want = match t {
                    -2 | -4 => (0, -1, 0),
                    -1 => (0, -1, log.end(p as usize)),
                    -5 => (0, -1, -1),
                    _ => (0, mts, moff),
                };
                expect(&format!("timestamp {t}"), got, vec![want])
            }),
        });
    }
    // Searches by timestamp: before everything, a record's exact timestamp, just after it, after everything.
    for pick in 0..4usize {
        let target = move |log: &Log| -> i64 {
            let recs: Vec<(i64, i64)> = log.batches[0].iter().flat_map(|b| b.2.iter().copied()).collect();
            let exact = recs[recs.len() / 2].1;
            [t0 - 5, exact, exact + 1, t0 + 1_000_000][pick]
        };
        steps.push(Step {
            request: Box::new(move |log, corr| list_request(corr, TOPIC, &[(0, target(log))])),
            check: Box::new(move |log, b, _, _| {
                let got = list_answer(b)?;
                let t = target(log);
                // Kafka: the first batch whose greatest timestamp reaches t, then its first record at or after t.
                let want = log.batches[0]
                    .iter()
                    .find(|b| b.2.iter().map(|r| r.1).max().unwrap() >= t)
                    .and_then(|b| b.2.iter().find(|r| r.1 >= t))
                    .map(|r| (0, r.1, r.0))
                    .unwrap_or((0, -1, -1));
                expect(&format!("search {t}"), got, vec![want])
            }),
        });
    }
    // A search into every batch of both partitions (every codec's records parsed): just below its greatest
    // timestamp.
    for p in 0..2usize {
        for k in 0..9usize {
            let target = move |log: &Log| -> i64 { log.batches[p][k].2.iter().map(|r| r.1).max().unwrap() - 5 };
            steps.push(Step {
                request: Box::new(move |log, corr| list_request(corr, TOPIC, &[(p as i32, target(log))])),
                check: Box::new(move |log, b, _, _| {
                    let got = list_answer(b)?;
                    let t = target(log);
                    let want = log.batches[p]
                        .iter()
                        .find(|b| b.2.iter().map(|r| r.1).max().unwrap() >= t)
                        .and_then(|b| b.2.iter().find(|r| r.1 >= t))
                        .map(|r| (0, r.1, r.0))
                        .unwrap_or((0, -1, -1));
                    expect(&format!("search {t} in partition {p}, batch {k}"), got, vec![want])
                }),
            });
        }
    }
    steps.push(Step {
        request: Box::new(|_, corr| {
            let req = ListOffsetsRequest::default()
                .with_replica_id(BrokerId(-1))
                .with_topics(vec![
                    ListOffsetsTopic::default().with_name(name()).with_partitions(vec![
                        ListOffsetsPartition::default()
                            .with_partition_index(0)
                            .with_current_leader_epoch(-1)
                            .with_timestamp(-1),
                        ListOffsetsPartition::default()
                            .with_partition_index(0)
                            .with_current_leader_epoch(-1)
                            .with_timestamp(-2),
                        ListOffsetsPartition::default()
                            .with_partition_index(5)
                            .with_current_leader_epoch(-1)
                            .with_timestamp(-1),
                    ]),
                    ListOffsetsTopic::default()
                        .with_name(TopicName(StrBytes::from_string("nope".into())))
                        .with_partitions(vec![
                            ListOffsetsPartition::default()
                                .with_partition_index(0)
                                .with_current_leader_epoch(-1)
                                .with_timestamp(-1),
                        ]),
                ]);
            framed(2, 7, corr, &req)
        }),
        check: Box::new(|_, b, _, _| {
            let got = list_answer(b)?;
            expect(
                "errors",
                got.iter().map(|x| x.0).collect::<Vec<_>>(),
                vec![42, 42, 3, 3],
            )
        }),
    });
    steps
}

/// The scripted client: create, produce the batches, learn the topic id, then run the reads.
#[cfg(test)]
struct Script {
    log: Rc<RefCell<Log>>,
    errors: Rc<RefCell<Vec<String>>>,
    /// The batches to produce: partition, bytes.
    to_produce: Vec<(usize, Vec<u8>, Vec<i64>)>,
    produced: usize,
    phase: u8,
    steps: Vec<Step>,
    step: usize,
    connected: bool,
    buf: Vec<u8>,
    corr: i32,
    sent_at: i64,
    done: Rc<RefCell<bool>>,
}

#[cfg(test)]
impl Script {
    fn next(&mut self, a: &mut StreamAction, now: i64) {
        self.corr += 1;
        self.sent_at = now;
        match self.phase {
            0 => {
                let req = CreateTopicsRequest::default()
                    .with_topics(vec![
                        CreatableTopic::default()
                            .with_name(name())
                            .with_num_partitions(2)
                            .with_replication_factor(1),
                    ])
                    .with_timeout_ms(1000);
                a.send = framed(19, 7, self.corr, &req);
            }
            1 => {
                let (p, b, _) = &self.to_produce[self.produced];
                let req = ProduceRequest::default()
                    .with_acks(-1)
                    .with_timeout_ms(1000)
                    .with_topic_data(vec![TopicProduceData::default().with_name(name()).with_partition_data(
                        vec![
                            PartitionProduceData::default()
                                .with_index(*p as i32)
                                .with_records(Some(Bytes::from(b.clone()))),
                        ],
                    )]);
                a.send = framed(0, 11, self.corr, &req);
            }
            2 => {
                let req = MetadataRequest::default()
                    .with_topics(Some(vec![MetadataRequestTopic::default().with_name(Some(name()))]));
                a.send = framed(3, 13, self.corr, &req);
            }
            _ => {
                if let Some(s) = self.steps.get(self.step) {
                    a.send = (s.request)(&self.log.borrow(), self.corr);
                } else {
                    *self.done.borrow_mut() = true;
                }
            }
        }
    }

    fn answer(&mut self, body: Bytes, now: i64) -> Result<(), String> {
        let mut b = body.clone();
        match self.phase {
            0 => {
                ResponseHeader::decode(&mut b, CreateTopicsResponse::header_version(7)).map_err(|e| e.to_string())?;
                let r = CreateTopicsResponse::decode(&mut b, 7).map_err(|e| e.to_string())?;
                expect("create", r.topics[0].error_code, 0)?;
                self.phase = 1;
            }
            1 => {
                ResponseHeader::decode(&mut b, ProduceResponse::header_version(11)).map_err(|e| e.to_string())?;
                let r = ProduceResponse::decode(&mut b, 11).map_err(|e| e.to_string())?;
                let pr = &r.responses[0].partition_responses[0];
                expect("produce", pr.error_code, 0)?;
                let (p, bytes, ts) = self.to_produce[self.produced].clone();
                let base = pr.base_offset;
                let recs = ts
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (base + i as i64, 1_700_000_000_000 + t))
                    .collect();
                self.log.borrow_mut().batches[p].push((base, bytes, recs));
                self.produced += 1;
                if self.produced == self.to_produce.len() {
                    self.phase = 2;
                }
            }
            2 => {
                ResponseHeader::decode(&mut b, MetadataResponse::header_version(13)).map_err(|e| e.to_string())?;
                let r = MetadataResponse::decode(&mut b, 13).map_err(|e| e.to_string())?;
                self.log.borrow_mut().topic_id = *r.topics[0].topic_id.as_bytes();
                self.phase = 3;
            }
            _ => {
                let s = &self.steps[self.step];
                if let Err(e) = (s.check)(&self.log.borrow(), body, self.sent_at, now) {
                    self.errors.borrow_mut().push(format!("step {}: {e}", self.step));
                }
                self.step += 1;
            }
        }
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
            StreamEvent::Wake => {}
            StreamEvent::Opened => self.next(&mut a, now),
            StreamEvent::Received(b) => {
                self.buf.extend_from_slice(b);
                if let Some(body) = one_frame(&mut self.buf) {
                    self.answer(body, now)?;
                    self.next(&mut a, now);
                }
            }
            StreamEvent::Closed(why) => return Err(format!("the connection closed: {why}")),
        }
        Ok(a)
    }
}

/// A consumer that long-polls at the end of partition 1 once the script is done, and records when its answer came.
#[cfg(test)]
struct Poller {
    log: Rc<RefCell<Log>>,
    ready: Rc<RefCell<bool>>,
    connected: bool,
    polled_at: Option<i64>,
    buf: Vec<u8>,
    result: Polled,
}

#[cfg(test)]
impl StreamClient for Poller {
    fn on(&mut self, now: i64, e: StreamEvent<'_>) -> Result<StreamAction, String> {
        let mut a = StreamAction::default();
        match e {
            StreamEvent::Wake if !*self.ready.borrow() => a.wake = Some(now + 10_000_000),
            StreamEvent::Wake if !self.connected => {
                self.connected = true;
                a.connect = Some((NodeId(0), Arc::from("kafka")));
            }
            StreamEvent::Wake => {}
            StreamEvent::Opened => {
                let log = self.log.borrow();
                a.send = fetch_request(&log, 1, &[(1, log.end(1), 1 << 20)], 1 << 24, 1, 5_000);
                self.polled_at = Some(now);
            }
            StreamEvent::Received(b) => {
                self.buf.extend_from_slice(b);
                if let Some(body) = one_frame(&mut self.buf) {
                    let (_, parts) = fetch_answer(body)?;
                    *self.result.borrow_mut() = Some((self.polled_at.unwrap_or(0), parts[0].3.clone(), now));
                }
            }
            StreamEvent::Closed(why) => return Err(format!("the poller's connection closed: {why}")),
        }
        Ok(a)
    }
}

/// A producer that, 300 ms after the poller starts, appends one batch to partition 1 and records when it was told.
#[cfg(test)]
struct LateProducer {
    reg: Arc<ExternRegistry>,
    ready: Rc<RefCell<bool>>,
    start: Option<i64>,
    connected: bool,
    buf: Vec<u8>,
    sent: Late,
}

#[cfg(test)]
impl StreamClient for LateProducer {
    fn on(&mut self, now: i64, e: StreamEvent<'_>) -> Result<StreamAction, String> {
        let mut a = StreamAction::default();
        match e {
            StreamEvent::Wake if !*self.ready.borrow() => a.wake = Some(now + 10_000_000),
            StreamEvent::Wake if self.start.is_none() => {
                self.start = Some(now);
                a.wake = Some(now + 300_000_000);
            }
            StreamEvent::Wake if !self.connected => {
                self.connected = true;
                a.connect = Some((NodeId(0), Arc::from("kafka")));
            }
            StreamEvent::Wake => {}
            StreamEvent::Opened => {
                let b = batch(&self.reg, Codec::None, "late", &[5_000]);
                let req = ProduceRequest::default()
                    .with_acks(-1)
                    .with_timeout_ms(1000)
                    .with_topic_data(vec![TopicProduceData::default().with_name(name()).with_partition_data(
                        vec![
                        PartitionProduceData::default().with_index(1).with_records(Some(Bytes::from(b.clone()))),
                    ],
                    )]);
                a.send = framed(0, 12, 1, &req);
                *self.sent.borrow_mut() = Some((b, 0, now));
            }
            StreamEvent::Received(b) => {
                self.buf.extend_from_slice(b);
                if let Some(mut body) = one_frame(&mut self.buf) {
                    ResponseHeader::decode(&mut body, ProduceResponse::header_version(12))
                        .map_err(|e| e.to_string())?;
                    let r = ProduceResponse::decode(&mut body, 12).map_err(|e| e.to_string())?;
                    let base = r.responses[0].partition_responses[0].base_offset;
                    if let Some(s) = self.sent.borrow_mut().as_mut() {
                        s.1 = base;
                    }
                }
            }
            StreamEvent::Closed(why) => return Err(format!("the producer's connection closed: {why}")),
        }
        Ok(a)
    }
}

#[test]
fn fetch_and_list_offsets_read_what_was_produced() {
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
    let reg = Arc::new(blossom_std_host::registry().unwrap());
    for seed in 1..=3u64 {
        let cfg = ClusterConfig {
            seed,
            clients: 0,
            chunk_max: 100 + seed as usize * 300,
            duration: 3_000_000_000,
            externs: reg.clone(),
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
        // Batches in every codec, spread over two partitions; timestamps rise, but not always within a batch.
        let mut to_produce = Vec::new();
        for (k, codec) in CODECS.iter().cycle().take(18).enumerate() {
            let base = k as i64 * 100;
            let ts: Vec<i64> = (0..1 + (k % 4) as i64)
                .map(|i| base + [0, 30, 10, 60][i as usize])
                .collect();
            to_produce.push((k % 2, batch(&reg, *codec, &format!("b{k}"), &ts), ts));
        }
        let log = Rc::new(RefCell::new(Log {
            batches: vec![Vec::new(), Vec::new()],
            topic_id: [0; 16],
        }));
        let errors = Rc::new(RefCell::new(Vec::new()));
        let done = Rc::new(RefCell::new(false));
        cluster.stream_client(Box::new(Script {
            log: log.clone(),
            errors: errors.clone(),
            to_produce,
            produced: 0,
            phase: 0,
            steps: reads(),
            step: 0,
            connected: false,
            buf: Vec::new(),
            corr: 0,
            sent_at: 0,
            done: done.clone(),
        }));
        let polled = Rc::new(RefCell::new(None));
        let late = Rc::new(RefCell::new(None));
        cluster.stream_client(Box::new(Poller {
            log: log.clone(),
            ready: done.clone(),
            connected: false,
            polled_at: None,
            buf: Vec::new(),
            result: polled.clone(),
        }));
        cluster.stream_client(Box::new(LateProducer {
            reg: reg.clone(),
            ready: done.clone(),
            start: None,
            connected: false,
            buf: Vec::new(),
            sent: late.clone(),
        }));
        cluster.run_until(3_000_000_000).unwrap();
        let run = cluster.run_so_far();
        assert!(
            run.violation.is_none(),
            "seed {seed}: {:?}\n{}",
            run.violation,
            run.log.join("\n")
        );
        assert!(*done.borrow(), "seed {seed}: the script did not finish");
        assert!(errors.borrow().is_empty(), "seed {seed}: {:#?}", errors.borrow());
        // The long poll was answered with the late batch, soon after it landed and long before its 5 s wait.
        let (polled_at, records, answered) = polled.borrow().clone().expect("the poll was answered");
        let (bytes, base, sent) = late.borrow().clone().expect("the late batch was sent");
        assert_eq!(records, stamped(&bytes, base), "seed {seed}: the long poll's records");
        assert!(answered >= sent, "seed {seed}: answered before the batch was sent");
        assert!(
            answered - polled_at < 1_000_000_000,
            "seed {seed}: the long poll waited {} ns",
            answered - polled_at
        );
    }
}
