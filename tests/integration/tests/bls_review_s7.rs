//! Regression tests for the slice 7 adversarial review of the language: each fixture in `fixtures/review7/` is one
//! confirmed wrong answer of the compiler (a program wrongly refused, a missing diagnostic, an internal error), with
//! the answer it must give.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use blossom_driver::bls::compile_file;
use blossom_front::api::{BlsError, NodeSpec};
use blossom_node::EngineEvaluator;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_value::Value;
use blossom_value::time::{Duration, NodeId, Tick};
use blossom_value::value::IntValue;

#[cfg(test)]
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/review7")
        .join(name)
}

/// The deployment the fixtures compile for: one node per role they declare, `n0` for the first.
#[cfg(test)]
fn nodes_for(name: &str) -> Vec<NodeSpec> {
    let text = std::fs::read_to_string(fixture(name)).unwrap();
    let roles: Vec<String> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("role "))
        .filter_map(|l| l.split(':').next())
        .map(|r| r.trim().to_owned())
        .collect();
    if roles.is_empty() {
        return vec![NodeSpec {
            name: "n0".to_owned(),
            role: None,
        }];
    }
    roles
        .iter()
        .enumerate()
        .map(|(i, r)| NodeSpec {
            name: format!("n{i}"),
            role: Some(r.clone()),
        })
        .collect()
}

/// The codes of the errors compiling a fixture reports (empty when it compiles), each with its message.
#[cfg(test)]
fn compile_diags(name: &str) -> Vec<(String, String)> {
    let (result, _) = compile_file(fixture(name).to_str().unwrap(), &nodes_for(name));
    match result {
        Ok(_) => Vec::new(),
        Err(BlsError::Rejected(d)) => d
            .iter()
            .map(|x| (x.code.as_str().to_owned(), x.message.clone()))
            .collect(),
        Err(e) => panic!("{name}: {e}"),
    }
}

/// Asserts that compiling a fixture reports exactly the errors `want` (none: it compiles), showing the messages
/// when it does not.
#[cfg(test)]
fn expect_codes(name: &str, want: &[&str]) {
    let diags = compile_diags(name);
    let codes: Vec<&str> = diags.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(codes, want, "{name}: {diags:?}");
}

#[test]
fn a_generator_binds_a_copy_of_the_element() {
    expect_codes("gen_copies_element.bls", &[]);
}

#[test]
fn a_block_condition_refines_a_variable_only_inside_the_block() {
    expect_codes("block_refines_inside.bls", &[]);
    expect_codes("block_refines_only_inside.bls", &["BLS0300"]);
}

#[test]
fn min_over_alternatives_of_different_roles_holds_their_join() {
    expect_codes("min_over_roles.bls", &[]);
}

#[test]
fn a_per_view_aggregate_without_an_identity_needs_a_default() {
    expect_codes("per_min_without_default.bls", &["BLS0511"]);
}

/// The rows of `out` at tick 1 of node `n0` when `go(row)` arrives then, computed by both evaluators, which agree.
#[cfg(test)]
fn out_rows(name: &str, row: Vec<Value>) -> Vec<Vec<Value>> {
    let (result, _) = compile_file(fixture(name).to_str().unwrap(), &nodes_for(name));
    let artifact = result.unwrap_or_else(|e| panic!("{name}: {e:?}")).0;
    let inputs = [InputEvent {
        node: NodeId(0),
        tick: Tick(1),
        rel: artifact.rel_named("go").unwrap(),
        row: Arc::from(row),
    }];
    let sim = BlsSim::new(&artifact, blossom_value::Seed::from_u64(0)).unwrap();
    let round = Duration::from_nanos(1_000_000_000);
    let reference = sim
        .run(&inputs, Tick(2), round, &FaultSchedule::default(), false)
        .unwrap();
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let mine = sim
        .run_on(&engine, &inputs, Tick(2), round, &FaultSchedule::default(), false)
        .unwrap();
    for (a, b) in reference.rounds.iter().zip(&mine.rounds) {
        for (x, y) in a.iter().zip(b) {
            assert_eq!(x.instance, y.instance, "the oracle and the engine differ");
        }
    }
    reference
        .node_tick(Tick(1), NodeId(0))
        .unwrap()
        .instance
        .rows(artifact.rel_named("out").unwrap())
        .map(|r| r.to_vec())
        .collect()
}

/// Comparisons of values whose types differ in roles compile, and both evaluators compute them.
#[test]
fn comparisons_relate_their_operands() {
    let out = out_rows(
        "compare_relates.bls",
        vec![Value::Node(NodeId(0)), Value::Node(NodeId(1))],
    );
    let (t, f) = (Value::Bool(true), Value::Bool(false));
    let two = Value::Int(IntValue::U64(2));
    assert_eq!(
        out,
        vec![vec![
            t.clone(),   // both_in
            f.clone(),   // member: n0 is not in {n1}
            t.clone(),   // has_key
            t.clone(),   // n0 < n1
            f.clone(),   // n0 >= n1
            t.clone(),   // n0 == n1 || (n0, 1) != (n1, 2)
            two.clone(), // [n0] ++ [n1]
            two,         // map[n0 => 1, n1 => 2]
            t,           // (2, "a") > (1, "b")
            f,           // (1, "a") >= (1, "b")
        ]]
    );
}

#[test]
fn the_library_additions_compute_on_both_evaluators() {
    let out = out_rows("library_additions.bls", vec![Value::Int(IntValue::U64(5))]);
    let u = |x: u64| Value::Int(IntValue::U64(x));
    let v = |xs: &[u64]| Value::Vec(xs.iter().map(|x| u(*x)).collect());
    assert_eq!(
        out,
        vec![vec![
            v(&[5, 6, 7]),
            v(&[6, 8, 11]),
            v(&[]),
            u(2),
            Value::some(Value::Str("c".into())),
            Value::none(),
        ]]
    );
}
