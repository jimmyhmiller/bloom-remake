//! SMT-LIB2 responses must parse without panicking or return a typed error.
#![no_main]
use blossom_smt::Sexp;
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(parsed) = Sexp::parse(text) {
            let printed = parsed.to_string();
            assert_eq!(Sexp::parse(&printed).ok(), Some(parsed));
        }
    }
});
