//! Slice 7, item 2: checkpoints that follow change (FOREIGN-PROTOCOLS §6). After a full image, a checkpoint is a delta
//! layer holding the net change since the checkpoint before it; the chain compacts to a full image again when its
//! layers outgrow the image or reach `MAX_CHECKPOINT_LAYERS`. A recovery at any point, after a restart too, finds
//! exactly the released rows, and a layer's size follows the change, not the state.

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
use blossom_store::{FileCheckpoints, OpenMode, SimFs, StoreIdentity, Vfs, WriteFate};
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
fn chain(k: &Store, fs: &SimFs) -> blossom_store::ChainInfo {
    FileCheckpoints::from_existing(Arc::new(fs.clone()), &k.dir)
        .chain()
        .unwrap()
        .unwrap()
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
fn layered_checkpoints_recover_exactly_across_restarts_and_compactions() {
    let k = Store::new();
    let fs = SimFs::default();
    let forget = k.artifact.rel_named("forget").unwrap();
    let mut t = 10i64;
    let mut saw_layers = 0;
    let mut saw_compaction = false;
    for run in 0..2 {
        let mut d = k.boot(&fs);
        let conn = ConnId(1);
        // A recovery that replayed WAL records must write a full image first.
        start(&mut d, conn, t + 1_000_000_000_000 * run);
        t += 1_000_000_000_000 * run + 10;
        let mut last_layers = None;
        // 61 chunks: the last is after the last checkpoint, so a restart replays WAL records past it.
        for i in 0..61u64 {
            let chunk = format!("run {run} chunk {i}").into_bytes();
            d.node
                .observe_stream(Observed::Bytes { conn, bytes: chunk })
                .unwrap();
            // Forget an earlier chunk now and then: layers carry deletes too.
            if i % 4 == 3 {
                let old = format!("run {run} chunk {}", i - 2).into_bytes();
                d.node.offer_input(forget, Arc::from(vec![Value::Bytes(Arc::from(&old[..]))]));
            }
            t += 1;
            d.run_until_quiescent(Instant(t)).unwrap();
            if i % 3 == 2 {
                d.checkpoint().unwrap();
                let c = chain(&k, &fs);
                if c.layers > 0 {
                    saw_layers += 1;
                }
                if last_layers.is_some_and(|l| l > 0) && c.layers == 0 {
                    saw_compaction = true;
                }
                last_layers = Some(c.layers);
                assert_eq!(recovered(&k, &fs), k.stored(&d), "run {run} after chunk {i}: {c:?}");
            }
        }
    }
    assert!(saw_layers > 10, "only {saw_layers} layered checkpoints");
    assert!(saw_compaction, "the chain never compacted");
}

#[test]
fn a_layer_is_the_size_of_the_change_not_the_state() {
    let k = Store::new();
    let fs = SimFs::default();
    let mut d = k.boot(&fs);
    let conn = ConnId(1);
    start(&mut d, conn, 1);
    let mut t = 10;
    let mut layer_sizes = Vec::new();
    let mut prev = None;
    // Five chunks per checkpoint, 400 chunks: the state grows 80-fold.
    for i in 0..400u64 {
        let chunk = format!("chunk {i:05}").into_bytes();
        d.node.observe_stream(Observed::Bytes { conn, bytes: chunk }).unwrap();
        t += 1;
        d.run_until_quiescent(Instant(t)).unwrap();
        if i % 5 == 4 {
            d.checkpoint().unwrap();
            let c = chain(&k, &fs);
            if let Some(p) = prev
                && c.layers == p + 1
            {
                layer_sizes.push(c.layer_bytes);
            }
            prev = Some(c.layers);
        }
    }
    // Layers keep coming (compaction resets the chain only when its layers outgrow the image), and each new layer
    // is small whatever the state: the layer bytes per layer never grow with the state.
    assert!(layer_sizes.len() > 20, "{layer_sizes:?}");
    let per_layer: Vec<u64> = layer_sizes
        .windows(2)
        .filter(|w| w[1] > w[0])
        .map(|w| w[1] - w[0])
        .collect();
    let (first, last) = (per_layer[..5].iter().max().unwrap(), per_layer[per_layer.len() - 5..].iter().max().unwrap());
    assert!(last <= &(first * 2), "layers grew with the state: {first} bytes early, {last} late");
}
