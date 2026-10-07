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
    /// Print what the law harness found for each user-defined lattice: its laws and each method's class claim.
    #[arg(long)]
    pub laws: bool,
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
        let artifact = result.as_ref().ok().map(|(a, _)| a.clone());
        out.add(result, &sources);
        // The law harness on the program's lattices (BLS0704 for a refuted claim).
        if let Some(a) = artifact {
            match crate::common::bls::laws(&a) {
                Ok((refuted, report)) => {
                    out.report(&refuted, &sources);
                    if args.laws {
                        print!("{}", law_text(&report));
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    out.internal = true;
                }
            }
        }
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

/// The law harness's report, one lattice to a paragraph.
fn law_text(report: &blossom_verify::laws::Report) -> String {
    use blossom_verify::laws::{Outcome, claim_word};
    let mut out = String::new();
    if report.lattices.is_empty() {
        out.push_str("no user-defined lattices\n");
    }
    for l in &report.lattices {
        let status = match l.status {
            blossom_ir::core::LawStatus::Proved => "proven (a product of lattices)",
            blossom_ir::core::LawStatus::Tested => "tested",
            blossom_ir::core::LawStatus::Builtin => "built in",
            blossom_ir::core::LawStatus::Refuted => "refuted",
        };
        out.push_str(&format!(
            "lattice {}: merge, ⊥ and order {status}; checked on {} case(s)\n",
            l.name, l.merge.checked
        ));
        for c in &l.claims {
            let what = match &c.outcome {
                Outcome::Tested(t) => format!("tested on {} case(s), {} set aside", t.checked, t.skipped),
                Outcome::Refuted(r) => format!("refuted ({}): {}", r.law, r.detail),
                Outcome::Untested(why) => format!("not tested: {why}"),
            };
            out.push_str(&format!("  {}: {}, {what}\n", c.method, claim_word(c.claim)));
        }
    }
    out
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
