//! The shared error base (ARCHITECTURE §12.1, ARCH-17).
//!
//! - [`Unimplemented`] is the only way to leave a path unimplemented. Create it with [`unimplemented_feature!`]
//!   (which returns it from the enclosing function) or [`unimplemented_error!`](crate::unimplemented_error) (which
//!   evaluates to it). It names the FEATURES id and says what is missing, by convention with the WP that will
//!   implement it: `unimplemented_feature!("ENG-063", "counted regime (WP M6.1)")`.
//! - [`InternalError`] reports a violated internal invariant, a bug. Create it with [`bug!`] (returns it) or
//!   [`internal_error!`](crate::internal_error) (evaluates to it). Library code uses these instead of `assert!`.
//!   They panic instead when the calling crate is built with `debug_assertions` or `BLOSSOM_PANIC_ON_BUG=1` is set,
//!   so bugs are loud in development and are reported as errors in production.
//!
//! Every crate's error enum wraps both: `#[error(transparent)] Unimplemented(#[from] Unimplemented)` and
//! `#[error(transparent)] Internal(#[from] InternalError)`.

use std::backtrace::Backtrace;
use std::ffi::OsStr;
use std::fmt;
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::idx::IdxOverflow;
use crate::span::Symbol;

/// A FEATURES.md id such as `ENG-063`, used by [`Unimplemented`] errors, capability tables and certificates.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct FeatureId(pub &'static str);

/// The FEATURES.md areas (FEATURES §0).
const AREAS: [&str; 10] = [
    "LANG", "SEM", "ENG", "DIST", "ANA", "TEST", "VER", "LIB", "FLAG", "BENCH",
];

impl FeatureId {
    /// Whether `id` has the form of a FEATURES id: a known area, `-`, and three or four digits.
    pub const fn is_well_formed(id: &str) -> bool {
        let mut areas: &[&str] = &AREAS;
        while let [area, rest @ ..] = areas {
            if let Some(tail) = strip_prefix(id.as_bytes(), area.as_bytes()) {
                return is_dash_digits(tail);
            }
            areas = rest;
        }
        false
    }

    /// The id text.
    pub const fn as_str(self) -> &'static str {
        self.0
    }

    /// Parses a FEATURES id read at runtime (for example from a certificate), interning its text.
    pub fn parse(id: &str) -> Result<FeatureId, MalformedFeatureId> {
        if Self::is_well_formed(id) {
            Ok(FeatureId(Symbol::intern(id).as_str()))
        } else {
            Err(MalformedFeatureId { text: id.into() })
        }
    }
}

/// `bytes` without `prefix`, if it starts with it.
const fn strip_prefix<'a>(bytes: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    let (mut bytes, mut prefix) = (bytes, prefix);
    loop {
        match (bytes, prefix) {
            (_, []) => return Some(bytes),
            ([x, rest_b @ ..], [y, rest_p @ ..]) if *x == *y => {
                bytes = rest_b;
                prefix = rest_p;
            }
            _ => return None,
        }
    }
}

/// Whether `tail` is `-` followed by three or four ASCII digits.
const fn is_dash_digits(tail: &[u8]) -> bool {
    let [b'-', digits @ ..] = tail else { return false };
    if digits.len() < 3 || digits.len() > 4 {
        return false;
    }
    let mut rest = digits;
    while let [d, more @ ..] = rest {
        if !d.is_ascii_digit() {
            return false;
        }
        rest = more;
    }
    true
}

impl fmt::Display for FeatureId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl Serialize for FeatureId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0)
    }
}

impl<'de> Deserialize<'de> for FeatureId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        FeatureId::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// A string that is not a FEATURES id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "`{text}` is not a FEATURES id (expected AREA-NNN with AREA one of LANG, SEM, ENG, DIST, ANA, TEST, VER, LIB, FLAG, BENCH)"
)]
pub struct MalformedFeatureId {
    /// The rejected text.
    pub text: Box<str>,
}

/// The only way to leave a path unimplemented (user rule: no silent stubs).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not implemented yet: {feature} — {detail} (at {file}:{line}:{column})")]
pub struct Unimplemented {
    /// The FEATURES id of the missing feature.
    pub feature: FeatureId,
    /// What is missing, conventionally ending with the WP that implements it: `"… (WP M6.1)"`.
    pub detail: Arc<str>,
    /// Source file of the `unimplemented_feature!` site.
    pub file: &'static str,
    /// Line of the site.
    pub line: u32,
    /// Column of the site.
    pub column: u32,
}

