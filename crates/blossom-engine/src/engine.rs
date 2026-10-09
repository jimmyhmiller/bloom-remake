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
    AggFunc, ConstructKind, HeadArg, HeadMode, LatticeCtor, Literal, Program, RelClass, Rule, RuleKind, ViolationAction,
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
    /// This node's role when it is a client member (CLIENTS.md §2): its id is outside the deployment, so `roles` and
    /// `node_names` do not name it.
    pub client_role: Option<RoleId>,
    /// The keyed members the host gave node ids (docs/design/KEYED.md §3): `self` and senders are their values, and
    /// sends to them go to those ids.
    pub members: Arc<blossom_ir::members::Members>,
    /// How many rows the engine's tiered stores keep of their recent probes, together (docs/design/DATABASE.md §7,
    /// the hot tier; `None`: [`crate::store::HOT_ROWS`]).
    pub hot_rows: Option<usize>,
    /// Keep every durable table in memory even when started on the node's database ([`Engine::reset_on`] tiers
    /// none): a deployment's `storage.tiered = false`, for state that fits in memory and the in-memory engine's speed.
    pub in_memory: bool,
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
    /// The work of each rule in the last tick (cleared at each tick's start).
    tick_work: BTreeMap<RuleId, RuleWork>,
    /// The blobs the current tick created (`Blob::of`), with their bytes.
    new_blobs: std::cell::RefCell<BTreeMap<blossom_value::BlobRef, Arc<[u8]>>>,
    /// Each (rule, driver literal)'s join order, with the sizes of the stores its atoms read when it was chosen, in
    /// powers of two: reused while every one stays in its power of two. A join order decides a term's cost, never
    /// its valuations, so reusing one changes only the work.
    orders: std::cell::RefCell<BTreeMap<(RuleId, Option<usize>), CachedOrder>>,
    /// The scratch buffers of a term's search, kept from one rule evaluation to the next.
    buffers: std::cell::RefCell<rule::TermBuffers>,
    /// Each function's work since profiling was switched on (`None`: off).
    fn_work: Option<std::cell::RefCell<BTreeMap<blossom_base::FnId, FnWork>>>,
    /// The derived tick-scoped relations ([`tick_scoped`]), emptied at the start of every tick.
    scoped: Vec<RelId>,
    /// The set tables carried by their frame (`FramePlan`), whose next state is a delta over their present rows: the
    /// frame's part (the rows of `Main` not deleted, and kept) is read from the stores it reads, and their `Next`
    /// store holds only the other next-state rules' support. A table keeps no second copy of its rows from one tick
    /// to the next (docs/design/DATABASE.md §7). A lattice table's next state merges the frame's rows with the other
    /// contributions, so its `Next` store holds them all.
    framed: BTreeMap<RelId, rule::FramePlan>,
    /// The framed tables that may be tiered (durable, and written by no rule but their next state's), with their key
    /// columns.
    tierable: BTreeMap<RelId, Vec<usize>>,
    /// The tables tiered since the last reset ([`Engine::reset_on`]): their rows are the cold side's, and their
    /// `Main` store takes each tick's change to its next state at the tick's end.
    tiered: BTreeSet<RelId>,
    /// How many rows each tiered table keeps of its recent probes.
    hot_rows: usize,
    /// Tier no table (`EngineConfig::in_memory`).
    in_memory: bool,
    /// The durable views kept on the cold side since the last reset (docs/design/DATABASE.md §8), and the rules that
    /// write them.
    views: BTreeSet<RelId>,
    /// The aggregate rules among the views' rules, by their shape: kept without the groups' tuples in memory.
    durable_aggs: BTreeMap<RuleId, AggShape>,
    /// The rules of mixed relations (kept on the cold side for their durable rules' support) that are not durable:
    /// their support is kept in memory only, and derived again after a restart.
    volatile_rules: BTreeSet<RuleId>,
    /// The durable tables kept in memory since the last reset onto the cold side (loaded from it): sources of durable
    /// views as the tiered tables are.
    durable_in_memory: BTreeSet<RelId>,
    view_rules: BTreeSet<RuleId>,
    /// The durable tables the views' rules read: each tick reports their rows it held beyond those carried into it.
    view_sources: BTreeSet<RelId>,
    /// The catch-up the first tick after a resume runs first, if the views resumed from the cold side.
    boot: Option<blossom_ir::tick::CatchUp>,
    /// The first tick after resuming on the cold side with its views: what memory held and the restart lost is
    /// derived in full (the views and tiered tables show only the tick's change).
    rebuilding: bool,
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
            LatticeCtor::Product { fields, .. } => Kind::Product(
                fields
                    .iter()
                    .map(|(_, id)| kind(p, &p.lattices.get(*id)?.ctor, depth + 1))
                    .collect::<Option<Vec<Kind>>>()?,
            ),
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
                return Err(
                    blossom_base::unimplemented_error!("LANG-051", "host-maintained tables in the engine").into(),
                );
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
                    AggFunc::Count
                        | AggFunc::Sum
                        | AggFunc::Min
                        | AggFunc::Max
                        | AggFunc::CollectVec
                        | AggFunc::CollectVecAt { .. }
                ) && agg.order.is_none()
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
    let decl = p
        .rels
        .get(rel)
        .ok_or_else(|| internal_error!("relation {rel:?} is not declared"))?;
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

/// The tick-scoped relations: those whose rows can hold only in a tick with an event. The events (host events and
/// delivered messages, which hold for their one tick), and a derived relation all of whose deductive rules on this node
/// read one positively and aggregate nothing, that has no facts, merges no lattice and is in no recursive stratum. Its
/// rules can be evaluated from scratch each tick instead of retracting the last tick's rows one by one
/// ([`Regime::Scoped`]); readers that are not themselves scoped see the retraction as a change, as before.
fn tick_scoped(p: &Program, rules: &[RuleId], recursive: &BTreeSet<RelId>) -> BTreeSet<RelId> {
    let mut scoped: BTreeSet<RelId> = p
        .rels
        .iter_enumerated()
        .filter(|(_, r)| matches!(r.class, RelClass::Event(_) | RelClass::Channel(_)))
        .map(|(id, _)| id)
        .collect();
    let with_facts: BTreeSet<RelId> = p.facts.iter().map(|f| f.rel).collect();
    let mut by_head: BTreeMap<RelId, Vec<&Rule>> = BTreeMap::new();
    let mut other_writes: BTreeSet<RelId> = BTreeSet::new();
    for id in rules {
        let Some(rule) = p.rules.get(*id) else { continue };
        match rule.kind {
            RuleKind::Deductive => by_head.entry(rule.head.rel).or_default().push(rule),
            _ => {
                other_writes.insert(rule.head.rel);
            }
        }
    }
    loop {
        let mut grew = false;
        for (rel, rules) in &by_head {
            if scoped.contains(rel) || with_facts.contains(rel) || recursive.contains(rel) || other_writes.contains(rel)
            {
                continue;
            }
            let Some(decl) = p.rels.get(*rel) else { continue };
            if decl.class != RelClass::Idb || !decl.schema.lattice.is_empty() {
                continue;
            }
            let all = rules.iter().all(|r| {
                !strata::is_aggregate(r)
                    && r.body
                        .lits
                        .iter()
                        .any(|l| matches!(l, Literal::Pos(a) if scoped.contains(&a.rel)))
            });
            if all {
                scoped.insert(*rel);
                grew = true;
            }
        }
        if !grew {
            return scoped;
        }
    }
}

/// Whether a table's frame carries `row` (`FramePlan`: in the table, not deleted, and kept), at the start of the
/// tick (`old`) or now.
fn frame_holds(rel: &Store, del: &Store, keep: Option<&Store>, row: &Row, old: bool) -> Result<bool, EvalError> {
    let has = |s: &Store| if old { s.contained(row) } else { s.contains(row) };
    Ok(has(rel)? && !has(del)? && keep.map(has).transpose()?.unwrap_or(true))
}

