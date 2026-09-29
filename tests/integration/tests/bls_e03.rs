//! Slice 4: e03's Raft election in the synchronous simulator. It needs deploy-time parameters, `argmax!`,
//! `rand_range`, `majority` and `bootstrap fresh`. Without faults exactly one leader emerges, and every server learns it;
//! under many run seeds, no term ever has two leaders.

use std::path::Path;

use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::BlsSim;
use blossom_value::Value;
use blossom_value::time::{Duration, NodeId, Tick};

#[cfg(test)]
fn e03() -> blossom_artifact::bls::BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/e03_raft_election.bls");
    let nodes: Vec<NodeSpec> = ["a", "b", "c"]
        .iter()
        .map(|n| NodeSpec {
            name: (*n).into(),
            role: Some("Server".into()),
        })
        .collect();
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    result.unwrap_or_else(|e| panic!("e03: {e:?}")).0
}

#[test]
fn one_leader_is_elected_and_every_server_learns_it() {
    let artifact = e03();
    let won = artifact.rel_named("won").unwrap();
    let leader_of = artifact.rel_named("leader_of").unwrap();
    let mut elected = 0;
    for seed in 0..20u64 {
        let sim = BlsSim::new(&artifact, blossom_value::Seed::from_u64(seed)).unwrap();
        let last = Tick(60);
        let run = sim
            .run(&[], last, Duration::from_nanos(25_000_000), &FaultSchedule::default(), false)
            .unwrap();
        // No term has two winners, at any tick.
        for t in 0..=last.0 {
            let mut winners: Vec<(u64, u32)> = Vec::new();
            for n in 0..3 {
                if let Some(nt) = run.node_tick(Tick(t), NodeId(n)) {
                    for r in nt.instance.rows(won) {
                        if let Some(Value::Int(term)) = r.first() {
                            winners.push((term.to_i128().unwrap() as u64, n));
                        }
                    }
                }
            }
            winners.sort();
            for w in winners.windows(2) {
                assert!(w[0].0 != w[1].0, "seed {seed}, tick {t}: two leaders of term {}", w[0].0);
            }
        }
        // At the end every server knows the same leader of the latest term.
        let known: Vec<Vec<blossom_oracle::Row>> = (0..3)
            .map(|n| run.node_tick(last, NodeId(n)).unwrap().instance.rows(leader_of).cloned().collect())
            .collect();
        if known.iter().all(|k| !k.is_empty()) {
            let latest = |k: &Vec<blossom_oracle::Row>| k.iter().max().cloned();
            assert!(known.iter().all(|k| latest(k) == latest(&known[0])), "seed {seed}: {known:?}");
            elected += 1;
        }
    }
    assert!(elected >= 18, "only {elected} of 20 runs settled on a leader");
}
