//! Slice 4: the Raft key-value store (`examples/e11_raft_kv.bls`) on three real nodes in the cluster simulator. Every
//! client history must be linearizable: without faults, under message loss, network partitions and crash-restarts.

use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::RelId;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::durable::DurableSchema;
use blossom_oracle::Row;
use blossom_sim::cluster::{ClientProtocol, Cluster, ClusterConfig, ClusterRun, Reply};
use blossom_sim::linearize::{KvInput, KvModel, KvOutput, Verdict, check_partitioned};
use blossom_value::Value;
use blossom_value::time::NodeId;
use blossom_value::value::IntValue;

#[cfg(test)]
fn raft_kv() -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/e11_raft_kv.bls");
    let nodes: Vec<NodeSpec> = ["s1", "s2", "s3"]
        .iter()
        .map(|n| NodeSpec {
            name: (*n).into(),
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

/// Runs the cluster with `cfg` and checks the history; returns the run.
#[cfg(test)]
fn run_and_check(artifact: &BlsArtifact, schema: &DurableSchema, cfg: ClusterConfig) -> ClusterRun {
    let seed = cfg.seed;
    let cluster = Cluster::new(
        artifact,
        schema,
        blossom_value::Seed::from_u64(seed),
        Vec::new(),
        Box::new(RaftProtocol::of(artifact)),
        cfg,
    )
    .unwrap();
    let run = cluster.run().unwrap();
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
    assert!(answered(&run) > 50, "only {} answered of {}", answered(&run), run.history.len());
}

#[test]
fn raft_kv_is_linearizable_under_loss_partitions_and_crashes() {
    let artifact = raft_kv();
    let schema = DurableSchema::of(artifact.program.get());
    let mut totals = (0usize, 0usize, 0u64, 0u64);
    for seed in 1..=6u64 {
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
    assert!(answered > 100, "only {answered} operations answered");
    assert!(unanswered > 0 && crashes > 3 && partitions > 3, "{unanswered} unanswered, {crashes} crashes, {partitions} partitions: the faults did not bite");
}
