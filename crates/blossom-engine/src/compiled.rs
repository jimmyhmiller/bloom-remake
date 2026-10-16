//! Compiled relational terms. Slots refer to source columns, not cloned variable environments.
//! Compilation fixes every probe, equality constraint and projection for one delta driver/order.
//! Storage snapshots and signed publication remain owned by the incremental engine.
use std::collections::BTreeMap;

use crate::rule::{Driver, Order, Plan, Step, StoreKey, Stores, atom_store};
use blossom_base::{ConstId, VarId, internal_error};
use blossom_ir::{
    core::{Atom, BinOp, Expr, HeadArg, Literal, Pattern, Rule, Term},
    tick::{EvalError, Row},
};
use blossom_value::Value;

#[derive(Clone, Debug)]
enum Source {
    Column(usize, usize),
    Constant(ConstId),
    Field(Box<Source>, usize),
}
impl Source {
    fn read<'a>(&self, cx: &'a blossom_ir::core::Program, rows: &'a [Option<Row>]) -> Result<&'a Value, EvalError> {
        match self {
            Self::Column(lit, col) => rows.get(*lit).and_then(Option::as_ref).and_then(|r| r.get(*col)),
            Self::Constant(id) => cx.consts.get(*id),
            Self::Field(base, index) => match base.read(cx, rows)? {
                Value::Tuple(fields) | Value::Struct(fields) => fields.get(*index),
                _ => None,
            },
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
                        checks.push((col, source.clone()));
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
    Compare {
        op: BinOp,
        lhs: Source,
        rhs: Source,
    },
    Shape {
        source: Source,
        tuple: bool,
        width: usize,
    },
    Join {
        binding: Binding,
        store: StoreKey,
        cols: Vec<usize>,
        values: Vec<Source>,
        range: Option<Range>,
    },
    Absent {
        lit: usize,
        store: StoreKey,
        cols: Vec<usize>,
        values: Vec<Source>,
    },
}
#[derive(Clone, Debug)]
struct Range {
    col: usize,
    lo: Option<(Source, bool)>,
    hi: Option<(Source, bool)>,
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
        Term::Var(v) => bound.get(v).cloned(),
        Term::Const(c) => Some(Source::Constant(*c)),
        Term::Wild => None,
    }
}
fn read_expr(e: &Expr) -> bool {
    match e {
        Expr::Term(Term::Var(_) | Term::Const(_)) => true,
        Expr::Field { base, .. } | Expr::Typed { expr: base, .. } => read_expr(base),
        _ => false,
    }
}
fn expr_source(e: &Expr, bound: &BTreeMap<VarId, Source>) -> Option<Source> {
    match e {
        Expr::Term(t) => source(t, bound),
        Expr::Field { base, index } => Some(Source::Field(Box::new(expr_source(base, bound)?), *index as usize)),
        Expr::Typed { expr, .. } => expr_source(expr, bound),
        _ => None,
    }
}
fn pattern_supported(p: &Pattern) -> bool {
    match p {
        Pattern::Var(_) | Pattern::Wild | Pattern::Const(_) => true,
        Pattern::Tuple(ps) => ps.iter().all(pattern_supported),
        Pattern::Struct { fields, .. } => fields.iter().all(|(_, p)| pattern_supported(p)),
        Pattern::Variant { .. } => false,
    }
}
fn comparison(op: &BinOp) -> bool {
    matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::CanonLt | BinOp::CanonLe
    )
}
fn bind_pattern(p: &Pattern, value: Source, bound: &mut BTreeMap<VarId, Source>, ops: &mut Vec<Op>) -> Option<()> {
    match p {
        Pattern::Wild => {}
        Pattern::Var(v) => {
            if let Some(existing) = bound.get(v) {
                ops.push(Op::Compare {
                    op: BinOp::Eq,
                    lhs: existing.clone(),
                    rhs: value,
                });
            } else {
                bound.insert(*v, value);
            }
        }
        Pattern::Const(c) => ops.push(Op::Compare {
            op: BinOp::Eq,
            lhs: Source::Constant(*c),
            rhs: value,
        }),
        Pattern::Tuple(ps) => {
            ops.push(Op::Shape {
                source: value.clone(),
                tuple: true,
                width: ps.len(),
            });
            for (i, p) in ps.iter().enumerate() {
                bind_pattern(p, Source::Field(Box::new(value.clone()), i), bound, ops)?;
            }
        }
        Pattern::Struct { fields, .. } => {
            let width = fields.iter().map(|(i, _)| *i as usize + 1).max().unwrap_or(0);
            ops.push(Op::Shape {
                source: value.clone(),
                tuple: false,
                width,
            });
            for (i, p) in fields {
                bind_pattern(p, Source::Field(Box::new(value.clone()), *i as usize), bound, ops)?;
            }
        }
        Pattern::Variant { .. } => return None,
    }
    Some(())
}
impl Kernel {
    pub(crate) fn eligible(rule: &Rule) -> bool {
        rule.body.lits.iter().all(|l| match l {
            Literal::Pos(a) | Literal::Neg(a) => a.weight.is_none(),
            Literal::Bind { pat, expr } => pattern_supported(pat) && read_expr(expr),
            Literal::Guard(Expr::Binary { op, lhs, rhs }) => comparison(op) && read_expr(lhs) && read_expr(rhs),
            _ => false,
        }) && rule
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
                    range,
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
                    let range = if let Some(r) = range {
                        let end = |e: &crate::rule::RangeEnd| Some((expr_source(&e.expr, &bound)?, e.inclusive));
                        Some(Range {
                            col: r.col,
                            lo: match &r.lo {
                                Some(e) => Some(end(e)?),
                                None => None,
                            },
                            hi: match &r.hi {
                                Some(e) => Some(end(e)?),
                                None => None,
                            },
                        })
                    } else {
                        None
                    };
                    ops.push(Op::Join {
                        binding,
                        range,
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
                    match rule.body.lits.get(lit)? {
                        Literal::Bind { pat, expr } => {
                            bind_pattern(pat, expr_source(expr, &bound)?, &mut bound, &mut ops)?;
                            continue;
                        }
                        Literal::Guard(Expr::Binary { op, lhs, rhs }) => {
                            ops.push(Op::Compare {
                                op: op.clone(),
                                lhs: expr_source(lhs, &bound)?,
                                rhs: expr_source(rhs, &bound)?,
                            });
                            continue;
                        }
                        _ => {}
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
        let holds = match op {
            Op::Compare { op, lhs, rhs } => {
                let (lhs, rhs) = (lhs.read(cx, rows)?, rhs.read(cx, rows)?);
                Some(match op {
                    BinOp::Eq => lhs == rhs,
                    BinOp::Ne => lhs != rhs,
                    BinOp::Lt | BinOp::CanonLt => lhs < rhs,
                    BinOp::Le | BinOp::CanonLe => lhs <= rhs,
                    BinOp::Gt => lhs > rhs,
                    BinOp::Ge => lhs >= rhs,
                    _ => return Err(internal_error!("unsupported compiled comparison").into()),
                })
            }
            Op::Shape { source, tuple, width } => Some(match (source.read(cx, rows)?, tuple) {
                (Value::Tuple(fields), true) => fields.len() == *width,
                (Value::Struct(fields), false) => fields.len() >= *width,
                _ => false,
            }),
            _ => None,
        };
        if let Some(holds) = holds {
            if holds {
                self.walk(pc + 1, cx, stores, old, rows, emit, examined)?;
            }
            return Ok(());
        }
        let (store, cols, sources, lit) = match op {
            Op::Join {
                binding,
                store,
                cols,
                values,
                ..
            } => (*store, cols, values, binding.lit),
            Op::Absent {
                lit,
                store,
                cols,
                values,
            } => (*store, cols, values, *lit),
            _ => return Err(internal_error!("compiled non-probe reached probe dispatch").into()),
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
            Op::Join { binding, range, .. } => {
                use std::ops::Bound;
                let end = |end: &Option<(Source, bool)>| -> Result<Option<Bound<Value>>, EvalError> {
                    let Some((source, inclusive)) = end else {
                        return Ok(Some(Bound::Unbounded));
                    };
                    let value = source.read(cx, rows)?;
                    Ok(
                        matches!(value, Value::Int(_) | Value::Duration(_) | Value::Instant(_)).then(|| {
                            if *inclusive {
                                Bound::Included(value.clone())
                            } else {
                                Bound::Excluded(value.clone())
                            }
                        }),
                    )
                };
                let bounds = match range {
                    Some(r) => match (end(&r.lo)?, end(&r.hi)?) {
                        (Some(lo), Some(hi)) => Some((r.col, lo, hi)),
                        _ => None,
                    },
                    None => None,
                };
                let candidates = match bounds {
                    Some((col, lo, hi)) => store.range_rows(old(lit), cols, &values, col, lo, hi)?,
                    None => store.rows(old(lit), cols, &values)?,
                };
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
            _ => return Err(internal_error!("compiled non-probe reached probe execution").into()),
        }
        Ok(())
    }
}
