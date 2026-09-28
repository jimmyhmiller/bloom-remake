//! Compiling Molly `.ded` files from disk (LANG-220).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use blossom_artifact::ded::DedArtifact;
use blossom_base::SourceDb;
use blossom_front::ded::{self, DedError, DedLoader, LoadedFile};

/// Loads `.ded` files from the file system: roots as given, `include`s relative to the including file, each file
/// keyed by its canonical path.
#[derive(Debug, Default)]
pub struct FsLoader;

impl DedLoader for FsLoader {
    fn load(&mut self, from: Option<&str>, path: &str) -> Result<LoadedFile, String> {
        let resolved = match from {
            Some(including) => Path::new(including)
                .parent()
                .map_or_else(|| PathBuf::from(path), |dir| dir.join(path)),
            None => PathBuf::from(path),
        };
        let key = resolved
            .canonicalize()
            .map_err(|e| format!("{}: {e}", resolved.display()))?;
        let text = std::fs::read_to_string(&key).map_err(|e| format!("{}: {e}", key.display()))?;
        Ok(LoadedFile {
            key: Arc::from(key.to_string_lossy().as_ref()),
            text,
        })
    }
}

/// Compiles the `.ded` files `roots` from disk for a deployment of `nodes`. The source database is returned either
/// way, so diagnostics can be rendered.
pub fn compile_files(roots: &[&str], nodes: &[&str]) -> (Result<DedArtifact, DedError>, SourceDb) {
    let mut sources = SourceDb::new();
    let result = ded::compile(roots, nodes, &mut FsLoader, &mut sources);
    (result, sources)
}
