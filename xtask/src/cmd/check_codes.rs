//! `check-codes`: the diagnostic code registry (ARCHITECTURE §1.6, §12.1). Implemented by WP M1.1.
//!
//! 1. `blossom_base::codes::REGISTRY` equals the tables of LANGUAGE §20 (parsed from the markdown) plus the
//!    ARCHITECTURE §0.3 amendments: the same codes, severities, meanings and origins.
//! 2. Every `BLS[R]?nnn[n]` written in `crates/*/src` (a string literal, including inside macros and attributes, or
//!    an identifier) is registered — test code included.
//! 3. Outside test code (`#[cfg(test)]`, `#[test]`, and the files of test-only modules declared as
//!    `#[cfg(test)] mod name;`), a code is written only in its owning crate or a crate the registry allows. The
//!    registry itself (`crates/blossom-base/src/codes.rs`) is exempt.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use blossom_base::codes::{CodeInfo, CodeOrigin, REGISTRY};
use blossom_base::diag::Severity;

use crate::rustsrc;
use crate::util;

/// Arguments of `check-codes`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The repository root (default: the root this xtask was built in).
    #[arg(long)]
    pub root: Option<PathBuf>,
}

/// Runs the task.
pub fn run(args: Args) -> ExitCode {
    let root = util::root_or_default(args.root);
    match check(&root, REGISTRY) {
        Ok((findings, files)) => util::finish(
            "check-codes",
            &findings,
            &format!("{} codes, {files} source files", REGISTRY.len()),
        ),
        Err(e) => util::fail("check-codes", e),
    }
}

fn check(root: &Path, registry: &[CodeInfo]) -> Result<(Vec<String>, usize), String> {
    let read = |p: &str| std::fs::read_to_string(root.join(p)).map_err(|e| format!("cannot read {p}: {e}"));
    let language = parse_language(&read("docs/design/LANGUAGE.md")?)?;
    let amendments = parse_amendments(&read("docs/design/ARCHITECTURE.md")?)?;
    let expected = expected_registry(&language, &amendments)?;
    let mut findings = compare_registry(&expected, registry);
    let (occurrences, files) = scan_sources(root)?;
    findings.extend(check_occurrences(&occurrences, registry));
    Ok((findings, files))
}

// ---- the documents -----------------------------------------------------------------------------------------------

/// A code as a document states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocCode {
    /// `E`, `W` or `R`.
    pub severity: char,
    /// The meaning.
    pub meaning: String,
}

/// An ARCHITECTURE §0.3 amendment that adds or extends a code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Amendment {
    /// `Add **CODE** (SEV) "meaning"`.
    Add {
        /// The amendment id (`L2`).
        item: String,
        /// The code.
        code: String,
        /// Its severity: `(E)`, `(W)` or `(R)` as written, or `R` for a runtime code that does not state one.
        severity: char,
        /// Its meaning.
        meaning: String,
    },
    /// `Extend CODE to …`.
    Extend {
        /// The amendment id.
        item: String,
        /// The code.
        code: String,
        /// What the meaning is extended to.
        extension: String,
    },
}

fn section<'a>(md: &'a str, heading: &str, end: &str) -> Result<&'a str, String> {
    let start = md
        .find(heading)
        .ok_or_else(|| format!("heading `{heading}` not found"))?;
    let rest = md.get(start..).unwrap_or("");
    let stop = rest
        .get(heading.len()..)
        .and_then(|r| r.find(end))
        .map_or(rest.len(), |i| i + heading.len());
    Ok(rest.get(..stop).unwrap_or(rest))
}

/// Whether `s` is a code: `BLS` and four digits, or `BLSR` and three.
fn is_code(s: &str) -> bool {
    let digits = |d: &str, n: usize| d.len() == n && d.bytes().all(|b| b.is_ascii_digit());
    match s.strip_prefix("BLSR") {
        Some(d) => digits(d, 3),
        None => s.strip_prefix("BLS").is_some_and(|d| digits(d, 4)),
    }
}

