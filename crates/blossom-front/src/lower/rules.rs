//! Bodies, handlers, statements and views (LANGUAGE §8, §9, §10.1–10.2).
//!
//! A body is lowered into one or more drafts: `outer` and `any` split a draft into one per case (§9.6, §9.7), so a
//! header or a view alternative with them is defined by several rules. Positive literals are lowered first, then
//! negative ones, because a negative literal's helper relation (`not { … }`, `forall`) is defined over the same
//! positive context (§9.3, §9.8): its defining rule repeats the enclosing positive literals, which keeps it range
//! restricted whatever the braces mention.

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::span::stable_hash_hex8;
use blossom_base::{InternalError, QualName, RelId, Span, Symbol, TypeId, internal_error};
use blossom_ir::core::{
    self as ir, AggCall, AggFunc, ConstructKind, Expr, GenSource, Head, HeadArg, HeadMode, Literal, Pattern, RuleKind,
    Term,
};
use blossom_value::value::IntValue;
use blossom_value::{TypeDef, Value};

use super::expr::{Draft, try_const};
use super::{Lowerer, atom as ir_atom, column, ir, suffixed, surface};
use crate::ast::Verb;
use crate::hir::*;

/// What a helper relation's defining rule repeats: a parent relation's atom, or a body's positive literals.
#[derive(Clone)]
pub(crate) enum Given<'a> {
    Rel { rel: RelId, vars: Vec<HVarId>, span: Span },
    Lits(&'a HBody),
}

/// The variables a body binds for the literals after it: positive atoms, lets, generators, and the variables every
/// alternative of an `any` binds; in order of first occurrence.
pub(crate) fn binders(body: &HBody) -> Vec<HVarId> {
    fn pat(p: &HPat, out: &mut Vec<HVarId>) {
        match p {
            HPat::Var(v, _) => {
                if !out.contains(v) {
                    out.push(*v);
                }
            }
            HPat::Tuple(ps, _) | HPat::Variant { fields: ps, .. } => ps.iter().for_each(|x| pat(x, out)),
            HPat::Wild(_) | HPat::Expr(_) => {}
        }
    }
    let mut out = Vec::new();
    for l in &body.lits {
        match l {
            HLit::Atom(a) | HLit::Outer(a) | HLit::Delta { atom: a, .. } | HLit::Per(a) => {
                a.args.iter().for_each(|p| pat(p, &mut out));
                if let Some(f) = &a.from {
                    pat(f, &mut out);
                }
            }
            HLit::Let { pat: p, .. }
            | HLit::RangeGen { pat: p, .. }
            | HLit::RoleGen { pat: p, .. }
            | HLit::Gen { pat: p, .. } => pat(p, &mut out),
            HLit::Any(alts, _) => {
                let sets: Vec<Vec<HVarId>> = alts.iter().map(binders).collect();
                if let Some(first) = sets.first() {
                    for v in first {
                        if sets.iter().all(|s| s.contains(v)) && !out.contains(v) {
                            out.push(*v);
                        }
                    }
                }
            }
            HLit::Not(_) | HLit::NotBody(..) | HLit::Guard(_) | HLit::Forall { .. } | HLit::Choose(_) => {}
        }
    }
    out
}

/// Every variable a body mentions (for the correlated variables of `not { … }` and `forall`).
fn mentioned(body: &HBody, out: &mut BTreeSet<HVarId>) {
    for l in &body.lits {
        mentioned_lit(l, out);
    }
}

fn mentioned_lit(l: &HLit, out: &mut BTreeSet<HVarId>) {
    match l {
        HLit::Atom(a) | HLit::Not(a) | HLit::Outer(a) | HLit::Delta { atom: a, .. } | HLit::Per(a) => {
            a.args.iter().for_each(|p| mentioned_pat(p, out));
            if let Some(f) = &a.from {
                mentioned_pat(f, out);
            }
        }
        HLit::NotBody(b, _) => mentioned(b, out),
        HLit::Let { pat, expr, .. } => {
            mentioned_pat(pat, out);
            mentioned_expr(expr, out);
        }
        HLit::Guard(e) => mentioned_expr(e, out),
        HLit::RangeGen { pat, lo, hi, .. } => {
            mentioned_pat(pat, out);
            mentioned_expr(lo, out);
            mentioned_expr(hi, out);
        }
        HLit::RoleGen { pat, .. } => mentioned_pat(pat, out),
        HLit::Gen { pat, src, .. } => {
            mentioned_pat(pat, out);
            mentioned_expr(src, out);
        }
        HLit::Choose(c) => {
            for e in c.chosen.iter().chain(&c.per).chain(c.cost.iter().map(|(e, _)| e)) {
                mentioned_expr(e, out);
            }
        }
        HLit::Any(alts, _) => alts.iter().for_each(|b| mentioned(b, out)),
        HLit::Forall { domain, body, .. } => {
            mentioned_lit(domain, out);
            mentioned(body, out);
        }
    }
}

fn mentioned_pat(p: &HPat, out: &mut BTreeSet<HVarId>) {
    match p {
        HPat::Var(v, _) => {
            out.insert(*v);
        }
        HPat::Expr(e) => mentioned_expr(e, out),
        HPat::Tuple(ps, _) | HPat::Variant { fields: ps, .. } => ps.iter().for_each(|x| mentioned_pat(x, out)),
        HPat::Wild(_) => {}
    }
}

fn mentioned_expr(e: &HExpr, out: &mut BTreeSet<HVarId>) {
    match &e.kind {
        HExprKind::Var(v) => {
            out.insert(*v);
        }
        HExprKind::Binary { lhs, rhs, .. } => {
            mentioned_expr(lhs, out);
            mentioned_expr(rhs, out);
        }
        HExprKind::Prefix { arg, .. } | HExprKind::Cast { expr: arg, .. } => mentioned_expr(arg, out),
        HExprKind::TupleIndex { base, .. } | HExprKind::Field { base, .. } => mentioned_expr(base, out),
        HExprKind::Tuple(es) | HExprKind::Variant { fields: es, .. } | HExprKind::Struct { fields: es, .. } => {
            es.iter().for_each(|x| mentioned_expr(x, out));
        }
        HExprKind::Builtin { args, .. }
        | HExprKind::Collection { elems: args, .. }
        | HExprKind::LatCtor { args, .. }
        | HExprKind::LatOp { args, .. }
        | HExprKind::Lookup { key: args, .. } => args.iter().for_each(|x| mentioned_expr(x, out)),
        HExprKind::In { elem, coll } => {
            mentioned_expr(elem, out);
            mentioned_expr(coll, out);
        }
        HExprKind::Method { recv, args, .. } => {
            mentioned_expr(recv, out);
            args.iter().for_each(|x| mentioned_expr(x, out));
        }
        HExprKind::Lift { expr, .. } => mentioned_expr(expr, out),
        HExprKind::If { cond, then, els } => {
            mentioned_expr(cond, out);
            mentioned_expr(then, out);
            mentioned_expr(els, out);
        }
        HExprKind::Match { scrut, arms } => {
            mentioned_expr(scrut, out);
            for (p, g, b) in arms {
                mentioned_pat(p, out);
                if let Some(g) = g {
                    mentioned_expr(g, out);
                }
                mentioned_expr(b, out);
            }
        }
        HExprKind::Call { args, .. } | HExprKind::CallParam { args, .. } | HExprKind::GenericCall { args, .. } => {
            args.iter().for_each(|x| mentioned_expr(x, out))
        }
        HExprKind::Let { pat, value, body, .. } => {
            mentioned_pat(pat, out);
            mentioned_expr(value, out);
            mentioned_expr(body, out);
        }
        HExprKind::Closure { params, body } => {
            out.extend(params.iter().copied());
            mentioned_expr(body, out);
        }
        HExprKind::Value(..)
        | HExprKind::IntLit(..)
        | HExprKind::TypedInt(..)
        | HExprKind::SelfNode
        | HExprKind::Now
        | HExprKind::Tick => {}
    }
}

/// Naming for the helper relations and rules of one rule body.
pub(crate) struct Names {
    /// The module path.
    pub module: QualName,
    /// The rule-label base, e.g. `chat.data::receive`.
    pub base: String,
    /// The relation-name stem, e.g. `receive`.
    pub stem: String,
    pub role: Option<HRoleId>,
    pub counter: u32,
}

impl Names {
    pub(crate) fn rel_segments(&self, suffix: &str) -> Vec<Symbol> {
        let mut segs = self.module.segments().to_vec();
        segs.push(Symbol::intern(&format!("{}{suffix}", self.stem)));
        segs
    }
}

fn int_zero(hir_types: &blossom_value::TypeTable, ty: TypeId) -> Option<Value> {
    match hir_types.get(ty) {
        Some(TypeDef::Int(t)) => IntValue::from_i128(*t, 0).map(Value::Int),
        _ => None,
    }
}

impl<'h> Lowerer<'h> {
    fn var_ty(&self, scope: ScopeId, v: HVarId) -> Result<TypeId, InternalError> {
        self.hir
            .var_types
            .get(scope.index())
            .and_then(|t| t.get(v.index()))
            .copied()
            .ok_or_else(|| internal_error!("variable {v:?} has no type"))
    }

