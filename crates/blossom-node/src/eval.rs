//! The `Evaluator` seam (ARCHITECTURE §5.1): what runs one node's tick.
//!
//! The architecture's evaluator is word-level (the engine ingests encoded batches). Until the engine exists it is
//! value-level: a tick reads a [`TickInput`] and produces a [`TickOutput`], which is exactly the oracle's interface.
//! The node, the simulator and (later) the fast interpreter all sit behind this trait.

use blossom_oracle::{Oracle, OracleError, TickInput, TickOutput};

/// Runs one node's tick.
pub trait Evaluator: Send + Sync {
    fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, OracleError>;
}

impl Evaluator for Oracle {
    fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, OracleError> {
        Oracle::tick(self, input)
    }
}

impl<E: Evaluator + ?Sized> Evaluator for std::sync::Arc<E> {
    fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, OracleError> {
        (**self).tick(input)
    }
}

impl<E: Evaluator + ?Sized> Evaluator for &E {
    fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, OracleError> {
        (**self).tick(input)
    }
}
