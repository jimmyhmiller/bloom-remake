use std::collections::BTreeSet;
use std::sync::Arc;

use blossom_base::{RelId, RuleId};
use blossom_ir::obs::FiringKind;
use blossom_prov::{Firing, GoalId, GoalKey, Premise, ProvGraph, Space};
use blossom_sat::select_backend;
use blossom_sim::{FaultSchedule, Omission};
use blossom_value::Value;
use blossom_value::time::{NodeId, Tick};
use blossom_value::value::IntValue;

use crate::faults::{FailureSpec, canonical, labels};
use crate::hazard::minimal_extensions;
use crate::reach::Preds;

const A: NodeId = NodeId(0);
const B: NodeId = NodeId(1);
const C: NodeId = NodeId(2);

fn name(n: NodeId) -> String {
    ["a", "b", "c"][n.0 as usize].to_owned()
}

fn goal(g: &mut ProvGraph, rel: u32, tick: u64, v: i64) -> GoalId {
    g.goal(
        GoalKey {
            space: Space::Protocol,
            rel: RelId::from_raw(rel),
            node: Some(C),
            tick: Tick(tick),
            row: Arc::from(vec![Value::Int(IntValue::I64(v))]),
        },
        Some(rel),
    )
}

fn firing(premises: Vec<Premise>) -> Firing {
    Firing {
        space: Space::Protocol,
        rule: RuleId::from_raw(0),
        node: Some(C),
        tick: Tick(1),
        kind: FiringKind::Rule,
        premises,
    }
}

fn clock(from: NodeId, to: NodeId, send: u64) -> Premise {
    Premise::Clock {
        from,
        to,
        send: Tick(send),
    }
}

fn extensions(g: &ProvGraph, spec: &FailureSpec, seed: FaultSchedule, target: GoalId) -> BTreeSet<Vec<String>> {
    let preds = Preds::default();
    let mut solver = select_backend("cadical-plain").unwrap();
    minimal_extensions(
        g,
        spec,
        &preds,
        crate::NegSupport::Conservative,
        None,
        solver.as_mut(),
        &seed,
        &[target],
    )
    .unwrap()
    .iter()
    .map(|f| labels(f, &name))
    .collect()
}

fn set(items: &[&[&str]]) -> BTreeSet<Vec<String>> {
    items
        .iter()
        .map(|s| s.iter().map(|x| (*x).to_owned()).collect())
        .collect()
}

/// The LDFI §4.3 shape: a goal with two alternative derivations, one through a->c at 2, one through b->c at 1.
fn two_supports() -> (ProvGraph, GoalId) {
    let mut g = ProvGraph::new();
    let target = goal(&mut g, 0, 3, 1);
    g.add_firing(target, firing(vec![clock(A, C, 2)]));
    g.add_firing(target, firing(vec![clock(B, C, 1)]));
    (g, target)
}

#[test]
fn every_alternative_must_fail() {
    let (g, target) = two_supports();
    let spec = FailureSpec::new(4, 3, 0, 3).unwrap();
    assert_eq!(
        extensions(&g, &spec, FaultSchedule::default(), target),
        set(&[&["O(a,c,2)", "O(b,c,1)"]])
    );
}

#[test]
fn omissions_after_eff_are_not_hypotheses() {
    let (g, target) = two_supports();
    // EFF 2: a->c at 2 cannot be lost, and without crashes nothing falsifies the goal.
    let spec = FailureSpec::new(4, 2, 0, 3).unwrap();
    assert!(extensions(&g, &spec, FaultSchedule::default(), target).is_empty());
}