impl Engine {
    /// Prepares `program` for node `node`: checks what it evaluates, stratifies, plans every rule the node runs, and
    /// creates and indexes every store.
    pub fn new(program: ValidatedProgram, node: NodeId, cfg: EngineConfig) -> Result<Engine, EvalError> {
        let p = program.get();
        blossom_ir::tick::bind_externs(p, &cfg.externs)?;
        check_supported(p)?;
        let kinds = kinds(p);
        let mut own_seed = None;
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
                // A keyed member's seed derives from its member name (`Game:"game-17"`), wherever it runs.
                if let Some(m) = cfg.members.get(node) {
                    let name = blossom_ir::members::member_name(p, &m);
                    own_seed = Some(
                        blossom_value::Seeds::derive(root, &name)
                            .map_err(|e| internal_error!("deriving the seed of member {name}: {e}"))?
                            .node,
                    );
                } else if node.is_client() {
                    // A client member's seed derives from its name, as a deployment node's does.
                    let name = blossom_ir::printer::node_text(node, &cfg.node_names);
                    own_seed = Some(
                        blossom_value::Seeds::derive(root, &name)
                            .map_err(|e| internal_error!("deriving the seed of client {name}: {e}"))?
                            .node,
                    );
                }
                (Some(choice), seeds)
            }
            None => (None, Vec::new()),
        };
        let my_role = if let Some(m) = cfg.members.get(node) {
            Some(m.role)
        } else if node.is_client() {
            cfg.client_role
        } else {
            cfg.roles.get(node.0 as usize).copied().flatten()
        };
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
        // Tick-scoped heads: emptied each tick, their rules evaluated from scratch while an event is there.
        let recursive: BTreeSet<RelId> = strata_list
            .iter()
            .filter(|s| s.recursive)
            .flat_map(|s| s.rules.iter().chain(&s.aggregates))
            .filter_map(|id| p.rules.get(*id).map(|r| r.head.rel))
            .collect();
        let running: Vec<RuleId> = p
            .rules
            .iter_enumerated()
            .filter(|(id, _)| plans.contains_key(id))
            .map(|(id, _)| id)
            .collect();
        let scoped_rels = tick_scoped(p, &running, &recursive);
        let mut scoped_derived = Vec::new();
        for id in &running {
            let Some(rule) = p.rules.get(*id) else { continue };
            if rule.kind != RuleKind::Deductive || !scoped_rels.contains(&rule.head.rel) {
                continue;
            }
            let Some(plan) = plans.get(id) else { continue };
            let mut plan = Plan::clone(plan);
            plan.regime = Regime::Scoped;
            plan.scoped = rule
                .body
                .lits
                .iter()
                .filter_map(|l| match l {
                    Literal::Pos(a) if scoped_rels.contains(&a.rel) => Some(rule::atom_store(a)),
                    _ => None,
                })
                .collect();
            plans.insert(*id, plan);
            if !scoped_derived.contains(&rule.head.rel) {
                scoped_derived.push(rule.head.rel);
            }
        }
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
                let rel = p
                    .rules
                    .get(*id)
                    .map(|r| r.head.rel)
                    .ok_or_else(|| internal_error!("rule {id:?}"))?;
                let spec = cell_spec(p, &kinds, rel, 0)?;
                stores.insert_absent(plan.head, || Store::new(spec, rel_holds_blobs(p, rel)));
            }
        }
        let mut framed = BTreeMap::new();
        for id in inductive.iter() {
            if let Some(plan) = plans.get(id)
                && let (Some(frame), StoreKey::Next(rel)) = (&plan.frame, plan.head)
                && stores.get(&plan.head).is_some_and(|s| s.cell.is_none())
            {
                framed.insert(rel, frame.clone());
            }
        }
        let deduced: BTreeSet<RelId> = p
            .rules
            .iter_enumerated()
            .filter(|(id, r)| plans.contains_key(id) && r.kind == RuleKind::Deductive)
            .map(|(_, r)| r.head.rel)
            .collect();
        let tierable = framed
            .keys()
            .filter_map(|rel| {
                let decl = p.rels.get(*rel)?;
                (decl.durable && !deduced.contains(rel))
                    .then(|| (*rel, decl.schema.key.iter().map(|c| c.index()).collect()))
            })
            .collect();
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
                own_seed,
                roles: cfg.roles,
                members: cfg.members,
                kinds,
                externs: cfg.externs,
                node_names: cfg.node_names,
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
            tick_work: BTreeMap::new(),
            new_blobs: std::cell::RefCell::new(BTreeMap::new()),
            orders: std::cell::RefCell::new(BTreeMap::new()),
            buffers: std::cell::RefCell::new(rule::TermBuffers::default()),
            fn_work: None,
            scoped: scoped_derived,
            framed,
            tierable,
            tiered: BTreeSet::new(),
            hot_rows: cfg.hot_rows.unwrap_or(crate::store::HOT_ROWS),
            in_memory: cfg.in_memory,
            views: BTreeSet::new(),
            durable_aggs: BTreeMap::new(),
            volatile_rules: BTreeSet::new(),
            durable_in_memory: BTreeSet::new(),
            view_rules: BTreeSet::new(),
            view_sources: BTreeSet::new(),
            boot: None,
            rebuilding: false,
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
            let rule = p
                .rules
                .get(plan.rule)
                .ok_or_else(|| internal_error!("rule {:?}", plan.rule))?;
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
        self.tiered.clear();
        self.views.clear();
        self.durable_aggs.clear();
        self.volatile_rules.clear();
        self.durable_in_memory.clear();
        self.view_rules.clear();
        self.view_sources.clear();
        self.boot = None;
        self.rebuilding = false;
        for s in self.stores.values_mut() {
            *s = s.emptied();
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

    /// Starts over as [`Engine::reset`] does, from the durable tables of `cold` at its newest version and the volatile
    /// rows `carried` (none of a durable table's): the tables that can be tiered are (docs/design/DATABASE.md §7),
    /// their rows read from the cold side and memory keeping only what it does not hold yet; the others are loaded.
    pub fn reset_on(
        &mut self,
        carried: Instance,
        cold: Arc<dyn crate::cold::ColdTables>,
        resume: crate::cold::Resume,
    ) -> Result<(), EvalError> {
        if let Some(rel) = self
            .tierable
            .keys()
            .find(|r| carried.rels.get(r).is_some_and(|rows| !rows.is_empty()))
        {
            return Err(internal_error!("a reset onto the cold side carries rows of the tiered table {rel:?}").into());
        }
        // The durable tables the engine keeps in memory start from the cold side's rows.
        let tiering = if self.in_memory {
            BTreeMap::new()
        } else {
            self.tierable.clone()
        };
        let mut carried = carried;
        if let Some(v) = cold.version()? {
            for rel in cold.tables() {
                if tiering.contains_key(&rel) {
                    continue;
                }
                for row in cold.probe(rel, &[], &[], None, v)? {
                    carried.insert(rel, row);
                }
            }
        }
        self.reset(carried)?;
        let p = self.program.clone();
        let p = p.get();
        for rel in tiering.keys() {
            self.tiered.insert(*rel);
        }
        self.durable_in_memory = cold.tables().into_iter().filter(|r| !self.tiered.contains(r)).collect();
        // The durable views (a view's blob is one a row it reads holds: its rules make none), and the mixed relations
        // kept on the cold side for their durable rules' support.
        let (views, volatile_rules) = if self.in_memory {
            (BTreeSet::new(), BTreeSet::new())
        } else {
            let views = self.durable_views();
            let (mixed, volatile) = self.mixed_relations(&views);
            (views.union(&mixed).copied().collect(), volatile)
        };
        let defs = self.view_definitions(&views, &resume.statics)?;
        let ready = if views.is_empty() {
            false
        } else {
            cold.open_views(&defs, resume.catch_up.is_some())?
        };
        // Resuming: the views are as the cold side keeps them, caught up at the first tick; else they are built at
        // the first tick from all their tables' rows, as after any reset.
        let resuming = ready && resume.catch_up.is_some();
        for (rel, key) in tiering {
            self.stores.insert(
                StoreKey::Main(rel),
                Store::tiered(crate::store::Tiered::new(
                    rel,
                    cold.clone(),
                    key,
                    rel_holds_blobs(p, rel),
                    self.hot_rows,
                    false,
                    !resuming,
                )?),
            );
        }
        for rel in &views {
            self.stores.insert(
                StoreKey::Main(*rel),
                Store::tiered(crate::store::Tiered::new(
                    *rel,
                    cold.clone(),
                    Vec::new(),
                    rel_holds_blobs(p, *rel),
                    self.hot_rows,
                    true,
                    !resuming,
                )?),
            );
        }
        self.view_rules = p
            .rules
            .iter_enumerated()
            .filter(|(id, r)| {
                self.plans.contains_key(id) && views.contains(&r.head.rel) && !volatile_rules.contains(id)
            })
            .map(|(id, _)| id)
            .collect();
        self.view_sources = self
            .view_rules
            .iter()
            .filter_map(|id| p.rules.get(*id))
            .flat_map(|rule| {
                rule.body.lits.iter().filter_map(|l| match l {
                    Literal::Pos(a) | Literal::Neg(a) => Some(a.rel),
                    Literal::Lookup { rel, .. } => Some(*rel),
                    _ => None,
                })
            })
            .filter(|rel| self.tiered.contains(rel) || self.durable_in_memory.contains(rel))
            .collect();
        self.volatile_rules = volatile_rules;
        self.durable_aggs = self
            .view_rules
            .iter()
            .filter_map(|id| {
                let rule = p.rules.get(*id)?;
                let plan = self.plans.get(id)?;
                if !plan.aggregate {
                    return None;
                }
                agg_shape(rule).map(|s| (*id, s))
            })
            .collect();
        self.views = views;
        if resuming {
            // The static rows and facts are as before the restart: loaded without showing as a change.
            let wrap = |e: ExprError| to_eval(e, Tick(0), None);
            let mut statics: BTreeMap<StoreKey, BTreeSet<Row>> = BTreeMap::new();
            for (rel, row) in &resume.statics {
                statics.entry(StoreKey::Main(*rel)).or_default().insert(row.clone());
            }
            for (key, rows) in &statics {
                for row in rows {
                    self.store(*key)?.add(row.clone(), 1).map_err(wrap)?;
                }
            }
            for f in &p.facts {
                let row = f
                    .row
                    .iter()
                    .map(|c| {
                        p.consts
                            .get(*c)
                            .cloned()
                            .ok_or_else(|| internal_error!("unknown constant {c:?}"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.store(StoreKey::Main(f.rel))?
                    .add(Row::from(row), 1)
                    .map_err(wrap)?;
            }
            self.facts_loaded = true;
            self.inputs = statics;
            // The durable tables kept in memory start as the views' version had them (the WAL's changes since
            // undone): the catch-up shows the changes but the last, the first tick the last (as their pending change).
            if let Some(c) = &resume.catch_up {
                let base = self.baseline.clone().unwrap_or_default();
                for rel in self.durable_in_memory.clone() {
                    let mut rows: BTreeSet<Row> = base.rows(rel).cloned().collect();
                    for changes in [&c.last, &c.before] {
                        for row in changes.inserted.get(&rel).into_iter().flatten() {
                            rows.remove(row);
                        }
                        for row in changes.deleted.get(&rel).into_iter().flatten() {
                            rows.insert(row.clone());
                        }
                    }
                    for row in rows {
                        self.store(StoreKey::Main(rel))?.add(row, 1).map_err(wrap)?;
                    }
                    self.pending.inserted.remove(&rel);
                    self.pending.deleted.remove(&rel);
                    if let Some(rows) = c.last.inserted.get(&rel) {
                        self.pending.inserted.insert(rel, rows.clone());
                    }
                    if let Some(rows) = c.last.deleted.get(&rel) {
                        self.pending.deleted.insert(rel, rows.clone());
                    }
                }
            }
            self.settle(Tick(0))?;
            self.stores.clear_deltas();
            self.boot = resume.catch_up;
            self.rebuilding = true;
        }
        Ok(())
    }

    /// Each durable view's definition hash (docs/design/DATABASE.md §8): its rules, and what they read (a view's
    /// definition, a table's schema, a static relation's rows), so another program's view or other statics name
    /// another keyspace.
    fn view_definitions(
        &self,
        views: &BTreeSet<RelId>,
        statics: &[(RelId, Row)],
    ) -> Result<Vec<(RelId, [u8; 32])>, EvalError> {
        let p = self.program.get();
        let mut done: BTreeMap<RelId, [u8; 32]> = BTreeMap::new();
        let mut pending: Vec<RelId> = views.iter().copied().collect();
        let mut guard = 0usize;
        while let Some(rel) = pending.pop() {
            guard += 1;
            if guard > views.len().saturating_mul(views.len()).saturating_add(64) {
                return Err(internal_error!("the durable views' definitions do not order").into());
            }
            if done.contains_key(&rel) {
                continue;
            }
            let rules: Vec<&Rule> = p
                .rules
                .iter_enumerated()
                .filter(|(id, r)| r.head.rel == rel && self.plans.contains_key(id))
                .map(|(_, r)| r)
                .collect();
            let mut reads: BTreeSet<RelId> = BTreeSet::new();
            for r in &rules {
                for l in &r.body.lits {
                    match l {
                        Literal::Pos(a) | Literal::Neg(a) => {
                            reads.insert(a.rel);
                        }
                        Literal::Lookup { rel, .. } => {
                            reads.insert(*rel);
                        }
                        _ => {}
                    }
                }
            }
            let waiting: Vec<RelId> = reads
                .iter()
                .copied()
                .filter(|r| views.contains(r) && !done.contains_key(r) && *r != rel)
                .collect();
            if !waiting.is_empty() {
                pending.push(rel);
                pending.extend(waiting);
                continue;
            }
            let mut h = blake3::Hasher::new();
            let mut put = |bytes: &[u8]| {
                h.update(&(bytes.len() as u64).to_be_bytes());
                h.update(bytes);
            };
            let decl = p
                .rels
                .get(rel)
                .ok_or_else(|| internal_error!("durable view {rel:?} is not declared"))?;
            put(decl.name.to_string().as_bytes());
            put(format!("{:?}", decl.schema).as_bytes());
            let mut texts: Vec<String> = rules.iter().map(|r| blossom_ir::printer::rule_text(p, r)).collect();
            texts.sort();
            for t in &texts {
                put(t.as_bytes());
            }
            for r in &reads {
                let d = p
                    .rels
                    .get(*r)
                    .ok_or_else(|| internal_error!("relation {r:?} is not declared"))?;
                put(d.name.to_string().as_bytes());
                put(format!("{:?}", d.schema).as_bytes());
                if let Some(def) = done.get(r) {
                    put(def);
                }
                if d.class == RelClass::Static {
                    let mut rows: Vec<String> = statics
                        .iter()
                        .filter(|(sr, _)| sr == r)
                        .map(|(_, row)| format!("{row:?}"))
                        .chain(p.facts.iter().filter(|f| f.rel == *r).map(|f| format!("{:?}", f.row)))
                        .collect();
                    rows.sort();
                    for row in &rows {
                        put(row.as_bytes());
                    }
                }
            }
            done.insert(rel, *h.finalize().as_bytes());
        }
        Ok(views.iter().filter_map(|v| done.get(v).map(|d| (*v, *d))).collect())
    }

    /// The derived relations that are functions of the tiered tables alone (docs/design/DATABASE.md §8): each rule
    /// defining one is a deductive rule kept by delta queries (no aggregate, no recursion, no reading of time), whose
    /// atoms read tiered tables, static relations or other such relations; no fact holds a row of one, and no other
    /// rule writes one. Their rows can be kept in the database with the tables', and are the same after a restart.
    pub fn durable_views(&self) -> BTreeSet<RelId> {
        let p = self.program.get();
        let recursive: BTreeSet<RelId> = self
            .strata
            .iter()
            .filter(|s| s.recursive)
            .flat_map(|s| s.rules.iter().chain(&s.aggregates))
            .filter_map(|id| p.rules.get(*id).map(|r| r.head.rel))
            .collect();
        let facts: BTreeSet<RelId> = p.facts.iter().map(|f| f.rel).collect();
        let mut by_head: BTreeMap<RelId, Vec<RuleId>> = BTreeMap::new();
        for (id, rule) in p.rules.iter_enumerated() {
            if self.plans.contains_key(&id) {
                by_head.entry(rule.head.rel).or_default().push(id);
            }
        }
        let pure: std::cell::RefCell<BTreeMap<blossom_base::FnId, bool>> = std::cell::RefCell::new(BTreeMap::new());
        let mut views: BTreeSet<RelId> = by_head
            .iter()
            .filter(|(rel, ids)| {
                p.rels
                    .get(**rel)
                    .is_some_and(|d| d.class == RelClass::Idb && !d.durable)
                    && !self.scoped.contains(rel)
                    && !recursive.contains(rel)
                    && !facts.contains(rel)
                    && ids.iter().all(|id| {
                        let (Some(rule), Some(plan)) = (p.rules.get(*id), self.plans.get(id)) else {
                            return false;
                        };
                        rule.kind == RuleKind::Deductive
                            && plan.regime == Regime::Delta
                            && (!plan.aggregate || (ids.len() == 1 && agg_shape(rule).is_some()))
                            && plan.frame.is_none()
                            && crate::purity::rule_is_pure(p, rule, &mut pure.borrow_mut())
                    })
            })
            .map(|(rel, _)| *rel)
            .collect();
        let source = |rel: &RelId, views: &BTreeSet<RelId>| {
            self.tiered.contains(rel)
                || self.durable_in_memory.contains(rel)
                || views.contains(rel)
                || p.rels.get(*rel).is_some_and(|d| d.class == RelClass::Static)
        };
        loop {
            let reads_other = |rel: &RelId| {
                by_head.get(rel).into_iter().flatten().any(|id| {
                    p.rules.get(*id).is_none_or(|rule| {
                        rule.body.lits.iter().any(|l| match l {
                            Literal::Pos(a) | Literal::Neg(a) => a.sender.is_some() || !source(&a.rel, &views),
                            Literal::Lookup { rel, .. } => !source(rel, &views),
                            _ => false,
                        })
                    })
                })
            };
            let out: Vec<RelId> = views.iter().copied().filter(|r| reads_other(r)).collect();
            if out.is_empty() {
                return views;
            }
            for r in out {
                views.remove(&r);
            }
        }
    }

    /// A resume's catch-up of the durable views, before the first tick (docs/design/DATABASE.md §8): the tiered tables
    /// read as of the tick before the last released one (its change undone in their overlays), showing the net
    /// change since the views' version, and only the views' rules run; then the last tick's change is carried, to
    /// show as the first tick's change. The views' changes are the first tick's.
    fn catch_up(&mut self, input: &StepInput<'_>, c: &blossom_ir::tick::CatchUp) -> Result<(), EvalError> {
        let program = self.program.clone();
        let p = program.get();
        self.stores.clear_deltas();
        let empty = Vec::new();
        for rel in self.tiered.clone() {
            let store = self.store(StoreKey::Main(rel))?;
            for row in c.last.inserted.get(&rel).unwrap_or(&empty) {
                store.rewind(row, false)?;
            }
            for row in c.last.deleted.get(&rel).unwrap_or(&empty) {
                store.rewind(row, true)?;
            }
            let ins: BTreeSet<Row> = c.before.inserted.get(&rel).unwrap_or(&empty).iter().cloned().collect();
            let del: BTreeSet<Row> = c.before.deleted.get(&rel).unwrap_or(&empty).iter().cloned().collect();
            store.set_change(ins, del);
        }
        let wrap = |e: ExprError| to_eval(e, input.tick, None);
        for rel in self.durable_in_memory.clone() {
            for row in c.before.deleted.get(&rel).unwrap_or(&empty) {
                self.store(StoreKey::Main(rel))?.add(row.clone(), -1).map_err(wrap)?;
            }
            for row in c.before.inserted.get(&rel).unwrap_or(&empty) {
                self.store(StoreKey::Main(rel))?.add(row.clone(), 1).map_err(wrap)?;
            }
        }
        self.settle(input.tick)?;
        for rel in self.views.clone() {
            self.store(StoreKey::Main(rel))?.begin_tick(input.tick.0)?;
        }
        let strata = self.strata.clone();
        for s in strata.iter() {
            // As at any tick: a stratum's aggregates first, then its rules.
            for id in &s.aggregates {
                if self.view_rules.contains(id) {
                    self.maintain(p, input, *id)?;
                }
            }
            self.settle(input.tick)?;
            for id in &s.rules {
                if self.view_rules.contains(id) {
                    self.maintain(p, input, *id)?;
                }
            }
            self.settle(input.tick)?;
        }
        for rel in self.tiered.clone() {
            let store = self.store(StoreKey::Main(rel))?;
            for row in c.last.deleted.get(&rel).unwrap_or(&empty) {
                store.carry(row, false, c.last_tick)?;
            }
            for row in c.last.inserted.get(&rel).unwrap_or(&empty) {
                store.carry(row, true, c.last_tick)?;
            }
        }
        self.stores.clear_deltas();
        Ok(())
    }

    /// Keeps the tiered stores' hot tiers within the engine's budget together: past it, the largest gives up half,
    /// the least recently used first, until they fit.
    fn balance_hot(&mut self) {
        let keys: Vec<StoreKey> = self
            .tiered
            .iter()
            .chain(&self.views)
            .map(|r| StoreKey::Main(*r))
            .collect();
        loop {
            let sizes: Vec<(usize, StoreKey)> = keys
                .iter()
                .filter_map(|k| self.stores.get(k).map(|s| (s.hot_rows(), *k)))
                .collect();
            let total: usize = sizes.iter().map(|(n, _)| n).sum();
            if total <= self.hot_rows {
                return;
            }
            let Some((largest, key)) = sizes.into_iter().max_by_key(|(n, _)| *n) else {
                return;
            };
            if largest == 0 {
                return;
            }
            let Some(s) = self.stores.get_mut(&key) else {
                return;
            };
            s.shrink_hot(largest / 2);
            if s.hot_rows() >= largest {
                // Nothing more to give up.
                return;
            }
        }
    }

    /// The relations mixing durable rules (as a durable view's: deductive by delta queries, pure, reading durable
    /// sources and `views`) with others (deductive, no aggregate or recursion), and those other rules: their durable
    /// support can be kept on the cold side, the rest in memory (DATABASE.md §8). Not sources of durable views: part of
    /// them is lost on a restart.
    fn mixed_relations(&self, views: &BTreeSet<RelId>) -> (BTreeSet<RelId>, BTreeSet<RuleId>) {
        let p = self.program.get();
        let recursive: BTreeSet<RelId> = self
            .strata
            .iter()
            .filter(|s| s.recursive)
            .flat_map(|s| s.rules.iter().chain(&s.aggregates))
            .filter_map(|id| p.rules.get(*id).map(|r| r.head.rel))
            .collect();
        let facts: BTreeSet<RelId> = p.facts.iter().map(|f| f.rel).collect();
        let source = |rel: &RelId| {
            self.tiered.contains(rel)
                || self.durable_in_memory.contains(rel)
                || views.contains(rel)
                || p.rels.get(*rel).is_some_and(|d| d.class == RelClass::Static)
        };
        let mut memo = BTreeMap::new();
        let mut by_head: BTreeMap<RelId, Vec<RuleId>> = BTreeMap::new();
        for (id, rule) in p.rules.iter_enumerated() {
            if self.plans.contains_key(&id) {
                by_head.entry(rule.head.rel).or_default().push(id);
            }
        }
        let (mut mixed, mut volatile) = (BTreeSet::new(), BTreeSet::new());
        for (rel, ids) in by_head {
            let candidate = p.rels.get(rel).is_some_and(|d| d.class == RelClass::Idb && !d.durable)
                && !views.contains(&rel)
                && !self.scoped.contains(&rel)
                && !recursive.contains(&rel)
                && !facts.contains(&rel)
                && ids.iter().all(|id| {
                    let (Some(rule), Some(plan)) = (p.rules.get(*id), self.plans.get(id)) else {
                        return false;
                    };
                    rule.kind == RuleKind::Deductive && !plan.aggregate && plan.frame.is_none()
                });
            if !candidate {
                continue;
            }
            let durable: Vec<bool> = ids
                .iter()
                .map(|id| {
                    let (Some(rule), Some(plan)) = (p.rules.get(*id), self.plans.get(id)) else {
                        return false;
                    };
                    plan.regime == Regime::Delta
                        && crate::purity::rule_is_pure(p, rule, &mut memo)
                        && rule.body.lits.iter().all(|l| match l {
                            Literal::Pos(a) | Literal::Neg(a) => a.sender.is_none() && source(&a.rel),
                            Literal::Lookup { rel, .. } => source(rel),
                            _ => true,
                        })
                })
                .collect();
            if durable.iter().any(|d| *d) {
                mixed.insert(rel);
                volatile.extend(ids.iter().zip(&durable).filter(|(_, d)| !**d).map(|(id, _)| *id));
            }
        }
        (mixed, volatile)
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
            return Err(
                internal_error!("an engine for node {} ran a tick of node {}", self.node.0, input.node.0).into(),
            );
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
        self.tick_work.clear();
        let wrap = |e: ExprError| to_eval(e, tick, None);
        // 1. What changes.
        self.stores.clear_deltas();
        // A resume's catch-up of the durable views runs first (DATABASE.md §8).
        if let Some(catch_up) = self.boot.take() {
            self.catch_up(input, &catch_up)?;
        }
        // A tiered table took the last tick's change at that tick's end: it shows as this tick's change.
        for rel in self.tiered.iter().chain(&self.views).copied().collect::<Vec<_>>() {
            self.store(StoreKey::Main(rel))?.begin_tick(input.tick.0)?;
        }
        let pending = std::mem::take(&mut self.pending);
        for (rel, rows) in &pending.deleted {
            if self.tiered.contains(rel) {
                continue;
            }
            for r in rows {
                self.store(StoreKey::Main(*rel))?.add(r.clone(), -1).map_err(wrap)?;
            }
        }
        for (rel, rows) in &pending.inserted {
            if self.tiered.contains(rel) {
                continue;
            }
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
                    .map(|c| {
                        p.consts
                            .get(*c)
                            .cloned()
                            .ok_or_else(|| internal_error!("unknown constant {c:?}"))
                    })
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
            now_inputs
                .entry(StoreKey::Main(d.rel))
                .or_default()
                .insert(d.row.clone());
            let mut with_sender = d.row.to_vec();
            with_sender.push(self.shared.members.value(d.from));
            now_inputs
                .entry(StoreKey::Sent(d.rel))
                .or_default()
                .insert(Row::from(with_sender));
        }
        for g in input.ingress {
            now_inputs
                .entry(StoreKey::Main(g.rel))
                .or_default()
                .insert(g.row.clone());
            let mut with_sender = g.row.to_vec();
            with_sender.push(Value::Session(g.session));
            now_inputs
                .entry(StoreKey::Sent(g.rel))
                .or_default()
                .insert(Row::from(with_sender));
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
        // The derived tick-scoped relations start every tick empty: their rules derive this tick's rows afresh.
        for rel in self.scoped.clone() {
            self.store(StoreKey::Main(rel))?.retract_all().map_err(wrap)?;
        }
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
            let (ins, del) = match self.framed.get(&rel) {
                Some(frame) => self.framed_change(frame, &plan.head)?,
                None => {
                    let s = self
                        .stores
                        .get(&plan.head)
                        .ok_or_else(|| internal_error!("no next store"))?;
                    (s.ins.iter().cloned().collect(), s.del.iter().cloned().collect())
                }
            };
            if !ins.is_empty() {
                changes.inserted.insert(rel, ins);
            }
            if !del.is_empty() {
                changes.deleted.insert(rel, del);
            }
        }
        if let Some(base) = self.baseline.take() {
            // The first tick after a reset: the change is relative to the carried state it started from (a tiered
            // table's started from the cold side, as its stores did).
            let next = self.next_instance(false)?;
            let mut fresh = Changes::between(&base, &next);
            for rel in &self.tiered {
                if let Some(rows) = changes.inserted.remove(rel) {
                    fresh.inserted.insert(*rel, rows);
                }
                if let Some(rows) = changes.deleted.remove(rel) {
                    fresh.deleted.insert(*rel, rows);
                }
            }
            changes = fresh;
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
            let s = self
                .stores
                .get(&plan.head)
                .ok_or_else(|| internal_error!("no async store"))?;
            let to_host = matches!(
                self.program.get().rels.get(rel).map(|r| &r.class),
                Some(RelClass::HostOut(_))
            );
            for row in s.present()? {
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
                    Some(Value::Member(m)) => {
                        let to = self.shared.members.id(m).ok_or_else(|| {
                            EvalError::NoMember(blossom_ir::members::member_name(self.program.get(), m))
                        })?;
                        out.outbox.insert(Send {
                            rel,
                            to,
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
                .map(Store::present_sorted)
                .transpose()?
                .unwrap_or_default();
            out.observed.insert(*rel, rows);
        }
        // The durable views' changes (DATABASE.md §8), and how their sources' rows at the tick differ from those
        // carried in: the views' rows were computed from them.
        for rel in self.views.clone() {
            let changes = self.store(StoreKey::Main(rel))?.take_changes()?;
            if !changes.is_empty() {
                out.views.insert(rel, changes);
            }
        }
        for rel in self.view_sources.clone() {
            let (shown, hidden) = self.written(rel, &pending)?;
            if !shown.is_empty() {
                out.written.inserted.insert(rel, shown);
            }
            if !hidden.is_empty() {
                out.written.deleted.insert(rel, hidden);
            }
        }
        self.rebuilding = false;
        self.balance_hot();
        // A tiered table takes its change now: its rows are the next tick's from here on.
        for rel in self.tiered.clone() {
            let store = self.store(StoreKey::Main(rel))?;
            for r in out.changes.deleted.get(&rel).into_iter().flatten() {
                store.carry(r, false, tick.0)?;
            }
            for r in out.changes.inserted.get(&rel).into_iter().flatten() {
                store.carry(r, true, tick.0)?;
            }
        }
        Ok(out)
    }

    /// How a durable table's rows at the tick's end differ from those carried into the tick (`pending`: the change
    /// the tick started with, the carry of a table kept in memory): the rows its rules wrote this tick that were not
    /// carried, and the carried rows not present (a lattice's, merged past). A tiered table's carried rows stay.
    fn written(&self, rel: RelId, pending: &Changes) -> Result<(Vec<Row>, Vec<Row>), EvalError> {
        let s = self
            .stores
            .get(&StoreKey::Main(rel))
            .ok_or_else(|| internal_error!("no store for the durable table {rel:?}"))?;
        if self.tiered.contains(&rel) {
            return Ok((s.uncarried(), Vec::new()));
        }
        // A table kept in memory: its change since the tick began, against the carry applied at its start.
        let (pins, pdel): (BTreeSet<&Row>, BTreeSet<&Row>) = (
            pending.inserted.get(&rel).into_iter().flatten().collect(),
            pending.deleted.get(&rel).into_iter().flatten().collect(),
        );
        let mut shown: Vec<Row> = s.ins.iter().filter(|r| !pins.contains(r)).cloned().collect();
        let mut hidden: Vec<Row> = s.del.iter().filter(|r| !pdel.contains(r)).cloned().collect();
        for r in &pdel {
            if s.contains(r)? {
                shown.push((*r).clone());
            }
        }
        for r in &pins {
            if !s.contains(r)? {
                hidden.push((*r).clone());
            }
        }
        shown.sort();
        hidden.sort();
        Ok((shown, hidden))
    }

    /// The next tick's carried state, with the tiered tables' rows (`tiered`: read from the cold side, after the
    /// tick's end) or without them.
    fn next_instance(&self, tiered: bool) -> Result<Instance, EvalError> {
        let mut next = Instance::default();
        for (key, s) in self.stores.iter() {
            if let StoreKey::Next(rel) = key {
                if self.tiered.contains(&rel) {
                    if tiered {
                        for r in self.tiered_rows(rel)? {
                            next.insert(rel, r);
                        }
                    }
                    continue;
                }
                for r in s.present()? {
                    next.insert(rel, r.clone());
                }
                if let Some(frame) = self.framed.get(&rel) {
                    for r in self.frame_rows(frame)? {
                        next.insert(rel, r);
                    }
                }
            }
        }
        Ok(next)
    }

    /// A tiered table's rows: after a tick's end, the next tick's.
    fn tiered_rows(&self, rel: RelId) -> Result<Vec<Row>, EvalError> {
        self.stores
            .get(&StoreKey::Main(rel))
            .ok_or_else(|| internal_error!("no store for the tiered table {rel:?}"))?
            .present_sorted()
    }

    /// The stores a table's frame reads.
    fn frame_stores(&self, frame: &rule::FramePlan) -> Result<(&Store, &Store, Option<&Store>), EvalError> {
        let store = |k: &StoreKey| {
            self.stores
                .get(k)
                .ok_or_else(|| EvalError::from(internal_error!("no store for {k:?}")))
        };
        Ok((
            store(&frame.rel)?,
            store(&frame.del)?,
            frame.keep.as_ref().map(store).transpose()?,
        ))
    }

    /// The rows a framed table's frame carries into the next state: its present rows not deleted, and kept.
    fn frame_rows(&self, frame: &rule::FramePlan) -> Result<Vec<Row>, EvalError> {
        let (rel, del, keep) = self.frame_stores(frame)?;
        let mut out = Vec::new();
        for r in rel.present()? {
            if frame_holds(rel, del, keep, r, false)? {
                out.push(r.clone());
            }
        }
        Ok(out)
    }

    /// How the tick changed a framed table's next state (`Engine::framed`): the rows it gained and those it lost. A
    /// row is in the next state while the frame carries it or another next-state rule supports it (its `Next` store,
    /// `next`), so it can change only where it changed in a store the frame reads or in `next`; its membership at the
    /// start of the tick reads each store as it was then.
    fn framed_change(&self, frame: &rule::FramePlan, next: &StoreKey) -> Result<(Vec<Row>, Vec<Row>), EvalError> {
        let (rel, del, keep) = self.frame_stores(frame)?;
        let next = self
            .stores
            .get(next)
            .ok_or_else(|| internal_error!("no store for {next:?}"))?;
        let mut rows: BTreeSet<&Row> = rel.delta().map(|(r, _)| r).collect();
        rows.extend(del.delta().map(|(r, _)| r));
        if let Some(k) = keep {
            rows.extend(k.delta().map(|(r, _)| r));
        }
        rows.extend(next.delta().map(|(r, _)| r));
        let (mut ins, mut gone) = (Vec::new(), Vec::new());
        for row in rows {
            // A tiered table's next state as the last tick left it is its carry (the first tick after a reset that
            // showed all its rows as new: what the cold side held, as the baseline does for the other relations).
            let was = match rel.carried_in(row)? {
                Some(carried) => carried,
                None => frame_holds(rel, del, keep, row, true)? || next.contained(row)?,
            };
            let is = frame_holds(rel, del, keep, row, false)? || next.contains(row)?;
            match (was, is) {
                (false, true) => ins.push(row.clone()),
                (true, false) => gone.push(row.clone()),
                _ => {}
            }
        }
        Ok((ins, gone))
    }

    /// The whole carried state: what the next tick starts from (O(state)).
    pub fn carried_instance(&self) -> Result<Instance, EvalError> {
        match &self.baseline {
            Some(base) => {
                let mut carried = base.clone();
                for rel in &self.tiered {
                    for r in self.tiered_rows(*rel)? {
                        carried.insert(*rel, r);
                    }
                }
                Ok(carried)
            }
            None => self.next_instance(true),
        }
    }

    /// How the last tick changed `rel`'s rows: those it gained and those it lost, against the tick before (O(the
    /// change); empty before the first tick or for a relation the program never fills).
    pub fn changes_of(&self, rel: RelId) -> (Vec<Row>, Vec<Row>) {
        self.stores
            .get(&StoreKey::Main(rel))
            .map(|s| (s.ins.iter().cloned().collect(), s.del.iter().cloned().collect()))
            .unwrap_or_default()
    }

    /// The carried rows of `rel`: what the next tick starts from.
    pub fn carried_rows(&self, rel: RelId) -> Result<Vec<Row>, EvalError> {
        if self.tiered.contains(&rel) {
            return self.tiered_rows(rel);
        }
        if let Some(base) = &self.baseline {
            return Ok(base.rows(rel).cloned().collect());
        }
        let mut rows = self
            .stores
            .get(&StoreKey::Next(rel))
            .map(Store::present_sorted)
            .transpose()?
            .unwrap_or_default();
        if let Some(frame) = self.framed.get(&rel) {
            rows.extend(self.frame_rows(frame)?);
            rows.sort_unstable();
            rows.dedup();
        }
        Ok(rows)
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
            let plan = self
                .plans
                .get(&id)
                .ok_or_else(|| internal_error!("rule {id:?} has no plan"))?;
            // A framed table's frame writes nothing: its part of the next state is read where it is wanted.
            if plan.frame.is_some() && self.framed.contains_key(&rule.head.rel) {
                return Ok(());
            }
            if plan.regime == Regime::Scoped {
                // No row in a scoped atom: no valuation, and the head was emptied at the start of the tick.
                let live = plan
                    .scoped
                    .iter()
                    .any(|k| self.stores.get(k).is_some_and(|s| s.present_len() > 0));
                if !live {
                    return Ok(());
                }
            }
            let unchanged = !plan.dep_keys.iter().any(|k| self.stores.changed(k));
            let rebuild = self.rebuilding && !self.view_rules.contains(&id);
            if plan.regime == Regime::Delta && unchanged && !rebuild {
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
        let plan = self
            .plans
            .get(&id)
            .ok_or_else(|| internal_error!("rule {id:?} has no plan"))?
            .clone();
        match plan.regime {
            Regime::Recompute => self.recompute_rule(p, input, rule, &plan),
            Regime::Scoped => {
                let terms = self.evaluate(p, input, rule, &plan, true)?;
                self.count(rule.id, terms.examined, terms.steps);
                self.apply(p, input, rule, &plan, terms)
            }
            Regime::Delta => {
                // The first tick after a resume derives what memory lost in full (the views it reads show only the
                // tick's change).
                let full = self.rebuilding && !self.view_rules.contains(&id);
                let terms = self.evaluate(p, input, rule, &plan, full)?;
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
                let Some(key) = rule.body.lits.get(lit).and_then(dep_store) else {
                    continue;
                };
                let store = self
                    .stores
                    .get(&key)
                    .ok_or_else(|| internal_error!("no store for {key:?}"))?;
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
            let was = frame_holds(rel, del, keep, row, true)?;
            let is = frame_holds(rel, del, keep, row, false)?;
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
        // The join order of each driver's terms, from the stores as they are now.
        let cost = |lit: usize, cols: &[usize], range: bool| -> usize {
            let Some(Literal::Pos(a)) = rule.body.lits.get(lit) else {
                return usize::MAX;
            };
            self.stores
                .get(&rule::atom_store(a))
                .map_or(0, |s| s.estimate_probe(cols, range))
        };
        // The orders are kept while every store stays within its power of two; stores below `SMALL_STORE` rows count
        // as one size (they flip between a few rows from tick to tick, and any order joins them cheaply).
        let sizes: Vec<u64> = plan
            .atoms
            .iter()
            .map(|lit| match rule.body.lits.get(*lit) {
                Some(Literal::Pos(a)) => self
                    .stores
                    .get(&rule::atom_store(a))
                    .map_or(0, |s| s.size_class(SMALL_STORE)),
                _ => 0,
            })
            .collect();
        // A rule has few drivers' literals: a short list, searched.
        let mut orders: Vec<(Option<usize>, Arc<rule::Order>)> = Vec::new();
        for (driver, _) in &drivers {
            let lit = driver.lit();
            if orders.iter().any(|(l, _)| *l == lit) {
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
            orders.push((lit, order));
        }
        let mut buffers = self
            .buffers
            .try_borrow_mut()
            .map_err(|_| internal_error!("a rule evaluation inside another"))?;
        for (driver, pos) in drivers {
            let order = orders
                .iter()
                .find(|(l, _)| *l == driver.lit())
                .map(|(_, o)| o)
                .ok_or_else(|| internal_error!("no join order for driver {:?}", driver.lit()))?;
            let old = |lit: usize| plan.dep_pos.get(lit).copied().flatten().is_some_and(|q| q > pos);
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
    fn apply(
        &mut self,
        p: &Program,
        input: &StepInput<'_>,
        rule: &Rule,
        plan: &Plan,
        terms: Terms,
    ) -> Result<(), EvalError> {
        let tick = input.tick;
        if let Some((_, (_, Some(e)))) = terms.errors.into_iter().find(|(_, (n, _))| *n > 0) {
            return Err(to_eval(e, tick, Some(rule)));
        }
        if let Some(shape) = self.durable_aggs.get(&rule.id).cloned() {
            return self.apply_durable_aggregate(p, rule, plan, &shape, terms.aggs, tick);
        }
        if plan.aggregate {
            return self.apply_aggregates(p, rule, plan, terms.aggs, tick);
        }
        let volatile = self.volatile_rules.contains(&rule.id);
        let store = self.store(plan.head)?;
        let mut writes = 0u64;
        for (row, w) in terms.heads {
            if w != 0 {
                writes += 1;
            }
            store
                .add_by(row, w, volatile)
                .map_err(|e| to_eval(e, tick, Some(rule)))?;
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
            let slot = g
                .tuples
                .get_mut(col)
                .ok_or_else(|| internal_error!("aggregate column {col} out of range"))?;
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

    /// A durable view's aggregate (DATABASE.md §8): each touched group's row from its source as it is now, without
    /// the groups' tuples in memory. A `max` or `min` moves by the tuples that came, and reads its group only when its
    /// current value went; the others read their group.
    fn apply_durable_aggregate(
        &mut self,
        p: &Program,
        rule: &Rule,
        plan: &Plan,
        shape: &AggShape,
        changes: BTreeMap<(Vec<Value>, usize, Vec<Value>), i64>,
        tick: Tick,
    ) -> Result<(), EvalError> {
        let wrap = |e: ExprError| to_eval(e, tick, Some(rule));
        let mut by_group: BTreeMap<Vec<Value>, Vec<(Vec<Value>, i64)>> = BTreeMap::new();
        for ((group, _, tuple), w) in changes {
            if w != 0 {
                by_group.entry(group).or_default().push((tuple, w));
            }
        }
        let src = StoreKey::Main(shape.src);
        let mut edits: Vec<(Option<Row>, Option<Row>)> = Vec::new();
        for (group, tuples) in by_group {
            let head = self
                .stores
                .get(&plan.head)
                .ok_or_else(|| internal_error!("no store for {:?}", plan.head))?;
            let current = head.new_rows(&shape.head_group, &group)?.into_iter().next();
            let source = self
                .stores
                .get(&src)
                .ok_or_else(|| internal_error!("no store for {src:?}"))?;
            let mut probe_cols = shape.src_group.clone();
            probe_cols.extend(&shape.src_tuple);
            let present = |old: bool, t: &[Value]| -> Result<bool, EvalError> {
                let mut values = group.clone();
                values.extend_from_slice(t);
                source.any(old, &probe_cols, &values)
            };
            let incremental = matches!(shape.func, AggFunc::Max | AggFunc::Min);
            let new = match (&current, incremental) {
                (Some(cur), true) => {
                    let value = cur
                        .get(shape.head_agg)
                        .ok_or_else(|| internal_error!("an aggregate's row without its value"))?;
                    let mut went = false;
                    let mut best: Option<Value> = None;
                    for (t, w) in &tuples {
                        let v = t.first().ok_or_else(|| internal_error!("an empty aggregate tuple"))?;
                        if *w < 0 && v == value && !present(false, t)? {
                            went = true;
                        }
                        if *w > 0 {
                            let better = best.as_ref().is_none_or(|b| match shape.func {
                                AggFunc::Max => v > b,
                                _ => v < b,
                            });
                            if better {
                                best = Some(v.clone());
                            }
                        }
                    }
                    if went {
                        self.group_from_source(p, rule, shape, &group, tick)?
                    } else {
                        match best {
                            Some(b)
                                if match shape.func {
                                    AggFunc::Max => b > *value,
                                    _ => b < *value,
                                } =>
                            {
                                let mut row = cur.to_vec();
                                if let Some(slot) = row.get_mut(shape.head_agg) {
                                    *slot = b;
                                }
                                Some(Row::from(row))
                            }
                            _ => Some(cur.clone()),
                        }
                    }
                }
                _ => self.group_from_source(p, rule, shape, &group, tick)?,
            };
            if new != current {
                edits.push((current, new));
            }
        }
        let store = self.store(plan.head)?;
        let mut writes = 0u64;
        for (old, new) in edits {
            if let Some(o) = old {
                writes += 1;
                store.add(o, -1).map_err(wrap)?;
            }
            if let Some(n) = new {
                writes += 1;
                store.add(n, 1).map_err(wrap)?;
            }
        }
        self.count_writes(rule.id, writes);
        Ok(())
    }

    /// A group's aggregate row from its source's rows now (`None`: the group has none).
    fn group_from_source(
        &self,
        p: &Program,
        rule: &Rule,
        shape: &AggShape,
        group: &[Value],
        tick: Tick,
    ) -> Result<Option<Row>, EvalError> {
        let source = self
            .stores
            .get(&StoreKey::Main(shape.src))
            .ok_or_else(|| internal_error!("no store for {:?}", shape.src))?;
        let mut set: BTreeMap<Vec<Value>, i64> = BTreeMap::new();
        for row in source.new_rows(&shape.src_group, group)? {
            let tuple: Vec<Value> = shape
                .src_tuple
                .iter()
                .map(|c| {
                    row.get(*c)
                        .cloned()
                        .ok_or_else(|| internal_error!("a source row too short"))
                })
                .collect::<Result<_, _>>()?;
            set.insert(tuple, 1);
        }
        if set.is_empty() {
            return Ok(None);
        }
        group_row(p, rule, group, &[set])
            .map(Some)
            .map_err(|e| to_eval(e, tick, Some(rule)))
    }

    /// A recompute rule: evaluated in full, its change is the difference from its last output.
    fn recompute_rule(
        &mut self,
        p: &Program,
        input: &StepInput<'_>,
        rule: &Rule,
        plan: &Plan,
    ) -> Result<(), EvalError> {
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
                rows.insert(
                    group_row(p, rule, &group, &tuples).map_err(|e| to_eval(e, tick, Some(rule)))?,
                    1,
                );
            }
            rows
        } else {
            terms.heads.into_iter().filter(|(_, w)| *w > 0).collect()
        };
        // Only the difference from the last output is applied: a row in both, with the same support, is left alone
        // (retracting and re-adding it would change nothing but rebuild its index entries and touch the store).
        let old = self.prev.remove(&rule.id).unwrap_or_default();
        let volatile = self.volatile_rules.contains(&rule.id);
        let store = self.store(plan.head)?;
        let mut writes = 0u64;
        for (row, w) in &old {
            let d = new.get(row).copied().unwrap_or(0) - w;
            if d != 0 {
                writes += 1;
                store
                    .add_by(row.clone(), d, volatile)
                    .map_err(|e| to_eval(e, tick, Some(rule)))?;
            }
        }
        for (row, w) in &new {
            if !old.contains_key(row) {
                writes += 1;
                store
                    .add_by(row.clone(), *w, volatile)
                    .map_err(|e| to_eval(e, tick, Some(rule)))?;
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
        let mut dirty = self.rebuilding;
        for id in &ids {
            let plan = self
                .plans
                .get(id)
                .ok_or_else(|| internal_error!("rule {id:?} has no plan"))?;
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
            let head = self
                .plans
                .get(id)
                .map(|pl| pl.head)
                .ok_or_else(|| internal_error!("rule {id:?}"))?;
            let rule = p.rules.get(*id).ok_or_else(|| internal_error!("rule {id:?}"))?;
            let store = self.store(head)?;
            for (row, w) in old {
                store.add(row, -w).map_err(|e| to_eval(e, tick, Some(rule)))?;
            }
        }
        self.settle(tick)?;
        // The aggregates read only lower strata: once, first.
        for id in &s.aggregates {
            let plan = self
                .plans
                .get(id)
                .ok_or_else(|| internal_error!("rule {id:?}"))?
                .clone();
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
                let plan = self
                    .plans
                    .get(id)
                    .ok_or_else(|| internal_error!("rule {id:?}"))?
                    .clone();
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
                    self.store(plan.head)?
                        .add(row, 1)
                        .map_err(|e| to_eval(e, tick, Some(rule)))?;
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
        if heads
            .iter()
            .any(|h| self.stores.get(h).is_none_or(|st| st.cell.is_some()))
        {
            return false;
        }
        s.rules.iter().all(|id| {
            let (Some(rule), Some(plan)) = (p.rules.get(*id), self.plans.get(id)) else {
                return false;
            };
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
                let Some(id) = s.rules.iter().find(|id| raised.contains(id)) else {
                    break;
                };
                let plan = self
                    .plans
                    .get(id)
                    .ok_or_else(|| internal_error!("rule {id:?}"))?
                    .clone();
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
                let plan = self
                    .plans
                    .get(id)
                    .ok_or_else(|| internal_error!("rule {id:?}"))?
                    .clone();
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
                        let fresh = !store.contains(&row)?;
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
            let Some(store) = self.stores.get(&StoreKey::Main(*rel)) else {
                continue;
            };
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
            let HeadMode::Violation { invariant } = rule.head.mode else {
                continue;
            };
            let Some(row) = self
                .stores
                .get(&StoreKey::Main(rule.head.rel))
                .map(Store::present)
                .transpose()?
                .and_then(|mut rows| rows.next().map(|first| rows.fold(first, std::cmp::min)))
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
        if !self.tiered.is_empty() {
            return Err(internal_error!("a whole-instance tick of an engine whose tables are tiered").into());
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
                for r in s.present()? {
                    instance.insert(rel, r.clone());
                }
            }
        }
        Ok(TickOutput {
            instance,
            next: self.next_instance(true)?,
            outbox: step.outbox,
            egress: step.egress,
            host: step.host,
            firings: Vec::new(),
            blobs: step.blobs,
        })
    }

    /// Whether a row of any store holds `b`: derived rows keep their blobs across ticks without re-creating them.
    /// Each store counts its rows' blobs as they change, so this is a lookup per store that can hold blobs.
    /// A tiered table holds only carried rows once a tick has ended, so it answers for none: the node counts the
    /// carried rows' blobs itself.
    pub fn holds_blob(&self, b: &blossom_value::BlobRef) -> bool {
        self.stores.values().any(|s| s.holds_blob(b))
    }

    /// The rows the engine's stores hold in memory now, all relations together: its state's size in rows. A table
    /// carried by its frame counts once (`Engine::framed`), not once for its rows and again for its next state; a
    /// tiered table counts only what memory holds of it.
    pub fn held_rows(&self) -> usize {
        self.stores.values().map(Store::resident_len).sum()
    }

    /// The rows each store holds in memory, largest first: the relation, which of its stores (`main`, `next`, `sent`,
    /// `async`; a tiered table's `main` counts what memory holds of it), and how many. For operators: what the state
    /// in memory is made of.
    pub fn resident_by_store(&self) -> Vec<(RelId, &'static str, usize)> {
        let mut out: Vec<(RelId, &'static str, usize)> = self
            .stores
            .iter()
            .map(|(key, s)| {
                let (rel, kind) = match key {
                    StoreKey::Main(r) if self.tiered.contains(&r) => (r, "tiered"),
                    StoreKey::Main(r) if self.views.contains(&r) => (r, "view"),
                    StoreKey::Main(r) => (r, "main"),
                    StoreKey::Next(r) => (r, "next"),
                    StoreKey::Sent(r) => (r, "sent"),
                    StoreKey::Async(r) => (r, "async"),
                };
                (rel, kind, s.resident_len())
            })
            .filter(|(_, _, n)| *n > 0)
            .collect();
        out.sort_by_key(|e| std::cmp::Reverse(e.2));
        out
    }

    /// Every relation's rows as the last tick left them (O(state), for checking against a reference): the stores of
    /// the relations at the tick, a durable view's from the cold side, and not a tiered table's (it holds the next
    /// tick's rows by then).
    pub fn instance_now(&self) -> Result<BTreeMap<RelId, BTreeSet<Row>>, EvalError> {
        let mut out = BTreeMap::new();
        for (key, s) in self.stores.iter() {
            let StoreKey::Main(rel) = key else { continue };
            if self.tiered.contains(&rel) {
                continue;
            }
            out.insert(rel, s.present_sorted()?.into_iter().collect());
        }
        Ok(out)
    }

    /// The work of each rule in the last tick (the rules that did any).
    pub fn last_tick_work(&self) -> &BTreeMap<RuleId, RuleWork> {
        &self.tick_work
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
            self.tick_work.entry(rule).or_default().writes += writes;
        }
    }

    fn count(&mut self, rule: RuleId, examined: u64, steps: u64) {
        self.examined += examined;
        let t = self.tick_work.entry(rule).or_default();
        t.rows += examined;
        t.steps += steps;
        t.evals += 1;
        let w = self.examined_by.entry(rule).or_default();
        w.rows += examined;
        w.steps += steps;
        w.evals += 1;
    }

    pub fn node(&self) -> NodeId {
        self.node
    }
}

/// A join order with the sizes of the stores it was chosen for, in powers of two (`Engine::orders`).
type CachedOrder = (Vec<u64>, Arc<rule::Order>);

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
/// A durable aggregate's shape (DATABASE.md §8): one aggregate over one source atom, its group and tuple read from
/// the atom's columns.
#[derive(Clone, Debug)]
pub(crate) struct AggShape {
    src: RelId,
    /// The source columns holding the group's values, in the head's order, and the aggregate's tuple's.
    src_group: Vec<usize>,
    src_tuple: Vec<usize>,
    /// The head's group columns and its aggregate column.
    head_group: Vec<usize>,
    head_agg: usize,
    func: AggFunc,
}

/// The shape of `rule` if it is an aggregate a durable view can keep without its groups' tuples: one positive atom
/// (no sender, no weight), group terms and the aggregate's tuple all variables of the atom, one `count`, `min`,
/// `max`, `sum` or `collect` with no order (a `min` or `max` of one value).
fn agg_shape(rule: &Rule) -> Option<AggShape> {
    let [Literal::Pos(atom)] = rule.body.lits.as_slice() else {
        return None;
    };
    if atom.sender.is_some() || atom.weight.is_some() {
        return None;
    }
    let col_of = |t: &blossom_ir::core::Term| -> Option<usize> {
        let blossom_ir::core::Term::Var(v) = t else { return None };
        atom.args
            .iter()
            .position(|a| matches!(a, blossom_ir::core::Term::Var(x) if x == v))
    };
    let (mut src_group, mut head_group, mut agg) = (Vec::new(), Vec::new(), None);
    for (i, a) in rule.head.args.iter().enumerate() {
        match a {
            HeadArg::Term(t) => {
                src_group.push(col_of(t)?);
                head_group.push(i);
            }
            HeadArg::Agg(call) => {
                if agg.is_some() || call.order.is_some() {
                    return None;
                }
                agg = Some((i, call));
            }
        }
    }
    let (head_agg, call) = agg?;
    let src_tuple: Vec<usize> = call.args.iter().map(col_of).collect::<Option<_>>()?;
    let ok = match call.func {
        AggFunc::Min | AggFunc::Max | AggFunc::Sum => src_tuple.len() == 1,
        AggFunc::Count | AggFunc::CollectVec | AggFunc::CollectVecAt { .. } => !src_tuple.is_empty(),
        _ => false,
    };
    ok.then(|| AggShape {
        src: atom.rel,
        src_group,
        src_tuple,
        head_group,
        head_agg,
        func: call.func.clone(),
    })
}

fn group_row(p: &Program, rule: &Rule, group: &[Value], tuples: &[BTreeMap<Vec<Value>, i64>]) -> expr::ExprResult<Row> {
    let mut keys = group.iter();
    let mut aggs = tuples.iter();
    let mut row = Vec::with_capacity(rule.head.args.len());
    for (col, a) in rule.head.args.iter().enumerate() {
        match a {
            HeadArg::Term(_) => row.push(
                keys.next()
                    .cloned()
                    .ok_or_else(|| bug("a group key too short".into()))?,
            ),
            HeadArg::Agg(agg) => {
                let set = aggs.next().ok_or_else(|| bug("aggregate tuples missing".into()))?;
                row.push(fold(p, rule, col, &agg.func, set)?);
            }
        }
    }
    Ok(Row::from(row))
}

/// An aggregate over a group's distinct argument tuples (LANG-100).
fn fold(
    p: &Program,
    rule: &Rule,
    col: usize,
    func: &AggFunc,
    set: &BTreeMap<Vec<Value>, i64>,
) -> expr::ExprResult<Value> {
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
        AggFunc::Min => set
            .keys()
            .next()
            .map(single)
            .ok_or_else(|| bug("min over an empty group".into()))?,
        AggFunc::Max => set
            .keys()
            .next_back()
            .map(single)
            .ok_or_else(|| bug("max over an empty group".into()))?,
        AggFunc::Sum => {
            // The first component of each distinct tuple; the rest is the valuation it belongs to.
            let vals = set
                .keys()
                .map(|t| {
                    t.first()
                        .cloned()
                        .ok_or_else(|| bug("a sum over an empty tuple".into()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            expr::int_sum(vals.iter())
        }
        // The first component of each distinct tuple, in the set's (canonical) order.
        AggFunc::CollectVec => {
            let vals = set
                .keys()
                .map(|t| {
                    t.first()
                        .cloned()
                        .ok_or_else(|| bug("a collect over an empty tuple".into()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Vec(vals.into()))
        }
        // The component `at` of each distinct tuple, in the set's (canonical) order: by the keys before it.
        AggFunc::CollectVecAt { at } => {
            let vals = set
                .keys()
                .map(|t| {
                    t.get(*at as usize)
                        .cloned()
                        .ok_or_else(|| bug("a collect past a tuple's end".into()))
                })
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
