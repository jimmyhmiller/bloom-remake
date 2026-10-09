//! The `.bls` profile: running a compiled Blossom program in the synchronous-round world (TEST-006).
//!
//! Every node runs the program; rules placed at a role run only on that role's nodes. The world feeds the runtime
//! events: `boot()` in every node's first round (tick 0, SEM-012) and each physical timer's firings, with round `t`
//! at time `t × round` (LANGUAGE §15.2; [`crate::runtime`] says when a timer fires). A crashed node is frozen from
//! its crash round (CR-20); one that restarts boots again with its durable relations.

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{RelId, internal_error};
use blossom_oracle::{Oracle, Row};
use blossom_value::time::{Duration, NodeId, Tick};

use crate::sync::{CrashView, FaultSchedule, SimError, SyncConfig, SyncRun, SyncWorld};

/// An input event: `row` in the root input `rel` at `node`, in round `tick`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InputEvent {
    pub node: NodeId,
    pub tick: Tick,
    pub rel: RelId,
    pub row: Row,
}

/// A compiled `.bls` program ready to run.
pub struct BlsSim<'a> {
    artifact: &'a BlsArtifact,
    oracle: Oracle,
    /// `boot()` and the timers.
    runtime: crate::runtime::Runtime,
    /// Whether runs connect the nodes' byte streams ([`crate::fabric`]); otherwise stream events are scheduled
    /// inputs, and requests to the host are only recorded.
    connect_streams: bool,
}

impl<'a> BlsSim<'a> {
    /// Prepares the program (stratifies it and plans every rule) for a run seeded with `seed` (SEM-084). The
    /// program may declare no host functions; [`BlsSim::with_externs`] binds them.
    pub fn new(artifact: &'a BlsArtifact, seed: blossom_value::Seed) -> Result<BlsSim<'a>, SimError> {
        BlsSim::with_externs(
            artifact,
            seed,
            std::sync::Arc::new(blossom_value::ExternRegistry::new()),
        )
    }

    /// [`BlsSim::new`] with the host functions the program's `extern fn`s call.
    pub fn with_externs(
        artifact: &'a BlsArtifact,
        seed: blossom_value::Seed,
        externs: std::sync::Arc<blossom_value::ExternRegistry>,
    ) -> Result<BlsSim<'a>, SimError> {
        let members = artifact
            .members()
            .map_err(|e| SimError::Load(blossom_base::internal_error!("the deployment's members: {e}").into()))?;
        let oracle = Oracle::with_externs(artifact.program.clone(), blossom_oracle::Limits::default(), externs)
            .map_err(SimError::Load)?
            .with_roles(artifact.roles.clone())
            .with_members(std::sync::Arc::new(members))
            .with_seed(seed)
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
        let runtime = crate::runtime::Runtime::of(artifact.program.get())?;
        Ok(BlsSim {
            artifact,
            oracle,
            runtime,
            connect_streams: false,
        })
    }

    /// Runs connect the nodes' byte streams: a connect stream dialing `sim://NODE/STREAM` reaches that node's listen
    /// stream ([`crate::fabric`]).
    pub fn connecting_streams(mut self) -> BlsSim<'a> {
        self.connect_streams = true;
        self
    }

    pub fn artifact(&self) -> &BlsArtifact {
        self.artifact
    }

    pub fn oracle(&self) -> &Oracle {
        &self.oracle
    }

    /// Runs rounds `0..=last` with the given input events under `faults`.
    pub fn run(
        &self,
        inputs: &[InputEvent],
        last: Tick,
        round: Duration,
        faults: &FaultSchedule,
        capture: bool,
    ) -> Result<SyncRun, SimError> {
        self.run_on(&self.oracle, inputs, last, round, faults, capture)
    }

    /// [`BlsSim::run`] with another evaluator of the program (the engine, for the differential suite).
    pub fn run_on<E: crate::Evaluator>(
        &self,
        eval: &E,
        inputs: &[InputEvent],
        last: Tick,
        round: Duration,
        faults: &FaultSchedule,
        capture: bool,
    ) -> Result<SyncRun, SimError> {
        let n = u32::try_from(self.artifact.nodes.len()).map_err(|_| internal_error!("too many nodes"))?;
        let mut world = SyncWorld::new(eval, n);
        if let Some((rel, row)) = self.runtime.boot_at(Tick(0)) {
            for node in 0..n {
                world.input(NodeId(node), Tick(0), rel, row.clone());
            }
        }
        for e in inputs {
            world.input(e.node, e.tick, e.rel, e.row.clone());
        }
        let (durable, boot, recovered) = crate::sync::restart_shape(self.artifact.program.get());
        world.run(
            &SyncConfig {
                first: Tick(0),
                last,
                crash_view: CrashView::Frozen,
                round,
                capture,
                halt: self.artifact.halt,
                timers: self.runtime.timers(&self.artifact.roles)?,
                durable,
                boot,
                recovered,
                streams: self.connect_streams.then(|| {
                    let names: Vec<std::sync::Arc<str>> = self
                        .artifact
                        .nodes
                        .iter()
                        .map(|n| std::sync::Arc::from(n.as_str()))
                        .collect();
                    crate::fabric::StreamsConfig::of(self.artifact.program.get(), &names, &self.artifact.roles)
                }),
                links: crate::sync::Links::of(self.artifact.program.get(), &self.artifact.roles, self.oracle.members()),
            },
            faults,
        )
    }
}
