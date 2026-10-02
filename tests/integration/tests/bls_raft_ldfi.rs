//! LDFI on the Blossom Raft of `examples/e12_raft.bls`, from source text. Each spec of `examples/e12_raft_specs.bls`
//! is compiled with its target and checked; the verdict must be the one its `check ldfi expect …` states, which is
//! the Molly verdict of its `.ded` counterpart (BENCH-137a–h, the corpus's compact Raft), and a counterexample must
//! be a real one that violates the property the seeded bug breaks.

use std::collections::BTreeSet;
use std::path::Path;

use blossom_driver::bls::compile_spec_file;
use blossom_ldfi::report::fault_labels;
use blossom_ldfi::{FailureSpec, LdfiConfig, Verdict};
use blossom_sim::spec::{SpecSim, is_good};
use blossom_value::Value;

/// What a check found: the verdict, the first counterexample's faults, and the properties (the `post` rows of the
/// failure-free run) that its replay violates.
#[cfg(test)]
struct Checked {
    verdict: Verdict,
    faults: Vec<String>,
    violated: BTreeSet<String>,
}

#[cfg(test)]
fn check(spec: &str) -> Checked {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/e12_raft_specs.bls");
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
    // `blossom ldfi`'s budget: with a crash, the lineage-driven search does not exhaust its hypotheses within it, and
    // exhaustive certification decides (about 30,000 states for each crash spec here).
    config.max_runs = 20_000;
    let report = blossom_ldfi::run(&sim, &config).unwrap();
    let want = if expect {
        Verdict::NoCounterexample
    } else {
        Verdict::Counterexample
    };
    assert_eq!(report.verdict, want, "{spec}");
    let mut checked = Checked {
        verdict: report.verdict,
        faults: Vec::new(),
        violated: BTreeSet::new(),
    };
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
        for row in ff_post
            .iter()
            .filter(|r| outcome.pre.contains(*r) && !outcome.post.contains(*r))
        {
            match &**row {
                [Value::Str(prop)] => checked.violated.insert(prop.to_string()),
                other => panic!("{spec}: unexpected `post` row {other:?}"),
            };
        }
        checked.faults = fault_labels(&artifact, &ce.faults);
    }
    checked
}

#[cfg(test)]
fn violates(c: &Checked, prop: &str) {
    assert_eq!(c.violated, BTreeSet::from([prop.to_owned()]), "faults {:?}", c.faults);
}

#[test]
fn election_safety_holds_under_omissions() {
    // BENCH-137a, 8/5/0.
    assert_eq!(check("ElectionFaults").verdict, Verdict::NoCounterexample);
}

#[test]
fn voting_twice_elects_two_leaders() {
    // BENCH-137b, 8/5/0: A's messages stop reaching B and C, both stand for term 2, and A votes for both.
    let c = check("VoteTwiceFaults");
    violates(&c, "election safety");
    assert!(c.faults.iter().all(|f| f.starts_with("O(")), "{:?}", c.faults);
}

#[test]
fn commit_holds_under_omissions() {
    // BENCH-137c, 8/4/0.
    assert_eq!(check("CommitFaults").verdict, Verdict::NoCounterexample);
}

#[test]
fn eager_commit_loses_a_committed_value() {
    // BENCH-137d, 8/4/0: A commits "x" alone, and B, which never stored it, leads term 2 with
    // C's vote and commits "y" at the same index.
    let c = check("EagerCommitFaults");
    violates(&c, "state machine safety");
    assert!(c.faults.iter().any(|f| f.starts_with("O(A,B,")), "{:?}", c.faults);
}

#[test]
#[ignore = "full tier"]
fn commit_holds_under_omissions_over_a_long_run() {
    // BENCH-137e, 12/6/0.
    assert_eq!(check("CommitLongFaults").verdict, Verdict::NoCounterexample);
}

#[test]
fn votes_without_the_log_check_overwrite_a_committed_value() {
    // BENCH-137f, 12/6/0: B misses "x", wins term 2 with the vote of a server that holds it, and overwrites it.
    let c = check("VoteUncheckedFaults");
    violates(&c, "state machine safety");
    assert!(c.faults.iter().any(|f| f.starts_with("O(A,B,")), "{:?}", c.faults);
}

#[test]
#[ignore = "full tier"]
fn commit_holds_with_a_crash() {
    // BENCH-137g, 12/6/1.
    assert_eq!(check("CommitCrashFaults").verdict, Verdict::NoCounterexample);
}

#[test]
#[ignore = "full tier"]
fn election_safety_holds_with_a_crash() {
    // BENCH-137h, 10/7/1.
    assert_eq!(check("ElectionCrashFaults").verdict, Verdict::NoCounterexample);
}