/// Parses LANGUAGE §20: the `| Code | Sev | Meaning |` tables and the Lints and Runtime lists.
pub fn parse_language(md: &str) -> Result<BTreeMap<String, DocCode>, String> {
    let sec = section(md, "## 20. Diagnostics", "\n## ")?;
    let mut out = BTreeMap::new();
    let mut add = |code: &str, severity: char, meaning: &str| -> Result<(), String> {
        let meaning = meaning.split_whitespace().collect::<Vec<_>>().join(" ");
        if out.insert(code.to_string(), DocCode { severity, meaning }).is_some() {
            return Err(format!("LANGUAGE §20 lists {code} twice"));
        }
        Ok(())
    };
    for line in sec.lines() {
        let Some(row) = line.trim().strip_prefix('|').and_then(|r| r.strip_suffix('|')) else {
            continue;
        };
        let cells: Vec<&str> = row.split('|').map(str::trim).collect();
        let (Some(code), Some(sev)) = (cells.first(), cells.get(1)) else {
            continue;
        };
        if !is_code(code) {
            continue;
        }
        let severity = match *sev {
            "E" => 'E',
            "W" => 'W',
            "R" => 'R',
            other => return Err(format!("LANGUAGE §20: {code} has unknown severity `{other}`")),
        };
        // A meaning containing `|` spans several cells.
        let meaning = cells.get(2..).map(|m| m.join("|")).unwrap_or_default();
        add(code, severity, &meaning)?;
    }
    for (label, severity) in [
        ("**Lints (BLS1xxx)**, warnings by default:", 'W'),
        ("**Runtime (BLSRxxx)**:", 'R'),
    ] {
        let start = sec
            .find(label)
            .ok_or_else(|| format!("LANGUAGE §20: `{label}` not found"))?;
        let rest = sec.get(start + label.len()..).unwrap_or("");
        let paragraph = rest.split("\n\n").next().unwrap_or("");
        let text = paragraph.split_whitespace().collect::<Vec<_>>().join(" ");
        let text = text.strip_suffix('.').unwrap_or(&text);
        for item in split_code_list(text) {
            let (code, meaning) = item
                .split_once(' ')
                .ok_or_else(|| format!("LANGUAGE §20: cannot read `{item}`"))?;
            if !is_code(code) {
                return Err(format!("LANGUAGE §20: `{code}` in `{label}` is not a code"));
            }
            add(code, severity, meaning)?;
        }
    }
    Ok(out)
}

/// Splits `BLS1001 a; BLS1002 b (x; y); …` at the `; ` that precede a code.
fn split_code_list(text: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut start = 0;
    let mut search = 0;
    while let Some(i) = text.get(search..).and_then(|r| r.find("; ")).map(|i| i + search) {
        let next = text.get(i + 2..).unwrap_or("");
        if next.split(' ').next().is_some_and(is_code) {
            items.push(text.get(start..i).unwrap_or(""));
            start = i + 2;
        }
        search = i + 2;
    }
    items.push(text.get(start..).unwrap_or(""));
    items.into_iter().map(str::trim).filter(|s| !s.is_empty()).collect()
}

