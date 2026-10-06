//! Rule drafts, expressions, patterns and atom arguments.
//!
//! A [`Draft`] is one IR rule under construction: its variables (each HIR variable of one scope maps to one IR
//! variable) and its literals. Rules are built from drafts only when complete, because building a literal may need
//! the builder (constants) while a `RuleBuilder` holds it.

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::{InternalError, RelId, RoleId, RuleId, RuleLabel, Span, Symbol, TypeId, VarId, internal_error};
use blossom_ir::build::IrBuilder;
use blossom_ir::core::{self as ir, Atom, Expr, Head, Literal, Pattern, Term};
use blossom_value::{TypeDef, Value};

use super::{Lowerer, ir as ir_err};
use crate::ast::{BinOp, PrefixOp};
use crate::hir::*;
use crate::resolve::int_value;

/// One IR rule under construction.
#[derive(Clone, Debug)]
pub(crate) struct Draft {
    /// The HIR scope whose variables this draft maps.
    pub scope: ScopeId,
    pub vars: Vec<(Symbol, TypeId)>,
    pub lits: Vec<Literal>,
    pub map: BTreeMap<HVarId, VarId>,
    /// Variables whose type in this rule is not their scope's: inside a handler block, what its condition refines.
    pub refined: BTreeMap<HVarId, TypeId>,
    names: BTreeSet<Symbol>,
    fresh: u32,
}

impl Draft {
    pub fn new(scope: ScopeId) -> Draft {
        Draft {
            scope,
            vars: Vec::new(),
            lits: Vec::new(),
            map: BTreeMap::new(),
            refined: BTreeMap::new(),
            names: BTreeSet::new(),
            fresh: 0,
        }
    }

    /// A draft for a rule inside handler blocks, whose variables `refined` have those types.
    pub fn refined(scope: ScopeId, refined: BTreeMap<HVarId, TypeId>) -> Draft {
        Draft {
            refined,
            ..Draft::new(scope)
        }
    }

    fn declare(&mut self, name: Symbol, ty: TypeId) -> VarId {
        let mut n = name;
        let mut k = 1;
        while self.names.contains(&n) {
            n = Symbol::intern(&format!("{}'{k}", name.as_str()));
            k += 1;
        }
        self.names.insert(n);
        self.vars.push((n, ty));
        VarId::from_raw((self.vars.len() - 1) as u32)
    }

    /// A fresh variable (for a computed argument or a destination).
    pub fn fresh(&mut self, ty: TypeId) -> VarId {
        let name = Symbol::intern(&format!("${}", self.fresh));
        self.fresh += 1;
        self.declare(name, ty)
    }

    /// The IR variable of a HIR variable, declared on first use.
    pub fn var(&mut self, hir: &Hir, v: HVarId) -> Result<VarId, InternalError> {
        if let Some(id) = self.map.get(&v) {
            return Ok(*id);
        }
        let sc = hir.scope(self.scope)?;
        let name = sc
            .vars
            .get(v.index())
            .map(|x| x.name)
            .ok_or_else(|| internal_error!("variable {v:?} is not in its scope"))?;
        let ty = match self.refined.get(&v) {
            Some(ty) => *ty,
            None => hir
                .var_types
                .get(self.scope.index())
                .and_then(|t| t.get(v.index()))
                .copied()
                .ok_or_else(|| internal_error!("variable {} has no type", name.as_str()))?,
        };
        let id = self.declare(name, ty);
        self.map.insert(v, id);
        Ok(id)
    }

    pub fn var_ty(&self, v: VarId) -> Option<TypeId> {
        self.vars.get(v.index()).map(|(_, t)| *t)
    }

    /// Builds the rule.
    pub fn build(
        self,
        b: &mut IrBuilder,
        kind: ir::RuleKind,
        label: RuleLabel,
        span: Span,
        head: Head,
        role: Option<HRoleId>,
    ) -> Result<RuleId, InternalError> {
        let mut rb = b.rule(kind, label, span);
        for (name, ty) in &self.vars {
            rb.var(*name, *ty).map_err(ir_err)?;
        }
        for l in self.lits {
            rb.lit(l);
        }
        rb.head(head, role.map(|r| RoleId::from_raw(r.0))).map_err(ir_err)
    }
}

/// A HIR expression's type.
pub(crate) fn ty_of(e: &HExpr) -> Result<TypeId, InternalError> {
    e.ty.ok_or_else(|| internal_error!("an expression has no type after type checking"))
}

