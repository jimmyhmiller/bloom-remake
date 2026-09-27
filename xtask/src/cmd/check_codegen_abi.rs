//! `check-codegen-abi`: checking that generated code names only `blossom_engine::abi` and `blossom_wire::abi` (ARCH-04).
//!
//! Implemented by WP M8.5. Until then the task accepts any arguments and exits with code 7, naming the feature and the WP.

use std::ffi::OsString;
use std::process::ExitCode;

use crate::util;

/// Arguments of `check-codegen-abi`. Not parsed yet: WP M8.5 replaces them with the real options.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The task's arguments.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub args: Vec<OsString>,
}

/// Runs the task.
pub fn run(args: Args) -> ExitCode {
    let _ = args;
    util::not_implemented("ENG-005", "M8.5")
}
