#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-syntax`: the lexer, the lossless CST, the error-recovering parser, typed AST views, the formatter and
//! editions; the Molly `.ded` lexer and parser.
//!
//! See ARCHITECTURE §1.2. Implemented by WP M2.3, M3.7 (the `.ded` modules: M3.6; the P2 frontends: M15.2); until
//! then this crate is a placeholder that exposes nothing (PLAN §4 D1).
//!
//! The modules are declared up front because several WPs share this crate; each owns its own modules
//! (PLAN §4 D6).

pub mod ast;
pub mod bloom;
pub mod ded;
pub mod fmt;
pub mod hydro;
pub mod lexer;
pub mod overlog;
pub mod parser;
