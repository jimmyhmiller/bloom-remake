//! Slice 4: the Raft key-value store (`examples/e11_raft_kv.bls`) on real nodes in the cluster simulator. Every
//! client history must be linearizable: without faults, under message loss, network partitions (isolations, splits,
//! one-way cuts) and crashes (some between a WAL append and its sync, some with downtime). After every step an
//! observer checks Raft's safety properties (Fig. 3) on the nodes' state: election safety, state machine safety over
//! the committed prefix, and leader completeness. A directed scenario drives the stale-leader-state case an earlier
//! version got wrong.

use blossom_integration_tests::{scaled, seeds};
use std::collections::BTreeMap;
use std::path::Path;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::RelId;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::durable::DurableSchema;
use blossom_oracle::{Instance, Row};
use blossom_sim::cluster::{ClientProtocol, Cluster, ClusterConfig, ClusterRun, CrashWrites, Observer, Reply};
use blossom_sim::linearize::{KvInput, KvModel, KvOutput, Verdict, check_partitioned};
use blossom_value::Value;
use blossom_value::time::NodeId;
use blossom_value::value::IntValue;

#[cfg(test)]
fn raft_kv() -> BlsArtifact {
    raft_kv_on(3)
}

#[cfg(test)]
fn raft_kv_on(servers: usize) -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/e11_raft_kv.bls");
    let nodes: Vec<NodeSpec> = (1..=servers)
        .map(|n| NodeSpec {
            name: format!("s{n}"),
            role: Some("Server".into()),
        })
        .collect();
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    result.unwrap_or_else(|e| panic!("e11: {e:?}")).0
}

/// The e11 client protocol: put/get/del, answered by put_ok/get_resp/del_ok or redirect.
#[cfg(test)]
struct RaftProtocol {
    put: RelId,
    get: RelId,
    del: RelId,
    put_ok: RelId,
    get_resp: RelId,
    del_ok: RelId,
    redirect: RelId,
}

#[cfg(test)]
impl RaftProtocol {
    fn of(a: &BlsArtifact) -> RaftProtocol {
        let r = |n: &str| a.rel_named(n).unwrap();
        RaftProtocol {
            put: r("put"),
            get: r("get"),
            del: r("del"),
            put_ok: r("put_ok"),
            get_resp: r("get_resp"),
            del_ok: r("del_ok"),
            redirect: r("redirect"),
        }
    }
}

#[cfg(test)]
impl ClientProtocol for RaftProtocol {
    fn request(&self, op: &KvInput, id: u64) -> Result<(RelId, Vec<Value>), String> {
        let key = |k: &[u8]| Value::Str(String::from_utf8(k.to_vec()).unwrap().into());
        let id = Value::Int(IntValue::U64(id));
        Ok(match op {
            KvInput::Put { key: k, val } => (self.put, vec![id, key(k), Value::Bytes(val.as_slice().into())]),
            KvInput::Get { key: k } => (self.get, vec![id, key(k)]),
            KvInput::Delete { key: k } => (self.del, vec![id, key(k)]),
        })
    }

    fn reply(&self, rel: RelId, row: &Row) -> Option<(u64, Reply)> {
        let Value::Int(IntValue::U64(id)) = row.get(1)? else {
            return None;
        };
        let reply = if rel == self.put_ok {
            Reply::Done(KvOutput::PutOk)
        } else if rel == self.get_resp {
            Reply::Done(KvOutput::Value(match row.get(3)? {
                Value::Option(None) => None,
                Value::Option(Some(v)) => match &**v {
                    Value::Bytes(b) => Some(b.to_vec()),
                    _ => return None,
                },
                _ => return None,
            }))
        } else if rel == self.del_ok {
            match row.get(2)? {
                Value::Bool(b) => Reply::Done(KvOutput::Deleted(*b)),
                _ => return None,
            }
        } else if rel == self.redirect {
            match row.get(2)? {
                Value::Option(None) => Reply::Redirect(None),
                Value::Option(Some(v)) => match &**v {
                    Value::Node(n) => Reply::Redirect(Some(NodeId(n.0))),
                    _ => return None,
                },
                _ => return None,
            }
        } else {
            return None;
        };
        Some((*id, reply))
    }
}

