//! The value crate's error type (ARCHITECTURE §12.1).

use std::sync::Arc;

use blossom_base::idx::IdxOverflow;
use blossom_base::{InternalError, TypeId, Unimplemented};

/// Errors from types, values, encodings, stores and the extern registry.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ValueError {
    /// A [`TypeId`] that is not in the table.
    #[error("unknown type {0:?}")]
    UnknownType(TypeId),
    /// A type definition that violates a structural rule (duplicate field names, an out-of-range `Mod` width, …).
    #[error("ill-formed type: {0}")]
    InvalidType(String),
    /// A value that does not have the shape of its type.
    #[error("value does not conform to type {ty:?}: {reason}")]
    TypeMismatch {
        /// The expected type.
        ty: TypeId,
        /// What does not match.
        reason: String,
    },
    /// A value that violates its own invariant (for example a `Mod` value with bits above its width).
    #[error("invalid value: {0}")]
    InvalidValue(String),
    /// Two host functions registered under one path.
    #[error("host function `{0}` is registered twice")]
    DuplicateExtern(Arc<str>),
    /// A host function lacks a typed binding or its registered type disagrees with the source declaration.
    #[error("extern `{path}` signature mismatch: {reason}")]
    ExternSignature { path: Arc<str>, reason: String },
    /// More types than a [`TypeId`] can number.
    #[error(transparent)]
    TooManyTypes(#[from] IdxOverflow),
    /// A feature that is not implemented yet.
    #[error(transparent)]
    Unimplemented(#[from] Unimplemented),
    /// A violated internal invariant.
    #[error(transparent)]
    Internal(#[from] InternalError),
}
