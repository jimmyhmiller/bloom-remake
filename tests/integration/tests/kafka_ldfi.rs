//! S11: the Blossom Kafka broker under LDFI (`examples/kafka/ldfi.bls`): the broker and the Blossom Kafka client in the
//! synchronous-round world, connected over byte streams, checked with the producer guarantees (an acknowledged batch
//! stays in the partition's committed log; a client that read to the end read it) under crash-restarts. The correct
//! broker holds with one broker and with three replicating ones (one or two batches; any one crash-restart, or any one
//! lost message); a producer configured with `acks=1` loses an acknowledged batch to a crash of the leader right after
//! it answered. With retries, a lost answer makes the client send a batch again: a producer without idempotence has it
//! stored twice, an idempotent one once. A consumer that commits the offset it read (outside group management, through
//! the coordinator FindCoordinator names) keeps the commit through a crash-restart. Enumerating every admissible
//! fault schedule (each run in full) confirms the verdicts it can afford.

use std::path::Path;

use blossom_artifact::sim::SimArtifact;
use blossom_driver::bls::compile_spec_file;
use blossom_ldfi::report::fault_labels;
use blossom_ldfi::{FailureSpec, LdfiConfig, Method, Verdict};
use blossom_sim::spec::{SpecSim, is_good};

/// `spec` of `examples/kafka/ldfi.bls` compiled: its artifact, its failure spec, and its `check ldfi expect …`.
#[cfg(test)]
fn compile(spec: &str) -> (SimArtifact, FailureSpec, bool) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/kafka/ldfi.bls");
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

#[cfg(test)]
fn sim(artifact: &SimArtifact) -> SpecSim<'_> {
    SpecSim::with_externs(artifact, std::sync::Arc::new(blossom_std_host::registry().unwrap())).unwrap()
}

/// Checks `spec` by enumerating every admissible fault schedule: the verdict must be its `check ldfi expect …`.
/// Returns the counterexample's fault labels (one with the fewest faults).
#[cfg(test)]
fn enumerate(spec: &str) -> Vec<String> {
    let (artifact, fs, expect) = compile(spec);
    let sim = sim(&artifact);
    let mut config = LdfiConfig::new(fs);
    config.workers = 2;
    let report = blossom_ldfi::enumerate(&sim, &config).unwrap_or_else(|e| panic!("{spec}: {e}"));
    assert!(
        matches!(report.method, Method::Enumerated { after: None, .. }),
        "{spec}"
    );
    assert_eq!(report.verdict == Verdict::NoCounterexample, expect, "{spec}");
    report
        .counterexamples
        .first()
        .map(|ce| fault_labels(&artifact, &ce.faults))
        .unwrap_or_default()
}

/// Checks `spec` of `examples/kafka/ldfi.bls`: the verdict must be its `check ldfi expect …`, and a counterexample
/// must reproduce. Returns the counterexample's fault labels.
#[cfg(test)]
fn check(spec: &str) -> Vec<String> {
    let (artifact, fs, expect) = compile(spec);
    let sim = sim(&artifact);
    let mut config = LdfiConfig::new(fs.clone());
    config.workers = 2;
    config.exhaustive_fallback = None;
    let report = blossom_ldfi::run(&sim, &config).unwrap_or_else(|e| panic!("{spec}: {e}"));
    let want = if expect {
        Verdict::NoCounterexample
    } else {
        Verdict::Counterexample
    };
    assert_eq!(report.verdict, want, "{spec}");
    let Some(ce) = report.counterexamples.first() else {
        return Vec::new();
    };
    let ff = sim.run(fs.eot, &Default::default(), false).unwrap();
    let ff_post = sim.outcome(&ff, fs.eot, false).unwrap().post;
    let run = sim.run(fs.eot, &ce.faults, false).unwrap();
    assert!(
        !is_good(&ff_post, &sim.outcome(&run, fs.eot, false).unwrap()),
        "{spec}: the counterexample does not reproduce"
    );
    fault_labels(&artifact, &ce.faults)
}

#[test]
fn one_broker_keeps_an_acknowledged_batch_through_a_crash_restart() {
    check("SingleRestart");
}

#[test]
fn every_schedule_of_one_broker_keeps_an_acknowledged_batch() {
    enumerate("SingleRestart");
}

#[test]
#[ignore = "full tier"]
fn every_schedule_confirms_the_three_broker_verdicts() {
    enumerate("TripleRestart");
    enumerate("TripleOneLoss");
    // Two partitions, a batch to each (LDFI holds too, in 377 runs: as many as there are schedules).
    enumerate("TriplePartitionsRestart");
    // The smallest counterexample is the one LDFI finds: the leader crashes right after it answered.
    assert_eq!(enumerate("TripleRestartAcks1"), ["C(B2,18)", "R(B2,21)"]);
}

#[test]
#[ignore = "full tier"]
fn every_single_delay_keeps_the_guarantees() {
    // The asynchronous model (S13): any one batch (a channel's, or the stream traffic, between two nodes in a round)
    // arriving a round late.
    enumerate("TripleDelay");
}

#[test]
#[ignore = "full tier"]
fn three_brokers_keep_an_acknowledged_batch_through_a_crash_restart() {
    check("TripleRestart");
}

#[test]
#[ignore = "full tier"]
fn three_brokers_keep_two_batches_at_their_own_offsets_through_a_crash_restart() {
    check("TripleTwoRestart");
}

#[test]
#[ignore = "full tier"]
fn three_brokers_keep_an_acknowledged_batch_through_any_one_lost_message() {
    check("TripleOneLoss");
}

#[test]
#[ignore = "full tier"]
fn acks_1_loses_an_acknowledged_batch_to_a_leader_crash() {
    let faults = check("TripleRestartAcks1");
    // The leader crashes after it answered and comes back as a follower without the batch its successor never got.
    assert!(faults.iter().any(|f| f.starts_with("C(B")), "{faults:?}");
}

#[test]
#[ignore = "full tier"]
fn retries_without_idempotence_store_a_batch_twice() {
    // The leader's answer is lost: the connection resets, the client sends the batch again.
    let faults = check("TripleRetry");
    assert!(
        faults.len() == 1 && faults[0].starts_with("O(B") && faults[0].contains(",C1,"),
        "{faults:?}"
    );
}

#[test]
#[ignore = "full tier"]
fn an_idempotent_producer_stores_a_retried_batch_once() {
    check("TripleRetryIdempotent");
}

#[test]
#[ignore = "full tier"]
fn an_acknowledged_offset_commit_survives_a_crash_restart() {
    check("TripleCommitRestart");
}
