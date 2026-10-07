//! S17: every kind of timer (LANGUAGE §15.2) — `every d times m`, `once after d`, `once` and the logical
//! `every n ticks times m`. The fixture `fixtures/timers/kinds.bls` runs in the synchronous world (on the oracle and
//! the engine, which must agree) and on a node under the manual driver (on both). A physical timer fires at the same
//! places on the boot timeline in both worlds; a logical timer counts the node's ticks, which on a node come one after
//! another at the same instant (the timer keeps the node ticking), and a spent timer no longer wakes the node.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::durable::DurableSchema;
use blossom_node::manual::ManualDriver;
use blossom_node::recovery::{self, StoreSpec};
use blossom_node::{EngineEvaluator, Executor, Node, NodeConfig, OracleExecutor};
use blossom_oracle::Oracle;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::BlsSim;
use blossom_store::{OpenMode, SimFs, StoreIdentity, Vfs};
use blossom_value::Value;
use blossom_value::time::{Duration, Instant, NodeId, Tick};
use blossom_value::value::IntValue;

/// The round in the synchronous world: round `t` is at `t × ROUND`.
const ROUND: i64 = 100_000_000;
const MS: i64 = 1_000_000;

#[cfg(test)]
fn nodes() -> [NodeSpec; 1] {
    [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }]
}

#[cfg(test)]
fn compile() -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/timers/kinds.bls");
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes());
    result.unwrap_or_else(|e| panic!("kinds.bls: {e:?}")).0
}

/// A recording table's rows as (count, at in ms), in count order.
#[cfg(test)]
fn fired<'a>(rows: impl Iterator<Item = &'a [Value]>) -> Vec<(u64, i64)> {
    let mut out: Vec<(u64, i64)> = rows
        .map(|r| match (&r[0], &r[1]) {
            (Value::Int(IntValue::U64(k)), Value::Instant(at)) => (*k, at.0 / MS),
            other => panic!("not a firing: {other:?}"),
        })
        .collect();
    out.sort_unstable();
    out
}

#[test]
fn every_kind_of_timer_fires_on_its_schedule_in_the_synchronous_world() {
    let artifact = compile();
    let sim = BlsSim::new(&artifact, blossom_value::Seed::from_u64(0)).unwrap();
    let round = Duration::from_nanos(ROUND);
    let last = Tick(8);
    let reference = sim.run(&[], last, round, &FaultSchedule::default(), false).unwrap();
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let mine = sim
        .run_on(&engine, &[], last, round, &FaultSchedule::default(), false)
        .unwrap();
    for (t, (a, b)) in reference.rounds.iter().zip(&mine.rounds).enumerate() {
        assert_eq!(
            a[0].instance, b[0].instance,
            "tick {t}: the oracle and the engine differ"
        );
    }
    let end = &reference.rounds.last().unwrap()[0].instance;
    let table = |name: &str| fired(end.rows(artifact.rel_named(name).unwrap()).map(|r| &r[..]));
    assert_eq!(
        table("beat_fired"),
        [(0, 100), (1, 200), (2, 300)],
        "every 100ms times 3"
    );
    assert_eq!(
        table("kick_fired"),
        [(0, 250)],
        "once after 250ms: delivered in round 3, at its due time"
    );
    assert_eq!(table("start_fired"), [(0, 0)], "once: in the boot round");
    assert_eq!(
        table("probe_fired"),
        [(0, 200), (1, 500)],
        "every 3 ticks times 2: rounds 2 and 5"
    );
}

#[cfg(test)]
fn identity() -> StoreIdentity {
    StoreIdentity {
        store_uuid: [0; 16],
        deployment_id: [1; 16],
        program_id: [2; 16],
        node_name: "n1".into(),
        principal: "spiffe://test/timers/n1".into(),
        format: recovery::FORMAT,
        directory_digest: [3; 16],
    }
}

