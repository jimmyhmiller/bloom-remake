//! Generic functions (LANGUAGE §16.1, EXTENSIONS 2.2).
//!
//! A function with type parameters or function parameters is a *template*. Its body is resolved once, in its own
//! scope, into HIR in which a call of a function parameter is a [`HExprKind::CallParam`] and a call of a generic
//! function a [`HExprKind::GenericCall`]. Each call of a template from a function or rule that is not itself a
//! template makes an *instance*: a copy of the template's body in a fresh scope, with every function parameter
//! replaced by the named function the call passes and every generic call instantiated in turn. An instance is an
//! ordinary [`HFn`] whose [`HScheme`] gives its signature over the type parameters; type checking infers them at the
//! instance's one call and writes the concrete types in. Lowering merges instances with the same template, function
//! arguments and types into one IR function, so the IR stays first-order and monomorphic.
//!
//! Templates are not type checked themselves, only their instances; an error in a template's body that depends on
//! the types it is used with is reported at the instance's call. A template that is never called is name-resolved
//! but not type checked.

use std::collections::BTreeSet;

use blossom_base::{QualName, Span, Symbol, code};
use blossom_value::TypeDef;

use super::{Resolver, ScopeIdx};
use crate::ast::{self, Ident};
use crate::hir::*;

/// The most generic function instances a program may make (BLS0220).
const MAX_INSTANCES: usize = 10_000;

/// A generic function's template.
pub(crate) struct Template<'t> {
    pub item: &'t ast::FnItem,
    /// The module scope it is declared in, which its body is resolved in.
    pub ms: ScopeIdx,
    pub name: QualName,
    pub tparams: Vec<Symbol>,
    /// The parameters in declaration order.
    pub params: Vec<TParam>,
    pub ret: HTy,
    pub state: TemplateState,
}

pub(crate) enum TParam {
    Value { name: Ident, ty: HTy },
    Fn { name: Ident, ty: HFnTy },
}

pub(crate) enum TemplateState {
    /// Declared; its body is resolved once every function of its module is declared.
    Declared,
    /// Its body failed to resolve (and was reported).
    Failed,
    /// The body's variables (the value parameters first), the module path for rule ids, and the body.
    Resolved {
        vars: Vec<HVar>,
        module: QualName,
        body: HExpr,
    },
}

/// Whether a function item is a template: it has type parameters or a function parameter.
pub(crate) fn is_template(f: &ast::FnItem) -> bool {
    !f.generics.is_empty() || f.params.iter().any(|(_, t)| matches!(t, ast::Type::Fn { .. }))
}

