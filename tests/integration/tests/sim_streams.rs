//! Slice 6: byte streams in the cluster simulator (FOREIGN-PROTOCOLS §1.4). Rust stream clients talk to Blossom
//! servers over simulated pipes that split bytes at random boundaries, under node crashes (some between a WAL append
//! and its sync) and connection resets from the nemesis. Every byte a client receives must be what the protocol
//! defines for what it sent, and a session the client half-closes must end with every reply delivered before the
//! server's close reaches it, unless a crash or a reset cut it.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::durable::DurableSchema;
use blossom_sim::cluster::{Cluster, ClusterConfig, NoKvClients, StreamAction, StreamClient, StreamEvent};
use blossom_value::time::NodeId;

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
fn compile(file: &str) -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/streams")
        .join(file);
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    result.unwrap_or_else(|e| panic!("{file}: {e:?}")).0
}

/// What the clients saw, over every session.
#[cfg(test)]
#[derive(Default, Debug)]
struct Tally {
    /// Sessions whose every reply arrived.
    complete: u64,
    /// Sessions cut by a crash or a reset.
    cut: u64,
    /// Connection attempts refused (the node was down).
    refused: u64,
}

/// A protocol a client speaks: a session's request bytes, and the replies they must produce.
#[cfg(test)]
trait Protocol {
    fn session(&self, rng: &mut Rng) -> (Vec<u8>, Vec<u8>);
    /// The longest prefix of the expected replies that the bytes sent so far can have produced.
    fn owed(&self, sent: &[u8], expected: &[u8]) -> usize;
}

/// The line echo: every complete line comes back.
#[cfg(test)]
struct Echo;

#[cfg(test)]
impl Protocol for Echo {
    fn session(&self, rng: &mut Rng) -> (Vec<u8>, Vec<u8>) {
        let mut text = Vec::new();
        for _ in 0..1 + rng.below(8) {
            for _ in 0..rng.below(20) {
                text.push(b'a' + rng.below(26) as u8);
            }
            text.push(b'\n');
        }
        let reply = text.clone();
        // A trailing partial line is never echoed.
        text.extend(std::iter::repeat_n(b'z', rng.below(4) as usize));
        (text, reply)
    }
    fn owed(&self, sent: &[u8], _expected: &[u8]) -> usize {
        sent.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1)
    }
}

/// The framing server: each length-prefixed frame comes back reversed.
#[cfg(test)]
struct Frames;