/// Parses the code-adding rows of ARCHITECTURE §0.3.
pub fn parse_amendments(md: &str) -> Result<Vec<Amendment>, String> {
    let sec = section(md, "### 0.3 Amendments requested of LANGUAGE.md", "\n---")?;
    let mut out = Vec::new();
    for line in sec.lines() {
        let Some(row) = line.trim().strip_prefix("| L") else {
            continue;
        };
        let item = format!("L{}", row.split('|').next().unwrap_or("").trim());
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        let amendment = cells.get(3).copied().unwrap_or("");
        // `**CODE** (SEV) "meaning"` — possibly several in one cell.
        let mut rest = amendment;
        while let Some(i) = rest.find("**BLS") {
            let after = rest.get(i + 2..).unwrap_or("");
            let end = after
                .find("**")
                .ok_or_else(|| format!("ARCHITECTURE §0.3 {item}: unterminated code"))?;
            let code = after.get(..end).unwrap_or("").to_string();
            let tail = after.get(end + 2..).unwrap_or("").trim_start();
            // `(E)`, `(W)` or `(R)`; a runtime code (BLSRnnn) may leave it out, since its prefix says it.
            let (severity, tail) = match tail.strip_prefix('(').and_then(|t| t.split_once(')')) {
                Some(("E", t)) => ('E', t.trim_start()),
                Some(("W", t)) => ('W', t.trim_start()),
                Some(("R", t)) => ('R', t.trim_start()),
                _ if code.starts_with("BLSR") => ('R', tail),
                _ => {
                    return Err(format!(
                        "ARCHITECTURE §0.3 {item}: {code} has no severity `(E)`, `(W)` or `(R)`"
                    ));
                }
            };
            let meaning = tail
                .strip_prefix('"')
                .and_then(|t| t.split_once('"'))
                .map(|(m, _)| m.to_string())
                .ok_or_else(|| format!("ARCHITECTURE §0.3 {item}: {code} has no quoted meaning"))?;
            if !is_code(&code) {
                return Err(format!("ARCHITECTURE §0.3 {item}: `{code}` is not a code"));
            }
            out.push(Amendment::Add {
                item: item.clone(),
                code,
                severity,
                meaning,
            });
            rest = tail;
        }
        if let Some(ext) = amendment.strip_prefix("Extend ") {
            let (code, extension) = ext
                .split_once(" to ")
                .ok_or_else(|| format!("ARCHITECTURE §0.3 {item}: cannot read `{ext}`"))?;
            if !is_code(code) {
                return Err(format!("ARCHITECTURE §0.3 {item}: `{code}` is not a code"));
            }
            out.push(Amendment::Extend {
                item: item.clone(),
                code: code.to_string(),
                extension: extension.trim_end_matches('.').to_string(),
            });
        }
    }
    Ok(out)
}

/// A code as the registry must record it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedCode {
    /// `E`, `W` or `R`.
    pub severity: char,
    /// The meaning.
    pub meaning: String,
    /// The origin.
    pub origin: CodeOrigin,
}

/// LANGUAGE §20 with the amendments applied. An extension appends
/// `; extended (ARCHITECTURE §0.3 Ln) to <extension>` to the meaning.
pub fn expected_registry(
    language: &BTreeMap<String, DocCode>,
    amendments: &[Amendment],
) -> Result<BTreeMap<String, ExpectedCode>, String> {
    let mut out: BTreeMap<String, ExpectedCode> = language
        .iter()
        .map(|(code, d)| {
            (
                code.clone(),
                ExpectedCode {
                    severity: d.severity,
                    meaning: d.meaning.clone(),
                    origin: CodeOrigin::Language,
                },
            )
        })
        .collect();
    for a in amendments {
        match a {
            Amendment::Add {
                item,
                code,
                severity,
                meaning,
            } => {
                let origin = CodeOrigin::Amendment(leak(item));
                let prev = out.insert(
                    code.clone(),
                    ExpectedCode {
                        severity: *severity,
                        meaning: meaning.clone(),
                        origin,
                    },
                );
                if prev.is_some() {
                    return Err(format!(
                        "ARCHITECTURE §0.3 {item} adds {code}, which LANGUAGE §20 already has"
                    ));
                }
            }
            Amendment::Extend { item, code, extension } => {
                let entry = out
                    .get_mut(code)
                    .ok_or_else(|| format!("ARCHITECTURE §0.3 {item} extends {code}, which LANGUAGE §20 lacks"))?;
                entry.meaning = format!("{}; extended (ARCHITECTURE §0.3 {item}) to {extension}", entry.meaning);
                entry.origin = CodeOrigin::Extended(leak(item));
            }
        }
    }
    Ok(out)
}

