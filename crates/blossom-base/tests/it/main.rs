//! blossom-base as other crates use it: the exported macros expand `$crate::…` paths, which only a separate crate
//! exercises.

use blossom_base::{Diagnostic, FeatureId, Idx, IndexVec, InternalError, Severity, Unimplemented, code};

blossom_base::define_idx! {
    /// An id defined outside blossom-base.
    pub struct WidgetId;
}

#[derive(Debug, thiserror::Error)]
enum WidgetError {
    #[error(transparent)]
    Unimplemented(#[from] Unimplemented),
    #[error(transparent)]
    Internal(#[from] InternalError),
}

fn frobnicate(n: u32) -> Result<u32, WidgetError> {
    if n == 0 {
        blossom_base::unimplemented_feature!("LANG-221", "frobnicating nothing (WP {})", "M15.2");
    }
    Ok(n + 1)
}

#[test]
fn idx_macro_from_another_crate() {
    let mut widgets: IndexVec<WidgetId, &str> = IndexVec::new();
    let id = widgets.push("w").unwrap();
    assert_eq!(id, WidgetId::from_raw(0));
    assert_eq!(<WidgetId as Idx>::try_from_usize(5).unwrap().index(), 5);
    assert_eq!(serde_json::to_string(&id).unwrap(), "0");
    assert_eq!(format!("{id:?}"), "WidgetId(0)");
}

#[test]
fn unimplemented_macro_message_from_another_crate() {
    assert_eq!(frobnicate(1).unwrap(), 2);
    let WidgetError::Unimplemented(err) = frobnicate(0).unwrap_err() else {
        panic!("expected Unimplemented")
    };
    assert_eq!(err.feature, FeatureId("LANG-221"));
    assert!(err.file.ends_with("main.rs"), "{}", err.file);
    assert!(
        err.to_string()
            .starts_with("not implemented yet: LANG-221 — frobnicating nothing (WP M15.2) (at ")
    );
    let d = Diagnostic::from_unimplemented(&err, "main.olg");
    assert_eq!(d.code.as_str(), "BLS0908");
}

#[test]
fn code_macro_from_another_crate() {
    let d = Diagnostic::new(code!("BLS0502"), "a negative edge on a same-tick cycle");
    assert_eq!(d.severity, Severity::Error);
    assert_eq!(d.code.info().owner_crate, "blossom-analysis");
}

fn checked(n: u32) -> Result<u32, WidgetError> {
    if n > 10 {
        blossom_base::bug!("{n} passed validation");
    }
    Ok(n)
}

#[test]
#[cfg_attr(debug_assertions, should_panic(expected = "11 passed validation"))]
fn bug_macro_from_another_crate() {
    assert_eq!(checked(3).unwrap(), 3);
    let WidgetError::Internal(err) = checked(11).unwrap_err() else {
        panic!("expected InternalError")
    };
    assert!(err.file.ends_with("main.rs"));
    let expr: InternalError = blossom_base::internal_error!("expression form {}", 1);
    assert_eq!(&*expr.what, "expression form 1");
}