/// The value of a constant expression, if it is one.
pub(crate) fn try_const(hir: &Hir, e: &HExpr) -> Option<Value> {
    match &e.kind {
        HExprKind::Value(v, _) => Some(v.clone()),
        HExprKind::IntLit(n, neg) | HExprKind::TypedInt(n, _, neg) => {
            let ity = match e.ty.and_then(|t| hir.types.get(t)) {
                Some(TypeDef::Int(t)) => *t,
                _ => return None,
            };
            int_value(*n, ity, *neg)
        }
        HExprKind::Tuple(es) => {
            let vs: Option<Vec<Value>> = es.iter().map(|x| try_const(hir, x)).collect();
            Some(Value::Tuple(vs?.into()))
        }
        HExprKind::Variant { ty, variant, fields } => {
            let vs: Option<Vec<Value>> = fields.iter().map(|x| try_const(hir, x)).collect();
            let vs = vs?;
            let is_option = match ty {
                TypeRef::Known(t) => matches!(hir.types.get(*t), Some(TypeDef::Option(_))),
                TypeRef::Option => true,
            };
            if is_option {
                return Some(match (variant, vs.into_iter().next()) {
                    (1, Some(v)) => Value::some(v),
                    _ => Value::none(),
                });
            }
            Some(Value::Enum {
                variant: *variant,
                fields: vs.into(),
            })
        }
        HExprKind::Struct { fields, .. } => {
            let vs: Option<Vec<Value>> = fields.iter().map(|x| try_const(hir, x)).collect();
            Some(Value::Struct(vs?.into()))
        }
        HExprKind::Collection { kind, elems } => {
            let vs: Option<Vec<Value>> = elems.iter().map(|x| try_const(hir, x)).collect();
            let vs = vs?;
            Some(match kind {
                CollectionKind::Vec => Value::Vec(vs.into()),
                CollectionKind::Set => Value::Set(std::sync::Arc::new(vs.into_iter().collect())),
                CollectionKind::Map => {
                    let mut m = BTreeMap::new();
                    let mut it = vs.into_iter();
                    while let (Some(k), Some(v)) = (it.next(), it.next()) {
                        m.insert(k, v);
                    }
                    Value::Map(std::sync::Arc::new(m))
                }
            })
        }
        HExprKind::Lift { expr, lattice } => {
            let v = try_const(hir, expr)?;
            let kind = super::lattice::kind_of_type(&hir.types, &hir.lattices, *lattice)?;
            kind.eval(blossom_lattice::Op::Lift, &[v]).ok()
        }
        HExprKind::LatCtor { bot, args, .. } => {
            let kind = super::lattice::kind_of_type(&hir.types, &hir.lattices, e.ty?)?;
            if *bot {
                return Some(Value::Lattice(kind.bottom()));
            }
            let vs: Option<Vec<Value>> = args.iter().map(|x| try_const(hir, x)).collect();
            kind.eval(blossom_lattice::Op::Of, &vs?).ok()
        }
        HExprKind::Cast { expr, .. } if matches!(expr.kind, HExprKind::IntLit(..)) => {
            let HExprKind::IntLit(n, neg) = expr.kind else {
                return None;
            };
            let ity = match e.ty.and_then(|t| hir.types.get(t)) {
                Some(TypeDef::Int(t)) => *t,
                _ => return None,
            };
            int_value(n, ity, neg)
        }
        _ => None,
    }
}

/// The type of a tuple of `tys`: `()` when there are none (the empty tuple is written `()`).
pub(crate) fn tuple_type(b: &mut IrBuilder, tys: Vec<TypeId>) -> Result<TypeId, InternalError> {
    let def = if tys.is_empty() {
        TypeDef::Unit
    } else {
        TypeDef::Tuple(tys)
    };
    b.types()
        .insert(def)
        .map_err(|e| internal_error!("interning a type: {e}"))
}

/// A tuple of `fields` of type `ty` (from [`tuple_type`]): the constant `()` when there are none.
pub(crate) fn tuple_expr(unit: blossom_base::ConstId, ty: TypeId, fields: Vec<Expr>) -> Expr {
    if fields.is_empty() {
        Expr::Term(Term::Const(unit))
    } else {
        Expr::Construct {
            ty,
            variant: None,
            fields,
        }
    }
}

/// `ty` with every `Node<R>` inside it widened to `Node`, interned in `types`.
pub(crate) fn erase_roles(types: &mut blossom_value::TypeTable, ty: TypeId) -> Result<TypeId, InternalError> {
    let def = match types.get(ty).cloned() {
        Some(TypeDef::Node(Some(_))) => TypeDef::Node(None),
        Some(TypeDef::Tuple(ts)) => {
            let mut out = Vec::new();
            for t in ts {
                out.push(erase_roles(types, t)?);
            }
            TypeDef::Tuple(out)
        }
        Some(TypeDef::Option(t)) => TypeDef::Option(erase_roles(types, t)?),
        Some(TypeDef::Vec(t)) => TypeDef::Vec(erase_roles(types, t)?),
        Some(TypeDef::Set(t)) => TypeDef::Set(erase_roles(types, t)?),
        Some(TypeDef::Map(k, v)) => {
            let k = erase_roles(types, k)?;
            TypeDef::Map(k, erase_roles(types, v)?)
        }
        _ => return Ok(ty),
    };
    types.insert(def).map_err(|e| internal_error!("interning a type: {e}"))
}

/// The value of an expression that must be constant (a fact's row).
pub(crate) fn const_eval(hir: &Hir, e: &HExpr) -> Result<Value, InternalError> {
    try_const(hir, e).ok_or_else(|| internal_error!("a fact value is not a constant"))
}

