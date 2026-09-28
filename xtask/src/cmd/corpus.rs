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

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use blossom_artifact::ded::DedArtifact;
use blossom_driver::ded::compile_files;
use blossom_driver::render::render;
use blossom_front::ded::DedError;
use blossom_ldfi::report::fault_labels;
use blossom_ldfi::{FailureSpec, LdfiConfig, Verdict, falsifiers};
use blossom_sim::FaultSchedule;
use blossom_sim::ded::DedSim;
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
    /// LDFI's run budget per case.
    #[arg(long, default_value_t = 3_000_000)]
    pub max_runs: u64,
    /// The repository root (default: the one containing xtask).
    #[arg(long)]
    pub root: Option<PathBuf>,
}

/// What running one backend of one case found.
#[derive(Debug)]
enum Outcome {
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
            let in_gate = gate_scope(&milestone, &args.area, backend);
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

/// Whether a backend of a case in `area` belongs to the current slice's gate (docs/design/SLICES.md).
fn gate_scope(milestone: &str, area: &str, backend: &str) -> bool {
    match milestone {
        "S1" => area == "ldfi" && (backend == "oracle" || backend == "ldfi"),
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
    if !program.ends_with(".ded") {
        return Outcome::NotRunnable("`.bls` programs arrive with slice 2".into());
    }
    let mut files = vec![case.join(program)];
    if let Some(extra) = m.get("include").and_then(toml::Value::as_array) {
        files.extend(extra.iter().filter_map(toml::Value::as_str).map(|f| case.join(f)));
    }
    match backend {
        "oracle" => oracle_backend(&files, m),
        "ldfi" => ldfi_backend(&files, m, workers, max_runs),
        other => Outcome::NotRunnable(format!("the `{other}` backend arrives with a later slice")),
    }
}

fn compile(files: &[PathBuf], nodes: &[String]) -> Result<DedArtifact, Outcome> {
    let files: Vec<String> = files.iter().map(|f| f.to_string_lossy().into_owned()).collect();
    let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();
    let node_refs: Vec<&str> = nodes.iter().map(String::as_str).collect();
    let (result, sources) = compile_files(&file_refs, &node_refs);
    match result {
        Ok(a) => Ok(a),
        Err(DedError::Rejected(d)) => {
            let unimplemented = d.iter().any(|x| x.code.as_str() == "BLS0908");
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
    let sim = match DedSim::new(&artifact) {
        Ok(s) => s,
        Err(e) => return Outcome::Fail(e.to_string()),
    };
    let run = match sim.run(Tick(last), &FaultSchedule::default(), false) {
        Ok(r) => r,
        Err(e) => return Outcome::Fail(e.to_string()),
    };
    let mut bad = Vec::new();
    let mut checked = 0;
    for x in m.get("expect").and_then(toml::Value::as_array).into_iter().flatten() {
        let (Some(node_name), Some(rel_name)) = (
            x.get("node").and_then(toml::Value::as_str),
            x.get("rel").and_then(toml::Value::as_str),
        ) else {
            continue;
        };
        let Some(node) = artifact.node_id(node_name) else {
            return Outcome::Fail(format!("unknown node `{node_name}`"));
        };
        let Some(rel) = artifact
            .rels
            .iter()
            .find(|r| r.name.as_str() == rel_name)
            .and_then(|r| r.protocol)
        else {
            return Outcome::Fail(format!("`{rel_name}` is not a protocol relation"));
        };
        let program = artifact.protocol.get();
        let types: Vec<TypeDef> = program
            .rels
            .get(rel)
            .map(|d| {
                d.schema
                    .cols
                    .iter()
                    .filter_map(|c| program.types.get(c.ty).cloned())
                    .collect()
            })
            .unwrap_or_default();
        let decode = |row: &toml::Value| -> Option<Vec<Value>> {
            let items = row.as_array()?;
            if items.len() != types.len() {
                return None;
            }
            items
                .iter()
                .zip(&types)
                .map(|(v, ty)| value(&artifact, v, ty))
                .collect()
        };
        let at = |t: u64| -> BTreeSet<Vec<Value>> {
            run.node_tick(Tick(t), node)
                .map(|nt| nt.instance.rows(rel).map(|r| r.to_vec()).collect())
                .unwrap_or_default()
        };
        checked += 1;
        if let Some(t) = x.get("tick").and_then(toml::Value::as_integer) {
            let Ok(t) = u64::try_from(t) else { continue };
            let want: Option<BTreeSet<Vec<Value>>> = x
                .get("rows")
                .and_then(toml::Value::as_array)
                .map(|rows| rows.iter().map(decode).collect())
                .unwrap_or(None);
            match want {
                Some(want) if at(t) == want => {}
                Some(_) => bad.push(format!("{rel_name}@{node_name} tick {t}: rows differ")),
                None => bad.push(format!(
                    "{rel_name}@{node_name}: rows do not match the relation's columns"
                )),
            }
            continue;
        }
        let Some(row) = x.get("row").and_then(decode) else {
            bad.push(format!(
                "{rel_name}@{node_name}: row does not match the relation's columns"
            ));
            continue;
        };
        for (key, present) in [("holds", true), ("absent", false)] {
            if let Some(range) = x.get(key) {
                for t in ticks_of(range, last) {
                    if at(t).contains(&row) != present {
                        bad.push(format!(
                            "{rel_name}{row:?}@{node_name} tick {t}: expected {}",
                            if present { "present" } else { "absent" }
                        ));
                    }
                }
            }
        }
    }
    if bad.is_empty() {
        Outcome::Pass(format!("{checked} expectation(s) hold"))
    } else {
        Outcome::Fail(bad.join("; "))
    }
}

/// A manifest value decoded by the column's type (PLAN §5.1): strings in node columns name nodes.
fn value(a: &DedArtifact, v: &toml::Value, ty: &TypeDef) -> Option<Value> {
    Some(match (v, ty) {
        (toml::Value::Integer(i), TypeDef::Int(IntTy::U64)) => Value::Int(IntValue::U64(u64::try_from(*i).ok()?)),
        (toml::Value::Integer(i), TypeDef::Int(IntTy::I64)) => Value::Int(IntValue::I64(*i)),
        (toml::Value::String(s), TypeDef::Node(_)) => Value::Node(a.node_id(s)?),
        (toml::Value::String(s), TypeDef::Str) => Value::Str(s.as_str().into()),
        _ => return None,
    })
}

/// `k`, `a..=b`, `a..` (to the last tick) or `..=b` (from tick 0).
fn ticks_of(range: &toml::Value, last: u64) -> Vec<u64> {
    let text = match range {
        toml::Value::Integer(i) => return u64::try_from(*i).map(|t| vec![t]).unwrap_or_default(),
        toml::Value::String(s) => s.clone(),
        _ => return Vec::new(),
    };
    match text.split_once("..") {
        None => text.parse().map(|t| vec![t]).unwrap_or_default(),
        Some((lo, hi)) => {
            let lo = if lo.is_empty() { 0 } else { lo.parse().unwrap_or(0) };
            let hi = match hi.strip_prefix('=') {
                Some(h) => h.parse().unwrap_or(last),
                None => last,
            };
            (lo..=hi).collect()
        }
    }
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
    let sim = match DedSim::new(&artifact) {
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
    let mut note = format!("{got} in {} runs", report.runs);
    if let Some(max) = int("runs_max") {
        note.push_str(&format!(" (published {max})"));
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
