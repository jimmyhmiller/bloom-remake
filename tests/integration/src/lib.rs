#![deny(unsafe_op_in_unsafe_fn)]
//! Shared helpers for the cross-crate tests in `tests/integration/tests/<prefix>_*.rs` (PLAN §4 D4).
//!
//! Each WP owns the test files with its prefix (for example `front1_*.rs` for M3.5, `engine1_*.rs` for M6.1).
//! Cargo discovers them automatically, so adding one needs no manifest edit. There are no shared helpers yet.

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
