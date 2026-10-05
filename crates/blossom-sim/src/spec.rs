//! A compiled program on the synchronous-round world, and its outcome spec judged at EOT (ARCHITECTURE §8.1),
//! under the artifact's [`Profile`]:
//!
//! - Molly's (LANGUAGE §21.1): round `k` is tick `k` (CR-13), facts `p(…)@k` are input events of tick `k`, no node
//!   runs at tick 0, and a crashed node keeps running without sending ([`CrashView::MollyContinue`]);
//! - Blossom's: tick 0 is the boot tick, the runtime feeds `boot()` and timers ([`crate::runtime`]), and a crashed
//!   node is frozen ([`CrashView::Frozen`], CR-20).
//!
//! The spec reads `pre` and `post` at EOT (TEST-022). The spec engine feeds the spec program with every node's tuples
//! at EOT (each prefixed with its node), the snapshots at fixed ticks, and the crash oracle.

use std::collections::BTreeSet;
use std::sync::Arc;

use blossom_artifact::sim::{Profile, SimArtifact, SpecFeed};
use blossom_base::{RelId, internal_error};
use blossom_ir::obs::FiringRecord;
use blossom_oracle::{Ingress, Instance, Oracle, Row, TickInput};
use blossom_value::{
    Value,
    time::{NodeId, Tick},
    value::IntValue,
};

use crate::runtime::{Runtime, role_of};
use crate::sync::{CrashView, FaultSchedule, SimError, SyncConfig, SyncRun, SyncWorld, now_at};

/// A compiled program ready to run: its protocol and spec oracles.
pub struct SpecSim<'a> {
    artifact: &'a SimArtifact,
    protocol: Oracle,
    spec: Option<Oracle>,
    runtime: Runtime,
    /// The deployment's byte streams, which a run connects (`None` when no node runs one).
    streams: Option<crate::fabric::StreamsConfig>,
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

impl<'a> SpecSim<'a> {
    /// A simulator for `artifact`, whose programs may declare no host functions; [`SpecSim::with_externs`] binds
    /// them.
    pub fn new(artifact: &'a SimArtifact) -> Result<SpecSim<'a>, SimError> {
        SpecSim::with_externs(artifact, std::sync::Arc::new(blossom_value::ExternRegistry::new()))
    }

    /// [`SpecSim::new`] with the host functions the protocol's and the spec's `extern fn`s call.
    pub fn with_externs(
        artifact: &'a SimArtifact,
        externs: std::sync::Arc<blossom_value::ExternRegistry>,
    ) -> Result<SpecSim<'a>, SimError> {
        let limits = blossom_oracle::Limits::default();
        let protocol = Oracle::with_externs(artifact.protocol.clone(), limits, externs.clone())
            .map_err(SimError::Load)?
            .with_roles(artifact.roles.clone())
            .with_seed(artifact.seed)
            .and_then(|o| {
                o.with_node_names(
                    artifact
                        .nodes
                        .iter()
                        .map(|n| std::sync::Arc::from(n.as_str()))
                        .collect(),
                )
            })
            .map_err(SimError::Load)?;
        let runtime = match artifact.profile {
            Profile::Molly => Runtime::default(),
            Profile::Blossom { .. } => Runtime::of(artifact.protocol.get())?,
        };
        let spec = match &artifact.spec {
            Some(s) => Some(Oracle::with_externs(s.program.clone(), limits, externs).map_err(SimError::Load)?),
            None => None,
        };
        let names: Vec<Arc<str>> = artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect();
        let streams = crate::fabric::StreamsConfig::of(artifact.protocol.get(), &names, &artifact.roles);
        Ok(SpecSim {
            artifact,
            protocol,
            spec,
            runtime,
            streams: streams.any().then_some(streams),
        })
    }

    pub fn artifact(&self) -> &SimArtifact {
        self.artifact
    }

    /// The crash view of the artifact's profile.
    pub fn crash_view(&self) -> CrashView {
        if self.artifact.profile.frozen() {
            CrashView::Frozen
        } else {
            CrashView::MollyContinue
        }
    }

    /// Runs ticks `0..=last` under `faults` in the artifact's profile.
    pub fn run(&self, last: Tick, faults: &FaultSchedule, capture: bool) -> Result<SyncRun, SimError> {
        self.run_with_view(last, faults, capture, self.crash_view())
    }

    /// Runs ticks `0..=last` under `faults` with the given crash view (the synchronous test harness uses CR-20's
    /// frozen view for every program; Molly's is LDFI's for `.ded` programs).
    pub fn run_with_view(
        &self,
        last: Tick,
        faults: &FaultSchedule,
        capture: bool,
        crash_view: CrashView,
    ) -> Result<SyncRun, SimError> {
        self.run_on(&self.protocol, last, faults, capture, crash_view)
    }

    /// [`SpecSim::run_with_view`] with another evaluator of the protocol (the engine, for the differential suite).
    pub fn run_on<E: crate::Evaluator>(
        &self,
        eval: &E,
        last: Tick,
        faults: &FaultSchedule,
        capture: bool,
        crash_view: CrashView,
    ) -> Result<SyncRun, SimError> {
        let nodes = u32::try_from(self.artifact.nodes.len()).map_err(|_| internal_error!("too many nodes"))?;
        let mut world = SyncWorld::new(eval, nodes);
        let profile = self.artifact.profile;
        for t in profile.first_tick().0..=last.0 {
            for n in 0..nodes {
                for (rel, row) in self.scheduled(NodeId(n), Tick(t)) {
                    world.input(NodeId(n), Tick(t), rel, row);
                }
                for m in self.ingress(NodeId(n), Tick(t)) {
                    world.ingress(NodeId(n), Tick(t), m);
                }
            }
        }
        let (durable, boot, recovered) = crate::sync::restart_shape(self.artifact.protocol.get());
        world.run(
            &SyncConfig {
                first: profile.first_tick(),
                last,
                crash_view,
                round: profile.round(),
                capture,
                halt: self.artifact.halt,
                timers: self.runtime.timers(&self.artifact.roles)?,
                durable,
                boot,
                recovered,
                streams: self.streams.clone(),
            },
            faults,
        )
    }

