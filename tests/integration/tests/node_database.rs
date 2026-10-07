//! S24: the node's database is its durable state (docs/design/DATABASE.md). The manual driver applies each released
//! tick to it and flushes it, truncating the WAL behind; a recovery at any point, after a restart too, starts from its
//! tables and the WAL after them and finds exactly the released rows, through flushes and compactions.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::durable::DurableSchema;
use blossom_node::manual::ManualDriver;
use blossom_node::recovery::{self, StoreSpec};
use blossom_node::streams::Observed;
use blossom_node::{Executor, Node, NodeConfig, OracleExecutor};
use blossom_oracle::Oracle;
use blossom_store::{OpenMode, SimFs, StoreIdentity, Vfs, WriteFate};
use blossom_value::Value;
use blossom_value::time::{Instant, NodeId};
use blossom_value::value::ConnId;

#[cfg(test)]
fn identity() -> StoreIdentity {
    StoreIdentity {
        store_uuid: [0; 16],
        deployment_id: [1; 16],
        program_id: [2; 16],
        node_name: "n1".into(),
        principal: "spiffe://test/ckpt/n1".into(),
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
}

#[cfg(test)]
impl Store {
    fn new() -> Store {
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
                // Small, so the run flushes on its own and compacts.
                database: blossom_store::lsm::LsmOptions {
                    memtable_bytes: 2 << 10,
                    block_bytes: 256,
                    tier: 3,
                    max_tables: 6,
                    ..blossom_store::lsm::LsmOptions::default()
                },
            },
            &self.artifact.program,
            self.names.clone(),
            Instant(0),
            7,
        )
        .unwrap();
        let cfg = NodeConfig::new(NodeId(0), None);
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

    fn stored(&self, d: &ManualDriver<'_, Box<dyn Executor>>) -> Vec<Vec<u8>> {
        let rel = self.artifact.rel_named("stored").unwrap();
        let mut out: Vec<Vec<u8>> = d
            .node
            .released_image()
            .rows
            .get(&rel)
            .into_iter()
            .flatten()
            .map(|r| match &r[0] {
                Value::Bytes(k) => k.to_vec(),
                other => panic!("{other:?}"),
            })
            .collect();
        out.sort();
        out
    }
}

/// A clean crash of a copy of `fs` (everything written survives), recovered.
#[cfg(test)]
fn recovered(k: &Store, fs: &SimFs) -> Vec<Vec<u8>> {
    let mut image = fs.fork().unwrap();
    image.crash(&mut |_| WriteFate::Survive).unwrap();
    let d = k.boot(&image);
    k.stored(&d)
}

#[cfg(test)]
fn start(d: &mut ManualDriver<'_, Box<dyn Executor>>, conn: ConnId, at: i64) {
    d.run_until_quiescent(Instant(at)).unwrap();
    d.node
        .observe_stream(Observed::Opened {
            stream: 0,
            conn,
            peer: "peer".into(),
            req: None,
            at: Instant(at),
        })
        .unwrap();
    d.run_until_quiescent(Instant(at + 1)).unwrap();
}

#[test]
fn recovery_from_the_database_is_exact_across_restarts_flushes_and_compactions() {
    let k = Store::new();
    let fs = SimFs::default();
    let forget = k.artifact.rel_named("forget").unwrap();
    let mut t = 10i64;
    let mut saw_compaction = false;
    let mut checks = 0;
    for run in 0..2 {
        let mut d = k.boot(&fs);
        let conn = ConnId(1);
        start(&mut d, conn, t + 1_000_000_000_000 * run);
        t += 1_000_000_000_000 * run + 10;
        let mut last_tables = 0;
        // 61 chunks: the last is after the last explicit flush, so a restart replays WAL records past the tables.
        for i in 0..61u64 {
            let chunk = format!("run {run} chunk {i}").into_bytes();
            d.node.observe_stream(Observed::Bytes { conn, bytes: chunk }).unwrap();
            // Forget an earlier chunk now and then: the database holds deletes too.
            if i % 4 == 3 {
                let old = format!("run {run} chunk {}", i - 2).into_bytes();
                d.node
                    .offer_input(forget, Arc::from(vec![Value::Bytes(Arc::from(&old[..]))]));
            }
            t += 1;
            d.run_until_quiescent(Instant(t)).unwrap();
            if i % 3 == 2 {
                d.flush().unwrap();
                let tables = d.database().info().unwrap().tables.len();
                if tables < last_tables {
                    saw_compaction = true;
                }
                last_tables = tables;
                assert_eq!(recovered(&k, &fs), k.stored(&d), "run {run} after chunk {i}");
                checks += 1;
            }
        }
    }
    assert!(checks > 30);
    assert!(saw_compaction, "the database never compacted");
}

