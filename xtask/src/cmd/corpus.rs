//! `corpus`: the golden-corpus runner with the status ratchet (PLAN §5).
//!
//! ```text
//! cargo run -p xtask -- corpus --check [--area ldfi] [--case SUBSTR]   # run every runnable backend, apply §5.3
//! cargo run -p xtask -- corpus --ratchet [...]                         # also flip passing backends to `pass`
//! cargo run -p xtask -- corpus --gate [...]                            # also require the slice's gate to pass
//! ```
//!
//! Slice 1 (docs/design/SLICES.md) runs `.ded` programs on two backends: `oracle` (the failure-free run in
//! synchronous rounds against `[[expect]]`) and `ldfi` (the verdict of `[expect_ldfi]`, and its falsifier sets when
//! stated). Every other backend, and `.bls` programs, report "not runnable in this build", which an `unimplemented`
//! status accepts. The ratchet (§5.3): a `pass` status must pass; an `unimplemented` backend that passes is stale
//! (`--ratchet` flips it to `pass` and drops its `unimplemented` and `until` keys); nothing else is ever edited, and
//! nothing is skipped. `--gate` fails every case of the slice's gate that does not pass: for S1, every `oracle` and
//! `ldfi` backend of `tests/corpus/ldfi`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use blossom_artifact::sim::SimArtifact;
use blossom_driver::ded::compile_files;
use blossom_driver::render::render;
use blossom_front::ded::DedError;
use blossom_ldfi::report::fault_labels;
use blossom_ldfi::{FailureSpec, LdfiConfig, Verdict, falsifiers};
use blossom_sim::spec::SpecSim;
use blossom_value::time::Tick;
use blossom_value::types::IntTy;
use blossom_value::value::IntValue;
use blossom_value::{TypeDef, Value};

use crate::util;

/// Arguments of `corpus`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// Run the cases and apply the ratchet rules.
    #[arg(long)]
    pub check: bool,
    /// Also flip every `unimplemented` backend that passes to `pass`.
    #[arg(long)]
    pub ratchet: bool,
    /// Also fail every case of the current slice's gate that does not pass.
    #[arg(long)]
    pub gate: bool,
    /// The corpus area (a directory under tests/corpus).
    #[arg(long, default_value = "ldfi")]
    pub area: String,
    /// Only cases whose directory name contains this.
    #[arg(long)]
    pub case: Option<String>,
    /// Worker threads for LDFI (default: the machine's parallelism).
    #[arg(long)]
    pub jobs: Option<usize>,
    /// Runs the lineage-driven search may make per case before exhaustive certification decides.
    #[arg(long, default_value_t = 20_000)]
    pub max_runs: u64,
    /// The repository root (default: the one containing xtask).
    #[arg(long)]
    pub root: Option<PathBuf>,
}

/// What running one backend of one case found.
#[derive(Debug)]
pub(super) enum Outcome {
    Pass(String),
    Fail(String),
    /// The build cannot run this backend for this case.
    NotRunnable(String),
}

