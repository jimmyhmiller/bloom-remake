//! Host functions of `std::gc` (the Rust side of `std/gc/**.bls`). Implemented by WP M9.7.

use blossom_value::error::ValueError;
use blossom_value::externs::ExternRegistry;

/// Registers the `extern fn`s and `extern table fn`s of `std::gc`. The module declares none yet, so nothing is
/// registered; a program that names an unregistered extern is refused at load time (`ExternRegistry::unbound`).
pub fn register(reg: &mut ExternRegistry) -> Result<(), ValueError> {
    let _ = reg;
    Ok(())
}
