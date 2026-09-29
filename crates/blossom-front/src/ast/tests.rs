//! Every example and corpus program converts without an unexpected tree shape.

use std::path::{Path, PathBuf};

use blossom_base::{Diagnostics, FileId};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "bls") {
            out.push(p);
        }
    }
}

#[test]
fn every_program_converts() {
    let mut files = Vec::new();
    collect(&root().join("examples"), &mut files);
    collect(&root().join("tests/corpus"), &mut files);
    files.sort();
    assert!(files.len() > 200, "found only {} programs", files.len());
    let mut bad = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        let parse = blossom_syntax::parser::parse(FileId::from_raw(0), &text);
        if !parse.errors.is_empty() {
            // Negative parser cases are the parser's business.
            continue;
        }
        let mut diags = Diagnostics::new();
        super::convert(FileId::from_raw(0), &parse.syntax(), &mut diags);
        for d in diags.iter().filter(|d| d.message.contains("syntax shape")) {
            bad.push(format!("{}: {} at {:?}", f.display(), d.message, d.primary));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
