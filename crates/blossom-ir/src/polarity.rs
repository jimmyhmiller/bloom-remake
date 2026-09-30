//! The polarity of lattice reads (SEM-102, LANGUAGE §11.4): which body reads of a program are monotone.
//!
//! A read is a positive atom over a lattice-valued relation or a lookup `V = r[k̄]`. Its value flows through the rule
//! body into guards, generators, bindings and the head; each lattice operation passes it on according to the class of
//! the argument it occupies (the operation catalogue, R04 §2.4): morphisms, monotone operations and thresholds keep
//! the polarity, antitone ones flip it, non-monotone ones make it exact, and so does every plain operator (a
//! threshold's `bool` compared with `==` is not monotone). A read is monotone when every path from it to a guard or
//! a head keeps it monotone; otherwise it is a point of order, like a negation, and must see its relation complete.
//!
//! A generated relation (a handler's header, a block, a helper of `not { … }`, …) holds valuations: a lattice value in
//! it is plain data, not merged. A value that passes through one is followed to its uses in the rules that read the
//! helper, so the read it came from is judged by those uses. Values bound by a generator over a set-like lattice or
//! the keys of a map lattice only appear as the lattice grows: they are data like an atom's columns and stay monotone
//! in any use; the values of a map lattice are lattices and carry the map's polarity.

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::{RelId, RuleId, VarId};
use blossom_value::TypeDef;

use crate::core::{BinOp, Expr, GenSource, HeadArg, Literal, MonoClass, Origin, Pattern, Program, Rule, Term, UnOp};

/// How a read reaches a use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Polarity {
    Monotone,
    Antitone,
    Exact,
}

impl Polarity {
    /// `outer ∘ inner`: the polarity of a value used at polarity `outer` that carries a read at polarity `inner`.
    pub fn then(self, inner: Polarity) -> Polarity {
        match (self, inner) {
            (Polarity::Exact, _) | (_, Polarity::Exact) => Polarity::Exact,
            (Polarity::Monotone, x) => x,
            (Polarity::Antitone, Polarity::Monotone) => Polarity::Antitone,
            (Polarity::Antitone, Polarity::Antitone) => Polarity::Monotone,
        }
    }
}

/// Where a lattice value comes from: a read (a literal of a rule), or a lattice-typed column of a generated relation,
/// which is followed to the reads that fill it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Src {
    Read(RuleId, usize),
    Col(RelId, usize),
}

/// What flows into a value: reads whose value it follows (at a polarity), and reads that make it appear (data).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Flow {
    lat: BTreeSet<(Src, Polarity)>,
    data: BTreeSet<Src>,
}

impl Flow {
    fn union(mut self, other: Flow) -> Flow {
        self.lat.extend(other.lat);
        self.data.extend(other.data);
        self
    }

    /// The flow used at polarity `p`.
    fn at(&self, p: Polarity) -> Flow {
        Flow {
            lat: self.lat.iter().map(|(s, q)| (*s, p.then(*q))).collect(),
            data: self.data.clone(),
        }
    }
}

/// The reads of `p` (rule and body index) that reach some use antitone or exact, through generated relations
/// included: its points of order besides negations and aggregates.
pub fn non_monotone_reads(p: &Program) -> BTreeSet<(RuleId, usize)> {
    let mut uses: BTreeMap<Src, Polarity> = BTreeMap::new();
    let mut into: BTreeMap<(RelId, usize), BTreeSet<(Src, Polarity)>> = BTreeMap::new();
    for (id, rule) in p.rules.iter_enumerated() {
        rule_flows(p, id, rule, &mut uses, &mut into);
    }
    // Follow uses of generated columns back to the reads that fill them.
    let mut out = BTreeSet::new();
    let mut todo: Vec<(Src, Polarity)> = uses.into_iter().collect();
    let mut seen: BTreeSet<(Src, Polarity)> = BTreeSet::new();
    while let Some((src, pol)) = todo.pop() {
        if !seen.insert((src, pol)) {
            continue;
        }
        match src {
            Src::Read(r, i) => {
                if pol != Polarity::Monotone {
                    out.insert((r, i));
                }
            }
            Src::Col(rel, c) => {
                for (s, q) in into.get(&(rel, c)).into_iter().flatten() {
                    todo.push((*s, pol.then(*q)));
                }
            }
        }
    }
    out
}

