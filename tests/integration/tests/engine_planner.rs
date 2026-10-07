//! Slice 5: the engine's join planner against the reference oracle. Each fixture in `fixtures/engine/` exercises one
//! planning decision whose exactness is subtle: a `let` hoisted before an atom (its error counts only for complete
//! valuations), a range probe from a guard (not past a check that can fail), and range probes over an ordered index
//! as rows are inserted and deleted; and the semi-naive evaluation of recursive strata (chains, mutual recursion, a
//! recursive table with carried rows, an error inside a recursion, and the cost of a long chain). The engine must
//! agree with the oracle at every tick: the same rows, or the same error at the same tick.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_ir::tick::{Instance, TickInput};
use blossom_node::EngineEvaluator;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_sim::sync::{SimError, SyncRun};
use blossom_value::Value;
use blossom_value::time::{Duration, NodeId, Tick};
use blossom_value::value::IntValue;

#[cfg(test)]
fn compile(name: &str) -> BlsArtifact {
    compile_on(
        name,
        &[NodeSpec {
            name: "n1".to_owned(),
            role: None,
        }],
    )
}

#[cfg(test)]
fn compile_on(name: &str, nodes: &[NodeSpec]) -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/engine").join(name);
    let (result, _) = compile_file(path.to_str().unwrap(), nodes);
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
    let rows: Vec<(u64, u32, &str, Vec<Value>)> = inputs
        .iter()
        .map(|(tick, rel, v)| (*tick, 0, *rel, vec![Value::Int(IntValue::U64(*v))]))
        .collect();
    differential_on(
        name,
        &[NodeSpec {
            name: "n1".to_owned(),
            role: None,
        }],
        &rows,
        last,
    )
}

