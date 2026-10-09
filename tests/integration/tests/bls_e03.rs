//! Slice 4: e03's Raft election in the synchronous simulator. It needs deploy-time parameters, `argmax!`,
//! `rand_range`, `majority` and `bootstrap fresh`. Without faults exactly one leader emerges, and every server learns it.
//! Under many run seeds, with and without message loss and a crashed server, no term ever has two leaders.

use std::path::Path;

use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use std::collections::BTreeMap;

use blossom_sim::FaultSchedule;
use blossom_sim::bls::BlsSim;
use blossom_sim::sync::{Omission, SyncRun};
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
            .run(
                &[],
                last,
                Duration::from_nanos(25_000_000),
                &FaultSchedule::default(),
                false,
            )
            .unwrap();
        assert_one_winner_per_term(&run, won, last, seed);
        // At the end every server knows the same leader of the latest term.
        let known: Vec<Vec<blossom_oracle::Row>> = (0..3)
            .map(|n| {
                run.node_tick(last, NodeId(n))
                    .unwrap()
                    .instance
                    .rows(leader_of)
                    .cloned()
                    .collect()
            })
            .collect();
        if known.iter().all(|k| !k.is_empty()) {
            let latest = |k: &Vec<blossom_oracle::Row>| k.iter().max().cloned();
            assert!(
                known.iter().all(|k| latest(k) == latest(&known[0])),
                "seed {seed}: {known:?}"
            );
            elected += 1;
        }
    }
    assert!(elected >= 18, "only {elected} of 20 runs settled on a leader");
}

/// No term has two winners, over the whole run (a term's winners at different ticks count too).
#[cfg(test)]
fn assert_one_winner_per_term(run: &SyncRun, won: blossom_base::RelId, last: Tick, seed: u64) {
    let mut winners: BTreeMap<u64, u32> = BTreeMap::new();
    for t in 0..=last.0 {
        for n in 0..3 {
            let Some(nt) = run.node_tick(Tick(t), NodeId(n)) else {
                continue;
            };
            for r in nt.instance.rows(won) {
                let Some(Value::Int(term)) = r.first() else { continue };
                let term = u64::try_from(term.to_i128().unwrap()).unwrap();
                if let Some(other) = winners.insert(term, n) {
                    assert_eq!(
                        other, n,
                        "seed {seed}, tick {t}: nodes {other} and {n} both won term {term}"
                    );
                }
            }
        }
    }
}

/// SplitMix64, for the fault schedules.
#[cfg(test)]
fn mix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[test]
fn no_term_has_two_leaders_under_loss_and_a_crash() {
    let artifact = e03();
    let won = artifact.rel_named("won").unwrap();
    let last = Tick(80);
    let mut elections = 0usize;
    for seed in 0..30u64 {
        // Each message is lost with probability 1/5; half the runs crash a server partway.
        let mut faults = FaultSchedule::default();
        for t in 0..=last.0 {
            for from in 0..3u32 {
                for to in 0..3u32 {
                    if from != to && mix(seed << 32 ^ t << 8 ^ u64::from(from) << 4 ^ u64::from(to)).is_multiple_of(5) {
                        faults.omissions.insert(Omission {
                            from: NodeId(from),
                            to: NodeId(to),
                            send: Tick(t),
                        });
                    }
                }
            }
        }
        if seed % 2 == 1 {
            faults
                .crashes
                .insert(NodeId((seed % 3) as u32), Tick(20 + mix(seed) % 30));
        }
        let sim = BlsSim::new(&artifact, blossom_value::Seed::from_u64(seed)).unwrap();
        let run = sim
            .run(&[], last, Duration::from_nanos(25_000_000), &faults, false)
            .unwrap();
        assert_one_winner_per_term(&run, won, last, seed);
        elections += (0..3)
            .filter_map(|n| run.node_tick(last, NodeId(n)))
            .map(|nt| nt.instance.rows(won).count())
            .sum::<usize>();
    }
    assert!(
        elections > 30,
        "only {elections} wins across the runs: the faults stopped every election"
    );
}
