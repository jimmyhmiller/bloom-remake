//! The language slice (docs/design/EXTENSIONS.md), end to end: each extension runs on the oracle and on the engine,
//! which must agree at every tick, and against the explicit program it abbreviates.

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
        .join("fixtures/extensions")
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

// ---------------------------------------------------------------- `table … while BODY` (EXTENSIONS 2.3)

/// Each guarded table holds, at every tick, exactly the rows of its twin that an explicit clean-up rule keeps;
/// and the guards do drop rows (each twin's clean-up fires), so the comparison is not vacuous.
#[test]
fn a_guarded_table_keeps_what_its_explicit_clean_up_keeps() {
    let artifact = compile("guarded.bls");
    let rel = |n: &str| artifact.rel_named(n).unwrap();
    let values = [10u64, 50, 150, 199, 200, 250];
    let mut dropped = [0usize; 3];
    let mut compared = 0usize;
    for seed in 0..12u64 {
        let mut rng = Rng(seed);
        let last = 30u64;
        let mut inputs = Vec::new();
        for t in 1..last {
            for _ in 0..rng.below(5) {
                let c = rng.below(3);
                let (name, row) = match rng.below(10) {
                    0 | 1 => ("open", vec![u(c)]),
                    2 => ("close", vec![u(c)]),
                    3 => ("ban", vec![u(values[rng.below(values.len() as u64) as usize])]),
                    4 => ("take", vec![u(c), u(rng.below(4))]),
                    _ => (
                        "put",
                        vec![
                            u(c),
                            u(rng.below(4)),
                            u(values[rng.below(values.len() as u64) as usize]),
                        ],
                    ),
                };
                inputs.push(InputEvent {
                    node: NodeId(0),
                    tick: Tick(t),
                    rel: rel(name),
                    row: Arc::from(row),
                });
            }
        }
        let run = differential(&artifact, &inputs, last);
        let rows = |t: u64, name: &str| -> BTreeSet<Vec<Value>> {
            run.node_tick(Tick(t), NodeId(0))
                .unwrap()
                .instance
                .rows(rel(name))
                .map(|r| r.to_vec())
                .collect()
        };
        for t in 0..=last {
            for (k, (g, m)) in [("g1", "m1"), ("g2", "m2"), ("g3", "m3")].into_iter().enumerate() {
                let (gs, ms) = (rows(t, g), rows(t, m));
                assert_eq!(gs, ms, "seed {seed} tick {t}: `{g}` and its twin `{m}` differ");
                compared += gs.len();
                // A row of the twin that the next tick neither keeps nor re-derives was dropped by a clean-up or
                // a `take`; count the ticks where that happens.
                if t < last && !ms.is_subset(&rows(t + 1, m)) {
                    dropped[k] += 1;
                }
            }
        }
    }
    assert!(compared > 1000, "only {compared} rows compared");
    assert!(
        dropped.iter().all(|d| *d > 20),
        "too few drops to exercise the guards: {dropped:?}"
    );
}
