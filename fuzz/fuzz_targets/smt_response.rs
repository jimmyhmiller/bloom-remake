//! Fuzz target `smt_response` (ARCHITECTURE §11.8): the SMT-LIB2 response parser: no panic; typed errors.
//!
//! Implemented by WP M2.5, together with its stable proptest mirror. Until then running it is an immediate
//! finding, so the target can never pass silently.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = data;
    panic!("fuzz target smt_response is not implemented yet (WP M2.5)");
});