#[test]
fn every_kind_of_timer_fires_the_same_on_a_node_and_a_spent_timer_sleeps() {
    let artifact = compile();
    let schema = DurableSchema::of(artifact.program.get());
    let names: Arc<[Arc<str>]> = artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect();
    for engine in [false, true] {
        let fs = SimFs::default();
        let vfs: Arc<dyn Vfs> = Arc::new(fs.clone());
        let opened = recovery::open(
            vfs,
            &StoreSpec {
                dir: PathBuf::from("/data/n1"),
                identity: identity(),
                mode: OpenMode::InitFresh,
                certification: blossom_store::Certification::Strict,
                database: blossom_store::lsm::LsmOptions::default(),
            },
            &artifact.program,
            names.clone(),
            Instant(0),
            7,
        )
        .unwrap();
        let exec: Box<dyn Executor> = if engine {
            let ecfg = blossom_engine::EngineConfig {
                roles: artifact.roles.clone(),
                node_names: names.to_vec(),
                seed: Some(blossom_value::Seed::from_u64(0)),
                ..blossom_engine::EngineConfig::default()
            };
            Box::new(blossom_engine::Engine::new(artifact.program.clone(), NodeId(0), ecfg).unwrap())
        } else {
            let oracle = Oracle::new(artifact.program.clone())
                .unwrap()
                .with_roles(artifact.roles.clone())
                .with_seed(blossom_value::Seed::from_u64(0))
                .unwrap()
                .with_node_names(names.to_vec())
                .unwrap();
            Box::new(OracleExecutor::new(Arc::new(oracle)))
        };
        let node = Node::boot(
            NodeConfig::new(NodeId(0), None),
            &artifact.program,
            exec,
            opened.boot.clone(),
        )
        .unwrap();
        let mut d = ManualDriver::new(node, artifact.program.get(), &schema, names.clone(), opened);
        // At boot the logical timer keeps the node ticking until it is spent: its 3rd and 6th ticks, all at 0.
        d.run_until_quiescent(Instant(0)).unwrap();
        assert_eq!(
            d.node.next_deadline().unwrap(),
            Some(Instant(100 * MS)),
            "engine {engine}: the beat is next"
        );
        let mut wakeups = Vec::new();
        while let Some(next) = d.node.next_deadline().unwrap() {
            assert!(next.0 <= 1000 * MS, "engine {engine}: a timer due at {next:?}");
            wakeups.push(next.0 / MS);
            d.run_until_quiescent(next).unwrap();
        }
        // Then only the physical firings wake it, and once every timer is spent nothing does.
        assert_eq!(wakeups, [100, 200, 250, 300], "engine {engine}: wake-ups");
        let table = |name: &str| {
            let rows = d.node.carried_rows(artifact.rel_named(name).unwrap()).unwrap();
            fired(rows.iter().map(|r| &r[..]))
        };
        assert_eq!(table("beat_fired"), [(0, 100), (1, 200), (2, 300)], "engine {engine}");
        assert_eq!(table("kick_fired"), [(0, 250)], "engine {engine}");
        assert_eq!(table("start_fired"), [(0, 0)], "engine {engine}");
        assert_eq!(
            table("probe_fired"),
            [(0, 0), (1, 0)],
            "engine {engine}: logical, at boot's instant"
        );
    }
}

/// The codes a program's compile reports (none when it compiles).
#[cfg(test)]
fn codes(tag: &str, src: &str) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!("blossom-timer-kinds-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{tag}.bls"));
    std::fs::write(&path, src).unwrap();
    match compile_file(path.to_str().unwrap(), &nodes()).0 {
        Ok(_) => Vec::new(),
        Err(blossom_front::api::BlsError::Rejected(d)) => d.iter().map(|x| x.code.as_str().to_owned()).collect(),
        Err(e) => panic!("{tag}: {e}"),
    }
}

#[test]
fn timer_declarations_need_positive_values_of_their_types() {
    let program = |timer: &str| format!("program k version 1;\n{timer}\ntable g(n: u64) key();\n");
    for ok in [
        "timer t every 1s times 2;",
        "timer t every 4 ticks;",
        "timer t every 4 ticks times 1;",
        "timer t once after 10ms;",
        "timer t once;",
        "timer t every 5 ticks while g;",
        "timer t once after 1s while g;",
    ] {
        assert_eq!(codes("ok", &program(ok)), Vec::<String>::new(), "{ok}");
    }
    for (bad, code) in [
        ("timer t every 1s times 0;", "BLS0300"),
        ("timer t every 0 ticks;", "BLS0300"),
        ("timer t every 1s ticks;", "BLS0300"),
        ("timer t once after 5;", "BLS0300"),
        ("timer t every 2 ticks times 1s;", "BLS0300"),
        // `once` fires in the boot tick, before a guard can hold.
        ("timer t once while g;", "BLS0412"),
    ] {
        assert_eq!(codes("bad", &program(bad)), vec![code], "{bad}");
    }
}

#[test]
fn a_deployed_build_warns_of_a_logical_timer() {
    let dir = std::env::temp_dir().join(format!("blossom-timer-kinds-deployed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("deployed.bls");
    std::fs::write(
        &path,
        "program k version 1;\ntimer probe every 4 ticks;\ntimer beat every 1s;\n",
    )
    .unwrap();
    let path = path.to_str().unwrap();
    let params = std::collections::BTreeMap::new();
    let (deployed, _) = blossom_driver::bls::compile_deployed(path, &nodes(), &params);
    let (_, warnings) = deployed.unwrap();
    let found: Vec<&str> = warnings.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(found, ["BLS1006"], "the logical timer only");
    assert!(warnings.iter().any(|d| d.message.contains("`probe`")));
    // A build for simulation does not warn.
    let (simulated, _) = compile_file(path, &nodes());
    assert!(simulated.unwrap().1.iter().next().is_none());
}
