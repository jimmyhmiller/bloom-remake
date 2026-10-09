//! The page's side of a client member's link (docs/design/CLIENTS.md §3, §5): the protocol state the page keeps
//! across reconnects and reloads.
//!
//! The page sends `HELLO` with its token (none the first time), the last batch it took from the server and the last of
//! its own the server acknowledged. The server answers with its `HELLO`, `HELLO_OK` and `WELCOME` (the member's
//! identity and seed, whether the link resumed, and the last of the page's batches it holds). Each direction numbers
//! its batches; the page acknowledges each batch it takes, drops one it took already, and keeps its own until the
//! server acknowledges them: sends made while the link is down wait in that queue (the offline queue) and go out after
//! the next `WELCOME`. [`Link::state`] is what the page stores to survive a reload.

use std::collections::{BTreeMap, VecDeque};

use blossom_artifact::bls::BlsArtifact;
use blossom_base::RelId;
use blossom_ir::tick::{Delivery, Send};
use blossom_value::time::NodeId;
use blossom_wire::frame::{Frame, Peer};
use blossom_wire::link::{Catalog, Identity, batch_rows, batches, check_hello, hello, wire_codec};
use serde::{Deserialize, Serialize};

use crate::HostError;

/// What the page stores of its link (hex for bytes), to resume it after a reload.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkState {
    /// The member's id, token and seed, once the server gave them.
    pub member: Option<u32>,
    pub token: String,
    pub seed: String,
    /// The last batch the page took from the server.
    pub received: u64,
    /// The last of the page's batches the server acknowledged, and the number of its next one.
    pub acked: u64,
    pub out_next: u64,
    /// The page's batches the server has not acknowledged: number and frame.
    pub unacked: Vec<(u64, String)>,
}

/// The member's identity as the server gave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub id: NodeId,
    pub token: Vec<u8>,
    pub seed: [u8; 16],
}

/// What a frame from the server meant for the page.
#[derive(Debug, PartialEq, Eq)]
pub enum Heard {
    /// The handshake finished: the member's identity, and whether the link took up where the last one left off.
    Welcome { member: Member, resumed: bool },
    /// Messages from the server, for the next round.
    Deliveries(Vec<Delivery>),
    /// Nothing for the program (the server's `HELLO`, an acknowledgement, a batch taken already).
    Nothing,
}

/// The page's link.
pub struct Link {
    artifact: BlsArtifact,
    role: String,
    /// The digest of the part of the program the page runs (CLIENTS.md §8).
    part: [u8; 16],
    server: NodeId,
    id: Identity,
    catalog: Catalog,
    /// The keyed member the link goes to, when its server hosts one (its role's name and its key).
    keyed: Option<(String, String)>,
    member: Option<Member>,
    received: u64,
    acked: u64,
    out_next: u64,
    unacked: VecDeque<(u64, Vec<u8>)>,
    /// The server's channel ids, from its `HELLO`.
    inbound: BTreeMap<u32, RelId>,
    /// Whether the handshake finished on the current connection, and whether that link resumed.
    up: bool,
    resumed: bool,
    /// Sends to another node than the server the page is connected to (dropped, CLIENTS.md §3).
    pub unroutable: u64,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>, HostError> {
    let bad = || HostError::Store(format!("`{s}` is not hex"));
    if !s.len().is_multiple_of(2) {
        return Err(bad());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            s.get(i..i + 2)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(bad)
        })
        .collect()
}

fn link_error(e: impl std::fmt::Display) -> HostError {
    HostError::Link(e.to_string())
}

impl Link {
    /// The link of a member of `role`, running the part `part` of the program, to the node `server`, resuming `state`
    /// when the page stored one.
    pub fn new(
        artifact: &BlsArtifact,
        role: &str,
        part: [u8; 16],
        server: NodeId,
        id: Identity,
        keyed: Option<(String, String)>,
        state: Option<&LinkState>,
    ) -> Result<Link, HostError> {
        let catalog = Catalog::of(artifact.program.get()).map_err(link_error)?;
        let mut link = Link {
            artifact: artifact.clone(),
            role: role.to_owned(),
            part,
            server,
            id,
            catalog,
            keyed,
            member: None,
            received: 0,
            acked: 0,
            out_next: 1,
            unacked: VecDeque::new(),
            inbound: BTreeMap::new(),
            up: false,
            resumed: false,
            unroutable: 0,
        };
        if let Some(s) = state {
            if let Some(m) = s.member {
                let seed: [u8; 16] = unhex(&s.seed)?
                    .try_into()
                    .map_err(|_| HostError::Store("a member seed is 16 bytes".into()))?;
                link.member = Some(Member {
                    id: NodeId(m),
                    token: unhex(&s.token)?,
                    seed,
                });
            }
            link.received = s.received;
            link.acked = s.acked;
            link.out_next = s.out_next.max(1);
            for (seq, f) in &s.unacked {
                link.unacked.push_back((*seq, unhex(f)?));
            }
        }
        Ok(link)
    }

    /// The member's identity, once known.
    pub fn member(&self) -> Option<&Member> {
        self.member.as_ref()
    }

    /// The server node the link goes to.
    pub fn server(&self) -> NodeId {
        self.server
    }

    /// Whether the handshake finished on the current connection.
    pub fn is_up(&self) -> bool {
        self.up
    }

