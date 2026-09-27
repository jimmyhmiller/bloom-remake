//! Fuzz target `front` (ARCHITECTURE §11.8): resolve + typeck + lower: no panic; `Err(diagnostics)` or a program that passes the validator.
//!
//! Implemented by WP M13.3, together with its stable proptest mirror. Until then running it is an immediate
//! finding, so the target can never pass silently.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = data;
    panic!("fuzz target front is not implemented yet (WP M13.3)");
});
