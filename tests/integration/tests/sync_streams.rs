//! S11: byte streams in the synchronous-round world (`blossom_sim::fabric`), the world LDFI searches. Two nodes of
//! a Blossom program connect over a stream: a dial in round `t` opens both ends in round `t + 1`, a write arrives
//! the round after it, in `seq` order; a pause holds what arrives; a crash resets the connection at the other end;
//! a lost message resets it, or fails the dial it carries. Every stream event records what it comes from.

use std::collections::BTreeMap;
use std::path::Path;

use blossom_artifact::bls::BlsArtifact;
use blossom_front::api::{NodeSpec, ParamBinding};
use blossom_sim::bls::BlsSim;
use blossom_sim::fabric::Cause;
use blossom_sim::sync::SyncRun;
use blossom_sim::{FaultSchedule, Omission};
use blossom_value::Value;
use blossom_value::time::{Duration, NodeId, Tick};
use blossom_value::value::IntValue;

#[cfg(test)]
const CLI: NodeId = NodeId(0);
#[cfg(test)]
const SRV: NodeId = NodeId(1);

#[cfg(test)]
fn compile(name: &str, params: &BTreeMap<String, ParamBinding>) -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/streams")
        .join(name);
    let nodes = [
        NodeSpec {
            name: "cli".to_owned(),
            role: Some("Client".to_owned()),
        },
        NodeSpec {
            name: "srv".to_owned(),
            role: Some("Server".to_owned()),
        },
    ];
    let (result, _) = blossom_driver::bls::compile_file_with(path.to_str().unwrap(), &nodes, params);
    result.unwrap_or_else(|e| panic!("{name}: {e:?}")).0
}

#[cfg(test)]
fn simulate(artifact: &BlsArtifact, last: u64, faults: &FaultSchedule) -> SyncRun {
    let sim = BlsSim::new(artifact, blossom_value::Seed::from_u64(0))
        .unwrap()
        .connecting_streams();
    sim.run(&[], Tick(last), Duration::from_nanos(1_000_000), faults, false)
        .unwrap()
}

/// The stream events `node` took in round `t`: the event's relation name, and its causes.
#[cfg(test)]
fn events(artifact: &BlsArtifact, run: &SyncRun, t: u64, node: NodeId) -> Vec<(String, Vec<Cause>)> {
    let names: BTreeMap<_, _> = artifact
        .program
        .get()
        .rels
        .iter_enumerated()
        .map(|(id, r)| (id, r.name.to_string()))
        .collect();
    run.node_tick(Tick(t), node)
        .unwrap()
        .streams
        .iter()
        .map(|e| (names[&e.rel].clone(), e.causes.clone()))
        .collect()
}

/// Every round's stream events at `node`, as (round, relation name).
#[cfg(test)]
fn timeline(artifact: &BlsArtifact, run: &SyncRun, node: NodeId) -> Vec<(u64, String)> {
    (0..run.rounds.len() as u64)
        .flat_map(|t| {
            events(artifact, run, t, node)
                .into_iter()
                .map(move |(name, _)| (t, name))
        })
        .collect()
}

#[cfg(test)]
fn rows(artifact: &BlsArtifact, run: &SyncRun, node: NodeId, rel: &str) -> Vec<Vec<Value>> {
    let rel = artifact.rel_named(rel).unwrap();
    let last = run.last().unwrap();
    run.node_tick(last, node)
        .unwrap()
        .instance
        .rows(rel)
        .map(|r| r.to_vec())
        .collect()
}

#[cfg(test)]
fn act(node: NodeId, t: u64) -> Cause {
    Cause::Act { node, tick: Tick(t) }
}

#[cfg(test)]
fn text(s: &str) -> Value {
    Value::Str(s.into())
}

#[test]
#[cfg(test)]
fn a_dial_opens_both_ends_the_next_round_and_bytes_take_a_round_each_way() {
    let artifact = compile("pair.bls", &BTreeMap::new());
    let run = simulate(&artifact, 4, &FaultSchedule::default());
    assert_eq!(
        timeline(&artifact, &run, CLI),
        [(1, "up.opened".to_owned()), (3, "up.data".to_owned())]
    );
    assert_eq!(
        timeline(&artifact, &run, SRV),
        [(1, "echo.opened".to_owned()), (2, "echo.data".to_owned())]
    );
    assert_eq!(
        events(&artifact, &run, 1, SRV)[0].1,
        [act(CLI, 0)],
        "opened by the dial"
    );
    assert_eq!(
        events(&artifact, &run, 2, SRV)[0].1,
        [act(CLI, 1)],
        "the client's write"
    );
    assert_eq!(events(&artifact, &run, 3, CLI)[0].1, [act(SRV, 2)], "the server's echo");
    assert_eq!(
        rows(&artifact, &run, CLI, "got"),
        [vec![
            Value::Int(IntValue::U64(0)),
            Value::Bytes(b"ping\n".to_vec().into())
        ]]
    );
    assert!(run.stream_violations.is_empty());
}

