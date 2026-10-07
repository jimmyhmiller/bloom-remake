#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-oracle`: the naive per-tick Dedalus^L evaluator over `Value` with its own stratifier, and the choice-
//! validity checker. Deliberately independent of the kernel, the engine, the planner and the analyses.
//!
//! See ARCHITECTURE §1.2 and §11.2. Implemented by slice 1 (docs/design/SLICES.md) for the constructs the `.ded`
//! frontend produces and by slice 2 for the Blossom subset: lattice-valued relations over the core built-in lattices
//! (merged per key, SEM-100), lookups, lattice operations, collections and generators. WP M4.1's remaining
//! constructs (weighted relations, choices, the other lattices) fail with `Unimplemented`.
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

mod cells;
mod eval;
mod expr;
mod library;
mod plan;
mod strata;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_base::{RelId, RoleId, RuleId};
use blossom_ir::ValidatedProgram;
use blossom_value::{Value, time::NodeId};

pub use strata::Stratum;

pub use blossom_ir::tick::{Delivery, Egress, HostOut, Ingress, Instance, Row, Send, TickInput, TickOutput};

/// Why the oracle could not evaluate: the evaluators' shared error.
pub type OracleError = blossom_ir::tick::EvalError;

/// Why [`Oracle::eval_op`] gave no value: a program error (a hard runtime error, by its code and message) or an
/// evaluator error.
#[derive(Debug)]
pub enum CallError {
    Program(String),
    Oracle(OracleError),
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
    /// The built-in lattice of each declared lattice, by id.
    kinds: Vec<Option<blossom_lattice::Kind>>,
    /// How the rows of each lattice-valued relation merge.
    cells: BTreeMap<RelId, cells::CellInfo>,
    /// The choice seed σc every node shares (SEM-084), for seeded choices and resolution policies.
    choice: Option<blossom_value::Seed>,
    /// The root seed, and each node's seed σn derived from it and the node's name (for `rand`, LANG-175).
    root: Option<blossom_value::Seed>,
    node_names: Vec<Arc<str>>,
    node_seeds: Vec<blossom_value::Seed>,
    /// Each node's role, for the `$role(R)` guards of rules placed at a role (LANGUAGE §6.10). Empty for a
    /// role-free program.
    roles: Vec<Option<RoleId>>,
    /// The deployment's values of deploy-time parameters (LANG-010); a parameter it does not bind takes its
    /// declared default.
    params: BTreeMap<blossom_base::ParamId, Value>,
    /// The host functions the program's `extern fn`s call, bound when the oracle was built.
    externs: Arc<blossom_value::ExternRegistry>,
}

impl Oracle {
    /// Prepares `program`: checks that every construct is one the oracle evaluates, stratifies the deductive rules
    /// and plans every rule.
    pub fn new(program: ValidatedProgram) -> Result<Oracle, OracleError> {
        Oracle::with_limits(program, Limits::default())
    }

    /// [`Oracle::new`] with explicit limits.
    pub fn with_limits(program: ValidatedProgram, limits: Limits) -> Result<Oracle, OracleError> {
        Oracle::with_externs(program, limits, Arc::new(blossom_value::ExternRegistry::new()))
    }

    /// [`Oracle::with_limits`] with the host functions the program's `extern fn`s call. Every one must be in
    /// `externs` with its declared signature (LANG-181); a program that declares none needs none.
    pub fn with_externs(
        program: ValidatedProgram,
        limits: Limits,
        externs: Arc<blossom_value::ExternRegistry>,
    ) -> Result<Oracle, OracleError> {
        blossom_ir::tick::bind_externs(program.get(), &externs)?;
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
        let kinds = cells::kinds(program.get());
        let cells = cells::cells(program.get(), &kinds)?;
        Ok(Oracle {
            kinds,
            cells,
            choice: None,
            root: None,
            node_names: Vec::new(),
            node_seeds: Vec::new(),
            program,
            strata,
            plans,
            inductive,
            asynchronous,
            statics,
            limits,
            roles: Vec::new(),
            params: BTreeMap::new(),
            externs,
        })
    }

