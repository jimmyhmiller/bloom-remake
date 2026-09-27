//! User-facing diagnostics (ARCHITECTURE §12.1; LANGUAGE §20, TEST-091).
//!
//! A [`Diagnostic`] has a registered [`Code`], a severity, a message, a primary span, secondary labels for every
//! piece of evidence, notes and optional fix-its. It serializes with serde (the driver renders it as text with
//! `codespan-reporting` or as JSON for `--message-format=json`). Spans are byte ranges; the [`SourceDb`] maps them
//! to lines and columns.
//!
//! [`SourceDb`]: crate::SourceDb

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::codes::Code;
use crate::error::{FeatureId, Unimplemented};
use crate::span::Span;

/// How serious a diagnostic is (LANGUAGE §20).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum Severity {
    /// **E**: the program is rejected.
    Error,
    /// **W**: a warning; an error under `--strict` (ODD-10 (c)).
    Warning,
    /// **R**: a runtime hard error; the tick aborts with a located report.
    Runtime,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Runtime => "runtime error",
        })
    }
}

/// A secondary span with a message: one piece of evidence.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Label {
    /// Where.
    pub span: Span,
    /// What it shows.
    pub message: String,
}

/// One replacement of a fix-it.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TextEdit {
    /// The text to replace (empty for an insertion).
    pub span: Span,
    /// The new text.
    pub replacement: String,
}

/// A suggested fix: a set of edits applied together.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct FixIt {
    /// What the fix does, e.g. "insert the bang".
    pub message: String,
    /// The edits.
    pub edits: Vec<TextEdit>,
}

/// A diagnostic with a stable code (LANGUAGE §20).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Diagnostic {
    /// The registered code.
    pub code: Code,
    /// The severity; starts as the code's registered severity (warnings become errors under `--strict`).
    pub severity: Severity,
    /// The main message.
    pub message: String,
    /// The primary span, when the diagnostic has a source location.
    pub primary: Option<Span>,
    /// Evidence spans.
    pub labels: Vec<Label>,
    /// Notes.
    pub notes: Vec<String>,
    /// Suggested fixes.
    pub fixits: Vec<FixIt>,
}

impl Diagnostic {
    /// A diagnostic with the code's registered severity and no spans yet.
    pub fn new(code: Code, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code,
            severity: code.severity(),
            message: message.into(),
            primary: None,
            labels: Vec::new(),
            notes: Vec::new(),
            fixits: Vec::new(),
        }
    }

    /// BLS0908 (ARCHITECTURE §0.3 L2): "not implemented in this build: FEATURE-ID (what), needed by LABEL". This is
    /// the only constructor of BLS0908; build- and load-time capability checks report unimplemented features with it.
    pub fn not_implemented(feature: FeatureId, what: &str, needed_by: &str) -> Diagnostic {
        Diagnostic::new(
            crate::code!("BLS0908"),
            format!("not implemented in this build: {feature} ({what}), needed by {needed_by}"),
        )
    }

    /// BLS0908 for an [`Unimplemented`] error met at build or load time.
    pub fn from_unimplemented(err: &Unimplemented, needed_by: &str) -> Diagnostic {
        Diagnostic::not_implemented(err.feature, &err.detail, needed_by)
            .with_note(format!("reported at {}:{}:{}", err.file, err.line, err.column))
    }

    /// Sets the primary span.
    pub fn with_primary(mut self, span: Span) -> Diagnostic {
        self.primary = Some(span);
        self
    }

    /// Adds an evidence label.
    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Diagnostic {
        self.labels.push(Label {
            span,
            message: message.into(),
        });
        self
    }

    /// Adds a note.
    pub fn with_note(mut self, note: impl Into<String>) -> Diagnostic {
        self.notes.push(note.into());
        self
    }

    /// Adds a fix-it.
    pub fn with_fixit(mut self, fixit: FixIt) -> Diagnostic {
        self.fixits.push(fixit);
        self
    }

    /// Whether this diagnostic rejects the program (an error or a runtime error).
    pub fn is_error(&self) -> bool {
        self.severity != Severity::Warning
    }
}

impl fmt::Display for Diagnostic {
    /// `error[BLS0502]: message` (the driver adds source snippets).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}[{}]: {}", self.severity, self.code, self.message)
    }
}

/// An ordered collection of diagnostics.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    /// An empty collection.
    pub fn new() -> Diagnostics {
        Diagnostics::default()
    }

    /// Adds one.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.items.push(diagnostic);
    }

    /// The number of diagnostics.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The diagnostics in insertion order.
    pub fn iter(&self) -> std::slice::Iter<'_, Diagnostic> {
        self.items.iter()
    }

    /// Whether any diagnostic rejects the program.
    pub fn has_errors(&self) -> bool {
        self.items.iter().any(Diagnostic::is_error)
    }

    /// The number of diagnostics that reject the program.
    pub fn error_count(&self) -> usize {
        self.items.iter().filter(|d| d.is_error()).count()
    }

    /// The number of warnings.
    pub fn warning_count(&self) -> usize {
        self.items.len() - self.error_count()
    }

    /// `--strict` (ODD-10 (c)): every warning becomes an error.
    pub fn escalate_warnings(&mut self) {
        for d in &mut self.items {
            if d.severity == Severity::Warning {
                d.severity = Severity::Error;
            }
        }
    }

    /// Sorts into the canonical reporting order: by primary span (diagnostics without one last), then code, then
    /// message. Stable, so equal keys keep their insertion order.
    pub fn sort_canonical(&mut self) {
        self.items.sort_by(|a, b| {
            let key = |d: &Diagnostic| (d.primary.is_none(), d.primary, d.code);
            key(a).cmp(&key(b)).then_with(|| a.message.cmp(&b.message))
        });
    }

    /// Unwraps into the vector.
    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.items
    }
}

