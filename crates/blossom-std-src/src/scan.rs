//! Maps the files under `std/` to standard-library module paths. Shared by `build.rs` (through `#[path]`) and the
//! crate's tests, so the rules are tested where they are defined.
//!
//! - `std/a/b.bls` and `std/a/b/mod.bls` are module `std::a::b` (LANGUAGE §6.1); `std/mod.bls` is `std`.
//! - Files that do not end in `.bls`, and hidden files and directories, are ignored.
//! - A module that cannot be embedded is *rejected*, not fatal: a name that is not an identifier, a file that is
//!   not UTF-8, or a module defined by both `b.bls` and `b/mod.bls`. Rejections are per module, so one broken file
//!   never affects a program that does not import it (PLAN §4 D10).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A module that can be embedded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    /// The module path, e.g. `std::bcast::reliable`.
    pub path: String,
    /// The source file.
    pub file: PathBuf,
}

/// A module that cannot be embedded, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    /// The module path (best effort when the file name itself is the problem).
    pub path: String,
    /// Why.
    pub reason: String,
}

/// The result of scanning `std/`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// Embeddable modules, sorted by path.
    pub modules: Vec<Module>,
    /// Rejected modules, sorted by path.
    pub rejected: Vec<Rejected>,
    /// Every directory and `.bls` file seen, for `cargo::rerun-if-changed`.
    pub watched: Vec<PathBuf>,
}

/// Whether `segment` can name a module: an ASCII identifier.
pub fn is_module_segment(segment: &str) -> bool {
    let mut chars = segment.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Scans `std_dir`. A missing directory is an empty standard library.
pub fn scan(std_dir: &Path) -> io::Result<Scan> {
    let mut found: Vec<(Vec<String>, PathBuf, Option<String>)> = Vec::new();
    let mut out = Scan::default();
    if !std_dir.is_dir() {
        return Ok(out);
    }
    walk(std_dir, &mut Vec::new(), &mut found, &mut out.watched)?;
    // Group by module path to detect `b.bls` + `b/mod.bls`.
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut i = 0;
    while let Some((segments, file, problem)) = found.get(i) {
        let path = module_path(segments);
        let same = found.iter().skip(i).take_while(|(s, _, _)| s == segments).count();
        if same > 1 {
            let files: Vec<String> = found
                .iter()
                .skip(i)
                .take(same)
                .map(|(_, f, _)| f.display().to_string())
                .collect();
            out.rejected.push(Rejected {
                path,
                reason: format!("defined by more than one file: {}", files.join(", ")),
            });
        } else if let Some(problem) = problem {
            out.rejected.push(Rejected {
                path,
                reason: problem.clone(),
            });
        } else {
            let bytes = fs::read(file)?;
            match std::str::from_utf8(&bytes) {
                Ok(_) => out.modules.push(Module {
                    path,
                    file: file.clone(),
                }),
                Err(e) => out.rejected.push(Rejected {
                    path,
                    reason: format!(
                        "{} is not valid UTF-8 (first invalid byte at offset {})",
                        file.display(),
                        e.valid_up_to()
                    ),
                }),
            }
        }
        i += same.max(1);
    }
    out.modules.sort_by(|a, b| a.path.cmp(&b.path));
    out.rejected.sort_by(|a, b| a.path.cmp(&b.path));
    out.watched.sort();
    Ok(out)
}

fn module_path(segments: &[String]) -> String {
    std::iter::once("std")
        .chain(segments.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("::")
}

fn walk(
    dir: &Path,
    prefix: &mut Vec<String>,
    found: &mut Vec<(Vec<String>, PathBuf, Option<String>)>,
    watched: &mut Vec<PathBuf>,
) -> io::Result<()> {
    watched.push(dir.to_path_buf());
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<io::Result<_>>()?;
    entries.sort();
    for entry in entries {
        let name = entry
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        let utf8_name = entry.file_name().and_then(|n| n.to_str()).is_some();
        if entry.is_dir() {
            prefix.push(name);
            walk(&entry, prefix, found, watched)?;
            prefix.pop();
            continue;
        }
        let Some(stem) = name.strip_suffix(".bls") else {
            continue;
        };
        watched.push(entry.clone());
        let mut segments = prefix.clone();
        if stem != "mod" {
            segments.push(stem.to_string());
        }
        let bad_segment = segments.iter().find(|s| !is_module_segment(s));
        let problem = if !utf8_name || entry.to_str().is_none() {
            Some(format!("{} has a file name that is not valid UTF-8", entry.display()))
        } else {
            bad_segment.map(|s| {
                format!(
                    "`{s}` in {} is not a module name (an ASCII identifier)",
                    entry.display()
                )
            })
        };
        found.push((segments, entry, problem));
    }
    Ok(())
}
