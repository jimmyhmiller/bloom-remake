//! Keyed members on `blossom run` (docs/design/KEYED.md §4, sub-slice 2): a lobby node and two hosts of `Game` over
//! TCP. Games are created by the first message to them, on the host rendezvous hashing picks; they answer the lobby,
//! the lobby answers them by their sender value, and they message the `ledger` game on their own host and across
//! hosts. A host is stopped with a game half played and comes back from its store: the game ends where it left off,
//! and a game still in play beats on its timer again without any message reaching it.
//! (A message to a host that is down may be lost, as any message may; the admin sends the last move until it lands,
//! and a move is a row, so a second copy changes nothing.)

use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_runtime::client::Client;
use blossom_runtime::deploy::DeploymentSpec;
use blossom_runtime::hosting::Hosting;
use blossom_runtime::keyed::Routing;
use blossom_runtime::server::{Server, ServerConfig, identity};
use blossom_store::OpenMode;
use blossom_value::Value;
use blossom_value::time::NodeId;
use blossom_value::value::IntValue;

#[cfg(test)]
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[cfg(test)]
fn setup() -> (DeploymentSpec, Arc<BlsArtifact>) {
    let dir = std::env::temp_dir().join(format!("blossom-keyed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = dir.join("games.secrets");
    std::fs::write(&secrets, "seed = \"00112233445566778899aabbccddeeff\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/keyed/games_run.bls");
    let node = |name: &str, role: &str, extra: &str| {
        format!(
            "[[node]]\nname = \"{name}\"\nrole = \"{role}\"\naddr = \"127.0.0.1:{}\"\n{extra}\
             principal = \"spiffe://test/games/{role}/{name}\"\n",
            free_port()
        )
    };
    let text = format!(
        "format = 1\n[deployment]\nid = \"games-run\"\nprogram = \"games_run\"\nversion = 1\nsource = \"{}\"\n\
         secrets = \"games.secrets\"\n{}{}{}[security]\nmode = \"insecure-dev\"\n[storage]\ndata_dir = \"data\"\n",
        source.display(),
        node("h1", "Game", ""),
        node("h2", "Game", ""),
        node(
            "lobby",
            "Lobby",
            &format!("client_addr = \"127.0.0.1:{}\"\n", free_port())
        ),
    );
    let spec = DeploymentSpec::parse(&text, &dir).unwrap();
    let nodes: Vec<NodeSpec> = spec
        .nodes
        .iter()
        .map(|n| NodeSpec {
            name: n.name.clone(),
            role: n.role.clone(),
        })
        .collect();
    let (compiled, _) = compile_file(&spec.source.to_string_lossy(), &nodes);
    (spec, Arc::new(compiled.unwrap().0))
}

#[cfg(test)]
fn config(spec: &DeploymentSpec, a: &Arc<BlsArtifact>, node: &str, mode: OpenMode) -> ServerConfig {
    ServerConfig {
        spec: spec.clone(),
        artifact: a.clone(),
        node: node.into(),
        mode,
        dir: None,
        backend: blossom_node::Backend::Engine,
        externs: Arc::new(blossom_std_host::registry().unwrap()),
        record: None,
        web: None,
        admin: None,
    }
}

#[cfg(test)]
fn u(n: u64) -> Value {
    Value::Int(IntValue::U64(n))
}

/// The lobby's rows of durable relation `rel`, newest, sorted.
#[cfg(test)]
fn rows(lobby: &Server, a: &BlsArtifact, rel: &str) -> Vec<Vec<Value>> {
    let db = &lobby.database;
    let Some(tick) = db.range().unwrap().1 else {
        return Vec::new();
    };
    let mut out: Vec<Vec<Value>> = db
        .rows(a.rel_named(rel).unwrap(), &[], tick)
        .unwrap()
        .iter()
        .map(|r| r.to_vec())
        .collect();
    out.sort();
    out
}

/// Tries `ready` every 20 ms for up to 20 s; panics with `what` (given the last try) if it never holds.
#[cfg(test)]
fn eventually(mut ready: impl FnMut() -> Result<(), String>) {
    let mut last = String::new();
    for _ in 0..1000 {
        match ready() {
            Ok(()) => return,
            Err(e) => last = e,
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{last}");
}

/// Waits until the lobby's `rel` holds `want` (sorted).
#[cfg(test)]
fn wait_for(lobby: &Server, a: &BlsArtifact, rel: &str, want: &[Vec<Value>]) {
    let mut want = want.to_vec();
    want.sort();
    eventually(|| {
        let got = rows(lobby, a, rel);
        if got == want {
            Ok(())
        } else {
            Err(format!("the lobby's `{rel}` holds {got:?}, not {want:?}"))
        }
    });
}

#[test]
fn games_run_on_their_hosts_and_survive_a_host_restart() {
    let (spec, a) = setup();
    let p = a.program.get();
    let game_role = p.keyed_role_named("Game").unwrap();
    let member = |key: &str| p.member(game_role, key);
    let routing = Routing::of(&spec, &a).unwrap();
    let (h1, h2) = (a.node_id("h1").unwrap(), a.node_id("h2").unwrap());
    let host = |key: &str| routing.host_of(&member(key)).unwrap();
    let ledger = host("ledger");
    // A game on the ledger's host, one on the other host, and one on h1 to stop h1 under.
    let pick = |want: &dyn Fn(NodeId) -> bool| (1u64..200).find(|n| want(host(&format!("game-{n}")))).unwrap();
    let same = pick(&|h| h == ledger);
    let cross = pick(&|h| h != ledger);
    let kill = (1u64..200)
        .find(|n| *n != same && *n != cross && host(&format!("game-{n}")) == h1)
        .unwrap();
    let live = (1u64..200)
        .find(|n| ![same, cross, kill].contains(n) && host(&format!("game-{n}")) == h1)
        .unwrap();

    // A host runs members, not the role's rules as a node of its own.
    let err = Server::start(config(&spec, &a, "h1", OpenMode::InitFresh))
        .err()
        .unwrap();
    assert!(err.to_string().contains("node h1 hosts a keyed role"), "{err}");
    let mut hosts = vec![
        Hosting::start(config(&spec, &a, "h1", OpenMode::InitFresh)).unwrap(),
        Hosting::start(config(&spec, &a, "h2", OpenMode::InitFresh)).unwrap(),
    ];
    let lobby = Server::start(config(&spec, &a, "lobby", OpenMode::InitFresh)).unwrap();
    let mut admin = Client::connect(
        lobby.client_addr.unwrap(),
        a.clone(),
        &identity(&spec, &a),
        "admin",
        Duration::from_secs(5),
    )
    .unwrap();
    let (pair, play) = (admin.rel("pair").unwrap(), admin.rel("play").unwrap());
    let s = Value::str;
    admin
        .send(
            pair,
            &[
                vec![u(same), s("ada"), s("bob")],
                vec![u(cross), s("cy"), s("di")],
                vec![u(kill), s("eve"), s("fay")],
            ],
        )
        .unwrap();
    for cell in [0, 4, 8] {
        admin
            .send(
                play,
                &[vec![u(same), s("ada"), u(cell)], vec![u(cross), s("di"), u(cell)]],
            )
            .unwrap();
    }
    // Each game reported itself to the lobby, the lobby's thanks reached it, and the ledger counted it.
    let g = |n: u64| Value::Member(member(&format!("game-{n}")));
    let key = |n: u64| s(&format!("game-{n}"));
    wait_for(
        &lobby,
        &a,
        "results",
        &[vec![g(same), key(same), s("ada")], vec![g(cross), key(cross), s("di")]],
    );
    wait_for(&lobby, &a, "acks", &[vec![key(same)], vec![key(cross)]]);
    wait_for(&lobby, &a, "heard", &[vec![g(same)], vec![g(cross)]]);
    let ledger_v = Value::Member(member("ledger"));
    wait_for(
        &lobby,
        &a,
        "tallies",
        &[vec![g(same), ledger_v.clone()], vec![g(cross), ledger_v.clone()]],
    );
    assert!(host(&format!("game-{same}")) == ledger && host(&format!("game-{cross}")) != ledger);
    assert!(
        hosts
            .iter()
            .all(|h| h.stats.opened.load(std::sync::atomic::Ordering::Relaxed) >= 1)
    );

    // Two moves of the third game, and a fourth game started, then their host stops; the third move is sent while it
    // is down.
    admin.send(pair, &[vec![u(live), s("gus"), s("hal")]]).unwrap();
    admin
        .send(play, &[vec![u(kill), s("eve"), u(1)], vec![u(kill), s("eve"), u(2)]])
        .unwrap();
    wait_for(
        &lobby,
        &a,
        "taken",
        &[
            vec![key(kill), s("eve"), u(1)],
            vec![key(kill), s("eve"), u(2)],
            vec![key(same), s("ada"), u(0)],
            vec![key(same), s("ada"), u(4)],
            vec![key(same), s("ada"), u(8)],
            vec![key(cross), s("di"), u(0)],
            vec![key(cross), s("di"), u(4)],
            vec![key(cross), s("di"), u(8)],
        ],
    );
    // The live game beats (its beats are durable, so they count on across the restart).
    let beats = |lobby: &Server| -> u64 {
        rows(lobby, &a, "pulses")
            .iter()
            .filter(|r| r[0] == key(live))
            .filter_map(|r| match r[1] {
                Value::Int(IntValue::U64(n)) => Some(n),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    };
    eventually(|| match beats(&lobby) {
        n if n >= 2 => Ok(()),
        n => Err(format!("the live game beat {n} times")),
    });
    let stopped = hosts.remove(0);
    assert_eq!(stopped.node, h1);
    stopped.stop().unwrap();
    let before = beats(&lobby);
    admin.send(play, &[vec![u(kill), s("eve"), u(3)]]).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(rows(&lobby, &a, "results").len(), 2, "the game's host is down");
    let restarted = Hosting::start(config(&spec, &a, "h1", OpenMode::Existing)).unwrap();
    assert_eq!(restarted.restarts, 2);
    // Nothing is sent to the live game: its timer, restored with it, makes it beat again.
    eventually(|| match beats(&lobby) {
        n if n > before + 2 => Ok(()),
        n => Err(format!("the live game stopped beating at {n} after its host restarted")),
    });
    eventually(|| {
        if rows(&lobby, &a, "taken").contains(&vec![key(kill), s("eve"), u(3)]) {
            return Ok(());
        }
        admin.send(play, &[vec![u(kill), s("eve"), u(3)]]).unwrap();
        Err("the last move never landed".into())
    });
    // The game came back from its store (its players and two moves) and took the third move.
    wait_for(
        &lobby,
        &a,
        "results",
        &[
            vec![g(same), key(same), s("ada")],
            vec![g(cross), key(cross), s("di")],
            vec![g(kill), key(kill), s("eve")],
        ],
    );
    wait_for(
        &lobby,
        &a,
        "tallies",
        &[
            vec![g(same), ledger_v.clone()],
            vec![g(cross), ledger_v.clone()],
            vec![g(kill), ledger_v],
        ],
    );
    let _ = h2;
    lobby.stop().unwrap();
    restarted.stop().unwrap();
    for h in hosts {
        h.stop().unwrap();
    }
}
