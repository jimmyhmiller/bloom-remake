//! Slice 6, items 5 and 6: the Blossom Kafka library (`examples/kafka/`) against an independent Rust implementation of
//! the protocol, the `kafka-protocol` crate (test-only, decision K5), through the harness `fixtures/kafka/codec.bls`
//! on the oracle and the engine (which must agree).
//!
//! - Requests the Rust implementation encodes, at every version the broker answers and with random contents, decode
//!   in Blossom to the same header and body.
//! - Responses Blossom encodes, from random fields, decode in the Rust implementation to those fields.
//! - The golden captures of real clients (kcat, the Java tools, franz-go) decode.
//! - Frame splitting finds exactly the complete frames of random streams.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::EngineEvaluator;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_sim::sync::SyncRun;
use blossom_value::Value;
use blossom_value::time::{Duration, NodeId, Tick};
use blossom_value::value::IntValue;
use bytes::{Bytes, BytesMut};
use kafka_protocol::messages::create_topics_request::{
    CreatableReplicaAssignment, CreatableTopic, CreatableTopicConfig,
};
use kafka_protocol::messages::delete_topics_request::DeleteTopicState;
use kafka_protocol::messages::describe_configs_request::DescribeConfigsResource;
use kafka_protocol::messages::metadata_request::MetadataRequestTopic;
use kafka_protocol::messages::produce_request::{PartitionProduceData, TopicProduceData};
use kafka_protocol::messages::{
    ApiVersionsRequest, ApiVersionsResponse, BrokerId, CreateTopicsRequest, CreateTopicsResponse, DeleteTopicsRequest,
    DeleteTopicsResponse, DescribeConfigsRequest, DescribeConfigsResponse, MetadataRequest, MetadataResponse,
    RequestHeader, ResponseHeader, TopicName,
};
use kafka_protocol::messages::{ProduceRequest, ProduceResponse};
use kafka_protocol::protocol::{Decodable, Encodable, HeaderVersion, StrBytes};

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
    fn pick_i16<'a>(&mut self, xs: &'a [i16]) -> &'a i16 {
        &xs[self.below(xs.len() as u64) as usize]
    }
    fn text(&mut self) -> String {
        let alphabet = ['a', 'b', '-', '.', '_', 'z', '9', 'é', '日'];
        (0..self.below(12))
            .map(|_| alphabet[self.below(alphabet.len() as u64) as usize])
            .collect()
    }
}

#[cfg(test)]
fn compile() -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/kafka/codec.bls");
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    result.unwrap_or_else(|e| panic!("codec.bls: {e:?}")).0
}

/// Runs the harness on the oracle and the engine (which must agree), with the standard host functions, and returns
/// the oracle's run.
#[cfg(test)]
fn run(artifact: &BlsArtifact, inputs: &[InputEvent]) -> SyncRun {
    let externs = Arc::new(blossom_std_host::registry().unwrap());
    let sim = BlsSim::with_externs(artifact, blossom_value::Seed::from_u64(0), externs.clone()).unwrap();
    let round = Duration::from_nanos(1_000_000_000);
    let reference = sim
        .run(inputs, Tick(1), round, &FaultSchedule::default(), false)
        .unwrap();
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        externs,
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let mine = sim
        .run_on(&engine, inputs, Tick(1), round, &FaultSchedule::default(), false)
        .unwrap();
    for (t, (a, b)) in reference.rounds.iter().zip(&mine.rounds).enumerate() {
        assert_eq!(
            a[0].instance, b[0].instance,
            "tick {t}: the oracle and the engine differ"
        );
    }
    reference
}

#[cfg(test)]
fn input(artifact: &BlsArtifact, rel: &str, row: Vec<Value>) -> InputEvent {
    InputEvent {
        node: NodeId(0),
        tick: Tick(1),
        rel: artifact.rel_named(rel).unwrap(),
        row: Arc::from(row),
    }
}

/// The rows of view `name` at tick 1.
#[cfg(test)]
fn rows(artifact: &BlsArtifact, run: &SyncRun, name: &str) -> Vec<Vec<Value>> {
    run.node_tick(Tick(1), NodeId(0))
        .unwrap()
        .instance
        .rows(artifact.rel_named(name).unwrap())
        .map(|r| r.to_vec())
        .collect()
}

#[cfg(test)]
fn i8v(x: i8) -> Value {
    Value::Int(IntValue::I8(x))
}
#[cfg(test)]
fn i16v(x: i16) -> Value {
    Value::Int(IntValue::I16(x))
}
#[cfg(test)]
fn i32v(x: i32) -> Value {
    Value::Int(IntValue::I32(x))
}
#[cfg(test)]
fn s(x: &str) -> Value {
    Value::Str(x.into())
}
#[cfg(test)]
fn opt(x: Option<Value>) -> Value {
    Value::Option(x.map(Arc::new))
}
#[cfg(test)]
fn bytes(b: &[u8]) -> Value {
    Value::Bytes(Arc::from(b))
}
#[cfg(test)]
fn strukt(fields: Vec<Value>) -> Value {
    Value::Struct(fields.into())
}

