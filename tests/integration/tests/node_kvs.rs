//! Slice 3: the e01 key-value store on the sans-IO node over the simulated filesystem. An acknowledged put survives
//! every crash; a reply is released only after its tick's WAL record is synced (Invariant R); recovery replays the
//! WAL on top of the checkpoint and never reuses a tick number.

use blossom_integration_tests::{scaled, seeds};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::durable::DurableSchema;
use blossom_node::manual::ManualDriver;
use blossom_node::recovery::{self, StoreSpec};
use blossom_node::{Executor, Node, NodeConfig, OracleExecutor, ReleasedTick};
use blossom_oracle::{Ingress, Oracle};
use blossom_store::{OpenMode, SimFs, StoreIdentity, Vfs, WriteFate};
use blossom_value::Value;
use blossom_value::time::{Instant, NodeId, Tick};
use blossom_value::value::{IntValue, SessionId};

#[cfg(test)]
fn kvs() -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/e01_kvs.bls");
    let (result, _) = compile_file(
        path.to_str().unwrap(),
        &[NodeSpec {
            name: "s1".into(),
            role: Some("Server".into()),
        }],
    );
    result.unwrap_or_else(|e| panic!("e01: {e:?}")).0
}

#[cfg(test)]
fn identity() -> StoreIdentity {
    StoreIdentity {
        store_uuid: [0; 16],
        deployment_id: [1; 16],
        program_id: [2; 16],
        node_name: "s1".into(),
        principal: "spiffe://test/kvs/Server/s1".into(),
        format: recovery::FORMAT,
        directory_digest: [3; 16],
    }
}

struct Kvs {
    artifact: BlsArtifact,
    oracle: Arc<Oracle>,
    schema: DurableSchema,
    names: Arc<[Arc<str>]>,
    dir: PathBuf,
    certification: blossom_store::Certification,
}

#[cfg(test)]
impl Kvs {
    fn new() -> Kvs {
        let artifact = kvs();
        let oracle = Arc::new(
            Oracle::new(artifact.program.clone())
                .unwrap()
                .with_roles(artifact.roles.clone())
                .with_seed(blossom_value::Seed([9; 16]))
                .unwrap()
                .with_node_names(artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect())
                .unwrap(),
        );
        let schema = DurableSchema::of(artifact.program.get());
        let names: Arc<[Arc<str>]> = artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect();
        Kvs {
            artifact,
            oracle,
            schema,
            names,
            dir: PathBuf::from("/data/s1"),
            certification: blossom_store::Certification::Strict,
        }
    }

    /// The same node with stores that certify their WAL tails as `c`.
    fn certified(mut self, c: blossom_store::Certification) -> Kvs {
        self.certification = c;
        self
    }

    fn rel(&self, name: &str) -> blossom_base::RelId {
        self.artifact
            .rel_named(name)
            .unwrap_or_else(|| panic!("no relation {name}"))
    }