impl Extend<Diagnostic> for Diagnostics {
    fn extend<T: IntoIterator<Item = Diagnostic>>(&mut self, iter: T) {
        self.items.extend(iter);
    }
}

impl FromIterator<Diagnostic> for Diagnostics {
    fn from_iter<T: IntoIterator<Item = Diagnostic>>(iter: T) -> Self {
        Diagnostics {
            items: iter.into_iter().collect(),
        }
    }
}

impl IntoIterator for Diagnostics {
    type Item = Diagnostic;
    type IntoIter = std::vec::IntoIter<Diagnostic>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl<'a> IntoIterator for &'a Diagnostics {
    type Item = &'a Diagnostic;
    type IntoIter = std::slice::Iter<'a, Diagnostic>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idx::FileId;

    fn span(lo: u32, hi: u32) -> Span {
        Span::new(FileId::from_raw(0), lo, hi)
    }

    fn sample() -> Diagnostic {
        Diagnostic::new(crate::code!("BLS0700"), "`s.is_empty()` is antitone and needs a bang")
            .with_primary(span(10, 20))
            .with_label(span(4, 8), "`s` is an LSet")
            .with_note("LANGUAGE §11.4")
            .with_fixit(FixIt {
                message: "insert the bang".into(),
                edits: vec![TextEdit {
                    span: span(18, 18),
                    replacement: "!".into(),
                }],
            })
    }

    #[test]
    fn diagnostic_json_roundtrip() {
        let d = sample();
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains(r#""code":"BLS0700""#), "{json}");
        assert!(json.contains(r#""severity":"Error""#), "{json}");
        let back: Diagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
        let all: Diagnostics = [d.clone(), Diagnostic::new(crate::code!("BLS1002"), "unused")]
            .into_iter()
            .collect();
        let json = serde_json::to_string(&all).unwrap();
        assert_eq!(serde_json::from_str::<Diagnostics>(&json).unwrap(), all);
        // An unregistered code never deserializes.
        // (Built with format! so no unregistered code is written in the source; check-codes rejects those.)
        let bad = json.replace("BLS1002", &format!("BLS{}", 1999));
        assert!(serde_json::from_str::<Diagnostics>(&bad).is_err());
    }

    #[test]
    fn diagnostic_severity_counts_and_strict() {
        let mut all = Diagnostics::new();
        all.push(Diagnostic::new(
            crate::code!("BLS0505"),
            "`while` with an event literal",
        ));
        assert!(!all.has_errors());
        assert_eq!((all.error_count(), all.warning_count()), (0, 1));
        all.escalate_warnings();
        assert!(all.has_errors());
        assert_eq!(all.iter().next().map(|d| d.severity), Some(Severity::Error));
    }

    #[test]
    fn diagnostic_not_implemented_is_bls0908() {
        let d = Diagnostic::not_implemented(FeatureId("ENG-084"), "leapfrog triejoin", "tc/join:path");
        assert_eq!(d.code.as_str(), "BLS0908");
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(
            d.message,
            "not implemented in this build: ENG-084 (leapfrog triejoin), needed by tc/join:path"
        );
        assert_eq!(
            d.to_string(),
            "error[BLS0908]: not implemented in this build: ENG-084 (leapfrog triejoin), needed by tc/join:path"
        );
        let err = crate::unimplemented_error!("LANG-221", "the Overlog frontend (WP M15.2)");
        let d = Diagnostic::from_unimplemented(&err, "main.olg");
        assert!(
            d.message
                .contains("LANG-221 (the Overlog frontend (WP M15.2)), needed by main.olg")
        );
        assert_eq!(d.notes.len(), 1);
    }

    #[test]
    fn diagnostic_canonical_sort() {
        let mut all: Diagnostics = [
            Diagnostic::new(crate::code!("BLS0300"), "b").with_primary(span(30, 31)),
            Diagnostic::new(crate::code!("BLS0908"), "no span"),
            Diagnostic::new(crate::code!("BLS0200"), "a").with_primary(span(5, 6)),
            Diagnostic::new(crate::code!("BLS0100"), "c").with_primary(span(30, 31)),
        ]
        .into_iter()
        .collect();
        all.sort_canonical();
        let order: Vec<&str> = all.iter().map(|d| d.code.as_str()).collect();
        assert_eq!(order, vec!["BLS0200", "BLS0100", "BLS0300", "BLS0908"]);
    }
}
