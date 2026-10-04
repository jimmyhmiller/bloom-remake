//! Byte streams in the synchronous-round world (FOREIGN-PROTOCOLS §1): connections between nodes of the deployment,
//! a connect stream dialing `sim://NODE/STREAM` (a node's listen stream, as in the cluster simulator).
//!
//! The rules are the cluster simulator's ([`crate::cluster`]), on rounds:
//! - What a node asks of its host in round `t` (writes, pauses and resumes, closes, the connections whose `closed` it
//!   took, dials, in that order, as the runtime releases them) is done by its host at the end of the round: first
//!   what is local to it, at every node ([`Fabric::release`]: a write's place in `seq` order, a pause or resume, the
//!   node end's own close), then what crosses the network, which arrives at the start of round `t + 1`
//!   ([`Fabric::deliver`]: bytes, a close the other end learns of, a dial). So a node's pause or close in round `t`
//!   holds or drops what its peers send in round `t`, whatever the nodes' order. The runtime's [`StreamInbox`] gives
//!   each node its stream events, in the language's order (§1.2a): a connection's `opened` comes in an earlier round
//!   than its `data`, and its `closed` after its last `data`.
//! - A dial in round `t` connects if the node and listen stream it names exist and the node runs round `t + 1`: both
//!   ends get `opened` in round `t + 1`. Otherwise the dialing end gets `failed` in round `t + 1`.
//! - A node end writes through a [`SeqWriter`]: bytes leave in `seq` order; a bad `seq` closes the connection, the
//!   node end's `closed` carrying the error.
//! - A program's `close`: the other end learns, after the bytes sent, and the node end gets its own `closed`. A node
//!   end whose `closed` the node took retires: the other end learns.
//! - A `pause` holds what arrives for that end, in order, a close behind it included, until its `resume`.
//! - A reset tells both ends at once and drops what is in flight, a paused end learning when it resumes: a crash of
//!   either end's node (from the crash round), and a lost message. An omission of `from → to` in round `t` (the
//!   faults' vocabulary: everything `from` sends `to` in round `t` is lost) resets every connection that carries
//!   bytes or a close from `from` to `to` in round `t`, and fails a dial from `from` to `to` made in round `t`.
//! - A restarted node begins with no connections; its connection ids carry its restart count above bit 32.
//!
//! For lineage, every stream event a node takes is recorded with its causes (the program requests and the stream
//! events it comes from, or the fault that reset its connection), the connection it is on and the round its causes
//! are as of; every connection is recorded with its dial and the rounds traffic crossed it in.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use blossom_base::{RelId, internal_error};
use blossom_ir::core::StreamKind;
use blossom_ir::tick::HostOut;
use blossom_node::streams::{HostRequest, NodeStream, Observed, SeqWriter, StreamInbox, host_request};
use blossom_oracle::Row;
use blossom_value::Value;
use blossom_value::time::{Instant, NodeId, Tick};
use blossom_value::value::ConnId;

use crate::sync::SimError;

/// The streams of a deployment.
#[derive(Clone, Debug)]
pub struct StreamsConfig {
    /// Each node's streams (those its role runs), in the program's order: a stream's index here is the index its
    /// host requests decode to.
    pub nodes: Vec<Vec<NodeStream>>,
    /// Each node's name, for `sim://NODE/STREAM`.
    pub names: Vec<Arc<str>>,
    /// The most bytes one connection's `data` carries in one round (`max_stream_bytes`).
    pub budget: usize,
}

impl StreamsConfig {
    /// The streams of `program` on a deployment whose nodes have these names and roles (`budget` as the runtime's
    /// default, 1 MiB).
    pub fn of(
        program: &blossom_ir::core::Program,
        names: &[Arc<str>],
        roles: &[Option<blossom_base::RoleId>],
    ) -> StreamsConfig {
        StreamsConfig {
            nodes: roles
                .iter()
                .map(|r| blossom_node::streams::node_streams(program, *r))
                .collect(),
            names: names.to_vec(),
            budget: 1024 * 1024,
        }
    }