    /// Opens (recovers) the store on `fs` and boots the node.
    fn boot<'a>(&'a self, fs: &SimFs, wall: i64) -> ManualDriver<'a, Box<dyn Executor>> {
        // A clone of a `SimFs` is another handle on the same filesystem.
        let fs: Arc<dyn Vfs> = Arc::new(fs.clone());
        let opened = recovery::open(
            fs,
            &StoreSpec {
                dir: self.dir.clone(),
                identity: identity(),
                mode: OpenMode::InitFresh,
                certification: self.certification,
                database: blossom_store::lsm::LsmOptions::default(),
            },
            &self.artifact.program,
            self.names.clone(),
            Instant(wall),
            7,
        )
        .unwrap();
        let mut cfg = NodeConfig::new(NodeId(0), self.artifact.roles.first().copied().flatten());
        cfg.halt = self.artifact.halt;
        let exec: Box<dyn Executor> = Box::new(OracleExecutor::new(self.oracle.clone()));
        let node = Node::boot(cfg, &self.artifact.program, exec, opened.boot.clone()).unwrap();
        ManualDriver::new(
            node,
            self.artifact.program.get(),
            &self.schema,
            self.names.clone(),
            opened,
        )
    }

    fn put(&self, session: u64, id: u64, key: &str, val: &[u8]) -> Ingress {
        Ingress {
            rel: self.rel("put"),
            session: SessionId(session),
            row: Arc::from(vec![
                Value::Node(NodeId(0)),
                Value::Int(IntValue::U64(id)),
                Value::Str(key.into()),
                Value::Bytes(val.into()),
            ]),
        }
    }

    fn get(&self, session: u64, id: u64, key: &str) -> Ingress {
        Ingress {
            rel: self.rel("get"),
            session: SessionId(session),
            row: Arc::from(vec![
                Value::Node(NodeId(0)),
                Value::Int(IntValue::U64(id)),
                Value::Str(key.into()),
            ]),
        }
    }

    fn store(&self, d: &ManualDriver<'_, Box<dyn Executor>>) -> Vec<(String, Vec<u8>)> {
        let rel = self.rel("store");
        d.released_image()
            .unwrap()
            .rows
            .get(&rel)
            .into_iter()
            .flatten()
            .map(|r| match (&r[0], &r[1]) {
                (Value::Str(k), Value::Bytes(v)) => (k.to_string(), v.to_vec()),
                other => panic!("store row {other:?}"),
            })
            .collect()
    }
}

/// The replies of released ticks, as (channel name, request id, rendered row).
#[cfg(test)]
fn replies(k: &Kvs, released: &[ReleasedTick]) -> Vec<(String, u64, String)> {
    let p = k.artifact.program.get();
    let mut out = Vec::new();
    for t in released {
        assert!(t.sends.is_empty(), "the KVS sends only to clients");
        for e in &t.egress {
            let name = p.rels.get(e.rel).unwrap().name.to_string();
            let id = match &e.row[1] {
                Value::Int(IntValue::U64(id)) => *id,
                other => panic!("reply id {other:?}"),
            };
            out.push((name, id, format!("{:?}", &e.row[1..])));
        }
    }
    out
}

#[test]
fn a_put_is_acknowledged_and_read_back() {
    let k = Kvs::new();
    let fs = SimFs::default();
    let mut d = k.boot(&fs, 1_000);
    // Tick 0 is boot.
    assert!(
        d.run_until_quiescent(Instant(1_000))
            .unwrap()
            .iter()
            .all(|t| t.egress.is_empty())
    );
    d.node.offer_ingress(k.put(1, 10, "a", b"x"));
    let released = d.run_until_quiescent(Instant(2_000)).unwrap();
    assert_eq!(
        replies(&k, &released)
            .iter()
            .map(|r| (r.0.as_str(), r.1))
            .collect::<Vec<_>>(),
        [("put_ok", 10)]
    );
    d.node.offer_ingress(k.get(1, 11, "a"));
    let released = d.run_until_quiescent(Instant(3_000)).unwrap();
    let r = replies(&k, &released);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, "get_resp");
    assert!(r[0].2.contains("120"), "the value b\"x\" (120) is returned: {}", r[0].2);
    assert_eq!(k.store(&d), [("a".to_string(), b"x".to_vec())]);
}

/// `recovered()` holds iff an earlier incarnation's boot became durable: a crash before the first boot tick's WAL
/// record is synced leaves a store that boots fresh again (so `bootstrap fresh` runs), and one after it recovers.
#[test]
fn a_crash_before_the_first_boot_is_durable_boots_fresh_again() {
    let k = Kvs::new();
    let mut fs = SimFs::default();
    let d = k.boot(&fs, 1_000);
    assert!(!d.node.recovered());
    d.crash_before_sync(Instant(1_000)).unwrap();
    fs.crash(&mut |_| WriteFate::Lost).unwrap();
    let mut d = k.boot(&fs, 2_000);
    assert!(!d.node.recovered(), "the first boot never became durable");
    assert_eq!(d.meta().restarts, 2);
    let now = d.node.last_now();
    d.run_until_quiescent(now).unwrap();
    fs.crash(&mut |_| WriteFate::Lost).unwrap();
    let d = k.boot(&fs, 3_000);
    assert!(d.node.recovered(), "the second incarnation's boot was durable");
}

