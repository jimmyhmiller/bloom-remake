//! Slice 5: the engine's join planner against the reference oracle. Each fixture in `fixtures/engine/` exercises one
//! planning decision whose exactness is subtle: a `let` hoisted before an atom (its error counts only for complete
//! valuations), a range probe from a guard (not past a check that can fail), and range probes over an ordered index
//! as rows are inserted and deleted. The engine must agree with the oracle at every tick: the same rows, or the same
//! error at the same tick.

use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::EngineEvaluator;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_sim::sync::{SimError, SyncRun};
use blossom_value::Value;
use blossom_value::time::{Duration, NodeId, Tick};
use blossom_value::value::IntValue;

#[cfg(test)]
fn compile(name: &str) -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/engine").join(name);
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    result.unwrap_or_else(|e| panic!("{name}: {e:?}")).0
}

/// What a run did: every tick, or a program error at a tick.
#[cfg(test)]
enum Outcome {
    Ran(SyncRun),
    Failed { tick: Tick, code: String },
}

/// Runs `name` on the oracle and on the engine; they must agree. Returns the oracle's outcome.
#[cfg(test)]
fn differential(name: &str, inputs: &[(u64, &str, u64)], last: u64) -> Outcome {
    let artifact = compile(name);
    let sim = BlsSim::new(&artifact, blossom_value::Seed::from_u64(0)).unwrap();
    let inputs: Vec<InputEvent> = inputs
        .iter()
        .map(|(tick, rel, v)| InputEvent {
            node: NodeId(0),
            tick: Tick(*tick),
            rel: artifact.rel_named(rel).unwrap(),
            row: Arc::from(vec![Value::Int(IntValue::U64(*v))]),
        })
        .collect();
    let round = Duration::from_nanos(1_000_000_000);
    let reference = sim.run(&inputs, Tick(last), round, &FaultSchedule::default(), false);
    let cfg = blossom_engine::EngineConfig {
        node_names: vec![Arc::from("n1")],
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let mine = sim.run_on(&engine, &inputs, Tick(last), round, &FaultSchedule::default(), false);
    match (&reference, &mine) {
        (Ok(a), Ok(b)) => {
            for (t, (ra, rb)) in a.rounds.iter().zip(&b.rounds).enumerate() {
                for (x, y) in ra.iter().zip(rb) {
                    assert_eq!(x.instance, y.instance, "{name}: tick {t}");
                }
            }
            assert_eq!(a.rounds.len(), b.rounds.len(), "{name}");
        }
        (Err(SimError::Node { tick: t1, error: e1, .. }), Err(SimError::Node { tick: t2, error: e2, .. })) => {
            assert_eq!(t1, t2, "{name}: the oracle failed at {t1:?} ({e1}), the engine at {t2:?} ({e2})");
            assert_eq!(code_of(&reference), code_of(&mine), "{name}: {e1} against {e2}");
        }
        (a, b) => panic!("{name}: the oracle gave {:?}, the engine {:?}", a.as_ref().err(), b.as_ref().err()),
    }
    match reference {
        Ok(run) => Outcome::Ran(run),
        Err(SimError::Node { tick, .. }) => Outcome::Failed {
            tick,
            code: code_of(&mine).unwrap_or_default(),
        },
        Err(e) => panic!("{name}: {e}"),
    }
}

#[cfg(test)]
fn code_of(r: &Result<SyncRun, SimError>) -> Option<String> {
    match r {
        Err(SimError::Node {
            error: blossom_oracle::OracleError::Program { error, .. },
            ..
        }) => Some(error.code.to_string()),
        _ => None,
    }
}

#[test]
fn a_hoisted_let_raises_only_for_complete_valuations() {
    assert!(matches!(differential("hoisted_let_no_completion.bls", &[], 1), Outcome::Ran(_)));
    assert!(matches!(
        differential("hoisted_let_completed.bls", &[], 1),
        Outcome::Failed { code, .. } if code == "BLSR004"
    ));
}

#[test]
fn a_range_guard_does_not_narrow_past_a_check_that_can_fail() {
    assert!(matches!(
        differential("range_after_fallible.bls", &[(1, "s", 0)], 2),
        Outcome::Failed { tick: Tick(1), code } if code == "BLSR004"
    ));
}

#[test]
fn range_probes_follow_inserts_deletes_and_a_moving_bound() {
    let mut inputs = Vec::new();
    for i in 1..=12u64 {
        inputs.push((1, "add", i));
    }
    inputs.extend([(3, "mark", 2), (4, "drop", 4), (5, "mark", 6), (6, "drop", 8), (6, "add", 20), (7, "mark", 17)]);
    let Outcome::Ran(run) = differential("range_probe.bls", &inputs, 9) else {
        panic!("range_probe.bls failed");
    };
    let artifact = compile("range_probe.bls");
    let above = artifact.rel_named("above").unwrap();
    let at = |t: u64| -> Vec<u64> {
        let mut v: Vec<u64> = run
            .node_tick(Tick(t), NodeId(0))
            .unwrap()
            .instance
            .rows(above)
            .map(|r| match r.first() {
                Some(Value::Int(IntValue::U64(x))) => *x,
                other => panic!("{other:?}"),
            })
            .collect();
        v.sort_unstable();
        v
    };
    // Tick 2: the adds land; low 0. Tick 4: low 2 (entry 4 is deleted from tick 5). Tick 6: low 6. Tick 7: 8 is
    // gone and 20 arrived. Tick 8: low 17.
    assert_eq!(at(2), vec![1, 2, 3]);
    assert_eq!(at(4), vec![3, 4, 5]);
    assert_eq!(at(5), vec![3, 5]);
    assert_eq!(at(6), vec![7, 8, 9]);
    assert_eq!(at(7), vec![7, 9]);
    assert_eq!(at(8), vec![20]);
}
