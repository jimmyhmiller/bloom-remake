//! S20: user-defined lattices (LANGUAGE §11.8). A product and a method of every class run alike on the oracle and the
//! engine; the law harness tests true class claims and refutes false ones with a counterexample (BLS0704); and a
//! stable read guarded by its threshold is monotone, while a banged one is a point of order.

use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::{BlsError, NodeSpec};
use blossom_node::EngineEvaluator;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_value::time::{Duration, NodeId, Tick};
use blossom_value::value::IntValue;
use blossom_value::{ExternRegistry, Value};
use blossom_verify::laws::{self, Outcome};

#[cfg(test)]
fn path(rel: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/lattices")
        .join(rel)
        .to_str()
        .expect("a UTF-8 path")
        .to_owned()
}

#[cfg(test)]
fn nodes() -> [NodeSpec; 1] {
    [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }]
}

#[cfg(test)]
fn compile(rel: &str) -> BlsArtifact {
    let (result, _) = compile_file(&path(rel), &nodes());
    result.unwrap_or_else(|e| panic!("{rel}: {e:?}")).0
}

#[cfg(test)]
fn u(n: u64) -> Value {
    Value::Int(IntValue::U64(n))
}

/// (tick, relation, row): votes on questions 1 and 2, then their quorums (2 each), so both pass.
#[cfg(test)]
fn events(a: &BlsArtifact) -> Vec<InputEvent> {
    let vote = |t: u64, q: u64, v: u64, yes: bool| (t, "vote", vec![u(q), u(v), Value::Bool(yes)]);
    let fix = |t: u64, q: u64, n: u64| (t, "fix", vec![u(q), u(n)]);
    [
        vote(1, 1, 10, true),
        vote(1, 1, 11, false),
        vote(1, 2, 10, true),
        fix(2, 1, 2),
        vote(2, 1, 12, true),
        vote(3, 2, 12, true),
        fix(3, 2, 2),
        vote(4, 2, 13, false),
    ]
    .into_iter()
    .map(|(t, rel, row)| InputEvent {
        node: NodeId(0),
        tick: Tick(t),
        rel: a.rel_named(rel).unwrap_or_else(|| panic!("no relation {rel}")),
        row: Arc::from(row),
    })
    .collect()
}

