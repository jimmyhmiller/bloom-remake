#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-store`: the `Vfs` with `RealFs` and `SimFs`, the WAL, checkpoints, `MetaStore`, store identity and lock,
//! recovery and migrations ordering, and store tooling.
//!
//! See ARCHITECTURE §1.2. Implemented by WP M2.6, M5.4; until then this crate is a placeholder that exposes nothing
//! (PLAN §4 D1).
