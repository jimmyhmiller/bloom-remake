//! Slice 1: LDFI end to end on Molly programs, from source text: the lineage-driven search's verdicts on small
//! corpus cases, the demo's counterexample, and exhaustive certification agreeing with the lineage-driven search.
//! (The whole LDFI corpus runs through `cargo run -p xtask -- corpus --check --area ldfi`.)

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use blossom_artifact::ded::DedArtifact;
use blossom_driver::ded::compile_files;
use blossom_ldfi::certify::exhaustive;
use blossom_ldfi::report::fault_labels;
use blossom_ldfi::{FailureSpec, LdfiConfig, Method, Verdict};
use blossom_sim::FaultSchedule;
use blossom_sim::ded::DedSim;

#[cfg(test)]
struct Case {
    artifact: DedArtifact,
    spec: FailureSpec,
    verdict: Verdict,
}

#[cfg(test)]
fn case(name: &str) -> Case {
    let dir: PathBuf = std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/ldfi/molly"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.file_name().unwrap().to_string_lossy().starts_with(name))
        .unwrap_or_else(|| panic!("no case {name}"));
    let m: toml::Table = toml::from_str(&std::fs::read_to_string(dir.join("manifest.toml")).unwrap()).unwrap();
    let ld = m["expect_ldfi"].as_table().unwrap();
    let nodes: Vec<String> = ld["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_owned())
        .collect();
    let node_refs: Vec<&str> = nodes.iter().map(String::as_str).collect();
    let program = dir.join(m["program"].as_str().unwrap());
    let (artifact, _) = compile_files(&[program.to_str().unwrap()], &node_refs);
    let artifact = artifact.unwrap();
    let int = |k: &str| ld[k].as_integer().unwrap() as u64;
    let spec = FailureSpec::new(int("eot"), int("eff"), int("crashes") as u32, nodes.len() as u32).unwrap();
    let verdict = match ld["verdict"].as_str().unwrap() {
        "counterexample" => Verdict::Counterexample,
        _ => Verdict::NoCounterexample,
    };
    Case {
        artifact,
        spec,
        verdict,
    }
}

#[test]
fn simple_deliv_counterexample_is_the_papers() {
    let c = case("BENCH-130a");
    let sim = DedSim::new(&c.artifact).unwrap();
    let report = blossom_ldfi::run(&sim, &LdfiConfig::new(c.spec.clone())).unwrap();
    assert_eq!(report.verdict, Verdict::Counterexample);
    assert_eq!(report.method, Method::Lineage);
    assert_eq!(report.runs, 2, "LDFI Figure 12: two executions");
    let labels = fault_labels(&c.artifact, &report.counterexamples[0].faults);
    assert_eq!(labels, ["O(a,b,1)"]);
}

#[test]
fn lineage_verdicts_on_small_cases() {
    for name in [
        "BENCH-130b",
        "BENCH-130c",
        "BENCH-130f",
        "BENCH-130k",
        "BENCH-131c",
        "BENCH-131p",
        "BENCH-132c",
        "BENCH-135a",
        "BENCH-137i",
    ] {
        let c = case(name);
        let sim = DedSim::new(&c.artifact).unwrap();
        let mut config = LdfiConfig::new(c.spec.clone());
        config.exhaustive_fallback = None;
        let report = blossom_ldfi::run(&sim, &config).unwrap();
        assert_eq!(report.verdict, c.verdict, "{name}");
    }
}

#[test]
fn exhaustive_certification_agrees_with_the_lineage_driven_search() {
    for name in [
        "BENCH-130b",
        "BENCH-130c",
        "BENCH-130f",
        "BENCH-131c",
        "BENCH-131p",
        "BENCH-135b",
        "BENCH-137i",
    ] {
        let c = case(name);
        let sim = DedSim::new(&c.artifact).unwrap();
        let ff = sim.run(c.spec.eot, &FaultSchedule::default(), false).unwrap();
        let ff_post: BTreeSet<_> = sim.outcome(&ff, c.spec.eot, false).unwrap().post;
        let cert = exhaustive(&sim, &c.spec, &ff_post, 1, 10_000_000).unwrap();
        let got = if cert.counterexample.is_some() {
            Verdict::Counterexample
        } else {
            Verdict::NoCounterexample
        };
        assert_eq!(got, c.verdict, "{name}");
        if let Some(faults) = cert.counterexample {
            let run = sim.run(c.spec.eot, &faults, false).unwrap();
            let outcome = sim.outcome(&run, c.spec.eot, false).unwrap();
            assert!(
                !blossom_sim::ded::is_good(&ff_post, &outcome),
                "{name}: the witness reproduces"
            );
        }
    }
}
