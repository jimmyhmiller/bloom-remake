//! S11: LDFI under crash-restarts (crash-recovery, TEST-037) and over guarded timers. A crash-restart fault takes a
//! node down for `restart` ticks; it comes back with its durable relations only. The toy store of
//! `fixtures/ldfi/store.bls` must hold when it keeps an acknowledged write durably, and give a counterexample (a crash
//! and its restart after the acknowledgement) when it keeps it in a volatile table, a bug crash-stop faults cannot
//! expose. The sender of `fixtures/ldfi/retry.bls` retransmits on a timer guarded by its pending state: lost when the
//! pending state is volatile and the sender restarts, which only the guard's lineage shows.

use std::path::Path;

use blossom_driver::bls::compile_spec_file;
use blossom_ldfi::report::fault_labels;
use blossom_ldfi::{FailureSpec, LdfiConfig, Verdict};
use blossom_sim::spec::{SpecSim, is_good};

/// Checks `spec` of `fixtures/ldfi/FILE`: the verdict must be its `check ldfi expect …`, and a counterexample must
/// reproduce. Returns the counterexample's fault labels.
#[cfg(test)]
fn check(file: &str, spec: &str) -> Vec<String> {
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
