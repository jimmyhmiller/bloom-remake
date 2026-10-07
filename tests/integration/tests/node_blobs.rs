//! Slice 7, item 1: blobs in durable rows (FOREIGN-PROTOCOLS §5) on the sans-IO node over the simulated filesystem.
//! A blob a durable row references is made durable before its tick's WAL record syncs: crash at every durable syscall
//! of a workload (with checkpoints), recover under every write fate, and every row whose acknowledgement was released
//! has its blob, with its bytes. After rows are deleted and a checkpoint is installed, their blobs are collected.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::durable::DurableSchema;
use blossom_node::manual::ManualDriver;
use blossom_node::recovery::{self, StoreSpec};
use blossom_node::streams::{HostRequest, Observed, host_request};
use blossom_node::{Executor, Node, NodeConfig, OracleExecutor};
use blossom_oracle::Oracle;
use blossom_store::{BlobStore, OpenMode, SimFs, StoreIdentity, Vfs, WriteFate};
use blossom_value::time::{Instant, NodeId};
use blossom_value::value::ConnId;
use blossom_value::{BlobRef, Value};

#[cfg(test)]
fn identity() -> StoreIdentity {
    StoreIdentity {
        store_uuid: [0; 16],
        deployment_id: [1; 16],
        program_id: [2; 16],
        node_name: "n1".into(),
        principal: "spiffe://test/blobs/n1".into(),
        format: recovery::FORMAT,
        directory_digest: [3; 16],
    }
}

#[cfg(test)]
struct Store {
    artifact: BlsArtifact,
    oracle: Arc<Oracle>,
    schema: DurableSchema,
    names: Arc<[Arc<str>]>,
    dir: PathBuf,
    engine: bool,
}

#[cfg(test)]
impl Store {
    fn new(engine: bool) -> Store {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/blobs/store.bls");
        let (result, _) = compile_file(
            path.to_str().unwrap(),
            &[NodeSpec {
                name: "n1".into(),
                role: None,
            }],
        );
        let artifact = result.unwrap_or_else(|e| panic!("store.bls: {e:?}")).0;
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
        Store {
            artifact,
            oracle,
            schema,
            names,
            dir: PathBuf::from("/data/n1"),
            engine,
        }
    }

    fn boot<'a>(&'a self, fs: &SimFs) -> ManualDriver<'a, Box<dyn Executor>> {
        let fs: Arc<dyn Vfs> = Arc::new(fs.clone());
        let opened = recovery::open(
            fs,
            &StoreSpec {
                dir: self.dir.clone(),
                identity: identity(),
                mode: OpenMode::InitFresh,
                certification: blossom_store::Certification::Strict,
                database: blossom_store::lsm::LsmOptions::default(),
            },
            &self.artifact.program,
            self.names.clone(),
            Instant(0),
            7,
        )
        .unwrap();
        let mut cfg = NodeConfig::new(NodeId(0), None);
        // A small cache budget, so the cache is collected during the workload too.
        cfg.blob_cache_bytes = 64;
        let exec: Box<dyn Executor> = if self.engine {
            let ecfg = blossom_engine::EngineConfig {
                roles: self.artifact.roles.clone(),
                node_names: self.names.to_vec(),
                seed: Some(blossom_value::Seed([9; 16])),
                ..blossom_engine::EngineConfig::default()
            };
            Box::new(blossom_engine::Engine::new(self.artifact.program.clone(), NodeId(0), ecfg).unwrap())
        } else {
            Box::new(OracleExecutor::new(self.oracle.clone()))
        };
        let node = Node::boot(cfg, &self.artifact.program, exec, opened.boot.clone()).unwrap();
        ManualDriver::new(
            node,
            self.artifact.program.get(),
            &self.schema,
            self.names.clone(),
            opened,
        )
    }

    /// The stored rows of the released (durable) image: chunk and blob.
    fn stored(&self, d: &ManualDriver<'_, Box<dyn Executor>>) -> Vec<(Vec<u8>, BlobRef)> {
        let rel = self.artifact.rel_named("stored").unwrap();
        d.node
            .released_image()
            .rows
            .get(&rel)
            .into_iter()
            .flatten()
            .map(|r| match (&r[0], &r[1]) {
                (Value::Bytes(k), Value::Blob(b)) => (k.to_vec(), *b),
                other => panic!("stored row {other:?}"),
            })
            .collect()
    }
}

