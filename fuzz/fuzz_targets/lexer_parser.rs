//! Fuzz target `lexer_parser` (ARCHITECTURE §11.8): the lexer and parser: no panic; `print(parse(s)) == s` (lossless CST); every error has a span.
//!
//! Implemented by WP M2.3, together with its stable proptest mirror. Until then running it is an immediate
//! finding, so the target can never pass silently.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = data;
    panic!("fuzz target lexer_parser is not implemented yet (WP M2.3)");
});
