//! Type checking (ARCHITECTURE §13.6, LANGUAGE §5.6).
//!
//! Inference is global: every rule variable and every view column gets a type term, and one union-find solves all
//! of them together, so a view's column types flow into the rules that read it and back. Declared columns are
//! fixed. Unsuffixed integer literals are integer-constrained terms that default to `i64` when nothing else fixes
//! them. Constraints that need a known operand type (arithmetic over `Instant` and `Duration`, field access, casts)
//! are deferred until their operands are solved.
//!
//! `Node<R>` is a subtype of `Node` (LANGUAGE §5.3): a variable constrained by both becomes `Node<R>` (the meet), and
//! the IR accepts it where `Node` is expected.
//!
//! The checker walks the HIR twice in the same order: the first walk creates terms and constraints, the second
//! writes the solved types into every expression, variable and `Option` pattern.

use std::collections::BTreeSet;

use blossom_base::{Diagnostic, Diagnostics, InternalError, RoleId, Span, TypeId, code};
use blossom_value::{TypeDef, TypeTable, types::IntTy};

use crate::ast::{BinOp, PrefixOp};
use crate::hir::*;

/// Type-checks `hir`, filling [`Hir::var_types`], view column types and every expression's type.
pub fn check(hir: &mut Hir, diags: &mut Diagnostics) -> Result<(), InternalError> {
    let mut cx = Checker {
        bugs: Vec::new(),
        count_cols: Vec::new(),
        uf: Vec::new(),
        deferred: Vec::new(),
        diags,
        var_terms: Vec::new(),
        view_terms: Vec::new(),
        expr_terms: Vec::new(),
        cursor: 0,
        apply: false,
        errors_before: 0,
    };
    cx.errors_before = cx.diags.error_count();
    cx.run(hir);
    match cx.bugs.into_iter().next() {
        Some(bug) => Err(bug),
        None => Ok(()),
    }
}

type T = u32;

#[derive(Clone, Debug)]
enum Node {
    Unbound { int: bool },
    Link(T),
    Bound(Shape),
}

#[derive(Clone, Debug, PartialEq)]
enum Shape {
    /// A leaf type: scalars, nodes, structs, enums.
    Con(TypeId),
    Tuple(Vec<T>),
    Option(T),
    Vec(T),
    Set(T),
    Map(T, T),
}

#[derive(Clone, Debug)]
enum Deferred {
    /// `res = l op r` for arithmetic operators.
    Arith { op: BinOp, l: T, r: T, res: T, span: Span },
    /// `res = base.field`.
    Field {
        base: T,
        name: blossom_base::Symbol,
        res: T,
        span: Span,
    },
    /// `res = base.index`.
    TupleIndex { base: T, index: u32, res: T, span: Span },
    /// A numeric cast.
    Cast { from: T, to: T, span: Span },
    /// `base.len()`.
    Len { base: T, span: Span },
    /// An ordering comparison: the operands must be ordered scalars.
    Ordered { t: T, span: Span },
    /// An aggregate `count!` result: an integer column.
    IntColumn { t: T, span: Span },
}

struct Checker<'d> {
    /// Frontend bugs met while checking (a failed lookup of a resolver-minted id).
    bugs: Vec<InternalError>,
    /// Columns that receive a `count!`: unconstrained, they are `u64` (the IR's count).
    count_cols: Vec<T>,
    uf: Vec<Node>,
    deferred: Vec<Deferred>,
    diags: &'d mut Diagnostics,
    /// Per scope, per variable: its term.
    var_terms: Vec<Vec<T>>,
    /// Per relation, per column: its term.
    view_terms: Vec<Vec<T>>,
    /// Terms of expressions and `Option` patterns, in walk order.
    expr_terms: Vec<T>,
    cursor: usize,
    apply: bool,
    errors_before: usize,
}

