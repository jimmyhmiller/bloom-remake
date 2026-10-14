//! Native store paths against the independent oracle, including guarded carry and simultaneous deltas.
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::EngineEvaluator;
use blossom_sim::{
    FaultSchedule,
    bls::{BlsSim, InputEvent},
};
use blossom_value::{
    Seed, Value,
    time::{Duration, NodeId, Tick},
};
use std::{path::Path, sync::Arc};
#[cfg(test)]
fn check(inputs: Vec<(u64, &str, Vec<Value>)>, last: u64) -> bool {
    check_fixture("native_upsert", inputs, last)
}
#[cfg(test)]
fn check_fixture(fixture: &str, inputs: Vec<(u64, &str, Vec<Value>)>, last: u64) -> bool {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("fixtures/engine/{fixture}.bls"));
    let (result, _) = compile_file(
        path.to_str().unwrap(),
        &[NodeSpec {
            name: "n1".into(),
            role: None,
        }],
    );
    let artifact = result.unwrap().0;
    let sim = BlsSim::new(&artifact, Seed::from_u64(7)).unwrap();
    let events: Vec<_> = inputs
        .into_iter()
        .map(|(t, r, row)| InputEvent {
            tick: Tick(t),
            node: NodeId(0),
            rel: artifact.rel_named(r).unwrap(),
            row: Arc::from(row),
        })
        .collect();
    let cfg = blossom_engine::EngineConfig {
        seed: Some(Seed::from_u64(7)),
        node_names: vec![Arc::from("n1")],
        ..Default::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let oracle = sim.run(
        &events,
        Tick(last),
        Duration::from_nanos(1_000_000),
        &FaultSchedule::default(),
        false,
    );
    let actual = sim.run_on(
        &engine,
        &events,
        Tick(last),
        Duration::from_nanos(1_000_000),
        &FaultSchedule::default(),
        false,
    );
    match (oracle, actual) {
        (Ok(a), Ok(b)) => {
            assert_eq!(a.rounds.len(), b.rounds.len());
            for (x, y) in a.rounds.iter().zip(&b.rounds) {
                for (x, y) in x.iter().zip(y) {
                    assert_eq!(x.instance, y.instance);
                    assert_eq!(x.egress, y.egress);
                    assert_eq!(x.delivered, y.delivered);
                    assert_eq!(x.host, y.host);
                }
            }
            false
        }
        (
            Err(blossom_sim::sync::SimError::Node {
                node: a,
                tick: ta,
                error: blossom_oracle::OracleError::Program { error: ea, .. },
                ..
            }),
            Err(blossom_sim::sync::SimError::Node {
                node: b,
                tick: tb,
                error: blossom_oracle::OracleError::Program { error: eb, .. },
                ..
            }),
        ) => {
            assert_eq!((a, ta), (b, tb));
            assert_eq!(ea.code, eb.code);
            assert_eq!(ea.code.to_string(), "BLSR002");
            true
        }
        (a, b) => panic!(
            "oracle/engine mismatch: oracle={:?} engine={:?}",
            a.as_ref().err(),
            b.as_ref().err()
        ),
    }
}
#[test]
fn composite_upserts_guarded_carry_and_deletions_match_the_oracle() {
    for seed in 0..8u64 {
        let mut random = seed + 1;
        let mut inputs = vec![];
        for t in 0..40 {
            for a in 0..4 {
                for b in 0..3 {
                    let key = vec![Value::u64(a), Value::u64(b)];
                    random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                    if random & 1 == 0 {
                        inputs.push((t, "allow", key.clone()));
                    }
                    inputs.push((t, "watch", key.clone()));
                    if random & 4 != 0 {
                        let mut row = key.clone();
                        row.push(Value::Vec(Arc::from([Value::u64(t), Value::u64(seed)])));
                        inputs.push((t, "put", row.clone()));
                        if random & 8 != 0 {
                            inputs.push((t, "put", row));
                        }
                    }
                    if random & 16 != 0 {
                        inputs.push((t, "drop", key));
                    }
                }
            }
        }
        assert!(!check(inputs, 42));
    }
}
#[test]
fn conflicting_staged_payloads_have_the_oracles_error() {
    let row = |v| vec![Value::u64(1), Value::u64(2), Value::Vec(Arc::from([Value::u64(v)]))];
    assert!(check(vec![(0, "put", row(1)), (0, "put", row(2))], 2));
}
#[test]
fn an_identity_view_reuses_the_immutable_source_row() {
    use blossom_ir::tick::{Instance, StepInput};
    use blossom_value::time::Instant;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/engine/native_upsert.bls");
    let (result, _) = compile_file(
        path.to_str().unwrap(),
        &[NodeSpec {
            name: "n1".into(),
            role: None,
        }],
    );
    let artifact = result.unwrap().0;
    let mut engine = blossom_engine::Engine::new(artifact.program.clone(), NodeId(0), Default::default()).unwrap();
    engine.reset(Instance::default()).unwrap();
    let t = artifact.rel_named("t").unwrap();
    let view = artifact.rel_named("current").unwrap();
    for tick in 0..2u64 {
        let events = if tick == 0 {
            vec![(
                artifact.rel_named("put").unwrap(),
                Arc::from([Value::u64(1), Value::u64(2), Value::Vec(Arc::from([Value::u64(3)]))]),
            )]
        } else {
            vec![]
        };
        let out = engine
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
                &[t, view],
            )
            .unwrap();
        if tick == 1 {
            assert_eq!(out.observed[&t].len(), 1);
            assert!(Arc::ptr_eq(&out.observed[&t][0], &out.observed[&view][0]));
        }
    }
}

