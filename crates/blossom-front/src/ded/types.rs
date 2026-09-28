//! Type inference for `.ded` programs (LANGUAGE §21.1: "Types are inferred").
//!
//! Every column of every relation, every variable of every rule and every literal gets a type variable; body atoms,
//! heads, comparisons and arithmetic unify them. Molly's INT, STRING and LOCATION become `i64`, `String` and `Node`:
//!
//! - the first column of a protocol relation is its location, a `Node`; so are the first two columns of `crash`,
//!   whose third is a time (`i64`);
//! - an integer literal is `i64` unless it meets a `u64` (the result of `count<X>`, as the IR's `count` yields);
//! - a string literal is a `String` unless it meets a `Node`, where it names a node of the deployment;
//! - arithmetic and ordering comparisons take integers of one type;
//! - a column that nothing constrains (a relation that is read but never written, whose variables meet no other
//!   column) holds no values, and is typed `String`.

use std::collections::BTreeMap;

use blossom_artifact::ded::DedRelKind;
use blossom_base::{Diagnostic, Diagnostics, Span, Symbol, code};
use blossom_syntax::ded::{AggFunc, Arg, BodyItem, Expr, Rule, Term};

use super::load::Program;
use super::model::{CRASH, Model, body_atoms};

/// A resolved column, variable or literal type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ColTy {
    I64,
    U64,
    Str,
    Node,
}

impl ColTy {
    fn describe(self) -> &'static str {
        match self {
            ColTy::I64 => "i64",
            ColTy::U64 => "u64",
            ColTy::Str => "String",
            ColTy::Node => "Node",
        }
    }
}

/// The inferred types.
#[derive(Debug, Default)]
pub(crate) struct Types {
    /// Per relation (in `Model::rels` order), per column.
    pub cols: Vec<Vec<ColTy>>,
    /// The type of every term occurrence and every binary expression, by span.
    pub at: BTreeMap<Span, ColTy>,
}

impl Types {
    pub fn col(&self, rel: usize, col: usize) -> Option<ColTy> {
        self.cols.get(rel).and_then(|c| c.get(col)).copied()
    }
}

/// A type variable's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ty {
    Unknown,
    IntLit,
    StrLit,
    Known(ColTy),
}

impl Ty {
    fn describe(self) -> &'static str {
        match self {
            Ty::Unknown => "unknown",
            Ty::IntLit => "an integer",
            Ty::StrLit => "a string",
            Ty::Known(t) => t.describe(),
        }
    }

    fn join(self, other: Ty) -> Option<Ty> {
        use ColTy::*;
        use Ty::*;
        Some(match (self, other) {
            (Unknown, x) | (x, Unknown) => x,
            (IntLit, IntLit) => IntLit,
            (StrLit, StrLit) => StrLit,
            (IntLit, Known(t @ (I64 | U64))) | (Known(t @ (I64 | U64)), IntLit) => Known(t),
            (StrLit, Known(t @ (Str | Node))) | (Known(t @ (Str | Node)), StrLit) => Known(t),
            (Known(a), Known(b)) if a == b => Known(a),
            _ => return None,
        })
    }

    fn resolve(self) -> ColTy {
        match self {
            Ty::IntLit => ColTy::I64,
            Ty::StrLit | Ty::Unknown => ColTy::Str,
            Ty::Known(t) => t,
        }
    }
}

/// Union-find over type variables, each root carrying its type and the span that fixed it.
struct Uf {
    parent: Vec<usize>,
    ty: Vec<Ty>,
    why: Vec<Option<Span>>,
}

impl Uf {
    fn fresh(&mut self, ty: Ty, why: Option<Span>) -> usize {
        let id = self.parent.len();
        self.parent.push(id);
        self.ty.push(ty);
        self.why.push(why);
        id
    }

    fn find(&mut self, mut x: usize) -> usize {
        let mut root = x;
        while let Some(&p) = self.parent.get(root) {
            if p == root {
                break;
            }
            root = p;
        }
        while let Some(&p) = self.parent.get(x) {
            if p == root {
                break;
            }
            if let Some(slot) = self.parent.get_mut(x) {
                *slot = root;
            }
            x = p;
        }
        root
    }

