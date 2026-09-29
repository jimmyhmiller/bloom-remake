//! Name resolution, instantiation and literal classification (ARCHITECTURE §13.4, §13.5): the surface AST of a
//! program root and its modules becomes the [`Hir`].
//!
//! A program is processed as a tree of **module scopes**: the root, and one scope per `import … as a` (an instance
//! whose relations are named `a.r`, and `a.b.r` for nested instances). Each scope is processed in passes, because
//! items are unordered (LANGUAGE §6.1):
//!
//! 1. roles (declared at the root, bound by `with (…)` in a choreography instance);
//! 2. declarations: relations, views, timers, the interfaces of the module's protocols, and interpositions;
//! 3. imports: each instance is created and processed recursively (its relation parameters name relations of this
//!    scope, which exist after pass 2);
//! 4. rules: handlers, bootstraps, views' alternatives and facts, resolved against this scope and its instances.
//!
//! Types, constants and modules are looked up lazily through the scope's file ([`types`]), so their order does not
//! matter either. Bodies, statements and expressions are resolved in [`body`].

mod body;
mod types;

pub(crate) use types::int_value;

use std::collections::BTreeMap;

use blossom_base::TypeId;
use blossom_base::{Diagnostic, Diagnostics, InternalError, QualName, SourceDb, Span, Symbol, code};
use blossom_value::{TypeDef, TypeTable, Value};

use crate::ast::{self, Ident, ItemKind, RelKind};
use crate::hir::*;
use crate::modules::ModuleTree;

/// Resolves the program rooted at `tree.root`. Returns `None` when the program is rejected; `diags` says why.
pub fn resolve(tree: &ModuleTree, sources: &SourceDb, diags: &mut Diagnostics) -> Result<Option<Hir>, InternalError> {
    let Some(header) = &tree.root.header else {
        diags.push(
            Diagnostic::new(
                code!("BLS0110"),
                "the root file has no `program NAME version N;` header, so it is a library, not a program",
            )
            .with_primary(Span::point(tree.root.span.file, 0)),
        );
        return Ok(None);
    };
    let mut r = Resolver::new(tree, sources, diags, header.name.name, header.version, header.edition);
    let root = r.new_scope(ModScope::empty(FileKey::Root));
    r.process(root, &tree.root.items, None);
    r.finish()
}

/// Resolves the module `path` of the root file (or of a module it uses), instantiated as a program root: the
/// target of a spec (LANGUAGE §17.2). Its inputs are host-fed, its value parameters take `args` or their defaults.
pub fn resolve_module_root(
    tree: &ModuleTree,
    sources: &SourceDb,
    diags: &mut Diagnostics,
    path: &[Ident],
    type_args: &[ast::Type],
    args: &[ast::Arg],
    spec: Option<SpecMode>,
) -> Result<Option<(Hir, Option<SpecMode>)>, InternalError> {
    let name = path.last().map_or(Symbol::intern("<spec>"), |i| i.name);
    let mut r = Resolver::new(tree, sources, diags, name, 1, 1);
    let file_scope = r.new_scope(ModScope::empty(FileKey::Root));
    let Some((file, module)) = r.find_module(file_scope, path) else {
        let names: Vec<&str> = path.iter().map(Ident::as_str).collect();
        let span = path.first().map_or(Span::point(tree.root.span.file, 0), |i| i.span);
        r.error(code!("BLS0200"), span, format!("unknown module `{}`", names.join("::")));
        return Ok(None);
    };
    let root = r.new_scope(ModScope {
        own_items: Some(&module.items),
        ..ModScope::empty(file)
    });
    if module.generics.len() != type_args.len() {
        let span = path.first().map_or(Span::point(tree.root.span.file, 0), |i| i.span);
        r.error(
            code!("BLS0301"),
            span,
            format!(
                "`{}` takes {} type argument(s), {} given",
                module.name.as_str(),
                module.generics.len(),
                type_args.len()
            ),
        );
        return Ok(None);
    }
    for (g, a) in module.generics.iter().zip(type_args) {
        let Some(t) = r.resolve_type(file_scope, a) else {
            return r.finish().map(|_| None);
        };
        r.scope_mut(root).generics.insert(g.name.name, t);
    }
    let mut given: BTreeMap<Symbol, &ast::Expr> = BTreeMap::new();
    for a in args {
        match a {
            ast::Arg::Named(n, e) => {
                given.insert(n.name, e);
            }
            other => r.error(
                code!("BLS0205"),
                other.span(),
                "module arguments are written `NAME = value`",
            ),
        }
    }
    for p in &module.params {
        match &p.kind {
            ast::ModParamKind::Value { ty, default } => {
                let Some(t) = r.resolve_type(root, ty) else { continue };
                let value = match (given.remove(&p.name.name), default) {
                    (Some(e), _) => r.const_value(file_scope, e, Some(t)),
                    (None, Some(d)) => r.const_value(root, d, Some(t)),
                    (None, None) => {
                        r.error(
                            code!("BLS0205"),
                            p.span,
                            format!("module parameter `{}` has no default and is not given", p.name.as_str()),
                        );
                        None
                    }
                };
                if let Some(v) = value {
                    r.scope_mut(root).values.insert(p.name.name, v);
                }
            }
            ast::ModParamKind::Rel { .. } => {
                r.unsupported("LANG-010", "relation parameters of a spec target", p.span);
            }
        }
    }
    for (name, e) in given {
        r.error(
            code!("BLS0205"),
            e.span,
            format!("`{}` has no parameter `{}`", module.name.as_str(), name.as_str()),
        );
    }
    for proto in &module.protocols {
        r.protocol_interfaces(root, proto, None);
    }
    r.spec = spec;
    r.process(root, &module.items, None);
    let spec = r.spec.take();
    Ok(r.finish()?.map(|h| (h, spec)))
}