#[test]
#[cfg(test)]
fn writes_leave_in_seq_order_and_a_pause_holds_bytes_and_the_close_behind_them() {
    let artifact = compile("rounds.bls", &BTreeMap::new());
    let run = simulate(&artifact, 8, &FaultSchedule::default());
    assert_eq!(
        timeline(&artifact, &run, SRV),
        [
            (1, "s.opened".to_owned()),
            (5, "s.data".to_owned()),
            (6, "s.closed".to_owned())
        ],
        "paused from round 1 until its resume in round 4"
    );
    assert_eq!(
        rows(&artifact, &run, SRV, "got"),
        [vec![Value::Int(IntValue::U64(0)), Value::Bytes(b"ab".to_vec().into())]],
        "seq 1 waited for seq 0, written a round later"
    );
    assert_eq!(
        events(&artifact, &run, 5, SRV)[0].1,
        [act(CLI, 2), act(SRV, 4)],
        "the write, and the resume that let it through"
    );
    assert_eq!(
        rows(&artifact, &run, SRV, "srv_ended"),
        [vec![text("closed by the peer")]]
    );
    assert_eq!(
        timeline(&artifact, &run, CLI),
        [(1, "up.opened".to_owned()), (3, "up.closed".to_owned())]
    );
    assert_eq!(
        rows(&artifact, &run, CLI, "cli_ended"),
        [vec![text("closed by the program")]]
    );
}

#[test]
#[cfg(test)]
fn a_duplicate_seq_is_a_violation_that_closes_the_connection() {
    let params = [("DUP".to_owned(), ParamBinding::Bool(true))].into_iter().collect();
    let artifact = compile("rounds.bls", &params);
    let run = simulate(&artifact, 8, &FaultSchedule::default());
    assert_eq!(run.stream_violations.len(), 1, "{:?}", run.stream_violations);
    assert_eq!(
        (run.stream_violations[0].node, run.stream_violations[0].tick),
        (CLI, Tick(2))
    );
    assert_eq!(
        rows(&artifact, &run, CLI, "cli_ended"),
        [vec![text("write seq 1 was already written")]]
    );
    assert_eq!(
        rows(&artifact, &run, SRV, "got"),
        [vec![Value::Int(IntValue::U64(0)), Value::Bytes(b"ab".to_vec().into())]],
        "the bytes in order before the duplicate still arrive"
    );
}

#[test]
#[cfg(test)]
fn a_crash_resets_the_connection_at_the_other_end_and_a_restart_begins_with_none() {
    let artifact = compile("pair.bls", &BTreeMap::new());
    let mut faults = FaultSchedule::default();
    faults.crashes.insert(SRV, Tick(2));
    faults.restarts.insert(SRV, Tick(4));
    let run = simulate(&artifact, 6, &faults);
    assert_eq!(
        events(&artifact, &run, 2, CLI),
        [(
            "up.closed".to_owned(),
            vec![Cause::Crash {
                node: SRV,
                tick: Tick(2)
            }]
        )],
        "the client learns in the crash round"
    );
    assert_eq!(
        timeline(&artifact, &run, SRV),
        [(1, "echo.opened".to_owned())],
        "the ping it never read is gone, and the restarted server has no connection"
    );
    assert!(rows(&artifact, &run, CLI, "got").is_empty());
}

#[test]
#[cfg(test)]
fn a_dial_to_a_node_that_is_down_fails() {
    let artifact = compile("pair.bls", &BTreeMap::new());
    let mut faults = FaultSchedule::default();
    faults.crashes.insert(SRV, Tick(1));
    let run = simulate(&artifact, 3, &faults);
    assert_eq!(
        events(&artifact, &run, 1, CLI),
        [("up.failed".to_owned(), vec![act(CLI, 0)])]
    );
    assert_eq!(
        rows(&artifact, &run, CLI, "failures"),
        [vec![Value::Int(IntValue::U64(1))]]
    );
}

#[test]
#[cfg(test)]
fn a_lost_message_resets_the_connection_it_carries_or_fails_the_dial() {
    let artifact = compile("pair.bls", &BTreeMap::new());
    let lost = |from, to, send| Omission {
        from,
        to,
        send: Tick(send),
    };
    let mut faults = FaultSchedule::default();
    faults.omissions.insert(lost(CLI, SRV, 1));
    let run = simulate(&artifact, 4, &faults);
    let reset = vec![Cause::Omission {
        from: CLI,
        to: SRV,
        tick: Tick(1),
    }];
    assert_eq!(
        events(&artifact, &run, 2, CLI),
        [("up.closed".to_owned(), reset.clone())]
    );
    assert_eq!(events(&artifact, &run, 2, SRV), [("echo.closed".to_owned(), reset)]);
    assert!(rows(&artifact, &run, CLI, "got").is_empty());

    let mut faults = FaultSchedule::default();
    faults.omissions.insert(lost(CLI, SRV, 0));
    let run = simulate(&artifact, 4, &faults);
    assert_eq!(
        events(&artifact, &run, 1, CLI),
        [(
            "up.failed".to_owned(),
            vec![
                act(CLI, 0),
                Cause::Omission {
                    from: CLI,
                    to: SRV,
                    tick: Tick(0)
                }
            ]
        )]
    );
    assert!(timeline(&artifact, &run, SRV).is_empty());
}