/// Runs the task.
pub fn run(args: Args) -> ExitCode {
    if !(args.check || args.ratchet || args.gate) {
        eprintln!("corpus: give --check, --ratchet or --gate");
        return ExitCode::from(2);
    }
    let root = util::root_or_default(args.root.clone());
    let area = root.join("tests/corpus").join(&args.area);
    let milestone = fs::read_to_string(root.join("docs/plan/MILESTONE")).unwrap_or_default();
    let milestone = milestone.trim().to_owned();
    let mut cases = Vec::new();
    if let Err(e) = collect_cases(&area, &mut cases) {
        eprintln!("corpus: {}: {e}", area.display());
        return ExitCode::from(1);
    }
    cases.sort();
    let workers = args
        .jobs
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get));
    let mut problems = Vec::new();
    let mut flipped = 0;
    let mut counts = (0usize, 0usize, 0usize);
    for case in cases {
        let name = case
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if args.case.as_deref().is_some_and(|c| !name.contains(c)) {
            continue;
        }
        let text = match fs::read_to_string(case.join("manifest.toml")) {
            Ok(t) => t,
            Err(e) => {
                problems.push(format!("{name}: {e}"));
                continue;
            }
        };
        let manifest: toml::Table = match toml::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                problems.push(format!("{name}: manifest: {e}"));
                continue;
            }
        };
        let Some(backends) = manifest.get("backend").and_then(toml::Value::as_table) else {
            continue;
        };
        let mut new_text = text.clone();
        for (backend, table) in backends {
            let status = table.get("status").and_then(toml::Value::as_str).unwrap_or("");
            // Wall-clock time only reports how long each case took; it never affects a result.
            #[allow(clippy::disallowed_methods)]
            let started = Instant::now();
            let outcome = run_backend(&case, &manifest, backend, workers, args.max_runs);
            let secs = started.elapsed().as_secs_f64();
            let features = strings(manifest.get("features"));
            let in_gate = gate_scope(&milestone, &args.area, &name, &features, backend);
            match (&outcome, status) {
                (Outcome::Pass(msg), "pass") => {
                    counts.0 += 1;
                    println!("ok    {name} [{backend}] {msg} ({secs:.1}s)");
                }
                (Outcome::Pass(msg), _) => {
                    counts.0 += 1;
                    if args.ratchet {
                        match flip_to_pass(&new_text, backend) {
                            Some(t) => {
                                new_text = t;
                                flipped += 1;
                                println!("pass  {name} [{backend}] {msg} ({secs:.1}s): status flipped to pass");
                            }
                            None => problems.push(format!("{name} [{backend}]: cannot edit the status line")),
                        }
                    } else {
                        println!("stale {name} [{backend}] {msg} ({secs:.1}s): passes; run --ratchet");
                        problems.push(format!(
                            "{name} [{backend}]: stale status `{status}`: update the manifest"
                        ));
                    }
                }
                (Outcome::Fail(msg), "pass") => {
                    counts.1 += 1;
                    println!("FAIL  {name} [{backend}] {msg} ({secs:.1}s)");
                    problems.push(format!("{name} [{backend}]: status `pass` but {msg}"));
                }
                (Outcome::Fail(msg), _) => {
                    counts.1 += 1;
                    println!("fail  {name} [{backend}] {msg} ({secs:.1}s)");
                    if args.gate && in_gate {
                        problems.push(format!("{name} [{backend}]: in the {milestone} gate but {msg}"));
                    }
                }
                (Outcome::NotRunnable(why), "pass") => {
                    counts.2 += 1;
                    problems.push(format!("{name} [{backend}]: status `pass` but not runnable: {why}"));
                }
                (Outcome::NotRunnable(why), _) => {
                    counts.2 += 1;
                    let first = why.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                    println!("n/a   {name} [{backend}] {first}");
                    if args.gate && in_gate {
                        problems.push(format!(
                            "{name} [{backend}]: in the {milestone} gate but not runnable: {why}"
                        ));
                    }
                }
            }
        }
        if new_text != text
            && let Err(e) = fs::write(case.join("manifest.toml"), &new_text)
        {
            problems.push(format!("{name}: writing the manifest: {e}"));
        }
    }
    println!(
        "corpus {}: {} passed, {} failed, {} not runnable in this build; {flipped} status(es) flipped",
        args.area, counts.0, counts.1, counts.2
    );
    if problems.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!("corpus: {} problem(s):", problems.len());
        for p in &problems {
            eprintln!("  {p}");
        }
        ExitCode::from(1)
    }
}

