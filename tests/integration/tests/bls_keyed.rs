//! Keyed roles (docs/design/KEYED.md): `role Game: keyed;`, whose members are named by key (`Game.named(k)`) and
//! created on demand. In simulation a keyed member is a node of the deployment named by its key (`game-1=Game`): the
//! lobby fixture runs on the oracle and the engine alike, a member is a `Value::Member` wherever a program sees it
//! (`self`, a sender, a row), and a send to a member the simulation does not run is a hard error.

use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::{BlsError, NodeSpec};
use blossom_node::EngineEvaluator;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_value::Value;
use blossom_value::time::{Duration, MemberRef, Tick};
use blossom_value::value::IntValue;

#[cfg(test)]
fn compile(nodes: &[(&str, &str)]) -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/keyed/games.bls");
    let nodes: Vec<NodeSpec> = nodes
        .iter()
        .map(|(n, r)| NodeSpec {
            name: (*n).to_owned(),
            role: Some((*r).to_owned()),
        })
        .collect();
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    match result {
        Ok((a, _)) => a,
        Err(BlsError::Rejected(d)) => panic!("games.bls: {:?}", d.iter().map(|x| &x.message).collect::<Vec<_>>()),
        Err(e) => panic!("games.bls: {e}"),
    }
}

#[cfg(test)]
fn input(a: &BlsArtifact, rel: &str, t: u64, row: Vec<Value>) -> InputEvent {
    InputEvent {
        node: a.node_id("lobby").unwrap(),
        tick: Tick(t),
        rel: a.rel_named(rel).unwrap(),
        row: Arc::from(row),
    }
}

#[cfg(test)]
fn u(n: u64) -> Value {
    Value::Int(IntValue::U64(n))
}

#[cfg(test)]
fn game(a: &BlsArtifact, key: &str) -> Value {
    let role = a
        .program
        .get()
        .roles
        .iter_enumerated()
        .find(|(_, r)| r.name.to_string() == "Game")
        .map(|(id, _)| id)
        .unwrap();
    Value::Member(MemberRef { role, key: key.into() })
}

#[cfg(test)]
fn engine(a: &BlsArtifact) -> EngineEvaluator {
    let cfg = blossom_engine::EngineConfig {
        roles: a.roles.clone(),
        node_names: a.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        members: Arc::new(a.members().unwrap()),
        ..blossom_engine::EngineConfig::default()
    };
    EngineEvaluator::new(a.program.clone(), cfg)
}

#[test]
fn games_are_keyed_members_on_the_oracle_and_the_engine() {
    let a = compile(&[("lobby", "Lobby"), ("game-1", "Game"), ("game-2", "Game")]);
    let sim = BlsSim::new(&a, blossom_value::Seed::from_u64(0)).unwrap();
    let round = Duration::from_nanos(1_000_000);
    let s = Value::str;
    let mut inputs = vec![
        input(&a, "pair", 1, vec![u(2), s("cy"), s("di")]),
        input(&a, "pair", 1, vec![u(1), s("ada"), s("bob")]),
    ];
    // Ada makes three moves in game 1 (and an outsider's move is ignored); Di makes three in game 2, a round later.
    for (t, cell) in [(3, 0), (4, 4), (5, 8)] {
        inputs.push(input(&a, "play", t, vec![u(1), s("ada"), u(cell)]));
        inputs.push(input(&a, "play", t + 1, vec![u(2), s("di"), u(cell)]));
    }
    inputs.push(input(&a, "play", 3, vec![u(1), s("eve"), u(5)]));
    let last = Tick(12);
    let reference = sim.run(&inputs, last, round, &FaultSchedule::default(), false).unwrap();
    let mine = sim
        .run_on(&engine(&a), &inputs, last, round, &FaultSchedule::default(), false)
        .unwrap();
    for (t, (x, y)) in reference.rounds.iter().zip(&mine.rounds).enumerate() {
        for (n, (p, q)) in x.iter().zip(y).enumerate() {
            assert_eq!(
                p.instance, q.instance,
                "round {t}, node {n}: the oracle and the engine differ"
            );
        }
    }
    let rows = |node: &str, rel: &str| -> Vec<Vec<Value>> {
        let n = a.node_id(node).unwrap();
        let mut out: Vec<Vec<Value>> = reference.rounds[last.0 as usize][n.0 as usize]
            .instance
            .rows(a.rel_named(rel).unwrap())
            .map(|r| r.to_vec())
            .collect();
        out.sort();
        out
    };
    let (g1, g2) = (game(&a, "game-1"), game(&a, "game-2"));
    // Each game reported itself (`self`, `self.key()`) with its winner.
    assert_eq!(
        rows("lobby", "results"),
        vec![
            vec![g1.clone(), s("game-1"), s("ada")],
            vec![g2.clone(), s("game-2"), s("di")]
        ]
    );
    // The sender of a message from a member is the member; members order by key.
    assert_eq!(rows("lobby", "heard"), vec![vec![g1.clone()], vec![g2]]);
    assert_eq!(rows("lobby", "first"), vec![vec![g1]]);
    // The lobby's reply reached each game by its sender value.
    assert_eq!(rows("game-1", "thanked"), vec![vec![s("game-1")]]);
    assert_eq!(rows("game-2", "thanked"), vec![vec![s("game-2")]]);
    assert_eq!(rows("game-1", "players"), vec![vec![s("ada"), s("bob")]]);
    assert_eq!(rows("game-1", "moves").len(), 3, "the outsider's move is not taken");
}

