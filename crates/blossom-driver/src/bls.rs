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
pub fn compile_file(root: &str, nodes: &[NodeSpec]) -> (Result<(BlsArtifact, Diagnostics), BlsError>, SourceDb) {
    let mut sources = SourceDb::new();
    let result = api::compile(root, nodes, &mut FsLoader, &mut sources);
    (result, sources)
}
