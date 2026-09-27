//! Fuzz target `admission` (ARCHITECTURE §11.8): the ingress admission pipeline: no panic; typed errors.
//!
//! Implemented by WP M13.3, together with its stable proptest mirror. Until then running it is an immediate
//! finding, so the target can never pass silently.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = data;
    panic!("fuzz target admission is not implemented yet (WP M13.3)");
});
