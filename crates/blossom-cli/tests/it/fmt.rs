//! `blossom fmt`: rewriting files and directories, `--check`, standard input, and files that do not parse.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::blossom;

/// A fresh directory for one test (removed when it ends).
#[cfg(test)]
struct Scratch(PathBuf);

#[cfg(test)]
impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("blossom-fmt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        Scratch(dir)
    }
}

#[cfg(test)]
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const MESSY: &str = "program p version 1;\ninput go(k:u64);\noutput o(k:u64);\nh:on go(k){emit o(k);}\n";
const TIDY: &str = "program p version 1;\ninput go(k: u64);\noutput o(k: u64);\nh: on go(k) {\n    emit o(k);\n}\n";

#[test]
fn fmt_rewrites_files_and_directories_and_check_lists_what_would_change() {
    let s = Scratch::new("files");
    let (a, b, broken) = (s.0.join("a.bls"), s.0.join("sub/b.bls"), s.0.join("sub/broken.bls"));
    std::fs::write(&a, MESSY).unwrap();
    std::fs::write(&b, TIDY).unwrap();
    std::fs::write(s.0.join("sub/notes.txt"), "not blossom").unwrap();
    let dir = s.0.to_str().unwrap();
    // --check: the messy file is listed, nothing is written, exit 1.
    let out = blossom(&["fmt", "--check", dir]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), a.display().to_string());
    assert_eq!(std::fs::read_to_string(&a).unwrap(), MESSY);
    // Formatting rewrites it; then --check passes.
    assert_eq!(blossom(&["fmt", dir]).status.code(), Some(0));
    assert_eq!(std::fs::read_to_string(&a).unwrap(), TIDY);
    assert_eq!(std::fs::read_to_string(&b).unwrap(), TIDY);
    assert_eq!(blossom(&["fmt", "--check", dir]).status.code(), Some(0));
    // A file that does not parse is left as it is, its error reported.
    std::fs::write(&broken, "program p version 1;\ntable t(x: u64;\n").unwrap();
    let out = blossom(&["fmt", broken.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("not formatted") && err.contains("error["), "{err}");
    assert_eq!(
        std::fs::read_to_string(&broken).unwrap(),
        "program p version 1;\ntable t(x: u64;\n"
    );
}

#[test]
fn fmt_formats_standard_input_to_standard_output() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_blossom"))
        .arg("fmt")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(MESSY.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8(out.stdout).unwrap(), TIDY);
}