    /// Whether the current link took up where the last one left off.
    pub fn resumed(&self) -> bool {
        self.resumed
    }

    /// The first frame of a connection.
    pub fn hello(&mut self) -> Vec<u8> {
        self.up = false;
        let peer = Peer::Member {
            role: self.role.clone(),
            part: self.part,
            token: self.member.as_ref().map(|m| m.token.clone()),
            received: self.received,
            acked: self.acked,
            keyed: self.keyed.clone(),
        };
        hello(&self.id, peer, 0, 0, &self.catalog).encode()
    }

    /// The connection ended: sends wait in the queue until the next `WELCOME`.
    pub fn down(&mut self) {
        self.up = false;
    }

    /// Takes a frame from the server: what it means, and the frames to send back.
    pub fn recv(&mut self, bytes: &[u8]) -> Result<(Heard, Vec<Vec<u8>>), HostError> {
        let frame = match Frame::parse(bytes, &Default::default()).map_err(link_error)? {
            Some((f, used)) if used == bytes.len() => f,
            _ => return Err(HostError::Link("a link message that is not exactly one frame".into())),
        };
        match frame {
            Frame::Hello(h) => {
                check_hello(&h, &self.id).map_err(|(r, d)| HostError::Refused {
                    reason: format!("{r:?}").to_lowercase(),
                    detail: d,
                })?;
                self.inbound = self.catalog.accept(&h.channels);
                Ok((Heard::Nothing, Vec::new()))
            }
            Frame::HelloOk { .. } => Ok((Heard::Nothing, Vec::new())),
            Frame::Reject { reason, detail } => Err(HostError::Refused {
                reason: format!("{reason:?}").to_lowercase(),
                detail,
            }),
            Frame::Welcome {
                member,
                token,
                resumed,
                floor,
                seed,
            } => {
                let member = Member {
                    id: NodeId(member),
                    token,
                    seed,
                };
                // A new identity (the server lost the old one): what the old one sent is gone with it.
                if self.member.as_ref().is_some_and(|m| m.id != member.id) {
                    self.unacked.clear();
                    self.received = 0;
                }
                self.member = Some(member.clone());
                self.up = true;
                self.resumed = resumed;
                self.drop_acked(floor);
                let resend = self.unacked.iter().map(|(_, f)| f.clone()).collect();
                Ok((Heard::Welcome { member, resumed }, resend))
            }
            Frame::Msg { seq, batch } => {
                let ack = vec![Frame::Ack { seq }.encode()];
                if seq <= self.received {
                    return Ok((Heard::Nothing, ack));
                }
                self.received = seq;
                let Some(rel) = self.inbound.get(&batch.sid).copied() else {
                    // A channel whose schema differs between the ends: dropped, and taken.
                    return Ok((Heard::Nothing, ack));
                };
                let p = self.artifact.program.get();
                let rows = batch_rows(&wire_codec(p), p, rel, &batch).map_err(link_error)?;
                let deliveries = rows
                    .into_iter()
                    .map(|row| Delivery {
                        rel,
                        from: self.server,
                        row,
                    })
                    .collect();
                Ok((Heard::Deliveries(deliveries), ack))
            }
            Frame::Ack { seq } => {
                self.drop_acked(seq);
                Ok((Heard::Nothing, Vec::new()))
            }
            other => Err(HostError::Link(format!("the server sent {other:?} on the link"))),
        }
    }

    fn drop_acked(&mut self, upto: u64) {
        self.acked = self.acked.max(upto);
        while self.unacked.front().is_some_and(|(s, _)| *s <= upto) {
            self.unacked.pop_front();
        }
    }

    /// Queues a round's sends to the server: the frames to write now (none while the link is down).
    pub fn send(&mut self, sends: &[Send], tick: u64) -> Result<Vec<Vec<u8>>, HostError> {
        let mut by_rel: BTreeMap<RelId, Vec<&blossom_ir::tick::Row>> = BTreeMap::new();
        for s in sends {
            if s.to == self.server {
                by_rel.entry(s.rel).or_default().push(&s.row);
            } else {
                self.unroutable += 1;
            }
        }
        let p = self.artifact.program.get();
        let codec = wire_codec(p);
        let mut out = Vec::new();
        for (rel, rows) in by_rel {
            let sid = self
                .catalog
                .sid(rel)
                .ok_or_else(|| HostError::Link(format!("{rel:?} is not a channel")))?;
            let (bs, _) = batches(&codec, p, sid, rel, tick, &rows).map_err(link_error)?;
            for b in bs {
                let seq = self.out_next;
                self.out_next += 1;
                let frame = Frame::Msg { seq, batch: b }.encode();
                if self.up {
                    out.push(frame.clone());
                }
                self.unacked.push_back((seq, frame));
            }
        }
        Ok(out)
    }

    /// What the page stores to resume the link.
    pub fn state(&self) -> LinkState {
        LinkState {
            member: self.member.as_ref().map(|m| m.id.0),
            token: self.member.as_ref().map(|m| hex(&m.token)).unwrap_or_default(),
            seed: self.member.as_ref().map(|m| hex(&m.seed)).unwrap_or_default(),
            received: self.received,
            acked: self.acked,
            out_next: self.out_next,
            unacked: self.unacked.iter().map(|(s, f)| (*s, hex(f))).collect(),
        }
    }
}
