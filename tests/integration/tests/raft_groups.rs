//! Slice 8, item 1: the replicated-log layer (`examples/kafka/raft.bls`), one Raft group per `Group` over a member
//! relation. A harness (`fixtures/raft/groups.bls`) runs five groups of sizes 4, 3, 3, 2 and 1 over four brokers,
//! each leader appending entries, in the cluster simulator under message loss, partitions, crashes (some between a
//! WAL append and its sync) and downtime. An observer checks Raft's safety properties (Fig. 3) per group on the
//! brokers' state: election safety, log matching through each entry's recorded `prev`, state machine safety over
//! committed entries, and leader completeness. Directed scenarios check a re-elected leader's follower state and the
//! quiescence of idle groups (no heartbeats while every member is caught up; a leader's broker that dies or restarts
//! is replaced; a member cut off from a quiet leader does not depose it).

use blossom_integration_tests::seeds;
use std::collections::BTreeMap;
use std::path::Path;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file_with;
use blossom_front::api::NodeSpec;
use blossom_integration_tests::raft_safety::GroupSafety;
use blossom_ir::tick::Row;
use blossom_node::durable::DurableSchema;
use blossom_sim::cluster::{Cluster, ClusterConfig, NoKvClients};
use blossom_value::Value;
use blossom_value::value::IntValue;

#[cfg(test)]
fn compile() -> BlsArtifact {
    compile_with(true)
}

/// The harness, with CheckQuorum and PreVote on or off (`RAFT_CHECK_QUORUM`, `RAFT_PRE_VOTE`).
#[cfg(test)]
fn compile_with(check_quorum: bool) -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/raft/groups.bls");
    let nodes: Vec<NodeSpec> = (1..=4)
        .map(|i| NodeSpec {
            name: format!("b{i}"),
            role: Some("Broker".to_owned()),
        })
        .collect();
    // PreVote goes with CheckQuorum (a leader refuses pre-votes).
    let params = [
        (
            "RAFT_CHECK_QUORUM".to_owned(),
            blossom_front::api::ParamBinding::Bool(check_quorum),
        ),
        (
            "RAFT_PRE_VOTE".to_owned(),
            blossom_front::api::ParamBinding::Bool(check_quorum),
        ),
    ]
    .into_iter()
    .collect();
    let (result, _) = compile_file_with(path.to_str().unwrap(), &nodes, &params);
    result.unwrap_or_else(|e| panic!("groups.bls: {e:?}")).0
}

#[cfg(test)]
fn u64_at(row: &Row, col: usize) -> u64 {
    match row.get(col) {
        Some(Value::Int(IntValue::U64(x))) => *x,
        Some(Value::Int(IntValue::U8(x))) => u64::from(*x),
        other => panic!("column {col} of {row:?} is {other:?}, not a u64"),
    }
}

/// Runs the harness; returns each group's highest commit index the observer saw, and how many configurations group
/// `moving` committed (on the broker holding the most).
#[cfg(test)]
fn run(seed: u64, faults: bool) -> (BTreeMap<Value, u64>, usize) {
    let artifact = compile();
    let schema = DurableSchema::of(artifact.program.get());
    let cfg = ClusterConfig {
        seed,
        clients: 0,
        loss_ppm: if faults { 20_000 } else { 0 },
        nemesis: if faults { 400_000_000 } else { 0 },
        crashes: faults,
        partitions: faults,
        downtime: 300_000_000,
        duration: 6_000_000_000,
        ..ClusterConfig::default()
    };
    let mut cluster = Cluster::new(
        &artifact,
        &schema,
        blossom_value::Seed::from_u64(seed),
        Vec::new(),
        Box::new(NoKvClients),
        cfg,
    )
    .unwrap();
    let (safety, observer) = GroupSafety::of(&artifact).unwrap().shared();
    cluster.observe(observer);
    cluster.run_until(6_000_000_000).unwrap();
    let run = cluster.run_so_far();
    assert!(
        run.violation.is_none(),
        "seed {seed}: {:?}\n{}",
        run.violation,
        run.log.join("\n")
    );
    let moving = Value::Tuple(vec![Value::Bytes(b"moving".to_vec().into()), Value::Int(IntValue::I32(4))].into());
    let (rlog, commit) = (
        artifact.rel_named("rlog").unwrap(),
        artifact.rel_named("commit").unwrap(),
    );
    let configs = (0..4u32)
        .filter_map(|n| cluster.state(blossom_value::time::NodeId(n)).unwrap())
        .map(|s| {
            let c = s.rows(commit).find(|r| r[0] == moving).map_or(0, |r| u64_at(r, 1));
            s.rows(rlog)
                .filter(|r| r[0] == moving && u64_at(r, 4) == 3 && u64_at(r, 1) <= c)
                .count()
        })
        .max()
        .unwrap_or(0);
    let progress = safety.borrow().progress.clone();
    (progress, configs)
}

