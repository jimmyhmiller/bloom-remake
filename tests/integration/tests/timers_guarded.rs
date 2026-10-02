//! HD item 4: guarded timers (`timer t every d while G`, LANGUAGE §15.2). A guarded timer fires only while its
//! guard held at the end of the node's latest tick; when the guard comes to hold, it resumes at its first firing
//! after that tick (the firings it missed are skipped). The fixture `fixtures/timers/guarded.bls` runs in the
//! synchronous world (on the oracle and the engine, which must agree) and on a node under the manual driver (on
//! both), with the same inputs at the same instants, and a dormant node has no deadline.
//!
//! The guard is the relation's contents in the tick, so `upsert running(n)` makes it hold from the next tick: on a
//! node that is a staged tick at the same instant, in the synchronous world the next round. So the node resumes a
//! round earlier than the synchronous world does, as any state change shows a round later there.

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
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_store::{OpenMode, SimFs, StoreIdentity, Vfs};
use blossom_value::Value;
use blossom_value::time::{Duration, Instant, NodeId, Tick};
use blossom_value::value::IntValue;

/// The round, and each timer tick's instant: tick `t` is at `t × ROUND`.
const ROUND: i64 = 100_000_000;

/// The inputs, as (tick, relation, n): start at 2 (`armed` too), stop at 6, start at 9 (not `armed`), stop at 12.
const INPUTS: [(u64, &str, u64); 4] = [(2, "start", 5), (6, "stop", 0), (9, "start", 1), (12, "stop", 0)];

#[cfg(test)]
fn compile() -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/timers/guarded.bls");
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    result.unwrap_or_else(|e| panic!("guarded.bls: {e:?}")).0
}

#[cfg(test)]
fn u64_of(v: &Value) -> u64 {
    match v {
        Value::Int(IntValue::U64(x)) => *x,
        other => panic!("{other:?}"),
    }
}

/// The counts in a relation's rows (column 0).
#[cfg(test)]
fn counts<'a>(rows: impl Iterator<Item = &'a [Value]>) -> Vec<u64> {
    let mut out: Vec<u64> = rows.map(|r| u64_of(&r[0])).collect();
    out.sort_unstable();
    out
}

/// What the synchronous world fires: `beat` (every 100 ms) from the round after `running` holds (the start at tick
/// 2 holds from tick 3: firings 3 to 6, the stop at 6 taking effect at 7; the start at 9: firings 10 to 12); `slow`
/// (every 250 ms, while started above 1): its firing at 500 ms (its firing at 250 ms is due in tick 3, before the
/// guard held at the end of a tick).
#[cfg(test)]
const SYNC_BEATS: [u64; 7] = [3, 4, 5, 6, 10, 11, 12];
#[cfg(test)]
const SYNC_SLOWS: [u64; 1] = [1];
/// What a node fires: from its first firing after the staged tick at the start's instant (200 ms: firings 2 to 5,
/// the one at 600 ms in the tick of the stop; 900 ms: firings 9 to 11); `slow` at 250 and 500 ms.
#[cfg(test)]
const NODE_BEATS: [u64; 7] = [2, 3, 4, 5, 9, 10, 11];
#[cfg(test)]
const NODE_SLOWS: [u64; 2] = [0, 1];

