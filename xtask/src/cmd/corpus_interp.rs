//! The `interp` backend's differential check (ARCHITECTURE §11.2–§11.3): the engine runs the same scenario as the
//! oracle, and the two runs must agree at every tick — every node's instance, what it received and replied, every
//! message — or fail with the same program error at the same node and tick.

use blossom_oracle::OracleError;
use blossom_sim::{SimError, SyncRun};

/// The engine's configuration for a deployment: its roles, node names and root seed.
pub(super) fn engine_config(
    roles: &[Option<blossom_base::RoleId>],
    names: &[blossom_base::Symbol],
    seed: blossom_value::Seed,
) -> blossom_engine::EngineConfig {
    blossom_engine::EngineConfig {
        roles: roles.to_vec(),
        node_names: names.iter().map(|n| std::sync::Arc::from(n.as_str())).collect(),
        seed: Some(seed),
        ..blossom_engine::EngineConfig::default()
    }
}

/// Whether the oracle's run `reference` and the engine's run `mine` agree; the first difference otherwise.
pub(super) fn compare(reference: &Result<SyncRun, SimError>, mine: &Result<SyncRun, SimError>) -> Result<(), String> {
    match (reference, mine) {
        (Ok(a), Ok(b)) => compare_runs(a, b),
        (
            Err(SimError::Node {
                node: n1,
                tick: t1,
                error: e1,
            }),
            Err(SimError::Node {
                node: n2,
                tick: t2,
                error: e2,
            }),
        ) => {
            let code = |e: &OracleError| match e {
                OracleError::Program { error, .. } => Some(error.code),
                _ => None,
            };
            if n1 == n2 && t1 == t2 && code(e1).is_some() && code(e1) == code(e2) {
                Ok(())
            } else {
                Err(format!(
                    "the oracle failed at node {} tick {} ({e1}); the engine at node {} tick {} ({e2})",
                    n1.0, t1.0, n2.0, t2.0
                ))
            }
        }
        (Ok(_), Err(e)) => Err(format!("the engine failed where the oracle did not: {e}")),
        (Err(e), Ok(_)) => Err(format!("the oracle failed ({e}) where the engine did not")),
        (Err(e1), Err(e2)) => Err(format!("the oracle failed ({e1}); the engine failed ({e2})")),
    }
}

fn compare_runs(a: &SyncRun, b: &SyncRun) -> Result<(), String> {
    if a.rounds.len() != b.rounds.len() {
        return Err(format!("{} rounds against {}", a.rounds.len(), b.rounds.len()));
    }
    for (t, (ra, rb)) in a.rounds.iter().zip(&b.rounds).enumerate() {
        for (n, (x, y)) in ra.iter().zip(rb).enumerate() {
            if x.ran != y.ran {
                return Err(format!("tick {t} node {n}: ran {} against {}", x.ran, y.ran));
            }
            if x.instance != y.instance {
                let rels: std::collections::BTreeSet<_> = x.instance.rels.keys().chain(y.instance.rels.keys()).collect();
                for rel in rels {
                    let (ox, oy) = (x.instance.rels.get(rel), y.instance.rels.get(rel));
                    if ox != oy {
                        let only_oracle: Vec<_> = ox
                            .into_iter()
                            .flatten()
                            .filter(|r| oy.is_none_or(|s| !s.contains(*r)))
                            .take(3)
                            .collect();
                        let only_engine: Vec<_> = oy
                            .into_iter()
                            .flatten()
                            .filter(|r| ox.is_none_or(|s| !s.contains(*r)))
                            .take(3)
                            .collect();
                        return Err(format!(
                            "tick {t} node {n} relation {rel:?}: only the oracle has {only_oracle:?}; only the engine has \
                             {only_engine:?}"
                        ));
                    }
                }
            }
            if x.delivered != y.delivered || x.ingress != y.ingress {
                return Err(format!("tick {t} node {n}: different deliveries"));
            }
            if x.egress != y.egress {
                return Err(format!("tick {t} node {n}: replies {:?} against {:?}", x.egress, y.egress));
            }
        }
    }
    if a.messages != b.messages {
        let first = a
            .messages
            .iter()
            .zip(&b.messages)
            .find(|(x, y)| x != y)
            .map(|(x, y)| format!("{x:?} against {y:?}"))
            .unwrap_or_else(|| format!("{} messages against {}", a.messages.len(), b.messages.len()));
        return Err(format!("different messages: {first}"));
    }
    Ok(())
}