/// Opens the stream's connection and sends `chunk`; returns the acknowledgement's bytes and the recorded cut count
/// when it was released.
#[cfg(test)]
fn send(
    fs: &SimFs,
    d: &mut ManualDriver<'_, Box<dyn Executor>>,
    conn: ConnId,
    chunk: &[u8],
    at: i64,
) -> Vec<(Vec<u8>, usize)> {
    d.node
        .observe_stream(Observed::Bytes {
            conn,
            bytes: chunk.to_vec(),
        })
        .unwrap();
    let mut released = Vec::new();
    let mut hosts = Vec::new();
    d.run_until_quiescent_with(Instant(at), &mut |t| {
        for h in t.host {
            hosts.push((h, fs.cut_count().unwrap()));
        }
    })
    .unwrap();
    for (h, cut) in hosts {
        let blobs = d.node.blobs();
        match host_request(d.node.streams(), &h, &blobs).unwrap() {
            HostRequest::Write { bytes, .. } => released.push((bytes, cut)),
            other => panic!("{other:?}"),
        }
    }
    released
}

#[cfg(test)]
fn open_conn(d: &mut ManualDriver<'_, Box<dyn Executor>>, conn: ConnId) {
    d.node
        .observe_stream(Observed::Opened {
            stream: 0,
            conn,
            peer: "peer".into(),
            req: None,
            at: Instant(0),
        })
        .unwrap();
    d.run_until_quiescent(Instant(1)).unwrap();
}

