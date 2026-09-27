//! `blossom self-check`: run the embedded program on the oracle against the executor.
//!
//! Implemented by WP M12.4 (ENG-067). Until then the command accepts any arguments and exits with code 7
//! (ARCHITECTURE §12.5), naming the feature and the WP.

use std::ffi::OsString;
use std::process::ExitCode;

use crate::common::Context;

/// Arguments of `blossom self-check`. Not parsed yet: WP M12.4 replaces them with the real options.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The command's arguments.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub args: Vec<OsString>,
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = (args, cx);
    crate::exit::not_implemented("ENG-067", "M12.4")
}
