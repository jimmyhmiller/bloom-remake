//! The cluster simulator's livelock guard (S8 item 2): a node still ready after `LIVELOCK_TICKS` ticks at one instant
//! never quiesces, and the run stops with an error naming the relations that keep changing, instead of hanging.

use std::path::Path;

use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::durable::DurableSchema;
use blossom_sim::cluster::{Cluster, ClusterConfig, NoKvClients};

#[test]
fn a_node_that_never_quiesces_stops_the_run_and_names_what_changes() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sim/livelock.bls");
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    let artifact = result.unwrap_or_else(|e| panic!("livelock.bls: {e:?}")).0;
    let schema = DurableSchema::of(artifact.program.get());
    let cfg = ClusterConfig {
        duration: 1_000_000_000,
        ..ClusterConfig::default()
    };
    let cluster = Cluster::new(
        &artifact,
        &schema,
        blossom_value::Seed::from_u64(1),
        Vec::new(),
        Box::new(NoKvClients),
        cfg,
    )
    .unwrap();
    let err = match cluster.run() {
        Ok(_) => panic!("a node that changes its state at every tick ran to the end"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("livelocks"), "{err}");
    assert!(
        err.contains("\"count\""),
        "the report names the relation that changes: {err}"
    );
    assert!(!err.contains("\"still\""), "the report names only what changes: {err}");
}
