//! `blossom explain`: explain a diagnostic code (LANGUAGE §20).
//!
//! `blossom explain BLS0430` prints the code's severity, its meaning (as the registry, LANGUAGE §20, states it) and
//! the places LANGUAGE.md uses it outside the table: each with its section, so the rule behind the code can be read in
//! context. `blossom explain` alone lists every code. A code that is not registered is a user error (exit 1).

use std::process::ExitCode;

use blossom_base::codes::{CodeInfo, CodeOrigin, REGISTRY};
use blossom_base::{Code, Severity};

use crate::common::Context;
use crate::exit::Exit;

/// The language reference, built into the binary so `explain` works anywhere.
const LANGUAGE: &str = include_str!("../../../../docs/design/LANGUAGE.md");

/// The most places shown for one code, and lines of one place.
const MAX_PLACES: usize = 12;
const MAX_LINES: usize = 8;

/// Arguments of `blossom explain`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The code (`BLS0430`, `BLSR004`; the `BLS` may be left out). None: every code, one per line.
    pub code: Option<String>,
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = cx;
    let Some(text) = args.code else {
        for info in REGISTRY {
            println!("{}  {}  {}", info.code, letter(info.severity), info.meaning);
        }
        return Exit::Ok.into();
    };
    let wanted = text.trim().to_ascii_uppercase();
    let wanted = if wanted.starts_with("BLS") {
        wanted
    } else {
        format!("BLS{wanted}")
    };
    let Some(code) = Code::lookup(&wanted) else {
        eprintln!("`{text}` is not a diagnostic code (`blossom explain` lists them)");
        return Exit::UserError.into();
    };
    print!("{}", explanation(code.info()));
    Exit::Ok.into()
}

fn letter(s: Severity) -> &'static str {
    match s {
        Severity::Error => "E",
        Severity::Warning => "W",
        Severity::Runtime => "R",
    }
}

/// A code's explanation: its header, its meaning, and where the language reference uses it.
fn explanation(info: &CodeInfo) -> String {
    let severity = match info.severity {
        Severity::Error => "error: the program is rejected",
        Severity::Warning => "warning (an error under `--strict`)",
        Severity::Runtime => "runtime error: the tick aborts with a located report",
    };
    let origin = match info.origin {
        CodeOrigin::Language => String::new(),
        CodeOrigin::Amendment(a) => format!(" (ARCHITECTURE §0.3 amendment {a})"),
        CodeOrigin::Extended(a) => format!(" (extended by ARCHITECTURE §0.3 amendment {a})"),
    };
    let mut out = format!("{} — {severity}{origin}\n\n  {}\n", info.code, info.meaning);
    let places = places(info.code);
    if !places.is_empty() {
        out.push_str("\nIn LANGUAGE.md:\n");
        for (section, line) in places.iter().take(MAX_PLACES) {
            out.push_str(&format!("\n  {section}\n    {line}\n"));
        }
        if places.len() > MAX_PLACES {
            out.push_str(&format!("\n  … and {} more\n", places.len() - MAX_PLACES));
        }
    }
    out
}

/// The paragraphs (or list items, table rows, code lines) of LANGUAGE.md that use `code`, with their sections; the
/// rows of the code tables themselves (`| BLS…`) are left out.
fn places(code: &str) -> Vec<(String, String)> {
    let mut section = String::from("LANGUAGE.md");
    let mut out = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    let mut fenced = false;
    let flush = |block: &mut Vec<&str>, section: &str, out: &mut Vec<(String, String)>| {
        if block.iter().any(|l| mentions(l, code)) {
            let mut text: Vec<String> = block.iter().take(MAX_LINES).map(|l| l.trim().to_owned()).collect();
            if block.len() > MAX_LINES {
                text.push("…".to_owned());
            }
            out.push((section.to_owned(), text.join("\n    ")));
        }
        block.clear();
    };
    for line in LANGUAGE.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            flush(&mut block, &section, &mut out);
            fenced = !fenced;
            continue;
        }
        if !fenced && line.starts_with('#') {
            flush(&mut block, &section, &mut out);
            section = line.trim_start_matches('#').trim().to_owned();
            continue;
        }
        // A paragraph ends at a blank line; a list item, a table row and a code line stand alone.
        let alone = fenced || trimmed.starts_with('|');
        if trimmed.is_empty() || alone || trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            flush(&mut block, &section, &mut out);
        }
        if trimmed.is_empty() || trimmed.starts_with("| BLS") {
            continue;
        }
        block.push(line);
        if alone {
            flush(&mut block, &section, &mut out);
        }
    }
    flush(&mut block, &section, &mut out);
    out
}

/// Whether `line` names `code` as a whole word (`BLS0430`, not `BLS04301`).
fn mentions(line: &str, code: &str) -> bool {
    line.match_indices(code).any(|(i, _)| {
        line.get(i + code.len()..)
            .and_then(|rest| rest.chars().next())
            .is_none_or(|c| !c.is_ascii_alphanumeric())
    })
}
