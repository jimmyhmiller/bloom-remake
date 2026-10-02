//! Slice 7, item 4: the Blossom broker's topics (`examples/kafka/topics_node.bls`) under concurrent admin requests,
//! broker crashes and dropped connections, in the cluster simulator.
//!
//! Rust clients (requests encoded and responses decoded by `kafka-protocol`, decision K5) send random CreateTopics,
//! DeleteTopics, Metadata (with auto-creation) and DescribeConfigs requests over several connections at once. Every
//! request is recorded with the instants it was sent and answered (unanswered when its connection closed first), and
//! the history is checked for linearizability against a sequential model of Kafka's topic rules written here,
//! independently of the broker. At the end the broker's durable partition tables must match its topics.
//!
//! Slice 8: the same with three brokers under crashes, splits and one-way cuts, each client asking a random broker.
//! Creations and deletions go through the controller's log, so they stay linearizable whichever broker they reach;
//! Metadata and DescribeConfigs read a broker's own copy of the metadata, which may lag (as Kafka's do), so those runs
//! send creations (some only validating) and deletions only. At the end every broker holds the same topics, and each
//! replica the partitions it replicates.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file_with;
use blossom_front::api::NodeSpec;
use blossom_node::durable::DurableSchema;
use blossom_sim::cluster::{Cluster, ClusterConfig, CrashWrites, NoKvClients, StreamAction, StreamClient, StreamEvent};
use blossom_sim::linearize::{self, Model, Operation, Verdict};
use blossom_value::Value;
use blossom_value::time::NodeId;
use blossom_value::value::IntValue;
use bytes::{Bytes, BytesMut};
use kafka_protocol::messages::create_topics_request::{CreatableTopic, CreatableTopicConfig};
use kafka_protocol::messages::delete_topics_request::DeleteTopicState;
use kafka_protocol::messages::describe_configs_request::DescribeConfigsResource;
use kafka_protocol::messages::metadata_request::MetadataRequestTopic;
use kafka_protocol::messages::{
    CreateTopicsRequest, CreateTopicsResponse, DeleteTopicsRequest, DeleteTopicsResponse, DescribeConfigsRequest,
    DescribeConfigsResponse, MetadataRequest, MetadataResponse, RequestHeader, ResponseHeader, TopicName,
};
use kafka_protocol::protocol::{Decodable, Encodable, HeaderVersion, StrBytes};

#[cfg(test)]
type Id = [u8; 16];

#[cfg(test)]
/// A CreateTopics entry: name, partition count, replication factor and configs.
#[derive(Clone, Debug)]
struct Entry {
    name: String,
    partitions: i32,
    replication: i16,
    configs: Vec<(String, Option<String>)>,
}

#[cfg(test)]
#[derive(Clone, Debug)]
enum Input {
    Create {
        entries: Vec<Entry>,
        validate: bool,
    },
    /// Entries by name, or by id (a zero id means by name).
    Delete {
        entries: Vec<(Option<String>, Id)>,
    },
    /// Topics by name (`None`: all), and whether the request allows auto-creation.
    Metadata {
        names: Option<Vec<String>>,
        auto: bool,
    },
    Describe {
        names: Vec<String>,
    },
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
enum Output {
    /// Per entry: error, id, partition count.
    Create(Vec<(i16, Id, i32)>),
    /// Per entry: error, name, id.
    Delete(Vec<(i16, Option<String>, Id)>),
    /// Per topic: error, name, id, partition count.
    Metadata(Vec<(i16, Option<String>, Id, i32)>),
    /// Per resource: error, and the (name, value) of each configuration.
    Describe(Vec<(i16, Vec<(String, String)>)>),
}

#[cfg(test)]
/// A topic of the model: its id (unknown until an answer reveals it, for a creation that was never answered), its
/// partition count and its own configurations.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Topic {
    id: Option<Id>,
    partitions: i32,
    configs: Vec<(String, String)>,
}

#[cfg(test)]
type State = BTreeMap<String, Topic>;

#[cfg(test)]
const TOPIC_CONFIGS: [(&str, &str); 8] = [
    ("cleanup.policy", "delete"),
    ("compression.type", "producer"),
    ("max.message.bytes", "1048588"),
    ("message.timestamp.type", "CreateTime"),
    ("min.insync.replicas", "1"),
    ("retention.bytes", "-1"),
    ("retention.ms", "604800000"),
    ("segment.bytes", "1073741824"),
];

