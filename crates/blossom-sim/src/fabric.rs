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
//! Every stream event a node takes is recorded with its causes (the writes, dials, closes, crashes and omissions it
//! comes from), for lineage.

use std::collections::{BTreeMap, VecDeque};
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
    /// What `node` asked of its host in round `tick`: the write whose bytes an event carries, the dial it answers,
    /// the close or retirement it reports, the resume that let it through.
    Act { node: NodeId, tick: Tick },
    /// A reset by the crash of `node` in round `tick`.
    Crash { node: NodeId, tick: Tick },
    /// A reset, or a failed dial, by the omission of `from → to` in round `tick`.
    Omission { from: NodeId, to: NodeId, tick: Tick },
}

/// A stream event a node took in a round, with its causes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamEvent {
    pub rel: RelId,
    pub row: Row,
    pub causes: Vec<Cause>,
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
    },
}

/// What a paused end has not been given yet.
#[derive(Debug)]
enum Held {
    Bytes(Vec<u8>, Vec<Cause>),
    Closed(Arc<str>, Vec<Cause>),
}

/// A connection: end 0 dialed, end 1 accepted.
#[derive(Debug)]
struct Pipe {
    ends: [End; 2],
    paused: [bool; 2],
    held: [VecDeque<Held>; 2],
    writers: [SeqWriter; 2],
    /// Whether each end was told the connection closed (it gets nothing more).
    told: [bool; 2],
}