    fn var_terms(&mut self, d: &mut Draft, vars: &[HVarId]) -> Result<Vec<Term>, InternalError> {
        vars.iter().map(|v| d.var(self.hir, *v).map(Term::Var)).collect()
    }

    /// Lowers the `given` context into each draft (positive literals only).
    fn given(
        &mut self,
        drafts: Vec<Draft>,
        given: &[Given<'_>],
        names: &mut Names,
    ) -> Result<Vec<Draft>, InternalError> {
        let mut drafts = drafts;
        for g in given {
            match g {
                Given::Rel { rel, vars, span } => {
                    for d in &mut drafts {
                        let args = self.var_terms(d, vars)?;
                        d.lits.push(Literal::Pos(ir_atom(*rel, args, *span)));
                    }
                }
                Given::Lits(body) => drafts = self.positives(drafts, body, names)?,
            }
        }
        Ok(drafts)
    }

    /// Lowers a whole body: positive literals, then negative ones (whose helpers repeat `given` and the positives).
    pub(crate) fn body(
        &mut self,
        drafts: Vec<Draft>,
        body: &HBody,
        given: &[Given<'_>],
        names: &mut Names,
    ) -> Result<Vec<Draft>, InternalError> {
        let mut drafts = self.positives(drafts, body, names)?;
        let mut context: Vec<Given<'_>> = given.to_vec();
        context.push(Given::Lits(body));
        for l in &body.lits {
            match l {
                HLit::Not(a) => {
                    for d in &mut drafts {
                        let at = self.atom(d, a.rel, &a.args, a.from.as_ref(), true, a.span)?;
                        d.lits.push(Literal::Neg(at));
                    }
                }
                HLit::NotBody(inner, span) => {
                    for d in &mut drafts {
                        self.not_body(d, inner, *span, &context, names)?;
                    }
                }
                HLit::Forall { domain, body: fb, span } => {
                    for d in &mut drafts {
                        self.forall(d, domain, fb, *span, &context, names)?;
                    }
                }
                _ => {}
            }
        }
        // A choice filters the valuations of everything else in the body.
        for l in &body.lits {
            if let HLit::Choose(c) = l {
                for d in &mut drafts {
                    self.choose(d, c, names)?;
                }
            }
        }
        Ok(drafts)
    }

    /// The positive literals of a body.
    fn positives(&mut self, drafts: Vec<Draft>, body: &HBody, names: &mut Names) -> Result<Vec<Draft>, InternalError> {
        // Variables bound other than by `outer`: those already in the draft, and the other literals' binders.
        let non_outer: BTreeSet<HVarId> = {
            let without_outer = HBody {
                lits: body
                    .lits
                    .iter()
                    .filter(|l| !matches!(l, HLit::Outer(_)))
                    .cloned()
                    .collect(),
                span: None,
            };
            binders(&without_outer).into_iter().collect()
        };
        let mut drafts = drafts;
        for l in &body.lits {
            match l {
                HLit::Atom(a) | HLit::Per(a) => {
                    for d in &mut drafts {
                        let at = self.atom(d, a.rel, &a.args, a.from.as_ref(), false, a.span)?;
                        d.lits.push(Literal::Pos(at));
                    }
                }
                HLit::Let { pat, expr, .. } => {
                    for d in &mut drafts {
                        let x = self.expr(d, expr)?;
                        let mut post = Vec::new();
                        let p = self.pattern(d, pat, &mut post)?;
                        d.lits.push(Literal::Bind { pat: p, expr: x });
                        d.lits.extend(post);
                    }
                }
                // `a && b` is two checks, so the one that cannot fail can run first (LANGUAGE §9.14).
                HLit::Guard(e) => {
                    for d in &mut drafts {
                        let x = self.expr(d, e)?;
                        split_and(x, &mut d.lits);
                    }
                }
                HLit::RangeGen { pat, lo, hi, kind, .. } => {
                    for d in &mut drafts {
                        let lo = self.expr(d, lo)?;
                        let hi = self.expr(d, hi)?;
                        let mut post = Vec::new();
                        let p = self.pattern(d, pat, &mut post)?;
                        d.lits.push(Literal::Gen {
                            pat: p,
                            src: GenSource::Range {
                                lo,
                                hi,
                                kind: match kind {
                                    RangeKind::HalfOpen => ir::RangeKind::HalfOpen,
                                    RangeKind::Closed => ir::RangeKind::Closed,
                                    RangeKind::OpenOpen => ir::RangeKind::OpenOpen,
                                    RangeKind::OpenClosed => ir::RangeKind::OpenClosed,
                                },
                                ring_bits: None,
                            },
                        });
                        d.lits.extend(post);
                    }
                }
                HLit::Gen { pat, src, .. } => {
                    let over_lattice = src.ty.and_then(|t| self.lattice_id(t)).is_some();
                    for d in &mut drafts {
                        let x = self.expr(d, src)?;
                        let mut post = Vec::new();
                        let p = self.pattern(d, pat, &mut post)?;
                        d.lits.push(Literal::Gen {
                            pat: p,
                            src: if over_lattice {
                                GenSource::Lattice(x)
                            } else {
                                GenSource::Value(x)
                            },
                        });
                        d.lits.extend(post);
                    }
                }
                HLit::RoleGen { pat, members, span, .. } => {
                    for d in &mut drafts {
                        let at = self.atom(d, *members, std::slice::from_ref(pat), None, false, *span)?;
                        d.lits.push(Literal::Pos(at));
                    }
                }
                HLit::Delta { inserted, atom } => {
                    let prev = self.prev_rel(atom.rel, atom.span)?;
                    for d in &mut drafts {
                        // Wildcards become variables: the delta compares whole tuples.
                        let mut args = atom.args.clone();
                        let mut fresh = Vec::new();
                        for (c, p) in args.iter_mut().enumerate() {
                            if matches!(p, HPat::Wild(_)) {
                                fresh.push(c);
                            }
                        }
                        let now = self.atom(d, atom.rel, &args, atom.from.as_ref(), false, atom.span)?;
                        let mut now = now;
                        for c in fresh {
                            let col = self.ir_col(atom.rel, c)?;
                            let ty = self.b_col_ty(now.rel, col)?;
                            let f = d.fresh(ty);
                            if let Some(slot) = now.args.get_mut(col) {
                                *slot = Term::Var(f);
                            }
                        }
                        let mut before = now.clone();
                        before.rel = prev;
                        before.sender = None;
                        if *inserted {
                            d.lits.push(Literal::Pos(now));
                            d.lits.push(Literal::Neg(before));
                        } else {
                            let mut now_neg = now;
                            now_neg.sender = None;
                            d.lits.push(Literal::Pos(before));
                            d.lits.push(Literal::Neg(now_neg));
                        }
                    }
                }
                HLit::Outer(a) => {
                    let mut out = Vec::new();
                    for d in drafts {
                        let (some, none) = self.outer(d, a, &non_outer)?;
                        out.push(some);
                        out.push(none);
                    }
                    drafts = out;
                }
                HLit::Any(alts, _) => {
                    let mut out = Vec::new();
                    for d in drafts {
                        for alt in alts {
                            out.extend(self.body(vec![d.clone()], alt, &[], names)?);
                        }
                    }
                    drafts = out;
                }
                HLit::Not(_) | HLit::NotBody(..) | HLit::Forall { .. } | HLit::Choose(_) => {}
            }
        }
        Ok(drafts)
    }

    /// `outer r(…)` (LANGUAGE §9.6): a case where the atom matches (its own variables wrapped in `Some`) and a case
    /// where nothing matches (they are `None`).
    fn outer(&mut self, d: Draft, a: &HAtom, non_outer: &BTreeSet<HVarId>) -> Result<(Draft, Draft), InternalError> {
        let own: Vec<HVarId> = {
            let mut vs = Vec::new();
            for p in &a.args {
                if let HPat::Var(v, _) = p
                    && !non_outer.contains(v)
                    && !d.map.contains_key(v)
                    && !vs.contains(v)
                {
                    vs.push(*v);
                }
            }
            vs
        };
        // Some: the atom with fresh inner variables, and `v := Some(inner)`.
        let mut some = d.clone();
        let mut args = a.args.clone();
        let mut inner = Vec::new();
        for (c, p) in args.iter_mut().enumerate() {
            if let HPat::Var(v, span) = p
                && own.contains(v)
            {
                inner.push((c, *v));
                *p = HPat::Wild(*span);
            }
        }
        let mut at = self.atom(&mut some, a.rel, &args, a.from.as_ref(), false, a.span)?;
        for (c, v) in &inner {
            let col = self.ir_col(a.rel, *c)?;
            let col_ty = self.b_col_ty(at.rel, col)?;
            let i = some.fresh(col_ty);
            if let Some(slot) = at.args.get_mut(col) {
                *slot = Term::Var(i);
            }
            let outer_var = some.var(self.hir, *v)?;
            let opt_ty = some
                .var_ty(outer_var)
                .ok_or_else(|| internal_error!("outer variable without a type"))?;
            some.lits.push(Literal::Bind {
                pat: Pattern::Var(outer_var),
                expr: Expr::Construct {
                    ty: opt_ty,
                    variant: Some(1),
                    fields: vec![Expr::Term(Term::Var(i))],
                },
            });
        }
        some.lits.push(Literal::Pos(at));
        // None: no tuple matches on the columns bound elsewhere.
        let mut none = d;
        let neg = self.atom(&mut none, a.rel, &args, None, true, a.span)?;
        none.lits.push(Literal::Neg(neg));
        for (_, v) in &inner {
            let outer_var = none.var(self.hir, *v)?;
            let c = self.b.intern_const(Value::none()).map_err(ir)?;
            none.lits.push(Literal::Bind {
                pat: Pattern::Var(outer_var),
                expr: Expr::Term(Term::Const(c)),
            });
        }
        Ok((some, none))
    }

    /// The `$prev` shadow of a delta-read relation (LANGUAGE §9.10), created once.
    fn prev_rel(&mut self, h: HRelId, span: Span) -> Result<RelId, InternalError> {
        if let Some(p) = self.prev.get(&h) {
            return Ok(*p);
        }
        let r = self.hir.rel(h)?.clone();
        let rel = self.rel(h)?;
        let construct = self
            .b
            .begin_construct(
                ConstructKind::DeltaRead { rel, prev: rel },
                surface(&r.name, None, span),
            )
            .map_err(ir)?;
        let cols: Vec<ir::Column> = self
            .b
            .program()
            .rels
            .get(rel)
            .map(|d| d.schema.cols.clone())
            .ok_or_else(|| internal_error!("unknown relation"))?;
        let cols: Vec<ir::Column> = cols
            .into_iter()
            .map(|mut c| {
                c.hidden_dest = false;
                c
            })
            .collect();
        let prev = self.generated(suffixed(&r.name, "$prev"), cols.clone(), None, r.role, r.durable, span)?;
        self.b
            .set_construct_kind(construct, ConstructKind::DeltaRead { rel, prev })
            .map_err(ir)?;
        let label = self.label(format!("{}$prev", r.name));
        let mut d = Draft::new(ScopeId(0));
        let vars: Vec<Term> = cols.iter().map(|c| Term::Var(d.fresh(c.ty))).collect();
        d.lits.push(Literal::Pos(ir_atom(rel, vars.clone(), span)));
        d.build(
            &mut self.b,
            RuleKind::Inductive,
            label,
            span,
            Head {
                rel: prev,
                args: vars.into_iter().map(HeadArg::Term).collect(),
                mode: HeadMode::Insert,
            },
            r.role,
        )?;
        self.b.end_construct(construct).map_err(ir)?;
        self.prev.insert(h, prev);
        Ok(prev)
    }

    /// The correlated variables of a nested body: the ones the enclosing draft has bound, in a stable order.
    fn correlated(&self, d: &Draft, nested: &BTreeSet<HVarId>) -> Vec<HVarId> {
        let mut vs: Vec<HVarId> = nested.iter().copied().filter(|v| d.map.contains_key(v)).collect();
        vs.sort();
        vs
    }

    /// `not { B }` (LANGUAGE §9.3): `not$h(Ō) :- context, B.` and `notin not$h(Ō)`.
    fn not_body(
        &mut self,
        d: &mut Draft,
        inner: &HBody,
        span: Span,
        context: &[Given<'_>],
        names: &mut Names,
    ) -> Result<(), InternalError> {
        let mut m = BTreeSet::new();
        mentioned(inner, &mut m);
        let outer = self.correlated(d, &m);
        let cols: Vec<ir::Column> = outer
            .iter()
            .enumerate()
            .map(|(i, v)| {
                Ok(column(
                    Symbol::intern(&format!("c{i}")),
                    self.var_ty(d.scope, *v)?,
                    false,
                ))
            })
            .collect::<Result<_, InternalError>>()?;
        names.counter += 1;
        let tag = format!("$not#{}", names.counter);
        let module = names.module.clone();
        let construct = self
            .b
            .begin_construct(
                ConstructKind::NotExists {
                    helper: RelId::from_raw(0),
                },
                surface(&module, None, span),
            )
            .map_err(ir)?;
        let helper = self.generated(names.rel_segments(&tag), cols, None, names.role, false, span)?;
        self.b
            .set_construct_kind(construct, ConstructKind::NotExists { helper })
            .map_err(ir)?;
        let seed = self.given(vec![Draft::new(d.scope)], context, names)?;
        for hd in self.body(seed, inner, &[], names)? {
            let mut hd = hd;
            let args = self.var_terms(&mut hd, &outer)?;
            let label = self.label(format!("{}{tag}", names.base));
            hd.build(
                &mut self.b,
                RuleKind::Deductive,
                label,
                span,
                Head {
                    rel: helper,
                    args: args.into_iter().map(HeadArg::Term).collect(),
                    mode: HeadMode::Insert,
                },
                names.role,
            )?;
        }
        self.b.end_construct(construct).map_err(ir)?;
        let args = self.var_terms(d, &outer)?;
        d.lits.push(Literal::Neg(ir_atom(helper, args, span)));
        Ok(())
    }

    /// `forall D { B }` (LANGUAGE §9.8): `fa(Ō, X̄) :- context, D, B.`, `fa$miss(Ō) :- context, D, notin fa(Ō, X̄).`
    /// and `notin fa$miss(Ō)`.
    fn forall(
        &mut self,
        d: &mut Draft,
        domain: &HLit,
        fbody: &HBody,
        span: Span,
        context: &[Given<'_>],
        names: &mut Names,
    ) -> Result<(), InternalError> {
        let mut m = BTreeSet::new();
        mentioned_lit(domain, &mut m);
        mentioned(fbody, &mut m);
        let outer = self.correlated(d, &m);
        let dom_body = HBody {
            lits: vec![domain.clone()],
            span: None,
        };
        let own: Vec<HVarId> = binders(&dom_body).into_iter().filter(|v| !outer.contains(v)).collect();
        let all: Vec<HVarId> = outer.iter().chain(own.iter()).copied().collect();
        let col = |l: &Self, i: usize, v: &HVarId| -> Result<ir::Column, InternalError> {
            Ok(column(Symbol::intern(&format!("c{i}")), l.var_ty(d.scope, *v)?, false))
        };
        let fa_cols: Vec<ir::Column> = all
            .iter()
            .enumerate()
            .map(|(i, v)| col(self, i, v))
            .collect::<Result<_, _>>()?;
        let miss_cols: Vec<ir::Column> = outer
            .iter()
            .enumerate()
            .map(|(i, v)| col(self, i, v))
            .collect::<Result<_, _>>()?;
        names.counter += 1;
        let tag = format!("$fa#{}", names.counter);
        let module = names.module.clone();
        let construct = self
            .b
            .begin_construct(
                ConstructKind::Forall {
                    fa: RelId::from_raw(0),
                    miss: RelId::from_raw(0),
                    closed: false,
                },
                surface(&module, None, span),
            )
            .map_err(ir)?;
        let fa = self.generated(names.rel_segments(&tag), fa_cols, None, names.role, false, span)?;
        let miss = self.generated(
            names.rel_segments(&format!("{tag}$miss")),
            miss_cols,
            None,
            names.role,
            false,
            span,
        )?;
        // A closed domain: a role's members, or a static relation (facts only).
        let closed = match domain {
            HLit::RoleGen { .. } => true,
            HLit::Atom(a) => matches!(
                self.hir.rel(a.rel)?.kind,
                HRelKind::Static | HRelKind::Members(_) | HRelKind::NodeDir
            ),
            _ => false,
        };
        self.b
            .set_construct_kind(construct, ConstructKind::Forall { fa, miss, closed })
            .map_err(ir)?;
        // fa(Ō, X̄) :- context, D, B.
        let seed = self.given(vec![Draft::new(d.scope)], context, names)?;
        let seed = self.positives(seed, &dom_body, names)?;
        for hd in self.body(seed, fbody, &[], names)? {
            let mut hd = hd;
            let args = self.var_terms(&mut hd, &all)?;
            let label = self.label(format!("{}{tag}", names.base));
            hd.build(
                &mut self.b,
                RuleKind::Deductive,
                label,
                span,
                Head {
                    rel: fa,
                    args: args.into_iter().map(HeadArg::Term).collect(),
                    mode: HeadMode::Insert,
                },
                names.role,
            )?;
        }
        // fa$miss(Ō) :- context, D, notin fa(Ō, X̄).
        let seed = self.given(vec![Draft::new(d.scope)], context, names)?;
        for md in self.positives(seed, &dom_body, names)? {
            let mut md = md;
            let fa_args = self.var_terms(&mut md, &all)?;
            md.lits.push(Literal::Neg(ir_atom(fa, fa_args, span)));
            let args = self.var_terms(&mut md, &outer)?;
            let label = self.label(format!("{}{tag}$miss", names.base));
            md.build(
                &mut self.b,
                RuleKind::Deductive,
                label,
                span,
                Head {
                    rel: miss,
                    args: args.into_iter().map(HeadArg::Term).collect(),
                    mode: HeadMode::Insert,
                },
                names.role,
            )?;
        }
        self.b.end_construct(construct).map_err(ir)?;
        let args = self.var_terms(d, &outer)?;
        d.lits.push(Literal::Neg(ir_atom(miss, args, span)));
        Ok(())
    }

    // ------------------------------------------------------------------ handlers

    pub(crate) fn handlers(&mut self) -> Result<(), InternalError> {
        for h in &self.hir.handlers {
            self.handler(h)?;
        }
        Ok(())
    }

    fn handler(&mut self, h: &'h HHandler) -> Result<(), InternalError> {
        let module = self.hir.scope(h.scope)?.module.clone();
        let label = match (h.kind, h.label) {
            (HandlerKind::Bootstrap, _) => "bootstrap".to_owned(),
            (HandlerKind::BootstrapFresh, _) => "bootstrap_fresh".to_owned(),
            (_, Some(l)) => l.as_str().to_owned(),
            (_, None) => format!("h#{}", stable_hash_hex8(h.text.as_bytes())),
        };
        let base = if module.segments().is_empty() {
            label.clone()
        } else {
            format!("{module}::{label}")
        };
        let mut names = Names {
            module: module.clone(),
            base: base.clone(),
            stem: label.clone(),
            role: h.role,
            counter: 0,
        };
        let vars = binders(&h.header);
        let cols: Vec<ir::Column> = vars
            .iter()
            .map(|v| {
                let name = self.hir.var(h.scope, *v)?.name;
                Ok(column(name, self.var_ty(h.scope, *v)?, false))
            })
            .collect::<Result<_, InternalError>>()?;
        let construct = self
            .b
            .begin_construct(
                ConstructKind::HandlerHeader {
                    when: RelId::from_raw(0),
                },
                surface(&module, h.label, h.span),
            )
            .map_err(ir)?;
        let when = self.generated(names.rel_segments("$when"), cols, None, h.role, false, h.span)?;
        self.b
            .set_construct_kind(construct, ConstructKind::HandlerHeader { when })
            .map_err(ir)?;
        for d in self.body(vec![Draft::new(h.scope)], &h.header, &[], &mut names)? {
            let mut d = d;
            let args = self.var_terms(&mut d, &vars)?;
            let l = self.label(format!("{base}$when"));
            d.build(
                &mut self.b,
                RuleKind::Deductive,
                l,
                h.span,
                Head {
                    rel: when,
                    args: args.into_iter().map(HeadArg::Term).collect(),
                    mode: HeadMode::Insert,
                },
                h.role,
            )?;
        }
        self.b.end_construct(construct).map_err(ir)?;
        // Statements that share verb and target get a hash suffix (LANGUAGE §4.3).
        let mut counts: BTreeMap<(Verb, HRelId), u32> = BTreeMap::new();
        count_targets(&h.stmts, &mut counts);
        self.stmts(&h.stmts, h.scope, (when, vars), &BTreeMap::new(), &mut names, &counts)
    }

    /// A handler's statements under `parent` (its header's or enclosing block's relation), whose variables `refined`
    /// have those types there.
    fn stmts(
        &mut self,
        stmts: &'h [HStmt],
        scope: ScopeId,
        parent: (RelId, Vec<HVarId>),
        refined: &BTreeMap<HVarId, TypeId>,
        names: &mut Names,
        counts: &BTreeMap<(Verb, HRelId), u32>,
    ) -> Result<(), InternalError> {
        for s in stmts {
            match s {
                HStmt::Verb(v) => self.verb(v, scope, &parent, refined, names, counts)?,
                HStmt::Block {
                    kind,
                    cond,
                    stmts: inner,
                    text,
                    span,
                    refined: here,
                } => {
                    let mut inside = refined.clone();
                    inside.extend(here.iter().copied());
                    let word = match kind {
                        BlockKind::If | BlockKind::Else => "if",
                        BlockKind::For => "for",
                    };
                    let tag = format!("${word}#{}", stable_hash_hex8(text.as_bytes()));
                    let mut vars = parent.1.clone();
                    for v in binders(cond) {
                        if !vars.contains(&v) {
                            vars.push(v);
                        }
                    }
                    let cols: Vec<ir::Column> = vars
                        .iter()
                        .map(|v| {
                            let name = self.hir.var(scope, *v)?.name;
                            let ty = match inside.get(v) {
                                Some(ty) => *ty,
                                None => self.var_ty(scope, *v)?,
                            };
                            Ok(column(name, ty, false))
                        })
                        .collect::<Result<_, InternalError>>()?;
                    let module = names.module.clone();
                    let construct = self
                        .b
                        .begin_construct(
                            ConstructKind::Block {
                                rel: RelId::from_raw(0),
                            },
                            surface(&module, None, *span),
                        )
                        .map_err(ir)?;
                    let rel = self.generated(names.rel_segments(&tag), cols, None, names.role, false, *span)?;
                    self.b
                        .set_construct_kind(construct, ConstructKind::Block { rel })
                        .map_err(ir)?;
                    let given = [Given::Rel {
                        rel: parent.0,
                        vars: parent.1.clone(),
                        span: *span,
                    }];
                    let seed = self.given(vec![Draft::refined(scope, inside.clone())], &given, names)?;
                    for d in self.body(seed, cond, &given, names)? {
                        let mut d = d;
                        let args = self.var_terms(&mut d, &vars)?;
                        let l = self.label(format!("{}{tag}", names.base));
                        d.build(
                            &mut self.b,
                            RuleKind::Deductive,
                            l,
                            *span,
                            Head {
                                rel,
                                args: args.into_iter().map(HeadArg::Term).collect(),
                                mode: HeadMode::Insert,
                            },
                            names.role,
                        )?;
                    }
                    self.b.end_construct(construct).map_err(ir)?;
                    self.stmts(inner, scope, (rel, vars), &inside, names, counts)?;
                }
            }
        }
        Ok(())
    }

    /// One statement: one rule reading the enclosing header or block relation (LANGUAGE §8.2).
    fn verb(
        &mut self,
        v: &'h HVerbStmt,
        scope: ScopeId,
        parent: &(RelId, Vec<HVarId>),
        refined: &BTreeMap<HVarId, TypeId>,
        names: &mut Names,
        counts: &BTreeMap<(Verb, HRelId), u32>,
    ) -> Result<(), InternalError> {
        let target = self.hir.rel(v.target)?.clone();
        let short = target.name.last().map(|s| s.as_str().to_owned()).unwrap_or_default();
        let mut text = format!("{}/{}:{short}", names.base, v.verb.as_str());
        if counts.get(&(v.verb, v.target)).copied().unwrap_or(0) > 1 {
            text.push('#');
            text.push_str(&stable_hash_hex8(v.text.as_bytes()));
        }
        let label = self.label(text);
        let mut d = Draft::refined(scope, refined.clone());
        let pargs = self.var_terms(&mut d, &parent.1)?;
        d.lits.push(Literal::Pos(ir_atom(parent.0, pargs, v.span)));
        // Head columns, in IR order.
        let mut rel = self.rel(v.target)?;
        let n_ir = self.b.program().rels.get(rel).map(|r| r.schema.cols.len()).unwrap_or(0);
        let mut args: Vec<Option<HeadArg>> = vec![None; n_ir];
        for (c, a) in v.args.iter().enumerate() {
            let col = self.ir_col(v.target, c)?;
            let arg = match a {
                HHeadArg::Expr(e) => HeadArg::Term(self.term(&mut d, e)?),
                HHeadArg::Agg(g) => HeadArg::Agg(self.agg_call(&mut d, g, &parent.1)?),
            };
            if let Some(slot) = args.get_mut(col) {
                *slot = Some(arg);
            }
        }
        // Under `resolve prefer`, a write is staged with its precedence and arbitrated before it applies.
        if let Some(rank) = v.rank {
            let (listed, unlisted) = self.prefer_rels(v.target, v.span)?;
            let mut args: Vec<HeadArg> = args
                .into_iter()
                .map(|a| a.ok_or_else(|| internal_error!("a head column was not filled")))
                .collect::<Result<_, _>>()?;
            let upsert = self.b.intern_const(Value::Bool(v.verb == Verb::Upsert)).map_err(ir)?;
            let rel = match rank {
                HRank::Listed(i) => {
                    let i = self
                        .b
                        .intern_const(Value::Int(IntValue::U64(u64::from(i))))
                        .map_err(ir)?;
                    args.push(HeadArg::Term(Term::Const(i)));
                    listed
                }
                HRank::Unlisted => unlisted,
            };
            args.push(HeadArg::Term(Term::Const(upsert)));
            d.build(
                &mut self.b,
                RuleKind::Deductive,
                label,
                v.span,
                Head {
                    rel,
                    args,
                    mode: HeadMode::Insert,
                },
                names.role,
            )?;
            return Ok(());
        }
        let kind = match v.verb {
            Verb::Emit => RuleKind::Deductive,
            // A resolved table's `next` inserts are candidates for t+1, written this tick (LANGUAGE §10.7).
            Verb::Next if self.resolved.contains_key(&v.target) => {
                rel = *self
                    .resolved
                    .get(&v.target)
                    .ok_or_else(|| internal_error!("a resolved table without `$n`"))?;
                RuleKind::Deductive
            }
            Verb::Next => RuleKind::Inductive,
            // A request to the host (a stream's write, close or dial) has no destination column.
            Verb::Send if matches!(target.kind, HRelKind::Stream(HStreamRel::Host(_))) => RuleKind::Async,
            Verb::Send => {
                let ch = match &target.kind {
                    HRelKind::Channel(ch) => ch.clone(),
                    _ => return Err(internal_error!("`send` into a non-channel reached lowering")),
                };
                if ch.dest_col.is_none() {
                    let dest = match &v.to {
                        Some(to) => self.term(&mut d, to)?,
                        None => {
                            // A loopback: the destination is self.
                            let ty = self.b_col_ty(rel, 0)?;
                            let f = d.fresh(ty);
                            d.lits.push(Literal::Bind {
                                pat: Pattern::Var(f),
                                expr: Expr::Scalar(ir::BuiltinScalar::SelfNode),
                            });
                            Term::Var(f)
                        }
                    };
                    if let Some(slot) = args.get_mut(0) {
                        *slot = Some(HeadArg::Term(dest));
                    }
                }
                RuleKind::Async
            }
            Verb::Delete => {
                rel = *self
                    .del
                    .get(&v.target)
                    .ok_or_else(|| internal_error!("`delete` from a relation without `$del`"))?;
                RuleKind::Deductive
            }
            Verb::Upsert => {
                rel = self.ups_rel(v.target, v.span)?;
                RuleKind::Deductive
            }
            Verb::Seal => return Err(internal_error!("`seal` reached lowering")),
        };
        let args: Vec<HeadArg> = args
            .into_iter()
            .map(|a| a.ok_or_else(|| internal_error!("a head column was not filled")))
            .collect::<Result<_, _>>()?;
        d.build(
            &mut self.b,
            kind,
            label,
            v.span,
            Head {
                rel,
                args,
                mode: HeadMode::Insert,
            },
            names.role,
        )?;
        Ok(())
    }

    /// A head aggregate over the valuations of `over` (the statement's header or block variables).
    fn agg_call(&mut self, d: &mut Draft, g: &HAgg, over: &[HVarId]) -> Result<AggCall, InternalError> {
        let func = match g.func {
            AggKind::Count => AggFunc::Count,
            AggKind::Sum => AggFunc::Sum,
            AggKind::Min => AggFunc::Min,
            AggKind::Max => AggFunc::Max,
            AggKind::Collect => AggFunc::CollectVec,
            AggKind::Index => return Err(internal_error!("`index!` reached a plain aggregate")),
        };
        let args = if g.args.is_empty() {
            self.var_terms(d, over)?
        } else {
            let mut out = Vec::new();
            for e in &g.args {
                out.push(self.term(d, e)?);
            }
            // `sum!(e)` adds `e` once per distinct valuation of the group (LANGUAGE §10.1), not per distinct value,
            // and `collect!(e)` holds `e` once per valuation.
            if matches!(g.func, AggKind::Sum | AggKind::Collect) {
                out.extend(self.var_terms(d, over)?);
            }
            out
        };
        Ok(AggCall {
            func,
            args,
            order: None,
        })
    }

    /// A `resolve prefer` table's staging relations and their rules (LANGUAGE §10.7), created once: `r$w(x̄, rank, u)`
    /// holds one tick's writes by listed handlers, `r$wx(x̄, u)` those by others (`u`: an upsert, not a `next`).
    /// Per key the least rank survives; the survivors and every unlisted write then apply as their verb does, so two
    /// different values that survive still conflict (SEM-050, SEM-051).
    ///
    /// ```text
    /// r$wmin(k̄, min<R>) :- r$w(k̄, v̄, R, _).
    /// r$ups(k̄, v̄)       :- r$w(k̄, v̄, R, true), r$wmin(k̄, R).      r$ups(k̄, v̄) :- r$wx(k̄, v̄, true).
    /// r(k̄, v̄)@next      :- r$w(k̄, v̄, R, false), r$wmin(k̄, R).     r(k̄, v̄)@next :- r$wx(k̄, v̄, false).
    /// ```
    fn prefer_rels(&mut self, h: HRelId, span: Span) -> Result<(RelId, RelId), InternalError> {
        if let Some(x) = self.prefer.get(&h) {
            return Ok(*x);
        }
        let r = self.hir.rel(h)?.clone();
        let rel = self.rel(h)?;
        let key = r
            .key
            .clone()
            .ok_or_else(|| internal_error!("`resolve prefer` on an unkeyed relation"))?;
        let mut verbs = BTreeSet::new();
        for hd in &self.hir.handlers {
            prefer_verbs(&hd.stmts, h, &mut verbs);
        }
        // `$ups` is its own construct: made before this one begins.
        let ups = if verbs.contains(&Verb::Upsert) {
            Some(self.ups_rel(h, span)?)
        } else {
            None
        };
        let (cols, _) = self.ir_columns(h)?;
        let u64t = self
            .b
            .types()
            .insert(TypeDef::Int(blossom_value::types::IntTy::U64))
            .map_err(|e| internal_error!("interning a type: {e}"))?;
        let boolt = self
            .b
            .types()
            .insert(TypeDef::Bool)
            .map_err(|e| internal_error!("interning a type: {e}"))?;
        let n = cols.len();
        let spec = |candidates: RelId| {
            ConstructKind::Resolve(ir::ResolveSpec {
                rel,
                candidates,
                output: rel,
                group: key.iter().map(|c| super::col_idx(*c)).collect(),
                policy: ir::ResolvePolicy::Prefer {
                    rank: super::col_idx(n),
                },
                site: None,
            })
        };
        let construct = self
            .b
            .begin_construct(spec(rel), surface(&r.name, None, span))
            .map_err(ir)?;
        let mut wcols = cols.clone();
        wcols.push(column(Symbol::intern("rank"), u64t, false));
        wcols.push(column(Symbol::intern("upsert"), boolt, false));
        let w = self.generated(suffixed(&r.name, "$w"), wcols, None, r.role, false, span)?;
        let mut xcols = cols.clone();
        xcols.push(column(Symbol::intern("upsert"), boolt, false));
        let wx = self.generated(suffixed(&r.name, "$wx"), xcols, None, r.role, false, span)?;
        let mut mcols: Vec<_> = key.iter().filter_map(|c| cols.get(*c).cloned()).collect();
        mcols.push(column(Symbol::intern("rank"), u64t, false));
        let k = key.len();
        let wmin = self.generated(
            suffixed(&r.name, "$wmin"),
            mcols,
            Some(&(0..k).collect::<Vec<_>>()),
            r.role,
            false,
            span,
        )?;
        self.b.set_construct_kind(construct, spec(w)).map_err(ir)?;
        // r$wmin(k̄, min<R>) :- r$w(k̄, v̄, R, _).
        let mut d = Draft::new(ScopeId(0));
        let vars: Vec<Term> = cols.iter().map(|c| Term::Var(d.fresh(c.ty))).collect();
        let rank = d.fresh(u64t);
        let mut wargs = vars.clone();
        wargs.push(Term::Var(rank));
        wargs.push(Term::Wild);
        d.lits.push(Literal::Pos(ir_atom(w, wargs, span)));
        let mut head: Vec<HeadArg> = key
            .iter()
            .filter_map(|c| vars.get(*c).cloned())
            .map(HeadArg::Term)
            .collect();
        head.push(HeadArg::Agg(AggCall {
            func: AggFunc::Min,
            args: vec![Term::Var(rank)],
            order: None,
        }));
        let label = self.label(format!("{}$prefer/min", r.name));
        d.build(
            &mut self.b,
            RuleKind::Deductive,
            label,
            span,
            Head {
                rel: wmin,
                args: head,
                mode: HeadMode::Insert,
            },
            r.role,
        )?;
        // The survivors, and the unlisted writes, by verb.
        let targets = [
            (true, ups.map(|u| (u, RuleKind::Deductive))),
            (false, verbs.contains(&Verb::Next).then_some((rel, RuleKind::Inductive))),
        ];
        for (upsert, target) in targets {
            let Some((target, kind)) = target else { continue };
            let flag = self.b.intern_const(Value::Bool(upsert)).map_err(ir)?;
            let verb = if upsert { "upsert" } else { "next" };
            // The least rank's.
            let mut d = Draft::new(ScopeId(0));
            let vars: Vec<Term> = cols.iter().map(|c| Term::Var(d.fresh(c.ty))).collect();
            let rank = d.fresh(u64t);
            let mut wargs = vars.clone();
            wargs.push(Term::Var(rank));
            wargs.push(Term::Const(flag));
            d.lits.push(Literal::Pos(ir_atom(w, wargs, span)));
            let mut margs: Vec<Term> = key.iter().filter_map(|c| vars.get(*c).cloned()).collect();
            margs.push(Term::Var(rank));
            d.lits.push(Literal::Pos(ir_atom(wmin, margs, span)));
            let label = self.label(format!("{}$prefer/{verb}", r.name));
            d.build(
                &mut self.b,
                kind.clone(),
                label,
                span,
                Head {
                    rel: target,
                    args: vars.into_iter().map(HeadArg::Term).collect(),
                    mode: HeadMode::Insert,
                },
                r.role,
            )?;
            // Every unlisted write.
            let mut d = Draft::new(ScopeId(0));
            let vars: Vec<Term> = cols.iter().map(|c| Term::Var(d.fresh(c.ty))).collect();
            let mut xargs = vars.clone();
            xargs.push(Term::Const(flag));
            d.lits.push(Literal::Pos(ir_atom(wx, xargs, span)));
            let label = self.label(format!("{}$prefer/{verb}#unlisted", r.name));
            d.build(
                &mut self.b,
                kind,
                label,
                span,
                Head {
                    rel: target,
                    args: vars.into_iter().map(HeadArg::Term).collect(),
                    mode: HeadMode::Insert,
                },
                r.role,
            )?;
        }
        self.b.end_construct(construct).map_err(ir)?;
        self.prefer.insert(h, (w, wx));
        Ok((w, wx))
    }

    /// The `$ups` staging relation of an upserted table and its rules (LANGUAGE §8.2), created once.
    fn ups_rel(&mut self, h: HRelId, span: Span) -> Result<RelId, InternalError> {
        if let Some(u) = self.ups.get(&h) {
            return Ok(*u);
        }
        let r = self.hir.rel(h)?.clone();
        let rel = self.rel(h)?;
        let del = *self
            .del
            .get(&h)
            .ok_or_else(|| internal_error!("`upsert` into a relation without `$del`"))?;
        let key = r
            .key
            .clone()
            .ok_or_else(|| internal_error!("`upsert` into an unkeyed relation"))?;
        let construct = self
            .b
            .begin_construct(
                ConstructKind::Upsert { rel, staging: rel, del },
                surface(&r.name, None, span),
            )
            .map_err(ir)?;
        let (cols, _) = self.ir_columns(h)?;
        let ups = self.generated(suffixed(&r.name, "$ups"), cols.clone(), Some(&key), r.role, false, span)?;
        self.b
            .set_construct_kind(construct, ConstructKind::Upsert { rel, staging: ups, del })
            .map_err(ir)?;
        // r$del(k̄, v̄0) :- r$ups(k̄, _), r(k̄, v̄0).
        let mut d = Draft::new(ScopeId(0));
        let olds: Vec<Term> = cols.iter().map(|c| Term::Var(d.fresh(c.ty))).collect();
        let staged: Vec<Term> = olds
            .iter()
            .enumerate()
            .map(|(i, t)| if key.contains(&i) { t.clone() } else { Term::Wild })
            .collect();
        d.lits.push(Literal::Pos(ir_atom(ups, staged, span)));
        d.lits.push(Literal::Pos(ir_atom(rel, olds.clone(), span)));
        let label = self.label(format!("{}$ups/del", r.name));
        d.build(
            &mut self.b,
            RuleKind::Deductive,
            label,
            span,
            Head {
                rel: del,
                args: olds.into_iter().map(HeadArg::Term).collect(),
                mode: HeadMode::Insert,
            },
            r.role,
        )?;
        // r(k̄, v̄)@next :- r$ups(k̄, v̄).
        let mut d = Draft::new(ScopeId(0));
        let vars: Vec<Term> = cols.iter().map(|c| Term::Var(d.fresh(c.ty))).collect();
        d.lits.push(Literal::Pos(ir_atom(ups, vars.clone(), span)));
        let label = self.label(format!("{}$ups/next", r.name));
        d.build(
            &mut self.b,
            RuleKind::Inductive,
            label,
            span,
            Head {
                rel,
                args: vars.into_iter().map(HeadArg::Term).collect(),
                mode: HeadMode::Insert,
            },
            r.role,
        )?;
        self.b.end_construct(construct).map_err(ir)?;
        self.ups.insert(h, ups);
        Ok(ups)
    }

    // ------------------------------------------------------------------ invariants

    /// `invariant name: never B;` lowers to `M::name$violation(x̄) :- B.`, a violation head (LANGUAGE §17.1): every
    /// valuation of the body aborts the tick (BLSR003), the default action.
    pub(crate) fn invariants(&mut self) -> Result<(), InternalError> {
        for inv in &self.hir.invariants {
            let module = self.hir.scope(inv.scope)?.module.clone();
            let mut segs = module.segments().to_vec();
            segs.push(inv.name);
            let name = QualName::new(segs);
            let id = self
                .b
                .declare_invariant(ir::InvariantDecl {
                    id: blossom_base::InvariantId::from_raw(0),
                    name: name.clone(),
                    action: ir::ViolationAction::Abort,
                    span: inv.span,
                })
                .map_err(ir)?;
            let base = if module.segments().is_empty() {
                inv.name.as_str().to_owned()
            } else {
                format!("{module}::{}", inv.name.as_str())
            };
            let mut names = Names {
                module: module.clone(),
                base: base.clone(),
                stem: inv.name.as_str().to_owned(),
                role: inv.role,
                counter: 0,
            };
            let vars = binders(&inv.body);
            let cols: Vec<ir::Column> = vars
                .iter()
                .map(|v| {
                    Ok(column(
                        self.hir.var(inv.scope, *v)?.name,
                        self.var_ty(inv.scope, *v)?,
                        false,
                    ))
                })
                .collect::<Result<_, InternalError>>()?;
            let construct = self
                .b
                .begin_construct(
                    ConstructKind::Invariant { id },
                    surface(&module, Some(inv.name), inv.span),
                )
                .map_err(ir)?;
            let rel = self.generated(names.rel_segments("$violation"), cols, None, inv.role, false, inv.span)?;
            for d in self.body(vec![Draft::new(inv.scope)], &inv.body, &[], &mut names)? {
                let mut d = d;
                let args = self.var_terms(&mut d, &vars)?;
                let l = self.label(format!("{base}$violation"));
                d.build(
                    &mut self.b,
                    RuleKind::Deductive,
                    l,
                    inv.span,
                    Head {
                        rel,
                        args: args.into_iter().map(HeadArg::Term).collect(),
                        mode: HeadMode::Violation { invariant: id },
                    },
                    inv.role,
                )?;
            }
            self.b.end_construct(construct).map_err(ir)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------ views

    pub(crate) fn views(&mut self) -> Result<(), InternalError> {
        for v in &self.hir.views {
            self.view(v)?;
        }
        Ok(())
    }

    fn view(&mut self, v: &'h HView) -> Result<(), InternalError> {
        let r = self.hir.rel(v.rel)?.clone();
        let rel = self.rel(v.rel)?;
        let module = QualName::new(
            r.name
                .segments()
                .iter()
                .take(r.name.segments().len().saturating_sub(1))
                .copied()
                .collect::<Vec<_>>(),
        );
        let vname = r.name.last().map(|s| s.as_str().to_owned()).unwrap_or_default();
        let base = if module.segments().is_empty() {
            vname.clone()
        } else {
            format!("{module}::{vname}")
        };
        let mut names = Names {
            module: module.clone(),
            base: base.clone(),
            stem: vname.clone(),
            role: r.role,
            counter: 0,
        };
        let construct = self
            .b
            .begin_construct(
                ConstructKind::ViewAlternatives { view: rel },
                surface(&module, Some(Symbol::intern(&vname)), v.span),
            )
            .map_err(ir)?;
        let multi = v.alternatives.len() > 1;
        match &v.shape {
            HViewShape::Plain { cols } => {
                for (ai, (scope, body)) in v.alternatives.iter().enumerate() {
                    let alt_label = if multi {
                        format!(
                            "{base}#{}",
                            stable_hash_hex8(v.texts.get(ai).map_or("", String::as_str).as_bytes())
                        )
                    } else {
                        base.clone()
                    };
                    for d in self.body(vec![Draft::new(*scope)], body, &[], &mut names)? {
                        let mut d = d;
                        let mut args = Vec::new();
                        for per_alt in cols {
                            let hv = per_alt
                                .get(ai)
                                .ok_or_else(|| internal_error!("view column without a variable"))?;
                            args.push(HeadArg::Term(Term::Var(d.var(self.hir, *hv)?)));
                        }
                        let l = self.label(alt_label.clone());
                        d.build(
                            &mut self.b,
                            RuleKind::Deductive,
                            l,
                            v.span,
                            Head {
                                rel,
                                args,
                                mode: HeadMode::Insert,
                            },
                            r.role,
                        )?;
                    }
                }
            }
            HViewShape::Aggregate {
                union,
                shared,
                cols,
                driver,
            } => self.aggregate_view(v, rel, &r, *union, shared, cols, driver.as_ref(), &mut names)?,
        }
        self.b.end_construct(construct).map_err(ir)?;
        Ok(())
    }

    /// A view with an `index!()` column (LANGUAGE §10.5): each distinct head tuple of the tick gets its dense 0-based
    /// rank in canonical order. The reference lowering is quadratic; an engine sorts instead.
    ///
    /// ```ir
    /// v$h(Ḡ) :- v$u(…).                                  // the head tuples, without the index
    /// v$lt(Ḡ, Ḡ2) :- v$h(Ḡ), v$h(Ḡ2), (Ḡ2) < (Ḡ).        // canonical order
    /// v(Ḡ, count<Ḡ2>) :- v$lt(Ḡ, Ḡ2).
    /// v$ak(Ḡ) :- v$lt(Ḡ, _).
    /// v(Ḡ, 0) :- v$h(Ḡ), notin v$ak(Ḡ).
    /// ```
    #[allow(clippy::too_many_arguments)]
    fn index_view(
        &mut self,
        v: &'h HView,
        rel: RelId,
        r: &HRel,
        union: ScopeId,
        u: RelId,
        union_vars: &[HVarId],
        cols: &'h [HViewAggCol],
        names: &mut Names,
    ) -> Result<(), InternalError> {
        let base = names.base.clone();
        let mut groups: Vec<HVarId> = Vec::new();
        let mut index_at = None;
        for (i, c) in cols.iter().enumerate() {
            match c {
                HViewAggCol::Group(uv) => groups.push(*uv),
                HViewAggCol::Agg(a) if a.func == AggKind::Index && index_at.is_none() => index_at = Some(i),
                HViewAggCol::Agg(a) => {
                    return Err(internal_error!(
                        "a view with `index!` and another aggregate column ({:?}) reached lowering",
                        a.func
                    ));
                }
            }
        }
        let index_at = index_at.ok_or_else(|| internal_error!("an index view without its index column"))?;
        let gtys: Vec<TypeId> = groups
            .iter()
            .map(|g| self.var_ty(union, *g))
            .collect::<Result<_, _>>()?;
        let gcols = |pre: &str| -> Vec<ir::Column> {
            gtys.iter()
                .enumerate()
                .map(|(i, t)| column(Symbol::intern(&format!("{pre}{i}")), *t, false))
                .collect()
        };
        let construct = self
            .b
            .begin_construct(
                ConstructKind::Index(ir::IndexSpec {
                    input: RelId::from_raw(0),
                    output: rel,
                    group: Vec::new(),
                    order: ir::OrderSpec { keys: Vec::new() },
                    mode: ir::IndexMode::Index,
                }),
                surface(&names.module, None, v.span),
            )
            .map_err(ir)?;
        let h = self.generated(suffixed(&r.name, "$h"), gcols("g"), None, r.role, false, v.span)?;
        self.b
            .set_construct_kind(
                construct,
                ConstructKind::Index(ir::IndexSpec {
                    input: h,
                    output: rel,
                    group: Vec::new(),
                    order: ir::OrderSpec { keys: Vec::new() },
                    mode: ir::IndexMode::Index,
                }),
            )
            .map_err(ir)?;
        let mut lt_cols = gcols("g");
        lt_cols.extend(gcols("h"));
        let lt = self.generated(suffixed(&r.name, "$lt"), lt_cols, None, r.role, false, v.span)?;
        let ak = self.generated(suffixed(&r.name, "$ak"), gcols("g"), None, r.role, false, v.span)?;
        let role = r.role;
        // v$h(Ḡ) :- v$u(union).
        let mut d = Draft::new(union);
        let uargs = self.var_terms(&mut d, union_vars)?;
        d.lits.push(Literal::Pos(ir_atom(u, uargs, v.span)));
        let gargs = self.var_terms(&mut d, &groups)?;
        let l = self.label(format!("{base}$h"));
        d.build(
            &mut self.b,
            RuleKind::Deductive,
            l,
            v.span,
            Head {
                rel: h,
                args: gargs.into_iter().map(HeadArg::Term).collect(),
                mode: HeadMode::Insert,
            },
            role,
        )?;
        let head_args = |xs: Vec<Term>, idx: Term| -> Vec<HeadArg> {
            let mut out: Vec<HeadArg> = xs.into_iter().map(HeadArg::Term).collect();
            out.insert(index_at.min(out.len()), HeadArg::Term(idx));
            out
        };
        let unit = self.b.intern_const(Value::Unit).map_err(ir)?;
        let gtuple = super::expr::tuple_type(&mut self.b, gtys.clone())?;
        // v$lt(Ḡ, Ḡ2) :- v$h(Ḡ), v$h(Ḡ2), (Ḡ2) < (Ḡ).
        let mut d = Draft::new(union);
        let a: Vec<Term> = gtys.iter().map(|t| Term::Var(d.fresh(*t))).collect();
        let b2: Vec<Term> = gtys.iter().map(|t| Term::Var(d.fresh(*t))).collect();
        d.lits.push(Literal::Pos(ir_atom(h, a.clone(), v.span)));
        d.lits.push(Literal::Pos(ir_atom(h, b2.clone(), v.span)));
        let tup = |xs: &[Term]| super::expr::tuple_expr(unit, gtuple, xs.iter().cloned().map(Expr::Term).collect());
        d.lits.push(Literal::Guard(Expr::Binary {
            op: ir::BinOp::CanonLt,
            lhs: Box::new(tup(&b2)),
            rhs: Box::new(tup(&a)),
        }));
        let mut lt_args = a.clone();
        lt_args.extend(b2.clone());
        let l = self.label(format!("{base}$lt"));
        d.build(
            &mut self.b,
            RuleKind::Deductive,
            l,
            v.span,
            Head {
                rel: lt,
                args: lt_args.into_iter().map(HeadArg::Term).collect(),
                mode: HeadMode::Insert,
            },
            role,
        )?;
        // v(Ḡ, count<Ḡ2>) :- v$lt(Ḡ, Ḡ2).
        let mut d = Draft::new(union);
        let a: Vec<Term> = gtys.iter().map(|t| Term::Var(d.fresh(*t))).collect();
        let b2: Vec<Term> = gtys.iter().map(|t| Term::Var(d.fresh(*t))).collect();
        let mut lt_args = a.clone();
        lt_args.extend(b2.clone());
        d.lits.push(Literal::Pos(ir_atom(lt, lt_args, v.span)));
        let mut head: Vec<HeadArg> = a.iter().cloned().map(HeadArg::Term).collect();
        head.insert(
            index_at.min(head.len()),
            HeadArg::Agg(AggCall {
                func: AggFunc::Count,
                args: b2,
                order: None,
            }),
        );
        let l = self.label(format!("{base}$index"));
        d.build(
            &mut self.b,
            RuleKind::Deductive,
            l,
            v.span,
            Head {
                rel,
                args: head,
                mode: HeadMode::Insert,
            },
            role,
        )?;
        // v$ak(Ḡ) :- v$lt(Ḡ, _).
        let mut d = Draft::new(union);
        let a: Vec<Term> = gtys.iter().map(|t| Term::Var(d.fresh(*t))).collect();
        let mut lt_args = a.clone();
        lt_args.extend(gtys.iter().map(|_| Term::Wild));
        d.lits.push(Literal::Pos(ir_atom(lt, lt_args, v.span)));
        let l = self.label(format!("{base}$ak"));
        d.build(
            &mut self.b,
            RuleKind::Deductive,
            l,
            v.span,
            Head {
                rel: ak,
                args: a.into_iter().map(HeadArg::Term).collect(),
                mode: HeadMode::Insert,
            },
            role,
        )?;
        // v(Ḡ, 0) :- v$h(Ḡ), notin v$ak(Ḡ).
        let mut d = Draft::new(union);
        let a: Vec<Term> = gtys.iter().map(|t| Term::Var(d.fresh(*t))).collect();
        d.lits.push(Literal::Pos(ir_atom(h, a.clone(), v.span)));
        d.lits.push(Literal::Neg(ir_atom(ak, a.clone(), v.span)));
        let zero = self
            .b
            .intern_const(Value::Int(blossom_value::value::IntValue::U64(0)))
            .map_err(ir)?;
        let l = self.label(format!("{base}$index0"));
        d.build(
            &mut self.b,
            RuleKind::Deductive,
            l,
            v.span,
            Head {
                rel,
                args: head_args(a, Term::Const(zero)),
                mode: HeadMode::Insert,
            },
            role,
        )?;
        self.b.end_construct(construct).map_err(ir)?;
        Ok(())
    }

    /// A view with aggregate columns (LANGUAGE §8.3, §10.1–10.2).
    #[allow(clippy::too_many_arguments)]
    fn aggregate_view(
        &mut self,
        v: &'h HView,
        rel: RelId,
        r: &HRel,
        union: ScopeId,
        shared: &[Vec<HVarId>],
        cols: &'h [HViewAggCol],
        driver: Option<&'h HAtom>,
        names: &mut Names,
    ) -> Result<(), InternalError> {
        let base = names.base.clone();
        let n_union = self.hir.scope(union)?.vars.len();
        let union_vars: Vec<HVarId> = (0..n_union).map(|i| HVarId(i as u32)).collect();
        let ucols: Vec<ir::Column> = union_vars
            .iter()
            .map(|uv| {
                let name = self.hir.var(union, *uv)?.name;
                Ok(column(name, self.var_ty(union, *uv)?, false))
            })
            .collect::<Result<_, InternalError>>()?;
        let u = self.generated(suffixed(&r.name, "$u"), ucols, None, r.role, false, v.span)?;
        // v$u(shared) :- alternative.
        for (ai, (scope, body)) in v.alternatives.iter().enumerate() {
            let sv = shared
                .get(ai)
                .ok_or_else(|| internal_error!("a view alternative without shared variables"))?;
            for d in self.body(vec![Draft::new(*scope)], body, &[], names)? {
                let mut d = d;
                let args = self.var_terms(&mut d, sv)?;
                let l = self.label(format!("{base}$u"));
                d.build(
                    &mut self.b,
                    RuleKind::Deductive,
                    l,
                    v.span,
                    Head {
                        rel: u,
                        args: args.into_iter().map(HeadArg::Term).collect(),
                        mode: HeadMode::Insert,
                    },
                    r.role,
                )?;
            }
        }
        if cols
            .iter()
            .any(|c| matches!(c, HViewAggCol::Agg(a) if a.func == AggKind::Index))
        {
            return self.index_view(v, rel, r, union, u, &union_vars, cols, names);
        }
        // Defaults: explicit `default e`, or the identity of count and sum under a driver.
        let groups: Vec<(usize, HVarId)> = cols
            .iter()
            .enumerate()
            .filter_map(|(i, c)| match c {
                HViewAggCol::Group(uv) => Some((i, *uv)),
                HViewAggCol::Agg(_) => None,
            })
            .collect();
        let mut defaults: Vec<Option<Value>> = Vec::new();
        for (i, c) in cols.iter().enumerate() {
            defaults.push(match c {
                HViewAggCol::Group(_) => None,
                HViewAggCol::Agg(a) => {
                    let col_ty = r
                        .cols
                        .get(i)
                        .and_then(|c| c.ty)
                        .ok_or_else(|| internal_error!("view column without a type"))?;
                    match &a.default {
                        Some(e) => {
                            Some(try_const(self.hir, e).ok_or_else(|| internal_error!("a non-constant `default`"))?)
                        }
                        None if driver.is_some() && matches!(a.func, AggKind::Count | AggKind::Sum) => Some(
                            int_zero(&self.hir.types, col_ty)
                                .ok_or_else(|| internal_error!("a count or sum column that is not an integer"))?,
                        ),
                        // An empty group collects the empty vector (§10.2).
                        None if driver.is_some() && a.func == AggKind::Collect => Some(Value::Vec(Vec::new().into())),
                        None => None,
                    }
                }
            });
        }
        let any_default = defaults.iter().any(Option::is_some);
        let target = if any_default {
            let (all_cols, _) = (
                self.b
                    .program()
                    .rels
                    .get(rel)
                    .map(|d| d.schema.cols.clone())
                    .unwrap_or_default(),
                (),
            );
            self.generated(suffixed(&r.name, "$a"), all_cols, None, r.role, false, v.span)?
        } else {
            rel
        };
        // The aggregate rule: target(groups…, aggs…) :- v$u(union vars).
        let mut d = Draft::new(union);
        let uargs = self.var_terms(&mut d, &union_vars)?;
        d.lits.push(Literal::Pos(ir_atom(u, uargs, v.span)));
        let mut head = Vec::new();
        for c in cols {
            head.push(match c {
                HViewAggCol::Group(uv) => HeadArg::Term(Term::Var(d.var(self.hir, *uv)?)),
                HViewAggCol::Agg(a) => HeadArg::Agg(self.agg_call(&mut d, a, &union_vars)?),
            });
        }
        let l = self.label(format!("{base}$agg"));
        d.build(
            &mut self.b,
            RuleKind::Deductive,
            l,
            v.span,
            Head {
                rel: target,
                args: head,
                mode: HeadMode::Insert,
            },
            r.role,
        )?;
        if !any_default {
            return Ok(());
        }
        // v(x̄) :- v$a(x̄).
        let all_tys: Vec<TypeId> = r
            .cols
            .iter()
            .map(|c| c.ty.ok_or_else(|| internal_error!("view column without a type")))
            .collect::<Result<_, _>>()?;
        let mut d = Draft::new(union);
        let xs: Vec<Term> = all_tys.iter().map(|t| Term::Var(d.fresh(*t))).collect();
        d.lits.push(Literal::Pos(ir_atom(target, xs.clone(), v.span)));
        let l = self.label(format!("{base}$a"));
        d.build(
            &mut self.b,
            RuleKind::Deductive,
            l,
            v.span,
            Head {
                rel,
                args: xs.into_iter().map(HeadArg::Term).collect(),
                mode: HeadMode::Insert,
            },
            r.role,
        )?;
        // v$ak(groups) :- v$a(groups, _).
        let gcols: Vec<ir::Column> = groups
            .iter()
            .map(|(i, _)| {
                let ty = all_tys
                    .get(*i)
                    .copied()
                    .ok_or_else(|| internal_error!("group column out of range"))?;
                Ok(column(Symbol::intern(&format!("c{i}")), ty, false))
            })
            .collect::<Result<_, InternalError>>()?;
        let ak = self.generated(suffixed(&r.name, "$ak"), gcols, None, r.role, false, v.span)?;
        let mut d = Draft::new(union);
        let mut a_args = Vec::new();
        let mut g_args = Vec::new();
        for (i, ty) in all_tys.iter().enumerate() {
            if groups.iter().any(|(g, _)| *g == i) {
                let x = Term::Var(d.fresh(*ty));
                a_args.push(x.clone());
                g_args.push(x);
            } else {
                a_args.push(Term::Wild);
            }
        }
        d.lits.push(Literal::Pos(ir_atom(target, a_args, v.span)));
        let l = self.label(format!("{base}$ak"));
        d.build(
            &mut self.b,
            RuleKind::Deductive,
            l,
            v.span,
            Head {
                rel: ak,
                args: g_args.into_iter().map(HeadArg::Term).collect(),
                mode: HeadMode::Insert,
            },
            r.role,
        )?;
        // The default row: v(groups, defaults) :- driver, notin v$ak(groups).
        let scope0 = v
            .alternatives
            .first()
            .map(|a| a.0)
            .ok_or_else(|| internal_error!("a view without alternatives"))?;
        let shared0 = shared.first().cloned().unwrap_or_default();
        let mut d = Draft::new(scope0);
        if let Some(drv) = driver {
            let at = self.atom(&mut d, drv.rel, &drv.args, None, false, drv.span)?;
            d.lits.push(Literal::Pos(at));
        } else if !groups.is_empty() {
            return Err(internal_error!(
                "a default over grouping columns without a `per` driver reached lowering"
            ));
        }
        let mut ak_args = Vec::new();
        let mut head = Vec::new();
        for (i, c) in cols.iter().enumerate() {
            match c {
                HViewAggCol::Group(uv) => {
                    let hv = shared0
                        .get(uv.index())
                        .ok_or_else(|| internal_error!("a group variable missing from the alternative"))?;
                    let t = Term::Var(d.var(self.hir, *hv)?);
                    ak_args.push(t.clone());
                    head.push(HeadArg::Term(t));
                }
                HViewAggCol::Agg(_) => {
                    let val =
                        defaults.get(i).cloned().flatten().ok_or_else(|| {
                            internal_error!("an aggregate column without a default among defaulted ones")
                        })?;
                    head.push(HeadArg::Term(self.konst(val)?));
                }
            }
        }
        d.lits.push(Literal::Neg(ir_atom(ak, ak_args, v.span)));
        let l = self.label(format!("{base}$default"));
        d.build(
            &mut self.b,
            RuleKind::Deductive,
            l,
            v.span,
            Head {
                rel,
                args: head,
                mode: HeadMode::Insert,
            },
            r.role,
        )?;
        Ok(())
    }
}

fn count_targets(stmts: &[HStmt], counts: &mut BTreeMap<(Verb, HRelId), u32>) {
    for s in stmts {
        match s {
            HStmt::Verb(v) => *counts.entry((v.verb, v.target)).or_default() += 1,
            HStmt::Block { stmts, .. } => count_targets(stmts, counts),
        }
    }
}

/// The verbs of the arbitrated writes (`resolve prefer`) into `target` among `stmts`, blocks included.
fn prefer_verbs(stmts: &[HStmt], target: HRelId, out: &mut BTreeSet<Verb>) {
    for s in stmts {
        match s {
            HStmt::Verb(v) if v.target == target && v.rank.is_some() => {
                out.insert(v.verb);
            }
            HStmt::Verb(_) => {}
            HStmt::Block { stmts, .. } => prefer_verbs(stmts, target, out),
        }
    }
}

/// The top-level conjuncts of a guard, each as its own guard, left to right.
fn split_and(e: Expr, out: &mut Vec<Literal>) {
    match e {
        Expr::Binary {
            op: ir::BinOp::And,
            lhs,
            rhs,
        } => {
            split_and(*lhs, out);
            split_and(*rhs, out);
        }
        other => out.push(Literal::Guard(other)),
    }
}
