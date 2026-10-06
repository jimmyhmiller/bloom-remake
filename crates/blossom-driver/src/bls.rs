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
/// After the frontend, the program's deductive rules must stratify (BLS0502), its `monotone` regions must have no
/// point of order (BLS0702) and its explicit ACLs must admit its own senders (BLS0800), all checked by
/// `blossom-analysis`.
pub fn compile_file(root: &str, nodes: &[NodeSpec]) -> (Result<(BlsArtifact, Diagnostics), BlsError>, SourceDb) {
    compile_file_with(root, nodes, &std::collections::BTreeMap::new())
}

/// [`compile_file`] with the deployment's values of deploy-time parameters (LANG-010).
pub fn compile_file_with(
    root: &str,
    nodes: &[NodeSpec],
    params: &std::collections::BTreeMap<String, api::ParamBinding>,
) -> (Result<(BlsArtifact, Diagnostics), BlsError>, SourceDb) {
    compile_with_loader(root, nodes, params, &mut FsLoader)
}

/// [`compile_file_with`] from any [`Loader`]: sources in memory (an editor, the browser) as well as on disk.
pub fn compile_with_loader(
    root: &str,
    nodes: &[NodeSpec],
    params: &std::collections::BTreeMap<String, api::ParamBinding>,
    loader: &mut dyn Loader,
) -> (Result<(BlsArtifact, Diagnostics), BlsError>, SourceDb) {
    let mut sources = SourceDb::new();
    let result = api::compile_with(root, nodes, params, loader, &mut sources).and_then(with_analyses);
    (result, sources)
}

/// Checks the program rooted at `root` (`blossom check`): compiled with no deployment given (a role-free program on
/// one node, else one node per role, `api::compile_checking`), then the analyses.
pub fn check_file(root: &str) -> (Result<(BlsArtifact, Diagnostics), BlsError>, SourceDb) {
    let mut sources = SourceDb::new();
    let result = api::compile_checking(root, &std::collections::BTreeMap::new(), &mut FsLoader, &mut sources)
        .and_then(with_analyses);
    (result, sources)
}

/// What the file `root` holds for `blossom check`: whether it is a program, and its specs that have a target.
pub fn file_contents(
    root: &str,
) -> (
    Result<(blossom_front::spec::FileContents, Diagnostics), BlsError>,
    SourceDb,
) {
    let mut sources = SourceDb::new();
    let result = blossom_front::spec::file_contents(root, &mut FsLoader, &mut sources);
    (result, sources)
}

/// A compiled program with the analyses' findings added; rejected when one is an error.
fn with_analyses((artifact, mut diags): (BlsArtifact, Diagnostics)) -> Result<(BlsArtifact, Diagnostics), BlsError> {
    let found = analyses(artifact.program.get())?;
    let rejected = found.has_errors();
    for d in found.iter() {
        diags.push(d.clone());
    }
    if rejected {
        Err(BlsError::Rejected(diags))
    } else {
        Ok((artifact, diags))
    }
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
                let found = analyses(p.get())?;
                rejected |= found.has_errors();
                for d in found.iter() {
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

/// The analyses every compiled program passes: stratification (BLS0502), the determinism lints (BLS0601), `monotone`
/// assertions (BLS0702) and ACL consistency (BLS0800).
fn analyses(p: &blossom_ir::core::Program) -> Result<Diagnostics, blossom_base::InternalError> {
    let mut out = blossom_analysis::strata::check(p)?;
    for d in blossom_analysis::determinism::check(p).iter() {
        out.push(d.clone());
    }
    for d in blossom_analysis::monotone::check(p).iter() {
        out.push(d.clone());
    }
    for d in blossom_analysis::acl::check(p).iter() {
        out.push(d.clone());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use blossom_base::SourceDb;
    use blossom_front::api::{self, BlsError, NodeSpec};
    use blossom_front::ded::LoadedFile;
    use blossom_front::modules::Loader;

    struct One(String);

    impl Loader for One {
        fn load(&mut self, from: Option<&str>, path: &str) -> Result<LoadedFile, String> {
            match from {
                None => Ok(LoadedFile {
                    key: Arc::from(path),
                    text: self.0.clone(),
                }),
                Some(_) => Err(format!("no module `{path}` in this test")),
            }
        }
    }

    /// A column-form channel `pipe` into B that both A and C send on; `{ACCEPT}` is its attribute.
    const PIPE: &str = "program t version 1;
role A;
role B;
role C;
static peers(n: Node<B>);
{ACCEPT} channel pipe(@dst: Node<B>, k: u64);
at A { input go(k: u64); s: on go(k), peers(n) { send pipe(n, k); } }
at C { input go2(k: u64); t: on go2(k), peers(n) { send pipe(n, k); } }
at B { output got(k: u64); r: on pipe(_, k) { emit got(k); } }
";

    /// The codes of the analyses' diagnostics for `PIPE` with `accept`; the frontend must accept the program.
    fn analysis_codes(accept: &str) -> Vec<String> {
        let nodes: Vec<NodeSpec> = [("a", "A"), ("b", "B"), ("c", "C")]
            .into_iter()
            .map(|(n, r)| NodeSpec {
                name: n.to_owned(),
                role: Some(r.to_owned()),
            })
            .collect();
        let mut sources = SourceDb::new();
        let mut loader = One(PIPE.replace("{ACCEPT}", accept));
        let (artifact, _) = match api::compile("test.bls", &nodes, &mut loader, &mut sources) {
            Ok(ok) => ok,
            Err(BlsError::Rejected(d)) => panic!("{:?}", d.iter().map(|d| d.message.clone()).collect::<Vec<_>>()),
            Err(e) => panic!("{e}"),
        };
        super::analyses(artifact.program.get())
            .unwrap()
            .iter()
            .map(|d| d.code.as_str().to_owned())
            .collect()
    }

    #[test]
    fn an_acl_that_excludes_a_sender_is_bls0800() {
        assert_eq!(analysis_codes(""), Vec::<String>::new());
        assert_eq!(analysis_codes("#[accept(A, C)]"), Vec::<String>::new());
        assert_eq!(analysis_codes("#[accept(A)]"), vec!["BLS0800"]);
        assert_eq!(analysis_codes("#[accept(B)]"), vec!["BLS0800", "BLS0800"]);
    }
}