/// A `&'static str` for comparing with registry origins (amendment ids are a handful of short strings).
fn leak(s: &str) -> &'static str {
    blossom_base::Symbol::intern(s).as_str()
}

fn severity_char(s: Severity) -> char {
    match s {
        Severity::Error => 'E',
        Severity::Warning => 'W',
        Severity::Runtime => 'R',
    }
}

/// Differences between the documents and the registry.
pub fn compare_registry(expected: &BTreeMap<String, ExpectedCode>, registry: &[CodeInfo]) -> Vec<String> {
    let mut out = Vec::new();
    for info in registry {
        match expected.get(info.code) {
            None => out.push(format!(
                "{} is registered but not allocated by LANGUAGE §20 or ARCHITECTURE §0.3",
                info.code
            )),
            Some(e) => {
                if severity_char(info.severity) != e.severity {
                    out.push(format!(
                        "{}: registry severity {} but the documents say {}",
                        info.code,
                        severity_char(info.severity),
                        e.severity
                    ));
                }
                if info.meaning != e.meaning {
                    out.push(format!(
                        "{}: registry meaning {:?} but the documents say {:?}",
                        info.code, info.meaning, e.meaning
                    ));
                }
                if info.origin != e.origin {
                    out.push(format!(
                        "{}: registry origin {:?} but the documents say {:?}",
                        info.code, info.origin, e.origin
                    ));
                }
            }
        }
    }
    for code in expected.keys() {
        if !registry.iter().any(|i| i.code == code) {
            out.push(format!(
                "{code} is allocated by the documents but missing from blossom_base::codes::REGISTRY"
            ));
        }
    }
    out
}

// ---- the sources -------------------------------------------------------------------------------------------------

/// A code written in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    /// The crate's package name.
    pub crate_name: String,
    /// The file, relative to the root.
    pub file: String,
    /// 1-based line.
    pub line: usize,
    /// The code text.
    pub code: String,
    /// Whether it is in test-only code.
    pub in_test: bool,
}

/// The code-like substrings of `text`: `BLS`, an optional `R`, and a run of at least three digits.
pub fn find_codes(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = text.get(i..).and_then(|r| r.find("BLS")) {
        let start = i + off;
        let mut j = start + 3;
        if bytes.get(j) == Some(&b'R') {
            j += 1;
        }
        let digits_start = j;
        while bytes.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        if j - digits_start >= 3
            && let Some(code) = text.get(start..j)
        {
            out.push(code.to_string());
        }
        i = start + 3;
    }
    out
}

/// The package name of the crate at `dir`.
fn package_name(dir: &Path) -> Result<String, String> {
    let manifest = dir.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest).map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
    let value: toml::Value = toml::from_str(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
    value
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{} has no package name", manifest.display()))
}

/// Every code occurrence in `crates/*/src/**/*.rs`, and the number of files scanned.
fn scan_sources(root: &Path) -> Result<(Vec<Occurrence>, usize), String> {
    let crates_dir = root.join("crates");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&crates_dir)
        .map_err(|e| format!("cannot list {}: {e}", crates_dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("Cargo.toml").is_file())
        .collect();
    dirs.sort();
    let registry_file = root.join("crates").join("blossom-base").join("src").join("codes.rs");
    let mut out = Vec::new();
    let mut files = 0;
    for dir in dirs {
        let crate_name = package_name(&dir)?;
        let mut scanned = Vec::new();
        for file in util::rust_files(&dir.join("src")).map_err(|e| format!("cannot list {}: {e}", dir.display()))? {
            files += 1;
            if file == registry_file {
                continue;
            }
            let shown = util::display_relative(&file, root);
            let text = std::fs::read_to_string(&file).map_err(|e| format!("cannot read {shown}: {e}"))?;
            let s = rustsrc::scan(&text).map_err(|e| format!("{shown}: cannot parse: {e}"))?;
            scanned.push((file, shown, s));
        }
        let test_files = test_module_files(&scanned);
        for (file, shown, s) in scanned {
            out.extend(occurrences(&crate_name, &shown, s, test_files.contains(&file)));
        }
    }
    Ok((out, files))
}