fn bin_op(op: BinOp) -> Result<ir::BinOp, InternalError> {
    Ok(match op {
        BinOp::Add => ir::BinOp::Add,
        BinOp::Sub => ir::BinOp::Sub,
        BinOp::Mul => ir::BinOp::Mul,
        BinOp::Div => ir::BinOp::Div,
        BinOp::Rem => ir::BinOp::Rem,
        BinOp::Eq => ir::BinOp::Eq,
        BinOp::Ne => ir::BinOp::Ne,
        BinOp::Lt => ir::BinOp::Lt,
        BinOp::Le => ir::BinOp::Le,
        BinOp::Gt => ir::BinOp::Gt,
        BinOp::Ge => ir::BinOp::Ge,
        BinOp::And => ir::BinOp::And,
        BinOp::Or => ir::BinOp::Or,
        BinOp::BitAnd => ir::BinOp::BitAnd,
        BinOp::BitOr => ir::BinOp::BitOr,
        BinOp::BitXor => ir::BinOp::BitXor,
        BinOp::Shl => ir::BinOp::Shl,
        BinOp::Shr => ir::BinOp::Shr,
        other => return Err(internal_error!("operator {other:?} reached lowering")),
    })
}

/// Whether values of a type order numerically within their kind (the IR's `Lt`): integers, durations, instants,
/// strings, bytes and nodes. Everything else orders by the canonical order.
fn scalar_ordered(def: Option<&TypeDef>) -> bool {
    matches!(
        def,
        Some(TypeDef::Int(_) | TypeDef::Duration | TypeDef::Instant | TypeDef::Str | TypeDef::Bytes | TypeDef::Node(_))
    )
}

