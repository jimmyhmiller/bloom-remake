//! The evaluation depth bound (LANGUAGE §16.1, BLS0217): a program as deep as `MAX_EVAL_DEPTH` evaluates on the oracle
//! and the engine within `EVAL_STACK_BYTES` (test threads get that stack from `.cargo/config.toml`, as the runtime's
//! engine thread, the CLI and LDFI's workers do), and one level deeper is refused at compile time.
//!
//! Each shape stresses one kind of frame at the bound: chains of functions called plainly, chains of functions called
//! inside a closure a `fold` applies (the library's frames), both alternating, a chain of operators in one expression
//! (the parser loops over it, every later phase recurses), and a rule with as many literals as the bound (both
//! evaluators recurse once per literal). Past the bound, a program is refused with a diagnostic, never a crash, even
//! when it is far past it.

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

/// How a chain of functions links `fi` to `f(i+1)`.
#[derive(Clone, Copy, Debug)]
enum Link {
    Plain,
    Fold,
    Alternating,
}

/// A chain of `n` functions: `f0` calls `f1` … down to `fn`, which returns its argument; every link adds 1.
#[cfg(test)]
fn chain(link: Link, n: usize) -> String {
    let mut src = String::from("program deep version 1;\ninput go(k: u64);\n");
    for i in 0..n {
        let next = i + 1;
        let fold = match link {
            Link::Plain => false,
            Link::Fold => true,
            Link::Alternating => i % 2 == 1,
        };
        if fold {
            src.push_str(&format!(
                "fn f{i}(x: u64) -> u64 {{ [x].fold(1u64, |acc, y| acc + f{next}(y)) }}\n"
            ));
        } else {
            src.push_str(&format!("fn f{i}(x: u64) -> u64 {{ f{next}(x) + 1u64 }}\n"));
        }
    }
    src.push_str(&format!("fn f{n}(x: u64) -> u64 {{ x }}\n"));
    src.push_str("view out(k, y) = go(k), let y = f0(k);\n");
    src
}

/// One expression adding 1 to `k` `n` times: `k + 1u64 + … + 1u64`.
#[cfg(test)]
fn operators(n: usize) -> String {
    let mut e = String::from("k");
    for _ in 0..n {
        e.push_str(" + 1u64");
    }
    format!("program deep version 1;\ninput go(k: u64);\nview out(k, y) = go(k), let y = {e};\n")
}

/// A rule with `n` guards, each true, and a `let` that adds their count to `k`.
#[cfg(test)]
fn literals(n: usize) -> String {
    let mut body = String::from("go(k)");
    for i in 0..n {
        body.push_str(&format!(", k + {i}u64 >= k"));
    }
    format!("program deep version 1;\ninput go(k: u64);\nview out(k, y) = {body}, let y = k + {n}u64;\n")
}

#[cfg(test)]
fn compile(src: &str, tag: &str) -> Result<BlsArtifact, BlsError> {
    let dir = std::env::temp_dir().join(format!("blossom-depth-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{tag}.bls"));
    std::fs::write(&path, src).unwrap();
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    result.map(|x| x.0)
}

/// Whether a compile error is the depth bound's: BLS0217, or the parser's bound on an expression's height (BLS0100).
#[cfg(test)]
fn refused_for_depth(e: &BlsError) -> bool {
    matches!(e, BlsError::Rejected(d) if d.iter().any(|x| {
        x.code.as_str() == "BLS0217" || (x.code.as_str() == "BLS0100" && x.message.contains("levels deep"))
    }))
}

