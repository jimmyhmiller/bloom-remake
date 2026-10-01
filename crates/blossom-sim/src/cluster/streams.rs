//! Byte streams in the cluster simulator (FOREIGN-PROTOCOLS §1.4): simulated TCP connections ("pipes") between a
//! node's stream and a Rust test client, or between two nodes (a connect stream dialing `sim://NODE/STREAM`).
//!
//! A pipe carries bytes in order in each direction. Whatever an end sends is split at random byte boundaries and
//! each chunk arrives after a random latency, never before the chunk sent ahead of it, so programs see arbitrary
//! chunking. A node end writes through [`SeqWriter`], as the runtime does, and its writes leave only when their tick
//! is released. Closing follows the runtime:
//! - A client's close is a half-close: the node end learns after the bytes in flight, and the client still reads
//!   what the node writes until the node end retires.
//! - A program's `close`: the other end learns after the bytes in flight, and the node end gets its own `closed`.
//! - A node end retires when the tick that delivered its `closed` is released; the other end then learns, after the
//!   bytes in flight.
//! - A node end writing a bad `seq`, as in the runtime: the host closes it; the other end learns after the bytes in
//!   flight, and the node end's `closed` carries the error.
//! - A reset tells both ends at once and drops what is in flight: a crash of a node end's node, a partition between
//!   its two nodes (also one that begins while a connection is being made or carries bytes), or the nemesis. A
//!   connecting end that was not yet told the connection opened learns that it failed.
//! - A connection's opening is ordered with what follows it: the accepting end gets it before the connecting end's
//!   bytes and close, and the connecting end learns it opened before the accepting end's bytes and close.
//! - A request through a stream the connection does not belong to is refused and counted as a violation.
//! - A program's `pause` stops a node end reading, as the runtime's reader stops: what arrives for it is held, in
//!   order, and so is a peer's close or a reset behind it (a reader that does not read learns of neither), until its
//!   `resume` delivers them. A node end's own close, or its node's crash, is not held.
//!
//! Every choice is drawn from the simulation's seeded generator, so a run replays exactly.

use std::collections::VecDeque;
use std::sync::Arc;

use blossom_base::internal_error;
use blossom_node::streams::{HostRequest, Observed, SeqWriter, host_request};
use blossom_value::time::{Instant, NodeId};
use blossom_value::value::ConnId;

use super::{Cluster, Envelope, node_id};
use crate::sync::SimError;

