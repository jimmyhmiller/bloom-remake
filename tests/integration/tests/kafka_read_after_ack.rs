//! HD item 3: an `acks=all` produce is answered in the tick that sees Raft's commit index pass its entry, before the
//! entry is materialized into the partition log (that follows a tick later, so the answer waits for no durable write).
//! A read that reaches the broker right after the answer, in the tick that materializes the entry, must still find
//! the record: Fetch and ListOffsets read up to the commit index, the batches not materialized yet included.
//!
//! The simulator cannot place a request there (it runs a node until it is quiescent before delivering the next
//! message), so this drives one broker tick by tick with the manual driver: produce, run single ticks until the answer
//! is released, then send a Fetch and a ListOffsets and run exactly one more tick.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::durable::DurableSchema;
use blossom_node::manual::ManualDriver;
use blossom_node::recovery::{self, StoreSpec};
use blossom_node::streams::{HostRequest, Observed, host_request};
use blossom_node::{Executor, Node, NodeConfig};
use blossom_store::{OpenMode, SimFs, StoreIdentity, Vfs};
use blossom_value::time::{Instant, NodeId};
use blossom_value::value::ConnId;
use bytes::{Bytes, BytesMut};
use kafka_protocol::messages::create_topics_request::CreatableTopic;
use kafka_protocol::messages::fetch_request::{FetchPartition, FetchTopic};
use kafka_protocol::messages::list_offsets_request::{ListOffsetsPartition, ListOffsetsTopic};
use kafka_protocol::messages::produce_request::{PartitionProduceData, TopicProduceData};
use kafka_protocol::messages::{
    CreateTopicsRequest, CreateTopicsResponse, FetchRequest, FetchResponse, ListOffsetsRequest, ListOffsetsResponse,
    ProduceRequest, ProduceResponse, RequestHeader, ResponseHeader, TopicName,
};
use kafka_protocol::protocol::{Decodable, Encodable, HeaderVersion, StrBytes};
use kafka_protocol::records::{Compression, Record, RecordBatchEncoder, RecordEncodeOptions, TimestampType};

#[cfg(test)]
const TOPIC: &str = "acked";

#[cfg(test)]
fn identity() -> StoreIdentity {
    StoreIdentity {
        store_uuid: [0; 16],
        deployment_id: [1; 16],
        program_id: [2; 16],
        node_name: "b1".into(),
        principal: "spiffe://test/kafka/b1".into(),
        format: recovery::FORMAT,
        directory_digest: [3; 16],
    }
}

