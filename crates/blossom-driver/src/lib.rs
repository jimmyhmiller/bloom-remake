#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-driver`: the compilation pipeline (`CompileSession`), artifact caching by digest, diagnostic rendering
//! and certificate output.
//!
//! See ARCHITECTURE §1.2. Implemented by WP M6.3; slice 1 (docs/design/SLICES.md) delivers compiling `.ded` files
//! from disk ([`ded`]) and rendering diagnostics against their sources ([`render`]). The rest is a placeholder.

pub mod ded;
pub mod render;
