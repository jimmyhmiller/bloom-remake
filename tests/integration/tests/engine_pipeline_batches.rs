//! Physical pipeline stages retain every logical view, duplicate support and old-version read.
use std::{path::Path, sync::Arc};

use blossom_driver::bls::compile_file;
use blossom_engine::{Engine, EngineConfig};
use blossom_front::api::NodeSpec;
use blossom_ir::tick::{Instance, Row, StepInput};
use blossom_value::{
    Value,
    time::{Instant, NodeId, Tick},
};

#[cfg(test)]
fn compile() -> blossom_artifact::bls::BlsArtifact {
    compile_file(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/engine/functional_keys.bls")
            .to_str()
            .unwrap(),
        &[NodeSpec {
            name: "n1".into(),
            role: None,
        }],
    )
    .0
    .unwrap()
    .0
}

#[test]
fn keys_are_inferred_conservatively_and_pipeline_views_remain_observable() {
    let a = compile();
    let mut events_for_oracle = Vec::new();
    for (batches, keys, workers) in [(false, true, 1), (true, false, 1), (true, true, 1), (true, true, 2)] {
        let cfg = EngineConfig {
            pipeline_batches: batches,
            infer_keys: keys,
            workers,
            map_cache_rows: 128,
            map_cache_joins: true,
            lazy_indexes: true,
            ..Default::default()
        };
        let mut candidate = Engine::new(a.program.clone(), NodeId(0), cfg).unwrap();
        let mut reference = Engine::new(a.program.clone(), NodeId(0), Default::default()).unwrap();
        candidate.reset(Instance::default()).unwrap();
        reference.reset(Instance::default()).unwrap();
        let plan = candidate.plan_report();
        if keys {
            let key_of = |name: &str| {
                plan.stores
                    .iter()
                    .find(|s| s.key == format!("{name}:main"))
                    .unwrap()
                    .physical_key
                    .clone()
                    .unwrap()
            };
            assert_eq!(key_of("decoded"), vec![0]);
            assert_eq!(key_of("joined"), vec![0]);
            assert_eq!(key_of("packed"), vec![0]);
            // Generator membership is not functionally determined by its source key.
            assert_eq!(key_of("generated"), vec![0, 1]);
            // Multiple writers and time-varying expressions must retain their original key.
            assert_eq!(key_of("multi"), vec![0, 1]);
            assert_eq!(key_of("timed"), vec![0, 1, 2]);
        }
        if batches {
            assert!(
                plan.rules
                    .iter()
                    .filter(|r| r.head == "mirror:main" || r.head == "mirror_chain:main")
                    .all(|r| r.operator == "alias")
            );
            assert!(
                plan.stores
                    .iter()
                    .find(|s| s.key == "decoded:main")
                    .unwrap()
                    .batch_backed
            );
            assert!(!plan.stores.iter().find(|s| s.key == "multi:main").unwrap().batch_backed);
        }
        for t in 0..24 {
            if t == 12 {
                candidate.reset(Instance::default()).unwrap();
                reference.reset(Instance::default()).unwrap();
            }
            let events = if t % 3 == 0 {
                (0..5)
                    .map(|k| {
                        (
                            a.rel_named("put").unwrap(),
                            Row::from(vec![Value::u64(k), Value::u64(t % 4)]),
                        )
                    })
                    .collect::<Vec<_>>()
            } else if t % 3 == 1 {
                vec![(a.rel_named("erase").unwrap(), Row::from(vec![Value::u64(t % 5)]))]
            } else {
                vec![]
            };
            if batches && keys && workers == 1 && t < 12 {
                events_for_oracle.extend(events.iter().map(|(rel, row)| blossom_sim::bls::InputEvent {
                    tick: Tick(t),
                    node: NodeId(0),
                    rel: *rel,
                    row: row.clone(),
                }));
            }
            let input = StepInput {
                node: NodeId(0),
                incarnation: 1,
                tick: Tick(t),
                now: Instant(t as i64),
                events: &events,
                delivered: &[],
                ingress: &[],
                blobs: &blossom_value::NoBlobs,
            };
            let x = reference.step(&input, &[]).unwrap();
            let y = candidate.step(&input, &[]).unwrap();
            assert_eq!(x.changes, y.changes);
            assert_eq!(x.outbox, y.outbox);
            assert_eq!(x.host, y.host);
            assert_eq!(reference.instance_now().unwrap(), candidate.instance_now().unwrap());
        }
    }
    let seed = blossom_value::Seed::from_u64(7);
    let sim = blossom_sim::bls::BlsSim::new(&a, seed).unwrap();
    let evaluator = blossom_node::EngineEvaluator::new(
        a.program.clone(),
        EngineConfig {
            seed: Some(seed),
            node_names: vec![Arc::from("n1")],
            pipeline_batches: true,
            infer_keys: true,
            map_cache_rows: 128,
            map_cache_joins: true,
            ..Default::default()
        },
    );
    let config = Default::default();
    let oracle = sim
        .run(
            &events_for_oracle,
            Tick(12),
            blossom_value::time::Duration::from_nanos(1),
            &config,
            false,
        )
        .unwrap();
    let actual = sim
        .run_on(
            &evaluator,
            &events_for_oracle,
            Tick(12),
            blossom_value::time::Duration::from_nanos(1),
            &config,
            false,
        )
        .unwrap();
    for (x, y) in oracle.rounds.iter().zip(&actual.rounds) {
        for (x, y) in x.iter().zip(y) {
            assert_eq!(x.instance, y.instance);
            assert_eq!(x.egress, y.egress);
        }
    }
}