#[test]
fn every_group_elects_and_commits_without_faults() {
    let (progress, configs) = run(1, false);
    assert_eq!(progress.len(), 6, "{progress:?}");
    for (g, c) in &progress {
        assert!(*c > 50, "group {g:?} committed only {c} entries");
    }
    assert!(configs >= 8, "group moving committed only {configs} configurations");
}

#[test]
fn every_group_stays_safe_under_loss_partitions_and_crashes() {
    for seed in seeds(1..=4) {
        let (progress, configs) = run(seed, true);
        assert_eq!(progress.len(), 6, "seed {seed}: {progress:?}");
        for (g, c) in &progress {
            assert!(*c > 10, "seed {seed}: group {g:?} committed only {c} entries");
        }
        assert!(
            configs >= 3,
            "seed {seed}: group moving committed only {configs} configurations"
        );
    }
}

/// The directed scenario of the S4 review (`raft_kv.rs`), on group `left` (b1, b2, b3; b4 is kept apart): a leader
/// re-elected after losing leadership must not trust what it recorded about its followers in an earlier term. L
/// leads and a follower F acknowledges an uncommitted suffix of L's; another leader C truncates L; L wins again. A
/// stale acknowledgement of F's would let L commit a new entry at those indexes with only itself holding it, and a
/// later leader commits a different entry there.
#[cfg(test)]
struct Directed<'a> {
    c: Cluster<'a>,
    g: Value,
    rterm: blossom_base::RelId,
    won: blossom_base::RelId,
    rlog: blossom_base::RelId,
    hold: blossom_base::RelId,
    release: blossom_base::RelId,
}

#[cfg(test)]
impl Directed<'_> {
    fn leads(&self, n: blossom_value::time::NodeId) -> Option<u64> {
        let state = self.c.state(n).unwrap()?;
        let term = state
            .rows(self.rterm)
            .filter(|r| r[0] == self.g)
            .map(|r| u64_at(r, 1))
            .max()?;
        state
            .rows(self.won)
            .any(|r| r[0] == self.g && u64_at(r, 1) == term)
            .then_some(term)
    }

    /// Node `n`'s last entry of the group, as (term, index).
    fn last(&self, n: blossom_value::time::NodeId) -> (u64, u64) {
        self.c
            .state(n)
            .unwrap()
            .and_then(|s| {
                s.rows(self.rlog)
                    .filter(|r| r[0] == self.g)
                    .map(|r| (u64_at(r, 2), u64_at(r, 1)))
                    .max()
            })
            .unwrap_or((0, 0))
    }

    fn wait(&mut self, d: i64) {
        let at = self.c.now() + d;
        self.c.step_until(at).unwrap();
        assert!(
            self.c.violation().is_none(),
            "{}",
            self.c.violation().unwrap_or_default()
        );
    }

    fn await_until(&mut self, within: i64, what: &str, cond: impl Fn(&Self) -> bool) {
        let end = self.c.now() + within;
        while !cond(self) {
            assert!(self.c.now() < end, "{what}: not within {}ms", within / 1_000_000);
            self.wait(10_000_000);
        }
    }

    fn await_leader(
        &mut self,
        among: &[blossom_value::time::NodeId],
        above: u64,
        within: i64,
    ) -> (blossom_value::time::NodeId, u64) {
        let end = self.c.now() + within;
        loop {
            if let Some(l) = among
                .iter()
                .filter_map(|&n| self.leads(n).filter(|t| *t > above).map(|t| (n, t)))
                .max_by_key(|x| x.1)
            {
                return l;
            }
            assert!(
                self.c.now() < end,
                "no leader of {:?} among {among:?} above term {above}",
                self.g
            );
            self.wait(10_000_000);
        }
    }

    /// Lets `n` propose (it proposes only while it leads) until `cond` holds, then holds it again.
    fn write_until(&mut self, n: blossom_value::time::NodeId, within: i64, what: &str, cond: impl Fn(&Self) -> bool) {
        self.c
            .input(n, self.release, std::sync::Arc::from(vec![self.g.clone()]))
            .unwrap();
        self.await_until(within, what, cond);
        self.c
            .input(n, self.hold, std::sync::Arc::from(vec![self.g.clone()]))
            .unwrap();
        self.wait(1_000_000);
    }
}