#[cfg(test)]
fn legal(n: &str) -> bool {
    (1..=249).contains(&n.len())
        && n != "."
        && n != ".."
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
fn key(n: &str) -> String {
    n.replace('.', "_")
}

#[cfg(test)]
fn config_ok(name: &str, value: &str) -> bool {
    let int = value.parse::<i64>().ok();
    match name {
        "cleanup.policy" => value == "delete",
        "compression.type" => value == "producer",
        "message.timestamp.type" => value == "CreateTime",
        "retention.ms" | "retention.bytes" => int.is_some_and(|n| n >= -1),
        "max.message.bytes" | "min.insync.replicas" | "segment.bytes" => {
            int.is_some_and(|n| (0..=i64::from(i32::MAX)).contains(&n))
        }
        _ => false,
    }
}

#[cfg(test)]
/// An id an answer gives for topic `name`: it must be the model's, or become it if the model does not know it yet.
fn learn(state: &mut State, name: &str, id: Id) -> bool {
    match state.get_mut(name) {
        Some(t) => match t.id {
            Some(known) => known == id,
            None => {
                t.id = Some(id);
                true
            }
        },
        None => false,
    }
}

#[cfg(test)]
/// Kafka's topic rules for one broker, applied one request at a time.
struct Topics {
    /// The brokers: a replication factor from 1 to this is valid.
    brokers: i16,
}

#[cfg(test)]
impl Topics {
    /// A CreateTopics request's error per entry, against `state`.
    fn create_errors(&self, state: &State, entries: &[Entry]) -> Vec<i16> {
        entries
            .iter()
            .map(|e| {
                let k = key(&e.name);
                if entries.iter().filter(|x| x.name == e.name).count() > 1 {
                    42
                } else if !legal(&e.name) || entries.iter().any(|x| x.name != e.name && key(&x.name) == k) {
                    17
                } else if state.contains_key(&e.name) {
                    36
                } else if state.keys().any(|n| key(n) == k) {
                    17
                } else if e.partitions != -1 && !(1..=10000).contains(&e.partitions) {
                    37
                } else if e.replication != -1 && !(1..=self.brokers).contains(&e.replication) {
                    38
                } else if e
                    .configs
                    .iter()
                    .any(|(n, v)| v.as_deref().is_none_or(|v| !config_ok(n, v)))
                {
                    40
                } else {
                    0
                }
            })
            .collect()
    }
}

#[cfg(test)]
impl Model for Topics {
    type State = State;
    type Input = Input;
    type Output = Output;

    fn init(&self) -> State {
        State::new()
    }

    fn step(&self, state: &State, input: &Input, output: Option<&Output>) -> Option<State> {
        let mut next = state.clone();
        match (input, output) {
            (Input::Create { entries, validate }, out) => {
                let errors = self.create_errors(state, entries);
                let got = match out {
                    Some(Output::Create(got)) => Some(got),
                    Some(_) => return None,
                    None => None,
                };
                if let Some(got) = got
                    && got.len() != entries.len()
                {
                    return None;
                }
                for (i, (e, err)) in entries.iter().zip(&errors).enumerate() {
                    let answer = got.map(|g| g[i]);
                    if let Some((code, id, parts)) = answer {
                        let want_parts = if *err != 0 {
                            -1
                        } else if e.partitions == -1 {
                            1
                        } else {
                            e.partitions
                        };
                        if code != *err || parts != want_parts {
                            return None;
                        }
                        // A validated or refused topic has the zero id; a created one another.
                        if (*err == 0 && !validate) != (id != [0; 16]) {
                            return None;
                        }
                    }
                    if *err == 0 && !validate {
                        let configs = e
                            .configs
                            .iter()
                            .filter_map(|(n, v)| v.clone().map(|v| (n.clone(), v)))
                            .collect();
                        next.insert(
                            e.name.clone(),
                            Topic {
                                id: answer.map(|a| a.1),
                                partitions: if e.partitions == -1 { 1 } else { e.partitions },
                                configs,
                            },
                        );
                    }
                }
                Some(next)
            }
            (Input::Delete { entries }, out) => {
                let got = match out {
                    Some(Output::Delete(got)) if got.len() == entries.len() => Some(got),
                    Some(_) => return None,
                    None => None,
                };
                for (i, (name, id)) in entries.iter().enumerate() {
                    let by_id = *id != [0; 16];
                    let dup = entries.iter().enumerate().any(|(j, x)| {
                        j != i
                            && if by_id {
                                x.1 == *id
                            } else {
                                x.1 == [0; 16] && x.0 == *name
                            }
                    });
                    let (code, found) = if dup || (by_id && name.is_some()) {
                        (42, None)
                    } else if by_id {
                        match state.iter().find(|(_, t)| t.id == Some(*id)) {
                            Some((n, _)) => (0, Some(n.clone())),
                            None => (100, None),
                        }
                    } else {
                        match name.as_ref().and_then(|n| state.get(n).map(|t| (n, t))) {
                            // A name whose topic another entry gives by id is refused (Kafka: "The provided topic
                            // name maps to an ID that was already supplied").
                            Some((_, t)) if t.id.is_some_and(|tid| entries.iter().any(|x| x.1 == tid)) => (42, None),
                            Some((n, _)) => (0, Some(n.clone())),
                            None => (3, None),
                        }
                    };
                    if let Some(got) = got {
                        let (gc, gn, gid) = &got[i];
                        if *gc != code {
                            return None;
                        }
                        match &found {
                            Some(n) => {
                                if gn.as_ref() != Some(n) || !learn(&mut next, n, *gid) {
                                    return None;
                                }
                            }
                            None => {
                                let want = if code == 100 { (None, *id) } else { (name.clone(), *id) };
                                if (gn.clone(), *gid) != want {
                                    return None;
                                }
                            }
                        }
                    }
                    if let Some(n) = found {
                        next.remove(&n);
                    }
                }
                Some(next)
            }
            (Input::Metadata { names, auto }, out) => {
                let got = match out {
                    Some(Output::Metadata(got)) => Some(got),
                    Some(_) => return None,
                    None => None,
                };
                // Auto-creation makes the legal names asked for that no topic or other asked name collides with.
                if let (Some(names), true) = (names, *auto) {
                    for n in names {
                        let k = key(n);
                        let alone = !names.iter().any(|m| m != n && key(m) == k);
                        if legal(n) && alone && !next.keys().any(|x| key(x) == k) {
                            next.insert(
                                n.clone(),
                                Topic {
                                    id: None,
                                    partitions: 1,
                                    configs: Vec::new(),
                                },
                            );
                        }
                    }
                }
                let Some(got) = got else { return Some(next) };
                let shown = |state: &mut State, n: &str, g: &(i16, Option<String>, Id, i32)| -> bool {
                    match state.get(n).cloned() {
                        Some(t) => g.0 == 0 && g.1.as_deref() == Some(n) && g.3 == t.partitions && learn(state, n, g.2),
                        None => {
                            let code = if legal(n) { 3 } else { 17 };
                            *g == (code, Some(n.to_owned()), [0; 16], 0)
                        }
                    }
                };
                match names {
                    Some(names) => {
                        if got.len() != names.len() {
                            return None;
                        }
                        for (n, g) in names.iter().zip(got) {
                            if !shown(&mut next, n, g) {
                                return None;
                            }
                        }
                    }
                    None => {
                        let mut listed: Vec<&str> = got.iter().filter_map(|g| g.1.as_deref()).collect();
                        listed.sort();
                        if got.len() != next.len() || listed != next.keys().map(String::as_str).collect::<Vec<_>>() {
                            return None;
                        }
                        for g in got {
                            let n = g.1.clone().unwrap_or_default();
                            if !shown(&mut next, &n, g) {
                                return None;
                            }
                        }
                    }
                }
                Some(next)
            }
            (Input::Describe { names }, out) => {
                let Some(out) = out else { return Some(next) };
                let Output::Describe(got) = out else { return None };
                let want: Vec<(i16, Vec<(String, String)>)> = names
                    .iter()
                    .map(|n| match state.get(n) {
                        Some(t) => (
                            0,
                            TOPIC_CONFIGS
                                .iter()
                                .map(|(k, d)| {
                                    let v = t.configs.iter().find(|c| c.0 == *k).map(|c| c.1.clone());
                                    (k.to_string(), v.unwrap_or_else(|| d.to_string()))
                                })
                                .collect(),
                        ),
                        None => (3, Vec::new()),
                    })
                    .collect();
                (*got == want).then_some(next)
            }
        }
    }
}

#[cfg(test)]
/// SplitMix64.
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
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

#[cfg(test)]
/// The names the clients use: some collide (`t.2` and `t_2`), one is illegal.
const NAMES: [&str; 6] = ["t0", "t1", "t.2", "t_2", "t3", "bad name"];

#[cfg(test)]
const CONFIGS: [(&str, Option<&str>); 6] = [
    ("retention.ms", Some("1000")),
    ("segment.bytes", Some("1048576")),
    ("cleanup.policy", Some("delete")),
    ("retention.ms", Some("-5")),
    ("cleanup.policy", Some("compact")),
    ("unknown.key", Some("x")),
];

#[cfg(test)]
fn random_input(rng: &mut Rng, ids: &[Id], names: &[&str], writes_only: bool) -> Input {
    match rng.below(if writes_only { 6 } else { 10 }) {
        0..=3 => Input::Create {
            entries: (0..1 + rng.below(3))
                .map(|_| Entry {
                    name: rng.pick(names).to_string(),
                    partitions: *rng.pick(&[-1, 1, 2, 3, 0]),
                    replication: *rng.pick(&[-1, 1, 1, 3]),
                    configs: (0..rng.below(2))
                        .map(|_| {
                            let (k, v) = *rng.pick(&CONFIGS);
                            (
                                k.to_owned(),
                                if rng.below(8) == 0 { None } else { v.map(str::to_owned) },
                            )
                        })
                        .collect(),
                })
                .collect(),
            validate: rng.below(5) == 0,
        },
        4 | 5 => Input::Delete {
            entries: (0..1 + rng.below(2))
                .map(|_| {
                    if !ids.is_empty() && rng.below(3) == 0 {
                        (None, *rng.pick(ids))
                    } else if rng.below(10) == 0 {
                        (None, [7; 16])
                    } else {
                        (Some(rng.pick(names).to_string()), [0; 16])
                    }
                })
                .collect(),
        },
        6..=8 => Input::Metadata {
            names: if rng.below(3) == 0 {
                None
            } else {
                Some((0..1 + rng.below(2)).map(|_| rng.pick(names).to_string()).collect())
            },
            auto: rng.below(2) == 0,
        },
        _ => Input::Describe {
            names: (0..1 + rng.below(2)).map(|_| rng.pick(names).to_string()).collect(),
        },
    }
}

#[cfg(test)]
fn framed<M: Encodable + HeaderVersion>(key: i16, version: i16, corr: i32, body: &M) -> Vec<u8> {
    let mut buf = BytesMut::new();
    RequestHeader::default()
        .with_request_api_key(key)
        .with_request_api_version(version)
        .with_correlation_id(corr)
        .with_client_id(Some(StrBytes::from_string("topics".into())))
        .encode(&mut buf, M::header_version(version))
        .unwrap();
    body.encode(&mut buf, version).unwrap();
    let mut out = (buf.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&buf);
    out
}

#[cfg(test)]
fn topic_name(n: &str) -> TopicName {
    TopicName(StrBytes::from_string(n.to_owned()))
}

#[cfg(test)]
fn encode(input: &Input, corr: i32, timeout_ms: i32) -> Vec<u8> {
    match input {
        Input::Create { entries, validate } => {
            let topics = entries
                .iter()
                .map(|e| {
                    CreatableTopic::default()
                        .with_name(topic_name(&e.name))
                        .with_num_partitions(e.partitions)
                        .with_replication_factor(e.replication)
                        .with_configs(
                            e.configs
                                .iter()
                                .map(|(k, v)| {
                                    CreatableTopicConfig::default()
                                        .with_name(StrBytes::from_string(k.clone()))
                                        .with_value(v.clone().map(StrBytes::from_string))
                                })
                                .collect(),
                        )
                })
                .collect();
            let req = CreateTopicsRequest::default()
                .with_topics(topics)
                .with_timeout_ms(timeout_ms)
                .with_validate_only(*validate);
            framed(19, 7, corr, &req)
        }
        Input::Delete { entries } => {
            let req = DeleteTopicsRequest::default()
                .with_topics(
                    entries
                        .iter()
                        .map(|(n, id)| {
                            DeleteTopicState::default()
                                .with_name(n.as_deref().map(topic_name))
                                .with_topic_id(uuid::Uuid::from_bytes(*id))
                        })
                        .collect(),
                )
                .with_timeout_ms(timeout_ms);
            framed(20, 6, corr, &req)
        }
        Input::Metadata { names, auto } => {
            let req = MetadataRequest::default()
                .with_topics(names.as_ref().map(|ns| {
                    ns.iter()
                        .map(|n| MetadataRequestTopic::default().with_name(Some(topic_name(n))))
                        .collect()
                }))
                .with_allow_auto_topic_creation(*auto);
            framed(3, 13, corr, &req)
        }
        Input::Describe { names } => {
            let req = DescribeConfigsRequest::default().with_resources(
                names
                    .iter()
                    .map(|n| {
                        DescribeConfigsResource::default()
                            .with_resource_type(2)
                            .with_resource_name(StrBytes::from_string(n.clone()))
                    })
                    .collect(),
            );
            framed(32, 4, corr, &req)
        }
    }
}

#[cfg(test)]
fn e<E: std::fmt::Display>(x: E) -> String {
    x.to_string()
}

#[cfg(test)]
fn decode(input: &Input, mut body: Bytes) -> Result<Output, String> {
    let out = match input {
        Input::Create { .. } => {
            ResponseHeader::decode(&mut body, CreateTopicsResponse::header_version(7)).map_err(e)?;
            let r = CreateTopicsResponse::decode(&mut body, 7).map_err(e)?;
            Output::Create(
                r.topics
                    .iter()
                    .map(|t| (t.error_code, *t.topic_id.as_bytes(), t.num_partitions))
                    .collect(),
            )
        }
        Input::Delete { .. } => {
            ResponseHeader::decode(&mut body, DeleteTopicsResponse::header_version(6)).map_err(e)?;
            let r = DeleteTopicsResponse::decode(&mut body, 6).map_err(e)?;
            Output::Delete(
                r.responses
                    .iter()
                    .map(|t| {
                        (
                            t.error_code,
                            t.name.as_ref().map(|n| n.0.to_string()),
                            *t.topic_id.as_bytes(),
                        )
                    })
                    .collect(),
            )
        }
        Input::Metadata { .. } => {
            ResponseHeader::decode(&mut body, MetadataResponse::header_version(13)).map_err(e)?;
            let r = MetadataResponse::decode(&mut body, 13).map_err(e)?;
            Output::Metadata(
                r.topics
                    .iter()
                    .map(|t| {
                        (
                            t.error_code,
                            t.name.as_ref().map(|n| n.0.to_string()),
                            *t.topic_id.as_bytes(),
                            t.partitions.len() as i32,
                        )
                    })
                    .collect(),
            )
        }
        Input::Describe { .. } => {
            ResponseHeader::decode(&mut body, DescribeConfigsResponse::header_version(4)).map_err(e)?;
            let r = DescribeConfigsResponse::decode(&mut body, 4).map_err(e)?;
            Output::Describe(
                r.results
                    .iter()
                    .map(|x| {
                        (
                            x.error_code,
                            x.configs
                                .iter()
                                .map(|c| {
                                    (
                                        c.name.to_string(),
                                        c.value.as_ref().map(|v| v.to_string()).unwrap_or_default(),
                                    )
                                })
                                .collect(),
                        )
                    })
                    .collect(),
            )
        }
    };
    if !body.is_empty() {
        return Err(format!("{} bytes after a response", body.len()));
    }
    Ok(out)
}

#[cfg(test)]
/// What the clients share: the history, and every topic id an answer revealed (for deletions by id).
#[derive(Default)]
struct Shared {
    history: Vec<Operation<Input, Output>>,
    ids: Vec<Id>,
    /// Requests answered REQUEST_TIMED_OUT (recorded unanswered).
    timeouts: usize,
}

#[cfg(test)]
/// A client: one request at a time over its connection, reconnecting after a close.
struct Client {
    shared: Rc<RefCell<Shared>>,
    rng: Rng,
    left: u32,
    connected: bool,
    /// Whether its connection is open.
    open: bool,
    buf: Vec<u8>,
    /// The request in flight: its index in the history and correlation id.
    pending: Option<(usize, i32)>,
    corr: i32,
    /// When set, the client sends only at multiples of this many nanoseconds, so the clients' requests arrive
    /// together and are answered in one tick; otherwise it thinks a random while between requests.
    grid: Option<i64>,
    /// The names it uses.
    names: &'static [&'static str],
    /// How many brokers there are (each connection goes to a random one), and whether it sends creations and
    /// deletions only.
    brokers: u32,
    writes_only: bool,
}

