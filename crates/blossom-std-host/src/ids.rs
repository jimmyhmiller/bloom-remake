//! Host functions of `std::ids` (the Rust side of `std/ids/**.bls`). Implemented by WP M8.4.

use blossom_value::error::ValueError;
use blossom_value::externs::ExternRegistry;

/// Registers the `extern fn`s and `extern table fn`s of `std::ids`. The module declares none yet, so nothing is
/// registered; a program that names an unregistered extern is refused at load time (`ExternRegistry::unbound`).
pub fn register(reg: &mut ExternRegistry) -> Result<(), ValueError> {
    let _ = reg;
    Ok(())
}
