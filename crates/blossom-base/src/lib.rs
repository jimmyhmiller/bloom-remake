#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-base`: the foundation every Blossom crate builds on (ARCHITECTURE §1.2, §2.1, §12.1, Appendix B).
//!
//! - [`idx`] — dense `u32` id newtypes ([`define_idx!`]), [`IndexVec`], and every id type of ARCHITECTURE §2.1;
//! - [`span`] — [`Span`], [`FileId`], [`SourceDb`], the [`Symbol`] interner, [`QualName`] and [`RuleLabel`];
//! - [`diag`] — [`Diagnostic`] and the [`Diagnostics`] collection;
//! - [`error`] — [`FeatureId`], [`Unimplemented`], [`InternalError`], [`unimplemented_feature!`] and [`bug!`];
//! - [`codes`] — the registry of every `BLSnnnn`/`BLSRnnn` code ([`codes::REGISTRY`]) and the [`code!`] macro;
//! - [`det`] — [`DetMap`]/[`DetSet`], the only hashed collections, with the keyed deterministic hasher;
//! - [`graph`] — Tarjan SCC, topological sort, condensation, shortest witness cycles, Hopcroft–Karp and chain covers.
//!
//! Implemented by WP M1.1. The crate is frozen after milestone M1: changing a public item needs an ARCHITECTURE
//! amendment, a DECISIONS.md line and an [`API_VERSION`] bump in one commit (ARCHITECTURE §1.6).

pub mod codes;
pub mod det;
pub mod diag;
pub mod error;
pub mod graph;
pub mod idx;
pub mod span;

pub use codes::{Code, CodeInfo, CodeOrigin};
pub use det::{DetMap, DetSet, DetState};
pub use diag::{Diagnostic, Diagnostics, FixIt, Label, Severity, TextEdit};
pub use error::{FeatureId, InternalError, Unimplemented};
pub use idx::{
    AggTableId, BufferId, ColIdx, ConstId, ConstructId, FileId, FiringId, FnId, GoalId, GroupTypeId, Idx, IdxOverflow,
    IndexId, IndexVec, InvariantId, LatticeTypeId, NativeId, OccId, OpId, ParamId, RelId, RoleId, RuleId, ServiceId,
    SiteId, StratumId, TypeId, UdaId, VarId,
};
pub use span::{LineCol, QualName, RuleLabel, SourceDb, SourceError, Span, Symbol};

/// The version of this crate's public API (ARCHITECTURE §1.6). Bumped together with an ARCHITECTURE amendment.
pub const API_VERSION: u32 = 1;

/// Re-exports used by this crate's macros. Not part of the public API.
#[doc(hidden)]
pub mod __private {
    pub use serde;
}