#[cfg(test)]
impl Client {
    fn issue(&mut self, now: i64, a: &mut StreamAction) {
        if self.left == 0 {
            return;
        }
        self.left -= 1;
        self.corr += 1;
        let input = random_input(&mut self.rng, &self.shared.borrow().ids, self.names, self.writes_only);
        // Three brokers: a request whose command the controller does not apply within a second answers
        // REQUEST_TIMED_OUT (recorded unanswered).
        a.send = encode(&input, self.corr, if self.brokers > 1 { 1_000 } else { 30_000 });
        let mut sh = self.shared.borrow_mut();
        sh.history.push(Operation {
            call: now as u64,
            ret: None,
            input,
            output: None,
        });
        self.pending = Some((sh.history.len() - 1, self.corr));
    }
}

#[cfg(test)]
impl StreamClient for Client {
    fn on(&mut self, now: i64, e: StreamEvent<'_>) -> Result<StreamAction, String> {
        let mut a = StreamAction::default();
        match e {
            StreamEvent::Wake => {
                if !self.connected && self.left > 0 {
                    self.connected = true;
                    self.open = false;
                    a.connect = Some((
                        NodeId(self.rng.below(u64::from(self.brokers)) as u32),
                        Arc::from("kafka"),
                    ));
                } else if self.open && self.pending.is_none() {
                    self.issue(now, &mut a);
                }
            }
            StreamEvent::Opened => {
                self.buf.clear();
                self.open = true;
                match self.grid {
                    Some(g) => a.wake = Some((now / g + 1) * g),
                    None => self.issue(now, &mut a),
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
                if self.buf.len() > 4 + n {
                    return Err("a response arrived with no request in flight".into());
                }
                let body = Bytes::copy_from_slice(&self.buf[4..]);
                self.buf.clear();
                let corr = i32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                let Some((at, want)) = self.pending.take() else {
                    return Err("a response arrived with no request in flight".into());
                };
                if corr != want {
                    return Err(format!("the response to {corr} arrived while {want} was in flight"));
                }
                {
                    let mut sh = self.shared.borrow_mut();
                    let out = decode(&sh.history[at].input, body)?;
                    let revealed: Vec<Id> = match &out {
                        Output::Create(xs) => xs.iter().map(|x| x.1).collect(),
                        Output::Delete(xs) => xs.iter().map(|x| x.2).collect(),
                        Output::Metadata(xs) => xs.iter().map(|x| x.2).collect(),
                        Output::Describe(_) => Vec::new(),
                    };
                    sh.ids.extend(revealed.into_iter().filter(|id| *id != [0; 16]));
                    // REQUEST_TIMED_OUT: the controller did not apply the request in time; it may still. The request
                    // stays unanswered (it took effect at some point after it was sent, or never).
                    let timed_out = match &out {
                        Output::Create(xs) => xs.iter().any(|x| x.0 == 7),
                        Output::Delete(xs) => xs.iter().any(|x| x.0 == 7),
                        _ => false,
                    };
                    if timed_out {
                        sh.timeouts += 1;
                    } else {
                        let op = &mut sh.history[at];
                        op.ret = Some(now as u64);
                        op.output = Some(out);
                    }
                }
                // Think a while, so requests stay in flight across the nemesis's faults (or wait for the grid).
                a.wake = Some(match self.grid {
                    Some(g) => (now / g + 1) * g,
                    None => now + self.rng.below(60_000_000) as i64,
                });
            }
            StreamEvent::Closed(_) => {
                // The request in flight stays unanswered: it may or may not have taken effect.
                self.pending = None;
                self.connected = false;
                self.open = false;
                a.wake = Some(now + 15_000_000);
            }
        }
        Ok(a)
    }
}

#[cfg(test)]
fn bytes_of(v: &Value) -> Vec<u8> {
    match v {
        Value::Bytes(b) => b.to_vec(),
        other => panic!("{other:?}"),
    }
}

#[cfg(test)]
fn int(v: &Value) -> i64 {
    match v {
        Value::Int(IntValue::I32(x)) => i64::from(*x),
        Value::Int(IntValue::I64(x)) => *x,
        Value::Int(IntValue::U64(x)) => i64::try_from(*x).unwrap(),
        other => panic!("{other:?}"),
    }
}

#[cfg(test)]
/// The controller's Raft group (`controller()` in `cluster.bls`).
fn controller() -> Value {
    let mut id = vec![0u8; 16];
    id[15] = 1;
    Value::Tuple(vec![Value::Bytes(id.into()), Value::Int(IntValue::I32(0))].into())
}

#[cfg(test)]
/// How a run is set up: its fault schedule, clients and names.
struct Setup {
    seeds: std::ops::RangeInclusive<u64>,
    crashes: bool,
    stream_drops: bool,
    latency: (i64, i64),
    clients: u64,
    requests: u32,
    grid: Option<i64>,
    names: &'static [&'static str],
    brokers: u32,
    /// Splits and one-way cuts between the brokers.
    partitions: bool,
    /// Parameters the program is compiled with (name, value: an integer, else text such as a duration).
    params: &'static [(&'static str, &'static str)],
    /// The last broker is stopped over this span (from, to): the others go on without it.
    down: Option<(i64, i64)>,
}

