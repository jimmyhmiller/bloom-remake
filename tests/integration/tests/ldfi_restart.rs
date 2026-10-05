//! S11: LDFI under crash-restarts (crash-recovery, TEST-037) and over guarded timers. A crash-restart fault takes a
//! node down for `restart` ticks; it comes back with its durable relations only. The toy store of
//! `fixtures/ldfi/store.bls` must hold when it keeps an acknowledged write durably, and give a counterexample (a crash
//! and its restart after the acknowledgement) when it keeps it in a volatile table, a bug crash-stop faults cannot
//! expose. The sender of `fixtures/ldfi/retry.bls` retransmits on a timer guarded by its pending state: lost when the
//! pending state is volatile and the sender restarts, which only the guard's lineage shows. `relay_specs.bls`
//! checks the store as a program file that its specs target.

use std::path::Path;

use blossom_artifact::sim::SimArtifact;
use blossom_driver::bls::compile_spec_file;
use blossom_ldfi::report::fault_labels;
use blossom_ldfi::{FailureSpec, LdfiConfig, LdfiError, Method, Verdict};
use blossom_sim::spec::{SpecSim, is_good};

/// Every spec of `fixtures/ldfi` with an LDFI check.
#[cfg(test)]
const SPECS: &[(&str, &str)] = &[
    ("store.bls", "DurableRestart"),
    ("store.bls", "VolatileRestart"),
    ("store.bls", "VolatileStop"),
    ("retry.bls", "Omissions"),
    ("retry.bls", "DurableRestart"),
    ("retry.bls", "VolatileRestart"),
    ("armed.bls", "Heard"),
    ("armed.bls", "HeardTwice"),
    ("armed.bls", "HeardTwiceOneLoss"),
    ("stream_store.bls", "DurableRestart"),
    ("stream_store.bls", "VolatileRestart"),
    ("stream_store.bls", "EagerOmission"),
    ("relay_specs.bls", "DurableRestart"),
    ("relay_specs.bls", "VolatileRestart"),
];

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

