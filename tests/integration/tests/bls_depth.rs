//! The evaluation depth bound (LANGUAGE §16.1, BLS0217): a program as deep as `MAX_EVAL_DEPTH` evaluates on the oracle
//! and the engine within `EVAL_STACK_BYTES` (test threads get that stack from `.cargo/config.toml`, as the runtime's
//! engine thread, the CLI and LDFI's workers do), and one level deeper is refused at compile time.
//!
//! The programs are chains of functions, alternating a plain call under an addition with a call inside a closure a
//! `fold` applies, so both the evaluators' expression frames and their library frames are on the path.

use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::{BlsError, NodeSpec};
use blossom_node::EngineEvaluator;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_value::Value;
use blossom_value::time::{Duration, NodeId, Tick};
use blossom_value::value::IntValue;

/// A chain of `n` functions: `f0` calls `f1` … down to `fn`, which returns its argument; every link adds 1.
#[cfg(test)]
fn chain(n: usize) -> String {
    let mut src = String::from("program deep version 1;\ninput go(k: u64);\n");
    for i in 0..n {
        let next = i + 1;
        if i % 2 == 0 {
            src.push_str(&format!("fn f{i}(x: u64) -> u64 {{ f{next}(x) + 1u64 }}\n"));
        } else {
            src.push_str(&format!(
                "fn f{i}(x: u64) -> u64 {{ [x].fold(1u64, |acc, y| acc + f{next}(y)) }}\n"
            ));
        }
    }
    src.push_str(&format!("fn f{n}(x: u64) -> u64 {{ x }}\n"));
    src.push_str("view out(k, y) = go(k), let y = f0(k);\n");
    src
}

#[cfg(test)]
fn compile(n: usize) -> Result<BlsArtifact, BlsError> {
    let dir = std::env::temp_dir().join(format!("blossom-depth-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("deep{n}.bls"));
    std::fs::write(&path, chain(n)).unwrap();
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    result.map(|x| x.0)
}

#[cfg(test)]
fn refused_for_depth(e: &BlsError) -> bool {
    matches!(e, BlsError::Rejected(d) if d.iter().any(|x| x.code.as_str() == "BLS0217"))
}

#[test]
fn a_program_at_the_depth_bound_evaluates_and_one_deeper_is_refused() {
    // The longest chain that compiles: every longer one is refused for its depth.
    let (mut lo, mut hi) = (1usize, 2048usize);
    assert!(compile(lo).is_ok());
    assert!(compile(hi).as_ref().is_err_and(refused_for_depth));
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        match compile(mid) {
            Ok(_) => lo = mid,
            Err(e) => {
                assert!(refused_for_depth(&e), "chain {mid}: {e:?}");
                hi = mid;
            }
        }
    }
    let artifact = compile(lo).unwrap();
    let (depth, _) = blossom_ir::depth::deepest(artifact.program.get()).unwrap();
    assert!(
        depth <= blossom_ir::depth::MAX_EVAL_DEPTH && depth + 6 > blossom_ir::depth::MAX_EVAL_DEPTH,
        "{depth}"
    );
    // It evaluates on both evaluators, which agree: f0(5) is 5 plus 1 per link.
    let inputs = [InputEvent {
        node: NodeId(0),
        tick: Tick(1),
        rel: artifact.rel_named("go").unwrap(),
        row: Arc::from(vec![Value::Int(IntValue::U64(5))]),
    }];
    let sim = BlsSim::new(&artifact, blossom_value::Seed::from_u64(0)).unwrap();
    let round = Duration::from_nanos(1_000_000_000);
    let reference = sim
        .run(&inputs, Tick(2), round, &FaultSchedule::default(), false)
        .unwrap();
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let mine = sim
        .run_on(&engine, &inputs, Tick(2), round, &FaultSchedule::default(), false)
        .unwrap();
    for (a, b) in reference.rounds.iter().zip(&mine.rounds) {
        assert_eq!(a[0].instance, b[0].instance, "the oracle and the engine differ");
    }
    let out: Vec<Vec<Value>> = reference
        .node_tick(Tick(1), NodeId(0))
        .unwrap()
        .instance
        .rows(artifact.rel_named("out").unwrap())
        .map(|r| r.to_vec())
        .collect();
    assert_eq!(
        out,
        vec![vec![
            Value::Int(IntValue::U64(5)),
            Value::Int(IntValue::U64(5 + lo as u64))
        ]]
    );
}
