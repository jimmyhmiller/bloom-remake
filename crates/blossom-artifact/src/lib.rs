#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-artifact`: compiler outputs as data: `CompileOutput`, role and spec artifacts, certificate records and
//! the artifact header.
//!
//! See ARCHITECTURE §1.2. Implemented by WP M3.4; [`ded`] (the compiled form of a Molly `.ded` program) by slice 1
//! (docs/design/SLICES.md). The rest of the crate is a placeholder that exposes nothing (PLAN §4 D1).

pub mod bls;
pub mod client;
pub mod sim;