#[test]
fn a_reelected_leader_forgets_its_old_follower_state() {
    use blossom_value::time::NodeId;
    // The scenario has a leader cut off go on appending for seconds, which CheckQuorum would stop (a leader that
    // hears from no majority steps down), and then win again by raising its term, which PreVote would stop: Raft
    // without them, whose safety this checks.
    let artifact = compile_with(false);
    let schema = DurableSchema::of(artifact.program.get());
    let mut completed = 0;
    // Whether a seed sets the scenario up depends on who wins its elections: the fast tier tries the seeds in order
    // until one does, the full tier runs them all.
    for seed in 1..=12 {
        if completed > 0 && !blossom_integration_tests::full_tier() {
            break;
        }
        let c = Cluster::new(
            &artifact,
            &schema,
            blossom_value::Seed::from_u64(seed),
            Vec::new(),
            Box::new(NoKvClients),
            ClusterConfig {
                seed,
                clients: 0,
                ..ClusterConfig::default()
            },
        )
        .unwrap();
        let r = |n: &str| artifact.rel_named(n).unwrap();
        let mut d = Directed {
            c,
            g: Value::Tuple(std::sync::Arc::from(vec![
                Value::Bytes(std::sync::Arc::from(&b"left"[..])),
                Value::Int(IntValue::I32(0)),
            ])),
            rterm: r("rterm"),
            won: r("won"),
            rlog: r("rlog"),
            hold: r("hold"),
            release: r("release"),
        };
        d.c.observe(Box::new(GroupSafety::of(&artifact).unwrap()));
        let b4 = [NodeId(3)];
        for n in 0..4 {
            d.c.input(NodeId(n), d.hold, std::sync::Arc::from(vec![d.g.clone()]))
                .unwrap();
        }
        let all = [NodeId(0), NodeId(1), NodeId(2)];
        // 1. A leader L commits a prefix.
        let (l, t_l) = d.await_leader(&all, 0, 3_000_000_000);
        let start = d.last(l);
        d.write_until(l, 1_000_000_000, "L commits a prefix", |d| d.last(l).1 > start.1 + 3);
        d.wait(400_000_000);
        // 2. Isolated, L appends a long suffix nobody else has: longer than what C will have when it truncates L, so
        // F's later acknowledgement of it points past that truncation.
        let others: Vec<NodeId> = all.iter().copied().filter(|n| *n != l).collect();
        d.c.partition(&[&[l], &others, &b4]).unwrap();
        let before = d.last(l);
        d.write_until(l, 2_000_000_000, "L appends", |d| d.last(l).1 > before.1 + 10);
        // 3. C leads the others, and appends entries it replicates to nobody.
        let (cc, t_c) = d.await_leader(&others, t_l, 3_000_000_000);
        let f = others.iter().copied().find(|n| *n != cc).unwrap();
        d.c.partition(&[&[l], &[cc], &[f], &b4]).unwrap();
        d.write_until(cc, 1_000_000_000, "C appends", |d| {
            d.last(cc).0 == t_c && d.last(cc).1 > d.last(f).1
        });
        // 4. L and F: L (the longer log) wins, and F takes L's old suffix.
        d.c.partition(&[&[l, f], &[cc], &b4]).unwrap();
        let (l2, _) = d.await_leader(&[l, f], t_c, 5_000_000_000);
        if l2 != l {
            continue; // the scenario did not set up with this seed
        }
        d.await_until(3_000_000_000, "F takes L's suffix", |d| d.last(f) == d.last(l));
        d.wait(200_000_000);
        // 5. C and F: C (the later last term) wins, and overwrites F.
        d.c.partition(&[&[cc, f], &[l], &b4]).unwrap();
        let (c2, _) = d.await_leader(&[cc, f], 0, 5_000_000_000);
        if c2 != cc {
            continue;
        }
        d.await_until(3_000_000_000, "C overwrites F", |d| d.last(f) == d.last(cc));
        // 6. Healed (but b4), C's log leads everyone and truncates L. L's term grew while it was alone, so there is
        // a new election, which C or F wins (they hold the same log): the winner is C from here on.
        d.c.partition(&[&all, &b4]).unwrap();
        d.await_until(5_000_000_000, "C's log leads everyone", |d| {
            (d.leads(cc).is_some() || d.leads(f).is_some()) && d.last(l) == d.last(cc) && d.last(f) == d.last(cc)
        });
        let (cc, f) = if d.leads(cc).is_some() { (cc, f) } else { (f, cc) };
        d.c.partition(&[&[cc, l], &[f], &b4]).unwrap();
        let before = d.last(f);
        d.write_until(cc, 1_000_000_000, "C's entries reach L only", |d| d.last(l) > before);
        d.wait(100_000_000);
        let t1 = d.leads(cc).unwrap_or(0);
        // 7. L and F: L (the longer log) wins again, and appends through itself.
        d.c.partition(&[&[l, f], &[cc], &b4]).unwrap();
        let (l3, t2) = d.await_leader(&[l, f], t1, 5_000_000_000);
        if l3 != l {
            continue;
        }
        let before = d.last(l);
        d.write_until(l, 2_000_000_000, "L appends again", |d| d.last(l).1 > before.1 + 14);
        d.wait(100_000_000);
        // 8. C and F take over and append; then everyone converges. The observer checks every step.
        d.c.partition(&[&[cc, f], &[l], &b4]).unwrap();
        let (c3, _) = d.await_leader(&[cc, f], t2, 5_000_000_000);
        let before = d.last(c3);
        d.write_until(c3, 1_000_000_000, "C appends again", |d| d.last(c3).1 > before.1 + 2);
        d.wait(500_000_000);
        d.c.heal();
        d.wait(1_000_000_000);
        assert!(d.c.violation().is_none(), "seed {seed}: {:?}", d.c.violation());
        completed += 1;
    }
    assert!(completed >= 1, "the scenario never set up");
}

