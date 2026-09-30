//! Slice 6: stream writes and durability (FOREIGN-PROTOCOLS §1.2) on the sans-IO node over the simulated
//! filesystem. A stream write is released only after its tick's WAL record is synced, exactly like a session reply:
//! crash at every durable syscall of a workload (with checkpoints), recover under every write fate, and every chunk
//! whose acknowledgement was written is in the recovered store.

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
        principal: "spiffe://test/store/n1".into(),
        format: recovery::FORMAT,
        directory_digest: [3; 16],
    }
}

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
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/streams/store.bls");
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
            },
            self.artifact.program.get(),
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

    /// The recorded chunks in the released (durable) image.
    fn seen(&self, d: &ManualDriver<'_, Box<dyn Executor>>) -> Vec<Vec<u8>> {
        let rel = self.artifact.rel_named("seen").unwrap();
        d.node
            .released_image()
            .rows
            .get(&rel)
            .into_iter()
            .flatten()
            .map(|r| match &r[0] {
                Value::Bytes(b) => b.to_vec(),
                other => panic!("seen row {other:?}"),
            })
            .collect()
    }
}

#[test]
fn every_crash_point_keeps_every_acknowledged_chunk() {
    let k = Store::new();
    let fs = SimFs::default();
    fs.enable_crash_recording().unwrap();
    let conn = ConnId(1);
    // For each acknowledged chunk: the chunk and the number of recorded cuts when its write was released.
    let mut acked: Vec<(Vec<u8>, usize)> = Vec::new();
    {
        let mut d = k.boot(&fs);
        d.run_until_quiescent(Instant(0)).unwrap();
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
        for i in 0..12u64 {
            let chunk = format!("chunk-{i}").into_bytes();
            d.node
                .observe_stream(Observed::Bytes {
                    conn,
                    bytes: chunk.clone(),
                })
                .unwrap();
            let mut released_at = Vec::new();
            d.run_until_quiescent_with(Instant(2 + i as i64), &mut |t| {
                for h in &t.host {
                    // The acknowledgement echoes the chunk.
                    assert!(matches!(&h.row[2], Value::Vec(_)));
                    released_at.push(fs.cut_count().unwrap());
                }
            })
            .unwrap();
            assert_eq!(released_at.len(), 1, "chunk {i} acknowledged once");
            acked.push((chunk, released_at[0]));
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
            let seen = k.seen(&d);
            for (chunk, at) in &acked {
                if *at <= c + 1 {
                    assert!(
                        seen.contains(chunk),
                        "cut {c} ({what}): acknowledged {:?} was lost",
                        String::from_utf8_lossy(chunk)
                    );
                }
            }
        }
    }
}
