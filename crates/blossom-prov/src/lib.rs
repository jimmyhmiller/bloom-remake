#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-prov`: provenance graphs from Tier C logs and Tier B annotations, `why`/`whynot`, the Nemo graph algebra,
//! semiring annotations and rendering.
//!
//! See ARCHITECTURE §1.2 and §8.2. Implemented by WP M5.7, M7.6; slice 1 (docs/design/SLICES.md) delivers the
//! provenance graph of a simulated run ([`graph`], TEST-023) and its rendering. The rest is a placeholder.

pub mod graph;

pub use graph::{
    Firing, FiringId, Goal, GoalId, GoalKey, Loc, Names, NegId, NegRead, Premise, ProvGraph, Space, Support,
};
