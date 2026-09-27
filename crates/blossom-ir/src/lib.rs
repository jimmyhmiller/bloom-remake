#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-ir`: the Dedalus^L core IR, the physical plan IR, strata, observation records, `IrBuilder`,
//! `ValidatedProgram` and the validator, the IR printer, canonical digests and fixture programs.
//!
//! See ARCHITECTURE §1.2. Implemented by WP M2.2, M3.2; until then this crate is a placeholder that exposes nothing
//! (PLAN §4 D1).

pub mod core;

pub mod build;
pub mod error;
mod program;
mod validate;
mod visit;
pub use error::IrError;
pub use program::{ProgramDigest, ValidatedProgram};

pub mod obs;
pub mod plan;
pub mod spec;
pub mod strata;

mod canonical;

pub mod printer;

#[cfg(test)]
mod tests;

mod projection;

/// Version of the core IR and plan data contract.
pub const API_VERSION: u32 = 1;
