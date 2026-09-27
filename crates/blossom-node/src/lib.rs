#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-node`: the sans-IO node state machine: the `Evaluator` trait, admission, timers, seeds and incarnations,
//! Invariant R, outbox release, sessions and probation.
//!
//! See ARCHITECTURE §1.2. Implemented by WP M5.5, M7.2, M8.2, M9.8, M11.3; until then this crate is a placeholder
//! that exposes nothing (PLAN §4 D1).
