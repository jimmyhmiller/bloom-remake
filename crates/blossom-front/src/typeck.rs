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

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::{Diagnostic, Diagnostics, InternalError, RoleId, Span, Symbol, TypeId, code, internal_error};
use blossom_ir::core::LatticeCtor;
use blossom_lattice::{Kind, Op};
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
        lifts: Vec::new(),
        lift_cursor: 0,
        methods: Vec::new(),
        method_cursor: 0,
        bindings: BTreeMap::new(),
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
    /// A built-in lattice (LANGUAGE §11.5); a map's value term is a lattice.
    Lat(LatS),
}

#[derive(Clone, Debug, PartialEq)]
enum LatS {
    Bool,
    Max(T),
    Min(T),
    Set(T),
    PSet(T),
    Point(T),
    Map(T, T),
}

impl LatS {
    fn args(&self) -> Vec<T> {
        match self {
            LatS::Bool => Vec::new(),
            LatS::Max(e) | LatS::Min(e) | LatS::Set(e) | LatS::PSet(e) | LatS::Point(e) => vec![*e],
            LatS::Map(k, v) => vec![*k, *v],
        }
    }

    fn name(&self) -> &'static str {
        match self {
            LatS::Bool => "LBool",
            LatS::Max(_) => "LMax",
            LatS::Min(_) => "LMin",
            LatS::Set(_) => "LSet",
            LatS::PSet(_) => "LPSet",
            LatS::Point(_) => "LPoint",
            LatS::Map(..) => "LMap",
        }
    }

    /// The element term of a lattice with one element type.
    fn elem(&self) -> Option<T> {
        match self {
            LatS::Max(e) | LatS::Min(e) | LatS::Set(e) | LatS::PSet(e) | LatS::Point(e) => Some(*e),
            _ => None,
        }
    }
}

/// One binding occurrence of a variable: a positive atom's column, or `None` for any other binding.
type Binding = Option<(HRelId, usize)>;

/// A method call on a lattice value, resolved by the solver: the operation and, per argument (receiver excluded),
/// the coercion slot through which it may be lifted.
#[derive(Clone, Debug)]
struct MethodRes {
    target: MethodTarget,
    lifts: Vec<Option<usize>>,
}

/// What a method call resolved to.
#[derive(Clone, Copy, Debug)]
enum MethodTarget {
    /// An operation of the receiver's lattice.
    Lattice(Op),
    /// A method of a plain value (the standard library, Appendix B).
    Plain(Builtin),
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
    /// `a ++ b`: strings, bytes or vectors.
    Concat { t: T, span: Span },
    /// A value of term `from` where `to` is expected: lifted when `to` is a lattice and `from` is not (LANGUAGE §5.6).
    Coerce { slot: usize, from: T, to: T, span: Span },
    /// `l op r` for `<`, `<=`, `>`, `>=`: plain operands, or a lattice threshold against a scalar (§11.4).
    Compare { op: BinOp, l: T, r: T, span: Span },
    /// `r[k̄]` on a lattice-valued relation.
    Lookup {
        rel: HRelId,
        keys: Vec<T>,
        res: T,
        span: Span,
    },
    /// `elem in coll` as a test.
    In { elem: T, coll: T, span: Span },
    /// `pat in src` as a generator.
    Gen { pat: T, src: T, span: Span },
    /// `recv.name(args)` (or `reveal!(recv)`).
    Method {
        slot: usize,
        recv: T,
        name: Symbol,
        banged: bool,
        /// The receiver's variable, when it is one (its bindings decide the non-⊥ refinement, SEM-101 N4).
        recv_var: Option<(u32, u32)>,
        args: Vec<T>,
        res: T,
        span: Span,
    },
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
    /// Coercion sites in walk order: whether the solver lifted the value, and the expected term.
    lifts: Vec<(bool, T)>,
    lift_cursor: usize,
    /// Method calls in walk order and their resolutions.
    methods: Vec<Option<MethodRes>>,
    method_cursor: usize,
    /// Every binding occurrence of each variable (per scope): the relation and column of a positive atom that binds it
    /// directly, or `None` for any other binding (a `let`, a generator, a nested pattern, `outer`).
    bindings: BTreeMap<(u32, u32), Vec<Binding>>,
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

