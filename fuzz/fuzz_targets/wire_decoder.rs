//! Fuzz target `wire_decoder` (ARCHITECTURE §11.8): the wire decoder: no panic under `WireLimits`; `decode(encode(t)) == t`; unknown fields and variants round-trip.
//!
//! Implemented by WP M4.4, together with its stable proptest mirror. Until then running it is an immediate
//! finding, so the target can never pass silently.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = data;
    panic!("fuzz target wire_decoder is not implemented yet (WP M4.4)");
});
