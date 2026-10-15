//! Read-only worker evaluation must preserve complete tick results, errors, function budgets and profiling.
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_ir::tick::StepInput;
use blossom_value::{
    IntValue, Value,
    time::{Instant, NodeId, Tick},
};
use std::{path::Path, sync::Arc};

#[cfg(test)]
fn compile() -> blossom_artifact::bls::BlsArtifact {
    compile_file(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/engine/parallel.bls")
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
#[cfg(test)]
fn u(x: u64) -> Value {
    Value::Int(IntValue::U64(x))
}

#[test]
fn serial_parallel_and_oracle_ticks_agree_through_simultaneous_changes_and_reset() {
    use blossom_node::EngineEvaluator;
    use blossom_sim::{
        FaultSchedule,
        bls::{BlsSim, InputEvent},
    };
    use blossom_value::{Seed, time::Duration};
    let artifact = compile();
    let sim = BlsSim::new(&artifact, Seed::from_u64(7)).unwrap();
    let mut inputs = Vec::new();
    for tick in 0..6u64 {
        for k in 0..128 {
            inputs.push(InputEvent {
                node: NodeId(0),
                tick: Tick(tick),
                rel: artifact.rel_named("pair").unwrap(),
                row: Arc::from(vec![u(k), u(tick + 2), u(tick + 3)]),
            });
            inputs.push(InputEvent {
                node: NodeId(0),
                tick: Tick(tick),
                rel: artifact.rel_named("put").unwrap(),
                row: Arc::from(vec![u(k), u(tick + k)]),
            });
            inputs.push(InputEvent {
                node: NodeId(0),
                tick: Tick(tick),
                rel: artifact.rel_named("watch").unwrap(),
                row: Arc::from(vec![u(k * 2)]),
            });
        }
    }
    let cfg = blossom_engine::EngineConfig {
        node_names: vec![Arc::from("n1")],
        seed: Some(Seed::from_u64(7)),
        workers: 2,
        ..Default::default()
    };
    let actual = sim
        .run_on(
            &EngineEvaluator::new(artifact.program.clone(), cfg),
            &inputs,
            Tick(8),
            Duration::from_nanos(1000000),
            &FaultSchedule::default(),
            false,
        )
        .unwrap();
    let oracle = sim
        .run(
            &inputs,
            Tick(8),
            Duration::from_nanos(1000000),
            &FaultSchedule::default(),
            false,
        )
        .unwrap();
    for (a, b) in actual.rounds.iter().zip(&oracle.rounds) {
        assert_eq!(a[0].instance, b[0].instance);
    }

    let mut serial = blossom_engine::Engine::new(
        artifact.program.clone(),
        NodeId(0),
        blossom_engine::EngineConfig {
            node_names: vec![Arc::from("n1")],
            ..Default::default()
        },
    )
    .unwrap();
    let mut parallel = blossom_engine::Engine::new(
        artifact.program.clone(),
        NodeId(0),
        blossom_engine::EngineConfig {
            node_names: vec![Arc::from("n1")],
            workers: 2,
            ..Default::default()
        },
    )
    .unwrap();
    serial.set_profile_functions(true);
    parallel.set_profile_functions(true);
    let observe: Vec<_> = artifact
        .program
        .get()
        .rels
        .iter_enumerated()
        .map(|(id, _)| id)
        .collect();
    for tick in 0..10 {
        let events: Vec<_> = inputs
            .iter()
            .filter(|x| x.tick == Tick(tick))
            .map(|x| (x.rel, x.row.clone()))
            .collect();
        let input = StepInput {
            node: NodeId(0),
            incarnation: 1,
            tick: Tick(tick),
            now: Instant(tick as i64),
            events: &events,
            delivered: &[],
            ingress: &[],
            blobs: &blossom_value::NoBlobs,
        };
        let a = serial.step(&input, &observe).unwrap();
        let b = parallel.step(&input, &observe).unwrap();
        assert_eq!(a.changes, b.changes);
        assert_eq!(a.observed, b.observed);
        assert_eq!(serial.carried_instance().unwrap(), parallel.carried_instance().unwrap());
        assert_eq!(serial.work_by_function(), parallel.work_by_function());
        if tick == 3 {
            serial.reset(serial.carried_instance().unwrap()).unwrap();
            parallel.reset(parallel.carried_instance().unwrap()).unwrap();
        }
    }
    assert!(parallel.parallel_terms() >= 128);
    assert_eq!(serial.parallel_terms(), 0);
}

#[test]
fn worker_errors_match_serial_and_reset_restores_evaluation() {
    let artifact = compile();
    let events: Vec<_> = (0..128)
        .map(|k| {
            (
                artifact.rel_named("fail").unwrap(),
                Arc::from(vec![u(k), u(if k == 79 { 0 } else { 2 })]),
            )
        })
        .collect();
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
    let mut messages = Vec::new();
    for workers in [1, 2, 4] {
        let mut engine = blossom_engine::Engine::new(
            artifact.program.clone(),
            NodeId(0),
            blossom_engine::EngineConfig {
                node_names: vec![Arc::from("n1")],
                workers,
                ..Default::default()
            },
        )
        .unwrap();
        messages.push(engine.step(&input, &[]).unwrap_err().to_string());
        engine.reset(Default::default()).unwrap();
        let safe: Vec<_> = events.iter().filter(|(_, r)| r[1] != u(0)).cloned().collect();
        engine
            .step(
                &StepInput {
                    events: &safe,
                    ..input.clone()
                },
                &[],
            )
            .unwrap();
    }
    assert!(messages.windows(2).all(|w| w[0] == w[1]));
}

#[test]
fn independent_small_rules_share_workers_and_publish_in_serial_error_order() {
    let artifact = compile_file(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/engine/stage_schedule.bls")
            .to_str()
            .unwrap(),
        &[NodeSpec {
            name: "n1".into(),
            role: None,
        }],
    )
    .0
    .unwrap()
    .0;
    let mut serial = blossom_engine::Engine::new(artifact.program.clone(), NodeId(0), Default::default()).unwrap();
    let mut parallel = blossom_engine::Engine::new(
        artifact.program.clone(),
        NodeId(0),
        blossom_engine::EngineConfig {
            workers: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let observe: Vec<_> = artifact
        .program
        .get()
        .rels
        .iter_enumerated()
        .map(|(id, _)| id)
        .collect();
    serial.set_profile_functions(true);
    parallel.set_profile_functions(true);
    let oracle = blossom_oracle::Oracle::new(artifact.program.clone()).unwrap();
    for tick in 0..5 {
        // Each rule sees fewer rows than the within-rule worker threshold. Only cross-rule scheduling
        // can activate the pool here; the dependent join still runs after all three sources publish.
        let events: Vec<_> = (0..40)
            .map(|k| {
                (
                    artifact
                        .rel_named(if tick == 3 { "divisors" } else { "values" })
                        .unwrap(),
                    Arc::from(vec![u(k), u(if tick == 3 { 0 } else { tick + k })]),
                )
            })
            .collect();
        let input = StepInput {
            node: NodeId(0),
            incarnation: 1,
            tick: Tick(tick),
            now: Instant(tick as i64),
            events: &events,
            delivered: &[],
            ingress: &[],
            blobs: &blossom_value::NoBlobs,
        };
        let a = serial.step(&input, &observe);
        let b = parallel.step(&input, &observe);
        if tick == 3 {
            assert_eq!(a.unwrap_err().to_string(), b.unwrap_err().to_string());
            serial.reset(Default::default()).unwrap();
            parallel.reset(Default::default()).unwrap();
        } else {
            let (a, b) = (a.unwrap(), b.unwrap());
            assert_eq!(a.observed, b.observed);
            assert_eq!(a.changes, b.changes);
            assert_eq!(serial.work_by_function(), parallel.work_by_function());
            let o = oracle
                .tick(&blossom_ir::tick::TickInput {
                    node: input.node,
                    incarnation: input.incarnation,
                    tick: input.tick,
                    now: input.now,
                    carried: &Default::default(),
                    events: &events,
                    delivered: &[],
                    ingress: &[],
                    capture: false,
                    blobs: &blossom_value::NoBlobs,
                })
                .unwrap();
            for rel in &observe {
                assert_eq!(
                    b.observed.get(rel).unwrap(),
                    &o.instance.rows(*rel).cloned().collect::<Vec<_>>()
                );
            }
        }
        if tick == 2 {
            assert!(parallel.parallel_terms() >= 3, "independent stages did not use workers");
        }
    }
    assert_eq!(serial.parallel_terms(), 0);
}

#[test]
fn expensive_small_retained_batches_use_workers_after_learning_their_work() {
    let artifact = compile_file(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/engine/parallel_cost.bls")
            .to_str()
            .unwrap(),
        &[NodeSpec {
            name: "n1".into(),
            role: None,
        }],
    )
    .0
    .unwrap()
    .0;
    let cfg = blossom_engine::EngineConfig {
        map_cache_rows: 256,
        ..Default::default()
    };
    let mut serial = blossom_engine::Engine::new(artifact.program.clone(), NodeId(0), cfg.clone()).unwrap();
    let mut parallel = blossom_engine::Engine::new(
        artifact.program.clone(),
        NodeId(0),
        blossom_engine::EngineConfig { workers: 2, ..cfg },
    )
    .unwrap();
    serial.set_profile_functions(true);
    parallel.set_profile_functions(true);
    let observe = [artifact.rel_named("results").unwrap()];
    for tick in 0..3 {
        let events: Vec<_> = (0..8)
            .map(|k| (artifact.rel_named("values").unwrap(), Arc::from([u(k), u(tick + k)])))
            .collect();
        let input = StepInput {
            node: NodeId(0),
            incarnation: 1,
            tick: Tick(tick),
            now: Instant(tick as i64),
            events: &events,
            delivered: &[],
            ingress: &[],
            blobs: &blossom_value::NoBlobs,
        };
        let a = serial.step(&input, &observe).unwrap();
        let b = parallel.step(&input, &observe).unwrap();
        assert_eq!(a.observed, b.observed);
        assert_eq!(serial.work_by_function(), parallel.work_by_function());
        if tick == 0 {
            assert_eq!(parallel.parallel_terms(), 0);
        } else {
            assert!(
                parallel.parallel_terms() >= 8,
                "expensive short batch never used workers"
            );
        }
    }
}