impl Checker<'_> {
    fn rel_of(&mut self, hir: &Hir, id: HRelId) -> HRel {
        match hir.rel(id) {
            Ok(r) => r.clone(),
            Err(e) => {
                self.bugs.push(e);
                HRel::placeholder()
            }
        }
    }

    fn role_kind(&mut self, hir: &Hir, id: HRoleId) -> RoleKind {
        match hir.role(id) {
            Ok(r) => r.kind,
            Err(e) => {
                self.bugs.push(e);
                RoleKind::Process
            }
        }
    }

    fn fresh(&mut self, int: bool) -> T {
        self.uf.push(Node::Unbound { int });
        (self.uf.len() - 1) as T
    }

    fn bound(&mut self, s: Shape) -> T {
        self.uf.push(Node::Bound(s));
        (self.uf.len() - 1) as T
    }

    /// The node of term `t` (terms are created by `fresh`/`bound`, so `t` is always in range).
    fn node(&self, t: T) -> Node {
        self.uf.get(t as usize).cloned().unwrap_or(Node::Unbound { int: false })
    }

    fn set(&mut self, t: T, n: Node) {
        if let Some(slot) = self.uf.get_mut(t as usize) {
            *slot = n;
        }
    }

    fn var_term(&self, scope: ScopeId, v: HVarId) -> T {
        self.var_terms
            .get(scope.index())
            .and_then(|s| s.get(v.index()))
            .copied()
            .unwrap_or(0)
    }

    fn col_term(&self, rel: usize, c: usize) -> T {
        self.view_terms.get(rel).and_then(|r| r.get(c)).copied().unwrap_or(0)
    }

    fn find(&mut self, t: T) -> T {
        let mut r = t;
        while let Node::Link(n) = self.node(r) {
            r = n;
        }
        let mut c = t;
        while let Node::Link(n) = self.node(c) {
            self.set(c, Node::Link(r));
            c = n;
        }
        r
    }

    /// A term for a known type.
    fn of_type(&mut self, types: &TypeTable, ty: TypeId) -> T {
        let shape = match types.get(ty) {
            Some(TypeDef::Tuple(ts)) => {
                let ts = ts.clone();
                Shape::Tuple(ts.iter().map(|t| self.of_type(types, *t)).collect())
            }
            Some(TypeDef::Option(t)) => {
                let t = *t;
                Shape::Option(self.of_type(types, t))
            }
            Some(TypeDef::Vec(t)) => {
                let t = *t;
                Shape::Vec(self.of_type(types, t))
            }
            Some(TypeDef::Set(t)) => {
                let t = *t;
                Shape::Set(self.of_type(types, t))
            }
            Some(TypeDef::Map(k, v)) => {
                let (k, v) = (*k, *v);
                let k = self.of_type(types, k);
                let v = self.of_type(types, v);
                Shape::Map(k, v)
            }
            _ => Shape::Con(ty),
        };
        self.bound(shape)
    }

    fn con(&mut self, types: &mut TypeTable, def: TypeDef) -> T {
        let ty = intern(types, def);
        self.bound(Shape::Con(ty))
    }

    fn error(&mut self, span: Span, msg: String) {
        self.diags
            .push(Diagnostic::new(code!("BLS0300"), msg).with_primary(span));
    }

    fn describe(&mut self, types: &TypeTable, t: T) -> String {
        let r = self.find(t);
        match self.node(r) {
            Node::Unbound { int: true } => "an integer".into(),
            Node::Unbound { int: false } => "an unknown type".into(),
            Node::Link(_) => "?".into(),
            Node::Bound(s) => match s {
                Shape::Con(ty) => type_name(types, ty),
                Shape::Tuple(ts) => {
                    let parts: Vec<String> = ts.iter().map(|t| self.describe(types, *t)).collect();
                    format!("({})", parts.join(", "))
                }
                Shape::Option(t) => format!("Option<{}>", self.describe(types, t)),
                Shape::Vec(t) => format!("Vec<{}>", self.describe(types, t)),
                Shape::Set(t) => format!("Set<{}>", self.describe(types, t)),
                Shape::Map(k, v) => format!("Map<{}, {}>", self.describe(types, k), self.describe(types, v)),
            },
        }
    }

    fn unify(&mut self, types: &TypeTable, a: T, b: T, span: Span) {
        if !self.unify_inner(types, a, b) {
            let da = self.describe(types, a);
            let db = self.describe(types, b);
            self.error(span, format!("type mismatch: {da} and {db}"));
        }
    }

    fn unify_inner(&mut self, types: &TypeTable, a: T, b: T) -> bool {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return true;
        }
        match (self.node(ra), self.node(rb)) {
            (Node::Unbound { int: ia }, Node::Unbound { int: ib }) => {
                self.set(ra, Node::Link(rb));
                self.set(rb, Node::Unbound { int: ia || ib });
                true
            }
            (Node::Unbound { int }, Node::Bound(s)) => {
                if int && !is_int_shape(types, &s) {
                    return false;
                }
                self.set(ra, Node::Link(rb));
                true
            }
            (Node::Bound(s), Node::Unbound { int }) => {
                if int && !is_int_shape(types, &s) {
                    return false;
                }
                self.set(rb, Node::Link(ra));
                true
            }
            (Node::Bound(sa), Node::Bound(sb)) => {
                let ok = match (&sa, &sb) {
                    (Shape::Con(x), Shape::Con(y)) => {
                        if x == y {
                            true
                        } else if let (Some(TypeDef::Node(p)), Some(TypeDef::Node(q))) = (types.get(*x), types.get(*y))
                        {
                            // The meet of Node<R> and Node is Node<R>.
                            match (p, q) {
                                (Some(_), None) => true,
                                (None, Some(_)) => {
                                    self.set(ra, Node::Bound(Shape::Con(*y)));
                                    true
                                }
                                _ => false,
                            }
                        } else {
                            false
                        }
                    }
                    (Shape::Tuple(xs), Shape::Tuple(ys)) => {
                        xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| self.unify_inner(types, *x, *y))
                    }
                    (Shape::Option(x), Shape::Option(y))
                    | (Shape::Vec(x), Shape::Vec(y))
                    | (Shape::Set(x), Shape::Set(y)) => self.unify_inner(types, *x, *y),
                    (Shape::Map(k1, v1), Shape::Map(k2, v2)) => {
                        self.unify_inner(types, *k1, *k2) && self.unify_inner(types, *v1, *v2)
                    }
                    _ => false,
                };
                if ok {
                    let rb2 = self.find(rb);
                    let ra2 = self.find(ra);
                    if ra2 != rb2 {
                        self.set(rb2, Node::Link(ra2));
                    }
                }
                ok
            }
            _ => false,
        }
    }

    /// The leaf type of a solved term, if it is a leaf.
    fn leaf(&mut self, t: T) -> Option<TypeId> {
        let r = self.find(t);
        match self.node(r) {
            Node::Bound(Shape::Con(ty)) => Some(ty),
            _ => None,
        }
    }

    fn is_unbound(&mut self, t: T) -> bool {
        let r = self.find(t);
        matches!(self.node(r), Node::Unbound { .. })
    }

    /// The solved type of a term (interning structural types), or `None` if it is not fully known.
    fn solved(&mut self, types: &mut TypeTable, t: T) -> Option<TypeId> {
        let r = self.find(t);
        let shape = match self.node(r) {
            Node::Bound(s) => s,
            _ => return None,
        };
        Some(match shape {
            Shape::Con(ty) => ty,
            Shape::Tuple(ts) => {
                let mut ids = Vec::new();
                for t in ts {
                    ids.push(self.solved(types, t)?);
                }
                intern(types, TypeDef::Tuple(ids))
            }
            Shape::Option(t) => {
                let inner = self.solved(types, t)?;
                intern(types, TypeDef::Option(inner))
            }
            Shape::Vec(t) => {
                let inner = self.solved(types, t)?;
                intern(types, TypeDef::Vec(inner))
            }
            Shape::Set(t) => {
                let inner = self.solved(types, t)?;
                intern(types, TypeDef::Set(inner))
            }
            Shape::Map(k, v) => {
                let k = self.solved(types, k)?;
                let v = self.solved(types, v)?;
                intern(types, TypeDef::Map(k, v))
            }
        })
    }

    // ------------------------------------------------------------------ the walk

    fn run(&mut self, hir: &mut Hir) {
        // Terms for variables and view columns.
        for sc in &hir.scopes {
            let n = sc.vars.len();
            let terms = (0..n).map(|_| self.fresh(false)).collect();
            self.var_terms.push(terms);
        }
        let mut view_terms = Vec::new();
        for rel in &hir.rels {
            let mut cols = Vec::new();
            for c in &rel.cols {
                cols.push(match c.ty {
                    Some(ty) => self.of_type(&hir.types, ty),
                    None => self.fresh(false),
                });
            }
            view_terms.push(cols);
        }
        self.view_terms = view_terms;
        self.walk(hir);
        self.solve(hir);
        if self.diags.error_count() > self.errors_before {
            return;
        }
        self.apply = true;
        self.cursor = 0;
        self.walk(hir);
        self.finish(hir);
    }

    fn walk(&mut self, hir: &mut Hir) {
        let mut handlers = std::mem::take(&mut hir.handlers);
        for h in &mut handlers {
            self.body(hir, h.scope, &mut h.header, h.role);
            self.stmts(hir, h.scope, &mut h.stmts, h.role);
        }
        hir.handlers = handlers;
        let mut views = std::mem::take(&mut hir.views);
        for v in &mut views {
            let role = self.rel_of(hir, v.rel).role;
            for (scope, body) in &mut v.alternatives {
                self.body(hir, *scope, body, role);
            }
            if self.apply {
                continue;
            }
            let rel = v.rel.index();
            match &mut v.shape {
                HViewShape::Plain { cols } => {
                    for (c, per_alt) in cols.iter().enumerate() {
                        for (alt, var) in per_alt.iter().enumerate() {
                            let Some(scope) = v.alternatives.get(alt).map(|a| a.0) else {
                                continue;
                            };
                            let vt = self.var_term(scope, *var);
                            let ct = self.col_term(rel, c);
                            self.unify(&hir.types, vt, ct, v.span);
                        }
                    }
                }
                HViewShape::Aggregate { .. } => {}
            }
        }
        for v in &mut views {
            let rel = v.rel.index();
            if let HViewShape::Aggregate {
                union,
                shared,
                cols,
                driver,
            } = &mut v.shape
            {
                let union = *union;
                if !self.apply {
                    for (alt, vars) in shared.iter().enumerate() {
                        let Some(scope) = v.alternatives.get(alt).map(|a| a.0) else {
                            continue;
                        };
                        for (i, var) in vars.iter().enumerate() {
                            let a = self.var_term(scope, *var);
                            let u = self.var_term(union, HVarId(i as u32));
                            self.unify(&hir.types, a, u, v.span);
                        }
                    }
                }
                for (c, col) in cols.iter_mut().enumerate() {
                    let ct = self.col_term(rel, c);
                    match col {
                        HViewAggCol::Group(var) => {
                            if !self.apply {
                                let u = self.var_term(union, *var);
                                self.unify(&hir.types, u, ct, v.span);
                            }
                        }
                        HViewAggCol::Agg(agg) => self.agg(hir, union, agg, ct),
                    }
                }
                let _ = driver;
            }
        }
        hir.views = views;
        let mut facts = std::mem::take(&mut hir.facts);
        for f in &mut facts {
            for (c, e) in f.row.iter_mut().enumerate() {
                // A string in a `Node` column of a fact names a node of the deployment (LANGUAGE §2.4).
                let col_ty = hir
                    .rels
                    .get(f.rel.index())
                    .and_then(|r| r.cols.get(c))
                    .and_then(|c| c.ty);
                if let (HExprKind::Value(blossom_value::Value::Str(_), _), Some(ty)) = (&e.kind, col_ty)
                    && matches!(hir.types.get(ty), Some(TypeDef::Node(_)))
                {
                    e.ty = Some(ty);
                    continue;
                }
                let t = self.expr(hir, f.scope, e);
                if !self.apply {
                    let ct = self.col_term(f.rel.index(), c);
                    self.unify(&hir.types, t, ct, e.span);
                }
            }
        }
        hir.facts = facts;
    }

    fn stmts(&mut self, hir: &mut Hir, scope: ScopeId, stmts: &mut [HStmt], role: Option<HRoleId>) {
        for s in stmts {
            match s {
                HStmt::Verb(v) => self.verb(hir, scope, v),
                HStmt::Block { cond, stmts, .. } => {
                    self.body(hir, scope, cond, role);
                    self.stmts(hir, scope, stmts, role);
                }
            }
        }
    }

    fn verb(&mut self, hir: &mut Hir, scope: ScopeId, v: &mut HVerbStmt) {
        let rel = v.target.index();
        for (c, a) in v.args.iter_mut().enumerate() {
            let ct = self.col_term(rel, c);
            match a {
                HHeadArg::Expr(e) => {
                    let t = self.expr(hir, scope, e);
                    if !self.apply {
                        self.unify(&hir.types, t, ct, e.span);
                    }
                }
                HHeadArg::Agg(agg) => self.agg(hir, scope, agg, ct),
            }
        }
        if let Some(to) = &mut v.to {
            let t = self.expr(hir, scope, to);
            if !self.apply {
                let dst = match &self.rel_of(hir, v.target).kind {
                    HRelKind::Channel(ChannelInfo {
                        direction: Some((_, dst)),
                        ..
                    }) => Some(*dst),
                    _ => None,
                };
                let want = if let Some(dst) = dst {
                    if self.role_kind(hir, dst) == RoleKind::External {
                        self.con(&mut hir.types, TypeDef::Session)
                    } else {
                        self.con(&mut hir.types, TypeDef::Node(None))
                    }
                } else {
                    self.con(&mut hir.types, TypeDef::Node(None))
                };
                self.unify(&hir.types, t, want, to.span);
            }
        }
    }

    fn agg(&mut self, hir: &mut Hir, scope: ScopeId, agg: &mut HAgg, col: T) {
        let mut arg_terms = Vec::new();
        for e in &mut agg.args {
            arg_terms.push(self.expr(hir, scope, e));
        }
        let default = agg.default.as_mut().map(|d| self.expr(hir, scope, d));
        if self.apply {
            return;
        }
        match agg.func {
            AggKind::Count => {
                self.deferred.push(Deferred::IntColumn { t: col, span: agg.span });
                self.count_cols.push(col);
            }
            AggKind::Sum | AggKind::Min | AggKind::Max => {
                if let Some(a) = arg_terms.first() {
                    self.unify(&hir.types, *a, col, agg.span);
                }
                if agg.func == AggKind::Sum {
                    self.deferred.push(Deferred::Arith {
                        op: BinOp::Add,
                        l: col,
                        r: col,
                        res: col,
                        span: agg.span,
                    });
                } else {
                    self.deferred.push(Deferred::Ordered { t: col, span: agg.span });
                }
            }
        }
        if let Some(d) = default {
            self.unify(&hir.types, d, col, agg.span);
        }
    }

    /// The variables bound by the positive literals of a body other than `outer` (and of enclosing bodies, which the
    /// caller passes in `outer_bound`), for typing `outer`'s variables as options.
    fn positively_bound(body: &HBody, out: &mut BTreeSet<HVarId>) {
        fn pat_vars(p: &HPat, out: &mut BTreeSet<HVarId>) {
            match p {
                HPat::Var(v, _) => {
                    out.insert(*v);
                }
                HPat::Tuple(ps, _) | HPat::Variant { fields: ps, .. } => ps.iter().for_each(|x| pat_vars(x, out)),
                HPat::Wild(_) | HPat::Expr(_) => {}
            }
        }
        for l in &body.lits {
            match l {
                HLit::Atom(a) | HLit::Delta { atom: a, .. } | HLit::Per(a) => {
                    a.args.iter().for_each(|p| pat_vars(p, out));
                    if let Some(f) = &a.from {
                        pat_vars(f, out);
                    }
                }
                HLit::Let { pat, .. } | HLit::RangeGen { pat, .. } | HLit::RoleGen { pat, .. } => pat_vars(pat, out),
                _ => {}
            }
        }
    }

    fn body(&mut self, hir: &mut Hir, scope: ScopeId, body: &mut HBody, role: Option<HRoleId>) {
        let mut bound = BTreeSet::new();
        Self::positively_bound(body, &mut bound);
        for l in &mut body.lits {
            self.lit(hir, scope, l, role, &bound);
        }
    }

    fn lit(&mut self, hir: &mut Hir, scope: ScopeId, l: &mut HLit, role: Option<HRoleId>, bound: &BTreeSet<HVarId>) {
        match l {
            HLit::Atom(a) | HLit::Not(a) | HLit::Per(a) | HLit::Delta { atom: a, .. } => {
                self.atom(hir, scope, a, None);
            }
            HLit::Outer(a) => self.atom(hir, scope, a, Some(bound)),
            HLit::NotBody(b, _) => self.body(hir, scope, b, role),
            HLit::Let { pat, expr, span } => {
                let t = self.expr(hir, scope, expr);
                let p = self.pat(hir, scope, pat);
                if !self.apply {
                    self.unify(&hir.types, p, t, *span);
                }
            }
            HLit::Guard(e) => {
                let t = self.expr(hir, scope, e);
                if !self.apply {
                    let b = self.con(&mut hir.types, TypeDef::Bool);
                    self.unify(&hir.types, t, b, e.span);
                }
            }
            HLit::RangeGen { pat, lo, hi, span, .. } => {
                let a = self.expr(hir, scope, lo);
                let b = self.expr(hir, scope, hi);
                let p = self.pat(hir, scope, pat);
                if !self.apply {
                    self.unify(&hir.types, a, b, *span);
                    self.unify(&hir.types, a, p, *span);
                    self.deferred.push(Deferred::IntColumn { t: a, span: *span });
                }
            }
            HLit::RoleGen { pat, role: r, span, .. } => {
                let p = self.pat(hir, scope, pat);
                if !self.apply {
                    let n = self.con(&mut hir.types, TypeDef::Node(Some(RoleId::from_raw(r.0))));
                    self.unify(&hir.types, p, n, *span);
                }
            }
            HLit::Any(bodies, _) => {
                for b in bodies {
                    self.body(hir, scope, b, role);
                }
            }
            HLit::Forall { domain, body, .. } => {
                let empty = BTreeSet::new();
                self.lit(hir, scope, domain, role, &empty);
                self.body(hir, scope, body, role);
            }
        }
    }

    /// An atom: each argument against its column. For `outer`, variables not bound elsewhere are options.
    fn atom(&mut self, hir: &mut Hir, scope: ScopeId, a: &mut HAtom, outer: Option<&BTreeSet<HVarId>>) {
        let rel = a.rel.index();
        for (c, p) in a.args.iter_mut().enumerate() {
            let ct = self.col_term(rel, c);
            let pt = self.pat(hir, scope, p);
            if self.apply {
                continue;
            }
            match (outer, &*p) {
                (Some(bound), HPat::Var(v, span)) if !bound.contains(v) => {
                    let opt = self.bound(Shape::Option(ct));
                    self.unify(&hir.types, pt, opt, *span);
                }
                _ => self.unify(&hir.types, pt, ct, p.span()),
            }
        }
        if let Some(f) = &mut a.from {
            let ft = self.pat(hir, scope, f);
            if !self.apply {
                let src = match &self.rel_of(hir, a.rel).kind {
                    HRelKind::Channel(ChannelInfo {
                        direction: Some((src, _)),
                        ..
                    }) => Some(*src),
                    _ => None,
                };
                let def = match src {
                    Some(r) if self.role_kind(hir, r) == RoleKind::External => TypeDef::Session,
                    Some(r) => TypeDef::Node(Some(RoleId::from_raw(r.0))),
                    None => TypeDef::Node(None),
                };
                let t = self.con(&mut hir.types, def);
                self.unify(&hir.types, ft, t, f.span());
            }
        }
    }

    fn pat(&mut self, hir: &mut Hir, scope: ScopeId, p: &mut HPat) -> T {
        match p {
            HPat::Var(v, _) => self.var_term(scope, *v),
            HPat::Wild(_) => {
                if self.apply {
                    0
                } else {
                    self.fresh(false)
                }
            }
            HPat::Expr(e) => self.expr(hir, scope, e),
            HPat::Tuple(ps, _) => {
                let mut ts = Vec::new();
                for x in ps.iter_mut() {
                    ts.push(self.pat(hir, scope, x));
                }
                if self.apply { 0 } else { self.bound(Shape::Tuple(ts)) }
            }
            HPat::Variant {
                ty,
                variant,
                fields,
                span,
            } => {
                let mut fts = Vec::new();
                for f in fields.iter_mut() {
                    fts.push(self.pat(hir, scope, f));
                }
                self.variant(hir, ty, *variant, &fts, *span)
            }
        }
    }

    /// The term of a variant constructor or pattern; in the apply walk, resolves `Option` to its type.
    fn variant(&mut self, hir: &mut Hir, ty: &mut TypeRef, variant: u32, fields: &[T], span: Span) -> T {
        if self.apply {
            let t = self.next_term();
            if *ty == TypeRef::Option {
                match self.solved(&mut hir.types, t) {
                    Some(id) => *ty = TypeRef::Known(id),
                    None => self.error(span, "cannot infer the type of this `Option`".into()),
                }
            }
            return t;
        }
        let t = match *ty {
            TypeRef::Option => {
                let inner = self.fresh(false);
                if variant == 1
                    && let Some(f) = fields.first()
                {
                    self.unify(&hir.types, inner, *f, span);
                }
                self.bound(Shape::Option(inner))
            }
            TypeRef::Known(id) => {
                let payload: Vec<TypeId> = match hir.types.get(id) {
                    Some(TypeDef::Enum(e)) => e
                        .variants
                        .iter()
                        .find(|v| v.number == variant)
                        .map(|v| v.payload.iter().map(|f| f.ty).collect())
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                for (f, pty) in fields.iter().zip(payload) {
                    let pt = self.of_type(&hir.types, pty);
                    self.unify(&hir.types, *f, pt, span);
                }
                self.bound(Shape::Con(id))
            }
        };
        self.expr_terms.push(t);
        t
    }

    fn next_term(&mut self) -> T {
        let t = self.expr_terms.get(self.cursor).copied().unwrap_or(0);
        self.cursor += 1;
        t
    }

    /// An expression's term. In the apply walk, writes the solved type into the expression.
    fn expr(&mut self, hir: &mut Hir, scope: ScopeId, e: &mut HExpr) -> T {
        let span = e.span;
        let t = match &mut e.kind {
            HExprKind::Var(v) => {
                let t = self.var_term(scope, *v);
                self.record(t)
            }
            HExprKind::Value(_, ty) => {
                let ty = *ty;
                if self.apply {
                    self.next_term()
                } else {
                    let t = self.of_type(&hir.types, ty);
                    self.record(t)
                }
            }
            HExprKind::IntLit(..) => {
                if self.apply {
                    self.next_term()
                } else {
                    let t = self.fresh(true);
                    self.record(t)
                }
            }
            HExprKind::TypedInt(_, ity, _) => {
                let ity = *ity;
                if self.apply {
                    self.next_term()
                } else {
                    let t = self.con(&mut hir.types, TypeDef::Int(ity));
                    self.record(t)
                }
            }
            HExprKind::Binary { op, lhs, rhs } => {
                let op = *op;
                let a = self.expr(hir, scope, lhs);
                let b = self.expr(hir, scope, rhs);
                if self.apply {
                    self.next_term()
                } else {
                    let t = match op {
                        BinOp::Eq | BinOp::Ne => {
                            self.unify(&hir.types, a, b, span);
                            self.con(&mut hir.types, TypeDef::Bool)
                        }
                        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                            self.unify(&hir.types, a, b, span);
                            self.deferred.push(Deferred::Ordered { t: a, span });
                            self.con(&mut hir.types, TypeDef::Bool)
                        }
                        BinOp::And | BinOp::Or => {
                            let bt = self.con(&mut hir.types, TypeDef::Bool);
                            self.unify(&hir.types, a, bt, span);
                            self.unify(&hir.types, b, bt, span);
                            bt
                        }
                        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                            let res = self.fresh(false);
                            self.deferred.push(Deferred::Arith {
                                op,
                                l: a,
                                r: b,
                                res,
                                span,
                            });
                            res
                        }
                        BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                            self.unify(&hir.types, a, b, span);
                            self.deferred.push(Deferred::IntColumn { t: a, span });
                            a
                        }
                        _ => {
                            self.error(span, "this operator is not a value operator".into());
                            a
                        }
                    };
                    self.record(t)
                }
            }
            HExprKind::Prefix { op, arg } => {
                let op = *op;
                let a = self.expr(hir, scope, arg);
                if self.apply {
                    self.next_term()
                } else {
                    let t = match op {
                        PrefixOp::Not => {
                            let bt = self.con(&mut hir.types, TypeDef::Bool);
                            self.unify(&hir.types, a, bt, span);
                            bt
                        }
                        PrefixOp::Neg => {
                            self.deferred.push(Deferred::Arith {
                                op: BinOp::Sub,
                                l: a,
                                r: a,
                                res: a,
                                span,
                            });
                            a
                        }
                        PrefixOp::BitNot => {
                            self.deferred.push(Deferred::IntColumn { t: a, span });
                            a
                        }
                    };
                    self.record(t)
                }
            }
            HExprKind::Tuple(es) => {
                let mut ts = Vec::new();
                for x in es.iter_mut() {
                    ts.push(self.expr(hir, scope, x));
                }
                if self.apply {
                    self.next_term()
                } else {
                    let t = self.bound(Shape::Tuple(ts));
                    self.record(t)
                }
            }
            HExprKind::Variant { ty, variant, fields } => {
                let variant = *variant;
                let mut fts = Vec::new();
                for f in fields.iter_mut() {
                    fts.push(self.expr(hir, scope, f));
                }
                let mut tyref = *ty;
                let t = self.variant(hir, &mut tyref, variant, &fts, span);
                *ty = tyref;
                t
            }
            HExprKind::Struct { ty, fields } => {
                let ty = *ty;
                let ftys: Vec<TypeId> = match hir.types.get(ty) {
                    Some(TypeDef::Struct(s)) => s.fields.iter().map(|f| f.ty).collect(),
                    _ => Vec::new(),
                };
                for (f, fty) in fields.iter_mut().zip(ftys) {
                    let ft = self.expr(hir, scope, f);
                    if !self.apply {
                        let want = self.of_type(&hir.types, fty);
                        self.unify(&hir.types, ft, want, f.span);
                    }
                }
                if self.apply {
                    self.next_term()
                } else {
                    let t = self.bound(Shape::Con(ty));
                    self.record(t)
                }
            }
            HExprKind::TupleIndex { base, index } => {
                let index = *index;
                let b = self.expr(hir, scope, base);
                if self.apply {
                    self.next_term()
                } else {
                    let res = self.fresh(false);
                    self.deferred.push(Deferred::TupleIndex {
                        base: b,
                        index,
                        res,
                        span,
                    });
                    self.record(res)
                }
            }
            HExprKind::Field { base, name, index } => {
                let name = *name;
                let b = self.expr(hir, scope, base);
                if self.apply {
                    let bt = self.leaf(b);
                    if let Some(TypeDef::Struct(s)) = bt.and_then(|t| hir.types.get(t)) {
                        *index = s.fields.iter().position(|f| f.name == name).map(|i| i as u32);
                    }
                    self.next_term()
                } else {
                    let res = self.fresh(false);
                    self.deferred.push(Deferred::Field {
                        base: b,
                        name,
                        res,
                        span,
                    });
                    self.record(res)
                }
            }
            HExprKind::If { cond, then, els } => {
                let c = self.expr(hir, scope, cond);
                let a = self.expr(hir, scope, then);
                let b = self.expr(hir, scope, els);
                if self.apply {
                    self.next_term()
                } else {
                    let bt = self.con(&mut hir.types, TypeDef::Bool);
                    self.unify(&hir.types, c, bt, span);
                    self.unify(&hir.types, a, b, span);
                    self.record(a)
                }
            }
            HExprKind::Match { scrut, arms } => {
                let s = self.expr(hir, scope, scrut);
                let res = if self.apply { 0 } else { self.fresh(false) };
                for (p, g, body) in arms.iter_mut() {
                    let pt = self.pat(hir, scope, p);
                    let gt = g.as_mut().map(|g| self.expr(hir, scope, g));
                    let bt = self.expr(hir, scope, body);
                    if !self.apply {
                        self.unify(&hir.types, pt, s, span);
                        if let Some(gt) = gt {
                            let b = self.con(&mut hir.types, TypeDef::Bool);
                            self.unify(&hir.types, gt, b, span);
                        }
                        self.unify(&hir.types, bt, res, span);
                    }
                }
                if self.apply { self.next_term() } else { self.record(res) }
            }
            HExprKind::Cast { expr, ty } => {
                let ty = *ty;
                let a = self.expr(hir, scope, expr);
                if self.apply {
                    self.next_term()
                } else {
                    let to = self.of_type(&hir.types, ty);
                    self.deferred.push(Deferred::Cast { from: a, to, span });
                    self.record(to)
                }
            }
            HExprKind::SelfNode => {
                if self.apply {
                    self.next_term()
                } else {
                    let t = self.con(&mut hir.types, TypeDef::Node(None));
                    self.record(t)
                }
            }
            HExprKind::Now => {
                if self.apply {
                    self.next_term()
                } else {
                    let t = self.con(&mut hir.types, TypeDef::Instant);
                    self.record(t)
                }
            }
            HExprKind::Tick => {
                if self.apply {
                    self.next_term()
                } else {
                    let t = self.con(&mut hir.types, TypeDef::Int(IntTy::U64));
                    self.record(t)
                }
            }
            HExprKind::Builtin { f, args } => {
                let f = *f;
                let mut ats = Vec::new();
                for a in args.iter_mut() {
                    ats.push(self.expr(hir, scope, a));
                }
                if self.apply {
                    self.next_term()
                } else {
                    let t = match f {
                        Builtin::Len => {
                            if let Some(b) = ats.first() {
                                self.deferred.push(Deferred::Len { base: *b, span });
                            }
                            self.con(&mut hir.types, TypeDef::Int(IntTy::U64))
                        }
                        Builtin::RoleSize(_) => self.con(&mut hir.types, TypeDef::Int(IntTy::U64)),
                    };
                    self.record(t)
                }
            }
        };
        if self.apply {
            match self.solved(&mut hir.types, t) {
                Some(ty) => e.ty = Some(ty),
                None => self.error(span, "cannot infer the type of this expression".into()),
            }
        }
        t
    }

    /// In the collect walk, remembers `t` as the next expression's term; in the apply walk, returns the recorded one.
    fn record(&mut self, t: T) -> T {
        if self.apply {
            self.next_term()
        } else {
            self.expr_terms.push(t);
            t
        }
    }

    // ------------------------------------------------------------------ solving

    fn solve(&mut self, hir: &mut Hir) {
        let mut defaulted = false;
        loop {
            let mut progress = true;
            while progress {
                progress = false;
                let pending = std::mem::take(&mut self.deferred);
                for d in pending {
                    if self.try_deferred(hir, &d) {
                        progress = true;
                    } else {
                        self.deferred.push(d);
                    }
                }
            }
            if self.deferred.is_empty() && defaulted {
                break;
            }
            if defaulted {
                break;
            }
            // An unconstrained count column is u64; then integer literals default to i64 (LANGUAGE §5.6).
            let u64t = intern(&mut hir.types, TypeDef::Int(IntTy::U64));
            for t in self.count_cols.clone() {
                if self.is_unbound(t) {
                    let r = self.find(t);
                    self.set(r, Node::Bound(Shape::Con(u64t)));
                }
            }
            let i64t = intern(&mut hir.types, TypeDef::Int(IntTy::I64));
            for n in &mut self.uf {
                if let Node::Unbound { int: true } = n {
                    *n = Node::Bound(Shape::Con(i64t));
                }
            }
            defaulted = true;
        }
        let pending = std::mem::take(&mut self.deferred);
        for d in pending {
            let span = match d {
                Deferred::Arith { span, .. }
                | Deferred::Field { span, .. }
                | Deferred::TupleIndex { span, .. }
                | Deferred::Cast { span, .. }
                | Deferred::Len { span, .. }
                | Deferred::Ordered { span, .. }
                | Deferred::IntColumn { span, .. } => span,
            };
            self.error(span, "cannot infer the operand types of this expression".into());
        }
    }

    /// Tries a deferred constraint; `true` when it was discharged (successfully or with an error).
    fn try_deferred(&mut self, hir: &mut Hir, d: &Deferred) -> bool {
        match *d {
            Deferred::Arith { op, l, r, res, span } => {
                let (lt, rt) = (self.leaf(l), self.leaf(r));
                let ld = lt.and_then(|t| hir.types.get(t).cloned());
                let rd = rt.and_then(|t| hir.types.get(t).cloned());
                match (&ld, &rd) {
                    (Some(TypeDef::Instant), _) | (_, Some(TypeDef::Instant)) => {
                        match (op, &ld, &rd) {
                            (BinOp::Sub, Some(TypeDef::Instant), Some(TypeDef::Instant)) => {
                                let d = self.con(&mut hir.types, TypeDef::Duration);
                                self.unify(&hir.types, res, d, span);
                            }
                            (BinOp::Add | BinOp::Sub, Some(TypeDef::Instant), _) => {
                                let d = self.con(&mut hir.types, TypeDef::Duration);
                                self.unify(&hir.types, r, d, span);
                                let i = self.con(&mut hir.types, TypeDef::Instant);
                                self.unify(&hir.types, res, i, span);
                            }
                            (BinOp::Add, _, Some(TypeDef::Instant)) => {
                                let d = self.con(&mut hir.types, TypeDef::Duration);
                                self.unify(&hir.types, l, d, span);
                                let i = self.con(&mut hir.types, TypeDef::Instant);
                                self.unify(&hir.types, res, i, span);
                            }
                            _ if ld.is_none() || rd.is_none() => return false,
                            _ => self.error(span, "invalid arithmetic on Instant".into()),
                        }
                        true
                    }
                    (Some(TypeDef::Duration), _) | (_, Some(TypeDef::Duration)) => {
                        if matches!(op, BinOp::Add | BinOp::Sub) {
                            self.unify(&hir.types, l, r, span);
                            self.unify(&hir.types, l, res, span);
                        } else {
                            self.unsupported(span, "scaling durations");
                        }
                        true
                    }
                    (Some(_), _) | (_, Some(_)) => {
                        self.unify(&hir.types, l, r, span);
                        self.unify(&hir.types, l, res, span);
                        let t = self.leaf(l);
                        if !t.is_some_and(|t| matches!(hir.types.get(t), Some(TypeDef::Int(_)))) {
                            let dl = self.describe(&hir.types, l);
                            self.error(
                                span,
                                format!("arithmetic needs integers, Duration or Instant, found {dl}"),
                            );
                        }
                        true
                    }
                    _ => {
                        // Both unknown: if either is an integer literal, they are one integer type.
                        let (rl, rr) = (self.find(l), self.find(r));
                        let int_l = matches!(self.node(rl), Node::Unbound { int: true });
                        let int_r = matches!(self.node(rr), Node::Unbound { int: true });
                        if int_l && int_r {
                            self.unify(&hir.types, l, r, span);
                            self.unify(&hir.types, l, res, span);
                            return true;
                        }
                        false
                    }
                }
            }
            Deferred::Field { base, name, res, span } => {
                let Some(bt) = self.leaf(base) else {
                    return !self.is_unbound(base) && {
                        self.error(span, format!("no field `{}` on a non-struct value", name.as_str()));
                        true
                    };
                };
                match hir.types.get(bt).cloned() {
                    Some(TypeDef::Struct(s)) => match s.fields.iter().find(|f| f.name == name) {
                        Some(f) => {
                            let ft = self.of_type(&hir.types, f.ty);
                            self.unify(&hir.types, res, ft, span);
                        }
                        None => self.error(span, format!("no field `{}`", name.as_str())),
                    },
                    _ => self.error(
                        span,
                        format!("no field `{}` on {}", name.as_str(), type_name(&hir.types, bt)),
                    ),
                }
                true
            }
            Deferred::TupleIndex { base, index, res, span } => {
                let r = self.find(base);
                match self.node(r) {
                    Node::Bound(Shape::Tuple(ts)) => match ts.get(index as usize) {
                        Some(t) => self.unify(&hir.types, res, *t, span),
                        None => self.error(span, format!("the tuple has no element {index}")),
                    },
                    Node::Bound(_) => self.error(span, "tuple indexing on a non-tuple".into()),
                    _ => return false,
                }
                true
            }
            Deferred::Cast { from, to, span } => {
                let (Some(a), Some(b)) = (self.leaf(from), self.leaf(to)) else {
                    if self.is_unbound(from) {
                        let r = self.find(from);
                        if matches!(self.node(r), Node::Unbound { int: true }) {
                            // `5 as u8`: the literal takes the target type if it is an integer.
                            if let Some(b) = self.leaf(to)
                                && matches!(hir.types.get(b), Some(TypeDef::Int(_)))
                            {
                                self.unify(&hir.types, from, to, span);
                                return true;
                            }
                        }
                        return false;
                    }
                    self.error(span, "casts convert between integer types".into());
                    return true;
                };
                let ok = matches!(
                    (hir.types.get(a), hir.types.get(b)),
                    (Some(TypeDef::Int(_)), Some(TypeDef::Int(_)))
                );
                if !ok {
                    self.unsupported(span, "casts other than between integer types");
                }
                true
            }
            Deferred::Len { base, span } => {
                let r = self.find(base);
                match self.node(r) {
                    Node::Bound(Shape::Vec(_) | Shape::Set(_) | Shape::Map(..)) => true,
                    Node::Bound(Shape::Con(t)) => {
                        if !matches!(hir.types.get(t), Some(TypeDef::Str | TypeDef::Bytes)) {
                            self.error(span, "`.len()` needs a string, bytes or a collection".into());
                        }
                        true
                    }
                    Node::Bound(_) => {
                        self.error(span, "`.len()` needs a string, bytes or a collection".into());
                        true
                    }
                    _ => false,
                }
            }
            Deferred::Ordered { t, span } => {
                let Some(ty) = self.leaf(t) else {
                    return !self.is_unbound(t);
                };
                if matches!(
                    hir.types.get(ty),
                    Some(TypeDef::Bool | TypeDef::Enum(_) | TypeDef::Struct(_))
                ) {
                    self.error(
                        span,
                        format!(
                            "`<` compares numbers, strings, durations and instants, not {}",
                            type_name(&hir.types, ty)
                        ),
                    );
                }
                true
            }
            Deferred::IntColumn { t, span } => {
                if self.is_unbound(t) {
                    let r = self.find(t);
                    self.set(r, Node::Unbound { int: true });
                    return true;
                }
                let ok = self
                    .leaf(t)
                    .is_some_and(|ty| matches!(hir.types.get(ty), Some(TypeDef::Int(_))));
                if !ok {
                    let d = self.describe(&hir.types, t);
                    self.error(span, format!("expected an integer, found {d}"));
                }
                true
            }
        }
    }

    fn unsupported(&mut self, span: Span, what: &str) {
        self.diags.push(
            Diagnostic::not_implemented(
                blossom_base::FeatureId("LANG-084"),
                what,
                "the Blossom frontend (slice 2)",
            )
            .with_primary(span),
        );
    }

    /// Writes variable and view column types.
    fn finish(&mut self, hir: &mut Hir) {
        let mut var_types = Vec::new();
        for (sc, terms) in hir.scopes.iter().zip(self.var_terms.clone()) {
            let mut tys = Vec::new();
            for (v, t) in sc.vars.iter().zip(terms) {
                match self.solved(&mut hir.types, t) {
                    Some(ty) => tys.push(ty),
                    None => {
                        self.error(v.span, format!("cannot infer the type of `{}`", v.name.as_str()));
                        tys.push(intern(&mut hir.types, TypeDef::Unit));
                    }
                }
            }
            var_types.push(tys);
        }
        hir.var_types = var_types;
        let view_terms = self.view_terms.clone();
        let mut types = std::mem::take(&mut hir.types);
        for (rel, cols) in hir.rels.iter_mut().zip(view_terms) {
            for (col, t) in rel.cols.iter_mut().zip(cols) {
                if col.ty.is_some() {
                    continue;
                }
                match self.solved(&mut types, t) {
                    Some(ty) => col.ty = Some(ty),
                    None => self.diags.push(
                        Diagnostic::new(
                            code!("BLS0310"),
                            format!("cannot infer the type of column `{}` of `{}`", col.name, rel.name),
                        )
                        .with_primary(rel.span),
                    ),
                }
            }
        }
        hir.types = types;
    }
}