#[cfg(test)]
/// Runs each seed and checks it; returns how many requests were answered, left unanswered, and sent at the same
/// instant as another client's.
fn check_runs(setup: &Setup) -> (usize, usize, usize, usize) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/kafka/sim_cluster.bls");
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
    let params = setup
        .params
        .iter()
        .map(|(n, v)| {
            let binding = match v.parse::<i128>() {
                Ok(i) => blossom_front::api::ParamBinding::Int(i),
                Err(_) => blossom_front::api::ParamBinding::Text((*v).into()),
            };
            ((*n).to_owned(), binding)
        })
        .collect();
    let (result, _) = compile_file_with(path.to_str().unwrap(), &nodes, &params);
    let artifact: BlsArtifact = result.unwrap_or_else(|e| panic!("sim_cluster.bls: {e:?}")).0;
    let schema = DurableSchema::of(artifact.program.get());
    let rel = |n: &str| artifact.rel_named(n).unwrap();
    let ctl = controller();
    let (mut answered, mut unanswered, mut together, mut timeouts) = (0, 0, 0, 0);
    for seed in setup.seeds.clone() {
        let cfg = ClusterConfig {
            seed,
            clients: 0,
            latency: setup.latency,
            chunk_max: 1 + (seed as usize % 7) * 16,
            nemesis: 150_000_000,
            crashes: setup.crashes && seed % 4 != 0,
            partitions: setup.partitions,
            downtime: 40_000_000,
            stream_drops: setup.stream_drops,
            duration: 3_000_000_000,
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
        let shared = Rc::new(RefCell::new(Shared::default()));
        for c in 0..setup.clients {
            cluster.stream_client(Box::new(Client {
                shared: shared.clone(),
                rng: Rng(seed * 1000 + c),
                left: setup.requests,
                connected: false,
                open: false,
                buf: Vec::new(),
                pending: None,
                corr: 0,
                grid: setup.grid,
                names: setup.names,
                brokers: setup.brokers,
                writes_only: setup.brokers > 1,
            }));
        }
        let mut had = 0;
        if let Some((from, to)) = setup.down {
            let stopped = NodeId(setup.brokers - 1);
            cluster.run_until(from).unwrap();
            // The last controller entry the stopped broker holds (at most this: a crash may lose unsynced writes).
            had = cluster
                .state(stopped)
                .unwrap()
                .rows(rel("rlog"))
                .filter(|r| r[0] == ctl)
                .map(|r| int(&r[1]))
                .max()
                .unwrap_or(0);
            cluster.crash(stopped, CrashWrites::Random).unwrap();
            cluster.run_until(to).unwrap();
            // Every other broker compacted the controller's log past it: whichever leads can only send it the
            // applied metadata.
            for n in (0..setup.brokers - 1).map(NodeId) {
                let point = cluster
                    .state(n)
                    .unwrap_or_else(|| panic!("seed {seed}: broker {n:?} is down"))
                    .rows(rel("rsnap"))
                    .find(|r| r[0] == ctl)
                    .map(|r| int(&r[1]))
                    .unwrap_or(0);
                assert!(
                    point > had,
                    "seed {seed}: broker {n:?} compacted the controller's log to {point}, not past {had}"
                );
            }
            cluster.restart(stopped).unwrap();
        }
        cluster.run_until(3_000_000_000).unwrap();
        let run = cluster.run_so_far();
        assert!(
            run.violation.is_none(),
            "seed {seed}: {:?}\n{}",
            run.violation,
            run.log.join("\n")
        );
        let history = shared.borrow().history.clone();
        timeouts += shared.borrow().timeouts;
        answered += history.iter().filter(|o| o.ret.is_some()).count();
        unanswered += history.iter().filter(|o| o.ret.is_none()).count();
        together += history
            .iter()
            .enumerate()
            .filter(|(i, o)| history.iter().enumerate().any(|(j, p)| j != *i && p.call == o.call))
            .count();
        let model = Topics {
            brokers: setup.brokers as i16,
        };
        match linearize::check(&model, &history, 5_000_000) {
            Verdict::Linearizable => {}
            other => {
                let lines: Vec<String> = history.iter().enumerate().map(|(i, o)| format!("{i}: {o:?}")).collect();
                panic!("seed {seed}: {other:?}\n{}\n{}", lines.join("\n"), run.log.join("\n"));
            }
        }
        // Let the brokers settle, then check their tables agree: every broker holds the same topics and placements,
        // and each replica exactly its partitions' offsets.
        cluster.heal();
        cluster.step_until(4_000_000_000).unwrap();
        let rows = |n: u32, rel: &str| -> Vec<Vec<Value>> {
            let state = cluster
                .state(NodeId(n))
                .unwrap_or_else(|| panic!("seed {seed}: broker {n} is down"));
            let mut rs: Vec<Vec<Value>> = state
                .rows(artifact.rel_named(rel).unwrap())
                .map(|r| r.to_vec())
                .collect();
            rs.sort();
            rs
        };
        let topics = rows(0, "mtopic");
        let assigned = rows(0, "massign");
        for n in 1..setup.brokers {
            assert_eq!(
                rows(n, "mtopic"),
                topics,
                "seed {seed}: brokers 0 and {n} hold different topics"
            );
            assert_eq!(
                rows(n, "massign"),
                assigned,
                "seed {seed}: brokers 0 and {n} place topics differently"
            );
            // The rest of the applied metadata is the same too: it is a function of the applied index.
            for name in ["applied_m", "mreassign", "mdeleted", "done", "outcome", "next_pid"] {
                assert_eq!(
                    rows(n, name),
                    rows(0, name),
                    "seed {seed}: brokers 0 and {n} differ in {name}"
                );
            }
        }
        // The applied commands are kept for `DONE_WINDOW` entries, not forever.
        if let Some((_, w)) = setup.params.iter().find(|(n, _)| *n == "DONE_WINDOW") {
            let w: usize = w.parse().unwrap();
            let done = rows(0, "done");
            assert!(
                done.len() <= w,
                "seed {seed}: {} applied commands kept, past the window of {w}",
                done.len()
            );
            let deleted = rows(0, "mdeleted").len();
            assert!(
                deleted <= w,
                "seed {seed}: {deleted} deleted topics kept, past the window of {w}"
            );
            // An outcome goes with its command's `done` row.
            for r in rows(0, "outcome") {
                assert!(
                    done.iter().any(|d| d[0] == r[0] && d[1] == r[1]),
                    "seed {seed}: an outcome outlived its command: {r:?}"
                );
            }
        }
        if setup.down.is_some() {
            // The stopped broker came back to a snapshot point past what it held (InstallSnapshot), so it applied no
            // entry up to it: its metadata, the same as the others', is the leader's it adopted.
            let point = rows(setup.brokers - 1, "rsnap")
                .iter()
                .find(|r| r[0] == ctl)
                .map(|r| int(&r[1]))
                .unwrap_or(0);
            assert!(
                point > had,
                "seed {seed}: the stopped broker's controller snapshot point {point} is not past {had}"
            );
        }
        for n in 0..setup.brokers {
            let id = i64::from(n + 1);
            let mut want: Vec<(Vec<u8>, i64, i64)> = assigned
                .iter()
                .filter(|r| match &r[2] {
                    Value::Vec(rs) => rs.iter().any(|x| int(x) == id),
                    other => panic!("{other:?}"),
                })
                .map(|r| (bytes_of(&r[0]), int(&r[1]), 0))
                .collect();
            want.sort();
            for rel in ["log_end", "log_start"] {
                let got: Vec<(Vec<u8>, i64, i64)> = rows(n, rel)
                    .iter()
                    .map(|r| (bytes_of(&r[0]), int(&r[1]), int(&r[2])))
                    .collect();
                assert_eq!(got, want, "seed {seed}: broker {n}'s {rel}");
            }
        }
    }
    (answered, unanswered, together, timeouts)
}

