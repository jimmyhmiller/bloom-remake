//! Compiled relational maintenance against the interpreter and independent oracle.
use blossom_driver::bls::compile_file;
use blossom_engine::{Engine, EngineConfig};
use blossom_front::api::NodeSpec;
use blossom_ir::tick::{Instance, StepInput};
use blossom_value::{
    Seed, Value,
    time::{Duration, Instant, NodeId, Tick},
};
use std::{path::Path, sync::Arc};

#[test]
fn compiled_deltas_preserve_negation_duplicates_self_joins_and_reset() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/engine/compiled_joins.bls");
    let a = compile_file(
        path.to_str().unwrap(),
        &[NodeSpec {
            name: "n1".into(),
            role: None,
        }],
    )
    .0
    .unwrap()
    .0;
    let mut replay = Vec::new();
    for workers in [1, 2] {
        let mut reference = Engine::new(a.program.clone(), NodeId(0), EngineConfig::default()).unwrap();
        let config = EngineConfig {
            compiled_joins: true,
            packed_indexes: true,
            workers,
            map_cache_rows: 64,
            map_cache_joins: true,
            ..Default::default()
        };
        let mut candidate = Engine::new(a.program.clone(), NodeId(0), config).unwrap();
        for t in 0..24u64 {
            if t == 12 {
                reference.reset(Instance::default()).unwrap();
                candidate.reset(Instance::default()).unwrap();
            }
            let mut events = Vec::new();
            let mut add = |name: &str, values: Vec<u64>| {
                events.push((
                    a.rel_named(name).unwrap(),
                    values.into_iter().map(Value::u64).collect::<Vec<_>>().into(),
                ))
            };
            for k in 0..80u64 {
                match (t + k) % 5 {
                    0 | 1 => {
                        add("put_left", vec![k, t % 4, if k % 2 == 0 { t % 4 } else { 9 }]);
                        add("put_right", vec![k, (t + k) % 9]);
                    }
                    2 => {
                        add("remove_right", vec![k]);
                        add("block", vec![k]);
                    }
                    3 => {
                        add("unblock", vec![k]);
                        add("put_right", vec![k, 7]);
                    }
                    _ => {
                        add("remove_left", vec![k]);
                        add("unblock", vec![k]);
                    }
                }
            }
            if workers == 1 && t < 12 {
                replay.extend(events.iter().map(|(rel, row)| blossom_sim::bls::InputEvent {
                    tick: Tick(t),
                    node: NodeId(0),
                    rel: *rel,
                    row: Arc::clone(row),
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
            assert_eq!(x.changes, y.changes, "tick {t}");
            assert!(
                candidate
                    .plan_report()
                    .stores
                    .iter()
                    .map(|s| s.packed_indexes)
                    .sum::<usize>()
                    > 0,
                "packed arrangements at tick {t}"
            );
            assert!(
                candidate
                    .plan_report()
                    .rules
                    .iter()
                    .map(|r| r.work.compiled_terms)
                    .sum::<u64>()
                    > 0
            );
            assert_eq!(
                reference
                    .plan_report()
                    .rules
                    .iter()
                    .map(|r| r.work.compiled_terms)
                    .sum::<u64>(),
                0
            );
            assert_eq!(x.outbox, y.outbox);
            assert_eq!(x.host, y.host);
            assert_eq!(
                reference.instance_now().unwrap(),
                candidate.instance_now().unwrap(),
                "tick {t}"
            );
        }
    }
    let sim = blossom_sim::bls::BlsSim::new(&a, Seed::from_u64(7)).unwrap();
    let executor = blossom_node::EngineEvaluator::new(
        a.program.clone(),
        EngineConfig {
            compiled_joins: true,
            packed_indexes: true,
            node_names: vec![Arc::from("n1")],
            seed: Some(Seed::from_u64(7)),
            ..Default::default()
        },
    );
    let faults = blossom_sim::FaultSchedule::default();
    let oracle = sim
        .run(&replay, Tick(11), Duration::from_nanos(1_000_000_000), &faults, false)
        .unwrap();
    let compiled = sim
        .run_on(
            &executor,
            &replay,
            Tick(11),
            Duration::from_nanos(1_000_000_000),
            &faults,
            false,
        )
        .unwrap();
    assert_eq!(oracle.rounds.len(), compiled.rounds.len());
    for (a, b) in oracle.rounds.iter().zip(&compiled.rounds) {
        for (a, b) in a.iter().zip(b) {
            assert_eq!(a.instance, b.instance);
        }
    }
}