    /// The term of column `c` of relation `rel` at one use. A declared column's type is instantiated afresh at every
    /// use, so a use that meets it with `Node<R>` refines only itself; an inferred view column has one shared term.
    fn col_term(&mut self, hir: &Hir, rel: usize, c: usize) -> T {
        if let Some(ty) = hir.rels.get(rel).and_then(|r| r.cols.get(c)).and_then(|c| c.ty) {
            return self.of_type(hir, ty);
        }
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
    fn of_type(&mut self, hir: &Hir, ty: TypeId) -> T {
        let shape = match hir.types.get(ty) {
            Some(TypeDef::Tuple(ts)) => {
                let ts = ts.clone();
                Shape::Tuple(ts.iter().map(|t| self.of_type(hir, *t)).collect())
            }
            Some(TypeDef::Option(t)) => {
                let t = *t;
                Shape::Option(self.of_type(hir, t))
            }
            Some(TypeDef::Vec(t)) => {
                let t = *t;
                Shape::Vec(self.of_type(hir, t))
            }
            Some(TypeDef::Set(t)) => {
                let t = *t;
                Shape::Set(self.of_type(hir, t))
            }
            Some(TypeDef::Map(k, v)) => {
                let (k, v) = (*k, *v);
                let k = self.of_type(hir, k);
                let v = self.of_type(hir, v);
                Shape::Map(k, v)
            }
            Some(TypeDef::Lattice(_)) => match hir.lattice_of(ty).map(|(_, c)| c.clone()) {
                Some(ctor) => Shape::Lat(match ctor {
                    LatticeCtor::Bool => LatS::Bool,
                    LatticeCtor::Max(t) => LatS::Max(self.of_type(hir, t)),
                    LatticeCtor::Min(t) => LatS::Min(self.of_type(hir, t)),
                    LatticeCtor::Set(t) => LatS::Set(self.of_type(hir, t)),
                    LatticeCtor::PSet(t) => LatS::PSet(self.of_type(hir, t)),
                    LatticeCtor::Point(t) => LatS::Point(self.of_type(hir, t)),
                    LatticeCtor::Map(k, inner) => {
                        let k = self.of_type(hir, k);
                        let v = match hir.types.lookup(&TypeDef::Lattice(inner)) {
                            Some(v) => self.of_type(hir, v),
                            None => {
                                self.bugs.push(internal_error!("lattice {inner:?} has no type"));
                                self.fresh(false)
                            }
                        };
                        LatS::Map(k, v)
                    }
                    other => {
                        self.bugs
                            .push(internal_error!("lattice {other:?} reached type checking"));
                        return self.fresh(false);
                    }
                }),
                None => {
                    self.bugs
                        .push(internal_error!("type {ty:?} names an undeclared lattice"));
                    Shape::Con(ty)
                }
            },
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
                Shape::Lat(l) => {
                    let args: Vec<String> = l.args().iter().map(|t| self.describe(types, *t)).collect();
                    if args.is_empty() {
                        l.name().to_owned()
                    } else {
                        format!("{}<{}>", l.name(), args.join(", "))
                    }
                }
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
                    (Shape::Lat(x), Shape::Lat(y)) => {
                        let (xs, ys) = (x.args(), y.args());
                        x.name() == y.name()
                            && xs.len() == ys.len()
                            && xs.iter().zip(&ys).all(|(a, b)| self.unify_inner(types, *a, *b))
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

    /// The solved type of a term (interning structural and lattice types), or `None` if it is not fully known.
    fn solved(&mut self, hir: &mut Hir, t: T) -> Option<TypeId> {
        let r = self.find(t);
        if let Node::Bound(Shape::Lat(l)) = self.node(r) {
            let ctor = match l {
                LatS::Bool => LatticeCtor::Bool,
                LatS::Max(e) => LatticeCtor::Max(self.solved(hir, e)?),
                LatS::Min(e) => LatticeCtor::Min(self.solved(hir, e)?),
                LatS::Set(e) => LatticeCtor::Set(self.solved(hir, e)?),
                LatS::PSet(e) => LatticeCtor::PSet(self.solved(hir, e)?),
                LatS::Point(e) => LatticeCtor::Point(self.solved(hir, e)?),
                LatS::Map(k, v) => {
                    let k = self.solved(hir, k)?;
                    let v = self.solved(hir, v)?;
                    let (inner, _) = hir.lattice_of(v)?;
                    LatticeCtor::Map(k, inner)
                }
            };
            return match hir.intern_lattice(ctor) {
                Ok(ty) => Some(ty),
                Err(e) => {
                    self.bugs.push(e);
                    None
                }
            };
        }
        self.solved_types(hir, t)
    }

    fn solved_types(&mut self, hir: &mut Hir, t: T) -> Option<TypeId> {
        let r = self.find(t);
        let shape = match self.node(r) {
            Node::Bound(s) => s,
            _ => return None,
        };
        Some(match shape {
            Shape::Con(ty) => ty,
            Shape::Lat(_) => return None,
            Shape::Tuple(ts) => {
                let mut ids = Vec::new();
                for t in ts {
                    ids.push(self.solved(hir, t)?);
                }
                intern(&mut hir.types, TypeDef::Tuple(ids))
            }
            Shape::Option(t) => {
                let inner = self.solved(hir, t)?;
                intern(&mut hir.types, TypeDef::Option(inner))
            }
            Shape::Vec(t) => {
                let inner = self.solved(hir, t)?;
                intern(&mut hir.types, TypeDef::Vec(inner))
            }
            Shape::Set(t) => {
                let inner = self.solved(hir, t)?;
                intern(&mut hir.types, TypeDef::Set(inner))
            }
            Shape::Map(k, v) => {
                let k = self.solved(hir, k)?;
                let v = self.solved(hir, v)?;
                intern(&mut hir.types, TypeDef::Map(k, v))
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
                    Some(ty) => self.of_type(hir, ty),
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
        self.lift_cursor = 0;
        self.method_cursor = 0;
        self.walk(hir);
        self.finish(hir);
        lattice_keys(hir, self.diags);
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
                            let ct = self.col_term(hir, rel, c);
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
                    let ct = self.col_term(hir, rel, c);
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
        let mut invariants = std::mem::take(&mut hir.invariants);
        for inv in &mut invariants {
            self.body(hir, inv.scope, &mut inv.body, inv.role);
        }
        hir.invariants = invariants;
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
                let ct = self.col_term(hir, f.rel.index(), c);
                self.coerce_site(hir, e, t, ct);
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
            let ct = self.col_term(hir, rel, c);
            match a {
                HHeadArg::Expr(e) => {
                    let t = self.expr(hir, scope, e);
                    self.coerce_site(hir, e, t, ct);
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
            AggKind::Index => {
                let u64t = self.con(&mut hir.types, TypeDef::Int(IntTy::U64));
                self.unify(&hir.types, col, u64t, agg.span);
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
                HLit::Let { pat, .. }
                | HLit::RangeGen { pat, .. }
                | HLit::RoleGen { pat, .. }
                | HLit::Gen { pat, .. } => pat_vars(pat, out),
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
            HLit::Atom(a) | HLit::Per(a) | HLit::Delta { atom: a, .. } => {
                if !self.apply {
                    for (c, p) in a.args.iter().enumerate() {
                        match p {
                            HPat::Var(v, _) => self.bindings.entry((scope.0, v.0)).or_default().push(Some((a.rel, c))),
                            other => self.other_bindings(scope, other),
                        }
                    }
                }
                self.atom(hir, scope, a, None);
            }
            HLit::Not(a) => self.atom(hir, scope, a, None),
            HLit::Outer(a) => {
                if !self.apply {
                    for p in &a.args {
                        self.other_bindings(scope, p);
                    }
                }
                self.atom(hir, scope, a, Some(bound))
            }
            HLit::NotBody(b, _) => self.body(hir, scope, b, role),
            HLit::Let { pat, expr, span } => {
                if !self.apply {
                    self.other_bindings(scope, pat);
                }
                let t = self.expr(hir, scope, expr);
                let p = self.pat(hir, scope, pat);
                if matches!(pat, HPat::Var(..)) {
                    // `let x = e` where `x` is a lattice (a lattice view column, say) lifts `e` (LANGUAGE §5.6).
                    self.coerce_site(hir, expr, t, p);
                } else if !self.apply {
                    self.unify(&hir.types, p, t, *span);
                }
            }
            HLit::Choose(c) => {
                for e in c.chosen.iter_mut().chain(c.per.iter_mut()) {
                    self.expr(hir, scope, e);
                }
                if let Some((e, _)) = &mut c.cost {
                    let t = self.expr(hir, scope, e);
                    if !self.apply {
                        self.deferred.push(Deferred::Ordered { t, span: e.span });
                    }
                }
            }
            HLit::Gen { pat, src, span } => {
                if !self.apply {
                    self.other_bindings(scope, pat);
                }
                let t = self.expr(hir, scope, src);
                let p = self.pat(hir, scope, pat);
                if !self.apply {
                    self.deferred.push(Deferred::Gen {
                        pat: p,
                        src: t,
                        span: *span,
                    });
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
            let ct = self.col_term(hir, rel, c);
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
                match self.solved(hir, t) {
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
                    let pt = self.of_type(hir, pty);
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
                    let t = self.of_type(hir, ty);
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
                    if matches!(op, BinOp::Eq | BinOp::Ne) && lhs.ty.is_some_and(|t| hir.lattice_of(t).is_some()) {
                        self.diags.push(
                            Diagnostic::new(
                                code!("BLS0305"),
                                "`==` and `!=` do not apply to lattice values: compare with a threshold, `a.leq!(b)`, \
                                 or `reveal!`",
                            )
                            .with_primary(span),
                        );
                    }
                    self.next_term()
                } else {
                    let t = match op {
                        BinOp::Eq | BinOp::Ne => {
                            self.unify(&hir.types, a, b, span);
                            self.con(&mut hir.types, TypeDef::Bool)
                        }
                        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                            self.deferred.push(Deferred::Compare { op, l: a, r: b, span });
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
                        BinOp::Concat => {
                            self.unify(&hir.types, a, b, span);
                            self.deferred.push(Deferred::Concat { t: a, span });
                            a
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
                        let want = self.of_type(hir, fty);
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
                    let to = self.of_type(hir, ty);
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
                        Builtin::RandRange => {
                            // `lo` and `hi` share a type that subtracts to itself: an integer or a duration.
                            match (ats.first().copied(), ats.get(1).copied()) {
                                (Some(lo), Some(hi)) => {
                                    self.unify(&hir.types, lo, hi, span);
                                    self.deferred.push(Deferred::Arith {
                                        op: BinOp::Sub,
                                        l: lo,
                                        r: hi,
                                        res: lo,
                                        span,
                                    });
                                    lo
                                }
                                _ => self.fresh(false),
                            }
                        }
                        Builtin::Majority(role) => {
                            if let Some(s) = ats.first().copied() {
                                let node = self.con(&mut hir.types, TypeDef::Node(Some(RoleId::from_raw(role.0))));
                                self.deferred.push(Deferred::In {
                                    elem: node,
                                    coll: s,
                                    span,
                                });
                            }
                            self.con(&mut hir.types, TypeDef::Bool)
                        }
                        Builtin::Contains => {
                            if let (Some(c), Some(x)) = (ats.first(), ats.get(1)) {
                                self.deferred.push(Deferred::In {
                                    elem: *x,
                                    coll: *c,
                                    span,
                                });
                            }
                            self.con(&mut hir.types, TypeDef::Bool)
                        }
                    };
                    self.record(t)
                }
            }
            HExprKind::Lookup { rel, key } => {
                let rel = *rel;
                let mut keys = Vec::new();
                for k in key.iter_mut() {
                    keys.push(self.expr(hir, scope, k));
                }
                if self.apply {
                    self.next_term()
                } else {
                    let res = self.fresh(false);
                    self.deferred.push(Deferred::Lookup { rel, keys, res, span });
                    self.record(res)
                }
            }
            HExprKind::In { elem, coll } => {
                let a = self.expr(hir, scope, elem);
                let b = self.expr(hir, scope, coll);
                if self.apply {
                    self.next_term()
                } else {
                    self.deferred.push(Deferred::In { elem: a, coll: b, span });
                    let t = self.con(&mut hir.types, TypeDef::Bool);
                    self.record(t)
                }
            }
            HExprKind::Collection { kind, elems } => {
                let kind = *kind;
                let mut ts = Vec::new();
                for x in elems.iter_mut() {
                    ts.push(self.expr(hir, scope, x));
                }
                if self.apply {
                    self.next_term()
                } else {
                    let t = match kind {
                        CollectionKind::Vec | CollectionKind::Set => {
                            let el = self.fresh(false);
                            for x in &ts {
                                self.unify(&hir.types, *x, el, span);
                            }
                            self.bound(if kind == CollectionKind::Vec {
                                Shape::Vec(el)
                            } else {
                                Shape::Set(el)
                            })
                        }
                        CollectionKind::Map => {
                            let (k, v) = (self.fresh(false), self.fresh(false));
                            for (i, x) in ts.iter().enumerate() {
                                self.unify(&hir.types, *x, if i % 2 == 0 { k } else { v }, span);
                            }
                            self.bound(Shape::Map(k, v))
                        }
                    };
                    self.record(t)
                }
            }
            HExprKind::LatCtor { kind, bot, args } => {
                let (kind, bot) = (*kind, *bot);
                let mut ts = Vec::new();
                for x in args.iter_mut() {
                    ts.push(self.expr(hir, scope, x));
                }
                if self.apply {
                    // `LMap::of(k, v)`: the value may be lifted into the map's value lattice.
                    if let (LatCtorKind::Map, false) = (kind, bot) {
                        let (Some(v), Some(vt)) = (args.get_mut(1), ts.get(1)) else {
                            return self.next_term();
                        };
                        self.coerce_site(hir, v, *vt, 0);
                    }
                    self.next_term()
                } else {
                    let int_elem = kind == LatCtorKind::PSet;
                    let lat = match kind {
                        LatCtorKind::Bool => LatS::Bool,
                        LatCtorKind::Max => LatS::Max(self.fresh(false)),
                        LatCtorKind::Min => LatS::Min(self.fresh(false)),
                        LatCtorKind::Set => LatS::Set(self.fresh(false)),
                        LatCtorKind::PSet => LatS::PSet(self.fresh(false)),
                        LatCtorKind::Point => LatS::Point(self.fresh(false)),
                        LatCtorKind::Map => LatS::Map(self.fresh(false), self.fresh(false)),
                    };
                    if let Some(e) = lat.elem() {
                        if int_elem {
                            self.deferred.push(Deferred::IntColumn { t: e, span });
                        }
                        if !bot && let Some(x) = ts.first() {
                            self.unify(&hir.types, *x, e, span);
                        }
                    }
                    match (&lat, bot) {
                        (LatS::Bool, false) => {
                            if let Some(x) = ts.first() {
                                let b = self.con(&mut hir.types, TypeDef::Bool);
                                self.unify(&hir.types, *x, b, span);
                            }
                        }
                        (LatS::Map(k, v), false) => {
                            if let (Some(x), Some(y), Some(value)) = (ts.first(), ts.get(1), args.get_mut(1)) {
                                self.unify(&hir.types, *x, *k, span);
                                let (y, v) = (*y, *v);
                                self.coerce_site(hir, value, y, v);
                            }
                        }
                        _ => {}
                    }
                    let t = self.bound(Shape::Lat(lat));
                    self.record(t)
                }
            }
            HExprKind::Method {
                recv,
                name,
                banged,
                args,
            } => {
                let (name, banged) = (*name, *banged);
                let recv_var = match recv.kind {
                    HExprKind::Var(v) => Some((scope.0, v.0)),
                    _ => None,
                };
                let r = self.expr(hir, scope, recv);
                let mut ts = Vec::new();
                for x in args.iter_mut() {
                    ts.push(self.expr(hir, scope, x));
                }
                if self.apply {
                    let res = self.methods.get(self.method_cursor).cloned().flatten();
                    self.method_cursor += 1;
                    let t = self.next_term();
                    let Some(res) = res else {
                        return t;
                    };
                    // The call becomes the resolved operation on its receiver's lattice, with lifted arguments.
                    let Some(lattice) = self.solved(hir, r) else {
                        return t;
                    };
                    let mut out = vec![std::mem::replace(recv.as_mut(), HExpr::new(HExprKind::Tick, span))];
                    for (x, lift) in std::mem::take(args)
                        .into_iter()
                        .zip(res.lifts.iter().chain(std::iter::repeat(&None)))
                    {
                        out.push(match lift.and_then(|slot| self.lifts.get(slot).copied()) {
                            Some((true, to)) => self.lifted(hir, x, to),
                            _ => x,
                        });
                    }
                    e.kind = match res.target {
                        MethodTarget::Lattice(op) => HExprKind::LatOp { lattice, op, args: out },
                        MethodTarget::Plain(f) => HExprKind::Builtin { f, args: out },
                    };
                    match self.solved(hir, t) {
                        Some(ty) => e.ty = Some(ty),
                        None => self.error(span, "cannot infer the type of this expression".into()),
                    }
                    return t;
                } else {
                    let slot = self.methods.len();
                    self.methods.push(None);
                    let res = self.fresh(false);
                    self.deferred.push(Deferred::Method {
                        slot,
                        recv: r,
                        name,
                        banged,
                        recv_var,
                        args: ts,
                        res,
                        span,
                    });
                    self.record(res)
                }
            }
            HExprKind::LatOp { .. } | HExprKind::Lift { .. } => {
                self.bugs
                    .push(internal_error!("a resolved lattice operation reached type checking"));
                0
            }
        };
        if self.apply {
            match self.solved(hir, t) {
                Some(ty) => e.ty = Some(ty),
                None => self.error(span, "cannot infer the type of this expression".into()),
            }
        }
        t
    }

    /// A coercion site: `e`, of term `from`, where a value of term `to` is expected (a head column, a `let`
    /// variable, a constructor argument). The solver decides whether `e` is lifted into a lattice (LANGUAGE §5.6); the
    /// apply walk then wraps it. Both walks call this once per site, in the same order.
    fn coerce_site(&mut self, hir: &mut Hir, e: &mut HExpr, from: T, to: T) {
        if !self.apply {
            let slot = self.lifts.len();
            self.lifts.push((false, to));
            self.deferred.push(Deferred::Coerce {
                slot,
                from,
                to,
                span: e.span,
            });
            return;
        }
        let site = self.lifts.get(self.lift_cursor).copied();
        self.lift_cursor += 1;
        if let Some((true, to)) = site {
            let inner = std::mem::replace(e, HExpr::new(HExprKind::Tick, e.span));
            *e = self.lifted(hir, inner, to);
        }
    }

    /// `e` lifted into the lattice of term `to`.
    fn lifted(&mut self, hir: &mut Hir, e: HExpr, to: T) -> HExpr {
        let span = e.span;
        let ty = self.solved(hir, to);
        if ty.is_none() {
            self.error(span, "cannot infer the lattice this value is lifted into".into());
        }
        if let (Some(from), Some(to)) = (e.ty, ty)
            && !lifts_from(hir, from, to, true)
        {
            self.diags.push(
                Diagnostic::not_implemented(
                    blossom_base::FeatureId("LANG-122"),
                    "lifting a map that mixes plain and lattice values",
                    "the Blossom frontend (slice 2)",
                )
                .with_primary(span),
            );
        }
        HExpr {
            ty,
            span,
            kind: HExprKind::Lift {
                expr: Box::new(e),
                lattice: ty.unwrap_or(TypeId::from_raw(0)),
            },
        }
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
            // No lattice was expected where a coercion or comparison is still undecided: it is plain.
            if let Some(i) = self
                .deferred
                .iter()
                .position(|d| matches!(d, Deferred::Coerce { .. } | Deferred::Compare { .. }))
            {
                let d = self.deferred.remove(i);
                self.settle_plain(hir, &d);
                continue;
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
                | Deferred::IntColumn { span, .. }
                | Deferred::Concat { span, .. }
                | Deferred::Coerce { span, .. }
                | Deferred::Compare { span, .. }
                | Deferred::Lookup { span, .. }
                | Deferred::In { span, .. }
                | Deferred::Gen { span, .. }
                | Deferred::Method { span, .. } => span,
            };
            self.error(span, "cannot infer the operand types of this expression".into());
        }
    }

    /// Tries a deferred constraint; `true` when it was discharged (successfully or with an error).
    fn try_deferred(&mut self, hir: &mut Hir, d: &Deferred) -> bool {
        match *d {
            Deferred::Arith { op, l, r, res, span } => {
                if let Some(done) = self.lattice_arith(hir, op, l, r, res, span) {
                    return done;
                }
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
                            let ft = self.of_type(hir, f.ty);
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
            Deferred::Concat { t, span } => {
                let r = self.find(t);
                match self.node(r) {
                    Node::Bound(Shape::Vec(_)) => {}
                    Node::Bound(Shape::Con(ty)) if matches!(hir.types.get(ty), Some(TypeDef::Str | TypeDef::Bytes)) => {
                    }
                    Node::Bound(_) => {
                        let d = self.describe(&hir.types, t);
                        self.error(span, format!("`++` joins strings, bytes or vectors, not {d}"));
                    }
                    _ => return false,
                }
                true
            }
            Deferred::Coerce { slot, from, to, span } => {
                let to_lat = self.lat(to);
                if to_lat.is_none() {
                    if self.is_unbound(to) {
                        return false;
                    }
                    self.unify(&hir.types, from, to, span);
                    return true;
                }
                if self.lat(from).is_some() {
                    self.unify(&hir.types, from, to, span);
                    return true;
                }
                let rf = self.find(from);
                let from_int = matches!(self.node(rf), Node::Unbound { int: true });
                if self.is_unbound(from) && !from_int {
                    return false;
                }
                if let Some(site) = self.lifts.get_mut(slot) {
                    site.0 = true;
                }
                self.lift_into(hir, from, to, span);
                true
            }
            Deferred::Compare { op, l, r, span } => {
                let (ll, rl) = (self.lat(l), self.lat(r));
                if ll.is_none() && rl.is_none() {
                    if self.is_unbound(l) || self.is_unbound(r) {
                        return false;
                    }
                    self.settle_plain(hir, d);
                    return true;
                }
                // A threshold: the lattice on the left after flipping `c <= x` into `x >= c`.
                let (lat, scalar, op) = match (ll, rl) {
                    (Some(x), None) => (x, r, op),
                    (None, Some(x)) => (x, l, flip(op)),
                    _ => {
                        self.diags.push(
                            Diagnostic::new(
                                code!("BLS0306"),
                                "two lattice values are compared with methods (`a.leq!(b)`), not operators",
                            )
                            .with_primary(span),
                        );
                        return true;
                    }
                };
                let ok = matches!(
                    (&lat, op),
                    (LatS::Max(_), BinOp::Ge | BinOp::Gt) | (LatS::Min(_), BinOp::Le | BinOp::Lt)
                );
                if !ok {
                    self.diags.push(
                        Diagnostic::new(
                            code!("BLS0306"),
                            format!(
                                "`{}` compares an `{}` against a scalar only in the threshold direction \
                                 (`>=`/`>` on LMax, `<=`/`<` on LMin); write `reveal!(x) {} c` (an exact read) or \
                                 `not (…)`",
                                bin_text(op),
                                lat.name(),
                                bin_text(op)
                            ),
                        )
                        .with_primary(span),
                    );
                    return true;
                }
                if let Some(e) = lat.elem() {
                    self.unify(&hir.types, scalar, e, span);
                }
                true
            }
            Deferred::Lookup {
                rel,
                ref keys,
                res,
                span,
            } => {
                let n = self.rel_of(hir, rel).cols.len();
                let cols: Vec<T> = (0..n).map(|c| self.col_term(hir, rel.index(), c)).collect();
                if cols.iter().any(|c| self.is_unbound(*c)) {
                    return false;
                }
                let r = self.rel_of(hir, rel);
                let lattice: Vec<usize> = (0..n)
                    .filter(|c| cols.get(*c).is_some_and(|t| self.lat(*t).is_some()))
                    .collect();
                let key: Vec<usize> = match &r.key {
                    Some(k) => k.clone(),
                    None => (0..n).filter(|c| !lattice.contains(c)).collect(),
                };
                let [value] = lattice.as_slice() else {
                    if lattice.is_empty() {
                        self.diags.push(
                            Diagnostic::not_implemented(
                                blossom_base::FeatureId("LANG-091"),
                                &format!("lookups `r[k]` on the set relation `{}`", r.name),
                                "the Blossom frontend (slice 2)",
                            )
                            .with_primary(span),
                        );
                    } else {
                        self.diags.push(
                            Diagnostic::not_implemented(
                                blossom_base::FeatureId("LANG-280"),
                                &format!("lookups on `{}`, which has several lattice columns", r.name),
                                "the Blossom frontend (slice 2)",
                            )
                            .with_primary(span),
                        );
                    }
                    return true;
                };
                if keys.len() != key.len() {
                    self.diags.push(
                        Diagnostic::new(
                            code!("BLS0301"),
                            format!("`{}[…]` takes {} key value(s), {} given", r.name, key.len(), keys.len()),
                        )
                        .with_primary(span),
                    );
                    return true;
                }
                for (k, c) in keys.iter().zip(&key) {
                    let ct = self.col_term(hir, rel.index(), *c);
                    self.unify(&hir.types, *k, ct, span);
                }
                let vt = self.col_term(hir, rel.index(), *value);
                self.unify(&hir.types, res, vt, span);
                true
            }
            Deferred::In { elem, coll, span } => {
                let rc = self.find(coll);
                match self.node(rc) {
                    Node::Bound(Shape::Lat(LatS::Set(e) | LatS::PSet(e)) | Shape::Set(e) | Shape::Vec(e)) => {
                        self.unify(&hir.types, elem, e, span);
                    }
                    Node::Bound(_) => {
                        let d = self.describe(&hir.types, coll);
                        self.error(
                            span,
                            format!("`x in e` needs a set-like lattice, a set or a vector, found {d}"),
                        );
                    }
                    _ => return false,
                }
                true
            }
            Deferred::Gen { pat, src, span } => {
                let rc = self.find(src);
                match self.node(rc) {
                    Node::Bound(Shape::Lat(LatS::Set(e) | LatS::PSet(e)) | Shape::Set(e) | Shape::Vec(e)) => {
                        self.unify(&hir.types, pat, e, span);
                    }
                    Node::Bound(Shape::Lat(LatS::Map(k, v)) | Shape::Map(k, v)) => {
                        let pair = self.bound(Shape::Tuple(vec![k, v]));
                        self.unify(&hir.types, pat, pair, span);
                    }
                    Node::Bound(_) => {
                        let d = self.describe(&hir.types, src);
                        self.error(
                            span,
                            format!(
                                "a generator ranges over a set-like lattice, a map lattice or a collection, found {d}"
                            ),
                        );
                    }
                    _ => return false,
                }
                true
            }
            Deferred::Method {
                slot,
                recv,
                name,
                banged,
                recv_var,
                ref args,
                res,
                span,
            } => {
                let nonbot = recv_var.is_some_and(|v| self.non_bottom(hir, v));
                self.method(hir, slot, recv, name, banged, nonbot, args, res, span)
            }
        }
    }

    /// Records the variables of a pattern that binds them other than as a positive atom's column.
    fn other_bindings(&mut self, scope: ScopeId, p: &HPat) {
        match p {
            HPat::Var(v, _) => self.bindings.entry((scope.0, v.0)).or_default().push(None),
            HPat::Tuple(ps, _) | HPat::Variant { fields: ps, .. } => {
                for x in ps {
                    self.other_bindings(scope, x);
                }
            }
            HPat::Wild(_) | HPat::Expr(_) => {}
        }
    }

    /// Whether a variable is known non-⊥ (SEM-101 N4): every binding of it is the one lattice column of a positive
    /// atom (a row with several lattice columns may hold ⊥ in all but one).
    fn non_bottom(&mut self, hir: &Hir, var: (u32, u32)) -> bool {
        let Some(occs) = self.bindings.get(&var).cloned() else {
            return false;
        };
        !occs.is_empty()
            && occs.iter().all(|o| match o {
                Some((rel, col)) => {
                    let n = hir.rels.get(rel.index()).map_or(0, |r| r.cols.len());
                    let lattice: Vec<usize> = (0..n)
                        .filter(|c| {
                            let t = self.col_term(hir, rel.index(), *c);
                            self.lat(t).is_some()
                        })
                        .collect();
                    lattice == vec![*col]
                }
                None => false,
            })
    }

    /// The lattice shape of a solved term.
    fn lat(&mut self, t: T) -> Option<LatS> {
        let r = self.find(t);
        match self.node(r) {
            Node::Bound(Shape::Lat(l)) => Some(l),
            _ => None,
        }
    }

    /// The built-in lattice of a lattice term, once its nested value lattices are known.
    fn lat_kind(&mut self, t: T) -> Option<Kind> {
        Some(match self.lat(t)? {
            LatS::Bool => Kind::Bool,
            LatS::Max(_) => Kind::Max,
            LatS::Min(_) => Kind::Min,
            LatS::Set(_) => Kind::Set,
            LatS::PSet(_) => Kind::PSet,
            LatS::Point(_) => Kind::Point,
            LatS::Map(_, v) => Kind::Map(Box::new(self.lat_kind(v)?)),
        })
    }

    /// Settles a coercion or comparison that no lattice reached: plain unification.
    fn settle_plain(&mut self, hir: &mut Hir, d: &Deferred) {
        match *d {
            Deferred::Coerce { from, to, span, .. } => self.unify(&hir.types, from, to, span),
            Deferred::Compare { l, r, span, .. } => {
                self.unify(&hir.types, l, r, span);
                self.deferred.push(Deferred::Ordered { t: l, span });
            }
            _ => {}
        }
    }

    /// Constrains a plain value `from` to lift into the lattice `to` (LANGUAGE §5.6): `T` into `LMax<T>`, `LMin<T>`,
    /// `LPoint<T>`; `bool` into `LBool`; `Set<T>` into `LSet<T>`/`LPSet<T>`; `Map<K, V>` into `LMap<K, L>` when `V`
    /// lifts into `L`.
    fn lift_into(&mut self, hir: &mut Hir, from: T, to: T, span: Span) {
        let Some(lat) = self.lat(to) else {
            self.unify(&hir.types, from, to, span);
            return;
        };
        match lat {
            LatS::Max(e) | LatS::Min(e) | LatS::Point(e) => self.unify(&hir.types, from, e, span),
            LatS::Bool => {
                let b = self.con(&mut hir.types, TypeDef::Bool);
                self.unify(&hir.types, from, b, span);
            }
            LatS::Set(e) | LatS::PSet(e) => {
                let s = self.bound(Shape::Set(e));
                self.unify(&hir.types, from, s, span);
            }
            LatS::Map(k, v) => {
                let (fk, fv) = (self.fresh(false), self.fresh(false));
                let m = self.bound(Shape::Map(fk, fv));
                self.unify(&hir.types, from, m, span);
                self.unify(&hir.types, fk, k, span);
                // The values lift (or already are the value lattice): no site, the lift is deep.
                let slot = self.lifts.len();
                self.lifts.push((false, v));
                self.deferred.push(Deferred::Coerce {
                    slot,
                    from: fv,
                    to: v,
                    span,
                });
            }
        }
    }

    /// Arithmetic with a lattice operand (LANGUAGE §11.5): `x + c`, `c + x` and `x - c` on `LMax`/`LMin` with a
    /// scalar are morphisms, `a + b` on two of them a bimorphism. `None` when neither operand is a lattice.
    fn lattice_arith(&mut self, hir: &mut Hir, op: BinOp, l: T, r: T, res: T, span: Span) -> Option<bool> {
        let (ll, rl) = (self.lat(l), self.lat(r));
        let (lat, lat_t, scalar) = match (&ll, &rl) {
            (None, None) => return None,
            (Some(a), Some(_)) => {
                if matches!(op, BinOp::Add) && matches!(a, LatS::Max(_) | LatS::Min(_)) {
                    self.unify(&hir.types, l, r, span);
                    self.unify(&hir.types, l, res, span);
                } else {
                    self.error(
                        span,
                        format!("`{}` on two lattice values: only `+` on LMax/LMin", bin_text(op)),
                    );
                }
                return Some(true);
            }
            (Some(a), None) => (a.clone(), l, r),
            (None, Some(b)) => (b.clone(), r, l),
        };
        let scalar_first = ll.is_none();
        let ok = match (&lat, op) {
            (LatS::Max(_) | LatS::Min(_), BinOp::Add) => true,
            (LatS::Max(_), BinOp::Sub) => !scalar_first,
            _ => false,
        };
        if !ok {
            self.error(
                span,
                format!(
                    "`{}` with an `{}` is not a monotone operation: use `reveal!` for exact arithmetic",
                    bin_text(op),
                    lat.name()
                ),
            );
            return Some(true);
        }
        if let Some(e) = lat.elem() {
            self.unify(&hir.types, scalar, e, span);
            self.deferred.push(Deferred::IntColumn { t: e, span });
        }
        self.unify(&hir.types, lat_t, res, span);
        Some(true)
    }

    /// Resolves `recv.name(args)` once the receiver's lattice is known; `false` while it is not.
    #[allow(clippy::too_many_arguments)]
    fn method(
        &mut self,
        hir: &mut Hir,
        slot: usize,
        recv: T,
        name: Symbol,
        banged: bool,
        nonbot: bool,
        args: &[T],
        res: T,
        span: Span,
    ) -> bool {
        let Some(lat) = self.lat(recv) else {
            if self.is_unbound(recv) {
                return false;
            }
            // Methods of plain values are the standard library's (Appendix B).
            let rr = self.find(recv);
            let elem = match self.node(rr) {
                Node::Bound(Shape::Vec(e) | Shape::Set(e) | Shape::Map(e, _)) => Some(e),
                _ => None,
            };
            if let (Some(e), "contains", false, [x]) = (elem, name.as_str(), banged, args) {
                self.unify(&hir.types, *x, e, span);
                let b = self.con(&mut hir.types, TypeDef::Bool);
                self.unify(&hir.types, res, b, span);
                if let Some(m) = self.methods.get_mut(slot) {
                    *m = Some(MethodRes {
                        target: MethodTarget::Plain(Builtin::Contains),
                        lifts: vec![None],
                    });
                }
                return true;
            }
            let d = self.describe(&hir.types, recv);
            self.diags.push(
                Diagnostic::not_implemented(
                    blossom_base::FeatureId("LANG-180"),
                    &format!("the method `{}` on {d}", name.as_str()),
                    "the Blossom frontend (slice 2)",
                )
                .with_primary(span),
            );
            return true;
        };
        let Some(kind) = self.lat_kind(recv) else {
            return false;
        };
        let op = if name.as_str() == "reveal" {
            if nonbot && matches!(kind, Kind::Max | Kind::Min | Kind::Point) {
                Op::RevealNonBot
            } else {
                Op::Reveal
            }
        } else {
            match Op::method(&kind, name.as_str()) {
                Some(op) => op,
                None => {
                    self.error(span, format!("`{}` has no method `{}`", lat.name(), name.as_str()));
                    return true;
                }
            }
        };
        if args.len() + 1 != op.arity(&kind) {
            self.diags.push(
                Diagnostic::new(
                    code!("BLS0301"),
                    format!(
                        "`{}` takes {} argument(s), {} given",
                        name.as_str(),
                        op.arity(&kind) - 1,
                        args.len()
                    ),
                )
                .with_primary(span),
            );
            return true;
        }
        let sig = op.sig(&kind);
        if sig.needs_bang() && !banged {
            self.diags.push(
                Diagnostic::new(
                    code!("BLS0700"),
                    format!(
                        "`{}` is not monotone: write `{}!` to make the exact read visible",
                        name.as_str(),
                        name.as_str()
                    ),
                )
                .with_primary(span),
            );
        } else if banged && !sig.needs_bang() {
            self.diags.push(
                Diagnostic::new(
                    code!("BLS0701"),
                    format!("`{}` is monotone: the bang is superfluous", name.as_str()),
                )
                .with_primary(span),
            );
        }
        let mut lifts = vec![None; args.len()];
        let first = args.first().copied();
        let bool_t = self.con(&mut hir.types, TypeDef::Bool);
        let result = match (op, &lat) {
            (Op::Join, _) | (Op::And, _) | (Op::Or, _) => {
                if let Some(a) = first {
                    let s = self.lifts.len();
                    self.lifts.push((false, recv));
                    self.deferred.push(Deferred::Coerce {
                        slot: s,
                        from: a,
                        to: recv,
                        span,
                    });
                    if let Some(l) = lifts.first_mut() {
                        *l = Some(s);
                    }
                }
                recv
            }
            (Op::Leq | Op::Less | Op::Intersect, _) => {
                if let Some(a) = first {
                    self.unify(&hir.types, a, recv, span);
                }
                if op == Op::Intersect { recv } else { bool_t }
            }
            (Op::Reveal | Op::RevealNonBot, _) => self.reveal_term(hir, &lat, op == Op::RevealNonBot),
            (Op::IsBot | Op::Nonempty | Op::IsEmpty, _) => bool_t,
            (Op::Not, _) => recv,
            (Op::Contains, l) => {
                if let (Some(a), Some(e)) = (first, l.elem()) {
                    self.unify(&hir.types, a, e, span);
                }
                bool_t
            }
            (Op::Size, _) => {
                let u = self.con(&mut hir.types, TypeDef::Int(IntTy::U64));
                self.bound(Shape::Lat(LatS::Max(u)))
            }
            (Op::At | Op::HasKey, LatS::Map(k, v)) => {
                if let Some(a) = first {
                    self.unify(&hir.types, a, *k, span);
                }
                if op == Op::At { *v } else { bool_t }
            }
            (Op::KeySet, LatS::Map(k, _)) => self.bound(Shape::Lat(LatS::Set(*k))),
            (Op::Sum, l) => match l.elem() {
                Some(e) => {
                    self.deferred.push(Deferred::IntColumn { t: e, span });
                    self.bound(Shape::Lat(LatS::Max(e)))
                }
                None => res,
            },
            (Op::Get, l) => match l.elem() {
                Some(e) => self.bound(Shape::Option(e)),
                None => res,
            },
            (Op::MinOf, l) => {
                if let (Some(a), Some(e)) = (first, l.elem()) {
                    self.unify(&hir.types, a, e, span);
                }
                recv
            }
            (Op::MinElem, l) => match l.elem() {
                Some(e) => self.bound(Shape::Lat(LatS::Min(e))),
                None => res,
            },
            (Op::MaxElem, l) => match l.elem() {
                Some(e) => self.bound(Shape::Lat(LatS::Max(e))),
                None => res,
            },
            (other, _) => {
                self.bugs
                    .push(internal_error!("method {other:?} resolved on {}", lat.name()));
                return true;
            }
        };
        self.unify(&hir.types, res, result, span);
        if let Some(m) = self.methods.get_mut(slot) {
            *m = Some(MethodRes {
                target: MethodTarget::Lattice(op),
                lifts,
            });
        }
        true
    }

    /// The type of `reveal!` of a lattice value (LANGUAGE §11.4): deep, `Option<T>` for a possibly-⊥ chain or point.
    fn reveal_term(&mut self, hir: &mut Hir, lat: &LatS, nonbot: bool) -> T {
        match lat {
            LatS::Bool => self.con(&mut hir.types, TypeDef::Bool),
            LatS::Max(e) | LatS::Min(e) | LatS::Point(e) => {
                if nonbot {
                    *e
                } else {
                    self.bound(Shape::Option(*e))
                }
            }
            LatS::Set(e) | LatS::PSet(e) => self.bound(Shape::Set(*e)),
            LatS::Map(k, v) => {
                let inner = match self.lat(*v) {
                    Some(l) => self.reveal_term(hir, &l, true),
                    None => self.fresh(false),
                };
                self.bound(Shape::Map(*k, inner))
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
        let scopes: Vec<Vec<(Span, Symbol)>> = hir
            .scopes
            .iter()
            .map(|sc| sc.vars.iter().map(|v| (v.span, v.name)).collect())
            .collect();
        for (vars, terms) in scopes.into_iter().zip(self.var_terms.clone()) {
            let mut tys = Vec::new();
            for ((span, name), t) in vars.into_iter().zip(terms) {
                match self.solved(hir, t) {
                    Some(ty) => tys.push(ty),
                    None => {
                        self.error(span, format!("cannot infer the type of `{}`", name.as_str()));
                        tys.push(intern(&mut hir.types, TypeDef::Unit));
                    }
                }
            }
            var_types.push(tys);
        }
        hir.var_types = var_types;
        let view_terms = self.view_terms.clone();
        for (r, cols) in view_terms.into_iter().enumerate() {
            for (c, t) in cols.into_iter().enumerate() {
                let Some(col) = hir.rels.get(r).and_then(|rel| rel.cols.get(c)) else {
                    continue;
                };
                if col.ty.is_some() {
                    continue;
                }
                let solved = self.solved(hir, t);
                let Some(rel) = hir.rels.get_mut(r) else { continue };
                let span = rel.span;
                let (rel_name, col_name) = (rel.name.clone(), rel.cols.get(c).map(|c| c.name));
                match (solved, rel.cols.get_mut(c)) {
                    (Some(ty), Some(col)) => col.ty = Some(ty),
                    _ => self.diags.push(
                        Diagnostic::new(
                            code!("BLS0310"),
                            format!(
                                "cannot infer the type of column `{}` of `{}`",
                                col_name.map(|n| n.as_str()).unwrap_or("?"),
                                rel_name
                            ),
                        )
                        .with_primary(span),
                    ),
                }
            }
        }
    }
}

fn intern(types: &mut TypeTable, def: TypeDef) -> TypeId {
    match types.insert(def) {
        Ok(t) => t,
        Err(_) => types.insert(TypeDef::Unit).unwrap_or(TypeId::from_raw(0)),
    }
}

/// Whether a value of type `from` lifts into the lattice `to` (LANGUAGE §5.6): its plain form, or at the top a map
/// whose values already are the value lattice.
fn lifts_from(hir: &Hir, from: TypeId, to: TypeId, top: bool) -> bool {
    let Some((_, ctor)) = hir.lattice_of(to) else {
        return from == to;
    };
    let lattice_type = |id: blossom_base::LatticeTypeId| hir.types.lookup(&TypeDef::Lattice(id));
    let fits = |a: TypeId, b: TypeId| assignable(&hir.types, a, b);
    match (ctor, hir.types.get(from)) {
        (LatticeCtor::Bool, Some(TypeDef::Bool)) => true,
        (LatticeCtor::Max(e) | LatticeCtor::Min(e) | LatticeCtor::Point(e), _) => fits(from, *e),
        (LatticeCtor::Set(e) | LatticeCtor::PSet(e), Some(TypeDef::Set(x))) => fits(*x, *e),
        (LatticeCtor::Map(k, inner), Some(TypeDef::Map(k2, v2))) => {
            fits(*k2, *k)
                && match lattice_type(*inner) {
                    Some(it) => (top && *v2 == it) || lifts_from(hir, *v2, it, false),
                    None => false,
                }
        }
        _ => false,
    }
}

/// `Node<R>` stands where `Node` is expected, also inside tuples, options and collections (the IR's rule).
fn assignable(types: &TypeTable, actual: TypeId, expected: TypeId) -> bool {
    if actual == expected {
        return true;
    }
    match (types.get(actual), types.get(expected)) {
        (Some(TypeDef::Node(Some(_))), Some(TypeDef::Node(None))) => true,
        (Some(TypeDef::Tuple(a)), Some(TypeDef::Tuple(b))) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| assignable(types, *x, *y))
        }
        (Some(TypeDef::Option(a)), Some(TypeDef::Option(b)))
        | (Some(TypeDef::Vec(a)), Some(TypeDef::Vec(b)))
        | (Some(TypeDef::Set(a)), Some(TypeDef::Set(b))) => assignable(types, *a, *b),
        (Some(TypeDef::Map(ka, va)), Some(TypeDef::Map(kb, vb))) => {
            assignable(types, *ka, *kb) && assignable(types, *va, *vb)
        }
        _ => false,
    }
}

/// `a op b` ⇔ `b flip(op) a`.
fn flip(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Gt,
        BinOp::Le => BinOp::Ge,
        BinOp::Gt => BinOp::Lt,
        BinOp::Ge => BinOp::Le,
        other => other,
    }
}

fn bin_text(op: BinOp) -> &'static str {
    match op {
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        _ => "operator",
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

/// BLS0304 (LANGUAGE §11.1): a lattice column is never a join key and is never matched against a value. An atom's
/// lattice column binds a fresh variable (or is `_`), and that variable appears in no other atom of the rule.
fn lattice_keys(hir: &Hir, diags: &mut Diagnostics) {
    fn stmt_conds<'h>(stmts: &'h [HStmt], out: &mut Vec<&'h HBody>) {
        for s in stmts {
            if let HStmt::Block { cond, stmts, .. } = s {
                out.push(cond);
                stmt_conds(stmts, out);
            }
        }
    }
    for h in &hir.handlers {
        let mut bodies = vec![&h.header];
        stmt_conds(&h.stmts, &mut bodies);
        check_lattice_keys(hir, &bodies, diags);
    }
    for v in &hir.views {
        for (_, b) in &v.alternatives {
            check_lattice_keys(hir, &[b], diags);
        }
    }
    for inv in &hir.invariants {
        check_lattice_keys(hir, &[&inv.body], diags);
    }
}

fn check_lattice_keys(hir: &Hir, bodies: &[&HBody], diags: &mut Diagnostics) {
    fn pat_vars(p: &HPat, out: &mut Vec<HVarId>) {
        match p {
            HPat::Var(v, _) => out.push(*v),
            HPat::Tuple(ps, _) | HPat::Variant { fields: ps, .. } => ps.iter().for_each(|x| pat_vars(x, out)),
            HPat::Wild(_) | HPat::Expr(_) => {}
        }
    }
    fn lit_atoms<'h>(l: &'h HLit, out: &mut Vec<&'h HAtom>) {
        match l {
            HLit::Atom(a) | HLit::Not(a) | HLit::Outer(a) | HLit::Per(a) | HLit::Delta { atom: a, .. } => out.push(a),
            HLit::NotBody(b, _) => atoms(b, out),
            HLit::Any(alts, _) => alts.iter().for_each(|b| atoms(b, out)),
            HLit::Forall { domain, body, .. } => {
                lit_atoms(domain, out);
                atoms(body, out);
            }
            _ => {}
        }
    }
    fn atoms<'h>(body: &'h HBody, out: &mut Vec<&'h HAtom>) {
        for l in &body.lits {
            lit_atoms(l, out);
        }
    }
    let mut all = Vec::new();
    for b in bodies {
        atoms(b, &mut all);
    }
    let is_lattice = |rel: HRelId, c: usize| {
        hir.rels
            .get(rel.index())
            .and_then(|r| r.cols.get(c))
            .and_then(|c| c.ty)
            .is_some_and(|t| hir.lattice_of(t).is_some())
    };
    let mut count: std::collections::BTreeMap<HVarId, usize> = std::collections::BTreeMap::new();
    for a in &all {
        let mut vs = Vec::new();
        for p in &a.args {
            pat_vars(p, &mut vs);
        }
        for v in vs {
            *count.entry(v).or_default() += 1;
        }
    }
    // A variable a `let` or a generator binds is a value: at a lattice column it would be a join too.
    fn lit_binders(l: &HLit, out: &mut Vec<HVarId>) {
        match l {
            HLit::Let { pat, .. } | HLit::Gen { pat, .. } | HLit::RangeGen { pat, .. } | HLit::RoleGen { pat, .. } => {
                pat_vars(pat, out)
            }
            HLit::NotBody(b, _) => b.lits.iter().for_each(|x| lit_binders(x, out)),
            HLit::Any(alts, _) => alts
                .iter()
                .for_each(|b| b.lits.iter().for_each(|x| lit_binders(x, out))),
            HLit::Forall { domain, body, .. } => {
                lit_binders(domain, out);
                body.lits.iter().for_each(|x| lit_binders(x, out));
            }
            _ => {}
        }
    }
    let mut bound_otherwise = Vec::new();
    for b in bodies {
        for l in &b.lits {
            lit_binders(l, &mut bound_otherwise);
        }
    }
    for v in bound_otherwise {
        *count.entry(v).or_default() += 1;
    }
    let mut reported = BTreeSet::new();
    for a in &all {
        for (c, p) in a.args.iter().enumerate() {
            if !is_lattice(a.rel, c) {
                continue;
            }
            let bad = match p {
                HPat::Wild(_) => None,
                HPat::Var(v, span) => {
                    (count.get(v).copied().unwrap_or(0) > 1 && reported.insert(*v)).then_some((*span, "joins on it"))
                }
                other => Some((other.span(), "matches it against a value")),
            };
            if let Some((span, what)) = bad {
                diags.push(
                    Diagnostic::new(
                        code!("BLS0304"),
                        format!(
                            "this atom {what}, but a lattice column is never a key or a join key: bind a fresh \
                             variable and compare with a threshold or `reveal!`"
                        ),
                    )
                    .with_primary(span),
                );
            }
        }
    }
}
