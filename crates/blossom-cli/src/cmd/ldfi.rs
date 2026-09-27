//! `blossom ldfi`: lineage-driven fault injection (Molly-2).
//!
//! Implemented by WP M8.1 (TEST-029). Until then the command accepts any arguments and exits with code 7
//! (ARCHITECTURE §12.5), naming the feature and the WP.

use std::ffi::OsString;
use std::process::ExitCode;

use crate::common::Context;

/// Arguments of `blossom ldfi`. Not parsed yet: WP M8.1 replaces them with the real options.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The command's arguments.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub args: Vec<OsString>,
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = (args, cx);
    crate::exit::not_implemented("TEST-029", "M8.1")
}