/// Returns `Err(Unimplemented { .. }.into())` from the enclosing function (ARCHITECTURE §12.1).
///
/// The first argument must be a string literal naming a FEATURES id; a malformed id is a compile error. The rest is
/// a `format!` message saying what is missing and, where known, which WP implements it.
///
/// ```
/// use blossom_base::{unimplemented_feature, Unimplemented};
/// fn counted_regime() -> Result<(), Unimplemented> {
///     unimplemented_feature!("ENG-063", "the counted maintenance regime (WP {})", "M6.1")
/// }
/// let err = counted_regime().unwrap_err();
/// assert_eq!(err.feature.as_str(), "ENG-063");
/// assert!(err.to_string().starts_with("not implemented yet: ENG-063 — the counted maintenance regime (WP M6.1)"));
/// ```
#[macro_export]
macro_rules! unimplemented_feature {
    ($feature:literal, $($fmt:tt)*) => {
        return ::core::result::Result::Err($crate::unimplemented_error!($feature, $($fmt)*).into())
    };
}

/// Evaluates to an [`Unimplemented`] error (the expression form of [`unimplemented_feature!`]).
#[macro_export]
macro_rules! unimplemented_error {
    ($feature:literal, $($fmt:tt)*) => {
        $crate::error::Unimplemented {
            feature: {
                const FEATURE: $crate::error::FeatureId = $crate::error::FeatureId::__checked($feature);
                FEATURE
            },
            detail: ::std::format!($($fmt)*).into(),
            file: ::core::file!(),
            line: ::core::line!(),
            column: ::core::column!(),
        }
    };
}

impl FeatureId {
    /// Used by [`unimplemented_error!`]; evaluated at compile time, where a malformed id is a compile error.
    #[doc(hidden)]
    #[allow(clippy::panic)] // Only evaluated in a `const` item: the panic is a compile-time error, never a runtime one.
    pub const fn __checked(id: &'static str) -> FeatureId {
        if Self::is_well_formed(id) {
            FeatureId(id)
        } else {
            panic!("malformed FEATURES id in unimplemented_feature!/unimplemented_error!")
        }
    }
}

/// A violated internal invariant: a bug in Blossom, never a user error (ARCHITECTURE §12.1).
#[derive(Debug, Clone, thiserror::Error)]
#[error("internal error (a bug in Blossom): {what} (at {file}:{line})")]
pub struct InternalError {
    /// What went wrong.
    pub what: Arc<str>,
    /// Source file of the site that detected it.
    pub file: &'static str,
    /// Line of the site.
    pub line: u32,
    /// The backtrace at the site (captured per `RUST_BACKTRACE`/`RUST_LIB_BACKTRACE`).
    pub backtrace: Arc<Backtrace>,
}

impl InternalError {
    /// Creates the error for a site; used by [`bug!`], [`internal_error!`](crate::internal_error) and
    /// [`IndexVec::get_or_bug`](crate::IndexVec::get_or_bug). Panics instead when `debug_assertions` (of the calling
    /// crate) is on or `BLOSSOM_PANIC_ON_BUG=1` is set.
    #[doc(hidden)]
    pub fn at(what: impl Into<Arc<str>>, file: &'static str, line: u32, debug_assertions: bool) -> InternalError {
        InternalError::with_policy(what, file, line, debug_assertions || panic_on_bug())
    }

    /// Creates the error, panicking with its message instead when `panic` is set.
    #[allow(clippy::panic)] // The documented contract of `bug!`: loud in development builds (ARCHITECTURE §12.1).
    fn with_policy(what: impl Into<Arc<str>>, file: &'static str, line: u32, panic: bool) -> InternalError {
        let err = InternalError {
            what: what.into(),
            file,
            line,
            backtrace: Arc::new(Backtrace::capture()),
        };
        if panic {
            panic!("{err}");
        }
        err
    }
}

impl From<IdxOverflow> for InternalError {
    #[track_caller]
    fn from(overflow: IdxOverflow) -> Self {
        let location = std::panic::Location::caller();
        InternalError::at(
            overflow.to_string(),
            location.file(),
            location.line(),
            cfg!(debug_assertions),
        )
    }
}

/// Whether `BLOSSOM_PANIC_ON_BUG` asks internal errors to panic. Read once per process.
fn panic_on_bug() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| panic_on_bug_setting(std::env::var_os("BLOSSOM_PANIC_ON_BUG").as_deref()))
}

/// Interprets the value of `BLOSSOM_PANIC_ON_BUG`: only `1` enables it.
fn panic_on_bug_setting(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
}

/// Returns `Err(InternalError { .. }.into())` from the enclosing function: a violated internal invariant.
///
/// Takes a `format!` message. Panics instead when the calling crate is built with `debug_assertions` or
/// `BLOSSOM_PANIC_ON_BUG=1` is set.
///
/// ```
/// use blossom_base::{bug, InternalError};
/// fn checked_div(a: u32, b: u32) -> Result<u32, InternalError> {
///     if b == 0 {
///         bug!("divisor {b} must have been rejected by the validator");
///     }
///     Ok(a / b)
/// }
/// assert_eq!(checked_div(6, 3).unwrap(), 2);
/// ```
#[macro_export]
macro_rules! bug {
    ($($fmt:tt)+) => {
        return ::core::result::Result::Err($crate::internal_error!($($fmt)+).into())
    };
}

