//! `crashcheck`: ALICE-style crash-consistency checks over SimFs (ARCHITECTURE §11.7).
//!
//! Implemented by WP M5.4. Until then the task accepts any arguments and exits with code 7, naming the feature and the WP.

use std::ffi::OsString;
use std::process::ExitCode;

use crate::util;

/// Arguments of `crashcheck`. Not parsed yet: WP M5.4 replaces them with the real options.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The task's arguments.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub args: Vec<OsString>,
}

/// Runs the task.
pub fn run(args: Args) -> ExitCode {
    let _ = args;
    util::not_implemented("DIST-021", "M5.4")
}