/// The largest `n` for which `shape(n)` compiles, where every larger one is refused for its depth; then it runs on
/// both evaluators, which agree that `out(5, 5 + value(n))`.
#[cfg(test)]
fn at_the_bound(tag: &str, shape: impl Fn(usize) -> String, value: impl Fn(usize) -> u64) {
    let (mut lo, mut hi) = (1usize, 4096usize);
    assert!(
        compile(&shape(lo), tag).is_ok(),
        "{tag}: the smallest program is refused"
    );
    assert!(
        compile(&shape(hi), tag).as_ref().is_err_and(refused_for_depth),
        "{tag}: {hi} compiles"
    );
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        match compile(&shape(mid), tag) {
            Ok(_) => lo = mid,
            Err(e) => {
                assert!(refused_for_depth(&e), "{tag} {mid}: {e:?}");
                hi = mid;
            }
        }
    }
    let artifact = compile(&shape(lo), tag).unwrap();
    let (depth, _) = blossom_ir::depth::deepest(artifact.program.get()).unwrap();
    // The bound that stops the shape is the evaluation's (not a parser bound below it, which would leave the
    // evaluators untested at the limit).
    assert!(
        depth <= blossom_ir::depth::MAX_EVAL_DEPTH && depth + 8 > blossom_ir::depth::MAX_EVAL_DEPTH,
        "{tag}: {lo} is {depth} deep"
    );
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
        assert_eq!(a[0].instance, b[0].instance, "{tag}: the oracle and the engine differ");
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
            Value::Int(IntValue::U64(5 + value(lo)))
        ]],
        "{tag}"
    );
}

#[test]
fn an_alternating_chain_of_functions_at_the_bound_evaluates_and_one_deeper_is_refused() {
    at_the_bound("alternating", |n| chain(Link::Alternating, n), |n| n as u64);
}

#[test]
fn a_chain_of_plain_calls_at_the_bound_evaluates_and_one_deeper_is_refused() {
    at_the_bound("plain", |n| chain(Link::Plain, n), |n| n as u64);
}

#[test]
fn a_chain_of_folds_at_the_bound_evaluates_and_one_deeper_is_refused() {
    at_the_bound("fold", |n| chain(Link::Fold, n), |n| n as u64);
}

#[test]
fn a_chain_of_operators_at_the_bound_evaluates_and_one_deeper_is_refused() {
    at_the_bound("operators", operators, |n| n as u64);
}

#[test]
fn a_rule_of_many_literals_at_the_bound_evaluates_and_one_more_is_refused() {
    at_the_bound("literals", literals, |n| n as u64);
}

/// Far past the bound, a program is refused by the parser, before any phase that recurses over its expressions.
#[test]
fn an_expression_far_past_the_bound_is_refused_not_a_crash() {
    for (tag, src) in [
        ("far-operators", operators(200_000)),
        ("far-calls", {
            let mut e = String::from("k");
            for _ in 0..200_000 {
                e.push_str(".max(1u64)");
            }
            format!("program deep version 1;\ninput go(k: u64);\nview out(k, y) = go(k), let y = {e};\n")
        }),
    ] {
        let e = compile(&src, tag).err().unwrap_or_else(|| panic!("{tag} compiles"));
        assert!(refused_for_depth(&e), "{tag}: {e:?}");
    }
}

/// A Molly expression chain nests too: at the evaluation bound it is BLS0217, and far past it the parser refuses it.
#[test]
fn a_ded_chain_past_the_bound_is_refused() {
    let dir = std::env::temp_dir().join(format!("blossom-depth-ded-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let codes = |n: usize| -> Vec<String> {
        let mut e = String::from("X");
        for _ in 0..n {
            e.push_str(" + X");
        }
        let src = format!("out(H, {e}) :- go(H, X);\ngo(\"n1\", 5)@1;\n");
        let path = dir.join(format!("chain{n}.ded"));
        std::fs::write(&path, src).unwrap();
        let (result, _) = blossom_driver::ded::compile_files(&[path.to_str().unwrap()], &["n1"]);
        match result {
            Ok(_) => Vec::new(),
            Err(blossom_front::ded::DedError::Rejected(d)) => d.iter().map(|x| x.code.as_str().to_owned()).collect(),
            Err(e) => panic!("{n}: {e}"),
        }
    };
    assert_eq!(codes(10), Vec::<String>::new());
    assert_eq!(codes(1000), Vec::<String>::new());
    assert_eq!(codes(1023), vec!["BLS0217"]);
    let far = codes(200_000);
    assert!(!far.is_empty() && !far.contains(&"BLS0217".to_owned()), "{far:?}");
}
