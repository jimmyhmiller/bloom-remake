//! Slice 6: byte streams (FOREIGN-PROTOCOLS §1) in the evaluators. A Blossom line-echo server
//! (`fixtures/streams/echo.bls`) is fed scripted stream events — chunks split at arbitrary byte boundaries, one per
//! tick — on the oracle and on the engine, which must agree at every tick on the instance and on the requests to the
//! host; the writes must be exactly the complete lines, in order, with contiguous sequence numbers.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_ir::tick::HostOut;
use blossom_node::EngineEvaluator;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_sim::sync::SyncRun;
use blossom_value::Value;
use blossom_value::time::{Duration, Instant, NodeId, Tick};
use blossom_value::value::{ConnId, IntValue};

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
fn compile(name: &str) -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/streams")
        .join(name);
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    result.unwrap_or_else(|e| panic!("{name}: {e:?}")).0
}

/// Runs on the oracle and on the engine; both must agree on every tick's instance and host requests.
#[cfg(test)]
fn differential(artifact: &BlsArtifact, inputs: &[InputEvent], last: u64) -> SyncRun {
    let sim = BlsSim::new(artifact, blossom_value::Seed::from_u64(0)).unwrap();
    let round = Duration::from_nanos(1_000_000_000);
    let reference = sim
        .run(inputs, Tick(last), round, &FaultSchedule::default(), false)
        .unwrap();
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let mine = sim
        .run_on(&engine, inputs, Tick(last), round, &FaultSchedule::default(), false)
        .unwrap();
    for (t, (ra, rb)) in reference.rounds.iter().zip(&mine.rounds).enumerate() {
        for (x, y) in ra.iter().zip(rb) {
            assert_eq!(x.instance, y.instance, "tick {t}: instances differ");
            assert_eq!(x.host, y.host, "tick {t}: host requests differ");
        }
    }
    reference
}

#[cfg(test)]
struct Stream<'a> {
    artifact: &'a BlsArtifact,
    inputs: Vec<InputEvent>,
}

#[cfg(test)]
impl Stream<'_> {
    fn event(&mut self, tick: u64, rel: &str, row: Vec<Value>) {
        self.inputs.push(InputEvent {
            node: NodeId(0),
            tick: Tick(tick),
            rel: self.artifact.rel_named(rel).unwrap(),
            row: Arc::from(row),
        });
    }
    fn opened(&mut self, tick: u64, c: u64) {
        self.event(
            tick,
            "echo.opened",
            vec![conn(c), Value::Str("peer".into()), Value::Instant(Instant(0))],
        );
    }
    fn data(&mut self, tick: u64, c: u64, seq: u64, bytes: &[u8]) {
        self.event(tick, "echo.data", vec![conn(c), u(seq), Value::Bytes(bytes.into())]);
    }
    fn closed(&mut self, tick: u64, c: u64) {
        self.event(tick, "echo.closed", vec![conn(c), Value::Str("eof".into())]);
    }
}

#[cfg(test)]
fn conn(c: u64) -> Value {
    Value::Conn(ConnId(c))
}

#[cfg(test)]
fn u(n: u64) -> Value {
    Value::Int(IntValue::U64(n))
}

/// Every write of the run, per connection: (tick, seq, bytes), in tick order.
#[cfg(test)]
fn writes(artifact: &BlsArtifact, run: &SyncRun) -> BTreeMap<u64, Vec<(usize, u64, Vec<u8>)>> {
    let write = artifact.rel_named("echo.write").unwrap();
    let mut out: BTreeMap<u64, Vec<(usize, u64, Vec<u8>)>> = BTreeMap::new();
    for (t, round) in run.rounds.iter().enumerate() {
        for HostOut { rel, row } in &round[0].host {
            assert_eq!(*rel, write, "only writes are requested");
            let (Value::Conn(c), Value::Int(IntValue::U64(seq)), Value::Vec(parts)) = (&row[0], &row[1], &row[2])
            else {
                panic!("a write row {row:?}");
            };
            let mut bytes = Vec::new();
            for p in parts.iter() {
                match p {
                    Value::Enum { variant: 0, fields } => match fields.first() {
                        Some(Value::Bytes(b)) => bytes.extend_from_slice(b),
                        other => panic!("a part {other:?}"),
                    },
                    other => panic!("a part {other:?}"),
                }
            }
            out.entry(c.0).or_default().push((t, *seq, bytes));
        }
    }
    out
}

#[test]
fn the_echo_server_writes_back_complete_lines_across_chunk_boundaries() {
    let artifact = compile("echo.bls");
    let mut s = Stream {
        artifact: &artifact,
        inputs: Vec::new(),
    };
    s.opened(1, 7);
    s.data(2, 7, 0, b"hel");
    s.data(3, 7, 1, b"lo\nwor");
    s.data(4, 7, 2, b"ld\nand ");
    s.data(5, 7, 3, b"more\n\n");
    s.closed(6, 7);
    let run = differential(&artifact, &s.inputs, 7);
    let w = writes(&artifact, &run);
    assert_eq!(
        w.get(&7).cloned().unwrap_or_default(),
        vec![
            (3, 0, b"hello\n".to_vec()),
            (4, 1, b"world\n".to_vec()),
            (5, 2, b"and more\n\n".to_vec())
        ]
    );
    // The connection's state is dropped when it closes.
    let buf = artifact.rel_named("buf").unwrap();
    assert_eq!(run.rounds[7][0].instance.rows(buf).count(), 0);
}

#[test]
fn random_chunking_on_many_connections_echoes_every_line_in_order() {
    let artifact = compile("echo.bls");
    for seed in 0..12u64 {
        let mut rng = Rng(seed);
        let mut s = Stream {
            artifact: &artifact,
            inputs: Vec::new(),
        };
        let mut sent: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        let mut last = 0;
        for c in 0..1 + rng.below(4) {
            // A connection's text: lines of random printable bytes, possibly ending mid-line.
            let mut text = Vec::new();
            for _ in 0..rng.below(8) {
                for _ in 0..rng.below(12) {
                    text.push(b'a' + rng.below(26) as u8);
                }
                text.push(b'\n');
            }
            text.extend(std::iter::repeat_n(b'z', rng.below(5) as usize));
            let start = 1 + rng.below(4);
            s.opened(start, c);
            let mut tick = start + 1;
            let mut seq = 0;
            let mut rest = text.as_slice();
            while !rest.is_empty() {
                let n = (1 + rng.below(9) as usize).min(rest.len());
                s.data(tick, c, seq, &rest[..n]);
                rest = &rest[n..];
                seq += 1;
                tick += 1 + rng.below(2);
            }
            s.closed(tick, c);
            last = last.max(tick + 1);
            sent.insert(c, text);
        }
        let run = differential(&artifact, &s.inputs, last);
        let w = writes(&artifact, &run);
        for (c, text) in &sent {
            let got = w.get(c).cloned().unwrap_or_default();
            // Contiguous sequence numbers from 0, and the complete lines exactly, in order.
            assert!(
                got.iter().enumerate().all(|(i, (_, seq, _))| *seq == i as u64),
                "seed {seed} conn {c}: {got:?}"
            );
            let echoed: Vec<u8> = got.iter().flat_map(|(_, _, b)| b.clone()).collect();
            let complete = text.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
            assert_eq!(echoed, text[..complete].to_vec(), "seed {seed} conn {c}");
        }
    }
}