/// The harness's groups.
#[cfg(test)]
fn groups() -> Vec<Value> {
    [
        ("all", 0),
        ("left", 0),
        ("right", 1),
        ("pair", 2),
        ("solo", 3),
        ("moving", 4),
    ]
    .iter()
    .map(|(n, p)| {
        Value::Tuple(std::sync::Arc::from(vec![
            Value::Bytes(std::sync::Arc::from(n.as_bytes())),
            Value::Int(IntValue::I32(*p)),
        ]))
    })
    .collect()
}

/// A directed run of the harness (CheckQuorum and PreVote on) with every group held at every broker, once each has
/// a leader: no group proposes, so every one goes idle.
#[cfg(test)]
fn quiet_run<'a>(artifact: &'a BlsArtifact, schema: &'a DurableSchema, seed: u64) -> Directed<'a> {
    quiet_run_with(artifact, schema, seed, ClusterConfig::default().latency)
}

/// `quiet_run` with this one-way message latency.
#[cfg(test)]
fn quiet_run_with<'a>(
    artifact: &'a BlsArtifact,
    schema: &'a DurableSchema,
    seed: u64,
    latency: (i64, i64),
) -> Directed<'a> {
    let c = Cluster::new(
        artifact,
        schema,
        blossom_value::Seed::from_u64(seed),
        Vec::new(),
        Box::new(NoKvClients),
        ClusterConfig {
            seed,
            clients: 0,
            latency,
            record: blossom_integration_tests::sim_record(&format!("quiet-seed{seed}")),
            ..ClusterConfig::default()
        },
    )
    .unwrap();
    let r = |n: &str| artifact.rel_named(n).unwrap();
    let mut d = Directed {
        c,
        g: groups()[0].clone(),
        rterm: r("rterm"),
        won: r("won"),
        rlog: r("rlog"),
        hold: r("hold"),
        release: r("release"),
    };
    d.c.observe(Box::new(GroupSafety::of(artifact).unwrap()));
    d.wait(1_500_000_000);
    hold_everything(&mut d, &[0, 1, 2, 3]);
    d
}

