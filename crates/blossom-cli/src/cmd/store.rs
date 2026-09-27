//! `blossom store`: inspect, verify, dump, back up and restore node stores.
//!
//! Implemented by WP M5.4 (DIST-021). Until then the command accepts any arguments and exits with code 7
//! (ARCHITECTURE §12.5), naming the feature and the WP.

use std::ffi::OsString;
use std::process::ExitCode;

use crate::common::Context;

/// Arguments of `blossom store`. Not parsed yet: WP M5.4 replaces them with the real options.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The command's arguments.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub args: Vec<OsString>,
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = (args, cx);
    crate::exit::not_implemented("DIST-021", "M5.4")
}