/// Evaluates to an [`InternalError`] (the expression form of [`bug!`]), with the same panic policy.
///
/// Useful where a value is needed: `map.get(&k).ok_or_else(|| internal_error!("{k:?} was registered"))?`.
#[macro_export]
macro_rules! internal_error {
    ($($fmt:tt)+) => {
        $crate::error::InternalError::at(
            ::std::format!($($fmt)+),
            ::core::file!(),
            ::core::line!(),
            ::core::cfg!(debug_assertions),
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    enum DemoError {
        #[error(transparent)]
        Unimplemented(#[from] Unimplemented),
        #[error(transparent)]
        Internal(#[from] InternalError),
    }

    fn not_yet(n: u32) -> Result<u32, DemoError> {
        if n > 1 {
            unimplemented_feature!("ENG-063", "counting {n} derivations (WP {})", "M6.1");
        }
        Ok(n)
    }

    #[test]
    fn unimplemented_macro_message() {
        assert_eq!(not_yet(1).unwrap(), 1);
        let DemoError::Unimplemented(err) = not_yet(5).unwrap_err() else {
            panic!("expected Unimplemented")
        };
        assert_eq!(err.feature, FeatureId("ENG-063"));
        assert_eq!(&*err.detail, "counting 5 derivations (WP M6.1)");
        assert!(err.file.ends_with("error.rs"), "{}", err.file);
        assert!(err.line > 0 && err.column > 0);
        let msg = err.to_string();
        let expected = format!(
            "not implemented yet: ENG-063 — counting 5 derivations (WP M6.1) (at {}:{}:{})",
            err.file, err.line, err.column
        );
        assert_eq!(msg, expected);
    }

    #[test]
    fn unimplemented_error_expression_form() {
        let err = crate::unimplemented_error!("LANG-221", "the Overlog frontend");
        assert_eq!(err.feature.as_str(), "LANG-221");
        assert_eq!(&*err.detail, "the Overlog frontend");
    }

    #[test]
    fn feature_id_well_formed() {
        for good in ["LANG-024", "BENCH-000", "FLAG-152", "SEM-084", "ENG-1000"] {
            assert!(FeatureId::is_well_formed(good), "{good}");
        }
        for bad in [
            "",
            "LANG",
            "LANG-",
            "LANG-24",
            "LANG-02a",
            "lang-024",
            "ODD-01",
            "CR-14",
            "LANG-12345",
            "XLANG-024",
        ] {
            assert!(!FeatureId::is_well_formed(bad), "{bad}");
        }
    }

    #[test]
    fn feature_id_serde_roundtrip() {
        let id = FeatureId("TEST-029");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"TEST-029\"");
        let back: FeatureId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
        assert!(serde_json::from_str::<FeatureId>("\"NOPE-1\"").is_err());
        assert_eq!(FeatureId::parse("CR-01").unwrap_err().text.as_ref(), "CR-01");
    }

    fn broken_invariant(fail: bool) -> Result<(), DemoError> {
        if fail {
            bug!("stratum {} was scheduled twice", 3);
        }
        Ok(())
    }

    // `bug!` panics in debug builds and returns the error in release builds.
    #[test]
    #[cfg_attr(
        debug_assertions,
        should_panic(expected = "internal error (a bug in Blossom): stratum 3 was scheduled twice")
    )]
    fn bug_macro() {
        assert!(broken_invariant(false).is_ok());
        let DemoError::Internal(err) = broken_invariant(true).unwrap_err() else {
            panic!("expected InternalError")
        };
        assert_eq!(&*err.what, "stratum 3 was scheduled twice");
        assert!(err.file.ends_with("error.rs"));
        assert!(
            err.to_string()
                .starts_with("internal error (a bug in Blossom): stratum 3 was scheduled twice")
        );
    }

    #[test]
    fn bug_macro_panic_on_bug_setting() {
        assert!(panic_on_bug_setting(Some(OsStr::new("1"))));
        assert!(!panic_on_bug_setting(Some(OsStr::new("0"))));
        assert!(!panic_on_bug_setting(Some(OsStr::new("yes"))));
        assert!(!panic_on_bug_setting(None));
    }

    #[test]
    fn bug_macro_release_path_returns_error() {
        // The release-build path of `bug!` (debug_assertions off, BLOSSOM_PANIC_ON_BUG unset) returns the error.
        let err = InternalError::with_policy("release path", "x.rs", 9, false);
        assert_eq!(
            err.to_string(),
            "internal error (a bug in Blossom): release path (at x.rs:9)"
        );
        assert_eq!((&*err.what, err.file, err.line), ("release path", "x.rs", 9));
    }

    #[test]
    #[should_panic(expected = "internal error (a bug in Blossom): loud path (at y.rs:3)")]
    fn bug_macro_panic_path_panics_with_the_message() {
        let _ = InternalError::with_policy("loud path", "y.rs", 3, true);
    }
}