#[test]
fn products_and_methods_agree_with_the_oracle_at_every_tick() {
    let a = compile("user.bls");
    let sim = BlsSim::new(&a, blossom_value::Seed::from_u64(0)).unwrap();
    let round = Duration::from_nanos(1_000_000);
    let (inputs, last) = (events(&a), Tick(6));
    let reference = sim.run(&inputs, last, round, &FaultSchedule::default(), false).unwrap();
    let cfg = blossom_engine::EngineConfig {
        roles: a.roles.clone(),
        node_names: a.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(a.program.clone(), cfg);
    let mine = sim
        .run_on(&engine, &inputs, last, round, &FaultSchedule::default(), false)
        .unwrap();
    for (t, (x, y)) in reference.rounds.iter().zip(&mine.rounds).enumerate() {
        assert_eq!(
            x[0].instance, y[0].instance,
            "tick {t}: the oracle and the engine differ"
        );
    }
    let rows = |t: usize, rel: &str| -> Vec<Vec<Value>> {
        let mut out: Vec<Vec<Value>> = reference.rounds[t][0]
            .instance
            .rows(a.rel_named(rel).unwrap())
            .map(|r| r.to_vec())
            .collect();
        out.sort();
        out
    };
    // Question 1 passes at tick 2 (quorum 2, yes 10 and 12), and with it question 0 (their merge); question 2 at
    // tick 3. The stable reads, guarded in a header, a view's body and an `if`, all give the quorum.
    assert_eq!(rows(1, "passed_with"), Vec::<Vec<Value>>::new());
    assert_eq!(rows(2, "passed_with"), vec![vec![u(0), u(2)], vec![u(1), u(2)]]);
    assert_eq!(
        rows(3, "passed_with"),
        vec![vec![u(0), u(2)], vec![u(1), u(2)], vec![u(2), u(2)]]
    );
    assert_eq!(rows(3, "passed_too"), rows(3, "passed_with"));
    // The exact reads see every state: question 2's lead is 2 at tick 3 and 1 after its no vote.
    let leads = rows(4, "leads");
    assert!(leads.contains(&vec![u(2), Value::Int(IntValue::I64(1))]), "{leads:?}");
    assert!(leads.contains(&vec![u(1), Value::Int(IntValue::I64(1))]), "{leads:?}");
    // `reveal!` of a product is the struct of its fields' reveals.
    let revealed = rows(1, "revealed");
    let set = |xs: &[u64]| Value::Set(Arc::new(xs.iter().map(|x| u(*x)).collect()));
    assert!(
        revealed.contains(&vec![
            u(1),
            Value::Struct(Arc::from(vec![set(&[10]), set(&[11]), Value::none()]))
        ]),
        "{revealed:?}"
    );
    // The merge of the two questions (question 0) holds both quorums' agreement and every vote.
    let merged = rows(4, "revealed");
    assert!(
        merged.contains(&vec![
            u(0),
            Value::Struct(Arc::from(vec![set(&[10, 12]), set(&[11, 13]), Value::some(u(2))]))
        ]),
        "{merged:?}"
    );
    // `ayes_with` (both questions' yes voters) joined with question 1's yes voters.
    assert_eq!(
        rows(4, "both"),
        vec![vec![Value::Lattice(match set(&[10, 12]) {
            Value::Set(s) => blossom_value::value::LatValue::Set(s),
            other => panic!("{other:?}"),
        })]]
    );
}

#[test]
fn the_law_harness_tests_true_claims_and_refutes_false_ones() {
    let externs = Arc::new(ExternRegistry::new());
    let good = compile("user.bls");
    let report = laws::check(&good, externs.clone()).unwrap();
    assert_eq!(report.lattices.len(), 1);
    let tally = &report.lattices[0];
    assert!(tally.merge.checked > 0);
    assert_eq!(
        tally.claims.len(),
        6,
        "every classed method but `lead` claims something"
    );
    for c in &tally.claims {
        match &c.outcome {
            Outcome::Tested(t) => assert!(t.checked > 0, "{} was tested on no case", c.method),
            other => panic!("{}: {other:?}", c.method),
        }
    }
    assert!(laws::diagnostics(&report, &good).is_empty());

    let bad = compile("refuted.bls");
    let report = laws::check(&bad, externs).unwrap();
    let outcomes: Vec<(String, bool)> = report.lattices[0]
        .claims
        .iter()
        .map(|c| (c.method.to_string(), matches!(c.outcome, Outcome::Refuted(_))))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("has_a", false),
            ("small", true),
            ("shrinking", true),
            ("count", true),
            ("first", true),
            ("members", true),
            ("common", true)
        ]
        .map(|(m, r)| (m.to_owned(), r))
    );
    let diags = laws::diagnostics(&report, &bad);
    assert_eq!(diags.iter().filter(|d| d.code.as_str() == "BLS0704").count(), 6);
    assert!(
        diags.iter().all(|d| d.primary.is_some()),
        "every refutation points at its method"
    );
}

#[test]
fn a_guarded_stable_read_is_monotone_and_a_banged_one_is_not() {
    let (result, _) = compile_file(&path("monotone_reads.bls"), &nodes());
    let diags = match result {
        Err(BlsError::Rejected(d)) => d,
        other => panic!("monotone_reads.bls compiled: {:?}", other.map(|(_, w)| w)),
    };
    let flagged: Vec<String> = diags
        .iter()
        .filter(|d| d.code.as_str() == "BLS0702")
        .map(|d| d.message.clone())
        .collect();
    assert_eq!(flagged.len(), 2, "{flagged:?}");
    assert!(flagged.iter().any(|m| m.contains("`caps`")), "{flagged:?}");
    assert!(flagged.iter().any(|m| m.contains("`spares`")), "{flagged:?}");
}
