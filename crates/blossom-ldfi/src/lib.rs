#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-ldfi`: molly-2 lineage-driven fault injection: failure specs, crash views, the hazard DAG and encodings,
//! minimal enumeration, the forward/backward driver and reports.
//!
//! See ARCHITECTURE §1.2 and §8. Implemented by WP M8.1, M9.4; slice 1 (docs/design/SLICES.md) delivers LDFI for
//! compiled `.ded` programs under Molly's crash view:
//!
//! 1. run the program without faults, recording every firing, and judge it with its outcome spec;
//! 2. turn the run into a provenance graph ([`lineage`]);
//! 3. encode, per `post` goal, "these faults make the goal underivable" as CNF ([`hazard`]) and enumerate its
//!    minimal models with an incremental SAT solver: each is a hypothesis, a superset of the run's own faults;
//! 4. run the hypotheses in order of size ([`driver`]); a run that violates Molly's oracle is a counterexample, a
//!    good run contributes its own lineage and new hypotheses.
//!
//! When no hypothesis is left, the program has no counterexample within the failure spec.

pub mod driver;
pub mod faults;
pub mod hazard;
pub mod lineage;
pub mod reach;
pub mod report;

pub use driver::{LdfiConfig, LdfiReport, Verdict, falsifiers, run};
pub use faults::FailureSpec;

use blossom_base::{InternalError, Unimplemented};

/// Why LDFI could not reach a verdict.
#[derive(Debug, thiserror::Error)]
pub enum LdfiError {
    /// The failure spec is malformed.
    #[error("bad failure spec: {0}")]
    Spec(String),
    /// The program has no `pre` and `post` (BLS0900, CR-30).
    #[error("the program has no outcome spec: define both `pre` and `post` (CR-30)")]
    NoSpec,
    /// A run of the program failed.
    #[error(transparent)]
    Sim(#[from] blossom_sim::SimError),
    #[error(transparent)]
    Sat(#[from] blossom_sat::SatError),
    /// The search exceeded its run budget before reaching a verdict.
    #[error("no verdict within {0} runs")]
    Budget(u64),
    #[error(transparent)]
    Unimplemented(#[from] Unimplemented),
    #[error(transparent)]
    Internal(#[from] InternalError),
}

#[cfg(test)]
mod tests;
