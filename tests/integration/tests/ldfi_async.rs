//! S13: the asynchronous fault model. A batch (everything one node sends another in a round) may be delayed: it arrives
//! 2 to `delay` rounds after its send, so batches of different rounds can arrive out of order. The race of
//! `fixtures/ldfi/race.bls` (a server keeping the latest value it received) holds in the synchronous model, fails
//! under a delay that reorders its two writes, and holds again when the server ignores stale values.

use std::path::Path;

use blossom_artifact::sim::SimArtifact;
use blossom_driver::bls::compile_spec_file;
use blossom_ldfi::report::fault_labels;
use blossom_ldfi::{FailureSpec, LdfiConfig, Method, Verdict};
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

/// Decides `spec` by the lineage-driven search: the verdict must be its `check ldfi expect …`. Returns the
/// counterexample's labels.
#[cfg(test)]
fn lineage(file: &str, spec: &str) -> Vec<String> {
    let (artifact, fs, expect) = compile(file, spec);
    let sim = SpecSim::new(&artifact).unwrap();
    let mut config = LdfiConfig::new(fs);
    config.workers = 2;
    config.exhaustive_fallback = None;
    let report = blossom_ldfi::run(&sim, &config).unwrap_or_else(|e| panic!("{spec}: {e}"));
    assert!(matches!(report.method, Method::Lineage), "{spec}");
    assert_eq!(report.verdict == Verdict::NoCounterexample, expect, "{spec}");
    report
        .counterexamples
        .first()
        .map(|ce| fault_labels(&artifact, &ce.faults))
        .unwrap_or_default()
}

#[test]
fn the_lineage_driven_search_finds_the_reordering_and_proves_the_versioned_server() {
    // A delay falsifies a delivery's arrival (the second write's), and makes a tuple appear later (the first write's):
    // the lineage leads to the same smallest counterexample as enumeration.
    assert_eq!(lineage("race.bls", "NaiveAsync"), ["D(C,S,1,put,+3)"]);
    assert!(lineage("race.bls", "VersionedAsync").is_empty());
    assert!(lineage("race.bls", "NaiveSync").is_empty());
}

#[test]
fn delays_of_streams_are_found_both_ways() {
    // A write delayed past the end of the run loses the value the eager client counts acknowledged; the
    // acknowledging one holds. The lineage reaches the delay through the event's arrival (the flight and those before
    // it on the connection).
    assert_eq!(enumerate("stream_store.bls", "EagerDelay").0, ["D(C,S,1,streams,+6)"]);
    assert_eq!(lineage("stream_store.bls", "EagerDelay"), ["D(C,S,1,streams,+6)"]);
    assert!(enumerate("stream_store.bls", "DurableDelay").0.is_empty());
    assert!(lineage("stream_store.bls", "DurableDelay").is_empty());
}

#[test]
fn the_lineage_driven_search_agrees_with_enumeration_under_delays() {
    // Every asynchronous spec of the fixtures but the program error (which only enumeration anticipates).
    for (file, spec) in [
        ("race.bls", "NaiveSync"),
        ("race.bls", "NaiveAsync"),
        ("race.bls", "VersionedAsync"),
        ("stream_store.bls", "DurableDelay"),
        ("stream_store.bls", "EagerDelay"),
    ] {
        let (by_lineage, (by_enumeration, _)) = (lineage(file, spec), enumerate(file, spec));
        assert_eq!(by_lineage.is_empty(), by_enumeration.is_empty(), "{file} {spec}");
        assert!(
            by_enumeration.len() <= by_lineage.len(),
            "{file} {spec}: {by_lineage:?} vs {by_enumeration:?}"
        );
    }
}

#[test]
fn a_program_error_under_a_delay_is_a_counterexample() {
    // The first write, delayed a round, arrives with the second: two writes of one key in one round. Enumeration finds
    // it; the lineage-driven search's hazards cover the outcome spec, not program errors, which it meets only in the
    // runs it makes.
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
