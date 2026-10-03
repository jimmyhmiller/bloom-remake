#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-trace`: the observation vocabulary and the record/replay trace format.
//!
//! See ARCHITECTURE §1.2, §6.4. [`node`] is one node's input trace, which `blossom run --record` writes and
//! `blossom trace` replays. The scheduler-level trace of the asynchronous simulator (WP M4.7) is not here yet.

pub mod node;
