#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-verify`: bounded model checking, the ASP bounded encoding, first-order transition systems, EPR, inductive
//! invariants, law proofs, confluence certificates and trusted-module checks.
//!
//! See ARCHITECTURE §1.2. S20 brings the law harness ([`laws`], TEST-083); the rest is implemented by WP M9.5, M10.4.

pub mod laws;