#[test]
fn crashes_are_tried_at_their_latest_useful_tick() {
    let (g, target) = two_supports();
    // One crash, no omissions: crashing a at 2 kills its send at 2, but b's send at 1 needs b crashed at 1, and two
    // crashes are over budget.
    let one = FailureSpec::new(4, 0, 1, 3).unwrap();
    assert!(extensions(&g, &one, FaultSchedule::default(), target).is_empty());
    let two = FailureSpec::new(4, 0, 2, 3).unwrap();
    // Minimal models: a crashes at 2 (not earlier), b at 1.
    assert_eq!(
        extensions(&g, &two, FaultSchedule::default(), target),
        set(&[&["C(a,2)", "C(b,1)"]])
    );
    // With omissions allowed too, every mix is minimal.
    let mixed = FailureSpec::new(4, 3, 2, 3).unwrap();
    assert_eq!(
        extensions(&g, &mixed, FaultSchedule::default(), target),
        set(&[
            &["O(a,c,2)", "O(b,c,1)"],
            &["C(a,2)", "O(b,c,1)"],
            &["C(b,1)", "O(a,c,2)"],
            &["C(a,2)", "C(b,1)"],
        ])
    );
}

#[test]
fn hypotheses_extend_the_seed() {
    let (g, target) = two_supports();
    let spec = FailureSpec::new(4, 3, 0, 3).unwrap();
    let mut seed = FaultSchedule::default();
    seed.omissions.insert(Omission {
        from: A,
        to: C,
        send: Tick(2),
    });
    assert_eq!(
        extensions(&g, &spec, seed.clone(), target),
        set(&[&["O(a,c,2)", "O(b,c,1)"]])
    );
    // A seed that already falsifies the goal gives nothing new.
    seed.omissions.insert(Omission {
        from: B,
        to: C,
        send: Tick(1),
    });
    assert!(extensions(&g, &spec, seed, target).is_empty());
}

#[test]
fn a_premise_chain_is_falsified_anywhere() {
    // target <- mid (sent a->c at 1) <- leaf; target also needs b->c at 1.
    let mut g = ProvGraph::new();
    let leaf = goal(&mut g, 2, 1, 0);
    g.set_leaf(leaf);
    let mid = goal(&mut g, 1, 2, 0);
    g.add_firing(mid, firing(vec![clock(A, C, 1), Premise::Goal(leaf)]));
    let target = goal(&mut g, 0, 3, 0);
    g.add_firing(target, firing(vec![Premise::Goal(mid), clock(B, C, 1)]));
    let spec = FailureSpec::new(4, 2, 0, 3).unwrap();
    assert_eq!(
        extensions(&g, &spec, FaultSchedule::default(), target),
        set(&[&["O(a,c,1)"], &["O(b,c,1)"]])
    );
}

#[test]
fn a_derivation_through_its_own_goal_is_no_support() {
    // p <- q (cycle) and p <- a->c at 1: the cyclic alternative does not protect p.
    let mut g = ProvGraph::new();
    let p = goal(&mut g, 0, 1, 0);
    let q = goal(&mut g, 1, 1, 0);
    g.add_firing(p, firing(vec![Premise::Goal(q)]));
    g.add_firing(p, firing(vec![clock(A, C, 1)]));
    g.add_firing(q, firing(vec![Premise::Goal(p)]));
    let spec = FailureSpec::new(3, 2, 0, 3).unwrap();
    assert_eq!(
        extensions(&g, &spec, FaultSchedule::default(), p),
        set(&[&["O(a,c,1)"]])
    );
}

#[test]
fn fault_sets_normalize_and_compare_by_removed_clocks() {
    let spec = FailureSpec::new(4, 3, 1, 3).unwrap();
    let mut f = FaultSchedule::default();
    f.crashes.insert(A, Tick(2));
    f.omissions.insert(Omission {
        from: A,
        to: B,
        send: Tick(2),
    });
    f.omissions.insert(Omission {
        from: A,
        to: B,
        send: Tick(1),
    });
    let f = canonical(f);
    assert_eq!(
        labels(&f, &name),
        ["C(a,2)", "O(a,b,1)"],
        "the crash implies the later omission"
    );
    assert!(spec.admits(&f));
    // C(a,2) removes a->b and a->c from 2 to EOT, plus the omission.
    assert_eq!(spec.removed_clocks(&f).len(), 2 * 3 + 1);
    let mut bad = FaultSchedule::default();
    bad.omissions.insert(Omission {
        from: A,
        to: A,
        send: Tick(1),
    });
    assert!(!spec.admits(&bad), "a node's messages to itself cannot be lost");
    assert!(FailureSpec::new(4, 4, 0, 3).is_err(), "EFF must be before EOT");
}
