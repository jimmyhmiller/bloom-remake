//! Rules: handlers, bootstraps, views and facts; their bodies, statements, patterns and expressions (LANGUAGE §8,
//! §9).
//!
//! Bodies are unordered conjunctions, so a body is resolved in two phases. First every variable that a binding
//! literal introduces is declared: the plain variables of positive atoms (and of `outer`, `inserted`, `deleted`,
//! `per` atoms and `from` suffixes), the pattern variables of `let`, the variables bound by every alternative of an
//! `any`, and then the variables of `x in e` generators that no other literal binds. Then every literal is resolved,
//! and a name in any other position must already be a variable (BLS0500) or a constant.
//!
//! Scoping (ARCHITECTURE §13.4): an `if`/`for` block, `not { … }`, a `forall` and each alternative of `any` open a
//! frame; the variables they introduce are local to it.

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::TypeId;
use blossom_base::{Span, Symbol, code};
use blossom_value::{TypeDef, Value};

use super::{Resolver, ScopeIdx};
use crate::ast::{self, Arg, BinOp, ExprKind, Ident, Lit, LitValue, PrefixOp, Stmt, Verb};
use crate::hir::*;

/// The state of one rule scope being resolved.
pub(crate) struct RuleCx {
    pub ms: ScopeIdx,
    pub scope: ScopeId,
    frames: Vec<BTreeMap<Symbol, HVarId>>,
    /// The frames below this one are hidden: inside a fragment's call, only its parameters and its own variables
    /// are visible (docs/design/SUGAR.md §4).
    barrier: usize,
    pub placement: Option<HRoleId>,
    /// The interposition aliases `outside`/`inside` in effect, if any.
    aliases: BTreeMap<Symbol, HRelId>,
    /// Whether a choice literal may appear here: a labelled handler's header or a single-alternative view (BLS0600).
    pub choice_allowed: bool,
    /// The choice literals of the body being resolved.
    pub choices: u32,
    /// Whether the statements are a plain `bootstrap`'s (which may not write durable relations, BLS0402).
    pub plain_bootstrap: bool,
    /// Whether this is a function body (LANGUAGE §16.1): `let` blocks and closures are allowed, and relations,
    /// `now()`, `tick()`, `self`, randomness and role members are not (BLS0215).
    pub in_fn: bool,
    /// The functions this scope calls, for the recursion check (BLS0213).
    pub calls: BTreeSet<HFnId>,
    /// A generic function's template being resolved: its function parameters are in scope, and calls of generic
    /// functions stay calls of their templates until it is instantiated (LANGUAGE §16.1).
    pub template: Option<usize>,
    /// The label of the handler whose statements are being resolved (for `resolve prefer`, LANGUAGE §10.7).
    pub label: Option<Symbol>,
    /// A method's receiver: what `self` names in its body (LANGUAGE §11.8).
    pub receiver: Option<HVarId>,
}

/// The functions a call resolves to before any declared one (LANGUAGE §9.12, §15, §16.1, Appendix B); a `fn` may not
/// take one of these names, which a call would never reach.
pub(super) const BUILTIN_FNS: &[&str] = &[
    "now",
    "tick",
    "random",
    "rand",
    "rand_range",
    "rand_float",
    "majority",
    "abs",
    "min",
    "max",
    "clamp",
    "hash64",
    "range",
    "error",
];

/// The operations every lattice has (LANGUAGE §11.4–11.5), which a method may not shadow.
const RESERVED_METHODS: &[&str] = &["join", "reveal", "is_bot", "leq", "lt", "of", "bot"];

fn is_var_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') && name != "_"
}