/// Whether a backend of a case belongs to the current slice's gate (docs/design/SLICES.md).
/// Slice 2's Blossom subset (docs/design/SLICES.md, slice 2; docs/plan/notes/S2.md): the FEATURES ids the `.bls`
/// frontend and the oracle implement. A `core/` or `async/` case whose language features all lie here is in the gate.
const S2_SUBSET: &[&str] = &[
    // Programs, declarations, handlers, views, statements, bodies, expressions, aggregates, facts and bootstrap.
    "LANG-020", "LANG-023", "LANG-040", "LANG-041", "LANG-042", "LANG-043", "LANG-045", "LANG-046", "LANG-047",
    "LANG-052", "LANG-060", "LANG-061", "LANG-062", "LANG-063", "LANG-064", "LANG-065", "LANG-066", "LANG-067",
    "LANG-069", "LANG-080", "LANG-081", "LANG-082", "LANG-083", "LANG-084", "LANG-085", "LANG-086", "LANG-088",
    "LANG-089", "LANG-090", "LANG-100", "LANG-101", "LANG-190",
    // Lattices (the core built-ins), locations and roles, timers, invariants, `.ded` includes, the directory, senders.
    "LANG-120", "LANG-121", "LANG-122", "LANG-123", "LANG-124", "LANG-125", "LANG-126", "LANG-127", "LANG-128",
    "LANG-280", "LANG-150", "LANG-152", "LANG-153", "LANG-172", "LANG-200", "LANG-220", "LANG-240", "LANG-241",
    // Semantics the oracle realizes.
    "SEM-003", "SEM-004", "SEM-005", "SEM-006", "SEM-007", "SEM-008", "SEM-012", "SEM-013", "SEM-020", "SEM-021",
    "SEM-022", "SEM-031", "SEM-050", "SEM-060", "SEM-061", "SEM-101", "SEM-103",
    // The engine behaviors those cases pin (persistence, fixpoints, lattice evaluation, keys).
    "ENG-003", "ENG-004", "ENG-041", "ENG-042", "ENG-043", "ENG-062", "ENG-067",
];

fn gate_scope(milestone: &str, area: &str, case: &str, features: &[String], backend: &str) -> bool {
    // The P1 search reductions behind Molly's published run counts (single-shot mode, vacuity pruning, symmetry)
    // are not built yet; cases that list them wait for them (SLICES.md, slice 1, "Stretch").
    let needs_p1_reductions = features
        .iter()
        .any(|f| matches!(f.as_str(), "TEST-030" | "TEST-031" | "TEST-032"));
    match milestone {
        // BENCH-133d (Flux 22/21/1) is excluded from S1's gate: SLICES.md, slice 1, "Exception".
        "S1" => {
            area == "ldfi"
                && (backend == "oracle" || backend == "ldfi")
                && !case.starts_with("BENCH-133d")
                && !needs_p1_reductions
        }
        // Analyses (ANA) and verification tooling (TEST, VER) are other backends' concerns: the oracle case runs
        // without them.
        "S2" => {
            (area == "core" || area == "async")
                && (backend == "oracle" || backend == "compile")
                && features.iter().all(|f| {
                    f.starts_with("ANA-")
                        || f.starts_with("TEST-")
                        || f.starts_with("VER-")
                        || S2_SUBSET.contains(&f.as_str())
                })
        }
        _ => false,
    }
}

fn collect_cases(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            if path.join("manifest.toml").is_file() {
                out.push(path);
            } else {
                collect_cases(&path, out)?;
            }
        }
    }
    Ok(())
}

/// `status = "pass"` in `[backend.<name>]`, dropping its `unimplemented` and `until` keys (a key's value may span
/// lines until its closing bracket).
fn flip_to_pass(text: &str, backend: &str) -> Option<String> {
    let header = format!("[backend.{backend}]");
    let mut out = Vec::new();
    let mut inside = false;
    let mut skipping = false;
    let mut flipped = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') && !skipping {
            inside = trimmed.trim_end() == header;
        }
        if inside {
            if skipping {
                if line.contains(']') {
                    skipping = false;
                }
                continue;
            }
            if trimmed.starts_with("status") {
                out.push("status = \"pass\"".to_owned());
                flipped = true;
                continue;
            }
            if trimmed.starts_with("unimplemented") || trimmed.starts_with("until") || trimmed.starts_with("issue") {
                if trimmed.contains('[') && !trimmed.contains(']') {
                    skipping = true;
                }
                continue;
            }
        }
        out.push(line.to_owned());
    }
    flipped.then(|| {
        let mut s = out.join("\n");
        if text.ends_with('\n') {
            s.push('\n');
        }
        s
    })
}

