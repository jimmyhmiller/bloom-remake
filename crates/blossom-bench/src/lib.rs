#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-bench`: benchmarks and baselines.
//!
//! See ARCHITECTURE §1.2. Slice 3 (docs/design/SLICES.md) delivers the key-value workload ([`kv`]): closed-loop
//! clients that record a history for the linearizability checker and latencies for the benchmark, against a Blossom
//! node running e01 ([`blossom_kv`]). The engine benchmarks are later slices.

pub mod blossom_kv;
pub mod kv;
pub mod stopwatch;