/// Kafka's topic name rule: 1 to 249 characters of `[a-zA-Z0-9._-]`, not `.` or `..`.
#[cfg(test)]
fn legal_topic_name(n: &str) -> bool {
    (1..=249).contains(&n.len())
        && n != "."
        && n != ".."
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// A request encoded by the Rust implementation: header then body, without the size prefix.
#[cfg(test)]
fn encode_request<M: Encodable + HeaderVersion>(
    key: i16,
    version: i16,
    corr: i32,
    client: Option<&str>,
    body: &M,
) -> Vec<u8> {
    let mut buf = BytesMut::new();
    let header = RequestHeader::default()
        .with_request_api_key(key)
        .with_request_api_version(version)
        .with_correlation_id(corr)
        .with_client_id(client.map(|c| StrBytes::from_string(c.to_owned())));
    header.encode(&mut buf, M::header_version(version)).unwrap();
    body.encode(&mut buf, version).unwrap();
    buf.to_vec()
}

/// The Blossom value of a decoded request header.
#[cfg(test)]
fn header_value(key: i16, version: i16, corr: i32, client: Option<&str>) -> Value {
    opt(Some(strukt(vec![
        i16v(key),
        i16v(version),
        i32v(corr),
        opt(client.map(s)),
    ])))
}

#[test]
fn requests_encoded_by_the_rust_implementation_decode_in_blossom() {
    let artifact = compile();
    let mut rng = Rng(5);
    let mut inputs = Vec::new();
    // Per request frame: its header and body as Blossom should decode them.
    let mut expected: Vec<(Vec<u8>, Value, &str, Value)> = Vec::new();
    for i in 0..200 {
        let corr = rng.next() as i32;
        let client = if rng.below(4) == 0 { None } else { Some(rng.text()) };
        if i % 2 == 0 {
            let version = 3 + (rng.below(2) as i16);
            let (name, ver) = (rng.text(), rng.text());
            let req = ApiVersionsRequest::default()
                .with_client_software_name(StrBytes::from_string(name.clone()))
                .with_client_software_version(StrBytes::from_string(ver.clone()));
            let frame = encode_request(18, version, corr, client.as_deref(), &req);
            let body = opt(Some(strukt(vec![s(&name), s(&ver)])));
            expected.push((
                frame.clone(),
                header_value(18, version, corr, client.as_deref()),
                "v_apiv",
                body,
            ));
            inputs.push(input(&artifact, "req", vec![bytes(&frame)]));
        } else {
            let topics = if rng.below(4) == 0 {
                None
            } else {
                Some(
                    (0..rng.below(5))
                        .map(|_| {
                            let id = uuid::Uuid::from_u128(u128::from(rng.next()) << 64 | u128::from(rng.next()));
                            let name = if rng.below(5) == 0 { None } else { Some(rng.text()) };
                            (id, name)
                        })
                        .collect::<Vec<_>>(),
                )
            };
            let (auto, ops) = (rng.below(2) == 0, rng.below(2) == 0);
            let req = MetadataRequest::default()
                .with_topics(topics.as_ref().map(|ts| {
                    ts.iter()
                        .map(|(id, name)| {
                            MetadataRequestTopic::default()
                                .with_topic_id(*id)
                                .with_name(name.as_ref().map(|n| TopicName(StrBytes::from_string(n.clone()))))
                        })
                        .collect()
                }))
                .with_allow_auto_topic_creation(auto)
                .with_include_topic_authorized_operations(ops);
            let frame = encode_request(3, 13, corr, client.as_deref(), &req);
            let topics_value = opt(topics.as_ref().map(|ts| {
                Value::Vec(
                    ts.iter()
                        .map(|(id, name)| strukt(vec![bytes(id.as_bytes()), opt(name.as_deref().map(s))]))
                        .collect(),
                )
            }));
            let body = opt(Some(strukt(vec![topics_value, Value::Bool(auto), Value::Bool(ops)])));
            expected.push((
                frame.clone(),
                header_value(3, 13, corr, client.as_deref()),
                "v_meta",
                body,
            ));
            inputs.push(input(&artifact, "req", vec![bytes(&frame)]));
        }
    }
    let r = run(&artifact, &inputs);
    let headers: BTreeSet<Vec<Value>> = rows(&artifact, &r, "v_header").into_iter().collect();
    let apiv: BTreeSet<Vec<Value>> = rows(&artifact, &r, "v_apiv").into_iter().collect();
    let meta: BTreeSet<Vec<Value>> = rows(&artifact, &r, "v_meta").into_iter().collect();
    for (frame, header, view, body) in &expected {
        let f = bytes(frame);
        assert!(
            headers.contains(&vec![f.clone(), header.clone()]),
            "header of {frame:02x?}: {headers:?}"
        );
        let got = if *view == "v_apiv" { &apiv } else { &meta };
        assert!(got.contains(&vec![f.clone(), body.clone()]), "{view} of {frame:02x?}");
    }
}

#[test]
fn responses_encoded_in_blossom_decode_in_the_rust_implementation() {
    let artifact = compile();
    let mut rng = Rng(9);
    let mut inputs = Vec::new();
    type Broker = (i32, String, i32, Option<String>);
    // Per response: its correlation id, brokers, cluster id, controller, requested topics and the broker's topics.
    type Topic = ([u8; 16], Option<String>);
    type Have = (String, [u8; 16], i32);
    type Meta = (i32, Vec<Broker>, String, i32, Option<Vec<Topic>>, Vec<Have>);
    let mut metas: Vec<Meta> = Vec::new();
    for _ in 0..60 {
        let corr = rng.next() as i32;
        let brokers: Vec<Broker> = (0..1 + rng.below(4))
            .map(|_| {
                let rack = if rng.below(2) == 0 { None } else { Some(rng.text()) };
                (rng.next() as i32, rng.text(), rng.below(65536) as i32, rack)
            })
            .collect();
        // The broker's topics (distinct names and ids), and topics asked for by name or id, some of them the
        // broker's.
        let mut have: Vec<Have> = Vec::new();
        for _ in 0..rng.below(4) {
            let name = rng.text();
            if !have.iter().any(|h| h.0 == name) {
                have.push((
                    name,
                    (u128::from(rng.next()) << 64 | u128::from(rng.next()) | 1).to_be_bytes(),
                    rng.below(4) as i32,
                ));
            }
        }
        let by_id = rng.below(3) == 0;
        let topics = if rng.below(3) == 0 {
            None
        } else {
            Some(
                (0..rng.below(4))
                    .map(|_| {
                        let theirs = (!have.is_empty() && rng.below(2) == 0)
                            .then(|| have[rng.below(have.len() as u64) as usize].clone());
                        let id = if by_id && rng.below(2) == 0 {
                            match &theirs {
                                Some(h) => h.1,
                                None => (u128::from(rng.next()) << 64 | u128::from(rng.next()) | 1).to_be_bytes(),
                            }
                        } else {
                            [0; 16]
                        };
                        let name = if id != [0; 16] && rng.below(2) == 0 {
                            None
                        } else {
                            Some(theirs.map(|h| h.0).unwrap_or_else(|| rng.text()))
                        };
                        (id, name)
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let (cluster, controller) = (rng.text(), (rng.next() % 1000) as i32);
        let row = vec![
            i32v(corr),
            Value::Vec(
                brokers
                    .iter()
                    .map(|(id, host, port, rack)| {
                        Value::Tuple(vec![i32v(*id), s(host), i32v(*port), opt(rack.as_deref().map(s))].into())
                    })
                    .collect(),
            ),
            s(&cluster),
            i32v(controller),
            opt(topics.as_ref().map(|ts| {
                Value::Vec(
                    ts.iter()
                        .map(|(id, name)| Value::Tuple(vec![bytes(id), opt(name.as_deref().map(s))].into()))
                        .collect(),
                )
            })),
            Value::Vec(
                have.iter()
                    .map(|(n, id, p)| Value::Tuple(vec![s(n), bytes(id), i32v(*p)].into()))
                    .collect(),
            ),
        ];
        inputs.push(input(&artifact, "meta_resp", row));
        metas.push((corr, brokers, cluster, controller, topics, have));
    }
    let corrs: Vec<i32> = (0..20).map(|_| rng.next() as i32).collect();
    for (i, corr) in corrs.iter().enumerate() {
        inputs.push(input(
            &artifact,
            "apiv_resp",
            vec![i32v(*corr), Value::Bool(i % 2 == 1)],
        ));
    }
    let r = run(&artifact, &inputs);
    // The size prefix, then the rest.
    let unframe = |b: &Value| -> Bytes {
        let Value::Bytes(b) = b else { panic!("{b:?}") };
        let n = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
        assert_eq!(n, b.len() - 4, "the size prefix is the frame's size");
        Bytes::copy_from_slice(&b[4..])
    };
    let meta_rows = rows(&artifact, &r, "v_meta_resp");
    assert_eq!(meta_rows.len(), metas.len());
    for (corr, brokers, cluster, controller, topics, have) in &metas {
        let row = meta_rows.iter().find(|r| r[0] == i32v(*corr)).unwrap();
        let mut buf = unframe(&row[1]);
        let header = ResponseHeader::decode(&mut buf, MetadataResponse::header_version(13)).unwrap();
        assert_eq!(header.correlation_id, *corr);
        let m = MetadataResponse::decode(&mut buf, 13).unwrap();
        assert!(buf.is_empty(), "trailing bytes after the Metadata response");
        assert_eq!(
            (m.throttle_time_ms, m.error_code, m.controller_id),
            (0, 0, BrokerId(*controller))
        );
        assert_eq!(m.cluster_id.as_ref().map(|c| c.to_string()), Some(cluster.clone()));
        let got: Vec<Broker> = m
            .brokers
            .iter()
            .map(|b| {
                (
                    b.node_id.0,
                    b.host.to_string(),
                    b.port,
                    b.rack.as_ref().map(|r| r.to_string()),
                )
            })
            .collect();
        assert_eq!(&got, brokers);
        // As Kafka answers: by id when any topic is asked for by id (a known one, or UNKNOWN_TOPIC_ID with no name),
        // else by name (a known one, INVALID_TOPIC_EXCEPTION for an illegal name, else UNKNOWN_TOPIC_OR_PARTITION);
        // every topic when none is asked for. A known topic's partitions are all led by the controller.
        type Seen = (i16, Option<String>, [u8; 16], Vec<(i32, i32, i32, Vec<i32>, Vec<i32>)>);
        let known = |h: &Have| -> Seen {
            let parts = (0..h.2)
                .map(|p| (p, *controller, 0, vec![*controller], vec![*controller]))
                .collect();
            (0, Some(h.0.clone()), h.1, parts)
        };
        let want: Vec<Seen> = match topics {
            None => have.iter().map(known).collect(),
            Some(asked) => {
                let ids: Vec<[u8; 16]> = asked.iter().map(|t| t.0).filter(|id| *id != [0; 16]).collect();
                if ids.is_empty() {
                    asked
                        .iter()
                        .map(|(_, n)| match have.iter().find(|h| Some(&h.0) == n.as_ref()) {
                            Some(h) => known(h),
                            None => {
                                let code = if n.as_deref().is_some_and(legal_topic_name) {
                                    3
                                } else {
                                    17
                                };
                                (if n.is_none() { 3 } else { code }, n.clone(), [0; 16], Vec::new())
                            }
                        })
                        .collect()
                } else {
                    ids.iter()
                        .map(|id| match have.iter().find(|h| h.1 == *id) {
                            Some(h) => known(h),
                            None => (100, None, *id, Vec::new()),
                        })
                        .collect()
                }
            }
        };
        let got: Vec<Seen> = m
            .topics
            .iter()
            .map(|t| {
                assert!(
                    t.partitions
                        .iter()
                        .all(|p| p.error_code == 0 && p.offline_replicas.is_empty())
                );
                (
                    t.error_code,
                    t.name.as_ref().map(|n| n.0.to_string()),
                    *t.topic_id.as_bytes(),
                    t.partitions
                        .iter()
                        .map(|p| {
                            (
                                p.partition_index,
                                p.leader_id.0,
                                p.leader_epoch,
                                p.replica_nodes.iter().map(|b| b.0).collect(),
                                p.isr_nodes.iter().map(|b| b.0).collect(),
                            )
                        })
                        .collect(),
                )
            })
            .collect();
        assert_eq!(got, want);
    }
    let apiv_rows = rows(&artifact, &r, "v_apiv_resp");
    for (i, corr) in corrs.iter().enumerate() {
        let unsupported = i % 2 == 1;
        let row = apiv_rows
            .iter()
            .find(|r| r[0] == i32v(*corr) && r[1] == Value::Bool(unsupported))
            .unwrap();
        let mut buf = unframe(&row[2]);
        // ApiVersions answers with response header v0 at every version; the refusal is laid out as version 0.
        let header = ResponseHeader::decode(&mut buf, ApiVersionsResponse::header_version(3)).unwrap();
        assert_eq!(header.correlation_id, *corr);
        let resp = ApiVersionsResponse::decode(&mut buf, if unsupported { 0 } else { 3 }).unwrap();
        assert!(buf.is_empty(), "trailing bytes after the ApiVersions response");
        let keys: Vec<(i16, i16, i16)> = resp
            .api_keys
            .iter()
            .map(|k| (k.api_key, k.min_version, k.max_version))
            .collect();
        if unsupported {
            assert_eq!((resp.error_code, keys), (35, vec![(18, 3, 4)]));
        } else {
            assert_eq!(
                (resp.error_code, keys, resp.throttle_time_ms),
                (
                    0,
                    vec![
                        (0, 10, 12),
                        (1, 16, 17),
                        (2, 7, 10),
                        (3, 13, 13),
                        (18, 3, 4),
                        (19, 7, 7),
                        (20, 6, 6),
                        (22, 3, 5),
                        (32, 4, 4),
                        (45, 0, 1),
                        (46, 0, 0),
                        (60, 0, 2)
                    ],
                    0
                )
            );
        }
    }
}

#[cfg(test)]
fn uuid_of(rng: &mut Rng) -> uuid::Uuid {
    if rng.below(3) == 0 {
        uuid::Uuid::nil()
    } else {
        uuid::Uuid::from_u128(u128::from(rng.next()) << 64 | u128::from(rng.next()))
    }
}

#[test]
fn topic_requests_encoded_by_the_rust_implementation_decode_in_blossom() {
    let artifact = compile();
    let mut rng = Rng(11);
    let mut inputs = Vec::new();
    // Per request frame: the view that decodes it and the value it should decode to.
    let mut expected: Vec<(Vec<u8>, &str, Value)> = Vec::new();
    for i in 0..150 {
        let corr = rng.next() as i32;
        let client = Some(rng.text());
        let (frame, view, want) = match i % 3 {
            0 => {
                let mut topics = Vec::new();
                let mut want_topics = Vec::new();
                for _ in 0..rng.below(4) {
                    let name = rng.text();
                    let (parts, rf) = (rng.next() as i32 % 20, rng.next() as i16 % 4);
                    let assignments: Vec<(i32, Vec<i32>)> = (0..rng.below(3))
                        .map(|p| (p as i32, (0..rng.below(3)).map(|_| rng.next() as i32).collect()))
                        .collect();
                    let configs: Vec<(String, Option<String>)> = (0..rng.below(3))
                        .map(|_| (rng.text(), if rng.below(4) == 0 { None } else { Some(rng.text()) }))
                        .collect();
                    topics.push(
                        CreatableTopic::default()
                            .with_name(TopicName(StrBytes::from_string(name.clone())))
                            .with_num_partitions(parts)
                            .with_replication_factor(rf)
                            .with_assignments(
                                assignments
                                    .iter()
                                    .map(|(p, bs)| {
                                        CreatableReplicaAssignment::default()
                                            .with_partition_index(*p)
                                            .with_broker_ids(bs.iter().map(|b| BrokerId(*b)).collect())
                                    })
                                    .collect(),
                            )
                            .with_configs(
                                configs
                                    .iter()
                                    .map(|(k, v)| {
                                        CreatableTopicConfig::default()
                                            .with_name(StrBytes::from_string(k.clone()))
                                            .with_value(v.clone().map(StrBytes::from_string))
                                    })
                                    .collect(),
                            ),
                    );
                    want_topics.push(strukt(vec![
                        s(&name),
                        i32v(parts),
                        i16v(rf),
                        Value::Vec(
                            assignments
                                .iter()
                                .map(|(p, bs)| {
                                    Value::Tuple(
                                        vec![i32v(*p), Value::Vec(bs.iter().map(|b| i32v(*b)).collect())].into(),
                                    )
                                })
                                .collect(),
                        ),
                        Value::Vec(
                            configs
                                .iter()
                                .map(|(k, v)| Value::Tuple(vec![s(k), opt(v.as_deref().map(s))].into()))
                                .collect(),
                        ),
                    ]));
                }
                let (timeout, validate) = (rng.next() as i32, rng.below(2) == 0);
                let req = CreateTopicsRequest::default()
                    .with_topics(topics)
                    .with_timeout_ms(timeout)
                    .with_validate_only(validate);
                let want = opt(Some(strukt(vec![
                    Value::Vec(want_topics.into()),
                    i32v(timeout),
                    Value::Bool(validate),
                ])));
                (encode_request(19, 7, corr, client.as_deref(), &req), "v_create", want)
            }
            1 => {
                let states: Vec<(Option<String>, uuid::Uuid)> = (0..rng.below(4))
                    .map(|_| {
                        (
                            if rng.below(3) == 0 { None } else { Some(rng.text()) },
                            uuid_of(&mut rng),
                        )
                    })
                    .collect();
                let timeout = rng.next() as i32;
                let req = DeleteTopicsRequest::default()
                    .with_topics(
                        states
                            .iter()
                            .map(|(n, id)| {
                                DeleteTopicState::default()
                                    .with_name(n.clone().map(|n| TopicName(StrBytes::from_string(n))))
                                    .with_topic_id(*id)
                            })
                            .collect(),
                    )
                    .with_timeout_ms(timeout);
                let want = opt(Some(Value::Tuple(
                    vec![
                        Value::Vec(
                            states
                                .iter()
                                .map(|(n, id)| strukt(vec![opt(n.as_deref().map(s)), bytes(id.as_bytes())]))
                                .collect(),
                        ),
                        i32v(timeout),
                    ]
                    .into(),
                )));
                (encode_request(20, 6, corr, client.as_deref(), &req), "v_delete", want)
            }
            _ => {
                let resources: Vec<(i8, String, Option<Vec<String>>)> = (0..rng.below(4))
                    .map(|_| {
                        let keys = if rng.below(3) == 0 {
                            None
                        } else {
                            Some((0..rng.below(3)).map(|_| rng.text()).collect())
                        };
                        ([2i8, 4, 8][rng.below(3) as usize], rng.text(), keys)
                    })
                    .collect();
                let req = DescribeConfigsRequest::default()
                    .with_resources(
                        resources
                            .iter()
                            .map(|(k, n, keys)| {
                                DescribeConfigsResource::default()
                                    .with_resource_type(*k)
                                    .with_resource_name(StrBytes::from_string(n.clone()))
                                    .with_configuration_keys(
                                        keys.as_ref()
                                            .map(|ks| ks.iter().map(|x| StrBytes::from_string(x.clone())).collect()),
                                    )
                            })
                            .collect(),
                    )
                    .with_include_synonyms(rng.below(2) == 0)
                    .with_include_documentation(rng.below(2) == 0);
                let want = opt(Some(Value::Vec(
                    resources
                        .iter()
                        .map(|(k, n, keys)| {
                            strukt(vec![
                                i8v(*k),
                                s(n),
                                opt(keys.as_ref().map(|ks| Value::Vec(ks.iter().map(|x| s(x)).collect()))),
                            ])
                        })
                        .collect(),
                )));
                (encode_request(32, 4, corr, client.as_deref(), &req), "v_describe", want)
            }
        };
        inputs.push(input(&artifact, "req", vec![bytes(&frame)]));
        expected.push((frame, view, want));
    }
    let r = run(&artifact, &inputs);
    let decoded: Vec<(&str, BTreeSet<Vec<Value>>)> = ["v_create", "v_delete", "v_describe"]
        .into_iter()
        .map(|v| (v, rows(&artifact, &r, v).into_iter().collect()))
        .collect();
    for (frame, view, want) in &expected {
        let got = &decoded.iter().find(|d| d.0 == *view).unwrap().1;
        let row = got.iter().find(|r| r[0] == bytes(frame)).unwrap();
        assert_eq!(&row[1], want, "{view} of {frame:02x?}");
    }
}

/// The configurations the broker knows, with their defaults and Kafka types, in the order it lists them.
#[cfg(test)]
const TOPIC_CONFIGS: [(&str, &str, i8); 8] = [
    ("cleanup.policy", "delete", 2),
    ("compression.type", "producer", 2),
    ("max.message.bytes", "1048588", 3),
    ("message.timestamp.type", "CreateTime", 2),
    ("min.insync.replicas", "1", 3),
    ("retention.bytes", "-1", 5),
    ("retention.ms", "604800000", 5),
    ("segment.bytes", "1073741824", 3),
];

#[test]
fn topic_responses_encoded_in_blossom_decode_in_the_rust_implementation() {
    let artifact = compile();
    let mut rng = Rng(13);
    let mut inputs = Vec::new();
    // A topic's own configurations: some of the known ones, with random values.
    let own = |rng: &mut Rng| -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (k, _, _) in TOPIC_CONFIGS {
            if rng.below(3) == 0 {
                out.push((k.to_string(), rng.text()));
            }
        }
        out
    };
    type Created = (String, [u8; 16], i16, Option<String>, i32, Vec<(String, String)>);
    let mut creates: Vec<(i32, Vec<Created>)> = Vec::new();
    type Deleted = (Option<String>, [u8; 16], i16, Option<String>);
    let mut deletes: Vec<(i32, Vec<Deleted>)> = Vec::new();
    type Described = (i8, String, Option<Vec<String>>, Option<Vec<(String, String)>>);
    let mut describes: Vec<(i32, Vec<Described>)> = Vec::new();
    for _ in 0..40 {
        let corr = rng.next() as i32;
        let outcomes: Vec<Created> = (0..rng.below(4))
            .map(|_| {
                let ok = rng.below(2) == 0;
                let id = uuid_of(&mut rng).into_bytes();
                if ok {
                    (rng.text(), id, 0, None, 1 + rng.below(8) as i32, own(&mut rng))
                } else {
                    (
                        rng.text(),
                        [0; 16],
                        [36i16, 37, 40, 42][rng.below(4) as usize],
                        Some(rng.text()),
                        -1,
                        Vec::new(),
                    )
                }
            })
            .collect();
        let pairs = |cs: &[(String, String)]| {
            Value::Vec(cs.iter().map(|(k, v)| Value::Tuple(vec![s(k), s(v)].into())).collect())
        };
        inputs.push(input(
            &artifact,
            "create_resp",
            vec![
                i32v(corr),
                Value::Vec(
                    outcomes
                        .iter()
                        .map(|(n, id, e, m, p, cs)| {
                            Value::Tuple(
                                vec![s(n), bytes(id), i16v(*e), opt(m.as_deref().map(s)), i32v(*p), pairs(cs)].into(),
                            )
                        })
                        .collect(),
                ),
            ],
        ));
        creates.push((corr, outcomes));
        let corr = rng.next() as i32;
        let outcomes: Vec<Deleted> = (0..rng.below(4))
            .map(|_| {
                let name = if rng.below(3) == 0 { None } else { Some(rng.text()) };
                let err = [0i16, 3, 100, 42][rng.below(4) as usize];
                (
                    name,
                    uuid_of(&mut rng).into_bytes(),
                    err,
                    (err != 0).then(|| rng.text()),
                )
            })
            .collect();
        inputs.push(input(
            &artifact,
            "delete_resp",
            vec![
                i32v(corr),
                Value::Vec(
                    outcomes
                        .iter()
                        .map(|(n, id, e, m)| {
                            Value::Tuple(
                                vec![opt(n.as_deref().map(s)), bytes(id), i16v(*e), opt(m.as_deref().map(s))].into(),
                            )
                        })
                        .collect(),
                ),
            ],
        ));
        deletes.push((corr, outcomes));
        let corr = rng.next() as i32;
        let resources: Vec<Described> = (0..rng.below(4))
            .map(|_| {
                let kind = [2i8, 2, 4, 8][rng.below(4) as usize];
                let keys = if rng.below(2) == 0 {
                    None
                } else {
                    let mut ks: Vec<String> = TOPIC_CONFIGS
                        .iter()
                        .filter(|_| rng.below(2) == 0)
                        .map(|c| c.0.to_string())
                        .collect();
                    if rng.below(3) == 0 {
                        ks.push(rng.text());
                    }
                    Some(ks)
                };
                let set = (rng.below(3) != 0).then(|| own(&mut rng));
                (kind, rng.text(), keys, set)
            })
            .collect();
        inputs.push(input(
            &artifact,
            "describe_resp",
            vec![
                i32v(corr),
                Value::Vec(
                    resources
                        .iter()
                        .map(|(k, n, keys, set)| {
                            Value::Tuple(
                                vec![
                                    i8v(*k),
                                    s(n),
                                    opt(keys.as_ref().map(|ks| Value::Vec(ks.iter().map(|x| s(x)).collect()))),
                                    opt(set.as_ref().map(|cs| pairs(cs))),
                                ]
                                .into(),
                            )
                        })
                        .collect(),
                ),
            ],
        ));
        describes.push((corr, resources));
    }
    let r = run(&artifact, &inputs);
    let unframe = |b: &Value| -> Bytes {
        let Value::Bytes(b) = b else { panic!("{b:?}") };
        let n = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
        assert_eq!(n, b.len() - 4, "the size prefix is the frame's size");
        Bytes::copy_from_slice(&b[4..])
    };
    // A configuration's value and source (1: the topic's own, 5: the default).
    let value_of = |set: &[(String, String)], k: &str, default: &str| -> (String, i8) {
        match set.iter().find(|c| c.0 == k) {
            Some(c) => (c.1.clone(), 1),
            None => (default.to_owned(), 5),
        }
    };
    let create_rows = rows(&artifact, &r, "v_create_resp");
    for (corr, outcomes) in &creates {
        let row = create_rows.iter().find(|r| r[0] == i32v(*corr)).unwrap();
        let mut buf = unframe(&row[1]);
        let header = ResponseHeader::decode(&mut buf, CreateTopicsResponse::header_version(7)).unwrap();
        assert_eq!(header.correlation_id, *corr);
        let m = CreateTopicsResponse::decode(&mut buf, 7).unwrap();
        assert!(buf.is_empty(), "trailing bytes after the CreateTopics response");
        assert_eq!(m.throttle_time_ms, 0);
        assert_eq!(m.topics.len(), outcomes.len());
        for (t, (name, id, err, msg, parts, set)) in m.topics.iter().zip(outcomes) {
            let ok = *err == 0;
            assert_eq!(t.name.0.to_string(), *name);
            assert_eq!(*t.topic_id.as_bytes(), *id);
            assert_eq!(
                (t.error_code, t.error_message.as_ref().map(|x| x.to_string())),
                (*err, msg.clone())
            );
            assert_eq!(t.topic_config_error_code, 0);
            assert_eq!(
                (t.num_partitions, t.replication_factor),
                if ok { (*parts, 1) } else { (-1, -1) }
            );
            let configs = t.configs.as_ref().map(|cs| {
                cs.iter()
                    .map(|c| {
                        assert!(!c.read_only && !c.is_sensitive);
                        (
                            c.name.to_string(),
                            c.value.as_ref().map(|v| v.to_string()),
                            c.config_source,
                        )
                    })
                    .collect::<Vec<_>>()
            });
            let want = ok.then(|| {
                TOPIC_CONFIGS
                    .iter()
                    .map(|(k, d, _)| {
                        let (v, src) = value_of(set, k, d);
                        (k.to_string(), Some(v), src)
                    })
                    .collect::<Vec<_>>()
            });
            assert_eq!(configs, want);
        }
    }
    let delete_rows = rows(&artifact, &r, "v_delete_resp");
    for (corr, outcomes) in &deletes {
        let row = delete_rows.iter().find(|r| r[0] == i32v(*corr)).unwrap();
        let mut buf = unframe(&row[1]);
        let header = ResponseHeader::decode(&mut buf, DeleteTopicsResponse::header_version(6)).unwrap();
        assert_eq!(header.correlation_id, *corr);
        let m = DeleteTopicsResponse::decode(&mut buf, 6).unwrap();
        assert!(buf.is_empty(), "trailing bytes after the DeleteTopics response");
        let got: Vec<Deleted> = m
            .responses
            .iter()
            .map(|t| {
                (
                    t.name.as_ref().map(|n| n.0.to_string()),
                    *t.topic_id.as_bytes(),
                    t.error_code,
                    t.error_message.as_ref().map(|x| x.to_string()),
                )
            })
            .collect();
        assert_eq!((m.throttle_time_ms, &got), (0, outcomes));
    }
    let describe_rows = rows(&artifact, &r, "v_describe_resp");
    for (corr, resources) in &describes {
        let row = describe_rows.iter().find(|r| r[0] == i32v(*corr)).unwrap();
        let mut buf = unframe(&row[1]);
        let header = ResponseHeader::decode(&mut buf, DescribeConfigsResponse::header_version(4)).unwrap();
        assert_eq!(header.correlation_id, *corr);
        let m = DescribeConfigsResponse::decode(&mut buf, 4).unwrap();
        assert!(buf.is_empty(), "trailing bytes after the DescribeConfigs response");
        assert_eq!((m.throttle_time_ms, m.results.len()), (0, resources.len()));
        for (res, (kind, name, keys, set)) in m.results.iter().zip(resources) {
            assert_eq!(
                (res.resource_type, res.resource_name.to_string()),
                (*kind, name.clone())
            );
            // A topic is described (or unknown); a broker has no configuration described; other types are refused.
            let (err, configs): (i16, Vec<(String, String, i8, i8)>) = match (kind, set) {
                (2, Some(set)) => (
                    0,
                    TOPIC_CONFIGS
                        .iter()
                        .filter(|(k, _, _)| {
                            keys.as_ref()
                                .is_none_or(|ks| ks.is_empty() || ks.iter().any(|x| x == k))
                        })
                        .map(|(k, d, ty)| {
                            let (v, src) = value_of(set, k, d);
                            (k.to_string(), v, src, *ty)
                        })
                        .collect(),
                ),
                (2, None) => (3, Vec::new()),
                (4, _) => (0, Vec::new()),
                _ => (42, Vec::new()),
            };
            assert_eq!(res.error_code, err);
            assert_eq!(res.error_message.is_some(), err != 0);
            let got: Vec<(String, String, i8, i8)> = res
                .configs
                .iter()
                .map(|c| {
                    assert!(!c.read_only && !c.is_sensitive && c.synonyms.is_empty() && c.documentation.is_none());
                    (
                        c.name.to_string(),
                        c.value.as_ref().unwrap().to_string(),
                        c.config_source,
                        c.config_type,
                    )
                })
                .collect();
            assert_eq!(got, configs);
        }
    }
}

#[cfg(test)]
fn bytes_opt(rng: &mut Rng) -> Option<Vec<u8>> {
    if rng.below(5) == 0 {
        None
    } else {
        Some((0..rng.below(40)).map(|_| rng.next() as u8).collect())
    }
}

#[test]
fn produce_requests_decode_and_produce_responses_encode() {
    let artifact = compile();
    let mut rng = Rng(17);
    let mut inputs = Vec::new();
    // Per request: its frame, the value Blossom should decode, and the answers given to it.
    type Answer = (u64, i16, i64, i64, Option<String>, Option<i32>);
    let mut cases: Vec<(i32, Vec<u8>, Value, Vec<Answer>)> = Vec::new();
    for _ in 0..80 {
        let corr = rng.next() as i32;
        let version = 10 + rng.below(3) as i16;
        let tid = if rng.below(4) == 0 { Some(rng.text()) } else { None };
        let (acks, timeout) = ([-1i16, 0, 1, 5][rng.below(4) as usize], rng.next() as i32);
        // Per topic: its name and each partition's index and records.
        type Topic = (String, Vec<(i32, Option<Vec<u8>>)>);
        let topics: Vec<Topic> = (0..rng.below(3))
            .map(|_| {
                (
                    rng.text(),
                    (0..rng.below(3))
                        .map(|_| (rng.next() as i32, bytes_opt(&mut rng)))
                        .collect(),
                )
            })
            .collect();
        let req = ProduceRequest::default()
            .with_transactional_id(
                tid.clone()
                    .map(|t| kafka_protocol::messages::TransactionalId(StrBytes::from_string(t))),
            )
            .with_acks(acks)
            .with_timeout_ms(timeout)
            .with_topic_data(
                topics
                    .iter()
                    .map(|(n, ps)| {
                        TopicProduceData::default()
                            .with_name(TopicName(StrBytes::from_string(n.clone())))
                            .with_partition_data(
                                ps.iter()
                                    .map(|(i, r)| {
                                        PartitionProduceData::default()
                                            .with_index(*i)
                                            .with_records(r.clone().map(Bytes::from))
                                    })
                                    .collect(),
                            )
                    })
                    .collect(),
            );
        let frame = encode_request(0, version, corr, Some("p"), &req);
        let want = opt(Some(strukt(vec![
            opt(tid.as_deref().map(s)),
            i16v(acks),
            i32v(timeout),
            Value::Vec(
                topics
                    .iter()
                    .map(|(n, ps)| {
                        strukt(vec![
                            s(n),
                            Value::Vec(
                                ps.iter()
                                    .map(|(i, r)| strukt(vec![i32v(*i), opt(r.as_deref().map(bytes))]))
                                    .collect(),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ])));
        let entries: usize = topics.iter().map(|t| t.1.len()).sum();
        let answers: Vec<Answer> = (0..entries as u64)
            .map(|k| {
                let err = [0i16, 2, 3, 10, 87][rng.below(5) as usize];
                let msg = (err != 0).then(|| rng.text());
                let bad = (err == 87).then(|| rng.below(3) as i32);
                (k, err, rng.next() as i64 >> 2, rng.next() as i64 >> 3, msg, bad)
            })
            .collect();
        inputs.push(input(&artifact, "req", vec![bytes(&frame)]));
        inputs.push(input(
            &artifact,
            "produce_resp",
            vec![
                i32v(corr),
                bytes(&frame),
                Value::Vec(
                    answers
                        .iter()
                        .map(|(k, e, b, ls, m, bad)| {
                            Value::Tuple(
                                vec![
                                    Value::Int(IntValue::U64(*k)),
                                    i16v(*e),
                                    Value::Int(IntValue::I64(*b)),
                                    Value::Int(IntValue::I64(*ls)),
                                    opt(m.as_deref().map(s)),
                                    opt(bad.map(i32v)),
                                ]
                                .into(),
                            )
                        })
                        .collect(),
                ),
            ],
        ));
        cases.push((corr, frame, want, answers));
    }
    let r = run(&artifact, &inputs);
    let decoded = rows(&artifact, &r, "v_produce");
    let encoded = rows(&artifact, &r, "v_produce_resp");
    for (corr, frame, want, answers) in &cases {
        let got = decoded.iter().find(|x| x[0] == bytes(frame)).unwrap();
        assert_eq!(&got[1], want, "the decoded request {frame:02x?}");
        let row = encoded.iter().find(|x| x[0] == i32v(*corr)).unwrap();
        let Value::Option(Some(b)) = &row[1] else {
            panic!("{:?}", row[1])
        };
        let Value::Bytes(b) = &**b else { panic!("{b:?}") };
        let mut buf = Bytes::copy_from_slice(&b[4..]);
        let header = ResponseHeader::decode(&mut buf, ProduceResponse::header_version(12)).unwrap();
        assert_eq!(header.correlation_id, *corr);
        let m = ProduceResponse::decode(&mut buf, 12).unwrap();
        assert!(buf.is_empty(), "trailing bytes after the Produce response");
        let got: Vec<Answer> = m
            .responses
            .iter()
            .flat_map(|t| t.partition_responses.iter())
            .enumerate()
            .map(|(k, p)| {
                assert_eq!(p.log_append_time_ms, -1);
                let bad = p.record_errors.first().map(|e| {
                    assert_eq!(
                        e.batch_index_error_message.as_ref().map(|x| x.to_string()),
                        p.error_message.as_ref().map(|x| x.to_string())
                    );
                    e.batch_index
                });
                (
                    k as u64,
                    p.error_code,
                    p.base_offset,
                    p.log_start_offset,
                    p.error_message.as_ref().map(|x| x.to_string()),
                    bad,
                )
            })
            .collect();
        assert_eq!(&got, answers);
    }
}

#[test]
fn fetch_and_list_offsets_decode_and_answers_encode() {
    use kafka_protocol::messages::fetch_request::{FetchPartition, FetchTopic};
    use kafka_protocol::messages::list_offsets_request::{ListOffsetsPartition, ListOffsetsTopic};
    use kafka_protocol::messages::{FetchRequest, FetchResponse, ListOffsetsRequest, ListOffsetsResponse};
    let artifact = compile();
    let mut rng = Rng(19);
    let mut inputs = Vec::new();
    type FetchAns = (u64, i16, i64, i64, Vec<Vec<u8>>);
    type ListAns = (u64, i16, i64, i64, i32);
    // Per request: its correlation id, frame, decoded value, (request-level error,) and answers.
    type FetchCase = (i32, Vec<u8>, Value, i16, Vec<FetchAns>);
    type ListCase = (i32, Vec<u8>, Value, Vec<ListAns>);
    type FetchTopicCase = ([u8; 16], Vec<(i32, i32, i64, i32)>);
    type ListTopicCase = (String, Vec<(i32, i32, i64)>);
    let mut fetches: Vec<FetchCase> = Vec::new();
    let mut lists: Vec<ListCase> = Vec::new();
    for _ in 0..40 {
        // A Fetch: random topics by id, partitions, offsets and limits.
        let corr = rng.next() as i32;
        let version = 16 + rng.below(2) as i16;
        let topics: Vec<FetchTopicCase> = (0..rng.below(3))
            .map(|_| {
                let id = uuid_of(&mut rng).into_bytes();
                let ps = (0..rng.below(3))
                    .map(|_| {
                        (
                            rng.next() as i32,
                            rng.next() as i32 >> 20,
                            rng.next() as i64 >> 2,
                            rng.next() as i32 >> 8,
                        )
                    })
                    .collect();
                (id, ps)
            })
            .collect();
        let (wait, minb, maxb, iso, sid, sep) = (
            rng.next() as i32,
            rng.next() as i32,
            rng.next() as i32,
            rng.below(2) as i8,
            rng.below(3) as i32,
            rng.next() as i32,
        );
        let req = FetchRequest::default()
            .with_max_wait_ms(wait)
            .with_min_bytes(minb)
            .with_max_bytes(maxb)
            .with_isolation_level(iso)
            .with_session_id(sid)
            .with_session_epoch(sep)
            .with_topics(
                topics
                    .iter()
                    .map(|(id, ps)| {
                        FetchTopic::default()
                            .with_topic_id(uuid::Uuid::from_bytes(*id))
                            .with_partitions(
                                ps.iter()
                                    .map(|(p, e, o, m)| {
                                        FetchPartition::default()
                                            .with_partition(*p)
                                            .with_current_leader_epoch(*e)
                                            .with_fetch_offset(*o)
                                            .with_partition_max_bytes(*m)
                                    })
                                    .collect(),
                            )
                    })
                    .collect(),
            )
            .with_rack_id(StrBytes::from_string(rng.text()));
        let frame = encode_request(1, version, corr, Some("c"), &req);
        let want = opt(Some(strukt(vec![
            i32v(wait),
            i32v(minb),
            i32v(maxb),
            i8v(iso),
            i32v(sid),
            i32v(sep),
            Value::Vec(
                topics
                    .iter()
                    .map(|(id, ps)| {
                        strukt(vec![
                            bytes(id),
                            Value::Vec(
                                ps.iter()
                                    .map(|(p, e, o, m)| {
                                        strukt(vec![i32v(*p), i32v(*e), Value::Int(IntValue::I64(*o)), i32v(*m)])
                                    })
                                    .collect(),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ])));
        let err = [0i16, 0, 70, 71][rng.below(4) as usize];
        let entries: usize = topics.iter().map(|t| t.1.len()).sum();
        let answers: Vec<FetchAns> = (0..entries as u64)
            .map(|k| {
                let e = [0i16, 0, 1, 3, 100][rng.below(5) as usize];
                let bs = (0..rng.below(3))
                    .map(|_| (0..1 + rng.below(30)).map(|_| rng.next() as u8).collect())
                    .collect();
                (k, e, rng.next() as i64 >> 2, rng.next() as i64 >> 3, bs)
            })
            .collect();
        inputs.push(input(&artifact, "req", vec![bytes(&frame)]));
        inputs.push(input(
            &artifact,
            "fetch_resp",
            vec![
                i32v(corr),
                bytes(&frame),
                i16v(err),
                Value::Vec(
                    answers
                        .iter()
                        .map(|(k, e, hw, ls, bs)| {
                            Value::Tuple(
                                vec![
                                    Value::Int(IntValue::U64(*k)),
                                    i16v(*e),
                                    Value::Int(IntValue::I64(*hw)),
                                    Value::Int(IntValue::I64(*ls)),
                                    Value::Vec(bs.iter().map(|b| bytes(b)).collect()),
                                ]
                                .into(),
                            )
                        })
                        .collect(),
                ),
            ],
        ));
        fetches.push((corr, frame, want, err, answers));
        // A ListOffsets: random topics by name, partitions, epochs and timestamps.
        let corr = rng.next() as i32;
        let version = 7 + rng.below(4) as i16;
        let ltopics: Vec<ListTopicCase> = (0..rng.below(3))
            .map(|_| {
                let ps = (0..rng.below(3))
                    .map(|_| (rng.next() as i32, rng.next() as i32 >> 20, rng.next() as i64 >> 1))
                    .collect();
                (rng.text(), ps)
            })
            .collect();
        let iso = rng.below(2) as i8;
        let req = ListOffsetsRequest::default()
            .with_replica_id(BrokerId(-1))
            .with_isolation_level(iso)
            .with_topics(
                ltopics
                    .iter()
                    .map(|(n, ps)| {
                        ListOffsetsTopic::default()
                            .with_name(TopicName(StrBytes::from_string(n.clone())))
                            .with_partitions(
                                ps.iter()
                                    .map(|(p, e, t)| {
                                        ListOffsetsPartition::default()
                                            .with_partition_index(*p)
                                            .with_current_leader_epoch(*e)
                                            .with_timestamp(*t)
                                    })
                                    .collect(),
                            )
                    })
                    .collect(),
            )
            .with_timeout_ms(rng.next() as i32);
        let frame = encode_request(2, version, corr, Some("c"), &req);
        let want = opt(Some(strukt(vec![
            i8v(iso),
            Value::Vec(
                ltopics
                    .iter()
                    .map(|(n, ps)| {
                        strukt(vec![
                            s(n),
                            Value::Vec(
                                ps.iter()
                                    .map(|(p, e, t)| strukt(vec![i32v(*p), i32v(*e), Value::Int(IntValue::I64(*t))]))
                                    .collect(),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ])));
        let entries: usize = ltopics.iter().map(|t| t.1.len()).sum();
        let answers: Vec<ListAns> = (0..entries as u64)
            .map(|k| {
                (
                    k,
                    [0i16, 3, 42, 74][rng.below(4) as usize],
                    rng.next() as i64 >> 2,
                    rng.next() as i64 >> 2,
                    rng.next() as i32,
                )
            })
            .collect();
        inputs.push(input(&artifact, "req", vec![bytes(&frame)]));
        inputs.push(input(
            &artifact,
            "list_resp",
            vec![
                i32v(corr),
                bytes(&frame),
                Value::Vec(
                    answers
                        .iter()
                        .map(|(k, e, t, o, ep)| {
                            Value::Tuple(
                                vec![
                                    Value::Int(IntValue::U64(*k)),
                                    i16v(*e),
                                    Value::Int(IntValue::I64(*t)),
                                    Value::Int(IntValue::I64(*o)),
                                    i32v(*ep),
                                ]
                                .into(),
                            )
                        })
                        .collect(),
                ),
            ],
        ));
        lists.push((corr, frame, want, answers));
    }
    let r = run(&artifact, &inputs);
    let decoded_fetch = rows(&artifact, &r, "v_fetch");
    let decoded_list = rows(&artifact, &r, "v_list");
    let fetch_out = rows(&artifact, &r, "v_fetch_resp");
    let list_out = rows(&artifact, &r, "v_list_resp");
    let body = |v: &Value| -> Bytes {
        let Value::Option(Some(b)) = v else { panic!("{v:?}") };
        let Value::Bytes(b) = &**b else { panic!("{b:?}") };
        let n = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
        assert_eq!(n, b.len() - 4, "the size prefix is the frame's size");
        Bytes::copy_from_slice(&b[4..])
    };
    for (corr, frame, want, err, answers) in &fetches {
        let got = decoded_fetch.iter().find(|x| x[0] == bytes(frame)).unwrap();
        assert_eq!(&got[1], want, "the decoded Fetch {frame:02x?}");
        let row = fetch_out.iter().find(|x| x[0] == i32v(*corr)).unwrap();
        let mut buf = body(&row[1]);
        let header = ResponseHeader::decode(&mut buf, FetchResponse::header_version(17)).unwrap();
        assert_eq!(header.correlation_id, *corr);
        let m = FetchResponse::decode(&mut buf, 17).unwrap();
        assert!(buf.is_empty(), "trailing bytes after the Fetch response");
        assert_eq!((m.throttle_time_ms, m.error_code, m.session_id), (0, *err, 0));
        let got: Vec<FetchAns> = m
            .responses
            .iter()
            .flat_map(|t| t.partitions.iter())
            .enumerate()
            .map(|(k, p)| {
                assert_eq!(p.last_stable_offset, p.high_watermark);
                assert_eq!(p.preferred_read_replica.0, -1);
                assert!(p.aborted_transactions.as_ref().is_none_or(|a| a.is_empty()));
                let records = p.records.clone().unwrap_or_default().to_vec();
                (
                    k as u64,
                    p.error_code,
                    p.high_watermark,
                    p.log_start_offset,
                    vec![records],
                )
            })
            .collect();
        let want: Vec<FetchAns> = answers
            .iter()
            .map(|(k, e, hw, ls, bs)| (*k, *e, *hw, *ls, vec![bs.concat()]))
            .collect();
        assert_eq!(got, want);
    }
    for (corr, frame, want, answers) in &lists {
        let got = decoded_list.iter().find(|x| x[0] == bytes(frame)).unwrap();
        assert_eq!(&got[1], want, "the decoded ListOffsets {frame:02x?}");
        let row = list_out.iter().find(|x| x[0] == i32v(*corr)).unwrap();
        let mut buf = body(&row[1]);
        let header = ResponseHeader::decode(&mut buf, ListOffsetsResponse::header_version(10)).unwrap();
        assert_eq!(header.correlation_id, *corr);
        let m = ListOffsetsResponse::decode(&mut buf, 10).unwrap();
        assert!(buf.is_empty(), "trailing bytes after the ListOffsets response");
        let got: Vec<ListAns> = m
            .topics
            .iter()
            .flat_map(|t| t.partitions.iter())
            .enumerate()
            .map(|(k, p)| (k as u64, p.error_code, p.timestamp, p.offset, p.leader_epoch))
            .collect();
        assert_eq!(&got, answers);
    }
}

/// A one-record batch of producer `pid` at `epoch` and sequence `seq` (`pid` -1: no idempotence), taking `n` offsets.
#[test]
fn describe_cluster_requests_decode_and_answers_encode() {
    use kafka_protocol::messages::{DescribeClusterRequest, DescribeClusterResponse};
    type Broker = (i32, String, i32, Option<String>);
    let artifact = compile();
    let mut rng = Rng(60);
    let mut inputs = Vec::new();
    let mut reqs = Vec::new();
    let mut answers = Vec::new();
    for _ in 0..60 {
        let version = rng.below(3) as i16;
        let corr = rng.next() as i32;
        let ops = rng.below(2) == 0;
        let endpoint = if version >= 1 { 1 + rng.below(2) as i8 } else { 1 };
        let fenced = version >= 2 && rng.below(2) == 0;
        let mut req = DescribeClusterRequest::default().with_include_cluster_authorized_operations(ops);
        if version >= 1 {
            req = req.with_endpoint_type(endpoint);
        }
        if version >= 2 {
            req = req.with_include_fenced_brokers(fenced);
        }
        let frame = encode_request(60, version, corr, Some("dc"), &req);
        inputs.push(input(&artifact, "req", vec![bytes(&frame)]));
        reqs.push((frame, ops, endpoint, fenced));

        let brokers: Vec<Broker> = (0..rng.below(4))
            .map(|i| {
                let rack = if rng.below(2) == 0 { None } else { Some(rng.text()) };
                (i as i32 + 1, rng.text(), 9000 + rng.below(1000) as i32, rack)
            })
            .collect();
        let (cluster, controller) = (rng.text(), (rng.next() % 1000) as i32);
        inputs.push(input(
            &artifact,
            "dcluster_resp",
            vec![
                i32v(corr),
                i16v(version),
                Value::Bool(ops),
                i8v(endpoint),
                Value::Vec(
                    brokers
                        .iter()
                        .map(|(id, host, port, rack)| {
                            Value::Tuple(vec![i32v(*id), s(host), i32v(*port), opt(rack.as_deref().map(s))].into())
                        })
                        .collect(),
                ),
                s(&cluster),
                i32v(controller),
            ],
        ));
        answers.push((corr, version, ops, endpoint, brokers, cluster, controller));
    }
    let r = run(&artifact, &inputs);
    let decoded = rows(&artifact, &r, "v_dcluster");
    for (frame, ops, endpoint, fenced) in &reqs {
        let row = decoded.iter().find(|r| r[0] == bytes(frame)).unwrap();
        assert_eq!(
            row[1],
            opt(Some(strukt(vec![
                Value::Bool(*ops),
                i8v(*endpoint),
                Value::Bool(*fenced)
            ]))),
            "a DescribeCluster request"
        );
    }
    let encoded = rows(&artifact, &r, "v_dcluster_resp");
    for (corr, version, ops, endpoint, brokers, cluster, controller) in &answers {
        let row = encoded.iter().find(|r| r[0] == i32v(*corr)).unwrap();
        let Value::Bytes(b) = &row[1] else { panic!() };
        let n = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
        assert_eq!(n, b.len() - 4, "the size prefix is the frame's size");
        let mut buf = Bytes::copy_from_slice(&b[4..]);
        let header = ResponseHeader::decode(&mut buf, DescribeClusterResponse::header_version(*version)).unwrap();
        assert_eq!(header.correlation_id, *corr);
        let d = DescribeClusterResponse::decode(&mut buf, *version).unwrap();
        assert!(
            buf.is_empty(),
            "trailing bytes after a DescribeCluster v{version} response"
        );
        let got: Vec<Broker> = d
            .brokers
            .iter()
            .map(|b| {
                (
                    b.broker_id.0,
                    b.host.to_string(),
                    b.port,
                    b.rack.as_ref().map(|r| r.to_string()),
                )
            })
            .collect();
        assert!(d.brokers.iter().all(|b| !b.is_fenced));
        if *endpoint == 1 {
            assert_eq!(
                (d.error_code, d.cluster_id.to_string(), d.controller_id.0, got),
                (0, cluster.clone(), *controller, brokers.clone()),
                "v{version}"
            );
            let want_ops = if *ops { 8096 } else { i32::MIN };
            assert_eq!(d.cluster_authorized_operations, want_ops);
        } else {
            // Asked for controllers: a broker answers UNSUPPORTED_ENDPOINT_TYPE.
            assert_eq!((d.error_code, d.brokers.len()), (119, 0), "v{version}");
            assert!(d.error_message.is_some());
        }
        if *version >= 1 {
            assert_eq!(d.endpoint_type, *endpoint);
        }
    }
}

#[test]
fn reassignment_requests_decode_and_answers_encode() {
    use kafka_protocol::messages::alter_partition_reassignments_request::{ReassignablePartition, ReassignableTopic};
    use kafka_protocol::messages::list_partition_reassignments_request::ListPartitionReassignmentsTopics;
    use kafka_protocol::messages::{
        AlterPartitionReassignmentsRequest, AlterPartitionReassignmentsResponse, ListPartitionReassignmentsRequest,
        ListPartitionReassignmentsResponse,
    };
    let int = |v: &Value| match v {
        Value::Int(IntValue::I32(x)) => i64::from(*x),
        other => panic!("{other:?}"),
    };
    let artifact = compile();
    let mut rng = Rng(45);
    let mut inputs = Vec::new();
    // (frame, version, entries (topic, partition, replicas), allow, errors) for Alter; (frame, asked, ongoing) for List.
    type Entry = (String, i32, Option<Vec<i32>>);
    type Alter = (Vec<u8>, i16, i32, Vec<Entry>, bool, Vec<i16>);
    let mut alters: Vec<Alter> = Vec::new();
    type Ongoing = (String, i32, Vec<i32>, Vec<i32>);
    type Asked = Option<Vec<(String, Vec<i32>)>>;
    type List = (Vec<u8>, i32, Asked, Vec<Ongoing>);
    let mut lists: Vec<List> = Vec::new();
    type Listed = (String, i32, Vec<i32>, Vec<i32>, Vec<i32>);
    for _ in 0..40 {
        let version = rng.below(2) as i16;
        let corr = rng.next() as i32;
        let allow = version == 0 || rng.below(2) == 0;
        let mut entries: Vec<Entry> = Vec::new();
        let topics: Vec<ReassignableTopic> = (0..rng.below(3))
            .map(|t| {
                let name = format!("t{t}{}", rng.text());
                let parts: Vec<ReassignablePartition> = (0..1 + rng.below(3))
                    .map(|p| {
                        let replicas = if rng.below(4) == 0 {
                            None
                        } else {
                            Some((0..rng.below(4)).map(|_| (rng.next() % 6) as i32).collect::<Vec<i32>>())
                        };
                        entries.push((name.clone(), p as i32, replicas.clone()));
                        ReassignablePartition::default()
                            .with_partition_index(p as i32)
                            .with_replicas(replicas.map(|rs| rs.into_iter().map(BrokerId).collect()))
                    })
                    .collect();
                ReassignableTopic::default()
                    .with_name(TopicName(StrBytes::from_string(name)))
                    .with_partitions(parts)
            })
            .collect();
        let mut req = AlterPartitionReassignmentsRequest::default()
            .with_timeout_ms(5000)
            .with_topics(topics);
        if version >= 1 {
            req = req.with_allow_replication_factor_change(allow);
        }
        let frame = encode_request(45, version, corr, Some("reassign"), &req);
        let errors: Vec<i16> = entries.iter().map(|_| *rng.pick_i16(&[0, 3, 39, 38, 85, 7])).collect();
        inputs.push(input(&artifact, "req", vec![bytes(&frame)]));
        inputs.push(input(
            &artifact,
            "alter_resp",
            vec![
                i32v(corr),
                bytes(&frame),
                Value::Vec(
                    errors
                        .iter()
                        .enumerate()
                        .map(|(k, e)| Value::Tuple(vec![Value::Int(IntValue::U64(k as u64)), i16v(*e)].into()))
                        .collect(),
                ),
            ],
        ));
        alters.push((frame, version, corr, entries, allow, errors));

        let corr = rng.next() as i32;
        let names = ["a", "b", "c"];
        let ongoing: Vec<Ongoing> = names
            .iter()
            .flat_map(|n| (0..rng.below(3) as i32).map(move |p| (n.to_string(), p)))
            .map(|(n, p)| (n, p, vec![1, 2, 3], vec![2 + p, 3, 4]))
            .collect();
        let asked = if rng.below(2) == 0 {
            None
        } else {
            Some(vec![("a".to_string(), vec![0, 1]), ("c".to_string(), vec![1])])
        };
        let req = ListPartitionReassignmentsRequest::default()
            .with_timeout_ms(1000)
            .with_topics(asked.clone().map(|ts| {
                ts.into_iter()
                    .map(|(n, ps)| {
                        ListPartitionReassignmentsTopics::default()
                            .with_name(TopicName(StrBytes::from_string(n)))
                            .with_partition_indexes(ps)
                    })
                    .collect()
            }));
        let frame = encode_request(46, 0, corr, Some("reassign"), &req);
        inputs.push(input(&artifact, "req", vec![bytes(&frame)]));
        let i32s = |xs: &[i32]| Value::Vec(xs.iter().map(|x| i32v(*x)).collect());
        inputs.push(input(
            &artifact,
            "list_resp_r",
            vec![
                i32v(corr),
                bytes(&frame),
                Value::Vec(
                    ongoing
                        .iter()
                        .map(|(n, p, rs, t)| Value::Tuple(vec![s(n), i32v(*p), i32s(rs), i32s(t)].into()))
                        .collect(),
                ),
            ],
        ));
        lists.push((frame, corr, asked, ongoing));
    }
    let r = run(&artifact, &inputs);
    let decoded = rows(&artifact, &r, "v_alter_reassign");
    let alter_out = rows(&artifact, &r, "v_alter_resp");
    let list_out = rows(&artifact, &r, "v_list_resp_r");
    let unframe = |b: &Value| -> Bytes {
        let Value::Bytes(b) = b else { panic!("{b:?}") };
        let n = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
        assert_eq!(n, b.len() - 4, "the size prefix is the frame's size");
        Bytes::copy_from_slice(&b[4..])
    };
    for (frame, version, corr, entries, allow, errors) in &alters {
        // The request as Blossom decoded it: timeout, allow, and each partition's replicas.
        let row = decoded.iter().find(|x| x[0] == bytes(frame)).unwrap();
        let Value::Option(Some(req)) = &row[1] else {
            panic!("an AlterPartitionReassignments v{version} does not decode")
        };
        let Value::Struct(req) = &**req else { panic!() };
        assert_eq!(req[1], Value::Bool(*allow));
        let Value::Vec(ts) = &req[2] else { panic!() };
        let got: Vec<Entry> = ts
            .iter()
            .flat_map(|t| {
                let Value::Struct(t) = t else { panic!() };
                let Value::Str(n) = &t[0] else { panic!() };
                let Value::Vec(ps) = &t[1] else { panic!() };
                ps.iter()
                    .map(|p| {
                        let Value::Struct(p) = p else { panic!() };
                        let rs = match &p[1] {
                            Value::Option(None) => None,
                            Value::Option(Some(v)) => {
                                let Value::Vec(v) = &**v else { panic!() };
                                Some(v.iter().map(|x| int(x) as i32).collect())
                            }
                            other => panic!("{other:?}"),
                        };
                        (n.to_string(), int(&p[0]) as i32, rs)
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(&got, entries);
        // The answer, decoded by the Rust implementation.
        let row = alter_out.iter().find(|x| x[0] == i32v(*corr)).unwrap();
        let mut buf = unframe(&row[1]);
        let h =
            ResponseHeader::decode(&mut buf, AlterPartitionReassignmentsResponse::header_version(*version)).unwrap();
        assert_eq!(h.correlation_id, *corr);
        let resp = AlterPartitionReassignmentsResponse::decode(&mut buf, *version).unwrap();
        assert!(
            buf.is_empty(),
            "trailing bytes after an AlterPartitionReassignments v{version} response"
        );
        let codes: Vec<(String, i32, i16)> = resp
            .responses
            .iter()
            .flat_map(|t| {
                t.partitions
                    .iter()
                    .map(|p| (t.name.to_string(), p.partition_index, p.error_code))
            })
            .collect();
        let want: Vec<(String, i32, i16)> = entries
            .iter()
            .zip(errors)
            .map(|(e, c)| (e.0.clone(), e.1, *c))
            .collect();
        assert_eq!(codes, want);
        assert!(
            resp.responses
                .iter()
                .flat_map(|t| &t.partitions)
                .all(|p| (p.error_code == 0) == p.error_message.is_none())
        );
        if *version >= 1 {
            assert_eq!(resp.allow_replication_factor_change, *allow);
        }
    }
    for (_frame, corr, asked, ongoing) in &lists {
        let row = list_out.iter().find(|x| x[0] == i32v(*corr)).unwrap();
        let mut buf = unframe(&row[1]);
        let h = ResponseHeader::decode(&mut buf, ListPartitionReassignmentsResponse::header_version(0)).unwrap();
        assert_eq!(h.correlation_id, *corr);
        let resp = ListPartitionReassignmentsResponse::decode(&mut buf, 0).unwrap();
        assert!(
            buf.is_empty(),
            "trailing bytes after a ListPartitionReassignments response"
        );
        let want: Vec<Listed> = ongoing
            .iter()
            .filter(|o| {
                asked
                    .as_ref()
                    .is_none_or(|ts| ts.iter().any(|t| t.0 == o.0 && t.1.contains(&o.1)))
            })
            .map(|(n, p, rs, t)| {
                let shown: Vec<i32> = t.iter().chain(rs.iter().filter(|r| !t.contains(r))).copied().collect();
                let adding: Vec<i32> = t.iter().filter(|x| !rs.contains(x)).copied().collect();
                let removing: Vec<i32> = rs.iter().filter(|x| !t.contains(x)).copied().collect();
                (n.clone(), *p, shown, adding, removing)
            })
            .collect();
        let got: Vec<Listed> = resp
            .topics
            .iter()
            .flat_map(|t| {
                t.partitions.iter().map(|p| {
                    let ids = |xs: &[BrokerId]| xs.iter().map(|b| b.0).collect::<Vec<i32>>();
                    (
                        t.name.to_string(),
                        p.partition_index,
                        ids(&p.replicas),
                        ids(&p.adding_replicas),
                        ids(&p.removing_replicas),
                    )
                })
            })
            .collect();
        assert_eq!(got, want);
    }
}

#[cfg(test)]
fn producer_batch(pid: i64, epoch: i16, seq: i32, n: usize) -> Vec<u8> {
    use kafka_protocol::records::{Compression, Record, RecordBatchEncoder, RecordEncodeOptions, TimestampType};
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
            sequence: if pid == -1 {
                i as i32 - 1
            } else {
                seq.wrapping_add(i as i32)
            },
            timestamp: 1_700_000_000_000,
            key: None,
            value: Some(Bytes::from(format!("{pid}/{epoch}/{seq}/{i}"))),
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

/// Kafka's idempotent-producer rules (`ProducerAppendInfo`), batch by batch against one partition.
#[test]
fn idempotent_batches_follow_kafkas_sequence_rules() {
    let artifact = compile();
    let b = producer_batch;
    // Each case: the batches in order, and each one's (error, base offset).
    type Case = (Vec<Vec<u8>>, Vec<(i16, i64)>);
    let cases: Vec<Case> = vec![
        // In sequence, a resend of the last batch (its original offset), a gap, an older epoch, a new epoch that
        // does not start at 0, and one that does.
        (
            vec![
                b(7, 0, 0, 2),
                b(7, 0, 0, 2),
                b(7, 0, 2, 1),
                b(7, 0, 5, 1),
                b(7, 1, 3, 1),
                b(7, 1, 0, 1),
                b(7, 0, 3, 1),
            ],
            vec![(0, 0), (0, 0), (0, 2), (45, -1), (45, -1), (0, 3), (47, -1)],
        ),
        // Only the last five batches are remembered: a resend of the sixth-last is out of order, of the fifth-last
        // a duplicate.
        (
            (0..6)
                .map(|q| b(9, 0, q, 1))
                .chain([b(9, 0, 0, 1), b(9, 0, 1, 1)])
                .collect(),
            vec![(0, 0), (0, 1), (0, 2), (0, 3), (0, 4), (0, 5), (45, -1), (0, 1)],
        ),
        // A producer the partition has no state for starts at any sequence; sequences wrap past i32::MAX to 0.
        (
            vec![b(3, 0, i32::MAX, 1), b(3, 0, 0, 1), b(3, 0, 5, 1)],
            vec![(0, 0), (0, 1), (45, -1)],
        ),
        // Batches without idempotence take offsets between another producer's and change nothing for it: its
        // resend of sequence 10 is still a duplicate of offset 0, and 12 continues it.
        (
            vec![
                b(4, 2, 10, 1),
                b(-1, -1, 0, 3),
                b(4, 2, 11, 1),
                b(4, 2, 10, 1),
                b(4, 2, 12, 1),
            ],
            vec![(0, 0), (0, 1), (0, 4), (0, 0), (0, 5)],
        ),
    ];
    let inputs: Vec<InputEvent> = cases
        .iter()
        .enumerate()
        .map(|(i, (bs, _))| {
            input(
                &artifact,
                "seq_case",
                vec![
                    Value::Int(IntValue::U64(i as u64)),
                    Value::Vec(bs.iter().map(|x| bytes(x)).collect()),
                ],
            )
        })
        .collect();
    let r = run(&artifact, &inputs);
    let got = rows(&artifact, &r, "v_seq");
    for (i, (_, want)) in cases.iter().enumerate() {
        let row = got
            .iter()
            .find(|x| x[0] == Value::Int(IntValue::U64(i as u64)))
            .unwrap();
        let want = Value::Vec(
            want.iter()
                .map(|(e, o)| Value::Tuple(vec![i16v(*e), Value::Int(IntValue::I64(*o))].into()))
                .collect(),
        );
        assert_eq!(row[1], want, "case {i}");
    }
}

/// Every request frame of a capture file (without its size prefix), with its API key and version.
#[cfg(test)]
fn captured_requests(file: &str) -> Vec<(i16, i16, Vec<u8>)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/kafka/captures")
        .join(file);
    let text = std::fs::read_to_string(path).unwrap();
    text.lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|r| r["dir"] == "req")
        .map(|r| {
            let hex = r["hex"].as_str().unwrap();
            let raw: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(hex.get(i..i + 2).unwrap(), 16).unwrap())
                .collect();
            (
                r["api_key"].as_i64().unwrap() as i16,
                r["api_version"].as_i64().unwrap() as i16,
                raw[4..].to_vec(),
            )
        })
        .collect()
}

#[test]
fn the_golden_captures_of_real_clients_decode() {
    let artifact = compile();
    let mut inputs = Vec::new();
    let mut frames = Vec::new();
    for file in [
        "kcat.jsonl",
        "java-apiversions.jsonl",
        "java-topics.jsonl",
        "franz.jsonl",
        "s7-java-admin.jsonl",
        "s7-java-producer.jsonl",
        "s7-java-consumer.jsonl",
        "s7-java-get-offsets.jsonl",
        "s7-kcat.jsonl",
        "s7-franz.jsonl",
    ] {
        for (key, version, frame) in captured_requests(file) {
            inputs.push(input(&artifact, "req", vec![bytes(&frame)]));
            frames.push((file, key, version, frame));
        }
    }
    let r = run(&artifact, &inputs);
    let headers = rows(&artifact, &r, "v_header");
    let apiv = rows(&artifact, &r, "v_apiv");
    let meta = rows(&artifact, &r, "v_meta");
    let create = rows(&artifact, &r, "v_create");
    let delete = rows(&artifact, &r, "v_delete");
    let describe = rows(&artifact, &r, "v_describe");
    let produce = rows(&artifact, &r, "v_produce");
    let produce_check = rows(&artifact, &r, "v_produce_check");
    let init_pid = rows(&artifact, &r, "v_init_pid");
    let fetch = rows(&artifact, &r, "v_fetch");
    let list = rows(&artifact, &r, "v_list");
    let dcluster = rows(&artifact, &r, "v_dcluster");
    let list_reassign = rows(&artifact, &r, "v_list_reassign");
    let find = |rs: &[Vec<Value>], f: &[u8]| rs.iter().find(|r| r[0] == bytes(f)).map(|r| r[1].clone());
    let mut clients = BTreeSet::new();
    for (file, key, version, frame) in &frames {
        let Some(Value::Option(Some(h))) = find(&headers, frame) else {
            panic!("{file}: the header of a {key} v{version} request does not decode");
        };
        let Value::Struct(h) = &*h else { panic!() };
        assert_eq!((h[0].clone(), h[1].clone()), (i16v(*key), i16v(*version)), "{file}");
        match (key, version) {
            (18, 3 | 4) => {
                let Some(Value::Option(Some(body))) = find(&apiv, frame) else {
                    panic!("{file}: an ApiVersions v{version} body does not decode");
                };
                let Value::Struct(b) = &*body else { panic!() };
                clients.insert(format!("{:?}", b[0]));
            }
            (3, 13) => {
                let Some(Value::Option(Some(_))) = find(&meta, frame) else {
                    panic!("{file}: a Metadata v13 body does not decode");
                };
            }
            (19, 7) | (20, 6) | (32, 4) => {
                let (rs, what) = match key {
                    19 => (&create, "CreateTopics"),
                    20 => (&delete, "DeleteTopics"),
                    _ => (&describe, "DescribeConfigs"),
                };
                let Some(Value::Option(Some(_))) = find(rs, frame) else {
                    panic!("{file}: a {what} v{version} body does not decode");
                };
            }
            // franz-go's first ApiVersions is v5, which the broker refuses without reading its body.
            (18, 5) => assert!(file.ends_with("franz.jsonl"), "{file}"),
            (0, 10..=12) => {
                let Some(Value::Option(Some(_))) = find(&produce, frame) else {
                    panic!("{file}: a Produce v{version} body does not decode");
                };
                // Every batch a real client sent passes the broker's checks (the Java producer's are idempotent).
                let Some(Value::Vec(checks)) = find(&produce_check, frame) else {
                    panic!("{file}: a Produce v{version} request was not checked");
                };
                for c in checks.iter() {
                    let Value::Tuple(c) = c else { panic!("{c:?}") };
                    assert_eq!(c[0], i16v(0), "{file}: a Produce v{version} batch: {:?}", c[1]);
                }
            }
            (22, 5) => {
                let Some(Value::Option(Some(_))) = find(&init_pid, frame) else {
                    panic!("{file}: an InitProducerId v5 body does not decode");
                };
            }
            (60, 2) => {
                let Some(Value::Option(Some(_))) = find(&dcluster, frame) else {
                    panic!("{file}: a DescribeCluster v2 body does not decode");
                };
            }
            (46, 0) => {
                let Some(Value::Option(Some(_))) = find(&list_reassign, frame) else {
                    panic!("{file}: a ListPartitionReassignments v0 body does not decode");
                };
            }
            // The API the broker does not advertise: the Java admin's DescribeTopicPartitions. Only its header is
            // read here.
            (1, 16 | 17) => {
                let Some(Value::Option(Some(_))) = find(&fetch, frame) else {
                    panic!("{file}: a Fetch v{version} body does not decode");
                };
            }
            (2, 7..=10) => {
                let Some(Value::Option(Some(_))) = find(&list, frame) else {
                    panic!("{file}: a ListOffsets v{version} body does not decode");
                };
            }
            (75, 0) => {
                assert!(file.starts_with("s7-"), "{file}")
            }
            other => panic!("{file}: an unexpected request {other:?}"),
        }
    }
    // The client software names the three clients announce.
    let names = format!("{clients:?}");
    for want in ["librdkafka", "apache-kafka-java", "kgo"] {
        assert!(names.contains(want), "{want} not among {names}");
    }
}

#[test]
fn frame_splitting_finds_exactly_the_complete_frames() {
    let artifact = compile();
    let mut rng = Rng(3);
    let mut inputs = Vec::new();
    let mut expected = Vec::new();
    for _ in 0..100 {
        let mut stream = Vec::new();
        let mut frames = Vec::new();
        for _ in 0..rng.below(5) {
            let body: Vec<u8> = (0..rng.below(20)).map(|_| rng.next() as u8).collect();
            stream.extend((body.len() as u32).to_be_bytes());
            stream.extend(&body);
            frames.push(body);
        }
        let used = stream.len();
        // A trailing partial frame, sometimes with a bad (negative) size.
        let bad = rng.below(6) == 0;
        if bad {
            stream.extend((-5i32).to_be_bytes());
        } else {
            let tail: Vec<u8> = (0..rng.below(4)).map(|_| rng.next() as u8 & 0x0f).collect();
            stream.extend(tail);
        }
        inputs.push(input(&artifact, "stream_bytes", vec![bytes(&stream)]));
        expected.push((stream, frames, used, bad));
    }
    let r = run(&artifact, &inputs);
    let got = rows(&artifact, &r, "v_split");
    for (stream, frames, used, bad) in expected {
        let row = got.iter().find(|r| r[0] == bytes(&stream)).unwrap();
        let want = Value::Tuple(
            vec![
                Value::Vec(frames.iter().map(|f| bytes(f)).collect()),
                Value::Int(IntValue::U64(used as u64)),
                Value::Bool(bad),
            ]
            .into(),
        );
        assert_eq!(row[1], want, "{stream:02x?}");
    }
}

/// Frames fed in chunks, split at random, are reassembled exactly; so is one large frame fed in small chunks, which
/// finishes only because reassembly is linear (up to a log factor) in the bytes, not quadratic.
#[test]
fn reassembly_finds_exactly_the_frames_however_they_are_chunked() {
    let artifact = compile();
    let mut rng = Rng(11);
    let mut inputs = Vec::new();
    let mut expected = Vec::new();
    let chunked = |rng: &mut Rng, stream: &[u8], most: u64| -> Value {
        let mut cs = Vec::new();
        let mut at = 0;
        while at < stream.len() {
            let n = (1 + rng.below(most) as usize).min(stream.len() - at);
            cs.push(bytes(&stream[at..at + n]));
            at += n;
        }
        Value::Vec(cs.into())
    };
    for id in 0..100u64 {
        let mut stream = Vec::new();
        let mut frames = Vec::new();
        for _ in 0..rng.below(6) {
            let body: Vec<u8> = (0..rng.below(40)).map(|_| rng.next() as u8).collect();
            stream.extend((body.len() as u32).to_be_bytes());
            stream.extend(&body);
            frames.push(body);
        }
        let used = stream.len();
        let bad = rng.below(6) == 0;
        if bad {
            stream.extend((-5i32).to_be_bytes());
        } else {
            let tail: Vec<u8> = (0..rng.below(4)).map(|_| rng.next() as u8 & 0x0f).collect();
            stream.extend(tail);
        }
        let most = 1 + rng.below(12);
        let cs = chunked(&mut rng, &stream, most);
        inputs.push(input(&artifact, "chunks", vec![Value::Int(IntValue::U64(id)), cs]));
        // After a bad size nothing more is read; otherwise the leftover is the partial tail.
        let left = if bad { None } else { Some((stream.len() - used) as u64) };
        expected.push((id, frames, bad, left));
    }
    // One 1 MiB frame, then a small one, in chunks of at most 256 bytes.
    let big: Vec<u8> = (0..1 << 20).map(|i: u32| (i * 7) as u8).collect();
    let mut stream = (big.len() as u32).to_be_bytes().to_vec();
    stream.extend(&big);
    stream.extend(3u32.to_be_bytes());
    stream.extend([1, 2, 3]);
    let cs = chunked(&mut rng, &stream, 256);
    inputs.push(input(&artifact, "chunks", vec![Value::Int(IntValue::U64(1000)), cs]));
    expected.push((1000, vec![big, vec![1, 2, 3]], false, Some(0)));
    let r = run(&artifact, &inputs);
    let got = rows(&artifact, &r, "v_feed");
    for (id, frames, bad, left) in expected {
        let row = got.iter().find(|r| r[0] == Value::Int(IntValue::U64(id))).unwrap();
        let fields = match &row[1] {
            Value::Tuple(f) => f.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            fields[0],
            Value::Vec(frames.iter().map(|f| bytes(f)).collect()),
            "chunks {id}: frames"
        );
        assert_eq!(fields[1], Value::Bool(bad), "chunks {id}: bad");
        if let Some(left) = left {
            assert_eq!(fields[2], Value::Int(IntValue::U64(left)), "chunks {id}: left over");
        }
    }
}

/// Lengths a client controls (tagged-field sizes, compact string lengths) as large as a varint holds make the request
/// malformed (`None`), never a fault of the tick; a boolean byte other than 0 and 1 reads as true, as in Kafka.
#[test]
fn hostile_lengths_are_malformed_requests_not_faults() {
    let artifact = compile();
    let max_varint = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];
    // ApiVersions v3 header: key, version, correlation id, client id "c".
    let head = |key: i16, version: i16| -> Vec<u8> {
        let mut h = Vec::new();
        h.extend(key.to_be_bytes());
        h.extend(version.to_be_bytes());
        h.extend(7i32.to_be_bytes());
        h.extend(1i16.to_be_bytes());
        h.push(b'c');
        h
    };
    // One tagged field whose size is the largest varint.
    let mut huge_tag = head(18, 3);
    huge_tag.push(1);
    huge_tag.push(0);
    huge_tag.extend(max_varint);
    huge_tag.push(0);
    // A compact string (the client software name) whose length is the largest varint.
    let mut huge_string = head(18, 3);
    huge_string.push(0);
    huge_string.extend(max_varint);
    huge_string.extend(b"abc");
    // Metadata v13 with a null topic list and the booleans 7 and 1.
    let mut bools = head(3, 13);
    bools.push(0);
    bools.push(0);
    bools.extend([7, 1, 0]);
    let inputs: Vec<InputEvent> = [&huge_tag, &huge_string, &bools]
        .iter()
        .map(|f| input(&artifact, "req", vec![bytes(f)]))
        .collect();
    let r = run(&artifact, &inputs);
    let headers = rows(&artifact, &r, "v_header");
    let header_of = |f: &[u8]| headers.iter().find(|r| r[0] == bytes(f)).unwrap()[1].clone();
    assert_eq!(
        header_of(&huge_tag),
        opt(None),
        "a tagged field larger than the request"
    );
    let apiv = rows(&artifact, &r, "v_apiv");
    let apiv_of = |f: &[u8]| apiv.iter().find(|r| r[0] == bytes(f)).unwrap()[1].clone();
    assert_eq!(apiv_of(&huge_string), opt(None), "a string longer than the request");
    let meta = rows(&artifact, &r, "v_meta");
    let meta_of = |f: &[u8]| meta.iter().find(|r| r[0] == bytes(f)).unwrap()[1].clone();
    assert_eq!(
        meta_of(&bools),
        opt(Some(strukt(vec![opt(None), Value::Bool(true), Value::Bool(true)])))
    );
}
