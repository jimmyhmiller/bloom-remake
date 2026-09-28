#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-oracle`: the naive per-tick Dedalus^L evaluator over `Value` with its own stratifier, and the choice-
//! validity checker. Deliberately independent of the kernel, the engine, the planner and the analyses.
//!
//! See ARCHITECTURE §1.2 and §11.2. Implemented by slice 1 (docs/design/SLICES.md) for the constructs the `.ded`
//! frontend produces; WP M4.1's remaining constructs (lattice columns, weighted relations, choices, lookups,
//! generators) fail with `Unimplemented`.
//!
//! The oracle is the executable definition of one node's tick (ARCHITECTURE §11.2):
//!
//! - the tick's instance starts as the carried state (last tick's `@next` heads), the tick's input events, the
//!   channel tuples delivered to the node, and the static facts;
//! - the deductive rules run stratum by stratum, each to its fixpoint by naive iteration (every rule re-run
//!   against the whole instance until nothing changes); an aggregate rule runs once its inputs are complete;
//! - on the completed instance, inductive heads become the next tick's carried state and async heads the outbox.
//!
//! With capture on, every distinct rule firing is reported as a [`FiringRecord`] (Tier C with the literal profile,
//! ARCHITECTURE §4.9): that is what provenance graphs and LDFI are built from.

mod eval;
mod expr;
mod plan;
mod strata;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_base::{InternalError, RelId, RuleId, Unimplemented};
use blossom_ir::ValidatedProgram;
use blossom_ir::obs::{FiringRecord, ProgramErrorRecord};
use blossom_value::{
    Value,
    time::{NodeId, Tick},
};

pub use strata::Stratum;

/// A tuple.
pub type Row = Arc<[Value]>;

/// Relation contents: every non-empty relation's rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Instance {
    pub rels: BTreeMap<RelId, BTreeSet<Row>>,
}

impl Instance {
    /// The rows of `rel` (empty when it has none).
    pub fn rows(&self, rel: RelId) -> impl Iterator<Item = &Row> {
        self.rels.get(&rel).into_iter().flatten()
    }

    /// Whether `rel` holds `row`.
    pub fn contains(&self, rel: RelId, row: &[Value]) -> bool {
        self.rels.get(&rel).is_some_and(|rows| rows.contains(row))
    }

    /// Adds a row; whether it was new.
    pub fn insert(&mut self, rel: RelId, row: Row) -> bool {
        self.rels.entry(rel).or_default().insert(row)
    }

    /// Whether no relation has a row.
    pub fn is_empty(&self) -> bool {
        self.rels.values().all(BTreeSet::is_empty)
    }
}

/// A channel tuple delivered to the node this tick. Column 0 of `row` is the destination, the node itself.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Delivery {
    pub rel: RelId,
    pub from: NodeId,
    pub row: Row,
}

/// A channel tuple the node sends: column 0 of `row` is the destination.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Send {
    pub rel: RelId,
    pub to: NodeId,
    pub row: Row,
}

/// Everything one tick of one node reads.
#[derive(Clone, Debug)]
pub struct TickInput<'a> {
    /// `$self`.
    pub node: NodeId,
    pub tick: Tick,
    /// Last tick's `@next` heads.
    pub carried: &'a Instance,
    /// The tick's input events.
    pub events: &'a [(RelId, Row)],
    /// The channel tuples delivered this tick.
    pub delivered: &'a [Delivery],
    /// Whether to report the tick's firings.
    pub capture: bool,
}

/// Everything one tick of one node produces.
#[derive(Clone, Debug, Default)]
pub struct TickOutput {
    /// The tick's final instance: every relation's contents at this tick.
    pub instance: Instance,
    /// The inductive heads: next tick's carried state.
    pub next: Instance,
    /// The async heads.
    pub outbox: BTreeSet<Send>,
    /// The distinct firings of the tick, in canonical order; empty unless capture was requested.
    pub firings: Vec<FiringRecord>,
}

/// Why the oracle could not evaluate.
#[derive(Debug, thiserror::Error)]
pub enum OracleError {
    /// The deductive rules do not stratify: a negated or aggregated read on a same-tick cycle (SEM-020).
    #[error("the program does not stratify: {0}")]
    NotStratifiable(String),
    /// A runtime hard error of the program at this tick (BLSRnnn, ARCHITECTURE §6.6).
    #[error("{} at tick {}: {}", .error.code, .tick.0, .error.detail)]
    Program { tick: Tick, error: ProgramErrorRecord },
    #[error(transparent)]
    Unimplemented(#[from] Unimplemented),
    #[error(transparent)]
    Internal(#[from] InternalError),
}

/// Evaluation limits.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Naive rounds per stratum before the tick fails with BLSR007 (CR-53).
    pub max_rounds: u32,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { max_rounds: 10_000 }
    }
}

/// The oracle for one program: its strata and rule plans, shared by every node and tick.
pub struct Oracle {
    program: ValidatedProgram,
    strata: Vec<Stratum>,
    plans: BTreeMap<RuleId, plan::RulePlan>,
    inductive: Vec<RuleId>,
    asynchronous: Vec<RuleId>,
    statics: Instance,
    limits: Limits,
}

impl Oracle {
    /// Prepares `program`: checks that every construct is one the oracle evaluates, stratifies the deductive rules
    /// and plans every rule.
    pub fn new(program: ValidatedProgram) -> Result<Oracle, OracleError> {
        Oracle::with_limits(program, Limits::default())
    }

    /// [`Oracle::new`] with explicit limits.
    pub fn with_limits(program: ValidatedProgram, limits: Limits) -> Result<Oracle, OracleError> {
        eval::check_supported(program.get())?;
        let strata = strata::stratify(program.get())?;
        let mut plans = BTreeMap::new();
        let mut inductive = Vec::new();
        let mut asynchronous = Vec::new();
        for (id, rule) in program.get().rules.iter_enumerated() {
            plans.insert(id, plan::RulePlan::new(rule)?);
            match rule.kind {
                blossom_ir::core::RuleKind::Deductive => {}
                blossom_ir::core::RuleKind::Inductive => inductive.push(id),
                blossom_ir::core::RuleKind::Async => asynchronous.push(id),
            }
        }
        let statics = eval::statics(program.get())?;
        Ok(Oracle {
            program,
            strata,
            plans,
            inductive,
            asynchronous,
            statics,
            limits,
        })
    }

    /// The program.
    pub fn program(&self) -> &ValidatedProgram {
        &self.program
    }

    /// The deductive strata, in evaluation order.
    pub fn strata(&self) -> &[Stratum] {
        &self.strata
    }

    /// Runs one tick of one node.
    pub fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, OracleError> {
        eval::tick(self, input)
    }
}
