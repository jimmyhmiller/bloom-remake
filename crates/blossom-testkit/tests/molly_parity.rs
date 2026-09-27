//! The Molly verdict-parity runner (TEST-020–040, BENCH-130–137). Implemented by WP M8.1.
//!
//! Until then this harness reports zero tests: no parity case is run here, so none is reported as passing.

fn main() {
    let args = libtest_mimic::Arguments::from_args();
    libtest_mimic::run(&args, Vec::new()).exit();
}
