//! Structural IR validation failures retain their rule and source location.
use blossom_base::{InternalError, Span, Unimplemented, idx::RuleId};
/// A small error handle; detailed context lives off the success path.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct IrError(pub Box<IrErrorKind>);
/// An invalid program or an internal builder failure.
#[derive(Debug, thiserror::Error)]
pub enum IrErrorKind {
    /// A numbered validator invariant failed.
    #[error("V{invariant}: {detail}")]
    Validation {
        invariant: u8,
        rule: Option<RuleId>,
        span: Option<Span>,
        detail: String,
    },
    /// Builder declaration is inconsistent.
    #[error("IR builder: {0}")]
    Builder(String),
    /// A later feature is unavailable.
    #[error(transparent)]
    Unimplemented(#[from] Unimplemented),
    /// An internal invariant failed.
    #[error(transparent)]
    Internal(#[from] InternalError),
}
impl IrError {
    pub(crate) fn validation(
        invariant: u8,
        rule: Option<RuleId>,
        span: Option<Span>,
        detail: impl Into<String>,
    ) -> Self {
        Self(Box::new(IrErrorKind::Validation {
            invariant,
            rule,
            span,
            detail: detail.into(),
        }))
    }
    pub(crate) fn builder(detail: impl Into<String>) -> Self {
        Self(Box::new(IrErrorKind::Builder(detail.into())))
    }
    /// Which numbered validation invariant failed, if applicable.
    pub fn invariant(&self) -> Option<u8> {
        match &*self.0 {
            IrErrorKind::Validation { invariant, .. } => Some(*invariant),
            _ => None,
        }
    }
}
impl From<InternalError> for IrError {
    fn from(e: InternalError) -> Self {
        Self(Box::new(IrErrorKind::Internal(e)))
    }
}
impl From<Unimplemented> for IrError {
    fn from(e: Unimplemented) -> Self {
        Self(Box::new(IrErrorKind::Unimplemented(e)))
    }
}