/// Requests in flight across broker crashes and dropped connections: an acknowledged change survives, and an
/// unanswered one happened or did not.
#[test]
fn admin_requests_are_linearizable_across_crashes() {
    let (answered, unanswered, _, _) = check_runs(&Setup {
        seeds: 1..=8,
        crashes: true,
        stream_drops: true,
        latency: ClusterConfig::default().latency,
        clients: 3,
        requests: 20,
        grid: None,
        names: &NAMES,
        brokers: 1,
        partitions: false,
        params: &[],
        down: None,
    });
    assert!(answered > 300, "only {answered} requests were answered");
    assert!(
        unanswered > 0,
        "no request was left unanswered: the faults did not bite"
    );
}

/// Requests that arrive together, over a few names, are answered in one tick: creations and deletions of one topic
/// race, and each tick's answers must still fit one order.
#[test]
fn admin_requests_answered_in_one_tick_are_linearizable() {
    let (answered, _, together, _) = check_runs(&Setup {
        seeds: 1..=10,
        crashes: false,
        stream_drops: false,
        latency: (1_000_000, 1_000_000),
        clients: 5,
        requests: 25,
        grid: Some(10_000_000),
        names: &["t0", "t.1", "t_1"],
        brokers: 1,
        partitions: false,
        params: &[],
        down: None,
    });
    assert!(answered > 1000, "only {answered} requests were answered");
    assert!(
        together > 1000,
        "only {together} requests were sent together with another"
    );
}