    /// Evaluates the catalogue operation `op` of `lattice` on `args`, outside any tick (as node 0 at tick 0): for the
    /// law harness, which calls a lattice's methods on generated values. A method is a pure function, so it reads
    /// nothing a tick would give it.
    pub fn eval_op(
        &self,
        lattice: blossom_base::LatticeTypeId,
        op: blossom_base::Symbol,
        args: &[Value],
    ) -> Result<Value, CallError> {
        use blossom_ir::core::{Expr, LatOpRef, Term};
        // The arguments are the values of variables 0, 1, … of an environment of their own.
        let mut vars = Vec::with_capacity(args.len());
        for i in 0..args.len() {
            let v = u32::try_from(i)
                .map_err(|_| CallError::Oracle(blossom_base::internal_error!("too many arguments").into()))?;
            vars.push(Expr::Term(Term::Var(blossom_base::VarId::from_raw(v))));
        }
        let e = Expr::Lattice {
            op: LatOpRef { lattice, op },
            args: vars,
        };
        let env: Vec<Option<Value>> = args.iter().cloned().map(Some).collect();
        let scope = expr::Scope {
            incarnation: 1,
            program: self.program.get(),
            node: blossom_value::time::NodeId(0),
            tick: blossom_value::time::Tick(0),
            now: blossom_value::time::Instant(0),
            oracle: self,
            fuel: expr::Fuel::default(),
            blobs: &blossom_value::NoBlobs,
            new_blobs: std::cell::RefCell::new(BTreeMap::new()),
        };
        expr::eval(&scope, &env, &e).map_err(|e| match e {
            expr::ExprError::Arithmetic(m) => CallError::Program(format!("BLSR004: {m}")),
            expr::ExprError::Conflict(m) => CallError::Program(format!("BLSR006: {m}")),
            expr::ExprError::Refused(m) => CallError::Program(format!("BLSR010: {m}")),
            expr::ExprError::Budget(m) => CallError::Program(format!("BLSR012: {m}")),
            expr::ExprError::Oracle(e) => CallError::Oracle(e),
        })
    }

    /// The host function registered at `path` (bound when the oracle was built).
    pub(crate) fn host_fn(&self, path: &str) -> Option<&Arc<dyn blossom_value::ExternFn>> {
        self.externs.lookup_fn(path)
    }

    /// Places the deployment's nodes: `roles[n]` is node `n`'s role. A rule placed at a role runs only on that
    /// role's nodes.
    pub fn with_roles(mut self, roles: Vec<Option<RoleId>>) -> Oracle {
        self.roles = roles;
        self
    }

    /// Binds deploy-time parameters (LANG-010). A parameter left unbound takes its declared default; one with no
    /// default is an error when it is read.
    pub fn with_params(mut self, params: BTreeMap<blossom_base::ParamId, Value>) -> Oracle {
        self.params = params;
        self
    }

    /// The value of parameter `p`: the deployment's binding, else the declared default.
    pub(crate) fn param(&self, p: blossom_base::ParamId) -> Result<Value, OracleError> {
        if let Some(v) = self.params.get(&p) {
            return Ok(v.clone());
        }
        let program = self.program.get();
        let decl = program
            .params
            .get(p)
            .ok_or_else(|| blossom_base::internal_error!("parameter {p:?} is not declared"))?;
        let Some(c) = decl.default else {
            return Err(OracleError::Unbound(decl.name.to_string()));
        };
        program.consts.get(c).cloned().ok_or_else(|| {
            blossom_base::internal_error!("the default of parameter {} is not a constant", decl.name).into()
        })
    }

    /// Seeds the run: σc = PRF(ρ, "choose") from the root seed ρ (the run seed in simulation), which seeded choices
    /// and resolution policies need.
    pub fn with_seed(mut self, root: blossom_value::Seed) -> Result<Oracle, OracleError> {
        let seeds = blossom_value::Seeds::derive(root, "")
            .map_err(|e| blossom_base::internal_error!("deriving the choice seed: {e}"))?;
        self.choice = Some(seeds.choice);
        self.root = Some(root);
        self.derive_node_seeds()?;
        Ok(self)
    }

