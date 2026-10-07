//! The evaluator seams (ARCHITECTURE §5.1).
//!
//! - [`Evaluator`] is the reference interface: a tick reads a [`TickInput`] (carried state included) and produces a
//!   full [`TickOutput`]. The oracle implements it; the simulator, provenance and LDFI use it, since they look at
//!   every relation at every tick.
//! - [`Executor`] is what a node runs: a stateful evaluator that keeps the carried state itself, takes only the
//!   tick's inputs, and reports the changes to the carried state. The engine implements it natively, at a cost
//!   proportional to the change; [`OracleExecutor`] runs any `Evaluator` behind it (at the oracle's cost).
//!
//! The architecture's evaluator is word-level (the engine ingests encoded batches); this one is value-level.

use std::collections::BTreeMap;

use blossom_base::{FnId, RelId, RuleId};
use blossom_ir::tick::{
    Changes, EvalError, FnWork, Instance, Row, RuleWork, StepInput, StepOutput, TickInput, TickOutput,
};
use blossom_oracle::Oracle;

/// Runs one node's tick from an explicit carried state (the reference interface).
pub trait Evaluator: Send + Sync {
    fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, EvalError>;
}

impl Evaluator for Oracle {
    fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, EvalError> {
        Oracle::tick(self, input)
    }
}

impl<E: Evaluator + ?Sized> Evaluator for std::sync::Arc<E> {
    fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, EvalError> {
        (**self).tick(input)
    }
}

impl<E: Evaluator + ?Sized> Evaluator for &E {
    fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, EvalError> {
        (**self).tick(input)
    }
}

/// A stateful evaluator: what a node runs.
pub trait Executor: Send {
    /// Starts from `carried` (at boot: the recovered durable state). The next step continues from it.
    fn reset(&mut self, carried: Instance) -> Result<(), EvalError>;
    /// Runs one tick from the executor's own carried state. `observe` names relations whose final contents at this
    /// tick the caller needs (the `halt` output).
    fn step(&mut self, input: &StepInput<'_>, observe: &[RelId]) -> Result<StepOutput, EvalError>;
    /// The carried rows of `rel` (for admission's `principal in REL` on a volatile table).
    fn carried_rows(&self, rel: RelId) -> Result<Vec<Row>, EvalError>;
    /// The whole carried state, for inspection (O(state)).
    fn carried(&self) -> Result<Instance, EvalError>;
    /// The join work done so far, in rows examined, if the executor measures it.
    fn rows_examined(&self) -> Option<u64>;
    /// The work of each rule so far (rows examined, expression nodes evaluated), if the executor measures it.
    fn work_by_rule(&self) -> Option<BTreeMap<RuleId, RuleWork>>;
    /// Starts (afresh) or stops counting each function's work; false if the executor cannot.
    fn profile_functions(&mut self, on: bool) -> bool;
    /// Each function's work since counting started, if it is counted.
    fn work_by_function(&self) -> Option<BTreeMap<FnId, FnWork>>;
    /// Whether a row the executor keeps beyond its carried state holds `b`: a blob it may still copy into a durable
    /// row or a request without creating it again. (The node counts the carried rows' blobs itself, from each tick's
    /// changes.)
    fn holds_blob(&self, b: &blossom_value::BlobRef) -> bool;
}

/// Any [`Evaluator`] as an [`Executor`]: it keeps the carried state and diffs each tick's next state against it.
pub struct OracleExecutor<E: Evaluator> {
    eval: E,
    carried: Instance,
}

impl<E: Evaluator> OracleExecutor<E> {
    pub fn new(eval: E) -> OracleExecutor<E> {
        OracleExecutor {
            eval,
            carried: Instance::default(),
        }
    }
}

impl<E: Evaluator> Executor for OracleExecutor<E> {
    fn reset(&mut self, carried: Instance) -> Result<(), EvalError> {
        self.carried = carried;
        Ok(())
    }

