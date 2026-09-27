#![deny(unsafe_op_in_unsafe_fn)]
//! Shared helpers for the cross-crate tests in `tests/integration/tests/<prefix>_*.rs` (PLAN §4 D4).
//!
//! Each WP owns the test files with its prefix (for example `front1_*.rs` for M3.5, `engine1_*.rs` for M6.1).
//! Cargo discovers them automatically, so adding one needs no manifest edit. There are no shared helpers yet.
