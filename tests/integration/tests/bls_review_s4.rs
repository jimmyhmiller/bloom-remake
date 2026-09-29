//! Regression tests for the slice 4 adversarial review: each fixture in `fixtures/review4/` is one confirmed wrong
//! answer of the compiler (a missing diagnostic, an internal error), with the answer it must give.

use std::path::{Path, PathBuf};

use blossom_driver::bls::compile_file;
use blossom_front::api::{BlsError, NodeSpec};
use blossom_sim::FaultSchedule;
use blossom_sim::bls::BlsSim;
use blossom_value::Value;
use blossom_value::time::{Duration, Tick};
use blossom_value::value::IntValue;

#[cfg(test)]
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/review4")
        .join(name)
}

/// The deployment the fixtures compile for: one node per role they declare.
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
            name: "n1".to_owned(),
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

/// The codes of the errors compiling a program root reports (empty when it compiles).
#[cfg(test)]
fn compile_codes(name: &str) -> Vec<String> {
    let (result, _) = compile_file(fixture(name).to_str().unwrap(), &nodes_for(name));
    match result {
        Ok(_) => Vec::new(),
        Err(BlsError::Rejected(d)) => d.iter().map(|x| x.code.as_str().to_owned()).collect(),
        Err(e) => panic!("{name}: {e}"),
    }
}

#[test]
fn a_durable_write_in_a_plain_bootstrap_is_rejected() {
    assert_eq!(compile_codes("durable_in_plain_bootstrap.bls"), vec!["BLS0402"]);
    assert_eq!(compile_codes("durable_in_fresh_bootstrap.bls"), Vec::<String>::new());
}

#[test]
fn rand_range_needs_its_key() {
    assert_eq!(compile_codes("rand_range_without_key.bls"), vec!["BLS0301"]);
}

#[test]
fn majority_counts_a_set_like_lattice() {
    assert_eq!(compile_codes("majority_over_set.bls"), vec!["BLS0300"]);
}

#[test]
fn an_lset_of_self_compiles() {
    assert_eq!(compile_codes("lset_of_self.bls"), Vec::<String>::new());
}

#[test]
fn rand_range_draws_over_ranges_wider_than_64_bits() {
    let name = "rand_range_wide.bls";
    let (result, _) = compile_file(fixture(name).to_str().unwrap(), &nodes_for(name));
    let artifact = result.unwrap().0;
    let sim = BlsSim::new(&artifact, blossom_value::Seed::from_u64(3)).unwrap();
    let run = sim
        .run(
            &[],
            Tick(0),
            Duration::from_nanos(1_000_000_000),
            &FaultSchedule::default(),
            false,
        )
        .unwrap();
    let inst = &run.rounds.first().unwrap().first().unwrap().instance;
    let one = |rel: &str| -> Value {
        let rows: Vec<_> = inst.rows(artifact.rel_named(rel).unwrap()).collect();
        assert_eq!(rows.len(), 1, "{rel}: {rows:?}");
        rows.first().unwrap().first().unwrap().clone()
    };
    assert!(matches!(one("signed"), Value::Int(IntValue::I128(_))));
    match one("unsigned") {
        Value::Int(IntValue::U128(x)) => {
            assert!((340_282_366_920_938_463_463_374_607_431_768_211_000..u128::MAX).contains(&x))
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(one("full64"), Value::Int(IntValue::I64(x)) if x < i64::MAX));
}

/// Compiles a fixture with deployment bindings and returns each relation's rows at tick 0, as text.
#[cfg(test)]
fn boot_rows(name: &str, params: &[(&str, blossom_front::api::ParamBinding)]) -> Vec<(String, String)> {
    let params = params.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect();
    let (result, _) =
        blossom_driver::bls::compile_file_with(fixture(name).to_str().unwrap(), &nodes_for(name), &params);
    let artifact = result.unwrap_or_else(|e| panic!("{name}: {e}")).0;
    let sim = BlsSim::new(&artifact, blossom_value::Seed::from_u64(0)).unwrap();
    let run = sim
        .run(
            &[],
            Tick(0),
            Duration::from_nanos(1_000_000_000),
            &FaultSchedule::default(),
            false,
        )
        .unwrap();
    let inst = &run.rounds.first().unwrap().first().unwrap().instance;
    let mut out = Vec::new();
    for rel in ["root_limit", "module_limit", "derived"] {
        for row in inst.rows(artifact.rel_named(rel).unwrap()) {
            out.push((rel.to_owned(), format!("{row:?}")));
        }
    }
    out
}

#[test]
fn constants_and_parameters_fold_in_dependency_order_and_bind_by_qualified_name() {
    use blossom_front::api::ParamBinding as B;
    let rows = |params: &[(&str, B)]| boot_rows("params_in_modules.bls", params);
    let u = |n: u64| format!("{:?}", [Value::Int(IntValue::U64(n))]);
    let defaults = rows(&[]);
    assert!(defaults.contains(&("root_limit".into(), u(5))), "{defaults:?}");
    assert!(defaults.contains(&("module_limit".into(), u(1))), "{defaults:?}");
    assert!(defaults.contains(&(
        "derived".into(),
        format!("{:?}", [Value::Int(IntValue::U64(10)), Value::Int(IntValue::U64(11))])
    )));
    // The root's binding reaches the root's parameter and what depends on it, not the module's.
    let bound = rows(&[("LIMIT", B::Int(7))]);
    assert!(bound.contains(&("root_limit".into(), u(7))), "{bound:?}");
    assert!(bound.contains(&("module_limit".into(), u(1))), "{bound:?}");
    assert!(bound.contains(&(
        "derived".into(),
        format!("{:?}", [Value::Int(IntValue::U64(14)), Value::Int(IntValue::U64(15))])
    )));
    // The module's parameter is bound by its qualified name.
    let module = rows(&[("c.LIMIT", B::Int(3))]);
    assert!(module.contains(&("module_limit".into(), u(3))), "{module:?}");
    assert!(module.contains(&("root_limit".into(), u(5))), "{module:?}");
    assert_eq!(compile_codes("const_cycle.bls"), vec!["BLS0200", "BLS0200"]);
}

#[test]
fn a_negative_duration_binding_is_rejected() {
    assert_eq!(
        blossom_front::api::parse_duration("150ms").map(|d| d.as_nanos()),
        Some(150_000_000)
    );
    assert_eq!(blossom_front::api::parse_duration("-5ms"), None);
}

/// The review suspected `argmax!` in a correlated `not { … }` ranged over every row; it is per outer valuation.
#[test]
fn argmax_in_a_correlated_not_is_per_outer_valuation() {
    let name = "argmax_in_correlated_not.bls";
    let (result, _) = compile_file(fixture(name).to_str().unwrap(), &nodes_for(name));
    let artifact = result.unwrap().0;
    let sim = BlsSim::new(&artifact, blossom_value::Seed::from_u64(0)).unwrap();
    let run = sim
        .run(
            &[],
            Tick(0),
            Duration::from_nanos(1_000_000_000),
            &FaultSchedule::default(),
            false,
        )
        .unwrap();
    let inst = &run.rounds.first().unwrap().first().unwrap().instance;
    let quiet: Vec<_> = inst.rows(artifact.rel_named("quiet").unwrap()).cloned().collect();
    assert_eq!(quiet, vec![std::sync::Arc::from(vec![Value::Int(IntValue::U64(1))])]);
}
