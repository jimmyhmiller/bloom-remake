//! Fuzz target `wal_recovery` (ARCHITECTURE §11.8): WAL and checkpoint recovery: recovers an acknowledged prefix or refuses with a typed error; never panics.
//!
//! Implemented by WP M2.6, together with its stable proptest mirror. Until then running it is an immediate
//! finding, so the target can never pass silently.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = data;
    panic!("fuzz target wal_recovery is not implemented yet (WP M2.6)");
});