#[test]
fn acknowledged_puts_survive_a_crash_and_ticks_are_never_reused() {
    let k = Kvs::new();
    let mut fs = SimFs::default();
    let last_tick;
    {
        let mut d = k.boot(&fs, 1_000);
        d.run_until_quiescent(Instant(1_000)).unwrap();
        for i in 0..20u64 {
            d.node
                .offer_ingress(k.put(1, i, &format!("k{}", i % 7), format!("v{i}").as_bytes()));
            let r = d.run_until_quiescent(Instant(2_000 + i as i64)).unwrap();
            assert_eq!(replies(&k, &r).len(), 1);
        }
        last_tick = d.node.next_tick();
        assert_eq!(k.store(&d).len(), 7);
    }
    // Every acknowledged write was synced: a crash that loses every unsynced write keeps them all.
    fs.crash(&mut |_| WriteFate::Lost).unwrap();
    let mut d = k.boot(&fs, 500);
    assert!(
        d.node.next_tick() > last_tick,
        "boot tick {:?} reuses a tick before {last_tick:?}",
        d.node.next_tick()
    );
    assert_eq!(d.meta().restarts, 2);
    let store = k.store(&d);
    assert_eq!(store.len(), 7);
    for (key, val) in &store {
        let i: u64 = String::from_utf8(val.clone())
            .unwrap()
            .strip_prefix('v')
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(key, &format!("k{}", i % 7));
        assert!(i >= 13, "{key} holds the last put to it");
    }
    // The clock never goes back across incarnations even when the wall clock does: the boot instant is after the
    // last instant the previous incarnation used.
    assert!(
        d.node.last_now() > Instant(2_019),
        "boot instant {:?}",
        d.node.last_now()
    );
    let now = d.node.last_now();
    d.run_until_quiescent(now).unwrap();
}

#[test]
fn recovery_replays_the_wal_after_a_checkpoint() {
    let k = Kvs::new();
    let mut fs = SimFs::default();
    {
        let mut d = k.boot(&fs, 0);
        d.run_until_quiescent(Instant(0)).unwrap();
        for i in 0..10u64 {
            d.node.offer_ingress(k.put(1, i, &format!("k{i}"), b"old"));
            d.run_until_quiescent(Instant(10 + i as i64)).unwrap();
        }
        d.flush().unwrap();
        for i in 0..5u64 {
            d.node.offer_ingress(k.put(1, 100 + i, &format!("k{i}"), b"new"));
            d.run_until_quiescent(Instant(100 + i as i64)).unwrap();
        }
    }
    fs.crash(&mut |_| WriteFate::Lost).unwrap();
    let d = k.boot(&fs, 0);
    let mut store = k.store(&d);
    store.sort();
    assert_eq!(store.len(), 10);
    for (key, val) in store {
        let i: u64 = key.strip_prefix('k').unwrap().parse().unwrap();
        assert_eq!(val, if i < 5 { b"new".to_vec() } else { b"old".to_vec() }, "{key}");
    }
}

#[test]
fn a_torn_tail_loses_only_the_unacknowledged_tick() {
    let k = Kvs::new();
    let mut fs = SimFs::default();
    {
        let mut d = k.boot(&fs, 0);
        d.run_until_quiescent(Instant(0)).unwrap();
        d.node.offer_ingress(k.put(1, 1, "a", b"acked"));
        let r = d.run_until_quiescent(Instant(1)).unwrap();
        assert_eq!(replies(&k, &r).len(), 1);
        // Compute the next tick but never report its sync: its reply must stay parked.
        d.node.offer_ingress(k.put(1, 2, "b", b"unacked"));
        let fx = d.node.run_tick(Instant(2)).unwrap();
        assert!(fx.wal.is_some());
        assert!(
            d.node.release_ready().unwrap().is_empty(),
            "a tick with a WAL record waits for its sync"
        );
    }
    // Even a torn partial write of anything unsynced leaves the acknowledged state.
    fs.crash(&mut |_| WriteFate::Torn { sectors: 1 }).unwrap();
    let d = k.boot(&fs, 0);
    assert_eq!(k.store(&d), [("a".to_string(), b"acked".to_vec())]);
}

