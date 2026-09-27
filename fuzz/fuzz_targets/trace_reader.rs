//! Fuzz target `trace_reader` (ARCHITECTURE §11.8): the trace reader: no panic; typed errors.
//!
//! Implemented by WP M4.7, together with its stable proptest mirror. Until then running it is an immediate
//! finding, so the target can never pass silently.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = data;
    panic!("fuzz target trace_reader is not implemented yet (WP M4.7)");
});