fn run_backend(case: &Path, m: &toml::Table, backend: &str, workers: usize, max_runs: u64) -> Outcome {
    let Some(program) = m.get("program").and_then(toml::Value::as_str) else {
        return Outcome::NotRunnable("multi-program cases arrive with slice 2".into());
    };
    if program.ends_with(".bls") {
        if m.contains_key("include") || m.contains_key("spec") {
            return Outcome::NotRunnable(
                "`.bls` cases with extra sources or a spec file arrive with a later slice".into(),
            );
        }
        return super::corpus_bls::run(case, m, program, backend);
    }
    if !program.ends_with(".ded") {
        return Outcome::Fail(format!("`{program}` is neither a `.bls` nor a `.ded` program"));
    }
    let mut files = vec![case.join(program)];
    if let Some(extra) = m.get("include").and_then(toml::Value::as_array) {
        files.extend(extra.iter().filter_map(toml::Value::as_str).map(|f| case.join(f)));
    }
    match backend {
        "oracle" => oracle_backend(&files, m),
        "ldfi" => ldfi_backend(&files, m, workers, max_runs),
        "compile" => ded_compile_backend(&files, m),
        other => Outcome::NotRunnable(format!("the `{other}` backend arrives with a later slice")),
    }
}

fn compile(files: &[PathBuf], nodes: &[String]) -> Result<SimArtifact, Outcome> {
    let files: Vec<String> = files.iter().map(|f| f.to_string_lossy().into_owned()).collect();
    let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();
    let node_refs: Vec<&str> = nodes.iter().map(String::as_str).collect();
    let (result, sources) = compile_files(&file_refs, &node_refs);
    match result {
        Ok(a) => Ok(a),
        Err(DedError::Rejected(d)) => {
            let unimplemented = d.iter().any(blossom_driver::render::is_not_implemented);
            let text: String = d.iter().map(|x| render(x, &sources)).collect();
            Err(if unimplemented {
                Outcome::NotRunnable(text)
            } else {
                Outcome::Fail(format!("compile error:\n{text}"))
            })
        }
        Err(e) => Err(Outcome::Fail(e.to_string())),
    }
}

/// The `.ded` frontend and the stratification check against `[[expect_diag]]`, exactly as a multiset.
fn ded_compile_backend(files: &[PathBuf], m: &toml::Table) -> Outcome {
    let files: Vec<String> = files.iter().map(|f| f.to_string_lossy().into_owned()).collect();
    let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();
    let (result, sources) = compile_files(&file_refs, &[]);
    let diags: Vec<blossom_base::Diagnostic> = match result {
        Ok(a) => match blossom_analysis::strata::check(a.protocol.get()) {
            Ok(d) => d.iter().cloned().collect(),
            Err(e) => return Outcome::Fail(e.to_string()),
        },
        Err(DedError::Rejected(d)) => d.iter().cloned().collect(),
        Err(e) => return Outcome::Fail(e.to_string()),
    };
    if diags.iter().any(blossom_driver::render::is_not_implemented) {
        let text: String = diags.iter().map(|x| render(x, &sources)).collect();
        return Outcome::NotRunnable(text);
    }
    super::corpus_bls::compare_diags(&diags, &sources, m)
}