    /// Whether any node runs a stream.
    pub fn any(&self) -> bool {
        self.nodes.iter().any(|s| !s.is_empty())
    }
}

/// What a stream event comes from.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cause {
    /// A request of `node`'s program in round `tick`, a row of a stream's `write`, `close`, `resume` or `dial`: the
    /// write whose bytes an event carries, the close it reports, the resume that let it through, the dial it answers.
    Request {
        node: NodeId,
        tick: Tick,
        rel: RelId,
        row: Row,
    },
    /// A stream event `node` took in round `tick`: the `closed` after which its end retired (the peer learns).
    Taken {
        node: NodeId,
        tick: Tick,
        rel: RelId,
        row: Row,
    },
    /// A reset by the crash of `node` in round `tick`.
    Crash { node: NodeId, tick: Tick },
    /// A reset, or a failed dial, by the omission of `from → to` in round `tick`.
    Omission { from: NodeId, to: NodeId, tick: Tick },
}

/// A stream event a node took in a round, with its causes and its connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamEvent {
    pub rel: RelId,
    pub row: Row,
    pub causes: Vec<Cause>,
    /// The connection the event is on (none for a failed dial), and the latest round of its causes: the connection
    /// had to last until then.
    pub conn: Option<ConnRef>,
}

/// A stream event's connection: its index in [`Fabric::connections`], and the round the event's causes are as of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnRef {
    pub pipe: usize,
    pub as_of: Tick,
}

/// A connection of a run: end 0 dialed in round `dialed`, both ends opened in the next; and every round something
/// crossed it, from one node to the other (a lost message in such a round resets it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connection {
    pub ends: [ConnEnd; 2],
    pub dialed: Tick,
    pub traffic: BTreeSet<(NodeId, NodeId, Tick)>,
}

/// One end of a [`Connection`]: its node, its id there, and the relation its `opened` event is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnEnd {
    pub node: NodeId,
    pub conn: ConnId,
    pub opened: RelId,
}

/// What a delivery comes from, and the round that is as of.
#[derive(Clone, Debug, Default)]
struct Trace {
    causes: Vec<Cause>,
    as_of: Tick,
}

impl Trace {
    fn of(cause: &Cause, as_of: Tick) -> Trace {
        Trace {
            causes: vec![cause.clone()],
            as_of,
        }
    }
}

/// A request a node's host refused: a located runtime error of the program (a bad `seq`, a request through a stream
/// the connection is not of, a blob range outside its blob), counted as the cluster simulator counts it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct StreamViolation {
    pub node: NodeId,
    pub tick: Tick,
    pub why: String,
}

/// One connection end at a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct End {
    node: NodeId,
    conn: ConnId,
    stream: usize,
}

/// What crosses the network in a round: towards end `to` of pipe `p`, or a dial.
#[derive(Debug)]
enum Flight {
    Towards {
        p: usize,
        to: usize,
        what: Held,
    },
    Dial {
        from: NodeId,
        stream: usize,
        req: u64,
        addr: Arc<str>,
        cause: Cause,
    },
}

/// What a paused end has not been given yet.
#[derive(Debug)]
enum Held {
    Bytes(Vec<u8>, Trace),
    Closed(Arc<str>, Trace),
}

/// A connection: end 0 dialed, end 1 accepted.
#[derive(Debug)]
struct Pipe {
    ends: [End; 2],
    paused: [bool; 2],
    held: [VecDeque<Held>; 2],
    writers: [SeqWriter; 2],
    /// Per end: the next `seq` its writer sends, and the write request of each `seq` it holds back.
    seq_next: [u64; 2],
    seq_causes: [BTreeMap<u64, Cause>; 2],
    /// Whether each end was told the connection closed (it gets nothing more).
    told: [bool; 2],
}

/// What a node's inbox holds for one connection, by cause (the inbox itself keeps the bytes).
#[derive(Debug, Default)]
struct Mirror {
    pipe: usize,
    opened: Trace,
    /// The bytes not yet delivered, as runs of one trace.
    pending: VecDeque<(usize, Trace)>,
    closing: Option<Trace>,
}

