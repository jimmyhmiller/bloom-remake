//! Slice 6: pure functions (LANGUAGE §16.1) and the built-in library (Appendix B), end to end. The fixture
//! `fixtures/functions/library.bls` applies every library function and combinator inside functions; random inputs
//! run on the oracle and on the engine, which must agree at every tick, and every view must equal what this file
//! computes for it directly in Rust (a third, independent implementation).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::EngineEvaluator;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_sim::sync::SyncRun;
use blossom_value::Value;
use blossom_value::time::{Duration, NodeId, Tick};
use blossom_value::value::IntValue;

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
        .join("fixtures/functions")
        .join(name);
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    result.unwrap_or_else(|e| panic!("{name}: {e:?}")).0
}

/// Runs `artifact` on the oracle and on the engine; they must agree at every tick. Returns the oracle's run.
#[cfg(test)]
fn differential(artifact: &BlsArtifact, inputs: &[InputEvent], last: u64) -> SyncRun {
    let sim = BlsSim::new(artifact, blossom_value::Seed::from_u64(0)).unwrap();
    let round = Duration::from_nanos(1_000_000_000);
    let reference = sim
        .run(inputs, Tick(last), round, &FaultSchedule::default(), false)
        .unwrap_or_else(|e| panic!("the oracle failed: {e}"));
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let mine = sim
        .run_on(&engine, inputs, Tick(last), round, &FaultSchedule::default(), false)
        .unwrap_or_else(|e| panic!("the engine failed: {e}"));
    assert_eq!(reference.rounds.len(), mine.rounds.len());
    for (t, (ra, rb)) in reference.rounds.iter().zip(&mine.rounds).enumerate() {
        for (x, y) in ra.iter().zip(rb) {
            assert_eq!(x.instance, y.instance, "tick {t}: the oracle and the engine differ");
        }
    }
    reference
}

#[cfg(test)]
fn u(n: u64) -> Value {
    Value::Int(IntValue::U64(n))
}

#[cfg(test)]
fn vec_u(xs: impl IntoIterator<Item = u64>) -> Value {
    Value::Vec(xs.into_iter().map(u).collect())
}

#[cfg(test)]
fn tuple(xs: Vec<Value>) -> Value {
    Value::Tuple(xs.into())
}

#[cfg(test)]
fn opt(x: Option<Value>) -> Value {
    Value::Option(x.map(Arc::new))
}

#[cfg(test)]
fn bytes(b: &[u8]) -> Value {
    Value::Bytes(Arc::from(b))
}

/// The value each view's function computes, written directly.
#[cfg(test)]
fn expected(view: &str, n: u64, s: &str, b: &[u8]) -> (Value, Value) {
    let evens: Vec<u64> = (0..n).filter(|i| i % 2 == 0).collect();
    match view {
        "v_sum" => (u(n), u((0..n).sum())),
        "v_evens" => (u(n), vec_u(evens)),
        "v_scaled" => (u(n), vec_u((0..n).map(|i| i * n))),
        "v_thirds" => (u(n), vec_u((0..n).filter(|i| i % 3 == 0).map(|i| i / 3))),
        "v_triangle" => (u(n), vec_u((0..n).map(|i| i + (0..i).sum::<u64>()))),
        "v_quant" => (
            u(n),
            tuple(vec![Value::Bool(n <= 5), Value::Bool(n >= 8), Value::Bool(false)]),
        ),
        "v_ends" => (
            u(n),
            tuple(vec![
                opt(evens.first().map(|x| u(*x))),
                opt(evens.last().map(|x| u(*x))),
                opt(evens.get(2).map(|x| u(*x))),
            ]),
        ),
        "v_reshaped" => {
            let mut v = evens;
            v.push(n);
            v.extend([0, 10]);
            v.reverse();
            (u(n), vec_u(v))
        }
        "v_shadowed" => (u(n), u(3 * n + 2)),
        "v_words" => (
            Value::Str(s.into()),
            Value::Vec(
                s.split_whitespace()
                    .enumerate()
                    .map(|(i, w)| tuple(vec![u(i as u64), Value::Str(w.to_lowercase().into())]))
                    .collect(),
            ),
        ),
        "v_blank" => (Value::Str(s.into()), Value::Bool(s.split_whitespace().next().is_none())),
        "v_fields" => (
            bytes(b),
            opt((b.len() >= 3).then(|| tuple(vec![bytes(&b[0..1]), bytes(&b[1..3])]))),
        ),
        "v_doubled" => (bytes(b), bytes(&[b, b].concat())),
        "v_defaults" => {
            let o = [0u64, 3, 6, 9].get(n as usize).copied();
            (
                u(n),
                tuple(vec![
                    u(o.unwrap_or(100)),
                    Value::Bool(o.is_some()),
                    Value::Bool(o.is_none()),
                ]),
            )
        }
        "v_classify" => (
            u(n),
            Value::Str(
                match (n / 10, n % 10) {
                    (0, _) => "small",
                    (_, 0) => "round",
                    _ => "big",
                }
                .into(),
            ),
        ),
        "v_range" => (u(n), u(n + 1)),
        other => panic!("no expectation for {other}"),
    }
}

