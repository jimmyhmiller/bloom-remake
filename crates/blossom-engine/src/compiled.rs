//! Compiled relational terms. Slots refer to source columns, not cloned variable environments.
//! Compilation fixes every probe, equality constraint and projection for one delta driver/order.
//! Storage snapshots and signed publication remain owned by the incremental engine.
use std::collections::BTreeMap;

use crate::rule::{Driver, Order, Plan, Step, StoreKey, Stores, atom_store};
use blossom_base::{ConstId, VarId, internal_error};
use blossom_ir::{
    core::{Atom, HeadArg, Literal, Rule, Term},
    tick::{EvalError, Row},
};
use blossom_value::Value;

#[derive(Clone, Copy, Debug)]
enum Source {
    Column(usize, usize),
    Constant(ConstId),
}
impl Source {
    fn read<'a>(&self, cx: &'a blossom_ir::core::Program, rows: &'a [Option<Row>]) -> Result<&'a Value, EvalError> {
        match *self {
            Self::Column(lit, col) => rows.get(lit).and_then(Option::as_ref).and_then(|r| r.get(col)),
            Self::Constant(id) => cx.consts.get(id),
        }
        .ok_or_else(|| internal_error!("compiled relational source is unavailable: {self:?}").into())
    }
}
#[derive(Clone, Debug)]
struct Binding {
    lit: usize,
    width: usize,
    checks: Vec<(usize, Source)>,
}
impl Binding {
    fn compile(lit: usize, terms: impl Iterator<Item = Term>, bound: &mut BTreeMap<VarId, Source>) -> Self {
        let mut checks = Vec::new();
        let mut width = 0;
        for (col, term) in terms.enumerate() {
            width += 1;
            match term {
                Term::Wild => {}
                Term::Const(id) => checks.push((col, Source::Constant(id))),
                Term::Var(v) => {
                    if let Some(source) = bound.get(&v) {
                        checks.push((col, *source));
                    } else {
                        bound.insert(v, Source::Column(lit, col));
                    }
                }
            }
        }
        Self { lit, width, checks }
    }
    fn matches(&self, cx: &blossom_ir::core::Program, rows: &[Option<Row>]) -> Result<bool, EvalError> {
        let row = rows
            .get(self.lit)
            .and_then(Option::as_ref)
            .ok_or_else(|| internal_error!("compiled binding has no row"))?;
        if row.len() != self.width {
            return Err(internal_error!("compiled binding row width mismatch").into());
        }
        for (col, source) in &self.checks {
            if row.get(*col) != Some(source.read(cx, rows)?) {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
#[derive(Clone, Debug)]
enum Op {
    Join {
        binding: Binding,
        store: StoreKey,
        cols: Vec<usize>,
        values: Vec<Source>,
    },
    Absent {
        lit: usize,
        store: StoreKey,
        cols: Vec<usize>,
        values: Vec<Source>,
    },
}
#[derive(Clone, Debug)]
pub(crate) struct Kernel {
    seed: Option<Binding>,
    ops: Vec<Op>,
    head: Vec<Source>,
    identity_head: Option<usize>,
    slots: usize,
}
fn terms(a: &Atom) -> impl Iterator<Item = Term> + '_ {
    a.args.iter().chain(a.sender.iter()).cloned()
}
fn source(t: &Term, bound: &BTreeMap<VarId, Source>) -> Option<Source> {
    match t {
        Term::Var(v) => bound.get(v).copied(),
        Term::Const(c) => Some(Source::Constant(*c)),
        Term::Wild => None,
    }
}
impl Kernel {
    pub(crate) fn eligible(rule: &Rule) -> bool {
        rule.body
            .lits
            .iter()
            .all(|l| matches!(l, Literal::Pos(a) | Literal::Neg(a) if a.weight.is_none()))
            && rule
                .head
                .args
                .iter()
                .all(|a| matches!(a, HeadArg::Term(Term::Var(_) | Term::Const(_))))
    }
    pub(crate) fn compile(rule: &Rule, plan: &Plan, order: &Order, driver: Option<usize>) -> Option<Self> {
        if !Self::eligible(rule) {
            return None;
        }
        let mut bound = BTreeMap::new();
        let seed = match driver {
            None => None,
            Some(lit) => Some(match rule.body.lits.get(lit)? {
                Literal::Pos(a) => Binding::compile(lit, terms(a), &mut bound),
                Literal::Neg(a) => Binding::compile(
                    lit,
                    a.args.iter().filter(|t| !matches!(t, Term::Wild)).cloned(),
                    &mut bound,
                ),
                _ => return None,
            }),
        };
        let mut ops = Vec::new();
        for step in &order.steps {
            match step {
                Step::Atom {
                    lit,
                    cols,
                    range: None,
                    key: None,
                } => {
                    let Literal::Pos(a) = rule.body.lits.get(*lit)? else {
                        return None;
                    };
                    let ts: Vec<_> = terms(a).collect();
                    let values = cols
                        .iter()
                        .map(|c| source(ts.get(*c)?, &bound))
                        .collect::<Option<Vec<_>>>()?;
                    let mut binding = Binding::compile(*lit, ts.into_iter(), &mut bound);
                    // The exact index probe already proves these equalities. Only row-local
                    // repeated variables still need a comparison.
                    binding.checks.retain(|(col, _)| !cols.contains(col));
                    ops.push(Op::Join {
                        binding,
                        store: atom_store(a),
                        cols: cols.clone(),
                        values,
                    });
                }
                Step::Check(k) => {
                    let lit = *plan.checks.get(*k)?;
                    if Some(lit) == driver {
                        continue;
                    }
                    let Literal::Neg(a) = rule.body.lits.get(lit)? else {
                        return None;
                    };
                    let cols: Vec<_> = a
                        .args
                        .iter()
                        .enumerate()
                        .filter_map(|(c, t)| (!matches!(t, Term::Wild)).then_some(c))
                        .collect();
                    let values = cols
                        .iter()
                        .map(|c| source(a.args.get(*c)?, &bound))
                        .collect::<Option<Vec<_>>>()?;
                    ops.push(Op::Absent {
                        lit,
                        store: StoreKey::Main(a.rel),
                        cols,
                        values,
                    });
                }
                _ => return None,
            }
        }
        let head = rule
            .head
            .args
            .iter()
            .map(|a| match a {
                HeadArg::Term(t) => source(t, &bound),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        let identity_head = match head.first() {
            Some(Source::Column(lit, 0))
                if head
                    .iter()
                    .enumerate()
                    .all(|(i, s)| matches!(s, Source::Column(l,c) if l == lit && *c == i)) =>
            {
                Some(*lit)
            }
            _ => None,
        };
        Some(Self {
            seed,
            ops,
            head,
            identity_head,
            slots: rule.body.lits.len(),
        })
    }
    pub(crate) fn run(
        &self,
        cx: &blossom_ir::core::Program,
        stores: &Stores,
        driver: &Driver,
        old: &dyn Fn(usize) -> bool,
        emit: &mut impl FnMut(Row, &[Option<Row>]) -> Result<(), EvalError>,
    ) -> Result<u64, EvalError> {
        let mut rows = vec![None; self.slots];
        if let Some(seed) = &self.seed {
            let row = match driver {
                Driver::Atom { row, .. } => row.clone(),
                Driver::Neg { key, .. } => Row::from(key.clone()),
                _ => return Err(internal_error!("compiled driver mismatch").into()),
            };
            *rows
                .get_mut(seed.lit)
                .ok_or_else(|| internal_error!("compiled seed slot missing"))? = Some(row);
            if !seed.matches(cx, &rows)? {
                return Ok(0);
            }
        }
        let mut examined = 0;
        self.walk(0, cx, stores, old, &mut rows, emit, &mut examined)?;
        Ok(examined)
    }
    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        pc: usize,
        cx: &blossom_ir::core::Program,
        stores: &Stores,
        old: &dyn Fn(usize) -> bool,
        rows: &mut [Option<Row>],
        emit: &mut impl FnMut(Row, &[Option<Row>]) -> Result<(), EvalError>,
        examined: &mut u64,
    ) -> Result<(), EvalError> {
        let Some(op) = self.ops.get(pc) else {
            if let Some(row) = self
                .identity_head
                .and_then(|lit| rows.get(lit))
                .and_then(Option::as_ref)
                && row.len() == self.head.len()
            {
                return emit(row.clone(), rows);
            }
            let head = self
                .head
                .iter()
                .map(|s| s.read(cx, rows).cloned())
                .collect::<Result<Vec<_>, _>>()?;
            return emit(Row::from(head), rows);
        };
        let (store, cols, sources, lit) = match op {
            Op::Join {
                binding,
                store,
                cols,
                values,
            } => (*store, cols, values, binding.lit),
            Op::Absent {
                lit,
                store,
                cols,
                values,
            } => (*store, cols, values, *lit),
        };
        let values = sources
            .iter()
            .map(|s| s.read(cx, rows).cloned())
            .collect::<Result<Vec<_>, _>>()?;
        let store = stores
            .get(&store)
            .ok_or_else(|| internal_error!("compiled probe store missing"))?;
        match op {
            Op::Absent { .. } => {
                if !store.any(old(lit), cols, &values)? {
                    self.walk(pc + 1, cx, stores, old, rows, emit, examined)?;
                }
            }
            Op::Join { binding, .. } => {
                let candidates = store.rows(old(lit), cols, &values)?;
                *examined += candidates.len() as u64;
                for row in candidates {
                    *rows
                        .get_mut(lit)
                        .ok_or_else(|| internal_error!("compiled join slot missing"))? = Some(row);
                    if binding.matches(cx, rows)? {
                        self.walk(pc + 1, cx, stores, old, rows, emit, examined)?;
                    }
                }
                *rows
                    .get_mut(lit)
                    .ok_or_else(|| internal_error!("compiled join slot missing"))? = None;
            }
        }
        Ok(())
    }
}
