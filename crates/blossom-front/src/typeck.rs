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
use blossom_ir::core::{LatticeCtor, LibFn};
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
        block_terms: Vec::new(),
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
        closures: BTreeMap::new(),
        fn_sigs: Vec::new(),
        fn_terms: BTreeMap::new(),
        role_edges: Vec::new(),
        conjunct_eq: false,
        placed: None,
        in_conjunction: true,
        settle_duration: false,
        roles: BTreeMap::new(),
        free: BTreeSet::new(),
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
    /// Values of `from` flow into `to` (LANGUAGE §5.3): one shape, but a `Node` position of `to` is only as precise
    /// as what flows into it. With `check`, `to` is a requirement (a column written, a parameter, a result) and a
    /// value less precise than it is an error. With `relate`, the two are only compared (`==`): one shape, no flow.
    Flow {
        from: T,
        to: T,
        check: bool,
        relate: bool,
        span: Span,
    },
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
    Coerce {
        slot: usize,
        from: T,
        to: T,
        /// `to` is a requirement (a column written, a result), not a variable the value defines.
        check: bool,
        span: Span,
    },
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
    /// `majority(coll, R)`: `coll` is a set-like lattice of `elem` (LANGUAGE §11.6).
    Quorum { elem: T, coll: T, span: Span },
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
    /// Per handler block, in walk order: the terms of the variables bound outside it, inside it.
    block_terms: Vec<Vec<(HVarId, T)>>,
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
    /// Each closure's term (never bound: a closure is not a value) with its parameters' and body's terms. A
    /// combinator's resolution types the closure through this (LANGUAGE §16.1).
    closures: BTreeMap<T, (Vec<T>, T)>,
    /// Each function's parameter and result types, by `HFnId`.
    fn_sigs: Vec<(Vec<TypeId>, TypeId)>,
    /// Each generic function instance's terms, by `HFnId`: its parameters', its result's, and its type parameters'.
    /// Its one call and its body share them, so the call infers the type parameters (LANGUAGE §16.1).
    fn_terms: BTreeMap<usize, InstanceTerms>,
    /// Flows between `Node` leaves: `(from, to, check, span)`. Roles are solved over them once shapes are known.
    role_edges: Vec<(T, T, bool, Span)>,
    /// The role the rule being walked is placed at, if any.
    placed: Option<HRoleId>,
    /// Set just before typing a guard that is a conjunct of a rule body: its `==` is an equation.
    conjunct_eq: bool,
    /// Whether the body being walked is a conjunction the rule's valuations satisfy (not under `not`, `any` or
    /// `forall`).
    in_conjunction: bool,
    /// Set while settling a `Duration` sum whose other operand nothing else decided: it is then a `Duration`.
    settle_duration: bool,
    /// The solved role of each `Node` leaf's root, once roles are solved.
    roles: BTreeMap<T, Role>,
    /// `Node` leaves a flow created, whose role is only what flows in (nothing, for a `None`'s): the roots of classes
    /// that no declared type joined.
    free: BTreeSet<T>,
}

#[derive(Clone, Debug)]
struct InstanceTerms {
    params: Vec<T>,
    ret: T,
    tparams: Vec<T>,
}

/// A `Node` position's role, as solved: no value reaches it (`Bot`), members of one role, or any node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Bot,
    Of(RoleId),
    Any,
}

impl Role {
    fn of(def: Option<&TypeDef>) -> Option<Role> {
        match def {
            Some(TypeDef::Node(Some(r))) => Some(Role::Of(*r)),
            Some(TypeDef::Node(None)) => Some(Role::Any),
            _ => None,
        }
    }

    /// The least role both fit in: what a merge of the two holds.
    fn lub(self, o: Role) -> Role {
        match (self, o) {
            (Role::Bot, x) | (x, Role::Bot) => x,
            (Role::Of(a), Role::Of(b)) if a == b => Role::Of(a),
            _ => Role::Any,
        }
    }

    /// The greatest role within both: what a value that is both holds.
    fn glb(self, o: Role) -> Role {
        match (self, o) {
            (Role::Any, x) | (x, Role::Any) => x,
            (Role::Of(a), Role::Of(b)) if a == b => Role::Of(a),
            _ => Role::Bot,
        }
    }

