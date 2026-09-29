//! A deterministic cluster simulator over real nodes: every server is a sans-IO [`Node`] with its store on a
//! [`SimFs`], driven on a virtual clock, connected by a simulated network that delays, drops and partitions
//! messages, and crashed and restarted by a nemesis. Clients run a key-value workload through a
//! [`ClientProtocol`] and record a history for the linearizability checker ([`crate::linearize`]).
//!
//! This is the network runtime's semantics in simulation: admission by ACL, durable-before-release (each tick's WAL
//! record is synced before its sends leave), recovery from the store after a crash (unsynced writes lost or torn),
//! incarnation-unique session ids, and replies to closed sessions dropped. Everything is a function of the seed, so a
//! failing run replays exactly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{RelId, internal_error};
use blossom_node::acl::{AclTable, Source};
use blossom_node::durable::DurableSchema;
use blossom_node::manual::ManualDriver;
use blossom_node::recovery::{self, StoreSpec};
use blossom_node::{Node, NodeConfig, ReleasedTick};
use blossom_oracle::{Delivery, Ingress, Oracle, Row};
use blossom_store::{OpenMode, SimFs, StoreIdentity, Vfs, WriteFate};
use blossom_value::Value;
use blossom_value::time::{Instant, NodeId};
use blossom_value::value::SessionId;

use crate::linearize::{KvInput, KvOutput, Operation};
use crate::sync::SimError;

/// How a key-value workload speaks a program's client protocol.
pub trait ClientProtocol {
    /// The channel and columns (after the destination) of request `id` for `op`.
    fn request(&self, op: &KvInput, id: u64) -> Result<(RelId, Vec<Value>), String>;
    /// Reads a reply: its request id and what it says, or `None` if the row is not a reply this protocol knows.
    fn reply(&self, rel: RelId, row: &Row) -> Option<(u64, Reply)>;
}

/// What a reply says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Done(KvOutput),
    /// Not the leader: try `leader` (or another server when unknown). The request had no effect.
    Redirect(Option<NodeId>),
}

/// The simulation's knobs.
#[derive(Clone, Debug)]
pub struct ClusterConfig {
    pub seed: u64,
    /// One-way message latency, uniformly in `[min, max]` nanoseconds.
    pub latency: (i64, i64),
    /// Per-message loss probability, in parts per million.
    pub loss_ppm: u32,
    pub clients: usize,
    pub keys: usize,
    /// Weights of put, get and delete.
    pub mix: (u32, u32, u32),
    /// Pause between a client's operations, uniformly in `[0, think]` nanoseconds.
    pub think: i64,
    /// A client gives up on an operation after this long: it is recorded unanswered.
    pub timeout: i64,
    /// The nemesis acts every `[nemesis/2, nemesis]` nanoseconds (0: never).
    pub nemesis: i64,
    /// Whether the nemesis may crash and restart nodes, and partition the network.
    pub crashes: bool,
    pub partitions: bool,
    /// How long the run lasts, in virtual nanoseconds.
    pub duration: i64,
    /// The principal clients claim.
    pub principal: String,
}

impl Default for ClusterConfig {
    fn default() -> ClusterConfig {
        ClusterConfig {
            seed: 1,
            latency: (200_000, 2_000_000),
            loss_ppm: 0,
            clients: 4,
            keys: 4,
            mix: (45, 40, 15),
            think: 5_000_000,
            timeout: 500_000_000,
            nemesis: 0,
            crashes: false,
            partitions: false,
            duration: 5_000_000_000,
            principal: "spiffe://sim/client".into(),
        }
    }
}

/// What a run produced.
#[derive(Debug, Default)]
pub struct ClusterRun {
    pub history: Vec<Operation<KvInput, KvOutput>>,
    pub crashes: u64,
    pub partitions: u64,
    pub messages: u64,
    pub dropped: u64,
    pub ticks: u64,
    /// The nemesis's actions, for a failure report.
    pub log: Vec<String>,
}