impl<'t> Resolver<'t, '_> {
    /// Declares a template: its type parameters, its value and function parameters and its result type, over the
    /// type parameters. Its body is resolved later ([`Resolver::resolve_template`]).
    pub(super) fn declare_template(&mut self, s: ScopeIdx, f: &'t ast::FnItem) -> Option<usize> {
        let name = f.name;
        if super::body::BUILTIN_FNS.contains(&name.as_str()) {
            self.error(
                code!("BLS0201"),
                name.span,
                format!("`{}` is a built-in function; name this one differently", name.as_str()),
            );
            return None;
        }
        let sc = self.scope(s);
        if sc.fns.contains_key(&name.name)
            || sc.generic_fns.contains_key(&name.name)
            || sc.rels.contains_key(&name.name)
            || sc.instances.contains_key(&name.name)
        {
            self.error(
                code!("BLS0201"),
                name.span,
                format!("`{}` is declared twice", name.as_str()),
            );
            return None;
        }
        let mut tparams: Vec<Symbol> = Vec::new();
        for g in &f.generics {
            if tparams.contains(&g.name.name) {
                self.error(
                    code!("BLS0201"),
                    g.name.span,
                    format!("type parameter `{}` is declared twice", g.name.as_str()),
                );
                return None;
            }
            tparams.push(g.name.name);
        }
        let mut params = Vec::new();
        let mut seen = BTreeSet::new();
        let mut ok = true;
        for (p, ty) in &f.params {
            if !seen.insert(p.name) {
                self.error(
                    code!("BLS0201"),
                    p.span,
                    format!("parameter `{}` is declared twice", p.as_str()),
                );
                ok = false;
                continue;
            }
            match ty {
                ast::Type::Fn { .. }
                    if super::body::BUILTIN_FNS.contains(&p.as_str())
                        || matches!(p.as_str(), "Some" | "None")
                        || self.scope(s).rels.contains_key(&p.name) =>
                {
                    self.error(
                        code!("BLS0201"),
                        p.span,
                        format!(
                            "function parameter `{}` would be shadowed by the built-in or relation of that name",
                            p.as_str()
                        ),
                    );
                    ok = false;
                }
                ast::Type::Fn { params: ps, ret, .. } => {
                    let mut fps = Vec::new();
                    for t in ps {
                        match self.scheme_type(s, t, &tparams) {
                            Some(h) => fps.push(h),
                            None => ok = false,
                        }
                    }
                    match self.scheme_type(s, ret, &tparams) {
                        Some(r) => params.push(TParam::Fn {
                            name: *p,
                            ty: HFnTy { params: fps, ret: r },
                        }),
                        None => ok = false,
                    }
                }
                _ => match self.scheme_type(s, ty, &tparams) {
                    Some(h) => params.push(TParam::Value { name: *p, ty: h }),
                    None => ok = false,
                },
            }
        }
        let ret = self.scheme_type(s, &f.ret, &tparams);
        let (Some(ret), true) = (ret, ok) else {
            return None;
        };
        let id = self.templates.len();
        let qual = self.qual(s, name.name);
        self.templates.push(Template {
            item: f,
            ms: s,
            name: qual,
            tparams,
            params,
            ret,
            state: TemplateState::Declared,
        });
        self.scope_mut(s).generic_fns.insert(name.name, id);
        Some(id)
    }

    /// A signature type over the type parameters `tparams`. A type parameter may occur inside tuples, `Option`,
    /// `Vec`, `Set` and `Map`; a type without one is resolved as usual.
    fn scheme_type(&mut self, s: ScopeIdx, ty: &ast::Type, tparams: &[Symbol]) -> Option<HTy> {
        if !mentions(ty, tparams) {
            let t = self.resolve_type(s, ty)?;
            if holds_lattice(&self.hir.types, t) {
                self.unsupported(
                    "LANG-182",
                    "lattice-typed function parameters and results (they need a monotonicity class, `monotone fn` …)",
                    ty.span(),
                );
                return None;
            }
            return Some(HTy::Con(t));
        }
        match ty {
            ast::Type::Named { path, args, span } => {
                let [name] = path.as_slice() else {
                    self.unsupported("LANG-001", "qualified type paths", *span);
                    return None;
                };
                if let Some(i) = tparams.iter().position(|t| *t == name.name) {
                    if !args.is_empty() {
                        self.error(
                            code!("BLS0301"),
                            *span,
                            format!("type parameter `{}` takes no type arguments", name.as_str()),
                        );
                        return None;
                    }
                    return Some(HTy::Param(u32::try_from(i).ok()?));
                }
                let mut inner = Vec::new();
                for a in args {
                    inner.push(self.scheme_type(s, a, tparams)?);
                }
                let one = |inner: &mut Vec<HTy>| inner.pop().map(Box::new);
                match (name.as_str(), inner.len()) {
                    ("Option", 1) => Some(HTy::Option(one(&mut inner)?)),
                    ("Vec", 1) => Some(HTy::Vec(one(&mut inner)?)),
                    ("Set", 1) => Some(HTy::Set(one(&mut inner)?)),
                    ("Map", 2) => {
                        let v = one(&mut inner)?;
                        let k = one(&mut inner)?;
                        Some(HTy::Map(k, v))
                    }
                    ("Option" | "Vec" | "Set" | "Map", n) => {
                        self.error(
                            code!("BLS0301"),
                            *span,
                            format!("`{}` given {n} type argument(s)", name.as_str()),
                        );
                        None
                    }
                    (other, _) => {
                        self.unsupported(
                            "LANG-180",
                            &format!(
                                "a type parameter inside `{other}<…>` (tuples, `Option`, `Vec`, `Set` and `Map` may \
                                 hold one)"
                            ),
                            *span,
                        );
                        None
                    }
                }
            }
            ast::Type::Tuple { elems, .. } => {
                if let [one] = elems.as_slice() {
                    return self.scheme_type(s, one, tparams);
                }
                let mut out = Vec::new();
                for e in elems {
                    out.push(self.scheme_type(s, e, tparams)?);
                }
                Some(HTy::Tuple(out))
            }
            ast::Type::Unsafe { span, .. } => {
                self.error(code!("BLS0300"), *span, "`unsafe` applies only to `DomPair<K, V>`");
                None
            }
            ast::Type::Fn { span, .. } => {
                self.error(
                    code!("BLS0219"),
                    *span,
                    "a function type is the type of a function's parameter only (LANGUAGE §16.1)",
                );
                None
            }
        }
    }

    /// The index among the function parameters of the template being resolved, and the type, of the function
    /// parameter `name`.
    pub(super) fn fn_param(&self, template: Option<usize>, name: Symbol) -> Option<(u32, HFnTy)> {
        let t = self.templates.get(template?)?;
        t.params
            .iter()
            .filter_map(|p| match p {
                TParam::Fn { name, ty } => Some((name.name, ty)),
                TParam::Value { .. } => None,
            })
            .enumerate()
            .find(|(_, (n, _))| *n == name)
            .and_then(|(i, (_, ty))| Some((u32::try_from(i).ok()?, ty.clone())))
    }

    /// Instantiates template `g` for one call, the named functions `fn_args` being passed for its function
    /// parameters: a fresh scope with the template's variables, its body with each function parameter's call
    /// calling the function passed and each generic call instantiated in turn. `None` after an error (reported).
    pub(super) fn instantiate_fn(&mut self, g: usize, fn_args: &[HFnId], call: Span) -> Option<HFnId> {
        // Each call is an instance, and an instance's generic calls are instances of their own: nested generic calls
        // multiply them, so their number is bounded.
        if self.instance_calls.len() >= MAX_INSTANCES {
            if !self.instances_capped {
                self.instances_capped = true;
                self.error(
                    code!("BLS0220"),
                    call,
                    format!(
                        "generic functions are instantiated more than {MAX_INSTANCES} times (each call is an instance, \
                         and nested generic calls multiply them)"
                    ),
                );
            }
            return None;
        }
        if self.instantiating.contains(&g) {
            if self.recursive.insert(g)
                && let Some(t) = self.templates.get(g)
            {
                let (name, span) = (t.name.clone(), t.item.span);
                self.error(
                    code!("BLS0213"),
                    span,
                    format!("function `{name}` is recursive; functions are total (LANGUAGE §16.1)"),
                );
            }
            return None;
        }
        let Some(t) = self.templates.get(g) else {
            self.bugs
                .push(blossom_base::internal_error!("template {g} was not declared"));
            return None;
        };
        let (vars, module, mut body) = match &t.state {
            TemplateState::Resolved { vars, module, body } => (vars.clone(), module.clone(), body.clone()),
            TemplateState::Failed => return None,
            TemplateState::Declared => {
                let name = t.name.clone();
                self.bugs.push(blossom_base::internal_error!(
                    "template `{name}` was instantiated before its body was resolved"
                ));
                return None;
            }
        };
        let mut value_tys = Vec::new();
        let mut fn_params = Vec::new();
        for p in &t.params {
            match p {
                TParam::Value { ty, .. } => value_tys.push(ty.clone()),
                TParam::Fn { name, ty } => fn_params.push((name.name, ty.clone())),
            }
        }
        if fn_params.len() != fn_args.len() {
            let name = t.name.clone();
            self.bugs.push(blossom_base::internal_error!(
                "template `{name}` instantiated with {} functions for {} function parameters",
                fn_args.len(),
                fn_params.len()
            ));
            return None;
        }
        let (name, tparams, ret, span) = (t.name.clone(), t.tparams.clone(), t.ret.clone(), t.item.span);
        let scope = ScopeId(u32::try_from(self.hir.scopes.len()).ok()?);
        self.hir.scopes.push(HScope { vars, module });
        // Types over type parameters are placeholders until type checking writes the inferred ones.
        let placeholder = self.intern_type(TypeDef::Unit, span);
        let concrete = |t: &HTy| match t {
            HTy::Con(ty) => *ty,
            _ => placeholder,
        };
        let params = value_tys
            .iter()
            .enumerate()
            .map(|(k, t)| (HVarId(u32::try_from(k).unwrap_or(u32::MAX)), concrete(t)))
            .collect();
        let id = HFnId(u32::try_from(self.hir.fns.len()).ok()?);
        self.hir.fns.push(HFn {
            name: name.clone(),
            scope,
            params,
            ret: concrete(&ret),
            // A placeholder until the body is instantiated below; a failure is always reported, so it never
            // reaches type checking.
            body: HFnBody::Expr(HExpr::new(HExprKind::Tuple(Vec::new()), span)),
            span,
            scheme: Some(HScheme {
                generic: name,
                template: u32::try_from(g).ok()?,
                tparams,
                targs: Vec::new(),
                params: value_tys,
                ret,
                fn_args: fn_params
                    .into_iter()
                    .zip(fn_args)
                    .map(|((n, ty), f)| (n, ty, *f))
                    .collect(),
                call,
            }),
        });
        self.instantiating.push(g);
        let mut calls = BTreeSet::new();
        let ok = self.instantiate_expr(&mut body, fn_args, &mut calls);
        self.instantiating.pop();
        if !ok {
            return None;
        }
        if let Some(f) = self.hir.fns.get_mut(id.index()) {
            f.body = HFnBody::Expr(body);
        }
        self.instance_calls.insert(id, calls);
        Some(id)
    }

    /// Rewrites a copy of a template's body for an instance (see [`Resolver::instantiate_fn`]), collecting the
    /// functions it calls.
    fn instantiate_expr(&mut self, e: &mut HExpr, fn_args: &[HFnId], calls: &mut BTreeSet<HFnId>) -> bool {
        for c in children(&mut e.kind) {
            if !self.instantiate_expr(c, fn_args, calls) {
                return false;
            }
        }
        let call = match &mut e.kind {
            HExprKind::CallParam { param, args } => {
                let Some(f) = fn_args.get(*param as usize).copied() else {
                    self.bugs.push(blossom_base::internal_error!(
                        "function parameter {param} was not passed"
                    ));
                    return false;
                };
                (f, std::mem::take(args))
            }
            HExprKind::GenericCall {
                template,
                args,
                fn_args: passed,
            } => {
                let mut ids = Vec::new();
                for a in passed.iter() {
                    match a {
                        HFnArg::Fn(f) => ids.push(*f),
                        HFnArg::Param(i) => match fn_args.get(*i as usize) {
                            Some(f) => ids.push(*f),
                            None => {
                                self.bugs
                                    .push(blossom_base::internal_error!("function parameter {i} was not passed"));
                                return false;
                            }
                        },
                    }
                }
                let Some(f) = self.instantiate_fn(*template as usize, &ids, e.span) else {
                    return false;
                };
                (f, std::mem::take(args))
            }
            HExprKind::Call { f, .. } => {
                calls.insert(*f);
                return true;
            }
            _ => return true,
        };
        calls.insert(call.0);
        e.kind = HExprKind::Call {
            f: call.0,
            args: call.1,
        };
        true
    }
}