/// Invariant R under arbitrary interleavings of ticks and sync reports: released ticks come out in order, and a
/// tick is released iff every earlier tick with a record is synced.
#[test]
fn invariant_r_holds_under_random_sync_schedules() {
    use proptest::prelude::*;
    let k = Kvs::new();
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(64));
    runner
        .run(&proptest::collection::vec((any::<bool>(), 0u8..4), 1..40), |steps| {
            let fs = SimFs::default();
            let opened = recovery::open(
                Arc::new(fs.clone()),
                &StoreSpec {
                    dir: k.dir.clone(),
                    identity: identity(),
                    mode: OpenMode::InitFresh,
                    certification: k.certification,
                    database: blossom_store::lsm::LsmOptions::default(),
                },
                &k.artifact.program,
                k.names.clone(),
                Instant(0),
                1,
            )
            .unwrap();
            let cfg = NodeConfig::new(NodeId(0), k.artifact.roles.first().copied().flatten());
            let exec: Box<dyn Executor> = Box::new(OracleExecutor::new(k.oracle.clone()));
            let mut node = Node::boot(cfg, &k.artifact.program, exec, opened.boot).unwrap();
            let mut computed: Vec<(Tick, bool)> = Vec::new();
            let mut released: Vec<Tick> = Vec::new();
            let mut synced: Option<Tick> = None;
            let mut now = 0;
            for (i, (sync, kind)) in steps.into_iter().enumerate() {
                now += 1;
                if sync {
                    // Report a sync of everything computed so far with a record.
                    if let Some(t) = computed.iter().rev().find(|(_, w)| *w).map(|(t, _)| *t) {
                        synced = Some(t);
                        released.extend(node.wal_synced(t).unwrap().iter().map(|r| r.tick));
                    }
                } else {
                    match kind {
                        0 => node.offer_ingress(k.get(1, i as u64, "a")),
                        _ => node.offer_ingress(k.put(1, i as u64, "a", &[kind, i as u8])),
                    };
                    let fx = node.run_tick(Instant(now)).unwrap();
                    computed.push((fx.tick, fx.wal.is_some()));
                    released.extend(node.release_ready().unwrap().iter().map(|r| r.tick));
                }
                // The released ticks are exactly the longest prefix allowed by Invariant R.
                let allowed: Vec<Tick> = computed
                    .iter()
                    .take_while(|(t, w)| !*w || synced.is_some_and(|s| *t <= s))
                    .map(|(t, _)| *t)
                    .collect();
                prop_assert_eq!(&released, &allowed);
            }
            Ok(())
        })
        .unwrap();
}

/// Crash at every durable syscall of a workload (with checkpoints in it), recover, and check that every put
/// acknowledged before the crash is in the recovered store, and nothing that was never sent is.
#[test]
fn every_crash_point_keeps_every_acknowledged_put() {
    crash_points(&Kvs::new());
}

/// The same with one sync per group commit (`Certification::Crc`, etcd's model): the guarantee to clients is the same.
#[test]
fn every_crash_point_keeps_every_acknowledged_put_with_one_sync_per_commit() {
    crash_points(&Kvs::new().certified(blossom_store::Certification::Crc));
}