#[cfg(test)]
fn hold_everything(d: &mut Directed<'_>, nodes: &[u32]) {
    for g in groups() {
        for &n in nodes {
            d.c.input(
                blossom_value::time::NodeId(n),
                d.hold,
                std::sync::Arc::from(vec![g.clone()]),
            )
            .unwrap();
        }
    }
}

/// Every group's leader and term among `among` (the highest term led), or `None`.
#[cfg(test)]
fn leaders(d: &mut Directed<'_>, among: &[u32]) -> Vec<(Value, Option<(u32, u64)>)> {
    let mut out = Vec::new();
    for g in groups() {
        d.g = g.clone();
        let l = among
            .iter()
            .filter_map(|&n| d.leads(blossom_value::time::NodeId(n)).map(|t| (n, t)))
            .max_by_key(|x| x.1);
        out.push((g, l));
    }
    out
}

/// Idle groups stop their heartbeats: what is left is each broker's liveness (4 brokers x 3 peers x 20 per second),
/// and no group elects again while quiet. A group that gets a proposal wakes and commits it under the same leader.
#[test]
fn idle_groups_go_quiet_and_wake_for_a_proposal() {
    let artifact = compile();
    let schema = DurableSchema::of(artifact.program.get());
    let mut d = quiet_run(&artifact, &schema, 1);
    d.wait(1_500_000_000);
    let before = leaders(&mut d, &[0, 1, 2, 3]);
    assert!(
        before.iter().all(|(_, l)| l.is_some()),
        "a group has no leader: {before:?}"
    );
    let m0 = d.c.run_so_far().messages;
    d.wait(3_000_000_000);
    let per_second = (d.c.run_so_far().messages - m0) / 3;
    assert!(
        per_second < 400,
        "{per_second} messages a second while every group is idle"
    );
    assert_eq!(
        leaders(&mut d, &[0, 1, 2, 3]),
        before,
        "a group elected again while quiet"
    );
    // "left" gets proposals again: it wakes, appends and commits.
    d.g = groups()[1].clone();
    let (l, t) = before[1].1.unwrap();
    let l = blossom_value::time::NodeId(l);
    let start = d.last(l);
    d.write_until(l, 1_000_000_000, "left appends", |d| d.last(l).1 > start.1 + 3);
    let others: Vec<_> = (0..3u32).map(blossom_value::time::NodeId).filter(|n| *n != l).collect();
    d.await_until(1_000_000_000, "left's followers take its entries", |d| {
        others.iter().all(|f| d.last(*f) == d.last(l))
    });
    assert_eq!(d.leads(l), Some(t), "left's leader changed");
}

/// A quiet group that wakes keeps its leader whenever in a CheckQuorum period it wakes: its members acknowledged
/// nothing while quiet, but their brokers were alive, which counts for the period. (The leader once counted them only
/// while quiet, so a wake shortly before a check, the members' acknowledgements still on their way, deposed it.)
/// The latency is high so that the acknowledgements take a good part of a period to arrive.
#[test]
fn a_quiet_group_that_wakes_keeps_its_leader() {
    let artifact = compile();
    let schema = DurableSchema::of(artifact.program.get());
    let mut d = quiet_run_with(&artifact, &schema, 5, (20_000_000, 40_000_000));
    d.wait(1_500_000_000);
    let before = leaders(&mut d, &[0, 1, 2, 3]);
    d.g = groups()[1].clone();
    let (l, t) = before[1].1.unwrap();
    let l = blossom_value::time::NodeId(l);
    // Wakes at phases across the check period (300 ms): quiet for a while, then a proposal.
    for k in 0..16 {
        d.wait(400_000_000 + k * 19_000_000);
        let start = d.last(l);
        d.write_until(l, 1_000_000_000, "left appends", |d| d.last(l).1 > start.1);
        d.wait(320_000_000);
        assert_eq!(d.leads(l), Some(t), "left's leader stepped down after wake {k}");
    }
}