/// The streams of a run: each node's inbox, and the connections between them.
pub struct Fabric<'c> {
    cfg: &'c StreamsConfig,
    inboxes: Vec<StreamInbox>,
    pipes: Vec<Pipe>,
    /// Each live node end: its pipe and side.
    ends: BTreeMap<(NodeId, ConnId), (usize, usize)>,
    mirrors: BTreeMap<(NodeId, ConnId), Mirror>,
    /// Per node, the causes of the failed dials its inbox holds, in its order.
    failed: Vec<VecDeque<Trace>>,
    /// The `closed` events each node took this round, by connection: their ends retire when the round is released.
    taken_closed: BTreeMap<(NodeId, ConnId), Cause>,
    /// Per node: its restarts, and the next connection number of its incarnation.
    restarts: Vec<u64>,
    next_conn: Vec<u64>,
    /// What the round's releases sent, in order, for [`Fabric::deliver`].
    flights: Vec<Flight>,
    pub violations: Vec<StreamViolation>,
    /// Every connection made, by pipe index.
    pub connections: Vec<Connection>,
}

fn side<T>(pair: &[T; 2], i: usize) -> Result<&T, SimError> {
    pair.get(i)
        .ok_or_else(|| internal_error!("a connection has no end {i}").into())
}

fn side_mut<T>(pair: &mut [T; 2], i: usize) -> Result<&mut T, SimError> {
    pair.get_mut(i)
        .ok_or_else(|| internal_error!("a connection has no end {i}").into())
}

fn node_error(node: NodeId, e: blossom_node::NodeError) -> SimError {
    SimError::Internal(internal_error!("node {}: {e}", node.0))
}

