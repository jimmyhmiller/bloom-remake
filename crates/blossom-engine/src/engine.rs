//! The engine: one node's program, evaluated tick by tick from state that lives across ticks.
//!
//! A tick:
//! 1. clears every store's tick change, then applies what changes: the carried state (what the last tick's inductive
//!    rules derived, relative to the tick before), the program's facts on the first tick, and the tick's inputs
//!    (events, deliveries, client messages) relative to the last tick's;
//! 2. brings each stratum up to date, in dependency order: a delta rule runs one term per dependency that changed; a
//!    recompute rule runs in full and is diffed against its last output; a recursive stratum whose inputs changed is
//!    re-evaluated to its fixpoint and diffed (errors judged at the fixpoint, as the reference semantics does);
//! 3. brings the inductive rules (the next tick's state) and the asynchronous rules (the tick's sends) up to date the
//!    same way;
//! 4. checks keys (SEM-050/051) on the rows that changed, and invariants;
//! 5. reports the change to the carried state, the sends, the replies, and what the caller observes.
//!
//! A tick that fails leaves the engine unusable until `reset`: in the node's semantics a failed tick is a crash.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_base::{ParamId, RelId, RoleId, RuleId, internal_error};
use blossom_ir::ValidatedProgram;
use blossom_ir::core::{
    AggFunc, ConstructKind, HeadArg, HeadMode, LatticeCtor, Literal, Program, RelClass, Rule, RuleKind,
    ViolationAction,
};
use blossom_ir::obs::ProgramErrorRecord;
use blossom_ir::tick::{
    Changes, Egress, EvalError, FnWork, Instance, Row, RuleWork, Send, StepInput, StepOutput, TickInput, TickOutput,
};
use blossom_lattice::Kind;
use blossom_value::time::{NodeId, Tick};
use blossom_value::value::IntValue;
use blossom_value::{Seed, TypeDef, Value};

use crate::expr::{self, Ctx, ExprError, Shared, bug};
use crate::rule::{self, Driver, Found, Plan, Plans, Regime, StoreKey, Stores, Token, dep_store, non_wild};
use crate::store::{CellSpec, Store};
use crate::strata::{self, Stratum};

/// Stores this small share one size for the join-order cache (`run_drivers`).
const SMALL_STORE: usize = 32;

/// How to set up an engine for one node of a deployment.
#[derive(Clone, Debug, Default)]
pub struct EngineConfig {
    /// Each node's role, by node id (empty for a role-free program).
    pub roles: Vec<Option<RoleId>>,
    /// Each node's stable name, by node id (node seeds derive from names).
    pub node_names: Vec<Arc<str>>,
    /// The deployment's root seed ρ (choices and `rand` derive from it).
    pub seed: Option<Seed>,
    /// The deployment's values of deploy-time parameters.
    pub params: BTreeMap<ParamId, Value>,
    /// Rounds a recursive stratum may take before the tick fails with BLSR007.
    pub max_rounds: u32,
    /// The host functions the program's `extern fn`s call (LANG-181); each is bound when the engine is built.
    pub externs: Arc<blossom_value::ExternRegistry>,
}

/// The per-group state of an aggregate rule: per aggregate column, the support of each distinct argument tuple, and
/// the group's current head row.
#[derive(Clone, Debug, Default)]
struct Group {
    tuples: Vec<BTreeMap<Vec<Value>, i64>>,
    row: Option<Row>,
}

pub struct Engine {
    program: ValidatedProgram,
    shared: Shared,
    node: NodeId,
    plans: Plans,
    strata: Arc<[Stratum]>,
    inductive: Arc<[RuleId]>,
    asynchronous: Arc<[RuleId]>,
    stores: Stores,
    /// A recompute rule's (or a recursive stratum rule's) head rows at its last evaluation, with their support.
    prev: BTreeMap<RuleId, BTreeMap<Row, i64>>,
    /// Per re-evaluated rule that may be left alone (`Plan::skip`), by index: evaluated since the last reset, and
    /// when its output may change with nothing it reads changing (`None`: not before they change).
    recomputed: Vec<Option<Option<blossom_value::time::Instant>>>,
    groups: BTreeMap<RuleId, BTreeMap<Vec<Value>, Group>>,
    /// The tick-local input rows of the last tick, per store.
    inputs: BTreeMap<StoreKey, BTreeSet<Row>>,
    /// Changes to the carried state, applied at the next tick's start.
    pending: Changes,
    /// The carried state `reset` loaded, until the first tick replaces it.
    baseline: Option<Instance>,
    facts_loaded: bool,
    max_rounds: u32,
    /// Relations with payload columns: their key columns, and whether they stage an upsert (BLSR002).
    keyed: Vec<(RelId, Vec<usize>, bool)>,
    violations: Vec<RuleId>,
    poisoned: bool,
    /// Lattice stores written since the last settle.
    unsettled: BTreeSet<StoreKey>,
    /// Rows the atom probes returned since the engine was created: the join work, measured without a clock.
    examined: u64,
    /// The work of each rule (rows examined, expression nodes evaluated).
    examined_by: BTreeMap<RuleId, RuleWork>,
    /// The blobs the current tick created (`Blob::of`), with their bytes.
    new_blobs: std::cell::RefCell<BTreeMap<blossom_value::BlobRef, Arc<[u8]>>>,
    /// Each (rule, driver literal)'s join order, with the sizes of the stores its atoms read when it was chosen, in
    /// powers of two: reused while every one stays in its power of two. A join order decides a term's cost, never
    /// its valuations, so reusing one changes only the work.
    orders: std::cell::RefCell<BTreeMap<(RuleId, Option<usize>), CachedOrder>>,
    /// Each function's work since profiling was switched on (`None`: off).
    fn_work: Option<std::cell::RefCell<BTreeMap<blossom_base::FnId, FnWork>>>,
}

fn kinds(p: &Program) -> Vec<Option<Kind>> {
    fn kind(p: &Program, ctor: &LatticeCtor, depth: usize) -> Option<Kind> {
        if depth > p.lattices.len() {
            return None;
        }
        Some(match ctor {
            LatticeCtor::Bool => Kind::Bool,
            LatticeCtor::Max(_) => Kind::Max,
            LatticeCtor::Min(_) => Kind::Min,
            LatticeCtor::Set(_) => Kind::Set,
            LatticeCtor::PSet(_) => Kind::PSet,
            LatticeCtor::Point(_) => Kind::Point,
            LatticeCtor::Map(_, inner) => Kind::Map(Box::new(kind(p, &p.lattices.get(*inner)?.ctor, depth + 1)?)),
            _ => return None,
        })
    }
    p.lattices.iter().map(|l| kind(p, &l.ctor, 0)).collect()
}

/// The constructs the engine evaluates; anything else fails loudly.
fn check_supported(p: &Program) -> Result<(), EvalError> {
    for r in p.rels.iter() {
        match r.class {
            RelClass::Weighted(_) => {
                return Err(blossom_base::unimplemented_error!("LANG-138", "weighted relations in the engine").into());
            }
            RelClass::HostTable => {
                return Err(blossom_base::unimplemented_error!("LANG-051", "host-maintained tables in the engine").into());
            }
            _ => {}
        }
    }
    for rule in p.rules.iter() {
        let lattice_head = p.rels.get(rule.head.rel).is_some_and(|r| !r.schema.lattice.is_empty());
        if lattice_head && strata::is_aggregate(rule) {
            return Err(blossom_base::unimplemented_error!(
                "LANG-100",
                "aggregates into a lattice-valued relation in the engine"
            )
            .into());
        }
        if matches!(rule.head.mode, HeadMode::ZAdd { .. }) {
            return Err(blossom_base::unimplemented_error!("LANG-138", "weighted heads in the engine").into());
        }
        for a in &rule.head.args {
            if let HeadArg::Agg(agg) = a {
                let ok = matches!(
                    agg.func,
                    AggFunc::Count | AggFunc::Sum | AggFunc::Min | AggFunc::Max | AggFunc::CollectVec
                )
                    && agg.order.is_none()
                    && (!agg.args.is_empty() || matches!(agg.func, AggFunc::Count));
                if !ok {
                    return Err(blossom_base::unimplemented_error!(
                        "LANG-100",
                        "the aggregate {:?} in the engine",
                        agg.func
                    )
                    .into());
                }
            }
        }
        for lit in &rule.body.lits {
            if let Literal::Pos(a) | Literal::Neg(a) = lit {
                if a.principal.is_some() || a.weight.is_some() {
                    return Err(blossom_base::unimplemented_error!(
                        "LANG-241",
                        "`principal` and weight bindings in the engine"
                    )
                    .into());
                }
                if matches!(lit, Literal::Neg(_)) && a.sender.is_some() {
                    return Err(blossom_base::unimplemented_error!(
                        "LANG-241",
                        "`from` on a negated channel atom in the engine"
                    )
                    .into());
                }
            }
        }
    }
    Ok(())
}

