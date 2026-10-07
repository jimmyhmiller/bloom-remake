//! `blossom build`: compile a program to artifacts.
//!
//! `blossom build --deploy D --role R --out F` writes the part of the deployment's program that the client role `R`'s
//! pages run (docs/design/CLIENTS.md §8): the program projected onto `R`, checked to hold nothing placed at another
//! role, encoded as `blossom run --web` serves it. The other forms are WP M6.3's (LANG-002) and exit with code 7
//! (ARCHITECTURE §12.5), naming the feature and the WP.

use std::path::PathBuf;
use std::process::ExitCode;

use blossom_artifact::client::ClientArtifact;

use crate::common::Context;
use crate::exit::Exit;

/// Arguments of `blossom build`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The deployment spec (`deploy.toml`) whose program to build.
    #[arg(long = "deploy", value_name = "FILE")]
    pub deploy: Option<PathBuf>,
    /// Build the part of the program the client role ROLE's pages run.
    #[arg(long, value_name = "ROLE")]
    pub role: Option<String>,
    /// Where to write the artifact.
    #[arg(long, short = 'o', value_name = "FILE")]
    pub out: Option<PathBuf>,
    /// A program's root file (the forms of WP M6.3, not implemented yet).
    pub file: Option<PathBuf>,
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = cx;
    let (Some(deploy), Some(role), Some(out), None) = (&args.deploy, &args.role, &args.out, &args.file) else {
        if args.role.is_some() || args.deploy.is_some() {
            eprintln!("blossom build: a client role's artifact needs --deploy FILE, --role ROLE and --out FILE");
            return Exit::Usage.into();
        }
        return crate::exit::not_implemented("LANG-002", "M6.3");
    };
    let (_, artifact) = match crate::cmd::run::load(deploy) {
        Ok(x) => x,
        Err(code) => return code,
    };
    let client = match ClientArtifact::project(&artifact, role) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("blossom build: {e}");
            return Exit::Refused.into();
        }
    };
    let leaks = client.leaks(&artifact);
    if !leaks.is_empty() {
        eprintln!(
            "blossom build: the part of the program `{role}`'s pages run would show them: {}",
            leaks.join("; ")
        );
        return Exit::Internal.into();
    }
    let bytes = match client.encode() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("blossom build: {e}");
            return Exit::Internal.into();
        }
    };
    if let Err(e) = std::fs::write(out, &bytes) {
        eprintln!("blossom build: writing {}: {e}", out.display());
        return Exit::UserError.into();
    }
    let p = client.program.get();
    println!(
        "{}: the part of the program `{role}` runs: {} relations, {} rules, {} bytes",
        out.display(),
        p.rels.len(),
        p.rules.len(),
        bytes.len()
    );
    ExitCode::SUCCESS
}