impl<'c> Fabric<'c> {
    pub fn new(cfg: &'c StreamsConfig) -> Fabric<'c> {
        let n = cfg.nodes.len();
        Fabric {
            cfg,
            inboxes: cfg
                .nodes
                .iter()
                .map(|s| StreamInbox::new(s.clone(), cfg.budget))
                .collect(),
            pipes: Vec::new(),
            ends: BTreeMap::new(),
            mirrors: BTreeMap::new(),
            failed: vec![VecDeque::new(); n],
            taken_closed: BTreeMap::new(),
            restarts: vec![0; n],
            next_conn: vec![0; n],
            flights: Vec::new(),
            violations: Vec::new(),
            connections: Vec::new(),
        }
    }

    /// The stream events `node` takes in round `tick`, and the connections whose `closed` they deliver (which retire
    /// when the round is released: give them to [`Fabric::release`]).
    pub fn take(&mut self, node: NodeId, tick: Tick) -> Result<(Vec<StreamEvent>, Vec<ConnId>), SimError> {
        let i = node.0 as usize;
        let inbox = self
            .inboxes
            .get_mut(i)
            .ok_or_else(|| internal_error!("no stream inbox for node {i}"))?;
        let events = inbox.take(tick.0).map_err(|e| node_error(node, e))?;
        let retired = inbox.take_retired();
        let streams = self
            .cfg
            .nodes
            .get(i)
            .ok_or_else(|| internal_error!("no streams for node {i}"))?;
        let mut out = Vec::with_capacity(events.len());
        for (rel, row) in events {
            if streams.iter().any(|s| s.failed == Some(rel)) {
                let trace = self
                    .failed
                    .get_mut(i)
                    .and_then(|f| f.pop_front())
                    .ok_or_else(|| internal_error!("a failed dial at node {i} without its cause"))?;
                out.push(StreamEvent {
                    rel,
                    row,
                    causes: trace.causes,
                    conn: None,
                });
                continue;
            }
            let Some(Value::Conn(conn)) = row.first() else {
                return Err(internal_error!("a stream event {row:?} without its connection").into());
            };
            let key = (node, *conn);
            let mirror = self
                .mirrors
                .get_mut(&key)
                .ok_or_else(|| internal_error!("a stream event of connection {conn:?} the fabric does not know"))?;
            let pipe = mirror.pipe;
            let trace = if streams.iter().any(|s| s.opened == rel) {
                std::mem::take(&mut mirror.opened)
            } else if streams.iter().any(|s| s.data == rel) {
                let Some(Value::Bytes(b)) = row.get(2) else {
                    return Err(internal_error!("a data event {row:?} without its bytes").into());
                };
                take_trace(&mut mirror.pending, b.len())?
            } else if streams.iter().any(|s| s.closed == rel) {
                let trace = mirror
                    .closing
                    .take()
                    .ok_or_else(|| internal_error!("a closed event of connection {conn:?} without its close"))?;
                self.mirrors.remove(&key);
                self.taken_closed.insert(
                    key,
                    Cause::Taken {
                        node,
                        tick,
                        rel,
                        row: row.clone(),
                    },
                );
                trace
            } else {
                return Err(internal_error!("stream event {rel:?} of no stream of node {i}").into());
            };
            out.push(StreamEvent {
                rel,
                row,
                causes: trace.causes,
                conn: Some(ConnRef {
                    pipe,
                    as_of: trace.as_of,
                }),
            });
        }
        Ok((out, retired))
    }

    /// Does what is local to `node` of what it asked of its host in round `tick` (its host requests, then the
    /// retirement of the connections whose `closed` it took), and sends the rest, for [`Fabric::deliver`].
    pub fn release(
        &mut self,
        node: NodeId,
        tick: Tick,
        host: &[HostOut],
        retired: &[ConnId],
        blobs: &dyn blossom_value::BlobSource,
    ) -> Result<(), SimError> {
        let streams = self
            .cfg
            .nodes
            .get(node.0 as usize)
            .ok_or_else(|| internal_error!("no streams for node {}", node.0))?;
        let mut writes = Vec::new();
        let mut pauses = Vec::new();
        let mut closes = Vec::new();
        let mut refused = Vec::new();
        let mut dials = Vec::new();
        for h in host {
            let cause = Cause::Request {
                node,
                tick,
                rel: h.rel,
                row: h.row.clone(),
            };
            match host_request(streams, h, blobs).map_err(|e| node_error(node, e))? {
                HostRequest::Write {
                    stream,
                    conn,
                    seq,
                    bytes,
                } => writes.push((conn, seq, stream, bytes, cause)),
                HostRequest::Close { stream, conn } => closes.push((stream, conn, cause)),
                HostRequest::Pause { stream, conn, paused } => pauses.push((!paused, conn, stream, cause)),
                HostRequest::Refused { stream, conn, why } => refused.push((stream, conn, why, cause)),
                HostRequest::Dial { stream, req, addr } => dials.push((stream, req, addr, cause)),
            }
        }
        for (stream, conn, why, cause) in refused {
            if let Some(&(p, end)) = self.ends.get(&(node, conn))
                && self.through_its_stream(node, tick, p, end, stream, "write")?
            {
                self.violate(node, tick, why.clone());
                self.host_close(p, end, &why, &cause, tick)?;
            }
        }
        writes.sort_by_key(|w| (w.0, w.1));
        for (conn, seq, stream, bytes, cause) in writes {
            let Some(&(p, end)) = self.ends.get(&(node, conn)) else {
                continue;
            };
            if !self.through_its_stream(node, tick, p, end, stream, "write")? {
                continue;
            }
            let pipe = self.pipe_mut(p)?;
            if *side(&pipe.told, end)? && *side(&pipe.told, 1 - end)? {
                continue;
            }
            // Each `seq`'s write request, until its bytes leave (a gap holds later ones back).
            let before = side_mut(&mut pipe.seq_causes, end)?.insert(seq, cause.clone());
            match side_mut(&mut pipe.writers, end)?.accept(seq, bytes) {
                Ok(ready) => {
                    let first = *side(&pipe.seq_next, end)?;
                    *side_mut(&mut pipe.seq_next, end)? = first + ready.len() as u64;
                    let mut leaving = Vec::with_capacity(ready.len());
                    for (k, b) in ready.into_iter().enumerate() {
                        let request = side_mut(&mut pipe.seq_causes, end)?
                            .remove(&(first + k as u64))
                            .ok_or_else(|| internal_error!("bytes of seq {} without their write", first + k as u64))?;
                        if !b.is_empty() {
                            leaving.push((b, request));
                        }
                    }
                    for (b, request) in leaving {
                        self.flights.push(Flight::Towards {
                            p,
                            to: 1 - end,
                            what: Held::Bytes(b, Trace::of(&request, tick)),
                        });
                    }
                }
                Err(why) => {
                    // The refused write is not held: the `seq` keeps the request it had, if any.
                    let causes = side_mut(&mut pipe.seq_causes, end)?;
                    match before {
                        Some(c) => causes.insert(seq, c),
                        None => causes.remove(&seq),
                    };
                    self.violate(node, tick, why.clone());
                    self.host_close(p, end, &why, &cause, tick)?;
                }
            }
        }
        // Pauses before resumes, as the runtime applies them: a round that asks both reads the connection.
        pauses.sort_by_key(|p| (p.0, p.1, p.2));
        for (resume, conn, stream, cause) in pauses {
            let Some(&(p, end)) = self.ends.get(&(node, conn)) else {
                continue;
            };
            let what = if resume { "resume" } else { "pause" };
            if self.through_its_stream(node, tick, p, end, stream, what)? {
                self.pause(p, end, !resume, &cause, tick)?;
            }
        }
        for (stream, conn, cause) in closes {
            let Some(&(p, end)) = self.ends.get(&(node, conn)) else {
                continue;
            };
            if self.through_its_stream(node, tick, p, end, stream, "close")? {
                self.host_close(p, end, "closed by the program", &cause, tick)?;
            }
        }
        for conn in retired {
            let taken = self
                .taken_closed
                .remove(&(node, *conn))
                .ok_or_else(|| internal_error!("connection {conn:?} retires without the closed event it took"))?;
            if let Some((p, end)) = self.ends.remove(&(node, *conn)) {
                self.half_close(p, end, &taken, tick);
            }
        }
        for (stream, req, addr, cause) in dials {
            self.flights.push(Flight::Dial {
                from: node,
                stream,
                req,
                addr,
                cause,
            });
        }
        Ok(())
    }

    /// Delivers what the releases of round `tick` sent, at the start of round `tick + 1`. `up_next` says which nodes
    /// run that round, `lost(from, to)` whether the fault schedule loses what `from` sends `to` in round `tick`,
    /// and `at` is the clock of round `tick + 1`.
    pub fn deliver(
        &mut self,
        tick: Tick,
        up_next: &dyn Fn(NodeId) -> bool,
        lost: &dyn Fn(NodeId, NodeId) -> bool,
        at: Instant,
    ) -> Result<(), SimError> {
        for f in std::mem::take(&mut self.flights) {
            match f {
                Flight::Towards { p, to, what } => {
                    let pipe = self.pipe(p)?;
                    if *side(&pipe.told, to)? {
                        continue;
                    }
                    let (from, dest) = (side(&pipe.ends, 1 - to)?.node, side(&pipe.ends, to)?.node);
                    if from != dest
                        && let Some(c) = self.connections.get_mut(p)
                    {
                        c.traffic.insert((from, dest, tick));
                    }
                    if from != dest && lost(from, dest) {
                        let cause = Cause::Omission { from, to: dest, tick };
                        // The connection lasted until the round before the lost message.
                        let as_of = tick.prev().unwrap_or(tick);
                        self.reset(p, "connection reset (a lost message)", &cause, as_of)?;
                        continue;
                    }
                    self.deliver_to(p, to, what)?;
                }
                Flight::Dial {
                    from,
                    stream,
                    req,
                    addr,
                    cause,
                } => self.dial(from, tick, stream, req, &addr, &cause, up_next, lost, at)?,
            }
        }
        Ok(())
    }

    /// `node` goes down in round `tick`: every connection it has resets, and it forgets them all (a restarted node
    /// begins with none).
    pub fn node_down(&mut self, node: NodeId, tick: Tick) -> Result<(), SimError> {
        if !self.flights.is_empty() {
            return Err(internal_error!("a node went down with a round's releases not delivered").into());
        }
        let mine: Vec<(usize, usize)> = self
            .ends
            .iter()
            .filter(|((n, _), _)| *n == node)
            .map(|(_, pe)| *pe)
            .collect();
        let cause = Cause::Crash { node, tick };
        let as_of = tick.prev().unwrap_or(tick);
        for (p, end) in mine {
            // The node's own end learns nothing: it is gone.
            *side_mut(&mut self.pipe_mut(p)?.told, end)? = true;
            self.reset(p, "the node crashed", &cause, as_of)?;
        }
        self.ends.retain(|(n, _), _| *n != node);
        self.mirrors.retain(|(n, _), _| *n != node);
        self.taken_closed.retain(|(n, _), _| *n != node);
        let i = node.0 as usize;
        if let Some(inbox) = self.inboxes.get_mut(i) {
            inbox.clear();
        }
        if let Some(f) = self.failed.get_mut(i) {
            f.clear();
        }
        if let Some(r) = self.restarts.get_mut(i) {
            *r += 1;
        }
        if let Some(c) = self.next_conn.get_mut(i) {
            *c = 0;
        }
        Ok(())
    }

    fn pipe_mut(&mut self, p: usize) -> Result<&mut Pipe, SimError> {
        self.pipes
            .get_mut(p)
            .ok_or_else(|| internal_error!("no connection {p}").into())
    }

    fn pipe(&self, p: usize) -> Result<&Pipe, SimError> {
        self.pipes
            .get(p)
            .ok_or_else(|| internal_error!("no connection {p}").into())
    }

    fn violate(&mut self, node: NodeId, tick: Tick, why: String) {
        self.violations.push(StreamViolation { node, tick, why });
    }

    /// Whether a request through `stream` names a connection of that stream; otherwise it is refused, a violation.
    fn through_its_stream(
        &mut self,
        node: NodeId,
        tick: Tick,
        p: usize,
        end: usize,
        stream: usize,
        what: &str,
    ) -> Result<bool, SimError> {
        let e = *side(&self.pipe(p)?.ends, end)?;
        if e.stream == stream {
            return Ok(true);
        }
        self.violate(
            node,
            tick,
            format!(
                "a {what} through stream {stream} to connection {} of stream {}",
                e.conn.0, e.stream
            ),
        );
        Ok(false)
    }

    fn allocate_conn(&mut self, node: NodeId) -> Result<ConnId, SimError> {
        let i = node.0 as usize;
        let restarts = self.restarts.get(i).copied().unwrap_or(0);
        let next = self
            .next_conn
            .get_mut(i)
            .ok_or_else(|| internal_error!("no node {i}"))?;
        let c = ConnId(restarts << 32 | *next);
        *next += 1;
        Ok(c)
    }

    /// The index of `node`'s listen stream named `name`.
    fn listen_stream(&self, node: NodeId, name: &str) -> Option<usize> {
        self.cfg
            .nodes
            .get(node.0 as usize)?
            .iter()
            .position(|s| &*s.name == name && s.kind == StreamKind::Listen)
    }

    #[allow(clippy::too_many_arguments)]
    fn dial(
        &mut self,
        from: NodeId,
        tick: Tick,
        stream: usize,
        req: u64,
        addr: &str,
        act: &Cause,
        up_next: &dyn Fn(NodeId) -> bool,
        lost: &dyn Fn(NodeId, NodeId) -> bool,
        at: Instant,
    ) -> Result<(), SimError> {
        let target = addr.strip_prefix("sim://").and_then(|rest| rest.split_once('/'));
        let found = target.and_then(|(name, s)| {
            let n = self.cfg.names.iter().position(|x| &**x == name)?;
            let n = NodeId(u32::try_from(n).ok()?);
            Some((n, self.listen_stream(n, s)?))
        });
        let traced = |causes: Vec<Cause>| Trace { causes, as_of: tick };
        let (to, listen) = match found {
            None => {
                let why = format!("cannot reach `{addr}`");
                return self.dial_failed(from, stream, req, why, traced(vec![act.clone()]));
            }
            Some((to, _)) if to != from && lost(from, to) => {
                let why = format!("`{addr}` is unreachable (a lost message)");
                let cause = Cause::Omission { from, to, tick };
                return self.dial_failed(from, stream, req, why, traced(vec![act.clone(), cause]));
            }
            Some((to, _)) if !up_next(to) => {
                let why = format!("node {} is down", to.0);
                return self.dial_failed(from, stream, req, why, traced(vec![act.clone()]));
            }
            Some(x) => x,
        };
        let p = self.pipes.len();
        let dialer = End {
            node: from,
            conn: self.allocate_conn(from)?,
            stream,
        };
        let acceptor = End {
            node: to,
            conn: self.allocate_conn(to)?,
            stream: listen,
        };
        self.pipes.push(Pipe {
            ends: [dialer, acceptor],
            paused: [false, false],
            held: [VecDeque::new(), VecDeque::new()],
            writers: [SeqWriter::default(), SeqWriter::default()],
            seq_next: [0, 0],
            seq_causes: [BTreeMap::new(), BTreeMap::new()],
            told: [false, false],
        });
        let opened_rel = |e: End| -> Result<RelId, SimError> {
            self.cfg
                .nodes
                .get(e.node.0 as usize)
                .and_then(|s| s.get(e.stream))
                .map(|s| s.opened)
                .ok_or_else(|| internal_error!("connection end of an unknown stream").into())
        };
        self.connections.push(Connection {
            ends: [
                ConnEnd {
                    node: dialer.node,
                    conn: dialer.conn,
                    opened: opened_rel(dialer)?,
                },
                ConnEnd {
                    node: acceptor.node,
                    conn: acceptor.conn,
                    opened: opened_rel(acceptor)?,
                },
            ],
            dialed: tick,
            traffic: BTreeSet::new(),
        });
        let peer: Arc<str> = Arc::from(format!("sim-pipe-{p}"));
        for (end, e, req) in [(1, acceptor, None), (0, dialer, Some(req))] {
            self.ends.insert((e.node, e.conn), (p, end));
            self.mirrors.insert(
                (e.node, e.conn),
                Mirror {
                    pipe: p,
                    opened: Trace::of(act, tick),
                    ..Mirror::default()
                },
            );
            self.observe(
                e.node,
                Observed::Opened {
                    stream: e.stream,
                    conn: e.conn,
                    peer: peer.clone(),
                    req,
                    at,
                },
            )?;
        }
        Ok(())
    }

    fn dial_failed(
        &mut self,
        node: NodeId,
        stream: usize,
        req: u64,
        why: String,
        trace: Trace,
    ) -> Result<(), SimError> {
        self.failed
            .get_mut(node.0 as usize)
            .ok_or_else(|| internal_error!("no node {}", node.0))?
            .push_back(trace);
        self.observe(
            node,
            Observed::Failed {
                stream,
                req,
                reason: Arc::from(why),
            },
        )
    }

    fn observe(&mut self, node: NodeId, o: Observed) -> Result<(), SimError> {
        self.inboxes
            .get_mut(node.0 as usize)
            .ok_or_else(|| internal_error!("no stream inbox for node {}", node.0))?
            .observe(o)
            .map_err(|e| node_error(node, e))
    }

    /// Gives end `to` of pipe `p` bytes or its close, or holds them while it is paused.
    fn deliver_to(&mut self, p: usize, to: usize, h: Held) -> Result<(), SimError> {
        let pipe = self.pipe_mut(p)?;
        if *side(&pipe.told, to)? {
            return Ok(());
        }
        if *side(&pipe.paused, to)? {
            side_mut(&mut pipe.held, to)?.push_back(h);
            return Ok(());
        }
        let e = *side(&pipe.ends, to)?;
        match h {
            Held::Bytes(bytes, trace) => {
                self.mirrors
                    .get_mut(&(e.node, e.conn))
                    .ok_or_else(|| internal_error!("bytes for connection {:?} the fabric does not know", e.conn))?
                    .pending
                    .push_back((bytes.len(), trace));
                self.observe(e.node, Observed::Bytes { conn: e.conn, bytes })
            }
            Held::Closed(reason, trace) => {
                *side_mut(&mut pipe.told, to)? = true;
                self.mirrors
                    .get_mut(&(e.node, e.conn))
                    .ok_or_else(|| internal_error!("a close of connection {:?} the fabric does not know", e.conn))?
                    .closing
                    .get_or_insert(trace);
                self.observe(e.node, Observed::Closed { conn: e.conn, reason })
            }
        }
    }

    /// End `from` sends no more, by `cause` in round `tick`: the other end learns, after the bytes sent.
    fn half_close(&mut self, p: usize, from: usize, cause: &Cause, tick: Tick) {
        self.flights.push(Flight::Towards {
            p,
            to: 1 - from,
            what: Held::Closed(Arc::from("closed by the peer"), Trace::of(cause, tick)),
        });
    }

    /// The host closes end `end` of pipe `p`, by `cause` in round `tick`: the other end learns after the bytes sent,
    /// and the end gets its own `closed` with `reason` (not held by a pause: its own host closed it).
    fn host_close(&mut self, p: usize, end: usize, reason: &str, cause: &Cause, tick: Tick) -> Result<(), SimError> {
        self.half_close(p, end, cause, tick);
        let pipe = self.pipe_mut(p)?;
        *side_mut(&mut pipe.paused, end)? = false;
        side_mut(&mut pipe.held, end)?.clear();
        self.deliver_to(p, end, Held::Closed(Arc::from(reason), Trace::of(cause, tick)))
    }

    /// Pauses end `end` of pipe `p`, or resumes it (by `cause` in round `tick`): a resume gives it what arrived
    /// meanwhile, in order.
    fn pause(&mut self, p: usize, end: usize, paused: bool, cause: &Cause, tick: Tick) -> Result<(), SimError> {
        let pipe = self.pipe_mut(p)?;
        *side_mut(&mut pipe.paused, end)? = paused;
        if paused {
            return Ok(());
        }
        let held: Vec<Held> = side_mut(&mut pipe.held, end)?.drain(..).collect();
        for h in held {
            let with = |mut t: Trace| {
                t.causes.push(cause.clone());
                t.as_of = tick;
                t
            };
            let h = match h {
                Held::Bytes(b, t) => Held::Bytes(b, with(t)),
                Held::Closed(r, t) => Held::Closed(r, with(t)),
            };
            self.deliver_to(p, end, h)?;
        }
        Ok(())
    }

    /// Resets pipe `p` by `cause`, the connection having lasted until `as_of`: both ends learn at once and nothing in
    /// flight arrives; a paused end learns when it resumes.
    fn reset(&mut self, p: usize, why: &str, cause: &Cause, as_of: Tick) -> Result<(), SimError> {
        for end in 0..2 {
            let pipe = self.pipe_mut(p)?;
            if *side(&pipe.told, end)? {
                continue;
            }
            let closed = Held::Closed(Arc::from(why), Trace::of(cause, as_of));
            if *side(&pipe.paused, end)? {
                let held = side_mut(&mut pipe.held, end)?;
                held.clear();
                held.push_back(closed);
            } else {
                self.deliver_to(p, end, closed)?;
            }
        }
        Ok(())
    }
}

/// The trace of the first `n` pending bytes, which leave: every cause (in order, once), as of the latest round.
fn take_trace(pending: &mut VecDeque<(usize, Trace)>, mut n: usize) -> Result<Trace, SimError> {
    let mut out = Trace::default();
    while n > 0 {
        let Some((len, trace)) = pending.front_mut() else {
            return Err(internal_error!("a data event longer than the bytes the fabric delivered").into());
        };
        out.causes.extend(trace.causes.iter().cloned());
        out.as_of = out.as_of.max(trace.as_of);
        if *len <= n {
            n -= *len;
            pending.pop_front();
        } else {
            *len -= n;
            n = 0;
        }
    }
    out.causes.sort();
    out.causes.dedup();
    Ok(out)
}
