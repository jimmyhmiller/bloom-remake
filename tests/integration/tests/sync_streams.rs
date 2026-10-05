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

/// The stream events `node` took in round `t`: the event's relation name, and its causes, as `rel@node:round` for a
/// request or a taken event, `crash node@round` and `omission from->to@round`.
#[cfg(test)]
fn events(artifact: &BlsArtifact, run: &SyncRun, t: u64, node: NodeId) -> Vec<(String, Vec<String>)> {
    let names: BTreeMap<_, _> = artifact
        .program
        .get()
        .rels
        .iter_enumerated()
        .map(|(id, r)| (id, r.name.to_string()))
        .collect();
    let who = |n: &NodeId| artifact.nodes[n.0 as usize].as_str().to_owned();
    let label = |c: &Cause| match c {
        Cause::Request { node, tick, rel, .. } | Cause::Taken { node, tick, rel, .. } => {
            format!("{}@{}:{}", names[rel], who(node), tick.0)
        }
        Cause::Crash { node, tick } => format!("crash {}@{}", who(node), tick.0),
        Cause::Omission { from, to, tick } => format!("omission {}->{}@{}", who(from), who(to), tick.0),
    };
    run.node_tick(Tick(t), node)
        .unwrap()
        .streams
        .iter()
        .map(|e| (names[&e.rel].clone(), e.causes.iter().map(label).collect()))
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
fn labels(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|x| (*x).to_owned()).collect()
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
        ["up.dial@cli:0"],
        "opened by the dial"
    );
    assert_eq!(
        events(&artifact, &run, 2, SRV)[0].1,
        ["up.write@cli:1"],
        "the client's write"
    );
    assert_eq!(
        events(&artifact, &run, 3, CLI)[0].1,
        ["echo.write@srv:2"],
        "the server's echo"
    );
    // One connection, dialed at 0, with traffic each way.
    assert_eq!(run.connections.len(), 1);
    assert_eq!(run.connections[0].dialed, Tick(0));
    assert_eq!(
        run.connections[0]
            .traffic
            .iter()
            .map(|(f, t, s)| (f.0, t.0, s.0))
            .collect::<Vec<_>>(),
        [(0, 1, 1), (1, 0, 2)]
    );
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
        labels(&["up.write@cli:1", "up.write@cli:2", "s.resume@srv:4"]),
        "the writes of seq 1 (held from round 1) and seq 0 (round 2), and the resume that let them through"
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
        [("up.closed".to_owned(), labels(&["crash srv@2"]))],
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
        [("up.failed".to_owned(), labels(&["up.dial@cli:0"]))]
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
    let reset = labels(&["omission cli->srv@1"]);
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
            labels(&["up.dial@cli:0", "omission cli->srv@0"])
        )]
    );
    assert!(timeline(&artifact, &run, SRV).is_empty());
}

/// A fault schedule delaying the stream traffic `from` sends `to` in round `send` by `by` rounds.
#[cfg(test)]
fn delayed(from: NodeId, to: NodeId, send: u64, by: u64) -> FaultSchedule {
    let mut f = FaultSchedule::default();
    f.delays.insert(
        blossom_sim::Delayed {
            batch: Omission {
                from,
                to,
                send: Tick(send),
            },
            path: blossom_sim::Path::Streams,
        },
        by,
    );
    f
}

#[test]
fn a_delayed_write_or_dial_arrives_later() {
    let artifact = compile("pair.bls", &BTreeMap::new());
    // The client's write of round 1 arrives at 3, not 2; the echo follows.
    let run = simulate(&artifact, 8, &delayed(CLI, SRV, 1, 2));
    assert_eq!(
        timeline(&artifact, &run, SRV),
        [(1, "echo.opened".to_owned()), (3, "echo.data".to_owned())]
    );
    assert_eq!(
        timeline(&artifact, &run, CLI),
        [(1, "up.opened".to_owned()), (4, "up.data".to_owned())]
    );
    // The echo of round 2 delayed instead.
    let run = simulate(&artifact, 8, &delayed(SRV, CLI, 2, 2));
    assert_eq!(
        timeline(&artifact, &run, CLI),
        [(1, "up.opened".to_owned()), (4, "up.data".to_owned())]
    );
    // The dial of round 0 delayed: both ends open at 3, and the connection says so.
    let run = simulate(&artifact, 8, &delayed(CLI, SRV, 0, 3));
    assert_eq!(
        timeline(&artifact, &run, SRV),
        [(3, "echo.opened".to_owned()), (4, "echo.data".to_owned())]
    );
    assert_eq!(
        timeline(&artifact, &run, CLI),
        [(3, "up.opened".to_owned()), (5, "up.data".to_owned())]
    );
    assert_eq!(
        run.connections.iter().map(|c| (c.dialed, c.opened)).collect::<Vec<_>>(),
        [(Tick(0), Tick(3))]
    );
    assert!(run.stream_violations.is_empty());
}

#[test]
fn a_delayed_flight_holds_back_the_later_ones_on_its_connection() {
    let artifact = compile("trickle.bls", &BTreeMap::new());
    let chunks = |run: &SyncRun| rows(&artifact, run, SRV, "got");
    let all = vec![
        vec![Value::Int(IntValue::U64(0)), Value::Bytes(b"x".to_vec().into())],
        vec![Value::Int(IntValue::U64(1)), Value::Bytes(b"y".to_vec().into())],
        vec![Value::Int(IntValue::U64(2)), Value::Bytes(b"z".to_vec().into())],
    ];
    // On time: a chunk a round, written at 1, 2 and 3.
    let run = simulate(&artifact, 8, &FaultSchedule::default());
    let arrivals: Vec<u64> = timeline(&artifact, &run, SRV)
        .into_iter()
        .filter(|(_, e)| e == "s.data")
        .map(|(t, _)| t)
        .collect();
    assert_eq!(arrivals, [2, 3, 4]);
    assert_eq!(chunks(&run), all);
    // The flight of round 1 arrives at 4: those of 2 and 3 wait behind it, so nothing arrives before 4.
    let run = simulate(&artifact, 8, &delayed(CLI, SRV, 1, 3));
    let arrivals: Vec<u64> = timeline(&artifact, &run, SRV)
        .into_iter()
        .filter(|(_, e)| e == "s.data")
        .map(|(t, _)| t)
        .collect();
    assert_eq!(arrivals, [4], "one chunk: the three flights arrive together");
    assert_eq!(
        chunks(&run),
        [vec![Value::Int(IntValue::U64(0)), Value::Bytes(b"xyz".to_vec().into())]],
        "in order"
    );
    // A delay past the end of the run: nothing arrives.
    let run = simulate(&artifact, 8, &delayed(CLI, SRV, 1, 8));
    assert!(chunks(&run).is_empty());
}