#[cfg(test)]
impl Protocol for Frames {
    fn session(&self, rng: &mut Rng) -> (Vec<u8>, Vec<u8>) {
        let mut req = Vec::new();
        let mut reply = Vec::new();
        for _ in 0..1 + rng.below(6) {
            let payload: Vec<u8> = (0..rng.below(30)).map(|_| rng.next() as u8).collect();
            req.extend((payload.len() as u32).to_be_bytes());
            req.extend(&payload);
            reply.extend((payload.len() as u32).to_be_bytes());
            reply.extend(payload.iter().rev());
        }
        (req, reply)
    }
    fn owed(&self, sent: &[u8], _expected: &[u8]) -> usize {
        // Each complete frame sent owes its reply, which is the same length.
        let mut pos = 0;
        while let Some(len) = sent
            .get(pos..pos + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
        {
            if pos + 4 + len > sent.len() {
                break;
            }
            pos += 4 + len;
        }
        pos
    }
}

#[cfg(test)]
enum State {
    Idle,
    Connecting,
    Open,
    /// It sent everything and half-closed: it waits for the rest of the replies and the server's close.
    Done,
}

#[cfg(test)]
struct Client<P: Protocol> {
    protocol: P,
    stream: Arc<str>,
    rng: Rng,
    state: State,
    request: Vec<u8>,
    expected: Vec<u8>,
    sent: usize,
    got: Vec<u8>,
    tally: Rc<RefCell<Tally>>,
}

#[cfg(test)]
impl<P: Protocol> Client<P> {
    fn new(protocol: P, stream: &str, seed: u64, tally: Rc<RefCell<Tally>>) -> Client<P> {
        Client {
            protocol,
            stream: Arc::from(stream),
            rng: Rng(seed),
            state: State::Idle,
            request: Vec::new(),
            expected: Vec::new(),
            sent: 0,
            got: Vec::new(),
            tally,
        }
    }
}

#[cfg(test)]
impl<P: Protocol> StreamClient for Client<P> {
    fn on(&mut self, now: i64, e: StreamEvent<'_>) -> Result<StreamAction, String> {
        let soon = now + 1 + self.rng.below(3_000_000) as i64;
        let mut a = StreamAction::default();
        match (e, &self.state) {
            (StreamEvent::Wake, State::Idle) => {
                (self.request, self.expected) = self.protocol.session(&mut self.rng);
                self.sent = 0;
                self.got.clear();
                self.state = State::Connecting;
                a.connect = Some((NodeId(0), self.stream.clone()));
            }
            (StreamEvent::Opened, State::Connecting) => {
                self.state = State::Open;
                a.wake = Some(soon);
            }
            (StreamEvent::Wake, State::Open) => {
                // Send the next piece of the request; once all of it is out, half-close.
                let n = (1 + self.rng.below(12) as usize).min(self.request.len() - self.sent);
                a.send = self.request[self.sent..self.sent + n].to_vec();
                self.sent += n;
                if self.sent == self.request.len() {
                    a.close = true;
                    self.state = State::Done;
                } else {
                    a.wake = Some(soon);
                }
            }
            (StreamEvent::Received(b), State::Open | State::Done) => {
                self.got.extend_from_slice(b);
                let owed = self.protocol.owed(&self.request[..self.sent], &self.expected);
                if self.got.len() > owed || self.expected.get(..self.got.len()) != Some(self.got.as_slice()) {
                    return Err(format!(
                        "received {} bytes that are not the replies owed for {} sent",
                        self.got.len(),
                        self.sent
                    ));
                }
            }
            (StreamEvent::Closed(why), _) => {
                let cut = why.contains("reset") || why.contains("crashed");
                match &self.state {
                    State::Connecting if !cut => self.tally.borrow_mut().refused += 1,
                    State::Done if !cut => {
                        // The server closed after our half-close: every reply was written before it.
                        if self.got != self.expected {
                            return Err(format!(
                                "closed ({why}) with {} of {} reply bytes",
                                self.got.len(),
                                self.expected.len()
                            ));
                        }
                        self.tally.borrow_mut().complete += 1;
                    }
                    _ if cut => self.tally.borrow_mut().cut += 1,
                    _ => return Err(format!("closed ({why}) in the middle of a session")),
                }
                self.state = State::Idle;
                a.wake = Some(soon);
            }
            (e, _) => return Err(format!("unexpected {e:?}")),
        }
        Ok(a)
    }
}

#[cfg(test)]
fn run<P: Protocol + Clone + 'static>(file: &str, stream: &str, protocol: P, faults: bool) -> Tally {
    let artifact = compile(file);
    let schema = DurableSchema::of(artifact.program.get());
    let mut total = Tally::default();
    for seed in 1..=6u64 {
        let cfg = ClusterConfig {
            seed,
            clients: 0,
            latency: (100_000, 3_000_000),
            chunk_max: 1 + (seed as usize % 7),
            nemesis: if faults { 150_000_000 } else { 0 },
            crashes: faults,
            downtime: 50_000_000,
            stream_drops: faults,
            duration: 3_000_000_000,
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
        let tally = Rc::new(RefCell::new(Tally::default()));
        for c in 0..4 {
            cluster.stream_client(Box::new(Client::new(
                protocol.clone(),
                stream,
                seed * 100 + c,
                tally.clone(),
            )));
        }
        let r = cluster.run().unwrap();
        assert!(
            r.violation.is_none(),
            "seed {seed}: {:?}\n{}",
            r.violation,
            r.log.join("\n")
        );
        assert_eq!(r.stream_violations, 0, "seed {seed}");
        let t = tally.borrow();
        total.complete += t.complete;
        total.cut += t.cut;
        total.refused += t.refused;
    }
    total
}

#[cfg(test)]
impl Clone for Echo {
    fn clone(&self) -> Echo {
        Echo
    }
}

#[cfg(test)]
impl Clone for Frames {
    fn clone(&self) -> Frames {
        Frames
    }
}

#[test]
fn the_echo_server_under_random_chunking_answers_every_session() {
    let t = run("echo.bls", "echo", Echo, false);
    assert!(t.complete > 100, "{t:?}");
    assert_eq!((t.cut, t.refused), (0, 0), "{t:?}");
}

#[test]
fn the_echo_server_under_crashes_and_resets_never_sends_a_wrong_byte() {
    let t = run("echo.bls", "echo", Echo, true);
    assert!(t.complete > 50, "{t:?}");
    assert!(t.cut > 0, "the faults cut no session: {t:?}");
}

#[test]
fn the_framing_server_under_crashes_and_resets_answers_every_frame() {
    let t = run("frames.bls", "frames", Frames, true);
    assert!(t.complete > 50, "{t:?}");
    assert!(t.cut > 0, "the faults cut no session: {t:?}");
}

#[test]
fn a_node_dials_another_nodes_stream_in_the_simulator() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/streams/pair.bls");
    let nodes = [
        NodeSpec {
            name: "cli".to_owned(),
            role: Some("Client".to_owned()),
        },
        NodeSpec {
            name: "srv".to_owned(),
            role: Some("Server".to_owned()),
        },
    ];
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    let artifact = result.unwrap_or_else(|e| panic!("pair.bls: {e:?}")).0;
    let schema = DurableSchema::of(artifact.program.get());
    for seed in 1..=5u64 {
        let cfg = ClusterConfig {
            seed,
            clients: 0,
            chunk_max: 1 + seed as usize,
            duration: 1_000_000_000,
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
        cluster.step_until(500_000_000).unwrap();
        let cli = artifact.nodes.iter().position(|n| n.as_str() == "cli").unwrap();
        let state = cluster.state(NodeId(cli as u32)).unwrap();
        let got = artifact.rel_named("got").unwrap();
        let mut chunks: Vec<(u64, Vec<u8>)> = state
            .rows(got)
            .map(|r| match (&r[0], &r[1]) {
                (blossom_value::Value::Int(blossom_value::value::IntValue::U64(s)), blossom_value::Value::Bytes(b)) => {
                    (*s, b.to_vec())
                }
                other => panic!("got {other:?}"),
            })
            .collect();
        chunks.sort();
        let echoed: Vec<u8> = chunks.into_iter().flat_map(|(_, b)| b).collect();
        assert_eq!(echoed, b"ping\n", "seed {seed}");
        assert_eq!(
            state.rows(artifact.rel_named("failures").unwrap()).count(),
            0,
            "seed {seed}"
        );
    }
}
