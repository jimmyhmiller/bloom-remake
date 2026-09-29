//! The polarity of lattice reads (SEM-102, LANGUAGE §11.4): which body reads of a rule are monotone.
//!
//! A read is a positive atom over a lattice-valued relation or a lookup `V = r[k̄]`. Its value flows through the rule
//! body into guards, generators, bindings and the head; each lattice operation passes it on according to the class of
//! the argument it occupies (the operation catalogue, R04 §2.4): morphisms, monotone operations and thresholds keep
//! the polarity, antitone ones flip it, non-monotone ones make it exact, and so does every plain operator (a
//! threshold's `bool` compared with `==` is not monotone). A read is monotone when every path from it to a guard or
//! the head keeps it monotone; otherwise it is a point of order, like a negation, and must see its relation complete.
//!
//! Values bound by a generator over a set-like lattice or a map (`x in s`) only appear as the lattice grows: they are
//! data like an atom's columns and stay monotone in any use.

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::VarId;

use crate::core::{BinOp, Expr, GenSource, HeadArg, Literal, MonoClass, Pattern, Program, Rule, Term, UnOp};

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

/// What flows into a value: reads whose value it follows (at a polarity), and reads that make it appear (data).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Flow {
    lat: BTreeSet<(usize, Polarity)>,
    data: BTreeSet<usize>,
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
            lat: self.lat.iter().map(|(l, q)| (*l, p.then(*q))).collect(),
            data: self.data.clone(),
        }
    }
}

/// The literals of `rule` (by body index) that read a relation non-monotonically: a lattice read that reaches a guard
/// or the head antitone or exact.
pub fn non_monotone_reads(p: &Program, rule: &Rule) -> BTreeSet<usize> {
    let mut cx = Cx {
        p,
        vars: BTreeMap::new(),
        reads: BTreeMap::new(),
    };
    for (i, lit) in rule.body.lits.iter().enumerate() {
        match lit {
            Literal::Pos(a) => {
                if let Some(rel) = p.rels.get(a.rel) {
                    for (col, _) in &rel.schema.lattice {
                        if let Some(Term::Var(v)) = a.args.get(col.index()) {
                            cx.vars.entry(*v).or_default().lat.insert((i, Polarity::Monotone));
                        }
                    }
                }
            }
            Literal::Lookup { var, .. } => {
                cx.vars.entry(*var).or_default().lat.insert((i, Polarity::Monotone));
            }
            _ => {}
        }
    }
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
                    let f = match src {
                        GenSource::Lattice(e) | GenSource::Value(e) => cx.walk(e, Polarity::Monotone),
                        GenSource::Range { lo, hi, .. } => {
                            cx.walk(lo, Polarity::Exact).union(cx.walk(hi, Polarity::Exact))
                        }
                        GenSource::TableFn { inputs, .. } => inputs
                            .iter()
                            .fold(Flow::default(), |acc, t| acc.union(cx.term(t, Polarity::Exact))),
                    };
                    // Generated elements appear as the source grows: data, from the monotone part of the source.
                    let data = Flow {
                        lat: BTreeSet::new(),
                        data: f
                            .lat
                            .iter()
                            .filter(|(_, q)| *q == Polarity::Monotone)
                            .map(|(l, _)| *l)
                            .chain(f.data.iter().copied())
                            .collect(),
                    };
                    let mut vs = BTreeSet::new();
                    pattern_vars(pat, &mut vs);
                    for v in vs {
                        cx.merge(v, data.clone());
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
                let f = match src {
                    GenSource::Lattice(e) | GenSource::Value(e) => cx.walk(e, Polarity::Monotone),
                    GenSource::Range { lo, hi, .. } => cx.walk(lo, Polarity::Exact).union(cx.walk(hi, Polarity::Exact)),
                    GenSource::TableFn { inputs, .. } => inputs
                        .iter()
                        .fold(Flow::default(), |acc, t| acc.union(cx.term(t, Polarity::Exact))),
                };
                cx.record(&f);
            }
            Literal::Pos(a) | Literal::Neg(a) => {
                let lattice_cols: BTreeSet<usize> = match (lit, p.rels.get(a.rel)) {
                    (Literal::Pos(_), Some(rel)) => rel.schema.lattice.iter().map(|(c, _)| c.index()).collect(),
                    _ => BTreeSet::new(),
                };
                for (c, t) in a.args.iter().enumerate() {
                    if !lattice_cols.contains(&c) {
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
        let at = if head_lattice.contains(&c) {
            Polarity::Monotone
        } else {
            Polarity::Exact
        };
        match a {
            HeadArg::Term(t) => {
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
    cx.reads
        .into_iter()
        .filter(|(_, pol)| *pol != Polarity::Monotone)
        .map(|(l, _)| l)
        .collect()
}

struct Cx<'a> {
    p: &'a Program,
    vars: BTreeMap<VarId, Flow>,
    /// The worst polarity each read reaches a use at.
    reads: BTreeMap<usize, Polarity>,
}

impl Cx<'_> {
    fn merge(&mut self, v: VarId, f: Flow) {
        let slot = self.vars.entry(v).or_default();
        *slot = std::mem::take(slot).union(f);
    }

    fn record(&mut self, f: &Flow) {
        for (l, pol) in &f.lat {
            let slot = self.reads.entry(*l).or_insert(Polarity::Monotone);
            *slot = (*slot).max(*pol);
        }
        for l in &f.data {
            self.reads.entry(*l).or_insert(Polarity::Monotone);
        }
    }

    fn term(&self, t: &Term, p: Polarity) -> Flow {
        match t {
            Term::Var(v) => self.vars.get(v).map(|f| f.at(p)).unwrap_or_default(),
            Term::Const(_) | Term::Wild => Flow::default(),
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