#[cfg(test)]
fn crash_points(k: &Kvs) {
    let fs = SimFs::default();
    fs.enable_crash_recording().unwrap();
    // For each acknowledged put: (key, value, the number of recorded cuts when its reply was released).
    let mut acked: Vec<(String, Vec<u8>, usize)> = Vec::new();
    {
        let mut d = k.boot(&fs, 0);
        d.run_until_quiescent(Instant(0)).unwrap();
        for i in 0..12u64 {
            let key = format!("k{}", i % 4);
            let val = format!("v{i}").into_bytes();
            d.node.offer_ingress(k.put(1, i, &key, &val));
            // The ack point is the moment the reply is released, measured in durable syscalls so far.
            let mut released_at = Vec::new();
            d.run_until_quiescent_with(Instant(1 + i as i64), &mut |t| {
                if !t.egress.is_empty() {
                    released_at.push(fs.cut_count().unwrap());
                }
            })
            .unwrap();
            assert_eq!(released_at.len(), 1);
            acked.push((key, val, released_at[0]));
            if i % 5 == 4 {
                d.flush().unwrap();
            }
        }
    }
    let cuts = fs.recorded_cuts().unwrap();
    assert!(cuts.len() > 30, "only {} cuts", cuts.len());
    // The value of each key must be the last put acknowledged by the cut, or any later put to it (a put may be
    // durable before its reply is released).
    let check = |d: &ManualDriver<'_, Box<dyn Executor>>, c: usize, what: &str| {
        let store: std::collections::BTreeMap<String, Vec<u8>> = k.store(d).into_iter().collect();
        for key in (0..4).map(|i| format!("k{i}")) {
            let last_acked = acked.iter().rfind(|(k2, _, at)| *k2 == key && *at <= c + 1);
            let allowed: Vec<&Vec<u8>> = acked
                .iter()
                .filter(|(k2, _, at)| *k2 == key && last_acked.is_none_or(|(_, _, a)| at >= a))
                .map(|(_, v, _)| v)
                .collect();
            match (store.get(&key), last_acked) {
                (None, None) => {}
                (Some(v), _) => assert!(
                    allowed.contains(&v),
                    "cut {c} ({what}): {key} = {v:?}, allowed {allowed:?}"
                ),
                (None, Some((_, v, _))) => panic!("cut {c} ({what}): {key} lost its acknowledged value {v:?}"),
            }
        }
    };
    // Each unsynced write's fate at a crash: all lost, all kept, all torn, or a mix.
    type Fate = fn(usize) -> WriteFate;
    let fates: [(&str, Fate); 4] = [
        ("lost", |_| WriteFate::Lost),
        ("kept", |_| WriteFate::Survive),
        ("torn", |_| WriteFate::Torn { sectors: 1 }),
        ("mixed", |i| match i % 3 {
            0 => WriteFate::Survive,
            1 => WriteFate::Lost,
            _ => WriteFate::Torn { sectors: 1 },
        }),
    ];
    for (c, cut) in cuts.into_iter().enumerate() {
        for (what, fate) in fates {
            let mut image = cut.fork().unwrap();
            let mut i = 0;
            image
                .crash(&mut |_| {
                    i += 1;
                    fate(i)
                })
                .unwrap();
            // A cut before the store was initialized has nothing to recover; `InitFresh` starts it again.
            if c % 7 != 0 || what != "mixed" {
                check(&k.boot(&image, 0), c, what);
                continue;
            }
            // Also crash during this recovery, at each of its durable syscalls, and recover again.
            image.enable_crash_recording().unwrap();
            check(&k.boot(&image, 0), c, what);
            for (r, mut again) in image.recorded_cuts().unwrap().into_iter().enumerate() {
                again.crash(&mut |_| WriteFate::Lost).unwrap();
                check(
                    &k.boot(&again, 0),
                    c,
                    &format!("{what}, then a crash at recovery step {r}"),
                );
            }
        }
    }
}

/// The e01 client protocol for the cluster simulator.
#[cfg(test)]
struct E01Protocol {
    put: blossom_base::RelId,
    get: blossom_base::RelId,
    del: blossom_base::RelId,
    put_ok: blossom_base::RelId,
    get_resp: blossom_base::RelId,
    del_ok: blossom_base::RelId,
}