#[test]
fn a_guarded_timer_fires_only_while_its_guard_holds_in_the_synchronous_world() {
    let artifact = compile();
    let sim = BlsSim::new(&artifact, blossom_value::Seed::from_u64(0)).unwrap();
    let inputs: Vec<InputEvent> = INPUTS
        .iter()
        .map(|(t, rel, n)| InputEvent {
            node: NodeId(0),
            tick: Tick(*t),
            rel: artifact.rel_named(rel).unwrap(),
            row: Arc::from(vec![Value::Int(IntValue::U64(*n))]),
        })
        .collect();
    let round = Duration::from_nanos(ROUND);
    let last = Tick(16);
    let reference = sim.run(&inputs, last, round, &FaultSchedule::default(), false).unwrap();
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let mine = sim
        .run_on(&engine, &inputs, last, round, &FaultSchedule::default(), false)
        .unwrap();
    for (t, (a, b)) in reference.rounds.iter().zip(&mine.rounds).enumerate() {
        assert_eq!(
            a[0].instance, b[0].instance,
            "tick {t}: the oracle and the engine differ"
        );
    }
    let end = &reference.rounds.last().unwrap()[0].instance;
    let fired = artifact.rel_named("fired").unwrap();
    assert_eq!(counts(end.rows(fired).map(|r| &r[..])), SYNC_BEATS, "the beats");
    // Each beat is at its place on the boot timeline: firing k at (k + 1) × 100 ms.
    for r in end.rows(fired) {
        assert_eq!(r[1], Value::Instant(Instant((u64_of(&r[0]) as i64 + 1) * ROUND)));
    }
    assert_eq!(
        counts(end.rows(artifact.rel_named("slow_fired").unwrap()).map(|r| &r[..])),
        SYNC_SLOWS,
        "the slow firings"
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
fn a_guarded_timer_on_a_node_fires_the_same_and_a_dormant_node_sleeps() {
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
            },
            artifact.program.get(),
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
        d.run_until_quiescent(Instant(0)).unwrap();
        // Before any start, and while stopped, no timer is due: the node sleeps.
        assert_eq!(
            d.node.next_deadline().unwrap(),
            None,
            "engine {engine}: dormant at boot"
        );
        let mut ticks = 0;
        let end = 16 * ROUND;
        let mut now = 0i64;
        let mut pending: Vec<(i64, &str, u64)> =
            INPUTS.iter().map(|(t, rel, n)| (*t as i64 * ROUND, *rel, *n)).collect();
        pending.reverse();
        loop {
            let deadline = d.node.next_deadline().unwrap().map(|i| i.0);
            let input = pending.last().map(|p| p.0);
            let Some(next) = [deadline, input].into_iter().flatten().min() else {
                break;
            };
            if next > end {
                break;
            }
            now = next;
            while pending.last().is_some_and(|p| p.0 == now) {
                let (_, rel, n) = pending.pop().unwrap();
                d.node.offer_input(
                    artifact.rel_named(rel).unwrap(),
                    Arc::from(vec![Value::Int(IntValue::U64(n))]),
                );
            }
            ticks += 1;
            d.run_until_quiescent(Instant(now)).unwrap();
            // Stopped (between the stop at 600 ms and the start at 900 ms, and after the stop at 1200 ms): no
            // deadline.
            if now == 6 * ROUND || now == 12 * ROUND {
                assert_eq!(
                    d.node.next_deadline().unwrap(),
                    None,
                    "engine {engine}: dormant after the stop at {now}"
                );
            }
        }
        let fired = d.node.carried_rows(artifact.rel_named("fired").unwrap());
        let slow = d.node.carried_rows(artifact.rel_named("slow_fired").unwrap());
        assert_eq!(
            counts(fired.iter().map(|r| &r[..])),
            NODE_BEATS,
            "engine {engine}: the beats"
        );
        assert_eq!(
            counts(slow.iter().map(|r| &r[..])),
            NODE_SLOWS,
            "engine {engine}: the slow firings"
        );
        for r in &fired {
            assert_eq!(r[1], Value::Instant(Instant((u64_of(&r[0]) as i64 + 1) * ROUND)));
        }
        // It woke only for the four inputs and the firings: beats at 300, 400, 500, 1000 and 1100 ms (those at 600
        // and 1200 ms share the stops' instants), and `slow` at 250 ms (the one at 500 ms shares a beat's).
        assert_eq!(ticks, 4 + 5 + 1, "engine {engine}: wake-ups (last at {now})");
    }
}

/// The codes a program's compile reports (none when it compiles).
#[cfg(test)]
fn codes(tag: &str, src: &str, nodes: &[(&str, Option<&str>)]) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!("blossom-timer-guards-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{tag}.bls"));
    std::fs::write(&path, src).unwrap();
    let nodes: Vec<NodeSpec> = nodes
        .iter()
        .map(|(n, r)| NodeSpec {
            name: (*n).to_owned(),
            role: r.map(str::to_owned),
        })
        .collect();
    match compile_file(path.to_str().unwrap(), &nodes).0 {
        Ok(_) => Vec::new(),
        Err(blossom_front::api::BlsError::Rejected(d)) => d.iter().map(|x| x.code.as_str().to_owned()).collect(),
        Err(e) => panic!("{tag}: {e}"),
    }
}

