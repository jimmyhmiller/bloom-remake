//! Host functions of `std::lease` (the Rust side of `std/lease/**.bls`). Implemented by WP M9.6.

use blossom_value::error::ValueError;
use blossom_value::externs::ExternRegistry;

/// Registers the `extern fn`s and `extern table fn`s of `std::lease`. The module declares none yet, so nothing is
/// registered; a program that names an unregistered extern is refused at load time (`ExternRegistry::unbound`).
pub fn register(reg: &mut ExternRegistry) -> Result<(), ValueError> {
    let _ = reg;
    Ok(())
}