/// Resolves a spec's views (`items`) in spec mode, over the target `target` (whose types and roles the spec program
/// shares, so type ids carry over).
pub fn resolve_spec_views(
    tree: &ModuleTree,
    sources: &SourceDb,
    diags: &mut Diagnostics,
    name: Symbol,
    target: &Hir,
    items: &[ast::Item],
    mode: SpecMode,
) -> Result<Option<(Hir, SpecMode)>, InternalError> {
    let mut r = Resolver::new(tree, sources, diags, name, 1, 1);
    r.hir.types = target.types.clone();
    r.hir.roles = target.roles.clone();
    r.spec = Some(mode);
    let root = r.new_scope(ModScope::empty(FileKey::Root));
    r.process(root, items, None);
    let mode = r.spec.take().unwrap_or_default();
    Ok(r.finish()?.map(|h| (h, mode)))
}

/// Which file a scope's module is defined in.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum FileKey {
    Root,
    Module(String),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ScopeIdx(usize);

/// One module scope: the root or an instance.
pub(crate) struct ModScope<'t> {
    pub file: FileKey,
    /// The instance path (`[]` for the root).
    pub prefix: Vec<Symbol>,
    /// Bound type parameters.
    pub generics: BTreeMap<Symbol, TypeId>,
    /// Value parameters and constants, folded.
    pub values: BTreeMap<Symbol, (Value, TypeId)>,
    /// Relations by surface name (declared here, views, timers, protocol interfaces, relation parameters).
    pub rels: BTreeMap<Symbol, HRelId>,
    /// Instances by alias: their relations by name, and which of those are interfaces.
    pub instances: BTreeMap<Symbol, Instance>,
    /// Roles visible by name.
    pub roles: BTreeMap<Symbol, HRoleId>,
    /// The template's own role names, in a choreography instance (every one must be bound).
    pub role_template: Option<Vec<Symbol>>,
    /// Whether the module declares roles (then non-shared items must be inside `at`, BLS0408).
    pub has_roles: bool,
    /// Writes to an instance input that is interposed go to the `$outside` relation (LANGUAGE §6.9).
    pub write_redirect: BTreeMap<HRelId, HRelId>,
    /// The module body's items, for module-local constants and types.
    pub own_items: Option<&'t [ast::Item]>,
}