/// Whether a value of type `ty` can hold a blob (a lattice is assumed to, as its elements are not traced here).
fn holds_blobs(p: &Program, ty: blossom_base::TypeId, depth: u32) -> bool {
    use blossom_value::TypeDef as D;
    // Types are acyclic; the depth guards a malformed table.
    if depth > 64 {
        return true;
    }
    match p.types.get(ty) {
        Some(D::Blob | D::Lattice(_)) | None => true,
        Some(D::Tuple(xs)) => xs.iter().any(|x| holds_blobs(p, *x, depth + 1)),
        Some(D::Struct(s)) => s.fields.iter().any(|f| holds_blobs(p, f.ty, depth + 1)),
        Some(D::Enum(e)) => e
            .variants
            .iter()
            .any(|v| v.payload.iter().any(|f| holds_blobs(p, f.ty, depth + 1))),
        Some(D::Vec(x) | D::Set(x) | D::Option(x)) => holds_blobs(p, *x, depth + 1),
        Some(D::Map(k, v)) => holds_blobs(p, *k, depth + 1) || holds_blobs(p, *v, depth + 1),
        Some(_) => false,
    }
}

/// Whether rows of `rel` can hold blobs.
fn rel_holds_blobs(p: &Program, rel: RelId) -> bool {
    p.rels
        .get(rel)
        .is_none_or(|r| r.schema.cols.iter().any(|c| holds_blobs(p, c.ty, 0)))
}

fn cell_spec(p: &Program, kinds: &[Option<Kind>], rel: RelId, extra: usize) -> Result<Option<CellSpec>, EvalError> {
    let decl = p.rels.get(rel).ok_or_else(|| internal_error!("relation {rel:?} is not declared"))?;
    if decl.schema.lattice.is_empty() {
        return Ok(None);
    }
    let mut lattice = Vec::new();
    for (col, l) in &decl.schema.lattice {
        match kinds.get(l.index()).cloned().flatten() {
            Some(k) => lattice.push((col.index(), k)),
            None => {
                return Err(blossom_base::unimplemented_error!(
                    "LANG-124",
                    "the lattice of `{}` in the engine",
                    decl.name
                )
                .into());
            }
        }
    }
    let mut ident: Vec<usize> = decl
        .schema
        .key
        .iter()
        .chain(decl.schema.payload.iter())
        .map(|c| c.index())
        .collect();
    ident.sort_unstable();
    Ok(Some(CellSpec { ident, extra, lattice }))
}

impl Engine {
    /// Prepares `program` for node `node`: checks what it evaluates, stratifies, plans every rule the node runs, and
    /// creates and indexes every store.
    pub fn new(program: ValidatedProgram, node: NodeId, cfg: EngineConfig) -> Result<Engine, EvalError> {
        let p = program.get();
        blossom_ir::tick::bind_externs(p, &cfg.externs)?;
        check_supported(p)?;
        let kinds = kinds(p);
        let (choice, node_seeds) = match cfg.seed {
            Some(root) => {
                let choice = blossom_value::Seeds::derive(root, "")
                    .map_err(|e| internal_error!("deriving the choice seed: {e}"))?
                    .choice;
                let mut seeds = Vec::new();
                for n in &cfg.node_names {
                    seeds.push(
                        blossom_value::Seeds::derive(root, n)
                            .map_err(|e| internal_error!("deriving the seed of node {n}: {e}"))?
                            .node,
                    );
                }
                (Some(choice), seeds)
            }
            None => (None, Vec::new()),
        };
        let my_role = cfg.roles.get(node.0 as usize).copied().flatten();
        let runs = |rule: &Rule| rule.role.is_none_or(|r| Some(r) == my_role);
        let mut plans = Plans::default();
        let mut inductive = Vec::new();
        let mut asynchronous = Vec::new();
        for (id, rule) in p.rules.iter_enumerated() {
            if !runs(rule) {
                continue;
            }
            let mut plan = Plan::new(rule)?;
            plan.frame = p
                .rels
                .get(rule.head.rel)
                .and_then(|r| rule::FramePlan::of(rule, &r.persistence));
            plans.insert(id, plan);
            match rule.kind {
                RuleKind::Deductive => {}
                RuleKind::Inductive => inductive.push(id),
                RuleKind::Async => asynchronous.push(id),
            }
        }
        let mut strata_list = strata::stratify(p)?;
        for s in &mut strata_list {
            s.aggregates.retain(|r| plans.contains_key(r));
            s.rules.retain(|r| plans.contains_key(r));
        }
        strata_list.retain(|s| !s.aggregates.is_empty() || !s.rules.is_empty());
        let mut stores = Stores::default();
        for (id, r) in p.rels.iter_enumerated() {
            let blobs = rel_holds_blobs(p, id);
            stores.insert(StoreKey::Main(id), Store::new(cell_spec(p, &kinds, id, 0)?, blobs));
            if matches!(r.class, RelClass::Channel(_)) {
                stores.insert(StoreKey::Sent(id), Store::new(cell_spec(p, &kinds, id, 1)?, blobs));
            }
        }
        for id in inductive.iter().chain(&asynchronous) {
            if let Some(plan) = plans.get(id) {
                let rel = p.rules.get(*id).map(|r| r.head.rel).ok_or_else(|| internal_error!("rule {id:?}"))?;
                let spec = cell_spec(p, &kinds, rel, 0)?;
                stores.insert_absent(plan.head, || Store::new(spec, rel_holds_blobs(p, rel)));
            }
        }
        let mut keyed = Vec::new();
        for (id, r) in p.rels.iter_enumerated() {
            if r.schema.payload.is_empty() {
                continue;
            }
            let cols: Vec<usize> = r.schema.key.iter().map(|c| c.index()).collect();
            let upsert = p
                .constructs
                .iter()
                .any(|c| matches!(c.kind, ConstructKind::Upsert { staging, .. } if staging == id));
            keyed.push((id, cols, upsert));
        }
        let violations = p
            .rules
            .iter_enumerated()
            .filter(|(id, r)| plans.contains_key(id) && matches!(r.head.mode, HeadMode::Violation { .. }))
            .map(|(id, _)| id)
            .collect();
        let mut engine = Engine {
            shared: Shared {
                params: cfg.params,
                choice,
                node_seeds,
                roles: cfg.roles,
                kinds,
                externs: cfg.externs,
            },
            node,
            plans,
            strata: strata_list.into(),
            inductive: inductive.into(),
            asynchronous: asynchronous.into(),
            stores,
            prev: BTreeMap::new(),
            recomputed: Vec::new(),
            groups: BTreeMap::new(),
            inputs: BTreeMap::new(),
            pending: Changes::default(),
            baseline: Some(Instance::default()),
            facts_loaded: false,
            max_rounds: if cfg.max_rounds == 0 { 10_000 } else { cfg.max_rounds },
            keyed,
            violations,
            poisoned: false,
            unsettled: BTreeSet::new(),
            examined: 0,
            examined_by: BTreeMap::new(),
            new_blobs: std::cell::RefCell::new(BTreeMap::new()),
            orders: std::cell::RefCell::new(BTreeMap::new()),
            fn_work: None,
            program,
        };
        engine.build_indexes()?;
        Ok(engine)
    }

