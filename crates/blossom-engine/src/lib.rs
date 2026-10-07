#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-engine`: the single-node evaluator that runs programs: incremental and indexed.
//!
//! See ARCHITECTURE §1.2, §3.4 and §4. Slice 5 (docs/plan/notes/S5.md) delivers a value-level engine: every relation
//! lives across ticks in an indexed store with counted support ([`store`]), and a tick costs work in proportion to
//! what changed — rules are maintained by delta queries over the changes of what they read ([`rule`]), aggregates and
//! lattice cells per touched group and cell, recursive strata and rules reading time-varying scalars by
//! re-evaluation and diffing. It shares no evaluation code with the reference oracle (ARCH-16): the differential suite
//! compares the two at every tick. The word-level kernel, the planner's physical plans and generated code are later
//! work.

mod cold;
mod engine;
mod expr;
mod func;
mod rule;
mod store;
mod strata;

pub use cold::{ColRange, ColdTables};
pub use engine::{Engine, EngineConfig};

