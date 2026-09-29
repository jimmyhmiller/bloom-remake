//! The `.ded` profile (LANGUAGE §21.1, ARCHITECTURE §8.1): a compiled Molly program on the synchronous-round world
//! under [`CrashView::MollyContinue`], and its outcome spec judged at EOT.
//!
//! Molly's round `k` is tick `k` (CR-13): facts `p(…)@k` are input events of tick `k`, tick 0 has no events, and the
//! spec reads `pre` and `post` at EOT (TEST-022). The spec engine feeds the spec program with every node's tuples at
//! EOT (each prefixed with its node), the snapshots at fixed ticks that `p(…)@k` atoms read, and the crash oracle
//! `crash(Observer, Node, Time)`, in which every node observes every crash of the run.

use std::collections::BTreeSet;
use std::sync::Arc;

use blossom_artifact::ded::{DedArtifact, SpecFeed};
use blossom_base::{RelId, internal_error};
use blossom_ir::obs::FiringRecord;
use blossom_oracle::{Instance, Oracle, Row, TickInput};
use blossom_value::{
    Value,
    time::{NodeId, Tick},
    value::IntValue,
};

use crate::sync::{CrashView, FaultSchedule, SimError, SyncConfig, SyncRun, SyncWorld};

/// A compiled `.ded` program ready to run: its protocol and spec oracles.
pub struct DedSim<'a> {
    artifact: &'a DedArtifact,
    protocol: Oracle,
    spec: Option<Oracle>,
}

/// The spec's verdict inputs for one run: `pre` and `post` at EOT.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub eot: Tick,
    pub pre: BTreeSet<Row>,
    pub post: BTreeSet<Row>,
    /// The spec program's instance at EOT, its inputs included.
    pub instance: Instance,
    /// The spec's firings (when capturing).
    pub firings: Vec<FiringRecord>,
}

impl<'a> DedSim<'a> {
    pub fn new(artifact: &'a DedArtifact) -> Result<DedSim<'a>, SimError> {
        let protocol = Oracle::new(artifact.protocol.clone()).map_err(|error| SimError::Node {
            node: NodeId(0),
            tick: Tick(0),
            error,
        })?;
        let spec = match &artifact.spec {
            Some(s) => Some(Oracle::new(s.program.clone()).map_err(|error| SimError::Node {
                node: NodeId(0),
                tick: Tick(0),
                error,
            })?),
            None => None,
        };
        Ok(DedSim {
            artifact,
            protocol,
            spec,
        })
    }

    pub fn artifact(&self) -> &DedArtifact {
        self.artifact
    }

    /// Runs ticks `0..=last` under `faults` (Molly's crash view).
    pub fn run(&self, last: Tick, faults: &FaultSchedule, capture: bool) -> Result<SyncRun, SimError> {
        let nodes = u32::try_from(self.artifact.nodes.len()).map_err(|_| internal_error!("too many nodes"))?;
        let mut world = SyncWorld::new(&self.protocol, nodes);
        for f in &self.artifact.inputs {
            world.input(f.node, f.tick, f.rel, Arc::from(f.row.clone()));
        }
        world.run(
            &SyncConfig {
                last,
                crash_view: CrashView::MollyContinue,
                capture,
            },
            faults,
        )
    }

    /// Evaluates the outcome spec on `run` at `eot`. Fails when the program has no spec (CR-30).
    pub fn outcome(&self, run: &SyncRun, eot: Tick, capture: bool) -> Result<Outcome, SimError> {
        let instances = |tick: Tick| -> Option<Vec<&Instance>> {
            (0..self.artifact.nodes.len())
                .map(|n| {
                    let node = NodeId(u32::try_from(n).ok()?);
                    run.node_tick(tick, node).map(|nt| &nt.instance)
                })
                .collect()
        };
        self.outcome_of(eot, &instances, &run.faults.crashes, capture)
    }

    /// Evaluates the outcome spec at `eot` over every node's instance at a tick, as `instances` gives them (one per
    /// node, in node order; `None` for a tick it does not have), with the crashes of the run.
    pub fn outcome_of<'i>(
        &self,
        eot: Tick,
        instances: &dyn Fn(Tick) -> Option<Vec<&'i Instance>>,
        crashes: &std::collections::BTreeMap<NodeId, Tick>,
        capture: bool,
    ) -> Result<Outcome, SimError> {
        let (Some(spec), Some(oracle)) = (&self.artifact.spec, &self.spec) else {
            return Err(internal_error!("outcome requested for a program without `pre` and `post` (CR-30)").into());
        };
        let mut events: Vec<(RelId, Row)> = Vec::new();
        for feed in &spec.feeds {
            match *feed {
                SpecFeed::AtEot { spec: rel, rel: ded } => {
                    self.snapshot(instances(eot).as_deref(), rel, ded, &mut events)?;
                }
                SpecFeed::AtTick {
                    spec: rel,
                    rel: ded,
                    tick,
                } => {
                    if tick <= eot {
                        self.snapshot(instances(tick).as_deref(), rel, ded, &mut events)?;
                    }
                }
                SpecFeed::Crash { spec: rel } => {
                    for observer in 0..self.artifact.nodes.len() {
                        let observer = node_id(observer)?;
                        for (node, at) in crashes {
                            let time = i64::try_from(at.0).map_err(|_| internal_error!("crash tick overflow"))?;
                            events.push((
                                rel,
                                Arc::from(vec![
                                    Value::Node(observer),
                                    Value::Node(*node),
                                    Value::Int(IntValue::I64(time)),
                                ]),
                            ));
                        }
                    }
                }
            }
        }
        let out = oracle
            .tick(&TickInput {
                node: NodeId(0),
                tick: eot,
                carried: &Instance::default(),
                events: &events,
                delivered: &[],
                capture,
            })
            .map_err(|error| SimError::Node {
                node: NodeId(0),
                tick: eot,
                error,
            })?;
        let rows = |rel: RelId| out.instance.rows(rel).cloned().collect::<BTreeSet<_>>();
        Ok(Outcome {
            eot,
            pre: rows(spec.pre),
            post: rows(spec.post),
            instance: out.instance,
            firings: out.firings,
        })
    }