#[cfg(test)]
impl E01Protocol {
    fn of(a: &BlsArtifact) -> E01Protocol {
        let r = |n: &str| a.rel_named(n).unwrap();
        E01Protocol {
            put: r("put"),
            get: r("get"),
            del: r("del"),
            put_ok: r("put_ok"),
            get_resp: r("get_resp"),
            del_ok: r("del_ok"),
        }
    }
}

#[cfg(test)]
impl blossom_sim::cluster::ClientProtocol for E01Protocol {
    fn request(
        &self,
        op: &blossom_sim::linearize::KvInput,
        id: u64,
    ) -> Result<(blossom_base::RelId, Vec<Value>), String> {
        use blossom_sim::linearize::KvInput;
        let key = |k: &[u8]| Value::Str(String::from_utf8(k.to_vec()).unwrap().into());
        let id = Value::Int(IntValue::U64(id));
        Ok(match op {
            KvInput::Put { key: k, val } => (self.put, vec![id, key(k), Value::Bytes(val.as_slice().into())]),
            KvInput::Get { key: k } => (self.get, vec![id, key(k)]),
            KvInput::Delete { key: k } => (self.del, vec![id, key(k)]),
        })
    }

    fn reply(&self, rel: blossom_base::RelId, row: &blossom_oracle::Row) -> Option<(u64, blossom_sim::cluster::Reply)> {
        use blossom_sim::cluster::Reply;
        use blossom_sim::linearize::KvOutput;
        let Value::Int(IntValue::U64(id)) = row.get(1)? else {
            return None;
        };
        let out = if rel == self.put_ok {
            KvOutput::PutOk
        } else if rel == self.get_resp {
            match row.get(3)? {
                Value::Option(None) => KvOutput::Value(None),
                Value::Option(Some(v)) => match &**v {
                    Value::Bytes(b) => KvOutput::Value(Some(b.to_vec())),
                    _ => return None,
                },
                _ => return None,
            }
        } else if rel == self.del_ok {
            match row.get(2)? {
                Value::Bool(b) => KvOutput::Deleted(*b),
                _ => return None,
            }
        } else {
            return None;
        };
        Some((*id, Reply::Done(out)))
    }
}

/// e01 in the cluster simulator: crash-restarts that lose or tear unsynced writes, many seeds, every history
/// linearizable.
#[test]
fn e01_in_the_cluster_simulator_is_linearizable_under_crashes() {
    use blossom_sim::cluster::{Cluster, ClusterConfig};
    use blossom_sim::linearize::{KvModel, Verdict, check_partitioned};
    let k = Kvs::new();
    let statics = vec![(
        k.rel("admins"),
        blossom_oracle::Row::from(vec![Value::Principal("spiffe://sim/client".into())]),
    )];
    let mut total = (0, 0, 0);
    for seed in seeds(1..=12) {
        let cfg = ClusterConfig {
            seed,
            clients: 5,
            keys: 3,
            nemesis: 150_000_000,
            crashes: true,
            duration: 3_000_000_000,
            timeout: 100_000_000,
            ..ClusterConfig::default()
        };
        let cluster = Cluster::new(
            &k.artifact,
            &k.schema,
            blossom_value::Seed([9; 16]),
            statics.clone(),
            Box::new(E01Protocol::of(&k.artifact)),
            cfg,
        )
        .unwrap();
        let run = cluster.run().unwrap();
        let answered = run.history.iter().filter(|o| o.ret.is_some()).count();
        total.0 += answered;
        total.1 += run.history.len() - answered;
        total.2 += run.crashes;
        let (verdict, key) = check_partitioned(&KvModel, &run.history, |i| i.key().to_vec(), 50_000_000);
        assert!(
            verdict == Verdict::Linearizable,
            "seed {seed}: not linearizable at key {:?}\n{}",
            key.map(|k| String::from_utf8_lossy(&k).into_owned()),
            run.log.join("\n")
        );
    }
    let all = 1..=12;
    assert!(total.0 > scaled(1000, &all), "only {} answered", total.0);
    assert!(
        total.1 > 0 && total.2 > scaled(50, &all) as u64,
        "unanswered {}, crashes {}",
        total.1,
        total.2
    );
}