/// SplitMix64: the simulation's only randomness.
struct Rng(u64);

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
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        let span = u64::try_from(hi.saturating_sub(lo)).unwrap_or(0);
        lo.saturating_add(i64::try_from(self.below(span.saturating_add(1))).unwrap_or(0))
    }
    fn chance_ppm(&mut self, ppm: u32) -> bool {
        self.below(1_000_000) < u64::from(ppm)
    }
}

enum Envelope {
    Peer { from: NodeId, to: NodeId, rel: RelId, row: Row },
    FromClient { client: usize, to: NodeId, rel: RelId, row: Row },
    ToClient { client: usize, rel: RelId, row: Row },
}

struct Pending {
    op: KvInput,
    call: i64,
    req: u64,
    /// When the client gives up.
    deadline: i64,
}

struct Client {
    /// The node it currently talks to.
    target: NodeId,
    /// Its session on each node, by node incarnation.
    sessions: BTreeMap<NodeId, (u64, SessionId)>,
    pending: Option<Pending>,
    next_req: u64,
    /// When it next acts (a new operation, or a retry after a redirect).
    wake: i64,
    retry: Option<NodeId>,
}

struct SimNode<'p> {
    fs: SimFs,
    driver: Option<ManualDriver<'p, Arc<Oracle>>>,
    restarts: u64,
    next_session: u64,
    /// Open sessions: which client each is.
    sessions: BTreeMap<SessionId, usize>,
}

/// A simulated cluster of one program's nodes.
pub struct Cluster<'p> {
    artifact: &'p BlsArtifact,
    schema: &'p DurableSchema,
    oracle: Arc<Oracle>,
    acl: AclTable,
    names: Arc<[Arc<str>]>,
    statics: Vec<(RelId, Row)>,
    nodes: Vec<SimNode<'p>>,
    clients: Vec<Client>,
    net: BTreeMap<(i64, u64), Envelope>,
    seq: u64,
    blocked: BTreeSet<(NodeId, NodeId)>,
    now: i64,
    rng: Rng,
    cfg: ClusterConfig,
    run: ClusterRun,
    protocol: Box<dyn ClientProtocol + 'p>,
}

const EPOCH: i64 = 1_000_000_000_000_000_000;

fn node_id(i: usize) -> Result<NodeId, SimError> {
    u32::try_from(i)
        .map(NodeId)
        .map_err(|_| internal_error!("too many nodes").into())
}

fn identity(names: &[Arc<str>], n: NodeId) -> StoreIdentity {
    StoreIdentity {
        store_uuid: [0; 16],
        deployment_id: [7; 16],
        program_id: [0; 16],
        node_name: names.get(n.0 as usize).cloned().unwrap_or_else(|| Arc::from("?")),
        principal: format!("spiffe://sim/node/{}", n.0).into(),
        format: recovery::FORMAT,
        directory_digest: [0; 16],
    }
}

