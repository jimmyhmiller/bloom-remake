//! A client session (LANGUAGE §18.4): connects to a node's client listener, sends messages on the program's
//! external-ingress channels, and receives the node's replies.
//!
//! The client links the same compiled program as the node, so it encodes and decodes with the same codec; the
//! handshake checks that both ends run the same deployment and program and matches channels by schema hash.

use std::collections::VecDeque;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::RelId;
use blossom_oracle::Row;
use blossom_value::Value;
use blossom_value::time::NodeId;
use blossom_wire::frame::{Frame, Peer};

use crate::RuntimeError;
use crate::net::{self, Catalog, Conn, Identity, PeerHello};

pub struct Client {
    artifact: Arc<BlsArtifact>,
    conn: Conn,
    catalog: Catalog,
    hello: PeerHello,
    server: NodeId,
    received: VecDeque<(RelId, Row)>,
}

impl Client {
    /// Opens a session to the node at `addr` as `principal`.
    pub fn connect(
        addr: SocketAddr,
        artifact: Arc<BlsArtifact>,
        id: &Identity,
        principal: &str,
        timeout: Duration,
    ) -> Result<Client, RuntimeError> {
        let stream = TcpStream::connect_timeout(&addr, timeout).map_err(RuntimeError::Io)?;
        stream.set_read_timeout(Some(timeout)).map_err(RuntimeError::Io)?;
        let mut conn = Conn::new(stream)?;
        let catalog = Catalog::of(artifact.program.get())?;
        let hello = net::open_handshake(
            &mut conn,
            id,
            Peer::Client {
                principal: principal.into(),
            },
            0,
            0,
            &catalog,
        )?;
        let Peer::Node(server) = hello.peer else {
            return Err(RuntimeError::Net("the server did not identify as a node".into()));
        };
        conn.reader.get_ref().set_read_timeout(None).map_err(RuntimeError::Io)?;
        Ok(Client {
            artifact,
            conn,
            catalog,
            hello,
            server: NodeId(server),
            received: VecDeque::new(),
        })
    }

    /// The node this session is connected to.
    pub fn server(&self) -> NodeId {
        self.server
    }

    pub fn rel(&self, name: &str) -> Result<RelId, RuntimeError> {
        self.artifact
            .rel_named(name)
            .ok_or_else(|| RuntimeError::Config(format!("the program has no channel `{name}`")))
    }

    /// Sends rows on channel `rel`; each row holds the channel's columns after the destination.
    pub fn send(&mut self, rel: RelId, rows: &[Vec<Value>]) -> Result<(), RuntimeError> {
        let sid = self
            .catalog
            .sid(rel)
            .ok_or_else(|| RuntimeError::Config(format!("{rel:?} is not a channel")))?;
        let full: Vec<Row> = rows
            .iter()
            .map(|r| {
                let mut v = Vec::with_capacity(r.len() + 1);
                v.push(Value::Node(self.server));
                v.extend(r.iter().cloned());
                Row::from(v)
            })
            .collect();
        let refs: Vec<&Row> = full.iter().collect();
        let program = self.artifact.program.get();
        let bytes = net::batch_frame(&net::wire_codec(program), program, sid, rel, 0, &refs)?;
        use std::io::Write;
        self.conn.writer.write_all(&bytes).map_err(RuntimeError::Io)?;
        self.conn.writer.flush().map_err(RuntimeError::Io)
    }

    /// Waits up to `timeout` for the next reply (channel, full row with the session in column 0). `Ok(None)` on
    /// timeout; an error when the connection closed.
    pub fn recv(&mut self, timeout: Option<Duration>) -> Result<Option<(RelId, Row)>, RuntimeError> {
        if let Some(r) = self.received.pop_front() {
            return Ok(Some(r));
        }
        self.conn.reader.get_ref().set_read_timeout(timeout).map_err(RuntimeError::Io)?;
        loop {
            let frame = match Frame::read(&mut self.conn.reader, &self.conn.limits) {
                Ok(Some(f)) => f,
                Ok(None) => return Err(RuntimeError::Net("the server closed the session".into())),
                Err(blossom_wire::frame::FrameIoError::Io(e))
                    if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
                {
                    return Ok(None);
                }
                Err(e) => return Err(RuntimeError::Net(e.to_string())),
            };
            let Frame::Batch(b) = frame else {
                continue;
            };
            let Some(&rel) = self.hello.inbound.get(&b.sid) else {
                continue;
            };
            let program = self.artifact.program.get();
            for row in net::batch_rows(&net::wire_codec(program), program, rel, &b)? {
                self.received.push_back((rel, row));
            }
            if let Some(r) = self.received.pop_front() {
                return Ok(Some(r));
            }
        }
    }
}
