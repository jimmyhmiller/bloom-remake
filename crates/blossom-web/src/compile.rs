//! The compiler in the page (feature `compiler`): a page on its own compiles its program from sources in memory, and
//! its editor re-runs edited sources. A client member's page has no compiler: it runs what its server projected
//! ([`crate::load_client`]).

use std::collections::BTreeMap;
use std::sync::Arc;

use blossom_base::SourceDb;
use blossom_front::api::{BlsError, NodeSpec};
use blossom_front::ded::LoadedFile;
use blossom_front::modules::Loader;

use crate::{Compiled, Diag, host_diag, interface};

/// Sources in memory: `path` → text.
struct Files<'a>(&'a BTreeMap<String, String>);

impl Loader for Files<'_> {
    fn load(&mut self, _from: Option<&str>, path: &str) -> Result<LoadedFile, String> {
        let path = path.trim_start_matches("./");
        self.0
            .get(path)
            .map(|text| LoadedFile {
                key: Arc::from(path),
                text: text.clone(),
            })
            .ok_or_else(|| format!("no file `{path}`"))
    }
}

fn diags(found: &blossom_base::Diagnostics, sources: &SourceDb) -> Vec<Diag> {
    found
        .iter()
        .map(|d| {
            let at = d.primary.and_then(|s| {
                let lc = sources.line_col(s.file, s.lo).ok()?;
                let file = sources.path(s.file).ok()?.to_string();
                // An editor's offsets (JavaScript's: UTF-16 code units).
                let text = sources.text(s.file).ok()?;
                let utf16 = |byte: u32| u32::try_from(text.get(..byte as usize)?.encode_utf16().count()).ok();
                Some((file, lc.line, lc.column, utf16(s.lo)?, utf16(s.hi)?))
            });
            Diag {
                severity: format!("{:?}", d.severity).to_lowercase(),
                code: d.code.as_str().to_owned(),
                message: d.message.clone(),
                rendered: blossom_driver::render::render(d, sources),
                file: at.as_ref().map(|a| a.0.clone()),
                line: at.as_ref().map(|a| a.1),
                column: at.as_ref().map(|a| a.2),
                range: at.as_ref().map(|a| (a.3, a.4)),
            }
        })
        .collect()
}

/// Compiles the program `root` of `files` (`path` → source) for the browser: one node, no roles. Its diagnostics
/// on failure.
pub fn compile(root: &str, files: &BTreeMap<String, String>) -> Result<Compiled, Vec<Diag>> {
    let nodes = [NodeSpec {
        name: "app".to_owned(),
        role: None,
    }];
    let (result, sources) = blossom_driver::bls::compile_with_loader(root, &nodes, &BTreeMap::new(), &mut Files(files));
    let (artifact, warnings) = match result {
        Ok(ok) => ok,
        Err(BlsError::Rejected(found)) => return Err(diags(&found, &sources)),
        Err(e) => return Err(vec![host_diag(e.to_string())]),
    };
    interface(artifact, diags(&warnings, &sources))
}