#[cfg(test)]
fn framed<M: Encodable + HeaderVersion>(key: i16, version: i16, corr: i32, body: &M) -> Vec<u8> {
    let mut buf = BytesMut::new();
    RequestHeader::default()
        .with_request_api_key(key)
        .with_request_api_version(version)
        .with_correlation_id(corr)
        .with_client_id(Some(StrBytes::from_string("ack".into())))
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
/// A batch of `n` records stamped `ts`, `ts + 1`, ...
fn batch(n: usize, ts: i64) -> Vec<u8> {
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
            timestamp: ts + i as i64,
            key: None,
            value: Some(Bytes::from(format!("record {i}"))),
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

/// One broker of `sim_cluster.bls`, on a simulated filesystem.
#[cfg(test)]
struct Broker {
    artifact: BlsArtifact,
    schema: DurableSchema,
    names: Arc<[Arc<str>]>,
}

#[cfg(test)]
impl Broker {
    fn new() -> Broker {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/kafka/sim_cluster.bls");
        let nodes = [
            NodeSpec {
                name: "b1".into(),
                role: Some("Broker".into()),
            },
            NodeSpec {
                name: "c1".into(),
                role: Some("Client".into()),
            },
        ];
        let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
        let artifact = result.unwrap_or_else(|e| panic!("sim_cluster.bls: {e:?}")).0;
        let schema = DurableSchema::of(artifact.program.get());
        let names: Arc<[Arc<str>]> = artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect();
        Broker {
            artifact,
            schema,
            names,
        }
    }

    fn boot(&self, fs: &SimFs) -> ManualDriver<Box<dyn Executor>> {
        let vfs: Arc<dyn Vfs> = Arc::new(fs.clone());
        let opened = recovery::open(
            vfs,
            &StoreSpec {
                dir: PathBuf::from("/data/b1"),
                identity: identity(),
                mode: OpenMode::InitFresh,
                certification: blossom_store::Certification::Strict,
                database: blossom_store::lsm::LsmOptions::default(),
            },
            &self.artifact.program,
            self.names.clone(),
            Instant(0),
            7,
        )
        .unwrap();
        let mut cfg = NodeConfig::new(NodeId(0), self.artifact.roles.first().copied().flatten());
        cfg.halt = self.artifact.halt;
        cfg.statics = blossom_integration_tests::kafka_brokers(&self.artifact).unwrap();
        let ecfg = blossom_engine::EngineConfig {
            roles: self.artifact.roles.clone(),
            node_names: self.names.to_vec(),
            seed: Some(blossom_value::Seed([9; 16])),
            externs: Arc::new(blossom_std_host::registry().unwrap()),
            ..blossom_engine::EngineConfig::default()
        };
        let exec: Box<dyn Executor> =
            Box::new(blossom_engine::Engine::new(self.artifact.program.clone(), NodeId(0), ecfg).unwrap());
        let node = Node::boot(cfg, &self.artifact.program, exec, opened.boot.clone()).unwrap();
        ManualDriver::new(
            node,
            &self.artifact.program,
            &self.schema,
            self.names.clone(),
            opened,
        )
    }
}

/// The broker's stream writes in released ticks, by connection.
#[cfg(test)]
fn writes(
    d: &ManualDriver<Box<dyn Executor>>,
    ticks: Vec<blossom_node::node::ReleasedTick>,
) -> Vec<(ConnId, Vec<u8>)> {
    let blobs = d.node.blobs();
    let mut out = Vec::new();
    for t in ticks {
        for h in t.host {
            if let HostRequest::Write { conn, bytes, .. } = host_request(d.node.streams(), &h, &blobs).unwrap() {
                out.push((conn, bytes));
            }
        }
    }
    out
}

/// A response frame's body (after its length).
#[cfg(test)]
fn body(frame: &[u8]) -> Bytes {
    let len = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    assert_eq!(frame.len(), 4 + len, "one whole response per write");
    Bytes::copy_from_slice(&frame[4..])
}

/// Runs the broker until quiescent at advancing instants, until a write to `conn` is released; returns it.
#[cfg(test)]
fn answer(d: &mut ManualDriver<Box<dyn Executor>>, now: &mut i64, conn: ConnId) -> Vec<u8> {
    for _ in 0..500 {
        *now += 10_000_000;
        let ticks = d.run_until_quiescent(Instant(*now)).unwrap();
        if let Some((_, b)) = writes(d, ticks).into_iter().find(|(c, _)| *c == conn) {
            return b;
        }
    }
    panic!("no answer on {conn:?}");
}

#[test]
fn a_read_in_the_tick_after_an_acks_all_answer_finds_the_record() {
    let k = Broker::new();
    let fs = SimFs::default();
    let mut d = k.boot(&fs);
    let stream = d.node.streams().iter().position(|s| &*s.name == "kafka").unwrap();
    let mut now = 0i64;
    // Settle: the controller elects itself and the broker catches up with it.
    for _ in 0..100 {
        now += 10_000_000;
        d.run_until_quiescent(Instant(now)).unwrap();
    }
    let (producer, reader, offsets) = (ConnId(1), ConnId(2), ConnId(3));
    let (search, greatest) = (ConnId(4), ConnId(5));
    for conn in [producer, reader, offsets, search, greatest] {
        d.node
            .observe_stream(Observed::Opened {
                stream,
                conn,
                peer: "peer".into(),
                req: None,
                at: Instant(now),
            })
            .unwrap();
    }
    let create = CreateTopicsRequest::default()
        .with_topics(vec![
            CreatableTopic::default()
                .with_name(topic_name())
                .with_num_partitions(1)
                .with_replication_factor(1),
        ])
        .with_timeout_ms(5_000);
    d.node
        .observe_stream(Observed::Bytes {
            conn: producer,
            bytes: framed(19, 7, 1, &create),
        })
        .unwrap();
    let mut b = body(&answer(&mut d, &mut now, producer));
    ResponseHeader::decode(&mut b, CreateTopicsResponse::header_version(7)).unwrap();
    let created = CreateTopicsResponse::decode(&mut b, 7).unwrap();
    assert_eq!(created.topics[0].error_code, 0, "{created:?}");
    let topic_id = created.topics[0].topic_id;
    // The partition's group elects its leader.
    for _ in 0..100 {
        now += 10_000_000;
        d.run_until_quiescent(Instant(now)).unwrap();
    }
    // Each batch's timestamps are later than every earlier batch's, so a search for its first timestamp, and for
    // the greatest, finds it: in the tick after its answer, among the batches not materialized yet.
    for (n, records) in [(0, 3usize), (1, 2)] {
        let ts = 1_700_000_000_000 + 1_000 * i64::from(n);
        let produce = ProduceRequest::default()
            .with_acks(-1)
            .with_timeout_ms(5_000)
            .with_topic_data(vec![
                TopicProduceData::default()
                    .with_name(topic_name())
                    .with_partition_data(vec![
                        PartitionProduceData::default()
                            .with_index(0)
                            .with_records(Some(Bytes::from(batch(records, ts)))),
                    ]),
            ]);
        d.node
            .observe_stream(Observed::Bytes {
                conn: producer,
                bytes: framed(0, 12, 10 + n, &produce),
            })
            .unwrap();
        // Single ticks until the answer is released.
        let mut acked = None;
        for _ in 0..200 {
            let ticks = d.run_one(Instant(now)).unwrap();
            if let Some((_, frame)) = writes(&d, ticks).into_iter().find(|(c, _)| *c == producer) {
                acked = Some(frame);
                break;
            }
        }
        let mut b = body(&acked.unwrap_or_else(|| panic!("produce {n} was not answered")));
        ResponseHeader::decode(&mut b, ProduceResponse::header_version(12)).unwrap();
        let r = ProduceResponse::decode(&mut b, 12).unwrap();
        let pr = &r.responses[0].partition_responses[0];
        assert_eq!(pr.error_code, 0, "produce {n}: {pr:?}");
        let base = pr.base_offset;
        // A Fetch at the acknowledged offset and a ListOffsets for the latest offset, in the very next tick.
        let fetch = FetchRequest::default()
            .with_max_bytes(1 << 16)
            .with_min_bytes(0)
            .with_max_wait_ms(0)
            .with_session_epoch(-1)
            .with_topics(vec![FetchTopic::default().with_topic_id(topic_id).with_partitions(
                vec![
                    FetchPartition::default()
                        .with_partition(0)
                        .with_current_leader_epoch(-1)
                        .with_fetch_offset(base)
                        .with_partition_max_bytes(1 << 16),
                ],
            )]);
        let list = |timestamp: i64| {
            ListOffsetsRequest::default()
                .with_replica_id(kafka_protocol::messages::BrokerId(-1))
                .with_topics(vec![
                    ListOffsetsTopic::default()
                        .with_name(topic_name())
                        .with_partitions(vec![
                            ListOffsetsPartition::default()
                                .with_partition_index(0)
                                .with_current_leader_epoch(-1)
                                .with_timestamp(timestamp),
                        ]),
                ])
        };
        d.node
            .observe_stream(Observed::Bytes {
                conn: reader,
                bytes: framed(1, 17, 20 + n, &fetch),
            })
            .unwrap();
        // Latest (-1), the batch's first timestamp, and the greatest timestamp (-3).
        for (conn, timestamp) in [(offsets, -1), (search, ts), (greatest, -3)] {
            d.node
                .observe_stream(Observed::Bytes {
                    conn,
                    bytes: framed(2, 7, 30 + n, &list(timestamp)),
                })
                .unwrap();
        }
        let ticks = d.run_one(Instant(now)).unwrap();
        let out = writes(&d, ticks);
        let read = out
            .iter()
            .find(|(c, _)| *c == reader)
            .unwrap_or_else(|| panic!("produce {n}: the Fetch was not answered in its tick"));
        let mut b = body(&read.1);
        ResponseHeader::decode(&mut b, FetchResponse::header_version(17)).unwrap();
        let f = FetchResponse::decode(&mut b, 17).unwrap();
        let fp = &f.responses[0].partitions[0];
        assert_eq!(fp.error_code, 0, "produce {n}: {fp:?}");
        assert_eq!(
            fp.high_watermark,
            base + records as i64,
            "produce {n}: the high watermark"
        );
        let got = fp.records.clone().unwrap_or_default();
        assert!(got.len() >= 8, "produce {n}: the Fetch at {base} returned no batch");
        assert_eq!(
            i64::from_be_bytes(got[0..8].try_into().unwrap()),
            base,
            "produce {n}: the first batch is not the acknowledged one"
        );
        let listed = |conn: ConnId| {
            let frame = out
                .iter()
                .find(|(c, _)| *c == conn)
                .unwrap_or_else(|| panic!("produce {n}: the ListOffsets on {conn:?} was not answered in its tick"));
            let mut b = body(&frame.1);
            ResponseHeader::decode(&mut b, ListOffsetsResponse::header_version(7)).unwrap();
            let l = ListOffsetsResponse::decode(&mut b, 7).unwrap();
            let lp = l.topics[0].partitions[0].clone();
            assert_eq!(lp.error_code, 0, "produce {n}: {lp:?}");
            lp
        };
        assert_eq!(
            listed(offsets).offset,
            base + records as i64,
            "produce {n}: the latest offset"
        );
        let found = listed(search);
        assert_eq!(
            (found.offset, found.timestamp),
            (base, ts),
            "produce {n}: the search by timestamp"
        );
        let max = listed(greatest);
        assert_eq!(
            (max.offset, max.timestamp),
            (base + records as i64 - 1, ts + records as i64 - 1),
            "produce {n}: the greatest timestamp"
        );
    }
}