/// A quiet group whose leader's broker crashes elects another among the rest once the broker's liveness stops.
#[test]
fn a_quiet_groups_crashed_leader_is_replaced() {
    let artifact = compile();
    let schema = DurableSchema::of(artifact.program.get());
    let mut d = quiet_run(&artifact, &schema, 2);
    d.wait(1_500_000_000);
    let before = leaders(&mut d, &[0, 1, 2, 3]);
    // The leader of "all" (every broker is a member) crashes.
    let (l, t) = before[0].1.unwrap();
    d.c.crash(
        blossom_value::time::NodeId(l),
        blossom_sim::cluster::CrashWrites::Random,
    )
    .unwrap();
    let rest: Vec<u32> = (0..4).filter(|n| *n != l).collect();
    d.g = groups()[0].clone();
    let among: Vec<_> = rest.iter().map(|n| blossom_value::time::NodeId(*n)).collect();
    d.await_leader(&among, t, 2_000_000_000);
}

/// A quiet group's leader that restarts at once (a new incarnation, its leadership forgotten) is replaced, or wins
/// again, in a later term: its followers do not go on counting the restarted broker's liveness as their leader's.
#[test]
fn a_quiet_groups_restarted_leader_is_followed_by_an_election() {
    let artifact = compile();
    let schema = DurableSchema::of(artifact.program.get());
    let mut d = quiet_run(&artifact, &schema, 3);
    d.wait(1_500_000_000);
    let before = leaders(&mut d, &[0, 1, 2, 3]);
    let (l, t) = before[0].1.unwrap();
    let ln = blossom_value::time::NodeId(l);
    d.c.crash(ln, blossom_sim::cluster::CrashWrites::Random).unwrap();
    d.wait(1_000_000);
    d.c.restart(ln).unwrap();
    hold_everything(&mut d, &[l]);
    d.g = groups()[0].clone();
    let all: Vec<_> = (0..4u32).map(blossom_value::time::NodeId).collect();
    d.await_leader(&all, t, 2_500_000_000);
}

/// A quiet group's leader that stops hearing its members (cut off one way: it still reaches them) steps down
/// (CheckQuorum); its members still hear its broker alive, so it tells them it was deposed, and they elect another.
#[test]
fn a_quiet_leader_that_hears_no_member_is_replaced() {
    let artifact = compile();
    let schema = DurableSchema::of(artifact.program.get());
    let mut d = quiet_run(&artifact, &schema, 6);
    d.wait(1_500_000_000);
    let before = leaders(&mut d, &[0, 1, 2, 3]);
    // "left" is b1, b2 and b3: its followers' messages no longer reach its leader.
    let (l, t) = before[1].1.unwrap();
    let followers: Vec<_> = (0..3u32).filter(|n| *n != l).map(blossom_value::time::NodeId).collect();
    for f in &followers {
        d.c.cut(*f, blossom_value::time::NodeId(l)).unwrap();
    }
    d.g = groups()[1].clone();
    d.await_leader(&followers, t, 2_500_000_000);
}

/// A member cut off from a quiet leader (both ways) finds its timeout passing, but the others, who hear the leader's
/// broker alive, refuse its pre-votes: no group's term moves.
#[test]
fn a_member_cut_off_from_a_quiet_leader_does_not_depose_it() {
    let artifact = compile();
    let schema = DurableSchema::of(artifact.program.get());
    let mut d = quiet_run(&artifact, &schema, 4);
    d.wait(1_500_000_000);
    let before = leaders(&mut d, &[0, 1, 2, 3]);
    // "left" is b1, b2 and b3: cut its leader off from one follower.
    let (l, _) = before[1].1.unwrap();
    let f = (0..3u32).find(|n| *n != l).unwrap();
    d.c.cut(blossom_value::time::NodeId(l), blossom_value::time::NodeId(f))
        .unwrap();
    d.c.cut(blossom_value::time::NodeId(f), blossom_value::time::NodeId(l))
        .unwrap();
    d.wait(3_000_000_000);
    assert_eq!(leaders(&mut d, &[0, 1, 2, 3]), before, "a group's term moved");
}
