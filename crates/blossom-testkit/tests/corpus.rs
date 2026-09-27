//! The golden-corpus runner (BENCH-000). Implemented by WP M5.2, which runs every `tests/corpus/**/manifest.toml`
//! case as an individually filterable test with the status ratchet of PLAN §5.3.
//!
//! Until then this harness reports zero tests: no case is run here, so none is reported as passing.

fn main() {
    let args = libtest_mimic::Arguments::from_args();
    libtest_mimic::run(&args, Vec::new()).exit();
}