/// Regression (S3 review): the boot instant is after every instant a released tick had, even when a checkpoint
/// covered (and the WAL truncation deleted) the records that carried it.
#[test]
fn the_clock_does_not_go_back_after_a_checkpoint() {
    let k = Kvs::new();
    let mut fs = SimFs::default();
    {
        let mut d = k.boot(&fs, 1_000);
        d.run_until_quiescent(Instant(1_000)).unwrap();
        d.node.offer_ingress(k.put(1, 1, "a", b"x"));
        let r = d.run_until_quiescent(Instant(5_000_000_000)).unwrap();
        assert_eq!(replies(&k, &r).len(), 1, "the put was released at instant 5 s");
        d.flush().unwrap();
        d.flush().unwrap(); // nothing new: a no-op
    }
    fs.crash(&mut |_| WriteFate::Lost).unwrap();
    let d = k.boot(&fs, 2_000);
    assert!(
        d.node.last_now() > Instant(5_000_000_000),
        "boot instant {:?}",
        d.node.last_now()
    );
}

/// Regression (S3 review): a tick that reads no WAL record still exposes its instant; after an idle stretch longer
/// than the time reservation, its reply waits for the reservation to be durable, and a restart boots after it.
#[test]
fn a_released_read_is_covered_by_the_time_reservation() {
    let k = Kvs::new();
    let mut fs = SimFs::default();
    let read_at = 60_000_000_000; // a minute after boot: far past the boot reservation
    {
        let mut d = k.boot(&fs, 0);
        d.run_until_quiescent(Instant(0)).unwrap();
        d.node.offer_ingress(k.get(1, 1, "a"));
        let r = d.run_until_quiescent(Instant(read_at)).unwrap();
        assert_eq!(replies(&k, &r).len(), 1);
        assert!(
            d.meta().last_now >= read_at,
            "META's time bound {} covers the read",
            d.meta().last_now
        );
    }
    fs.crash(&mut |_| WriteFate::Lost).unwrap();
    let d = k.boot(&fs, 0);
    assert!(d.node.last_now() > Instant(read_at));
}

/// Regression (S3 review): large messages are spread over ticks by the batch byte limit, so no tick's WAL record
/// exceeds the record limit.
#[test]
fn large_puts_are_spread_over_ticks() {
    let k = Kvs::new();
    let fs = SimFs::default();
    let mut d = k.boot(&fs, 1_000);
    d.run_until_quiescent(Instant(1_000)).unwrap();
    let big = vec![b'v'; 14 << 20];
    for i in 0..5u64 {
        d.node.offer_ingress(k.put(1 + i, i, &format!("k{i}"), &big));
    }
    let r = d.run_until_quiescent(Instant(2_000)).unwrap();
    assert_eq!(replies(&k, &r).len(), 5);
    assert_eq!(k.store(&d).len(), 5);
}

/// A store keeps the tail certification it was created with: opening it with another is refused.
#[test]
fn a_store_refuses_another_tail_certification() {
    let k = Kvs::new();
    let fs = SimFs::default();
    drop(k.boot(&fs, 0));
    let crc = Kvs::new().certified(blossom_store::Certification::Crc);
    let err = recovery::open(
        Arc::new(fs.clone()),
        &StoreSpec {
            dir: crc.dir.clone(),
            identity: identity(),
            mode: OpenMode::Existing,
            certification: crc.certification,
            database: blossom_store::lsm::LsmOptions::default(),
        },
        &crc.artifact.program,
        crc.names.clone(),
        Instant(10),
        2,
    )
    .err();
    let Some(err) = err else {
        panic!("a strict store opened as crc")
    };
    assert!(err.to_string().contains("tail certification"), "{err}");
}