/// Whether relation `rel`'s column `col` holds lattice values as plain data: a lattice-typed column of a generated
/// relation.
fn plain_lattice_col(p: &Program, rel: RelId, col: usize) -> bool {
    p.rels.get(rel).is_some_and(|r| {
        matches!(r.origin, Origin::Generated { .. })
            && !r.schema.lattice.iter().any(|(c, _)| c.index() == col)
            && r.schema
                .cols
                .get(col)
                .is_some_and(|c| matches!(p.types.get(c.ty), Some(TypeDef::Lattice(_))))
    })
}

fn rule_flows(
    p: &Program,
    id: RuleId,
    rule: &Rule,
    uses: &mut BTreeMap<Src, Polarity>,
    into: &mut BTreeMap<(RelId, usize), BTreeSet<(Src, Polarity)>>,
) {
    let mut cx = Cx {
        p,
        vars: BTreeMap::new(),
        uses,
    };
    for (i, lit) in rule.body.lits.iter().enumerate() {
        match lit {
            Literal::Pos(a) => {
                if let Some(rel) = p.rels.get(a.rel) {
                    for (col, _) in &rel.schema.lattice {
                        if let Some(Term::Var(v)) = a.args.get(col.index()) {
                            cx.vars
                                .entry(*v)
                                .or_default()
                                .lat
                                .insert((Src::Read(id, i), Polarity::Monotone));
                        }
                    }
                }
                for (c, t) in a.args.iter().enumerate() {
                    if let Term::Var(v) = t
                        && plain_lattice_col(p, a.rel, c)
                    {
                        cx.vars
                            .entry(*v)
                            .or_default()
                            .lat
                            .insert((Src::Col(a.rel, c), Polarity::Monotone));
                    }
                }
            }
            Literal::Lookup { var, .. } => {
                cx.vars
                    .entry(*var)
                    .or_default()
                    .lat
                    .insert((Src::Read(id, i), Polarity::Monotone));
            }
            _ => {}
        }
    }
    let is_lattice_var = |v: VarId| {
        rule.body
            .vars
            .get(v)
            .is_some_and(|d| matches!(p.types.get(d.ty), Some(TypeDef::Lattice(_))))
    };
    // Bindings and generators pass flows on; a body is small, so iterate to the fixpoint.
    loop {
        let before = cx.vars.clone();
        for lit in &rule.body.lits {
            match lit {
                Literal::Bind { pat, expr } => {
                    let f = cx.walk(expr, Polarity::Monotone);
                    match pat {
                        Pattern::Var(v) => cx.merge(*v, f),
                        // A refutable or destructuring pattern tests the value: conservatively exact.
                        other => {
                            let f = f.at(Polarity::Exact);
                            let mut vs = BTreeSet::new();
                            pattern_vars(other, &mut vs);
                            for v in vs {
                                cx.merge(v, f.clone());
                            }
                        }
                    }
                }
                Literal::Gen { pat, src } => {
                    let f = cx.gen_source(src);
                    // Elements and keys appear as the source grows (data, from its monotone part); the values of a
                    // map lattice are lattices themselves and follow the map.
                    let data = Flow {
                        lat: BTreeSet::new(),
                        data: f
                            .lat
                            .iter()
                            .filter(|(_, q)| *q == Polarity::Monotone)
                            .map(|(s, _)| *s)
                            .chain(f.data.iter().copied())
                            .collect(),
                    };
                    let mut vs = BTreeSet::new();
                    pattern_vars(pat, &mut vs);
                    for v in vs {
                        if is_lattice_var(v) {
                            cx.merge(v, f.clone());
                        } else {
                            cx.merge(v, data.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        if cx.vars == before {
            break;
        }
    }
    // Uses.
    for lit in &rule.body.lits {
        match lit {
            Literal::Guard(e) => {
                let f = cx.walk(e, Polarity::Monotone);
                cx.record(&f);
            }
            Literal::Bind { pat, expr } => {
                if !matches!(pat, Pattern::Var(_)) {
                    let f = cx.walk(expr, Polarity::Exact);
                    cx.record(&f);
                }
            }
            Literal::Gen { src, .. } => {
                let f = cx.gen_source(src);
                cx.record(&f);
            }
            Literal::Pos(a) | Literal::Neg(a) => {
                let lattice_cols: BTreeSet<usize> = match (lit, p.rels.get(a.rel)) {
                    (Literal::Pos(_), Some(rel)) => rel.schema.lattice.iter().map(|(c, _)| c.index()).collect(),
                    _ => BTreeSet::new(),
                };
                for (c, t) in a.args.iter().enumerate() {
                    let binds_plain_lattice = matches!(lit, Literal::Pos(_)) && plain_lattice_col(p, a.rel, c);
                    if !lattice_cols.contains(&c) && !binds_plain_lattice {
                        let f = cx.term(t, Polarity::Exact);
                        cx.record(&f);
                    }
                }
                if let Some(s) = &a.sender {
                    let f = cx.term(s, Polarity::Exact);
                    cx.record(&f);
                }
            }
            Literal::Lookup { key, .. } => {
                for t in key {
                    let f = cx.term(t, Polarity::Exact);
                    cx.record(&f);
                }
            }
        }
    }
    let head_lattice: BTreeSet<usize> = p
        .rels
        .get(rule.head.rel)
        .map(|r| r.schema.lattice.iter().map(|(c, _)| c.index()).collect())
        .unwrap_or_default();
    for (c, a) in rule.head.args.iter().enumerate() {
        match a {
            HeadArg::Term(t) if plain_lattice_col(p, rule.head.rel, c) => {
                // Into a generated relation: followed to where that column is read.
                let f = cx.term(t, Polarity::Monotone);
                let slot = into.entry((rule.head.rel, c)).or_default();
                slot.extend(f.lat.iter().copied());
                for s in f.data {
                    cx.uses.entry(s).or_insert(Polarity::Monotone);
                }
            }
            HeadArg::Term(t) => {
                let at = if head_lattice.contains(&c) {
                    Polarity::Monotone
                } else {
                    Polarity::Exact
                };
                let f = cx.term(t, at);
                cx.record(&f);
            }
            HeadArg::Agg(agg) => {
                for t in &agg.args {
                    let f = cx.term(t, Polarity::Exact);
                    cx.record(&f);
                }
            }
        }
    }
}

struct Cx<'a> {
    p: &'a Program,
    vars: BTreeMap<VarId, Flow>,
    /// The worst polarity each source reaches a use at.
    uses: &'a mut BTreeMap<Src, Polarity>,
}

impl Cx<'_> {
    fn merge(&mut self, v: VarId, f: Flow) {
        let slot = self.vars.entry(v).or_default();
        *slot = std::mem::take(slot).union(f);
    }

    fn record(&mut self, f: &Flow) {
        for (s, pol) in &f.lat {
            let slot = self.uses.entry(*s).or_insert(Polarity::Monotone);
            *slot = (*slot).max(*pol);
        }
        for s in &f.data {
            self.uses.entry(*s).or_insert(Polarity::Monotone);
        }
    }

    fn term(&self, t: &Term, p: Polarity) -> Flow {
        match t {
            Term::Var(v) => self.vars.get(v).map(|f| f.at(p)).unwrap_or_default(),
            Term::Const(_) | Term::Wild => Flow::default(),
        }
    }

    fn gen_source(&self, src: &GenSource) -> Flow {
        match src {
            GenSource::Lattice(e) | GenSource::Value(e) => self.walk(e, Polarity::Monotone),
            GenSource::Range { lo, hi, .. } => self.walk(lo, Polarity::Exact).union(self.walk(hi, Polarity::Exact)),
            GenSource::TableFn { inputs, .. } => inputs
                .iter()
                .fold(Flow::default(), |acc, t| acc.union(self.term(t, Polarity::Exact))),
        }
    }

    /// The flow into the value of `e`, used at polarity `p`.
    fn walk(&self, e: &Expr, p: Polarity) -> Flow {
        match e {
            Expr::Term(t) => self.term(t, p),
            Expr::Scalar(_) | Expr::Param(_) => Flow::default(),
            Expr::Unary { op: UnOp::Not, arg } => self.walk(arg, Polarity::Antitone.then(p)),
            Expr::Binary {
                op: BinOp::And | BinOp::Or,
                lhs,
                rhs,
            } => self.walk(lhs, p).union(self.walk(rhs, p)),
            Expr::Lattice { op, args } => {
                let classes: Vec<MonoClass> = self
                    .p
                    .lattices
                    .get(op.lattice)
                    .and_then(|l| l.ops.iter().find(|d| d.name == op.op))
                    .map(|d| d.params.iter().map(|(_, c)| *c).collect())
                    .unwrap_or_default();
                let mut out = Flow::default();
                for (i, a) in args.iter().enumerate() {
                    let q = match classes.get(i) {
                        Some(
                            MonoClass::Morphism | MonoClass::Bimorphism | MonoClass::Monotone | MonoClass::Threshold,
                        ) => p,
                        Some(MonoClass::Antitone) => Polarity::Antitone.then(p),
                        // A plain argument, a non-monotone one, or an operation the catalogue lacks.
                        Some(MonoClass::Constant | MonoClass::NonMonotone) | None => Polarity::Exact,
                    };
                    out = out.union(self.walk(a, q));
                }
                out
            }
            // Every other operator reads its operands exactly.
            Expr::Unary { arg, .. } => self.walk(arg, Polarity::Exact),
            Expr::Binary { lhs, rhs, .. } => self.walk(lhs, Polarity::Exact).union(self.walk(rhs, Polarity::Exact)),
            Expr::Call { args, .. } | Expr::Construct { fields: args, .. } | Expr::Collection { elems: args, .. } => {
                args.iter()
                    .fold(Flow::default(), |acc, a| acc.union(self.walk(a, Polarity::Exact)))
            }
            Expr::Field { base, .. } => self.walk(base, Polarity::Exact),
            Expr::If { cond, then, els } => self
                .walk(cond, Polarity::Exact)
                .union(self.walk(then, Polarity::Exact))
                .union(self.walk(els, Polarity::Exact)),
            Expr::Match { scrut, arms } => {
                let mut out = self.walk(scrut, Polarity::Exact);
                for (_, g, body) in arms {
                    if let Some(g) = g {
                        out = out.union(self.walk(g, Polarity::Exact));
                    }
                    out = out.union(self.walk(body, Polarity::Exact));
                }
                out
            }
            Expr::Let { value, body, .. } => self
                .walk(value, Polarity::Exact)
                .union(self.walk(body, Polarity::Exact)),
            Expr::Closure { body, .. } => self.walk(body, Polarity::Exact),
            Expr::Typed { expr, .. } => self.walk(expr, p),
        }
    }
}

fn pattern_vars(p: &Pattern, out: &mut BTreeSet<VarId>) {
    match p {
        Pattern::Var(v) => {
            out.insert(*v);
        }
        Pattern::Wild | Pattern::Const(_) => {}
        Pattern::Tuple(ps) | Pattern::Variant { fields: ps, .. } => ps.iter().for_each(|x| pattern_vars(x, out)),
        Pattern::Struct { fields, .. } => fields.iter().for_each(|(_, x)| pattern_vars(x, out)),
    }
}
