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
use kafka_protocol::messages::metadata_request::MetadataRequestTopic;
use kafka_protocol::messages::{
    ApiVersionsRequest, ApiVersionsResponse, BrokerId, MetadataRequest, MetadataResponse, RequestHeader,
    ResponseHeader, TopicName,
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
    // Per response: its correlation id, brokers, cluster id, controller and requested topics.
    type Meta = (i32, Vec<Broker>, String, i32, Option<Vec<String>>);
    let mut metas: Vec<Meta> = Vec::new();
    for _ in 0..60 {
        let corr = rng.next() as i32;
        let brokers: Vec<Broker> = (0..1 + rng.below(4))
            .map(|_| {
                let rack = if rng.below(2) == 0 { None } else { Some(rng.text()) };
                (rng.next() as i32, rng.text(), rng.below(65536) as i32, rack)
            })
            .collect();
        let topics = if rng.below(3) == 0 {
            None
        } else {
            Some((0..rng.below(4)).map(|_| rng.text()).collect::<Vec<_>>())
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
            opt(topics.as_ref().map(|ts| Value::Vec(ts.iter().map(|t| s(t)).collect()))),
        ];
        inputs.push(input(&artifact, "meta_resp", row));
        metas.push((corr, brokers, cluster, controller, topics));
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
    for (corr, brokers, cluster, controller, topics) in &metas {
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
        // Every requested topic is unknown (this cluster has none), by name, with no partitions.
        let want: Vec<String> = topics.clone().unwrap_or_default();
        let got: Vec<(i16, Option<String>, usize)> = m
            .topics
            .iter()
            .map(|t| {
                (
                    t.error_code,
                    t.name.as_ref().map(|n| n.0.to_string()),
                    t.partitions.len(),
                )
            })
            .collect();
        assert_eq!(got, want.iter().map(|n| (3, Some(n.clone()), 0)).collect::<Vec<_>>());
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
                (0, vec![(3, 13, 13), (18, 3, 4)], 0)
            );
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
                // Every client asked for all topics (a null topic list) or none.
                let Some(Value::Option(Some(_))) = find(&meta, frame) else {
                    panic!("{file}: a Metadata v13 body does not decode");
                };
            }
            // franz-go's first ApiVersions is v5, which the broker refuses without reading its body.
            (18, 5) => assert_eq!(*file, "franz.jsonl"),
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
