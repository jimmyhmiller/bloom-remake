#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-front`: module loading, name resolution, instantiation, role placement, type checking, event
//! classification, the HIR, lowering to the IR, specs and the schema lock.
//!
//! See ARCHITECTURE §1.2. Implemented by WP M3.5, M4.5, M5.3, M6.4, M6.5 (the `.ded` frontend: M3.6; the P2
//! frontends: M15.2); until then this crate is a placeholder that exposes nothing (PLAN §4 D1).
//!
//! The modules are declared up front because several WPs share this crate; each owns its own modules
//! (PLAN §4 D6).

pub mod api;
pub mod ast;
pub mod bloom;
pub mod classify;
pub mod ded;
pub mod hir;
pub mod hydro;
pub mod instantiate;
pub mod items;
pub mod lock;
pub mod lower;
pub mod modules;
pub mod overlog;
pub mod resolve;
pub mod roles;
pub mod spec;
pub mod typeck;