/// What happens to a simulated stream client.
#[derive(Debug)]
pub enum StreamEvent<'a> {
    /// Its wake-up time came (or it just joined).
    Wake,
    /// Its connection was established.
    Opened,
    /// Bytes arrived on its connection.
    Received(&'a [u8]),
    /// Its connection closed or could not be established, with why.
    Closed(&'a str),
}

/// What a stream client does in answer to an event.
#[derive(Debug, Default)]
pub struct StreamAction {
    /// Connect to this node's listen stream (when not connected).
    pub connect: Option<(NodeId, Arc<str>)>,
    /// Bytes to send on its connection.
    pub send: Vec<u8>,
    /// Close its connection (after `send`).
    pub close: bool,
    /// When to wake it next (virtual nanoseconds since the start).
    pub wake: Option<i64>,
}

/// A simulated peer of a node's byte stream: a Rust test client, driven by the simulator.
pub trait StreamClient {
    /// Handles an event at virtual time `now` (nanoseconds since the start). An error is an invariant violation:
    /// the run stops there.
    fn on(&mut self, now: i64, e: StreamEvent<'_>) -> Result<StreamAction, String>;
}

/// End `i` (0 or 1) of a pipe's per-end pair.
fn side<T>(pair: &[T; 2], i: usize) -> &T {
    let [a, b] = pair;
    if i == 0 { a } else { b }
}

fn side_mut<T>(pair: &mut [T; 2], i: usize) -> &mut T {
    let [a, b] = pair;
    if i == 0 { a } else { b }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum End {
    Client(usize),
    /// A node's connection, in one incarnation of the node.
    Node {
        node: NodeId,
        restarts: u64,
        conn: ConnId,
    },
    /// A node end not yet accepted: the target node and its listen stream.
    Pending {
        node: NodeId,
        stream: usize,
    },
    /// A connection attempt that reached no listener: it is refused.
    Nowhere,
}

/// What a paused node end has not been given yet.
#[derive(Debug)]
enum Held {
    Bytes(Vec<u8>),
    Closed(Arc<str>),
}

#[derive(Debug)]
pub(super) struct Pipe {
    ends: [End; 2],
    /// Whether each node end's program paused it, and what arrived for it since, in order.
    paused: [bool; 2],
    held: [VecDeque<Held>; 2],
    /// Each node end's writes in `seq` order.
    writers: [SeqWriter; 2],
    /// When the last delivery towards each end is due: the next arrives no earlier.
    due: [i64; 2],
    /// Whether each end still takes bytes.
    recv: [bool; 2],
    /// Whether each end was told the connection closed (it gets no more events).
    told: [bool; 2],
    /// A dialed pipe: the dialing node end is end 0, with its stream and request id.
    dial: Option<(usize, u64)>,
    /// Whether end 0 (the connecting end) was told the connection is open. Until then a reset is, to it, a failed
    /// connection attempt, not the close of a connection it has.
    opened: bool,
    /// The listen stream of the accepting node end, once accepted.
    accepted: Option<usize>,
}

impl Pipe {
    fn new(ends: [End; 2], now: i64, dial: Option<(usize, u64)>) -> Pipe {
        Pipe {
            ends,
            paused: [false, false],
            held: [VecDeque::new(), VecDeque::new()],
            writers: [SeqWriter::default(), SeqWriter::default()],
            due: [now, now],
            recv: [true, true],
            told: [false, false],
            dial,
            opened: false,
            accepted: None,
        }
    }

    /// The stream of node end `end`: the dialing stream of end 0, the listen stream of end 1.
    fn stream_of(&self, end: usize) -> Option<usize> {
        if end == 0 {
            self.dial.map(|d| d.0)
        } else {
            self.accepted
        }
    }

    fn live(&self) -> bool {
        !(self.told[0] && self.told[1])
    }
}

pub(super) struct ClientSlot<'p> {
    client: Box<dyn StreamClient + 'p>,
    pipe: Option<usize>,
    wake: i64,
}

impl<'p> Cluster<'p> {
    /// Adds a stream client; it gets its first event (`Wake`) at once.
    pub fn stream_client(&mut self, c: Box<dyn StreamClient + 'p>) {
        self.stream_clients.push(ClientSlot {
            client: c,
            pipe: None,
            wake: self.now,
        });
    }

    /// The earliest wake-up of a stream client.
    pub(super) fn stream_wake(&self) -> Option<i64> {
        self.stream_clients.iter().map(|c| c.wake).min()
    }

    /// Wakes the stream clients that are due.
    pub(super) fn stream_clients_step(&mut self) -> Result<(), SimError> {
        for i in 0..self.stream_clients.len() {
            if self.stream_clients.get(i).is_some_and(|c| c.wake <= self.now) {
                if let Some(c) = self.stream_clients.get_mut(i) {
                    c.wake = i64::MAX;
                }
                self.client_event(i, StreamEvent::Wake)?;
            }
        }
        Ok(())
    }

    fn client_event(&mut self, i: usize, e: StreamEvent<'_>) -> Result<(), SimError> {
        if self.run.violation.is_some() {
            return Ok(());
        }
        let now = self.now - super::EPOCH;
        let slot = self
            .stream_clients
            .get_mut(i)
            .ok_or_else(|| internal_error!("no stream client {i}"))?;
        let action = match slot.client.on(now, e) {
            Ok(a) => a,
            Err(v) => {
                let v = format!("{now}: stream client {i}: {v}");
                self.run.log.push(format!("violation: {v}"));
                self.run.violation = Some(v);
                return Ok(());
            }
        };
        if let Some(w) = action.wake {
            slot.wake = super::EPOCH.saturating_add(w).max(self.now + 1);
        }
        match (slot.pipe, action.connect) {
            (None, Some((node, stream))) => self.client_connect(i, node, &stream)?,
            (Some(_), Some(_)) => return Err(internal_error!("stream client {i} connects while connected").into()),
            _ => {}
        }
        let pipe = self.stream_clients.get(i).and_then(|c| c.pipe);
        match pipe {
            Some(p) => {
                if !action.send.is_empty() {
                    self.send_bytes(p, 0, action.send);
                }
                if action.close {
                    self.half_close(p, 0);
                }
            }
            None if !action.send.is_empty() || action.close => {
                return Err(internal_error!("stream client {i} sends without a connection").into());
            }
            None => {}
        }
        Ok(())
    }

    /// The index of `node`'s stream named `name`, if the node is up, runs it and it listens.
    fn listen_stream(&self, node: NodeId, name: &str) -> Option<usize> {
        let d = self.nodes.get(node.0 as usize)?.driver.as_ref()?;
        d.node
            .streams()
            .iter()
            .position(|s| &*s.name == name && s.kind == blossom_ir::core::StreamKind::Listen)
    }

    fn client_connect(&mut self, i: usize, node: NodeId, stream: &str) -> Result<(), SimError> {
        let p = self.pipes.len();
        let target = self.listen_stream(node, stream);
        let far = match target {
            Some(s) => End::Pending { node, stream: s },
            None => End::Nowhere,
        };
        self.pipes.push(Pipe::new([End::Client(i), far], self.now, None));
        if let Some(c) = self.stream_clients.get_mut(i) {
            c.pipe = Some(p);
        }
        if target.is_none() {
            // Nothing listens there (the node is down, or runs no such stream): refused, after the way there and
            // back, as a TCP reset comes. Bytes the client sends meanwhile go nowhere.
            if let Some(x) = self.pipes.get_mut(p) {
                x.told[1] = true;
                x.recv[1] = false;
            }
            let why = format!("node {} does not accept on `{stream}`", node.0);
            self.towards(
                p,
                0,
                Envelope::PipeClosed {
                    pipe: p,
                    to: 0,
                    reason: Arc::from(why),
                },
            );
            return Ok(());
        }
        // Ordered towards the accepting end, so bytes and a close the client sends after it arrive after it.
        self.towards(p, 1, Envelope::PipeConnect { pipe: p });
        Ok(())
    }

    /// A dial by a node's connect stream: `sim://NODE/STREAM` names a node of the deployment and its listen stream.
    fn node_dial(&mut self, from: NodeId, stream: usize, req: u64, addr: &str) -> Result<(), SimError> {
        let target = addr.strip_prefix("sim://").and_then(|rest| rest.split_once('/'));
        let found = target.and_then(|(name, s)| {
            let n = self.names.iter().position(|x| &**x == name)?;
            let n = node_id(n).ok()?;
            Some((n, self.listen_stream(n, s)?))
        });
        let restarts = self.nodes.get(from.0 as usize).map_or(0, |s| s.restarts);
        // A dial that cannot start fails after a delay, as an attempt does: reported at once, a node that dials again
        // on failure would dial forever at one instant.
        let (to, s) = match found {
            Some((to, _)) if self.blocked.contains(&(from, to)) || self.blocked.contains(&(to, from)) => {
                return self.dial_fails(from, restarts, stream, req, format!("`{addr}` is unreachable (partitioned)"));
            }
            Some(x) => x,
            None => return self.dial_fails(from, restarts, stream, req, format!("cannot reach `{addr}`")),
        };
        let conn = self.allocate_conn(from)?;
        let p = self.pipes.len();
        self.pipes.push(Pipe::new(
            [
                End::Node {
                    node: from,
                    restarts,
                    conn,
                },
                End::Pending { node: to, stream: s },
            ],
            self.now,
            Some((stream, req)),
        ));
        self.node_ends.insert((from, conn), (p, 0));
        self.towards(p, 1, Envelope::PipeConnect { pipe: p });
        Ok(())
    }

    /// Reports a dial that cannot start failed, after a delay.
    fn dial_fails(&mut self, node: NodeId, restarts: u64, stream: usize, req: u64, why: String) -> Result<(), SimError> {
        let delay = self.rng.range(self.cfg.latency.0, self.cfg.latency.1).max(1);
        self.schedule(
            delay,
            Envelope::DialFailed {
                node,
                restarts,
                stream,
                req,
                reason: Arc::from(why),
            },
        );
        Ok(())
    }

    /// A dial failure reported later reaches the node only in the incarnation that dialed.
    pub(super) fn dial_failed_later(
        &mut self,
        node: NodeId,
        restarts: u64,
        stream: usize,
        req: u64,
        why: &str,
    ) -> Result<(), SimError> {
        if !self.end_alive(node, restarts) {
            return Ok(());
        }
        self.stream_dial_failed(node, stream, req, why.to_owned())
    }

    fn stream_dial_failed(&mut self, node: NodeId, stream: usize, req: u64, why: String) -> Result<(), SimError> {
        self.observe_at(
            node,
            Observed::Failed {
                stream,
                req,
                reason: Arc::from(why),
            },
        )
    }

    /// Tells a node what its host observed on a stream (nothing if the node is down).
    fn observe_at(&mut self, node: NodeId, o: Observed) -> Result<(), SimError> {
        if let Some(d) = self.nodes.get_mut(node.0 as usize).and_then(|s| s.driver.as_mut()) {
            d.node
                .observe_stream(o)
                .map_err(|e| SimError::Internal(internal_error!("node {}: {e}", node.0)))?;
        }
        Ok(())
    }

    fn allocate_conn(&mut self, n: NodeId) -> Result<ConnId, SimError> {
        let slot = self
            .nodes
            .get_mut(n.0 as usize)
            .ok_or_else(|| internal_error!("no node {}", n.0))?;
        let c = ConnId(slot.restarts << 32 | slot.next_conn);
        slot.next_conn += 1;
        Ok(c)
    }

    fn node_now(&self, n: NodeId) -> Instant {
        let offset = self.nodes.get(n.0 as usize).map_or(0, |s| s.offset);
        Instant(self.now.saturating_add(offset))
    }

    fn end_alive(&self, node: NodeId, restarts: u64) -> bool {
        self.nodes
            .get(node.0 as usize)
            .is_some_and(|s| s.driver.is_some() && s.restarts == restarts)
    }

    /// A connection attempt arrives at its target node.
    pub(super) fn pipe_connect(&mut self, p: usize) -> Result<(), SimError> {
        let Some(pipe) = self.pipes.get(p) else {
            return Err(internal_error!("no pipe {p}").into());
        };
        let End::Pending { node, stream } = pipe.ends[1].clone() else {
            return Err(internal_error!("pipe {p} connects twice").into());
        };
        if pipe.told[0] {
            return Ok(());
        }
        if self.listen_stream_index_ok(node, stream).is_none() {
            return self.refuse(p, format!("node {} is down", node.0));
        }
        if let End::Node { node: from, .. } = pipe.ends[0]
            && self.partitioned(from, node)
        {
            return self.refuse(p, format!("node {} is unreachable (partitioned)", node.0));
        }
        let restarts = self.nodes.get(node.0 as usize).map_or(0, |s| s.restarts);
        let conn = self.allocate_conn(node)?;
        if let Some(pipe) = self.pipes.get_mut(p) {
            pipe.ends[1] = End::Node { node, restarts, conn };
            pipe.accepted = Some(stream);
        }
        self.node_ends.insert((node, conn), (p, 1));
        let at = self.node_now(node);
        self.observe_at(
            node,
            Observed::Opened {
                stream,
                conn,
                peer: Arc::from(format!("sim-pipe-{p}")),
                req: None,
                at,
            },
        )?;
        self.run.stream_connections += 1;
        // The connecting end learns it is open after the way back, before anything the accepting end sends.
        self.towards(p, 0, Envelope::PipeOpened { pipe: p });
        Ok(())
    }

    fn partitioned(&self, a: NodeId, b: NodeId) -> bool {
        self.blocked.contains(&(a, b)) || self.blocked.contains(&(b, a))
    }

    /// Whether `node` is up (its stream `stream` then exists: stream indexes are the program's).
    fn listen_stream_index_ok(&self, node: NodeId, stream: usize) -> Option<()> {
        let d = self.nodes.get(node.0 as usize)?.driver.as_ref()?;
        d.node.streams().get(stream).map(|_| ())
    }

    /// The connecting end learns the connection is established.
    pub(super) fn pipe_opened(&mut self, p: usize) -> Result<(), SimError> {
        let Some(pipe) = self.pipes.get(p) else {
            return Err(internal_error!("no pipe {p}").into());
        };
        if pipe.told[0] {
            return Ok(());
        }
        let dial = pipe.dial;
        if let Some(x) = self.pipes.get_mut(p) {
            x.opened = true;
        }
        let Some(pipe) = self.pipes.get(p) else {
            return Err(internal_error!("no pipe {p}").into());
        };
        match pipe.ends[0].clone() {
            End::Client(i) => self.client_event(i, StreamEvent::Opened),
            End::Node { node, restarts, conn } => {
                if !self.end_alive(node, restarts) {
                    return Ok(());
                }
                let (stream, req) = dial.ok_or_else(|| internal_error!("a node's pipe {p} that it did not dial"))?;
                let at = self.node_now(node);
                self.observe_at(
                    node,
                    Observed::Opened {
                        stream,
                        conn,
                        peer: Arc::from(format!("sim-pipe-{p}")),
                        req: Some(req),
                        at,
                    },
                )
            }
            End::Pending { .. } | End::Nowhere => {
                Err(internal_error!("pipe {p}'s connecting end is not a client or a node").into())
            }
        }
    }

    /// A connection that could not be established: the connecting end learns at once.
    fn refuse(&mut self, p: usize, why: String) -> Result<(), SimError> {
        let Some(pipe) = self.pipes.get_mut(p) else {
            return Ok(());
        };
        pipe.told = [true, true];
        pipe.recv = [false, false];
        let (end, dial) = (pipe.ends[0].clone(), pipe.dial);
        match end {
            End::Client(i) => {
                if let Some(c) = self.stream_clients.get_mut(i) {
                    c.pipe = None;
                }
                self.client_event(i, StreamEvent::Closed(&why))
            }
            End::Node { node, restarts, conn } => {
                self.node_ends.remove(&(node, conn));
                match dial {
                    Some((stream, req)) if self.end_alive(node, restarts) => {
                        self.stream_dial_failed(node, stream, req, why)
                    }
                    _ => Ok(()),
                }
            }
            End::Pending { .. } | End::Nowhere => Ok(()),
        }
    }

    /// Schedules a delivery towards end `to` of pipe `p`, after a random latency and after the one ahead of it.
    fn towards(&mut self, p: usize, to: usize, e: Envelope) {
        let latency = self.rng.range(self.cfg.latency.0, self.cfg.latency.1);
        let Some(pipe) = self.pipes.get_mut(p) else {
            return;
        };
        let at = (self.now + latency.max(1)).max(*side(&pipe.due, to) + 1);
        *side_mut(&mut pipe.due, to) = at;
        self.schedule_at(at, e);
    }

    /// Sends `bytes` from end `from` of pipe `p`, split at random byte boundaries.
    fn send_bytes(&mut self, p: usize, from: usize, bytes: Vec<u8>) {
        let to = 1 - from;
        let mut rest = bytes.as_slice();
        while !rest.is_empty() {
            let max = self.cfg.chunk_max.max(1) as u64;
            let n = (1 + self.rng.below(max) as usize).min(rest.len());
            let (chunk, tail) = rest.split_at(n);
            rest = tail;
            self.run.stream_bytes += chunk.len() as u64;
            self.towards(
                p,
                to,
                Envelope::PipeBytes {
                    pipe: p,
                    to,
                    bytes: chunk.to_vec(),
                },
            );
        }
    }

    /// Bytes arrive at end `to` of pipe `p`.
    pub(super) fn pipe_bytes(&mut self, p: usize, to: usize, bytes: Vec<u8>) -> Result<(), SimError> {
        let Some(pipe) = self.pipes.get(p) else {
            return Err(internal_error!("no pipe {p}").into());
        };
        if !*side(&pipe.recv, to) || *side(&pipe.told, to) {
            return Ok(());
        }
        if let (End::Node { node: a, .. }, End::Node { node: b, .. }) = (&pipe.ends[0], &pipe.ends[1])
            && self.partitioned(*a, *b)
        {
            return self.reset(p, "partitioned");
        }
        if *side(&pipe.paused, to) {
            if let Some(x) = self.pipes.get_mut(p) {
                side_mut(&mut x.held, to).push_back(Held::Bytes(bytes));
            }
            self.run.stream_held += 1;
            return Ok(());
        }
        match side(&pipe.ends, to).clone() {
            End::Client(i) => self.client_event(i, StreamEvent::Received(&bytes)),
            End::Node { node, restarts, conn } => {
                if !self.end_alive(node, restarts) {
                    return Ok(());
                }
                self.observe_at(node, Observed::Bytes { conn, bytes })
            }
            End::Pending { .. } | End::Nowhere => {
                Err(internal_error!("bytes to an end of pipe {p} that is not connected").into())
            }
        }
    }

    /// End `from` sends no more (a client's half-close): the other end learns after the bytes in flight.
    fn half_close(&mut self, p: usize, from: usize) {
        let to = 1 - from;
        if self.pipes.get(p).is_none_or(|x| *side(&x.told, to)) {
            return;
        }
        self.towards(
            p,
            to,
            Envelope::PipeClosed {
                pipe: p,
                to,
                reason: Arc::from("closed by the peer"),
            },
        );
    }

    /// End `to` of pipe `p` learns the connection closed (after what a pause holds, unless its own host closed it).
    pub(super) fn pipe_closed(&mut self, p: usize, to: usize, reason: &str) -> Result<(), SimError> {
        let Some(pipe) = self.pipes.get_mut(p) else {
            return Err(internal_error!("no pipe {p}").into());
        };
        if *side(&pipe.told, to) {
            return Ok(());
        }
        if *side(&pipe.paused, to) && *side(&pipe.recv, to) {
            side_mut(&mut pipe.held, to).push_back(Held::Closed(Arc::from(reason)));
            self.run.stream_held += 1;
            return Ok(());
        }
        *side_mut(&mut pipe.told, to) = true;
        *side_mut(&mut pipe.recv, to) = false;
        let end = side(&pipe.ends, to).clone();
        self.tell_closed(&end, reason)
    }

    fn tell_closed(&mut self, end: &End, reason: &str) -> Result<(), SimError> {
        match end {
            End::Client(i) => {
                if let Some(c) = self.stream_clients.get_mut(*i) {
                    c.pipe = None;
                }
                self.client_event(*i, StreamEvent::Closed(reason))
            }
            End::Node { node, restarts, conn } if self.end_alive(*node, *restarts) => self.observe_at(
                *node,
                Observed::Closed {
                    conn: *conn,
                    reason: Arc::from(reason),
                },
            ),
            End::Node { .. } | End::Pending { .. } | End::Nowhere => Ok(()),
        }
    }

    /// Resets pipe `p`: both ends learn at once and nothing in flight arrives.
    fn reset(&mut self, p: usize, why: &str) -> Result<(), SimError> {
        let Some(pipe) = self.pipes.get_mut(p) else {
            return Ok(());
        };
        let told = pipe.told;
        let paused = pipe.paused;
        pipe.told = [true, true];
        pipe.recv = [false, false];
        let (ends, opened, dial) = (pipe.ends.clone(), pipe.opened, pipe.dial);
        self.run.stream_resets += 1;
        for (i, (end, was_told)) in ends.iter().zip(told).enumerate() {
            if was_told {
                continue;
            }
            // A paused reader learns of the reset when it reads again, and the bytes it did not read are gone.
            if let End::Node { node, restarts, .. } = end
                && *side(&paused, i)
                && self.end_alive(*node, *restarts)
            {
                if let Some(x) = self.pipes.get_mut(p) {
                    *side_mut(&mut x.told, i) = false;
                    let held = side_mut(&mut x.held, i);
                    held.clear();
                    held.push_back(Held::Closed(Arc::from(why)));
                }
                continue;
            }
            match end {
                // The connecting node was never told the connection opened: to it, the dial failed.
                End::Node { node, restarts, conn } if i == 0 && !opened => {
                    self.node_ends.remove(&(*node, *conn));
                    if let Some((stream, req)) = dial
                        && self.end_alive(*node, *restarts)
                    {
                        self.stream_dial_failed(*node, stream, req, why.to_owned())?;
                    }
                }
                _ => self.tell_closed(end, why)?,
            }
        }
        Ok(())
    }

    /// A node's released tick: its stream writes (in connection and `seq` order), its pauses, its resumes, its closes,
    /// the connections whose `closed` it delivered, then its dials.
    pub(super) fn stream_released(
        &mut self,
        n: NodeId,
        host: &[blossom_ir::tick::HostOut],
        retired: &[ConnId],
    ) -> Result<(), SimError> {
        if host.is_empty() && retired.is_empty() {
            return Ok(());
        }
        let requests: Vec<HostRequest> = match self.nodes.get(n.0 as usize).and_then(|s| s.driver.as_ref()) {
            Some(d) => {
                let blobs = d.node.blobs();
                host.iter()
                    .map(|h| host_request(d.node.streams(), h, &blobs))
                    .collect::<Result<_, _>>()
                    .map_err(|e| SimError::Internal(internal_error!("node {}: {e}", n.0)))?
            }
            None => return Ok(()),
        };
        let mut writes = Vec::new();
        let mut closes = Vec::new();
        let mut pauses = Vec::new();
        let mut dials = Vec::new();
        let mut refused = Vec::new();
        for r in requests {
            match r {
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
        // A refused write is a located runtime error: the connection closes with it, as for a bad `seq`.
        for (stream, conn, why) in refused {
            let Some(&(p, end)) = self.node_ends.get(&(n, conn)) else {
                continue;
            };
            if self.through_its_stream(n, p, end, stream, conn, "write") {
                self.run.stream_violations += 1;
                self.run.log.push(format!(
                    "{}: node {} stream violation: {why}",
                    self.now - super::EPOCH,
                    n.0
                ));
                self.host_close(p, end, &why);
            }
        }
        writes.sort_by_key(|w| (w.0, w.1));
        for (conn, seq, stream, bytes) in writes {
            let Some(&(p, end)) = self.node_ends.get(&(n, conn)) else {
                self.run.dropped += 1;
                continue;
            };
            if !self.through_its_stream(n, p, end, stream, conn, "write") {
                continue;
            }
            let accepted = match self.pipes.get_mut(p) {
                Some(x) if x.live() => side_mut(&mut x.writers, end).accept(seq, bytes),
                _ => {
                    self.run.dropped += 1;
                    continue;
                }
            };
            match accepted {
                Ok(ready) => {
                    for b in ready {
                        self.send_bytes(p, end, b);
                    }
                }
                Err(why) => {
                    // A located runtime error, as in the runtime: the connection closes (the peer learns after the
                    // bytes already sent) and the program's `closed` event carries the error.
                    self.run.stream_violations += 1;
                    self.run.log.push(format!(
                        "{}: node {} stream violation: {why}",
                        self.now - super::EPOCH,
                        n.0
                    ));
                    self.host_close(p, end, &why);
                }
            }
        }
        // Pauses before resumes, as the runtime applies them: a tick that asks both reads the connection.
        pauses.sort();
        for (resume, conn, stream) in pauses {
            let Some(&(p, end)) = self.node_ends.get(&(n, conn)) else {
                continue;
            };
            let what = if resume { "resume" } else { "pause" };
            if self.through_its_stream(n, p, end, stream, conn, what) {
                self.pipe_pause(p, end, !resume)?;
            }
        }
        // A program's close: the other end learns after the bytes sent, and the node end gets its own `closed`.
        for (stream, conn) in closes {
            let Some(&(p, end)) = self.node_ends.get(&(n, conn)) else {
                continue;
            };
            if self.through_its_stream(n, p, end, stream, conn, "close") {
                self.host_close(p, end, "closed by the program");
            }
        }
        // The runtime closes a connection once the tick that delivered its `closed` is released.
        for conn in retired {
            if let Some((p, end)) = self.node_ends.remove(&(n, *conn)) {
                self.half_close(p, end);
            }
        }
        for (stream, req, addr) in dials {
            self.node_dial(n, stream, req, &addr)?;
        }
        Ok(())
    }

    /// Pauses node end `end` of pipe `p`, or resumes it: a resume delivers, in order, what arrived while it was paused.
    fn pipe_pause(&mut self, p: usize, end: usize, paused: bool) -> Result<(), SimError> {
        let held: Vec<Held> = match self.pipes.get_mut(p) {
            Some(x) => {
                *side_mut(&mut x.paused, end) = paused;
                if paused {
                    return Ok(());
                }
                side_mut(&mut x.held, end).drain(..).collect()
            }
            None => return Ok(()),
        };
        for h in held {
            match h {
                Held::Bytes(b) => self.pipe_bytes(p, end, b)?,
                Held::Closed(why) => self.pipe_closed(p, end, &why)?,
            }
        }
        Ok(())
    }

    /// Whether a request through `stream` names a connection of that stream; otherwise the request is refused, as a
    /// located runtime error the run counts and logs (FOREIGN-PROTOCOLS §1.2).
    fn through_its_stream(&mut self, n: NodeId, p: usize, end: usize, stream: usize, conn: ConnId, what: &str) -> bool {
        let belongs = self.pipes.get(p).and_then(|x| x.stream_of(end));
        if belongs == Some(stream) {
            return true;
        }
        self.run.stream_violations += 1;
        self.run.log.push(format!(
            "{}: node {} stream violation: a {what} through stream {stream} to connection {} of stream {belongs:?}",
            self.now - super::EPOCH,
            n.0,
            conn.0
        ));
        false
    }

    /// The host closes node end `end` of pipe `p`: the other end learns after the bytes sent, the node end gets its
    /// own `closed` with `reason`, and nothing more reaches it.
    fn host_close(&mut self, p: usize, end: usize, reason: &str) {
        self.half_close(p, end);
        if let Some(x) = self.pipes.get_mut(p) {
            *side_mut(&mut x.recv, end) = false;
        }
        if self.pipes.get(p).is_some_and(|x| !*side(&x.told, end)) {
            let delay = self.rng.range(self.cfg.latency.0, self.cfg.latency.1);
            self.schedule(
                delay,
                Envelope::PipeClosed {
                    pipe: p,
                    to: end,
                    reason: Arc::from(reason),
                },
            );
        }
    }

    /// A node went down: every pipe with an end in that incarnation resets.
    pub(super) fn streams_node_down(&mut self, n: NodeId) -> Result<(), SimError> {
        let pipes: Vec<usize> = (0..self.pipes.len())
            .filter(|p| {
                self.pipes.get(*p).is_some_and(|x| {
                    x.live()
                        && x.ends
                            .iter()
                            .any(|e| matches!(e, End::Node { node, .. } | End::Pending { node, .. } if *node == n))
                })
            })
            .collect();
        for p in pipes {
            self.reset(p, "the node crashed")?;
        }
        self.node_ends.retain(|(node, _), _| *node != n);
        Ok(())
    }

    /// A partition resets the pipes between the nodes it separates.
    pub(super) fn streams_partitioned(&mut self) -> Result<(), SimError> {
        let pipes: Vec<usize> = (0..self.pipes.len())
            .filter(|p| {
                self.pipes.get(*p).is_some_and(|x| {
                    x.live()
                        && match (&x.ends[0], &x.ends[1]) {
                            (End::Node { node: a, .. }, End::Node { node: b, .. } | End::Pending { node: b, .. }) => {
                                self.partitioned(*a, *b)
                            }
                            _ => false,
                        }
                })
            })
            .collect();
        for p in pipes {
            self.reset(p, "partitioned")?;
        }
        Ok(())
    }

    /// The nemesis resets a random live connection.
    pub(super) fn stream_drop(&mut self) -> Result<(), SimError> {
        let live: Vec<usize> = (0..self.pipes.len())
            .filter(|p| self.pipes.get(*p).is_some_and(|x| !x.told[0] && !x.told[1]))
            .collect();
        if live.is_empty() {
            return Ok(());
        }
        let pick = self.rng.below(live.len() as u64) as usize;
        if let Some(&p) = live.get(pick) {
            self.run
                .log
                .push(format!("{}: reset stream connection {p}", self.now - super::EPOCH));
            self.reset(p, "connection reset")?;
        }
        Ok(())
    }
}