/// What a node's inbox holds for one connection, by cause (the inbox itself keeps the bytes).
#[derive(Debug, Default)]
struct Mirror {
    opened: Vec<Cause>,
    /// The bytes not yet delivered, as runs of one set of causes.
    pending: VecDeque<(usize, Vec<Cause>)>,
    closing: Option<Vec<Cause>>,
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
    failed: Vec<VecDeque<Vec<Cause>>>,
    /// Per node: its restarts, and the next connection number of its incarnation.
    restarts: Vec<u64>,
    next_conn: Vec<u64>,
    /// What the round's releases sent, in order, for [`Fabric::deliver`].
    flights: Vec<Flight>,
    pub violations: Vec<StreamViolation>,
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
            restarts: vec![0; n],
            next_conn: vec![0; n],
            flights: Vec::new(),
            violations: Vec::new(),
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
            let causes = if streams.iter().any(|s| s.failed == Some(rel)) {
                self.failed
                    .get_mut(i)
                    .and_then(|f| f.pop_front())
                    .ok_or_else(|| internal_error!("a failed dial at node {i} without its cause"))?
            } else {
                let Some(Value::Conn(conn)) = row.first() else {
                    return Err(internal_error!("a stream event {row:?} without its connection").into());
                };
                let key = (node, *conn);
                let mirror = self
                    .mirrors
                    .get_mut(&key)
                    .ok_or_else(|| internal_error!("a stream event of connection {conn:?} the fabric does not know"))?;
                if streams.iter().any(|s| s.opened == rel) {
                    std::mem::take(&mut mirror.opened)
                } else if streams.iter().any(|s| s.data == rel) {
                    let Some(Value::Bytes(b)) = row.get(2) else {
                        return Err(internal_error!("a data event {row:?} without its bytes").into());
                    };
                    take_causes(&mut mirror.pending, b.len())?
                } else if streams.iter().any(|s| s.closed == rel) {
                    let causes = mirror.closing.take().unwrap_or_default();
                    self.mirrors.remove(&key);
                    causes
                } else {
                    return Err(internal_error!("stream event {rel:?} of no stream of node {i}").into());
                }
            };
            out.push(StreamEvent { rel, row, causes });
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
            match host_request(streams, h, blobs).map_err(|e| node_error(node, e))? {
                HostRequest::Write {
                    stream,
                    conn,
                    seq,
                    bytes,
                } => writes.push((conn, seq, stream, bytes)),
                HostRequest::Close { stream, conn } => closes.push((stream, conn)),
                HostRequest::Pause { stream, conn, paused } => pauses.push((!paused, conn, stream)),
                HostRequest::Refused { stream, conn, why } => refused.push((stream, conn, why)),
                HostRequest::Dial { stream, req, addr } => dials.push((stream, req, addr)),
            }
        }
        let act = Cause::Act { node, tick };
        for (stream, conn, why) in refused {
            if let Some(&(p, end)) = self.ends.get(&(node, conn))
                && self.through_its_stream(node, tick, p, end, stream, "write")?
            {
                self.violate(node, tick, why.clone());
                self.host_close(p, end, &why, &act)?;
            }
        }
        writes.sort_by_key(|w| (w.0, w.1));
        for (conn, seq, stream, bytes) in writes {
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
            match side_mut(&mut pipe.writers, end)?.accept(seq, bytes) {
                Ok(ready) => {
                    for b in ready.into_iter().filter(|b| !b.is_empty()) {
                        self.flights.push(Flight::Towards {
                            p,
                            to: 1 - end,
                            what: Held::Bytes(b, vec![act.clone()]),
                        });
                    }
                }
                Err(why) => {
                    self.violate(node, tick, why.clone());
                    self.host_close(p, end, &why, &act)?;
                }
            }
        }
        // Pauses before resumes, as the runtime applies them: a round that asks both reads the connection.
        pauses.sort();
        for (resume, conn, stream) in pauses {
            let Some(&(p, end)) = self.ends.get(&(node, conn)) else {
                continue;
            };
            let what = if resume { "resume" } else { "pause" };
            if self.through_its_stream(node, tick, p, end, stream, what)? {
                self.pause(p, end, !resume, &act)?;
            }
        }
        for (stream, conn) in closes {
            let Some(&(p, end)) = self.ends.get(&(node, conn)) else {
                continue;
            };
            if self.through_its_stream(node, tick, p, end, stream, "close")? {
                self.host_close(p, end, "closed by the program", &act)?;
            }
        }
        for conn in retired {
            if let Some((p, end)) = self.ends.remove(&(node, *conn)) {
                self.half_close(p, end, &act);
            }
        }
        for (stream, req, addr) in dials {
            self.flights.push(Flight::Dial {
                from: node,
                stream,
                req,
                addr,
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
                    if from != dest && lost(from, dest) {
                        let cause = Cause::Omission { from, to: dest, tick };
                        self.reset(p, "connection reset (a lost message)", &cause)?;
                        continue;
                    }
                    self.deliver_to(p, to, what)?;
                }
                Flight::Dial {
                    from,
                    stream,
                    req,
                    addr,
                } => self.dial(from, tick, stream, req, &addr, up_next, lost, at)?,
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
        for (p, end) in mine {
            // The node's own end learns nothing: it is gone.
            *side_mut(&mut self.pipe_mut(p)?.told, end)? = true;
            self.reset(p, "the node crashed", &cause)?;
        }
        self.ends.retain(|(n, _), _| *n != node);
        self.mirrors.retain(|(n, _), _| *n != node);
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
        up_next: &dyn Fn(NodeId) -> bool,
        lost: &dyn Fn(NodeId, NodeId) -> bool,
        at: Instant,
    ) -> Result<(), SimError> {
        let act = Cause::Act { node: from, tick };
        let target = addr.strip_prefix("sim://").and_then(|rest| rest.split_once('/'));
        let found = target.and_then(|(name, s)| {
            let n = self.cfg.names.iter().position(|x| &**x == name)?;
            let n = NodeId(u32::try_from(n).ok()?);
            Some((n, self.listen_stream(n, s)?))
        });
        let (to, listen) = match found {
            None => return self.dial_failed(from, stream, req, format!("cannot reach `{addr}`"), vec![act]),
            Some((to, _)) if to != from && lost(from, to) => {
                let why = format!("`{addr}` is unreachable (a lost message)");
                let cause = Cause::Omission { from, to, tick };
                return self.dial_failed(from, stream, req, why, vec![act, cause]);
            }
            Some((to, _)) if !up_next(to) => {
                return self.dial_failed(from, stream, req, format!("node {} is down", to.0), vec![act]);
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
            told: [false, false],
        });
        let peer: Arc<str> = Arc::from(format!("sim-pipe-{p}"));
        for (end, e, req) in [(1, acceptor, None), (0, dialer, Some(req))] {
            self.ends.insert((e.node, e.conn), (p, end));
            self.mirrors.insert(
                (e.node, e.conn),
                Mirror {
                    opened: vec![act.clone()],
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
        causes: Vec<Cause>,
    ) -> Result<(), SimError> {
        self.failed
            .get_mut(node.0 as usize)
            .ok_or_else(|| internal_error!("no node {}", node.0))?
            .push_back(causes);
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
            Held::Bytes(bytes, causes) => {
                self.mirrors
                    .get_mut(&(e.node, e.conn))
                    .ok_or_else(|| internal_error!("bytes for connection {:?} the fabric does not know", e.conn))?
                    .pending
                    .push_back((bytes.len(), causes));
                self.observe(e.node, Observed::Bytes { conn: e.conn, bytes })
            }
            Held::Closed(reason, causes) => {
                *side_mut(&mut pipe.told, to)? = true;
                self.mirrors
                    .get_mut(&(e.node, e.conn))
                    .ok_or_else(|| internal_error!("a close of connection {:?} the fabric does not know", e.conn))?
                    .closing
                    .get_or_insert(causes);
                self.observe(e.node, Observed::Closed { conn: e.conn, reason })
            }
        }
    }

    /// End `from` sends no more: the other end learns, after the bytes sent.
    fn half_close(&mut self, p: usize, from: usize, act: &Cause) {
        self.flights.push(Flight::Towards {
            p,
            to: 1 - from,
            what: Held::Closed(Arc::from("closed by the peer"), vec![act.clone()]),
        });
    }

    /// The host closes end `end` of pipe `p`: the other end learns after the bytes sent, and the end gets its own
    /// `closed` with `reason` (not held by a pause: its own host closed it).
    fn host_close(&mut self, p: usize, end: usize, reason: &str, act: &Cause) -> Result<(), SimError> {
        self.half_close(p, end, act);
        let pipe = self.pipe_mut(p)?;
        *side_mut(&mut pipe.paused, end)? = false;
        side_mut(&mut pipe.held, end)?.clear();
        self.deliver_to(p, end, Held::Closed(Arc::from(reason), vec![act.clone()]))
    }

    /// Pauses end `end` of pipe `p`, or resumes it: a resume gives it what arrived meanwhile, in order.
    fn pause(&mut self, p: usize, end: usize, paused: bool, act: &Cause) -> Result<(), SimError> {
        let pipe = self.pipe_mut(p)?;
        *side_mut(&mut pipe.paused, end)? = paused;
        if paused {
            return Ok(());
        }
        let held: Vec<Held> = side_mut(&mut pipe.held, end)?.drain(..).collect();
        for h in held {
            let with = |mut c: Vec<Cause>| {
                c.push(act.clone());
                c
            };
            let h = match h {
                Held::Bytes(b, c) => Held::Bytes(b, with(c)),
                Held::Closed(r, c) => Held::Closed(r, with(c)),
            };
            self.deliver_to(p, end, h)?;
        }
        Ok(())
    }

    /// Resets pipe `p`: both ends learn at once and nothing in flight arrives; a paused end learns when it resumes.
    fn reset(&mut self, p: usize, why: &str, cause: &Cause) -> Result<(), SimError> {
        for end in 0..2 {
            let pipe = self.pipe_mut(p)?;
            if *side(&pipe.told, end)? {
                continue;
            }
            let closed = Held::Closed(Arc::from(why), vec![cause.clone()]);
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

/// The causes of the first `n` pending bytes, which leave.
fn take_causes(pending: &mut VecDeque<(usize, Vec<Cause>)>, mut n: usize) -> Result<Vec<Cause>, SimError> {
    let mut out: Vec<Cause> = Vec::new();
    while n > 0 {
        let Some((len, causes)) = pending.front_mut() else {
            return Err(internal_error!("a data event longer than the bytes the fabric delivered").into());
        };
        for c in causes.iter() {
            if !out.contains(c) {
                out.push(c.clone());
            }
        }
        if *len <= n {
            n -= *len;
            pending.pop_front();
        } else {
            *len -= n;
            n = 0;
        }
    }
    Ok(out)
}
