//! Slice 8, item 1: the replicated-log layer (`examples/kafka/raft.bls`), one Raft group per `Group` over a member
//! relation. A harness (`fixtures/raft/groups.bls`) runs five groups of sizes 4, 3, 3, 2 and 1 over four brokers,
//! each leader appending entries, in the cluster simulator under message loss, partitions, crashes (some between a
//! WAL append and its sync) and downtime. An observer checks Raft's safety properties (Fig. 3) per group on the
//! brokers' state: election safety, log matching through each entry's recorded `prev`, state machine safety over
//! committed entries, and leader completeness.

use std::collections::BTreeMap;
use std::path::Path;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_integration_tests::raft_safety::GroupSafety;
use blossom_ir::tick::Row;
use blossom_node::durable::DurableSchema;
use blossom_sim::cluster::{Cluster, ClusterConfig, NoKvClients};
use blossom_value::Value;
use blossom_value::value::IntValue;

#[cfg(test)]
fn compile() -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/raft/groups.bls");
    let nodes: Vec<NodeSpec> = (1..=4)
        .map(|i| NodeSpec {
            name: format!("b{i}"),
            role: Some("Broker".to_owned()),
        })
        .collect();
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
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
        .filter_map(|n| cluster.state(blossom_value::time::NodeId(n)))
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
    for seed in 1..=4 {
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
        let state = self.c.state(n)?;
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
    let artifact = compile();
    let schema = DurableSchema::of(artifact.program.get());
    let mut completed = 0;
    for seed in 1..=12u64 {
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
