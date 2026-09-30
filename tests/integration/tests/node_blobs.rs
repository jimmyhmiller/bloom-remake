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
            },
            self.artifact.program.get(),
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
                    d.checkpoint().unwrap();
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
            d.checkpoint().unwrap();
        }
        // After a restart, the recovered rows' blobs are read from the store: a new connection's chunk that equals a
        // stored one is acknowledged from it.
        let mut d = k.boot(&fs);
        const LATER: i64 = 1_000_000_000_000;
        d.run_until_quiescent(Instant(LATER)).unwrap();
        let store = BlobStore::open(Arc::new(fs.clone()), &k.dir).unwrap();
        assert_eq!(store.list().unwrap().len(), 6, "engine {engine}");
        // Forget three chunks, checkpoint: their blobs go, the others stay.
        let forget = k.artifact.rel_named("forget").unwrap();
        for c in chunks.iter().take(3) {
            d.node
                .offer_input(forget, Arc::from(vec![Value::Bytes(Arc::from(&c[..]))]));
        }
        d.run_until_quiescent(Instant(LATER + 1)).unwrap();
        d.checkpoint().unwrap();
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
