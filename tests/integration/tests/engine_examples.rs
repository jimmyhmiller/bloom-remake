//! Slice 5: the engine against the reference oracle on every example program, under random inputs and message loss.
//! Each example runs for a few dozen ticks on a small deployment (two nodes per cluster role, one per process
//! role), with input events drawn from small domains (so keys collide and joins match) and random omissions. At
//! every tick the engine's instance must equal the oracle's, and a program error must be the same error at the same
//! node and tick.

use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::TypeId;
use blossom_driver::bls::compile_file;
use blossom_front::api::{BlsError, NodeSpec};
use blossom_ir::core::{EventSource, Placement, RelClass};
use blossom_node::EngineEvaluator;
use blossom_oracle::OracleError;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_sim::sync::{Omission, SimError};
use blossom_value::time::{Duration, NodeId, Tick};
use blossom_value::value::IntValue;
use blossom_value::{TypeDef, Value};

/// SplitMix64.
#[cfg(test)]
struct Rng(u64);

#[cfg(test)]
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

/// The deployment: nodes per role declared in the source (`role R: cluster;` two, a process role one, an external
/// role none), or two role-free nodes.
#[cfg(test)]
fn nodes_of(source: &str) -> Vec<NodeSpec> {
    let mut nodes = Vec::new();
    for line in source.lines() {
        // Top-level roles only (a module's roles are its parameters).
        let Some(rest) = line.strip_prefix("role ") else { continue };
        let rest = rest.trim_end_matches(';');
        let (name, kind) = rest.split_once(':').map_or((rest.trim(), ""), |(n, k)| (n.trim(), k.trim()));
        let count = match kind {
            "external" => 0,
            "cluster" => 2,
            _ => 1,
        };
        for i in 0..count {
            nodes.push(NodeSpec {
                name: format!("{}{i}", name.to_lowercase()),
                role: Some(name.to_owned()),
            });
        }
    }
    if nodes.is_empty() {
        for i in 0..2 {
            nodes.push(NodeSpec {
                name: format!("n{i}"),
                role: None,
            });
        }
    }
    nodes
}

/// A value of type `ty` from a small domain, or `None` for a type this harness does not generate.
#[cfg(test)]
fn small_value(a: &BlsArtifact, ty: TypeId, rng: &mut Rng) -> Option<Value> {
    let p = a.program.get();
    Some(match p.types.get(ty)? {
        TypeDef::Bool => Value::Bool(rng.below(2) == 0),
        TypeDef::Int(t) => Value::Int(IntValue::from_i128(*t, i128::from(rng.below(4)))?),
        TypeDef::Str => Value::Str(["a", "b", "c"][rng.below(3) as usize].into()),
        TypeDef::Bytes => Value::Bytes(vec![b'x' + rng.below(2) as u8].into()),
        TypeDef::Duration => Value::Duration(Duration::from_nanos(rng.below(3) as i64 * 1_000_000_000)),
        TypeDef::Node(_) => Value::Node(NodeId(rng.below(a.nodes.len() as u64) as u32)),
        TypeDef::Option(t) => {
            if rng.below(3) == 0 {
                Value::Option(None)
            } else {
                Value::Option(Some(Arc::new(small_value(a, *t, rng)?)))
            }
        }
        TypeDef::Tuple(ts) => Value::Tuple(ts.iter().map(|t| small_value(a, *t, rng)).collect::<Option<Vec<_>>>()?.into()),
        _ => return None,
    })
}

/// Compiles example `name` for its deployment; `None` if it needs a feature this build does not implement (a BLS0908,
/// with whatever errors follow from it).
#[cfg(test)]
fn compile_example(name: &str) -> Option<BlsArtifact> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples").join(name);
    let source = std::fs::read_to_string(&path).unwrap();
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes_of(&source));
    match result {
        Ok((a, _)) => Some(a),
        Err(BlsError::Rejected(d)) if d.iter().any(|x| x.code.as_str() == "BLS0908") => None,
        Err(e) => panic!("{name}: {e:?}"),
    }
}