/// A store from before the database (a checkpoint chain and the WAL after it, no `db/`): its first boot of this build
/// recovers from the checkpoint and the WAL, starts the database from the recovered rows and drops the checkpoints;
/// the boot after recovers from the database.
#[test]
fn a_store_from_before_the_database_moves_onto_one() {
    use blossom_store::{CheckpointWriter, FileCheckpoints, FileWal, Lsn, SegmentHeader, WalRecordBuf, WalWriter};
    let k = Store::new();
    let fs = SimFs::default();
    let conn = ConnId(1);
    let (mid, at_mid, expected) = {
        let mut d = k.boot(&fs);
        start(&mut d, conn, 1);
        let mut t = 10;
        let mut mid = None;
        for i in 0..20u64 {
            d.node
                .observe_stream(Observed::Bytes {
                    conn,
                    bytes: format!("chunk {i}").into_bytes(),
                })
                .unwrap();
            t += 1;
            d.run_until_quiescent(Instant(t)).unwrap();
            if i == 9 {
                mid = Some((d.node.released_tick().unwrap(), d.node.released_image().clone()));
            }
        }
        let (tick, image) = mid.unwrap();
        (tick, image, k.stored(&d))
    };
    // Make it a store from before the database: no `db/`, and a checkpoint of the rows at the middle tick.
    let mut legacy = fs.fork().unwrap();
    legacy.crash(&mut |_| WriteFate::Survive).unwrap();
    let vfs: Arc<dyn Vfs> = Arc::new(legacy.clone());
    for f in vfs.list(&k.dir.join("db").join("sst")).unwrap() {
        vfs.remove(&f).unwrap();
    }
    for f in vfs.list(&k.dir.join("db")).unwrap() {
        if f.file_name().is_some_and(|n| n != "sst") {
            vfs.remove(&f).unwrap();
        }
    }
    // A sync of the middle tick (the proof a checkpoint takes), from a WAL of its own.
    let scratch = PathBuf::from("/scratch-wal");
    let header = SegmentHeader {
        format: recovery::FORMAT,
        store_uuid: [5; 16],
        segment_seq: 0,
        restarts: 1,
        boot_nonce: 1,
        lsn_base: Lsn(0),
        catalog: Vec::new(),
    };
    let mut w = FileWal::create(vfs.clone(), &scratch, header, Lsn(0)).unwrap();
    w.append(&WalRecordBuf {
        batch: 1,
        tick: mid.0,
        now: 0,
        kind: 1,
        payload: Vec::new(),
    })
    .unwrap();
    let covers = w.sync().unwrap().synced_tick().unwrap();
    let codec = blossom_node::durable::DurableCodec::new(k.artifact.program.get(), &k.schema, k.names.clone());
    // A row only the checkpoint holds: found after the move, it shows recovery started from the checkpoint.
    let peeked = k.artifact.rel_named("peeked").unwrap();
    let marker: blossom_oracle::Row = Arc::from(vec![
        Value::Bytes(Arc::from(&b"marker"[..])),
        Value::Bytes(Arc::from(&b"from the checkpoint"[..])),
    ]);
    let mut at_mid = at_mid;
    at_mid.rows.entry(peeked).or_default().insert(marker.clone());
    let mut ckpt = FileCheckpoints::new(vfs.clone(), &k.dir).unwrap();
    let id = ckpt.write(codec.encode_image(&at_mid).unwrap(), covers).unwrap();
    ckpt.install(id).unwrap();
    assert!(vfs.list(&k.dir).unwrap().iter().any(|p| p.ends_with("ckpt")));
    // The first boot moves it onto a database.
    let d = k.boot(&legacy);
    assert_eq!(k.stored(&d), expected);
    let holds_marker = |d: &ManualDriver<'_, Box<dyn Executor>>| {
        d.node
            .released_image()
            .rows
            .get(&peeked)
            .is_some_and(|r| r.contains(&marker))
    };
    assert!(holds_marker(&d), "recovery started from the checkpoint");
    assert!(
        vfs.list(&k.dir.join("ckpt")).unwrap().is_empty(),
        "the checkpoints are gone"
    );
    assert!(d.database().flushed().unwrap().is_some());
    drop(d);
    // The next recovers from the database.
    let mut again = legacy.fork().unwrap();
    again.crash(&mut |_| WriteFate::Survive).unwrap();
    let d = k.boot(&again);
    assert_eq!(k.stored(&d), expected);
    assert!(holds_marker(&d), "the database holds what the checkpoint held");
}