fn strings(v: Option<&toml::Value>) -> Vec<String> {
    v.and_then(toml::Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

fn oracle_backend(files: &[PathBuf], m: &toml::Table) -> Outcome {
    let nodes: Vec<String> = m
        .get("deploy")
        .and_then(|d| d.get("nodes"))
        .and_then(toml::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|n| n.get("name").and_then(toml::Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let Some(ticks) = m
        .get("run")
        .and_then(|r| r.get("ticks"))
        .and_then(toml::Value::as_integer)
    else {
        return Outcome::Fail("a `.ded` oracle case needs [run] ticks".into());
    };
    let Ok(last) = u64::try_from(ticks - 1) else {
        return Outcome::Fail("[run] ticks must be at least 1".into());
    };
    let artifact = match compile(files, &nodes) {
        Ok(a) => a,
        Err(o) => return o,
    };
    let sim = match SpecSim::new(&artifact) {
        Ok(s) => s,
        Err(e) => return Outcome::Fail(e.to_string()),
    };
    let schedule = match super::corpus_bls::faults(&artifact, m) {
        Ok(f) => f,
        Err(o) => return o,
    };
    // The synchronous harness follows CR-20 (a crashed node is frozen); Molly's view is LDFI's.
    let run = match sim.run_with_view(Tick(last), &schedule, false, blossom_sim::CrashView::Frozen) {
        Ok(r) => r,
        Err(e) => return Outcome::Fail(e.to_string()),
    };
    let mut bad = Vec::new();
    let mut checked = 0;
    for x in m.get("expect").and_then(toml::Value::as_array).into_iter().flatten() {
        checked += 1;
        if let Err(e) = super::corpus_bls::expect(&artifact, &run, last, x) {
            bad.push(e);
        }
    }
    for x in m
        .get("expect_send")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        checked += 1;
        if let Err(e) = super::corpus_bls::expect_send(&artifact, &run, x) {
            bad.push(e);
        }
    }
    if m.contains_key("expect_error") {
        bad.push(
            "[[expect_error]] is not checked for `.ded` programs, whose runs have no runtime errors to expect".into(),
        );
    }
    if bad.is_empty() {
        Outcome::Pass(format!("{checked} expectation(s) hold"))
    } else {
        Outcome::Fail(bad.join("; "))
    }
}

impl super::corpus_bls::Subject for SimArtifact {
    fn node(&self, name: &str) -> Option<blossom_value::time::NodeId> {
        self.node_id(name)
    }
    fn node_name(&self, n: blossom_value::time::NodeId) -> String {
        self.nodes
            .get(n.0 as usize)
            .map_or_else(|| "?".to_owned(), |s| s.as_str().to_owned())
    }
    fn node_count(&self) -> usize {
        self.nodes.len()
    }
    fn rel(&self, name: &str) -> Result<blossom_base::RelId, String> {
        self.rels
            .iter()
            .find(|r| r.name.as_str() == name)
            .and_then(|r| r.protocol)
            .ok_or_else(|| format!("`{name}` is not a protocol relation"))
    }
    fn channel(&self, name: &str) -> Result<blossom_base::RelId, String> {
        self.rels
            .iter()
            .find(|r| r.name.as_str() == name)
            .and_then(|r| r.channel)
            .ok_or_else(|| format!("`{name}` is not sent with `@async`"))
    }
    /// A row without Molly's location column; a channel's destination column (0) is left open.
    fn row(&self, rel: blossom_base::RelId, v: &toml::Value) -> Result<Vec<Option<Value>>, String> {
        let program = self.protocol.get();
        let decl = program.rels.get(rel).ok_or("unknown relation")?;
        let is_channel = self.rels.iter().any(|r| r.channel == Some(rel));
        let items = v.as_array().ok_or_else(|| format!("row {v} is not an array"))?;
        let skip = usize::from(is_channel);
        if items.len() + skip != decl.schema.cols.len() {
            return Err(format!("row {v} does not match the relation's columns"));
        }
        let mut out = vec![None; skip];
        for (x, c) in items.iter().zip(decl.schema.cols.iter().skip(skip)) {
            let ty = program.types.get(c.ty).ok_or("unknown type")?;
            out.push(Some(
                value(self, x, ty).ok_or_else(|| format!("{x} does not decode as {ty:?}"))?,
            ));
        }
        Ok(out)
    }
}

/// A manifest value decoded by the column's type (PLAN §5.1): strings in node columns name nodes.
fn value(a: &SimArtifact, v: &toml::Value, ty: &TypeDef) -> Option<Value> {
    Some(match (v, ty) {
        (toml::Value::Integer(i), TypeDef::Int(IntTy::U64)) => Value::Int(IntValue::U64(u64::try_from(*i).ok()?)),
        (toml::Value::Integer(i), TypeDef::Int(IntTy::I64)) => Value::Int(IntValue::I64(*i)),
        (toml::Value::String(s), TypeDef::Node(_)) => Value::Node(a.node_id(s)?),
        (toml::Value::String(s), TypeDef::Str) => Value::Str(s.as_str().into()),
        _ => return None,
    })
}

fn ldfi_backend(files: &[PathBuf], m: &toml::Table, workers: usize, max_runs: u64) -> Outcome {
    let Some(ld) = m.get("expect_ldfi").and_then(toml::Value::as_table) else {
        return Outcome::Fail("an ldfi backend without [expect_ldfi]".into());
    };
    if ld.get("crash_view").and_then(toml::Value::as_str).unwrap_or("molly") != "molly" {
        return Outcome::NotRunnable("the frozen crash view arrives with slice 2".into());
    }
    let int = |k: &str| {
        ld.get(k)
            .and_then(toml::Value::as_integer)
            .and_then(|i| u64::try_from(i).ok())
    };
    let (Some(eot), Some(eff), Some(crashes)) = (int("eot"), int("eff"), int("crashes")) else {
        return Outcome::Fail("[expect_ldfi] needs eot, eff and crashes".into());
    };
    let nodes = strings(ld.get("nodes"));
    let want = ld.get("verdict").and_then(toml::Value::as_str).unwrap_or("");
    let artifact = match compile(files, &nodes) {
        Ok(a) => a,
        Err(Outcome::Fail(msg)) if want == "program_error" => return Outcome::Pass(format!("program error: {msg}")),
        Err(o) => return o,
    };
    let (Ok(crashes), Ok(n)) = (u32::try_from(crashes), u32::try_from(artifact.nodes.len())) else {
        return Outcome::Fail("crash or node count out of range".into());
    };
    let spec = match FailureSpec::new(eot, eff, crashes, n) {
        Ok(s) => s,
        Err(e) => return Outcome::Fail(e.to_string()),
    };
    let mut config = LdfiConfig::new(spec);
    config.workers = workers;
    config.max_runs = max_runs;
    let sim = match SpecSim::new(&artifact) {
        Ok(s) => s,
        Err(e) => return Outcome::Fail(e.to_string()),
    };
    let report = match blossom_ldfi::run(&sim, &config) {
        Ok(r) => r,
        Err(e) => return Outcome::Fail(e.to_string()),
    };
    let got = match report.verdict {
        Verdict::Counterexample => "counterexample",
        Verdict::NoCounterexample => "no_counterexample",
    };
    if got != want {
        return Outcome::Fail(format!("verdict {got} after {} runs, expected {want}", report.runs));
    }
    let mut note = match report.method {
        blossom_ldfi::Method::Lineage => format!("{got} in {} runs", report.runs),
        blossom_ldfi::Method::Exhaustive { states, .. } => {
            format!(
                "{got} by exhaustive certification ({states} states) after {} runs",
                report.runs
            )
        }
    };
    // A published run count is part of the expectation (the BENCH-136 cases pin Molly's counts, which need the P1
    // search reductions): exceeding it fails the case.
    if let Some(max) = int("runs_max") {
        let lineage = matches!(report.method, blossom_ldfi::Method::Lineage);
        if !lineage || report.runs > max {
            return Outcome::Fail(format!("{note}, but the published run count is at most {max}"));
        }
        note.push_str(&format!(" (published at most {max})"));
    }
    if let Some(stated) = ld.get("falsifiers").and_then(toml::Value::as_array) {
        let mut want_sets: Vec<Vec<String>> = stated
            .iter()
            .map(|set| {
                let mut v: Vec<String> = strings(Some(set)).into_iter().map(|l| l.replace(' ', "")).collect();
                v.sort();
                v
            })
            .collect();
        want_sets.sort();
        let got_sets = match falsifiers(&sim, &config) {
            Ok(sets) => {
                let mut v: Vec<Vec<String>> = sets.iter().map(|f| fault_labels(&artifact, f)).collect();
                v.sort();
                v
            }
            Err(e) => return Outcome::Fail(format!("falsifiers: {e}")),
        };
        if got_sets != want_sets {
            return Outcome::Fail(format!("falsifiers {got_sets:?}, expected {want_sets:?}"));
        }
        note.push_str(&format!("; {} falsifier set(s) exact", want_sets.len()));
    }
    Outcome::Pass(note)
}
