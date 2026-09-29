//! Regression tests for the slice 2 adversarial review: each fixture in `fixtures/review/` is one confirmed
//! wrong answer (an unsound LDFI verdict, a polarity bypass, a crash, a wrong result), with the answer it must give.

use std::path::{Path, PathBuf};

use blossom_driver::bls::{compile_file, compile_spec_file};
use blossom_front::api::{BlsError, NodeSpec};
use blossom_ldfi::report::DedNames;
use blossom_ldfi::{FailureSpec, LdfiConfig, LdfiError, Verdict};
use blossom_sim::spec::SpecSim;
use blossom_value::time::{NodeId, Tick};

#[cfg(test)]
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/review").join(name)
}

/// The codes of the errors compiling a program root reports (empty when it compiles).
#[cfg(test)]
fn compile_codes(name: &str) -> Vec<String> {
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let (result, _) = compile_file(fixture(name).to_str().unwrap(), &nodes);
    match result {
        Ok(_) => Vec::new(),
        Err(BlsError::Rejected(d)) => d.iter().map(|x| x.code.as_str().to_owned()).collect(),
        Err(e) => panic!("{name}: {e}"),
    }
}

/// The codes of the errors compiling a spec reports (empty when it compiles).
#[cfg(test)]
fn spec_codes(name: &str, spec: &str) -> Vec<String> {
    let (result, _) = compile_spec_file(fixture(name).to_str().unwrap(), spec);
    match result {
        Ok(_) => Vec::new(),
        Err(BlsError::Rejected(d)) => d.iter().map(|x| x.code.as_str().to_owned()).collect(),
        Err(e) => panic!("{name}: {e}"),
    }
}

/// The rows of relation `rel` at node 0 and `tick` in the spec's failure-free run, rendered.
#[cfg(test)]
fn rows(name: &str, spec: &str, rel: &str, tick: u64) -> Vec<String> {
    let (result, _) = compile_spec_file(fixture(name).to_str().unwrap(), spec);
    let (compiled, _) = result.unwrap_or_else(|e| panic!("{name}: {e:?}"));
    let artifact = compiled.artifact;
    let eot = compiled.faults.map_or(tick, |f| f.eot.max(tick));
    let sim = SpecSim::new(&artifact).unwrap();
    let run = sim.run(Tick(eot), &Default::default(), false).unwrap();
    let ir = artifact
        .rels
        .iter()
        .find(|r| r.name.as_str() == rel)
        .and_then(|r| r.protocol)
        .unwrap_or_else(|| panic!("no relation {rel}"));
    let names = DedNames { artifact: &artifact };
    let nt = run.node_tick(Tick(tick), NodeId(0)).unwrap();
    let mut out: Vec<String> = nt
        .instance
        .rows(ir)
        .map(|r| format!("{rel}({})", names.row(ir, r).join(", ")))
        .collect();
    out.sort();
    out
}

/// The LDFI verdict of a spec (lineage-driven, falling back to exhaustive certification); `None` when LDFI does not
/// handle the program yet.
#[cfg(test)]
fn ldfi(name: &str, spec: &str) -> Option<Verdict> {
    let (result, _) = compile_spec_file(fixture(name).to_str().unwrap(), spec);
    let (compiled, _) = result.unwrap_or_else(|e| panic!("{name}: {e:?}"));
    let faults = compiled.faults.expect("the spec has `faults`");
    let artifact = compiled.artifact;
    let fs = FailureSpec::new(faults.eot, faults.eff, faults.crashes, artifact.nodes.len() as u32).unwrap();
    let sim = SpecSim::new(&artifact).unwrap();
    match blossom_ldfi::run(&sim, &LdfiConfig::new(fs)) {
        Ok(r) => Some(r.verdict),
        Err(LdfiError::Unimplemented(_)) => None,
        Err(e) => panic!("{name}: {e}"),
    }
}

#[test]
fn a_header_binding_a_lattice_value_keeps_its_valuations() {
    let empty = rows("header_lattice_binding.bls", "S", "empty_a", 1);
    assert_eq!(empty, vec!["empty_a(2)"]);
    let counts = rows("header_lattice_binding.bls", "S", "count_a", 1);
    assert_eq!(counts, vec!["count_a(1, 1)", "count_a(2, 0)"]);
    assert_eq!(rows("header_lattice_points.bls", "S", "seen", 2), vec!["seen(7)"]);
}

#[test]
fn ldfi_sees_cells_change_both_ways() {
    assert_eq!(ldfi("ldfi_lattice_lookup.bls", "L1"), Some(Verdict::Counterexample));
    assert_eq!(
        ldfi("ldfi_lattice_lost_contribution.bls", "L4"),
        Some(Verdict::Counterexample)
    );
}

#[test]
fn ldfi_refuses_halting_programs() {
    assert_eq!(ldfi("ldfi_halt.bls", "H1"), None);
}

#[test]
fn map_lattice_values_carry_their_polarity() {
    assert!(compile_codes("polarity_map_values.bls").iter().any(|c| c == "BLS0502"));
    assert!(compile_codes("polarity_map_reveal.bls").iter().any(|c| c == "BLS0502"));
}

#[test]
fn lattice_columns_bind_fresh_variables() {
    assert_eq!(compile_codes("lattice_join_via_lookup.bls"), vec!["BLS0304"]);
    assert_eq!(compile_codes("lattice_join_via_let.bls"), vec!["BLS0304"]);
}

#[test]
fn the_non_bottom_refinement_needs_every_binding() {
    assert_eq!(spec_codes("nonbot_two_lattice_columns.bls", "S"), vec!["BLS0300"]);
    assert_eq!(spec_codes("nonbot_any.bls", "S"), vec!["BLS0300"]);
}

#[test]
fn resolve_with_an_empty_key_lowers() {
    assert_eq!(compile_codes("resolve_empty_key.bls"), Vec::<String>::new());
}

#[test]
fn points_compare_without_conflict() {
    assert_eq!(rows("point_leq.bls", "S", "le", 1), vec!["le(1, 1)", "le(2, 2)"]);
}

#[test]
fn a_rule_that_cannot_fire_raises_nothing() {
    assert_eq!(rows("unfired_arithmetic.bls", "S", "out", 1), Vec::<String>::new());
}

#[test]
fn diagnostics_of_the_review() {
    assert_eq!(compile_codes("monotone_forall_closed.bls"), Vec::<String>::new());
    assert_eq!(compile_codes("resolve_cycle.bls"), vec!["BLS0503"]);
    assert_eq!(spec_codes("session_wrong_role.bls", "S1"), vec!["BLS0405"]);
}