    fn step(&mut self, input: &StepInput<'_>, observe: &[RelId]) -> Result<StepOutput, EvalError> {
        let out = self.eval.tick(&TickInput {
            node: input.node,
            incarnation: input.incarnation,
            tick: input.tick,
            now: input.now,
            carried: &self.carried,
            events: input.events,
            delivered: input.delivered,
            ingress: input.ingress,
            capture: false,
            blobs: input.blobs,
        })?;
        let changes = Changes::between(&self.carried, &out.next);
        let observed: BTreeMap<RelId, Vec<Row>> = observe
            .iter()
            .map(|r| (*r, out.instance.rows(*r).cloned().collect()))
            .collect();
        self.carried = out.next;
        Ok(StepOutput {
            changes,
            outbox: out.outbox,
            egress: out.egress,
            host: out.host,
            observed,
            blobs: out.blobs,
        })
    }

    fn carried_rows(&self, rel: RelId) -> Result<Vec<Row>, EvalError> {
        Ok(self.carried.rows(rel).cloned().collect())
    }

    fn carried(&self) -> Result<Instance, EvalError> {
        Ok(self.carried.clone())
    }

    /// The reference evaluator does not count its work.
    fn rows_examined(&self) -> Option<u64> {
        None
    }

    fn work_by_rule(&self) -> Option<BTreeMap<RuleId, RuleWork>> {
        None
    }

    fn profile_functions(&mut self, _on: bool) -> bool {
        false
    }

    fn work_by_function(&self) -> Option<BTreeMap<FnId, FnWork>> {
        None
    }

    /// The reference evaluator recomputes every derived row, and the blobs they hold, at every tick: only the
    /// carried rows keep blobs across ticks.
    /// The reference evaluator keeps only its carried state: it derives every other row afresh each tick, which
    /// creates their blobs again.
    fn holds_blob(&self, _b: &blossom_value::BlobRef) -> bool {
        false
    }
}

impl<X: Executor + ?Sized> Executor for Box<X> {
    fn reset(&mut self, carried: Instance) -> Result<(), EvalError> {
        (**self).reset(carried)
    }

    fn step(&mut self, input: &StepInput<'_>, observe: &[RelId]) -> Result<StepOutput, EvalError> {
        (**self).step(input, observe)
    }

    fn carried_rows(&self, rel: RelId) -> Result<Vec<Row>, EvalError> {
        (**self).carried_rows(rel)
    }

    fn carried(&self) -> Result<Instance, EvalError> {
        (**self).carried()
    }

    fn rows_examined(&self) -> Option<u64> {
        (**self).rows_examined()
    }

    fn work_by_rule(&self) -> Option<BTreeMap<RuleId, RuleWork>> {
        (**self).work_by_rule()
    }

    fn profile_functions(&mut self, on: bool) -> bool {
        (**self).profile_functions(on)
    }

    fn work_by_function(&self) -> Option<BTreeMap<FnId, FnWork>> {
        (**self).work_by_function()
    }

    fn holds_blob(&self, b: &blossom_value::BlobRef) -> bool {
        (**self).holds_blob(b)
    }
}

impl Executor for blossom_engine::Engine {
    fn reset(&mut self, carried: Instance) -> Result<(), EvalError> {
        blossom_engine::Engine::reset(self, carried)
    }

    fn step(&mut self, input: &StepInput<'_>, observe: &[RelId]) -> Result<StepOutput, EvalError> {
        blossom_engine::Engine::step(self, input, observe)
    }

    fn carried_rows(&self, rel: RelId) -> Result<Vec<Row>, EvalError> {
        blossom_engine::Engine::carried_rows(self, rel)
    }

    fn carried(&self) -> Result<Instance, EvalError> {
        self.carried_instance()
    }

    fn rows_examined(&self) -> Option<u64> {
        Some(blossom_engine::Engine::rows_examined(self))
    }

    fn work_by_rule(&self) -> Option<BTreeMap<RuleId, RuleWork>> {
        Some(blossom_engine::Engine::work_by_rule(self).clone())
    }

    fn profile_functions(&mut self, on: bool) -> bool {
        self.set_profile_functions(on);
        true
    }

    fn work_by_function(&self) -> Option<BTreeMap<FnId, FnWork>> {
        blossom_engine::Engine::work_by_function(self)
    }

