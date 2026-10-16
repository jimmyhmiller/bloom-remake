//! Retained map outputs must preserve full-row set semantics, duplicate derivations and errors.
use blossom_driver::bls::compile_file;
use blossom_engine::{Engine, EngineConfig};
use blossom_front::api::NodeSpec;
use blossom_ir::tick::{Instance, StepInput};
use blossom_value::{
    Value,
    time::{Instant, NodeId, Tick},
};
use std::{path::Path, sync::Arc};

#[cfg(test)]
fn compile() -> blossom_artifact::bls::BlsArtifact {
    compile_file(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/engine/retained_map.bls")
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
fn retained_maps_match_serial_state_through_evictions_duplicates_and_reset() {
    let a = compile();
    let mut replay_events = Vec::new();
    for budget in [1, 4, 128] {
        let mut reference = Engine::new(a.program.clone(), NodeId(0), Default::default()).unwrap();
        let mut candidate = Engine::new(
            a.program.clone(),
            NodeId(0),
            EngineConfig {
                map_cache_rows: budget,
                map_cache_joins: true,
                lazy_indexes: true,
                pipeline_batches: true,
                infer_keys: true,
                ..Default::default()
            },
        )
        .unwrap();
        reference.reset(Instance::default()).unwrap();
        candidate.reset(Instance::default()).unwrap();
        for t in 0..40u64 {
            if t == 20 {
                reference.reset(Instance::default()).unwrap();
                candidate.reset(Instance::default()).unwrap();
            }
            let mut events: Vec<(blossom_base::RelId, blossom_ir::tick::Row)> = if t % 3 == 0 {
                (0..6)
                    .map(|k| {
                        (
                            a.rel_named("put").unwrap(),
                            Arc::from([
                                Value::u64(k),
                                Value::Vec(Arc::from([Value::u64(t + k), Value::u64(t + k), Value::u64(t + k + 1)])),
                                Value::u64(2),
                            ]),
                        )
                    })
                    .collect::<Vec<_>>()
            } else {
                vec![]
            };
            if t % 3 == 1 {
                for k in 0..6 {
                    events.push((
                        a.rel_named("set_right").unwrap(),
                        Arc::from([Value::u64(k), Value::u64(t)]),
                    ));
                }
            }
            if t % 5 == 0 {
                events.push((a.rel_named("remove_right").unwrap(), Arc::from([Value::u64(t % 6)])));
            }
            if t % 4 == 1 {
                events.push((a.rel_named("bump").unwrap(), Arc::from([Value::u64(t)])));
            }
            // Both operands change; new left equals old right only in the ghost valuation.
            events.push((
                a.rel_named("pair").unwrap(),
                Arc::from([Value::u64(1), Value::u64(t), Value::u64(t + 1)]),
            ));
            if budget == 1 {
                replay_events.extend(events.iter().map(|(rel, row)| blossom_sim::bls::InputEvent {
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
            assert!(candidate.plan_report().map_cache.retained_rows <= budget);
        }
        let report = candidate.plan_report();
        if budget == 128 {
            assert!(report.map_cache.retraction_hits > 0);
        }
        if budget == 4 {
            assert!(report.map_cache.evictions > 0);
        }
        assert!(
            report
                .rules
                .iter()
                .filter(|r| r.head == "timed:main")
                .all(|r| r.operator != "retained-map")
        );
        let source_ref = reference.work_by_rule().values().map(|w| w.steps).sum::<u64>();
        let source_new = candidate.work_by_rule().values().map(|w| w.steps).sum::<u64>();
        if budget == 128 {
            assert!(source_new < source_ref);
        }
    }
    let seed = blossom_value::Seed::from_u64(7);
    let sim = blossom_sim::bls::BlsSim::new(&a, seed).unwrap();
    let engine = blossom_node::EngineEvaluator::new(
        a.program.clone(),
        EngineConfig {
            seed: Some(seed),
            node_names: vec![Arc::from("n1")],
            map_cache_rows: 128,
            map_cache_joins: true,
            lazy_indexes: true,
            pipeline_batches: true,
            infer_keys: true,
            ..Default::default()
        },
    );
    let oracle = sim
        .run(
            &replay_events,
            Tick(42),
            blossom_value::time::Duration::from_nanos(1),
            &Default::default(),
            false,
        )
        .unwrap();
    let actual = sim
        .run_on(
            &engine,
            &replay_events,
            Tick(42),
            blossom_value::time::Duration::from_nanos(1),
            &Default::default(),
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
fn retained_map_errors_match_and_reset_discards_outputs() {
    let a = compile();
    let mut errors = Vec::new();
    for budget in [0, 128] {
        let mut engine = Engine::new(
            a.program.clone(),
            NodeId(0),
            EngineConfig {
                map_cache_rows: budget,
                map_cache_joins: true,
                lazy_indexes: true,
                pipeline_batches: true,
                infer_keys: true,
                ..Default::default()
            },
        )
        .unwrap();
        engine.reset(Instance::default()).unwrap();
        let mut failed = None;
        for t in 0..5u64 {
            let events = if t == 0 || t == 2 {
                vec![(
                    a.rel_named("put").unwrap(),
                    Arc::from([
                        Value::u64(1),
                        Value::Vec(Arc::from([Value::u64(2)])),
                        Value::u64(if t == 0 { 2 } else { 0 }),
                    ]),
                )]
            } else {
                vec![]
            };
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
            if let Err(e) = engine.step(&input, &[]) {
                let blossom_ir::tick::EvalError::Program { error, .. } = e else {
                    panic!("unexpected error: {e}")
                };
                failed = Some((t, error.code));
                break;
            }
        }
        let failed = failed.unwrap();
        assert_eq!(failed.0, 3);
        errors.push(failed);
        engine.reset(Instance::default()).unwrap();
        assert_eq!(engine.plan_report().map_cache.retained_rows, 0);
    }
    assert_eq!(errors[0], errors[1]);
}
