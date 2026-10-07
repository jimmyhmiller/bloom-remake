//! Slice 7, item 2: deletion at scale (FOREIGN-PROTOCOLS §6). Deleting rows from a durable table costs in proportion
//! to the rows deleted, whether by key or by a range sweep (retention), however many rows the table keeps. Counted
//! in rows the engine examined, not time, so the check is exact and machine-independent.

use std::path::Path;
use std::sync::Arc;

use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_ir::tick::StepInput;
use blossom_value::Value;
use blossom_value::time::{Instant, NodeId, Tick};
use blossom_value::value::IntValue;

/// Fills the table with `n` rows, then deletes 50 by key and 50 by a sweep; returns the rows examined by each
/// deleting tick, the rows left, and the rows the engine held once the table was full.
#[cfg(test)]
fn deletion_work(n: u64) -> (u64, u64, usize, usize) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/engine/deletes.bls");
    let (result, _) = compile_file(
        path.to_str().unwrap(),
        &[NodeSpec {
            name: "n1".to_owned(),
            role: None,
        }],
    );
    let artifact = result.unwrap_or_else(|e| panic!("deletes.bls: {e:?}")).0;
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|x| Arc::from(x.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let mut engine = blossom_engine::Engine::new(artifact.program.clone(), NodeId(0), cfg).unwrap();
    let rel = |r: &str| artifact.rel_named(r).unwrap();
    let u = |x: u64| -> blossom_ir::tick::Row { Arc::from(vec![Value::Int(IntValue::U64(x))]) };
    let mut tick = 0u64;
    let mut step = |engine: &mut blossom_engine::Engine, events: Vec<(blossom_base::RelId, blossom_ir::tick::Row)>| {
        engine
            .step(
                &StepInput {
                    node: NodeId(0),
                    incarnation: 1,
                    tick: Tick(tick),
                    now: Instant(tick as i64),
                    events: &events,
                    delivered: &[],
                    ingress: &[],
                    blobs: &blossom_value::NoBlobs,
                },
                &[],
            )
            .unwrap();
        tick += 1;
    };
    // Fill in batches of 1000.
    let mut k = 0;
    while k < n {
        let batch: Vec<_> = (k..(k + 1000).min(n)).map(|x| (rel("add"), u(x))).collect();
        step(&mut engine, batch);
        k += 1000;
    }
    step(&mut engine, Vec::new());
    let held = engine.held_rows();
    // 50 deletes by key, from the middle.
    let before = engine.rows_examined();
    let keys: Vec<_> = (0..50).map(|i| (rel("drop"), u(n / 2 + i))).collect();
    step(&mut engine, keys);
    step(&mut engine, Vec::new());
    let by_key = engine.rows_examined() - before;
    // A sweep of the 50 lowest rows.
    let before = engine.rows_examined();
    step(&mut engine, vec![(rel("drop_below"), u(50))]);
    step(&mut engine, Vec::new());
    let sweep = engine.rows_examined() - before;
    let left = engine.carried_rows(rel("t")).unwrap().len();
    (by_key, sweep, left, held)
}

#[test]
fn deleting_rows_costs_the_rows_deleted_not_the_rows_kept() {
    let (k1, s1, left1, _) = deletion_work(2_000);
    let (k2, s2, left2, _) = deletion_work(20_000);
    assert!(k1 >= 50 && s1 >= 50, "the deleting ticks examined {k1} and {s1} rows: the measure counts nothing");
    assert_eq!(left1, 2_000 - 100);
    assert_eq!(left2, 20_000 - 100);
    // Ten times the rows kept: about the same work to delete a hundred of them.
    assert!(k2 <= k1 * 2 + 50, "by key: {k2} rows examined with 20000 kept against {k1} with 2000");
    assert!(s2 <= s1 * 2 + 50, "sweep: {s2} rows examined with 20000 kept against {s1} with 2000");
}

#[test]
fn a_table_carried_by_its_frame_is_held_once() {
    // The table's next state is a delta over its rows (S24): 20000 rows carried from tick to tick are held once,
    // not once as the table and again as the state the next tick starts from.
    let (_, _, _, held) = deletion_work(20_000);
    assert!(held >= 20_000, "the engine holds {held} rows: fewer than the table's");
    assert!(held <= 20_000 + 100, "the engine holds {held} rows for a table of 20000");
}
