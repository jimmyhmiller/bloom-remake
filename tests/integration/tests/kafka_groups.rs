//! Slice 9, item 6: consumer groups on the Kafka broker (`examples/kafka`) as a cluster of three brokers in the
//! cluster simulator, under crashes, downtime, splits, one-way cuts and dropped connections.
//!
//! Rust group members (encoded and decoded by `kafka-protocol`) behave as Kafka's consumers do: they find the group's
//! coordinator, join (MEMBER_ID_REQUIRED, then with the id), the leader assigns the topic's partitions, everyone syncs,
//! heartbeats, commits offsets for the partitions it owns, and fetches the committed offsets; on an error they rejoin
//! or look for the coordinator again, as the Java consumer does. One member speaks the legacy versions librdkafka uses,
//! the others the latest. Members join over time; one leaves, one dies (falls silent). An observer checks Raft's
//! safety per group all along. The checks:
//! - every member's SyncGroup assignment is the one its generation's leader sent for it;
//! - an OffsetFetch never returns, for a partition, less than an offset whose commit was acknowledged before the
//!   fetch was sent (committed offsets never move backwards, and acknowledged ones survive coordinator failover);
//! - after the faults stop, the live members settle into one generation whose assignments cover every partition once,
//!   and every replica of an offsets partition holds the same committed offsets.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_front::api::NodeSpec;
use blossom_integration_tests::raft_safety::GroupSafety;
use blossom_integration_tests::seeds;
use blossom_node::durable::DurableSchema;
use blossom_sim::cluster::{Cluster, ClusterConfig, NoKvClients, StreamAction, StreamClient, StreamEvent};
use blossom_value::Value;
use blossom_value::time::NodeId;
use blossom_value::value::IntValue;
use bytes::{Bytes, BytesMut};
use kafka_protocol::messages::create_topics_request::CreatableTopic;
use kafka_protocol::messages::join_group_request::JoinGroupRequestProtocol;
use kafka_protocol::messages::leave_group_request::MemberIdentity;
use kafka_protocol::messages::offset_commit_request::{OffsetCommitRequestPartition, OffsetCommitRequestTopic};
use kafka_protocol::messages::offset_fetch_request::{
    OffsetFetchRequestGroup, OffsetFetchRequestTopic, OffsetFetchRequestTopics,
};
use kafka_protocol::messages::sync_group_request::SyncGroupRequestAssignment;
use kafka_protocol::messages::{
    CreateTopicsRequest, CreateTopicsResponse, FindCoordinatorRequest, FindCoordinatorResponse, GroupId,
    HeartbeatRequest, HeartbeatResponse, JoinGroupRequest, JoinGroupResponse, LeaveGroupRequest, LeaveGroupResponse,
    OffsetCommitRequest, OffsetCommitResponse, OffsetFetchRequest, OffsetFetchResponse, RequestHeader, ResponseHeader,
    SyncGroupRequest, SyncGroupResponse, TopicName,
};
use kafka_protocol::protocol::{Decodable, Encodable, HeaderVersion, StrBytes};

#[cfg(test)]
const TOPIC: &str = "events";
#[cfg(test)]
const PARTITIONS: i32 = 6;
#[cfg(test)]
const GROUP: &str = "readers";
#[cfg(test)]
const NONE: i16 = 0;
#[cfg(test)]
const REQUEST_TIMED_OUT: i16 = 7;
#[cfg(test)]
const COORDINATOR_LOAD_IN_PROGRESS: i16 = 14;
#[cfg(test)]
const COORDINATOR_NOT_AVAILABLE: i16 = 15;
#[cfg(test)]
const NOT_COORDINATOR: i16 = 16;
#[cfg(test)]
const ILLEGAL_GENERATION: i16 = 22;
#[cfg(test)]
const UNKNOWN_MEMBER_ID: i16 = 25;
#[cfg(test)]
const REBALANCE_IN_PROGRESS: i16 = 27;
#[cfg(test)]
const TOPIC_ALREADY_EXISTS: i16 = 36;
#[cfg(test)]
const MEMBER_ID_REQUIRED: i16 = 79;
/// The members' session and rebalance timeouts (the brokers' minimum session timeout is lowered to allow them).
#[cfg(test)]
const SESSION_MS: i32 = 1500;
#[cfg(test)]
const REBALANCE_MS: i32 = 3000;
/// A request is given up after this long (a JoinGroup waits for the rebalance, so longer), as Kafka's clients do.
#[cfg(test)]
const REQUEST_TIMEOUT: i64 = 1_500_000_000;
#[cfg(test)]
const JOIN_TIMEOUT: i64 = 6_000_000_000;
#[cfg(test)]
const MS: i64 = 1_000_000;

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

#[cfg(test)]
fn sb(x: &str) -> StrBytes {
    StrBytes::from_string(x.to_owned())
}