    fn ty(&mut self, x: usize) -> Ty {
        let r = self.find(x);
        self.ty.get(r).copied().unwrap_or(Ty::Unknown)
    }

    fn unify(&mut self, a: usize, b: usize, span: Span, what: &str, diags: &mut Diagnostics) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        let (ta, tb) = (self.ty(ra), self.ty(rb));
        match ta.join(tb) {
            Some(t) => {
                let why = self
                    .why
                    .get(ra)
                    .copied()
                    .flatten()
                    .or(self.why.get(rb).copied().flatten());
                if let Some(slot) = self.parent.get_mut(rb) {
                    *slot = ra;
                }
                if let Some(slot) = self.ty.get_mut(ra) {
                    *slot = t;
                }
                if let Some(slot) = self.why.get_mut(ra) {
                    *slot = why;
                }
            }
            None => {
                let mut d = Diagnostic::new(
                    code!("BLS0300"),
                    format!("type mismatch in {what}: {} and {}", ta.describe(), tb.describe()),
                )
                .with_primary(span);
                for (root, ty) in [(ra, ta), (rb, tb)] {
                    if let Some(w) = self.why.get(root).copied().flatten() {
                        d = d.with_label(w, format!("{} because of this", ty.describe()));
                    }
                }
                diags.push(d);
            }
        }
    }

    fn constrain(&mut self, a: usize, ty: Ty, span: Span, what: &str, diags: &mut Diagnostics) {
        let b = self.fresh(ty, Some(span));
        self.unify(a, b, span, what, diags);
    }
}

pub(crate) fn infer(program: &Program, model: &Model, diags: &mut Diagnostics) -> Types {
    let mut uf = Uf {
        parent: Vec::new(),
        ty: Vec::new(),
        why: Vec::new(),
    };
    let mut slots: Vec<Vec<usize>> = Vec::with_capacity(model.rels.len());
    for rel in &model.rels {
        let mut cols = Vec::with_capacity(rel.arity);
        for c in 0..rel.arity {
            let fixed = match (rel.kind, c) {
                (DedRelKind::Protocol, 0) | (DedRelKind::Crash, 0 | 1) => Ty::Known(ColTy::Node),
                (DedRelKind::Crash, 2) => Ty::Known(ColTy::I64),
                _ => Ty::Unknown,
            };
            let why = (fixed != Ty::Unknown).then_some(rel.first);
            cols.push(uf.fresh(fixed, why));
        }
        slots.push(cols);
    }
    let mut cx = Cx {
        uf,
        slots,
        model,
        occurrences: Vec::new(),
        diags,
    };
    for fact in &program.facts {
        for (i, t) in fact.args.iter().enumerate() {
            if let Some(slot) = cx.slot(fact.rel.text, i) {
                let n = cx.term(t, &mut BTreeMap::new());
                cx.unify(n, slot, t.span(), &format!("column {} of `{}`", i + 1, fact.rel.text));
            }
        }
    }
    for rule in &program.rules {
        cx.rule(rule);
    }
    let Cx {
        mut uf,
        slots,
        occurrences,
        ..
    } = cx;
    let mut types = Types::default();
    for cols in &slots {
        types.cols.push(cols.iter().map(|&n| uf.ty(n).resolve()).collect());
    }
    for (span, n) in occurrences {
        types.at.insert(span, uf.ty(n).resolve());
    }
    types
}

struct Cx<'a> {
    uf: Uf,
    slots: Vec<Vec<usize>>,
    model: &'a Model,
    occurrences: Vec<(Span, usize)>,
    diags: &'a mut Diagnostics,
}

