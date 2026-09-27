#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-runtime`: the production driver: engine threads, tokio I/O, TCP/TLS and QUIC transports, committer and
//! checkpoint threads, the ops listener, the admin plane, configuration and the host embedding API.
//!
//! See ARCHITECTURE §1.2. Implemented by WP M7.4, M8.7, M9.8, M11.5; until then this crate is a placeholder that
//! exposes nothing (PLAN §4 D1).