#[test]
fn every_crash_point_keeps_every_acknowledged_blob() {
    for engine in [false, true] {
        let k = Store::new(engine);
        let fs = SimFs::default();
        fs.enable_crash_recording().unwrap();
        let conn = ConnId(1);
        // For each acknowledged chunk: the chunk, and the recorded cuts when its acknowledgement was released.
        let mut acked: Vec<(Vec<u8>, usize)> = Vec::new();
        {
            let mut d = k.boot(&fs);
            d.run_until_quiescent(Instant(0)).unwrap();
            open_conn(&mut d, conn);
            for i in 0..12u64 {
                let chunk = format!("chunk-{i}-{}", "x".repeat(i as usize * 7)).into_bytes();
                let released = send(&fs, &mut d, conn, &chunk, 2 + i as i64);
                assert_eq!(released.len(), 1, "chunk {i} acknowledged once");
                // The acknowledgement is the blob's bytes, read by the host (`Part::Blob`).
                assert_eq!(released[0].0, chunk, "chunk {i}'s acknowledgement");
                acked.push((chunk, released[0].1));
                if i % 5 == 4 {
                    d.flush().unwrap();
                }
            }
        }
        let cuts = fs.recorded_cuts().unwrap();
        assert!(cuts.len() > 30, "only {} cuts", cuts.len());
        type Fate = fn(usize) -> WriteFate;
        let fates: [(&str, Fate); 3] = [
            ("lost", |_| WriteFate::Lost),
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
                let d = k.boot(&image);
                let stored = k.stored(&d);
                let store = BlobStore::open(Arc::new(image.clone()), &k.dir).unwrap();
                // Every recovered row's blob is there, with its bytes.
                for (chunk, b) in &stored {
                    let bytes = store
                        .read(b)
                        .unwrap_or_else(|e| panic!("cut {c} ({what}, engine {engine}): {e}"))
                        .unwrap_or_else(|| panic!("cut {c} ({what}, engine {engine}): the blob of a row is missing"));
                    assert_eq!(&bytes[..], &chunk[..], "cut {c} ({what})");
                }
                // Every acknowledged chunk was recovered.
                for (chunk, at) in &acked {
                    if *at <= c + 1 {
                        assert!(
                            stored.iter().any(|(k, _)| k == chunk),
                            "cut {c} ({what}, engine {engine}): acknowledged {:?} was lost",
                            String::from_utf8_lossy(chunk)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_recovered_node_reads_its_blobs_and_collects_the_unreferenced_ones() {
    for engine in [false, true] {
        let k = Store::new(engine);
        let fs = SimFs::default();
        let conn = ConnId(1);
        let chunks: Vec<Vec<u8>> = (0..6).map(|i| format!("blob number {i}").into_bytes()).collect();
        {
            let mut d = k.boot(&fs);
            d.run_until_quiescent(Instant(0)).unwrap();
            open_conn(&mut d, conn);
            for (i, c) in chunks.iter().enumerate() {
                send(&fs, &mut d, conn, c, 2 + i as i64);
            }
            d.flush().unwrap();
        }
        // A blob no row holds, as a crash between a blob's write and its record's sync leaves one.
        let orphan: Arc<[u8]> = Arc::from(&b"written, never recorded"[..]);
        BlobStore::open(Arc::new(fs.clone()), &k.dir)
            .unwrap()
            .put_all(&[(BlobRef::of(&orphan), orphan.clone())])
            .unwrap();
        // After a restart, the recovered rows' blobs are read from the store: a new connection's chunk that equals a
        // stored one is acknowledged from it.
        let mut d = k.boot(&fs);
        const LATER: i64 = 1_000_000_000_000;
        d.run_until_quiescent(Instant(LATER)).unwrap();
        let store = BlobStore::open(Arc::new(fs.clone()), &k.dir).unwrap();
        assert_eq!(store.list().unwrap().len(), 7, "engine {engine}");
        // Forget three chunks, checkpoint: their blobs go, and so does the orphan; the others stay.
        let forget = k.artifact.rel_named("forget").unwrap();
        for c in chunks.iter().take(3) {
            d.node
                .offer_input(forget, Arc::from(vec![Value::Bytes(Arc::from(&c[..]))]));
        }
        d.run_until_quiescent(Instant(LATER + 1)).unwrap();
        d.flush().unwrap();
        let mut left: Vec<BlobRef> = store.list().unwrap();
        left.sort();
        let mut want: Vec<BlobRef> = chunks.iter().skip(3).map(|c| BlobRef::of(c)).collect();
        want.sort();
        assert_eq!(left, want, "engine {engine}: the forgotten chunks' blobs are collected");
        // A kept blob is still read back through the stream after the collection.
        let conn2 = ConnId(2);
        d.node
            .observe_stream(Observed::Opened {
                stream: 0,
                conn: conn2,
                peer: "peer".into(),
                req: None,
                at: Instant(LATER + 2),
            })
            .unwrap();
        d.run_until_quiescent(Instant(LATER + 3)).unwrap();
        let again = send(&fs, &mut d, conn2, &chunks[4], LATER + 4);
        assert_eq!(again.first().map(|x| x.0.clone()), Some(chunks[4].clone()));
    }
}

/// Boots the store program's node alone (no driver): the test plays the committer, so it chooses when a tick's
/// blobs reach the store and when its record syncs.
#[cfg(test)]
fn bare_node(k: &Store, fs: &SimFs) -> Node<Box<dyn Executor>> {
    k.boot(fs).node
}

/// Review of S7, storage finding 1: a tick reading a blob a durable row took in an earlier tick whose record is not
/// synced yet (the pipelined runtime runs ahead of its committer) reads the bytes the node handed over, not a store
/// that does not have them yet.
#[test]
fn a_tick_reads_a_blob_whose_record_is_not_synced_yet() {
    for engine in [false, true] {
        let k = Store::new(engine);
        let fs = SimFs::default();
        let mut node = bare_node(&k, &fs);
        let rel = |n: &str| k.artifact.rel_named(n).unwrap();
        let key: Arc<[u8]> = Arc::from(&b"pipelined blob"[..]);
        node.run_tick(Instant(1)).unwrap();
        node.offer_input(rel("put"), Arc::from(vec![Value::Bytes(key.clone())]));
        let put = node.run_tick(Instant(2)).unwrap();
        assert_eq!(
            put.blobs.len(),
            1,
            "engine {engine}: the blob is handed over with the record"
        );
        // Neither written to the store nor synced: the next tick reads it all the same.
        node.offer_input(rel("peek"), Arc::from(vec![Value::Bytes(key.clone())]));
        node.run_tick(Instant(3))
            .unwrap_or_else(|f| panic!("engine {engine}: {f}"));
        let peeked = node.carried_rows(rel("peeked"));
        assert_eq!(peeked.len(), 1, "engine {engine}");
        assert_eq!(peeked[0][1], Value::Bytes(key.clone()), "engine {engine}");
    }
}

/// Review of S7, storage finding 2: a record after a checkpoint that inserts a row with an already-durable blob keeps
/// the blob a collection root, although a later tick deletes the row again: a recovery from the checkpoint replays
/// that record.
#[test]
fn a_blob_a_record_after_the_checkpoint_references_is_kept() {
    for engine in [false, true] {
        let k = Store::new(engine);
        let fs = SimFs::default();
        let mut node = bare_node(&k, &fs);
        let rel = |n: &str| k.artifact.rel_named(n).unwrap();
        let key: Arc<[u8]> = Arc::from(&b"re-inserted blob"[..]);
        let blob = BlobRef::of(&key);
        // The committer: a tick's blobs reach the store, then its record syncs.
        let store = BlobStore::open(Arc::new(fs.clone()), &k.dir).unwrap();
        let sync = |node: &mut Node<Box<dyn Executor>>, now: i64| {
            let fx = node.run_tick(Instant(now)).unwrap();
            store.put_all(&fx.blobs).unwrap();
            node.wal_synced(fx.tick).unwrap();
        };
        sync(&mut node, 1);
        node.offer_input(rel("put"), Arc::from(vec![Value::Bytes(key.clone())]));
        sync(&mut node, 2);
        node.offer_input(rel("forget"), Arc::from(vec![Value::Bytes(key.clone())]));
        sync(&mut node, 3);
        // A checkpoint of this state holds no row with the blob: it is a candidate its installation may let go.
        let checkpoint = node.next_tick().prev().unwrap();
        let outside = node.collection_candidates();
        assert!(outside.contains(&blob), "engine {engine}");
        // After it: the row again (the blob is still durable, so it is not written again), then deleted.
        node.offer_input(rel("put"), Arc::from(vec![Value::Bytes(key.clone())]));
        let fx = node.run_tick(Instant(4)).unwrap();
        assert!(fx.blobs.is_empty(), "engine {engine}: a durable blob is written again");
        store.put_all(&fx.blobs).unwrap();
        node.wal_synced(fx.tick).unwrap();
        node.offer_input(rel("forget"), Arc::from(vec![Value::Bytes(key.clone())]));
        sync(&mut node, 5);
        let gone = node.blob_garbage(checkpoint, &outside);
        assert!(
            !gone.contains(&blob),
            "engine {engine}: a replayable record's blob would be collected"
        );
        // A checkpoint after those records: nothing reaches the blob any more, and it goes. (The engine holds the
        // last tick's rows until the next tick runs: the deleted row, the event that deleted it.)
        sync(&mut node, 6);
        let later = node.next_tick().prev().unwrap();
        let outside = node.collection_candidates();
        let gone = node.blob_garbage(later, &outside);
        assert_eq!(gone, vec![blob], "engine {engine}");
        assert_eq!(store.delete(&gone).unwrap(), 1, "engine {engine}");
        // Gone, it is no longer durable: a row that needs it again writes it again.
        node.offer_input(rel("put"), Arc::from(vec![Value::Bytes(key.clone())]));
        let fx = node.run_tick(Instant(7)).unwrap();
        assert_eq!(
            fx.blobs.len(),
            1,
            "engine {engine}: a collected blob is not written again"
        );
    }
}

/// Review of S7, storage: a blob only a derived row holds (no carried row: the engine keeps the view's row across
/// ticks without creating the blob again) stays in the cache through its trims, so a later tick that copies it into a
/// durable row still has its bytes.
#[test]
fn a_blob_only_a_derived_row_holds_survives_the_cache_trims() {
    for engine in [false, true] {
        let k = Store::new(engine);
        let fs = SimFs::default();
        let mut node = bare_node(&k, &fs);
        let store = BlobStore::open(Arc::new(fs.clone()), &k.dir).unwrap();
        let rel = |n: &str| k.artifact.rel_named(n).unwrap();
        let key: Arc<[u8]> = Arc::from(vec![7u8; 200]);
        let mut now = 1;
        let mut tick = |node: &mut Node<Box<dyn Executor>>| {
            let fx = node
                .run_tick(Instant(now))
                .unwrap_or_else(|f| panic!("engine {engine}: {f}"));
            now += 1;
            store.put_all(&fx.blobs).unwrap();
            node.wal_synced(fx.tick).unwrap();
            fx
        };
        tick(&mut node);
        node.offer_input(rel("keep"), Arc::from(vec![Value::Bytes(key.clone())]));
        tick(&mut node);
        // Ticks that make other blobs, past the cache's 64-byte budget, so it is trimmed.
        for i in 0..4u8 {
            node.offer_input(rel("put"), Arc::from(vec![Value::Bytes(Arc::from(vec![i; 300]))]));
            tick(&mut node);
            tick(&mut node);
        }
        node.offer_input(rel("commit_kept"), Arc::from(vec![Value::Bytes(key.clone())]));
        let fx = tick(&mut node);
        assert_eq!(
            fx.blobs.len(),
            1,
            "engine {engine}: the kept blob is written with its row"
        );
        assert_eq!(fx.blobs[0].0, BlobRef::of(&key), "engine {engine}");
    }
}

/// HD item 3: a blob logged in its tick's WAL record (its file written, not synced) is synced as a file before the
/// WAL that logs it is truncated. Truncation removes only segments wholly below a checkpoint, and each incarnation
/// writes a segment of its own, so: one incarnation logs the blobs; the next recovers them (pending again) and
/// checkpoints, which truncates the first segment; then a crash loses every write not synced. Every row's blob is
/// still there.
#[test]
fn logged_blobs_are_synced_before_the_wal_that_logs_them_is_truncated() {
    for engine in [false, true] {
        let k = Store::new(engine);
        let fs = SimFs::default();
        let conn = ConnId(1);
        let chunks: Vec<Vec<u8>> = (0..4).map(|i| format!("logged blob {i}").into_bytes()).collect();
        let segments = |fs: &SimFs| {
            fs.list(&k.dir.join("wal"))
                .unwrap()
                .iter()
                .filter(|p| p.extension().is_some_and(|e| e == "seg"))
                .count()
        };
        {
            let mut d = k.boot(&fs);
            d.run_until_quiescent(Instant(0)).unwrap();
            open_conn(&mut d, conn);
            for (i, c) in chunks.iter().enumerate() {
                send(&fs, &mut d, conn, c, 2 + i as i64);
            }
        }
        {
            let mut d = k.boot(&fs);
            const LATER: i64 = 1_000_000_000_000;
            d.run_until_quiescent(Instant(LATER)).unwrap();
            // A durable change of its own, so the checkpoint has a synced tick to cover.
            let conn2 = ConnId(2);
            d.node
                .observe_stream(Observed::Opened {
                    stream: 0,
                    conn: conn2,
                    peer: "peer".into(),
                    req: None,
                    at: Instant(LATER + 1),
                })
                .unwrap();
            d.run_until_quiescent(Instant(LATER + 2)).unwrap();
            send(&fs, &mut d, conn2, b"after the restart", LATER + 3);
            assert_eq!(segments(&fs), 2, "engine {engine}");
            d.flush().unwrap();
            assert_eq!(segments(&fs), 1, "engine {engine}: the first segment was not truncated");
        }
        let mut image = fs.fork().unwrap();
        image.crash(&mut |_| WriteFate::Lost).unwrap();
        let d = k.boot(&image);
        let stored = k.stored(&d);
        assert_eq!(stored.len(), chunks.len() + 1, "engine {engine}");
        let store = BlobStore::open(Arc::new(image.clone()), &k.dir).unwrap();
        for (chunk, b) in &stored {
            assert_eq!(store.read(b).unwrap().as_deref(), Some(&chunk[..]), "engine {engine}");
        }
    }
}

/// From the HD review: a tick logs a new blob and stops before its record syncs; another blob's put then syncs the
/// blob directory (as a large blob's put, the checkpointer or a collection can, while a runtime batch is unsynced);
/// then power fails, keeping the logged blob's directory entry and losing its bytes. The record is lost, so recovery
/// must not hold the blob (its provisional file goes), and the same bytes sent again are stored and read whole.
#[test]
fn a_logged_blob_whose_record_is_lost_is_not_trusted_after_a_power_loss() {
    for engine in [false, true] {
        let k = Store::new(engine);
        let fs = SimFs::default();
        let conn = ConnId(1);
        let chunk = b"logged, then its record lost".to_vec();
        {
            let mut d = k.boot(&fs);
            d.run_until_quiescent(Instant(0)).unwrap();
            open_conn(&mut d, conn);
            d.node
                .observe_stream(Observed::Bytes {
                    conn,
                    bytes: chunk.clone(),
                })
                .unwrap();
            d.crash_before_sync(Instant(2)).unwrap();
        }
        let other: Arc<[u8]> = Arc::from(&b"a put that syncs the directory"[..]);
        BlobStore::open(Arc::new(fs.clone()), &k.dir)
            .unwrap()
            .put_all(&[(BlobRef::of(&other), other.clone())])
            .unwrap();
        let mut image = fs.fork().unwrap();
        image.crash(&mut |_| WriteFate::Lost).unwrap();
        let mut d = k.boot(&image);
        assert!(k.stored(&d).is_empty(), "engine {engine}: the lost record's row");
        let b = BlobRef::of(&chunk);
        let leftovers: Vec<_> = image
            .list(&k.dir.join("blobs"))
            .unwrap()
            .into_iter()
            .filter(|p| p.to_string_lossy().contains(&b.hex()))
            .collect();
        assert!(
            leftovers.is_empty(),
            "engine {engine}: the blob is still held: {leftovers:?}"
        );
        // The same bytes again: stored, and read back whole.
        const LATER: i64 = 1_000_000_000_000;
        d.run_until_quiescent(Instant(LATER)).unwrap();
        let conn2 = ConnId(2);
        d.node
            .observe_stream(Observed::Opened {
                stream: 0,
                conn: conn2,
                peer: "peer".into(),
                req: None,
                at: Instant(LATER + 1),
            })
            .unwrap();
        d.run_until_quiescent(Instant(LATER + 2)).unwrap();
        let again = send(&image, &mut d, conn2, &chunk, LATER + 3);
        assert_eq!(
            again.first().map(|x| x.0.clone()),
            Some(chunk.clone()),
            "engine {engine}"
        );
    }
}
