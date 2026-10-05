//! `blossom ldfi`: lineage-driven fault injection (Molly-2, ARCHITECTURE §8).
//!
//! Slice 1 (docs/design/SLICES.md) runs LDFI on Molly `.ded` programs, whose `pre` and `post` rules are the outcome
//! spec: `blossom ldfi simplog.ded deliv_assert.ded --eot 4 --eff 2 --nodes a,b,c --crashes 0` is Molly's
//! `SyncFTChecker` (LANGUAGE §21.1). For a Blossom program, `blossom ldfi specs.bls --spec AckRbFaults` checks the
//! spec: its target, scenario, `faults` and `pre`/`post` (LANGUAGE §17), and compares the verdict with its
//! `check ldfi expect …`.
//!
//! Exit codes: 0 when no counterexample exists within the failure spec, 3 when one does (verification failed), 1 for
//! a program error, 7 for an unimplemented feature.

use std::process::ExitCode;

use blossom_ldfi::report::{fault_labels, post_lineage, render};
use blossom_ldfi::{FailureSpec, LdfiConfig, LdfiError, Verdict, falsifiers};
use blossom_sim::spec::SpecSim;

use crate::common::{Context, ded};
use crate::exit::Exit;

/// Arguments of `blossom ldfi`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The program's `.ded` files (their `include`s are loaded too), or one `.bls` file holding the spec.
    #[arg(required = true)]
    pub files: Vec<String>,
    /// The spec to check (`.bls` only).
    #[arg(long)]
    pub spec: Option<String>,
    /// The deployment's nodes (`.ded` only: a spec names its own).
    #[arg(long, value_delimiter = ',')]
    pub nodes: Vec<String>,
    /// The end of time: the tick at which `pre` and `post` are read (`.ded` only: a spec has `faults`).
    #[arg(long)]
    pub eot: Option<u64>,
    /// The end of finite failures: messages sent before this tick may be lost (`.ded` only).
    #[arg(long)]
    pub eff: Option<u64>,
    /// How many nodes may crash (`.ded` only).
    #[arg(long)]
    pub crashes: Option<u32>,
    /// Report every counterexample, not only the first.
    #[arg(long)]
    pub find_all: bool,
    /// Print the Appendix-B-minimal falsifiers of the failure-free run's `post` goals instead of a verdict.
    #[arg(long)]
    pub falsifiers: bool,
    /// Print the failure-free lineage of every `post` tuple.
    #[arg(long)]
    pub lineage: bool,
    /// Print search statistics.
    #[arg(long)]
    pub stats: bool,
    /// Print a line per run of the lineage-driven search to stderr as it goes: its faults, its lineage's size, the
    /// hypotheses it suggested, and what running it, building its lineage and finding its hypotheses took.
    #[arg(long)]
    pub progress: bool,
    /// How negated reads are supported: precise (tuple-level), conservative (relation-level, CR-31) or off (unsound
    /// for non-monotone programs; for experiments).
    #[arg(long, default_value = "precise")]
    pub negative_support: String,
    /// Runs the lineage-driven search may make before exhaustive certification decides.
    #[arg(long, default_value_t = 20_000)]
    pub max_runs: u64,
    /// States exhaustive certification may explore.
    #[arg(long, default_value_t = 1_000_000)]
    pub max_states: u64,
    /// Report the lineage-driven search's budget error instead of certifying exhaustively.
    #[arg(long)]
    pub no_exhaustive: bool,
    /// How to decide: auto (by enumeration when the spec's admissible schedules fit --max-schedules, the faster
    /// exact search at that size; else by the lineage-driven search), lineage, or enumerate (every admissible
    /// schedule run in full, fewest faults first). --find-all and --lineage use the lineage-driven search.
    #[arg(long, default_value = "auto")]
    pub method: String,
    /// Fault schedules enumeration may run (`--method auto` enumerates when the spec has at most this many; also the
    /// budget of exhaustive certification of a program it cannot step round by round: crash-restarts, guarded timers,
    /// streams).
    #[arg(long, default_value_t = 100_000)]
    pub max_schedules: u64,
    /// Hazard entries the runs of the lineage-driven search may share (0: encode every run from scratch). Results do
    /// not depend on it.
    #[arg(long, default_value_t = 4_000_000)]
    pub shared_hazards: usize,
    /// Worker threads (default: the machine's parallelism). Results do not depend on it.
    #[arg(long)]
    pub jobs: Option<usize>,
    /// The SAT backend: cadical-plain (CaDiCaL tuned for many small incremental calls), cadical, batsat or
    /// exhaustive.
    #[arg(long, default_value = "cadical-plain")]
    pub sat: String,
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = cx;
    let (artifact, eot, eff, crashes, restart, max_omissions, delays, expect) = if ded::all_ded(&args.files) {
        if args.spec.is_some() {
            eprintln!("`--spec` names a spec of a `.bls` file");
            return Exit::Usage.into();
        }
        let (Some(eot), Some(eff)) = (args.eot, args.eff) else {
            eprintln!("a `.ded` program needs `--eot` and `--eff`");
            return Exit::Usage.into();
        };
        if args.nodes.is_empty() {
            eprintln!("a `.ded` program needs `--nodes`");
            return Exit::Usage.into();
        }
        let artifact = match ded::compile(&args.files, &args.nodes) {
            Ok(a) => a,
            Err(code) => return code,
        };
        (artifact, eot, eff, args.crashes.unwrap_or(0), None, None, None, None)
    } else {
        let ([file], Some(name)) = (args.files.as_slice(), &args.spec) else {
            eprintln!("a Blossom spec is checked with `blossom ldfi FILE.bls --spec NAME`");
            return Exit::Usage.into();
        };
        if !args.nodes.is_empty() || args.eot.is_some() || args.eff.is_some() || args.crashes.is_some() {
            eprintln!("a spec gives its own nodes and `faults`; drop `--nodes`, `--eot`, `--eff` and `--crashes`");
            return Exit::Usage.into();
        }
        let spec = match crate::common::bls::compile_spec(file, name) {
            Ok(s) => s,
            Err(code) => return code,
        };
        for (what, _) in &spec.not_run {
            eprintln!("note: {what} is not run: this build does not implement it");
        }
        let Some(faults) = spec.faults else {
            eprintln!("spec `{name}` has no `faults`");
            return Exit::UserError.into();
        };
        let expect = spec
            .checks
            .iter()
            .find(|c| c.tool.as_str() == "ldfi")
            .and_then(|c| c.expect);
        (
            spec.artifact,
            faults.eot,
            faults.eff,
            faults.crashes,
            faults.restart,
            faults.omissions,
            faults.delay.map(|d| (d, faults.delays)),
            expect,
        )
    };
    let nodes = match u32::try_from(artifact.nodes.len()) {
        Ok(n) => n,
        Err(_) => return Exit::UserError.into(),
    };
    let spec = match FailureSpec::new(eot, eff, crashes, nodes).and_then(|s| {
        let s = match max_omissions {
            Some(k) => s.with_max_omissions(k),
            None => s,
        };
        let s = match restart {
            Some(d) => s.with_restart(d)?,
            None => s,
        };
        match delays {
            Some((delay, max)) => s.with_delays(delay, max),
            None => Ok(s),
        }
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Exit::UserError.into();
        }
    };
    let mut config = LdfiConfig::new(spec);
    config.find_all = args.find_all;
    config.negative_support = match args.negative_support.as_str() {
        "precise" => blossom_ldfi::NegSupport::Precise,
        "conservative" => blossom_ldfi::NegSupport::Conservative,
        "off" => blossom_ldfi::NegSupport::Off,
        other => {
            eprintln!("unknown negative support `{other}`: expected precise, conservative or off");
            return Exit::Usage.into();
        }
    };
    config.max_runs = args.max_runs;
    config.exhaustive_fallback = (!args.no_exhaustive).then_some(args.max_states);
    config.max_schedules = args.max_schedules;
    config.shared_hazards = args.shared_hazards;
    config.sat = args.sat.clone();

    if args.progress {
        let names: Vec<String> = artifact.nodes.iter().map(|n| n.as_str().to_owned()).collect();
        config.observer = Some(blossom_ldfi::ObserverRef(std::sync::Arc::new(Progress {
            clock: crate::common::stopwatch::Stopwatch::start(),
            names,
        })));
    }
    config.workers = args
        .jobs
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get));
    let externs = match crate::common::std_externs() {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            return Exit::Internal.into();
        }
    };
    let sim = match SpecSim::with_externs(&artifact, externs) {
        Ok(s) => s,
        Err(e) => return fail(&LdfiError::Sim(e)),
    };
    if args.falsifiers {
        return match falsifiers(&sim, &config) {
            Ok(sets) => {
                println!("{} minimal falsifier(s)", sets.len());
                for f in sets {
                    println!("  {{{}}}", fault_labels(&artifact, &f).join(", "));
                }
                Exit::Ok.into()
            }
            Err(e) => fail(&e),
        };
    }
    let lineage_only = args.find_all || args.lineage;
    let report = match args.method.as_str() {
        "auto" if !lineage_only => blossom_ldfi::decide(&sim, &config),
        "auto" | "lineage" => blossom_ldfi::run(&sim, &config),
        "enumerate" if !lineage_only => blossom_ldfi::enumerate(&sim, &config),
        "enumerate" => {
            eprintln!("--find-all and --lineage need the lineage-driven search, not --method enumerate");
            return Exit::Usage.into();
        }
        other => {
            eprintln!("unknown method `{other}`: expected auto, lineage or enumerate");
            return Exit::Usage.into();
        }
    };
    match report {
        Ok(report) => {
            print!("{}", render(&artifact, &report));
            if args.stats {
                let s = &report.stats;
                println!(
                    "\nsearch: {} hypotheses suggested, queue peak {}, executed by fault count: {}",
                    s.suggested,
                    s.queue_peak,
                    s.by_size
                        .iter()
                        .map(|(k, n)| format!("{k}:{n}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
            if args.lineage {
                println!("\nfailure-free lineage of `post`:");
                print!("{}", post_lineage(&artifact, &report.failure_free_graph, &report));
            }
            let holds = report.verdict == Verdict::NoCounterexample;
            match expect {
                Some(want) => {
                    let word = if want { "holds" } else { "fails" };
                    if want == holds {
                        println!("check ldfi expect {word}: as expected");
                        Exit::Ok.into()
                    } else {
                        println!("check ldfi expect {word}: NOT as expected");
                        Exit::VerifyFailed.into()
                    }
                }
                None if holds => Exit::Ok.into(),
                None => Exit::VerifyFailed.into(),
            }
        }
        Err(e) => fail(&e),
    }
}

fn fail(e: &LdfiError) -> ExitCode {
    eprintln!("{e}");
    match e {
        LdfiError::Unimplemented(_) => Exit::Unimplemented.into(),
        LdfiError::Internal(_) | LdfiError::Sat(_) => Exit::Internal.into(),
        LdfiError::Sim(blossom_sim::SimError::Internal(_)) => Exit::Internal.into(),
        LdfiError::Sim(blossom_sim::SimError::Unimplemented(_)) => Exit::Unimplemented.into(),
        _ => Exit::UserError.into(),
    }
}

/// `--progress`: a line per committed run, on stderr.
struct Progress {
    clock: crate::common::stopwatch::Stopwatch,
    names: Vec<String>,
}

impl blossom_ldfi::Observer for Progress {
    fn now_nanos(&self) -> u64 {
        self.clock.nanos()
    }

    fn run_done(&self, p: &blossom_ldfi::RunProgress) {
        let name = |n: blossom_value::time::NodeId| {
            self.names
                .get(n.0 as usize)
                .cloned()
                .unwrap_or_else(|| format!("node#{}", n.0))
        };
        let faults = blossom_ldfi::faults::labels(&p.faults, &name).join(", ");
        let secs = |ns: u64| ns as f64 / 1e9;
        eprintln!(
            "[{:>8.1}s] run {} {{{faults}}} {}: {} goals, {} firings, +{} hypotheses, queue {}, {} counterexample(s) | \
             run {:.2}s lineage {:.2}s hypotheses {:.2}s (encode {:.2}s, enumerate {:.2}s, {} SAT calls)",
            secs(self.clock.nanos()),
            p.runs,
            if p.good { "good" } else { "BAD" },
            p.goals,
            p.firings,
            p.suggested,
            p.queue,
            p.counterexamples,
            secs(p.execute_ns),
            secs(p.lineage_ns),
            secs(p.hypotheses_ns),
            secs(p.encode_ns),
            secs(p.enumerate_ns),
            p.solves,
        );
    }
}
