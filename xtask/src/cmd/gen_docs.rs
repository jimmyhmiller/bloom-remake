//! `gen-docs`: generating the standard-library reference and the guide.
//!
//! Implemented by WP M12.5. Until then the task accepts any arguments and exits with code 7, naming what is missing and the WP.

use std::ffi::OsString;
use std::process::ExitCode;

use crate::util;

/// Arguments of `gen-docs`. Not parsed yet: WP M12.5 replaces them with the real options.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The task's arguments.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub args: Vec<OsString>,
}

/// Runs the task.
pub fn run(args: Args) -> ExitCode {
    let _ = args;
    util::not_implemented("the documentation generator", "M12.5")
}
