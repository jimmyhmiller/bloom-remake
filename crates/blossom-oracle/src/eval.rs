//! One tick of one node (ARCHITECTURE §11.2).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_base::{RelId, internal_error};
use blossom_ir::core::{AggFunc, Atom, HeadArg, HeadMode, Literal, Program, RelClass, Rule, Term};
use blossom_ir::obs::{FiringKind, FiringRecord, NegRead, PosRead, ProgramErrorRecord};
use blossom_value::{Value, value::IntValue};

use crate::cells::{CellInfo, insert_merged};
use crate::expr::{self, ExprError, Scope};
use crate::plan::{RulePlan, Step};
use crate::{Egress, Instance, Oracle, OracleError, Row, Send, TickInput, TickOutput};

/// Rejects programs that use what the oracle does not evaluate yet.
pub(crate) fn check_supported(p: &Program) -> Result<(), OracleError> {
    for r in p.rels.iter() {
        match r.class {
            RelClass::Weighted(_) => {
                blossom_base::unimplemented_feature!("LANG-138", "weighted relations in the oracle (WP M4.1)")
            }
            RelClass::HostTable => {
                blossom_base::unimplemented_feature!("LANG-051", "host-maintained tables in the oracle (WP M4.1)")
            }
            RelClass::Idb | RelClass::Static | RelClass::Event(_) | RelClass::Channel(_) | RelClass::HostOut(_) => {}
        }
    }
    for rule in p.rules.iter() {
        let lattice_head = p.rels.get(rule.head.rel).is_some_and(|r| !r.schema.lattice.is_empty());
        if lattice_head && rule.head.args.iter().any(|a| matches!(a, HeadArg::Agg(_))) {
            blossom_base::unimplemented_feature!("LANG-100", "aggregates into a lattice-valued relation in the oracle");
        }
        if matches!(rule.head.mode, HeadMode::ZAdd { .. }) {
            blossom_base::unimplemented_feature!("LANG-138", "weighted and violation heads in the oracle (WP M4.1)");
        }
        for a in &rule.head.args {
            if let HeadArg::Agg(agg) = a {
                // A count may be over the empty tuple (`count!(*)` over a header that binds nothing).
                let supported = matches!(agg.func, AggFunc::Count | AggFunc::Sum | AggFunc::Min | AggFunc::Max)
                    && agg.order.is_none()
                    && (!agg.args.is_empty() || matches!(agg.func, AggFunc::Count));
                if !supported {
                    blossom_base::unimplemented_feature!(
                        "LANG-100",
                        "aggregate {:?} in the oracle (WP M4.1): count, sum, min and max over a tuple are evaluated",
                        agg.func
                    );
                }
            }
        }
        for lit in &rule.body.lits {
            if let Literal::Pos(a) | Literal::Neg(a) = lit
                && (a.principal.is_some() || a.weight.is_some())
            {
                blossom_base::unimplemented_feature!("LANG-241", "`principal` and weight bindings in the oracle");
            }
            if let Literal::Neg(a) = lit
                && a.sender.is_some()
            {
                blossom_base::unimplemented_feature!("LANG-241", "`from` on a negated channel atom in the oracle");
            }
        }
    }
    Ok(())
}

