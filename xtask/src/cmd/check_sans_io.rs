//! `check-sans-io`: the node crate performs no I/O of its own (ARCH-03, ARCHITECTURE §1.3). Implemented by WP M1.1.
//!
//! Fails on any use of `std::fs`, `std::net`, `std::thread`, `std::time::Instant` or `tokio` in
//! `crates/blossom-node/src/**` — and of `std::os::unix::net`, the Unix-domain sockets that belong with `std::net` —
//! found syntactically: `use` trees at any depth (aliases expanded), paths in code, and paths inside macro
//! invocations. Test code is checked too. Comments do not count. The node reaches the world only through its
//! `Vfs`, `Transport`, `Clock` and `Entropy` traits.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::rustsrc;
use crate::util;

/// Arguments of `check-sans-io`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The repository root (default: the root this xtask was built in).
    #[arg(long)]
    pub root: Option<PathBuf>,
}

/// The forbidden paths: a use of a path with one of these prefixes is a violation.
const FORBIDDEN: &[&[&str]] = &[
    &["std", "fs"],
    &["std", "net"],
    &["std", "thread"],
    &["std", "time", "Instant"],
    &["std", "os", "unix", "net"],
    &["tokio"],
];

/// Runs the task.
pub fn run(args: Args) -> ExitCode {
    let root = util::root_or_default(args.root);
    let src = root.join("crates").join("blossom-node").join("src");
    match check_dir(&root, &src) {
        Ok((findings, files)) => util::finish(
            "check-sans-io",
            &findings,
            &format!("{files} files in {}", util::display_relative(&src, &root)),
        ),
        Err(e) => util::fail("check-sans-io", e),
    }
}

fn check_dir(root: &Path, src: &Path) -> Result<(Vec<String>, usize), String> {
    if !src.is_dir() {
        return Err(format!("{} does not exist", src.display()));
    }
    let files = util::rust_files(src).map_err(|e| format!("cannot list {}: {e}", src.display()))?;
    let mut findings = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let shown = util::display_relative(file, root);
        findings.extend(
            check_source(&text)
                .map_err(|e| format!("{shown}: cannot parse: {e}"))?
                .into_iter()
                .map(|f| format!("{shown}:{f}")),
        );
    }
    Ok((findings, files.len()))
}

/// The violations in one file, as `line: message`.
pub fn check_source(text: &str) -> syn::Result<Vec<String>> {
    let scanned = rustsrc::scan(text)?;
    let mut out = Vec::new();
    for path in &scanned.paths {
        let segments: Vec<&str> = path.segments.iter().map(String::as_str).collect();
        let hit = FORBIDDEN.iter().find(|f| {
            segments.starts_with(f)
                // `use std::*` / `use std::time::*` would import a forbidden item.
                || (path.glob && f.starts_with(&segments))
        });
        if let Some(f) = hit {
            let shown = if path.glob {
                format!("{}::*", segments.join("::"))
            } else {
                segments.join("::")
            };
            out.push(format!(
                "{}: `{shown}` uses {} in the sans-IO node (ARCH-03)",
                path.line,
                f.join("::")
            ));
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_sans_io_flags_io() {
        let src = "use std::fs;\nuse std::time::{Duration, Instant as Now};\nuse tokio::net::TcpStream;\n\
                   fn f() { let _ = fs::read(\"x\"); let _ = Now::now(); std::thread::spawn(|| ()); }\n\
                   fn g() { let _ = format!(\"{:?}\", std::net::Ipv4Addr::LOCALHOST); }\nuse std::time::*;";
        let found = check_source(src).unwrap();
        let joined = found.join("\n");
        for needle in [
            "`std::fs`",
            "`std::time::Instant`",
            "`tokio::net::TcpStream`",
            "`std::fs::read`",
            "`std::thread::spawn`",
            "`std::net::Ipv4Addr::LOCALHOST`",
            "`std::time::*`",
        ] {
            assert!(joined.contains(needle), "{needle} not in:\n{joined}");
        }
    }

    #[test]
    fn check_sans_io_flags_nested_uses_and_unix_sockets() {
        let src = "mod io { use std::fs::File as F; pub fn open() { let _ = F::open(\"x\"); } }\n\
                   fn tick() { use std::thread as t; t::yield_now(); }\n\
                   #[cfg(test)] mod tests { use std::os::unix::net::UnixStream; }";
        let found = check_source(src).unwrap();
        let joined = found.join("\n");
        for needle in [
            "1: `std::fs::File`",
            "1: `std::fs::File::open`",
            "2: `std::thread`",
            "2: `std::thread::yield_now`",
            "3: `std::os::unix::net::UnixStream`",
        ] {
            assert!(joined.contains(needle), "{needle} not in:\n{joined}");
        }
    }

    #[test]
    fn check_sans_io_allows_pure_code() {
        let src = "//! Uses std::fs in a comment only.\nuse std::time::Duration;\nuse std::collections::BTreeMap;\n\
                   /// std::net in a doc comment.\nfn f(d: Duration) -> BTreeMap<u8, Duration> { let _ = \"std::fs\"; BTreeMap::from([(1, d)]) }";
        assert_eq!(check_source(src).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn check_sans_io_real_node_crate() {
        let root = util::workspace_root();
        let (findings, files) = check_dir(&root, &root.join("crates/blossom-node/src")).unwrap();
        assert!(files >= 1);
        assert_eq!(findings, Vec::<String>::new());
    }
}
