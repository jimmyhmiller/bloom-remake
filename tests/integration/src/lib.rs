#![deny(unsafe_op_in_unsafe_fn)]
//! Shared helpers for the cross-crate tests in `tests/integration/tests/<prefix>_*.rs` (PLAN §4 D4).
//!
//! Each WP owns the test files with its prefix (for example `front1_*.rs` for M3.5, `engine1_*.rs` for M6.1).
//! Cargo discovers them automatically, so adding one needs no manifest edit.

pub mod raft_safety;

/// Whether this is the full test tier (`BLOSSOM_FULL=1`): every seed of a multi-seed simulation, and the tests marked
/// `#[ignore = "full tier"]` (run with `-- --include-ignored`). Otherwise the fast tier, for every change: one seed.
/// `scripts/test-tiers.sh` runs either.
pub fn full_tier() -> bool {
    std::env::var("BLOSSOM_FULL").is_ok_and(|v| v != "0" && !v.is_empty())
}

/// The seeds a multi-seed simulation runs: `all` in the full tier, its first otherwise.
pub fn seeds(all: std::ops::RangeInclusive<u64>) -> std::ops::RangeInclusive<u64> {
    if full_tier() { all } else { *all.start()..=*all.start() }
}

/// [`seeds`] for a half-open range.
pub fn seeds_of(all: std::ops::Range<u64>) -> std::ops::Range<u64> {
    if full_tier() || all.is_empty() {
        all
    } else {
        all.start..all.start + 1
    }
}

/// [`scaled`] for a half-open range of seeds ([`seeds_of`]).
pub fn scaled_of(total: usize, all: &std::ops::Range<u64>) -> usize {
    let every = all.clone().count().max(1);
    let run = seeds_of(all.clone()).count();
    total * run / every
}

/// A threshold a test asserts over every seed of `all`, scaled to the seeds that run ([`seeds`]): the share of the
/// seeds that run, rounded down.
pub fn scaled(total: usize, all: &std::ops::RangeInclusive<u64>) -> usize {
    let every = all.clone().count().max(1);
    let run = seeds(all.clone()).count();
    total * run / every
}

use blossom_artifact::bls::BlsArtifact;
use blossom_base::RelId;
use blossom_ir::tick::Row;
use blossom_value::Value;
use blossom_value::time::NodeId;
use blossom_value::value::IntValue;

/// The deployment rows of the Kafka programs' `broker` static (docs/plan/notes/S8.md D2): every node named `b<N>` is
/// broker id N, advertised at `b<N>.sim:9092`. `None` when the program has no `broker` relation or a node's name has
/// no id.
pub fn kafka_brokers(artifact: &BlsArtifact) -> Option<Vec<(RelId, Row)>> {
    let rel = artifact.rel_named("broker")?;
    let mut rows = Vec::new();
    for (i, name) in artifact.nodes.iter().enumerate() {
        let Some(digits) = name.as_str().strip_prefix('b') else {
            continue;
        };
        let id: i32 = digits.parse().ok()?;
        let node = NodeId(u32::try_from(i).ok()?);
        let row: Row = std::sync::Arc::from(vec![
            Value::Node(node),
            Value::Int(IntValue::I32(id)),
            Value::Str(format!("{name}.sim").into()),
            Value::Int(IntValue::I32(9092)),
        ]);
        rows.push((rel, row));
    }
    Some(rows)
}
