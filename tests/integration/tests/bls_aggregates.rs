//! Slice 7, item 4: the aggregate `collect!` and the built-in `rand(k…)`, which the broker's topic handlers use, on
//! the oracle and the engine (which must agree at every tick). The fixture is `fixtures/functions/aggregates.bls`.

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

#[cfg(test)]
fn compile() -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/functions/aggregates.bls");
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    result.unwrap_or_else(|e| panic!("aggregates.bls: {e:?}")).0
}

/// Runs the program on the oracle and the engine, which must agree at every tick, and returns the oracle's run.
#[cfg(test)]
fn run(artifact: &BlsArtifact, inputs: &[InputEvent], ticks: u64) -> SyncRun {
    let sim = BlsSim::new(artifact, blossom_value::Seed::from_u64(0)).unwrap();
    let round = Duration::from_nanos(1_000_000_000);
    let reference = sim.run(inputs, Tick(ticks), round, &FaultSchedule::default(), false).unwrap();
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let mine = sim.run_on(&engine, inputs, Tick(ticks), round, &FaultSchedule::default(), false).unwrap();
    assert_eq!(reference.rounds.len(), mine.rounds.len());
    for (t, (a, b)) in reference.rounds.iter().zip(&mine.rounds).enumerate() {
        assert_eq!(a[0].instance, b[0].instance, "tick {t}: the oracle and the engine differ");
    }
    reference
}

#[cfg(test)]
fn input(artifact: &BlsArtifact, tick: u64, rel: &str, row: Vec<Value>) -> InputEvent {
    InputEvent {
        node: NodeId(0),
        tick: Tick(tick),
        rel: artifact.rel_named(rel).unwrap(),
        row: Arc::from(row),
    }
}

#[cfg(test)]
fn rows(artifact: &BlsArtifact, run: &SyncRun, tick: u64, name: &str) -> Vec<Vec<Value>> {
    let mut out: Vec<Vec<Value>> = run
        .node_tick(Tick(tick), NodeId(0))
        .unwrap()
        .instance
        .rows(artifact.rel_named(name).unwrap())
        .map(|r| r.to_vec())
        .collect();
    out.sort();
    out
}

#[cfg(test)]
fn u(x: u64) -> Value {
    Value::Int(IntValue::U64(x))
}

#[cfg(test)]
fn s(x: &str) -> Value {
    Value::Str(x.into())
}

#[cfg(test)]
fn strs(xs: &[&str]) -> Value {
    Value::Vec(xs.iter().map(|x| s(x)).collect())
}

#[test]
fn collect_gathers_each_valuation_in_canonical_order() {
    let artifact = compile();
    let mut inputs = Vec::new();
    for (g, v, w) in [(1, "b", 1), (1, "a", 2), (1, "b", 3), (2, "z", 1)] {
        inputs.push(input(&artifact, 1, "item", vec![u(g), s(v), u(w)]));
    }
    for g in [1, 3] {
        inputs.push(input(&artifact, 1, "group", vec![u(g)]));
    }
    let r = run(&artifact, &inputs, 3);
    assert_eq!(
        rows(&artifact, &r, 1, "grouped"),
        vec![vec![u(1), strs(&["a", "b", "b"])], vec![u(2), strs(&["z"])]]
    );
    // The driver gives the empty group its row, and only driver rows appear.
    assert_eq!(
        rows(&artifact, &r, 1, "per_group"),
        vec![vec![u(1), strs(&["a", "b", "b"])], vec![u(3), strs(&[])]]
    );
    let pair = |g: u64, v: &str| Value::Tuple(vec![u(g), s(v)].into());
    assert_eq!(
        rows(&artifact, &r, 1, "pairs"),
        vec![vec![Value::Vec(vec![pair(1, "a"), pair(1, "b"), pair(2, "z")].into())]]
    );
    // An empty view without a driver has no row (CR-08); inputs are gone at the next tick.
    assert!(rows(&artifact, &r, 2, "grouped").is_empty());
    assert!(rows(&artifact, &r, 2, "pairs").is_empty());
}

#[test]
fn rand_is_stable_per_key_and_tick() {
    let artifact = compile();
    let mut inputs = Vec::new();
    for t in [1, 2] {
        for k in [5, 6] {
            inputs.push(input(&artifact, t, "key", vec![u(k)]));
        }
    }
    let r = run(&artifact, &inputs, 3);
    let draws = |t: u64| -> Vec<(u64, u64, u64)> {
        rows(&artifact, &r, t, "draws")
            .into_iter()
            .map(|row| match row.as_slice() {
                [_, Value::Int(IntValue::U64(a)), Value::Int(IntValue::U64(b)), Value::Int(IntValue::U64(c))] => {
                    (*a, *b, *c)
                }
                other => panic!("{other:?}"),
            })
            .collect()
    };
    let (one, two) = (draws(1), draws(2));
    assert_eq!(one.len(), 2);
    for (a, b, c) in one.iter().chain(&two) {
        assert_eq!(a, b, "one key, one tick: one value");
        assert_ne!(a, c, "another key: another value");
    }
    assert_ne!(one[0].0, one[1].0, "keys 5 and 6 draw differently");
    assert_ne!(one[0].0, two[0].0, "the next tick draws afresh");
}
