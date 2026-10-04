#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-sim`: the deterministic simulator: worlds, schedulers, fault injection, exploration, shrinking, replay,
//! space-time diagrams, history checkers and the spec engine driver.
//!
//! See ARCHITECTURE §1.2 and §6. Implemented by WP M7.2, M8.2; slice 1 (docs/design/SLICES.md) delivers the
//! synchronous-round world ([`sync`], TEST-006) over an [`Evaluator`], and the `.ded` profile ([`ded`]): running a
//! compiled Molly program under a fault schedule and judging it with its outcome spec. Everything else is a
//! placeholder that exposes nothing (PLAN §4 D1).

pub mod bls;
pub mod cluster;
pub mod fabric;
pub mod linearize;
pub mod replay;
pub mod runtime;
pub mod spec;
pub mod sync;

pub use sync::{
    CrashView, Evaluator, Fate, FaultSchedule, MessageRecord, NodeTick, Omission, SimError, SyncConfig, SyncRun,
    SyncWorld,
};
