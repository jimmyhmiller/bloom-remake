//! Fuzz target `formatter` (ARCHITECTURE §11.8): the formatter: idempotent; preserves the CST modulo trivia; never reorders items.
//!
//! Implemented by WP M3.7, together with its stable proptest mirror. Until then running it is an immediate
//! finding, so the target can never pass silently.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = data;
    panic!("fuzz target formatter is not implemented yet (WP M3.7)");
});