/// Checks `spec` of `fixtures/ldfi/FILE`: the verdict must be its `check ldfi expect …`, and a counterexample must
/// reproduce. Returns the counterexample's fault labels.
#[cfg(test)]
fn check(file: &str, spec: &str) -> Vec<String> {
    let (artifact, fs, expect) = compile(file, spec);
    let sim = SpecSim::new(&artifact).unwrap();
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
fn a_durable_store_survives_a_restart() {
    check("store.bls", "DurableRestart");
}

#[test]
fn a_volatile_store_loses_an_acknowledged_write_to_a_restart() {
    let faults = check("store.bls", "VolatileRestart");
    // The server crashes after it acknowledged (it acknowledges at 2) and comes back a tick later without the write.
    assert_eq!(faults.len(), 2, "{faults:?}");
    let crash: u64 = faults
        .iter()
        .find_map(|f| f.strip_prefix("C(S,")?.strip_suffix(')')?.parse().ok())
        .unwrap_or_else(|| panic!("no crash of S in {faults:?}"));
    assert!(crash >= 3, "{faults:?}");
    assert!(faults.contains(&format!("R(S,{})", crash + 1)), "{faults:?}");
}

#[test]
fn crash_stop_faults_do_not_expose_the_volatile_store() {
    check("store.bls", "VolatileStop");
}

#[test]
fn retransmissions_on_a_guarded_timer_survive_lost_messages() {
    check("retry.bls", "Omissions");
}

#[test]
fn a_durable_guard_survives_a_restart_of_the_sender() {
    check("retry.bls", "DurableRestart");
}

#[test]
fn a_volatile_guard_lost_to_a_restart_stops_the_retransmissions() {
    let faults = check("retry.bls", "VolatileRestart");
    // The first send is lost and the sender restarts before its timer resends: only the guard ties the resends to the
    // lost pending state.
    assert!(faults.iter().any(|f| f == "O(S,R,1)"), "{faults:?}");
    assert!(faults.iter().any(|f| f.starts_with("C(S,")), "{faults:?}");
}

#[test]
fn losing_the_message_that_arms_a_guarded_timer_silences_it() {
    // Only the guard's lineage leads from the pings back to the `arm` message.
    assert_eq!(check("armed.bls", "Heard"), ["O(C,P,1)"]);
}

#[test]
fn a_durable_store_over_a_stream_survives_lost_messages_and_a_restart() {
    check("stream_store.bls", "DurableRestart");
}

#[test]
fn a_volatile_store_over_a_stream_loses_an_acknowledged_value_to_a_restart() {
    let faults = check("stream_store.bls", "VolatileRestart");
    // The server acknowledges at 2 (the client wrote at 1, on the connection opened at 1).
    let crash: u64 = faults
        .iter()
        .find_map(|f| f.strip_prefix("C(S,")?.strip_suffix(')')?.parse().ok())
        .unwrap_or_else(|| panic!("no crash of S in {faults:?}"));
    assert!(crash >= 3, "{faults:?}");
}

#[test]
fn a_client_that_counts_its_own_write_as_acknowledged_loses_it_to_a_reset() {
    // The client writes at 1 on the connection opened at 1: losing what it sends the server then resets the connection
    // before the server reads the value.
    assert_eq!(check("stream_store.bls", "EagerOmission"), ["O(C,S,1)"]);
}

#[test]
fn a_bound_on_lost_messages_limits_the_fault_sets() {
    // Arming twice takes both arms lost; with at most one lost message the pinger is always armed.
    assert_eq!(check("armed.bls", "HeardTwice"), ["O(C,P,1)", "O(C,P,2)"]);
    check("armed.bls", "HeardTwiceOneLoss");
}

#[test]
fn a_spec_over_a_program_file_binds_its_param_and_finds_the_restart() {
    // `relay_specs.bls` targets the program file `relay.bls`; its seeded bug is a deploy-time `param`.
    check("relay_specs.bls", "DurableRestart");
    let faults = check("relay_specs.bls", "VolatileRestart");
    assert!(faults.iter().any(|f| f.starts_with("R(S,")), "{faults:?}");
}

#[test]
fn enumerating_every_schedule_agrees_with_the_lineage_driven_search() {
    // The oracle: every admissible schedule run in full. Same verdict on every fixture, and a counterexample no
    // larger than the lineage-driven search's (enumeration goes fewest faults first).
    for (file, spec) in SPECS {
        let (artifact, fs, expect) = compile(file, spec);
        let sim = SpecSim::new(&artifact).unwrap();
        let mut config = LdfiConfig::new(fs);
        config.workers = 2;
        config.exhaustive_fallback = None;
        let lineage = blossom_ldfi::run(&sim, &config).unwrap_or_else(|e| panic!("{spec}: {e}"));
        let enumerated = blossom_ldfi::enumerate(&sim, &config).unwrap_or_else(|e| panic!("{spec}: {e}"));
        assert!(
            matches!(enumerated.method, Method::Enumerated { after: None, .. }),
            "{file} {spec}"
        );
        assert_eq!(enumerated.verdict, lineage.verdict, "{file} {spec}");
        assert_eq!(enumerated.verdict == Verdict::NoCounterexample, expect, "{file} {spec}");
        if let (Some(a), Some(b)) = (enumerated.counterexamples.first(), lineage.counterexamples.first()) {
            assert!(
                a.faults.len() <= b.faults.len(),
                "{file} {spec}: {:?} vs {:?}",
                a.faults,
                b.faults
            );
        }
    }
}

#[test]
fn certification_after_a_spent_budget_enumerates_a_program_it_cannot_step() {
    // A crash-restart spec cannot be stepped round by round: the fallback runs every schedule instead.
    let (artifact, fs, _) = compile("store.bls", "VolatileRestart");
    let sim = SpecSim::new(&artifact).unwrap();
    let mut config = LdfiConfig::new(fs.clone());
    config.max_runs = 1;
    let report = blossom_ldfi::run(&sim, &config).unwrap();
    assert!(
        matches!(
            report.method,
            Method::Enumerated {
                after: Some(blossom_ldfi::Fallback::RunBudget),
                ..
            }
        ),
        "{:?}",
        report.method
    );
    assert_eq!(report.verdict, Verdict::Counterexample);
    // Beyond its budget it refuses before running anything.
    config.max_schedules = 3;
    assert!(matches!(
        blossom_ldfi::run(&sim, &config),
        Err(LdfiError::ScheduleBudget(3))
    ));
    assert!(blossom_ldfi::certify::schedule_count(&fs, blossom_ldfi::certify::paths(&sim).len()) > 3);
}

#[test]
fn stepped_certification_refuses_a_program_with_streams() {
    // Stepping nodes one round at a time leaves out the stream fabric: it must refuse, not judge a run whose
    // connections never open.
    let (artifact, fs, _) = compile("stream_store.bls", "EagerOmission");
    let sim = SpecSim::new(&artifact).unwrap();
    assert!(!blossom_ldfi::certify::steppable(&sim, &fs));
    let ff = sim.run(fs.eot, &Default::default(), false).unwrap();
    let post = sim.outcome(&ff, fs.eot, false).unwrap().post;
    let refused = blossom_ldfi::certify::exhaustive(&sim, &fs, &post, 1, 1000);
    assert!(matches!(refused, Err(LdfiError::Unimplemented(_))), "{refused:?}");
    assert!(
        sim.step(
            blossom_value::time::NodeId(0),
            blossom_value::time::Tick(0),
            &Default::default(),
            &[]
        )
        .is_err()
    );
}

#[test]
fn sharing_hazards_across_runs_and_workers_changes_no_result() {
    // Runs share the hazards they encoded (S12); what a run finds must not depend on which runs came before it.
    for (file, spec) in SPECS {
        let (artifact, fs, _) = compile(file, spec);
        let sim = SpecSim::new(&artifact).unwrap();
        let mut outcomes = Vec::new();
        for (workers, shared) in [(1, 0), (1, 4_000_000), (3, 4_000_000)] {
            let mut config = LdfiConfig::new(fs.clone());
            config.workers = workers;
            config.shared_hazards = shared;
            config.exhaustive_fallback = None;
            let report = blossom_ldfi::run(&sim, &config).unwrap_or_else(|e| panic!("{spec}: {e}"));
            let faults: Vec<_> = report.counterexamples.iter().map(|ce| ce.faults.clone()).collect();
            outcomes.push((report.verdict, report.runs, faults));
        }
        assert_eq!(outcomes[0], outcomes[1], "{file} {spec}");
        assert_eq!(outcomes[0], outcomes[2], "{file} {spec}");
    }
}