/// Whether a written type mentions one of the type parameters.
fn mentions(ty: &ast::Type, tparams: &[Symbol]) -> bool {
    match ty {
        ast::Type::Named { path, args, .. } => {
            (path.len() == 1 && path.first().is_some_and(|n| tparams.contains(&n.name)))
                || args.iter().any(|a| mentions(a, tparams))
        }
        ast::Type::Tuple { elems, .. } => elems.iter().any(|e| mentions(e, tparams)),
        ast::Type::Unsafe { inner, .. } => mentions(inner, tparams),
        ast::Type::Fn { params, ret, .. } => params.iter().any(|p| mentions(p, tparams)) || mentions(ret, tparams),
    }
}

/// The expressions directly inside an expression, those in its patterns included.
fn children(kind: &mut HExprKind) -> Vec<&mut HExpr> {
    let mut out: Vec<&mut HExpr> = Vec::new();
    match kind {
        HExprKind::Var(_)
        | HExprKind::Value(..)
        | HExprKind::IntLit(..)
        | HExprKind::TypedInt(..)
        | HExprKind::SelfNode
        | HExprKind::Now
        | HExprKind::Tick => {}
        HExprKind::Binary { lhs, rhs, .. } => {
            out.push(lhs);
            out.push(rhs);
        }
        HExprKind::In { elem, coll } => {
            out.push(elem);
            out.push(coll);
        }
        HExprKind::Prefix { arg, .. }
        | HExprKind::Cast { expr: arg, .. }
        | HExprKind::TupleIndex { base: arg, .. }
        | HExprKind::Field { base: arg, .. }
        | HExprKind::Lift { expr: arg, .. }
        | HExprKind::Closure { body: arg, .. } => out.push(arg),
        HExprKind::Tuple(es)
        | HExprKind::Variant { fields: es, .. }
        | HExprKind::Struct { fields: es, .. }
        | HExprKind::Builtin { args: es, .. }
        | HExprKind::Lookup { key: es, .. }
        | HExprKind::Collection { elems: es, .. }
        | HExprKind::LatCtor { args: es, .. }
        | HExprKind::LatOp { args: es, .. }
        | HExprKind::Call { args: es, .. }
        | HExprKind::CallParam { args: es, .. }
        | HExprKind::GenericCall { args: es, .. } => out.extend(es.iter_mut()),
        HExprKind::If { cond, then, els } => {
            out.push(cond);
            out.push(then);
            out.push(els);
        }
        HExprKind::Match { scrut, arms } => {
            out.push(scrut);
            for (p, g, b) in arms {
                pat_exprs(p, &mut out);
                if let Some(g) = g {
                    out.push(g);
                }
                out.push(b);
            }
        }
        HExprKind::Method { recv, args, .. } => {
            out.push(recv);
            out.extend(args.iter_mut());
        }
        HExprKind::Let { pat, value, body, .. } => {
            pat_exprs(pat, &mut out);
            out.push(value);
            out.push(body);
        }
    }
    out
}

fn pat_exprs<'a>(p: &'a mut HPat, out: &mut Vec<&'a mut HExpr>) {
    match p {
        HPat::Expr(e) => out.push(e),
        HPat::Tuple(ps, _) | HPat::Variant { fields: ps, .. } => {
            for x in ps {
                pat_exprs(x, out);
            }
        }
        HPat::Var(..) | HPat::Wild(_) => {}
    }
}
