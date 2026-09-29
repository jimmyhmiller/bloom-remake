#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-analysis`: every analysis that produces facts, certificates or diagnostics: stratification, locality,
//! polarity/CALM, Blazes, finality, FDs, determinism, streams, key conflicts, ACLs, version compatibility and
//! schedule branching.
//!
//! See ARCHITECTURE §1.2. Implemented by WP M4.2, M5.6, M6.6; until then this crate is a placeholder that exposes
//! nothing (PLAN §4 D1).

pub mod acl;
pub mod monotone;
pub mod strata;
