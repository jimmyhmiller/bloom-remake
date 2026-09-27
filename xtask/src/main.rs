//! `cargo run -p xtask -- <task>`: repository tasks (ARCHITECTURE §1.1, §11; PLAN M1.1 §6).
//!
//! This file is the dispatch table and is frozen after M1 (PLAN §4 D7). Each task lives in `src/cmd/<name>.rs`,
//! owned by the WP that implements it; tasks not implemented yet exit with code 7 naming their WP.

#![deny(unsafe_op_in_unsafe_fn)]
// A command-line tool reports on stdout and stderr.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod cmd;
mod rustsrc;
mod util;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Blossom repository tasks.
#[derive(Debug, Parser)]
#[command(
    name = "xtask",
    after_help = "Exit codes: 0 ok, 1 check failed, 2 usage error, 7 not implemented yet."
)]
struct Cli {
    #[command(subcommand)]
    command: Task,
}

#[derive(Debug, Subcommand)]
enum Task {
    /// Check every dependency edge against xtask/layers.toml (ARCH-01).
    CheckLayers(cmd::check_layers::Args),
    /// Check that the sans-IO node uses no file system, network, threads, clock or tokio (ARCH-03).
    CheckSansIo(cmd::check_sans_io::Args),
    /// Check diagnostic codes against the registry and LANGUAGE §20 (ARCHITECTURE §12.1).
    CheckCodes(cmd::check_codes::Args),
    /// Run, lint and ratchet the golden corpus (BENCH-000).
    Corpus(cmd::corpus::Args),
    /// Bless expected corpus outputs.
    Bless(cmd::bless::Args),
    /// Report FEATURES coverage.
    Coverage(cmd::coverage::Args),
    /// Crash-consistency checks over SimFs.
    Crashcheck(cmd::crashcheck::Args),
    /// Fetch benchmark datasets into datasets/.
    FetchDatasets(cmd::fetch_datasets::Args),
    /// Generate the typed AST from blossom.ungram.
    GenAst(cmd::gen_ast::Args),
    /// Generate the codegen corpus crate.
    GenCodegenCorpus(cmd::gen_codegen_corpus::Args),
    /// Check that generated code names only the execution ABIs.
    CheckCodegenAbi(cmd::check_codegen_abi::Args),
    /// Report benchmark results.
    BenchReport(cmd::bench_report::Args),
    /// Build reproducible, signed releases.
    Release(cmd::release::Args),
    /// Generate the standard-library and guide documentation.
    GenDocs(cmd::gen_docs::Args),
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Task::CheckLayers(args) => cmd::check_layers::run(args),
        Task::CheckSansIo(args) => cmd::check_sans_io::run(args),
        Task::CheckCodes(args) => cmd::check_codes::run(args),
        Task::Corpus(args) => cmd::corpus::run(args),
        Task::Bless(args) => cmd::bless::run(args),
        Task::Coverage(args) => cmd::coverage::run(args),
        Task::Crashcheck(args) => cmd::crashcheck::run(args),
        Task::FetchDatasets(args) => cmd::fetch_datasets::run(args),
        Task::GenAst(args) => cmd::gen_ast::run(args),
        Task::GenCodegenCorpus(args) => cmd::gen_codegen_corpus::run(args),
        Task::CheckCodegenAbi(args) => cmd::check_codegen_abi::run(args),
        Task::BenchReport(args) => cmd::bench_report::run(args),
        Task::Release(args) => cmd::release::run(args),
        Task::GenDocs(args) => cmd::gen_docs::run(args),
    }
}
