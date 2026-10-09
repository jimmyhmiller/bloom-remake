#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-runtime`: the production driver: engine threads, tokio I/O, TCP/TLS and QUIC transports, committer and
//! checkpoint threads, the ops listener, the admin plane, configuration and the host embedding API.
//!
//! See ARCHITECTURE §1.2 and §5.2. Slice 3 (docs/design/SLICES.md) delivers a node on the network: the deployment
//! spec ([`deploy`]), the driver for one node with its engine, committer and checkpoint threads over plaintext TCP
//! ([`server`], `--insecure-dev` only), and a client session library ([`client`]). TLS, QUIC, the ops listener and
//! the admin plane are later slices.

mod admin;
pub mod client;
pub mod clock;
pub mod db;
pub mod deploy;
pub mod hosting;
pub mod http_link;
pub mod keyed;
pub mod members;
pub mod net;
pub mod object;
pub mod query;
pub mod server;
pub mod stateless;
pub mod streams;
pub mod web;

use blossom_base::{InternalError, Unimplemented};

/// Why the runtime failed.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// The configuration is invalid (refuse to start).
    #[error("{0}")]
    Config(String),
    /// The store refused to open or recover (refuse to start).
    #[error(transparent)]
    Node(#[from] blossom_node::NodeError),
    /// A tick or the commit pipeline failed; restart from durable state.
    #[error("runtime fault: {0}")]
    Fault(String),
    #[error("network: {0}")]
    Net(String),
    #[error("i/o: {0}")]
    Io(std::io::Error),
    #[error(transparent)]
    Store(#[from] blossom_store::StoreError),
    #[error(transparent)]
    Wire(#[from] blossom_wire::codec::WireError),
    #[error(transparent)]
    Eval(#[from] blossom_oracle::OracleError),
    #[error(transparent)]
    Unimplemented(#[from] Unimplemented),
    #[error(transparent)]
    Internal(#[from] InternalError),
}
