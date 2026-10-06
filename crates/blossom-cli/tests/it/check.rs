//! `blossom check` and `blossom explain`: programs, spec files, `--spec`, `--strict`, and explaining codes.

use std::path::PathBuf;

use super::blossom;

/// A file of the repository's `examples/`.
#[cfg(test)]
fn example(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(name)
        .to_str()
        .unwrap()
        .to_owned()
}

/// A program written to a fresh file for one test (removed when it ends).
#[cfg(test)]
struct Source(PathBuf);

#[cfg(test)]
impl Source {
    fn new(name: &str, text: &str) -> Source {
        let path = std::env::temp_dir().join(format!("blossom-check-{name}-{}.bls", std::process::id()));
        std::fs::write(&path, text).unwrap();
        Source(path)
    }

    fn path(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

#[cfg(test)]
impl Drop for Source {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn check_accepts_programs_and_spec_files_and_reports_errors() {
    for name in ["e01_kvs.bls", "e11_raft_kv.bls", "e02_specs.bls", "kafka/broker.bls"] {
        let out = blossom(&["check", &example(name)]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let bad = Source::new(
        "bad",
        "program p version 1;\ninput go(k: u64);\noutput o(k: String);\nh: on go(k) { emit o(k); }\n",
    );
    let out = blossom(&["check", bad.path()]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.contains("error[BLS0300]") && err.ends_with(": 1 error(s)\n"),
        "{err}"
    );
}

#[test]
fn check_spec_names_the_specs_to_check() {
    let specs = example("e02_specs.bls");
    let out = blossom(&["check", &specs, "--spec", "RetryFaults,OneShotFaults"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    // An unknown spec, and one with no target (an include-only fragment).
    for (name, code) in [("Nope", "BLS0204"), ("Clique", "BLS0900")] {
        let out = blossom(&["check", &specs, "--spec", name]);
        assert_eq!(out.status.code(), Some(1), "{name}");
        assert!(String::from_utf8(out.stderr).unwrap().contains(code), "{name}");
    }
    // `--spec` checks specs, which give their own nodes.
    assert_eq!(
        blossom(&["check", &specs, "--spec", "RetryFaults", "--nodes", "a"])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn check_strict_rejects_warnings() {
    // `while` over an event literal is a warning (BLS0505), not an error.
    let warned = Source::new(
        "warned",
        "program p version 1;\ninput go(k: u64);\noutput o(k: u64);\nh: while go(k) { emit o(k); }\n",
    );
    let out = blossom(&["check", warned.path()]);
    let err = String::from_utf8(out.stderr).unwrap();
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(err.contains("warning[BLS0505]"), "{err}");
    assert_eq!(blossom(&["check", warned.path(), "--strict"]).status.code(), Some(1));
}

#[test]
fn explain_prints_a_code_its_meaning_and_where_the_reference_uses_it() {
    let out = blossom(&["explain", "BLS0300"]);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("BLS0300 — error: the program is rejected\n"), "{text}");
    assert!(
        text.contains("type mismatch") && text.contains("In LANGUAGE.md:"),
        "{text}"
    );
    // The prefix is optional and the case does not matter.
    assert_eq!(blossom(&["explain", "0300"]).stdout, text.as_bytes());
    assert_eq!(blossom(&["explain", "bls0300"]).stdout, text.as_bytes());
    let out = blossom(&["explain", "BLS9999"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    // No code: every code, one per line.
    let out = blossom(&["explain"]);
    let list = String::from_utf8(out.stdout).unwrap();
    assert_eq!(list.lines().count(), blossom_base::codes::REGISTRY.len());
    assert!(list.lines().any(|l| l.starts_with("BLSR004  R  ")), "{list}");
}
