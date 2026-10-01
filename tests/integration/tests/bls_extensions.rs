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

// ---------------------------------------------------------------- `resolve prefer(rule, …)` (EXTENSIONS 2.4)

/// How a run ended: the oracle's run, or a program error's tick and code.
#[cfg(test)]
enum Outcome {
    Ran(Box<SyncRun>),
    Failed(Tick, String),
}

/// Runs on both evaluators, which must agree on the rows or on the error.
#[cfg(test)]
fn differential_or_error(artifact: &BlsArtifact, inputs: &[InputEvent], last: u64) -> Outcome {
    let sim = BlsSim::new(artifact, blossom_value::Seed::from_u64(0)).unwrap();
    let round = Duration::from_nanos(1_000_000_000);
    let reference = sim.run(inputs, Tick(last), round, &FaultSchedule::default(), false);
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let mine = sim.run_on(&engine, inputs, Tick(last), round, &FaultSchedule::default(), false);
    let failure = |r: &Result<SyncRun, blossom_sim::sync::SimError>| match r {
        Ok(_) => None,
        Err(blossom_sim::sync::SimError::Node {
            tick,
            error: blossom_oracle::OracleError::Program { error, .. },
            ..
        }) => Some((*tick, error.code.to_string())),
        Err(e) => panic!("not a program error: {e}"),
    };
    assert_eq!(
        failure(&reference),
        failure(&mine),
        "the oracle and the engine fail differently"
    );
    if let (Ok(a), Ok(b)) = (&reference, &mine) {
        for (t, (ra, rb)) in a.rounds.iter().zip(&b.rounds).enumerate() {
            for (x, y) in ra.iter().zip(rb) {
                assert_eq!(x.instance, y.instance, "tick {t}: the oracle and the engine differ");
            }
        }
    }
    match failure(&reference) {
        Some((tick, code)) => Outcome::Failed(tick, code),
        None => Outcome::Ran(Box::new(reference.unwrap())),
    }
}

/// A preferring table holds, at every tick, exactly what its hand-guarded twin holds; the arbitration happens
/// (ticks where both writers hit one key), so the comparison is not vacuous.
#[test]
fn prefer_settles_same_tick_writes_as_the_hand_written_guards_do() {
    let artifact = compile("prefer.bls");
    let rel = |n: &str| artifact.rel_named(n).unwrap();
    let mut contested = 0usize;
    let mut compared = 0usize;
    for seed in 0..12u64 {
        let mut rng = Rng(seed);
        let last = 30u64;
        let mut inputs = Vec::new();
        for t in 1..last {
            // At most one event of each kind per key and tick: two from one handler would conflict by design.
            for name in ["reset", "advance", "fresh_a", "fresh_b"] {
                for k in 0..4u64 {
                    if rng.below(3) == 0 {
                        inputs.push(InputEvent {
                            node: NodeId(0),
                            tick: Tick(t),
                            rel: rel(name),
                            row: Arc::from(vec![u(k), u(rng.below(100))]),
                        });
                    }
                }
            }
        }
        for k in 0..4u64 {
            for t in 1..last {
                let has = |name: &str| {
                    inputs
                        .iter()
                        .any(|e| e.tick == Tick(t) && e.rel == rel(name) && e.row[0] == u(k))
                };
                if (has("reset") && has("advance")) || (has("fresh_a") && has("fresh_b")) {
                    contested += 1;
                }
            }
        }
        let run = match differential_or_error(&artifact, &inputs, last) {
            Outcome::Ran(run) => run,
            Outcome::Failed(t, code) => panic!("seed {seed}: {code} at {t:?}"),
        };
        for t in 0..=last {
            let rows = |name: &str| -> BTreeSet<Vec<Value>> {
                run.node_tick(Tick(t), NodeId(0))
                    .unwrap()
                    .instance
                    .rows(rel(name))
                    .map(|r| r.to_vec())
                    .collect()
            };
            for (a, b) in [("p", "q"), ("n", "m")] {
                let (x, y) = (rows(a), rows(b));
                assert_eq!(x, y, "seed {seed} tick {t}: `{a}` and its twin `{b}` differ");
                compared += x.len();
            }
        }
    }
    assert!(compared > 1000, "only {compared} rows compared");
    assert!(contested > 100, "only {contested} contested writes");
}

/// Two values from one listed handler, or a listed and an unlisted handler's values, still conflict (BLSR002).
#[test]
fn prefer_leaves_unarbitrated_conflicts_errors() {
    let artifact = compile("prefer.bls");
    let at = |name: &str, t: u64, k: u64, v: u64| InputEvent {
        node: NodeId(0),
        tick: Tick(t),
        rel: artifact.rel_named(name).unwrap(),
        row: Arc::from(vec![u(k), u(v)]),
    };
    for inputs in [
        vec![at("clash", 2, 1, 5)],
        vec![at("advance", 2, 1, 5), at("stray", 2, 1, 5)],
    ] {
        match differential_or_error(&artifact, &inputs, 4) {
            Outcome::Failed(tick, code) => assert_eq!((tick, code.as_str()), (Tick(2), "BLSR002")),
            Outcome::Ran(_) => panic!("{inputs:?}: no conflict"),
        }
    }
    // A listed and an unlisted write of one value do not conflict.
    let run = match differential_or_error(&artifact, &[at("advance", 2, 1, 7), at("stray", 2, 1, 1000)], 4) {
        Outcome::Ran(run) => run,
        Outcome::Failed(t, code) => panic!("one value conflicted: {code} at {t:?}"),
    };
    let rows: Vec<Vec<Value>> = run
        .node_tick(Tick(3), NodeId(0))
        .unwrap()
        .instance
        .rows(artifact.rel_named("p").unwrap())
        .map(|r| r.to_vec())
        .collect();
    assert_eq!(rows, vec![vec![u(1), u(1007)]]);
}

