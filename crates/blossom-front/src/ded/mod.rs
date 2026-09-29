//! The Molly `.ded` frontend (LANG-220, LANGUAGE §21.1, ARCHITECTURE §8.1 and §13.12).
//!
//! [`compile`] turns a set of `.ded` root files and a deployment's node names into a [`DedArtifact`]:
//!
//! 1. [`load`](load) parses the roots and everything they `include` (relative to the including file; a file
//!    included twice is loaded once, as Molly concatenates files);
//! 2. [`model`](model) collects Molly's relations, checks the static rules of the dialect, and splits the program
//!    into the protocol and the outcome spec (`pre`, `post` and the relations that only feed them);
//! 3. [`types`](types) infers every column's type: Molly's INT, STRING and LOCATION become `i64`, `String` and
//!    `Node`, and `count<X>` yields `u64` as the IR's `count` does;
//! 4. [`lower`](lower) builds the per-node protocol program, the input events and the spec program.
//!
//! Loading goes through the [`DedLoader`] trait, so the frontend does no I/O of its own.

// FEATURE: LANG-220

mod load;
mod lower;
mod model;
mod types;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use blossom_artifact::ded::DedArtifact;
use blossom_base::{Diagnostic, Diagnostics, InternalError, SourceDb, code};

/// A source file handed to the frontend by a [`DedLoader`].
#[derive(Clone, Debug)]
pub struct LoadedFile {
    /// A stable key for the file (a canonical path): a file is loaded once per key, and `include`s inside it are
    /// resolved relative to it.
    pub key: Arc<str>,
    pub text: String,
}

/// Where `.ded` sources come from.
pub trait DedLoader {
    /// Loads `path`. `from` is the key of the including file, or `None` for a root, which is resolved as given.
    fn load(&mut self, from: Option<&str>, path: &str) -> Result<LoadedFile, String>;
}

/// Why a `.ded` program was not compiled.
#[derive(Debug, thiserror::Error)]
pub enum DedError {
    /// The program is rejected; the diagnostics say why.
    #[error("the program has {} error(s)", .0.error_count())]
    Rejected(Diagnostics),
    /// A frontend bug: lowering produced a program the IR validator rejects, or an id table overflowed.
    #[error(transparent)]
    Internal(#[from] InternalError),
}

/// Compiles the `.ded` files `roots` for a deployment of `nodes` (LANGUAGE §21.1). With no nodes given, the
/// deployment is the program's own locations: every string constant the typer infers as a `Node` (the first column
/// of every fact, and any column unified with a location; tests/corpus/README.md). Source files are added to
/// `sources`, which diagnostics point into.
pub fn compile(
    roots: &[&str],
    nodes: &[&str],
    loader: &mut dyn DedLoader,
    sources: &mut SourceDb,
) -> Result<DedArtifact, DedError> {
    let mut diags = Diagnostics::new();
    let program = load::load(roots, loader, sources, &mut diags);
    if diags.has_errors() {
        return Err(DedError::Rejected(diags));
    }
    let model = model::Model::build(&program, &mut diags);
    if diags.has_errors() {
        return Err(DedError::Rejected(diags));
    }
    let types = types::infer(&program, &model, &mut diags);
    if diags.has_errors() {
        return Err(DedError::Rejected(diags));
    }
    let inferred;
    let nodes: Vec<&str> = if nodes.is_empty() {
        inferred = inferred_nodes(&program, &types);
        inferred.iter().map(String::as_str).collect()
    } else {
        nodes.to_vec()
    };
    let deployment = match lower::Deployment::new(&nodes) {
        Ok(d) => d,
        Err(message) => {
            diags.push(Diagnostic::new(code!("BLS0200"), message));
            return Err(DedError::Rejected(diags));
        }
    };
    let artifact = lower::lower(&program, &model, &types, &deployment, &mut diags)?;
    if diags.has_errors() {
        return Err(DedError::Rejected(diags));
    }
    Ok(artifact)
}

/// The string constants the typer inferred as nodes, sorted and deduplicated.
fn inferred_nodes(program: &load::Program, types: &types::Types) -> Vec<String> {
    use blossom_syntax::ded::{Arg, BodyItem, Expr, Term};
    let mut out = std::collections::BTreeSet::new();
    let mut term = |t: &Term| {
        if let Term::Str(text, span) = t
            && types.at.get(span) == Some(&types::ColTy::Node)
        {
            out.insert(text.to_string());
        }
    };
    fn expr(e: &Expr, term: &mut dyn FnMut(&Term)) {
        match e {
            Expr::Term(t) => term(t),
            Expr::Binary { lhs, rhs, .. } => {
                term(lhs);
                expr(rhs, term);
            }
        }
    }
    for f in &program.facts {
        for t in &f.args {
            term(t);
        }
    }
    for r in &program.rules {
        for a in &r.head.args {
            if let Arg::Expr(e) = a {
                expr(e, &mut term);
            }
        }
        for b in &r.body {
            match b {
                BodyItem::Atom(a) => {
                    for x in &a.args {
                        if let Arg::Expr(e) = x {
                            expr(e, &mut term);
                        }
                    }
                }
                BodyItem::Qual(e) => expr(e, &mut term),
            }
        }
    }
    out.into_iter().collect()
}
