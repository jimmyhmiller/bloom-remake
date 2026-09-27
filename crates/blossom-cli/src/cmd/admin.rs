//! `blossom admin`: admin-plane operations.
//!
//! Implemented by WP M11.5 (DIST-066). Until then the command accepts any arguments and exits with code 7
//! (ARCHITECTURE §12.5), naming the feature and the WP.

use std::ffi::OsString;
use std::process::ExitCode;

use crate::common::Context;

/// Arguments of `blossom admin`. Not parsed yet: WP M11.5 replaces them with the real options.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The command's arguments.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub args: Vec<OsString>,
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = (args, cx);
    crate::exit::not_implemented("DIST-066", "M11.5")
}
