#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-std-src`: the standard library's `.bls` sources, embedded at build time (ARCHITECTURE §1.2, §1.5).
//!
//! `build.rs` walks `std/**/*.bls` and embeds every module; `std/a/b.bls` and `std/a/b/mod.bls` are module
//! `std::a::b`. Loading is lazy and per module (PLAN §4 D10): the frontend asks for the modules a program imports,
//! so a broken `std/foo.bls` never affects a program that does not import `std::foo`. A file that cannot even be
//! embedded (not UTF-8, not a module name, or defined twice) is reported by [`lookup`] as
//! [`StdModule::Rejected`] with its reason, and nothing else is affected.
//!
//! Implemented by WP M1.1.

pub mod scan;

include!(concat!(env!("OUT_DIR"), "/std_sources.rs"));

/// The result of looking up a standard-library module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdModule {
    /// The module's source text.
    Source(&'static str),
    /// No such module.
    Missing,
    /// A file for the module exists but could not be embedded; the reason says why.
    Rejected(&'static str),
}

/// The source of module `path` (e.g. `std::bcast::reliable`). `None` both for a missing module and for one that
/// could not be embedded; use [`lookup`] to tell them apart.
pub fn source(path: &str) -> Option<&'static str> {
    match lookup(path) {
        StdModule::Source(text) => Some(text),
        StdModule::Missing | StdModule::Rejected(_) => None,
    }
}

/// Looks up module `path`.
pub fn lookup(path: &str) -> StdModule {
    lookup_in(STD_SOURCES, STD_REJECTED, path)
}

/// Every embedded module path, sorted.
pub fn modules() -> impl Iterator<Item = &'static str> {
    STD_SOURCES.iter().map(|(path, _)| *path)
}

/// Every module that could not be embedded, as `(module path, reason)`, sorted.
pub fn rejected() -> &'static [(&'static str, &'static str)] {
    STD_REJECTED
}

fn lookup_in(
    sources: &'static [(&'static str, &'static str)],
    rejected: &'static [(&'static str, &'static str)],
    path: &str,
) -> StdModule {
    if let Ok(i) = sources.binary_search_by(|(p, _)| (*p).cmp(path))
        && let Some((_, text)) = sources.get(i)
    {
        return StdModule::Source(text);
    }
    if let Ok(i) = rejected.binary_search_by(|(p, _)| (*p).cmp(path))
        && let Some((_, reason)) = rejected.get(i)
    {
        return StdModule::Rejected(reason);
    }
    StdModule::Missing
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    /// A scratch copy of a `std/` tree, removed on drop.
    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str, files: &[(&str, &[u8])]) -> Fixture {
            let root = std::env::temp_dir().join(format!("blossom-std-src-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            for (path, bytes) in files {
                let file = root.join(path);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(file, bytes).unwrap();
            }
            Fixture(root)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn leak_table(entries: Vec<(String, String)>) -> &'static [(&'static str, &'static str)] {
        let table: Vec<(&'static str, &'static str)> = entries
            .into_iter()
            .map(|(p, s)| (&*Box::leak(p.into_boxed_str()), &*Box::leak(s.into_boxed_str())))
            .collect();
        Box::leak(table.into_boxed_slice())
    }

    #[test]
    fn std_sources_lazy_lookup() {
        let fx = Fixture::new(
            "lazy",
            &[
                ("README.md", b"not a module"),
                ("fd.bls", b"// std::fd"),
                ("bcast/reliable.bls", b"// std::bcast::reliable"),
                ("consensus/mod.bls", b"// std::consensus"),
                ("consensus/raft.bls", b"// std::consensus::raft"),
                ("broken.bls", &[b'/', b'/', 0xff, 0xfe]),
                ("dup.bls", b"// one"),
                ("dup/mod.bls", b"// two"),
                ("bad-name.bls", b"// not an identifier"),
                (".hidden/x.bls", b"// ignored"),
                ("mod.bls", b"// std itself"),
            ],
        );
        let result = scan::scan(fx.path()).unwrap();
        let paths: Vec<&str> = result.modules.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "std",
                "std::bcast::reliable",
                "std::consensus",
                "std::consensus::raft",
                "std::fd"
            ]
        );
        let rejected: Vec<&str> = result.rejected.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(rejected, vec!["std::bad-name", "std::broken", "std::dup"]);
        assert!(result.rejected.iter().any(|r| r.reason.contains("not valid UTF-8")));
        assert!(result.rejected.iter().any(|r| r.reason.contains("more than one file")));
        assert!(result.watched.iter().any(|p| p.ends_with("bcast/reliable.bls")));

        // Build the same tables build.rs generates and look modules up: every good module resolves although other
        // files are broken, and each broken one is reported only for itself.
        let sources = leak_table(
            result
                .modules
                .iter()
                .map(|m| (m.path.clone(), std::fs::read_to_string(&m.file).unwrap()))
                .collect(),
        );
        let rejected = leak_table(
            result
                .rejected
                .iter()
                .map(|r| (r.path.clone(), r.reason.clone()))
                .collect(),
        );
        assert_eq!(
            lookup_in(sources, rejected, "std::consensus::raft"),
            StdModule::Source("// std::consensus::raft")
        );
        assert_eq!(lookup_in(sources, rejected, "std::fd"), StdModule::Source("// std::fd"));
        assert!(matches!(lookup_in(sources, rejected, "std::broken"), StdModule::Rejected(r) if r.contains("UTF-8")));
        assert_eq!(lookup_in(sources, rejected, "std::nope"), StdModule::Missing);
        assert_eq!(
            lookup_in(sources, rejected, "std::consensus::paxos"),
            StdModule::Missing
        );
    }

    #[test]
    fn std_sources_lazy_lookup_empty_std() {
        let fx = Fixture::new("empty", &[("README.md", b"only docs")]);
        let result = scan::scan(fx.path()).unwrap();
        assert!(result.modules.is_empty() && result.rejected.is_empty());
        let missing = std::env::temp_dir().join(format!("blossom-std-src-missing-{}", std::process::id()));
        assert_eq!(scan::scan(&missing).unwrap(), scan::Scan::default());
    }

    #[test]
    fn std_sources_embedded_tables_are_sorted() {
        let paths: Vec<&str> = modules().collect();
        assert!(paths.windows(2).all(|w| w[0] < w[1]), "{paths:?}");
        for path in paths {
            assert!(source(path).is_some(), "{path}");
        }
        assert!(rejected().windows(2).all(|w| w[0].0 < w[1].0));
        for (path, reason) in rejected() {
            assert_eq!(lookup(path), StdModule::Rejected(reason));
        }
        assert_eq!(lookup("std::__no_such_module__"), StdModule::Missing);
        assert_eq!(source("std::__no_such_module__"), None);
    }

    #[test]
    fn std_sources_module_segments() {
        assert!(scan::is_module_segment("bcast") && scan::is_module_segment("_x1"));
        assert!(!scan::is_module_segment("1x") && !scan::is_module_segment("a-b") && !scan::is_module_segment(""));
    }
}