/// Three brokers under crashes, splits, one-way cuts and dropped connections: creations and deletions through any
/// broker fit one order (the controller's log), and every broker ends with the same topics.
#[test]
fn admin_requests_through_any_of_three_brokers_are_linearizable() {
    let (answered, unanswered, _, timeouts) = check_runs(&Setup {
        seeds: 1..=8,
        crashes: true,
        stream_drops: true,
        latency: ClusterConfig::default().latency,
        clients: 4,
        requests: 20,
        grid: None,
        names: &NAMES,
        brokers: 3,
        partitions: true,
        params: &[],
        down: None,
    });
    assert!(answered > 300, "only {answered} requests were answered");
    assert!(
        unanswered > 0,
        "no request was left unanswered: the faults did not bite"
    );
    assert!(
        timeouts > 0,
        "no request timed out: the controller was never unavailable long enough"
    );
}

/// The controller's log compacted under the admin requests (a snapshot point every few entries, the applied commands
/// kept for ten), with one broker stopped while the others compact past it: it comes back from the leader's
/// applied metadata, and every broker ends with the same metadata. The requests stay linearizable.
#[test]
fn a_broker_behind_the_controllers_snapshot_adopts_the_leaders_metadata() {
    let (answered, _, _, _) = check_runs(&Setup {
        seeds: 1..=4,
        crashes: false,
        stream_drops: false,
        latency: ClusterConfig::default().latency,
        clients: 4,
        requests: 30,
        grid: None,
        names: &NAMES,
        brokers: 3,
        partitions: false,
        params: &[("CONTROLLER_KEEP", "4"), ("DONE_WINDOW", "10")],
        down: Some((300_000_000, 1_800_000_000)),
    });
    assert!(answered > 200, "only {answered} requests were answered");
}

/// The three-broker faults of `admin_requests_through_any_of_three_brokers_are_linearizable` with the controller's
/// log compacted every few entries and the applied commands kept for a few dozen: a broker partitioned away catches
/// up from snapshots, and a command sent again after its `done` row is gone is stale and skipped.
#[test]
fn admin_requests_stay_linearizable_with_the_controller_compacted() {
    let (answered, unanswered, _, _) = check_runs(&Setup {
        seeds: 1..=8,
        crashes: true,
        stream_drops: true,
        latency: ClusterConfig::default().latency,
        clients: 4,
        requests: 20,
        grid: None,
        names: &NAMES,
        brokers: 3,
        partitions: true,
        params: &[("CONTROLLER_KEEP", "3"), ("DONE_WINDOW", "12")],
        down: None,
    });
    assert!(answered > 300, "only {answered} requests were answered");
    assert!(
        unanswered > 0,
        "no request was left unanswered: the faults did not bite"
    );
}