#[test]
fn a_send_to_a_member_the_simulation_does_not_run_is_an_error() {
    let a = compile(&[("lobby", "Lobby"), ("game-1", "Game")]);
    let sim = BlsSim::new(&a, blossom_value::Seed::from_u64(0)).unwrap();
    let inputs = vec![input(&a, "pair", 1, vec![u(7), Value::str("x"), Value::str("o")])];
    let round = Duration::from_nanos(1_000_000);
    for run in [
        sim.run(&inputs, Tick(3), round, &FaultSchedule::default(), false),
        sim.run_on(&engine(&a), &inputs, Tick(3), round, &FaultSchedule::default(), false),
    ] {
        let err = run.unwrap_err().to_string();
        assert!(
            err.contains(r#"a send to Game:"game-7", a member no node runs"#),
            "{err}"
        );
    }
}

/// Members round-trip through the wire codec (dense node ids) and the durable one (names), typed `Node<Game>` (the
/// key alone) and `Node` (tagged, beside plain nodes); a member of another role is refused for `Node<Game>`.
#[test]
fn members_round_trip_through_the_codecs() {
    use blossom_wire::codec::{Codec, NodeEncoding, WireLimits};
    let a = compile(&[("lobby", "Lobby"), ("game-1", "Game")]);
    let p = a.program.get();
    let cols = |rel: &str| p.rels.get(a.rel_named(rel).unwrap()).unwrap().schema.cols.clone();
    let names: Arc<[Arc<str>]> = a.nodes.iter().map(|n| Arc::from(n.as_str())).collect();
    let lobby = Value::Node(a.node_id("lobby").unwrap());
    for enc in [NodeEncoding::Dense, NodeEncoding::ByName(names)] {
        let codec = Codec::new(p, enc, WireLimits::default());
        let rows = [
            (
                "results",
                vec![game(&a, "game-1"), Value::str("game-1"), Value::str("ada")],
            ),
            ("results", vec![game(&a, "a\0b é"), Value::str(""), Value::str("")]),
            ("heard", vec![game(&a, "game-1")]),
            ("heard", vec![lobby.clone()]),
        ];
        for (rel, row) in rows {
            let mut bytes = Vec::new();
            codec.encode_row(&cols(rel), &row, &mut bytes).unwrap();
            let back = codec.decode_row(&cols(rel), &mut bytes.as_slice()).unwrap();
            assert_eq!(back, row, "{rel}");
        }
        // `Node<Game>` holds no plain node.
        let mut bytes = Vec::new();
        assert!(
            codec
                .encode_row(
                    &cols("results"),
                    &[lobby.clone(), Value::str(""), Value::str("")],
                    &mut bytes
                )
                .is_err()
        );
    }
}

/// The durable path (the WAL, the database's order-preserving keys, recovery) keeps members: on the cluster simulator
/// a game and the lobby crash, lose every unsynced write, and recover their tables of members and keys.
#[test]
fn members_survive_crashes_on_the_cluster_simulator() {
    use blossom_node::durable::DurableSchema;
    use blossom_sim::cluster::{Cluster, ClusterConfig, CrashWrites, NoKvClients};
    let a = compile(&[("lobby", "Lobby"), ("game-1", "Game")]);
    let schema = DurableSchema::of(a.program.get());
    let cfg = ClusterConfig {
        clients: 0,
        duration: 60_000_000_000,
        ..ClusterConfig::default()
    };
    let mut c = Cluster::new(
        &a,
        &schema,
        blossom_value::Seed::from_u64(3),
        Vec::new(),
        Box::new(NoKvClients),
        cfg,
    )
    .unwrap();
    let (lobby, g) = (a.node_id("lobby").unwrap(), a.node_id("game-1").unwrap());
    let s = Value::str;
    let rel = |r: &str| a.rel_named(r).unwrap();
    let second = 1_000_000_000;
    let mut at = c.now();
    let step = |c: &mut Cluster<'_>, at: &mut i64| {
        *at += second;
        c.run_until(*at).unwrap();
    };
    c.input(lobby, rel("pair"), Arc::from(vec![u(1), s("ada"), s("bob")]))
        .unwrap();
    step(&mut c, &mut at);
    c.input(lobby, rel("play"), Arc::from(vec![u(1), s("ada"), u(0)]))
        .unwrap();
    step(&mut c, &mut at);
    // The game crashes after its first move and comes back with its players and move.
    c.crash(g, CrashWrites::Lost).unwrap();
    step(&mut c, &mut at);
    c.restart(g).unwrap();
    step(&mut c, &mut at);
    let state = c.state(g).unwrap().unwrap();
    assert!(state.contains(rel("players"), &[s("ada"), s("bob")]));
    assert!(state.contains(rel("moves"), &[s("ada"), u(0)]));
    for cell in [4, 8] {
        c.input(lobby, rel("play"), Arc::from(vec![u(1), s("ada"), u(cell)]))
            .unwrap();
        step(&mut c, &mut at);
    }
    step(&mut c, &mut at);
    let g1 = game(&a, "game-1");
    let state = c.state(lobby).unwrap().unwrap();
    assert!(state.contains(rel("results"), &[g1.clone(), s("game-1"), s("ada")]));
    assert!(state.contains(rel("heard"), std::slice::from_ref(&g1)));
    // The lobby crashes too and recovers its rows of members, typed and plain.
    c.crash(lobby, CrashWrites::Lost).unwrap();
    step(&mut c, &mut at);
    c.restart(lobby).unwrap();
    step(&mut c, &mut at);
    let state = c.state(lobby).unwrap().unwrap();
    assert!(state.contains(rel("results"), &[g1.clone(), s("game-1"), s("ada")]));
    assert!(state.contains(rel("heard"), &[g1]));
    assert!(c.state(g).unwrap().unwrap().contains(rel("done"), &[s("ada")]));
}

