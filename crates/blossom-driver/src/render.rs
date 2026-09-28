//! Rendering diagnostics with their source lines (ARCHITECTURE §12.2):
//!
//! ```text
//! error[BLS0300]: type mismatch in column 2 of `log`: Node and i64
//!   --> simple.ded:4:14
//!    |
//!  4 | log(Node2, 3)@async :- bcast(Node1, Pload), node(Node1, Node2);
//!    |            ^
//!    = note: …
//! ```

use std::fmt::Write;

use blossom_base::{Diagnostic, SourceDb, Span};

/// Renders one diagnostic. Spans whose file is not in `sources` are shown without a snippet.
pub fn render(d: &Diagnostic, sources: &SourceDb) -> String {
    let mut out = format!("{d}\n");
    if let Some(span) = d.primary {
        snippet(&mut out, sources, span, None);
    }
    for label in &d.labels {
        snippet(&mut out, sources, label.span, Some(&label.message));
    }
    for note in &d.notes {
        let _ = writeln!(out, "   = note: {note}");
    }
    out
}

fn snippet(out: &mut String, sources: &SourceDb, span: Span, message: Option<&str>) {
    let (Ok(path), Ok(at)) = (sources.path(span.file), sources.line_col(span.file, span.lo)) else {
        return;
    };
    let _ = writeln!(out, "  --> {path}:{}:{}", at.line, at.column);
    let Ok(line) = sources.line_text(span.file, at.line) else {
        return;
    };
    let width = sources
        .span_text(span)
        .map(|t| t.lines().next().unwrap_or("").chars().count().max(1))
        .unwrap_or(1);
    let gutter = at.line.to_string().len();
    let pad = " ".repeat(gutter);
    let _ = writeln!(out, " {pad} |");
    let _ = writeln!(out, " {} | {line}", at.line);
    let indent = " ".repeat(at.column.saturating_sub(1) as usize);
    let carets = "^".repeat(width);
    match message {
        Some(m) => {
            let _ = writeln!(out, " {pad} | {indent}{carets} {m}");
        }
        None => {
            let _ = writeln!(out, " {pad} | {indent}{carets}");
        }
    }
}