    fn within(self, o: Role) -> bool {
        self.glb(o) == self
    }
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
                        // The class is free only if both were: a declared type makes it a source.
                        if !(self.free.contains(&ra2) && self.free.contains(&rb2)) {
                            self.free.remove(&ra2);
                        }
                    }
                }
                ok
            }
            _ => false,
        }
    }

    /// Values of `from` flow into `to` (a merge, or a requirement with `check`).
    fn flow(&mut self, from: T, to: T, check: bool, span: Span) {
        self.deferred.push(Deferred::Flow {
            from,
            to,
            check,
            relate: false,
            span,
        });
    }

    /// `x` is a value of `of` (a copy of it, so `of` is not narrowed by `x`'s other equations): `x ∈ of`, or `x = of`.
    fn member(&mut self, of: T, x: T, span: Span) {
        let copy = self.fresh(false);
        self.flow(of, copy, false, span);
        // `copy` is fresh and unconstrained, so it can always join `x`'s class.
        let rx = self.find(x);
        self.set(copy, Node::Link(rx));
    }

    /// A plain value where a requirement (`check`) or a variable it defines is expected.
    fn coerce_plain(&mut self, from: T, to: T, check: bool, span: Span) {
        if check {
            self.flow(from, to, true, span);
        } else {
            self.member(from, to, span);
        }
    }

    /// `a` and `b` are compared: they must have one shape, and neither makes the other more precise.
    fn relate(&mut self, a: T, b: T, span: Span) {
        self.deferred.push(Deferred::Flow {
            from: a,
            to: b,
            check: false,
            relate: true,
            span,
        });
    }

    /// One step of a flow: `true` once it is decided (shapes matched, errors reported), `false` while a side is not
    /// known well enough. A side that is unknown takes the other's shape with fresh positions, so the two never share
    /// a `Node` position; `force` settles a flow whose source is still unknown by linking the two, as an equation.
    #[allow(clippy::too_many_arguments)]
    fn flow_step(
        &mut self,
        types: &TypeTable,
        from: T,
        to: T,
        check: bool,
        relate: bool,
        span: Span,
        force: bool,
    ) -> bool {
        let (rf, rt) = (self.find(from), self.find(to));
        if rf == rt {
            return true;
        }
        // An integer has no `Node` in it, so a flow of one is an equation (as a literal's type must be decided
        // together with where it goes).
        if matches!(self.node(rf), Node::Unbound { int: true }) || matches!(self.node(rt), Node::Unbound { int: true })
        {
            self.unify(types, from, to, span);
            return true;
        }
        match (self.node(rf), self.node(rt)) {
            (Node::Unbound { .. }, Node::Unbound { .. }) => {
                if force {
                    self.unify(types, from, to, span);
                }
                force
            }
            (Node::Bound(s), Node::Unbound { int }) => {
                if int && !is_int_shape(types, &s) {
                    let d = self.describe(types, from);
                    self.error(span, format!("type mismatch: {d} and an integer"));
                    return true;
                }
                self.copy_shape(types, rf, &s, rt, true, check, relate, span);
                true
            }
            (Node::Unbound { int }, Node::Bound(s)) => {
                // A requirement gives an unknown source its shape at once (a lattice target decides a lift); the
                // source's roles still come from its own values, and the requirement checks them.
                if !(relate || force || check) {
                    return false;
                }
                if !relate && !check {
                    // Nothing but this flow says what the source is (a `None`, an empty collection): it is what the
                    // target is.
                    self.unify(types, from, to, span);
                    return true;
                }
                if int && !is_int_shape(types, &s) {
                    let d = self.describe(types, to);
                    self.error(span, format!("type mismatch: an integer and {d}"));
                    return true;
                }
                self.copy_shape(types, rt, &s, rf, false, check, relate, span);
                true
            }
            (Node::Bound(a), Node::Bound(b)) => {
                let pairs: Option<Vec<(T, T)>> = match (&a, &b) {
                    (Shape::Con(x), Shape::Con(y)) => match (Role::of(types.get(*x)), Role::of(types.get(*y))) {
                        (Some(_), Some(_)) => {
                            if !relate {
                                self.role_edges.push((rf, rt, check, span));
                            }
                            Some(Vec::new())
                        }
                        _ => (x == y).then(Vec::new),
                    },
                    (Shape::Tuple(xs), Shape::Tuple(ys)) if xs.len() == ys.len() => {
                        Some(xs.iter().copied().zip(ys.iter().copied()).collect())
                    }
                    (Shape::Option(x), Shape::Option(y))
                    | (Shape::Vec(x), Shape::Vec(y))
                    | (Shape::Set(x), Shape::Set(y)) => Some(vec![(*x, *y)]),
                    (Shape::Map(k1, v1), Shape::Map(k2, v2)) => Some(vec![(*k1, *k2), (*v1, *v2)]),
                    (Shape::Lat(x), Shape::Lat(y)) if x.name() == y.name() && x.args().len() == y.args().len() => {
                        Some(x.args().into_iter().zip(y.args()).collect())
                    }
                    _ => None,
                };
                match pairs {
                    Some(ps) => {
                        for (x, y) in ps {
                            self.sub_flow(types, x, y, check, relate, span);
                        }
                    }
                    None => {
                        let (da, db) = (self.describe(types, from), self.describe(types, to));
                        self.error(span, format!("type mismatch: {da} and {db}"));
                    }
                }
                true
            }
            _ => true,
        }
    }

    /// A flow between two positions of a flow: decided now if it can be, else deferred.
    fn sub_flow(&mut self, types: &TypeTable, from: T, to: T, check: bool, relate: bool, span: Span) {
        if !self.flow_step(types, from, to, check, relate, span, false) {
            self.deferred.push(Deferred::Flow {
                from,
                to,
                check,
                relate,
                span,
            });
        }
    }

    /// Gives the unknown term `target` the shape `s` with fresh positions, each flowing from (`s_is_from`) or into the
    /// matching position of `s`. A `Node` leaf becomes a fresh `Node` whose role the flows decide.
    #[allow(clippy::too_many_arguments)]
    fn copy_shape(
        &mut self,
        types: &TypeTable,
        src: T,
        s: &Shape,
        target: T,
        s_is_from: bool,
        check: bool,
        relate: bool,
        span: Span,
    ) {
        let mut pairs = Vec::new();
        let mut fresh = |me: &mut Self, t: T| {
            let n = me.fresh(false);
            pairs.push((t, n));
            n
        };
        let shape = match s {
            Shape::Con(ty) => {
                if Role::of(types.get(*ty)).is_some() {
                    // A fresh `Node` position: its role is what flows into it (or, as a source, what it is).
                    let any = types.lookup(&TypeDef::Node(None)).unwrap_or(*ty);
                    self.set(target, Node::Bound(Shape::Con(any)));
                    self.free.insert(target);
                    if !relate {
                        let edge = if s_is_from { (src, target) } else { (target, src) };
                        self.role_edges.push((edge.0, edge.1, check, span));
                    }
                    return;
                }
                Shape::Con(*ty)
            }
            Shape::Tuple(ts) => Shape::Tuple(ts.iter().map(|t| fresh(self, *t)).collect()),
            Shape::Option(t) => Shape::Option(fresh(self, *t)),
            Shape::Vec(t) => Shape::Vec(fresh(self, *t)),
            Shape::Set(t) => Shape::Set(fresh(self, *t)),
            Shape::Map(k, v) => {
                let k = fresh(self, *k);
                Shape::Map(k, fresh(self, *v))
            }
            Shape::Lat(l) => Shape::Lat(match l {
                LatS::Bool => LatS::Bool,
                LatS::Max(e) => LatS::Max(fresh(self, *e)),
                LatS::Min(e) => LatS::Min(fresh(self, *e)),
                LatS::Set(e) => LatS::Set(fresh(self, *e)),
                LatS::PSet(e) => LatS::PSet(fresh(self, *e)),
                LatS::Point(e) => LatS::Point(fresh(self, *e)),
                LatS::Map(k, v) => {
                    let k = fresh(self, *k);
                    LatS::Map(k, fresh(self, *v))
                }
            }),
        };
        self.set(target, Node::Bound(shape));
        for (old, new) in pairs {
            if s_is_from {
                self.sub_flow(types, old, new, check, relate, span);
            } else {
                self.sub_flow(types, new, old, check, relate, span);
            }
        }
    }

    /// Solves the roles of `Node` positions over the flows between them (LANGUAGE §5.3). A position holds what its own
    /// type says (a column read, a parameter, `self`), narrowed by equations with other positions (a rule variable in
    /// several atoms, `==` in a rule body), and, when values flow into it (a merge: `if`, `match`, a collection, a
    /// fold), only what flows in: the least role of those, within its own. The least solution is found by iterating
    /// from "nothing flows in". A requirement (`check`) then holds when what flows out fits the target's own type.
    fn solve_roles(&mut self, hir: &mut Hir) {
        let edges: Vec<(T, T, bool, Span)> = self
            .role_edges
            .clone()
            .into_iter()
            .map(|(f, t, c, s)| (self.find(f), self.find(t), c, s))
            .collect();
        let mut base: BTreeMap<T, Role> = BTreeMap::new();
        for &(f, t, _, _) in &edges {
            for r in [f, t] {
                if let Node::Bound(Shape::Con(ty)) = self.node(r)
                    && let Some(role) = Role::of(hir.types.get(ty))
                {
                    base.insert(r, role);
                }
            }
        }
        let mut incoming: BTreeMap<T, Vec<T>> = BTreeMap::new();
        for &(f, t, _, _) in &edges {
            incoming.entry(t).or_default().push(f);
        }
        // A free position nothing flows into holds no value (a `None`'s): it fits any requirement.
        let mut role: BTreeMap<T, Role> = base
            .iter()
            .map(|(r, b)| {
                let start = if incoming.contains_key(r) || self.free.contains(r) {
                    Role::Bot
                } else {
                    *b
                };
                (*r, start)
            })
            .collect();
        let mut changed = true;
        while changed {
            changed = false;
            for (t, froms) in &incoming {
                let flowed = froms
                    .iter()
                    .fold(Role::Bot, |acc, f| acc.lub(role.get(f).copied().unwrap_or(Role::Any)));
                let own = if self.free.contains(t) {
                    Role::Any
                } else {
                    base.get(t).copied().unwrap_or(Role::Any)
                };
                let next = own.glb(flowed);
                if role.get(t) != Some(&next) {
                    role.insert(*t, next);
                    changed = true;
                }
            }
        }
        for &(f, t, check, span) in &edges {
            let got = role.get(&f).copied().unwrap_or(Role::Any);
            let own = base.get(&t).copied().unwrap_or(Role::Any);
            let name = |r: Role| match r {
                Role::Of(x) => match hir.roles.get(x.index()) {
                    Some(role) => format!("Node<{}>", role.name),
                    None => format!("Node<role {}>", x.index()),
                },
                _ => "Node".to_owned(),
            };
            if check && got != Role::Bot && !got.within(own) {
                let msg = format!("a value of type {} where {} is expected", name(got), name(own));
                self.error(span, msg);
            }
            if !check && got != Role::Bot && own != Role::Any && role.get(&t) == Some(&Role::Bot) {
                let msg = format!("a value of type {} can never be a {}", name(got), name(own));
                self.error(span, msg);
            }
        }
        // A position nothing reaches (a cycle of merges with no source) holds any node.
        self.roles = role
            .into_iter()
            .map(|(r, x)| (r, if x == Role::Bot { Role::Any } else { x }))
            .collect();
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
            Shape::Con(ty) => match (Role::of(hir.types.get(ty)), self.roles.get(&r)) {
                (Some(_), Some(role)) => {
                    let def = match role {
                        Role::Of(x) => TypeDef::Node(Some(*x)),
                        Role::Bot | Role::Any => TypeDef::Node(None),
                    };
                    intern(&mut hir.types, def)
                }
                _ => ty,
            },
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
        self.fn_sigs = hir
            .fns
            .iter()
            .map(|f| (f.params.iter().map(|p| p.1).collect(), f.ret))
            .collect();
        // A fresh `Node` position starts as `Node` (any node), so that type must exist.
        intern(&mut hir.types, TypeDef::Node(None));
        self.instances(hir);
        self.walk(hir);
        self.solve(hir);
        if self.diags.error_count() > self.errors_before {
            return;
        }
        self.solve_roles(hir);
        if self.diags.error_count() > self.errors_before {
            return;
        }
        self.instance_types(hir);
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
        constant_facts(hir, self.diags);
    }

    /// The terms of each generic function instance (LANGUAGE §16.1): a fresh term per type parameter, its
    /// signature over them, and each function argument's signature unified with its parameter's type.
    fn instances(&mut self, hir: &mut Hir) {
        for i in 0..hir.fns.len() {
            let Some(scheme) = hir.fns.get(i).and_then(|f| f.scheme.clone()) else {
                continue;
            };
            let tparams: Vec<T> = scheme.tparams.iter().map(|_| self.fresh(false)).collect();
            let params = scheme
                .params
                .iter()
                .map(|t| self.scheme_term(hir, t, &tparams))
                .collect();
            let ret = self.scheme_term(hir, &scheme.ret, &tparams);
            for (name, fty, g) in &scheme.fn_args {
                let Some((gparams, gret)) = self.fn_sigs.get(g.index()).cloned() else {
                    self.bugs
                        .push(internal_error!("function argument {g:?} was not declared"));
                    continue;
                };
                let gname = hir.fns.get(g.index()).map(|f| f.name.clone());
                if gparams.len() != fty.params.len() {
                    self.error(
                        scheme.call,
                        format!(
                            "`{}` takes {} argument(s), but `{}` of `{}` is called with {}",
                            gname.map(|n| n.to_string()).unwrap_or_default(),
                            gparams.len(),
                            name.as_str(),
                            scheme.generic,
                            fty.params.len()
                        ),
                    );
                    continue;
                }
                for (gp, p) in gparams.iter().zip(&fty.params) {
                    let a = self.of_type(hir, *gp);
                    let b = self.scheme_term(hir, p, &tparams);
                    self.unify(&hir.types, a, b, scheme.call);
                }
                let a = self.of_type(hir, gret);
                let b = self.scheme_term(hir, &fty.ret, &tparams);
                self.unify(&hir.types, a, b, scheme.call);
            }
            self.fn_terms.insert(i, InstanceTerms { params, ret, tparams });
        }
    }

    /// The term of a signature type, the type parameters being `tparams`.
    fn scheme_term(&mut self, hir: &Hir, t: &HTy, tparams: &[T]) -> T {
        let shape = match t {
            HTy::Con(ty) => return self.of_type(hir, *ty),
            HTy::Param(i) => match tparams.get(*i as usize) {
                Some(t) => return *t,
                None => {
                    self.bugs
                        .push(internal_error!("type parameter {i} of an instance is undeclared"));
                    return self.fresh(false);
                }
            },
            HTy::Tuple(ts) => Shape::Tuple(ts.iter().map(|t| self.scheme_term(hir, t, tparams)).collect()),
            HTy::Option(t) => Shape::Option(self.scheme_term(hir, t, tparams)),
            HTy::Vec(t) => Shape::Vec(self.scheme_term(hir, t, tparams)),
            HTy::Set(t) => Shape::Set(self.scheme_term(hir, t, tparams)),
            HTy::Map(k, v) => {
                let k = self.scheme_term(hir, k, tparams);
                Shape::Map(k, self.scheme_term(hir, v, tparams))
            }
        };
        self.bound(shape)
    }

    /// Writes each generic function instance's inferred parameter and result types, once solved. Type parameters
    /// its call does not determine are reported here, before anything in the body is.
    fn instance_types(&mut self, hir: &mut Hir) {
        let terms = self.fn_terms.clone();
        for (i, inst) in terms {
            let Some(scheme) = hir.fns.get(i).and_then(|f| f.scheme.clone()) else {
                continue;
            };
            let mut known = true;
            let mut targs = Vec::new();
            for (name, t) in scheme.tparams.iter().zip(&inst.tparams) {
                if let Some(ty) = self.solved(hir, *t) {
                    targs.push(ty);
                } else {
                    known = false;
                    self.error(
                        scheme.call,
                        format!(
                            "cannot infer the type parameter `{}` of `{}` at this call",
                            name.as_str(),
                            scheme.generic
                        ),
                    );
                }
            }
            let Some(span) = hir.fns.get(i).map(|f| f.span) else {
                continue;
            };
            if !known {
                continue;
            }
            let mut params = Vec::new();
            for t in &inst.params {
                params.push(self.solved(hir, *t));
            }
            let ret = self.solved(hir, inst.ret);
            let tys: Vec<TypeId> = params.iter().flatten().copied().chain(ret).collect();
            if tys.iter().any(|t| holds_lattice(&hir.types, *t)) {
                self.diags.push(
                    Diagnostic::not_implemented(
                        blossom_base::FeatureId("LANG-182"),
                        "a generic function instantiated with lattice types (they need a monotonicity class)",
                        "the Blossom frontend",
                    )
                    .with_primary(scheme.call),
                );
                continue;
            }
            let Some(f) = hir.fns.get_mut(i) else { continue };
            if let Some(s) = f.scheme.as_mut() {
                s.targs = targs;
            }
            for ((_, slot), t) in f.params.iter_mut().zip(params) {
                match t {
                    Some(t) => *slot = t,
                    None => self
                        .bugs
                        .push(internal_error!("an instance parameter of {span:?} is unsolved")),
                }
            }
            match ret {
                Some(t) => f.ret = t,
                None => self
                    .bugs
                    .push(internal_error!("an instance result of {span:?} is unsolved")),
            }
        }
    }

    fn walk(&mut self, hir: &mut Hir) {
        let mut handlers = std::mem::take(&mut hir.handlers);
        for h in &mut handlers {
            self.placed = h.role;
            self.body(hir, h.scope, &mut h.header, h.role);
            let mut outer = BTreeSet::new();
            Self::positively_bound(&h.header, &mut outer);
            self.stmts(hir, h.scope, &mut h.stmts, h.role, &outer);
        }
        hir.handlers = handlers;
        let mut views = std::mem::take(&mut hir.views);
        for v in &mut views {
            let role = self.rel_of(hir, v.rel).role;
            self.placed = role;
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
                            // A view's column holds what its alternatives put there.
                            let declared = hir
                                .rels
                                .get(rel)
                                .and_then(|r| r.cols.get(c))
                                .is_some_and(|c| c.ty.is_some());
                            self.flow(vt, ct, declared, v.span);
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
                        // The union holds every alternative's valuations: each flows into it (a merge, whose
                        // roles are the join of the alternatives').
                        for (i, var) in vars.iter().enumerate() {
                            let a = self.var_term(scope, *var);
                            let u = self.var_term(union, HVarId(i as u32));
                            self.flow(a, u, false, v.span);
                        }
                    }
                }
                for (c, col) in cols.iter_mut().enumerate() {
                    let ct = self.col_term(hir, rel, c);
                    match col {
                        HViewAggCol::Group(var) => {
                            if !self.apply {
                                let u = self.var_term(union, *var);
                                let declared = hir
                                    .rels
                                    .get(rel)
                                    .and_then(|r| r.cols.get(c))
                                    .is_some_and(|c| c.ty.is_some());
                                self.flow(u, ct, declared, v.span);
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
            self.placed = inv.role;
            self.body(hir, inv.scope, &mut inv.body, inv.role);
        }
        hir.invariants = invariants;
        let mut guards = std::mem::take(&mut hir.guards);
        for g in &mut guards {
            self.placed = g.role;
            self.body(hir, g.scope, &mut g.body, g.role);
        }
        hir.guards = guards;
        self.placed = None;
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
                self.coerce_site(hir, e, t, ct, true);
            }
        }
        hir.facts = facts;
        let mut fns = std::mem::take(&mut hir.fns);
        for (i, f) in fns.iter_mut().enumerate() {
            let inst = self.fn_terms.get(&i).cloned();
            if !self.apply {
                for (k, (v, ty)) in f.params.iter().enumerate() {
                    let vt = self.var_term(f.scope, *v);
                    let pt = match inst.as_ref().and_then(|t| t.params.get(k)) {
                        Some(pt) => *pt,
                        None => self.of_type(hir, *ty),
                    };
                    self.unify(&hir.types, vt, pt, f.span);
                }
            }
            let HFnBody::Expr(body) = &mut f.body else {
                continue;
            };
            let t = self.expr(hir, f.scope, body);
            let r = match (&inst, self.apply) {
                (_, true) => 0,
                (Some(inst), false) => inst.ret,
                (None, false) => self.of_type(hir, f.ret),
            };
            self.coerce_site(hir, body, t, r, true);
        }
        hir.fns = fns;
    }

    /// A handler's statements; `outer` holds the variables bound outside them (by the header and enclosing blocks).
    fn stmts(
        &mut self,
        hir: &mut Hir,
        scope: ScopeId,
        stmts: &mut [HStmt],
        role: Option<HRoleId>,
        outer: &BTreeSet<HVarId>,
    ) {
        for s in stmts {
            match s {
                HStmt::Verb(v) => self.verb(hir, scope, v),
                HStmt::Block { cond, stmts, span, .. } => {
                    // A block's condition holds only inside it, so what it says of a variable bound outside refines
                    // the variable there and not in the handler's other statements: inside, the variable is a copy
                    // the outer one flows into, whose roles are the meet of the outer ones and the condition's.
                    let saved = if self.apply {
                        Vec::new()
                    } else {
                        let saved = self.refine(scope, outer, *span);
                        let inside = saved.iter().map(|(v, _)| (*v, self.var_term(scope, *v))).collect();
                        self.block_terms.push(inside);
                        saved
                    };
                    self.body(hir, scope, cond, role);
                    let mut inner = outer.clone();
                    Self::positively_bound(cond, &mut inner);
                    self.stmts(hir, scope, stmts, role, &inner);
                    for (v, t) in saved {
                        self.set_var_term(scope, v, t);
                    }
                }
            }
        }
    }

    /// Writes each block's refined variable types (the solved terms of `block_terms`, from `next` on).
    fn refined_blocks(&mut self, hir: &mut Hir, stmts: &mut [HStmt], next: &mut usize) {
        for s in stmts {
            if let HStmt::Block { stmts, refined, .. } = s {
                let terms = self.block_terms.get(*next).cloned().unwrap_or_default();
                *next += 1;
                refined.clear();
                for (v, t) in terms {
                    // An unsolved term is reported for the variable itself (its outer term flows into this one).
                    if let Some(ty) = self.solved(hir, t) {
                        refined.push((v, ty));
                    }
                }
                self.refined_blocks(hir, stmts, next);
            }
        }
    }

    /// Gives each variable of `vars` a copy of its term for a block, returning the terms it had.
    fn refine(&mut self, scope: ScopeId, vars: &BTreeSet<HVarId>, span: Span) -> Vec<(HVarId, T)> {
        let mut saved = Vec::new();
        for v in vars {
            let old = self.var_term(scope, *v);
            let copy = self.fresh(false);
            self.flow(old, copy, false, span);
            self.set_var_term(scope, *v, copy);
            saved.push((*v, old));
        }
        saved
    }

    fn set_var_term(&mut self, scope: ScopeId, v: HVarId, t: T) {
        if let Some(slot) = self.var_terms.get_mut(scope.index()).and_then(|s| s.get_mut(v.index())) {
            *slot = t;
        }
    }

    fn verb(&mut self, hir: &mut Hir, scope: ScopeId, v: &mut HVerbStmt) {
        let rel = v.target.index();
        for (c, a) in v.args.iter_mut().enumerate() {
            let ct = self.col_term(hir, rel, c);
            match a {
                HHeadArg::Expr(e) => {
                    let t = self.expr(hir, scope, e);
                    let declared = hir
                        .rels
                        .get(rel)
                        .and_then(|r| r.cols.get(c))
                        .is_some_and(|c| c.ty.is_some());
                    self.coerce_site(hir, e, t, ct, declared);
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
                        // A channel to role R is sent to one of R's members.
                        self.con(&mut hir.types, TypeDef::Node(Some(RoleId::from_raw(dst.0))))
                    }
                } else {
                    self.con(&mut hir.types, TypeDef::Node(None))
                };
                self.flow(t, want, true, to.span);
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
            AggKind::Collect => {
                // The group's values merge into one vector: each flows into its element type (LANGUAGE §5.3), and
                // must fit a declared one.
                let el = self.fresh(false);
                if let Some(a) = arg_terms.first() {
                    self.flow(*a, el, true, agg.span);
                }
                let v = self.bound(Shape::Vec(el));
                self.unify(&hir.types, v, col, agg.span);
            }
            AggKind::Sum | AggKind::Min | AggKind::Max => {
                // The aggregate is one of the group's values (a sum is an integer), which goes into the column: a
                // flow that must fit the column's type, so neither narrows the other.
                if let Some(a) = arg_terms.first() {
                    self.flow(*a, col, true, agg.span);
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
            self.flow(d, col, true, agg.span);
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
                self.atom(hir, scope, a, None, false);
            }
            HLit::Not(a) => self.atom(hir, scope, a, None, true),
            HLit::Outer(a) => {
                if !self.apply {
                    for p in &a.args {
                        self.other_bindings(scope, p);
                    }
                }
                self.atom(hir, scope, a, Some(bound), false)
            }
            HLit::NotBody(b, _) => {
                let was = std::mem::replace(&mut self.in_conjunction, false);
                self.body(hir, scope, b, role);
                self.in_conjunction = was;
            }
            HLit::Let { pat, expr, span } => {
                if !self.apply {
                    self.other_bindings(scope, pat);
                }
                let t = self.expr(hir, scope, expr);
                let p = self.pat(hir, scope, pat);
                if matches!(pat, HPat::Var(..)) {
                    // `let x = e` where `x` is a lattice (a lattice view column, say) lifts `e` (LANGUAGE §5.6).
                    self.coerce_site(hir, expr, t, p, false);
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
                self.conjunct_eq = self.in_conjunction && !self.apply;
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
                let was = std::mem::replace(&mut self.in_conjunction, false);
                for b in bodies {
                    self.body(hir, scope, b, role);
                }
                self.in_conjunction = was;
            }
            HLit::Forall { domain, body, .. } => {
                let was = std::mem::replace(&mut self.in_conjunction, false);
                let empty = BTreeSet::new();
                self.lit(hir, scope, domain, role, &empty);
                self.body(hir, scope, body, role);
                self.in_conjunction = was;
            }
        }
    }

    /// An atom: each argument against its column. For `outer`, variables not bound elsewhere are options.
    fn atom(&mut self, hir: &mut Hir, scope: ScopeId, a: &mut HAtom, outer: Option<&BTreeSet<HVarId>>, negated: bool) {
        let rel = a.rel.index();
        for (c, p) in a.args.iter_mut().enumerate() {
            let ct = self.col_term(hir, rel, c);
            let pt = self.pat(hir, scope, p);
            if self.apply {
                continue;
            }
            let inferred = hir
                .rels
                .get(rel)
                .and_then(|r| r.cols.get(c))
                .is_some_and(|c| c.ty.is_none());
            match (outer, &*p) {
                (Some(bound), HPat::Var(v, span)) if !bound.contains(v) => {
                    let opt = self.bound(Shape::Option(ct));
                    self.unify(&hir.types, pt, opt, *span);
                }
                // A negated atom, or one under `not`, `any` or `forall`, does not bind what it tests: its columns
                // say nothing of the rule's variables.
                _ if negated || !self.in_conjunction => self.relate(pt, ct, p.span()),
                // An inferred view column's term is shared by every use: a read takes a copy of what it holds, so
                // the reading rule's own equations stay its own.
                _ if inferred => self.member(ct, pt, p.span()),
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
                self.variant(hir, ty, *variant, &fts, *span, true)
            }
        }
    }

    /// The term of a variant constructor or pattern; in the apply walk, resolves `Option` to its type.
    fn variant(&mut self, hir: &mut Hir, ty: &mut TypeRef, variant: u32, fields: &[T], span: Span, pattern: bool) -> T {
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
                    if pattern {
                        // A pattern's field is the payload.
                        self.unify(&hir.types, *f, pt, span);
                    } else {
                        // A constructor's argument must fit the payload's declared type.
                        self.flow(*f, pt, true, span);
                    }
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
        // Only the guard expression itself, not its subexpressions, is a conjunct.
        let meet = std::mem::take(&mut self.conjunct_eq);
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
                        // A comparison relates its operands; only `==` as a conjunct of a rule body makes them one
                        // value, so there it is an equation (LANGUAGE §9.1).
                        BinOp::Eq if meet => {
                            self.unify(&hir.types, a, b, span);
                            self.con(&mut hir.types, TypeDef::Bool)
                        }
                        BinOp::Eq | BinOp::Ne => {
                            self.relate(a, b, span);
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
                            let res = self.fresh(false);
                            self.flow(a, res, false, span);
                            self.flow(b, res, false, span);
                            self.deferred.push(Deferred::Concat { t: res, span });
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
                let t = self.variant(hir, &mut tyref, variant, &fts, span, false);
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
                        self.flow(ft, want, true, f.span);
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
                    // The branches merge: the value is one of them.
                    let res = self.fresh(false);
                    self.flow(a, res, false, span);
                    self.flow(b, res, false, span);
                    self.record(res)
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
                        self.flow(bt, res, false, span);
                    }
                }
                if self.apply {
                    if let Some(value) = crate::exhaustive::uncovered(hir, arms) {
                        self.diags.push(
                            Diagnostic::new(
                                code!("BLS0314"),
                                format!("this `match` does not cover every value: `{value}` reaches no arm"),
                            )
                            .with_primary(span)
                            .with_note("add an arm for it, or a final `_ => …`; an arm with a guard covers nothing"),
                        );
                    }
                    self.next_term()
                } else {
                    self.record(res)
                }
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
                    // A rule placed at role R runs on R's members: its `self` is a `Node<R>` (LANGUAGE §6.10).
                    let role = self.placed.map(|r| RoleId::from_raw(r.0));
                    let t = self.con(&mut hir.types, TypeDef::Node(role));
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
                        Builtin::RoleSize(_) | Builtin::Rand | Builtin::Hash64 => {
                            self.con(&mut hir.types, TypeDef::Int(IntTy::U64))
                        }
                        Builtin::Error => {
                            // The message is a String; the call never returns, so it takes its context's type.
                            if let Some(m) = ats.first() {
                                let s = self.con(&mut hir.types, TypeDef::Str);
                                self.unify(&hir.types, *m, s, span);
                            }
                            self.fresh(false)
                        }
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
                                self.deferred.push(Deferred::Quorum {
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
                        Builtin::Lib(LibFn::Range) => {
                            let u = self.con(&mut hir.types, TypeDef::Int(IntTy::U64));
                            for a in &ats {
                                self.unify(&hir.types, *a, u, span);
                            }
                            self.bound(Shape::Vec(u))
                        }
                        Builtin::Lib(LibFn::BytesFrom(it)) => {
                            let t = self.con(&mut hir.types, TypeDef::Int(it));
                            for a in &ats {
                                self.unify(&hir.types, *a, t, span);
                            }
                            self.con(&mut hir.types, TypeDef::Bytes)
                        }
                        Builtin::Lib(LibFn::BytesUvarint | LibFn::BytesVarint) => {
                            let it = if f == Builtin::Lib(LibFn::BytesUvarint) {
                                IntTy::U64
                            } else {
                                IntTy::I64
                            };
                            let t = self.con(&mut hir.types, TypeDef::Int(it));
                            for a in &ats {
                                self.unify(&hir.types, *a, t, span);
                            }
                            self.con(&mut hir.types, TypeDef::Bytes)
                        }
                        Builtin::Lib(LibFn::BytesEmpty) => self.con(&mut hir.types, TypeDef::Bytes),
                        Builtin::Lib(LibFn::DurationFromMillis) => {
                            let i = self.con(&mut hir.types, TypeDef::Int(IntTy::I64));
                            for a in &ats {
                                self.unify(&hir.types, *a, i, span);
                            }
                            self.con(&mut hir.types, TypeDef::Duration)
                        }
                        Builtin::Lib(LibFn::BlobOf) => {
                            let b = self.con(&mut hir.types, TypeDef::Bytes);
                            for a in &ats {
                                self.unify(&hir.types, *a, b, span);
                            }
                            self.con(&mut hir.types, TypeDef::Blob)
                        }
                        Builtin::Lib(LibFn::BytesJoin) => {
                            let b = self.con(&mut hir.types, TypeDef::Bytes);
                            let v = self.bound(Shape::Vec(b));
                            for a in &ats {
                                self.unify(&hir.types, *a, v, span);
                            }
                            b
                        }
                        Builtin::Lib(other) => {
                            // Library methods are resolved from `Method` by the solver; only `range` and the
                            // `Bytes::…` constructors are calls.
                            self.bugs.push(internal_error!(
                                "the library method {other:?} reached type checking resolved"
                            ));
                            0
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
                                self.flow(*x, el, false, span);
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
                                self.flow(*x, if i % 2 == 0 { k } else { v }, false, span);
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
                        self.coerce_site(hir, v, *vt, 0, false);
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
                                self.coerce_site(hir, value, y, v, false);
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
            HExprKind::Call { f, args } => {
                let Some((params, ret)) = self.fn_sigs.get(f.index()).cloned() else {
                    self.bugs.push(internal_error!("call of an undeclared function {f:?}"));
                    return 0;
                };
                let mut ts = Vec::new();
                for a in args.iter_mut() {
                    ts.push(self.expr(hir, scope, a));
                }
                if self.apply {
                    self.next_term()
                } else if let Some(inst) = self.fn_terms.get(&f.index()).cloned() {
                    // A generic function's instance: its one call shares its terms.
                    for (a, pt) in ts.iter().zip(&inst.params) {
                        self.flow(*a, *pt, true, span);
                    }
                    self.record(inst.ret)
                } else {
                    for (a, ty) in ts.iter().zip(&params) {
                        let pt = self.of_type(hir, *ty);
                        self.flow(*a, pt, true, span);
                    }
                    let r = self.of_type(hir, ret);
                    self.record(r)
                }
            }
            HExprKind::Let { pat, ty, value, body } => {
                let ty = *ty;
                if !self.apply {
                    self.other_bindings(scope, pat);
                }
                let v = self.expr(hir, scope, value);
                let p = self.pat(hir, scope, pat);
                if !self.apply {
                    match ty {
                        // `let x: T = v`: `x` is a `T`, and `v` must fit it.
                        Some(ty) => {
                            let a = self.of_type(hir, ty);
                            self.unify(&hir.types, p, a, span);
                            self.flow(v, a, true, span);
                        }
                        None => self.unify(&hir.types, p, v, span),
                    }
                }
                let b = self.expr(hir, scope, body);
                self.record(b)
            }
            HExprKind::CallParam { .. } | HExprKind::GenericCall { .. } => {
                self.bugs.push(internal_error!(
                    "a generic function's template call reached type checking (only instances are checked)"
                ));
                return 0;
            }
            HExprKind::Closure { params, body } => {
                let ps: Vec<T> = params.iter().map(|v| self.var_term(scope, *v)).collect();
                let b = self.expr(hir, scope, body);
                if self.apply {
                    // A closure is not a value: it has no type of its own, only its parameters and body do.
                    return self.next_term();
                }
                let t = self.fresh(false);
                self.closures.insert(t, (ps, b));
                return self.record(t);
            }
        };
        if self.apply {
            match self.solved(hir, t) {
                Some(ty) => {
                    e.ty = Some(ty);
                    self.literal_fits(hir, e);
                }
                None => self.error(span, "cannot infer the type of this expression".into()),
            }
        }
        t
    }

    /// An integer literal must fit the type it was given (an unsuffixed literal's type is inferred, so this is known
    /// only now): `Bytes::from_u8(300)` is BLS0300.
    fn literal_fits(&mut self, hir: &Hir, e: &HExpr) {
        let (HExprKind::IntLit(n, neg) | HExprKind::TypedInt(n, _, neg)) = e.kind else {
            return;
        };
        if let Some(TypeDef::Int(ity)) = e.ty.and_then(|t| hir.types.get(t))
            && crate::resolve::int_value(n, *ity, neg).is_none()
        {
            let sign = if neg { "-" } else { "" };
            self.error(e.span, format!("{sign}{n} does not fit in {}", ity.name()));
        }
    }

    /// A coercion site: `e`, of term `from`, where a value of term `to` is expected (a head column, a `let`
    /// variable, a constructor argument). The solver decides whether `e` is lifted into a lattice (LANGUAGE §5.6); the
    /// apply walk then wraps it. Both walks call this once per site, in the same order.
    fn coerce_site(&mut self, hir: &mut Hir, e: &mut HExpr, from: T, to: T, check: bool) {
        if !self.apply {
            let slot = self.lifts.len();
            self.lifts.push((false, to));
            self.deferred.push(Deferred::Coerce {
                slot,
                from,
                to,
                check,
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
                self.settle_plain(&d);
                continue;
            }
            // A sum or difference with a `Duration` whose other operand nothing decided: it is a duration too.
            let sums: Vec<(usize, T, T)> = self
                .deferred
                .iter()
                .enumerate()
                .filter_map(|(i, d)| match d {
                    Deferred::Arith { l, r, .. } => Some((i, *l, *r)),
                    _ => None,
                })
                .collect();
            let duration_sum = sums.into_iter().find(|&(_, l, r)| {
                [l, r].into_iter().any(|t| {
                    self.leaf(t)
                        .is_some_and(|ty| matches!(hir.types.get(ty), Some(TypeDef::Duration)))
                })
            });
            if let Some((i, _, _)) = duration_sum {
                let d = self.deferred.remove(i);
                self.settle_duration = true;
                let done = self.try_deferred(hir, &d);
                self.settle_duration = false;
                if !done {
                    return self.bugs.push(internal_error!("a Duration sum did not settle"));
                }
                continue;
            }
            // A flow whose source is still unknown after everything else: nothing but the flow constrains it, so it
            // is settled as an equation (the source takes the target's shape).
            if let Some(i) = self.deferred.iter().position(|d| matches!(d, Deferred::Flow { .. })) {
                if let Deferred::Flow {
                    from,
                    to,
                    check,
                    relate,
                    span,
                } = self.deferred.remove(i)
                {
                    self.flow_step(&hir.types, from, to, check, relate, span, true);
                }
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
                Deferred::Flow { span, .. }
                | Deferred::Arith { span, .. }
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
                | Deferred::Quorum { span, .. }
                | Deferred::Gen { span, .. }
                | Deferred::Method { span, .. } => span,
            };
            self.error(span, "cannot infer the operand types of this expression".into());
        }
    }

    /// Tries a deferred constraint; `true` when it was discharged (successfully or with an error).
    fn try_deferred(&mut self, hir: &mut Hir, d: &Deferred) -> bool {
        match *d {
            Deferred::Flow {
                from,
                to,
                check,
                relate,
                span,
            } => self.flow_step(&hir.types, from, to, check, relate, span, false),
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
                        // `x + d` and `x - d` with `x` not yet known may be an `Instant` plus a duration: wait for
                        // `x` (only `d - x` makes `x` a duration at once), and settle it as a duration only when
                        // nothing else decides it.
                        let open = match op {
                            BinOp::Add => ld.is_none() || rd.is_none(),
                            BinOp::Sub => ld.is_none(),
                            _ => false,
                        };
                        if open && !self.settle_duration {
                            return false;
                        }
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
                        if !matches!(hir.types.get(t), Some(TypeDef::Str | TypeDef::Bytes | TypeDef::Blob)) {
                            self.error(span, "`.len()` needs a string, bytes, a blob or a collection".into());
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
            // Every type has a canonical order (LANGUAGE §5.5), so `<` applies to any value; it waits only for its
            // operands' type, which lowering needs to pick the numeric or the canonical order.
            Deferred::Ordered { t, .. } => !self.is_unbound(t),
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
            Deferred::Coerce {
                slot,
                from,
                to,
                check,
                span,
            } => {
                let to_lat = self.lat(to);
                if to_lat.is_none() {
                    if self.is_unbound(to) {
                        return false;
                    }
                    self.coerce_plain(from, to, check, span);
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
                    self.settle_plain(d);
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
                    // A membership test compares `elem` with the elements: neither narrows the other.
                    Node::Bound(Shape::Lat(LatS::Set(e) | LatS::PSet(e)) | Shape::Set(e) | Shape::Vec(e)) => {
                        self.relate(elem, e, span);
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
            Deferred::Quorum { elem, coll, span } => {
                let rc = self.find(coll);
                match self.node(rc) {
                    Node::Bound(Shape::Lat(LatS::Set(e) | LatS::PSet(e))) => {
                        self.unify(&hir.types, elem, e, span);
                    }
                    Node::Bound(_) => {
                        let d = self.describe(&hir.types, coll);
                        self.error(
                            span,
                            format!(
                                "`majority` counts a set-like lattice of nodes (`LSet`, `LPSet`), found {d}; a quorum \
                                 only grows, so a plain set is not accepted"
                            ),
                        );
                    }
                    _ => return false,
                }
                true
            }
            Deferred::Gen { pat, src, span } => {
                let rc = self.find(src);
                match self.node(rc) {
                    // The pattern binds a copy of each element, so what else constrains its variables does not
                    // narrow the source's element type.
                    Node::Bound(Shape::Lat(LatS::Set(e) | LatS::PSet(e)) | Shape::Set(e) | Shape::Vec(e)) => {
                        self.member(e, pat, span);
                    }
                    Node::Bound(Shape::Lat(LatS::Map(k, v)) | Shape::Map(k, v)) => {
                        let pair = self.bound(Shape::Tuple(vec![k, v]));
                        self.member(pair, pat, span);
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

    /// Settles a coercion or comparison that no lattice reached: a plain flow, or a comparison's relation.
    fn settle_plain(&mut self, d: &Deferred) {
        match *d {
            Deferred::Coerce {
                from, to, check, span, ..
            } => self.coerce_plain(from, to, check, span),
            Deferred::Compare { l, r, span, .. } => {
                self.relate(l, r, span);
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
                    check: false,
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
            if !banged && let Some(done) = self.plain_method(hir, slot, recv, name, args, res, span) {
                return done;
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
        // A closure is an argument only of the collection combinators (LANGUAGE §16.1), never of a lattice method.
        if args.iter().any(|a| self.closures.contains_key(a)) {
            self.error(
                span,
                format!(
                    "`{}` does not take a closure: only the collection combinators do",
                    name.as_str()
                ),
            );
            return true;
        }
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
                        check: false,
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

    /// A method of a plain value (LANGUAGE Appendix B), by the receiver's shape: `Some(true)` when resolved (or
    /// reported), `None` when the receiver has no such method.
    #[allow(clippy::too_many_arguments)]
    fn plain_method(
        &mut self,
        hir: &mut Hir,
        slot: usize,
        recv: T,
        name: Symbol,
        args: &[T],
        res: T,
        span: Span,
    ) -> Option<bool> {
        let rr = self.find(recv);
        let Node::Bound(shape) = self.node(rr) else {
            return None;
        };
        let leaf = match &shape {
            Shape::Con(id) => hir.types.get(*id).cloned(),
            _ => None,
        };
        let n = name.as_str();
        // Which argument positions hold a closure, and how many arguments there are.
        let (target, closure_at, arity): (Builtin, Option<usize>, usize) = match (&shape, &leaf, n) {
            (Shape::Vec(_) | Shape::Set(_) | Shape::Map(..), _, "contains") => (Builtin::Contains, None, 1),
            (Shape::Vec(_), _, "get") => (Builtin::Lib(LibFn::VecGet), None, 1),
            (Shape::Vec(_), _, "first") => (Builtin::Lib(LibFn::VecFirst), None, 0),
            (Shape::Vec(_), _, "last") => (Builtin::Lib(LibFn::VecLast), None, 0),
            (Shape::Vec(_), _, "push") => (Builtin::Lib(LibFn::VecPush), None, 1),
            (Shape::Vec(_), _, "concat") => (Builtin::Lib(LibFn::VecConcat), None, 1),
            (Shape::Vec(_), _, "is_empty") => (Builtin::Lib(LibFn::VecIsEmpty), None, 0),
            (Shape::Vec(_), _, "reverse") => (Builtin::Lib(LibFn::VecReverse), None, 0),
            (Shape::Vec(_), _, "flatten") => (Builtin::Lib(LibFn::VecFlatten), None, 0),
            (Shape::Vec(_), _, "enumerate") => (Builtin::Lib(LibFn::VecEnumerate), None, 0),
            (Shape::Vec(_), _, "map") => (Builtin::Lib(LibFn::VecMap), Some(0), 1),
            (Shape::Vec(_), _, "filter") => (Builtin::Lib(LibFn::VecFilter), Some(0), 1),
            (Shape::Vec(_), _, "filter_map") => (Builtin::Lib(LibFn::VecFilterMap), Some(0), 1),
            (Shape::Vec(_), _, "all") => (Builtin::Lib(LibFn::VecAll), Some(0), 1),
            (Shape::Vec(_), _, "any") => (Builtin::Lib(LibFn::VecAny), Some(0), 1),
            (Shape::Vec(_), _, "fold") => (Builtin::Lib(LibFn::VecFold), Some(1), 2),
            (Shape::Vec(_), _, "scan") => (Builtin::Lib(LibFn::VecScan), Some(1), 2),
            (Shape::Vec(_), _, "to_set") => (Builtin::Lib(LibFn::VecToSet), None, 0),
            (Shape::Vec(_), _, "to_map") => (Builtin::Lib(LibFn::VecToMap), None, 0),
            (Shape::Map(..), _, "get") => (Builtin::Lib(LibFn::MapGet), None, 1),
            (Shape::Option(_), _, "is_some") => (Builtin::Lib(LibFn::OptIsSome), None, 0),
            (Shape::Option(_), _, "is_none") => (Builtin::Lib(LibFn::OptIsNone), None, 0),
            (Shape::Option(_), _, "unwrap_or") => (Builtin::Lib(LibFn::OptUnwrapOr), None, 1),
            (Shape::Option(_), _, "map") => (Builtin::Lib(LibFn::OptMap), Some(0), 1),
            (Shape::Option(_), _, "and_then") => (Builtin::Lib(LibFn::OptAndThen), Some(0), 1),
            (_, Some(TypeDef::Bytes), "slice") => (Builtin::Lib(LibFn::BytesSlice), None, 2),
            (_, Some(TypeDef::Bytes), "concat") => (Builtin::Lib(LibFn::BytesConcat), None, 1),
            (_, Some(TypeDef::Str), "split_whitespace") => (Builtin::Lib(LibFn::StrSplitWhitespace), None, 0),
            (_, Some(TypeDef::Str), "to_lowercase") => (Builtin::Lib(LibFn::StrToLowercase), None, 0),
            (_, Some(TypeDef::Str), "to_utf8") => (Builtin::Lib(LibFn::StrToUtf8), None, 0),
            (_, Some(TypeDef::Str), "parse_i64") => (Builtin::Lib(LibFn::StrParseI64), None, 0),
            (_, Some(TypeDef::Duration), "as_millis") => (Builtin::Lib(LibFn::DurationAsMillis), None, 0),
            (_, Some(TypeDef::Instant), "as_millis") => (Builtin::Lib(LibFn::InstantAsMillis), None, 0),
            (_, Some(TypeDef::Bytes), "from_utf8") => (Builtin::Lib(LibFn::BytesFromUtf8), None, 0),
            (_, Some(TypeDef::Blob), "read") => (Builtin::Lib(LibFn::BlobRead), None, 2),
            (_, Some(TypeDef::Bytes), "uvarint_at") => (Builtin::Lib(LibFn::BytesUvarintAt), None, 1),
            (_, Some(TypeDef::Bytes), "varint_at") => (Builtin::Lib(LibFn::BytesVarintAt), None, 1),
            (_, Some(TypeDef::Bytes), _) => {
                if let Some(it) = n.strip_suffix("_at").and_then(byte_int) {
                    (Builtin::Lib(LibFn::BytesRead(it)), None, 1)
                } else if let Some(it) = n.strip_prefix("put_").and_then(byte_int) {
                    (Builtin::Lib(LibFn::BytesPut(it)), None, 2)
                } else {
                    return None;
                }
            }
            _ => return None,
        };
        if args.len() != arity {
            self.diags.push(
                Diagnostic::new(
                    code!("BLS0301"),
                    format!("`{n}` takes {arity} argument(s), {} given", args.len()),
                )
                .with_primary(span),
            );
            return Some(true);
        }
        for (i, a) in args.iter().enumerate() {
            let is_closure = self.closures.contains_key(a);
            if is_closure != (closure_at == Some(i)) {
                let msg = if is_closure {
                    format!("`{n}` does not take a closure here")
                } else {
                    format!("`{n}` takes a closure here")
                };
                self.diags
                    .push(Diagnostic::new(code!("BLS0300"), msg).with_primary(span));
                return Some(true);
            }
        }
        // The closure's parameter and body terms, checked against the arity the combinator calls it with.
        let closure = |me: &mut Self, want: usize| -> Option<(Vec<T>, T)> {
            let c = closure_at
                .and_then(|i| args.get(i))
                .and_then(|a| me.closures.get(a))
                .cloned()?;
            if c.0.len() != want {
                me.diags.push(
                    Diagnostic::new(
                        code!("BLS0301"),
                        format!(
                            "`{n}` calls its closure with {want} argument(s); it takes {}",
                            c.0.len()
                        ),
                    )
                    .with_primary(span),
                );
                return None;
            }
            Some(c)
        };
        let bool_t = self.con(&mut hir.types, TypeDef::Bool);
        let u64_t = self.con(&mut hir.types, TypeDef::Int(IntTy::U64));
        let a0 = args.first().copied();
        let a1 = args.get(1).copied();
        let result = match (&shape, target) {
            (Shape::Vec(e) | Shape::Set(e) | Shape::Map(e, _), Builtin::Contains) => {
                if let Some(x) = a0 {
                    self.relate(x, *e, span);
                }
                bool_t
            }
            (Shape::Vec(e), Builtin::Lib(f)) => {
                let e = *e;
                match f {
                    LibFn::VecGet => {
                        if let Some(i) = a0 {
                            self.unify(&hir.types, i, u64_t, span);
                        }
                        self.bound(Shape::Option(e))
                    }
                    LibFn::VecFirst | LibFn::VecLast => self.bound(Shape::Option(e)),
                    // The result's elements merge the receiver's and the new ones (LANGUAGE §5.3).
                    LibFn::VecPush => {
                        let out = self.fresh(false);
                        self.flow(e, out, false, span);
                        if let Some(x) = a0 {
                            self.flow(x, out, false, span);
                        }
                        self.bound(Shape::Vec(out))
                    }
                    LibFn::VecConcat => {
                        let out = self.fresh(false);
                        self.flow(e, out, false, span);
                        if let Some(x) = a0 {
                            let xe = self.fresh(false);
                            let xv = self.bound(Shape::Vec(xe));
                            self.unify(&hir.types, x, xv, span);
                            self.flow(xe, out, false, span);
                        }
                        self.bound(Shape::Vec(out))
                    }
                    LibFn::VecIsEmpty => bool_t,
                    LibFn::VecReverse => recv,
                    // The elements are vectors, and the result is one of their type.
                    LibFn::VecFlatten => {
                        let x = self.fresh(false);
                        let inner = self.bound(Shape::Vec(x));
                        self.unify(&hir.types, e, inner, span);
                        inner
                    }
                    LibFn::VecEnumerate => {
                        let pair = self.bound(Shape::Tuple(vec![u64_t, e]));
                        self.bound(Shape::Vec(pair))
                    }
                    LibFn::VecMap => {
                        let Some((ps, b)) = closure(self, 1) else {
                            return Some(true);
                        };
                        self.unify_params(hir, &ps, &[e], span);
                        self.bound(Shape::Vec(b))
                    }
                    LibFn::VecFilter | LibFn::VecAll | LibFn::VecAny => {
                        let Some((ps, b)) = closure(self, 1) else {
                            return Some(true);
                        };
                        self.unify_params(hir, &ps, &[e], span);
                        self.unify(&hir.types, b, bool_t, span);
                        if f == LibFn::VecFilter { recv } else { bool_t }
                    }
                    LibFn::VecFilterMap => {
                        let Some((ps, b)) = closure(self, 1) else {
                            return Some(true);
                        };
                        self.unify_params(hir, &ps, &[e], span);
                        let out = self.fresh(false);
                        let opt = self.bound(Shape::Option(out));
                        self.unify(&hir.types, b, opt, span);
                        self.bound(Shape::Vec(out))
                    }
                    LibFn::VecFold => {
                        let Some((ps, b)) = closure(self, 2) else {
                            return Some(true);
                        };
                        let Some(init) = a0 else { return Some(true) };
                        // The accumulator holds the initial value and every step's result.
                        let acc = self.fresh(false);
                        self.flow(init, acc, false, span);
                        self.flow(b, acc, false, span);
                        self.unify_params(hir, &ps, &[acc, e], span);
                        acc
                    }
                    LibFn::VecScan => {
                        let Some((ps, b)) = closure(self, 2) else {
                            return Some(true);
                        };
                        let Some(init) = a0 else { return Some(true) };
                        // As a fold's: the accumulator holds the initial value and every step's result.
                        let acc = self.fresh(false);
                        self.flow(init, acc, false, span);
                        self.flow(b, acc, false, span);
                        self.unify_params(hir, &ps, &[acc, e], span);
                        self.bound(Shape::Vec(acc))
                    }
                    LibFn::VecToSet => self.bound(Shape::Set(e)),
                    LibFn::VecToMap => {
                        let (k, v) = (self.fresh(false), self.fresh(false));
                        let pair = self.bound(Shape::Tuple(vec![k, v]));
                        self.unify(&hir.types, e, pair, span);
                        self.bound(Shape::Map(k, v))
                    }
                    other => {
                        self.bugs.push(internal_error!("{other:?} dispatched on a vector"));
                        return Some(true);
                    }
                }
            }
            (Shape::Option(e), Builtin::Lib(f)) => {
                let e = *e;
                match f {
                    LibFn::OptIsSome | LibFn::OptIsNone => bool_t,
                    LibFn::OptUnwrapOr => {
                        let out = self.fresh(false);
                        self.flow(e, out, false, span);
                        if let Some(d) = a0 {
                            self.flow(d, out, false, span);
                        }
                        out
                    }
                    LibFn::OptMap => {
                        let Some((ps, b)) = closure(self, 1) else {
                            return Some(true);
                        };
                        self.unify_params(hir, &ps, &[e], span);
                        self.bound(Shape::Option(b))
                    }
                    LibFn::OptAndThen => {
                        let Some((ps, b)) = closure(self, 1) else {
                            return Some(true);
                        };
                        self.unify_params(hir, &ps, &[e], span);
                        let out = self.fresh(false);
                        let opt = self.bound(Shape::Option(out));
                        self.unify(&hir.types, b, opt, span);
                        b
                    }
                    other => {
                        self.bugs.push(internal_error!("{other:?} dispatched on an option"));
                        return Some(true);
                    }
                }
            }
            (Shape::Map(k, v), Builtin::Lib(LibFn::MapGet)) => {
                // A lookup compares the key with the map's keys.
                if let Some(x) = a0 {
                    self.relate(x, *k, span);
                }
                self.bound(Shape::Option(*v))
            }
            (_, Builtin::Lib(LibFn::BytesSlice)) => {
                for a in [a0, a1].into_iter().flatten() {
                    self.unify(&hir.types, a, u64_t, span);
                }
                self.bound(Shape::Option(recv))
            }
            (_, Builtin::Lib(LibFn::BlobRead)) => {
                for a in [a0, a1].into_iter().flatten() {
                    self.unify(&hir.types, a, u64_t, span);
                }
                let b = self.con(&mut hir.types, TypeDef::Bytes);
                self.bound(Shape::Option(b))
            }
            (_, Builtin::Lib(LibFn::BytesConcat)) => {
                if let Some(x) = a0 {
                    self.unify(&hir.types, x, recv, span);
                }
                recv
            }
            (_, Builtin::Lib(LibFn::StrSplitWhitespace)) => self.bound(Shape::Vec(recv)),
            (_, Builtin::Lib(LibFn::StrToLowercase)) => recv,
            (_, Builtin::Lib(LibFn::StrToUtf8)) => self.con(&mut hir.types, TypeDef::Bytes),
            (_, Builtin::Lib(LibFn::DurationAsMillis | LibFn::InstantAsMillis)) => {
                self.con(&mut hir.types, TypeDef::Int(IntTy::I64))
            }
            (_, Builtin::Lib(LibFn::StrParseI64)) => {
                let i = self.con(&mut hir.types, TypeDef::Int(IntTy::I64));
                self.bound(Shape::Option(i))
            }
            (_, Builtin::Lib(LibFn::BytesFromUtf8)) => {
                let st = self.con(&mut hir.types, TypeDef::Str);
                self.bound(Shape::Option(st))
            }
            (_, Builtin::Lib(f @ (LibFn::BytesUvarintAt | LibFn::BytesVarintAt))) => {
                if let Some(p) = a0 {
                    self.unify(&hir.types, p, u64_t, span);
                }
                let v = if f == LibFn::BytesUvarintAt {
                    u64_t
                } else {
                    self.con(&mut hir.types, TypeDef::Int(IntTy::I64))
                };
                let pair = self.bound(Shape::Tuple(vec![v, u64_t]));
                self.bound(Shape::Option(pair))
            }
            (_, Builtin::Lib(LibFn::BytesRead(it))) => {
                if let Some(p) = a0 {
                    self.unify(&hir.types, p, u64_t, span);
                }
                let t = self.con(&mut hir.types, TypeDef::Int(it));
                self.bound(Shape::Option(t))
            }
            (_, Builtin::Lib(LibFn::BytesPut(it))) => {
                if let Some(p) = a0 {
                    self.unify(&hir.types, p, u64_t, span);
                }
                if let Some(x) = a1 {
                    let t = self.con(&mut hir.types, TypeDef::Int(it));
                    self.unify(&hir.types, x, t, span);
                }
                self.bound(Shape::Option(recv))
            }
            (_, other) => {
                self.bugs.push(internal_error!("{other:?} dispatched on a plain value"));
                return Some(true);
            }
        };
        self.unify(&hir.types, res, result, span);
        if let Some(m) = self.methods.get_mut(slot) {
            *m = Some(MethodRes {
                target: MethodTarget::Plain(target),
                lifts: vec![None; args.len()],
            });
        }
        Some(true)
    }

    /// A closure's parameters against the values a combinator passes it.
    fn unify_params(&mut self, hir: &Hir, params: &[T], values: &[T], span: Span) {
        for (p, v) in params.iter().zip(values) {
            self.unify(&hir.types, *p, *v, span);
        }
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
        // The types of variables inside the blocks that refine them, in the walk's order of blocks.
        let mut handlers = std::mem::take(&mut hir.handlers);
        let mut next = 0;
        for h in &mut handlers {
            self.refined_blocks(hir, &mut h.stmts, &mut next);
        }
        hir.handlers = handlers;
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
        // A function's parameter has exactly its declared type. Unification may have met `Node` with `Node<R>` (an
        // equality with a role-typed value), which is sound for a rule variable after the join but not for a
        // parameter, whose callers pass any value of the declared type.
        for f in &hir.fns {
            for (v, ty) in &f.params {
                if let Some(slot) = var_types
                    .get_mut(f.scope.index())
                    .and_then(|tys| tys.get_mut(v.index()))
                {
                    *slot = *ty;
                }
            }
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
    for g in &hir.guards {
        check_lattice_keys(hir, &[&g.body], diags);
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

/// A fact's row is folded at compile time. Literals, constants and constructors over them fold; an operator or a
/// function call in a fact is not evaluated by this build (LANG-010), and says so rather than failing in lowering.
fn constant_facts(hir: &Hir, diags: &mut Diagnostics) {
    for f in &hir.facts {
        for e in &f.row {
            if crate::lower::try_const(hir, e).is_none() {
                diags.push(
                    Diagnostic::not_implemented(
                        blossom_base::FeatureId("LANG-010"),
                        "a computed value in a fact (an operator or a function call): a fact's values must be \
                         literals, constants or constructors over them",
                        "the Blossom frontend (slice 6)",
                    )
                    .with_primary(e.span),
                );
            }
        }
    }
}