    /// Creates every index a term, a check or a key check probes.
    fn build_indexes(&mut self) -> Result<(), EvalError> {
        let p = self.program.get();
        let mut wanted: BTreeSet<(StoreKey, Vec<usize>)> = BTreeSet::new();
        for plan in self.plans.values() {
            let rule = p.rules.get(plan.rule).ok_or_else(|| internal_error!("rule {:?}", plan.rule))?;
            for lit in &rule.body.lits {
                match lit {
                    Literal::Neg(a) => {
                        wanted.insert((StoreKey::Main(a.rel), non_wild(a)));
                    }
                    Literal::Lookup { rel, .. } => {
                        let cols: Vec<usize> = p
                            .rels
                            .get(*rel)
                            .map(|d| d.schema.key.iter().map(|c| c.index()).collect())
                            .unwrap_or_default();
                        wanted.insert((StoreKey::Main(*rel), cols));
                    }
                    _ => {}
                }
            }
        }
        for (rel, cols, _) in &self.keyed {
            wanted.insert((StoreKey::Main(*rel), cols.clone()));
        }
        for (key, cols) in wanted {
            if let Some(s) = self.stores.get_mut(&key) {
                s.ensure_index(&cols);
            }
        }
        Ok(())
    }

    /// Starts over from `carried` (at boot: the recovered durable state): every store is emptied, and the next tick
    /// sees the carried state, the facts and its inputs as new.
    pub fn reset(&mut self, carried: Instance) -> Result<(), EvalError> {
        for s in self.stores.values_mut() {
            *s = Store::new(s.cell.clone(), s.counts_blobs());
        }
        self.prev.clear();
        self.recomputed.clear();
        self.groups.clear();
        self.inputs.clear();
        self.facts_loaded = false;
        self.poisoned = false;
        let mut pending = Changes::default();
        for (rel, rows) in &carried.rels {
            pending.inserted.insert(*rel, rows.iter().cloned().collect());
        }
        self.pending = pending;
        self.baseline = Some(carried);
        self.build_indexes()
    }

    /// A store to write. A lattice store written is settled at the next [`Engine::settle`].
    fn store(&mut self, key: StoreKey) -> Result<&mut Store, EvalError> {
        let store = self
            .stores
            .get_mut(&key)
            .ok_or_else(|| EvalError::from(internal_error!("no store for {key:?}")))?;
        if store.cell.is_some() {
            self.unsettled.insert(key);
        }
        Ok(store)
    }

    /// Joins the changed cells of every lattice store written since the last settle. Called where the reference's
    /// state is defined: after the tick's inputs, after each stratum (and each step of a fixpoint), and after the
    /// next state and the sends; a conflict raised here is one the reference raises too.
    fn settle(&mut self, tick: Tick) -> Result<(), EvalError> {
        for key in std::mem::take(&mut self.unsettled) {
            if let Some(s) = self.stores.get_mut(&key) {
                s.settle().map_err(|e| to_eval(e, tick, None))?;
            }
        }
        Ok(())
    }

    /// Runs one tick.
    pub fn step(&mut self, input: &StepInput<'_>, observe: &[RelId]) -> Result<StepOutput, EvalError> {
        if self.poisoned {
            return Err(internal_error!("the engine is used after a failed tick without a reset").into());
        }
        if input.node != self.node {
            return Err(internal_error!("an engine for node {} ran a tick of node {}", self.node.0, input.node.0).into());
        }
        match self.step_inner(input, observe) {
            Ok(out) => Ok(out),
            Err(e) => {
                self.poisoned = true;
                Err(e)
            }
        }
    }

    fn step_inner(&mut self, input: &StepInput<'_>, observe: &[RelId]) -> Result<StepOutput, EvalError> {
        let tick = input.tick;
        let wrap = |e: ExprError| to_eval(e, tick, None);
        // 1. What changes.
        self.stores.clear_deltas();
        let pending = std::mem::take(&mut self.pending);
        for (rel, rows) in &pending.deleted {
            for r in rows {
                self.store(StoreKey::Main(*rel))?.add(r.clone(), -1).map_err(wrap)?;
            }
        }
        for (rel, rows) in &pending.inserted {
            for r in rows {
                self.store(StoreKey::Main(*rel))?.add(r.clone(), 1).map_err(wrap)?;
            }
        }
        if !self.facts_loaded {
            let p = self.program.get();
            let mut facts = Vec::new();
            for f in &p.facts {
                let row = f
                    .row
                    .iter()
                    .map(|c| p.consts.get(*c).cloned().ok_or_else(|| internal_error!("unknown constant {c:?}")))
                    .collect::<Result<Vec<_>, _>>()?;
                facts.push((f.rel, Row::from(row)));
            }
            for (rel, row) in facts {
                self.store(StoreKey::Main(rel))?.add(row, 1).map_err(wrap)?;
            }
            self.facts_loaded = true;
        }
        let mut now_inputs: BTreeMap<StoreKey, BTreeSet<Row>> = BTreeMap::new();
        for (rel, row) in input.events {
            now_inputs.entry(StoreKey::Main(*rel)).or_default().insert(row.clone());
        }
        for d in input.delivered {
            now_inputs.entry(StoreKey::Main(d.rel)).or_default().insert(d.row.clone());
            let mut with_sender = d.row.to_vec();
            with_sender.push(Value::Node(d.from));
            now_inputs.entry(StoreKey::Sent(d.rel)).or_default().insert(Row::from(with_sender));
        }
        for g in input.ingress {
            now_inputs.entry(StoreKey::Main(g.rel)).or_default().insert(g.row.clone());
            let mut with_sender = g.row.to_vec();
            with_sender.push(Value::Session(g.session));
            now_inputs.entry(StoreKey::Sent(g.rel)).or_default().insert(Row::from(with_sender));
        }
        let before = std::mem::take(&mut self.inputs);
        let keys: BTreeSet<StoreKey> = before.keys().chain(now_inputs.keys()).copied().collect();
        let empty = BTreeSet::new();
        for key in keys {
            let old = before.get(&key).unwrap_or(&empty);
            let new = now_inputs.get(&key).unwrap_or(&empty);
            for r in old.difference(new) {
                self.store(key)?.add(r.clone(), -1).map_err(wrap)?;
            }
            for r in new.difference(old) {
                self.store(key)?.add(r.clone(), 1).map_err(wrap)?;
            }
        }
        self.inputs = now_inputs;
        self.settle(tick)?;
        let program = self.program.clone();
        let p = program.get();
        // 2. The strata.
        let strata_list = self.strata.clone();
        for s in strata_list.iter() {
            if s.recursive {
                self.recursive_stratum(p, input, s)?;
            } else {
                for id in &s.aggregates {
                    self.maintain(p, input, *id)?;
                }
                self.settle(tick)?;
                for id in &s.rules {
                    self.maintain(p, input, *id)?;
                }
            }
            self.settle(tick)?;
        }
        // 3. The next tick's state and the tick's sends.
        let (inductive, asynchronous) = (self.inductive.clone(), self.asynchronous.clone());
        for id in inductive.iter().chain(asynchronous.iter()) {
            self.maintain(p, input, *id)?;
        }
        self.settle(tick)?;
        // 4. Keys and invariants.
        self.check_keys(p, tick)?;
        self.check_invariants(p, tick)?;
        // 5. The outputs.
        let mut changes = Changes::default();
        for id in self.inductive.iter() {
            let Some(plan) = self.plans.get(id) else { continue };
            let StoreKey::Next(rel) = plan.head else { continue };
            if changes.inserted.contains_key(&rel) || changes.deleted.contains_key(&rel) {
                continue;
            }
            let s = self.stores.get(&plan.head).ok_or_else(|| internal_error!("no next store"))?;
            if !s.ins.is_empty() {
                changes.inserted.insert(rel, s.ins.iter().cloned().collect());
            }
            if !s.del.is_empty() {
                changes.deleted.insert(rel, s.del.iter().cloned().collect());
            }
        }
        if let Some(base) = self.baseline.take() {
            // The first tick after a reset: the change is relative to the carried state it started from.
            let next = self.next_instance()?;
            changes = Changes::between(&base, &next);
        }
        self.pending = changes.clone();
        let mut out = StepOutput {
            changes,
            blobs: self.new_blobs.take(),
            ..StepOutput::default()
        };
        for id in self.asynchronous.iter() {
            let Some(plan) = self.plans.get(id) else { continue };
            let StoreKey::Async(rel) = plan.head else { continue };
            let s = self.stores.get(&plan.head).ok_or_else(|| internal_error!("no async store"))?;
            let to_host = matches!(self.program.get().rels.get(rel).map(|r| &r.class), Some(RelClass::HostOut(_)));
            for row in s.present() {
                if to_host {
                    out.host.insert(blossom_ir::tick::HostOut { rel, row: row.clone() });
                    continue;
                }
                match row.first() {
                    Some(Value::Node(to)) => {
                        out.outbox.insert(Send {
                            rel,
                            to: *to,
                            row: row.clone(),
                        });
                    }
                    Some(Value::Session(session)) => {
                        out.egress.insert(Egress {
                            rel,
                            session: *session,
                            row: row.clone(),
                        });
                    }
                    other => return Err(internal_error!("an async head's destination is {other:?}").into()),
                }
            }
        }
        for rel in observe {
            let rows = self
                .stores
                .get(&StoreKey::Main(*rel))
                .map(|s| s.present().cloned().collect())
                .unwrap_or_default();
            out.observed.insert(*rel, rows);
        }
        Ok(out)
    }