#[cfg(test)]
const VIEWS: &[&str] = &[
    "v_sum",
    "v_evens",
    "v_scaled",
    "v_thirds",
    "v_triangle",
    "v_quant",
    "v_ends",
    "v_reshaped",
    "v_shadowed",
    "v_words",
    "v_blank",
    "v_fields",
    "v_doubled",
    "v_defaults",
    "v_classify",
    "v_range",
];

#[test]
fn every_library_function_agrees_on_both_evaluators_and_with_its_definition() {
    let artifact = compile("library.bls");
    let e = artifact.rel_named("e").unwrap();
    let words = ["Foo", "bar", "BAZ", "qUx", "ümlaut", "ΣΙΣΥΦΟΣ"];
    let spaces = [" ", "  ", "\t", "\n ", ""];
    let mut checked = 0;
    for seed in 0..8u64 {
        let mut rng = Rng(seed);
        let last = 12u64;
        let mut per_tick: Vec<Vec<(u64, String, Vec<u8>)>> = vec![Vec::new(); last as usize + 1];
        let mut inputs = Vec::new();
        for t in 1..last {
            for _ in 0..rng.below(4) {
                let n = rng.below(25);
                let mut s = String::new();
                for _ in 0..rng.below(4) {
                    s.push_str(spaces[rng.below(spaces.len() as u64) as usize]);
                    s.push_str(words[rng.below(words.len() as u64) as usize]);
                }
                s.push_str(spaces[rng.below(spaces.len() as u64) as usize]);
                let b: Vec<u8> = (0..rng.below(6)).map(|_| rng.below(256) as u8).collect();
                inputs.push(InputEvent {
                    node: NodeId(0),
                    tick: Tick(t),
                    rel: e,
                    row: Arc::from(vec![u(n), Value::Str(s.as_str().into()), bytes(&b)]),
                });
                per_tick[t as usize].push((n, s, b));
            }
        }
        let run = differential(&artifact, &inputs, last);
        for (t, rows) in per_tick.iter().enumerate() {
            let instance = &run.node_tick(Tick(t as u64), NodeId(0)).unwrap().instance;
            for view in VIEWS {
                let rel = artifact.rel_named(view).unwrap();
                let got: BTreeSet<Vec<Value>> = instance.rows(rel).map(|r| r.to_vec()).collect();
                let want: BTreeSet<Vec<Value>> = rows
                    .iter()
                    .map(|(n, s, b)| {
                        let (k, v) = expected(view, *n, s, b);
                        vec![k, v]
                    })
                    .collect();
                assert_eq!(got, want, "seed {seed} tick {t} view {view}");
                checked += want.len();
            }
        }
    }
    assert!(checked > 500, "only {checked} rows checked");
}
