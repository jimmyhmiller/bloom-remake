//! `bench-report`: benchmark reports and gates (ARCHITECTURE §4.14).
//!
//! Implemented by WP M11.4. Until then the task accepts any arguments and exits with code 7, naming the feature and the WP.

use std::ffi::OsString;
use std::process::ExitCode;

use crate::util;

/// Arguments of `bench-report`. Not parsed yet: WP M11.4 replaces them with the real options.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The task's arguments.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub args: Vec<OsString>,
}

/// Runs the task.
pub fn run(args: Args) -> ExitCode {
    let _ = args;
    util::not_implemented("BENCH-200", "M11.4")
}