    fn holds_blob(&self, b: &blossom_value::BlobRef) -> bool {
        blossom_engine::Engine::holds_blob(self, b)
    }
}

/// The engine behind the reference interface, for the simulator and the differential suite: one engine per node,
/// created at the node's first tick from the carried state that tick names. Every later tick of the same incarnation
/// must name the state that engine carried (the harness feeds each node its own last output), which is checked; a new
/// incarnation (a restart) starts the engine over from the state its first tick names (what survived the crash).
pub struct EngineEvaluator {
    program: blossom_ir::ValidatedProgram,
    cfg: blossom_engine::EngineConfig,
    engines: std::sync::Mutex<BTreeMap<blossom_value::time::NodeId, (blossom_engine::Engine, u64)>>,
    /// Check at every tick that the carried state named is the one the engine carried (O(state) per tick).
    pub check_carried: bool,
}

impl EngineEvaluator {
    pub fn new(program: blossom_ir::ValidatedProgram, cfg: blossom_engine::EngineConfig) -> EngineEvaluator {
        EngineEvaluator {
            program,
            cfg,
            engines: std::sync::Mutex::new(BTreeMap::new()),
            check_carried: true,
        }
    }
}

impl Evaluator for EngineEvaluator {
    fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, EvalError> {
        let mut engines = self
            .engines
            .lock()
            .map_err(|_| blossom_base::internal_error!("the engine table's lock is poisoned"))?;
        let (engine, incarnation) = match engines.entry(input.node) {
            std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::btree_map::Entry::Vacant(v) => {
                let mut engine = blossom_engine::Engine::new(self.program.clone(), input.node, self.cfg.clone())?;
                engine.reset(input.carried.clone())?;
                v.insert((engine, input.incarnation))
            }
        };
        if *incarnation != input.incarnation {
            engine.reset(input.carried.clone())?;
            *incarnation = input.incarnation;
        }
        if self.check_carried {
            let mine = engine.carried_instance()?;
            if mine != *input.carried {
                return Err(blossom_base::internal_error!(
                    "node {} ticked from a carried state that is not the engine's own",
                    input.node.0
                )
                .into());
            }
        }
        engine.tick_full(input)
    }
}

/// Which evaluator a node runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    /// The incremental engine: a tick costs in proportion to what it changes.
    #[default]
    Engine,
    /// The reference oracle behind [`OracleExecutor`]: a tick re-evaluates the whole state.
    Oracle,
}

impl std::str::FromStr for Backend {
    type Err = String;

    fn from_str(s: &str) -> Result<Backend, String> {
        match s {
            "engine" => Ok(Backend::Engine),
            "oracle" => Ok(Backend::Oracle),
            other => Err(format!("unknown evaluator `{other}` (engine or oracle)")),
        }
    }
}

/// Makes the executors of one deployment's nodes: the program placed on its nodes (roles, stable names) and seeded.
pub struct Executors {
    backend: Backend,
    program: blossom_ir::ValidatedProgram,
    oracle: std::sync::Arc<Oracle>,
    engine: blossom_engine::EngineConfig,
}

impl Executors {
    pub fn new(
        backend: Backend,
        program: blossom_ir::ValidatedProgram,
        roles: Vec<Option<blossom_base::RoleId>>,
        names: Vec<std::sync::Arc<str>>,
        seed: blossom_value::Seed,
        externs: std::sync::Arc<blossom_value::ExternRegistry>,
    ) -> Result<Executors, EvalError> {
        let oracle = std::sync::Arc::new(
            Oracle::with_externs(program.clone(), blossom_oracle::Limits::default(), externs.clone())?
                .with_roles(roles.clone())
                .with_seed(seed)?
                .with_node_names(names.clone())?,
        );
        let engine = blossom_engine::EngineConfig {
            roles,
            node_names: names,
            seed: Some(seed),
            externs,
            ..blossom_engine::EngineConfig::default()
        };
        Ok(Executors {
            backend,
            program,
            oracle,
            engine,
        })
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// The oracle for the deployment (its static facts serve admission whichever backend runs).
    pub fn oracle(&self) -> &std::sync::Arc<Oracle> {
        &self.oracle
    }

    /// A fresh executor for node `node`; the node resets it to its recovered state at boot.
    pub fn make(&self, node: blossom_value::time::NodeId) -> Result<Box<dyn Executor>, EvalError> {
        Ok(match self.backend {
            Backend::Engine => Box::new(blossom_engine::Engine::new(
                self.program.clone(),
                node,
                self.engine.clone(),
            )?),
            Backend::Oracle => Box::new(OracleExecutor::new(self.oracle.clone())),
        })
    }
}