/// A guard must be a view or a table placed where the timer is (BLS0412), and must exist (BLS0200).
#[test]
fn a_timer_guard_must_be_a_known_view_or_table_where_the_timer_is() {
    let one = [("n1", None)];
    let ok = "program g version 1;\ntimer t every 1s while later;\ntable later(n: u64) key();\n";
    assert_eq!(codes("ok", ok, &one), Vec::<String>::new());
    let event = "program g version 1;\ninput go(n: u64);\ntimer t every 1s while go;\n";
    assert_eq!(codes("event", event, &one), vec!["BLS0412"]);
    let timer = "program g version 1;\ntimer a every 1s;\ntimer t every 1s while a;\n";
    assert_eq!(codes("timer", timer, &one), vec!["BLS0412"]);
    let unknown = "program g version 1;\ntimer t every 1s while nowhere;\n";
    assert_eq!(codes("unknown", unknown, &one), vec!["BLS0200"]);
    // A guard depends on carried state only: a view of an event, or of the clock, is BLS0412 (the node sees it only
    // when it ticks, so it and the synchronous world would fire differently).
    let flash = "program g version 1;\ninput poke(n: u64);\ntimer t every 1s while flash;\nview flash() = poke(_n);\n";
    assert_eq!(codes("flash", flash, &one), vec!["BLS0412"]);
    let clock = "program g version 1;\ntable since(at: Instant) key();\ntimer t every 1s while late;\n\
                 view late() = since(at) where now() > at;\n";
    assert_eq!(codes("clock", clock, &one), vec!["BLS0412"]);
    let roles = "program g version 1;\nrole A;\nrole B;\nat A {\n    timer t every 1s while there;\n}\nat B {\n    \
                 table there(n: u64) key();\n}\n";
    assert_eq!(
        codes("roles", roles, &[("a1", Some("A")), ("b1", Some("B"))]),
        vec!["BLS0412"]
    );
}

/// LDFI treats a timer's firings as inputs that faults cannot change; a guarded timer's depend on state that faults
/// can change, so LDFI refuses the program (as it refuses one that can `halt`) rather than miss a hazard.
#[test]
fn ldfi_refuses_a_program_with_a_guarded_timer() {
    let dir = std::env::temp_dir().join(format!("blossom-timer-guards-ldfi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("guarded_specs.bls");
    std::fs::write(
        &path,
        r#"//! A guarded timer under LDFI.
pub module Pinger {
    static member(n: Node);
    input start(n: u64);
    channel ping(n: u64);
    table armed(n: u64) key();
    table got(n: u64);
    timer beat every 1s while armed;
    arm: on start(n) {
        upsert armed(n);
    }
    fire: on beat(c, _), member(m) where m != self {
        send ping(c) to m;
    }
    recv: on ping(c) {
        emit got(c);
    }
}
spec Two {
    nodes A, B;
    fact member(A);
    fact member(B);
    fact start(1) @ A at tick 1;
}
spec Got for Pinger {
    include Two;
    view pre(x) = got(_) @ x;
    view post(x) = got(_) @ x;
    faults { eot: 4, eff: 2, crashes: 0, model: sync, round: 1s }
    check ldfi expect holds;
}
"#,
    )
    .unwrap();
    let (result, _) = blossom_driver::bls::compile_spec_file(path.to_str().unwrap(), "Got");
    let (compiled, _) = result.unwrap_or_else(|e| panic!("guarded_specs.bls: {e:?}"));
    let faults = compiled.faults.expect("the spec has `faults`");
    let artifact = compiled.artifact;
    let fs =
        blossom_ldfi::FailureSpec::new(faults.eot, faults.eff, faults.crashes, artifact.nodes.len() as u32).unwrap();
    let sim = blossom_sim::spec::SpecSim::new(&artifact).unwrap();
    let Err(err) = blossom_ldfi::run(&sim, &blossom_ldfi::LdfiConfig::new(fs.clone())) else {
        panic!("LDFI ran over a guarded timer");
    };
    let text = err.to_string();
    assert!(text.contains("LANG-172") && text.contains("guarded timer"), "{text}");
    // Exhaustive certification and the one-round step refuse too (they would not fire it).
    let Err(err) = blossom_ldfi::certify::exhaustive(&sim, &fs, &Default::default(), 1, 1_000) else {
        panic!("exhaustive certification ran over a guarded timer");
    };
    assert!(err.to_string().contains("LANG-172"), "{err}");
    let Err(err) = sim.step(blossom_value::time::NodeId(0), Tick(1), &Default::default(), &[]) else {
        panic!("a one-round step ran over a guarded timer");
    };
    assert!(err.to_string().contains("LANG-172"), "{err}");
}