fn intern(types: &mut TypeTable, def: TypeDef) -> TypeId {
    match types.insert(def) {
        Ok(t) => t,
        Err(_) => types.insert(TypeDef::Unit).unwrap_or(TypeId::from_raw(0)),
    }
}

fn is_int_shape(types: &TypeTable, s: &Shape) -> bool {
    matches!(s, Shape::Con(t) if matches!(types.get(*t), Some(TypeDef::Int(_))))
}

/// A readable type name for messages.
pub(crate) fn type_name(types: &TypeTable, ty: TypeId) -> String {
    match types.get(ty) {
        Some(TypeDef::Bool) => "bool".into(),
        Some(TypeDef::Int(i)) => i.name().into(),
        Some(TypeDef::F64) => "f64".into(),
        Some(TypeDef::Str) => "String".into(),
        Some(TypeDef::Bytes) => "Bytes".into(),
        Some(TypeDef::Unit) => "()".into(),
        Some(TypeDef::Duration) => "Duration".into(),
        Some(TypeDef::Instant) => "Instant".into(),
        Some(TypeDef::Session) => "Session".into(),
        Some(TypeDef::Principal) => "Principal".into(),
        Some(TypeDef::Node(None)) => "Node".into(),
        Some(TypeDef::Node(Some(r))) => format!("Node<role {}>", r.index()),
        Some(TypeDef::Tuple(ts)) => {
            let parts: Vec<String> = ts.iter().map(|t| type_name(types, *t)).collect();
            format!("({})", parts.join(", "))
        }
        Some(TypeDef::Option(t)) => format!("Option<{}>", type_name(types, *t)),
        Some(TypeDef::Vec(t)) => format!("Vec<{}>", type_name(types, *t)),
        Some(TypeDef::Set(t)) => format!("Set<{}>", type_name(types, *t)),
        Some(TypeDef::Map(k, v)) => format!("Map<{}, {}>", type_name(types, *k), type_name(types, *v)),
        Some(TypeDef::Struct(s)) => s.name.to_string(),
        Some(TypeDef::Enum(e)) => e.name.to_string(),
        Some(other) => format!("{other:?}"),
        None => "?".into(),
    }
}
