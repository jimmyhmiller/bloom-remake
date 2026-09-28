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

/// Compiles the `.ded` files `roots` for a deployment of `nodes` (LANGUAGE §21.1). Source files are added to
/// `sources`, which diagnostics point into.
pub fn compile(
    roots: &[&str],
    nodes: &[&str],
    loader: &mut dyn DedLoader,
    sources: &mut SourceDb,
) -> Result<DedArtifact, DedError> {
    let mut diags = Diagnostics::new();
    let deployment = match lower::Deployment::new(nodes) {
        Ok(d) => d,
        Err(message) => {
            diags.push(Diagnostic::new(code!("BLS0200"), message));
            return Err(DedError::Rejected(diags));
        }
    };
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
    let artifact = lower::lower(&program, &model, &types, &deployment, &mut diags)?;
    if diags.has_errors() {
        return Err(DedError::Rejected(diags));
    }
    Ok(artifact)
}
