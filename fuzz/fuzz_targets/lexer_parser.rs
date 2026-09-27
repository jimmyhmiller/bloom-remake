//! Lossless parser fuzz target (ARCHITECTURE §11.8).
#![no_main]
use blossom_base::FileId;
use blossom_syntax::parser::parse;
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if let Ok(source) = std::str::from_utf8(data) {
        let parsed = parse(FileId::from_raw(0), source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.iter().all(|e| e.primary.is_some()));
    }
});