impl<'p> Cluster<'p> {
    /// A cluster of every node of `artifact`, each with a fresh store. `seed` seeds the program (SEM-084);
    /// `statics` are the deployment's static rows.
    pub fn new(
        artifact: &'p BlsArtifact,
        schema: &'p DurableSchema,
        program_seed: blossom_value::Seed,
        statics: Vec<(RelId, Row)>,
        protocol: Box<dyn ClientProtocol + 'p>,
        cfg: ClusterConfig,
    ) -> Result<Cluster<'p>, SimError> {
        let oracle = Arc::new(
            Oracle::new(artifact.program.clone())
                .map_err(SimError::Load)?
                .with_roles(artifact.roles.clone())
                .with_seed(program_seed)
                .map_err(SimError::Load)?,
        );
        let names: Arc<[Arc<str>]> = artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect();
        let mut c = Cluster {
            artifact,
            schema,
            acl: AclTable::of(artifact.program.get()),
            oracle,
            names,
            statics,
            nodes: Vec::new(),
            clients: Vec::new(),
            net: BTreeMap::new(),
            seq: 0,
            blocked: BTreeSet::new(),
            now: EPOCH,
            rng: Rng(cfg.seed),
            run: ClusterRun::default(),
            protocol,
            cfg,
        };
        for i in 0..artifact.nodes.len() {
            c.nodes.push(SimNode {
                fs: SimFs::default(),
                driver: None,
                restarts: 0,
                next_session: 0,
                sessions: BTreeMap::new(),
            });
            c.boot(node_id(i)?, true)?;
        }
        for i in 0..c.cfg.clients {
            let target = node_id(i % c.nodes.len().max(1))?;
            let wake = c.now + c.rng.range(0, c.cfg.think);
            c.clients.push(Client {
                target,
                sessions: BTreeMap::new(),
                pending: None,
                next_req: 0,
                wake,
                retry: None,
            });
        }
        Ok(c)
    }

    fn boot(&mut self, n: NodeId, fresh: bool) -> Result<(), SimError> {
        let names = self.names.clone();
        let now = self.now;
        let artifact = self.artifact;
        let oracle = self.oracle.clone();
        let statics = self.statics.clone();
        let schema = self.schema;
        let slot = self
            .nodes
            .get_mut(n.0 as usize)
            .ok_or_else(|| internal_error!("no node {}", n.0))?;
        let fs: Arc<dyn Vfs> = Arc::new(slot.fs.clone());
        let opened = recovery::open(
            fs,
            &StoreSpec {
                dir: PathBuf::from(format!("/node{}", n.0)),
                identity: identity(&names, n),
                mode: if fresh { OpenMode::InitFresh } else { OpenMode::Existing },
            },
            artifact.program.get(),
            names.clone(),
            Instant(now),
            u64::from(n.0) ^ slot.restarts,
        )
        .map_err(|e| SimError::Internal(internal_error!("node {} cannot recover: {e}", n.0)))?;
        slot.restarts = opened.record.restarts;
        slot.next_session = 0;
        slot.sessions.clear();
        let mut cfg = NodeConfig::new(n, artifact.roles.get(n.0 as usize).copied().flatten());
        cfg.halt = artifact.halt;
        cfg.statics = statics;
        let node = Node::boot(cfg, &artifact.program, oracle, opened.boot.clone())
            .map_err(|e| SimError::Internal(internal_error!("node {} cannot boot: {e}", n.0)))?;
        slot.driver = Some(ManualDriver::new(node, artifact.program.get(), schema, names, opened));
        Ok(())
    }

    fn schedule(&mut self, delay: i64, e: Envelope) {
        self.seq += 1;
        self.run.messages += 1;
        self.net.insert((self.now + delay.max(1), self.seq), e);
    }

    /// Runs the workload to the end of the configured duration and returns the run.
    pub fn run(mut self) -> Result<ClusterRun, SimError> {
        let end = EPOCH + self.cfg.duration;
        let mut nemesis_at = if self.cfg.nemesis > 0 {
            self.now + self.rng.range(self.cfg.nemesis / 2, self.cfg.nemesis)
        } else {
            i64::MAX
        };
        while self.now < end {
            // Run every node that is ready now.
            for i in 0..self.nodes.len() {
                self.step_node(node_id(i)?)?;
            }
            // The next event.
            let mut next = end.min(nemesis_at);
            if let Some(((t, _), _)) = self.net.first_key_value() {
                next = next.min(*t);
            }
            for c in &self.clients {
                next = next.min(c.wake);
                if let Some(p) = &c.pending {
                    next = next.min(p.deadline);
                }
            }
            for n in &self.nodes {
                if let Some(d) = &n.driver
                    && let Some(t) = d.node.next_deadline().map_err(|e| internal_error!("{e}"))?
                {
                    next = next.min(t.0);
                }
            }
            self.now = next.max(self.now + 1);
            // Deliver what arrives by now.
            while let Some(entry) = self.net.first_entry() {
                if entry.key().0 > self.now {
                    break;
                }
                let e = entry.remove();
                self.deliver(e)?;
            }
            if self.now >= nemesis_at {
                self.nemesis()?;
                nemesis_at = self.now + self.rng.range(self.cfg.nemesis / 2, self.cfg.nemesis);
            }
            for c in 0..self.clients.len() {
                self.client_step(c)?;
            }
        }
        // Operations still in flight at the end never got an answer.
        for c in &mut self.clients {
            if let Some(p) = c.pending.take() {
                self.run.history.push(Operation {
                    call: u64::try_from(p.call - EPOCH).unwrap_or(0),
                    ret: None,
                    input: p.op,
                    output: None,
                });
            }
        }
        Ok(self.run)
    }

    fn step_node(&mut self, n: NodeId) -> Result<(), SimError> {
        let now = Instant(self.now);
        let Some(slot) = self.nodes.get_mut(n.0 as usize) else {
            return Err(internal_error!("no node {}", n.0).into());
        };
        let Some(driver) = slot.driver.as_mut() else {
            return Ok(());
        };
        let mut released: Vec<ReleasedTick> = Vec::new();
        let before = driver.node.next_tick();
        driver
            .run_until_quiescent_with(now, &mut |t| released.push(t))
            .map_err(|e| SimError::Internal(internal_error!("node {} failed: {e}", n.0)))?;
        self.run.ticks += driver.node.next_tick().0.saturating_sub(before.0);
        // Occasionally checkpoint, to exercise recovery from checkpoints.
        if self.rng.below(50) == 0 {
            driver
                .checkpoint()
                .map_err(|e| SimError::Internal(internal_error!("node {} checkpoint failed: {e}", n.0)))?;
        }
        let sessions = slot.sessions.clone();
        let mut local: Vec<Delivery> = Vec::new();
        for t in released {
            for s in t.sends {
                if s.to == n {
                    local.push(Delivery {
                        rel: s.rel,
                        from: n,
                        row: s.row,
                    });
                    continue;
                }
                if self.blocked.contains(&(n, s.to)) || self.rng.chance_ppm(self.cfg.loss_ppm) {
                    self.run.dropped += 1;
                    continue;
                }
                let delay = self.rng.range(self.cfg.latency.0, self.cfg.latency.1);
                self.schedule(
                    delay,
                    Envelope::Peer {
                        from: n,
                        to: s.to,
                        rel: s.rel,
                        row: s.row,
                    },
                );
            }
            for e in t.egress {
                let Some(&client) = sessions.get(&e.session) else {
                    self.run.dropped += 1;
                    continue;
                };
                let delay = self.rng.range(self.cfg.latency.0, self.cfg.latency.1);
                self.schedule(
                    delay,
                    Envelope::ToClient {
                        client,
                        rel: e.rel,
                        row: e.row,
                    },
                );
            }
        }
        if let Some(d) = self.nodes.get_mut(n.0 as usize).and_then(|s| s.driver.as_mut()) {
            for l in local {
                d.node.offer_delivery(l);
            }
        }
        Ok(())
    }

    fn deliver(&mut self, e: Envelope) -> Result<(), SimError> {
        match e {
            Envelope::Peer { from, to, rel, row } => {
                if self.blocked.contains(&(from, to)) {
                    self.run.dropped += 1;
                    return Ok(());
                }
                let principal = format!("spiffe://sim/node/{}", from.0);
                let role = self.artifact.roles.get(from.0 as usize).copied().flatten();
                let facts = self.oracle.static_facts();
                let Some(d) = self.nodes.get_mut(to.0 as usize).and_then(|s| s.driver.as_mut()) else {
                    self.run.dropped += 1;
                    return Ok(());
                };
                if d.node.admits(&self.acl, facts, rel, Source::Node { role, principal: &principal }) {
                    d.node.offer_delivery(Delivery { rel, from, row });
                } else {
                    self.run.dropped += 1;
                }
            }
            Envelope::FromClient { client, to, rel, row } => {
                let facts = self.oracle.static_facts();
                let principal = self.cfg.principal.clone();
                let Some(slot) = self.nodes.get_mut(to.0 as usize) else {
                    return Err(internal_error!("no node {}", to.0).into());
                };
                let Some(d) = slot.driver.as_mut() else {
                    self.run.dropped += 1;
                    return Ok(());
                };
                // The client's session on this incarnation of the node (opened by its first message).
                let c = self
                    .clients
                    .get_mut(client)
                    .ok_or_else(|| internal_error!("no client {client}"))?;
                let session = match c.sessions.get(&to) {
                    Some((inc, s)) if *inc == slot.restarts => *s,
                    _ => {
                        let s = SessionId(slot.restarts << 32 | slot.next_session);
                        slot.next_session += 1;
                        slot.sessions.insert(s, client);
                        c.sessions.insert(to, (slot.restarts, s));
                        s
                    }
                };
                if d.node.admits(&self.acl, facts, rel, Source::Session { principal: &principal }) {
                    d.node.offer_ingress(Ingress { rel, session, row });
                } else {
                    self.run.dropped += 1;
                }
            }
            Envelope::ToClient { client, rel, row } => self.client_reply(client, rel, &row)?,
        }
        Ok(())
    }

    fn client_reply(&mut self, client: usize, rel: RelId, row: &Row) -> Result<(), SimError> {
        let protocol = self.protocol()?;
        let Some((req, reply)) = protocol.reply(rel, row) else {
            return Ok(());
        };
        let now = self.now;
        let think = self.cfg.think;
        let wake = now + self.rng.range(0, think);
        let retry_wake = now + self.rng.range(1_000_000, 20_000_000);
        let nodes = self.nodes.len();
        let pick = node_id(usize::try_from(self.rng.below(nodes as u64)).unwrap_or(0))?;
        let c = self
            .clients
            .get_mut(client)
            .ok_or_else(|| internal_error!("no client {client}"))?;
        let Some(p) = &c.pending else {
            return Ok(());
        };
        if p.req != req {
            return Ok(()); // a late reply to an operation already given up on
        }
        match reply {
            Reply::Done(output) => {
                let Some(p) = c.pending.take() else {
                    return Ok(());
                };
                self.run.history.push(Operation {
                    call: u64::try_from(p.call - EPOCH).unwrap_or(0),
                    ret: Some(u64::try_from(now - EPOCH).unwrap_or(0)),
                    input: p.op,
                    output: Some(output),
                });
                c.wake = wake;
            }
            Reply::Redirect(leader) => {
                // The request had no effect: resend the same operation (same call time) elsewhere.
                c.retry = Some(leader.unwrap_or(pick));
                c.wake = retry_wake;
            }
        }
        Ok(())
    }

    fn protocol(&self) -> Result<&dyn ClientProtocol, SimError> {
        Ok(&*self.protocol)
    }

    fn client_step(&mut self, client: usize) -> Result<(), SimError> {
        let now = self.now;
        let Some(c) = self.clients.get(client) else {
            return Err(internal_error!("no client {client}").into());
        };
        // Give up on an operation that took too long: it may or may not have happened.
        if let Some(p) = &c.pending
            && now >= p.deadline
        {
            let wake = now + self.rng.range(0, self.cfg.think);
            let nodes = self.nodes.len() as u64;
            let target = node_id(usize::try_from(self.rng.below(nodes)).unwrap_or(0))?;
            let c = self.clients.get_mut(client).ok_or_else(|| internal_error!("no client"))?;
            if let Some(p) = c.pending.take() {
                self.run.history.push(Operation {
                    call: u64::try_from(p.call - EPOCH).unwrap_or(0),
                    ret: None,
                    input: p.op,
                    output: None,
                });
            }
            c.target = target;
            c.retry = None;
            c.wake = wake;
            return Ok(());
        }
        if now < c.wake {
            return Ok(());
        }
        // A redirected operation is resent; otherwise start a new one.
        let (op, req, to) = match (&c.pending, c.retry) {
            (Some(p), Some(to)) => (p.op.clone(), p.req, to),
            (Some(_), None) => return Ok(()),
            (None, _) => {
                let key = format!("k{}", self.rng.below(self.cfg.keys as u64)).into_bytes();
                let total = u64::from(self.cfg.mix.0 + self.cfg.mix.1 + self.cfg.mix.2);
                let pick = self.rng.below(total);
                let c = self.clients.get_mut(client).ok_or_else(|| internal_error!("no client"))?;
                c.next_req += 1;
                let req = c.next_req;
                let op = if pick < u64::from(self.cfg.mix.0) {
                    KvInput::Put {
                        key,
                        val: format!("c{client}-{req}").into_bytes(),
                    }
                } else if pick < u64::from(self.cfg.mix.0 + self.cfg.mix.1) {
                    KvInput::Get { key }
                } else {
                    KvInput::Delete { key }
                };
                c.pending = Some(Pending {
                    op: op.clone(),
                    call: now,
                    req,
                    deadline: now + self.cfg.timeout,
                });
                (op, req, c.target)
            }
        };
        let (rel, fields) = self
            .protocol()?
            .request(&op, req)
            .map_err(|e| internal_error!("client request: {e}"))?;
        let mut row = vec![Value::Node(to)];
        row.extend(fields);
        let delay = self.rng.range(self.cfg.latency.0, self.cfg.latency.1);
        let c = self.clients.get_mut(client).ok_or_else(|| internal_error!("no client"))?;
        c.target = to;
        c.retry = None;
        c.wake = i64::MAX;
        self.schedule(
            delay,
            Envelope::FromClient {
                client,
                to,
                rel,
                row: Row::from(row),
            },
        );
        Ok(())
    }

    fn nemesis(&mut self) -> Result<(), SimError> {
        let n = self.nodes.len();
        let mut actions: Vec<u8> = Vec::new();
        if self.cfg.crashes {
            actions.push(0);
        }
        if self.cfg.partitions {
            actions.push(1);
            actions.push(2);
        }
        if actions.is_empty() || n == 0 {
            return Ok(());
        }
        let pick = actions
            .get(usize::try_from(self.rng.below(actions.len() as u64)).unwrap_or(0))
            .copied()
            .unwrap_or(0);
        let victim = node_id(usize::try_from(self.rng.below(n as u64)).unwrap_or(0))?;
        match pick {
            0 => {
                // Crash (losing or tearing unsynced writes) and restart at once: a kill -9 and a supervisor.
                let slot = self
                    .nodes
                    .get_mut(victim.0 as usize)
                    .ok_or_else(|| internal_error!("no node"))?;
                slot.driver = None;
                let mut rng = Rng(self.rng.next());
                slot.fs
                    .crash(&mut |_| match rng.below(3) {
                        0 => WriteFate::Lost,
                        1 => WriteFate::Survive,
                        _ => WriteFate::Torn { sectors: 1 },
                    })
                    .map_err(|e| internal_error!("crash: {e}"))?;
                self.run.crashes += 1;
                self.run.log.push(format!("{}: crash and restart node {}", self.now - EPOCH, victim.0));
                self.boot(victim, false)?;
            }
            1 => {
                // Isolate one node from the others, both ways.
                self.blocked.clear();
                for o in 0..n {
                    let o = node_id(o)?;
                    if o != victim {
                        self.blocked.insert((victim, o));
                        self.blocked.insert((o, victim));
                    }
                }
                self.run.partitions += 1;
                self.run.log.push(format!("{}: isolate node {}", self.now - EPOCH, victim.0));
            }
            _ => {
                self.blocked.clear();
                self.run.log.push(format!("{}: heal", self.now - EPOCH));
            }
        }
        Ok(())
    }
}
