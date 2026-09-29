//! Slice 3: the e01 key-value store on the sans-IO node over the simulated filesystem. An acknowledged put survives
//! every crash; a reply is released only after its tick's WAL record is synced (Invariant R); recovery replays the
//! WAL on top of the checkpoint and never reuses a tick number.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::durable::DurableSchema;
use blossom_node::manual::ManualDriver;
use blossom_node::recovery::{self, StoreSpec};
use blossom_node::{Node, NodeConfig, ReleasedTick};
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
        }
    }

    fn rel(&self, name: &str) -> blossom_base::RelId {
        self.artifact.rel_named(name).unwrap_or_else(|| panic!("no relation {name}"))
    }

    /// Opens (recovers) the store on `fs` and boots the node.
    fn boot<'a>(&'a self, fs: &SimFs, wall: i64) -> ManualDriver<'a, Arc<Oracle>> {
        // A clone of a `SimFs` is another handle on the same filesystem.
        let fs: Arc<dyn Vfs> = Arc::new(fs.clone());
        let opened = recovery::open(
            fs,
            &StoreSpec {
                dir: self.dir.clone(),
                identity: identity(),
                mode: OpenMode::InitFresh,
            },
            self.artifact.program.get(),
            self.names.clone(),
            Instant(wall),
            7,
        )
        .unwrap();
        let mut cfg = NodeConfig::new(NodeId(0), self.artifact.roles.first().copied().flatten());
        cfg.halt = self.artifact.halt;
        let node = Node::boot(cfg, &self.artifact.program, self.oracle.clone(), opened.boot.clone()).unwrap();
        ManualDriver::new(node, self.artifact.program.get(), &self.schema, self.names.clone(), opened)
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

    fn store(&self, d: &ManualDriver<'_, Arc<Oracle>>) -> Vec<(String, Vec<u8>)> {
        let rel = self.rel("store");
        d.node
            .released_image()
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
    assert!(d.run_until_quiescent(Instant(1_000)).unwrap().iter().all(|t| t.egress.is_empty()));
    d.node.offer_ingress(k.put(1, 10, "a", b"x"));
    let released = d.run_until_quiescent(Instant(2_000)).unwrap();
    assert_eq!(replies(&k, &released).iter().map(|r| (r.0.as_str(), r.1)).collect::<Vec<_>>(), [("put_ok", 10)]);
    d.node.offer_ingress(k.get(1, 11, "a"));
    let released = d.run_until_quiescent(Instant(3_000)).unwrap();
    let r = replies(&k, &released);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, "get_resp");
    assert!(r[0].2.contains("120"), "the value b\"x\" (120) is returned: {}", r[0].2);
    assert_eq!(k.store(&d), [("a".to_string(), b"x".to_vec())]);
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
            d.node.offer_ingress(k.put(1, i, &format!("k{}", i % 7), format!("v{i}").as_bytes()));
            let r = d.run_until_quiescent(Instant(2_000 + i as i64)).unwrap();
            assert_eq!(replies(&k, &r).len(), 1);
        }
        last_tick = d.node.next_tick();
        assert_eq!(k.store(&d).len(), 7);
    }
    // Every acknowledged write was synced: a crash that loses every unsynced write keeps them all.
    fs.crash(&mut |_| WriteFate::Lost).unwrap();
    let mut d = k.boot(&fs, 500);
    assert!(d.node.next_tick() > last_tick, "boot tick {:?} reuses a tick before {last_tick:?}", d.node.next_tick());
    assert_eq!(d.meta().restarts, 2);
    let store = k.store(&d);
    assert_eq!(store.len(), 7);
    for (key, val) in &store {
        let i: u64 = String::from_utf8(val.clone()).unwrap().strip_prefix('v').unwrap().parse().unwrap();
        assert_eq!(key, &format!("k{}", i % 7));
        assert!(i >= 13, "{key} holds the last put to it");
    }
    // The clock never goes back across incarnations even when the wall clock does: the boot instant is after the
    // last instant the previous incarnation used.
    assert!(d.node.last_now() > Instant(2_019), "boot instant {:?}", d.node.last_now());
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
        d.checkpoint().unwrap();
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
        assert!(d.node.release_ready().is_empty(), "a tick with a WAL record waits for its sync");
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
                },
                k.artifact.program.get(),
                k.names.clone(),
                Instant(0),
                1,
            )
            .unwrap();
            let cfg = NodeConfig::new(NodeId(0), k.artifact.roles.first().copied().flatten());
            let mut node = Node::boot(cfg, &k.artifact.program, k.oracle.clone(), opened.boot).unwrap();
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
                        released.extend(node.wal_synced(t).iter().map(|r| r.tick));
                    }
                } else {
                    match kind {
                        0 => node.offer_ingress(k.get(1, i as u64, "a")),
                        _ => node.offer_ingress(k.put(1, i as u64, "a", &[kind, i as u8])),
                    }
                    let fx = node.run_tick(Instant(now)).unwrap();
                    computed.push((fx.tick, fx.wal.is_some()));
                    released.extend(node.release_ready().iter().map(|r| r.tick));
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
    let k = Kvs::new();
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
                d.checkpoint().unwrap();
            }
        }
    }
    let cuts = fs.recorded_cuts().unwrap();
    assert!(cuts.len() > 30, "only {} cuts", cuts.len());
    for (c, cut) in cuts.into_iter().enumerate() {
        let mut cut = cut;
        cut.crash(&mut |_| WriteFate::Lost).unwrap();
        // A cut before the store was initialized has nothing to recover; `InitFresh` starts it again.
        let d = k.boot(&cut, 0);
        let store: std::collections::BTreeMap<String, Vec<u8>> = k.store(&d).into_iter().collect();
        // The expected value of each key: the last put acknowledged at or before this cut, or any later put to it
        // (a put may be durable before its reply is released).
        for key in (0..4).map(|i| format!("k{i}")) {
            let last_acked = acked.iter().rfind(|(k2, _, at)| *k2 == key && *at <= c + 1);
            let allowed: Vec<&Vec<u8>> = acked
                .iter()
                .filter(|(k2, _, at)| *k2 == key && last_acked.is_none_or(|(_, _, a)| at >= a))
                .map(|(_, v, _)| v)
                .collect();
            match (store.get(&key), last_acked) {
                (None, None) => {}
                (Some(v), _) => assert!(allowed.contains(&v), "cut {c}: {key} = {v:?}, allowed {allowed:?}"),
                (None, Some((_, v, _))) => panic!("cut {c}: {key} lost its acknowledged value {v:?}"),
            }
        }
    }
}