// ---------------------------------------------------------------- the order of checks (EXTENSIONS 2.6)

/// Filters run before the fallible bindings and conjuncts written ahead of them, on both evaluators: no division by
/// zero, no underflow for a done key, and the rows are what the views say.
#[test]
fn filters_protect_the_expressions_written_before_them() {
    let artifact = compile("order.bls");
    let rel = |n: &str| artifact.rel_named(n).unwrap();
    let ev = |t: u64, name: &str, row: Vec<Value>| InputEvent {
        node: NodeId(0),
        tick: Tick(t),
        rel: rel(name),
        row: Arc::from(row),
    };
    let mut inputs = Vec::new();
    let mut want_ratio = BTreeSet::new();
    let mut want_gap = BTreeSet::new();
    let mut want_big = BTreeSet::new();
    let mut rng = Rng(7);
    for k in 0..40u64 {
        let (a, b) = (rng.below(10), rng.below(4));
        // A pair whose `a - b` would underflow is always done in the same tick.
        if a < b {
            inputs.push(ev(1, "done", vec![u(k)]));
        } else {
            want_gap.insert(vec![u(k), u(a - b)]);
        }
        inputs.push(ev(1, "pair", vec![u(k), u(a), u(b)]));
        if b != 0 {
            want_ratio.insert(vec![u(k), u(a / b)]);
            if a / b > 1 {
                want_big.insert(vec![u(k)]);
            }
        }
    }
    let run = match differential_or_error(&artifact, &inputs, 2) {
        Outcome::Ran(run) => run,
        Outcome::Failed(t, code) => panic!("{code} at {t:?}"),
    };
    let rows = |name: &str| -> BTreeSet<Vec<Value>> {
        run.node_tick(Tick(1), NodeId(0))
            .unwrap()
            .instance
            .rows(rel(name))
            .map(|r| r.to_vec())
            .collect()
    };
    assert!(want_ratio.len() > 10 && want_gap.len() > 10 && want_big.len() > 5);
    assert_eq!(rows("ratio"), want_ratio);
    assert_eq!(rows("gap"), want_gap);
    assert_eq!(rows("big"), want_big);
    // A fallible check still raises for a valuation its filters accept.
    match differential_or_error(&artifact, &[ev(1, "pair", vec![u(1), u(1), u(2)])], 2) {
        Outcome::Failed(tick, code) => assert_eq!((tick, code.as_str()), (Tick(1), "BLSR004")),
        Outcome::Ran(_) => panic!("1 - 2 raised nothing"),
    }
}

/// A range guard narrows the scan though a fallible `let` is written before it: answering costs the rows in the
/// range, not the log.
#[test]
fn a_range_guard_narrows_past_a_fallible_let() {
    use blossom_ir::tick::StepInput;
    use blossom_value::time::Instant;
    let artifact = compile("order.bls");
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|x| Arc::from(x.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let mut engine = blossom_engine::Engine::new(artifact.program.clone(), NodeId(0), cfg).unwrap();
    let rel = |r: &str| artifact.rel_named(r).unwrap();
    let mut tick = 0u64;
    let mut step = |engine: &mut blossom_engine::Engine, events: Vec<(blossom_base::RelId, blossom_ir::tick::Row)>| {
        engine
            .step(
                &StepInput {
                    node: NodeId(0),
                    incarnation: 1,
                    tick: Tick(tick),
                    now: Instant(tick as i64),
                    events: &events,
                    delivered: &[],
                    ingress: &[],
                    blobs: &blossom_value::NoBlobs,
                },
                &[],
            )
            .unwrap();
        tick += 1;
    };
    let one = |x: u64| -> blossom_ir::tick::Row { Arc::from(vec![u(x)]) };
    step(&mut engine, (0..20_000u64).map(|i| (rel("add"), one(i))).collect());
    step(&mut engine, Vec::new());
    let before = engine.rows_examined();
    step(&mut engine, vec![(rel("ask"), one(5_000))]);
    let examined = engine.rows_examined() - before;
    let got: BTreeSet<Vec<Value>> = engine
        .carried_rows(rel("got"))
        .into_iter()
        .map(|r| r.to_vec())
        .collect();
    let want: BTreeSet<Vec<Value>> = (5_001..=5_003u64).map(|i| vec![u(5_000), u(i), u(i * 6)]).collect();
    assert_eq!(got, want);
    assert!(examined < 100, "answering examined {examined} rows");
}