    fn next_instance(&self) -> Result<Instance, EvalError> {
        let mut next = Instance::default();
        for (key, s) in self.stores.iter() {
            if let StoreKey::Next(rel) = key {
                for r in s.present() {
                    next.insert(rel, r.clone());
                }
            }
        }
        Ok(next)
    }

    /// The whole carried state: what the next tick starts from (O(state)).
    pub fn carried_instance(&self) -> Instance {
        match &self.baseline {
            Some(base) => base.clone(),
            None => self.next_instance().unwrap_or_default(),
        }
    }

    /// The carried rows of `rel`: what the next tick starts from.
    pub fn carried_rows(&self, rel: RelId) -> Vec<Row> {
        if let Some(base) = &self.baseline {
            return base.rows(rel).cloned().collect();
        }
        self.stores
            .get(&StoreKey::Next(rel))
            .map(|s| s.present().cloned().collect())
            .unwrap_or_default()
    }

    fn ctx<'a>(&'a self, p: &'a Program, input: &StepInput<'a>) -> Ctx<'a> {
        Ctx {
            program: p,
            node: self.node,
            incarnation: input.incarnation,
            tick: input.tick,
            now: input.now,
            shared: &self.shared,
            fuel: crate::expr::Fuel::default(),
            steps: std::cell::Cell::new(0),
            fn_work: self.fn_work.as_ref(),
            callee_steps: std::cell::Cell::new(0),
            blobs: input.blobs,
            new_blobs: &self.new_blobs,
            flips_at: std::cell::Cell::new(None),
            reads_time: std::cell::Cell::new(false),
        }
    }

    /// Brings one rule's output up to date.
    fn maintain(&mut self, p: &Program, input: &StepInput<'_>, id: RuleId) -> Result<(), EvalError> {
        let rule = p.rules.get(id).ok_or_else(|| internal_error!("rule {id:?}"))?;
        {
            // Most rules see no change in a tick: tell so without taking a handle on the plan.
            let plan = self.plans.get(&id).ok_or_else(|| internal_error!("rule {id:?} has no plan"))?;
            let unchanged = !plan.dep_keys.iter().any(|k| self.stores.changed(k));
            if plan.regime == Regime::Delta && unchanged {
                return Ok(());
            }
            if plan.regime == Regime::Recompute
                && unchanged
                && let Some(Some(flips)) = self.recomputed.get(id.index())
                && plan.skip == rule::Skip::WhenUnchanged
                && flips.is_none_or(|t| input.now < t)
            {
                return Ok(());
            }
        }
        let plan = self.plans.get(&id).ok_or_else(|| internal_error!("rule {id:?} has no plan"))?.clone();
        match plan.regime {
            Regime::Recompute => self.recompute_rule(p, input, rule, &plan),
            Regime::Delta => {
                let terms = self.evaluate(p, input, rule, &plan, false)?;
                self.count(rule.id, terms.examined, terms.steps);
                self.apply(p, input, rule, &plan, terms)
            }
        }
    }

    /// Evaluates a rule: every term of its delta query (`full` false), or a full evaluation.
    fn evaluate(
        &self,
        p: &Program,
        input: &StepInput<'_>,
        rule: &Rule,
        plan: &Plan,
        full: bool,
    ) -> Result<Terms, EvalError> {
        if let (Some(frame), false) = (&plan.frame, full) {
            return self.frame_change(frame);
        }
        let cx = self.ctx(p, input);
        let mut drivers: Vec<(Driver, usize)> = Vec::new();
        if full {
            drivers.push((Driver::Full, usize::MAX));
        } else {
            for (pos, &lit) in plan.deps.iter().enumerate() {
                let Some(key) = rule.body.lits.get(lit).and_then(dep_store) else { continue };
                let store = self.stores.get(&key).ok_or_else(|| internal_error!("no store for {key:?}"))?;
                if !store.changed() {
                    continue;
                }
                match rule.body.lits.get(lit) {
                    Some(Literal::Pos(_)) => {
                        for (row, sign) in store.delta() {
                            drivers.push((
                                Driver::Atom {
                                    lit,
                                    row: row.clone(),
                                    sign,
                                },
                                pos,
                            ));
                        }
                    }
                    Some(Literal::Neg(a)) => {
                        let cols = non_wild(a);
                        let keys: BTreeSet<Vec<Value>> = store
                            .delta()
                            .map(|(r, _)| cols.iter().filter_map(|c| r.get(*c).cloned()).collect())
                            .collect();
                        for k in keys {
                            let absent_old = !store.any(true, &cols, &k)?;
                            let absent_new = !store.any(false, &cols, &k)?;
                            if absent_old == absent_new {
                                continue;
                            }
                            drivers.push((
                                Driver::Neg {
                                    lit,
                                    key: k,
                                    sign: if absent_new { 1 } else { -1 },
                                },
                                pos,
                            ));
                        }
                    }
                    Some(Literal::Lookup { rel, .. }) => {
                        let (cols, col, bottom) = rule::lookup_shape(&cx, *rel)?;
                        let keys: BTreeSet<Vec<Value>> = store
                            .delta()
                            .map(|(r, _)| cols.iter().filter_map(|c| r.get(*c).cloned()).collect())
                            .collect();
                        for k in keys {
                            let value = |rows: Vec<Row>| -> Result<Value, EvalError> {
                                match rows.first() {
                                    Some(r) => r
                                        .get(col)
                                        .cloned()
                                        .ok_or_else(|| internal_error!("a cell row without its value").into()),
                                    None => Ok(bottom.clone()),
                                }
                            };
                            let old_v = value(store.old_rows(&cols, &k)?)?;
                            let new_v = value(store.rows(false, &cols, &k)?)?;
                            if old_v == new_v {
                                continue;
                            }
                            drivers.push((
                                Driver::Lookup {
                                    lit,
                                    key: k.clone(),
                                    value: new_v,
                                    sign: 1,
                                },
                                pos,
                            ));
                            drivers.push((
                                Driver::Lookup {
                                    lit,
                                    key: k,
                                    value: old_v,
                                    sign: -1,
                                },
                                pos,
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
        self.run_drivers(p, input, rule, plan, drivers)
    }

    /// A table frame's change (`FramePlan`): for each row that changed in the table, its deletions or its guard this
    /// tick, the difference between its membership in the frame's output at the start of the tick and now.
    fn frame_change(&self, frame: &rule::FramePlan) -> Result<Terms, EvalError> {
        let store = |k: &StoreKey| {
            self.stores
                .get(k)
                .ok_or_else(|| EvalError::from(internal_error!("no store for {k:?}")))
        };
        let (rel, del) = (store(&frame.rel)?, store(&frame.del)?);
        let keep = frame.keep.as_ref().map(store).transpose()?;
        let mut rows: BTreeSet<&Row> = rel.delta().map(|(r, _)| r).collect();
        rows.extend(del.delta().map(|(r, _)| r));
        if let Some(k) = keep {
            rows.extend(k.delta().map(|(r, _)| r));
        }
        let mut out = Terms::default();
        for row in rows {
            let was = rel.contained(row) && !del.contained(row) && keep.is_none_or(|k| k.contained(row));
            let is = rel.contains(row) && !del.contains(row) && keep.is_none_or(|k| k.contains(row));
            if was != is {
                out.heads.insert(row.clone(), if is { 1 } else { -1 });
            }
        }
        Ok(out)
    }

    /// Evaluates the terms of `rule` that `drivers` drive, each with its position among the rule's dependencies:
    /// the dependencies after it are read at their old version (`usize::MAX`: every one at its current version).
    fn run_drivers(
        &self,
        p: &Program,
        input: &StepInput<'_>,
        rule: &Rule,
        plan: &Plan,
        mut drivers: Vec<(Driver, usize)>,
    ) -> Result<Terms, EvalError> {
        let cx = self.ctx(p, input);
        let mut out = Terms::default();
        // A copy rule's changed rows map straight to head rows (a full evaluation takes the general way).
        if let Some(copy) = &plan.copy {
            let mut rest = Vec::new();
            for (driver, pos) in drivers {
                match &driver {
                    Driver::Atom { row, sign, .. } => {
                        if let Some(h) = copy.row(&cx, row).map_err(|e| to_eval(e, input.tick, Some(rule)))? {
                            *out.heads.entry(h).or_insert(0) += sign;
                        }
                    }
                    _ => rest.push((driver, pos)),
                }
            }
            if rest.is_empty() {
                return Ok(out);
            }
            drivers = rest;
        }
        let position: BTreeMap<usize, usize> = plan.deps.iter().enumerate().map(|(i, l)| (*l, i)).collect();
        // The join order of each driver's terms, from the stores as they are now.
        let cost = |lit: usize, cols: &[usize], range: bool| -> usize {
            let Some(Literal::Pos(a)) = rule.body.lits.get(lit) else { return usize::MAX };
            let rows = self.stores.get(&rule::atom_store(a)).map_or(0, |s| s.estimate(cols));
            // A range keeps some of the rows its probe finds: assume a small fraction.
            if range { rows / 16 + 1 } else { rows }
        };
        // The orders are kept while every store stays within its power of two; stores below `SMALL_STORE` rows count
        // as one size (they flip between a few rows from tick to tick, and any order joins them cheaply).
        let sizes: Vec<u32> = plan
            .atoms
            .iter()
            .map(|lit| match rule.body.lits.get(*lit) {
                Some(Literal::Pos(a)) => self.stores.get(&rule::atom_store(a)).map_or(0, |s| {
                    (usize::BITS - s.present_len().leading_zeros()).max(SMALL_STORE.trailing_zeros())
                }),
                _ => 0,
            })
            .collect();
        let mut orders: BTreeMap<Option<usize>, Arc<rule::Order>> = BTreeMap::new();
        for (driver, _) in &drivers {
            let lit = driver.lit();
            if orders.contains_key(&lit) {
                continue;
            }
            let mut cache = self.orders.borrow_mut();
            let order = match cache.get(&(rule.id, lit)) {
                Some((at, order)) if *at == sizes => order.clone(),
                _ => {
                    let order = Arc::new(plan.order_for(rule, lit, &cost));
                    cache.insert((rule.id, lit), (sizes.clone(), order.clone()));
                    order
                }
            };
            orders.insert(lit, order);
        }
        let mut buffers = rule::TermBuffers::default();
        for (driver, pos) in drivers {
            let order = orders
                .get(&driver.lit())
                .ok_or_else(|| internal_error!("no join order for driver {:?}", driver.lit()))?;
            let old = |lit: usize| position.get(&lit).is_some_and(|q| *q > pos);
            let mut emit = |f: Found<'_>| -> expr::ExprResult<()> {
                if plan.aggregate {
                    let mut group = Vec::new();
                    let mut aggs = Vec::new();
                    for a in &rule.head.args {
                        match a {
                            HeadArg::Term(t) => group.push(expr::term(&cx, f.env, t)?),
                            HeadArg::Agg(agg) => {
                                let mut tuple = Vec::with_capacity(agg.args.len());
                                for t in &agg.args {
                                    tuple.push(expr::term(&cx, f.env, t)?);
                                }
                                aggs.push(tuple);
                            }
                        }
                    }
                    for (col, tuple) in aggs.into_iter().enumerate() {
                        *out.aggs.entry((group.clone(), col, tuple)).or_insert(0) += f.sign;
                    }
                } else {
                    let row = rule::head_row(&cx, rule, f.env)?;
                    *out.heads.entry(row).or_insert(0) += f.sign;
                }
                Ok(())
            };
            let mut errors: Vec<(Token, ExprError, i64)> = Vec::new();
            let mut error = |t: Token, e: ExprError, s: i64| errors.push((t, e, s));
            out.examined += rule::run_term(
                &cx,
                &self.stores,
                rule,
                plan,
                order,
                &driver,
                &old,
                &mut emit,
                &mut error,
                &mut buffers,
            )?;
            for (t, e, s) in errors {
                let slot = out.errors.entry(t).or_insert((0, None));
                slot.0 += s;
                if slot.1.is_none() {
                    slot.1 = Some(e);
                }
            }
        }
        out.steps = cx.steps.get();
        out.flips_at = cx.flips_at.get();
        out.reads_time = cx.reads_time.get();
        Ok(out)
    }

    /// Applies a rule's evaluated change: raises the errors real valuations hit, then updates its head.
    fn apply(&mut self, p: &Program, input: &StepInput<'_>, rule: &Rule, plan: &Plan, terms: Terms) -> Result<(), EvalError> {
        let tick = input.tick;
        if let Some((_, (_, Some(e)))) = terms.errors.into_iter().find(|(_, (n, _))| *n > 0) {
            return Err(to_eval(e, tick, Some(rule)));
        }
        if plan.aggregate {
            return self.apply_aggregates(p, rule, plan, terms.aggs, tick);
        }
        let store = self.store(plan.head)?;
        let mut writes = 0u64;
        for (row, w) in terms.heads {
            if w != 0 {
                writes += 1;
            }
            store.add(row, w).map_err(|e| to_eval(e, tick, Some(rule)))?;
        }
        self.count_writes(rule.id, writes);
        Ok(())
    }

    fn apply_aggregates(
        &mut self,
        p: &Program,
        rule: &Rule,
        plan: &Plan,
        changes: BTreeMap<(Vec<Value>, usize, Vec<Value>), i64>,
        tick: Tick,
    ) -> Result<(), EvalError> {
        let naggs = rule.head.args.iter().filter(|a| matches!(a, HeadArg::Agg(_))).count();
        let groups = self.groups.entry(rule.id).or_default();
        let mut touched = BTreeSet::new();
        for ((group, col, tuple), w) in changes {
            if w == 0 {
                continue;
            }
            let g = groups.entry(group.clone()).or_insert_with(|| Group {
                tuples: vec![BTreeMap::new(); naggs],
                row: None,
            });
            let slot = g.tuples.get_mut(col).ok_or_else(|| internal_error!("aggregate column {col} out of range"))?;
            let c = slot.entry(tuple.clone()).or_insert(0);
            *c += w;
            if *c == 0 {
                slot.remove(&tuple);
            } else if *c < 0 {
                return Err(internal_error!("an aggregate tuple's support went negative").into());
            }
            touched.insert(group);
        }
        let mut edits: Vec<(Option<Row>, Option<Row>)> = Vec::new();
        for group in touched {
            let Some(g) = groups.get_mut(&group) else { continue };
            let live = g.tuples.first().is_some_and(|t| !t.is_empty());
            let new = if live {
                Some(group_row(p, rule, &group, &g.tuples).map_err(|e| to_eval(e, tick, Some(rule)))?)
            } else {
                None
            };
            if new != g.row {
                edits.push((g.row.clone(), new.clone()));
                g.row = new;
            }
            if !live {
                groups.remove(&group);
            }
        }
        let store = self.store(plan.head)?;
        let mut writes = 0u64;
        for (old, new) in edits {
            if let Some(o) = old {
                writes += 1;
                store.add(o, -1).map_err(|e| to_eval(e, tick, Some(rule)))?;
            }
            if let Some(n) = new {
                writes += 1;
                store.add(n, 1).map_err(|e| to_eval(e, tick, Some(rule)))?;
            }
        }
        self.count_writes(rule.id, writes);
        Ok(())
    }

    /// A recompute rule: evaluated in full, its change is the difference from its last output.
    fn recompute_rule(&mut self, p: &Program, input: &StepInput<'_>, rule: &Rule, plan: &Plan) -> Result<(), EvalError> {
        let terms = self.evaluate(p, input, rule, plan, true)?;
        self.count(rule.id, terms.examined, terms.steps);
        let (flips_at, reads_time) = (terms.flips_at, terms.reads_time);
        let tick = input.tick;
        if let Some((_, (_, Some(e)))) = terms.errors.into_iter().find(|(_, (n, _))| *n > 0) {
            return Err(to_eval(e, tick, Some(rule)));
        }
        let new: BTreeMap<Row, i64> = if plan.aggregate {
            // The groups from scratch.
            let naggs = rule.head.args.iter().filter(|a| matches!(a, HeadArg::Agg(_))).count();
            let mut groups: BTreeMap<Vec<Value>, Vec<BTreeMap<Vec<Value>, i64>>> = BTreeMap::new();
            for ((group, col, tuple), w) in terms.aggs {
                if w <= 0 {
                    continue;
                }
                let g = groups.entry(group).or_insert_with(|| vec![BTreeMap::new(); naggs]);
                if let Some(slot) = g.get_mut(col) {
                    slot.insert(tuple, w);
                }
            }
            let mut rows = BTreeMap::new();
            for (group, tuples) in groups {
                rows.insert(group_row(p, rule, &group, &tuples).map_err(|e| to_eval(e, tick, Some(rule)))?, 1);
            }
            rows
        } else {
            terms.heads.into_iter().filter(|(_, w)| *w > 0).collect()
        };
        // Only the difference from the last output is applied: a row in both, with the same support, is left alone
        // (retracting and re-adding it would change nothing but rebuild its index entries and touch the store).
        let old = self.prev.remove(&rule.id).unwrap_or_default();
        let store = self.store(plan.head)?;
        let mut writes = 0u64;
        for (row, w) in &old {
            let d = new.get(row).copied().unwrap_or(0) - w;
            if d != 0 {
                writes += 1;
                store.add(row.clone(), d).map_err(|e| to_eval(e, tick, Some(rule)))?;
            }
        }
        for (row, w) in &new {
            if !old.contains_key(row) {
                writes += 1;
                store.add(row.clone(), *w).map_err(|e| to_eval(e, tick, Some(rule)))?;
            }
        }
        self.count_writes(rule.id, writes);
        self.prev.insert(rule.id, new);
        if plan.skip == rule::Skip::WhenUnchanged {
            let i = rule.id.index();
            if self.recomputed.len() <= i {
                self.recomputed.resize(i + 1, None);
            }
            if let Some(slot) = self.recomputed.get_mut(i) {
                // Read freely, the time may change its output at the next tick: it is evaluated again.
                *slot = if reads_time { None } else { Some(flips_at) };
            }
        }
        Ok(())
    }

    /// A recursive stratum: re-evaluated to its fixpoint when anything it reads changed, when its own relations'
    /// support from outside the stratum changed (the carried state, inputs, facts), or when a rule reads a
    /// time-varying scalar; its change is the difference from its last fixpoint.
    ///
    /// The support from outside matters even where the present rows did not change: the naive fixpoint starts from
    /// it, and a rule that reads a lattice cell into a set column keeps a row for every value the cell passes through
    /// on the way to the fixpoint (a cell that starts at its final value passes through none).
    fn recursive_stratum(&mut self, p: &Program, input: &StepInput<'_>, s: &Stratum) -> Result<(), EvalError> {
        let tick = input.tick;
        let ids: Vec<RuleId> = s.aggregates.iter().chain(&s.rules).copied().collect();
        let mut dirty = false;
        for id in &ids {
            let plan = self.plans.get(id).ok_or_else(|| internal_error!("rule {id:?} has no plan"))?;
            let rule = p.rules.get(*id).ok_or_else(|| internal_error!("rule {id:?}"))?;
            dirty |= plan.regime == Regime::Recompute
                || self.stores.get(&plan.head).is_some_and(|st| st.touched)
                || rule
                    .body
                    .lits
                    .iter()
                    .filter_map(dep_store)
                    .any(|k| self.stores.get(&k).is_some_and(Store::changed));
        }
        if !dirty {
            return Ok(());
        }
        // Take back the last fixpoint's derivations.
        for id in &ids {
            let old = self.prev.remove(id).unwrap_or_default();
            let head = self.plans.get(id).map(|pl| pl.head).ok_or_else(|| internal_error!("rule {id:?}"))?;
            let rule = p.rules.get(*id).ok_or_else(|| internal_error!("rule {id:?}"))?;
            let store = self.store(head)?;
            for (row, w) in old {
                store.add(row, -w).map_err(|e| to_eval(e, tick, Some(rule)))?;
            }
        }
        self.settle(tick)?;
        // The aggregates read only lower strata: once, first.
        for id in &s.aggregates {
            let plan = self.plans.get(id).ok_or_else(|| internal_error!("rule {id:?}"))?.clone();
            let rule = p.rules.get(*id).ok_or_else(|| internal_error!("rule {id:?}"))?;
            self.recompute_rule(p, input, rule, &plan)?;
        }
        self.settle(tick)?;
        if self.semi_naive_applies(p, s) {
            let derived = self.semi_naive(p, input, s)?;
            for (id, rows) in derived {
                self.prev.insert(id, rows.into_iter().map(|r| (r, 1)).collect());
            }
            return Ok(());
        }
        // The other rules, naively to the fixpoint. A program error counts only at the fixpoint, where every rule
        // runs once more with errors fatal (a row that run adds resumes the iteration).
        let mut derived: BTreeMap<RuleId, BTreeSet<Row>> = BTreeMap::new();
        let mut strict = false;
        let mut rounds = 0u32;
        loop {
            let mut changed = false;
            for id in &s.rules {
                let plan = self.plans.get(id).ok_or_else(|| internal_error!("rule {id:?}"))?.clone();
                let rule = p.rules.get(*id).ok_or_else(|| internal_error!("rule {id:?}"))?;
                let terms = self.evaluate(p, input, rule, &plan, true)?;
                self.count(rule.id, terms.examined, terms.steps);
                if let Some((_, (_, Some(e)))) = terms.errors.into_iter().find(|(_, (n, _))| *n > 0) {
                    if strict {
                        return Err(to_eval(e, tick, Some(rule)));
                    }
                    continue;
                }
                let mine = derived.entry(*id).or_default();
                let mut fresh = Vec::new();
                for (row, w) in terms.heads {
                    if w > 0 && !mine.contains(&row) {
                        fresh.push(row);
                    }
                }
                // A round changed the database if a row appeared or a cell's value moved (a row another rule
                // already derived, or a dominated contribution, changes nothing: the reference counts only changes).
                let before = self.stores.get(&plan.head).map_or(0, Store::generation);
                for row in fresh {
                    mine.insert(row.clone());
                    self.store(plan.head)?.add(row, 1).map_err(|e| to_eval(e, tick, Some(rule)))?;
                }
                self.settle(tick)?;
                if self.stores.get(&plan.head).map_or(0, Store::generation) != before {
                    changed = true;
                }
            }
            if strict && !changed {
                break;
            }
            strict = !changed;
            rounds += 1;
            if rounds >= self.max_rounds {
                let label = s.rules.first().and_then(|id| p.rules.get(*id)).map(|r| r.label.clone());
                return Err(EvalError::Program {
                    tick,
                    error: ProgramErrorRecord {
                        code: blossom_base::code!("BLSR007").as_str(),
                        rule: label,
                        detail: Arc::from(format!(
                            "the fixpoint did not converge within {} rounds (CR-53)",
                            self.max_rounds
                        )),
                    },
                });
            }
        }
        for (id, rows) in derived {
            self.prev.insert(id, rows.into_iter().map(|r| (r, 1)).collect());
        }
        Ok(())
    }

    /// Whether a recursive stratum's fixpoint can be reached semi-naively with the naive iteration's result: its
    /// heads are set relations (a lattice cell's rows depend on the values it passes through on the way), and its
    /// rules read their own stratum's relations only through positive atoms (stratification already keeps negations
    /// and lookups off them; this does not rely on it).
    fn semi_naive_applies(&self, p: &Program, s: &Stratum) -> bool {
        let mut heads: BTreeSet<StoreKey> = BTreeSet::new();
        for id in &s.rules {
            let Some(plan) = self.plans.get(id) else { return false };
            heads.insert(plan.head);
        }
        if heads.iter().any(|h| self.stores.get(h).is_none_or(|st| st.cell.is_some())) {
            return false;
        }
        s.rules.iter().all(|id| {
            let (Some(rule), Some(plan)) = (p.rules.get(*id), self.plans.get(id)) else { return false };
            !plan.aggregate
                && rule.body.lits.iter().all(|l| match l {
                    Literal::Neg(_) | Literal::Lookup { .. } => dep_store(l).is_none_or(|k| !heads.contains(&k)),
                    _ => true,
                })
        })
    }

    /// A recursive stratum's fixpoint, semi-naively, doing what the naive iteration does round for round.
    ///
    /// The naive iteration runs the rules in order, each over the stores as they are then (Gauss–Seidel), adding
    /// what it derives at once. Here each rule runs in full the first time; after that it is driven only by the rows
    /// that became present since it last ran (each such row drives the rule's atoms over its relation, the other
    /// atoms read as they are now). A valuation of only older rows was found when the rule last ran, over stores
    /// that held them, so the rule derives exactly what the naive iteration's full run derives at that point: the
    /// stores go through the same states, every round changes what the naive round changes, and the round bound
    /// (CR-53, BLSR007) is reached at the same round. The work is each valuation about once instead of once per
    /// round: a chain of n rows costs O(n), not O(n²).
    ///
    /// Errors, as the naive iteration has them: a rule with a valuation that raises is skipped for the round (it
    /// derives nothing), and since rows only accumulate that valuation stays, so it is skipped every round after
    /// (here: not run again). At the fixpoint the naive iteration runs every rule once more with errors fatal; a rule
    /// that never raised has had every valuation evaluated without error and derives nothing new, so only the first
    /// rule that raised needs that run, which reports its error.
    fn semi_naive(
        &mut self,
        p: &Program,
        input: &StepInput<'_>,
        s: &Stratum,
    ) -> Result<BTreeMap<RuleId, BTreeSet<Row>>, EvalError> {
        let tick = input.tick;
        let mut derived: BTreeMap<RuleId, BTreeSet<Row>> = BTreeMap::new();
        // The rows of each head that became present during the iteration, in order; and, per rule, how far into each
        // log it had read when it last ran (absent: it has not run).
        let mut log: BTreeMap<StoreKey, Vec<Row>> = BTreeMap::new();
        let mut read: BTreeMap<RuleId, BTreeMap<StoreKey, usize>> = BTreeMap::new();
        let mut raised: BTreeSet<RuleId> = BTreeSet::new();
        let mut strict = false;
        let mut rounds = 0u32;
        loop {
            if strict {
                // The naive iteration's last round: the first rule that raised reports its error.
                let Some(id) = s.rules.iter().find(|id| raised.contains(id)) else { break };
                let plan = self.plans.get(id).ok_or_else(|| internal_error!("rule {id:?}"))?.clone();
                let rule = p.rules.get(*id).ok_or_else(|| internal_error!("rule {id:?}"))?;
                let terms = self.evaluate(p, input, rule, &plan, true)?;
                self.count(rule.id, terms.examined, terms.steps);
                return match terms.errors.into_iter().find(|(_, (n, _))| *n > 0) {
                    Some((_, (_, Some(e)))) => Err(to_eval(e, tick, Some(rule))),
                    _ => Err(internal_error!("rule {id:?} raised during the iteration and not at its fixpoint").into()),
                };
            }
            let mut changed = false;
            for id in &s.rules {
                if raised.contains(id) {
                    continue;
                }
                let plan = self.plans.get(id).ok_or_else(|| internal_error!("rule {id:?}"))?.clone();
                let rule = p.rules.get(*id).ok_or_else(|| internal_error!("rule {id:?}"))?;
                let now: BTreeMap<StoreKey, usize> = log.iter().map(|(k, rows)| (*k, rows.len())).collect();
                let terms = match read.insert(*id, now) {
                    None => self.evaluate(p, input, rule, &plan, true)?,
                    Some(before) => {
                        let mut drivers: Vec<(Driver, usize)> = Vec::new();
                        for (lit, l) in rule.body.lits.iter().enumerate() {
                            let Literal::Pos(a) = l else { continue };
                            let key = rule::atom_store(a);
                            let Some(rows) = log.get(&key) else { continue };
                            let from = before.get(&key).copied().unwrap_or(0);
                            for row in rows.get(from..).unwrap_or_default() {
                                drivers.push((
                                    Driver::Atom {
                                        lit,
                                        row: row.clone(),
                                        sign: 1,
                                    },
                                    usize::MAX,
                                ));
                            }
                        }
                        if drivers.is_empty() {
                            continue;
                        }
                        self.run_drivers(p, input, rule, &plan, drivers)?
                    }
                };
                self.count(rule.id, terms.examined, terms.steps);
                if terms.errors.values().any(|(n, _)| *n > 0) {
                    raised.insert(*id);
                    continue;
                }
                let mine = derived.entry(*id).or_default();
                for (row, w) in terms.heads {
                    if w > 0 && !mine.contains(&row) {
                        mine.insert(row.clone());
                        let store = self.store(plan.head)?;
                        let fresh = !store.contains(&row);
                        store.add(row.clone(), 1).map_err(|e| to_eval(e, tick, Some(rule)))?;
                        if fresh {
                            changed = true;
                            log.entry(plan.head).or_default().push(row);
                        }
                    }
                }
                self.settle(tick)?;
            }
            strict = !changed;
            rounds += 1;
            if rounds >= self.max_rounds {
                let label = s.rules.first().and_then(|id| p.rules.get(*id)).map(|r| r.label.clone());
                return Err(EvalError::Program {
                    tick,
                    error: ProgramErrorRecord {
                        code: blossom_base::code!("BLSR007").as_str(),
                        rule: label,
                        detail: Arc::from(format!(
                            "the fixpoint did not converge within {} rounds (CR-53)",
                            self.max_rounds
                        )),
                    },
                });
            }
        }
        Ok(derived)
    }

    fn check_keys(&self, p: &Program, tick: Tick) -> Result<(), EvalError> {
        for (rel, cols, upsert) in &self.keyed {
            let Some(store) = self.stores.get(&StoreKey::Main(*rel)) else { continue };
            for row in &store.ins {
                let k: Vec<Value> = cols.iter().filter_map(|c| row.get(*c).cloned()).collect();
                let rows = store.new_rows(cols, &k)?;
                if rows.len() > 1 {
                    let name = p.rels.get(*rel).map(|d| d.name.to_string()).unwrap_or_default();
                    let code = if *upsert {
                        blossom_base::code!("BLSR002")
                    } else {
                        blossom_base::code!("BLSR001")
                    };
                    return Err(EvalError::Program {
                        tick,
                        error: ProgramErrorRecord {
                            code: code.as_str(),
                            rule: None,
                            detail: Arc::from(format!("{name}: {} tuples with key {k:?} in one tick", rows.len())),
                        },
                    });
                }
            }
        }
        Ok(())
    }

    fn check_invariants(&self, p: &Program, tick: Tick) -> Result<(), EvalError> {
        for id in &self.violations {
            let rule = p.rules.get(*id).ok_or_else(|| internal_error!("rule {id:?}"))?;
            let HeadMode::Violation { invariant } = rule.head.mode else { continue };
            let Some(row) = self
                .stores
                .get(&StoreKey::Main(rule.head.rel))
                .and_then(|s| s.present().next())
            else {
                continue;
            };
            let inv = p.invariants.get(invariant);
            if let Some(inv) = inv
                && inv.action != ViolationAction::Abort
            {
                return Err(blossom_base::unimplemented_error!(
                    "LANG-200",
                    "the `{:?}` violation action in the engine",
                    inv.action
                )
                .into());
            }
            let name = inv.map_or_else(|| format!("{invariant:?}"), |i| i.name.to_string());
            return Err(EvalError::Program {
                tick,
                error: ProgramErrorRecord {
                    code: blossom_base::code!("BLSR003").as_str(),
                    rule: Some(rule.label.clone()),
                    detail: Arc::from(format!("invariant `{name}` is violated by {row:?}")),
                },
            });
        }
        Ok(())
    }

    /// Runs one tick in the reference interface (for the differential suite): the carried state in `input` must be
    /// what this engine carried (the engine keeps its own), and the full instance is materialized.
    pub fn tick_full(&mut self, input: &TickInput<'_>) -> Result<TickOutput, EvalError> {
        if input.capture {
            return Err(blossom_base::unimplemented_error!("TEST-050", "provenance capture in the engine").into());
        }
        let step = self.step(
            &StepInput {
                node: input.node,
                incarnation: input.incarnation,
                tick: input.tick,
                now: input.now,
                events: input.events,
                delivered: input.delivered,
                ingress: input.ingress,
                blobs: input.blobs,
            },
            &[],
        )?;
        let mut instance = Instance::default();
        for (key, s) in self.stores.iter() {
            if let StoreKey::Main(rel) = key {
                for r in s.present() {
                    instance.insert(rel, r.clone());
                }
            }
        }
        Ok(TickOutput {
            instance,
            next: self.next_instance()?,
            outbox: step.outbox,
            egress: step.egress,
            host: step.host,
            firings: Vec::new(),
            blobs: step.blobs,
        })
    }

    /// Whether a row of any store holds `b`: derived rows keep their blobs across ticks without re-creating them.
    /// Each store counts its rows' blobs as they change, so this is a lookup per store that can hold blobs.
    pub fn holds_blob(&self, b: &blossom_value::BlobRef) -> bool {
        self.stores.values().any(|s| s.holds_blob(b))
    }

    /// Rows the atom probes returned since the engine was created: the join work, measured without a clock.
    pub fn rows_examined(&self) -> u64 {
        self.examined
    }

    /// The work of each rule since the engine was created (the rules that did any): rows examined, as
    /// [`Engine::rows_examined`], and expression nodes evaluated.
    pub fn work_by_rule(&self) -> &BTreeMap<RuleId, RuleWork> {
        &self.examined_by
    }

    /// Starts (afresh) or stops counting each function's work.
    pub fn set_profile_functions(&mut self, on: bool) {
        self.fn_work = on.then(|| std::cell::RefCell::new(BTreeMap::new()));
    }

    /// Each function's work since profiling was switched on (`None`: off).
    pub fn work_by_function(&self) -> Option<BTreeMap<blossom_base::FnId, FnWork>> {
        self.fn_work.as_ref().map(|w| w.borrow().clone())
    }

    fn count_writes(&mut self, rule: RuleId, writes: u64) {
        if writes > 0 {
            self.examined_by.entry(rule).or_default().writes += writes;
        }
    }

    fn count(&mut self, rule: RuleId, examined: u64, steps: u64) {
        self.examined += examined;
        if examined > 0 || steps > 0 {
            let w = self.examined_by.entry(rule).or_default();
            w.rows += examined;
            w.steps += steps;
        }
    }

    pub fn node(&self) -> NodeId {
        self.node
    }
}

/// A join order with the sizes of the stores it was chosen for, in powers of two (`Engine::orders`).
type CachedOrder = (Vec<u32>, Arc<rule::Order>);

/// A rule's evaluated change: head rows (or aggregate tuples) with signed weights, and runtime errors per valuation.
#[derive(Default)]
struct Terms {
    /// The rows its atom probes returned, and the expression nodes it evaluated (the work it did).
    examined: u64,
    steps: u64,
    heads: BTreeMap<Row, i64>,
    aggs: BTreeMap<(Vec<Value>, usize, Vec<Value>), i64>,
    errors: BTreeMap<Token, (i64, Option<ExprError>)>,
    /// When a comparison of `now()` it evaluated would come out the other way (`Ctx::flips_at`), and whether it
    /// read the time otherwise (`Ctx::reads_time`).
    flips_at: Option<blossom_value::time::Instant>,
    reads_time: bool,
}

/// An aggregate rule's head row for a group whose live argument tuples are `tuples` (per aggregate column).
fn group_row(
    p: &Program,
    rule: &Rule,
    group: &[Value],
    tuples: &[BTreeMap<Vec<Value>, i64>],
) -> expr::ExprResult<Row> {
    let mut keys = group.iter();
    let mut aggs = tuples.iter();
    let mut row = Vec::with_capacity(rule.head.args.len());
    for (col, a) in rule.head.args.iter().enumerate() {
        match a {
            HeadArg::Term(_) => row.push(keys.next().cloned().ok_or_else(|| bug("a group key too short".into()))?),
            HeadArg::Agg(agg) => {
                let set = aggs.next().ok_or_else(|| bug("aggregate tuples missing".into()))?;
                row.push(fold(p, rule, col, &agg.func, set)?);
            }
        }
    }
    Ok(Row::from(row))
}

/// An aggregate over a group's distinct argument tuples (LANG-100).
fn fold(p: &Program, rule: &Rule, col: usize, func: &AggFunc, set: &BTreeMap<Vec<Value>, i64>) -> expr::ExprResult<Value> {
    let single = |t: &Vec<Value>| -> expr::ExprResult<Value> {
        match t.as_slice() {
            [v] => Ok(v.clone()),
            _ => Err(bug(format!("{func:?} over a tuple of {} values", t.len()))),
        }
    };
    match func {
        AggFunc::Count => {
            let n = set.len() as u64;
            let ty = p
                .rels
                .get(rule.head.rel)
                .and_then(|r| r.schema.cols.get(col))
                .and_then(|c| p.types.get(c.ty))
                .ok_or_else(|| bug("a count's column has no type".into()))?;
            let overflow = || ExprError::Arithmetic(format!("count {n} does not fit its column"));
            use blossom_value::types::IntTy;
            Ok(Value::Int(match ty {
                TypeDef::Int(IntTy::U64) => IntValue::U64(n),
                TypeDef::Int(IntTy::I64) => IntValue::I64(i64::try_from(n).map_err(|_| overflow())?),
                TypeDef::Int(IntTy::U32) => IntValue::U32(u32::try_from(n).map_err(|_| overflow())?),
                TypeDef::Int(IntTy::I32) => IntValue::I32(i32::try_from(n).map_err(|_| overflow())?),
                TypeDef::Int(IntTy::U128) => IntValue::U128(u128::from(n)),
                TypeDef::Int(IntTy::I128) => IntValue::I128(i128::from(n)),
                TypeDef::Int(IntTy::U16) => IntValue::U16(u16::try_from(n).map_err(|_| overflow())?),
                TypeDef::Int(IntTy::I16) => IntValue::I16(i16::try_from(n).map_err(|_| overflow())?),
                TypeDef::Int(IntTy::U8) => IntValue::U8(u8::try_from(n).map_err(|_| overflow())?),
                TypeDef::Int(IntTy::I8) => IntValue::I8(i8::try_from(n).map_err(|_| overflow())?),
                other => return Err(bug(format!("a count in a column of type {other:?}"))),
            }))
        }
        // The tuples are single values, kept in value order: the least and greatest are the ends.
        AggFunc::Min => set.keys().next().map(single).ok_or_else(|| bug("min over an empty group".into()))?,
        AggFunc::Max => set
            .keys()
            .next_back()
            .map(single)
            .ok_or_else(|| bug("max over an empty group".into()))?,
        AggFunc::Sum => {
            // The first component of each distinct tuple; the rest is the valuation it belongs to.
            let vals = set
                .keys()
                .map(|t| t.first().cloned().ok_or_else(|| bug("a sum over an empty tuple".into())))
                .collect::<Result<Vec<_>, _>>()?;
            expr::int_sum(vals.iter())
        }
        // The first component of each distinct tuple, in the set's (canonical) order.
        AggFunc::CollectVec => {
            let vals = set
                .keys()
                .map(|t| t.first().cloned().ok_or_else(|| bug("a collect over an empty tuple".into())))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Vec(vals.into()))
        }
        other => Err(bug(format!("the aggregate {other:?} passed the support check"))),
    }
}

/// A runtime error as the tick's error, attributed to `rule` when known.
fn to_eval(e: ExprError, tick: Tick, rule: Option<&Rule>) -> EvalError {
    let program_error = |code: &'static str, detail: String| EvalError::Program {
        tick,
        error: ProgramErrorRecord {
            code,
            rule: rule.map(|r| r.label.clone()),
            detail: Arc::from(detail),
        },
    };
    match e {
        ExprError::Arithmetic(d) => program_error(blossom_base::code!("BLSR004").as_str(), d),
        ExprError::Conflict(d) => program_error(blossom_base::code!("BLSR006").as_str(), d),
        ExprError::Refused(d) => program_error(blossom_base::code!("BLSR010").as_str(), d),
        ExprError::Budget(d) => program_error(blossom_base::code!("BLSR012").as_str(), d),
        ExprError::Eval(e) => e,
    }
}
