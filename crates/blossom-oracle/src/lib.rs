#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-oracle`: the naive per-tick Dedalus^L evaluator over `Value` with its own stratifier, and the choice-
//! validity checker. Deliberately independent of the kernel, the engine, the planner and the analyses.
//!
//! See ARCHITECTURE §1.2. Implemented by WP M4.1; until then this crate is a placeholder that exposes nothing (PLAN
//! §4 D1).