impl Cx<'_> {
    fn slot(&self, rel: Symbol, col: usize) -> Option<usize> {
        let i = self.model.index(rel)?;
        self.slots.get(i).and_then(|c| c.get(col)).copied()
    }

    fn unify(&mut self, a: usize, b: usize, span: Span, what: &str) {
        self.uf.unify(a, b, span, what, self.diags);
    }

    fn term(&mut self, t: &Term, vars: &mut BTreeMap<Symbol, usize>) -> usize {
        let n = match t {
            Term::Var(v) => match vars.get(&v.text) {
                Some(&n) => n,
                None => {
                    let n = self.uf.fresh(Ty::Unknown, None);
                    vars.insert(v.text, n);
                    n
                }
            },
            Term::Wild(_) => self.uf.fresh(Ty::Unknown, None),
            Term::Int(_, s) => self.uf.fresh(Ty::IntLit, Some(*s)),
            Term::Str(_, s) => self.uf.fresh(Ty::StrLit, Some(*s)),
        };
        self.occurrences.push((t.span(), n));
        n
    }

    /// The type variable of a value expression: a term, or arithmetic over integers.
    fn value(&mut self, e: &Expr, vars: &mut BTreeMap<Symbol, usize>) -> usize {
        match e {
            Expr::Term(t) => self.term(t, vars),
            Expr::Binary { lhs, op, rhs, span } => {
                let l = self.term(lhs, vars);
                let r = self.value(rhs, vars);
                if op.is_comparison() {
                    self.diags.push(
                        Diagnostic::new(
                            code!("BLS0300"),
                            format!("`{}` yields a truth value, but a value is needed here", op.text()),
                        )
                        .with_primary(*span),
                    );
                } else {
                    self.uf
                        .constrain(l, Ty::IntLit, *span, &format!("`{}`", op.text()), self.diags);
                    self.unify(l, r, *span, &format!("`{}`", op.text()));
                }
                self.occurrences.push((*span, l));
                l
            }
        }
    }

    fn rule(&mut self, rule: &Rule) {
        let mut vars = BTreeMap::new();
        for atom in body_atoms(rule) {
            let rel = atom.rel.text;
            for (i, arg) in atom.args.iter().enumerate() {
                let Arg::Expr(Expr::Term(t)) = arg else {
                    continue;
                };
                let n = self.term(t, &mut vars);
                if let Some(slot) = self.slot(rel, i) {
                    let what = if rel.as_str() == CRASH {
                        format!("column {} of `crash(Observer, Node, Time)`", i + 1)
                    } else {
                        format!("column {} of `{rel}`", i + 1)
                    };
                    self.unify(n, slot, t.span(), &what);
                }
            }
        }
        for item in &rule.body {
            let BodyItem::Qual(Expr::Binary { lhs, op, rhs, span }) = item else {
                continue;
            };
            let l = self.term(lhs, &mut vars);
            let r = self.value(rhs, &mut vars);
            let what = format!("`{}`", op.text());
            if !matches!(op, blossom_syntax::ded::BinOp::Eq | blossom_syntax::ded::BinOp::Ne) {
                self.uf.constrain(l, Ty::IntLit, *span, &what, self.diags);
            }
            self.unify(l, r, *span, &what);
        }
        let head = rule.head.rel.text;
        for (i, arg) in rule.head.args.iter().enumerate() {
            let Some(slot) = self.slot(head, i) else {
                continue;
            };
            let what = format!("column {} of `{head}`", i + 1);
            match arg {
                Arg::Expr(e) => {
                    let n = self.value(e, &mut vars);
                    self.unify(n, slot, e.span(), &what);
                }
                Arg::Agg(g) => {
                    let v = self.term(&Term::Var(g.var), &mut vars);
                    match g.func {
                        AggFunc::Count => {
                            self.uf
                                .constrain(slot, Ty::Known(ColTy::U64), g.span, &what, self.diags);
                        }
                        AggFunc::Min | AggFunc::Max | AggFunc::Sum => {
                            self.uf
                                .constrain(v, Ty::IntLit, g.span, &format!("`{}<…>`", g.func.name()), self.diags);
                            self.unify(v, slot, g.span, &what);
                        }
                    }
                }
            }
        }
    }
}