/// Runs one scenario of `artifact` on both evaluators and compares them tick by tick; returns the input events it
/// fed.
#[cfg(test)]
fn scenario(name: &str, artifact: &BlsArtifact, seed: u64) -> usize {
    let artifact = artifact.clone();
    let p = artifact.program.get();
    let mut rng = Rng(seed);
    let last = Tick(30);
    // Input events: each root input gets a few rows at random ticks, at nodes of its role.
    let mut inputs = Vec::new();
    for (rel, decl) in p.rels.iter_enumerated() {
        if decl.class != RelClass::Event(EventSource::Input) {
            continue;
        }
        let at: Vec<NodeId> = (0..artifact.nodes.len())
            .filter(|n| match decl.placement {
                Placement::Shared => true,
                Placement::Role(r) => artifact.roles.get(*n).copied().flatten() == Some(r),
            })
            .map(|n| NodeId(n as u32))
            .collect();
        if at.is_empty() {
            continue;
        }
        for _ in 0..8 {
            let row: Option<Vec<Value>> = decl.schema.cols.iter().map(|c| small_value(&artifact, c.ty, &mut rng)).collect();
            let Some(row) = row else { break };
            inputs.push(InputEvent {
                node: at[rng.below(at.len() as u64) as usize],
                tick: Tick(1 + rng.below(last.0 - 5)),
                rel,
                row: Arc::from(row),
            });
        }
    }
    // Message loss: each directed link loses a random tenth of its rounds.
    let mut faults = FaultSchedule::default();
    let n = artifact.nodes.len() as u32;
    for t in 0..=last.0 {
        for from in 0..n {
            for to in 0..n {
                if from != to && rng.below(10) == 0 {
                    faults.omissions.insert(Omission {
                        from: NodeId(from),
                        to: NodeId(to),
                        send: Tick(t),
                    });
                }
            }
        }
    }
    let sim = BlsSim::new(&artifact, blossom_value::Seed::from_u64(seed)).unwrap();
    let round = Duration::from_nanos(250_000_000);
    let reference = sim.run(&inputs, last, round, &faults, false);
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(seed)),
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let mine = sim.run_on(&engine, &inputs, last, round, &faults, false);
    match (&reference, &mine) {
        (Ok(a), Ok(b)) => {
            assert_eq!(a.rounds.len(), b.rounds.len(), "{name} seed {seed}");
            for (t, (ra, rb)) in a.rounds.iter().zip(&b.rounds).enumerate() {
                for (node, (x, y)) in ra.iter().zip(rb).enumerate() {
                    assert!(
                        x.instance == y.instance,
                        "{name} seed {seed}: tick {t} node {node} differs\noracle {:?}\nengine {:?}",
                        x.instance,
                        y.instance
                    );
                    assert_eq!(x.egress, y.egress, "{name} seed {seed}: tick {t} node {node} replies");
                }
            }
            assert_eq!(a.messages, b.messages, "{name} seed {seed}: messages");
        }
        (
            Err(SimError::Node {
                node: n1,
                tick: t1,
                error: e1,
            }),
            Err(SimError::Node {
                node: n2,
                tick: t2,
                error: e2,
            }),
        ) => {
            let code = |e: &OracleError| match e {
                OracleError::Program { error, .. } => Some(error.code),
                _ => None,
            };
            assert!(
                n1 == n2 && t1 == t2 && code(e1).is_some() && code(e1) == code(e2),
                "{name} seed {seed}: the oracle failed at node {} tick {} ({e1}); the engine at node {} tick {} ({e2})",
                n1.0,
                t1.0,
                n2.0,
                t2.0
            );
        }
        (a, b) => panic!("{name} seed {seed}: the oracle gave {:?}; the engine {:?}", a.as_ref().err(), b.as_ref().err()),
    }
    inputs.len()
}

/// Examples that need a feature this build does not implement (BLS0908): user-defined lattices and impl blocks (e05),
/// reliable channels, seals, partitioning and final outputs (e06), soft tables (e08).
#[cfg(test)]
const SKIPPED: &[&str] = &["e05_lattices.bls", "e06_wordcount.bls", "e08_failure_detector.bls"];

#[test]
fn the_engine_agrees_with_the_oracle_on_every_example_under_random_inputs() {
    let mut examples: Vec<String> = std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.ends_with(".bls") && !n.contains("specs"))
        .filter(|n| {
            // Libraries (no `program` header) run inside their specs, which the LDFI tests cover.
            let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples").join(n));
            text.is_ok_and(|t| t.lines().any(|l| l.starts_with("program ")))
        })
        .collect();
    examples.sort();
    assert!(examples.len() >= 9, "{examples:?}");
    let mut skipped = Vec::new();
    let mut ran = 0;
    let mut events = 0;
    for name in &examples {
        let Some(artifact) = compile_example(name) else {
            skipped.push(name.as_str());
            continue;
        };
        for seed in 0..6u64 {
            events += scenario(name, &artifact, seed);
        }
        ran += 1;
    }
    // The examples that need features this build lacks; any other example must run.
    assert_eq!(skipped, SKIPPED, "the examples that do not compile in this build changed");
    assert!(ran >= 6, "only {ran} examples ran");
    assert!(events > 100, "only {events} input events: the scenarios exercised little");
}
