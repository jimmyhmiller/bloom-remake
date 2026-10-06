//! Physical timers (ARCHITECTURE §5.5, LANGUAGE §15.2): the table lives in `blossom_ir::timers`, shared with the
//! other hosts that run a program on a clock.

pub use blossom_ir::timers::{TimerError, TimerTable};