    /// Names the deployment's nodes (`names[n]` is node `n`): each node's seed σn is derived from its stable name,
    /// so it does not depend on the numbering (ARCHITECTURE §4.7).
    pub fn with_node_names(mut self, names: Vec<Arc<str>>) -> Result<Oracle, OracleError> {
        self.node_names = names;
        self.derive_node_seeds()?;
        Ok(self)
    }

    fn derive_node_seeds(&mut self) -> Result<(), OracleError> {
        let Some(root) = self.root else {
            return Ok(());
        };
        let mut seeds = Vec::with_capacity(self.node_names.len());
        for n in &self.node_names {
            seeds.push(
                blossom_value::Seeds::derive(root, n)
                    .map_err(|e| blossom_base::internal_error!("deriving the seed of node {n}: {e}"))?
                    .node,
            );
        }
        self.node_seeds = seeds;
        Ok(())
    }

    /// Node `n`'s seed σn.
    pub(crate) fn node_seed(&self, n: NodeId) -> Result<blossom_value::Seed, OracleError> {
        self.node_seeds.get(n.0 as usize).copied().ok_or_else(|| {
            blossom_base::internal_error!(
                "a `rand` draw on node {}, but the oracle was given no seed or no node names",
                n.0
            )
            .into()
        })
    }

    /// The number of nodes of role `r` among `nodes`.
    pub(crate) fn role_members(&self, r: RoleId, nodes: &BTreeSet<Value>) -> u64 {
        nodes
            .iter()
            .filter(|v| matches!(v, Value::Node(n) if self.roles.get(n.0 as usize).copied().flatten() == Some(r)))
            .count() as u64
    }

    /// Whether `rule` runs on `node`: rules without a role guard run everywhere.
    pub(crate) fn runs_on(&self, rule: &blossom_ir::core::Rule, node: NodeId) -> bool {
        match rule.role {
            None => true,
            Some(r) => self.roles.get(node.0 as usize).copied().flatten() == Some(r),
        }
    }

    /// The number of nodes in role `r`.
    pub(crate) fn role_size(&self, r: RoleId) -> u64 {
        self.roles.iter().filter(|x| **x == Some(r)).count() as u64
    }

    /// Each node's name, by node id (none unless [`Oracle::with_node_names`] gave them).
    pub fn node_names(&self) -> &[Arc<str>] {
        &self.node_names
    }

    /// The program.
    pub fn program(&self) -> &ValidatedProgram {
        &self.program
    }

    /// The rows of the program's `fact`s: the static rows every tick starts with.
    pub fn static_facts(&self) -> &Instance {
        &self.statics
    }

    /// The deductive strata, in evaluation order.
    pub fn strata(&self) -> &[Stratum] {
        &self.strata
    }

    /// Runs one tick of one node.
    pub fn tick(&self, input: &TickInput<'_>) -> Result<TickOutput, OracleError> {
        eval::tick(self, input)
    }

    /// Why the rules deriving `rel` do or do not derive a tuple matching `pattern` (`None`: any value) on the tick
    /// `input` describes, whose final instance is `instance` (its [`TickOutput::instance`]): one [`WhyNot`] per
    /// rule of the node with that head, with up to `samples` partial valuations each.
    pub fn why_not(
        &self,
        input: &TickInput<'_>,
        instance: &Instance,
        rel: RelId,
        pattern: &[Option<Value>],
        samples: usize,
    ) -> Result<Vec<WhyNot>, OracleError> {
        eval::why_not(self, input, instance, rel, pattern, samples)
    }
}

/// How far one rule got towards deriving a tuple (see [`Oracle::why_not`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhyNot {
    pub rule: RuleId,
    /// A head column whose constant (or repeated variable) differs from the pattern: the rule cannot derive it.
    pub head_differs: Option<usize>,
    /// How many steps of the rule's plan some valuation passed, of `steps`.
    pub passed: usize,
    pub steps: usize,
    /// The body literal (by index) of the first step no valuation passed.
    pub failed: Option<usize>,
    /// Some valuations that passed `passed` steps: the variables bound so far.
    pub partial: Vec<Vec<(blossom_base::VarId, Value)>>,
    /// How many valuations passed the whole body (the rule derives the tuple).
    pub complete: usize,
    /// An evaluation error on the way (an expression that fails on these values).
    pub error: Option<String>,
}
