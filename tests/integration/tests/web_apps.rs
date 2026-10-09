//! The shared example apps (examples/web/{polls,tictactoe,board,pixels}.bls) and the language fixes they needed. Each
//! app compiles for a server and tabs; tic-tac-toe plays a whole game in the simulator, two tabs and the server, on
//! the oracle and the engine alike; and `fixtures/apps/outer_generator.bls` pins an `outer` atom over a generator's
//! column and constant collections.

use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::{BlsError, NodeSpec};
use blossom_node::EngineEvaluator;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_sim::sync::SyncRun;
use blossom_value::Value;
use blossom_value::time::{Duration, NodeId, Tick};

#[cfg(test)]
fn compile(path: &Path, nodes: &[(&str, Option<&str>)]) -> BlsArtifact {
    let nodes: Vec<NodeSpec> = nodes
        .iter()
        .map(|(n, r)| NodeSpec {
            name: (*n).to_owned(),
            role: r.map(str::to_owned),
        })
        .collect();
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    match result {
        Ok((a, _)) => a,
        Err(BlsError::Rejected(d)) => panic!(
            "{}: {:?}",
            path.display(),
            d.iter().map(|x| &x.message).collect::<Vec<_>>()
        ),
        Err(e) => panic!("{}: {e}", path.display()),
    }
}

#[cfg(test)]
fn app(name: &str) -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/web").join(name);
    compile(
        &path,
        &[("s", Some("Server")), ("b1", Some("Browser")), ("b2", Some("Browser"))],
    )
}

