//! `blossom check`: compile a file's program and specs and report their diagnostics (LANGUAGE §20).
//!
//! `blossom check FILE` compiles the program rooted at `FILE` through the frontend and the analyses, and every spec in
//! `FILE` that has a target (`spec S for M`, LANGUAGE §17) with its target, and prints every diagnostic once. A file of
//! specs alone (no `program` header) is checked by its specs; `--spec S` checks only the specs named. With no
//! `--nodes` the program is checked on a deployment of its own: one node for a role-free program, else one node per
//! role that holds nodes; `--nodes a,b=Role` checks it on a given one. Exit codes: 0 when everything compiles (with
//! `--strict`, also without warnings), 1 when something does not, 7 when it uses a feature this build does not
//! implement.

use std::collections::BTreeSet;
use std::process::ExitCode;

use blossom_base::{Diagnostics, SourceDb};
use blossom_driver::render::{is_not_implemented, render};
use blossom_front::api::{BlsError, NodeSpec};

use crate::common::Context;
use crate::exit::Exit;

/// Arguments of `blossom check`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The root file (`.bls`): a program, specs, or both.
    pub file: String,
    /// The deployment's nodes (`name`, or `name=Role`), comma-separated or repeated (default: one per role).
    #[arg(long, value_delimiter = ',')]
    pub nodes: Vec<String>,
    /// Check only these specs of the file (and not its program), comma-separated or repeated.
    #[arg(long, value_delimiter = ',', conflicts_with = "nodes")]
    pub spec: Vec<String>,
    /// Treat warnings as errors.
    #[arg(long)]
    pub strict: bool,
}

/// What the checks found, across the program and the specs.
#[derive(Default)]
struct Outcome {
    /// Each diagnostic as rendered, once (a spec's target repeats its program's warnings).
    printed: BTreeSet<String>,
    errors: usize,
    warnings: usize,
    unimplemented: bool,
    internal: bool,
}

impl Outcome {
    fn report(&mut self, diags: &Diagnostics, sources: &SourceDb) {
        for d in diags.iter() {
            let text = render(d, sources);
            if self.printed.insert(text.clone()) {
                eprint!("{text}");
                if d.is_error() {
                    self.errors += 1;
                } else {
                    self.warnings += 1;
                }
            }
        }
    }

    fn add<T>(&mut self, result: Result<(T, Diagnostics), BlsError>, sources: &SourceDb) {
        match result {
            Ok((_, warnings)) => self.report(&warnings, sources),
            Err(BlsError::Rejected(diags)) => {
                self.unimplemented |= diags.iter().any(is_not_implemented);
                self.report(&diags, sources);
            }
            Err(BlsError::Internal(e)) => {
                eprintln!("{e}");
                self.internal = true;
            }
        }
    }
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = cx;
    let mut out = Outcome::default();
    let (program, specs) = if args.spec.is_empty() {
        let (contents, sources) = blossom_driver::bls::file_contents(&args.file);
        match contents {
            // A file with neither a program nor a spec is compiled as a program, which says it is a library.
            Ok((c, _)) => (c.program || c.specs.is_empty(), c.specs),
            Err(e) => {
                out.add::<()>(Err(e), &sources);
                (false, Vec::new())
            }
        }
    } else {
        (false, args.spec.clone())
    };
    if program {
        let (result, sources) = if args.nodes.is_empty() {
            blossom_driver::bls::check_file(&args.file)
        } else {
            blossom_driver::bls::compile_file(&args.file, &nodes(&args.nodes))
        };
        out.add(result, &sources);
    }
    for name in &specs {
        let (result, sources) = blossom_driver::bls::compile_spec_file(&args.file, name);
        out.add(result, &sources);
    }
    if out.errors > 0 {
        eprintln!("{}: {} error(s)", args.file, out.errors);
    }
    if out.internal {
        Exit::Internal.into()
    } else if out.unimplemented {
        Exit::Unimplemented.into()
    } else if out.errors > 0 || (args.strict && out.warnings > 0) {
        Exit::UserError.into()
    } else {
        Exit::Ok.into()
    }
}

/// `--nodes`: `name` or `name=Role` each.
fn nodes(given: &[String]) -> Vec<NodeSpec> {
    given
        .iter()
        .map(|n| match n.split_once('=') {
            Some((name, role)) => NodeSpec {
                name: name.to_owned(),
                role: Some(role.to_owned()),
            },
            None => NodeSpec {
                name: n.clone(),
                role: None,
            },
        })
        .collect()
}