/// The static relations' rows, present at every tick.
pub(crate) fn statics(p: &Program) -> Result<Instance, OracleError> {
    let mut out = Instance::default();
    for f in &p.facts {
        let row = f
            .row
            .iter()
            .map(|c| {
                p.consts
                    .get(*c)
                    .cloned()
                    .ok_or_else(|| internal_error!("unknown constant {c:?} in a fact"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        out.insert(f.rel, Arc::from(row));
    }
    Ok(out)
}

/// The rows of one relation by the values of some of their columns.
type Index = BTreeMap<Vec<Value>, Vec<Row>>;

/// The tick's database: rows by relation, with indexes on the column sets the rule plans probe. Received channel
/// tuples are also kept with their sender as a trailing column (`sent`), for atoms that bind it (`from s`). A
/// lattice-valued relation holds one row per cell, the join of everything derived for it (SEM-100).
struct Db<'o> {
    rows: BTreeMap<RelId, BTreeSet<Row>>,
    sent: BTreeMap<RelId, BTreeSet<Row>>,
    indexes: BTreeMap<(RelId, bool, Vec<usize>), Index>,
    cells: &'o BTreeMap<RelId, CellInfo>,
    /// Each lattice-valued relation's current row per cell.
    current: BTreeMap<(RelId, bool), BTreeMap<Vec<Value>, Row>>,
}

impl<'o> Db<'o> {
    fn new(cells: &'o BTreeMap<RelId, CellInfo>) -> Db<'o> {
        Db {
            rows: BTreeMap::new(),
            sent: BTreeMap::new(),
            indexes: BTreeMap::new(),
            cells,
            current: BTreeMap::new(),
        }
    }

    fn insert(&mut self, rel: RelId, row: Row) -> Result<bool, ExprError> {
        self.insert_in(rel, false, row)
    }

    /// Adds a row, or merges it into its cell; whether the relation changed.
    fn insert_in(&mut self, rel: RelId, sent: bool, row: Row) -> Result<bool, ExprError> {
        let Some(info) = self.cells.get(&rel) else {
            return Ok(self.add(rel, sent, row));
        };
        if info.is_bottom(&row).map_err(|e| ExprError::Oracle(e.into()))? {
            return Ok(false);
        }
        let id = info.ident(&row, usize::from(sent));
        let old = self.current.get(&(rel, sent)).and_then(|m| m.get(&id)).cloned();
        let new = match old {
            None => row,
            Some(old) => {
                let merged = info.merge(&old, &row)?;
                if merged == old {
                    return Ok(false);
                }
                self.remove(rel, sent, &old);
                merged
            }
        };
        self.current.entry((rel, sent)).or_default().insert(id, new.clone());
        Ok(self.add(rel, sent, new))
    }

    fn add(&mut self, rel: RelId, sent: bool, row: Row) -> bool {
        let table = if sent { &mut self.sent } else { &mut self.rows };
        if !table.entry(rel).or_default().insert(row.clone()) {
            return false;
        }
        for ((r, s, cols), index) in self.indexes.range_mut((rel, sent, Vec::new())..) {
            if *r != rel || *s != sent {
                break;
            }
            index.entry(key(&row, cols)).or_default().push(row.clone());
        }
        true
    }

    /// Removes a row a merge superseded.
    fn remove(&mut self, rel: RelId, sent: bool, row: &Row) {
        let table = if sent { &mut self.sent } else { &mut self.rows };
        if let Some(rows) = table.get_mut(&rel) {
            rows.remove(row);
        }
        for ((r, s, cols), index) in self.indexes.range_mut((rel, sent, Vec::new())..) {
            if *r != rel || *s != sent {
                break;
            }
            if let Some(bucket) = index.get_mut(&key(row, cols)) {
                bucket.retain(|x| x != row);
            }
        }
    }

    fn ensure_index(&mut self, rel: RelId, sent: bool, cols: &[usize]) {
        if cols.is_empty() || self.indexes.contains_key(&(rel, sent, cols.to_vec())) {
            return;
        }
        let table = if sent { &self.sent } else { &self.rows };
        let mut index = Index::new();
        for row in table.get(&rel).into_iter().flatten() {
            index.entry(key(row, cols)).or_default().push(row.clone());
        }
        self.indexes.insert((rel, sent, cols.to_vec()), index);
    }

    /// Makes sure every index `plan` probes exists.
    fn prepare(&mut self, rule: &Rule, plan: &RulePlan) {
        for step in &plan.steps {
            match step {
                Step::Scan { lit, rel, bound } => {
                    let sent = matches!(rule.body.lits.get(*lit), Some(Literal::Pos(a)) if a.sender.is_some());
                    self.ensure_index(*rel, sent, bound);
                }
                Step::Check { lit } => match rule.body.lits.get(*lit) {
                    Some(Literal::Neg(a)) => self.ensure_index(a.rel, false, &non_wild(a)),
                    Some(Literal::Lookup { rel, .. }) => {
                        let cols = self.key_cols(*rel);
                        self.ensure_index(*rel, false, &cols);
                    }
                    _ => {}
                },
            }
        }
    }

    /// The key columns of a lattice-valued relation (what a lookup `r[k̄]` names).
    fn key_cols(&self, rel: RelId) -> Vec<usize> {
        self.cells.get(&rel).map(|c| c.key.clone()).unwrap_or_default()
    }

    /// The rows of `rel` whose columns `cols` hold `values`.
    fn lookup<'a>(
        &'a self,
        rel: RelId,
        sent: bool,
        cols: &[usize],
        values: &[Value],
    ) -> Box<dyn Iterator<Item = &'a Row> + 'a> {
        let table = if sent { &self.sent } else { &self.rows };
        if cols.is_empty() {
            return Box::new(table.get(&rel).into_iter().flatten());
        }
        let found = self
            .indexes
            .get(&(rel, sent, cols.to_vec()))
            .and_then(|index| index.get(values));
        Box::new(found.into_iter().flatten())
    }
}

fn key(row: &[Value], cols: &[usize]) -> Vec<Value> {
    cols.iter().filter_map(|c| row.get(*c).cloned()).collect()
}

fn non_wild(a: &Atom) -> Vec<usize> {
    a.args
        .iter()
        .enumerate()
        .filter(|(_, t)| !matches!(t, Term::Wild))
        .map(|(i, _)| i)
        .collect()
}

/// One satisfying valuation of a rule body: the variables, and what the body read.
struct Valuation {
    env: Vec<Option<Value>>,
    reads: Vec<PosRead>,
    negations: Vec<NegRead>,
}

pub(crate) fn tick(oracle: &Oracle, input: &TickInput<'_>) -> Result<TickOutput, OracleError> {
    let program = oracle.program.get();
    let scope = Scope {
        incarnation: input.incarnation,
        program,
        node: input.node,
        tick: input.tick,
        now: input.now,
        oracle,
        fuel: expr::Fuel::default(),
    };
    let mut db = Db::new(&oracle.cells);
    let load = |e: ExprError| -> OracleError {
        match e {
            ExprError::Arithmetic(detail) => OracleError::Program {
                tick: input.tick,
                error: ProgramErrorRecord {
                    code: expr::arithmetic_code(),
                    rule: None,
                    detail: Arc::from(detail),
                },
            },
            ExprError::Conflict(detail) => OracleError::Program {
                tick: input.tick,
                error: ProgramErrorRecord {
                    code: expr::conflict_code(),
                    rule: None,
                    detail: Arc::from(detail),
                },
            },
            ExprError::Refused(detail) => OracleError::Program {
                tick: input.tick,
                error: ProgramErrorRecord {
                    code: expr::refused_code(),
                    rule: None,
                    detail: Arc::from(detail),
                },
            },
            ExprError::Budget(detail) => OracleError::Program {
                tick: input.tick,
                error: ProgramErrorRecord {
                    code: expr::budget_code(),
                    rule: None,
                    detail: Arc::from(detail),
                },
            },
            ExprError::Oracle(e) => e,
        }
    };
    for (rel, rows) in oracle.statics.rels.iter().chain(&input.carried.rels) {
        for row in rows {
            db.insert(*rel, row.clone()).map_err(load)?;
        }
    }
    for (rel, row) in input.events {
        db.insert(*rel, row.clone()).map_err(load)?;
    }
    for d in input.delivered {
        db.insert(d.rel, d.row.clone()).map_err(load)?;
        let mut with_sender: Vec<Value> = d.row.to_vec();
        with_sender.push(Value::Node(d.from));
        db.insert_in(d.rel, true, Arc::from(with_sender)).map_err(load)?;
    }
    for g in input.ingress {
        db.insert(g.rel, g.row.clone()).map_err(load)?;
        let mut with_sender: Vec<Value> = g.row.to_vec();
        with_sender.push(Value::Session(g.session));
        db.insert_in(g.rel, true, Arc::from(with_sender)).map_err(load)?;
    }
    // A firing is found once per evaluation of its rule, so only a recursive stratum, whose rules are evaluated
    // again every round, can find one twice.
    let mut firings: Vec<FiringRecord> = Vec::new();
    let fail = |rule: &Rule, e: ExprError| -> OracleError {
        match e {
            ExprError::Arithmetic(detail) => OracleError::Program {
                tick: input.tick,
                error: ProgramErrorRecord {
                    code: expr::arithmetic_code(),
                    rule: Some(rule.label.clone()),
                    detail: Arc::from(detail),
                },
            },
            ExprError::Conflict(detail) => OracleError::Program {
                tick: input.tick,
                error: ProgramErrorRecord {
                    code: expr::conflict_code(),
                    rule: Some(rule.label.clone()),
                    detail: Arc::from(detail),
                },
            },
            ExprError::Refused(detail) => OracleError::Program {
                tick: input.tick,
                error: ProgramErrorRecord {
                    code: expr::refused_code(),
                    rule: Some(rule.label.clone()),
                    detail: Arc::from(detail),
                },
            },
            ExprError::Budget(detail) => OracleError::Program {
                tick: input.tick,
                error: ProgramErrorRecord {
                    code: expr::budget_code(),
                    rule: Some(rule.label.clone()),
                    detail: Arc::from(detail),
                },
            },
            ExprError::Oracle(e) => e,
        }
    };
    for stratum in &oracle.strata {
        for &id in &stratum.aggregates {
            let (rule, plan) = rule_and_plan(oracle, id)?;
            if !oracle.runs_on(rule, input.node) {
                continue;
            }
            db.prepare(rule, plan);
            let rows = aggregate(&scope, &db, rule, plan, input.capture).map_err(|e| fail(rule, e))?;
            for (row, firing) in rows {
                db.insert(rule.head.rel, row).map_err(|e| fail(rule, e))?;
                firings.extend(firing);
            }
        }
        let mut recursive_firings: BTreeSet<FiringRecord> = BTreeSet::new();
        let mut rounds = 0u32;
        // In a recursive stratum a rule may meet a value its stratum has not finished growing (an `LMax` below its
        // fixpoint, say): a program error there is judged only at the fixpoint, where every rule runs once more with
        // errors fatal (and any row that run adds resumes the iteration).
        let mut strict = !stratum.recursive;
        loop {
            let mut changed = false;
            for &id in &stratum.rules {
                let (rule, plan) = rule_and_plan(oracle, id)?;
                if !oracle.runs_on(rule, input.node) {
                    continue;
                }
                db.prepare(rule, plan);
                let derived = match derive(&scope, &db, rule, plan, input.capture) {
                    Ok(rows) => rows,
                    // A program error counts only at the fixpoint (a value it depends on may still change).
                    Err(
                        ExprError::Arithmetic(_)
                        | ExprError::Conflict(_)
                        | ExprError::Refused(_)
                        | ExprError::Budget(_),
                    ) if !strict => continue,
                    Err(e) => return Err(fail(rule, e)),
                };
                for (row, firing) in derived {
                    changed |= db.insert(rule.head.rel, row).map_err(|e| fail(rule, e))?;
                    if let Some(f) = firing {
                        if stratum.recursive {
                            recursive_firings.insert(f);
                        } else {
                            firings.push(f);
                        }
                    }
                }
            }
            if !stratum.recursive || (strict && !changed) {
                firings.extend(std::mem::take(&mut recursive_firings));
                break;
            }
            // Quiescent: run once more with errors fatal; a row it adds resumes the iteration.
            strict = !changed;
            rounds += 1;
            if rounds >= oracle.limits.max_rounds {
                let label = stratum
                    .rules
                    .first()
                    .and_then(|id| program.rules.get(*id))
                    .map(|r| r.label.clone());
                return Err(OracleError::Program {
                    tick: input.tick,
                    error: ProgramErrorRecord {
                        code: expr::fixpoint_code(),
                        rule: label,
                        detail: Arc::from(format!(
                            "the fixpoint did not converge within {} rounds (CR-53)",
                            oracle.limits.max_rounds
                        )),
                    },
                });
            }
        }
    }
    let mut out = TickOutput::default();
    let mut outgoing: BTreeMap<(RelId, blossom_value::time::NodeId), BTreeSet<Row>> = BTreeMap::new();
    let mut replies: BTreeMap<(RelId, blossom_value::value::SessionId), BTreeSet<Row>> = BTreeMap::new();
    for &id in &oracle.inductive {
        let (rule, plan) = rule_and_plan(oracle, id)?;
        if !oracle.runs_on(rule, input.node) {
            continue;
        }
        db.prepare(rule, plan);
        for (row, firing) in heads(&scope, &db, rule, plan, input.capture).map_err(|e| fail(rule, e))? {
            let set = out.next.rels.entry(rule.head.rel).or_default();
            insert_merged(&oracle.cells, set, rule.head.rel, row).map_err(|e| fail(rule, e.into()))?;
            firings.extend(firing);
        }
    }
    for &id in &oracle.asynchronous {
        let (rule, plan) = rule_and_plan(oracle, id)?;
        if !oracle.runs_on(rule, input.node) {
            continue;
        }
        db.prepare(rule, plan);
        let to_host = matches!(program.rels.get(rule.head.rel).map(|r| &r.class), Some(RelClass::HostOut(_)));
        for (row, firing) in heads(&scope, &db, rule, plan, input.capture).map_err(|e| fail(rule, e))? {
            // A request to the host (a stream's write, close or dial) leaves with the tick (FOREIGN-PROTOCOLS §1).
            if to_host {
                out.host.insert(crate::HostOut {
                    rel: rule.head.rel,
                    row,
                });
                firings.extend(firing);
                continue;
            }
            let to = match row.first() {
                Some(Value::Node(n)) => *n,
                // A reply to a client session leaves the deployment (LANGUAGE §18.4); a session is a destination
                // like a node, so a lattice reply channel merges per session and key too (§14.2).
                Some(Value::Session(s)) => {
                    let set = replies.entry((rule.head.rel, *s)).or_default();
                    insert_merged(&oracle.cells, set, rule.head.rel, row).map_err(|e| fail(rule, e.into()))?;
                    firings.extend(firing);
                    continue;
                }
                other => return Err(internal_error!("an async head's destination is {other:?}").into()),
            };
            // A lattice channel sends one message per destination and key: the join of the tick's values (§11.9).
            let set = outgoing.entry((rule.head.rel, to)).or_default();
            insert_merged(&oracle.cells, set, rule.head.rel, row).map_err(|e| fail(rule, e.into()))?;
            firings.extend(firing);
        }
    }
    for ((rel, to), rows) in outgoing {
        for row in rows {
            out.outbox.insert(Send { rel, to, row });
        }
    }
    for ((rel, session), rows) in replies {
        for row in rows {
            out.egress.insert(Egress { rel, session, row });
        }
    }
    check_keys(program, &db.rows, input.tick)?;
    check_invariants(program, &db.rows, input.tick)?;
    out.instance = Instance {
        rels: db.rows.into_iter().filter(|(_, rows)| !rows.is_empty()).collect(),
    };
    out.firings = firings;
    Ok(out)
}

fn rule_and_plan(oracle: &Oracle, id: blossom_base::RuleId) -> Result<(&Rule, &RulePlan), OracleError> {
    let rule = oracle.program.get().rules.get_or_bug(id)?;
    let plan = oracle
        .plans
        .get(&id)
        .ok_or_else(|| internal_error!("rule {id:?} has no plan"))?;
    Ok((rule, plan))
}

/// Every valuation of the body of `rule`.
fn valuations(scope: &Scope<'_>, db: &Db, rule: &Rule, plan: &RulePlan) -> expr::ExprResult<Vec<Valuation>> {
    let mut out = Vec::new();
    let mut env = vec![None; plan.nvars];
    let mut reads: Vec<Option<Row>> = vec![None; rule.body.lits.len()];
    let mut negations = Vec::new();
    search(scope, db, rule, plan, 0, &mut env, &mut reads, &mut negations, &mut out)?;
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn search(
    scope: &Scope<'_>,
    db: &Db,
    rule: &Rule,
    plan: &RulePlan,
    step: usize,
    env: &mut Vec<Option<Value>>,
    reads: &mut Vec<Option<Row>>,
    negations: &mut Vec<NegRead>,
    out: &mut Vec<Valuation>,
) -> expr::ExprResult<()> {
    let Some(s) = plan.steps.get(step) else {
        let mut pos = Vec::new();
        for (i, lit) in rule.body.lits.iter().enumerate() {
            if let (Literal::Pos(Atom { rel, .. }) | Literal::Lookup { rel, .. }, Some(Some(row))) = (lit, reads.get(i))
            {
                pos.push(PosRead {
                    rel: *rel,
                    row: row.clone(),
                });
            }
        }
        out.push(Valuation {
            env: env.clone(),
            reads: pos,
            negations: negations.clone(),
        });
        return Ok(());
    };
    match s {
        Step::Scan { lit, rel, bound } => {
            let Some(Literal::Pos(atom)) = rule.body.lits.get(*lit) else {
                return Err(ExprError::Oracle(
                    internal_error!("a scan step names a non-atom").into(),
                ));
            };
            let sent = atom.sender.is_some();
            let values = bound
                .iter()
                .map(|c| {
                    let t = if *c == atom.args.len() {
                        atom.sender.as_ref()
                    } else {
                        atom.args.get(*c)
                    };
                    match t {
                        Some(t) => expr::term(scope, env, t),
                        None => Err(ExprError::Oracle(
                            internal_error!("a bound column is out of range").into(),
                        )),
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut bound_here: Vec<usize> = Vec::with_capacity(atom.args.len() + 1);
            for row in db.lookup(*rel, sent, bound, &values) {
                let matched = unify(scope, env, atom, row, &mut bound_here)?;
                if matched {
                    if let Some(slot) = reads.get_mut(*lit) {
                        // Provenance reads the tuple itself, without its sender.
                        *slot = Some(if sent {
                            Arc::from(row.get(..atom.args.len()).unwrap_or(&[]).to_vec())
                        } else {
                            row.clone()
                        });
                    }
                    search(scope, db, rule, plan, step + 1, env, reads, negations, out)?;
                    if let Some(slot) = reads.get_mut(*lit) {
                        *slot = None;
                    }
                }
                // Undo exactly the bindings this row made.
                for i in bound_here.drain(..) {
                    if let Some(slot) = env.get_mut(i) {
                        *slot = None;
                    }
                }
            }
            Ok(())
        }
        Step::Check { lit } => match rule.body.lits.get(*lit) {
            Some(Literal::Neg(atom)) => {
                let cols = non_wild(atom);
                let values = cols
                    .iter()
                    .map(|c| match atom.args.get(*c) {
                        Some(t) => expr::term(scope, env, t),
                        None => Err(ExprError::Oracle(
                            internal_error!("a negated column is out of range").into(),
                        )),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if db.lookup(atom.rel, false, &cols, &values).next().is_some() {
                    return Ok(());
                }
                let mut pattern = vec![None; atom.args.len()];
                for (c, v) in cols.iter().zip(values) {
                    if let Some(slot) = pattern.get_mut(*c) {
                        *slot = Some(v);
                    }
                }
                negations.push(NegRead { rel: atom.rel, pattern });
                let r = search(scope, db, rule, plan, step + 1, env, reads, negations, out);
                negations.pop();
                r
            }
            Some(Literal::Guard(e)) => {
                if expr::truth(&expr::eval(scope, env, e)?)? {
                    search(scope, db, rule, plan, step + 1, env, reads, negations, out)
                } else {
                    Ok(())
                }
            }
            Some(Literal::Bind { pat, expr: e }) => {
                let v = expr::eval(scope, env, e)?;
                let mut newly = Vec::new();
                let ok = expr::matches(scope, env, pat, &v, &mut newly)?;
                let r = if ok {
                    search(scope, db, rule, plan, step + 1, env, reads, negations, out)
                } else {
                    Ok(())
                };
                for i in newly {
                    if let Some(slot) = env.get_mut(i) {
                        *slot = None;
                    }
                }
                r
            }
            Some(Literal::Lookup { var, rel, key: terms }) => {
                // The cell's value, ⊥ when absent (LANGUAGE §9.9). A present cell is read like an atom's row; an
                // absent one like a negation over its key.
                let cols = db.key_cols(*rel);
                let values = terms
                    .iter()
                    .map(|t| expr::term(scope, env, t))
                    .collect::<Result<Vec<_>, _>>()?;
                let info = db.cells.get(rel).ok_or_else(|| {
                    ExprError::Oracle(internal_error!("a lookup on {rel:?}, which has no lattice column").into())
                })?;
                let [(col, kind)] = info.lattice.as_slice() else {
                    return Err(ExprError::Oracle(
                        internal_error!("a lookup needs exactly one lattice column").into(),
                    ));
                };
                let row = db.lookup(*rel, false, &cols, &values).next().cloned();
                let value = match &row {
                    Some(r) => r
                        .get(*col)
                        .cloned()
                        .ok_or_else(|| ExprError::Oracle(internal_error!("a cell row without its value").into()))?,
                    None => Value::Lattice(kind.bottom()),
                };
                let slot = env
                    .get_mut(var.index())
                    .ok_or_else(|| ExprError::Oracle(internal_error!("variable {var:?} out of range").into()))?;
                *slot = Some(value);
                let r = match row {
                    Some(row) => {
                        if let Some(read) = reads.get_mut(*lit) {
                            *read = Some(row);
                        }
                        let r = search(scope, db, rule, plan, step + 1, env, reads, negations, out);
                        if let Some(read) = reads.get_mut(*lit) {
                            *read = None;
                        }
                        r
                    }
                    None => {
                        let arity = scope.program.rels.get(*rel).map_or(0, |r| r.schema.cols.len());
                        let mut pattern = vec![None; arity];
                        for (c, v) in cols.iter().zip(values) {
                            if let Some(slot) = pattern.get_mut(*c) {
                                *slot = Some(v);
                            }
                        }
                        negations.push(NegRead { rel: *rel, pattern });
                        let r = search(scope, db, rule, plan, step + 1, env, reads, negations, out);
                        negations.pop();
                        r
                    }
                };
                if let Some(slot) = env.get_mut(var.index()) {
                    *slot = None;
                }
                r
            }
            Some(Literal::Gen { pat, src }) => {
                for v in expr::generate(scope, env, src)? {
                    let mut newly = Vec::new();
                    let ok = expr::matches(scope, env, pat, &v, &mut newly)?;
                    let r = if ok {
                        search(scope, db, rule, plan, step + 1, env, reads, negations, out)
                    } else {
                        Ok(())
                    };
                    for i in newly {
                        if let Some(slot) = env.get_mut(i) {
                            *slot = None;
                        }
                    }
                    r?;
                }
                Ok(())
            }
            other => Err(ExprError::Oracle(
                internal_error!("a check step names {other:?}").into(),
            )),
        },
    }
}

/// Binds the atom's unbound variables to `row`, recording each variable it binds in `bound_here` (also on a
/// mismatch, so the caller can undo them).
fn unify(
    scope: &Scope<'_>,
    env: &mut [Option<Value>],
    atom: &Atom,
    row: &[Value],
    bound_here: &mut Vec<usize>,
) -> expr::ExprResult<bool> {
    let arity = atom.args.len() + usize::from(atom.sender.is_some());
    if arity != row.len() {
        return Err(ExprError::Oracle(internal_error!("a row of the wrong arity").into()));
    }
    for (t, v) in atom.args.iter().chain(atom.sender.iter()).zip(row) {
        match t {
            Term::Wild => {}
            Term::Const(_) => {
                if expr::term(scope, env, t)? != *v {
                    return Ok(false);
                }
            }
            Term::Var(var) => match env.get_mut(var.index()) {
                Some(slot @ None) => {
                    *slot = Some(v.clone());
                    bound_here.push(var.index());
                }
                Some(Some(existing)) => {
                    if existing != v {
                        return Ok(false);
                    }
                }
                None => {
                    return Err(ExprError::Oracle(
                        internal_error!("variable {var:?} out of range").into(),
                    ));
                }
            },
        }
    }
    Ok(true)
}

fn head_value(scope: &Scope<'_>, env: &[Option<Value>], t: &Term) -> expr::ExprResult<Value> {
    expr::term(scope, env, t)
}

/// The head tuples of a rule evaluated once on the completed instance (inductive and async rules), aggregate or not.
fn heads(
    scope: &Scope<'_>,
    db: &Db,
    rule: &Rule,
    plan: &RulePlan,
    capture: bool,
) -> expr::ExprResult<Vec<(Row, Option<FiringRecord>)>> {
    if crate::strata::is_aggregate(rule) {
        aggregate(scope, db, rule, plan, capture)
    } else {
        derive(scope, db, rule, plan, capture)
    }
}

/// The head tuples of a non-aggregate rule, each with its firing when capturing.
fn derive(
    scope: &Scope<'_>,
    db: &Db,
    rule: &Rule,
    plan: &RulePlan,
    capture: bool,
) -> expr::ExprResult<Vec<(Row, Option<FiringRecord>)>> {
    let mut out = Vec::new();
    for v in valuations(scope, db, rule, plan)? {
        let mut row = Vec::with_capacity(rule.head.args.len());
        for a in &rule.head.args {
            match a {
                HeadArg::Term(t) => row.push(head_value(scope, &v.env, t)?),
                HeadArg::Agg(_) => {
                    return Err(ExprError::Oracle(
                        internal_error!("an aggregate rule reached derive").into(),
                    ));
                }
            }
        }
        let row: Row = Arc::from(row);
        let firing = capture.then(|| FiringRecord {
            rule: rule.id,
            kind: FiringKind::Rule,
            head: row.clone(),
            reads: v.reads,
            negations: v.negations,
        });
        out.push((row, firing));
    }
    Ok(out)
}

/// The rows of an aggregate rule: one per group of valuations (the non-aggregate head columns), each aggregate over
/// the distinct values of its arguments in the group (LANG-100). An empty group has no row.
fn aggregate(
    scope: &Scope<'_>,
    db: &Db,
    rule: &Rule,
    plan: &RulePlan,
    capture: bool,
) -> expr::ExprResult<Vec<(Row, Option<FiringRecord>)>> {
    struct Group {
        /// Per aggregate head column: the distinct argument tuples.
        values: Vec<BTreeSet<Vec<Value>>>,
        reads: BTreeSet<PosRead>,
        negations: BTreeSet<NegRead>,
    }
    let aggs: Vec<usize> = rule
        .head
        .args
        .iter()
        .enumerate()
        .filter(|(_, a)| matches!(a, HeadArg::Agg(_)))
        .map(|(i, _)| i)
        .collect();
    let mut groups: BTreeMap<Vec<Value>, Group> = BTreeMap::new();
    for v in valuations(scope, db, rule, plan)? {
        let mut key = Vec::new();
        for a in &rule.head.args {
            if let HeadArg::Term(t) = a {
                key.push(head_value(scope, &v.env, t)?);
            }
        }
        let group = groups.entry(key).or_insert_with(|| Group {
            values: vec![BTreeSet::new(); aggs.len()],
            reads: BTreeSet::new(),
            negations: BTreeSet::new(),
        });
        for (slot, &col) in group.values.iter_mut().zip(&aggs) {
            let Some(HeadArg::Agg(agg)) = rule.head.args.get(col) else {
                return Err(ExprError::Oracle(internal_error!("aggregate column moved").into()));
            };
            let tuple = agg
                .args
                .iter()
                .map(|t| expr::term(scope, &v.env, t))
                .collect::<Result<Vec<_>, _>>()?;
            slot.insert(tuple);
        }
        if capture {
            group.reads.extend(v.reads);
            group.negations.extend(v.negations);
        }
    }
    let mut out = Vec::new();
    for (key, group) in groups {
        let mut keys = key.into_iter();
        let mut values = group.values.iter();
        let mut row = Vec::with_capacity(rule.head.args.len());
        for (col, a) in rule.head.args.iter().enumerate() {
            match a {
                HeadArg::Term(_) => row.push(
                    keys.next()
                        .ok_or_else(|| ExprError::Oracle(internal_error!("group key too short").into()))?,
                ),
                HeadArg::Agg(agg) => {
                    let set = values
                        .next()
                        .ok_or_else(|| ExprError::Oracle(internal_error!("aggregate values missing").into()))?;
                    let folded = fold(agg.func.clone(), set)?;
                    row.push(match agg.func {
                        AggFunc::Count => count_as(scope, rule, col, folded)?,
                        _ => folded,
                    });
                }
            }
        }
        let row: Row = Arc::from(row);
        let firing = capture.then(|| FiringRecord {
            rule: rule.id,
            kind: FiringKind::Aggregate,
            head: row.clone(),
            reads: group.reads.into_iter().collect(),
            negations: group.negations.into_iter().collect(),
        });
        out.push((row, firing));
    }
    Ok(out)
}

/// A count (computed as `u64`) in the integer type of the head column it lands in (BLSR004 when it does not fit).
fn count_as(scope: &Scope<'_>, rule: &Rule, col: usize, count: Value) -> expr::ExprResult<Value> {
    let Value::Int(IntValue::U64(n)) = count else {
        return Err(ExprError::Oracle(internal_error!("a count is not a u64").into()));
    };
    let ty = scope
        .program
        .rels
        .get(rule.head.rel)
        .and_then(|r| r.schema.cols.get(col))
        .and_then(|c| scope.program.types.get(c.ty))
        .ok_or_else(|| ExprError::Oracle(internal_error!("a count's column has no type").into()))?;
    let overflow = || ExprError::Arithmetic(format!("count {n} does not fit its column"));
    use blossom_value::TypeDef;
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
        other => {
            return Err(ExprError::Oracle(
                internal_error!("a count in a column of type {other:?}").into(),
            ));
        }
    }))
}

fn fold(func: AggFunc, set: &BTreeSet<Vec<Value>>) -> expr::ExprResult<Value> {
    let single = |t: &Vec<Value>| -> expr::ExprResult<Value> {
        match t.as_slice() {
            [v] => Ok(v.clone()),
            _ => Err(ExprError::Oracle(
                internal_error!("{func:?} over a tuple of {} values", t.len()).into(),
            )),
        }
    };
    match func {
        AggFunc::Count => Ok(Value::Int(IntValue::U64(
            u64::try_from(set.len()).map_err(|_| ExprError::Arithmetic("count overflows u64".into()))?,
        ))),
        AggFunc::Min | AggFunc::Max => {
            let vals = set.iter().map(single).collect::<Result<Vec<_>, _>>()?;
            let pick = if matches!(func, AggFunc::Min) {
                vals.into_iter().min()
            } else {
                vals.into_iter().max()
            };
            pick.ok_or_else(|| ExprError::Oracle(internal_error!("{func:?} over an empty group").into()))
        }
        // The first component of each distinct tuple; the rest is the valuation it belongs to.
        AggFunc::Sum => {
            let vals = set
                .iter()
                .map(|t| {
                    t.first()
                        .cloned()
                        .ok_or_else(|| ExprError::Oracle(internal_error!("sum over an empty tuple").into()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            expr::int_sum(vals.iter())
        }
        other => Err(ExprError::Oracle(
            internal_error!("aggregate {other:?} passed the support check").into(),
        )),
    }
}

/// SEM-050 / SEM-051: two distinct tuples with one key in one tick are a runtime error (BLSR001; BLSR002 for the
/// staging relation of an `upsert`, where it means two upserts gave one key different values).
fn check_keys(
    program: &Program,
    rows: &BTreeMap<RelId, BTreeSet<Row>>,
    tick: blossom_value::time::Tick,
) -> Result<(), OracleError> {
    use blossom_ir::core::ConstructKind;
    for (rel, set) in rows {
        let Some(decl) = program.rels.get(*rel) else { continue };
        if decl.schema.payload.is_empty() || set.len() < 2 {
            continue;
        }
        let cols: Vec<usize> = decl.schema.key.iter().map(|c| c.index()).collect();
        let mut seen: BTreeMap<Vec<Value>, &Row> = BTreeMap::new();
        for row in set {
            let k = key(row, &cols);
            if let Some(prev) = seen.insert(k.clone(), row) {
                let upsert = program
                    .constructs
                    .iter()
                    .any(|c| matches!(c.kind, ConstructKind::Upsert { staging, .. } if staging == *rel));
                let code = if upsert {
                    blossom_base::code!("BLSR002")
                } else {
                    blossom_base::code!("BLSR001")
                };
                return Err(OracleError::Program {
                    tick,
                    error: ProgramErrorRecord {
                        code: code.as_str(),
                        rule: None,
                        detail: Arc::from(format!(
                            "{}: two tuples with key {k:?} in one tick: {prev:?} and {row:?}",
                            decl.name
                        )),
                    },
                });
            }
        }
    }
    Ok(())
}

/// LANG-200: a violation head that derives a row aborts the tick (BLSR003), the default action (`Record` and `Warn`
/// need the runtime's violation sink, which this evaluator does not have).
fn check_invariants(
    program: &Program,
    rows: &BTreeMap<RelId, BTreeSet<Row>>,
    tick: blossom_value::time::Tick,
) -> Result<(), OracleError> {
    for rule in program.rules.iter() {
        let HeadMode::Violation { invariant } = rule.head.mode else {
            continue;
        };
        let Some(row) = rows.get(&rule.head.rel).and_then(|r| r.iter().next()) else {
            continue;
        };
        let inv = program.invariants.get(invariant);
        if let Some(inv) = inv
            && inv.action != blossom_ir::core::ViolationAction::Abort
        {
            blossom_base::unimplemented_feature!("LANG-200", "the `{:?}` violation action in the oracle", inv.action);
        }
        let name = inv.map_or_else(|| format!("{invariant:?}"), |i| i.name.to_string());
        return Err(OracleError::Program {
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
