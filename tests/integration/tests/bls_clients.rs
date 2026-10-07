//! S21: client roles (docs/design/CLIENTS.md). Browser tabs are members of a `client` role that hold rules; the server
//! and the tabs see each other's links come and go (`Browser.connected`, `Server.disconnected`). In simulation a client
//! member is a node of the deployment: the chat fixture runs on the oracle and the engine alike, through a tab's
//! crash and restart.

use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::{BlsError, NodeSpec};
use blossom_node::EngineEvaluator;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_value::Value;
use blossom_value::time::{Duration, NodeId, Tick};

#[cfg(test)]
fn compile(nodes: &[(&str, &str)]) -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/clients/chat.bls");
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
        Err(BlsError::Rejected(d)) => panic!("chat.bls: {:?}", d.iter().map(|x| &x.message).collect::<Vec<_>>()),
        Err(e) => panic!("chat.bls: {e}"),
    }
}

#[cfg(test)]
fn line(a: &BlsArtifact, node: &str, t: u64, text: &str) -> InputEvent {
    InputEvent {
        node: a.node_id(node).unwrap(),
        tick: Tick(t),
        rel: a.rel_named("line").unwrap(),
        row: Arc::from(vec![Value::str(text)]),
    }
}

#[test]
fn tabs_chat_through_the_server_on_the_oracle_and_the_engine() {
    let a = compile(&[("s", "Server"), ("b1", "Browser"), ("b2", "Browser")]);
    let (b1, b2, s) = (
        a.node_id("b1").unwrap(),
        a.node_id("b2").unwrap(),
        a.node_id("s").unwrap(),
    );
    let sim = BlsSim::new(&a, blossom_value::Seed::from_u64(0)).unwrap();
    let round = Duration::from_nanos(1_000_000);
    let inputs = vec![
        line(&a, "b1", 1, "hi"),
        line(&a, "b2", 2, "yo"),
        line(&a, "b1", 6, "back?"),
    ];
    // b2 crashes at round 3 and restarts at round 5: its volatile `seen` is gone, and the server greets it again.
    let mut faults = FaultSchedule::default();
    faults.crashes.insert(b2, Tick(3));
    faults.restarts.insert(b2, Tick(5));
    let last = Tick(9);
    let reference = sim.run(&inputs, last, round, &faults, false).unwrap();
    let cfg = blossom_engine::EngineConfig {
        roles: a.roles.clone(),
        node_names: a.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(a.program.clone(), cfg);
    let mine = sim.run_on(&engine, &inputs, last, round, &faults, false).unwrap();
    for (t, (x, y)) in reference.rounds.iter().zip(&mine.rounds).enumerate() {
        for (n, (p, q)) in x.iter().zip(y).enumerate() {
            assert_eq!(
                p.instance, q.instance,
                "round {t}, node {n}: the oracle and the engine differ"
            );
        }
    }
    let rows = |t: usize, node: NodeId, rel: &str| -> Vec<Vec<Value>> {
        let mut out: Vec<Vec<Value>> = reference.rounds[t][node.0 as usize]
            .instance
            .rows(a.rel_named(rel).unwrap())
            .map(|r| r.to_vec())
            .collect();
        out.sort();
        out
    };
    let said = |who: NodeId, text: &str| vec![Value::Node(who), Value::str(text)];
    // Both tabs are online from the first round, and each sees the server's link.
    assert_eq!(rows(0, s, "online"), vec![vec![Value::Node(b1)], vec![Value::Node(b2)]]);
    assert_eq!(rows(0, b1, "up"), vec![vec![Value::Node(s)]]);
    // "hi" (b1, round 1) reaches the server in round 2 and both tabs in round 3; "yo" (b2, round 2) reaches b1 in
    // round 4. b2 crashed in round 3, so it saw neither.
    assert_eq!(rows(3, b1, "seen"), vec![said(b1, "hi")]);
    assert_eq!(rows(4, b1, "seen"), vec![said(b1, "hi"), said(b2, "yo")]);
    assert_eq!(rows(2, b2, "seen"), Vec::<Vec<Value>>::new());
    // b2's crash reaches the server in its crash round (the delete holds from the next); its restart brings it back
    // and the greeting replays the log into its fresh state.
    assert_eq!(rows(4, s, "online"), vec![vec![Value::Node(b1)]]);
    assert_eq!(rows(5, s, "online"), vec![vec![Value::Node(b1)], vec![Value::Node(b2)]]);
    assert_eq!(rows(6, b2, "seen"), vec![said(b1, "hi"), said(b2, "yo")]);
    // A line after the restart reaches both.
    assert_eq!(
        rows(8, b2, "seen"),
        vec![said(b1, "back?"), said(b1, "hi"), said(b2, "yo")]
    );
    assert_eq!(rows(8, b1, "seen"), rows(8, b2, "seen"));
}

/// A tab in production is not part of the deployment: it runs as a client id (outside the deployment's node ids) with
/// its role given, sends to the server, and the server answers it by that id.
#[test]
fn a_tab_runs_as_a_client_member_outside_the_deployment() {
    use blossom_node::eval::Evaluator;
    use blossom_oracle::{Delivery, Instance, Send, TickInput};
    let a = compile(&[("s", "Server")]);
    let p = a.program.get();
    let browser = p
        .roles
        .iter_enumerated()
        .find(|(_, r)| r.name.to_string() == "Browser")
        .map(|(id, _)| id)
        .unwrap();
    let server = a.node_id("s").unwrap();
    let me = NodeId::client(server, 3).unwrap();
    let cfg = |client_role| blossom_engine::EngineConfig {
        roles: a.roles.clone(),
        node_names: a.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        client_role,
        ..blossom_engine::EngineConfig::default()
    };
    let tick =
        |ev: &EngineEvaluator, node, events: &[(blossom_base::RelId, blossom_oracle::Row)], delivered: &[Delivery]| {
            ev.tick(&TickInput {
                node,
                incarnation: 1,
                tick: Tick(0),
                now: blossom_value::time::Instant(0),
                carried: &Instance::default(),
                events,
                delivered,
                ingress: &[],
                capture: false,
                blobs: &blossom_value::NoBlobs,
            })
            .unwrap()
        };
    let (line_rel, say, heard) = (
        a.rel_named("line").unwrap(),
        a.rel_named("say").unwrap(),
        a.rel_named("heard").unwrap(),
    );
    // The tab's rule sends `say` to the server; the server's rules do not run on it.
    let tab = EngineEvaluator::new(a.program.clone(), cfg(Some(browser)));
    let out = tick(&tab, me, &[(line_rel, Arc::from(vec![Value::str("x")]))], &[]);
    let said = Send {
        rel: say,
        to: server,
        row: Arc::from(vec![Value::Node(server), Value::str("x")]),
    };
    assert_eq!(out.outbox.into_iter().collect::<Vec<_>>(), vec![said.clone()]);
    // The server learns of the tab from its link and its message, and answers it by its id.
    let connected = a.rel_named("Browser.connected").unwrap();
    let srv = EngineEvaluator::new(a.program.clone(), cfg(None));
    let out = tick(
        &srv,
        server,
        &[(connected, Arc::from(vec![Value::Node(me), Value::Bool(false)]))],
        &[Delivery {
            rel: said.rel,
            from: me,
            row: said.row.clone(),
        }],
    );
    assert!(
        out.outbox
            .iter()
            .any(|s| s.rel == heard && s.to == me && s.row.get(1) == Some(&Value::Node(me))),
        "{:?}",
        out.outbox
    );
    // A client member is written with its role, serial and server.
    let node_t = p.rels.get(connected).unwrap().schema.cols[0].ty;
    let names: Vec<Arc<str>> = a.nodes.iter().map(|n| Arc::from(n.as_str())).collect();
    assert_eq!(
        blossom_ir::printer::to_string_text(p, &Value::Node(me), node_t, &names),
        "Browser#3@s"
    );
}
