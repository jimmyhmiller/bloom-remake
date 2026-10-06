//! S19: tick-scoped relations. A relation whose rules all read an event holds rows only in a tick with an event: the
//! engine empties it at the start of every tick and evaluates its rules from scratch while an event is there, instead
//! of retracting the last tick's rows one by one. The fixture reads such relations every way (an alternative of a view
//! that is not scoped, a negation, a chain, a join with a table, an aggregate, a `next`); the oracle and the engine
//! must agree at every tick, and a scoped rule does no work in a tick without its event.

use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_ir::tick::StepInput;
use blossom_node::EngineEvaluator;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_value::Value;
use blossom_value::time::{Duration, Instant, NodeId, Tick};
use blossom_value::value::IntValue;

/// (tick, relation, n): pokes in consecutive ticks, quiet ticks, stops.
const INPUTS: [(u64, &str, u64); 9] = [
    (1, "poke", 1),
    (2, "poke", 1),
    (2, "poke", 2),
    (4, "poke", 3),
    (4, "stop", 1),
    (5, "stop", 2),
    (6, "poke", 2),
    (7, "stop", 9),
    (9, "poke", 4),
];

#[cfg(test)]
fn compile() -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/engine/scoped.bls");
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    result.unwrap_or_else(|e| panic!("scoped.bls: {e:?}")).0
}

#[cfg(test)]
fn events(artifact: &BlsArtifact) -> Vec<InputEvent> {
    INPUTS
        .iter()
        .map(|(t, rel, n)| InputEvent {
            node: NodeId(0),
            tick: Tick(*t),
            rel: artifact.rel_named(rel).unwrap(),
            row: Arc::from(vec![Value::Int(IntValue::U64(*n))]),
        })
        .collect()
}

#[cfg(test)]
fn engine_config(artifact: &BlsArtifact) -> blossom_engine::EngineConfig {
    blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    }
}

#[test]
fn scoped_relations_agree_with_the_oracle_at_every_tick() {
    let artifact = compile();
    let sim = BlsSim::new(&artifact, blossom_value::Seed::from_u64(0)).unwrap();
    let round = Duration::from_nanos(1_000_000);
    let (inputs, last) = (events(&artifact), Tick(11));
    let reference = sim.run(&inputs, last, round, &FaultSchedule::default(), false).unwrap();
    let engine = EngineEvaluator::new(artifact.program.clone(), engine_config(&artifact));
    let mine = sim
        .run_on(&engine, &inputs, last, round, &FaultSchedule::default(), false)
        .unwrap();
    for (t, (a, b)) in reference.rounds.iter().zip(&mine.rounds).enumerate() {
        assert_eq!(
            a[0].instance, b[0].instance,
            "tick {t}: the oracle and the engine differ"
        );
    }
    // The relations hold what they should (a few spot checks; the oracle is the reference).
    let at = |t: usize, rel: &str| -> Vec<u64> {
        let mut out: Vec<u64> = reference.rounds[t][0]
            .instance
            .rows(artifact.rel_named(rel).unwrap())
            .map(|r| match &r[0] {
                Value::Int(IntValue::U64(x)) => *x,
                other => panic!("{other:?}"),
            })
            .collect();
        out.sort_unstable();
        out
    };
    assert_eq!(at(2, "shifted"), [3, 5], "the chain in a tick with two pokes");
    assert_eq!(at(3, "shifted"), Vec::<u64>::new(), "and gone in the quiet tick after");
    assert_eq!(at(3, "quiet"), [1, 2], "the negation holds again once the poke is gone");
    assert_eq!(
        at(2, "quiet"),
        Vec::<u64>::new(),
        "1 is poked again at 2; 2 is not seen yet"
    );
    assert_eq!(
        at(3, "mixed"),
        [1, 2],
        "the table alternative keeps what the event alternative no longer gives"
    );
}

#[test]
fn a_scoped_rule_does_no_work_in_a_tick_without_its_event() {
    let artifact = compile();
    let mut engine =
        blossom_engine::Engine::new(artifact.program.clone(), NodeId(0), engine_config(&artifact)).unwrap();
    let doubled = artifact
        .program
        .get()
        .rules
        .iter()
        .find(|r| r.head.rel == artifact.rel_named("doubled").unwrap())
        .map(|r| r.id)
        .unwrap();
    let poke = artifact.rel_named("poke").unwrap();
    let mut evals = Vec::new();
    for t in 0..6u64 {
        let events: Vec<_> = if t % 2 == 1 {
            vec![(poke, Arc::from(vec![Value::Int(IntValue::U64(t))]))]
        } else {
            Vec::new()
        };
        engine
            .step(
                &StepInput {
                    node: NodeId(0),
                    incarnation: 1,
                    tick: Tick(t),
                    now: Instant(t as i64),
                    events: &events,
                    delivered: &[],
                    ingress: &[],
                    blobs: &blossom_value::NoBlobs,
                },
                &[],
            )
            .unwrap();
        evals.push(engine.work_by_rule().get(&doubled).map_or(0, |w| w.evals));
    }
    // Evaluated in the three ticks with a poke only (the quiet ticks after them do not retract row by row).
    assert_eq!(evals, [0, 1, 1, 2, 2, 3]);
}
