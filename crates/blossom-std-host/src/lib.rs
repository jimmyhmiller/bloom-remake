#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-std-host`: the Rust implementations behind the standard library's `extern fn`s and
//! `extern table fn`s, as an [`ExternRegistry`] (ARCHITECTURE §1.2, §1.5).
//!
//! One module per standard-library area; each is owned by the WP that implements that area, and until then its
//! `register` adds nothing. This file is a dispatch file, frozen after M1 (PLAN §2.4): adding an area is a plan
//! change. The implementing WPs (plan.json):
//!
//! | WP | Areas |
//! |---|---|
//! | M8.3 | `delivery`, `bcast`, `membership`, `fd`, `timers`, `vote`, `commit`, `quorum`, `coord` |
//! | M8.4 | `ids`, `queue`, `seq`, `clock`, `seal`, `kvs`, `crdt` |
//! | M9.1 | `consensus` |
//! | M9.6 | `election`, `lease`, `lock` |
//! | M9.7 | `gc`, `zset`, `authz` |
//! | M10.7 | `examples` |
//! | M11.3 | `push` |
//! | M12.3 | `upgrade` |
//! | M15.4 | `crypto`, `oncetree` |
//! | M15.7 | `pipeline` |

use blossom_value::error::ValueError;
use blossom_value::externs::ExternRegistry;

pub mod authz;
pub mod bcast;
pub mod clock;
pub mod commit;
pub mod consensus;
pub mod coord;
pub mod crdt;
pub mod crypto;
pub mod delivery;
pub mod election;
pub mod examples;
pub mod fd;
pub mod gc;
pub mod ids;
pub mod kvs;
pub mod lease;
pub mod lock;
pub mod membership;
pub mod oncetree;
pub mod pipeline;
pub mod push;
pub mod queue;
pub mod quorum;
pub mod seal;
pub mod seq;
pub mod timers;
pub mod upgrade;
pub mod vote;
pub mod zset;

/// Registers every standard-library host function into `reg`.
pub fn register_all(reg: &mut ExternRegistry) -> Result<(), ValueError> {
    delivery::register(reg)?;
    bcast::register(reg)?;
    membership::register(reg)?;
    fd::register(reg)?;
    timers::register(reg)?;
    election::register(reg)?;
    lease::register(reg)?;
    vote::register(reg)?;
    commit::register(reg)?;
    lock::register(reg)?;
    quorum::register(reg)?;
    coord::register(reg)?;
    consensus::register(reg)?;
    ids::register(reg)?;
    queue::register(reg)?;
    seq::register(reg)?;
    clock::register(reg)?;
    seal::register(reg)?;
    kvs::register(reg)?;
    crdt::register(reg)?;
    gc::register(reg)?;
    zset::register(reg)?;
    examples::register(reg)?;
    authz::register(reg)?;
    upgrade::register(reg)?;
    crypto::register(reg)?;
    push::register(reg)?;
    oncetree::register(reg)?;
    pipeline::register(reg)?;
    Ok(())
}

/// The registry of every standard-library host function. Fails only if two modules register the same path.
pub fn registry() -> Result<ExternRegistry, ValueError> {
    let mut reg = ExternRegistry::new();
    register_all(&mut reg)?;
    Ok(reg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_registers_every_path_once() {
        // Building the registry fails with DuplicateExtern if two areas register one path.
        let reg = registry().unwrap();
        let paths = reg.paths();
        let mut distinct = paths.clone();
        distinct.dedup();
        assert_eq!(paths, distinct);
        assert_eq!(paths.len(), reg.len());
        // Registering everything again is refused exactly when something was registered: no area registers a
        // path silently twice.
        let mut again = registry().unwrap();
        assert_eq!(register_all(&mut again).is_err(), !reg.is_empty());
    }
}
