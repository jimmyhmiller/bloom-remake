//! One tick of one node (ARCHITECTURE §11.2).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_base::{RelId, internal_error};
use blossom_ir::core::{AggFunc, Atom, HeadArg, HeadMode, Literal, Pattern, Program, RelClass, Rule, Term};
use blossom_ir::obs::{FiringKind, FiringRecord, NegRead, PosRead, ProgramErrorRecord};
use blossom_value::{Value, value::IntValue};

use crate::expr::{self, ExprError, Scope};
use crate::plan::{RulePlan, Step};
use crate::{Instance, Oracle, OracleError, Row, Send, TickInput, TickOutput};

/// Rejects programs that use what the oracle does not evaluate yet.
pub(crate) fn check_supported(p: &Program) -> Result<(), OracleError> {
    for r in p.rels.iter() {
        if !r.schema.lattice.is_empty() {
            blossom_base::unimplemented_feature!("SEM-100", "lattice-valued relations in the oracle (WP M4.1)");
        }
        match r.class {
            RelClass::Weighted(_) => {
                blossom_base::unimplemented_feature!("LANG-138", "weighted relations in the oracle (WP M4.1)")
            }
            RelClass::HostTable => {
                blossom_base::unimplemented_feature!("LANG-051", "host-maintained tables in the oracle (WP M4.1)")
            }
            RelClass::Idb | RelClass::Static | RelClass::Event(_) | RelClass::Channel(_) => {}
        }
    }
    for rule in p.rules.iter() {
        if !matches!(rule.head.mode, HeadMode::Insert) {
            blossom_base::unimplemented_feature!("LANG-138", "weighted and violation heads in the oracle (WP M4.1)");
        }
        for a in &rule.head.args {
            if let HeadArg::Agg(agg) = a {
                let supported = matches!(agg.func, AggFunc::Count | AggFunc::Sum | AggFunc::Min | AggFunc::Max)
                    && agg.order.is_none()
                    && !agg.args.is_empty();
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
                && (a.sender.is_some() || a.principal.is_some() || a.weight.is_some())
            {
                blossom_base::unimplemented_feature!(
                    "LANG-241",
                    "`from`, `principal` and weight bindings in the oracle (WP M4.1)"
                );
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

/// The tick's database: rows by relation, with indexes on the column sets the rule plans probe.
#[derive(Default)]
struct Db {
    rows: BTreeMap<RelId, BTreeSet<Row>>,
    indexes: BTreeMap<(RelId, Vec<usize>), Index>,
}

impl Db {
    fn insert(&mut self, rel: RelId, row: Row) -> bool {
        if !self.rows.entry(rel).or_default().insert(row.clone()) {
            return false;
        }
        for ((r, cols), index) in self.indexes.range_mut((rel, Vec::new())..) {
            if *r != rel {
                break;
            }
            index.entry(key(&row, cols)).or_default().push(row.clone());
        }
        true
    }

    fn ensure_index(&mut self, rel: RelId, cols: &[usize]) {
        if cols.is_empty() || self.indexes.contains_key(&(rel, cols.to_vec())) {
            return;
        }
        let mut index = Index::new();
        for row in self.rows.get(&rel).into_iter().flatten() {
            index.entry(key(row, cols)).or_default().push(row.clone());
        }
        self.indexes.insert((rel, cols.to_vec()), index);
    }

    /// Makes sure every index `plan` probes exists.
    fn prepare(&mut self, rule: &Rule, plan: &RulePlan) {
        for step in &plan.steps {
            match step {
                Step::Scan { rel, bound, .. } => self.ensure_index(*rel, bound),
                Step::Check { lit } => {
                    if let Some(Literal::Neg(a)) = rule.body.lits.get(*lit) {
                        self.ensure_index(a.rel, &non_wild(a));
                    }
                }
            }
        }
    }

    /// The rows of `rel` whose columns `cols` hold `values`.
    fn lookup<'a>(&'a self, rel: RelId, cols: &[usize], values: &[Value]) -> Box<dyn Iterator<Item = &'a Row> + 'a> {
        if cols.is_empty() {
            return Box::new(self.rows.get(&rel).into_iter().flatten());
        }
        let found = self
            .indexes
            .get(&(rel, cols.to_vec()))
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
        program,
        node: input.node,
        tick: input.tick,
    };
    let mut db = Db::default();
    for (rel, rows) in oracle.statics.rels.iter().chain(&input.carried.rels) {
        for row in rows {
            db.insert(*rel, row.clone());
        }
    }
    for (rel, row) in input.events {
        db.insert(*rel, row.clone());
    }
    for d in input.delivered {
        db.insert(d.rel, d.row.clone());
    }
    let mut firings: BTreeSet<FiringRecord> = BTreeSet::new();
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
            ExprError::Oracle(e) => e,
        }
    };
    for stratum in &oracle.strata {
        for &id in &stratum.aggregates {
            let (rule, plan) = rule_and_plan(oracle, id)?;
            db.prepare(rule, plan);
            let rows = aggregate(&scope, &db, rule, plan, input.capture).map_err(|e| fail(rule, e))?;
            for (row, firing) in rows {
                db.insert(rule.head.rel, row);
                if let Some(f) = firing {
                    firings.insert(f);
                }
            }
        }
        let mut rounds = 0u32;
        loop {
            let mut changed = false;
            for &id in &stratum.rules {
                let (rule, plan) = rule_and_plan(oracle, id)?;
                db.prepare(rule, plan);
                let derived = derive(&scope, &db, rule, plan, input.capture).map_err(|e| fail(rule, e))?;
                for (row, firing) in derived {
                    changed |= db.insert(rule.head.rel, row);
                    if let Some(f) = firing {
                        firings.insert(f);
                    }
                }
            }
            if !stratum.recursive || !changed {
                break;
            }
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
    for &id in &oracle.inductive {
        let (rule, plan) = rule_and_plan(oracle, id)?;
        db.prepare(rule, plan);
        for (row, firing) in derive(&scope, &db, rule, plan, input.capture).map_err(|e| fail(rule, e))? {
            out.next.insert(rule.head.rel, row);
            if let Some(f) = firing {
                firings.insert(f);
            }
        }
    }
    for &id in &oracle.asynchronous {
        let (rule, plan) = rule_and_plan(oracle, id)?;
        db.prepare(rule, plan);
        for (row, firing) in derive(&scope, &db, rule, plan, input.capture).map_err(|e| fail(rule, e))? {
            let to = match row.first() {
                Some(Value::Node(n)) => *n,
                other => return Err(internal_error!("an async head's destination is {other:?}").into()),
            };
            out.outbox.insert(Send {
                rel: rule.head.rel,
                to,
                row,
            });
            if let Some(f) = firing {
                firings.insert(f);
            }
        }
    }
    out.instance = Instance {
        rels: db.rows.into_iter().filter(|(_, rows)| !rows.is_empty()).collect(),
    };
    out.firings = firings.into_iter().collect();
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
            if let (Literal::Pos(a), Some(Some(row))) = (lit, reads.get(i)) {
                pos.push(PosRead {
                    rel: a.rel,
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
            let values = bound
                .iter()
                .map(|c| match atom.args.get(*c) {
                    Some(t) => expr::term(scope, env, t),
                    None => Err(ExprError::Oracle(
                        internal_error!("a bound column is out of range").into(),
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let rows: Vec<Row> = db.lookup(*rel, bound, &values).cloned().collect();
            for row in rows {
                let saved = env.clone();
                if unify(scope, env, atom, &row)? {
                    if let Some(slot) = reads.get_mut(*lit) {
                        *slot = Some(row.clone());
                    }
                    search(scope, db, rule, plan, step + 1, env, reads, negations, out)?;
                    if let Some(slot) = reads.get_mut(*lit) {
                        *slot = None;
                    }
                }
                *env = saved;
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
                if db.lookup(atom.rel, &cols, &values).next().is_some() {
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
                let saved = env.clone();
                let ok = match pat {
                    Pattern::Var(var) => match env.get_mut(var.index()) {
                        Some(slot @ None) => {
                            *slot = Some(v);
                            true
                        }
                        Some(Some(existing)) => *existing == v,
                        None => {
                            return Err(ExprError::Oracle(
                                internal_error!("variable {var:?} out of range").into(),
                            ));
                        }
                    },
                    Pattern::Wild => true,
                    Pattern::Const(c) => expr::term(scope, env, &Term::Const(*c))? == v,
                    Pattern::Tuple(_) | Pattern::Variant { .. } | Pattern::Struct { .. } => {
                        return Err(ExprError::Oracle(
                            blossom_base::unimplemented_error!("LANG-088", "destructuring in the oracle (WP M4.1)")
                                .into(),
                        ));
                    }
                };
                let r = if ok {
                    search(scope, db, rule, plan, step + 1, env, reads, negations, out)
                } else {
                    Ok(())
                };
                *env = saved;
                r
            }
            other => Err(ExprError::Oracle(
                internal_error!("a check step names {other:?}").into(),
            )),
        },
    }
}

/// Binds the atom's unbound variables to `row`, checking its constants and repeated variables.
fn unify(scope: &Scope<'_>, env: &mut [Option<Value>], atom: &Atom, row: &[Value]) -> expr::ExprResult<bool> {
    if atom.args.len() != row.len() {
        return Err(ExprError::Oracle(internal_error!("a row of the wrong arity").into()));
    }
    for (t, v) in atom.args.iter().zip(row) {
        match t {
            Term::Wild => {}
            Term::Const(_) => {
                if expr::term(scope, env, t)? != *v {
                    return Ok(false);
                }
            }
            Term::Var(var) => match env.get_mut(var.index()) {
                Some(slot @ None) => *slot = Some(v.clone()),
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
        for a in &rule.head.args {
            match a {
                HeadArg::Term(_) => row.push(
                    keys.next()
                        .ok_or_else(|| ExprError::Oracle(internal_error!("group key too short").into()))?,
                ),
                HeadArg::Agg(agg) => {
                    let set = values
                        .next()
                        .ok_or_else(|| ExprError::Oracle(internal_error!("aggregate values missing").into()))?;
                    row.push(fold(agg.func.clone(), set)?);
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
        AggFunc::Sum => {
            let vals = set.iter().map(single).collect::<Result<Vec<_>, _>>()?;
            expr::int_sum(vals.iter())
        }
        other => Err(ExprError::Oracle(
            internal_error!("aggregate {other:?} passed the support check").into(),
        )),
    }
}
