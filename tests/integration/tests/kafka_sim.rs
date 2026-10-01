//! Slice 6, item 8: the Blossom Kafka client and a Rust client built on an independent protocol implementation (the
//! `kafka-protocol` crate) both talk to the Blossom broker in the cluster simulator, over byte pipes that split every
//! message at random, under broker and client crashes and connection resets. Each client asks for the supported API
//! versions and the cluster's metadata on every connection it gets; both must record the same answers, and those
//! answers are the broker's.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::durable::DurableSchema;
use blossom_sim::cluster::{Cluster, ClusterConfig, NoKvClients, StreamAction, StreamClient, StreamEvent};
use blossom_value::Value;
use blossom_value::time::NodeId;
use blossom_value::value::IntValue;
use bytes::{Bytes, BytesMut};
use kafka_protocol::messages::{
    ApiVersionsRequest, ApiVersionsResponse, MetadataRequest, MetadataResponse, RequestHeader, ResponseHeader,
};
use kafka_protocol::protocol::{Decodable, Encodable, HeaderVersion, StrBytes};

/// ApiVersions' answer: its error code and the (key, min, max) ranges.
#[cfg(test)]
type Versions = (i16, Vec<(i16, i16, i16)>);

/// Metadata's answer: brokers (id, host, port, rack), cluster id, controller, topics (error, name), error.
#[cfg(test)]
type Metadata = (
    Vec<(i32, String, i32, Option<String>)>,
    Option<String>,
    i32,
    Vec<(i16, Option<String>)>,
    i16,
);

#[cfg(test)]
#[derive(Default)]
struct Seen {
    versions: BTreeSet<Versions>,
    metadata: BTreeSet<Metadata>,
    /// Connections that got both answers.
    answered: u64,
}

/// A Kafka client in Rust: its requests encoded and its responses decoded by `kafka-protocol`.
#[cfg(test)]
struct OracleClient {
    seen: Rc<RefCell<Seen>>,
    buf: Vec<u8>,
    connected: bool,
    got: u8,
}

#[cfg(test)]
fn request<M: Encodable + HeaderVersion>(key: i16, version: i16, corr: i32, body: &M) -> Vec<u8> {
    let mut buf = BytesMut::new();
    RequestHeader::default()
        .with_request_api_key(key)
        .with_request_api_version(version)
        .with_correlation_id(corr)
        .with_client_id(Some(StrBytes::from_string("oracle".into())))
        .encode(&mut buf, M::header_version(version))
        .unwrap();
    body.encode(&mut buf, version).unwrap();
    let mut framed = (buf.len() as u32).to_be_bytes().to_vec();
    framed.extend_from_slice(&buf);
    framed
}

#[cfg(test)]
impl OracleClient {
    /// Records every complete response frame in the buffer.
    fn drain(&mut self) -> Result<(), String> {
        loop {
            let Some(n) = self
                .buf
                .get(..4)
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
            else {
                return Ok(());
            };
            if self.buf.len() < 4 + n {
                return Ok(());
            }
            let mut body = Bytes::copy_from_slice(&self.buf[4..4 + n]);
            self.buf.drain(..4 + n);
            let corr = i32::from_be_bytes([body[0], body[1], body[2], body[3]]);
            let mut seen = self.seen.borrow_mut();
            match corr {
                1 => {
                    let _ = ResponseHeader::decode(&mut body, ApiVersionsResponse::header_version(4))
                        .map_err(|e| e.to_string())?;
                    let r = ApiVersionsResponse::decode(&mut body, 4).map_err(|e| e.to_string())?;
                    let ranges = r
                        .api_keys
                        .iter()
                        .map(|k| (k.api_key, k.min_version, k.max_version))
                        .collect();
                    seen.versions.insert((r.error_code, ranges));
                }
                2 => {
                    let _ = ResponseHeader::decode(&mut body, MetadataResponse::header_version(13))
                        .map_err(|e| e.to_string())?;
                    let m = MetadataResponse::decode(&mut body, 13).map_err(|e| e.to_string())?;
                    seen.metadata.insert((
                        m.brokers
                            .iter()
                            .map(|b| {
                                (
                                    b.node_id.0,
                                    b.host.to_string(),
                                    b.port,
                                    b.rack.as_ref().map(|r| r.to_string()),
                                )
                            })
                            .collect(),
                        m.cluster_id.as_ref().map(|c| c.to_string()),
                        m.controller_id.0,
                        m.topics
                            .iter()
                            .map(|t| (t.error_code, t.name.as_ref().map(|n| n.0.to_string())))
                            .collect(),
                        m.error_code,
                    ));
                }
                other => return Err(format!("a response to correlation id {other}, which was never asked")),
            }
            if !body.is_empty() {
                return Err(format!("{} bytes after the response to {corr}", body.len()));
            }
            self.got += 1;
            if self.got == 2 {
                seen.answered += 1;
            }
        }
    }
}

