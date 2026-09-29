//! Slice 2: LDFI on Blossom programs, from source text. Each spec of `examples/e02_specs.bls` and
//! `examples/e10_specs.bls` is compiled with its target and checked; the verdict must be the one its
//! `check ldfi expect …` states, which is Molly's verdict for the `.ded` counterpart (retry_deliv, simple_deliv,
//! simplog, ack_rb), and a counterexample must be a real one.

use std::path::Path;

use blossom_driver::bls::compile_spec_file;
use blossom_ldfi::report::fault_labels;
use blossom_ldfi::{FailureSpec, LdfiConfig, Verdict};
use blossom_sim::spec::{SpecSim, is_good};

#[cfg(test)]
fn check(file: &str, spec: &str) -> (Verdict, Vec<String>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples").join(file);
    let (result, _) = compile_spec_file(path.to_str().unwrap(), spec);
    let (compiled, _) = result.unwrap_or_else(|e| panic!("{spec}: {e:?}"));
    let faults = compiled.faults.expect("the spec has `faults`");
    let expect = compiled
        .checks
        .iter()
        .find(|c| c.tool.as_str() == "ldfi")
        .and_then(|c| c.expect)
        .expect("the spec has `check ldfi expect …`");
    let artifact = compiled.artifact;
    let nodes = artifact.nodes.len() as u32;
    let fs = FailureSpec::new(faults.eot, faults.eff, faults.crashes, nodes).unwrap();
    let sim = SpecSim::new(&artifact).unwrap();
    let mut config = LdfiConfig::new(fs.clone());
    config.workers = 4;
    let report = blossom_ldfi::run(&sim, &config).unwrap();
    let want = if expect {
        Verdict::NoCounterexample
    } else {
        Verdict::Counterexample
    };
    assert_eq!(report.verdict, want, "{spec}");
    let mut labels = Vec::new();
    if let Some(ce) = report.counterexamples.first() {
        // The counterexample is real: replaying it violates the outcome spec.
        let ff = sim.run(fs.eot, &Default::default(), false).unwrap();
        let ff_post = sim.outcome(&ff, fs.eot, false).unwrap().post;
        let run = sim.run(fs.eot, &ce.faults, false).unwrap();
        let outcome = sim.outcome(&run, fs.eot, false).unwrap();
        assert!(
            !is_good(&ff_post, &outcome),
            "{spec}: the counterexample does not violate the spec"
        );
        labels = fault_labels(&artifact, &ce.faults);
    }
    (report.verdict, labels)
}

#[test]
fn e02_retries_survive_omissions() {
    check("e02_specs.bls", "RetryFaults");
}

#[test]
fn e02_origin_crash_after_a_lost_send_is_a_counterexample() {
    let (_, faults) = check("e02_specs.bls", "RetryCrashFaults");
    // retry_deliv 4/2/1's falsifier shape (BENCH-130f): the origin crashes after losing a first send.
    assert!(faults.iter().any(|f| f == "C(A,2)"), "{faults:?}");
    assert!(faults.iter().any(|f| f == "O(A,B,1)" || f == "O(A,C,1)"), "{faults:?}");
}

#[test]
fn e02_one_shot_loses_a_message() {
    let (_, faults) = check("e02_specs.bls", "OneShotFaults");
    assert_eq!(faults.len(), 1, "{faults:?}");
}

#[test]
fn e10_simplog_is_molly_s_counterexample() {
    let (_, faults) = check("e10_specs.bls", "SimpleLogFaults");
    assert_eq!(faults, vec!["O(A,B,1)".to_owned()]);
}