    /// The events of `node` at `tick` a run schedules: its inputs, `boot()` and its node statics. The timers'
    /// firings are not here: the round loop makes them (a guarded timer's depend on the node's previous round, and
    /// a restart starts every timer's count again).
    pub fn scheduled(&self, node: NodeId, tick: Tick) -> Vec<(RelId, Row)> {
        let mut events: Vec<(RelId, Row)> = self
            .artifact
            .inputs
            .iter()
            .filter(|f| f.node == node && f.tick == tick)
            .map(|f| (f.rel, Arc::from(f.row.clone())))
            .collect();
        events.extend(self.runtime.boot_at(tick));
        events.extend(
            self.artifact
                .statics
                .iter()
                .filter(|s| s.node == node)
                .map(|s| (s.rel, Arc::from(s.row.clone()))),
        );
        events
    }

    /// Every event of a first incarnation of `node` at `tick`: [`SpecSim::scheduled`]'s and the unguarded timers'
    /// firings.
    pub fn events(&self, node: NodeId, tick: Tick) -> Result<Vec<(RelId, Row)>, SimError> {
        let mut events = self.scheduled(node, tick);
        events.extend(self.runtime.timer_firings_at(
            role_of(&self.artifact.roles, node),
            tick,
            self.artifact.profile.round(),
        )?);
        Ok(events)
    }

    /// The client sessions' messages to `node` at `tick`.
    pub fn ingress(&self, node: NodeId, tick: Tick) -> Vec<Ingress> {
        self.artifact
            .ingress
            .iter()
            .filter(|f| f.node == node && f.tick == tick)
            .map(|f| Ingress {
                rel: f.rel,
                session: f.session,
                row: Arc::from(f.row.clone()),
            })
            .collect()
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
        self.outcome_of(eot, &instances, &run.faults, capture)
    }

    /// Evaluates the outcome spec at `eot` over every node's instance at a tick, as `instances` gives them (one per
    /// node, in node order; `None` for a tick it does not have), with the faults of the run: `crashed(n)` holds for a
    /// node that is down at `eot` (one that restarted is up), the `.ded` crash oracle lists every crash.
    pub fn outcome_of<'i>(
        &self,
        eot: Tick,
        instances: &dyn Fn(Tick) -> Option<Vec<&'i Instance>>,
        faults: &FaultSchedule,
        capture: bool,
    ) -> Result<Outcome, SimError> {
        let crashes = &faults.crashes;
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
                SpecFeed::Crashed { spec: rel } => {
                    for node in crashes.keys() {
                        if faults.crashed(*node, eot) {
                            events.push((rel, Arc::from(vec![Value::Node(*node)])));
                        }
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
                incarnation: 1,
                node: NodeId(0),
                tick: eot,
                now: now_at(self.artifact.profile.round(), eot)?,
                carried: &Instance::default(),
                events: &events,
                delivered: &[],
                ingress: &[],
                capture,
                blobs: &blossom_value::NoBlobs,
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

    /// Runs one node's tick directly (for searches that step every node themselves), with its events at `tick`.
    pub fn step(
        &self,
        node: NodeId,
        tick: Tick,
        carried: &Instance,
        delivered: &[blossom_oracle::Delivery],
    ) -> Result<blossom_oracle::TickOutput, SimError> {
        // A guarded timer's firings depend on the node's previous round, which this one-round step does not see.
        if self.runtime.has_guarded() {
            return Err(blossom_base::unimplemented_error!(
                "LANG-172",
                "stepping a program with a guarded timer one round at a time (`SpecSim::step`)"
            )
            .into());
        }
        // Nor does it connect streams: the fabric between the nodes lives in the round loop.
        if self.streams.is_some() {
            return Err(blossom_base::unimplemented_error!(
                "TEST-146",
                "stepping a program with streams one round at a time (`SpecSim::step`)"
            )
            .into());
        }
        let events = self.events(node, tick)?;
        let ingress = self.ingress(node, tick);
        self.protocol
            .tick(&TickInput {
                incarnation: 1,
                node,
                tick,
                now: now_at(self.artifact.profile.round(), tick)?,
                carried,
                events: &events,
                delivered,
                ingress: &ingress,
                capture: false,
                blobs: &blossom_value::NoBlobs,
            })
            .map_err(|error| SimError::Node { node, tick, error })
    }

    /// Every node's tuples of `ded`'s protocol relation, prefixed with the node, as rows of `rel`.
    fn snapshot(
        &self,
        instances: Option<&[&Instance]>,
        rel: RelId,
        ded: blossom_artifact::sim::LogicalIdx,
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
                full.extend(row.iter().map(trace_value));
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

/// A protocol value as a spec's trace holds it: a blob as its reference (its content's hash, then its length as an
/// 8-byte big-endian integer), anything else as itself.
pub fn trace_value(v: &Value) -> Value {
    match v {
        Value::Blob(r) => {
            let mut b = r.hash.to_vec();
            b.extend_from_slice(&r.len.to_be_bytes());
            Value::Bytes(b.into())
        }
        other => other.clone(),
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