#[test]
fn inferred_buckets_preserve_conflicting_payloads_and_failure_order() {
    let a = compile();
    let mut errors = Vec::new();
    for (batches, keys) in [(false, false), (false, true), (true, true)] {
        let mut engine = Engine::new(
            a.program.clone(),
            NodeId(0),
            EngineConfig {
                pipeline_batches: batches,
                infer_keys: keys,
                ..Default::default()
            },
        )
        .unwrap();
        engine.reset(Instance::default()).unwrap();
        let events = vec![
            (
                a.rel_named("put").unwrap(),
                Row::from(vec![Value::u64(1), Value::u64(2)]),
            ),
            (
                a.rel_named("put").unwrap(),
                Row::from(vec![Value::u64(1), Value::u64(3)]),
            ),
        ];
        let input = StepInput {
            node: NodeId(0),
            incarnation: 1,
            tick: Tick(0),
            now: Instant(0),
            events: &events,
            delivered: &[],
            ingress: &[],
            blobs: &blossom_value::NoBlobs,
        };
        errors.push(format!("{:?}", engine.step(&input, &[]).unwrap_err()));
    }
    assert_eq!(errors[0], errors[1]);
    assert_eq!(errors[0], errors[2]);
}

#[test]
fn derived_key_hints_never_drop_conflicting_carried_source_payloads() {
    let a = compile();
    let mut carried = Instance::default();
    carried.insert(
        a.rel_named("source").unwrap(),
        Row::from(vec![Value::u64(1), Value::u64(2)]),
    );
    carried.insert(
        a.rel_named("source").unwrap(),
        Row::from(vec![Value::u64(1), Value::u64(3)]),
    );
    let input = StepInput {
        node: NodeId(0),
        incarnation: 1,
        tick: Tick(0),
        now: Instant(0),
        events: &[],
        delivered: &[],
        ingress: &[],
        blobs: &blossom_value::NoBlobs,
    };
    let mut snapshots = Vec::new();
    let mut errors = Vec::new();
    for (batches, keys) in [(false, false), (false, true), (true, true)] {
        let mut engine = Engine::new(
            a.program.clone(),
            NodeId(0),
            EngineConfig {
                pipeline_batches: batches,
                infer_keys: keys,
                ..Default::default()
            },
        )
        .unwrap();
        engine.reset(carried.clone()).unwrap();
        errors.push(format!("{:?}", engine.step(&input, &[]).unwrap_err()));
        snapshots.push(engine.instance_now().unwrap());
    }
    assert_eq!(errors[0], errors[1]);
    assert_eq!(errors[0], errors[2]);
    assert_eq!(snapshots[0], snapshots[1]);
    assert_eq!(snapshots[0], snapshots[2]);
    assert_eq!(snapshots[1][&a.rel_named("decoded").unwrap()].len(), 2);
}