#[cfg(test)]
impl StreamClient for OracleClient {
    fn on(&mut self, now: i64, e: StreamEvent<'_>) -> Result<StreamAction, String> {
        let mut a = StreamAction::default();
        match e {
            StreamEvent::Wake if !self.connected => {
                self.connected = true;
                a.connect = Some((NodeId(0), Arc::from("kafka")));
            }
            StreamEvent::Wake => {}
            StreamEvent::Opened => {
                self.buf.clear();
                self.got = 0;
                let versions = ApiVersionsRequest::default()
                    .with_client_software_name(StrBytes::from_string("oracle".into()))
                    .with_client_software_version(StrBytes::from_string("1".into()));
                a.send = request(18, 4, 1, &versions);
                a.send.extend(request(
                    3,
                    13,
                    2,
                    &MetadataRequest::default().with_allow_auto_topic_creation(true),
                ));
            }
            StreamEvent::Received(b) => {
                self.buf.extend_from_slice(b);
                self.drain()?;
            }
            StreamEvent::Closed(_) => {
                // Reconnect a little later.
                self.connected = false;
                a.wake = Some(now + 20_000_000);
            }
        }
        Ok(a)
    }
}

#[test]
fn the_blossom_client_and_a_rust_client_see_the_same_answers() {
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
    let expected_versions: Versions = (0, vec![(0, 10, 12), (1, 16, 17), (2, 7, 10), (3, 13, 13), (18, 3, 4), (19, 7, 7), (20, 6, 6), (22, 3, 5), (32, 4, 4)]);
    let expected_metadata: Metadata = (
        vec![(1, "b1.sim".into(), 9092, None)],
        Some("blossom-sim".into()),
        1,
        vec![],
        0,
    );
    for seed in 1..=6u64 {
        let cfg = ClusterConfig {
            seed,
            clients: 0,
            chunk_max: 1 + (seed as usize % 5),
            nemesis: 120_000_000,
            crashes: true,
            downtime: 40_000_000,
            stream_drops: true,
            duration: 2_000_000_000,
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
        let seen = Rc::new(RefCell::new(Seen::default()));
        for _ in 0..2 {
            cluster.stream_client(Box::new(OracleClient {
                seen: seen.clone(),
                buf: Vec::new(),
                connected: false,
                got: 0,
            }));
        }
        cluster.run_until(2_000_000_000).unwrap();
        let run = cluster.run_so_far();
        assert!(
            run.violation.is_none(),
            "seed {seed}: {:?}\n{}",
            run.violation,
            run.log.join("\n")
        );
        assert!(
            run.crashes > 0 || run.stream_resets > 0,
            "seed {seed}: no faults happened"
        );
        // The Rust client's answers.
        let (oracle_versions, oracle_metadata, answered) = {
            let o = seen.borrow();
            (o.versions.clone(), o.metadata.clone(), o.answered)
        };
        assert!(answered > 0, "seed {seed}: the Rust client got no answer");
        assert_eq!(
            oracle_versions,
            [expected_versions.clone()].into_iter().collect(),
            "seed {seed}"
        );
        assert_eq!(
            oracle_metadata,
            [expected_metadata.clone()].into_iter().collect(),
            "seed {seed}"
        );
        // The Blossom client's: its tables survive only its current incarnation, so run it on without faults until
        // it has answers, then compare.
        cluster.step_until(2_500_000_000).unwrap();
        let state = cluster.state(NodeId(1)).expect("the client node is up");
        let versions: BTreeSet<Versions> = state
            .rows(artifact.rel_named("versions_seen").unwrap())
            .map(|r| (int16(&r[0]), ranges(&r[1])))
            .collect();
        let metadata: BTreeSet<Metadata> = state
            .rows(artifact.rel_named("metadata_seen").unwrap())
            .map(|r| metadata_of(&r[0]))
            .collect();
        assert_eq!(
            versions, oracle_versions,
            "seed {seed}: the Blossom client's ApiVersions answers"
        );
        assert_eq!(
            metadata, oracle_metadata,
            "seed {seed}: the Blossom client's Metadata answers"
        );
    }
}

#[cfg(test)]
fn int16(v: &Value) -> i16 {
    match v {
        Value::Int(IntValue::I16(x)) => *x,
        other => panic!("{other:?}"),
    }
}

#[cfg(test)]
fn int32(v: &Value) -> i32 {
    match v {
        Value::Int(IntValue::I32(x)) => *x,
        other => panic!("{other:?}"),
    }
}

#[cfg(test)]
fn text(v: &Value) -> String {
    match v {
        Value::Str(s) => s.to_string(),
        other => panic!("{other:?}"),
    }
}

#[cfg(test)]
fn opt_text(v: &Value) -> Option<String> {
    match v {
        Value::Option(o) => o.as_deref().map(text),
        other => panic!("{other:?}"),
    }
}

#[cfg(test)]
fn fields(v: &Value) -> &[Value] {
    match v {
        Value::Struct(f) | Value::Tuple(f) => f,
        other => panic!("{other:?}"),
    }
}

#[cfg(test)]
fn items(v: &Value) -> &[Value] {
    match v {
        Value::Vec(xs) => xs,
        other => panic!("{other:?}"),
    }
}

#[cfg(test)]
fn ranges(v: &Value) -> Vec<(i16, i16, i16)> {
    items(v)
        .iter()
        .map(|r| {
            let f = fields(r);
            (int16(&f[0]), int16(&f[1]), int16(&f[2]))
        })
        .collect()
}

#[cfg(test)]
fn metadata_of(v: &Value) -> Metadata {
    let f = fields(v);
    let brokers = items(&f[0])
        .iter()
        .map(|b| {
            let b = fields(b);
            (int32(&b[0]), text(&b[1]), int32(&b[2]), opt_text(&b[3]))
        })
        .collect();
    let topics = items(&f[3])
        .iter()
        .map(|t| {
            let t = fields(t);
            (int16(&t[0]), opt_text(&t[1]))
        })
        .collect();
    (brokers, opt_text(&f[1]), int32(&f[2]), topics, int16(&f[4]))
}

/// A client that creates a topic, then sends a Fetch that long-polls its empty partition (`max_wait_ms` 300) and,
/// behind it on the same connection, `n` ApiVersions requests, 20 at a time every millisecond, without waiting for
/// answers. Kafka answers a connection's requests in order, so they queue behind the Fetch. It checks every answer
/// arrives, in order.
#[cfg(test)]
struct Pipeliner {
    n: i32,
    sent: i32,
    buf: Vec<u8>,
    next: Rc<RefCell<i32>>,
    connected: bool,
    topic: Option<uuid::Uuid>,
}

#[cfg(test)]
impl Pipeliner {
    fn burst(&mut self, now: i64, a: &mut StreamAction) {
        let versions = ApiVersionsRequest::default()
            .with_client_software_name(StrBytes::from_string("pipeliner".into()))
            .with_client_software_version(StrBytes::from_string("1".into()));
        for _ in 0..20 {
            if self.sent == self.n {
                return;
            }
            self.sent += 1;
            a.send.extend(request(18, 4, 2 + self.sent, &versions));
        }
        a.wake = Some(now + 1_000_000);
    }

    fn answer(&mut self, mut body: Bytes, now: i64, a: &mut StreamAction) -> Result<(), String> {
        use kafka_protocol::messages::{CreateTopicsResponse, FetchResponse};
        let corr = i32::from_be_bytes([body[0], body[1], body[2], body[3]]);
        let mut next = self.next.borrow_mut();
        if corr != *next + 1 {
            return Err(format!("answer {corr} after {}", *next));
        }
        *next += 1;
        drop(next);
        match corr {
            1 => {
                ResponseHeader::decode(&mut body, CreateTopicsResponse::header_version(7)).map_err(|e| e.to_string())?;
                let r = CreateTopicsResponse::decode(&mut body, 7).map_err(|e| e.to_string())?;
                let t = r.topics.first().ok_or("no topic created")?;
                if t.error_code != 0 {
                    return Err(format!("CreateTopics: error {}", t.error_code));
                }
                self.topic = Some(t.topic_id);
                let fetch = kafka_protocol::messages::FetchRequest::default()
                    .with_max_wait_ms(300)
                    .with_min_bytes(1)
                    .with_max_bytes(1 << 20)
                    .with_session_epoch(-1)
                    .with_topics(vec![
                        kafka_protocol::messages::fetch_request::FetchTopic::default()
                            .with_topic_id(t.topic_id)
                            .with_partitions(vec![
                                kafka_protocol::messages::fetch_request::FetchPartition::default()
                                    .with_partition(0)
                                    .with_current_leader_epoch(-1)
                                    .with_fetch_offset(0)
                                    .with_partition_max_bytes(1 << 20),
                            ]),
                    ]);
                a.send = request(1, 17, 2, &fetch);
                self.burst(now, a);
            }
            2 => {
                ResponseHeader::decode(&mut body, FetchResponse::header_version(17)).map_err(|e| e.to_string())?;
                let r = FetchResponse::decode(&mut body, 17).map_err(|e| e.to_string())?;
                if r.error_code != 0 {
                    return Err(format!("Fetch: error {}", r.error_code));
                }
            }
            _ => {
                ResponseHeader::decode(&mut body, ApiVersionsResponse::header_version(4)).map_err(|e| e.to_string())?;
                let r = ApiVersionsResponse::decode(&mut body, 4).map_err(|e| e.to_string())?;
                if r.error_code != 0 {
                    return Err(format!("ApiVersions: error {}", r.error_code));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
impl StreamClient for Pipeliner {
    fn on(&mut self, now: i64, e: StreamEvent<'_>) -> Result<StreamAction, String> {
        let mut a = StreamAction::default();
        match e {
            StreamEvent::Wake if !self.connected => {
                self.connected = true;
                a.connect = Some((NodeId(0), Arc::from("kafka")));
            }
            StreamEvent::Wake if self.topic.is_some() => self.burst(now, &mut a),
            StreamEvent::Wake => {}
            StreamEvent::Opened => {
                let create = kafka_protocol::messages::CreateTopicsRequest::default()
                    .with_topics(vec![
                        kafka_protocol::messages::create_topics_request::CreatableTopic::default()
                            .with_name(kafka_protocol::messages::TopicName(StrBytes::from_string("held".into())))
                            .with_num_partitions(1)
                            .with_replication_factor(1),
                    ])
                    .with_timeout_ms(1000);
                a.send = request(19, 7, 1, &create);
            }
            StreamEvent::Received(b) => {
                self.buf.extend_from_slice(b);
                while let Some(n) = self
                    .buf
                    .get(..4)
                    .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
                {
                    if self.buf.len() < 4 + n {
                        break;
                    }
                    let body = Bytes::copy_from_slice(&self.buf[4..4 + n]);
                    self.buf.drain(..4 + n);
                    self.answer(body, now, &mut a)?;
                }
            }
            StreamEvent::Closed(why) => return Err(format!("the broker closed the connection: {why}")),
        }
        Ok(a)
    }
}

/// Review of S7, finding #10: a client pipelining far more requests than a connection's queue holds, behind one that
/// waits, is not read while the queue is full (the simulator holds what it sends meanwhile), and every request is
/// answered, in order.
#[test]
fn a_pipelining_client_is_throttled_and_answered_in_order() {
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
    let params = [("MAX_QUEUED_REQUESTS".to_owned(), blossom_front::api::ParamBinding::Int(4))]
        .into_iter()
        .collect();
    let (result, _) = blossom_driver::bls::compile_file_with(path.to_str().unwrap(), &nodes, &params);
    let artifact: BlsArtifact = result.unwrap_or_else(|e| panic!("sim_cluster.bls: {e:?}")).0;
    let schema = DurableSchema::of(artifact.program.get());
    const N: i32 = 300;
    for seed in 1..=3u64 {
        let cfg = ClusterConfig {
            seed,
            clients: 0,
            chunk_max: 64,
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
        let next = Rc::new(RefCell::new(0));
        cluster.stream_client(Box::new(Pipeliner {
            n: N,
            sent: 0,
            buf: Vec::new(),
            next: next.clone(),
            connected: false,
            topic: None,
        }));
        cluster.run_until(3_000_000_000).unwrap();
        let run = cluster.run_so_far();
        assert!(run.violation.is_none(), "seed {seed}: {:?}\n{}", run.violation, run.log.join("\n"));
        assert_eq!(*next.borrow(), N + 2, "seed {seed}: every request is answered");
        assert!(run.stream_held > 0, "seed {seed}: the broker never stopped reading the connection");
    }
}