impl ModScope<'_> {
    pub(crate) fn empty(file: FileKey) -> Self {
        ModScope {
            file,
            prefix: Vec::new(),
            generics: BTreeMap::new(),
            values: BTreeMap::new(),
            rels: BTreeMap::new(),
            instances: BTreeMap::new(),
            roles: BTreeMap::new(),
            role_template: None,
            has_roles: false,
            write_redirect: BTreeMap::new(),
            own_items: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Instance {
    /// Interface relations by name, with their direction (`true` for inputs).
    pub interface: BTreeMap<Symbol, (HRelId, bool)>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum BuiltinRel {
    Boot,
    LocalTick,
    Halt,
}

/// Spec mode (LANGUAGE §17.3): the target's relations are read through trace relations, one per relation and time
/// (`r(…) @ n` at the evaluation point, `r(…) @ n at tick k`), each with the node as column 0; `crashed(n)` is the
/// crash oracle; spec node constants name the scenario's nodes.
#[derive(Clone, Debug, Default)]
pub struct SpecMode {
    /// The target's relations by surface name: their columns (in the target's type table, which the spec shares).
    pub targets: BTreeMap<Symbol, Vec<HCol>>,
    /// Trace relations created so far, by target relation and time (`None` for the evaluation point).
    pub traces: BTreeMap<(Symbol, Option<u64>), HRelId>,
    /// The `crashed(n)` oracle, once used.
    pub crashed: Option<HRelId>,
    /// The scenario's node constants, by name, with their node index.
    pub nodes: BTreeMap<Symbol, u32>,
}

pub(crate) struct Resolver<'t, 'd> {
    pub tree: &'t ModuleTree,
    pub sources: &'t SourceDb,
    pub diags: &'d mut Diagnostics,
    pub hir: Hir,
    pub scopes: Vec<ModScope<'t>>,
    pub builtins: BTreeMap<BuiltinRel, HRelId>,
    pub members: BTreeMap<HRoleId, HRelId>,
    /// Structs and enums already interned, by defining file and name.
    pub nominal: BTreeMap<(FileKey, Symbol), TypeId>,
    pub rel_spans: BTreeMap<HRelId, Span>,
    /// Frontend bugs met while resolving (a lookup of an id the resolver minted that fails).
    pub bugs: Vec<InternalError>,
    /// What a missing scope reads as.
    pub empty: ModScope<'t>,
    /// Spec mode, when resolving a spec's views.
    pub spec: Option<SpecMode>,
}

impl<'t, 'd> Resolver<'t, 'd> {
    pub(crate) fn new(
        tree: &'t ModuleTree,
        sources: &'t SourceDb,
        diags: &'d mut Diagnostics,
        name: Symbol,
        version: u32,
        edition: u16,
    ) -> Resolver<'t, 'd> {
        Resolver {
            tree,
            sources,
            diags,
            hir: Hir {
                name,
                version,
                edition,
                types: TypeTable::new(),
                roles: Vec::new(),
                rels: Vec::new(),
                handlers: Vec::new(),
                views: Vec::new(),
                facts: Vec::new(),
                invariants: Vec::new(),
                scopes: Vec::new(),
                var_types: Vec::new(),
            },
            scopes: Vec::new(),
            builtins: BTreeMap::new(),
            members: BTreeMap::new(),
            nominal: BTreeMap::new(),
            rel_spans: BTreeMap::new(),
            bugs: Vec::new(),
            empty: ModScope::empty(FileKey::Root),
            spec: None,
        }
    }

    /// The HIR, unless a bug or an error was reported.
    pub(crate) fn finish(self) -> Result<Option<Hir>, InternalError> {
        if let Some(bug) = self.bugs.into_iter().next() {
            return Err(bug);
        }
        if self.diags.has_errors() {
            return Ok(None);
        }
        Ok(Some(self.hir))
    }

    pub fn error(&mut self, code: blossom_base::Code, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic::new(code, msg).with_primary(span));
    }

    pub fn unsupported(&mut self, feature: &'static str, what: &str, span: Span) {
        self.diags.push(
            Diagnostic::not_implemented(blossom_base::FeatureId(feature), what, "the Blossom frontend (slice 2)")
                .with_primary(span),
        );
    }

    /// The relation `id` (a clone); a miss is recorded as a bug and a placeholder returned.
    pub fn rel_of(&mut self, id: HRelId) -> HRel {
        match self.hir.rel(id) {
            Ok(r) => r.clone(),
            Err(e) => {
                self.bugs.push(e);
                HRel::placeholder()
            }
        }
    }

    /// The role `id` (a clone); a miss is recorded as a bug and a placeholder returned.
    pub fn role_of(&mut self, id: HRoleId) -> HRole {
        match self.hir.role(id) {
            Ok(r) => r.clone(),
            Err(e) => {
                self.bugs.push(e);
                HRole::placeholder()
            }
        }
    }

    fn new_scope(&mut self, s: ModScope<'t>) -> ScopeIdx {
        self.scopes.push(s);
        ScopeIdx(self.scopes.len() - 1)
    }

    /// Module scope `s`. Scope indexes are minted by `new_scope` and never invalidated; a miss (a bug) reads as the
    /// empty scope, whose lookups all fail loudly as unknown names.
    pub fn scope(&self, s: ScopeIdx) -> &ModScope<'t> {
        self.scopes.get(s.0).unwrap_or(&self.empty)
    }

    pub fn scope_mut(&mut self, s: ScopeIdx) -> &mut ModScope<'t> {
        if s.0 >= self.scopes.len() {
            self.bugs
                .push(blossom_base::internal_error!("module scope {s:?} does not exist"));
            return &mut self.empty;
        }
        match self.scopes.get_mut(s.0) {
            Some(sc) => sc,
            None => &mut self.empty,
        }
    }

    /// The items at the top of a file.
    pub fn file_items(&self, file: &FileKey) -> &'t [ast::Item] {
        match file {
            FileKey::Root => &self.tree.root.items,
            FileKey::Module(m) => self.tree.modules.get(m).map(|f| f.items.as_slice()).unwrap_or(&[]),
        }
    }

    /// The qualified name of `name` in scope `s`.
    pub fn qual(&self, s: ScopeIdx, name: Symbol) -> QualName {
        let mut segs = self.scope(s).prefix.clone();
        segs.push(name);
        QualName::new(segs)
    }

    pub fn module_path(&self, s: ScopeIdx) -> QualName {
        QualName::new(self.scope(s).prefix.clone())
    }

    pub fn add_rel(&mut self, rel: HRel) -> HRelId {
        let id = HRelId(u32::try_from(self.hir.rels.len()).unwrap_or(u32::MAX));
        self.hir.rels.push(rel);
        id
    }

    /// Declares `rel` under `name` in scope `s` (BLS0201 on a duplicate).
    fn bind_rel(&mut self, s: ScopeIdx, name: Ident, id: HRelId) {
        if let Some(prev) = self.scope(s).rels.get(&name.name).copied() {
            let prev_span = self.rel_spans.get(&prev).copied();
            let mut d = Diagnostic::new(code!("BLS0201"), format!("`{}` is declared twice", name.as_str()))
                .with_primary(name.span);
            if let Some(p) = prev_span {
                d = d.with_label(p, "first declared here");
            }
            self.diags.push(d);
            return;
        }
        self.rel_spans.insert(id, name.span);
        self.scope_mut(s).rels.insert(name.name, id);
    }

    /// A built-in relation, created on first use.
    pub fn builtin(&mut self, which: BuiltinRel, span: Span) -> HRelId {
        if let Some(id) = self.builtins.get(&which) {
            return *id;
        }
        let (name, kind) = match which {
            BuiltinRel::Boot => ("boot", HRelKind::Boot),
            BuiltinRel::LocalTick => ("localtick", HRelKind::LocalTick),
            BuiltinRel::Halt => ("halt", HRelKind::Halt),
        };
        let cols = match which {
            BuiltinRel::Halt => vec![HCol {
                name: Symbol::intern("kill"),
                ty: Some(self.intern_type(TypeDef::Bool, span)),
            }],
            _ => Vec::new(),
        };
        let id = self.add_rel(HRel {
            name: QualName::single(Symbol::intern(name)),
            kind,
            cols,
            key: None,
            durable: false,
            role: None,
            span,
        });
        self.builtins.insert(which, id);
        id
    }

    /// The spec oracle `crashed(n: Node)` (LANGUAGE §17.3), created on first use.
    pub fn crashed_oracle(&mut self, span: Span) -> HRelId {
        if let Some(id) = self.spec.as_ref().and_then(|s| s.crashed) {
            return id;
        }
        let ty = self.node_type(None);
        let id = self.add_rel(HRel {
            name: QualName::single(Symbol::intern("crashed")),
            kind: HRelKind::Input { root: true },
            cols: vec![HCol {
                name: Symbol::intern("n"),
                ty: Some(ty),
            }],
            key: None,
            durable: false,
            role: None,
            span,
        });
        if let Some(spec) = self.spec.as_mut() {
            spec.crashed = Some(id);
        }
        id
    }

    /// The trace relation of target relation `name` at `time` (`None`: the evaluation point), created on first use:
    /// the target's columns after a `node: Node` column.
    pub fn trace_rel(&mut self, name: Ident, time: Option<u64>) -> Option<HRelId> {
        let cols = self.spec.as_ref()?.targets.get(&name.name)?.clone();
        if let Some(id) = self.spec.as_ref().and_then(|s| s.traces.get(&(name.name, time))) {
            return Some(*id);
        }
        let node = self.node_type(None);
        let mut all = vec![HCol {
            name: Symbol::intern("node"),
            ty: Some(node),
        }];
        all.extend(cols);
        let suffix = match time {
            None => "$eot".to_owned(),
            Some(k) => format!("$at{k}"),
        };
        let id = self.add_rel(HRel {
            name: QualName::single(Symbol::intern(&format!("{}{suffix}", name.as_str()))),
            kind: HRelKind::Input { root: true },
            cols: all,
            key: None,
            durable: false,
            role: None,
            span: name.span,
        });
        if let Some(spec) = self.spec.as_mut() {
            spec.traces.insert((name.name, time), id);
        }
        Some(id)
    }

    /// `R$members(n: Node<R>)`, created on first use.
    pub fn members_rel(&mut self, role: HRoleId, span: Span) -> HRelId {
        if let Some(id) = self.members.get(&role) {
            return *id;
        }
        let ty = self.node_type(Some(role));
        let rname = self.role_of(role).name.clone();
        let mut segs = rname.segments().to_vec();
        if let Some(last) = segs.last_mut() {
            *last = Symbol::intern(&format!("{}$members", last.as_str()));
        }
        let id = self.add_rel(HRel {
            name: QualName::new(segs),
            kind: HRelKind::Members(role),
            cols: vec![HCol {
                name: Symbol::intern("n"),
                ty: Some(ty),
            }],
            key: None,
            durable: false,
            role: None,
            span,
        });
        self.members.insert(role, id);
        id
    }

    pub fn intern_type(&mut self, def: TypeDef, span: Span) -> TypeId {
        match self.hir.types.insert(def) {
            Ok(t) => t,
            Err(e) => {
                self.error(code!("BLS0300"), span, format!("invalid type: {e}"));
                self.hir.types.insert(TypeDef::Unit).unwrap_or(TypeId::from_raw(0))
            }
        }
    }

    pub fn node_type(&mut self, role: Option<HRoleId>) -> TypeId {
        let def = TypeDef::Node(role.map(|r| blossom_base::RoleId::from_raw(r.0)));
        self.intern_type(def, Span::point(self.tree.root.span.file, 0))
    }

    /// The normalized source text of `span`: comments removed, whitespace runs collapsed (the stand-in for the
    /// formatter's canonical printing in rule-id hashes, LANGUAGE §4.3).
    pub fn normalized(&self, span: Span) -> String {
        let Ok(text) = self.sources.text(span.file) else {
            return String::new();
        };
        let slice = text.get(span.lo as usize..span.hi as usize).unwrap_or("");
        let mut out = String::new();
        for line in slice.lines() {
            let code = strip_comment(line);
            for word in code.split_whitespace() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(word);
            }
        }
        out
    }

    /// Processes a module body in scope `s`: roles, declarations, imports, then rules.
    fn process(&mut self, s: ScopeIdx, items: &'t [ast::Item], placement: Option<HRoleId>) {
        self.fold_consts(s, items);
        self.roles(s, items);
        let has_roles = self.scope(s).has_roles;
        self.declare(s, items, placement, has_roles, false);
        self.imports(s, items, placement);
        self.rules(s, items, placement);
    }

    /// Pass 1: roles.
    fn roles(&mut self, s: ScopeIdx, items: &'t [ast::Item]) {
        for item in items {
            let ItemKind::Role { name, kind } = &item.kind else {
                continue;
            };
            self.scope_mut(s).has_roles = true;
            let role_kind = match kind.map(|k| k.as_str()) {
                None | Some("process") => RoleKind::Process,
                Some("cluster") => RoleKind::Cluster,
                Some("external") => RoleKind::External,
                Some(other) => {
                    self.error(
                        code!("BLS0200"),
                        kind.map_or(name.span, |k| k.span),
                        format!("unknown role kind `{other}`: expected process, cluster or external"),
                    );
                    RoleKind::Process
                }
            };
            if self.scope(s).role_template.is_some() {
                // A choreography's role: bound by the importer (checked in `instantiate`).
                if let Some(bound) = self.scope(s).roles.get(&name.name).copied() {
                    let actual = self.role_of(bound).kind;
                    if actual != role_kind {
                        self.error(
                            code!("BLS0206"),
                            name.span,
                            format!(
                                "role `{}` is {role_kind:?} but is bound to a {actual:?} role",
                                name.as_str()
                            ),
                        );
                    }
                }
                continue;
            }
            if self.scope(s).roles.contains_key(&name.name) {
                self.error(
                    code!("BLS0201"),
                    name.span,
                    format!("role `{}` is declared twice", name.as_str()),
                );
                continue;
            }
            let id = HRoleId(u32::try_from(self.hir.roles.len()).unwrap_or(u32::MAX));
            let qual = self.qual(s, name.name);
            self.hir.roles.push(HRole {
                name: qual,
                kind: role_kind,
                span: name.span,
            });
            self.scope_mut(s).roles.insert(name.name, id);
        }
        if let Some(template) = self.scope(s).role_template.clone() {
            for item in items {
                if let ItemKind::Role { name, .. } = &item.kind
                    && !template.contains(&name.name)
                {
                    self.error(
                        code!("BLS0206"),
                        name.span,
                        format!(
                            "role `{}` of the choreography is not bound by the import",
                            name.as_str()
                        ),
                    );
                }
            }
        }
    }

    /// Resolves the role of an `at R` section.
    fn at_role(&mut self, s: ScopeIdx, role: Ident) -> Option<HRoleId> {
        match self.scope(s).roles.get(&role.name).copied() {
            Some(r) => {
                if self.role_of(r).kind == RoleKind::External {
                    self.error(
                        code!("BLS0408"),
                        role.span,
                        format!(
                            "`{}` is an external role: no rules or relations can be placed there",
                            role.as_str()
                        ),
                    );
                }
                Some(r)
            }
            None => {
                self.error(code!("BLS0200"), role.span, format!("unknown role `{}`", role.as_str()));
                None
            }
        }
    }

    /// Pass 2: declarations. `in_at` says whether the items are inside an `at` section.
    fn declare(
        &mut self,
        s: ScopeIdx,
        items: &'t [ast::Item],
        placement: Option<HRoleId>,
        has_roles: bool,
        in_at: bool,
    ) {
        for item in items {
            match &item.kind {
                ItemKind::At { role, items: inner } => {
                    if in_at {
                        self.error(code!("BLS0110"), role.span, "`at` sections do not nest");
                        continue;
                    }
                    if !has_roles {
                        self.error(code!("BLS0110"), role.span, "`at` in a module that declares no roles");
                        continue;
                    }
                    let r = self.at_role(s, *role);
                    self.declare(s, inner, r, has_roles, true);
                }
                ItemKind::Rel(d) => {
                    let shared = matches!(d.kind, RelKind::Channel | RelKind::Static);
                    if has_roles && !in_at && !shared {
                        self.error(
                            code!("BLS0408"),
                            d.name.span,
                            "in a module with roles, only channels and static relations may be declared outside `at`",
                        );
                        continue;
                    }
                    let root = self.scope(s).prefix.is_empty();
                    if let Some(id) = self.rel_decl(s, d, if shared { None } else { placement }, root) {
                        self.bind_rel(s, d.name, id);
                    }
                }
                ItemKind::View(v) => {
                    if has_roles && !in_at {
                        self.error(
                            code!("BLS0408"),
                            v.name.span,
                            "in a module with roles, views go inside `at`",
                        );
                        continue;
                    }
                    let cols = v
                        .cols
                        .iter()
                        .map(|c| HCol {
                            name: c.name.name,
                            ty: None,
                        })
                        .collect();
                    let id = self.add_rel(HRel {
                        name: self.qual(s, v.name.name),
                        kind: HRelKind::View,
                        cols,
                        key: None,
                        durable: false,
                        role: placement,
                        span: v.name.span,
                    });
                    self.bind_rel(s, v.name, id);
                }
                ItemKind::Timer(t) => {
                    if has_roles && !in_at {
                        self.error(
                            code!("BLS0408"),
                            t.name.span,
                            "in a module with roles, timers go inside `at`",
                        );
                        continue;
                    }
                    if let Some(id) = self.timer(s, t, placement) {
                        self.bind_rel(s, t.name, id);
                    }
                }
                ItemKind::Interpose(_) => {
                    // Declared in `imports`, once the instance exists.
                }
                _ => {}
            }
        }
    }

    /// A timer: `timer name every d;` declares the event relation `name(count: u64, at: Instant)` (LANGUAGE §7.14).
    fn timer(&mut self, s: ScopeIdx, t: &'t ast::TimerDecl, placement: Option<HRoleId>) -> Option<HRelId> {
        let words: Vec<&str> = t.words.iter().map(Ident::as_str).collect();
        if words != ["every"] || t.exprs.len() != 1 {
            self.unsupported(
                "LANG-173",
                "timers other than `timer name every DURATION;` (logical, `once` and bounded timers)",
                t.span,
            );
            return None;
        }
        let every = match t.exprs.first().map(|e| self.const_value(s, e, None)) {
            Some(Some((Value::Duration(d), _))) if d.as_nanos() > 0 => d.as_nanos() as u128,
            Some(Some((Value::Duration(_), _))) => 0,
            Some(Some(_)) => {
                self.error(code!("BLS0300"), t.span, "a timer period must be a Duration");
                return None;
            }
            _ => return None,
        };
        if every == 0 {
            self.error(code!("BLS0300"), t.span, "a timer period must be positive");
            return None;
        }
        let u64_ty = self.intern_type(TypeDef::Int(blossom_value::types::IntTy::U64), t.span);
        let instant = self.intern_type(TypeDef::Instant, t.span);
        Some(self.add_rel(HRel {
            name: self.qual(s, t.name.name),
            kind: HRelKind::Timer { every },
            cols: vec![
                HCol {
                    name: Symbol::intern("count"),
                    ty: Some(u64_ty),
                },
                HCol {
                    name: Symbol::intern("at"),
                    ty: Some(instant),
                },
            ],
            key: None,
            durable: false,
            role: placement,
            span: t.name.span,
        }))
    }

    /// A relation declaration. `root` says whether the scope is the program root (inputs are then host-fed).
    fn rel_decl(&mut self, s: ScopeIdx, d: &'t ast::RelDecl, placement: Option<HRoleId>, root: bool) -> Option<HRelId> {
        if d.mods.soft {
            self.unsupported("LANG-048", "soft tables", d.span);
            return None;
        }
        if d.mods.sealed {
            self.unsupported("LANG-049", "sealed tables", d.span);
            return None;
        }
        if d.mods.zset || d.mods.bag {
            self.unsupported("LANG-138", "weighted tables", d.span);
            return None;
        }
        if d.mods.final_ {
            self.unsupported("LANG-212", "final outputs", d.span);
            return None;
        }
        if let Some((clause, span)) = d.other_clauses.first() {
            self.unsupported("LANG-020", &format!("the `{clause}` clause"), *span);
            return None;
        }
        if d.like.is_some() {
            self.unsupported("LANG-020", "`like` declarations", d.span);
            return None;
        }
        if d.mods.durable && d.kind != RelKind::Table {
            self.error(code!("BLS0106"), d.span, "`durable` applies to tables");
        }
        let mut cols = Vec::new();
        let mut dest_col = None;
        for (i, c) in d.cols.iter().enumerate() {
            if c.default.is_some() {
                self.unsupported("LANG-261", "column defaults", c.span);
            }
            if c.dest {
                if d.kind != RelKind::Channel {
                    self.error(code!("BLS0106"), c.span, "only a channel has an `@` destination column");
                } else if dest_col.is_some() {
                    self.error(code!("BLS0106"), c.span, "a channel has at most one `@` column");
                }
                dest_col = Some(i);
            }
            let ty = self.resolve_type(s, &c.ty)?;
            cols.push(HCol {
                name: c.name.name,
                ty: Some(ty),
            });
        }
        for (i, a) in d.cols.iter().enumerate() {
            if d.cols.iter().skip(i + 1).any(|b| b.name.name == a.name.name) {
                self.error(
                    code!("BLS0201"),
                    a.name.span,
                    format!("column `{}` is declared twice", a.name.as_str()),
                );
            }
        }
        let key = match &d.key {
            None => None,
            Some((names, span)) => {
                if matches!(d.kind, RelKind::Channel | RelKind::Loopback | RelKind::Input) {
                    self.unsupported("SEM-050", "keys on channels and inputs", *span);
                }
                let mut idx = Vec::new();
                for n in names {
                    match d.cols.iter().position(|c| c.name.name == n.name) {
                        Some(i) => idx.push(i),
                        None => self.error(
                            code!("BLS0302"),
                            n.span,
                            format!("no column `{}` to key on", n.as_str()),
                        ),
                    }
                }
                Some(idx)
            }
        };
        let kind = match d.kind {
            RelKind::Table => HRelKind::Table,
            RelKind::Scratch => HRelKind::Scratch,
            RelKind::Static => HRelKind::Static,
            RelKind::Input => HRelKind::Input { root },
            RelKind::Output => HRelKind::Output { root },
            RelKind::Channel | RelKind::Loopback => {
                let loopback = d.kind == RelKind::Loopback;
                let direction = match d.direction {
                    None => {
                        if self.scope(s).has_roles && !loopback && dest_col.is_none() {
                            self.error(
                                code!("BLS0404"),
                                d.name.span,
                                "a channel in a module with roles needs a direction `: Src -> Dst`",
                            );
                        }
                        None
                    }
                    Some((src, dst)) => {
                        if loopback {
                            self.error(code!("BLS0106"), d.span, "a loopback has no direction");
                        }
                        if dest_col.is_some() {
                            self.error(code!("BLS0106"), d.span, "a column-form channel has no direction");
                        }
                        let a = self.role_or_node(s, src);
                        let b = self.role_or_node(s, dst);
                        match (a, b) {
                            (Some(Some(a)), Some(Some(b))) => Some((a, b)),
                            (Some(None), Some(None)) => None,
                            _ => return None,
                        }
                    }
                };
                HRelKind::Channel(ChannelInfo {
                    loopback,
                    direction,
                    dest_col,
                })
            }
        };
        Some(self.add_rel(HRel {
            name: self.qual(s, d.name.name),
            kind,
            cols,
            key,
            durable: d.mods.durable,
            role: placement,
            span: d.name.span,
        }))
    }

    /// A channel endpoint: a role (`Some(Some(r))`) or `Node` (`Some(None)`).
    fn role_or_node(&mut self, s: ScopeIdx, name: Ident) -> Option<Option<HRoleId>> {
        if name.as_str() == "Node" {
            return Some(None);
        }
        match self.scope(s).roles.get(&name.name).copied() {
            Some(r) => Some(Some(r)),
            None => {
                self.error(code!("BLS0200"), name.span, format!("unknown role `{}`", name.as_str()));
                None
            }
        }
    }

    /// Pass 3: imports (instances) and interpositions.
    fn imports(&mut self, s: ScopeIdx, items: &'t [ast::Item], placement: Option<HRoleId>) {
        for item in items {
            match &item.kind {
                ItemKind::At { role, items: inner } => {
                    let r = self.scope(s).roles.get(&role.name).copied();
                    self.imports(s, inner, r);
                }
                ItemKind::Import(imp) => self.instantiate(s, imp, placement, item.span),
                _ => {}
            }
        }
        for item in items {
            if let ItemKind::Interpose(ip) = &item.kind {
                self.declare_interpose(s, ip, placement);
            }
        }
    }

    /// Pass 4: rules.
    fn rules(&mut self, s: ScopeIdx, items: &'t [ast::Item], placement: Option<HRoleId>) {
        let has_roles = self.scope(s).has_roles;
        for item in items {
            match &item.kind {
                ItemKind::At { role, items: inner } => {
                    let r = self.scope(s).roles.get(&role.name).copied();
                    self.rules(s, inner, r);
                }
                ItemKind::Handler(h) => {
                    if has_roles && placement.is_none() {
                        self.error(
                            code!("BLS0408"),
                            h.span,
                            "in a module with roles, handlers go inside `at`",
                        );
                        continue;
                    }
                    self.handler(s, h, placement, &item.attrs);
                }
                ItemKind::Bootstrap { fresh, block } => {
                    if has_roles && placement.is_none() {
                        self.error(
                            code!("BLS0408"),
                            item.span,
                            "in a module with roles, bootstraps go inside `at`",
                        );
                        continue;
                    }
                    self.bootstrap(s, *fresh, block, placement, item.span);
                }
                ItemKind::View(v) => {
                    if let Some(id) = self.scope(s).rels.get(&v.name.name).copied() {
                        self.view(s, v, id);
                    }
                }
                ItemKind::Fact(f) => self.fact(s, f),
                ItemKind::Interpose(ip) => self.interpose_rules(s, ip, placement),
                ItemKind::Invariant(inv) => {
                    if has_roles && placement.is_none() {
                        self.error(
                            code!("BLS0408"),
                            inv.span,
                            "in a module with roles, invariants go inside `at`",
                        );
                        continue;
                    }
                    self.invariant(s, inv, placement);
                }
                ItemKind::Spec(spec) => {
                    self.unsupported("TEST-020", "spec items inside a program", spec.span);
                }
                ItemKind::Use(_)
                | ItemKind::Import(_)
                | ItemKind::Include(_)
                | ItemKind::Const { .. }
                | ItemKind::TypeAlias { .. }
                | ItemKind::Struct(_)
                | ItemKind::Enum(_)
                | ItemKind::Module(_)
                | ItemKind::Protocol(_)
                | ItemKind::Role { .. }
                | ItemKind::Rel(_)
                | ItemKind::Timer(_)
                | ItemKind::Unsupported { .. } => {}
                ItemKind::Param { name, .. } => {
                    self.unsupported("LANG-010", "deploy-time parameters", name.span);
                }
            }
        }
    }

    /// `import M<T…>(K = v, …) as a [with (R = S, …)]` (LANGUAGE §6.5, §6.10).
    fn instantiate(&mut self, s: ScopeIdx, imp: &'t ast::Import, placement: Option<HRoleId>, span: Span) {
        let alias = imp.alias;
        if self.scope(s).instances.contains_key(&alias.name) || self.scope(s).rels.contains_key(&alias.name) {
            self.error(
                code!("BLS0201"),
                alias.span,
                format!("`{}` is already defined", alias.as_str()),
            );
            return;
        }
        let Some((file, module)) = self.find_module(s, &imp.module) else {
            let path: Vec<&str> = imp.module.iter().map(Ident::as_str).collect();
            self.error(
                code!("BLS0200"),
                imp.module.first().map_or(span, |i| i.span),
                format!("unknown module `{}`", path.join("::")),
            );
            return;
        };
        // Type parameters.
        if module.generics.len() != imp.type_args.len() {
            self.error(
                code!("BLS0301"),
                span,
                format!(
                    "`{}` takes {} type argument(s), {} given",
                    module.name.as_str(),
                    module.generics.len(),
                    imp.type_args.len()
                ),
            );
            return;
        }
        let mut generics = BTreeMap::new();
        for (g, a) in module.generics.iter().zip(&imp.type_args) {
            if !g.bounds.is_empty() {
                self.unsupported("LANG-006", "protocol-bounded type parameters", g.name.span);
                return;
            }
            let Some(t) = self.resolve_type(s, a) else { return };
            generics.insert(g.name.name, t);
        }
        let mut prefix = self.scope(s).prefix.clone();
        prefix.push(alias.name);
        let child = self.new_scope(ModScope {
            file: file.clone(),
            prefix,
            generics,
            values: BTreeMap::new(),
            rels: BTreeMap::new(),
            instances: BTreeMap::new(),
            roles: BTreeMap::new(),
            role_template: None,
            has_roles: false,
            write_redirect: BTreeMap::new(),
            own_items: Some(&module.items),
        });
        // Value and relation parameters.
        let mut given: BTreeMap<Symbol, &'t ast::Expr> = BTreeMap::new();
        for a in &imp.args {
            match a {
                ast::Arg::Named(n, e) => {
                    if given.insert(n.name, e).is_some() {
                        self.error(
                            code!("BLS0201"),
                            n.span,
                            format!("parameter `{}` given twice", n.as_str()),
                        );
                    }
                }
                other => {
                    self.error(
                        code!("BLS0205"),
                        other.span(),
                        "module arguments are written `NAME = value`",
                    );
                }
            }
        }
        for p in &module.params {
            let arg = given.remove(&p.name.name);
            match &p.kind {
                ast::ModParamKind::Value { ty, default } => {
                    let Some(t) = self.resolve_type(child, ty) else {
                        continue;
                    };
                    let value = match (arg, default) {
                        (Some(e), _) => self.const_value(s, e, Some(t)),
                        (None, Some(d)) => self.const_value(child, d, Some(t)),
                        (None, None) => {
                            self.error(
                                code!("BLS0205"),
                                span,
                                format!("module parameter `{}` has no default and is not given", p.name.as_str()),
                            );
                            None
                        }
                    };
                    if let Some(v) = value {
                        self.scope_mut(child).values.insert(p.name.name, v);
                    }
                }
                ast::ModParamKind::Rel { cols } => {
                    let Some(e) = arg else {
                        self.error(
                            code!("BLS0205"),
                            span,
                            format!("relation parameter `{}` is not bound", p.name.as_str()),
                        );
                        continue;
                    };
                    let target = match &e.kind {
                        ast::ExprKind::Path(path, targs) if targs.is_empty() => self.lookup_rel(s, path),
                        _ => None,
                    };
                    let Some(target) = target else {
                        self.error(
                            code!("BLS0205"),
                            e.span,
                            format!("relation parameter `{}` must be bound to a relation", p.name.as_str()),
                        );
                        continue;
                    };
                    let mut want = Vec::new();
                    for (_, t) in cols {
                        if let Some(t) = self.resolve_type(child, t) {
                            want.push(Some(t));
                        }
                    }
                    let target_rel = self.rel_of(target);
                    let have: Vec<Option<TypeId>> = target_rel.cols.iter().map(|c| c.ty).collect();
                    if want != have {
                        self.error(
                            code!("BLS0205"),
                            e.span,
                            format!(
                                "relation parameter `{}` expects columns of different types than `{}` has",
                                p.name.as_str(),
                                target_rel.name
                            ),
                        );
                        continue;
                    }
                    self.scope_mut(child).rels.insert(p.name.name, target);
                }
            }
        }
        for (name, e) in given {
            self.error(
                code!("BLS0205"),
                e.span,
                format!("`{}` has no parameter `{}`", module.name.as_str(), name.as_str()),
            );
        }
        // Roles.
        if module.choreography {
            let template: Vec<Symbol> = imp.roles.iter().map(|(t, _)| t.name).collect();
            for (t, target) in &imp.roles {
                match self.scope(s).roles.get(&target.name).copied() {
                    Some(r) => {
                        self.scope_mut(child).roles.insert(t.name, r);
                    }
                    None => self.error(
                        code!("BLS0200"),
                        target.span,
                        format!("unknown role `{}`", target.as_str()),
                    ),
                }
            }
            let declared: Vec<Symbol> = module
                .items
                .iter()
                .filter_map(|i| match &i.kind {
                    ItemKind::Role { name, .. } => Some(name.name),
                    _ => None,
                })
                .collect();
            for d in &declared {
                if !template.contains(d) {
                    self.error(
                        code!("BLS0206"),
                        span,
                        format!(
                            "role `{}` of `{}` is not bound by `with (…)`",
                            d.as_str(),
                            module.name.as_str()
                        ),
                    );
                }
            }
            self.scope_mut(child).role_template = Some(declared);
            self.scope_mut(child).has_roles = true;
            if placement.is_some() {
                self.error(
                    code!("BLS0110"),
                    span,
                    "a choreography is imported at the root, not inside `at`",
                );
            }
        } else if !imp.roles.is_empty() {
            self.error(
                code!("BLS0206"),
                span,
                "`with (…)` binds the roles of a choreography only",
            );
        }
        // Protocol interfaces.
        for proto in &module.protocols {
            self.protocol_interfaces(child, proto, placement);
        }
        self.process(child, &module.items, placement);
        // The importer sees the instance's interfaces.
        let mut instance = Instance {
            interface: BTreeMap::new(),
        };
        let names: Vec<(Symbol, HRelId)> = self.scope(child).rels.iter().map(|(k, v)| (*k, *v)).collect();
        for (name, id) in names {
            match self.rel_of(id).kind {
                HRelKind::Input { root: false } => {
                    instance.interface.insert(name, (id, true));
                }
                HRelKind::Output { root: false } => {
                    instance.interface.insert(name, (id, false));
                }
                _ => {}
            }
        }
        self.scope_mut(s).instances.insert(alias.name, instance);
    }

    /// Declares the interfaces of `module M: P<…>` in the instance scope (LANGUAGE §6.7).
    pub(crate) fn protocol_interfaces(&mut self, child: ScopeIdx, proto: &'t ast::Type, placement: Option<HRoleId>) {
        let ast::Type::Named { path, args, span } = proto else {
            self.error(code!("BLS0206"), proto.span(), "a module's protocols are named");
            return;
        };
        let Some((pfile, p)) = self.find_protocol(child, path) else {
            self.error(code!("BLS0200"), *span, "unknown protocol");
            return;
        };
        if p.generics.len() != args.len() {
            self.error(code!("BLS0301"), *span, "wrong number of protocol type arguments");
            return;
        }
        let mut bound = BTreeMap::new();
        for (g, a) in p.generics.iter().zip(args) {
            let Some(t) = self.resolve_type(child, a) else { return };
            bound.insert(g.name.name, t);
        }
        // Resolve the protocol's declarations in a scratch scope over the protocol's file, with its generics bound,
        // then move the relations into the instance.
        let prefix = self.scope(child).prefix.clone();
        let tmp = self.new_scope(ModScope {
            file: pfile,
            prefix,
            generics: bound,
            values: BTreeMap::new(),
            rels: BTreeMap::new(),
            instances: BTreeMap::new(),
            roles: BTreeMap::new(),
            role_template: None,
            has_roles: false,
            write_redirect: BTreeMap::new(),
            own_items: Some(&p.items),
        });
        for item in &p.items {
            match &item.kind {
                ItemKind::Rel(d) if matches!(d.kind, RelKind::Input | RelKind::Output) => {
                    let root = self.scope(child).prefix.is_empty();
                    if let Some(id) = self.rel_decl(tmp, d, placement, root) {
                        self.bind_rel(child, d.name, id);
                    }
                }
                ItemKind::Rel(d) => {
                    self.error(
                        code!("BLS0110"),
                        d.name.span,
                        "a protocol declares only inputs and outputs",
                    );
                }
                _ => {}
            }
        }
    }

    /// Looks up a relation by path in scope `s`: `r`, or `a.r` for an instance interface.
    pub fn lookup_rel(&mut self, s: ScopeIdx, path: &[Ident]) -> Option<HRelId> {
        match path {
            [name] => self
                .scope(s)
                .rels
                .get(&name.name)
                .copied()
                .or_else(|| match name.as_str() {
                    "boot" => Some(self.builtin(BuiltinRel::Boot, name.span)),
                    "localtick" => Some(self.builtin(BuiltinRel::LocalTick, name.span)),
                    "halt" => Some(self.builtin(BuiltinRel::Halt, name.span)),
                    "crashed" if self.spec.is_some() => Some(self.crashed_oracle(name.span)),
                    _ => None,
                }),
            [inst, name] => self
                .scope(s)
                .instances
                .get(&inst.name)
                .and_then(|i| i.interface.get(&name.name))
                .map(|(id, _)| *id),
            _ => None,
        }
    }

    /// `interpose a.i as (outside, inside) { … }`: declares the renamed pair (LANGUAGE §6.9).
    fn declare_interpose(&mut self, s: ScopeIdx, ip: &'t ast::Interpose, _placement: Option<HRoleId>) {
        let [inst, name] = ip.target.as_slice() else {
            self.error(
                code!("BLS0208"),
                ip.outside.span,
                "only an instance interface `a.i` can be interposed",
            );
            return;
        };
        let Some(&(real, is_input)) = self
            .scope(s)
            .instances
            .get(&inst.name)
            .and_then(|i| i.interface.get(&name.name))
        else {
            self.error(
                code!("BLS0208"),
                name.span,
                format!(
                    "`{}.{}` is not an interface of an instance",
                    inst.as_str(),
                    name.as_str()
                ),
            );
            return;
        };
        if !is_input {
            self.unsupported("LANG-008", "interposition on an output interface", ip.outside.span);
            return;
        }
        if self.scope(s).write_redirect.contains_key(&real) {
            self.error(code!("BLS0208"), name.span, "an interface may be interposed once");
            return;
        }
        let r = self.rel_of(real).clone();
        let mut segs = r.name.segments().to_vec();
        if let Some(last) = segs.last_mut() {
            *last = Symbol::intern(&format!("{}$outside", last.as_str()));
        }
        let outside = self.add_rel(HRel {
            name: QualName::new(segs),
            kind: HRelKind::Scratch,
            ..r
        });
        self.scope_mut(s).write_redirect.insert(real, outside);
    }

    /// Fold `const` items of the scope's module and file.
    fn fold_consts(&mut self, s: ScopeIdx, items: &'t [ast::Item]) {
        for item in items {
            if let ItemKind::Const { name, ty, value } = &item.kind {
                let Some(t) = self.resolve_type(s, ty) else { continue };
                if let Some(v) = self.const_value(s, value, Some(t)) {
                    if self.scope(s).values.contains_key(&name.name) {
                        self.error(
                            code!("BLS0201"),
                            name.span,
                            format!("`{}` is defined twice", name.as_str()),
                        );
                    }
                    self.scope_mut(s).values.insert(name.name, v);
                }
            }
        }
    }
}

/// `line` without a trailing `//` comment (a `//` inside a string literal is kept).
fn strip_comment(line: &str) -> &str {
    let mut in_str = false;
    let mut escaped = false;
    let mut prev_slash = None;
    for (i, c) in line.char_indices() {
        if in_str {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                prev_slash = None;
            }
            '/' => {
                if let Some(start) = prev_slash {
                    return line.get(..start).unwrap_or(line);
                }
                prev_slash = Some(i);
            }
            _ => prev_slash = None,
        }
    }
    line
}