/// The files that hold test-only modules: those declared by `#[cfg(test)] mod name;` (or by an out-of-line `mod`
/// inside test code), and every module file declared from one of them, to a fixpoint. A module `name` declared in
/// `dir/lib.rs`, `dir/main.rs` or `dir/mod.rs` lives in `dir/name.rs` or `dir/name/mod.rs`; one declared in
/// `dir/file.rs` lives below `dir/file/`; a `#[path]` attribute names the file relative to the declaring file's
/// directory.
fn test_module_files(scanned: &[(PathBuf, String, rustsrc::Scanned)]) -> BTreeSet<PathBuf> {
    let children = |file: &Path, decl: &rustsrc::ModDecl| -> Vec<PathBuf> {
        let Some(parent) = file.parent() else { return Vec::new() };
        if let Some(path) = &decl.path {
            return vec![parent.join(path)];
        }
        let is_root = file
            .file_name()
            .is_some_and(|n| n == "lib.rs" || n == "main.rs" || n == "mod.rs");
        let base = match (is_root, file.file_stem()) {
            (true, _) | (false, None) => parent.to_path_buf(),
            (false, Some(stem)) => parent.join(stem),
        };
        vec![
            base.join(format!("{}.rs", decl.name)),
            base.join(&decl.name).join("mod.rs"),
        ]
    };
    let mut test_files = BTreeSet::new();
    loop {
        let before = test_files.len();
        for (file, _, s) in scanned {
            let file_is_test = test_files.contains(file);
            for decl in &s.mods {
                if decl.in_test || file_is_test {
                    test_files.extend(children(file, decl));
                }
            }
        }
        if test_files.len() == before {
            return test_files;
        }
    }
}

/// The code occurrences of one scanned file; `test_file` marks every one as test code.
fn occurrences(crate_name: &str, file: &str, scanned: rustsrc::Scanned, test_file: bool) -> Vec<Occurrence> {
    let mut out = Vec::new();
    for token in scanned.tokens {
        for code in find_codes(&token.text) {
            out.push(Occurrence {
                crate_name: crate_name.to_string(),
                file: file.to_string(),
                line: token.line,
                code,
                in_test: test_file || token.in_test,
            });
        }
    }
    out
}

