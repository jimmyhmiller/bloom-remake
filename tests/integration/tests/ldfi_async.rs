//! S13: the asynchronous fault model. A batch (everything one node sends another in a round) may be delayed: it arrives
//! 2 to `delay` rounds after its send, so batches of different rounds can arrive out of order. The race of
//! `fixtures/ldfi/race.bls` (a server keeping the latest value it received) holds in the synchronous model, fails
//! under a delay that reorders its two writes, and holds again when the server ignores stale values.

use std::path::Path;

use blossom_artifact::sim::SimArtifact;
use blossom_driver::bls::compile_spec_file;
use blossom_ldfi::report::fault_labels;
use blossom_ldfi::{FailureSpec, LdfiConfig, LdfiError, Method, Verdict};
use blossom_sim::spec::SpecSim;

/// `spec` of `fixtures/ldfi/FILE` compiled: its artifact, its failure spec, and its `check ldfi expect …`.
#[cfg(test)]
fn compile(file: &str, spec: &str) -> (SimArtifact, FailureSpec, bool) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/ldfi").join(file);
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
    let mut fs = FailureSpec::new(faults.eot, faults.eff, faults.crashes, artifact.nodes.len() as u32).unwrap();
    if let Some(d) = faults.restart {
        fs = fs.with_restart(d).unwrap();
    }
    if let Some(k) = faults.omissions {
        fs = fs.with_max_omissions(k);
    }
    if let Some(d) = faults.delay {
        fs = fs.with_delays(d, faults.delays).unwrap();
    }
    (artifact, fs, expect)
}

/// Decides `spec` by enumeration: the verdict must be its `check ldfi expect …`. Returns the counterexample's labels
/// and the schedules run.
#[cfg(test)]
fn enumerate(file: &str, spec: &str) -> (Vec<String>, u64) {
    let (artifact, fs, expect) = compile(file, spec);
    let sim = SpecSim::new(&artifact).unwrap();
    let mut config = LdfiConfig::new(fs);
    config.workers = 2;
    let report = blossom_ldfi::enumerate(&sim, &config).unwrap_or_else(|e| panic!("{spec}: {e}"));
    assert_eq!(report.verdict == Verdict::NoCounterexample, expect, "{spec}");
    let Method::Enumerated { schedules, .. } = report.method else {
        panic!("{spec}: not enumerated")
    };
    let labels = report
        .counterexamples
        .first()
        .map(|ce| fault_labels(&artifact, &ce.faults))
        .unwrap_or_default();
    (labels, schedules)
}

#[test]
fn the_race_holds_in_order_and_fails_when_a_delay_reorders_the_writes() {
    assert_eq!(enumerate("race.bls", "NaiveSync"), (vec![], 1));
    // The first write (sent at 1) arrives at 4, after the second (sent at 2, arriving at 3).
    let (faults, _) = enumerate("race.bls", "NaiveAsync");
    assert_eq!(faults, ["D(C,S,1,put,+3)"]);
    // The versioned server holds under every delay: 6 batches (either way, sent at 1, 2 or 3) on its one channel by 2 or
    // 3 rounds, and none.
    let (faults, schedules) = enumerate("race.bls", "VersionedAsync");
    assert!(faults.is_empty());
    assert_eq!(schedules, 13);
    let (artifact, fs, _) = compile("race.bls", "VersionedAsync");
    let paths = blossom_ldfi::certify::paths(&SpecSim::new(&artifact).unwrap());
    assert_eq!(blossom_ldfi::certify::schedule_count(&fs, paths.len()), 13);
}

#[test]
fn the_lineage_driven_search_refuses_the_asynchronous_model_for_now() {
    let (artifact, fs, _) = compile("race.bls", "NaiveAsync");
    let sim = SpecSim::new(&artifact).unwrap();
    let mut config = LdfiConfig::new(fs);
    config.exhaustive_fallback = None;
    let refused = blossom_ldfi::run(&sim, &config);
    assert!(matches!(refused, Err(LdfiError::Unimplemented(_))), "{refused:?}");
    // Deciding by size enumerates it.
    let report = blossom_ldfi::decide(&sim, &config).unwrap();
    assert_eq!(report.verdict, Verdict::Counterexample);
}

#[test]
fn a_program_error_under_a_delay_is_a_counterexample() {
    // The first write, delayed a round, arrives with the second: two writes of one key in one round.
    let (artifact, fs, _) = compile("race.bls", "EachAsync");
    let sim = SpecSim::new(&artifact).unwrap();
    let mut config = LdfiConfig::new(fs);
    config.workers = 2;
    let report = blossom_ldfi::enumerate(&sim, &config).unwrap();
    assert_eq!(report.verdict, Verdict::Counterexample);
    let ce = &report.counterexamples[0];
    assert_eq!(fault_labels(&artifact, &ce.faults), ["D(C,S,1,put,+2)"]);
    let failure = ce.failure.as_deref().unwrap_or_default();
    assert!(failure.contains("BLSR002"), "{failure}");
}