impl<'t> Resolver<'t, '_> {
    fn rule_cx(&mut self, ms: ScopeIdx, placement: Option<HRoleId>) -> RuleCx {
        let module = self.module_path(ms);
        self.hir.scopes.push(HScope {
            vars: Vec::new(),
            module,
        });
        RuleCx {
            ms,
            scope: ScopeId(u32::try_from(self.hir.scopes.len() - 1).unwrap_or(u32::MAX)),
            frames: vec![BTreeMap::new()],
            barrier: 0,
            placement,
            aliases: BTreeMap::new(),
            choice_allowed: false,
            choices: 0,
            plain_bootstrap: false,
            in_fn: false,
            calls: BTreeSet::new(),
            template: None,
            label: None,
            receiver: None,
        }
    }

    /// Reports a read a function body may not make (BLS0215); true if `cx` is a function body.
    fn impure(&mut self, cx: &RuleCx, span: Span, what: &str) -> bool {
        if cx.in_fn {
            self.error(
                code!("BLS0215"),
                span,
                format!("a function is pure: its body cannot read {what} (LANGUAGE §16.1)"),
            );
        }
        cx.in_fn
    }

    /// Pure functions (LANGUAGE §16.1): every signature in the module is declared first, so bodies may call functions
    /// declared after them; then generic functions' templates are resolved (`generic`), so the other bodies can
    /// instantiate them; then those bodies, each in its own scope with the parameters as its first variables; then
    /// calls that form a cycle are reported (BLS0213).
    pub(crate) fn functions(&mut self, s: ScopeIdx, items: &'t [ast::Item]) {
        let mut fns: Vec<(&'t ast::FnItem, HFnId, Option<HVarId>)> = Vec::new();
        let mut templates = Vec::new();
        let mut impls = Vec::new();
        let top = self.scope(s).own_items.is_none();
        let mut stack = vec![(items, top)];
        while let Some((items, top)) = stack.pop() {
            for item in items {
                match &item.kind {
                    ast::ItemKind::Impl(imp) if top => impls.push(imp),
                    ast::ItemKind::Impl(imp) => self.error(
                        code!("BLS0110"),
                        imp.span,
                        "an `impl` is an item of a file's top level, outside modules and `at` sections",
                    ),
                    ast::ItemKind::Fn(f) if super::generic::is_template(f) => {
                        if let Some(t) = self.declare_template(s, f) {
                            templates.push(t);
                        }
                    }
                    ast::ItemKind::Fn(f) => {
                        let body = HFnBody::Expr(HExpr::new(HExprKind::Tuple(Vec::new()), f.body.span));
                        if let Some(id) = self.declare_fn(s, f.name, &f.params, &f.ret, f.span, body) {
                            if let Some(h) = self.hir.fns.get_mut(id.index()) {
                                h.metered = f.metered;
                            }
                            fns.push((f, id, None));
                        }
                    }
                    ast::ItemKind::ExternFn(f) => self.extern_fn(s, f),
                    ast::ItemKind::At { items, .. } => stack.push((items, false)),
                    _ => {}
                }
            }
        }
        for imp in impls {
            fns.extend(self.declare_impl(s, imp));
        }
        for t in templates {
            self.resolve_template(t);
        }
        let mut calls: BTreeMap<HFnId, BTreeSet<HFnId>> = BTreeMap::new();
        for (f, id, receiver) in fns {
            let (scope, params) = match self.hir.fns.get(id.index()) {
                Some(h) => (h.scope, h.params.clone()),
                None => {
                    self.bugs
                        .push(blossom_base::internal_error!("function {id:?} was not declared"));
                    continue;
                }
            };
            let mut cx = RuleCx {
                ms: s,
                scope,
                frames: vec![BTreeMap::new()],
                barrier: 0,
                placement: None,
                aliases: BTreeMap::new(),
                choice_allowed: false,
                choices: 0,
                plain_bootstrap: false,
                in_fn: true,
                calls: BTreeSet::new(),
                template: None,
                label: None,
                receiver,
            };
            // A method's receiver is its first parameter, not among the item's.
            let skip = usize::from(receiver.is_some());
            for ((name, _), (v, _)) in f.params.iter().zip(params.iter().skip(skip)) {
                if let Some(frame) = cx.frames.last_mut() {
                    frame.insert(name.name, *v);
                }
            }
            match self.expr(&mut cx, &f.body) {
                Some(body) => {
                    if let Some(h) = self.hir.fns.get_mut(id.index()) {
                        h.body = HFnBody::Expr(body);
                    }
                }
                None if !self.diags.has_errors() => self.bugs.push(blossom_base::internal_error!(
                    "the body of function `{}` failed to resolve without a diagnostic",
                    f.name.as_str()
                )),
                None => {}
            }
            calls.insert(id, cx.calls);
        }
        // Generic function instances call what their templates call, with the functions passed for their function
        // parameters (a cycle through one runs through a function of this module, whose instances are all made by
        // now).
        for (id, cs) in &self.instance_calls {
            calls.entry(*id).or_insert_with(|| cs.clone());
        }
        // A function is total when everything it calls is: peel those off until nothing changes; what remains calls
        // itself, directly or through others.
        let mut total: BTreeSet<HFnId> = BTreeSet::new();
        loop {
            let before = total.len();
            for (id, cs) in &calls {
                if !total.contains(id) && cs.iter().all(|c| total.contains(c) || !calls.contains_key(c)) {
                    total.insert(*id);
                }
            }
            if total.len() == before {
                break;
            }
        }
        for id in calls.keys().filter(|id| !total.contains(id)) {
            if let Some(h) = self.hir.fns.get(id.index()) {
                let (name, span) = (h.name.clone(), h.span);
                self.error(
                    code!("BLS0213"),
                    span,
                    format!("function `{name}` is recursive; functions are total (LANGUAGE §16.1)"),
                );
            }
        }
    }

    /// Resolves a template's body once, in its own scope with its value parameters as the first variables and its
    /// function parameters callable. The variables move into the template: the scope is left empty (type checking
    /// sees nothing there), and each instance gets a copy.
    fn resolve_template(&mut self, t: usize) {
        let Some(tm) = self.templates.get(t) else {
            self.bugs
                .push(blossom_base::internal_error!("template {t} was not declared"));
            return;
        };
        let (item, ms) = (tm.item, tm.ms);
        let value_params: Vec<Ident> = tm
            .params
            .iter()
            .filter_map(|p| match p {
                super::generic::TParam::Value { name, .. } => Some(*name),
                super::generic::TParam::Fn { .. } => None,
            })
            .collect();
        let mut cx = self.rule_cx(ms, None);
        cx.in_fn = true;
        cx.template = Some(t);
        for p in &value_params {
            self.new_var(&mut cx, p.name, p.span, false);
        }
        let body = self.expr(&mut cx, &item.body);
        let (vars, module) = match self.hir.scopes.get_mut(cx.scope.index()) {
            Some(sc) => (std::mem::take(&mut sc.vars), sc.module.clone()),
            None => {
                self.bugs.push(blossom_base::internal_error!(
                    "template scope {:?} does not exist",
                    cx.scope
                ));
                return;
            }
        };
        let state = match body {
            Some(body) => super::generic::TemplateState::Resolved { vars, module, body },
            None => {
                if !self.diags.has_errors() {
                    self.bugs.push(blossom_base::internal_error!(
                        "the body of generic function `{}` failed to resolve without a diagnostic",
                        item.name.as_str()
                    ));
                }
                super::generic::TemplateState::Failed
            }
        };
        if let Some(tm) = self.templates.get_mut(t) {
            tm.state = state;
        }
    }

    /// A call of the function parameter `param` of the template being resolved.
    fn call_param(
        &mut self,
        cx: &mut RuleCx,
        name: Ident,
        (param, ty): (u32, HFnTy),
        pos: &[&ast::Expr],
        span: Span,
    ) -> Option<HExpr> {
        if pos.len() != ty.params.len() {
            self.error(
                code!("BLS0301"),
                span,
                format!(
                    "`{}` takes {} argument(s), {} given",
                    name.as_str(),
                    ty.params.len(),
                    pos.len()
                ),
            );
            return None;
        }
        let mut args = Vec::new();
        for p in pos {
            args.push(self.expr(cx, p)?);
        }
        Some(HExpr::new(HExprKind::CallParam { param, args }, span))
    }

    /// A call of the generic function `g`: in a template, a call of `g`'s template; anywhere else, a call of a new
    /// instance of it (`generic`).
    fn generic_call(
        &mut self,
        cx: &mut RuleCx,
        name: Ident,
        g: usize,
        pos: &[&ast::Expr],
        span: Span,
    ) -> Option<HExpr> {
        let fn_params: Vec<Option<Symbol>> = match self.templates.get(g) {
            Some(t) => t
                .params
                .iter()
                .map(|p| match p {
                    super::generic::TParam::Fn { name, .. } => Some(name.name),
                    super::generic::TParam::Value { .. } => None,
                })
                .collect(),
            None => {
                self.bugs
                    .push(blossom_base::internal_error!("template {g} was not declared"));
                return None;
            }
        };
        if pos.len() != fn_params.len() {
            self.error(
                code!("BLS0301"),
                span,
                format!(
                    "`{}` takes {} argument(s), {} given",
                    name.as_str(),
                    fn_params.len(),
                    pos.len()
                ),
            );
            return None;
        }
        let mut args = Vec::new();
        let mut fn_args = Vec::new();
        for (p, a) in fn_params.iter().zip(pos) {
            match p {
                None => args.push(self.expr(cx, a)?),
                Some(param) => fn_args.push(self.fn_arg(cx, *param, a)?),
            }
        }
        if cx.template.is_some() {
            return Some(HExpr::new(
                HExprKind::GenericCall {
                    template: u32::try_from(g).ok()?,
                    args,
                    fn_args,
                },
                span,
            ));
        }
        let mut ids = Vec::new();
        for a in fn_args {
            match a {
                HFnArg::Fn(f) => ids.push(f),
                HFnArg::Param(_) => {
                    self.bugs.push(blossom_base::internal_error!(
                        "a function parameter was passed outside a template"
                    ));
                    return None;
                }
            }
        }
        let f = self.instantiate_fn(g, &ids, span)?;
        cx.calls.insert(f);
        Some(HExpr::new(HExprKind::Call { f, args }, span))
    }

    /// The function passed for the function parameter `param`: a named function, or a function parameter of the
    /// template being resolved (BLS0219 otherwise).
    fn fn_arg(&mut self, cx: &RuleCx, param: Symbol, a: &ast::Expr) -> Option<HFnArg> {
        if let ExprKind::Path(path, targs) = &a.kind
            && targs.is_empty()
            && let [name] = path.as_slice()
        {
            if let Some((i, _)) = self.fn_param(cx.template, name.name) {
                return Some(HFnArg::Param(i));
            }
            if let Some(f) = self.scope(cx.ms).fns.get(&name.name).copied() {
                return Some(HFnArg::Fn(f));
            }
            if self.scope(cx.ms).generic_fns.contains_key(&name.name) {
                self.error(
                    code!("BLS0219"),
                    a.span,
                    format!(
                        "`{}` is generic, and only a function with fixed types can be passed for `{}`; pass a function that calls it",
                        name.as_str(),
                        param.as_str()
                    ),
                );
                return None;
            }
        }
        self.error(
            code!("BLS0219"),
            a.span,
            format!("`{}` takes a function: pass one by its name", param.as_str()),
        );
        None
    }

    /// `extern fn name(…) -> T = "path";`: a host function of the standard catalog (LANGUAGE §16.2), declared with
    /// exactly the catalog's signature. Anything else is BLS0216.
    fn extern_fn(&mut self, s: ScopeIdx, f: &'t ast::ExternFnItem) {
        let Some(std) = blossom_value::std_extern(&f.path) else {
            self.error(
                code!("BLS0216"),
                f.path_span,
                format!("`{}` is not a host function of the standard library", f.path),
            );
            return;
        };
        let body = HFnBody::Extern(std::sync::Arc::from(f.path.as_str()));
        let Some(id) = self.declare_fn(s, f.name, &f.params, &f.ret, f.span, body) else {
            return;
        };
        let Some(h) = self.hir.fns.get(id.index()) else {
            return;
        };
        let params: Vec<TypeId> = h.params.iter().map(|p| p.1).collect();
        if !std.signature().matches(&self.hir.types, &params, &[h.ret]) {
            let host: Vec<String> = std.params.iter().map(ToString::to_string).collect();
            self.error(
                code!("BLS0216"),
                f.span,
                format!(
                    "`{}` is `fn({}) -> {}`; declare it with that signature",
                    f.path,
                    host.join(", "),
                    std.ret
                ),
            );
        }
    }

    /// Declares a function's signature: its scope, parameter variables and types. A body is resolved later.
    fn declare_fn(
        &mut self,
        s: ScopeIdx,
        name: Ident,
        fparams: &'t [(Ident, ast::Type)],
        fret: &'t ast::Type,
        span: Span,
        body: HFnBody,
    ) -> Option<HFnId> {
        if BUILTIN_FNS.contains(&name.as_str()) {
            self.error(
                code!("BLS0201"),
                name.span,
                format!("`{}` is a built-in function; name this one differently", name.as_str()),
            );
            return None;
        }
        if self.scope(s).fns.contains_key(&name.name)
            || self.scope(s).generic_fns.contains_key(&name.name)
            || self.scope(s).rels.contains_key(&name.name)
            || self.scope(s).instances.contains_key(&name.name)
        {
            self.error(
                code!("BLS0201"),
                name.span,
                format!("`{}` is declared twice", name.as_str()),
            );
            return None;
        }
        let mut cx = self.rule_cx(s, None);
        let mut params = Vec::new();
        let mut seen = BTreeSet::new();
        let mut ok = true;
        for (p, ty) in fparams {
            if !seen.insert(p.name) {
                self.error(
                    code!("BLS0201"),
                    p.span,
                    format!("parameter `{}` is declared twice", p.as_str()),
                );
                ok = false;
            }
            let t = self.resolve_type(s, ty);
            if let Some(t) = t
                && holds_lattice(&self.hir.types, t)
            {
                self.unsupported(
                    "LANG-182",
                    "lattice-typed function parameters (they need a monotonicity class, `monotone fn` …)",
                    p.span,
                );
                ok = false;
            }
            let v = self.new_var(&mut cx, p.name, p.span, false);
            match t {
                Some(t) => params.push((v, t)),
                None => ok = false,
            }
        }
        let ret = self.resolve_type(s, fret);
        if let Some(r) = ret
            && holds_lattice(&self.hir.types, r)
        {
            self.unsupported(
                "LANG-182",
                "lattice-typed function results (they need a monotonicity class, `monotone fn` …)",
                fret.span(),
            );
            ok = false;
        }
        let (Some(ret), true) = (ret, ok) else {
            return None;
        };
        let id = HFnId(u32::try_from(self.hir.fns.len()).unwrap_or(u32::MAX));
        let qual = self.qual(s, name.name);
        self.hir.fns.push(HFn {
            name: qual,
            scope: cx.scope,
            params,
            ret,
            // An expression body is a placeholder until it is resolved; a failure to resolve it is always
            // reported, so the placeholder never reaches type checking.
            body,
            span,
            scheme: None,
            metered: true,
        });
        self.scope_mut(s).fns.insert(name.name, id);
        Some(id)
    }

    /// The methods of an `impl` (LANGUAGE §11.8), declared once whatever scope reaches the item: each is a function
    /// whose first parameter is the receiver (the variable `self` names), with its class. The lattice must be a
    /// product; a method's name may not be one of the lattice's own operations or another method's.
    fn declare_impl(&mut self, s: ScopeIdx, imp: &'t ast::ImplItem) -> Vec<(&'t ast::FnItem, HFnId, Option<HVarId>)> {
        let mut out = Vec::new();
        if !self.impls_done.insert(imp as *const ast::ImplItem as usize) {
            return out;
        }
        let Some(lattice) = self.resolve_type(s, &imp.ty) else {
            return out;
        };
        let lattice_name = match self.hir.lattice_of(lattice) {
            Some((_, blossom_ir::core::LatticeCtor::Product { name, .. })) => name.clone(),
            _ => {
                self.error(
                    code!("BLS0110"),
                    imp.ty.span(),
                    format!(
                        "an `impl` gives methods to a product lattice (`lattice X {{ … }}`): `{}` is not one",
                        crate::typeck::type_name(&self.hir.types, lattice)
                    ),
                );
                return out;
            }
        };
        for m in &imp.methods {
            let name = m.f.name;
            if RESERVED_METHODS.contains(&name.as_str()) {
                self.error(
                    code!("BLS0201"),
                    name.span,
                    format!(
                        "`{}` is an operation of every lattice; name this method differently",
                        name.as_str()
                    ),
                );
                continue;
            }
            if self.hir.method(lattice, name.name).is_some() {
                self.error(
                    code!("BLS0201"),
                    name.span,
                    format!("`{lattice_name}` has two methods named `{}`", name.as_str()),
                );
                continue;
            }
            let class = match &m.class {
                ast::MethodClass::None => HClass::None,
                ast::MethodClass::Morphism => HClass::Morphism,
                ast::MethodClass::Bimorphism => HClass::Bimorphism,
                ast::MethodClass::Monotone => HClass::Monotone,
                ast::MethodClass::Antitone => HClass::Antitone,
                ast::MethodClass::Threshold => HClass::Threshold,
                ast::MethodClass::Stable { after } => HClass::Stable { after: after.name },
            };
            let mut cx = self.rule_cx(s, None);
            let receiver = self.new_var(&mut cx, Symbol::intern("self"), name.span, false);
            let mut params = vec![(receiver, lattice)];
            let mut seen = BTreeSet::new();
            let mut ok = true;
            for (p, ty) in &m.f.params {
                if !seen.insert(p.name) {
                    self.error(
                        code!("BLS0201"),
                        p.span,
                        format!("parameter `{}` is declared twice", p.as_str()),
                    );
                    ok = false;
                }
                let v = self.new_var(&mut cx, p.name, p.span, false);
                match self.resolve_type(s, ty) {
                    Some(t) => params.push((v, t)),
                    None => ok = false,
                }
            }
            let Some(ret) = self.resolve_type(s, &m.f.ret) else {
                continue;
            };
            if !ok {
                continue;
            }
            let id = HFnId(u32::try_from(self.hir.fns.len()).unwrap_or(u32::MAX));
            let mut segs = lattice_name.segments().to_vec();
            segs.push(name.name);
            self.hir.fns.push(HFn {
                name: blossom_base::QualName::new(segs),
                scope: cx.scope,
                params,
                ret,
                // A placeholder until the body is resolved (see `declare_fn`).
                body: HFnBody::Expr(HExpr::new(HExprKind::Tuple(Vec::new()), m.f.body.span)),
                span: m.f.span,
                scheme: None,
                metered: m.f.metered,
            });
            self.hir.methods.push(HMethod {
                lattice,
                name: name.name,
                class,
                f: id,
                span: name.span,
            });
            out.push((&m.f, id, Some(receiver)));
        }
        // A stable method's threshold is a threshold method of the same lattice.
        for m in &imp.methods {
            let ast::MethodClass::Stable { after } = &m.class else {
                continue;
            };
            let ok = self.hir.method(lattice, after.name).is_some_and(|t| {
                t.class == HClass::Threshold && self.hir.fns.get(t.f.index()).is_some_and(|f| f.params.len() == 1)
            });
            if !ok {
                self.error(
                    code!("BLS0300"),
                    after.span,
                    format!(
                        "`stable … after {}`: `{}` is not a `threshold fn {}(self)` of `{lattice_name}`",
                        after.as_str(),
                        after.as_str(),
                        after.as_str()
                    ),
                );
            }
        }
        out
    }

    /// A `let` pattern in a function body: a name (always a new variable, shadowing any earlier one), `_`, or a
    /// tuple of those. A refutable pattern belongs in a `match`.
    fn let_pattern(&mut self, cx: &mut RuleCx, e: &ast::Expr) -> Option<HPat> {
        match &e.kind {
            ExprKind::Wildcard => Some(HPat::Wild(e.span)),
            ExprKind::Path(path, targs) if targs.is_empty() && path.len() == 1 => {
                let name = path.first()?;
                if !is_var_name(name.as_str()) {
                    self.error(
                        code!("BLS0301"),
                        e.span,
                        "a `let` binds names, `_` and tuples of them; match other patterns with `match`",
                    );
                    return None;
                }
                Some(HPat::Var(self.new_var(cx, name.name, name.span, false), e.span))
            }
            ExprKind::Tuple(elems) if !elems.is_empty() => {
                let mut ps = Vec::new();
                for el in elems {
                    ps.push(self.let_pattern(cx, el)?);
                }
                Some(HPat::Tuple(ps, e.span))
            }
            _ => {
                self.error(
                    code!("BLS0301"),
                    e.span,
                    "a `let` binds names, `_` and tuples of them; match other patterns with `match`",
                );
                None
            }
        }
    }

    /// `{ let p = e; …; result }` in a function body: nested `Let`s, each binding visible to the ones after it.
    fn let_block(&mut self, cx: &mut RuleCx, lets: &[ast::BlockLet], result: &ast::Expr) -> Option<HExpr> {
        cx.frames.push(BTreeMap::new());
        let mut bound = Vec::new();
        let mut failed = false;
        for l in lets {
            // The value is resolved before the pattern binds, so `let x = x + 1` reads the earlier `x`.
            let value = self.expr(cx, &l.value);
            let ty = match &l.ty {
                Some(t) => self.resolve_type(cx.ms, t).map(Some),
                None => Some(None),
            };
            let pat = match first_duplicate(&l.pat, &mut BTreeSet::new()) {
                Some(dup) => {
                    self.error(
                        code!("BLS0201"),
                        dup.span,
                        format!("`{}` is bound twice by this pattern", dup.as_str()),
                    );
                    None
                }
                None => self.let_pattern(cx, &l.pat),
            };
            if pat.is_none() {
                // Declare the names the rejected pattern meant to bind, so their uses are not reported again.
                let mut names = BTreeSet::new();
                refutable_pattern_names(&l.pat, &mut names);
                for n in names {
                    self.new_var(cx, n, l.pat.span, false);
                }
            }
            match (pat, ty, value) {
                (Some(p), Some(t), Some(v)) => bound.push((p, t, v, l.span)),
                _ => failed = true,
            }
        }
        let result = self.expr(cx, result);
        cx.frames.pop();
        if failed {
            return None;
        }
        let mut out = result?;
        for (pat, ty, value, span) in bound.into_iter().rev() {
            let span = span.to(out.span).unwrap_or(span);
            out = HExpr::new(
                HExprKind::Let {
                    pat: Box::new(pat),
                    ty,
                    value: Box::new(value),
                    body: Box::new(out),
                },
                span,
            );
        }
        Some(out)
    }

    /// A closure argument of a built-in combinator, in a function body: its parameters are new variables.
    fn closure(&mut self, cx: &mut RuleCx, params: &[Ident], body: &ast::Expr, span: Span) -> Option<HExpr> {
        if !cx.in_fn {
            self.error(
                code!("BLS0214"),
                span,
                "closures are allowed only as combinator arguments in function bodies (LANGUAGE §16.1)",
            );
            return None;
        }
        cx.frames.push(BTreeMap::new());
        let mut vs = Vec::new();
        let mut seen = BTreeSet::new();
        let mut ok = true;
        for p in params {
            if p.as_str() == "_" {
                // An ignored parameter: a variable nothing can name.
                vs.push(self.new_var(cx, p.name, p.span, true));
                continue;
            }
            if !is_var_name(p.as_str()) {
                self.error(
                    code!("BLS0301"),
                    p.span,
                    format!("a closure parameter is a lowercase name or `_`, not `{}`", p.as_str()),
                );
                ok = false;
            } else if !seen.insert(p.name) {
                self.error(
                    code!("BLS0201"),
                    p.span,
                    format!("closure parameter `{}` is declared twice", p.as_str()),
                );
                ok = false;
            }
            vs.push(self.new_var(cx, p.name, p.span, false));
        }
        let body = self.expr(cx, body);
        cx.frames.pop();
        if !ok {
            return None;
        }
        Some(HExpr::new(
            HExprKind::Closure {
                params: vs,
                body: Box::new(body?),
            },
            span,
        ))
    }

    fn new_var(&mut self, cx: &mut RuleCx, name: Symbol, span: Span, generated: bool) -> HVarId {
        let Some(scope) = self.hir.scopes.get_mut(cx.scope.index()) else {
            self.bugs.push(blossom_base::internal_error!(
                "rule scope {:?} does not exist",
                cx.scope
            ));
            return HVarId(0);
        };
        let id = HVarId(u32::try_from(scope.vars.len()).unwrap_or(u32::MAX));
        scope.vars.push(HVar { name, span, generated });
        if !generated && let Some(f) = cx.frames.last_mut() {
            f.insert(name, id);
        }
        id
    }

    fn lookup_var(cx: &RuleCx, name: Symbol) -> Option<HVarId> {
        cx.frames
            .get(cx.barrier..)
            .unwrap_or(&[])
            .iter()
            .rev()
            .find_map(|f| f.get(&name).copied())
    }

    /// A relation named by an atom's callee: `r`, `a.r`, or an interposition alias.
    fn callee_rel(&mut self, cx: &RuleCx, callee: &ast::Expr) -> Option<HRelId> {
        match &callee.kind {
            ExprKind::Path(path, targs) if targs.is_empty() => {
                if let [name] = path.as_slice() {
                    if Self::lookup_var(cx, name.name).is_some() {
                        return None;
                    }
                    if let Some(r) = cx.aliases.get(&name.name) {
                        return Some(*r);
                    }
                }
                self.lookup_rel(cx.ms, path)
            }
            ExprKind::Field { base, name } => match &base.kind {
                ExprKind::Path(p, targs) if targs.is_empty() && p.len() == 1 => {
                    let inst = p.first()?;
                    if Self::lookup_var(cx, inst.name).is_some() {
                        return None;
                    }
                    self.lookup_rel(cx.ms, &[*inst, *name])
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether a rule placed as `cx` may read a link event to a node of `peer`: one of its role and `peer` is a client
    /// role and the other a process or cluster role (BLS0404 otherwise).
    fn link_readable(&mut self, cx: &RuleCx, peer: HRoleId, span: Span) -> bool {
        let here = cx.placement.map(|r| self.role_of(r).kind);
        let there = self.role_of(peer).kind;
        let ok = matches!(
            (here, there),
            (Some(RoleKind::Client), RoleKind::Process | RoleKind::Cluster)
                | (Some(RoleKind::Process | RoleKind::Cluster), RoleKind::Client)
        );
        if !ok {
            let name = self.role_of(peer).name.clone();
            self.error(
                code!("BLS0404"),
                span,
                format!(
                    "`{name}.connected` and `{name}.disconnected` are read across a client link: at a client role, \
                     of a process or cluster role's node, or at a process or cluster role, of a client role's member"
                ),
            );
        }
        ok
    }

    /// Whether `role`'s members are known when the program is compiled; a client role's are not (BLS0404).
    fn static_members(&mut self, role: HRoleId, span: Span, what: &str) -> bool {
        if self.role_of(role).kind != RoleKind::Client {
            return true;
        }
        let name = self.role_of(role).name.clone();
        self.error(
            code!("BLS0404"),
            span,
            format!(
                "{what}: `{name}` is a client role, whose members join at run time; a program learns of them from \
                 their messages and from `{name}.connected`"
            ),
        );
        false
    }

    /// The relation an atom literal reads, with its arguments (`None` for a bare relation name).
    fn atom_parts<'e>(&mut self, cx: &RuleCx, e: &'e ast::Expr) -> Option<(HRelId, Option<&'e [Arg]>)> {
        match &e.kind {
            ExprKind::Call { callee, args } => self.callee_rel(cx, callee).map(|r| (r, Some(args.as_slice()))),
            // `a.r(args)`, an instance interface, parses as a method call.
            ExprKind::Method { receiver, name, args } => match &receiver.kind {
                ExprKind::Path(p, targs) if targs.is_empty() && p.len() == 1 => {
                    let inst = p.first()?;
                    if Self::lookup_var(cx, inst.name).is_some() {
                        return None;
                    }
                    self.lookup_rel(cx.ms, &[*inst, *name])
                        .map(|r| (r, Some(args.as_slice())))
                }
                _ => None,
            },
            ExprKind::Path(..) | ExprKind::Field { .. } => self.callee_rel(cx, e).map(|r| (r, None)),
            _ => None,
        }
    }

    // ------------------------------------------------------------------ phase 1: declarations

    /// Declares the variables of a pattern that are not bound yet.
    fn declare_pattern(&mut self, cx: &mut RuleCx, e: &ast::Expr) {
        match &e.kind {
            ExprKind::Path(path, targs) if targs.is_empty() && path.len() == 1 => {
                if let Some(name) = path.first()
                    && is_var_name(name.as_str())
                    && Self::lookup_var(cx, name.name).is_none()
                    && !cx.aliases.contains_key(&name.name)
                {
                    self.new_var(cx, name.name, name.span, false);
                }
            }
            ExprKind::Tuple(elems) => {
                for el in elems {
                    self.declare_pattern(cx, el);
                }
            }
            ExprKind::Call { callee, args } if self.is_constructor(cx, callee) => {
                for a in args {
                    if let Arg::Pos(p) = a {
                        self.declare_pattern(cx, p);
                    }
                }
            }
            _ => {}
        }
    }

    /// Declares the variables of a match arm's pattern, in the arm's own frame. In a function body a name always
    /// binds a new variable, shadowing any outer one, as `let` and closure parameters do there. In a rule body a
    /// name the rule already binds is BLS0501: whether the arm should compare with it or bind afresh is ambiguous
    /// there, so the rule names the arm's variable differently and compares in a guard.
    fn declare_arm_pattern(&mut self, cx: &mut RuleCx, e: &ast::Expr) {
        match &e.kind {
            ExprKind::Path(path, targs) if targs.is_empty() && path.len() == 1 => {
                if let Some(name) = path.first()
                    && is_var_name(name.as_str())
                    && !cx.aliases.contains_key(&name.name)
                {
                    if !cx.in_fn && Self::lookup_var(cx, name.name).is_some() {
                        self.error(
                            code!("BLS0501"),
                            name.span,
                            format!(
                                "this match arm re-binds `{0}`, which the rule binds: name the arm's variable \
                                 differently and compare in a guard (`{0}2 if {0}2 == {0}`)",
                                name.as_str()
                            ),
                        );
                    }
                    self.new_var(cx, name.name, name.span, false);
                }
            }
            ExprKind::Tuple(elems) => {
                for el in elems {
                    self.declare_arm_pattern(cx, el);
                }
            }
            ExprKind::Call { callee, args } if self.is_constructor(cx, callee) => {
                for a in args {
                    if let Arg::Pos(p) = a {
                        self.declare_arm_pattern(cx, p);
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether a callee names a variant constructor (`Some`, `E::V`) rather than a relation.
    fn is_constructor(&mut self, cx: &RuleCx, callee: &ast::Expr) -> bool {
        match &callee.kind {
            ExprKind::Path(path, _) => match path.as_slice() {
                [n] => n.as_str() == "Some",
                [e, _] => self.enum_named(cx.ms, *e).is_some(),
                _ => false,
            },
            _ => false,
        }
    }

    fn declare_atom_args(&mut self, cx: &mut RuleCx, atom: &ast::AtomLit) {
        // A spec atom `r(args) @ loc [at tick k]` binds its arguments and its location.
        if self.spec.is_some()
            && let Some(loc) = &atom.at
        {
            if let Some((_, Some(args))) = spec_rel_path(&atom.expr) {
                for a in args {
                    if let Arg::Pos(p) | Arg::Named(_, p) = a {
                        self.declare_pattern(cx, p);
                    }
                }
            }
            self.declare_pattern(cx, loc);
            return;
        }
        if let Some((_, Some(args))) = self.atom_parts(cx, &atom.expr) {
            for a in args {
                match a {
                    Arg::Pos(p) | Arg::Named(_, p) => self.declare_pattern(cx, p),
                    Arg::Rest(_) | Arg::Star(_) | Arg::Spread(_) => {}
                }
            }
        }
        if let Some(f) = &atom.from {
            self.declare_pattern(cx, f);
        }
    }

    /// The names a body's binding literals bind (for `any`).
    fn bound_names(&mut self, cx: &RuleCx, body: &ast::Body, out: &mut BTreeSet<Symbol>) {
        fn pattern_names(e: &ast::Expr, out: &mut BTreeSet<Symbol>) {
            match &e.kind {
                ExprKind::Path(path, _) if path.len() == 1 => {
                    if let Some(n) = path.first()
                        && is_var_name(n.as_str())
                    {
                        out.insert(n.name);
                    }
                }
                ExprKind::Tuple(es) => es.iter().for_each(|x| pattern_names(x, out)),
                ExprKind::Call { args, .. } => {
                    for a in args {
                        if let Arg::Pos(p) | Arg::Named(_, p) = a {
                            pattern_names(p, out);
                        }
                    }
                }
                _ => {}
            }
        }
        for lit in &body.lits {
            match lit {
                Lit::Plain(a) | Lit::Outer(a) | Lit::Inserted(a) | Lit::Deleted(a) | Lit::Per(a) => {
                    if self.atom_parts(cx, &a.expr).is_some() {
                        pattern_names(&a.expr, out);
                    } else if let ExprKind::Binary { op: BinOp::In, lhs, .. } = &a.expr.kind {
                        pattern_names(lhs, out);
                    }
                    if let Some(f) = &a.from {
                        pattern_names(f, out);
                    }
                }
                Lit::Let { pat, .. } => pattern_names(pat, out),
                _ => {}
            }
        }
    }

    /// Phase 1 for a body: declares its variables in the current frame. Returns the indexes of the `x in e`
    /// literals that are generators.
    fn declare_body(&mut self, cx: &mut RuleCx, body: &ast::Body) -> BTreeSet<usize> {
        for lit in &body.lits {
            match lit {
                Lit::Plain(a) | Lit::Outer(a) | Lit::Inserted(a) | Lit::Deleted(a) | Lit::Per(a) => {
                    self.declare_atom_args(cx, a);
                }
                Lit::Let { pat, .. } => self.declare_pattern(cx, pat),
                Lit::Any(alts, _) => {
                    let mut common: Option<BTreeSet<Symbol>> = None;
                    for alt in alts {
                        let mut names = BTreeSet::new();
                        self.bound_names(cx, alt, &mut names);
                        common = Some(match common {
                            None => names,
                            Some(c) => c.intersection(&names).copied().collect(),
                        });
                    }
                    let mut common: Vec<Symbol> = common.unwrap_or_default().into_iter().collect();
                    common.sort_by_key(|s| s.as_str());
                    for name in common {
                        if Self::lookup_var(cx, name).is_none() {
                            self.new_var(cx, name, lit.span(), false);
                        }
                    }
                }
                _ => {}
            }
        }
        let mut generators = BTreeSet::new();
        for (i, lit) in body.lits.iter().enumerate() {
            if let Lit::Plain(a) = lit
                && let ExprKind::Binary { op: BinOp::In, lhs, .. } = &a.expr.kind
                && self.atom_parts(cx, &a.expr).is_none()
            {
                let mut names = BTreeSet::new();
                collect_pattern_names(lhs, &mut names);
                let unbound = names.iter().any(|n| Self::lookup_var(cx, *n).is_none());
                if unbound {
                    self.declare_pattern(cx, lhs);
                    generators.insert(i);
                }
            }
        }
        generators
    }

    // ------------------------------------------------------------------ phase 2: literals

    /// Resolves a body in a new frame (for blocks, `not { … }` and alternatives).
    fn body_in_frame(&mut self, cx: &mut RuleCx, body: &ast::Body) -> HBody {
        cx.frames.push(BTreeMap::new());
        let b = self.body(cx, body);
        cx.frames.pop();
        b
    }

    /// Resolves a body in the current frame.
    pub(crate) fn body(&mut self, cx: &mut RuleCx, body: &ast::Body) -> HBody {
        let generators = self.declare_body(cx, body);
        let mut out = HBody {
            lits: Vec::new(),
            span: Some(body.span),
        };
        for (i, lit) in body.lits.iter().enumerate() {
            if let Some(l) = self.lit(cx, lit, generators.contains(&i)) {
                out.lits.push(l);
            }
        }
        for g in &body.guards {
            if let Some(e) = self.expr(cx, g) {
                out.lits.push(HLit::Guard(e));
            }
        }
        out
    }

    fn lit(&mut self, cx: &mut RuleCx, lit: &Lit, generator: bool) -> Option<HLit> {
        match lit {
            Lit::Plain(a) => self.plain(cx, a, generator),
            Lit::Not(inner, span) => match inner.as_ref() {
                Lit::Plain(a) => {
                    if let Some(atom) = self.try_atom(cx, a)? {
                        Some(HLit::Not(atom))
                    } else {
                        // `not` on a scalar guard is boolean negation (LANGUAGE §9.3).
                        let e = self.expr(cx, &a.expr)?;
                        Some(HLit::Guard(HExpr {
                            ty: None,
                            kind: HExprKind::Prefix {
                                op: PrefixOp::Not,
                                arg: Box::new(e),
                            },
                            span: *span,
                        }))
                    }
                }
                _ => {
                    self.error(code!("BLS0110"), *span, "`not` applies to an atom, a guard or `{ … }`");
                    None
                }
            },
            Lit::NotBody(body, span) => {
                let b = self.body_in_frame(cx, body);
                Some(HLit::NotBody(b, *span))
            }
            Lit::Let { pat, value, span } => {
                let expr = self.expr(cx, value)?;
                let pat = self.pattern(cx, pat)?;
                Some(HLit::Let { pat, expr, span: *span })
            }
            Lit::Outer(a) => Some(HLit::Outer(self.need_atom(cx, a)?)),
            Lit::Inserted(a) => Some(HLit::Delta {
                inserted: true,
                atom: self.need_atom(cx, a)?,
            }),
            Lit::Deleted(a) => Some(HLit::Delta {
                inserted: false,
                atom: self.need_atom(cx, a)?,
            }),
            Lit::Per(a) => Some(HLit::Per(self.need_atom(cx, a)?)),
            Lit::Any(alts, span) => {
                let mut bodies = Vec::new();
                for alt in alts {
                    bodies.push(self.body_in_frame(cx, alt));
                }
                Some(HLit::Any(bodies, *span))
            }
            Lit::Forall { domain, body, span } => {
                cx.frames.push(BTreeMap::new());
                let domain_body = ast::Body {
                    lits: vec![Lit::Plain(domain.clone())],
                    guards: Vec::new(),
                    span: domain.span,
                };
                let generators = self.declare_body(cx, &domain_body);
                let d = self.plain(cx, domain, generators.contains(&0));
                let b = self.body_in_frame(cx, body);
                cx.frames.pop();
                Some(HLit::Forall {
                    domain: Box::new(d?),
                    body: b,
                    span: *span,
                })
            }
            Lit::Sealed(a) => {
                self.unsupported("LANG-207", "`sealed` tests", a.span);
                None
            }
            Lit::Final(a) => {
                self.unsupported("LANG-212", "`final` tests", a.span);
                None
            }
            Lit::Spec(s) => {
                self.error(code!("BLS0509"), s.span(), "this literal is allowed only in a spec");
                None
            }
        }
    }

    /// An atom literal, or an error.
    fn need_atom(&mut self, cx: &mut RuleCx, a: &ast::AtomLit) -> Option<HAtom> {
        match self.try_atom(cx, a)? {
            Some(atom) => Some(atom),
            None => {
                self.error(code!("BLS0202"), a.span, "expected a relation atom");
                None
            }
        }
    }

    /// `Some(Some(atom))` if the literal reads a relation, `Some(None)` if it does not, `None` on an error.
    /// The relation name an atom literal's expression names (`r` of `r(…)` or `r`), or a symbol that names none.
    fn rel_name_of(&self, e: &ast::Expr) -> Symbol {
        spec_rel_path(e).map_or(Symbol::intern(""), |(n, _)| n.name)
    }

    /// A spec atom `r(args) @ loc [at tick k]`: an atom of `r`'s trace relation at that time, the node first.
    fn spec_atom(&mut self, cx: &mut RuleCx, a: &ast::AtomLit, loc: &ast::Expr) -> Option<HAtom> {
        let Some((name, args)) = spec_rel_path(&a.expr) else {
            self.error(
                code!("BLS0200"),
                a.span,
                "`@ n` locates an atom of the target's relation",
            );
            return None;
        };
        let with_args = args.is_some();
        let args = args.unwrap_or(&[]);
        let time = match &a.at_tick {
            None => None,
            Some(k) => match self.const_value(cx.ms, k, None) {
                Some((blossom_value::Value::Int(i), _)) => match i.to_i128().and_then(|v| u64::try_from(v).ok()) {
                    Some(t) => Some(t),
                    None => {
                        self.error(code!("BLS0300"), k.span, "`at tick k` needs a non-negative tick");
                        return None;
                    }
                },
                _ => {
                    self.error(code!("BLS0300"), k.span, "`at tick k` needs a constant tick");
                    return None;
                }
            },
        };
        let Some(rel) = self.trace_rel(name, time) else {
            self.error(
                code!("BLS0200"),
                name.span,
                format!("`{}` is not a relation of the spec's target", name.as_str()),
            );
            return None;
        };
        let loc = self.pattern(cx, loc)?;
        let r = self.rel_of(rel);
        let cols: Vec<Symbol> = r.cols.iter().skip(1).map(|c| c.name).collect();
        let mut out = vec![loc];
        if with_args {
            out.extend(self.args_for(cx, name.as_str(), &cols, args, a.span)?);
        } else {
            out.extend(cols.iter().map(|_| HPat::Wild(a.span)));
        }
        Some(HAtom {
            rel,
            args: out,
            from: None,
            span: a.span,
        })
    }

    fn try_atom(&mut self, cx: &mut RuleCx, a: &ast::AtomLit) -> Option<Option<HAtom>> {
        if self.spec.is_some()
            && let Some(loc) = &a.at
        {
            return Some(Some(self.spec_atom(cx, a, loc)?));
        }
        let Some((rel, args)) = self.atom_parts(cx, &a.expr) else {
            if a.from.is_some() {
                self.error(code!("BLS0212"), a.span, "`from` applies only to channel atoms");
            }
            return Some(None);
        };
        // A link event is read only across a client link (CLIENTS.md §1).
        if let HRelKind::Link { peer, .. } = self.rel_of(rel).kind
            && !self.link_readable(cx, peer, a.span)
        {
            return None;
        }
        if a.principal.is_some() {
            self.unsupported("LANG-241", "`principal` bindings", a.span);
            return None;
        }
        if a.weight.is_some() {
            self.unsupported("LANG-138", "weight bindings", a.span);
            return None;
        }
        if a.at.is_some() || a.at_tick.is_some() {
            self.error(
                code!("BLS0509"),
                a.span,
                "`@ n` and `at tick k` are allowed only in a spec",
            );
            return None;
        }
        if self
            .spec
            .as_ref()
            .is_some_and(|s| s.targets.contains_key(&self.rel_name_of(&a.expr)))
        {
            self.error(
                code!("BLS0509"),
                a.span,
                "an atom of the target's relation names its location with `@ n` (LANGUAGE §17.3)",
            );
            return None;
        }
        let args = match args {
            Some(args) => self.atom_args(cx, rel, args, a.span)?,
            None => (0..self.rel_of(rel).cols.len()).map(|_| HPat::Wild(a.span)).collect(),
        };
        let from = match &a.from {
            None => None,
            Some(f) => {
                if !matches!(self.rel_of(rel).kind, HRelKind::Channel(_)) {
                    self.error(
                        code!("BLS0212"),
                        f.span,
                        "`from` applies only to channel and loopback atoms",
                    );
                    return None;
                }
                Some(self.pattern(cx, f)?)
            }
        };
        self.check_readable(cx, rel, a.span);
        Some(Some(HAtom {
            rel,
            args,
            from,
            span: a.span,
        }))
    }

    /// An importer may read only the outputs of an instance (BLS0203); a channel is read at its destination role.
    fn check_readable(&mut self, cx: &RuleCx, rel: HRelId, span: Span) {
        let r = self.rel_of(rel).clone();
        if let HRelKind::Stream(HStreamRel::Host(_)) = r.kind {
            self.error(
                code!("BLS0203"),
                span,
                format!("`{}` is a request to the host: it can be sent, not read", r.name),
            );
        }
        if let HRelKind::Input { root: false } = r.kind
            && self.is_foreign_interface(cx, rel)
        {
            self.error(
                code!("BLS0203"),
                span,
                format!("`{}` is an input of an instance: it can be written, not read", r.name),
            );
        }
        if let Some(there) = r.role
            && let Some(here) = cx.placement
            && here != there
        {
            let there_name = self.role_of(there).name.clone();
            self.error(
                code!("BLS0404"),
                span,
                format!(
                    "`{}` lives at `{there_name}`, so only rules placed there read it",
                    r.name
                ),
            );
        }
        if let HRelKind::Channel(ChannelInfo {
            direction: Some((_, dst)),
            ..
        }) = r.kind
            && let Some(here) = cx.placement
            && here != dst
        {
            let name = r.name.clone();
            let dst_name = self.role_of(dst).name.clone();
            self.error(
                code!("BLS0404"),
                span,
                format!("`{name}` is received at `{dst_name}`, so it can be read only there"),
            );
        }
    }

    /// Whether `rel` is an interface of an instance of the current scope (rather than a relation of the scope).
    /// Whether `rel` is declared in the scope of `cx` (a module's own relation, not an instance's).
    fn declared_here(&self, cx: &RuleCx, rel: HRelId) -> bool {
        self.scope(cx.ms).rels.values().any(|r| *r == rel)
    }

    /// Whether `rel` is an instance's interface as seen from `cx`: not declared here, and not an interposition's
    /// `inside`, which the interposition block reads.
    fn is_foreign_interface(&self, cx: &RuleCx, rel: HRelId) -> bool {
        !self.declared_here(cx, rel) && !cx.aliases.values().any(|r| *r == rel)
    }

    /// Arguments of an atom, positional or named (LANGUAGE §9.2), one pattern per column.
    fn atom_args(&mut self, cx: &mut RuleCx, rel: HRelId, args: &[Arg], span: Span) -> Option<Vec<HPat>> {
        let r = self.rel_of(rel);
        let cols: Vec<Symbol> = r.cols.iter().map(|c| c.name).collect();
        self.args_for(cx, &r.name.to_string(), &cols, args, span)
    }

    /// Arguments over the columns `cols` of the relation `name`, positional or named, one pattern per column.
    fn args_for(
        &mut self,
        cx: &mut RuleCx,
        name: &str,
        cols: &[Symbol],
        args: &[Arg],
        span: Span,
    ) -> Option<Vec<HPat>> {
        let named = args.iter().any(|a| matches!(a, Arg::Named(..) | Arg::Rest(_)));
        if !named {
            if args.len() != cols.len() {
                self.error(
                    code!("BLS0301"),
                    span,
                    format!("`{name}` has {} column(s), {} given", cols.len(), args.len()),
                );
                return None;
            }
            let mut out = Vec::new();
            for a in args {
                match a {
                    Arg::Pos(e) => out.push(self.pattern(cx, e)?),
                    Arg::Star(s) => {
                        self.error(code!("BLS0302"), *s, "`*` is not an atom argument");
                        return None;
                    }
                    Arg::Spread(_) => {
                        self.error(code!("BLS0302"), a.span(), "a spread is not an atom argument");
                        return None;
                    }
                    Arg::Named(..) | Arg::Rest(_) => return None,
                }
            }
            return Some(out);
        }
        let mut slots: Vec<Option<HPat>> = vec![None; cols.len()];
        let mut rest = false;
        for a in args {
            let (field, value) = match a {
                Arg::Named(f, v) => (*f, v.clone()),
                Arg::Pos(e) => match &e.kind {
                    ExprKind::Path(p, t) if t.is_empty() && p.len() == 1 => {
                        let f = p.first().copied()?;
                        (f, e.clone())
                    }
                    _ => {
                        self.error(
                            code!("BLS0302"),
                            e.span,
                            "in a named atom, every argument is `field: pattern` or a field name",
                        );
                        return None;
                    }
                },
                Arg::Rest(_) => {
                    rest = true;
                    continue;
                }
                Arg::Star(s) => {
                    self.error(code!("BLS0302"), *s, "`*` is not an atom argument");
                    return None;
                }
                Arg::Spread(_) => {
                    self.error(code!("BLS0302"), a.span(), "a spread is not an atom argument");
                    return None;
                }
            };
            let Some(i) = cols.iter().position(|c| *c == field.name) else {
                self.error(
                    code!("BLS0302"),
                    field.span,
                    format!("`{name}` has no column `{}`", field.as_str()),
                );
                return None;
            };
            let p = self.pattern(cx, &value)?;
            if slots.get_mut(i).and_then(|s| s.replace(p)).is_some() {
                self.error(
                    code!("BLS0302"),
                    field.span,
                    format!("column `{}` given twice", field.as_str()),
                );
                return None;
            }
        }
        let mut out = Vec::new();
        for (slot, col) in slots.into_iter().zip(cols) {
            match slot {
                Some(p) => out.push(p),
                None if rest => out.push(HPat::Wild(span)),
                None => {
                    self.error(
                        code!("BLS0302"),
                        span,
                        format!("column `{col}` of `{name}` is not given; write `..` to ignore the rest"),
                    );
                    return None;
                }
            }
        }
        Some(out)
    }

    /// A plain literal: an atom, a generator, a membership test, or a guard (LANGUAGE §9.1).
    fn plain(&mut self, cx: &mut RuleCx, a: &ast::AtomLit, generator: bool) -> Option<HLit> {
        if let Some(atom) = self.try_atom(cx, a)? {
            return Some(HLit::Atom(atom));
        }
        match &a.expr.kind {
            ExprKind::Binary {
                op: BinOp::In,
                lhs,
                rhs,
            } => self.membership(cx, lhs, rhs, generator, a.span),
            ExprKind::Bang { name, args, clauses }
                if matches!(name.as_str(), "choose" | "choose_least" | "choose_most") =>
            {
                self.choose(cx, *name, args, clauses, a.span)
            }
            ExprKind::Bang { name, args, clauses } if matches!(name.as_str(), "argmin" | "argmax") => {
                self.extreme(cx, *name, args, clauses, a.span)
            }
            ExprKind::Bang { name, .. } => {
                self.unsupported(
                    "LANG-108",
                    &format!("`{}!` in a body (choice and order filters)", name.as_str()),
                    a.span,
                );
                None
            }
            _ => Some(HLit::Guard(self.expr(cx, &a.expr)?)),
        }
    }

    /// `choose!(Ȳ per X̄ [least c | most c] [sticky])`, `choose_least!(Ȳ per X̄)` (the cost is Ȳ) and
    /// `choose_most!` (LANGUAGE §10.4).
    fn choose(
        &mut self,
        cx: &mut RuleCx,
        name: Ident,
        args: &[Arg],
        clauses: &[ast::BangClause],
        span: Span,
    ) -> Option<HLit> {
        if !cx.choice_allowed {
            self.error(
                code!("BLS0600"),
                span,
                "a choice belongs to a labelled handler's header or a single-alternative view",
            );
            return None;
        }
        cx.choices += 1;
        if cx.choices > 1 {
            self.unsupported("LANG-116", "several choices in one body (a multi-FD site)", span);
            return None;
        }
        let [Arg::Pos(y)] = args else {
            self.error(
                code!("BLS0301"),
                span,
                format!("`{}!` chooses one value or tuple", name.as_str()),
            );
            return None;
        };
        let parts = |e: &ast::Expr| -> Vec<ast::Expr> {
            match &e.kind {
                ExprKind::Tuple(es) if !es.is_empty() => es.clone(),
                _ => vec![e.clone()],
            }
        };
        let mut chosen = Vec::new();
        for e in parts(y) {
            chosen.push(self.expr(cx, &e)?);
        }
        let mut per = Vec::new();
        let mut cost = match name.as_str() {
            "choose_least" => Some((y.clone(), false)),
            "choose_most" => Some((y.clone(), true)),
            _ => None,
        };
        let mut sticky = false;
        for c in clauses {
            match c.keyword.as_str() {
                "per" => {
                    for e in &c.exprs {
                        for p in parts(e) {
                            per.push(self.expr(cx, &p)?);
                        }
                    }
                }
                "least" | "most" if name.as_str() == "choose" && cost.is_none() => {
                    let [e] = c.exprs.as_slice() else {
                        self.error(code!("BLS0301"), c.span, "a choice has one cost");
                        return None;
                    };
                    cost = Some((e.clone(), c.keyword.as_str() == "most"));
                }
                "sticky" => sticky = true,
                "durable" => {
                    self.unsupported("LANG-115", "`sticky durable` choices", c.span);
                    return None;
                }
                other => {
                    self.error(
                        code!("BLS0302"),
                        c.span,
                        format!("`{other}` is not a clause of `{}!`", name.as_str()),
                    );
                    return None;
                }
            }
        }
        let cost = match cost {
            Some((e, most)) => Some((self.expr(cx, &e)?, most)),
            None => None,
        };
        Some(HLit::Choose(Box::new(HChoose {
            chosen,
            per,
            cost,
            sticky,
            ties: false,
            span,
        })))
    }

    /// `argmin!(c [per X̄])` and `argmax!(c [per X̄])` (LANGUAGE §10.3): the valuations whose `c` is least
    /// (greatest) within their group, every tie included. Deterministic, so allowed anywhere a literal is.
    fn extreme(
        &mut self,
        cx: &mut RuleCx,
        name: Ident,
        args: &[Arg],
        clauses: &[ast::BangClause],
        span: Span,
    ) -> Option<HLit> {
        let [Arg::Pos(c)] = args else {
            self.error(
                code!("BLS0301"),
                span,
                format!("`{}!` orders by one value", name.as_str()),
            );
            return None;
        };
        let cost = self.expr(cx, c)?;
        let mut per = Vec::new();
        for cl in clauses {
            if cl.keyword.as_str() != "per" {
                self.error(
                    code!("BLS0302"),
                    cl.span,
                    format!("`{}` is not a clause of `{}!`", cl.keyword.as_str(), name.as_str()),
                );
                return None;
            }
            for e in &cl.exprs {
                let parts = match &e.kind {
                    ExprKind::Tuple(es) if !es.is_empty() => es.clone(),
                    _ => vec![e.clone()],
                };
                for p in parts {
                    per.push(self.expr(cx, &p)?);
                }
            }
        }
        Some(HLit::Choose(Box::new(HChoose {
            chosen: Vec::new(),
            per,
            cost: Some((cost, name.as_str() == "argmax")),
            sticky: false,
            ties: true,
            span,
        })))
    }

    /// `pat in e` (LANGUAGE §9.4).
    fn membership(
        &mut self,
        cx: &mut RuleCx,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        generator: bool,
        span: Span,
    ) -> Option<HLit> {
        // A role.
        if let ExprKind::Path(p, t) = &rhs.kind
            && t.is_empty()
            && let [name] = p.as_slice()
            && let Some(role) = self.role_named(cx.ms, name.name)
        {
            if !self.static_members(role, name.span, "`p in R`") {
                return None;
            }
            let pat = self.pattern(cx, lhs)?;
            let members = self.members_rel(role, name.span);
            if generator {
                return Some(HLit::RoleGen {
                    pat,
                    role,
                    members,
                    span,
                });
            }
            return Some(HLit::Atom(HAtom {
                rel: members,
                args: vec![pat],
                from: None,
                span,
            }));
        }
        // A unary relation (a cell is a lattice value, below).
        if let Some((rel, None)) = self.atom_parts(cx, rhs)
            && !self.rel_of(rel).cell
        {
            if self.rel_of(rel).cols.len() != 1 {
                self.error(code!("BLS0301"), rhs.span, "`x in r` needs a relation with one column");
                return None;
            }
            let pat = self.pattern(cx, lhs)?;
            self.check_readable(cx, rel, span);
            return Some(HLit::Atom(HAtom {
                rel,
                args: vec![pat],
                from: None,
                span,
            }));
        }
        // A range.
        if let ExprKind::Binary { op, lhs: lo, rhs: hi } = &rhs.kind
            && let Some(kind) = range_kind(*op)
        {
            let lo = self.expr(cx, lo)?;
            let hi = self.expr(cx, hi)?;
            if generator {
                let pat = self.pattern(cx, lhs)?;
                return Some(HLit::RangeGen {
                    pat,
                    lo,
                    hi,
                    kind,
                    span,
                });
            }
            // A test on a bound value: lo ≤ x < hi and the other forms.
            let x = self.expr(cx, lhs)?;
            let (lo_op, hi_op) = match kind {
                RangeKind::HalfOpen => (BinOp::Le, BinOp::Lt),
                RangeKind::Closed => (BinOp::Le, BinOp::Le),
                RangeKind::OpenOpen => (BinOp::Lt, BinOp::Lt),
                RangeKind::OpenClosed => (BinOp::Lt, BinOp::Le),
            };
            let bin = |op, l: HExpr, r: HExpr| HExpr {
                ty: None,
                kind: HExprKind::Binary {
                    op,
                    lhs: Box::new(l),
                    rhs: Box::new(r),
                },
                span,
            };
            return Some(HLit::Guard(bin(
                BinOp::And,
                bin(lo_op, lo, x.clone()),
                bin(hi_op, x, hi),
            )));
        }
        if generator {
            let pat = self.pattern(cx, lhs)?;
            let src = self.expr(cx, rhs)?;
            return Some(HLit::Gen { pat, src, span });
        }
        // A membership test in a value: a set-like lattice (a threshold) or a collection; type checking decides.
        let elem = self.expr(cx, lhs)?;
        let coll = self.expr(cx, rhs)?;
        Some(HLit::Guard(HExpr::new(
            HExprKind::In {
                elem: Box::new(elem),
                coll: Box::new(coll),
            },
            span,
        )))
    }

    // ------------------------------------------------------------------ patterns and expressions

    /// A pattern in an atom argument, a `let`, a generator or a `from` suffix.
    fn pattern(&mut self, cx: &mut RuleCx, e: &ast::Expr) -> Option<HPat> {
        match &e.kind {
            ExprKind::Wildcard => Some(HPat::Wild(e.span)),
            ExprKind::Path(path, targs) if targs.is_empty() && path.len() == 1 => {
                let name = path.first()?;
                if is_var_name(name.as_str()) {
                    return match Self::lookup_var(cx, name.name) {
                        Some(v) => Some(HPat::Var(v, e.span)),
                        None => {
                            self.error(
                                code!("BLS0500"),
                                e.span,
                                format!(
                                    "`{}` is not bound by a positive literal of this body (range restriction)",
                                    name.as_str()
                                ),
                            );
                            None
                        }
                    };
                }
                if name.as_str() == "None" {
                    return Some(HPat::Variant {
                        ty: TypeRef::Option,
                        variant: 0,
                        fields: Vec::new(),
                        span: e.span,
                    });
                }
                Some(HPat::Expr(self.expr(cx, e)?))
            }
            ExprKind::Path(path, targs) if targs.is_empty() && path.len() == 2 => {
                let (Some(en), Some(v)) = (path.first().copied(), path.get(1).copied()) else {
                    return None;
                };
                let (ty, variant, arity) = self.variant(cx.ms, en, v)?;
                if arity != 0 {
                    self.error(code!("BLS0301"), e.span, format!("variant `{}` has fields", v.as_str()));
                    return None;
                }
                Some(HPat::Variant {
                    ty: TypeRef::Known(ty),
                    variant,
                    fields: Vec::new(),
                    span: e.span,
                })
            }
            ExprKind::Tuple(elems) => {
                let mut ps = Vec::new();
                for el in elems {
                    ps.push(self.pattern(cx, el)?);
                }
                Some(HPat::Tuple(ps, e.span))
            }
            ExprKind::Call { callee, args } if self.is_constructor(cx, callee) => {
                let (ty, variant, arity) = match &callee.kind {
                    ExprKind::Path(p, _) if p.len() == 1 => (TypeRef::Option, 1, 1),
                    ExprKind::Path(p, _) => {
                        let (Some(en), Some(v)) = (p.first().copied(), p.get(1).copied()) else {
                            return None;
                        };
                        let (t, n, a) = self.variant(cx.ms, en, v)?;
                        (TypeRef::Known(t), n, a)
                    }
                    _ => return None,
                };
                if args.len() != arity {
                    self.error(code!("BLS0301"), e.span, format!("the variant takes {arity} field(s)"));
                    return None;
                }
                let mut fields = Vec::new();
                for a in args {
                    match a {
                        Arg::Pos(p) => fields.push(self.pattern(cx, p)?),
                        other => {
                            self.unsupported("LANG-023", "named fields in variant patterns", other.span());
                            return None;
                        }
                    }
                }
                Some(HPat::Variant {
                    ty,
                    variant,
                    fields,
                    span: e.span,
                })
            }
            ExprKind::StructLit { .. } => {
                self.unsupported("LANG-023", "struct patterns", e.span);
                None
            }
            _ => Some(HPat::Expr(self.expr(cx, e)?)),
        }
    }

    /// An enum variant `E::V`: its type, number and field count.
    fn variant(&mut self, ms: ScopeIdx, en: Ident, v: Ident) -> Option<(TypeId, u32, usize)> {
        let Some(ty) = self.enum_named(ms, en) else {
            self.error(code!("BLS0200"), en.span, format!("unknown enum `{}`", en.as_str()));
            return None;
        };
        let Some(TypeDef::Enum(def)) = self.hir.types.get(ty) else {
            return None;
        };
        match def.variants.iter().find(|x| x.name == v.name) {
            Some(x) => Some((ty, x.number, x.payload.len())),
            None => {
                self.error(
                    code!("BLS0200"),
                    v.span,
                    format!("`{}` has no variant `{}`", en.as_str(), v.as_str()),
                );
                None
            }
        }
    }

    /// Resolves an expression.
    pub(crate) fn expr(&mut self, cx: &mut RuleCx, e: &ast::Expr) -> Option<HExpr> {
        let span = e.span;
        let kind = match &e.kind {
            ExprKind::Lit(l) => match l {
                LitValue::Int { value, suffix: None } => HExprKind::IntLit(*value, false),
                LitValue::Int {
                    value,
                    suffix: Some(sfx),
                } => {
                    let ty = blossom_value::types::IntTy::ALL
                        .iter()
                        .copied()
                        .find(|t| t.name() == sfx.as_str())?;
                    HExprKind::TypedInt(*value, ty, false)
                }
                _ => {
                    let (v, t) = self.const_value(cx.ms, e, None)?;
                    HExprKind::Value(v, t)
                }
            },
            // `-1.5` is a constant.
            ExprKind::Prefix { op: PrefixOp::Neg, arg } if matches!(arg.kind, ExprKind::Lit(LitValue::Float(_))) => {
                let (v, t) = self.const_value(cx.ms, e, None)?;
                HExprKind::Value(v, t)
            }
            ExprKind::Prefix { op: PrefixOp::Neg, arg } if matches!(arg.kind, ExprKind::Lit(LitValue::Int { .. })) => {
                match &arg.kind {
                    ExprKind::Lit(LitValue::Int { value, suffix: None }) => HExprKind::IntLit(*value, true),
                    ExprKind::Lit(LitValue::Int {
                        value,
                        suffix: Some(sfx),
                    }) => {
                        let ty = blossom_value::types::IntTy::ALL
                            .iter()
                            .copied()
                            .find(|t| t.name() == sfx.as_str())?;
                        HExprKind::TypedInt(*value, ty, true)
                    }
                    _ => return None,
                }
            }
            ExprKind::Path(path, targs) => {
                if !targs.is_empty() {
                    self.unsupported("LANG-021", "explicit type arguments", span);
                    return None;
                }
                match path.as_slice() {
                    [name] => {
                        if is_var_name(name.as_str()) {
                            match Self::lookup_var(cx, name.name) {
                                Some(v) => HExprKind::Var(v),
                                None => {
                                    if let Some(rel) = self.callee_rel(cx, e)
                                        && self.rel_of(rel).cell
                                    {
                                        if self.impure(cx, span, "a relation") {
                                            return None;
                                        }
                                        // A cell's name is its lookup `c[]` (LANGUAGE §7.13).
                                        self.check_readable(cx, rel, span);
                                        return Some(HExpr::new(HExprKind::Lookup { rel, key: Vec::new() }, span));
                                    }
                                    if self.fn_param(cx.template, name.name).is_some()
                                        || self.scope(cx.ms).generic_fns.contains_key(&name.name)
                                    {
                                        self.error(
                                            code!("BLS0219"),
                                            span,
                                            format!(
                                                "`{}` is a function: call it, or pass it for a function parameter",
                                                name.as_str()
                                            ),
                                        );
                                    } else if self.scope(cx.ms).fns.contains_key(&name.name) {
                                        // A function as a value is the argument of a lattice operation
                                        // (`s.map(f)`, `s.filter(p)`, LANGUAGE §11.5).
                                        self.unsupported(
                                            "LANG-124",
                                            &format!(
                                                "functions passed as values (`{}`), as lattice operations take them",
                                                name.as_str()
                                            ),
                                            span,
                                        );
                                    } else if self.callee_rel(cx, e).is_some() {
                                        self.error(
                                            code!("BLS0202"),
                                            span,
                                            format!("`{}` is a relation, used here as a value", name.as_str()),
                                        );
                                    } else {
                                        self.error(
                                            code!("BLS0500"),
                                            span,
                                            format!(
                                                "`{}` is not bound by a positive literal of this body",
                                                name.as_str()
                                            ),
                                        );
                                    }
                                    return None;
                                }
                            }
                        } else if name.as_str() == "None" {
                            HExprKind::Variant {
                                ty: TypeRef::Option,
                                variant: 0,
                                fields: Vec::new(),
                            }
                        } else if let Some(i) = self.spec.as_ref().and_then(|s| s.nodes.get(&name.name)).copied() {
                            let t = self.node_type(None);
                            HExprKind::Value(Value::Node(blossom_value::time::NodeId(i)), t)
                        } else if let Some((v, t)) = self.lookup_value(cx.ms, name.name) {
                            HExprKind::Value(v, t)
                        } else {
                            self.error(code!("BLS0200"), span, format!("unknown name `{}`", name.as_str()));
                            return None;
                        }
                    }
                    [en, v] => {
                        let (ty, variant, arity) = self.variant(cx.ms, *en, *v)?;
                        if arity != 0 {
                            self.error(code!("BLS0301"), span, format!("variant `{}` has fields", v.as_str()));
                            return None;
                        }
                        HExprKind::Variant {
                            ty: TypeRef::Known(ty),
                            variant,
                            fields: Vec::new(),
                        }
                    }
                    _ => {
                        self.unsupported("LANG-001", "long paths", span);
                        return None;
                    }
                }
            }
            ExprKind::Call { callee, args } => return self.call(cx, callee, args, span),
            ExprKind::Method { receiver, name, args } => return self.method(cx, receiver, *name, args, span),
            // `reveal!(x)`: the exact read of a lattice value.
            ExprKind::Bang { name, args, clauses } if name.as_str() == "reveal" && clauses.is_empty() => {
                let [Arg::Pos(x)] = args.as_slice() else {
                    self.error(code!("BLS0301"), span, "`reveal!` takes one value");
                    return None;
                };
                HExprKind::Method {
                    recv: Box::new(self.expr(cx, x)?),
                    name: Symbol::intern("reveal"),
                    banged: true,
                    args: Vec::new(),
                }
            }
            ExprKind::Bang { name, .. } => {
                self.error(
                    code!("BLS0202"),
                    span,
                    format!("`{}!` is allowed only as a head or view aggregate here", name.as_str()),
                );
                return None;
            }
            ExprKind::Field { base, name } => {
                if self.callee_rel(cx, e).is_some() {
                    self.error(code!("BLS0202"), span, "an instance relation used as a value");
                    return None;
                }
                HExprKind::Field {
                    base: Box::new(self.expr(cx, base)?),
                    name: name.name,
                    index: None,
                }
            }
            ExprKind::TupleIndex { base, index } => HExprKind::TupleIndex {
                base: Box::new(self.expr(cx, base)?),
                index: *index,
            },
            ExprKind::Index { base, index } => {
                let Some((rel, None)) = self.atom_parts(cx, base) else {
                    self.unsupported(
                        "LANG-091",
                        "indexing values (only a relation's cell `r[k]` is read by index)",
                        span,
                    );
                    return None;
                };
                let keys: Vec<&ast::Expr> = match &index.kind {
                    ExprKind::Tuple(es) if !es.is_empty() => es.iter().collect(),
                    ExprKind::Tuple(_) => Vec::new(),
                    _ => vec![index.as_ref()],
                };
                if self.impure(cx, span, "a relation") {
                    return None;
                }
                let mut key = Vec::new();
                for k in keys {
                    key.push(self.expr(cx, k)?);
                }
                self.check_readable(cx, rel, span);
                HExprKind::Lookup { rel, key }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                if matches!(op, BinOp::In) {
                    // A membership test in a value (LANGUAGE §9.4); roles and relations are tested as literals.
                    let names_rel = self
                        .atom_parts(cx, rhs)
                        .is_some_and(|(r, a)| a.is_none() && !self.rel_of(r).cell);
                    let names_role = matches!(&rhs.kind, ExprKind::Path(p, t) if t.is_empty()
                        && p.len() == 1 && p.first().is_some_and(|n| self.role_named(cx.ms, n.name).is_some()));
                    if names_rel
                        || names_role
                        || matches!(&rhs.kind, ExprKind::Binary { op, .. } if range_kind(*op).is_some())
                    {
                        self.unsupported(
                            "LANG-088",
                            "membership in a role, relation or range inside an expression (write it as a literal)",
                            span,
                        );
                        return None;
                    }
                    let elem = self.expr(cx, lhs)?;
                    let coll = self.expr(cx, rhs)?;
                    return Some(HExpr::new(
                        HExprKind::In {
                            elem: Box::new(elem),
                            coll: Box::new(coll),
                        },
                        span,
                    ));
                }
                if range_kind(*op).is_some() {
                    self.unsupported("LANG-092", "ranges as values", span);
                    return None;
                }
                HExprKind::Binary {
                    op: *op,
                    lhs: Box::new(self.expr(cx, lhs)?),
                    rhs: Box::new(self.expr(cx, rhs)?),
                }
            }
            ExprKind::Prefix { op, arg } => HExprKind::Prefix {
                op: *op,
                arg: Box::new(self.expr(cx, arg)?),
            },
            ExprKind::Cast { expr, ty } => {
                let ty = self.resolve_type(cx.ms, ty)?;
                HExprKind::Cast {
                    expr: Box::new(self.expr(cx, expr)?),
                    ty,
                }
            }
            ExprKind::Ascribe { expr, ty } => {
                let ty = self.resolve_type(cx.ms, ty)?;
                HExprKind::Ascribe {
                    expr: Box::new(self.expr(cx, expr)?),
                    ty,
                }
            }
            ExprKind::Tuple(elems) => {
                if elems.is_empty() {
                    let t = self.intern_type(TypeDef::Unit, span);
                    HExprKind::Value(Value::Unit, t)
                } else {
                    let mut es = Vec::new();
                    for el in elems {
                        es.push(self.expr(cx, el)?);
                    }
                    HExprKind::Tuple(es)
                }
            }
            ExprKind::Vec(es) | ExprKind::Set(es) => {
                let mut elems = Vec::new();
                for x in es {
                    elems.push(self.expr(cx, x)?);
                }
                HExprKind::Collection {
                    kind: if matches!(&e.kind, ExprKind::Vec(_)) {
                        CollectionKind::Vec
                    } else {
                        CollectionKind::Set
                    },
                    elems,
                }
            }
            ExprKind::Map(pairs) => {
                let mut elems = Vec::new();
                for (k, v) in pairs {
                    elems.push(self.expr(cx, k)?);
                    elems.push(self.expr(cx, v)?);
                }
                HExprKind::Collection {
                    kind: CollectionKind::Map,
                    elems,
                }
            }
            ExprKind::If { cond, then, els } => {
                let Some(els) = els else {
                    // The parser rejects an `if` value without `else` (BLS0109).
                    self.bugs.push(blossom_base::internal_error!(
                        "an `if` value without `else` passed the parser"
                    ));
                    return None;
                };
                HExprKind::If {
                    cond: Box::new(self.expr(cx, cond)?),
                    then: Box::new(self.expr(cx, then)?),
                    els: Box::new(self.expr(cx, els)?),
                }
            }
            ExprKind::Match { scrut, arms } => {
                let scrut = self.expr(cx, scrut)?;
                let mut out = Vec::new();
                for arm in arms {
                    cx.frames.push(BTreeMap::new());
                    if let Some(dup) = first_duplicate(&arm.pat, &mut BTreeSet::new()) {
                        self.error(
                            code!("BLS0201"),
                            dup.span,
                            format!("`{}` is bound twice by this pattern", dup.as_str()),
                        );
                    }
                    self.declare_arm_pattern(cx, &arm.pat);
                    let pat = self.pattern(cx, &arm.pat);
                    let guard = arm.guard.as_ref().map(|g| self.expr(cx, g));
                    let body = self.expr(cx, &arm.body);
                    cx.frames.pop();
                    let guard = match guard {
                        Some(g) => Some(g?),
                        None => None,
                    };
                    out.push((pat?, guard, body?));
                }
                HExprKind::Match {
                    scrut: Box::new(scrut),
                    arms: out,
                }
            }
            ExprKind::StructLit { path, fields, base } => {
                let [name] = path.as_slice() else {
                    self.unsupported("LANG-023", "qualified struct names", span);
                    return None;
                };
                let Some(ty) = self
                    .struct_named(cx.ms, *name)
                    .or_else(|| self.product_named(cx.ms, *name))
                else {
                    self.error(
                        code!("BLS0200"),
                        name.span,
                        format!("unknown struct `{}`", name.as_str()),
                    );
                    return None;
                };
                // A struct's fields, or a product lattice's (whose literal may leave fields out: they are ⊥,
                // LANGUAGE §11.2), with the lattice type of each.
                let (field_names, product): (Vec<Symbol>, Option<Vec<TypeId>>) = match self.hir.types.get(ty).cloned() {
                    Some(TypeDef::Struct(def)) => (def.fields.iter().map(|f| f.name).collect(), None),
                    Some(TypeDef::Lattice(_)) => match self.hir.lattice_of(ty) {
                        Some((_, blossom_ir::core::LatticeCtor::Product { fields, .. })) => {
                            let fields = fields.clone();
                            let mut tys = Vec::new();
                            for (_, l) in &fields {
                                tys.push(self.intern_type(TypeDef::Lattice(*l), span));
                            }
                            (fields.iter().map(|f| f.0).collect(), Some(tys))
                        }
                        _ => return None,
                    },
                    _ => return None,
                };
                let mut slots: Vec<Option<HExpr>> = vec![None; field_names.len()];
                for (f, v) in fields {
                    let Some(i) = field_names.iter().position(|d| *d == f.name) else {
                        self.error(
                            code!("BLS0302"),
                            f.span,
                            format!("`{}` has no field `{}`", name.as_str(), f.as_str()),
                        );
                        return None;
                    };
                    let value = match v {
                        Some(v) => self.expr(cx, v)?,
                        None => {
                            let pun = ast::Expr {
                                kind: ExprKind::Path(vec![*f], Vec::new()),
                                span: f.span,
                            };
                            self.expr(cx, &pun)?
                        }
                    };
                    if let Some(slot) = slots.get_mut(i) {
                        *slot = Some(value);
                    }
                }
                // `..base`: the base is evaluated once, into a variable of the struct's type, and gives each field
                // not written.
                let from = match base {
                    Some(b) => {
                        let value = self.expr(cx, b)?;
                        let var = self.new_var(cx, Symbol::intern("struct$base"), b.span, true);
                        Some((var, value))
                    }
                    None => None,
                };
                let mut out = Vec::new();
                for (i, (s, field)) in slots.into_iter().zip(&field_names).enumerate() {
                    match (s, &from) {
                        (Some(v), _) => out.push(v),
                        (None, Some((var, value))) => out.push(HExpr::new(
                            HExprKind::Field {
                                base: Box::new(HExpr::new(HExprKind::Var(*var), value.span)),
                                name: *field,
                                index: None,
                            },
                            value.span,
                        )),
                        (None, None) => match product.as_ref().and_then(|tys| tys.get(i)) {
                            Some(t) => out.push(HExpr::new(HExprKind::Bottom(*t), span)),
                            None => {
                                self.error(
                                    code!("BLS0303"),
                                    span,
                                    format!("field `{field}` of `{}` is not given", name.as_str()),
                                );
                                return None;
                            }
                        },
                    }
                }
                if let Some((var, value)) = from {
                    let vspan = value.span;
                    let witness = HExpr::new(HExprKind::Var(var), vspan);
                    let lit = HExpr::new(
                        HExprKind::Struct {
                            ty,
                            fields: out,
                            base: Some(Box::new(witness)),
                        },
                        span,
                    );
                    // A `match` with one arm binds the base (a `let` expression is for function bodies only).
                    return Some(HExpr::new(
                        HExprKind::Match {
                            scrut: Box::new(value),
                            arms: vec![(HPat::Var(var, vspan), None, lit)],
                        },
                        span,
                    ));
                }
                HExprKind::Struct {
                    ty,
                    fields: out,
                    base: None,
                }
            }
            ExprKind::Wildcard => {
                self.error(code!("BLS0500"), span, "`_` is a pattern, not a value");
                return None;
            }
            ExprKind::SelfNode => {
                if let Some(v) = cx.receiver {
                    HExprKind::Var(v)
                } else {
                    if self.impure(cx, span, "`self`") {
                        return None;
                    }
                    HExprKind::SelfNode
                }
            }
            ExprKind::Block { lets, result } => {
                if !cx.in_fn {
                    self.error(
                        code!("BLS0214"),
                        span,
                        "a block with `let`s is allowed only in a function body (LANGUAGE §16.1)",
                    );
                    return None;
                }
                return self.let_block(cx, lets, result);
            }
            ExprKind::Closure { .. } => {
                self.error(
                    code!("BLS0214"),
                    span,
                    "a closure is allowed only as a combinator's argument in a function body (LANGUAGE §16.1)",
                );
                return None;
            }
            // Function bodies have no `?` left (`ast::desugar`); a rule body fails a match without one.
            ExprKind::Try(_) => {
                self.error(
                    code!("BLS0218"),
                    span,
                    "`?` returns early from a function: in a rule body, `let Some(x) = e` already derives nothing \
                     when `e` is `None`",
                );
                return None;
            }
        };
        Some(HExpr::new(kind, span))
    }

    fn call(&mut self, cx: &mut RuleCx, callee: &ast::Expr, args: &[Arg], span: Span) -> Option<HExpr> {
        let mut pos = Vec::new();
        for a in args {
            match a {
                Arg::Pos(e) => pos.push(e),
                other => {
                    self.error(code!("BLS0302"), other.span(), "function arguments are positional");
                    return None;
                }
            }
        }
        if self.callee_rel(cx, callee).is_some() {
            self.error(code!("BLS0202"), span, "a relation used as a function");
            return None;
        }
        let ExprKind::Path(path, _) = &callee.kind else {
            self.unsupported("LANG-180", "calls of computed functions", span);
            return None;
        };
        match path.as_slice() {
            [name] if name.as_str() == "Some" => {
                if pos.len() != 1 {
                    self.error(code!("BLS0301"), span, "`Some` takes one value");
                    return None;
                }
                let v = self.expr(cx, pos.first()?)?;
                Some(HExpr {
                    ty: None,
                    kind: HExprKind::Variant {
                        ty: TypeRef::Option,
                        variant: 1,
                        fields: vec![v],
                    },
                    span,
                })
            }
            [name] if matches!(name.as_str(), "now" | "tick") && pos.is_empty() && cx.in_fn => {
                self.impure(cx, span, &format!("`{}()`", name.as_str()));
                None
            }
            [name] if name.as_str() == "now" && pos.is_empty() => Some(HExpr {
                ty: None,
                kind: HExprKind::Now,
                span,
            }),
            [name] if name.as_str() == "tick" && pos.is_empty() => Some(HExpr {
                ty: None,
                kind: HExprKind::Tick,
                span,
            }),
            [en, v] if self.enum_named(cx.ms, *en).is_some() => {
                let (ty, variant, arity) = self.variant(cx.ms, *en, *v)?;
                if pos.len() != arity {
                    self.error(code!("BLS0301"), span, format!("the variant takes {arity} field(s)"));
                    return None;
                }
                let mut fields = Vec::new();
                for p in pos {
                    fields.push(self.expr(cx, p)?);
                }
                Some(HExpr {
                    ty: None,
                    kind: HExprKind::Variant {
                        ty: TypeRef::Known(ty),
                        variant,
                        fields,
                    },
                    span,
                })
            }
            [l, f] if LatCtorKind::named(l.as_str()).is_some() && matches!(f.as_str(), "of" | "bot") => {
                let kind = LatCtorKind::named(l.as_str())?;
                let bot = f.as_str() == "bot";
                let want = match (bot, kind) {
                    (true, _) => 0,
                    (false, LatCtorKind::Map) => 2,
                    (false, _) => 1,
                };
                if pos.len() != want {
                    self.error(
                        code!("BLS0301"),
                        span,
                        format!("`{}::{}` takes {want} value(s)", l.as_str(), f.as_str()),
                    );
                    return None;
                }
                let mut xs = Vec::new();
                for p in pos {
                    xs.push(self.expr(cx, p)?);
                }
                Some(HExpr::new(HExprKind::LatCtor { kind, bot, args: xs }, span))
            }
            [name] if matches!(name.as_str(), "rand" | "rand_range" | "rand_float" | "majority") && cx.in_fn => {
                let what = if name.as_str() == "majority" {
                    "a role's members"
                } else {
                    "randomness"
                };
                self.impure(cx, span, what);
                None
            }
            [name] if name.as_str() == "range" => {
                let [lo, hi] = pos.as_slice() else {
                    self.error(code!("BLS0301"), span, "`range` takes `lo` and `hi`");
                    return None;
                };
                let lo = self.expr(cx, lo);
                let hi = self.expr(cx, hi);
                Some(HExpr::new(
                    HExprKind::Builtin {
                        f: Builtin::Lib(blossom_ir::core::LibFn::Range),
                        args: vec![lo?, hi?],
                    },
                    span,
                ))
            }
            // A format's decoder and encoder (`Name::decode`, `Name::encode`, LANGUAGE §16.7).
            [ty, f]
                if let Some(id) = self
                    .scope(cx.ms)
                    .fns
                    .get(&Symbol::intern(&format!("{}::{}", ty.as_str(), f.as_str())))
                    .copied() =>
            {
                let arity = self.hir.fns.get(id.index()).map_or(0, |h| h.params.len());
                if pos.len() != arity {
                    self.error(
                        code!("BLS0301"),
                        span,
                        format!(
                            "`{}::{}` takes {arity} argument(s), {} given",
                            ty.as_str(),
                            f.as_str(),
                            pos.len()
                        ),
                    );
                    return None;
                }
                let mut xs = Vec::new();
                for p in &pos {
                    xs.push(self.expr(cx, p)?);
                }
                cx.calls.insert(id);
                Some(HExpr::new(HExprKind::Call { f: id, args: xs }, span))
            }
            [name] if let Some(param) = self.fn_param(cx.template, name.name) => {
                self.call_param(cx, *name, param, &pos, span)
            }
            [name] if let Some(g) = self.scope(cx.ms).generic_fns.get(&name.name).copied() => {
                self.generic_call(cx, *name, g, &pos, span)
            }
            [name] if let Some(f) = self.scope(cx.ms).fns.get(&name.name).copied() => {
                let arity = self.hir.fns.get(f.index()).map_or(0, |h| h.params.len());
                if pos.len() != arity {
                    self.error(
                        code!("BLS0301"),
                        span,
                        format!("`{}` takes {arity} argument(s), {} given", name.as_str(), pos.len()),
                    );
                    return None;
                }
                let mut xs = Vec::new();
                for p in pos {
                    xs.push(self.expr(cx, p)?);
                }
                cx.calls.insert(f);
                Some(HExpr::new(HExprKind::Call { f, args: xs }, span))
            }
            [ty, f] if ty.as_str() == "Duration" && f.as_str() == "from_millis" => {
                let [n] = pos.as_slice() else {
                    self.error(code!("BLS0301"), span, "`Duration::from_millis` takes 1 argument");
                    return None;
                };
                let x = self.expr(cx, n)?;
                Some(HExpr::new(
                    HExprKind::Builtin {
                        f: Builtin::Lib(blossom_ir::core::LibFn::DurationFromMillis),
                        args: vec![x],
                    },
                    span,
                ))
            }
            [ty, f] if ty.as_str() == "Blob" && f.as_str() == "of" => {
                if pos.len() != 1 {
                    self.error(
                        code!("BLS0301"),
                        span,
                        format!("`Blob::of` takes 1 argument, {} given", pos.len()),
                    );
                    return None;
                }
                let mut xs = Vec::new();
                for p in pos {
                    xs.push(self.expr(cx, p)?);
                }
                Some(HExpr::new(
                    HExprKind::Builtin {
                        f: Builtin::Lib(blossom_ir::core::LibFn::BlobOf),
                        args: xs,
                    },
                    span,
                ))
            }
            [ty, f] if ty.as_str() == "Bytes" => {
                use blossom_ir::core::LibFn;
                let n = f.as_str();
                let (lib, arity) = match n {
                    "uvarint" => (LibFn::BytesUvarint, 1),
                    "varint" => (LibFn::BytesVarint, 1),
                    "empty" => (LibFn::BytesEmpty, 0),
                    "join" => (LibFn::BytesJoin, 1),
                    _ => match n.strip_prefix("from_").and_then(byte_int) {
                        Some(it) => (LibFn::BytesFrom(it), 1),
                        None => {
                            self.unsupported("LANG-180", &format!("`Bytes::{n}`"), span);
                            return None;
                        }
                    },
                };
                if pos.len() != arity {
                    self.error(
                        code!("BLS0301"),
                        span,
                        format!("`Bytes::{n}` takes {arity} argument(s), {} given", pos.len()),
                    );
                    return None;
                }
                let mut xs = Vec::new();
                for p in pos {
                    xs.push(self.expr(cx, p)?);
                }
                Some(HExpr::new(
                    HExprKind::Builtin {
                        f: Builtin::Lib(lib),
                        args: xs,
                    },
                    span,
                ))
            }
            [name] if name.as_str() == "error" => {
                let [msg] = pos.as_slice() else {
                    self.error(code!("BLS0301"), span, "`error` takes a message");
                    return None;
                };
                let m = self.expr(cx, msg)?;
                Some(HExpr::new(
                    HExprKind::Builtin {
                        f: Builtin::Error,
                        args: vec![m],
                    },
                    span,
                ))
            }
            [name] if name.as_str() == "hash64" => {
                let [x] = pos.as_slice() else {
                    self.error(code!("BLS0301"), span, "`hash64` takes one value");
                    return None;
                };
                let x = self.expr(cx, x)?;
                Some(HExpr::new(
                    HExprKind::Builtin {
                        f: Builtin::Hash64,
                        args: vec![x],
                    },
                    span,
                ))
            }
            [name] if name.as_str() == "rand" => {
                // The key makes the draw stable (the same value for the same key within a tick, LANG-175).
                if pos.is_empty() {
                    self.error(code!("BLS0301"), span, "`rand` takes a key");
                    return None;
                }
                let mut xs = Vec::new();
                for p in pos {
                    xs.push(self.expr(cx, p)?);
                }
                Some(HExpr::new(
                    HExprKind::Builtin {
                        f: Builtin::Rand,
                        args: xs,
                    },
                    span,
                ))
            }
            [name] if name.as_str() == "rand_float" => {
                // As `rand`: the key makes the draw stable (LANG-175).
                if pos.is_empty() {
                    self.error(code!("BLS0301"), span, "`rand_float` takes a key");
                    return None;
                }
                let mut xs = Vec::new();
                for p in pos {
                    xs.push(self.expr(cx, p)?);
                }
                Some(HExpr::new(
                    HExprKind::Builtin {
                        f: Builtin::RandFloat,
                        args: xs,
                    },
                    span,
                ))
            }
            [name] if matches!(name.as_str(), "abs" | "min" | "max" | "clamp") => {
                use blossom_ir::core::LibFn;
                let (f, want) = match name.as_str() {
                    "abs" => (LibFn::Abs, 1),
                    "min" => (LibFn::Min, 2),
                    "max" => (LibFn::Max, 2),
                    _ => (LibFn::Clamp, 3),
                };
                if pos.len() != want {
                    self.error(
                        code!("BLS0301"),
                        span,
                        format!("`{}` takes {want} argument(s)", name.as_str()),
                    );
                    return None;
                }
                let mut xs = Vec::new();
                for p in pos {
                    xs.push(self.expr(cx, p)?);
                }
                Some(HExpr::new(
                    HExprKind::Builtin {
                        f: Builtin::Lib(f),
                        args: xs,
                    },
                    span,
                ))
            }
            [name] if name.as_str() == "rand_range" => {
                // The key makes the draw stable (the same value for the same key within a tick, LANG-175).
                if pos.len() < 3 {
                    self.error(code!("BLS0301"), span, "`rand_range` takes `lo`, `hi` and a key");
                    return None;
                }
                let mut xs = Vec::new();
                for p in pos {
                    xs.push(self.expr(cx, p)?);
                }
                Some(HExpr::new(
                    HExprKind::Builtin {
                        f: Builtin::RandRange,
                        args: xs,
                    },
                    span,
                ))
            }
            [name] if name.as_str() == "majority" => {
                let [set, domain] = pos.as_slice() else {
                    self.error(code!("BLS0301"), span, "`majority` takes a set of nodes and a role");
                    return None;
                };
                let ExprKind::Path(dp, dt) = &domain.kind else {
                    self.error(code!("BLS0301"), domain.span, "the domain of `majority` is a role");
                    return None;
                };
                let role = match dp.as_slice() {
                    [r] if dt.is_empty() => self.role_named(cx.ms, r.name),
                    _ => None,
                };
                let Some(role) = role else {
                    self.unsupported(
                        "LANG-113",
                        "`majority` over a domain other than a role (a closed unary relation)",
                        domain.span,
                    );
                    return None;
                };
                if !self.static_members(role, domain.span, "`majority(s, R)`") {
                    return None;
                }
                let s = self.expr(cx, set)?;
                Some(HExpr::new(
                    HExprKind::Builtin {
                        f: Builtin::Majority(role),
                        args: vec![s],
                    },
                    span,
                ))
            }
            [name] if self.scope(cx.ms).broken.contains(&name.name) => None,
            _ => {
                let names: Vec<&str> = path.iter().map(Ident::as_str).collect();
                self.unsupported("LANG-180", &format!("calls of `{}`", names.join("::")), span);
                None
            }
        }
    }

    fn method(
        &mut self,
        cx: &mut RuleCx,
        receiver: &ast::Expr,
        name: Ident,
        args: &[Arg],
        span: Span,
    ) -> Option<HExpr> {
        // `R.size()`.
        if let ExprKind::Path(p, t) = &receiver.kind
            && t.is_empty()
            && let [r] = p.as_slice()
            && let Some(role) = self.role_named(cx.ms, r.name)
        {
            if self.impure(cx, span, "a role's members") {
                return None;
            }
            if !self.static_members(role, r.span, "`R.size()`") {
                return None;
            }
            if name.as_str() == "size" && args.is_empty() {
                return Some(HExpr {
                    ty: None,
                    kind: HExprKind::Builtin {
                        f: Builtin::RoleSize(role),
                        args: Vec::new(),
                    },
                    span,
                });
            }
            self.unsupported("LANG-153", &format!("role method `{}`", name.as_str()), span);
            return None;
        }
        match name.as_str() {
            "len" if args.is_empty() => {
                let r = self.expr(cx, receiver)?;
                Some(HExpr {
                    ty: None,
                    kind: HExprKind::Builtin {
                        f: Builtin::Len,
                        args: vec![r],
                    },
                    span,
                })
            }
            other => {
                // Resolved by the receiver's type (lattice operations, LANGUAGE §11.5).
                let (name, banged) = match other.strip_suffix('!') {
                    Some(n) => (n, true),
                    None => (other, false),
                };
                let recv = self.expr(cx, receiver)?;
                let mut xs = Vec::new();
                for a in args {
                    let Arg::Pos(x) = a else {
                        self.error(code!("BLS0302"), a.span(), "method arguments are positional");
                        return None;
                    };
                    // A closure is a combinator's argument (LANGUAGE §16.1); type checking checks the method takes one.
                    match &x.kind {
                        ExprKind::Closure { params, body } => xs.push(self.closure(cx, params, body, x.span)?),
                        _ => xs.push(self.expr(cx, x)?),
                    }
                }
                Some(HExpr::new(
                    HExprKind::Method {
                        recv: Box::new(recv),
                        name: Symbol::intern(name),
                        banged,
                        args: xs,
                    },
                    span,
                ))
            }
        }
    }

    // ------------------------------------------------------------------ rules

    pub(crate) fn handler(&mut self, s: ScopeIdx, h: &'t ast::Handler, placement: Option<HRoleId>) {
        if let Some(l) = h.label {
            *self.handler_labels.entry((s, l.name)).or_insert(0) += 1;
        }
        if h.monotone {
            self.unsupported("ANA-020", "`monotone` assertions", h.span);
        }
        let mut cx = self.rule_cx(s, placement);
        cx.choice_allowed = h.label.is_some();
        cx.label = h.label.map(|l| l.name);
        let header = self.body(&mut cx, &h.header);
        cx.choice_allowed = false;
        let stmts = self.stmts(&mut cx, &h.block.stmts);
        let text = self.normalized(h.header.span);
        self.hir.handlers.push(HHandler {
            scope: cx.scope,
            label: h.label.map(|l| l.name),
            trigger: h.trigger,
            kind: HandlerKind::Plain,
            header,
            stmts,
            role: placement,
            text,
            span: h.span,
        });
    }

    pub(crate) fn bootstrap(
        &mut self,
        s: ScopeIdx,
        fresh: bool,
        block: &'t ast::Block,
        placement: Option<HRoleId>,
        span: Span,
    ) {
        let mut cx = self.rule_cx(s, placement);
        cx.plain_bootstrap = !fresh;
        let boot = self.builtin(super::BuiltinRel::Boot, span);
        let mut lits = vec![HLit::Atom(HAtom {
            rel: boot,
            args: Vec::new(),
            from: None,
            span,
        })];
        // `bootstrap fresh` runs only on a node's very first start: its header is `boot(), not recovered()`.
        if fresh {
            let recovered = self.builtin(super::BuiltinRel::Recovered, span);
            lits.push(HLit::Not(HAtom {
                rel: recovered,
                args: Vec::new(),
                from: None,
                span,
            }));
        }
        let header = HBody { lits, span: None };
        let stmts = self.stmts(&mut cx, &block.stmts);
        self.hir.handlers.push(HHandler {
            scope: cx.scope,
            label: None,
            trigger: ast::Trigger::On,
            kind: if fresh {
                HandlerKind::BootstrapFresh
            } else {
                HandlerKind::Bootstrap
            },
            header,
            stmts,
            role: placement,
            text: String::new(),
            span,
        });
    }

    fn stmts(&mut self, cx: &mut RuleCx, stmts: &[Stmt]) -> Vec<HStmt> {
        let mut out = Vec::new();
        for st in stmts {
            match st {
                Stmt::Verb(v) if Self::sugared(v) => {
                    // Child heads, spreads and trees (docs/design/SUGAR.md): plain statements, resolved as written.
                    if let Some(plain) = self.expand(cx, v) {
                        out.extend(self.stmts(cx, &plain));
                    }
                }
                Stmt::Verb(v) => {
                    if let Some(h) = self.verb_stmt(cx, v) {
                        out.push(HStmt::Verb(h));
                    }
                }
                Stmt::For { cond, block, span, .. } => {
                    cx.frames.push(BTreeMap::new());
                    let c = self.body(cx, cond);
                    let inner = self.stmts(cx, &block.stmts);
                    cx.frames.pop();
                    out.push(HStmt::Block {
                        kind: BlockKind::For,
                        cond: c,
                        stmts: inner,
                        text: self.normalized(cond.span),
                        span: *span,
                        refined: Vec::new(),
                    });
                }
                Stmt::If {
                    cond, then, els, span, ..
                } => self.if_stmt(cx, cond, then, els.as_deref(), *span, &mut out),
                Stmt::Call(e) => {
                    if let Some(f) = self.fragment_call(cx, e) {
                        out.extend(self.stmts(cx, std::slice::from_ref(&f)));
                    }
                }
                Stmt::Fragment {
                    name,
                    args,
                    params,
                    body,
                    text,
                    span,
                } => {
                    if self.fragments_expanding.contains(&name.name) {
                        self.error(
                            code!("BLS0433"),
                            *span,
                            format!("the fragment `{}` calls itself", name.as_str()),
                        );
                        continue;
                    }
                    // The arguments' values, in the caller's scope; then, with only those visible, the parameters.
                    cx.frames.push(BTreeMap::new());
                    let c0 = self.body(cx, args);
                    let saved = cx.barrier;
                    cx.barrier = cx.frames.len() - 1;
                    cx.frames.push(BTreeMap::new());
                    let c1 = self.body(cx, params);
                    self.fragments_expanding.push(name.name);
                    let inner = self.stmts(cx, &body.stmts);
                    self.fragments_expanding.pop();
                    cx.frames.pop();
                    cx.barrier = saved;
                    cx.frames.pop();
                    out.push(HStmt::Block {
                        kind: BlockKind::For,
                        cond: c0,
                        stmts: vec![HStmt::Block {
                            kind: BlockKind::For,
                            cond: c1,
                            stmts: inner,
                            text: format!("{text} $params"),
                            span: *span,
                            refined: Vec::new(),
                        }],
                        text: format!("{text} $args"),
                        span: *span,
                        refined: Vec::new(),
                    });
                }
            }
        }
        out
    }

    fn if_stmt(
        &mut self,
        cx: &mut RuleCx,
        cond: &ast::Body,
        then: &ast::Block,
        els: Option<&ast::Else>,
        span: Span,
        out: &mut Vec<HStmt>,
    ) {
        cx.frames.push(BTreeMap::new());
        let c = self.body(cx, cond);
        let inner = self.stmts(cx, &then.stmts);
        cx.frames.pop();
        let text = self.normalized(cond.span);
        let guard = match (&c.lits.as_slice(), els) {
            ([HLit::Guard(g)], Some(_)) => Some(g.clone()),
            (_, Some(_)) => {
                self.error(
                    code!("BLS0409"),
                    span,
                    "`else` needs an `if` whose condition is a single scalar guard; write `if not r(x) { … }` for the relational case",
                );
                None
            }
            _ => None,
        };
        out.push(HStmt::Block {
            kind: BlockKind::If,
            cond: c,
            stmts: inner,
            text: text.clone(),
            span,
            refined: Vec::new(),
        });
        let (Some(g), Some(els)) = (guard, els) else { return };
        let negated = HBody {
            lits: vec![HLit::Guard(HExpr {
                ty: None,
                span: g.span,
                kind: HExprKind::Prefix {
                    op: PrefixOp::Not,
                    arg: Box::new(g),
                },
            })],
            span: Some(cond.span),
        };
        let stmts = match els {
            ast::Else::Block(b) => {
                cx.frames.push(BTreeMap::new());
                let s = self.stmts(cx, &b.stmts);
                cx.frames.pop();
                s
            }
            ast::Else::If(st) => self.stmts(cx, std::slice::from_ref(st.as_ref())),
        };
        out.push(HStmt::Block {
            kind: BlockKind::Else,
            cond: negated,
            stmts,
            text: format!("not ({text})"),
            span,
            refined: Vec::new(),
        });
    }

    fn verb_stmt(&mut self, cx: &mut RuleCx, v: &ast::VerbStmt) -> Option<HVerbStmt> {
        // Every other attribute of a statement was reported when the file was loaded (`ast::attrs`).
        let allow_self_negation = v.attrs.iter().any(|a| {
            a.name.as_str() == "allow"
                && a.value.is_none()
                && !a.args.is_empty()
                && a.args.iter().all(|x| matches!(x, Arg::Pos(e) if matches!(&e.kind, ExprKind::Path(p, t) if t.is_empty() && p.len() == 1 && p.first().is_some_and(|n| n.as_str() == "self_negation"))))
        });
        if v.resolve.is_some() {
            self.unsupported("LANG-117", "`resolve` policies on statements", v.span);
            return None;
        }
        if v.weight.is_some() {
            self.unsupported("LANG-138", "weighted statements", v.span);
            return None;
        }
        let target = self.write_target(cx, &v.head.rel, v.verb, v.head.span)?;
        let rel = self.rel_of(target).clone();
        match v.verb {
            Verb::Seal => {
                self.unsupported("LANG-207", "`seal`", v.span);
                return None;
            }
            Verb::Upsert if rel.key.is_none() => {
                self.error(
                    code!("BLS0400"),
                    v.span,
                    format!("`upsert` needs a keyed table; `{}` has no key", rel.name),
                );
                return None;
            }
            _ => {}
        }
        let args = self.head_args(cx, &rel, &v.head.args, v.head.span)?;
        let to = match (&v.to, v.verb, &rel.kind) {
            (Some(to), Verb::Send, HRelKind::Channel(ch)) => {
                if ch.loopback || ch.dest_col.is_some() {
                    self.error(
                        code!("BLS0403"),
                        to.span,
                        "a loopback or column-form channel takes no `to`: the destination is self or the `@` column",
                    );
                    return None;
                }
                Some(self.expr(cx, to)?)
            }
            (None, Verb::Send, HRelKind::Channel(ch)) => {
                if !ch.loopback && ch.dest_col.is_none() {
                    self.error(
                        code!("BLS0403"),
                        v.span,
                        "`send` into a direction-form channel needs `to d`",
                    );
                    return None;
                }
                None
            }
            (Some(to), Verb::Send, HRelKind::Stream(_)) => {
                self.error(
                    code!("BLS0403"),
                    to.span,
                    "a request to the host takes no `to`: the connection is its first column",
                );
                return None;
            }
            (None, Verb::Send, HRelKind::Stream(HStreamRel::Host(_))) => None,
            (_, Verb::Send, _) => {
                self.error(
                    code!("BLS0400"),
                    v.span,
                    format!("`send` writes channels; `{}` is not one", rel.name),
                );
                return None;
            }
            (Some(to), _, _) => {
                self.error(code!("BLS0403"), to.span, "only `send` takes `to`");
                return None;
            }
            (None, _, HRelKind::Channel(_)) => {
                self.error(
                    code!("BLS0400"),
                    v.span,
                    format!("a channel is written only with `send`; `{}` is a channel", rel.name),
                );
                return None;
            }
            (None, _, _) => None,
        };
        let rank = match (&rel.prefer, v.verb) {
            (Some(rules), Verb::Next | Verb::Upsert) => {
                let listed = cx
                    .label
                    .zip(cx.label.and_then(|l| rules.iter().position(|(n, _)| *n == l)));
                Some(match listed {
                    Some((l, i)) => {
                        self.prefer_writers.insert((target, l));
                        HRank::Listed(u32::try_from(i).ok()?)
                    }
                    None => HRank::Unlisted,
                })
            }
            _ => None,
        };
        Some(HVerbStmt {
            verb: v.verb,
            target,
            args,
            to,
            allow_self_negation,
            rank,
            text: match &v.tag {
                Some(tag) => format!("{} {tag}", self.normalized(v.span)),
                None => self.normalized(v.span),
            },
            span: v.span,
        })
    }

    /// The relation a statement writes, after interposition, with the verb × collection legality (LANGUAGE §12).
    fn write_target(&mut self, cx: &RuleCx, path: &[Ident], verb: Verb, span: Span) -> Option<HRelId> {
        let found = match path {
            [name] => cx
                .aliases
                .get(&name.name)
                .copied()
                .or_else(|| self.scope(cx.ms).rels.get(&name.name).copied())
                .or_else(|| (name.as_str() == "localtick").then(|| self.builtin(super::BuiltinRel::LocalTick, span)))
                .or_else(|| (name.as_str() == "halt").then(|| self.builtin(super::BuiltinRel::Halt, span))),
            [inst, name] => {
                let found = self
                    .scope(cx.ms)
                    .instances
                    .get(&inst.name)
                    .and_then(|i| i.interface.get(&name.name))
                    .copied();
                match found {
                    Some((id, true)) => Some(id),
                    // A stream's events: the write is refused below, with the stream's reason.
                    Some((id, false)) if matches!(self.rel_of(id).kind, HRelKind::Stream(_)) => Some(id),
                    Some((_, false)) => {
                        self.error(
                            code!("BLS0203"),
                            span,
                            format!(
                                "`{}.{}` is an output of an instance: it can be read, not written",
                                inst.as_str(),
                                name.as_str()
                            ),
                        );
                        return None;
                    }
                    // A role's link events: the write is refused below (the runtime feeds them).
                    None if self.role_named(cx.ms, inst.name).is_some()
                        && matches!(name.as_str(), "connected" | "disconnected") =>
                    {
                        self.lookup_rel(cx.ms, path)
                    }
                    None => None,
                }
            }
            _ => None,
        };
        let Some(mut rel) = found else {
            if let [name] = path
                && self.scope(cx.ms).broken.contains(&name.name)
            {
                return None;
            }
            let names: Vec<&str> = path.iter().map(Ident::as_str).collect();
            self.error(
                code!("BLS0200"),
                span,
                format!("unknown relation `{}`", names.join(".")),
            );
            return None;
        };
        // Writes outside the interposition block go to `$outside`.
        if !cx.aliases.values().any(|a| *a == rel)
            && let Some(outside) = self.scope(cx.ms).write_redirect.get(&rel)
        {
            rel = *outside;
        }
        let r = self.rel_of(rel);
        let name = r.name.clone();
        let bad = |what: &str| format!("`{}` into `{name}`: {what}", verb.as_str());
        let err = match (&r.kind, verb) {
            (HRelKind::View, _) => Some((code!("BLS0406"), bad("a view is closed; no statement may write it"))),
            (HRelKind::Static, _) => Some((code!("BLS0400"), bad("a static relation gets its rows from facts"))),
            (HRelKind::Input { root: true }, _) => Some((code!("BLS0406"), bad("a module never writes its own input"))),
            // An importer writes an instance's input (an interposition block its `inside`); a module never writes
            // its own (LANGUAGE §7.6).
            (HRelKind::Input { root: false }, _) if self.declared_here(cx, rel) => {
                Some((code!("BLS0406"), bad("a module never writes its own input")))
            }
            (
                HRelKind::Timer { .. }
                | HRelKind::Boot
                | HRelKind::Recovered
                | HRelKind::Members(_)
                | HRelKind::NodeDir
                | HRelKind::Link { .. },
                _,
            ) => Some((code!("BLS0400"), bad("this relation is fed by the runtime"))),
            (HRelKind::LocalTick, v) if v != Verb::Next => {
                Some((code!("BLS0400"), bad("`localtick()` is requested with `next`")))
            }
            (HRelKind::Halt, v) if v != Verb::Emit => Some((code!("BLS0400"), bad("`halt` is written with `emit`"))),
            (HRelKind::Channel(_), v) if v != Verb::Send => None,
            (HRelKind::Stream(HStreamRel::Event(_)), _) => {
                Some((code!("BLS0400"), bad("a stream's events are fed by the runtime")))
            }
            (HRelKind::Stream(HStreamRel::Host(_)), v) if v != Verb::Send => {
                Some((code!("BLS0400"), bad("a request to the host is written with `send`")))
            }
            (k, Verb::Delete | Verb::Upsert) if !k.is_table() => {
                Some((code!("BLS0400"), bad("only tables accept `delete` and `upsert`")))
            }
            (_, Verb::Upsert) if r.resolve.is_some() => {
                self.unsupported("LANG-117", "`upsert` into a relation with a `resolve` policy", span);
                return None;
            }
            // A lattice only grows (LANG-284): it is reset by raising an epoch, never retracted.
            (_, Verb::Delete | Verb::Upsert)
                if r.cols
                    .iter()
                    .any(|c| c.ty.is_some_and(|t| self.hir.lattice_of(t).is_some())) =>
            {
                Some((
                    code!("BLS0410"),
                    bad(
                        "a lattice-valued relation cannot be deleted from or upserted (raise an epoch with `Lex` instead)",
                    ),
                ))
            }
            // A plain bootstrap runs after every restart, where durable state was reloaded (LANGUAGE §8.4).
            _ if cx.plain_bootstrap && r.durable => Some((
                code!("BLS0402"),
                bad(
                    "a plain `bootstrap` runs after every restart, over reloaded durable state; initial durable \
                     values go in `bootstrap fresh`",
                ),
            )),
            _ => None,
        };
        if let Some((c, msg)) = err {
            self.error(c, span, msg);
            return None;
        }
        if let HRelKind::Channel(ChannelInfo {
            direction: Some((src, _)),
            ..
        }) = r.kind
            && let Some(here) = cx.placement
            && here != src
        {
            let src_name = self.role_of(src).name.clone();
            self.error(
                code!("BLS0404"),
                span,
                format!("`{name}` is sent from `{src_name}`, so `send` must be placed there"),
            );
            return None;
        }
        if let Some(there) = r.role
            && let Some(here) = cx.placement
            && here != there
        {
            let there_name = self.role_of(there).name.clone();
            self.error(
                code!("BLS0404"),
                span,
                format!("`{name}` lives at `{there_name}`, so only rules placed there write it"),
            );
            return None;
        }
        Some(rel)
    }

    /// A head's arguments, one per column, positional or named (LANGUAGE §8.2).
    fn head_args(&mut self, cx: &mut RuleCx, rel: &HRel, args: &[Arg], span: Span) -> Option<Vec<HHeadArg>> {
        let cols: Vec<Symbol> = rel.cols.iter().map(|c| c.name).collect();
        let named = args.iter().any(|a| matches!(a, Arg::Named(..)));
        if args.iter().any(|a| matches!(a, Arg::Rest(_))) {
            self.error(code!("BLS0303"), span, "`..` is not allowed in a head");
            return None;
        }
        let mut slots: Vec<Option<HHeadArg>> = vec![None; cols.len()];
        if !named {
            if args.len() != cols.len() {
                self.error(
                    code!("BLS0301"),
                    span,
                    format!("`{}` has {} column(s), {} given", rel.name, cols.len(), args.len()),
                );
                return None;
            }
            for (i, a) in args.iter().enumerate() {
                let Arg::Pos(e) = a else {
                    self.error(code!("BLS0303"), a.span(), "unexpected argument form in a head");
                    return None;
                };
                let v = self.head_arg(cx, e)?;
                if let Some(slot) = slots.get_mut(i) {
                    *slot = Some(v);
                }
            }
        } else {
            for a in args {
                let (field, value) = match a {
                    Arg::Named(f, v) => (*f, v.clone()),
                    Arg::Pos(e) => match &e.kind {
                        ExprKind::Path(p, t) if t.is_empty() && p.len() == 1 => {
                            let f = p.first().copied()?;
                            (f, e.clone())
                        }
                        _ => {
                            self.error(
                                code!("BLS0303"),
                                e.span,
                                "in a named head, every argument is `column: value`",
                            );
                            return None;
                        }
                    },
                    _ => {
                        self.error(code!("BLS0303"), a.span(), "unexpected argument form in a head");
                        return None;
                    }
                };
                let Some(i) = cols.iter().position(|c| *c == field.name) else {
                    self.error(
                        code!("BLS0302"),
                        field.span,
                        format!("`{}` has no column `{}`", rel.name, field.as_str()),
                    );
                    return None;
                };
                let v = self.head_arg(cx, &value)?;
                if slots.get_mut(i).and_then(|s| s.replace(v)).is_some() {
                    self.error(
                        code!("BLS0303"),
                        field.span,
                        format!("column `{}` given twice", field.as_str()),
                    );
                    return None;
                }
            }
        }
        let mut out = Vec::new();
        for (s, col) in slots.into_iter().zip(&cols) {
            match s {
                Some(v) => out.push(v),
                None => {
                    self.error(
                        code!("BLS0303"),
                        span,
                        format!("column `{col}` of `{}` has no default and is not given", rel.name),
                    );
                    return None;
                }
            }
        }
        Some(out)
    }

    fn head_arg(&mut self, cx: &mut RuleCx, e: &ast::Expr) -> Option<HHeadArg> {
        if let ExprKind::Bang { name, args, clauses } = &e.kind {
            if name.as_str() == "index" {
                self.unsupported(
                    "LANG-097",
                    "`index!` in a statement head (write it as a view column)",
                    e.span,
                );
                return None;
            }
            return Some(HHeadArg::Agg(self.aggregate(cx, *name, args, clauses, e.span)?));
        }
        Some(HHeadArg::Expr(self.expr(cx, e)?))
    }

    /// A head aggregate (LANGUAGE §10.1).
    fn aggregate(
        &mut self,
        cx: &mut RuleCx,
        name: Ident,
        args: &[Arg],
        clauses: &[ast::BangClause],
        span: Span,
    ) -> Option<HAgg> {
        let func = match name.as_str() {
            "count" => AggKind::Count,
            "sum" => AggKind::Sum,
            "min" => AggKind::Min,
            "max" => AggKind::Max,
            "collect" => AggKind::Collect,
            "index" => {
                if let Some(c) = clauses.first() {
                    self.unsupported("LANG-097", &format!("`index!` with `{}`", c.keyword.as_str()), c.span);
                    return None;
                }
                if !args.is_empty() {
                    self.error(code!("BLS0301"), span, "`index!` takes no argument");
                    return None;
                }
                return Some(HAgg {
                    func: AggKind::Index,
                    args: Vec::new(),
                    default: None,
                    span,
                });
            }
            other => {
                self.unsupported("LANG-100", &format!("the aggregate `{other}!`"), span);
                return None;
            }
        };
        let mut default = None;
        for c in clauses {
            match c.keyword.as_str() {
                "default" => {
                    let [d] = c.exprs.as_slice() else {
                        self.error(code!("BLS0301"), c.span, "`default` takes one value");
                        return None;
                    };
                    default = Some(self.expr(cx, d)?);
                }
                other => {
                    self.unsupported("LANG-100", &format!("the aggregate clause `{other}`"), c.span);
                    return None;
                }
            }
        }
        let mut exprs = Vec::new();
        match args {
            [Arg::Star(_)] if func == AggKind::Count => {}
            [] => {
                self.error(
                    code!("BLS0301"),
                    span,
                    format!("`{}!` needs an argument", name.as_str()),
                );
                return None;
            }
            _ => {
                for a in args {
                    let Arg::Pos(e) = a else {
                        self.error(code!("BLS0202"), a.span(), "aggregate arguments are positional");
                        return None;
                    };
                    exprs.push(self.expr(cx, e)?);
                }
                if exprs.len() != 1 {
                    self.error(
                        code!("BLS0301"),
                        span,
                        format!("`{}!` takes one argument", name.as_str()),
                    );
                    return None;
                }
            }
        }
        Some(HAgg {
            func,
            args: exprs,
            default,
            span,
        })
    }

    /// A view's alternatives and columns (LANGUAGE §8.3).
    pub(crate) fn view(&mut self, s: ScopeIdx, v: &'t ast::ViewDecl, rel: HRelId) {
        let placement = self.rel_of(rel).role;
        // Annotated columns are declared; the others are inferred (LANGUAGE §5.6).
        for (i, c) in v.cols.iter().enumerate() {
            let Some(ty) = &c.ty else { continue };
            let Some(t) = self.resolve_type(s, ty) else { return };
            match self.hir.rels.get_mut(rel.index()).and_then(|r| r.cols.get_mut(i)) {
                Some(col) => col.ty = Some(t),
                None => {
                    self.bugs.push(blossom_base::internal_error!(
                        "view column {i} of {rel:?} was not declared"
                    ));
                    return;
                }
            }
        }
        let has_agg = v.cols.iter().any(|c| c.agg.is_some());
        let mut alternatives = Vec::new();
        let mut cxs = Vec::new();
        for alt in &v.alternatives {
            let mut cx = self.rule_cx(s, placement);
            cx.choice_allowed = v.alternatives.len() == 1;
            let body = self.body(&mut cx, alt);
            alternatives.push((cx.scope, body));
            cxs.push(cx);
        }
        let shape = if !has_agg {
            let mut cols = Vec::new();
            for c in &v.cols {
                let mut per_alt = Vec::new();
                for (i, cx) in cxs.iter().enumerate() {
                    match Self::lookup_var(cx, c.name.name) {
                        Some(var) => per_alt.push(var),
                        None => {
                            self.error(
                                code!("BLS0500"),
                                v.alternatives.get(i).map_or(v.span, |a| a.span),
                                format!("this alternative does not bind the column `{}`", c.name.as_str()),
                            );
                            return;
                        }
                    }
                }
                cols.push(per_alt);
            }
            HViewShape::Plain { cols }
        } else {
            // The variables that occur in every alternative, in the first alternative's order.
            let Some(first) = alternatives.first() else { return };
            let first_vars: Vec<(Symbol, bool)> = match self.hir.scope(first.0) {
                Ok(sc) => sc.vars.iter().map(|x| (x.name, x.generated)).collect(),
                Err(e) => {
                    self.bugs.push(e);
                    return;
                }
            };
            let mut shared_names = Vec::new();
            for (name, generated) in first_vars {
                if generated || shared_names.contains(&name) {
                    continue;
                }
                if cxs.iter().all(|cx| Self::lookup_var(cx, name).is_some()) {
                    shared_names.push(name);
                }
            }
            let mut ucx = self.rule_cx(s, placement);
            for n in &shared_names {
                self.new_var(&mut ucx, *n, v.span, false);
            }
            let shared: Vec<Vec<HVarId>> = cxs
                .iter()
                .map(|cx| shared_names.iter().filter_map(|n| Self::lookup_var(cx, *n)).collect())
                .collect();
            let mut cols = Vec::new();
            for c in &v.cols {
                match &c.agg {
                    Some(agg) => {
                        let ExprKind::Bang { name, args, clauses } = &agg.kind else {
                            self.error(
                                code!("BLS0202"),
                                agg.span,
                                "a view column `name = …` is an aggregate `agg!(…)`",
                            );
                            return;
                        };
                        let Some(a) = self.aggregate(&mut ucx, *name, args, clauses, agg.span) else {
                            return;
                        };
                        let has_agg = cols.iter().any(|c| matches!(c, HViewAggCol::Agg(_)));
                        let has_index = cols
                            .iter()
                            .any(|c| matches!(c, HViewAggCol::Agg(x) if x.func == crate::hir::AggKind::Index));
                        if has_agg && (a.func == crate::hir::AggKind::Index || has_index) {
                            self.unsupported(
                                "LANG-097",
                                "`index!` together with another aggregate column in one view",
                                agg.span,
                            );
                            return;
                        }
                        cols.push(HViewAggCol::Agg(a));
                    }
                    None => match Self::lookup_var(&ucx, c.name.name) {
                        Some(var) => cols.push(HViewAggCol::Group(var)),
                        None => {
                            self.error(
                                code!("BLS0500"),
                                c.span,
                                format!(
                                    "the grouping column `{}` is not bound by every alternative",
                                    c.name.as_str()
                                ),
                            );
                            return;
                        }
                    },
                }
            }
            let mut driver = None;
            let mut drivers = 0;
            for (_, body) in &alternatives {
                for l in &body.lits {
                    if let HLit::Per(a) = l {
                        drivers += 1;
                        driver = Some(a.clone());
                    }
                }
            }
            if drivers > 0 && alternatives.len() != 1 {
                self.error(
                    code!("BLS0600"),
                    v.span,
                    "a view with a `per` driver has exactly one alternative",
                );
                return;
            }
            if drivers > 1 {
                self.error(code!("BLS0511"), v.span, "a view has at most one `per` driver");
                return;
            }
            // A driver's group may be empty: an aggregate with no identity for it needs a `default` (§10.2).
            if driver.is_some() {
                let mut missing = false;
                for c in &cols {
                    if let HViewAggCol::Agg(a) = c
                        && a.default.is_none()
                        && !matches!(a.func, AggKind::Count | AggKind::Sum | AggKind::Collect)
                    {
                        let name = match a.func {
                            AggKind::Min => "min!",
                            AggKind::Max => "max!",
                            _ => "index!",
                        };
                        self.error(
                            code!("BLS0511"),
                            a.span,
                            format!(
                                "with a `per` driver a group may be empty, and `{name}` has no value for it: write \
                                 `default e` inside its parentheses"
                            ),
                        );
                        missing = true;
                    }
                }
                if missing {
                    return;
                }
            }
            HViewShape::Aggregate {
                union: ucx.scope,
                shared,
                cols,
                driver,
            }
        };
        let texts = v.alternatives.iter().map(|a| self.normalized(a.span)).collect();
        self.hir.views.push(HView {
            rel,
            alternatives,
            texts,
            shape,
            monotone: v.monotone,
            span: v.span,
        });
    }

    /// `invariant name ["message"]: never BODY;` (LANGUAGE §17.1).
    pub(crate) fn invariant(&mut self, s: ScopeIdx, inv: &'t ast::Invariant, placement: Option<HRoleId>) {
        let mut cx = self.rule_cx(s, placement);
        let body = self.body(&mut cx, &inv.body);
        self.hir.invariants.push(HInvariant {
            name: inv.name.name,
            message: inv.message.clone(),
            scope: cx.scope,
            body,
            role: placement,
            span: inv.span,
        });
    }

    /// `table p(c̄) … while BODY;` (LANGUAGE §7.2): the body `p(c̄), BODY` over the columns, named as declared.
    pub(crate) fn persist_guard(&mut self, s: ScopeIdx, d: &'t ast::RelDecl, placement: Option<HRoleId>) {
        let Some(guard) = &d.guard else { return };
        let Some(rel) = self.scope(s).rels.get(&d.name.name).copied() else {
            // The declaration failed (and was reported).
            return;
        };
        let r = self.rel_of(rel);
        if r.kind != HRelKind::Table || r.cell {
            self.error(code!("BLS0106"), guard.span, "`while` applies to tables");
            return;
        }
        if r.cols
            .iter()
            .any(|c| c.ty.is_some_and(|t| holds_lattice(&self.hir.types, t)))
        {
            self.unsupported("SEM-104", "`while` on a table holding lattice values", guard.span);
            return;
        }
        if let Some(c) = d.cols.iter().find(|c| !is_var_name(c.name.as_str())) {
            self.error(
                code!("BLS0106"),
                c.name.span,
                format!(
                    "a `while` condition reads the columns by name, so they are named as variables (lowercase); `{}` is not",
                    c.name.as_str()
                ),
            );
            return;
        }
        // `p(c̄)` first: it binds the columns, which the condition reads by name.
        let path = |n: Ident| ast::Expr::new(ExprKind::Path(vec![n], Vec::new()), n.span);
        let atom = ast::AtomLit {
            expr: ast::Expr::new(
                ExprKind::Call {
                    callee: Box::new(path(d.name)),
                    args: d.cols.iter().map(|c| Arg::Pos(path(c.name))).collect(),
                },
                d.name.span,
            ),
            from: None,
            principal: None,
            weight: None,
            at: None,
            at_tick: None,
            span: d.name.span,
        };
        let mut lits = vec![ast::Lit::Plain(atom)];
        lits.extend(guard.lits.iter().cloned());
        let body = ast::Body {
            lits,
            guards: guard.guards.clone(),
            span: guard.span,
        };
        let mut cx = self.rule_cx(s, placement.or(r.role));
        let body = self.body(&mut cx, &body);
        let cols = match body.lits.first() {
            Some(HLit::Atom(a)) if a.rel == rel => a
                .args
                .iter()
                .map(|p| match p {
                    HPat::Var(v, _) => Some(*v),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>(),
            _ => None,
        };
        let Some(cols) = cols else {
            if !self.diags.has_errors() {
                self.bugs.push(blossom_base::internal_error!(
                    "the persistence condition of `{}` does not start with its columns' atom",
                    r.name
                ));
            }
            return;
        };
        self.hir.guards.push(HGuard {
            rel,
            scope: cx.scope,
            body,
            cols,
            role: r.role,
            span: guard.span,
        });
    }

    /// `fact r(…);` (LANGUAGE §8.4).
    pub(crate) fn fact(&mut self, s: ScopeIdx, f: &'t ast::Fact) {
        if f.at.is_some() || f.tick.is_some() {
            self.error(
                code!("BLS0509"),
                f.span,
                "`fact … @ n [at tick k]` is allowed only in a spec",
            );
            return;
        }
        let Some(rel) = self.lookup_rel(s, &f.head.rel) else {
            self.error(code!("BLS0200"), f.head.span, "unknown relation");
            return;
        };
        let r = self.rel_of(rel);
        if r.kind != HRelKind::Static {
            self.error(
                code!("BLS0405"),
                f.span,
                format!(
                    "a fact asserts a row of a static relation; initial state of `{}` goes in `bootstrap`",
                    r.name
                ),
            );
            return;
        }
        let mut cx = self.rule_cx(s, None);
        let r = self.rel_of(rel).clone();
        let Some(args) = self.head_args(&mut cx, &r, &f.head.args, f.head.span) else {
            return;
        };
        let mut row = Vec::new();
        for a in args {
            match a {
                HHeadArg::Expr(e) => row.push(e),
                HHeadArg::Agg(g) => {
                    self.error(code!("BLS0202"), g.span, "a fact holds values, not aggregates");
                    return;
                }
            }
        }
        self.hir.facts.push(HFact {
            rel,
            row,
            scope: cx.scope,
            span: f.span,
        });
    }

    /// The rules inside `interpose a.i as (outside, inside) { … }`.
    pub(crate) fn interpose_rules(&mut self, s: ScopeIdx, ip: &'t ast::Interpose, placement: Option<HRoleId>) {
        let [inst, name] = ip.target.as_slice() else { return };
        let Some(&(real, _)) = self
            .scope(s)
            .instances
            .get(&inst.name)
            .and_then(|i| i.interface.get(&name.name))
        else {
            return;
        };
        let Some(outside) = self.scope(s).write_redirect.get(&real).copied() else {
            return;
        };
        for item in &ip.items {
            match &item.kind {
                ast::ItemKind::Handler(h) => {
                    let mut aliases = BTreeMap::new();
                    aliases.insert(ip.outside.name, outside);
                    aliases.insert(ip.inside.name, real);
                    self.handler_with_aliases(s, h, placement, aliases);
                }
                _ => self.error(code!("BLS0110"), item.span, "an interposition block holds handlers"),
            }
        }
    }

    fn handler_with_aliases(
        &mut self,
        s: ScopeIdx,
        h: &'t ast::Handler,
        placement: Option<HRoleId>,
        aliases: BTreeMap<Symbol, HRelId>,
    ) {
        let mut cx = self.rule_cx(s, placement);
        cx.aliases = aliases;
        let header = self.body(&mut cx, &h.header);
        let stmts = self.stmts(&mut cx, &h.block.stmts);
        let text = self.normalized(h.header.span);
        self.hir.handlers.push(HHandler {
            scope: cx.scope,
            label: h.label.map(|l| l.name),
            trigger: h.trigger,
            kind: HandlerKind::Plain,
            header,
            stmts,
            role: placement,
            text,
            span: h.span,
        });
    }
}

/// The names a pattern binds, variants' fields included.
fn refutable_pattern_names(e: &ast::Expr, out: &mut BTreeSet<Symbol>) {
    match &e.kind {
        ExprKind::Call { args, .. } => {
            for a in args {
                if let Arg::Pos(x) = a {
                    refutable_pattern_names(x, out);
                }
            }
        }
        ExprKind::Tuple(es) => es.iter().for_each(|x| refutable_pattern_names(x, out)),
        _ => collect_pattern_names(e, out),
    }
}

/// The first name a pattern binds twice (in its tuples and variant arguments).
fn first_duplicate(e: &ast::Expr, seen: &mut BTreeSet<Symbol>) -> Option<Ident> {
    match &e.kind {
        ExprKind::Path(path, targs) if targs.is_empty() && path.len() == 1 => {
            let n = path.first()?;
            (is_var_name(n.as_str()) && !seen.insert(n.name)).then_some(*n)
        }
        ExprKind::Tuple(es) => es.iter().find_map(|x| first_duplicate(x, seen)),
        ExprKind::Call { args, .. } => args.iter().find_map(|a| match a {
            Arg::Pos(x) => first_duplicate(x, seen),
            _ => None,
        }),
        _ => None,
    }
}

fn collect_pattern_names(e: &ast::Expr, out: &mut BTreeSet<Symbol>) {
    match &e.kind {
        ExprKind::Path(path, _) if path.len() == 1 => {
            if let Some(n) = path.first()
                && is_var_name(n.as_str())
            {
                out.insert(n.name);
            }
        }
        ExprKind::Tuple(es) => es.iter().for_each(|x| collect_pattern_names(x, out)),
        _ => {}
    }
}

fn range_kind(op: BinOp) -> Option<RangeKind> {
    Some(match op {
        BinOp::Range => RangeKind::HalfOpen,
        BinOp::RangeEq => RangeKind::Closed,
        BinOp::OpenRange => RangeKind::OpenOpen,
        BinOp::OpenRangeEq => RangeKind::OpenClosed,
        _ => return None,
    })
}

/// The target relation a spec atom names, with its arguments (`None` for a bare name): `r(…)`, `r`, or an instance's
/// relation `a.r(…)`, `a.r`, named `a.r` (LANGUAGE §17.3).
fn spec_rel_path(e: &ast::Expr) -> Option<(Ident, Option<&[Arg]>)> {
    let qualified = |inst: &Ident, name: &Ident| Ident {
        name: Symbol::intern(&format!("{}.{}", inst.as_str(), name.as_str())),
        span: name.span,
    };
    let single = |p: &[Ident]| match p {
        [one] => Some(*one),
        _ => None,
    };
    match &e.kind {
        ExprKind::Call { callee, args } => match &callee.kind {
            ExprKind::Path(p, t) if t.is_empty() => single(p).map(|n| (n, Some(args.as_slice()))),
            ExprKind::Field { base, name } => match &base.kind {
                ExprKind::Path(p, t) if t.is_empty() => single(p).map(|i| (qualified(&i, name), Some(args.as_slice()))),
                _ => None,
            },
            _ => None,
        },
        ExprKind::Method { receiver, name, args } => match &receiver.kind {
            ExprKind::Path(p, t) if t.is_empty() => single(p).map(|i| (qualified(&i, name), Some(args.as_slice()))),
            _ => None,
        },
        ExprKind::Field { base, name } => match &base.kind {
            ExprKind::Path(p, t) if t.is_empty() => single(p).map(|i| (qualified(&i, name), None)),
            _ => None,
        },
        ExprKind::Path(p, t) if t.is_empty() => single(p).map(|n| (n, None)),
        _ => None,
    }
}
