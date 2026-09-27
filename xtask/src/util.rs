//! Helpers shared by the xtask tasks.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The repository root (the directory above `xtask/`).
pub fn workspace_root() -> PathBuf {
    let xtask_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    xtask_dir
        .parent()
        .map_or_else(|| xtask_dir.to_path_buf(), Path::to_path_buf)
}

/// The root to operate on: `--root` if given, otherwise the repository root.
pub fn root_or_default(root: Option<PathBuf>) -> PathBuf {
    root.unwrap_or_else(workspace_root)
}

/// Reports a task that is not implemented yet and returns exit code 7 (ARCHITECTURE §12.5). `what` is the
/// FEATURES id (or, where FEATURES has none, a description); `wp` the work package that implements it.
pub fn not_implemented(what: &str, wp: &str) -> ExitCode {
    eprintln!("not implemented yet: {what} (WP {wp})");
    ExitCode::from(7)
}

/// Every `*.rs` file under `dir`, recursively, sorted. A missing directory has none.
pub fn rust_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if dir.is_dir() {
        collect(dir, &mut out)?;
    }
    out.sort();
    Ok(out)
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// `path` relative to `root` for messages (unchanged when it is not below `root`).
pub fn display_relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}

/// Ends a check: prints every finding and returns exit code 1 if there are any, else prints `ok` and returns 0.
pub fn finish(task: &str, findings: &[String], ok_summary: &str) -> ExitCode {
    if findings.is_empty() {
        println!("{task}: ok ({ok_summary})");
        ExitCode::SUCCESS
    } else {
        for f in findings {
            eprintln!("{task}: {f}");
        }
        eprintln!("{task}: {} problem(s)", findings.len());
        ExitCode::FAILURE
    }
}

/// Reports an error that stopped a task (it could not run, as opposed to finding problems): exit code 1.
pub fn fail(task: &str, message: impl std::fmt::Display) -> ExitCode {
    eprintln!("{task}: error: {message}");
    ExitCode::FAILURE
}
