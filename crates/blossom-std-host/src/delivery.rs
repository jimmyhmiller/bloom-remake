//! Host functions of `std::delivery` (the Rust side of `std/delivery/**.bls`). Implemented by WP M8.3, M9.6.

use blossom_value::error::ValueError;
use blossom_value::externs::ExternRegistry;

/// Registers the `extern fn`s and `extern table fn`s of `std::delivery`. The module declares none yet, so nothing is
/// registered; a program that names an unregistered extern is refused at load time (`ExternRegistry::unbound`).
pub fn register(reg: &mut ExternRegistry) -> Result<(), ValueError> {
    let _ = reg;
    Ok(())
}