#[test]
fn upserts_with_another_next_state_writer_match_the_oracle() {
    let u = |v| Value::Int(blossom_value::IntValue::U64(v));
    let payload = |v| Value::Vec(Arc::from(vec![u(v)]));
    assert!(!check_fixture(
        "upsert_other_next",
        vec![
            (0, "put", vec![u(1), u(2), payload(3)]),
            (0, "extra", vec![u(4), u(5), payload(6)]),
            (1, "allow", vec![u(1), u(2)]),
            (1, "allow", vec![u(4), u(5)]),
            (1, "put", vec![u(1), u(2), payload(7)]),
            (2, "allow", vec![u(1), u(2)]),
            (2, "extra", vec![u(8), u(9), payload(10)]),
            (3, "drop", vec![u(1), u(2)]),
            (3, "allow", vec![u(8), u(9)]),
        ],
        6
    ));
}

#[test]
fn staged_next_state_is_read_without_duplicate_maintenance() {
    use blossom_ir::core::{ConstructKind, RuleKind};
    use blossom_value::time::Instant;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/engine/native_upsert.bls");
    let artifact = compile_file(
        path.to_str().unwrap(),
        &[NodeSpec {
            name: "n1".into(),
            role: None,
        }],
    )
    .0
    .unwrap()
    .0;
    let program = artifact.program.get();
    let copy = program
        .constructs
        .iter()
        .find_map(|c| {
            let ConstructKind::Upsert { .. } = c.kind else {
                return None;
            };
            c.rules
                .iter()
                .copied()
                .find(|id| program.rules.get(*id).is_some_and(|r| r.kind == RuleKind::Inductive))
        })
        .unwrap();
    let t = artifact.rel_named("t").unwrap();
    let mut engine = blossom_engine::Engine::new(
        artifact.program.clone(),
        NodeId(0),
        blossom_engine::EngineConfig {
            node_names: vec![Arc::from("n1")],
            ..Default::default()
        },
    )
    .unwrap();
    let u = |v| Value::Int(blossom_value::IntValue::U64(v));
    for tick in 0..4 {
        let events = if tick == 0 {
            vec![(
                artifact.rel_named("put").unwrap(),
                Arc::from(vec![u(1), u(2), Value::Vec(Arc::from(vec![u(3)]))]),
            )]
        } else {
            vec![(artifact.rel_named("allow").unwrap(), Arc::from(vec![u(1), u(2)]))]
        };
        engine
            .step(
                &blossom_ir::tick::StepInput {
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
        assert_eq!(engine.carried_rows(t).unwrap().len(), 1);
        assert!(engine.work_by_rule().get(&copy).is_none_or(|w| w.writes == 0));
        assert!(
            !engine
                .resident_by_store()
                .iter()
                .any(|(r, kind, _)| *r == t && *kind == "next")
        );
        if tick == 1 {
            let carried = engine.carried_instance().unwrap();
            engine.reset(carried).unwrap();
        }
    }
}
