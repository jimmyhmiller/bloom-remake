//! Compiling Blossom `.bls` programs from disk (LANGUAGE §6.1).

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{Diagnostics, SourceDb};
use blossom_front::api::{self, BlsError, NodeSpec};
use blossom_front::ded::{DedLoader, LoadedFile};
use blossom_front::modules::Loader;

use crate::ded::FsLoader;

impl Loader for FsLoader {
    fn load(&mut self, from: Option<&str>, path: &str) -> Result<LoadedFile, String> {
        DedLoader::load(self, from, path)
    }
}

/// Compiles the program rooted at `root` from disk for a deployment of `nodes`. The source database is returned
/// either way, so diagnostics can be rendered.
///
/// After the frontend, the program's deductive rules must stratify (BLS0502, `blossom-analysis`).
pub fn compile_file(root: &str, nodes: &[NodeSpec]) -> (Result<(BlsArtifact, Diagnostics), BlsError>, SourceDb) {
    let mut sources = SourceDb::new();
    let result = api::compile(root, nodes, &mut FsLoader, &mut sources).and_then(|(artifact, mut diags)| {
        let strata = blossom_analysis::strata::check(artifact.program.get())?;
        let rejected = strata.has_errors();
        for d in strata.iter() {
            diags.push(d.clone());
        }
        if rejected {
            Err(BlsError::Rejected(diags))
        } else {
            Ok((artifact, diags))
        }
    });
    (result, sources)
}

/// Compiles the spec `name` of the file `root` with its target (LANGUAGE §17). After the frontend, the target's and
/// the spec's deductive rules must stratify (BLS0502).
pub fn compile_spec_file(
    root: &str,
    name: &str,
) -> (
    Result<(blossom_front::spec::CompiledSpec, Diagnostics), BlsError>,
    SourceDb,
) {
    let mut sources = SourceDb::new();
    let result =
        blossom_front::spec::compile_spec(root, name, &mut FsLoader, &mut sources).and_then(|(spec, mut diags)| {
            let mut rejected = false;
            let programs =
                std::iter::once(&spec.artifact.protocol).chain(spec.artifact.spec.as_ref().map(|s| &s.program));
            for p in programs {
                let strata = blossom_analysis::strata::check(p.get())?;
                rejected |= strata.has_errors();
                for d in strata.iter() {
                    diags.push(d.clone());
                }
            }
            if rejected {
                Err(BlsError::Rejected(diags))
            } else {
                Ok((spec, diags))
            }
        });
    (result, sources)
}
