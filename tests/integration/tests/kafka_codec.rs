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
use kafka_protocol::messages::create_topics_request::{CreatableReplicaAssignment, CreatableTopic, CreatableTopicConfig};
use kafka_protocol::messages::delete_topics_request::DeleteTopicState;
use kafka_protocol::messages::describe_configs_request::DescribeConfigsResource;
use kafka_protocol::messages::metadata_request::MetadataRequestTopic;
use kafka_protocol::messages::{
    ApiVersionsRequest, ApiVersionsResponse, BrokerId, CreateTopicsRequest, CreateTopicsResponse, DeleteTopicsRequest,
    DeleteTopicsResponse, DescribeConfigsRequest, DescribeConfigsResponse, MetadataRequest, MetadataResponse,
    RequestHeader, ResponseHeader, TopicName,
};
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

/// Runs the harness on the oracle and the engine (which must agree) and returns the oracle's run.
#[cfg(test)]
fn run(artifact: &BlsArtifact, inputs: &[InputEvent]) -> SyncRun {
    let sim = BlsSim::new(artifact, blossom_value::Seed::from_u64(0)).unwrap();
    let round = Duration::from_nanos(1_000_000_000);
    let reference = sim
        .run(inputs, Tick(1), round, &FaultSchedule::default(), false)
        .unwrap();
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
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
        && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
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
                have.push((name, (u128::from(rng.next()) << 64 | u128::from(rng.next()) | 1).to_be_bytes(), rng.below(4) as i32));
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
        let (cluster, controller) = (rng.text(), rng.next() as i32);
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
            let parts = (0..h.2).map(|p| (p, *controller, 0, vec![*controller], vec![*controller])).collect();
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
                                let code = if n.as_deref().is_some_and(legal_topic_name) { 3 } else { 17 };
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
                assert!(t.partitions.iter().all(|p| p.error_code == 0 && p.offline_replicas.is_empty()));
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
                (0, vec![(3, 13, 13), (18, 3, 4), (19, 7, 7), (20, 6, 6), (32, 4, 4)], 0)
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
                                    Value::Tuple(vec![i32v(*p), Value::Vec(bs.iter().map(|b| i32v(*b)).collect())].into())
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
                let want = opt(Some(strukt(vec![Value::Vec(want_topics.into()), i32v(timeout), Value::Bool(validate)])));
                (encode_request(19, 7, corr, client.as_deref(), &req), "v_create", want)
            }
            1 => {
                let states: Vec<(Option<String>, uuid::Uuid)> = (0..rng.below(4))
                    .map(|_| (if rng.below(3) == 0 { None } else { Some(rng.text()) }, uuid_of(&mut rng)))
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
                        let keys = if rng.below(3) == 0 { None } else { Some((0..rng.below(3)).map(|_| rng.text()).collect()) };
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
                                        keys.as_ref().map(|ks| ks.iter().map(|x| StrBytes::from_string(x.clone())).collect()),
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
                    (rng.text(), [0; 16], [36i16, 37, 40, 42][rng.below(4) as usize], Some(rng.text()), -1, Vec::new())
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
                            Value::Tuple(vec![s(n), bytes(id), i16v(*e), opt(m.as_deref().map(s)), i32v(*p), pairs(cs)].into())
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
                (name, uuid_of(&mut rng).into_bytes(), err, (err != 0).then(|| rng.text()))
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
                            Value::Tuple(vec![opt(n.as_deref().map(s)), bytes(id), i16v(*e), opt(m.as_deref().map(s))].into())
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
                    let mut ks: Vec<String> =
                        TOPIC_CONFIGS.iter().filter(|_| rng.below(2) == 0).map(|c| c.0.to_string()).collect();
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
            assert_eq!((t.error_code, t.error_message.as_ref().map(|x| x.to_string())), (*err, msg.clone()));
            assert_eq!(t.topic_config_error_code, 0);
            assert_eq!((t.num_partitions, t.replication_factor), if ok { (*parts, 1) } else { (-1, -1) });
            let configs = t.configs.as_ref().map(|cs| {
                cs.iter()
                    .map(|c| {
                        assert!(!c.read_only && !c.is_sensitive);
                        (c.name.to_string(), c.value.as_ref().map(|v| v.to_string()), c.config_source)
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
            assert_eq!((res.resource_type, res.resource_name.to_string()), (*kind, name.clone()));
            // A topic is described (or unknown); a broker has no configuration described; other types are refused.
            let (err, configs): (i16, Vec<(String, String, i8, i8)>) = match (kind, set) {
                (2, Some(set)) => (
                    0,
                    TOPIC_CONFIGS
                        .iter()
                        .filter(|(k, _, _)| keys.as_ref().is_none_or(|ks| ks.is_empty() || ks.iter().any(|x| x == k)))
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
                    (c.name.to_string(), c.value.as_ref().unwrap().to_string(), c.config_source, c.config_type)
                })
                .collect();
            assert_eq!(got, configs);
        }
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
            // Produce, Fetch and ListOffsets (items 5 and 6), and the APIs the broker does not advertise: the Java
            // admin's DescribeCluster, DescribeTopicPartitions and ListPartitionReassignments, and the Java producer's
            // InitProducerId (slice 9). Only their headers are read here.
            (0, 10..=12) | (1, 16 | 17) | (2, 7..=10) | (22, 5) | (46, 0) | (60, 2) | (75, 0) => {
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
        let row = got
            .iter()
            .find(|r| r[0] == Value::Int(IntValue::U64(id)))
            .unwrap();
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
    assert_eq!(header_of(&huge_tag), opt(None), "a tagged field larger than the request");
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