/// A request frame: size, header (with this client id), body.
#[cfg(test)]
fn framed<M: Encodable + HeaderVersion>(key: i16, version: i16, corr: i32, body: &M) -> Vec<u8> {
    let mut buf = BytesMut::new();
    RequestHeader::default()
        .with_request_api_key(key)
        .with_request_api_version(version)
        .with_correlation_id(corr)
        .with_client_id(Some(sb("member")))
        .encode(&mut buf, M::header_version(version))
        .unwrap();
    body.encode(&mut buf, version).unwrap();
    let mut out = (buf.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&buf);
    out
}

/// An assignment's bytes: the partitions, four bytes each (the members' own format; the broker passes it on).
#[cfg(test)]
fn assignment_bytes(ps: &[i32]) -> Bytes {
    Bytes::from(ps.iter().flat_map(|p| p.to_be_bytes()).collect::<Vec<u8>>())
}

#[cfg(test)]
fn assignment_of(b: &[u8]) -> Vec<i32> {
    b.chunks(4)
        .map(|c| i32::from_be_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// What the members saw, for the checks.
#[cfg(test)]
#[derive(Default)]
struct Shared {
    created: bool,
    /// Each generation's leader assignment, by (leader id, generation): member id → partitions.
    sent: BTreeMap<(String, i32), BTreeMap<String, Vec<i32>>>,
    /// Each SyncGroup answer: (leader id, generation, member id, partitions).
    received: Vec<(String, i32, String, Vec<i32>)>,
    /// Each acknowledged commit, per partition: (when its request was sent, when answered, offset).
    acked: BTreeMap<i32, Vec<(i64, i64, i64)>>,
    /// Every offset a commit carried (acknowledged or not), per partition.
    tried: BTreeMap<i32, BTreeSet<i64>>,
    /// Counters.
    joins: u64,
    generations: BTreeSet<(String, i32)>,
    commits: u64,
    fetches: u64,
    failovers: u64,
    /// Each live member's last state: (member id, leader id, generation, partitions), when steady.
    steady: BTreeMap<u64, Option<Settled>>,
}

/// A steady member: its id, its generation's leader and number, and its partitions.
#[cfg(test)]
type Settled = (String, String, i32, Vec<i32>);
/// A request in flight: what it is, when it was sent, when it is given up, and a commit's (partition, offset)s.
#[cfg(test)]
type InFlight = (Req, i64, i64, Vec<(i32, i64)>);

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Create,
    Find,
    Join,
    Sync,
    Steady,
    /// Left the group, or died: it sends nothing more.
    Gone,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
enum Req {
    Create,
    Find,
    Join,
    Sync,
    Beat,
    Commit,
    Fetch,
    Leave,
}

/// The API versions a member speaks: librdkafka's legacy ones, or the latest (the Java client's).
#[cfg(test)]
#[derive(Clone, Copy)]
struct Versions {
    find: i16,
    join: i16,
    sync: i16,
    beat: i16,
    leave: i16,
    commit: i16,
    fetch: i16,
}

#[cfg(test)]
const LEGACY: Versions = Versions {
    find: 2,
    join: 5,
    sync: 3,
    beat: 3,
    leave: 1,
    commit: 7,
    fetch: 7,
};
#[cfg(test)]
const LATEST: Versions = Versions {
    find: 6,
    join: 9,
    sync: 5,
    beat: 4,
    leave: 5,
    commit: 9,
    fetch: 9,
};

#[cfg(test)]
struct Member {
    me: u64,
    v: Versions,
    shared: Rc<RefCell<Shared>>,
    rng: Rng,
    brokers: BTreeMap<i32, NodeId>,
    phase: Phase,
    coordinator: Option<NodeId>,
    member_id: String,
    generation: i32,
    leader: String,
    members: Vec<String>,
    owned: Vec<i32>,
    conn: Option<NodeId>,
    open: bool,
    closing: bool,
    buf: Vec<u8>,
    corr: i32,
    /// The request in flight: what it is, when it was sent, when it is given up, and a commit's offsets.
    pending: Option<InFlight>,
    next_beat: i64,
    next_commit: i64,
    next_fetch: i64,
    start: i64,
    leave_at: Option<i64>,
    die_at: Option<i64>,
    /// An observer only fetches the group's committed offsets, often, wherever the coordinator is (as an admin tool
    /// does): it reads at a coordinator that has just taken over.
    observer: bool,
}

#[cfg(test)]
impl Member {
    fn new(me: u64, seed: u64, shared: Rc<RefCell<Shared>>, brokers: BTreeMap<i32, NodeId>, start: i64) -> Member {
        Member {
            me,
            v: if me == 0 { LEGACY } else { LATEST },
            shared,
            rng: Rng(seed * 104_729 + me),
            brokers,
            phase: Phase::Create,
            coordinator: None,
            member_id: String::new(),
            generation: -1,
            leader: String::new(),
            members: Vec::new(),
            owned: Vec::new(),
            conn: None,
            open: false,
            closing: false,
            buf: Vec::new(),
            corr: 0,
            pending: None,
            next_beat: 0,
            next_commit: 0,
            next_fetch: 0,
            start,
            leave_at: None,
            die_at: None,
            observer: false,
        }
    }

    fn close(&mut self, a: &mut StreamAction) {
        if self.conn.is_some() && !self.closing {
            a.close = true;
            self.closing = true;
        }
    }

    /// Look for the coordinator again (it moved, or failed).
    fn refind(&mut self, a: &mut StreamAction) {
        self.coordinator = None;
        self.phase = Phase::Find;
        self.close(a);
    }

    fn rejoin(&mut self) {
        self.phase = Phase::Join;
        self.owned.clear();
        self.shared.borrow_mut().steady.insert(self.me, None);
    }

    fn random_broker(&mut self) -> NodeId {
        let ids: Vec<NodeId> = self.brokers.values().copied().collect();
        ids[self.rng.below(ids.len() as u64) as usize]
    }

    fn step(&mut self, now: i64, a: &mut StreamAction) {
        if self.phase == Phase::Gone || self.closing || self.pending.is_some() || now < self.start {
            return;
        }
        if let Some(t) = self.die_at
            && now >= t
        {
            // Falls silent: no LeaveGroup, no more heartbeats; the coordinator must notice by the session timeout.
            self.phase = Phase::Gone;
            self.shared.borrow_mut().steady.remove(&self.me);
            self.close(a);
            return;
        }
        let target = match self.phase {
            Phase::Create | Phase::Find => match self.conn {
                Some(c) => c,
                None => self.random_broker(),
            },
            _ => match self.coordinator {
                Some(c) => c,
                None => {
                    self.phase = Phase::Find;
                    return;
                }
            },
        };
        match self.conn {
            None => {
                self.conn = Some(target);
                self.open = false;
                a.connect = Some((target, Arc::from("kafka")));
                return;
            }
            Some(_) if !self.open => return,
            Some(c) if c != target => {
                self.close(a);
                return;
            }
            Some(_) => {}
        }
        if let Some(t) = self.leave_at
            && now >= t
            && matches!(self.phase, Phase::Steady | Phase::Sync)
        {
            self.send_leave(now, a);
            return;
        }
        match self.phase {
            Phase::Create => {
                if self.shared.borrow().created {
                    self.phase = Phase::Find;
                    return self.step(now, a);
                }
                let req = CreateTopicsRequest::default().with_timeout_ms(3000).with_topics(vec![
                    CreatableTopic::default()
                        .with_name(TopicName(sb(TOPIC)))
                        .with_num_partitions(PARTITIONS)
                        .with_replication_factor(3),
                ]);
                self.send(
                    now,
                    a,
                    Req::Create,
                    framed(19, 7, self.corr + 1, &req),
                    REQUEST_TIMEOUT,
                    Vec::new(),
                );
            }
            Phase::Find => {
                let mut req = FindCoordinatorRequest::default().with_key_type(0);
                req = if self.v.find >= 4 {
                    req.with_coordinator_keys(vec![sb(GROUP)])
                } else {
                    req.with_key(sb(GROUP))
                };
                self.send(
                    now,
                    a,
                    Req::Find,
                    framed(10, self.v.find, self.corr + 1, &req),
                    REQUEST_TIMEOUT,
                    Vec::new(),
                );
            }
            Phase::Join => {
                let req = JoinGroupRequest::default()
                    .with_group_id(GroupId(sb(GROUP)))
                    .with_session_timeout_ms(SESSION_MS)
                    .with_rebalance_timeout_ms(REBALANCE_MS)
                    .with_member_id(sb(&self.member_id))
                    .with_protocol_type(sb("consumer"))
                    .with_protocols(vec![
                        JoinGroupRequestProtocol::default()
                            .with_name(sb("range"))
                            .with_metadata(Bytes::from_static(b"subscription")),
                    ]);
                self.send(
                    now,
                    a,
                    Req::Join,
                    framed(11, self.v.join, self.corr + 1, &req),
                    JOIN_TIMEOUT,
                    Vec::new(),
                );
            }
            Phase::Sync => {
                let mut req = SyncGroupRequest::default()
                    .with_group_id(GroupId(sb(GROUP)))
                    .with_generation_id(self.generation)
                    .with_member_id(sb(&self.member_id));
                if self.v.sync >= 5 {
                    req = req
                        .with_protocol_type(Some(sb("consumer")))
                        .with_protocol_name(Some(sb("range")));
                }
                if self.leader == self.member_id {
                    // Round-robin over the members, in id order.
                    let mut ids = self.members.clone();
                    ids.sort();
                    let mut plan: BTreeMap<String, Vec<i32>> = ids.iter().map(|m| (m.clone(), Vec::new())).collect();
                    for p in 0..PARTITIONS {
                        let m = &ids[p as usize % ids.len()];
                        plan.get_mut(m).unwrap().push(p);
                    }
                    req = req.with_assignments(
                        plan.iter()
                            .map(|(m, ps)| {
                                SyncGroupRequestAssignment::default()
                                    .with_member_id(sb(m))
                                    .with_assignment(assignment_bytes(ps))
                            })
                            .collect(),
                    );
                    self.shared
                        .borrow_mut()
                        .sent
                        .insert((self.leader.clone(), self.generation), plan);
                }
                self.send(
                    now,
                    a,
                    Req::Sync,
                    framed(14, self.v.sync, self.corr + 1, &req),
                    JOIN_TIMEOUT,
                    Vec::new(),
                );
            }
            Phase::Steady => {
                if now >= self.next_commit && !self.owned.is_empty() {
                    // Offsets that only grow: the time in milliseconds, so a later commit always carries more.
                    let offset = now / MS;
                    let parts: Vec<(i32, i64)> = self.owned.iter().map(|p| (*p, offset)).collect();
                    let req = OffsetCommitRequest::default()
                        .with_group_id(GroupId(sb(GROUP)))
                        .with_generation_id_or_member_epoch(self.generation)
                        .with_member_id(sb(&self.member_id))
                        .with_topics(vec![
                            OffsetCommitRequestTopic::default()
                                .with_name(TopicName(sb(TOPIC)))
                                .with_partitions(
                                    parts
                                        .iter()
                                        .map(|(p, o)| {
                                            OffsetCommitRequestPartition::default()
                                                .with_partition_index(*p)
                                                .with_committed_offset(*o)
                                        })
                                        .collect(),
                                ),
                        ]);
                    {
                        let mut sh = self.shared.borrow_mut();
                        for (p, o) in &parts {
                            sh.tried.entry(*p).or_default().insert(*o);
                        }
                    }
                    self.next_commit = now + 150 * MS + self.rng.below(100) as i64 * MS;
                    self.send(
                        now,
                        a,
                        Req::Commit,
                        framed(8, self.v.commit, self.corr + 1, &req),
                        REQUEST_TIMEOUT,
                        parts,
                    );
                } else if now >= self.next_fetch {
                    let all: Vec<i32> = (0..PARTITIONS).collect();
                    let req = if self.v.fetch >= 8 {
                        OffsetFetchRequest::default().with_groups(vec![
                            OffsetFetchRequestGroup::default()
                                .with_group_id(GroupId(sb(GROUP)))
                                .with_topics(Some(vec![
                                    OffsetFetchRequestTopics::default()
                                        .with_name(TopicName(sb(TOPIC)))
                                        .with_partition_indexes(all),
                                ])),
                        ])
                    } else {
                        OffsetFetchRequest::default()
                            .with_group_id(GroupId(sb(GROUP)))
                            .with_topics(Some(vec![
                                OffsetFetchRequestTopic::default()
                                    .with_name(TopicName(sb(TOPIC)))
                                    .with_partition_indexes(all),
                            ]))
                    };
                    self.next_fetch = now
                        + if self.observer {
                            50 * MS
                        } else {
                            400 * MS + self.rng.below(300) as i64 * MS
                        };
                    self.send(
                        now,
                        a,
                        Req::Fetch,
                        framed(9, self.v.fetch, self.corr + 1, &req),
                        REQUEST_TIMEOUT,
                        Vec::new(),
                    );
                } else if now >= self.next_beat && !self.observer {
                    let req = HeartbeatRequest::default()
                        .with_group_id(GroupId(sb(GROUP)))
                        .with_generation_id(self.generation)
                        .with_member_id(sb(&self.member_id));
                    self.next_beat = now + 300 * MS;
                    self.send(
                        now,
                        a,
                        Req::Beat,
                        framed(12, self.v.beat, self.corr + 1, &req),
                        REQUEST_TIMEOUT,
                        Vec::new(),
                    );
                }
            }
            Phase::Gone => {}
        }
    }

    fn send_leave(&mut self, now: i64, a: &mut StreamAction) {
        let mut req = LeaveGroupRequest::default().with_group_id(GroupId(sb(GROUP)));
        req = if self.v.leave >= 3 {
            req.with_members(vec![MemberIdentity::default().with_member_id(sb(&self.member_id))])
        } else {
            req.with_member_id(sb(&self.member_id))
        };
        self.leave_at = None;
        self.send(
            now,
            a,
            Req::Leave,
            framed(13, self.v.leave, self.corr + 1, &req),
            REQUEST_TIMEOUT,
            Vec::new(),
        );
    }

    fn send(&mut self, now: i64, a: &mut StreamAction, r: Req, frame: Vec<u8>, timeout: i64, parts: Vec<(i32, i64)>) {
        self.corr += 1;
        a.send = frame;
        self.pending = Some((r, now, now + timeout, parts));
    }

    fn answered(
        &mut self,
        now: i64,
        r: Req,
        sent: i64,
        parts: Vec<(i32, i64)>,
        mut body: Bytes,
        a: &mut StreamAction,
    ) -> Result<(), String> {
        macro_rules! decode {
            ($t:ty, $v:expr) => {{
                ResponseHeader::decode(&mut body, <$t>::header_version($v)).map_err(|x| x.to_string())?;
                let m = <$t>::decode(&mut body, $v).map_err(|x| x.to_string())?;
                if !body.is_empty() {
                    return Err(format!("trailing bytes after a {} answer", stringify!($t)));
                }
                m
            }};
        }
        let coordinator_moved = |e: i16| {
            matches!(
                e,
                NOT_COORDINATOR | COORDINATOR_NOT_AVAILABLE | COORDINATOR_LOAD_IN_PROGRESS
            )
        };
        match r {
            Req::Create => {
                let m = decode!(CreateTopicsResponse, 7);
                let e = m.topics.first().map_or(-1, |t| t.error_code);
                if e == NONE || e == TOPIC_ALREADY_EXISTS {
                    self.shared.borrow_mut().created = true;
                    self.phase = Phase::Find;
                }
            }
            Req::Find => {
                let m = decode!(FindCoordinatorResponse, self.v.find);
                let (e, node) = if self.v.find >= 4 {
                    m.coordinators.first().map_or((-1, -1), |c| (c.error_code, c.node_id.0))
                } else {
                    (m.error_code, m.node_id.0)
                };
                if e == NONE {
                    let Some(n) = self.brokers.get(&node).copied() else {
                        return Err(format!("FindCoordinator named broker {node}, which does not exist"));
                    };
                    self.coordinator = Some(n);
                    self.phase = if self.observer { Phase::Steady } else { Phase::Join };
                    if self.conn != Some(n) {
                        self.close(a);
                    }
                } else if e != COORDINATOR_NOT_AVAILABLE {
                    return Err(format!("FindCoordinator answered {e}"));
                }
            }
            Req::Join => {
                let m = decode!(JoinGroupResponse, self.v.join);
                match m.error_code {
                    NONE => {
                        self.generation = m.generation_id;
                        self.leader = m.leader.to_string();
                        self.member_id = m.member_id.to_string();
                        self.members = m.members.iter().map(|x| x.member_id.to_string()).collect();
                        if self.leader == self.member_id && self.members.is_empty() {
                            return Err("the leader's JoinGroup answer lists no members".into());
                        }
                        if self.leader != self.member_id && !self.members.is_empty() {
                            return Err("a follower's JoinGroup answer lists the members".into());
                        }
                        let mut sh = self.shared.borrow_mut();
                        sh.joins += 1;
                        sh.generations.insert((self.leader.clone(), self.generation));
                        drop(sh);
                        self.phase = Phase::Sync;
                    }
                    MEMBER_ID_REQUIRED => {
                        if self.v.join < 4 {
                            return Err("MEMBER_ID_REQUIRED to a JoinGroup before v4".into());
                        }
                        self.member_id = m.member_id.to_string();
                    }
                    UNKNOWN_MEMBER_ID => {
                        self.member_id.clear();
                        self.generation = -1;
                    }
                    REBALANCE_IN_PROGRESS => {}
                    e if coordinator_moved(e) => self.refind(a),
                    e => return Err(format!("JoinGroup answered {e}")),
                }
            }
            Req::Sync => {
                let m = decode!(SyncGroupResponse, self.v.sync);
                match m.error_code {
                    NONE => {
                        self.owned = assignment_of(&m.assignment);
                        self.shared.borrow_mut().received.push((
                            self.leader.clone(),
                            self.generation,
                            self.member_id.clone(),
                            self.owned.clone(),
                        ));
                        self.shared.borrow_mut().steady.insert(
                            self.me,
                            Some((
                                self.member_id.clone(),
                                self.leader.clone(),
                                self.generation,
                                self.owned.clone(),
                            )),
                        );
                        self.phase = Phase::Steady;
                        self.next_beat = now + 300 * MS;
                    }
                    REBALANCE_IN_PROGRESS | ILLEGAL_GENERATION => self.rejoin(),
                    UNKNOWN_MEMBER_ID => {
                        self.member_id.clear();
                        self.rejoin();
                    }
                    e if coordinator_moved(e) => self.refind(a),
                    e => return Err(format!("SyncGroup answered {e}")),
                }
            }
            Req::Beat => {
                let m = decode!(HeartbeatResponse, self.v.beat);
                match m.error_code {
                    NONE => {}
                    REBALANCE_IN_PROGRESS | ILLEGAL_GENERATION => self.rejoin(),
                    UNKNOWN_MEMBER_ID => {
                        self.member_id.clear();
                        self.rejoin();
                    }
                    e if coordinator_moved(e) => self.refind(a),
                    e => return Err(format!("Heartbeat answered {e}")),
                }
            }
            Req::Commit => {
                let m = decode!(OffsetCommitResponse, self.v.commit);
                let errors: Vec<(i32, i16)> = m
                    .topics
                    .iter()
                    .flat_map(|t| t.partitions.iter().map(|p| (p.partition_index, p.error_code)))
                    .collect();
                if errors.len() != parts.len() {
                    return Err(format!(
                        "an OffsetCommit answer for {} partitions of {}",
                        errors.len(),
                        parts.len()
                    ));
                }
                let mut sh = self.shared.borrow_mut();
                sh.commits += 1;
                let mut worst = NONE;
                for ((p, e), (q, o)) in errors.iter().zip(&parts) {
                    if p != q {
                        return Err(format!("an OffsetCommit answer for partition {p}, asked {q}"));
                    }
                    if *e == NONE {
                        sh.acked.entry(*p).or_default().push((sent, now, *o));
                    } else {
                        worst = *e;
                    }
                }
                drop(sh);
                match worst {
                    // Read what was just written: the next request is a fetch (the window between a commit's
                    // acknowledgement and its materialization).
                    NONE => self.next_fetch = now,
                    REBALANCE_IN_PROGRESS | ILLEGAL_GENERATION => self.rejoin(),
                    UNKNOWN_MEMBER_ID => {
                        self.member_id.clear();
                        self.rejoin();
                    }
                    REQUEST_TIMED_OUT => {}
                    e if coordinator_moved(e) => self.refind(a),
                    e => return Err(format!("OffsetCommit answered {e}")),
                }
            }
            Req::Fetch => {
                let m = decode!(OffsetFetchResponse, self.v.fetch);
                let (e, parts): (i16, Vec<(i32, i64, i16)>) = if self.v.fetch >= 8 {
                    let g = m.groups.first().ok_or("an OffsetFetch answer with no group")?;
                    (
                        g.error_code,
                        g.topics
                            .iter()
                            .flat_map(|t| {
                                t.partitions
                                    .iter()
                                    .map(|p| (p.partition_index, p.committed_offset, p.error_code))
                            })
                            .collect(),
                    )
                } else {
                    (
                        m.error_code,
                        m.topics
                            .iter()
                            .flat_map(|t| {
                                t.partitions
                                    .iter()
                                    .map(|p| (p.partition_index, p.committed_offset, p.error_code))
                            })
                            .collect(),
                    )
                };
                if coordinator_moved(e) {
                    self.refind(a);
                    return Ok(());
                }
                if e != NONE {
                    return Err(format!("OffsetFetch answered {e}"));
                }
                let mut sh = self.shared.borrow_mut();
                sh.fetches += 1;
                for (p, o, pe) in parts {
                    if pe != NONE {
                        return Err(format!("OffsetFetch answered {pe} for partition {p}"));
                    }
                    // Every commit acknowledged before this fetch was sent must show (offsets only grow).
                    let floor = sh
                        .acked
                        .get(&p)
                        .into_iter()
                        .flatten()
                        .filter(|(_, answered, _)| *answered < sent)
                        .map(|(_, _, off)| *off)
                        .max();
                    if let Some(f) = floor
                        && o < f
                    {
                        return Err(format!(
                            "partition {p}: OffsetFetch sent at {sent} returned {o}, but {f} was acknowledged before"
                        ));
                    }
                    if o >= 0 && !sh.tried.get(&p).is_some_and(|t| t.contains(&o)) {
                        return Err(format!(
                            "partition {p}: OffsetFetch returned {o}, which no commit carried"
                        ));
                    }
                }
            }
            Req::Leave => {
                let m = decode!(LeaveGroupResponse, self.v.leave);
                let e = if self.v.leave >= 3 {
                    m.members.first().map_or(m.error_code, |x| x.error_code)
                } else {
                    m.error_code
                };
                if coordinator_moved(m.error_code) {
                    // Try again at the coordinator.
                    self.leave_at = Some(now);
                    self.refind(a);
                    return Ok(());
                }
                if e != NONE && e != UNKNOWN_MEMBER_ID {
                    return Err(format!("LeaveGroup answered {e}"));
                }
                self.phase = Phase::Gone;
                self.shared.borrow_mut().steady.remove(&self.me);
                self.close(a);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
impl StreamClient for Member {
    fn on(&mut self, now: i64, e: StreamEvent<'_>) -> Result<StreamAction, String> {
        let mut a = StreamAction::default();
        match e {
            StreamEvent::Wake => {
                if let Some((_, _, deadline, _)) = self.pending
                    && now >= deadline
                {
                    self.pending = None;
                    self.refind(&mut a);
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
                let Some((r, sent, _, parts)) = self.pending.take() else {
                    return Err("a response with no request in flight".into());
                };
                self.answered(now, r, sent, parts, body, &mut a)
                    .map_err(|x| format!("member {}: {x}", self.me))?;
                if !a.close {
                    self.step(now, &mut a);
                }
            }
            StreamEvent::Closed(_) => {
                if self.pending.is_some() || !self.closing {
                    // A lost connection: the coordinator may have moved (a parked JoinGroup or SyncGroup is lost too).
                    if self.phase != Phase::Gone && self.phase != Phase::Create {
                        if self.phase == Phase::Steady || self.phase == Phase::Sync {
                            self.shared.borrow_mut().failovers += 1;
                        }
                        self.coordinator = None;
                        if self.phase != Phase::Find {
                            self.phase = Phase::Find;
                        }
                    }
                }
                self.pending = None;
                self.conn = None;
                self.open = false;
                self.closing = false;
                self.buf.clear();
            }
        }
        if a.wake.is_none() && self.phase != Phase::Gone {
            a.wake = Some(now + 50 * MS);
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
        other => panic!("not an integer: {other:?}"),
    }
}

/// Runs the group scenario on three brokers over `seeds`, with faults until 9 s, then settles; returns how many
/// times a member lost its coordinator.
#[cfg(test)]
fn check_groups(seeds: std::ops::RangeInclusive<u64>, faults: bool) -> u64 {
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
    let text = |s: &str| blossom_front::api::ParamBinding::Text(s.into());
    let params = [
        ("REPLICA_LAG_MAX".to_owned(), text("500ms")),
        (
            "OFFSETS_PARTITIONS".to_owned(),
            blossom_front::api::ParamBinding::Int(4),
        ),
        (
            "GROUP_MIN_SESSION_MS".to_owned(),
            blossom_front::api::ParamBinding::Int(500),
        ),
        (
            "GROUP_INITIAL_DELAY_MS".to_owned(),
            blossom_front::api::ParamBinding::Int(300),
        ),
    ]
    .into_iter()
    .collect();
    let (result, _) = blossom_driver::bls::compile_file_with(path.to_str().unwrap(), &nodes, &params);
    let artifact: BlsArtifact = result.unwrap_or_else(|e| panic!("sim_cluster.bls: {e:?}")).0;
    let schema = DurableSchema::of(artifact.program.get());
    let rel = |n: &str| artifact.rel_named(n).unwrap();
    let brokers: BTreeMap<i32, NodeId> = (1..=3).map(|i| (i as i32, NodeId(i - 1))).collect();
    let (mut joins, mut gens, mut commits, mut fetches, mut failovers) = (0u64, 0u64, 0u64, 0u64, 0u64);
    for seed in seeds {
        let cfg = ClusterConfig {
            seed,
            clients: 0,
            nemesis: if faults { 400_000_000 } else { 0 },
            crashes: faults,
            partitions: faults,
            stream_drops: faults,
            downtime: 400_000_000,
            duration: 20_000_000_000,
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
        let (_safety, observer) = GroupSafety::of(&artifact).unwrap().shared();
        cluster.observe(observer);
        let shared = Rc::new(RefCell::new(Shared::default()));
        // Members join over the first seconds; member 1 leaves at 7 s, member 2 dies at 8 s.
        for (me, start) in [(0u64, 300), (1, 800), (2, 2500), (3, 4000), (4, 1000)] {
            let mut m = Member::new(me, seed, shared.clone(), brokers.clone(), start * MS);
            m.observer = me == 4;
            if me == 1 {
                m.leave_at = Some(7000 * MS);
            }
            if me == 2 {
                m.die_at = Some(8000 * MS);
            }
            cluster.stream_client(Box::new(m));
        }
        let fail = |cluster: &Cluster<'_>, what: &str| -> String {
            format!("seed {seed}: {what}\n{}", cluster.run_so_far().log.join("\n"))
        };
        cluster.run_until(9_000_000_000).unwrap();
        assert!(
            cluster.violation().is_none(),
            "{}",
            fail(&cluster, cluster.violation().unwrap_or(""))
        );
        cluster.heal();
        cluster.step_until(20_000_000_000).unwrap();
        assert!(
            cluster.violation().is_none(),
            "{}",
            fail(&cluster, cluster.violation().unwrap_or(""))
        );

        let sh = shared.borrow();
        // Every SyncGroup answer is what the generation's leader sent for that member.
        for (leader, generation, member, got) in &sh.received {
            let want = sh
                .sent
                .get(&(leader.clone(), *generation))
                .and_then(|plan| plan.get(member))
                .unwrap_or_else(|| {
                    panic!(
                        "{}",
                        fail(
                            &cluster,
                            &format!("{member} synced generation {generation} of {leader}, which sent it nothing")
                        )
                    )
                });
            assert_eq!(
                got,
                want,
                "{}",
                fail(&cluster, "a member's assignment is not its leader's")
            );
        }
        // The live members (0 and 3) settled into one generation covering every partition once.
        let live: Vec<&Settled> = [0u64, 3]
            .iter()
            .map(|m| {
                sh.steady
                    .get(m)
                    .and_then(|s| s.as_ref())
                    .unwrap_or_else(|| panic!("{}", fail(&cluster, &format!("member {m} is not steady at the end"))))
            })
            .collect();
        assert!(
            live.iter()
                .all(|s| (s.1.clone(), s.2) == (live[0].1.clone(), live[0].2)),
            "{}",
            fail(
                &cluster,
                &format!("the live members are in different generations: {live:?}")
            )
        );
        let mut covered: Vec<i32> = live.iter().flat_map(|s| s.3.clone()).collect();
        covered.sort();
        assert_eq!(
            covered,
            (0..PARTITIONS).collect::<Vec<_>>(),
            "{}",
            fail(&cluster, "the final assignment")
        );
        assert!(
            [1u64, 2].iter().all(|m| !sh.steady.contains_key(m)),
            "{}",
            fail(&cluster, "a member that left or died is still steady")
        );
        assert!(!sh.acked.is_empty(), "{}", fail(&cluster, "no commit was acknowledged"));

        // Every replica of each offsets partition holds the same committed offsets, which include the last
        // acknowledged commit of each partition of the topic.
        let states: Vec<_> = brokers
            .values()
            .map(|n| {
                cluster
                    .state(*n)
                    .unwrap_or_else(|| panic!("{}", fail(&cluster, &format!("broker {n:?} is down"))))
            })
            .collect();
        let offsets_tid = states[0]
            .rows(rel("mtopic"))
            .find(|r| r[0] == Value::Str("__consumer_offsets".into()))
            .map(|r| r[1].clone())
            .unwrap_or_else(|| panic!("{}", fail(&cluster, "__consumer_offsets does not exist")));
        let mut by_part: BTreeMap<i64, BTreeSet<Vec<Value>>> = BTreeMap::new();
        let mut agreed: BTreeMap<i64, Vec<i32>> = BTreeMap::new();
        for (bi, s) in states.iter().enumerate() {
            let replicas: BTreeMap<i64, Vec<i32>> = s
                .rows(rel("massign"))
                .filter(|r| r[0] == offsets_tid)
                .map(|r| {
                    let Value::Vec(rs) = &r[2] else { panic!() };
                    (int(&r[1]), rs.iter().map(|x| int(x) as i32).collect())
                })
                .collect();
            for (p, rs) in &replicas {
                if !rs.contains(&(bi as i32 + 1)) {
                    continue;
                }
                let rows: BTreeSet<Vec<Value>> = s
                    .rows(rel("committed"))
                    .filter(|r| r[0] == offsets_tid && int(&r[1]) == *p)
                    .map(|r| r[2..].to_vec())
                    .collect();
                match by_part.get(p) {
                    Some(prev) => assert_eq!(
                        prev,
                        &rows,
                        "{}",
                        fail(&cluster, &format!("offsets partition {p}'s replicas differ"))
                    ),
                    None => {
                        by_part.insert(*p, rows);
                    }
                }
                agreed.insert(*p, rs.clone());
            }
        }
        let committed: BTreeMap<i64, i64> = by_part
            .values()
            .flatten()
            .filter(|r| r[0] == Value::Str(GROUP.into()) && r[1] == Value::Str(TOPIC.into()))
            .map(|r| (int(&r[2]), int(&r[4])))
            .collect();
        for (p, acks) in &sh.acked {
            let last = acks.iter().map(|a| a.2).max().unwrap();
            let got = committed.get(&i64::from(*p)).copied().unwrap_or(-1);
            assert!(
                got >= last,
                "{}",
                fail(
                    &cluster,
                    &format!("partition {p}: committed {got} after {last} was acknowledged")
                )
            );
        }
        // Members came, left and died: the group went through generations for each.
        assert!(
            sh.generations.len() >= 4,
            "{}",
            fail(&cluster, &format!("only {} generations", sh.generations.len()))
        );
        joins += sh.joins;
        gens += sh.generations.len() as u64;
        failovers += sh.failovers;
        commits += sh.commits;
        fetches += sh.fetches;
    }
    assert!(
        joins >= gens && commits > 0 && fetches > 0,
        "joins {joins}, generations {gens}, commits {commits}, fetches {fetches}"
    );
    failovers
}

#[test]
fn group_members_join_leave_and_die_and_rebalance() {
    check_groups(seeds(1..=2), false);
}

/// Under faults the members lose their coordinator (a crash, a split, a dropped connection) and find it again; over
/// the full tier's seeds that happens.
#[test]
fn groups_keep_acknowledged_offsets_under_faults() {
    let lost = check_groups(seeds(1..=3), true);
    assert!(
        !blossom_integration_tests::full_tier() || lost > 0,
        "no member ever lost its coordinator"
    );
}