/// [`differential`] on a deployment of `nodes`, with input rows `(tick, node, relation, row)`.
#[cfg(test)]
fn differential_on(name: &str, nodes: &[NodeSpec], inputs: &[(u64, u32, &str, Vec<Value>)], last: u64) -> Outcome {
    let artifact = compile_on(name, nodes);
    let sim = BlsSim::new(&artifact, blossom_value::Seed::from_u64(0)).unwrap();
    let inputs: Vec<InputEvent> = inputs
        .iter()
        .map(|(tick, node, rel, row)| InputEvent {
            node: NodeId(*node),
            tick: Tick(*tick),
            rel: artifact.rel_named(rel).unwrap(),
            row: Arc::from(row.clone()),
        })
        .collect();
    let round = Duration::from_nanos(1_000_000_000);
    let reference = sim.run(&inputs, Tick(last), round, &FaultSchedule::default(), false);
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
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

/// A guard that cannot fail runs before a division written ahead of it (LANGUAGE §9.14): rows it rejects raise
/// nothing, rows it accepts do.
#[test]
fn a_range_guard_runs_before_a_check_that_can_fail() {
    assert!(matches!(
        differential("range_after_fallible.bls", &[(1, "s", 0)], 2),
        Outcome::Ran(_)
    ));
    assert!(matches!(
        differential("range_after_fallible.bls", &[(1, "add", 7), (2, "s", 0)], 3),
        Outcome::Failed { tick: Tick(2), code } if code == "BLSR004"
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

#[cfg(test)]
fn u8v(x: u8) -> Value {
    Value::Int(IntValue::U8(x))
}

/// A runtime error raised by a check before a negation counts for the valuation whatever the negation says, even in
/// the tick the negation stops holding (found by the S5 review: the engine cancelled it against the flip).
#[test]
fn an_error_before_a_negation_is_raised_in_the_tick_the_negation_flips() {
    let solo = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let both = [(1, 0, "ia", vec![u8v(0)]), (1, 0, "ib", vec![u8v(0)])];
    assert!(matches!(
        differential_on("error_before_a_flipping_negation.bls", &solo, &both, 3),
        Outcome::Failed { tick: Tick(1), code } if code == "BLSR004"
    ));
    // A persistent row whose error becomes reachable as one negation starts holding and a later one stops.
    let flips = [(2, 0, "unblock", vec![u8v(0)]), (1, 0, "mark", vec![u8v(0)])];
    assert!(matches!(
        differential_on("error_between_flipping_negations.bls", &solo, &flips, 4),
        Outcome::Failed { code, .. } if code == "BLSR004"
    ));
}

/// A point lattice's cell whose only contribution changes from 5 to 3 in one tick is 3, not a conflict: the retraction
/// and the addition are one change (found by the S5 review).
#[test]
fn a_point_cell_changing_its_contribution_is_not_a_conflict() {
    let solo = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let set = [(2, 0, "set", vec![u8v(3)])];
    let set64 = [(2, 0, "set", vec![Value::Int(IntValue::U64(3))])];
    assert!(matches!(differential_on("point_scratch_changes.bls", &solo, &set64, 4), Outcome::Ran(_)));
    assert!(matches!(differential_on("point_view_changes.bls", &solo, &set, 4), Outcome::Ran(_)));
    let pair: Vec<NodeSpec> = (0..2)
        .map(|i| NodeSpec {
            name: format!("p{i}"),
            role: Some("P".to_owned()),
        })
        .collect();
    assert!(matches!(differential_on("point_send_changes.bls", &pair, &set, 4), Outcome::Ran(_)));
}

/// Rules that read the time only by comparing `now()` with an instant (each operator, `now()` on either side, under
/// `&&`), and a rule with no positive atom, agree with the oracle at every tick: the engine skips them while nothing
/// they read changes and the time is before their next flip, and must re-evaluate exactly at the flip.
#[test]
fn rules_skipped_until_the_time_flips_them_agree_with_the_oracle() {
    let inputs = [(1, "seen", 1), (2, "seen", 2), (6, "forget", 1), (7, "seen", 1), (9, "forget", 2)];
    let Outcome::Ran(run) = differential("time_guards.bls", &inputs, 14) else {
        panic!("time_guards.bls failed");
    };
    // The views change at ticks with no input: the skipped rules did come back.
    let artifact = compile("time_guards.bls");
    let fresh = artifact.rel_named("fresh").unwrap();
    let count = |t: u64| run.node_tick(Tick(t), NodeId(0)).unwrap().instance.rows(fresh).count();
    assert!((3..6).any(|t| count(t) != count(t + 1)), "fresh never changed between inputs");
}

#[test]
fn rand_range_draws_spans_above_2_64_on_the_engine() {
    let solo = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let big = 100_000_000_000_000_000_000i128;
    let go = [(1, 0, "go", vec![Value::Int(IntValue::I128(-big)), Value::Int(IntValue::I128(big))])];
    assert!(matches!(differential_on("rand_range_wide.bls", &solo, &go, 2), Outcome::Ran(_)));
}

/// A lattice reply channel merges per session and key, like one to a node (§14.2): two contributions to one key in a
/// tick are one reply, on the oracle and on the engine (found by the S5 review: the oracle kept both).
#[test]
fn a_lattice_reply_merges_per_session_and_key() {
    use blossom_ir::tick::{Ingress, Instance, TickInput};
    use blossom_node::Evaluator;
    let nodes = [NodeSpec {
        name: "server0".to_owned(),
        role: Some("Server".to_owned()),
    }];
    let artifact = compile_on("lattice_reply_merges.bls", &nodes);
    let seed = blossom_value::Seed::from_u64(0);
    let oracle = blossom_oracle::Oracle::new(artifact.program.clone())
        .unwrap()
        .with_roles(artifact.roles.clone())
        .with_seed(seed)
        .unwrap()
        .with_node_names(vec![Arc::from("server0")])
        .unwrap();
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: vec![Arc::from("server0")],
        seed: Some(seed),
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let ask = artifact.rel_named("ask").unwrap();
    let ingress = [Ingress {
        rel: ask,
        session: blossom_value::value::SessionId(7),
        row: Arc::from(vec![Value::Node(NodeId(0)), u8v(3)]),
    }];
    let carried = Instance::default();
    let tick = TickInput {
        node: NodeId(0),
        incarnation: 1,
        tick: Tick(0),
        now: blossom_value::time::Instant(0),
        carried: &carried,
        events: &[],
        delivered: &[],
        ingress: &ingress,
        capture: false,
        blobs: &blossom_value::NoBlobs,
    };
    let o = oracle.tick(&tick).unwrap();
    let e = engine.tick(&tick).unwrap();
    assert_eq!(o.egress, e.egress);
    assert_eq!(o.egress.len(), 1, "{:?}", o.egress);
}

#[test]
fn integer_casts_convert_and_are_checked() {
    let solo = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let u16v = |x: u16| Value::Int(IntValue::U16(x));
    let Outcome::Ran(run) = differential_on("integer_casts.bls", &solo, &[(1, 0, "e", vec![u16v(200)])], 2) else {
        panic!("an in-range cast failed");
    };
    let artifact = compile("integer_casts.bls");
    let narrowed: Vec<_> = run
        .node_tick(Tick(1), NodeId(0))
        .unwrap()
        .instance
        .rows(artifact.rel_named("narrowed").unwrap())
        .cloned()
        .collect();
    assert_eq!(narrowed, vec![Arc::from(vec![Value::Int(IntValue::U8(200))])]);
    assert!(matches!(
        differential_on("integer_casts.bls", &solo, &[(1, 0, "e", vec![u16v(300)])], 2),
        Outcome::Failed { tick: Tick(1), code } if code == "BLSR004"
    ));
}

/// The rows of `rel` in the single node's instance at tick `t` of a run.
#[cfg(test)]
fn rows_at(run: &SyncRun, artifact_name: &str, rel: &str, t: usize) -> Vec<Vec<Value>> {
    let artifact = compile(artifact_name);
    let rel = artifact.rel_named(rel).unwrap();
    run.rounds[t][0].instance.rows(rel).map(|r| r.to_vec()).collect()
}

/// A recursive view walking a table as a chain (a Kafka fetch's batches), evaluated semi-naively: the engine agrees
/// with the oracle as the chain grows, breaks where a batch is removed, and is mended.
#[test]
fn a_recursive_chain_agrees_with_the_oracle() {
    let mut inputs: Vec<(u64, &str, u64)> = (0..30).map(|b| (0, "add", b)).collect();
    // A delete takes effect at the next tick; an add (`emit`) at its own.
    inputs.extend([
        (1, "ask", 1000),
        (2, "ask", 31),
        (3, "remove", 10),
        (5, "add", 10),
        (6, "remove", 0),
        (8, "add", 0),
    ]);
    let Outcome::Ran(run) = differential("recursive_chain.bls", &inputs, 9) else {
        panic!("recursive_chain.bls failed")
    };
    let u = |x: u64| Value::Int(IntValue::U64(x));
    let len = |t: usize| rows_at(&run, "recursive_chain.bls", "chain_len", t);
    assert!(len(1).contains(&vec![u(1000), u(30)]), "{:?}", len(1));
    assert!(len(2).contains(&vec![u(31), u(16)]), "{:?}", len(2));
    assert!(len(4).contains(&vec![u(1000), u(10)]), "{:?}", len(4));
    assert!(len(5).contains(&vec![u(1000), u(30)]), "{:?}", len(5));
    assert!(len(7).is_empty(), "{:?}", len(7));
    assert!(len(8).contains(&vec![u(1000), u(30)]), "{:?}", len(8));
}

/// Mutual recursion over a graph with cycles, as edges come and go.
#[test]
fn mutual_recursion_agrees_with_the_oracle() {
    let mut inputs: Vec<(u64, &str, u64)> = (0..17).map(|x| (0, "link", x)).collect();
    inputs.extend([(1, "start", 0), (2, "unlink", 3), (3, "start", 5), (4, "unlink", 0), (5, "link", 3)]);
    assert!(matches!(differential("recursive_mutual.bls", &inputs, 7), Outcome::Ran(_)));
}

/// A table its own rules extend within the tick, with rows carried and seeded from outside the recursion.
#[test]
fn a_recursive_table_with_carried_rows_agrees_with_the_oracle() {
    let mut inputs: Vec<(u64, &str, u64)> = (0..13).map(|x| (0, "link", x)).collect();
    inputs.extend([(1, "seed", 0), (2, "unlink", 1), (3, "seed", 7), (4, "unlink", 7), (5, "seed", 2)]);
    assert!(matches!(differential("recursive_table.bls", &inputs, 7), Outcome::Ran(_)));
}

/// A recursive step that divides by zero partway along the chain: the engine fails at the oracle's tick with its
/// error (the semi-naive iteration hands an erring stratum to the naive one, which reports errors at the fixpoint).
#[test]
fn an_error_inside_a_recursion_is_the_oracles() {
    let mut inputs: Vec<(u64, &str, u64)> = (0..15).map(|b| (0, "add", b)).collect();
    inputs.push((1, "ask", 1000));
    assert!(matches!(differential("recursive_error.bls", &inputs, 3), Outcome::Failed { .. }));
    let mut short: Vec<(u64, &str, u64)> = (0..15).map(|b| (0, "add", b)).collect();
    short.push((1, "ask", 15));
    differential("recursive_error.bls", &short, 3);
}

/// The rows the engine examines to walk a chain of `n` batches once.
#[cfg(test)]
fn chain_work(n: u64) -> u64 {
    use blossom_ir::tick::StepInput;
    use blossom_value::time::Instant;
    let artifact = compile("recursive_chain.bls");
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|x| Arc::from(x.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let mut engine = blossom_engine::Engine::new(artifact.program.clone(), NodeId(0), cfg).unwrap();
    let rel = |r: &str| artifact.rel_named(r).unwrap();
    let u = |x: u64| -> blossom_ir::tick::Row { Arc::from(vec![Value::Int(IntValue::U64(x))]) };
    let step = |engine: &mut blossom_engine::Engine, tick: u64, events: Vec<(blossom_base::RelId, blossom_ir::tick::Row)>| {
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
    };
    step(&mut engine, 0, (0..n).map(|b| (rel("add"), u(b))).collect());
    let before = engine.rows_examined();
    step(&mut engine, 1, vec![(rel("ask"), u(u64::MAX))]);
    let work = engine.rows_examined() - before;
    let len = engine.carried_rows(rel("asked")).unwrap().len();
    assert_eq!(len, 1);
    work
}

/// A recursive chain costs in proportion to its length: each step's valuation is joined about once, not once per
/// round of the fixpoint (the naive iteration's n rounds of n rows made a Kafka fetch of a few hundred batches take
/// seconds). Counted in rows examined, so the check is exact and machine-independent.
#[test]
fn a_recursive_chain_costs_its_length_not_its_square() {
    let (small, large) = (chain_work(400), chain_work(800));
    assert!(large < 3 * small, "a chain of 400 examined {small} rows, of 800 {large}");
    assert!(large < 40 * 800, "a chain of 800 examined {large} rows");
}

/// A later binding keys a probe only past checks that cannot fail: a fallible guard ahead of it still sees, and
/// raises its error on, the rows the binding would reject.
#[test]
fn a_binding_keys_a_probe_only_past_checks_that_cannot_fail() {
    let fails = [(0, "put", 0), (0, "put", 6), (1, "go", 5)];
    assert!(matches!(
        differential("key_after_fallible.bls", &fails, 2),
        Outcome::Failed { tick: Tick(1), code } if code == "BLSR004"
    ));
    let Outcome::Ran(run) = differential("key_after_fallible.bls", &[(0, "put", 6), (0, "put", 7), (1, "go", 5)], 2)
    else {
        panic!("key_after_fallible.bls failed without a zero")
    };
    let u = |x: u64| Value::Int(IntValue::U64(x));
    assert_eq!(rows_at(&run, "key_after_fallible.bls", "out", 1), vec![vec![u(5), u(6)]]);
}

/// A recursion that never converges fails with BLSR007 once the rounds reach the bound (CR-53); the semi-naive
/// iteration hands it to the naive one, which reports it.
#[test]
fn a_recursion_that_never_converges_fails_at_the_round_bound() {
    use blossom_ir::tick::StepInput;
    use blossom_value::time::Instant;
    let artifact = compile("recursive_unbounded.bls");
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|x| Arc::from(x.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        max_rounds: 40,
        ..blossom_engine::EngineConfig::default()
    };
    let mut engine = blossom_engine::Engine::new(artifact.program.clone(), NodeId(0), cfg).unwrap();
    let events = vec![(artifact.rel_named("start").unwrap(), Arc::from(vec![Value::Int(IntValue::U64(0))]))];
    let r = engine.step(
        &StepInput {
            node: NodeId(0),
            incarnation: 1,
            tick: Tick(0),
            now: Instant(0),
            events: &events,
            delivered: &[],
            ingress: &[],
            blobs: &blossom_value::NoBlobs,
        },
        &[],
    );
    match r {
        Err(blossom_ir::tick::EvalError::Program { error, .. }) => assert_eq!(error.code, "BLSR007"),
        other => panic!("expected BLSR007, got {:?}", other.map(|_| ())),
    }
}

/// One tick of `name` from an empty state with input `events` (relation, u64), on the oracle and on the engine, both
/// bounded to `max_rounds` per recursive stratum: the instance, or the program error's code.
#[cfg(test)]
fn bounded_tick(
    name: &str,
    events: &[(&str, u64)],
    max_rounds: u32,
) -> (Result<Instance, String>, Result<Instance, String>) {
    use blossom_node::Evaluator;
    let artifact = compile(name);
    let seed = blossom_value::Seed::from_u64(0);
    let oracle = blossom_oracle::Oracle::with_limits(artifact.program.clone(), blossom_oracle::Limits { max_rounds })
        .unwrap()
        .with_roles(artifact.roles.clone())
        .with_seed(seed)
        .unwrap()
        .with_node_names(vec![Arc::from("n1")])
        .unwrap();
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: vec![Arc::from("n1")],
        seed: Some(seed),
        max_rounds,
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let events: Vec<(blossom_base::RelId, blossom_ir::tick::Row)> = events
        .iter()
        .map(|(r, v)| (artifact.rel_named(r).unwrap(), Arc::from(vec![Value::Int(IntValue::U64(*v))])))
        .collect();
    let carried = Instance::default();
    let input = TickInput {
        node: NodeId(0),
        incarnation: 1,
        tick: Tick(0),
        now: blossom_value::time::Instant(0),
        carried: &carried,
        events: &events,
        delivered: &[],
        ingress: &[],
        capture: false,
        blobs: &blossom_value::NoBlobs,
    };
    let o = oracle.tick(&input).map(|out| out.instance).map_err(|e| match e {
        blossom_oracle::OracleError::Program { error, .. } => error.code.to_string(),
        other => panic!("{name}: the oracle failed: {other}"),
    });
    let e = engine.tick(&input).map(|out| out.instance).map_err(|e| match e {
        blossom_ir::tick::EvalError::Program { error, .. } => error.code.to_string(),
        other => panic!("{name}: the engine failed: {other}"),
    });
    (o, e)
}

/// The semi-naive iteration goes through the naive iteration's rounds: under every round bound, from one round to
/// past the fixpoint, the engine converges exactly when the oracle (the naive iteration) does, to the same instance,
/// and fails with the same error when it does not (BLSR007, or an error a valuation raises at the fixpoint).
#[test]
fn semi_naive_rounds_are_the_naive_rounds() {
    let chain: Vec<(&str, u64)> = (0..12).map(|b| ("add", b)).chain([("ask", 1000)]).collect();
    let mutual: Vec<(&str, u64)> = (0..17).map(|x| ("link", x)).chain([("start", 0)]).collect();
    let table: Vec<(&str, u64)> = (0..13).map(|x| ("link", x)).chain([("seed", 0)]).collect();
    let error: Vec<(&str, u64)> = (0..15).map(|b| ("add", b)).chain([("ask", 1000)]).collect();
    for (name, events) in [
        ("recursive_chain.bls", &chain),
        ("recursive_mutual.bls", &mutual),
        ("recursive_table.bls", &table),
        ("recursive_error.bls", &error),
    ] {
        let mut outcomes = BTreeSet::new();
        for k in 1..=30 {
            let (o, e) = bounded_tick(name, events, k);
            assert_eq!(o, e, "{name} with at most {k} rounds");
            outcomes.insert(o.as_ref().err().cloned().unwrap_or_else(|| "converged".into()));
        }
        // The sweep reaches both sides of the bound.
        assert!(outcomes.contains("BLSR007") && outcomes.len() > 1, "{name}: {outcomes:?}");
    }
}
