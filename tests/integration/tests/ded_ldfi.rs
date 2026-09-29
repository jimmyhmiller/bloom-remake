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

/// Programs from the slice 1 review that once fooled the lineage-driven search: a `pre` that more faults can bring
/// back, crash reads with times, relations derived from crashes, an aggregate over a negated input, `pre`/`post`
/// columns of different literal types, and a rule that would fire at a tick 0 Molly does not have. Every one has a
/// counterexample (the reference checker's exhaustive mode agrees), which the lineage-driven search must find by
/// itself in both negative-support modes, and exhaustive certification too.
#[cfg(test)]
const REVIEW: &[(&str, &str, [u64; 3], &[&str])] = &[
    (
        "pre revives",
        "in(\"a\", \"x\")@1;\ndst(\"a\", \"b\")@1;\ndst(\"a\", \"c\")@1;\ngot(D, X)@async :- in(N, X), dst(N, D);\ngot(N, X)@next :- got(N, X);\npre(X) :- in(\"a\", X)@1, notin got(\"c\", X);\npost(X) :- got(\"b\", X);\n",
        [4, 3, 0],
        &["a", "b", "c"],
    ),
    (
        "crash at a time",
        "val(\"a\", \"x\")@1;\nval(N, X)@next :- val(N, X);\npre(X) :- val(\"a\", X);\npost(X) :- val(\"a\", X), notin crash(_, \"b\", 1);\n",
        [5, 2, 1],
        &["a", "b", "c"],
    ),
    (
        "early crash",
        "val(\"a\", \"x\")@1;\nval(N, X)@next :- val(N, X);\nearly(X) :- val(\"a\", X), crash(_, \"b\", T), T < 3;\npre(X) :- val(\"a\", X);\npost(X) :- val(\"a\", X), notin early(X);\n",
        [5, 2, 1],
        &["a", "b", "c"],
    ),
    (
        "two crashes",
        "val(\"a\", \"x\")@1;\nval(N, X)@next :- val(N, X);\nmem(\"b\", \"x\")@1;\nmem(\"c\", \"x\")@1;\nmem(N, X)@next :- mem(N, X);\ntwocrash(X) :- mem(N, X), crash(_, N, _), mem(M, X), crash(_, M, _), N != M;\npre(X) :- val(\"a\", X);\npost(X) :- val(\"a\", X), notin twocrash(X);\n",
        [4, 2, 2],
        &["a", "b", "c"],
    ),
    (
        "crash-derived relation",
        "val(\"a\", \"x\")@1;\nval(N, X)@next :- val(N, X);\ncrashed(N) :- crash(_, N, _);\npre(X) :- val(\"a\", X);\npost(X) :- val(\"a\", X), notin crashed(\"b\");\n",
        [4, 2, 1],
        &["a", "b", "c"],
    ),
    (
        "aggregate over a negated input",
        "item(\"a\", 1)@1;\nitem(\"a\", 2)@1;\nitem(N, V)@next :- item(N, V);\nblk(\"b\", 2)@1;\nblocked(A, V)@async :- blk(N, V), peer(N, A);\nblocked(N, V)@next :- blocked(N, V);\npeer(\"b\", \"a\")@1;\npeer(N, M)@next :- peer(N, M);\ncnt(N, count<V>) :- item(N, V), notin blocked(N, V);\nok(N) :- cnt(N, C), C == 1;\npre(N) :- item(N, 1)@1;\npost(N) :- ok(N);\n",
        [4, 3, 0],
        &["a", "b"],
    ),
    (
        "pre and post literal types",
        "log(N, X)@next :- log(N, X);\nlog(M, X)@async :- bc(N, X), peer(N, M);\npeer(N, M)@next :- peer(N, M);\nbc(\"a\", 1)@1;\npeer(\"a\", \"b\")@1;\npre(\"b\", X) :- bc(N, X)@1;\npost(N, X) :- log(N, X);\n",
        [4, 3, 0],
        &["a", "b"],
    ),
    (
        "no tick 0",
        "m(\"b\", 1)@async :- notin sent(\"a\", 1);\nsent(\"a\", 1)@next :- notin sent(\"a\", 1);\nsent(N, X)@next :- sent(N, X);\ngot(N, X) :- m(N, X);\ngot(N, X)@next :- got(N, X);\nwant(N, X)@next :- want(N, X);\nwant(\"b\", 1)@1;\npre(N, X) :- want(N, X);\npost(N, X) :- got(N, X);\n",
        [4, 2, 0],
        &["a", "b"],
    ),
];

#[cfg(test)]
struct OneFile(String);

#[cfg(test)]
impl blossom_front::ded::DedLoader for OneFile {
    fn load(&mut self, _from: Option<&str>, path: &str) -> Result<blossom_front::ded::LoadedFile, String> {
        Ok(blossom_front::ded::LoadedFile {
            key: path.into(),
            text: self.0.clone(),
        })
    }
}

#[test]
fn review_regressions_find_their_counterexamples() {
    for (name, text, [eot, eff, crashes], nodes) in REVIEW {
        let mut sources = blossom_base::SourceDb::new();
        let artifact = blossom_front::ded::compile(&["p.ded"], nodes, &mut OneFile((*text).to_owned()), &mut sources)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let spec = FailureSpec::new(*eot, *eff, *crashes as u32, nodes.len() as u32).unwrap();
        let sim = DedSim::new(&artifact).unwrap();
        for neg in [
            blossom_ldfi::NegSupport::Precise,
            blossom_ldfi::NegSupport::Conservative,
        ] {
            let mut config = LdfiConfig::new(spec.clone());
            config.negative_support = neg;
            config.exhaustive_fallback = None;
            let report = blossom_ldfi::run(&sim, &config).unwrap_or_else(|e| panic!("{name} ({neg:?}): {e}"));
            assert_eq!(report.verdict, Verdict::Counterexample, "{name} ({neg:?})");
        }
        let ff = sim.run(spec.eot, &FaultSchedule::default(), false).unwrap();
        let ff_post: BTreeSet<_> = sim.outcome(&ff, spec.eot, false).unwrap().post;
        let cert = exhaustive(&sim, &spec, &ff_post, 1, 1_000_000).unwrap();
        assert!(cert.counterexample.is_some(), "{name}: exhaustive certification");
    }
}

#[test]
fn results_do_not_depend_on_the_worker_count() {
    for name in ["BENCH-131q", "BENCH-137c"] {
        let c = case(name);
        let sim = DedSim::new(&c.artifact).unwrap();
        let mut outcomes = Vec::new();
        for workers in [1, 4] {
            let mut config = LdfiConfig::new(c.spec.clone());
            config.workers = workers;
            config.exhaustive_fallback = None;
            let report = blossom_ldfi::run(&sim, &config).unwrap();
            let faults: Vec<_> = report.counterexamples.iter().map(|ce| ce.faults.clone()).collect();
            outcomes.push((report.verdict, report.runs, faults));
        }
        assert_eq!(outcomes[0], outcomes[1], "{name}");
    }
}
