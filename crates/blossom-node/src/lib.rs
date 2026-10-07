#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-node`: the sans-IO node state machine: the `Evaluator` trait, admission, timers, seeds and incarnations,
//! Invariant R, outbox release, sessions and probation.
//!
//! See ARCHITECTURE §1.2 and §5.1. Slice 3 (docs/design/SLICES.md) delivers what a node on the network needs to run a
//! Blossom program: the value-level [`Evaluator`] seam (the oracle until the engine exists), the [`Node`] with its
//! timers, tick reservation and Invariant R, durable state as WAL records and checkpoints ([`durable`]), recovery
//! ([`recovery`]) and ACL admission ([`acl`]). Probation, poison isolation, subscriptions and host services are later
//! slices.

pub mod acl;
pub mod durable;
pub mod env;
pub mod eval;
pub mod keycode;
pub mod manual;
pub mod node;
pub mod record;
pub mod recovery;
pub mod streams;
pub mod timers;

use blossom_base::{InternalError, Unimplemented};
use blossom_oracle::OracleError;
use blossom_store::StoreError;
use blossom_value::time::Tick;
use blossom_wire::codec::WireError;

pub use eval::{Backend, EngineEvaluator, Evaluator, Executor, Executors, OracleExecutor};
pub use node::{Boot, Node, NodeConfig, NodeState, ReleasedTick, TickEffects};

/// Why a node operation failed.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    /// The store refused to open, or its contents do not fit this deployment.
    #[error("{0}")]
    Store(String),
    #[error(transparent)]
    StoreIo(#[from] StoreError),
    /// Durable state that this build's schema cannot read.
    #[error("{0}")]
    Schema(String),
    #[error("encoding: {0}")]
    Wire(#[from] WireError),
    /// The evaluator failed: a program error (BLSRnnn) or an evaluator error.
    #[error(transparent)]
    Eval(#[from] OracleError),
    #[error(transparent)]
    Unimplemented(#[from] Unimplemented),
    #[error(transparent)]
    Internal(#[from] InternalError),
}

impl From<blossom_ir::timers::TimerError> for NodeError {
    fn from(e: blossom_ir::timers::TimerError) -> NodeError {
        match e {
            blossom_ir::timers::TimerError::Unimplemented(u) => NodeError::Unimplemented(u),
            blossom_ir::timers::TimerError::Internal(i) => NodeError::Internal(i),
        }
    }
}

/// A tick that failed: it commits and releases nothing, and the node is faulted.
#[derive(Debug, thiserror::Error)]
#[error("tick {} failed: {error}", .tick.0)]
pub struct NodeFault {
    pub tick: Tick,
    pub error: NodeError,
}