/// Runs `a` on the oracle and the engine, which must agree at every round; the oracle's run.
#[cfg(test)]
fn differential(a: &BlsArtifact, inputs: &[InputEvent], last: u64) -> SyncRun {
    let sim = BlsSim::new(a, blossom_value::Seed::from_u64(0)).unwrap();
    let round = Duration::from_nanos(1_000_000);
    let faults = FaultSchedule::default();
    let reference = sim.run(inputs, Tick(last), round, &faults, false).unwrap();
    let cfg = blossom_engine::EngineConfig {
        roles: a.roles.clone(),
        node_names: a.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(a.program.clone(), cfg);
    let mine = sim.run_on(&engine, inputs, Tick(last), round, &faults, false).unwrap();
    for (t, (x, y)) in reference.rounds.iter().zip(&mine.rounds).enumerate() {
        for (n, (p, q)) in x.iter().zip(y).enumerate() {
            assert_eq!(p.instance, q.instance, "round {t}, node {n}: the oracle and the engine differ");
        }
    }
    reference
}

#[cfg(test)]
fn rows(a: &BlsArtifact, run: &SyncRun, t: u64, node: NodeId, rel: &str) -> Vec<Vec<Value>> {
    let mut out: Vec<Vec<Value>> = run.rounds[t as usize][node.0 as usize]
        .instance
        .rows(a.rel_named(rel).unwrap_or_else(|| panic!("no relation `{rel}`")))
        .map(|r| r.to_vec())
        .collect();
    out.sort();
    out
}

#[cfg(test)]
fn event(a: &BlsArtifact, node: &str, t: u64, rel: &str, args: &[&str]) -> InputEvent {
    InputEvent {
        node: a.node_id(node).unwrap(),
        tick: Tick(t),
        rel: a.rel_named(rel).unwrap(),
        row: Arc::from(args.iter().map(|s| Value::str(*s)).collect::<Vec<_>>()),
    }
}

#[test]
fn every_app_compiles_for_a_server_and_tabs() {
    for name in ["polls.bls", "tictactoe.bls", "board.bls", "pixels.bls", "chat.bls", "todos_shared.bls"] {
        let a = app(name);
        assert!(a.rel_named("Browser.connected").is_some(), "{name}");
    }
}

/// Two tabs sign in, press Play, and play until X has the top row; a tab clicking out of turn, or on a taken square,
/// changes nothing. The server decides the outcome and both tabs show it.
#[test]
fn tictactoe_plays_a_game_on_the_oracle_and_the_engine() {
    let a = app("tictactoe.bls");
    let (s, b1, b2) = (a.node_id("s").unwrap(), a.node_id("b1").unwrap(), a.node_id("b2").unwrap());
    let click = |node: &str, t: u64, id: &str| event(&a, node, t, "click", &[id]);
    let inputs = vec![
        event(&a, "b1", 1, "keydown", &["name", "Enter", "Ada"]),
        event(&a, "b2", 1, "keydown", &["name", "Enter", "Bob"]),
        click("b1", 2, "play"),
        click("b2", 3, "play"),
        // b1 waited longer: it plays X. b2 tries first, out of turn.
        click("b2", 8, "sq-0-4"),
        click("b1", 10, "sq-0-0"),
        click("b2", 14, "sq-0-4"),
        click("b1", 18, "sq-0-1"),
        // A taken square, then a free one.
        click("b2", 22, "sq-0-0"),
        click("b2", 26, "sq-0-8"),
        click("b1", 30, "sq-0-2"),
    ];
    let run = differential(&a, &inputs, 36);
    let u = |x: u64| Value::Int(blossom_value::value::IntValue::U64(x));
    assert_eq!(
        rows(&a, &run, 9, s, "games").iter().map(|r| (r[0].clone(), r[1].clone(), r[2].clone())).collect::<Vec<_>>(),
        vec![(u(0), Value::Node(b1), Value::Node(b2))]
    );
    // b2's out-of-turn click made no move.
    assert_eq!(rows(&a, &run, 13, s, "moves"), vec![vec![u(0), u(0), u(0)]]);
    let moves = rows(&a, &run, 36, s, "moves");
    assert_eq!(
        moves,
        vec![
            vec![u(0), u(0), u(0)],
            vec![u(0), u(1), u(4)],
            vec![u(0), u(2), u(1)],
            vec![u(0), u(3), u(8)],
            vec![u(0), u(4), u(2)],
        ]
    );
    assert_eq!(rows(&a, &run, 36, s, "outcome"), vec![vec![u(0), Value::str("X")]]);
    for tab in [b1, b2] {
        assert_eq!(rows(&a, &run, 36, tab, "outcomes"), vec![vec![u(0), Value::str("X")]]);
    }
    let headline = |tab| rows(&a, &run, 36, tab, "headline");
    assert_eq!(headline(b1), vec![vec![u(0), Value::str("You win!")]]);
    assert_eq!(headline(b2), vec![vec![u(0), Value::str("You lose.")]]);
}

#[test]
fn outer_over_a_generated_column_and_constant_collections() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/apps/outer_generator.bls");
    let a = compile(&path, &[("n1", None)]);
    let n1 = a.node_id("n1").unwrap();
    let u = |x: u64| Value::Int(blossom_value::value::IntValue::U64(x));
    let pair = |t: u64, x: u64, y: u64| InputEvent {
        node: n1,
        tick: Tick(t),
        rel: a.rel_named("pair").unwrap(),
        row: Arc::from(vec![u(x), u(y)]),
    };
    let run = differential(&a, &[pair(1, 1, 2), pair(2, 3, 1)], 4);
    let named = |x: u64, s: &str| vec![u(x), Value::str(s)];
    assert_eq!(
        rows(&a, &run, 4, n1, "named"),
        vec![named(1, "one"), named(2, "?"), named(3, "three")]
    );
    let squares: Vec<Vec<Value>> = [(0, 0), (0, 1), (0, 2), (1, 3), (1, 4), (1, 5)]
        .into_iter()
        .map(|(i, c)| vec![u(i), u(c)])
        .collect();
    assert_eq!(rows(&a, &run, 0, n1, "square"), squares);
    assert_eq!(rows(&a, &run, 0, n1, "const_name"), vec![named(1, "one"), named(4, "four")]);
    assert_eq!(rows(&a, &run, 0, n1, "odd_square"), vec![vec![u(1)], vec![u(3)], vec![u(5)]]);
}
