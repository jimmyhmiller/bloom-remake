//! `blossom ldfi`: lineage-driven fault injection (Molly-2, ARCHITECTURE §8).
//!
//! Slice 1 (docs/design/SLICES.md) runs LDFI on Molly `.ded` programs, whose `pre` and `post` rules are the outcome
//! spec: `blossom ldfi simplog.ded deliv_assert.ded --eot 4 --eff 2 --nodes a,b,c --crashes 0` is Molly's
//! `SyncFTChecker` (LANGUAGE §21.1). `.bls` programs with `spec` blocks follow in slice 2.
//!
//! Exit codes: 0 when no counterexample exists within the failure spec, 3 when one does (verification failed), 1 for
//! a program error, 7 for an unimplemented feature.

use std::process::ExitCode;

use blossom_ldfi::report::{fault_labels, post_lineage, render};
use blossom_ldfi::{FailureSpec, LdfiConfig, LdfiError, Verdict, falsifiers};
use blossom_sim::ded::DedSim;

use crate::common::{Context, ded};
use crate::exit::Exit;

/// Arguments of `blossom ldfi`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The program's `.ded` files (their `include`s are loaded too).
    #[arg(required = true)]
    pub files: Vec<String>,
    /// The deployment's nodes.
    #[arg(long, value_delimiter = ',', required = true)]
    pub nodes: Vec<String>,
    /// The end of time: the tick at which `pre` and `post` are read.
    #[arg(long)]
    pub eot: u64,
    /// The end of finite failures: messages sent before this tick may be lost.
    #[arg(long)]
    pub eff: u64,
    /// How many nodes may crash.
    #[arg(long, default_value_t = 0)]
    pub crashes: u32,
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
    /// How negated reads are supported: precise (tuple-level), conservative (relation-level, CR-31) or off (unsound
    /// for non-monotone programs; for experiments).
    #[arg(long, default_value = "precise")]
    pub negative_support: String,
    /// Runs the lineage-driven search may make before exhaustive certification decides.
    #[arg(long, default_value_t = 20_000)]
    pub max_runs: u64,
    /// States exhaustive certification may explore.
    #[arg(long, default_value_t = 50_000_000)]
    pub max_states: u64,
    /// Report the lineage-driven search's budget error instead of certifying exhaustively.
    #[arg(long)]
    pub no_exhaustive: bool,
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
    if !ded::all_ded(&args.files) {
        eprintln!("`blossom ldfi` on `.bls` specs arrives with slice 2 (docs/design/SLICES.md)");
        return crate::exit::not_implemented("TEST-029", "slice 2");
    }
    let artifact = match ded::compile(&args.files, &args.nodes) {
        Ok(a) => a,
        Err(code) => return code,
    };
    let nodes = match u32::try_from(artifact.nodes.len()) {
        Ok(n) => n,
        Err(_) => return Exit::UserError.into(),
    };
    let spec = match FailureSpec::new(args.eot, args.eff, args.crashes, nodes) {
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
    config.sat = args.sat.clone();

    config.workers = args
        .jobs
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get));
    let sim = match DedSim::new(&artifact) {
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
    match blossom_ldfi::run(&sim, &config) {
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
            match report.verdict {
                Verdict::NoCounterexample => Exit::Ok.into(),
                Verdict::Counterexample => Exit::VerifyFailed.into(),
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
