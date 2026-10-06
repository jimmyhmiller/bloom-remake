//! `blossom completions`: generate shell completions.
//!
//! `blossom completions SHELL` writes the completion script for `SHELL` (bash, zsh, fish, elvish or powershell) to
//! standard output, generated from the command tree itself, so it completes every subcommand and option this build
//! has. Install it where the shell looks, for example `blossom completions zsh > ~/.zfunc/_blossom` or
//! `blossom completions bash > /etc/bash_completion.d/blossom`.

use std::io::Write;
use std::process::ExitCode;

use clap::CommandFactory;
use clap_complete::Shell;

use crate::common::Context;
use crate::exit::Exit;

/// Arguments of `blossom completions`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The shell to write the completion script for.
    pub shell: Shell,
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = cx;
    let mut command = crate::Cli::command();
    let mut script = Vec::new();
    clap_complete::generate(args.shell, &mut command, "blossom", &mut script);
    let mut stdout = std::io::stdout().lock();
    match stdout.write_all(&script).and_then(|()| stdout.flush()) {
        Ok(()) => Exit::Ok.into(),
        Err(e) => {
            eprintln!("standard output: {e}");
            Exit::UserError.into()
        }
    }
}