/// On `blossom run` a deployment's node of a keyed role is a host: it runs that role's members, each a node of its
/// own, and is no member itself, so an object asked to run the host as a node refuses.
#[test]
fn a_host_is_not_run_as_a_node() {
    use blossom_runtime::deploy::DeploymentSpec;
    use blossom_runtime::object::{ObjectConfig, ObjectNode};
    use blossom_store::{KvFs, KvStore, MemKv};
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/keyed/games.bls");
    let node = |name: &str, role: &str, port: u16| {
        format!(
            "[[node]]\nname = \"{name}\"\nrole = \"{role}\"\naddr = \"127.0.0.1:{port}\"\n\
             principal = \"spiffe://test/games/{role}/{name}\"\n"
        )
    };
    let text = format!(
        "format = 1\n[deployment]\nid = \"games\"\nprogram = \"games\"\nversion = 1\nsource = \"{}\"\n{}{}\
         [security]\nmode = \"insecure-dev\"\n[storage]\ndata_dir = \"data\"\n",
        source.display(),
        node("h1", "Game", 1),
        node("lobby", "Lobby", 2),
    );
    let spec = DeploymentSpec::parse(&text, &std::env::temp_dir()).unwrap();
    let nodes: Vec<NodeSpec> = spec
        .nodes
        .iter()
        .map(|n| NodeSpec {
            name: n.name.clone(),
            role: n.role.clone(),
        })
        .collect();
    let (compiled, _) =
        blossom_driver::bls::compile_deployed(&spec.source.to_string_lossy(), &nodes, &Default::default());
    let kv = Arc::new(MemKv::default());
    let opened = ObjectNode::open(ObjectConfig {
        spec,
        artifact: Arc::new(compiled.unwrap().0),
        node: "h1".into(),
        member: None,
        members: Arc::new(blossom_ir::members::Members::open()),
        fs: Arc::new(KvFs::open(kv as Arc<dyn KvStore>).unwrap()),
        dir: "/h1".into(),
        seed: blossom_value::Seed([1; 16]),
        now: blossom_value::time::Instant(1),
        nonce: 1,
        random: Box::new(|_| Ok(())),
        externs: Arc::new(blossom_std_host::registry().unwrap()),
    });
    let err = match opened {
        Ok(_) => panic!("a host ran as a node"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("node h1 hosts the keyed role `Game`"), "{err}");
}
