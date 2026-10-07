//! `blossom`: the Blossom command-line tool (ARCHITECTURE §12.5).
//!
//! This file is the dispatch table and is frozen after M1 (PLAN §4 D7): each subcommand lives in
//! `src/cmd/<name>.rs`, owned by the WP that implements it; global options and start-up live in `src/common/`.
//! There is no `corpus` subcommand: the corpus runs through `cargo run -p xtask -- corpus` (PLAN §4 D8).

#![deny(unsafe_op_in_unsafe_fn)]
// A command-line tool reports on stdout and stderr.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod cmd;
mod common;
mod exit;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// The Blossom language: compile, run, simulate and verify Blossom programs.
#[derive(Debug, Parser)]
#[command(name = "blossom", version, long_version = common::long_version(), after_help = exit::HELP)]
struct Cli {
    #[command(flatten)]
    global: common::GlobalArgs,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Compile a program and report diagnostics.
    Check(cmd::check::Args),
    /// Format Blossom sources.
    Fmt(cmd::fmt::Args),
    /// Compile a program to artifacts.
    Build(cmd::build::Args),
    /// Dump the rewritten program, strata and physical plans.
    Plan(cmd::plan::Args),
    /// Explain a diagnostic code (BLSnnnn).
    Explain(cmd::explain::Args),
    /// Run a node.
    Run(cmd::run::Args),
    /// Query a running node's database (Datalog).
    Query(cmd::query::Args),
    /// Create a deployment, or launch one locally.
    Deploy(cmd::deploy::Args),
    /// Create node identities and report node status.
    Node(cmd::node::Args),
    /// Explain the effective configuration.
    Config(cmd::config::Args),
    /// Simulate a deployment deterministically, or replay a trace.
    Sim(cmd::sim::Args),
    /// Convert and render traces.
    Trace(cmd::trace::Args),
    /// Lineage-driven fault injection (Molly-2).
    Ldfi(cmd::ldfi::Args),
    /// Run a spec's verification checks (bmc, smt, asp).
    Verify(cmd::verify::Args),
    /// Explain why a fact holds (provenance).
    Why(cmd::why::Args),
    /// Explain why a fact does not hold.
    Whynot(cmd::whynot::Args),
    /// Check compatibility against schema.lock.
    Compat(cmd::compat::Args),
    /// Append the current version to schema.lock.
    Release(cmd::release::Args),
    /// Inspect, verify, dump, back up and restore node stores.
    Store(cmd::store::Args),
    /// Run the embedded program on the oracle against the executor.
    SelfCheck(cmd::self_check::Args),
    /// An interactive REPL.
    Repl(cmd::repl::Args),
    /// Orchestrate a rolling upgrade.
    Upgrade(cmd::upgrade::Args),
    /// Admin-plane operations.
    Admin(cmd::admin::Args),
    /// Generate shell completions.
    Completions(cmd::completions::Args),
    /// Run the language server.
    Lsp(cmd::lsp::Args),
}

/// Runs the command on a thread with the stack evaluation needs (`EVAL_STACK_BYTES`, sized for the compile-time
/// evaluation depth bound), since the main thread's is smaller.
fn main() -> ExitCode {
    let cli = Cli::parse();
    let spawned = std::thread::Builder::new()
        .name("blossom".into())
        .stack_size(blossom_ir::depth::EVAL_STACK_BYTES)
        .spawn(move || dispatch(cli));
    match spawned {
        Ok(h) => match h.join() {
            Ok(code) => code,
            Err(panic) => std::panic::resume_unwind(panic),
        },
        Err(e) => {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "error: the command's thread could not start: {e}");
            exit::Exit::Internal.into()
        }
    }
}

fn dispatch(cli: Cli) -> ExitCode {
    let cx = match common::init(&cli.global) {
        Ok(cx) => cx,
        Err(code) => return code,
    };
    match cli.command {
        Commands::Check(args) => cmd::check::run(args, &cx),
        Commands::Fmt(args) => cmd::fmt::run(args, &cx),
        Commands::Build(args) => cmd::build::run(args, &cx),
        Commands::Plan(args) => cmd::plan::run(args, &cx),
        Commands::Explain(args) => cmd::explain::run(args, &cx),
        Commands::Run(args) => cmd::run::run(args, &cx),
        Commands::Query(args) => cmd::query::run(args, &cx),
        Commands::Deploy(args) => cmd::deploy::run(args, &cx),
        Commands::Node(args) => cmd::node::run(args, &cx),
        Commands::Config(args) => cmd::config::run(args, &cx),
        Commands::Sim(args) => cmd::sim::run(args, &cx),
        Commands::Trace(args) => cmd::trace::run(args, &cx),
        Commands::Ldfi(args) => cmd::ldfi::run(args, &cx),
        Commands::Verify(args) => cmd::verify::run(args, &cx),
        Commands::Why(args) => cmd::why::run(args, &cx),
        Commands::Whynot(args) => cmd::whynot::run(args, &cx),
        Commands::Compat(args) => cmd::compat::run(args, &cx),
        Commands::Release(args) => cmd::release::run(args, &cx),
        Commands::Store(args) => cmd::store::run(args, &cx),
        Commands::SelfCheck(args) => cmd::self_check::run(args, &cx),
        Commands::Repl(args) => cmd::repl::run(args, &cx),
        Commands::Upgrade(args) => cmd::upgrade::run(args, &cx),
        Commands::Admin(args) => cmd::admin::run(args, &cx),
        Commands::Completions(args) => cmd::completions::run(args, &cx),
        Commands::Lsp(args) => cmd::lsp::run(args, &cx),
    }
}