impl Lowerer<'_> {
    pub fn konst(&mut self, v: Value) -> Result<Term, InternalError> {
        Ok(Term::Const(self.b.intern_const(v).map_err(ir_err)?))
    }

    /// A constant of type `ty` as an expression. The IR types an untyped constant by the first type its value fits,
    /// which is not `ty` when the value does not decide it (an empty collection, `None`): it is then ascribed.
    fn const_expr(&mut self, v: Value, ty: TypeId) -> Result<Expr, InternalError> {
        let types = self.b.types();
        let decided = types
            .iter()
            .find(|(t, _)| types.check_value(*t, &v).is_ok())
            .map(|(t, _)| t);
        let term = Expr::Term(self.konst(v)?);
        Ok(if decided == Some(ty) {
            term
        } else {
            Expr::Typed {
                ty,
                expr: Box::new(term),
            }
        })
    }

    /// An expression as a term: a variable, a constant, or a fresh variable bound to it.
    pub fn term(&mut self, d: &mut Draft, e: &HExpr) -> Result<Term, InternalError> {
        if let HExprKind::Var(v) = e.kind {
            return Ok(Term::Var(d.var(self.hir, v)?));
        }
        if let Some(v) = try_const(self.hir, e) {
            return self.konst(v);
        }
        let x = self.expr(d, e)?;
        let f = d.fresh(ty_of(e)?);
        d.lits.push(Literal::Bind {
            pat: Pattern::Var(f),
            expr: x,
        });
        Ok(Term::Var(f))
    }

    /// Lowers an expression.
    pub fn expr(&mut self, d: &mut Draft, e: &HExpr) -> Result<Expr, InternalError> {
        if let Some(v) = try_const(self.hir, e) {
            return self.const_expr(v, ty_of(e)?);
        }
        Ok(match &e.kind {
            HExprKind::Var(v) => Expr::Term(Term::Var(d.var(self.hir, *v)?)),
            HExprKind::Value(..) | HExprKind::IntLit(..) | HExprKind::TypedInt(..) => {
                return Err(internal_error!("a literal that does not fold to a value"));
            }
            HExprKind::Binary { op, lhs, rhs } => {
                if let Some(x) = self.lattice_binary(d, *op, lhs, rhs)? {
                    return Ok(x);
                }
                if *op == BinOp::Concat {
                    return Ok(Expr::Call {
                        f: ir::FnRef::Builtin(ir::BuiltinFn::Concat),
                        args: vec![self.expr(d, lhs)?, self.expr(d, rhs)?],
                    });
                }
                // `<` on values other than scalars of one kind is the canonical order (LANGUAGE §5.5). It is total,
                // so `a > b` is `!(a <= b)`: the operands keep their order, and so does their evaluation.
                if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
                    && !scalar_ordered(self.hir.types.get(ty_of(lhs)?))
                {
                    let (l, r) = (Box::new(self.expr(d, lhs)?), Box::new(self.expr(d, rhs)?));
                    let (canon, negated) = match op {
                        BinOp::Lt => (ir::BinOp::CanonLt, false),
                        BinOp::Le => (ir::BinOp::CanonLe, false),
                        BinOp::Gt => (ir::BinOp::CanonLe, true),
                        _ => (ir::BinOp::CanonLt, true),
                    };
                    let cmp = Expr::Binary {
                        op: canon,
                        lhs: l,
                        rhs: r,
                    };
                    return Ok(if negated {
                        Expr::Unary {
                            op: ir::UnOp::Not,
                            arg: Box::new(cmp),
                        }
                    } else {
                        cmp
                    });
                }
                Expr::Binary {
                    op: bin_op(*op)?,
                    lhs: Box::new(self.expr(d, lhs)?),
                    rhs: Box::new(self.expr(d, rhs)?),
                }
            }
            HExprKind::Prefix { op, arg } => Expr::Unary {
                op: match op {
                    PrefixOp::Not => ir::UnOp::Not,
                    PrefixOp::Neg => ir::UnOp::Neg,
                    PrefixOp::BitNot => ir::UnOp::BitNot,
                },
                arg: Box::new(self.expr(d, arg)?),
            },
            HExprKind::Tuple(es) => {
                let mut fields = Vec::new();
                for x in es {
                    fields.push(self.expr(d, x)?);
                }
                Expr::Construct {
                    ty: ty_of(e)?,
                    variant: None,
                    fields,
                }
            }
            HExprKind::Variant { ty, variant, fields } => {
                let TypeRef::Known(t) = ty else {
                    return Err(internal_error!("an `Option` whose type was not inferred"));
                };
                let mut fs = Vec::new();
                for x in fields {
                    fs.push(self.expr(d, x)?);
                }
                Expr::Construct {
                    ty: *t,
                    variant: Some(*variant),
                    fields: fs,
                }
            }
            HExprKind::Struct { ty, fields, .. } => {
                let mut fs = Vec::new();
                for x in fields {
                    fs.push(self.expr(d, x)?);
                }
                Expr::Construct {
                    ty: *ty,
                    variant: None,
                    fields: fs,
                }
            }
            HExprKind::TupleIndex { base, index } => Expr::Field {
                base: Box::new(self.expr(d, base)?),
                index: *index,
            },
            HExprKind::Field { base, index, name } => Expr::Field {
                base: Box::new(self.expr(d, base)?),
                index: index.ok_or_else(|| internal_error!("field `{}` was not resolved", name.as_str()))?,
            },
            HExprKind::If { cond, then, els } => Expr::If {
                cond: Box::new(self.expr(d, cond)?),
                then: Box::new(self.expr(d, then)?),
                els: Box::new(self.expr(d, els)?),
            },
            HExprKind::Match { scrut, arms } => {
                let s = self.expr(d, scrut)?;
                let mut out = Vec::new();
                for (p, g, body) in arms {
                    let mut post = Vec::new();
                    let pat = self.pattern(d, p, &mut post)?;
                    let g = match g {
                        Some(g) => Some(self.expr(d, g)?),
                        None => None,
                    };
                    // A pattern's equality tests (a bound variable or a computed value in it) run after it binds,
                    // before the arm's own guard.
                    let mut tests = Vec::new();
                    for l in post {
                        match l {
                            Literal::Guard(t) => tests.push(t),
                            other => return Err(internal_error!("a match pattern test {other:?} is not a guard")),
                        }
                    }
                    let g = tests.into_iter().chain(g).reduce(|a, b| Expr::Binary {
                        op: ir::BinOp::And,
                        lhs: Box::new(a),
                        rhs: Box::new(b),
                    });
                    out.push((pat, g, self.expr(d, body)?));
                }
                Expr::Match {
                    scrut: Box::new(s),
                    arms: out,
                }
            }
            HExprKind::Ascribe { expr, .. } => self.expr(d, expr)?,
            HExprKind::Cast { expr, ty } => {
                if expr.ty == Some(*ty) {
                    self.expr(d, expr)?
                } else {
                    // Type checking admits casts between integer types and `f64` only.
                    let f = match self.b.types().get(*ty) {
                        Some(TypeDef::Int(t)) => ir::BuiltinFn::IntCast(*t),
                        Some(TypeDef::F64) => ir::BuiltinFn::FloatCast,
                        other => return Err(internal_error!("a cast to {other:?} reached lowering")),
                    };
                    Expr::Call {
                        f: ir::FnRef::Builtin(f),
                        args: vec![self.expr(d, expr)?],
                    }
                }
            }
            HExprKind::SelfNode => Expr::Scalar(ir::BuiltinScalar::SelfNode),
            HExprKind::Now => Expr::Scalar(ir::BuiltinScalar::Now),
            HExprKind::Tick => Expr::Scalar(ir::BuiltinScalar::Tick),
            HExprKind::Lookup { rel, key } => {
                // `V = r[k̄]` as its own literal (LANGUAGE §9.9): the cell's value, ⊥ if absent.
                let mut keys = Vec::new();
                for k in key {
                    keys.push(self.term(d, k)?);
                }
                let v = d.fresh(ty_of(e)?);
                d.lits.push(Literal::Lookup {
                    var: v,
                    rel: self.rel(*rel)?,
                    key: keys,
                });
                Expr::Term(Term::Var(v))
            }
            HExprKind::In { elem, coll } => {
                let c = self.expr(d, coll)?;
                let x = self.expr(d, elem)?;
                match self.lattice_id(ty_of(coll)?) {
                    Some(lattice) => self.lat_op(lattice, blossom_lattice::Op::Contains, vec![c, x]),
                    None => Expr::Call {
                        f: ir::FnRef::Builtin(ir::BuiltinFn::Contains),
                        args: vec![c, x],
                    },
                }
            }
            HExprKind::Collection { kind, elems } => {
                let mut xs = Vec::new();
                if *kind == CollectionKind::Map {
                    // The IR's map literal holds (key, value) pairs, each typed as the map's entry (a key or value
                    // may be narrower in its roles than the map's, as `self` is).
                    let Some(TypeDef::Map(kt, vt)) = self.b.types().get(ty_of(e)?).cloned() else {
                        return Err(internal_error!("a map literal whose type is not a map"));
                    };
                    let ty = self
                        .b
                        .types()
                        .insert(TypeDef::Tuple(vec![kt, vt]))
                        .map_err(|e| internal_error!("interning a type: {e}"))?;
                    for pair in elems.chunks(2) {
                        let [k, v] = pair else {
                            return Err(internal_error!("a map literal with a dangling key"));
                        };
                        xs.push(Expr::Construct {
                            ty,
                            variant: None,
                            fields: vec![self.expr(d, k)?, self.expr(d, v)?],
                        });
                    }
                } else {
                    for x in elems {
                        // The IR types the literal by its widest element (`[self, d]`, where `self` is a `Node`):
                        // intern each candidate, and its role-free form.
                        let t = ty_of(x)?;
                        let wide = erase_roles(self.b.types(), t)?;
                        let mut candidates = vec![t, wide];
                        // The IR types `self` as a `Node<R>` of the rule's role R (LANGUAGE §6.10), which is not
                        // known here: intern the literal's type for every role.
                        if matches!(x.kind, HExprKind::SelfNode) {
                            for r in 0..self.hir.roles.len() {
                                let role =
                                    RoleId::from_raw(u32::try_from(r).map_err(|_| internal_error!("too many roles"))?);
                                candidates.push(
                                    self.b
                                        .types()
                                        .insert(TypeDef::Node(Some(role)))
                                        .map_err(|e| internal_error!("interning a type: {e}"))?,
                                );
                            }
                        }
                        for t in candidates {
                            let def = if *kind == CollectionKind::Vec {
                                TypeDef::Vec(t)
                            } else {
                                TypeDef::Set(t)
                            };
                            self.b
                                .types()
                                .insert(def)
                                .map_err(|e| internal_error!("interning a type: {e}"))?;
                        }
                        xs.push(self.expr(d, x)?);
                    }
                }
                Expr::Collection {
                    kind: match kind {
                        CollectionKind::Vec => ir::CollKind::Vec,
                        CollectionKind::Set => ir::CollKind::Set,
                        CollectionKind::Map => ir::CollKind::Map,
                    },
                    elems: xs,
                }
            }
            HExprKind::LatCtor { bot: true, .. } => {
                let ty = ty_of(e)?;
                let kind = self
                    .lattice_kind(ty)
                    .ok_or_else(|| internal_error!("`bot()` of a non-lattice type"))?;
                return Ok(Expr::Term(self.konst(Value::Lattice(kind.bottom()))?));
            }
            HExprKind::LatCtor { args, .. } => {
                let lattice = self
                    .lattice_id(ty_of(e)?)
                    .ok_or_else(|| internal_error!("a lattice constructor of a non-lattice type"))?;
                let mut xs = Vec::new();
                for x in args {
                    xs.push(self.expr(d, x)?);
                }
                self.lat_op(lattice, blossom_lattice::Op::Of, xs)
            }
            HExprKind::Lift { expr, lattice } => {
                let id = self
                    .lattice_id(*lattice)
                    .ok_or_else(|| internal_error!("a lift into a non-lattice type"))?;
                // A map whose values already are lattice values lifts entry by entry.
                let entries = match self.b.types().get(ty_of(expr)?) {
                    Some(TypeDef::Map(_, v)) => {
                        let v = *v;
                        self.lattice_id(v).is_some()
                    }
                    _ => false,
                };
                let x = self.expr(d, expr)?;
                let op = if entries {
                    blossom_lattice::Op::LiftEntries
                } else {
                    blossom_lattice::Op::Lift
                };
                self.lat_op(id, op, vec![x])
            }
            HExprKind::LatOp { lattice, op, args } => {
                let id = self
                    .lattice_id(*lattice)
                    .ok_or_else(|| internal_error!("a lattice operation on a non-lattice type"))?;
                let mut xs = Vec::new();
                for x in args {
                    xs.push(self.expr(d, x)?);
                }
                self.lat_op(id, *op, xs)
            }
            HExprKind::Method { name, .. } => {
                return Err(internal_error!(
                    "the method `{}` was not resolved by type checking",
                    name.as_str()
                ));
            }
            HExprKind::Call { f, args } => {
                let mut xs = Vec::new();
                for a in args {
                    xs.push(self.expr(d, a)?);
                }
                let id = self
                    .fns
                    .get(f.index())
                    .copied()
                    .ok_or_else(|| internal_error!("call of function {f:?}, which was not lowered"))?;
                Expr::Call {
                    f: ir::FnRef::Fn(id),
                    args: xs,
                }
            }
            HExprKind::Let { pat, value, body, .. } => {
                let v = self.expr(d, value)?;
                let mut post = Vec::new();
                let p = self.pattern(d, pat, &mut post)?;
                if !post.is_empty() {
                    return Err(internal_error!("a `let` pattern with a computed test"));
                }
                Expr::Let {
                    pat: p,
                    value: Box::new(v),
                    body: Box::new(self.expr(d, body)?),
                }
            }
            HExprKind::CallParam { .. } | HExprKind::GenericCall { .. } => {
                return Err(internal_error!(
                    "a generic function's template call reached lowering (templates are never lowered)"
                ));
            }
            HExprKind::Closure { params, body } => {
                let mut vs = Vec::new();
                for v in params {
                    vs.push(d.var(self.hir, *v)?);
                }
                Expr::Closure {
                    params: vs,
                    body: Box::new(self.expr(d, body)?),
                }
            }
            HExprKind::Builtin { f, args } => {
                let mut xs = Vec::new();
                for a in args {
                    xs.push(self.expr(d, a)?);
                }
                let f = match f {
                    Builtin::Len => ir::BuiltinFn::Len,
                    Builtin::Contains => ir::BuiltinFn::Contains,
                    Builtin::RoleSize(r) => ir::BuiltinFn::Size {
                        role: RoleId::from_raw(r.0),
                    },
                    Builtin::RandRange => ir::BuiltinFn::RandRange,
                    Builtin::Rand => ir::BuiltinFn::Rand,
                    Builtin::RandFloat => ir::BuiltinFn::RandFloat,
                    Builtin::Hash64 => ir::BuiltinFn::Hash64,
                    Builtin::Error => ir::BuiltinFn::Error { ty: ty_of(e)? },
                    Builtin::Majority(r) => ir::BuiltinFn::Majority {
                        domain: ir::MajorityDomain::Role(RoleId::from_raw(r.0)),
                    },
                    Builtin::Lib(f) => ir::BuiltinFn::Lib(*f),
                };
                Expr::Call {
                    f: ir::FnRef::Builtin(f),
                    args: xs,
                }
            }
        })
    }

    /// The pure functions (LANGUAGE §16.1), declared in HIR order. Instances of a generic function with the same
    /// template, type arguments and function arguments are one IR function (their bodies are the same), named after
    /// them: `read_array<Item, read_item>`. A body is one expression over the function's own variables, the
    /// parameters first.
    pub fn functions(&mut self) -> Result<(), InternalError> {
        // Every HIR function's IR function first: bodies call instances made after them.
        let mut seen: BTreeMap<(u32, Vec<TypeId>, Vec<HFnId>), blossom_base::FnId> = BTreeMap::new();
        let mut order = Vec::new();
        for (i, f) in self.hir.fns.iter().enumerate() {
            let next = blossom_base::FnId::from_raw(
                u32::try_from(order.len()).map_err(|_| internal_error!("too many functions"))?,
            );
            let id = match &f.scheme {
                Some(s) => {
                    let key = (s.template, s.targs.clone(), s.fn_args.iter().map(|a| a.2).collect());
                    *seen.entry(key).or_insert(next)
                }
                None => next,
            };
            if id == next {
                order.push(i);
            }
            self.fns.push(id);
        }
        for (k, i) in order.into_iter().enumerate() {
            let Some(f) = self.hir.fns.get(i) else {
                return Err(internal_error!("function {i} vanished while lowering"));
            };
            let mut d = Draft::new(f.scope);
            let mut params = Vec::new();
            for (v, ty) in &f.params {
                let id = d.var(self.hir, *v)?;
                let name = d
                    .vars
                    .get(id.index())
                    .map(|x| x.0)
                    .ok_or_else(|| internal_error!("parameter {v:?} of `{}` was not declared", f.name))?;
                params.push((name, *ty));
            }
            let body = match &f.body {
                HFnBody::Expr(e) => ir::FnBody::Ir(self.expr(&mut d, e)?),
                // Host functions are memoized per input per tick (LANGUAGE §16.2).
                HFnBody::Extern(path) => ir::FnBody::Extern {
                    path: path.clone(),
                    memo: true,
                },
            };
            if !d.lits.is_empty() {
                return Err(internal_error!(
                    "the body of `{}` lowered to rule literals (a relation read passed resolution)",
                    f.name
                ));
            }
            let mut vars = blossom_base::IndexVec::new();
            for (name, ty) in &d.vars {
                vars.push(ir::VarDecl {
                    name: *name,
                    ty: *ty,
                    non_bottom: false,
                })
                .map_err(|e| internal_error!("too many variables in `{}`: {e}", f.name))?;
            }
            let n = params.len();
            let name = match &f.scheme {
                Some(s) => self.instance_name(f, s),
                None => f.name.clone(),
            };
            let id = self
                .b
                .declare_fn(ir::FnDecl {
                    id: blossom_base::FnId::from_raw(0),
                    name,
                    params,
                    ret: f.ret,
                    vars,
                    body,
                    props: ir::FnProps {
                        // Parameters are plain values (lattice-typed ones are rejected until function classes,
                        // LANG-182), and every function is monotone in a value under its discrete order.
                        classes: vec![blossom_value::MonoClass::Monotone; n],
                        injective: blossom_value::Claim::Absent,
                        commutative: blossom_value::Claim::Absent,
                        associative: blossom_value::Claim::Absent,
                        idempotent: blossom_value::Claim::Absent,
                        stable_after: None,
                        metered: f.metered,
                    },
                })
                .map_err(ir_err)?;
            if id.index() != k {
                return Err(internal_error!("function `{}` is IR function {id:?}, not {k}", f.name));
            }
            self.fn_origins.push(HFnId(
                u32::try_from(i).map_err(|_| internal_error!("too many functions"))?,
            ));
        }
        Ok(())
    }

    /// A generic function instance's IR name: the generic function's, its last segment followed by the type
    /// arguments and the functions passed, `read_array<Item, read_item>`.
    fn instance_name(&self, f: &HFn, s: &HScheme) -> blossom_base::QualName {
        let mut args: Vec<String> = s
            .targs
            .iter()
            .map(|t| crate::typeck::type_name(&self.hir.types, *t))
            .collect();
        for (_, _, g) in &s.fn_args {
            let n = self.hir.fns.get(g.index()).and_then(|g| g.name.last());
            args.push(n.map(|n| n.as_str().to_string()).unwrap_or_default());
        }
        let mut segs: Vec<Symbol> = f.name.segments().to_vec();
        let last = segs.pop().map(|s| s.as_str().to_string()).unwrap_or_default();
        segs.push(Symbol::intern(&format!("{last}<{}>", args.join(", "))));
        blossom_base::QualName::new(segs)
    }

    /// The IR lattice of a lattice type.
    pub fn lattice_id(&mut self, ty: TypeId) -> Option<blossom_base::LatticeTypeId> {
        match self.b.types().get(ty) {
            Some(TypeDef::Lattice(id)) => Some(*id),
            _ => None,
        }
    }

    /// The built-in lattice of a lattice type.
    pub fn lattice_kind(&mut self, ty: TypeId) -> Option<blossom_lattice::Kind> {
        let id = self.lattice_id(ty)?;
        let ctors: Vec<ir::LatticeCtor> = self.b.program().lattices.iter().map(|l| l.ctor.clone()).collect();
        super::lattice::kind_of(&ctors, ctors.get(id.index())?)
    }

    fn lat_op(&self, lattice: blossom_base::LatticeTypeId, op: blossom_lattice::Op, args: Vec<Expr>) -> Expr {
        Expr::Lattice {
            op: ir::LatOpRef {
                lattice,
                op: Symbol::intern(op.name()),
            },
            args,
        }
    }

    /// A comparison or arithmetic with a lattice operand (type checking admitted only the monotone forms): a
    /// threshold `x >= c` (flipped when the lattice is on the right), `x + c`, `x - c`, or `a + b`.
    fn lattice_binary(
        &mut self,
        d: &mut Draft,
        op: BinOp,
        lhs: &HExpr,
        rhs: &HExpr,
    ) -> Result<Option<Expr>, InternalError> {
        use blossom_lattice::Op as L;
        let (lt, rt) = (self.lattice_id(ty_of(lhs)?), self.lattice_id(ty_of(rhs)?));
        let (lattice, lat_first) = match (lt, rt) {
            (None, None) => return Ok(None),
            (Some(l), _) => (l, true),
            (None, Some(r)) => (r, false),
        };
        let op = if lat_first {
            op
        } else {
            match op {
                BinOp::Lt => BinOp::Gt,
                BinOp::Le => BinOp::Ge,
                BinOp::Gt => BinOp::Lt,
                BinOp::Ge => BinOp::Le,
                other => other,
            }
        };
        let lat_op = match (op, lt.is_some() && rt.is_some()) {
            (BinOp::Ge, false) => L::AtLeast,
            (BinOp::Gt, false) => L::Above,
            (BinOp::Le, false) => L::AtMost,
            (BinOp::Lt, false) => L::Below,
            (BinOp::Add, false) => L::Add,
            (BinOp::Sub, false) if lat_first => L::Sub,
            (BinOp::Add, true) => L::AddLat,
            (other, _) => return Err(internal_error!("the lattice operator {other:?} reached lowering")),
        };
        let a = self.expr(d, lhs)?;
        let b = self.expr(d, rhs)?;
        let args = if lat_first { vec![a, b] } else { vec![b, a] };
        Ok(Some(self.lat_op(lattice, lat_op, args)))
    }

    /// A pattern of a `let`, a generator or a match arm. Computed sub-patterns (an expression over bound variables)
    /// become a fresh variable and an equality guard pushed to `post`.
    pub fn pattern(&mut self, d: &mut Draft, p: &HPat, post: &mut Vec<Literal>) -> Result<Pattern, InternalError> {
        Ok(match p {
            HPat::Var(v, _) => Pattern::Var(d.var(self.hir, *v)?),
            HPat::Wild(_) => Pattern::Wild,
            HPat::Expr(e) => {
                if let Some(v) = try_const(self.hir, e) {
                    Pattern::Const(self.b.intern_const(v).map_err(ir_err)?)
                } else {
                    let f = d.fresh(ty_of(e)?);
                    let x = self.expr(d, e)?;
                    post.push(Literal::Guard(Expr::Binary {
                        op: ir::BinOp::Eq,
                        lhs: Box::new(Expr::Term(Term::Var(f))),
                        rhs: Box::new(x),
                    }));
                    Pattern::Var(f)
                }
            }
            HPat::Tuple(ps, _) => {
                let mut out = Vec::new();
                for x in ps {
                    out.push(self.pattern(d, x, post)?);
                }
                Pattern::Tuple(out)
            }
            HPat::Variant {
                ty, variant, fields, ..
            } => {
                let TypeRef::Known(t) = ty else {
                    return Err(internal_error!("an `Option` pattern whose type was not inferred"));
                };
                let mut out = Vec::new();
                for x in fields {
                    out.push(self.pattern(d, x, post)?);
                }
                Pattern::Variant {
                    ty: *t,
                    number: *variant,
                    fields: out,
                }
            }
        })
    }

    /// An atom over HIR relation `h` with HIR argument patterns (one per HIR column). For a channel, the destination
    /// column is a wildcard unless the column form puts a pattern there. `negated` atoms cannot bind, so computed
    /// arguments are bound before them.
    pub fn atom(
        &mut self,
        d: &mut Draft,
        h: HRelId,
        args: &[HPat],
        from: Option<&HPat>,
        negated: bool,
        span: Span,
    ) -> Result<Atom, InternalError> {
        let rel = self.rel(h)?;
        let n_ir = self.b_rel_arity(rel)?;
        let mut terms = vec![Term::Wild; n_ir];
        for (c, p) in args.iter().enumerate() {
            let col = self.ir_col(h, c)?;
            let col_ty = self.b_col_ty(rel, col)?;
            let t = self.atom_arg(d, p, col_ty, negated)?;
            if let Some(slot) = terms.get_mut(col) {
                *slot = t;
            }
        }
        let sender = match from {
            Some(p) => {
                let ty = match p {
                    HPat::Var(v, _) => {
                        let id = d.var(self.hir, *v)?;
                        d.var_ty(id)
                    }
                    _ => None,
                };
                let ty = match ty {
                    Some(t) => t,
                    None => self
                        .b
                        .types()
                        .insert(TypeDef::Node(None))
                        .map_err(|e| internal_error!("interning a type: {e}"))?,
                };
                Some(self.atom_arg(d, p, ty, negated)?)
            }
            None => None,
        };
        Ok(Atom {
            rel,
            args: terms,
            sender,
            principal: None,
            weight: None,
            spec: None,
            span,
        })
    }

    fn b_rel_arity(&self, rel: RelId) -> Result<usize, InternalError> {
        self.b
            .program()
            .rels
            .get(rel)
            .map(|r| r.schema.cols.len())
            .ok_or_else(|| internal_error!("unknown relation {rel:?}"))
    }

    pub fn b_col_ty(&self, rel: RelId, col: usize) -> Result<TypeId, InternalError> {
        self.b
            .program()
            .rels
            .get(rel)
            .and_then(|r| r.schema.cols.get(col))
            .map(|c| c.ty)
            .ok_or_else(|| internal_error!("unknown column {col} of {rel:?}"))
    }

    /// One atom argument.
    fn atom_arg(&mut self, d: &mut Draft, p: &HPat, col_ty: TypeId, negated: bool) -> Result<Term, InternalError> {
        match p {
            HPat::Var(v, _) => Ok(Term::Var(d.var(self.hir, *v)?)),
            HPat::Wild(_) => Ok(Term::Wild),
            HPat::Expr(e) => {
                if let Some(v) = try_const(self.hir, e) {
                    return self.konst(v);
                }
                if let HExprKind::Var(v) = e.kind {
                    return Ok(Term::Var(d.var(self.hir, v)?));
                }
                let x = self.expr(d, e)?;
                let f = d.fresh(col_ty);
                if negated {
                    d.lits.push(Literal::Bind {
                        pat: Pattern::Var(f),
                        expr: x,
                    });
                } else {
                    d.lits.push(Literal::Guard(Expr::Binary {
                        op: ir::BinOp::Eq,
                        lhs: Box::new(Expr::Term(Term::Var(f))),
                        rhs: Box::new(x),
                    }));
                }
                Ok(Term::Var(f))
            }
            HPat::Tuple(..) | HPat::Variant { .. } => {
                if negated {
                    return Err(internal_error!(
                        "a destructuring pattern in a negated atom reached lowering"
                    ));
                }
                let f = d.fresh(col_ty);
                let mut post = Vec::new();
                let pat = self.pattern(d, p, &mut post)?;
                d.lits.push(Literal::Bind {
                    pat,
                    expr: Expr::Term(Term::Var(f)),
                });
                d.lits.extend(post);
                Ok(Term::Var(f))
            }
        }
    }
}