    /// Runs one node's tick directly (for searches that step every node themselves): its input events at `tick`
    /// come from the program's facts.
    pub fn step(
        &self,
        node: NodeId,
        tick: Tick,
        carried: &Instance,
        delivered: &[blossom_oracle::Delivery],
    ) -> Result<blossom_oracle::TickOutput, SimError> {
        let events: Vec<(RelId, Row)> = self
            .artifact
            .inputs
            .iter()
            .filter(|f| f.node == node && f.tick == tick)
            .map(|f| (f.rel, Arc::from(f.row.clone())))
            .collect();
        self.protocol
            .tick(&TickInput {
                node,
                tick,
                carried,
                events: &events,
                delivered,
                capture: false,
            })
            .map_err(|error| SimError::Node { node, tick, error })
    }

    /// Every node's tuples of `ded`'s protocol relation, prefixed with the node, as rows of `rel`.
    fn snapshot(
        &self,
        instances: Option<&[&Instance]>,
        rel: RelId,
        ded: blossom_artifact::ded::DedRelIdx,
        out: &mut Vec<(RelId, Row)>,
    ) -> Result<(), SimError> {
        let protocol = self
            .artifact
            .rel(ded)
            .and_then(|r| r.protocol)
            .ok_or_else(|| internal_error!("a spec feed names a relation without a protocol relation"))?;
        let Some(instances) = instances else { return Ok(()) };
        for (n, instance) in instances.iter().enumerate() {
            let node = node_id(n)?;
            for row in instance.rows(protocol) {
                let mut full = Vec::with_capacity(row.len() + 1);
                full.push(Value::Node(node));
                full.extend(row.iter().cloned());
                out.push((rel, Arc::from(full)));
            }
        }
        Ok(())
    }

    /// The ticks whose instances the spec reads besides EOT (its `p(…)@k` atoms).
    pub fn snapshot_ticks(&self) -> BTreeSet<Tick> {
        self.artifact
            .spec
            .iter()
            .flat_map(|s| s.feeds.iter())
            .filter_map(|f| match f {
                SpecFeed::AtTick { tick, .. } => Some(*tick),
                _ => None,
            })
            .collect()
    }
}

fn node_id(i: usize) -> Result<NodeId, SimError> {
    u32::try_from(i)
        .map(NodeId)
        .map_err(|_| internal_error!("node index overflow").into())
}

/// Molly's oracle `isGood` (TEST-022): a run is good iff its `post` equals the failure-free run's, or every
/// failure-free `post` tuple the run lacks is also absent from its `pre`.
pub fn is_good(failure_free_post: &BTreeSet<Row>, outcome: &Outcome) -> bool {
    if outcome.post == *failure_free_post {
        return true;
    }
    failure_free_post
        .iter()
        .filter(|g| !outcome.post.contains(*g))
        .all(|g| !outcome.pre.contains(g))
}