/// Unregistered codes anywhere, and codes constructed outside their owners in non-test code.
pub fn check_occurrences(occurrences: &[Occurrence], registry: &[CodeInfo]) -> Vec<String> {
    let mut out = Vec::new();
    for o in occurrences {
        match registry.iter().find(|i| i.code == o.code) {
            None => out.push(format!(
                "{}:{}: {} is not a registered code (LANGUAGE §20)",
                o.file, o.line, o.code
            )),
            Some(info) if !o.in_test && !info.may_be_constructed_in(&o.crate_name) => out.push(format!(
                "{}:{}: {} is owned by {}{}; {} may not construct it",
                o.file,
                o.line,
                o.code,
                info.owner_crate,
                if info.also.is_empty() {
                    String::new()
                } else {
                    format!(" (also {})", info.also.join(", "))
                },
                o.crate_name
            )),
            Some(_) => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LANG: &str = "# X\n\n## 20. Diagnostics (TEST-091)\n\n**Lexical (BLS00xx)**\n\n| Code | Sev | Meaning |\n\
        |---|---|---|\n| BLS0001 | E | unexpected character |\n| BLS0505 | W | `while` with an event literal: write `on` |\n\n\
        **Lints (BLS1xxx)**, warnings by default: BLS1001 naming convention; BLS1002 unused variable (ANA-008; x); BLS1003\n\
        possible conflict.\n\n**Runtime (BLSRxxx)**: BLSR001 key violation (SEM-050), naming both; BLSR010 `error(\"…\")` in a function.\n\n\
        ---\n\n## 21. Other\n| BLS0999 | E | not in §20 |\n";

    const ARCH: &str = "### 0.3 Amendments requested of LANGUAGE.md\n\n| # | § | Amendment | Why |\n|---|---|---|---|\n\
        | L1 | §20 | Give the runtime codes a table, and add **BLSR011** \"a dot reused\" | x |\n\
        | L2 | §20 | Add **BLS0908** (E) \"not implemented\" | y |\n\
        | L7 | §18.2 | Extend BLS1003 to channels without senders | z |\n\n---\n";

    #[test]
    fn check_codes_parses_language_tables_and_lists() {
        let lang = parse_language(LANG).unwrap();
        let codes: Vec<&str> = lang.keys().map(String::as_str).collect();
        assert_eq!(
            codes,
            vec![
                "BLS0001", "BLS0505", "BLS1001", "BLS1002", "BLS1003", "BLSR001", "BLSR010"
            ]
        );
        assert_eq!(
            lang["BLS0505"],
            DocCode {
                severity: 'W',
                meaning: "`while` with an event literal: write `on`".into()
            }
        );
        assert_eq!(lang["BLS1002"].meaning, "unused variable (ANA-008; x)");
        assert_eq!(lang["BLS1003"].meaning, "possible conflict");
        assert_eq!(
            lang["BLSR010"],
            DocCode {
                severity: 'R',
                meaning: "`error(\"…\")` in a function".into()
            }
        );
    }

    #[test]
    fn check_codes_applies_amendments() {
        let lang = parse_language(LANG).unwrap();
        let amendments = parse_amendments(ARCH).unwrap();
        assert_eq!(amendments.len(), 3);
        let expected = expected_registry(&lang, &amendments).unwrap();
        assert_eq!(expected["BLSR011"].severity, 'R');
        assert_eq!(
            expected["BLS0908"],
            ExpectedCode {
                severity: 'E',
                meaning: "not implemented".into(),
                origin: CodeOrigin::Amendment("L2")
            }
        );
        assert_eq!(
            expected["BLS1003"].meaning,
            "possible conflict; extended (ARCHITECTURE §0.3 L7) to channels without senders"
        );
        assert_eq!(expected["BLS1003"].origin, CodeOrigin::Extended("L7"));
    }

    #[test]
    fn check_codes_amendment_needs_a_severity() {
        let arch = "### 0.3 Amendments requested of LANGUAGE.md\n\n| # | § | Amendment | Why |\n|---|---|---|---|\n\
                    | L9 | §20 | Add **BLS0999** \"no severity given\" | x |\n\n---\n";
        let err = parse_amendments(arch).unwrap_err();
        assert!(err.contains("L9: BLS0999 has no severity"), "{err}");
    }

    #[test]
    fn check_codes_registry_matches_the_real_documents() {
        let root = util::workspace_root();
        let lang = parse_language(&std::fs::read_to_string(root.join("docs/design/LANGUAGE.md")).unwrap()).unwrap();
        let arch =
            parse_amendments(&std::fs::read_to_string(root.join("docs/design/ARCHITECTURE.md")).unwrap()).unwrap();
        assert_eq!(lang.len(), 117);
        let expected = expected_registry(&lang, &arch).unwrap();
        assert_eq!(compare_registry(&expected, REGISTRY), Vec::<String>::new());
    }

    #[test]
    fn check_codes_detects_registry_drift() {
        let lang = parse_language(LANG).unwrap();
        let expected = expected_registry(&lang, &parse_amendments(ARCH).unwrap()).unwrap();
        let wrong = [CodeInfo {
            code: "BLS0001",
            severity: Severity::Warning,
            owner_crate: "blossom-syntax",
            also: &[],
            origin: CodeOrigin::Language,
            meaning: "something else",
        }];
        let findings = compare_registry(&expected, &wrong);
        assert!(findings.iter().any(|f| f.starts_with("BLS0001: registry severity W")));
        assert!(findings.iter().any(|f| f.starts_with("BLS0001: registry meaning")));
        assert!(
            findings
                .iter()
                .any(|f| f.starts_with("BLS0505 is allocated by the documents but missing"))
        );
    }

    #[test]
    fn check_codes_ownership_and_registration() {
        let src = "/// BLS0502 in docs is fine\nfn f() { let _ = code!(\"BLS0502\"); }\n\
                   #[derive(thiserror::Error)] enum E { #[error(\"BLSR001 key\")] K }\n\
                   fn g() { let _ = \"BLS0510\"; }\n#[cfg(test)] mod t { fn h() { let _ = \"BLS0200\"; let _ = \"BLS9999\"; } }";
        let occ = occurrences(
            "blossom-front",
            "crates/blossom-front/src/x.rs",
            rustsrc::scan(src).unwrap(),
            false,
        );
        let findings = check_occurrences(&occ, REGISTRY);
        let joined = findings.join("\n");
        assert!(
            joined.contains("x.rs:2: BLS0502 is owned by blossom-analysis; blossom-front may not construct it"),
            "{joined}"
        );
        assert!(
            joined.contains("x.rs:3: BLSR001 is owned by blossom-engine (also blossom-oracle)"),
            "{joined}"
        );
        assert!(joined.contains("x.rs:4: BLS0510 is not a registered code"), "{joined}");
        assert!(
            joined.contains("BLS9999 is not a registered code"),
            "test code must use registered codes too: {joined}"
        );
        assert!(
            !joined.contains("BLS0200"),
            "test code may mention other crates' codes: {joined}"
        );
        assert_eq!(findings.len(), 4, "{joined}");
    }

    #[test]
    fn check_codes_test_module_files() {
        let file = |path: &str, src: &str| (PathBuf::from(path), path.to_string(), rustsrc::scan(src).unwrap());
        let scanned = vec![
            file("/c/src/lib.rs", "#[cfg(test)] mod tests; mod a;"),
            file("/c/src/tests.rs", "mod helpers; fn t() { let _ = \"BLS0502\"; }"),
            file("/c/src/tests/helpers.rs", "fn h() {}"),
            file("/c/src/a.rs", "#[cfg(test)] #[path = \"a_tests.rs\"] mod t; mod b;"),
            file("/c/src/a/b.rs", "fn b() {}"),
        ];
        let tests = test_module_files(&scanned);
        for yes in ["/c/src/tests.rs", "/c/src/tests/helpers.rs", "/c/src/a_tests.rs"] {
            assert!(tests.contains(Path::new(yes)), "{yes} in {tests:?}");
        }
        for no in ["/c/src/lib.rs", "/c/src/a.rs", "/c/src/a/b.rs"] {
            assert!(!tests.contains(Path::new(no)), "{no} in {tests:?}");
        }
        let (path, shown, s) = scanned.into_iter().nth(1).unwrap();
        let occ = occurrences("blossom-front", &shown, s, tests.contains(&path));
        assert!(occ.iter().all(|o| o.in_test && o.code == "BLS0502"), "{occ:?}");
        assert!(check_occurrences(&occ, REGISTRY).is_empty());
    }

    #[test]
    fn check_codes_find_codes() {
        assert_eq!(
            find_codes("x BLS0001, BLSR011;BLS12 BLS12345"),
            vec!["BLS0001", "BLSR011", "BLS12345"]
        );
        assert!(find_codes("BLSX001 BLS").is_empty());
    }
}