/// Raft's safety properties (Fig. 3) over e11's state, checked after every step of the cluster:
/// - election safety: at most one node ever wins a term;
/// - state machine safety: every node's committed prefix agrees with every committed entry seen before, at any node;
/// - leader completeness: a leader holds every entry seen committed in an earlier term;
/// - every entry's recorded `prev` is the term of the entry before it (0 before the first).
#[cfg(test)]
struct RaftSafety {
    log: RelId,
    commit: RelId,
    won: RelId,
    current_term: RelId,
    /// The node that won each term.
    winners: BTreeMap<u64, usize>,
    /// Each committed index: its entry, and the term of the node that was first seen to commit it.
    committed: BTreeMap<u64, (Row, u64)>,
}

#[cfg(test)]
impl RaftSafety {
    fn of(a: &BlsArtifact) -> RaftSafety {
        let r = |n: &str| a.rel_named(n).unwrap();
        RaftSafety {
            log: r("log"),
            commit: r("commit"),
            won: r("won"),
            current_term: r("current_term"),
            winners: BTreeMap::new(),
            committed: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
fn u64_at(row: &Row, col: usize) -> u64 {
    match row.get(col) {
        Some(Value::Int(IntValue::U64(x))) => *x,
        other => panic!("column {col} of {row:?} is {other:?}, not a u64"),
    }
}

#[cfg(test)]
impl Observer for RaftSafety {
    fn observe(&mut self, _now: i64, nodes: &[Option<&Instance>]) -> Result<(), String> {
        for (n, state) in nodes.iter().enumerate() {
            let Some(state) = state else { continue };
            let term = state.rows(self.current_term).map(|r| u64_at(r, 0)).max().unwrap_or(0);
            let log: BTreeMap<u64, &Row> = state.rows(self.log).map(|r| (u64_at(r, 0), r)).collect();
            for (i, entry) in &log {
                let before = if *i == 1 {
                    Some(0)
                } else {
                    log.get(&(i - 1)).map(|r| u64_at(r, 1))
                };
                if let Some(pt) = before
                    && u64_at(entry, 2) != pt
                {
                    return Err(format!(
                        "node {n}: entry {i} records prev {}, but the entry before it has term {pt}",
                        u64_at(entry, 2)
                    ));
                }
            }
            for won in state.rows(self.won).map(|r| u64_at(r, 0)) {
                if let Some(other) = self.winners.insert(won, n)
                    && other != n
                {
                    return Err(format!("election safety: nodes {other} and {n} both won term {won}"));
                }
            }
            let commit = state.rows(self.commit).map(|r| u64_at(r, 0)).max().unwrap_or(0);
            for i in 1..=commit {
                let Some(entry) = log.get(&i) else {
                    return Err(format!(
                        "node {n} has committed through {commit} but holds no entry {i}"
                    ));
                };
                match self.committed.get(&i) {
                    None => {
                        self.committed.insert(i, ((*entry).clone(), term));
                    }
                    Some((seen, _)) if seen != *entry => {
                        return Err(format!(
                            "state machine safety: node {n} committed {entry:?} at {i}, where {seen:?} was committed"
                        ));
                    }
                    Some(_) => {}
                }
            }
            if state.rows(self.won).any(|r| u64_at(r, 0) == term) {
                for (i, (entry, at)) in &self.committed {
                    if *at < term && log.get(i) != Some(&entry) {
                        return Err(format!(
                            "leader completeness: node {n} leads term {term} without {entry:?}, committed at {i} in \
                             term {at}"
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Runs the cluster with `cfg` under the safety observer and checks the history; returns the run.
#[cfg(test)]
fn run_and_check(artifact: &BlsArtifact, schema: &DurableSchema, cfg: ClusterConfig) -> ClusterRun {
    let seed = cfg.seed;
    let mut cluster = Cluster::new(
        artifact,
        schema,
        blossom_value::Seed::from_u64(seed),
        Vec::new(),
        Box::new(RaftProtocol::of(artifact)),
        cfg,
    )
    .unwrap();
    cluster.observe(Box::new(RaftSafety::of(artifact)));
    let run = cluster.run().unwrap();
    if let Some(v) = &run.violation {
        panic!("seed {seed}: {v}\n{}", run.log.join("\n"));
    }
    let (verdict, key) = check_partitioned(&KvModel, &run.history, |i| i.key().to_vec(), 50_000_000);
    assert!(
        verdict == Verdict::Linearizable,
        "seed {seed}: {} at key {:?}\n{}",
        match verdict {
            Verdict::NotLinearizable { .. } => "not linearizable",
            _ => "unknown",
        },
        key.map(|k| String::from_utf8_lossy(&k).into_owned()),
        run.log.join("\n")
    );
    run
}

#[cfg(test)]
fn answered(run: &ClusterRun) -> usize {
    run.history.iter().filter(|o| o.ret.is_some()).count()
}

#[test]
fn raft_kv_serves_linearizably_without_faults() {
    let artifact = raft_kv();
    let schema = DurableSchema::of(artifact.program.get());
    let run = run_and_check(
        &artifact,
        &schema,
        ClusterConfig {
            seed: 1,
            clients: 3,
            keys: 3,
            duration: 3_000_000_000,
            ..ClusterConfig::default()
        },
    );
    assert!(
        answered(&run) > 50,
        "only {} answered of {}",
        answered(&run),
        run.history.len()
    );
}

#[test]
fn raft_kv_is_linearizable_under_loss_partitions_and_crashes() {
    let artifact = raft_kv();
    let schema = DurableSchema::of(artifact.program.get());
    let mut totals = (0usize, 0usize, 0u64, 0u64);
    for seed in seeds(1..=6) {
        let run = run_and_check(
            &artifact,
            &schema,
            ClusterConfig {
                seed,
                clients: 3,
                keys: 3,
                loss_ppm: 20_000,
                nemesis: 400_000_000,
                crashes: true,
                partitions: true,
                timeout: 400_000_000,
                duration: 4_000_000_000,
                ..ClusterConfig::default()
            },
        );
        totals.0 += answered(&run);
        totals.1 += run.history.len() - answered(&run);
        totals.2 += run.crashes;
        totals.3 += run.partitions;
    }
    let (answered, unanswered, crashes, partitions) = totals;
    let all = 1..=6;
    assert!(answered > scaled(100, &all), "only {answered} operations answered");
    assert!(
        unanswered > 0 && crashes > scaled(3, &all) as u64 && partitions > scaled(3, &all) as u64,
        "{unanswered} unanswered, {crashes} crashes, {partitions} partitions: the faults did not bite"
    );
}

#[test]
fn raft_kv_is_safe_on_five_nodes_with_downtime() {
    let artifact = raft_kv_on(5);
    let schema = DurableSchema::of(artifact.program.get());
    for seed in seeds(1..=3) {
        let run = run_and_check(
            &artifact,
            &schema,
            ClusterConfig {
                seed,
                clients: 4,
                keys: 4,
                loss_ppm: 10_000,
                nemesis: 300_000_000,
                crashes: true,
                partitions: true,
                downtime: 1_500_000_000,
                timeout: 400_000_000,
                duration: 4_000_000_000,
                ..ClusterConfig::default()
            },
        );
        assert!(answered(&run) > 20, "seed {seed}: only {} answered", answered(&run));
    }
}

/// The term node `n` leads, if it is a leader.
#[cfg(test)]
fn leads(c: &Cluster, safety: &RaftSafety, n: NodeId) -> Option<u64> {
    let state = c.state(n)?;
    let term = state.rows(safety.current_term).map(|r| u64_at(r, 0)).max()?;
    state.rows(safety.won).any(|r| u64_at(r, 0) == term).then_some(term)
}

/// Waits (up to `within`) for a node of `among` to lead a term above `above`.
#[cfg(test)]
fn await_leader(c: &mut Cluster, safety: &RaftSafety, among: &[NodeId], above: u64, within: i64) -> (NodeId, u64) {
    let end = c.now() + within;
    while c.now() < end {
        if let Some(l) = among
            .iter()
            .filter_map(|&n| leads(c, safety, n).filter(|t| *t > above).map(|t| (n, t)))
            .max_by_key(|x| x.1)
        {
            return l;
        }
        let at = c.now() + 10_000_000;
        c.step_until(at).unwrap();
        assert!(c.violation().is_none(), "{}", c.violation().unwrap_or_default());
    }
    let states: Vec<String> = (0..3)
        .map(|n| {
            let st = c.state(NodeId(n));
            let st = st.as_ref();
            let term = st.and_then(|s| s.rows(safety.current_term).map(|r| u64_at(r, 0)).max());
            let last = st.and_then(|s| s.rows(safety.log).map(|r| (u64_at(r, 0), u64_at(r, 1))).max());
            let commit = st.and_then(|s| s.rows(safety.commit).map(|r| u64_at(r, 0)).max());
            format!(
                "n{n}: term {term:?} last {last:?} commit {commit:?} leads {:?}",
                leads(c, safety, NodeId(n))
            )
        })
        .collect();
    panic!("no leader among {among:?} above term {above}: {states:?}");
}

#[cfg(test)]
fn wait(c: &mut Cluster, d: i64) {
    let at = c.now() + d;
    c.step_until(at).unwrap();
    assert!(c.violation().is_none(), "{}", c.violation().unwrap_or_default());
}

/// Lets clients send operations to `to` for `d`.
#[cfg(test)]
fn write_through(c: &mut Cluster, to: NodeId, d: i64) {
    c.route_clients(to);
    c.pause_clients(false);
    wait(c, d);
    c.pause_clients(true);
}

/// Node `n`'s last log entry, as (term, index).
#[cfg(test)]
fn last_entry(c: &Cluster, safety: &RaftSafety, n: NodeId) -> (u64, u64) {
    c.state(n)
        .and_then(|s| s.rows(safety.log).map(|r| (u64_at(r, 1), u64_at(r, 0))).max())
        .unwrap_or((0, 0))
}

/// Steps until `cond` holds, up to `within`.
#[cfg(test)]
fn await_until(c: &mut Cluster, within: i64, what: &str, cond: impl Fn(&Cluster) -> bool) {
    let end = c.now() + within;
    while !cond(c) {
        assert!(c.now() < end, "{what}: not within {}ms", within / 1_000_000);
        wait(c, 10_000_000);
    }
}

/// Lets clients send operations to `to` until `cond` holds (up to `within`).
#[cfg(test)]
fn write_until(c: &mut Cluster, to: NodeId, within: i64, what: &str, cond: impl Fn(&Cluster) -> bool) {
    c.route_clients(to);
    c.pause_clients(false);
    await_until(c, within, what, cond);
    c.pause_clients(true);
}

/// A leader re-elected after losing leadership must not trust what it recorded about its followers in the earlier
/// term. The scenario: L leads and a follower F acknowledges an uncommitted suffix of L's; another leader C
/// truncates L; L wins again. A stale acknowledgement of F's would let L commit a new entry at those indexes with
/// only itself holding it, and a later leader commits a different entry there (found by the S4 review).
#[test]
fn a_reelected_leader_forgets_its_old_follower_state() {
    let artifact = raft_kv();
    let schema = DurableSchema::of(artifact.program.get());
    for seed in seeds(1..=3) {
        let safety = RaftSafety::of(&artifact);
        let mut c = Cluster::new(
            &artifact,
            &schema,
            blossom_value::Seed::from_u64(seed),
            Vec::new(),
            Box::new(RaftProtocol::of(&artifact)),
            ClusterConfig {
                seed,
                clients: 30,
                keys: 30,
                mix: (60, 40, 0),
                timeout: 300_000_000,
                ..ClusterConfig::default()
            },
        )
        .unwrap();
        c.observe(Box::new(RaftSafety::of(&artifact)));
        c.pause_clients(true);
        let all = [NodeId(0), NodeId(1), NodeId(2)];
        let last = |c: &Cluster, n: NodeId| last_entry(c, &safety, n);
        let s = &safety;
        // 1. A leader L commits a prefix.
        let (l, t_l) = await_leader(&mut c, s, &all, 0, 3_000_000_000);
        write_through(&mut c, l, 300_000_000);
        wait(&mut c, 400_000_000);
        // 2. Isolated, L appends a suffix nobody else has.
        let others: Vec<NodeId> = all.iter().copied().filter(|n| *n != l).collect();
        c.partition(&[&[l], &others]).unwrap();
        let before = last(&c, l);
        write_until(&mut c, l, 1_000_000_000, "L appends", |c| last(c, l) > before);
        // 3. C leads the others, and appends entries it replicates to nobody.
        let (cc, t_c) = await_leader(&mut c, s, &others, t_l, 3_000_000_000);
        let f = others.iter().copied().find(|n| *n != cc).unwrap();
        c.partition(&[&[l], &[cc], &[f]]).unwrap();
        write_until(&mut c, cc, 1_000_000_000, "C appends", |c| last(c, cc).0 == t_c);
        // 4. L and F: L (the longer log) wins, and F takes L's old suffix.
        c.partition(&[&[l, f], &[cc]]).unwrap();
        let (l2, _) = await_leader(&mut c, s, &[l, f], t_c, 5_000_000_000);
        assert_eq!(l2, l, "seed {seed}: F won instead of L; the scenario did not set up");
        await_until(&mut c, 3_000_000_000, "F takes L's suffix", |c| {
            last(c, f) == last(c, l)
        });
        wait(&mut c, 200_000_000);
        // 5. C and F: C (the later last term) wins, and overwrites F.
        c.partition(&[&[cc, f], &[l]]).unwrap();
        let (c2, _) = await_leader(&mut c, s, &[cc, f], 0, 5_000_000_000);
        assert_eq!(c2, cc, "seed {seed}: F won instead of C; the scenario did not set up");
        await_until(&mut c, 3_000_000_000, "C overwrites F", |c| last(c, f) == last(c, cc));
        // 6. Healed, C (the only up-to-date log) leads everyone and truncates L; C's next entry reaches only L.
        c.heal();
        await_until(&mut c, 5_000_000_000, "C leads everyone", |c| {
            leads(c, s, cc).is_some() && last(c, l) == last(c, cc) && last(c, f) == last(c, cc)
        });
        c.partition(&[&[cc, l], &[f]]).unwrap();
        let before = last(&c, f);
        write_until(&mut c, cc, 1_000_000_000, "C's entries reach L only", |c| {
            last(c, l) > before
        });
        wait(&mut c, 100_000_000);
        let t1 = leads(&c, s, cc).unwrap_or(0);
        // 7. L and F: L (the longer log) wins again, and clients write through it.
        c.partition(&[&[l, f], &[cc]]).unwrap();
        let (l3, t2) = await_leader(&mut c, s, &[l, f], t1, 5_000_000_000);
        assert_eq!(l3, l, "seed {seed}: F won instead of L; the scenario did not set up");
        write_through(&mut c, l, 200_000_000);
        wait(&mut c, 100_000_000);
        // 8. C and F take over and write; then everyone converges.
        c.partition(&[&[cc, f], &[l]]).unwrap();
        let (c3, _) = await_leader(&mut c, s, &[cc, f], t2, 5_000_000_000);
        write_through(&mut c, c3, 300_000_000);
        wait(&mut c, 500_000_000);
        c.heal();
        wait(&mut c, 1_000_000_000);
        let run = c.finish();
        let (verdict, key) = check_partitioned(&KvModel, &run.history, |i| i.key().to_vec(), 50_000_000);
        assert!(
            verdict == Verdict::Linearizable,
            "seed {seed}: {verdict:?} at key {:?}",
            key.map(|k| String::from_utf8_lossy(&k).into_owned())
        );
        assert!(answered(&run) > 20, "seed {seed}: only {} answered", answered(&run));
    }
}

/// A longer sweep for soak runs: `RAFT_SEEDS=1-200 cargo test --release --test raft_kv sweep -- --ignored`.
#[test]
#[ignore = "a soak run; pick seeds with RAFT_SEEDS"]
fn sweep() {
    let seeds = std::env::var("RAFT_SEEDS").unwrap_or_else(|_| "1-20".into());
    let (lo, hi) = seeds.split_once('-').unwrap_or((&seeds, &seeds));
    let (lo, hi): (u64, u64) = (lo.parse().unwrap(), hi.parse().unwrap());
    for servers in [3usize, 5] {
        let artifact = raft_kv_on(servers);
        let schema = DurableSchema::of(artifact.program.get());
        for seed in lo..=hi {
            let run = run_and_check(
                &artifact,
                &schema,
                ClusterConfig {
                    seed,
                    clients: 4,
                    keys: 4,
                    loss_ppm: 20_000,
                    nemesis: 250_000_000,
                    crashes: true,
                    partitions: true,
                    downtime: 1_000_000_000,
                    timeout: 400_000_000,
                    duration: 5_000_000_000,
                    ..ClusterConfig::default()
                },
            );
            assert!(answered(&run) > 0, "{servers} servers seed {seed}: nothing answered");
        }
    }
}

/// Power loss of the leader under load, twice: its unsynced writes are gone, it stays down while the others elect
/// a leader, and it comes back from its store. Nothing acknowledged is lost.
#[test]
fn leader_power_loss_loses_nothing_acknowledged() {
    let artifact = raft_kv();
    let schema = DurableSchema::of(artifact.program.get());
    let safety = RaftSafety::of(&artifact);
    let mut c = Cluster::new(
        &artifact,
        &schema,
        blossom_value::Seed::from_u64(9),
        Vec::new(),
        Box::new(RaftProtocol::of(&artifact)),
        ClusterConfig {
            seed: 9,
            clients: 6,
            keys: 6,
            timeout: 300_000_000,
            ..ClusterConfig::default()
        },
    )
    .unwrap();
    c.observe(Box::new(RaftSafety::of(&artifact)));
    let all = [NodeId(0), NodeId(1), NodeId(2)];
    for _ in 0..2 {
        let (l, t) = await_leader(&mut c, &safety, &all, 0, 3_000_000_000);
        wait(&mut c, 500_000_000);
        c.crash(l, CrashWrites::Lost).unwrap();
        let others: Vec<NodeId> = all.iter().copied().filter(|n| *n != l).collect();
        await_leader(&mut c, &safety, &others, t, 3_000_000_000);
        wait(&mut c, 500_000_000);
        c.restart(l).unwrap();
    }
    wait(&mut c, 1_000_000_000);
    let run = c.finish();
    assert_eq!(run.crashes, 2);
    let (verdict, key) = check_partitioned(&KvModel, &run.history, |i| i.key().to_vec(), 50_000_000);
    assert!(verdict == Verdict::Linearizable, "{verdict:?} at key {key:?}");
    assert!(answered(&run) > 50, "only {} answered", answered(&run));
}

/// Runs the fault-free workload for `secs` simulated seconds with constant latency: (ticks, rows examined).
#[cfg(test)]
fn join_work(artifact: &BlsArtifact, schema: &DurableSchema, secs: i64) -> (u64, u64) {
    let cluster = Cluster::new(
        artifact,
        schema,
        blossom_value::Seed::from_u64(1),
        Vec::new(),
        Box::new(RaftProtocol::of(artifact)),
        ClusterConfig {
            seed: 1,
            clients: 6,
            keys: 6,
            latency: (1_000_000, 1_000_000),
            duration: secs * 1_000_000_000,
            ..ClusterConfig::default()
        },
    )
    .unwrap();
    let run = cluster.run().unwrap();
    (run.ticks, run.rows_examined.expect("the engine counts its join work"))
}

/// The engine's work per tick does not grow with the log: a run four times as long (a log four times as long)
/// examines about as many rows per tick. Counted in rows, not time, so the check is exact and machine-independent.
#[test]
#[ignore = "full tier"]
fn join_work_per_tick_is_flat_as_the_log_grows() {
    let artifact = raft_kv();
    let schema = DurableSchema::of(artifact.program.get());
    let (t1, r1) = join_work(&artifact, &schema, 2);
    let (t2, r2) = join_work(&artifact, &schema, 8);
    let (a, b) = (r1 as f64 / t1 as f64, r2 as f64 / t2 as f64);
    assert!(t2 > 3 * t1, "the longer run has {t2} ticks against {t1}");
    assert!(
        b < a * 1.25,
        "{b:.1} rows per tick over 8s against {a:.1} over 2s: the work grows with the log"
    );
}

/// A new leader's first appending tick carries several requests while its last entry is of an older term: the one
/// case where an appended entry's `prev` is the current term rather than the last entry's (found by the S5 review: a
/// mutant recording the last entry's term for every slot passed the rest of this suite).
#[test]
fn a_new_leaders_first_append_is_a_burst() {
    let artifact = raft_kv();
    let schema = DurableSchema::of(artifact.program.get());
    let safety = RaftSafety::of(&artifact);
    let all = [NodeId(0), NodeId(1), NodeId(2)];
    for seed in seeds(1..=3) {
        let mut c = Cluster::new(
            &artifact,
            &schema,
            blossom_value::Seed::from_u64(seed),
            Vec::new(),
            Box::new(RaftProtocol::of(&artifact)),
            ClusterConfig {
                seed,
                clients: 8,
                keys: 4,
                think: 0,
                timeout: 1_000_000_000,
                latency: (1_000_000, 1_000_000),
                ..ClusterConfig::default()
            },
        )
        .unwrap();
        c.observe(Box::new(RaftSafety::of(&artifact)));
        c.pause_clients(true);
        let mut term = 0;
        let mut bursts = 0;
        for _ in 0..3 {
            let (l, t) = await_leader(&mut c, &safety, &all, term, 5_000_000_000);
            // Every client sends to the new leader at the same instant.
            write_through(&mut c, l, 1_500_000);
            wait(&mut c, 400_000_000);
            let st = c.state(l).unwrap();
            if st.rows(safety.log).filter(|r| u64_at(r, 1) == t).count() >= 2 {
                bursts += 1;
            }
            // Depose it: isolated, the others elect; healed, it hears the new term before anyone writes.
            let others: Vec<NodeId> = all.iter().copied().filter(|n| *n != l).collect();
            c.partition(&[&[l], &others]).unwrap();
            let (_, t2) = await_leader(&mut c, &safety, &others, t, 5_000_000_000);
            c.heal();
            wait(&mut c, 300_000_000);
            term = t2 - 1;
        }
        wait(&mut c, 1_000_000_000);
        let run = c.finish();
        let (verdict, key) = check_partitioned(&KvModel, &run.history, |i| i.key().to_vec(), 50_000_000);
        assert!(verdict == Verdict::Linearizable, "seed {seed}: {verdict:?} at {key:?}");
        assert!(bursts > 0, "seed {seed}: no leader appended a burst");
        assert_eq!(
            answered(&run),
            run.history.len(),
            "seed {seed}: operations went unanswered"
        );
    }
}

/// One server is its own majority: it commits and answers alone (found by the S5 review: with no followers no
/// acknowledgement ever counted).
#[test]
fn a_single_server_commits_alone() {
    let artifact = raft_kv_on(1);
    let schema = DurableSchema::of(artifact.program.get());
    let run = run_and_check(
        &artifact,
        &schema,
        ClusterConfig {
            seed: 1,
            clients: 3,
            keys: 3,
            duration: 2_000_000_000,
            ..ClusterConfig::default()
        },
    );
    assert!(
        answered(&run) > 50,
        "only {} answered of {}",
        answered(&run),
        run.history.len()
    );
}

/// The same guarantees on stores with one sync per group commit (`tail_certification = "crc"`, what the etcd
/// comparison runs): crashes (some between a WAL append and its sync) lose nothing acknowledged.
#[test]
fn raft_kv_is_linearizable_on_one_sync_per_commit() {
    let artifact = raft_kv();
    let schema = DurableSchema::of(artifact.program.get());
    for seed in seeds(1..=4) {
        let run = run_and_check(
            &artifact,
            &schema,
            ClusterConfig {
                seed,
                clients: 3,
                keys: 3,
                loss_ppm: 10_000,
                nemesis: 300_000_000,
                crashes: true,
                partitions: true,
                downtime: 800_000_000,
                timeout: 400_000_000,
                duration: 4_000_000_000,
                certification: blossom_store::Certification::Crc,
                ..ClusterConfig::default()
            },
        );
        assert!(run.crashes > 0, "seed {seed}: no crash");
        assert!(answered(&run) > 20, "seed {seed}: only {} answered", answered(&run));
    }
}
