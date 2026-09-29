//! The node's environment (ARCHITECTURE §5.5): where time and entropy come from. Implemented by `blossom-runtime`
//! (real) and the simulator (virtual); nothing else reads ambient time or randomness.

use blossom_value::time::Instant;

/// A clock that never goes backwards within an incarnation.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// Fresh entropy (DIST-033): boot nonces. Never the deployment seed.
pub trait Entropy: Send {
    fn boot_nonce(&mut self) -> Result<u64, String>;
}
