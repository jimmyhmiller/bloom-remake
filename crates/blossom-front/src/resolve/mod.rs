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
mod generic;
mod sugar;
mod types;

pub(crate) use types::int_value;

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::TypeId;
use blossom_base::{Diagnostic, Diagnostics, InternalError, QualName, SourceDb, Span, Symbol, code};
use blossom_value::{TypeDef, TypeTable, Value};

use crate::ast::{self, Ident, ItemKind, RelKind};
use crate::hir::*;
use crate::modules::ModuleTree;

/// Resolves the program rooted at `tree.root`. Returns `None` when the program is rejected; `diags` says why.
pub fn resolve(
    tree: &ModuleTree,
    sources: &SourceDb,
    diags: &mut Diagnostics,
    params: &BTreeMap<String, crate::api::ParamBinding>,
) -> Result<Option<Hir>, InternalError> {
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
    r.param_bindings = params.clone();
    let root = r.new_scope(ModScope::empty(FileKey::Root));
    r.process(root, &tree.root.items, None);
    for name in params.keys() {
        if !r.params_declared.contains(name) {
            let span = Span::point(tree.root.span.file, 0);
            r.error(
                code!("BLS0205"),
                span,
                format!("the deployment binds `{name}`, which the program does not declare as a `param`"),
            );
        }
    }
    r.finish()
}

/// Resolves the module `path` of the root file (or of a module it uses), instantiated as a program root: the
/// target of a spec (LANGUAGE §17.2). Its inputs are host-fed, its value parameters take `args` or their defaults.
/// A one-segment `path` that names no module may name a program file next to the root (`spec S for e03_raft`): its
/// items are the target, its `param`s bound by `args` as a deployment binds them.
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
    // The target's parts: a module's, or a program file's (its items at its own file level, no header parameters).
    let target = match r.find_module(file_scope, path) {
        Some((file, module)) => Target {
            file,
            name: module.name.as_str().to_owned(),
            generics: &module.generics,
            params: &module.params,
            protocols: &module.protocols,
            items: &module.items,
            own_items: Some(&module.items),
        },
        None => match path {
            [one] if tree.modules.get(one.as_str()).is_some_and(|f| f.header.is_some()) => {
                let items = tree.modules.get(one.as_str()).map_or(&[][..], |f| f.items.as_slice());
                Target {
                    file: FileKey::Module(one.as_str().to_owned()),
                    name: one.as_str().to_owned(),
                    generics: &[],
                    params: &[],
                    protocols: &[],
                    items,
                    own_items: None,
                }
            }
            _ => {
                let names: Vec<&str> = path.iter().map(Ident::as_str).collect();
                let span = path.first().map_or(Span::point(tree.root.span.file, 0), |i| i.span);
                let message = match path {
                    [one] if tree.modules.contains_key(one.as_str()) => format!(
                        "`{0}.bls` is not a program (it has no `program` header) and declares no module `{0}`",
                        one.as_str()
                    ),
                    _ => format!("unknown module `{}`", names.join("::")),
                };
                r.error(code!("BLS0200"), span, message);
                return Ok(None);
            }
        },
    };
    let root = r.new_scope(ModScope {
        own_items: target.own_items,
        ..ModScope::empty(target.file.clone())
    });
    if target.generics.len() != type_args.len() {
        let span = path.first().map_or(Span::point(tree.root.span.file, 0), |i| i.span);
        r.error(
            code!("BLS0301"),
            span,
            format!(
                "`{}` takes {} type argument(s), {} given",
                target.name,
                target.generics.len(),
                type_args.len()
            ),
        );
        return Ok(None);
    }
    for (g, a) in target.generics.iter().zip(type_args) {
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
    for p in target.params {
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
    // An argument that is not a parameter of the module's header binds a `param` of its body, as a deployment does.
    let mut body_params = Vec::new();
    for (name, e) in given {
        let binding = match r.const_value(file_scope, e, None) {
            Some((Value::Int(i), _)) => i.to_i128().map(crate::api::ParamBinding::Int),
            Some((Value::Bool(b), _)) => Some(crate::api::ParamBinding::Bool(b)),
            Some((Value::Str(t), _)) => Some(crate::api::ParamBinding::Text(t.to_string())),
            Some((Value::Duration(d), _)) => Some(crate::api::ParamBinding::Text(format!("{}ns", d.as_nanos()))),
            _ => None,
        };
        let Some(binding) = binding else {
            r.error(
                code!("BLS0205"),
                e.span,
                format!(
                    "the value for `{}` is not an integer, a bool, a string or a duration",
                    name.as_str()
                ),
            );
            continue;
        };
        r.param_bindings.insert(name.as_str().to_owned(), binding);
        body_params.push((name, e.span));
    }
    for proto in target.protocols {
        r.protocol_interfaces(root, proto, None);
    }
    r.spec = spec;
    r.process(root, target.items, None);
    for (name, span) in body_params {
        if !r.params_declared.contains(name.as_str()) {
            r.error(
                code!("BLS0205"),
                span,
                format!("`{}` has no parameter `{}`", target.name, name.as_str()),
            );
        }
    }
    let spec = r.spec.take();
    Ok(r.finish()?.map(|h| (h, spec)))
}

/// What a spec targets: a module, or a program file.
struct Target<'a> {
    file: FileKey,
    name: String,
    generics: &'a [ast::GenericParam],
    params: &'a [ast::ModParam],
    protocols: &'a [ast::Type],
    items: &'a [ast::Item],
    /// The module's own items (a program file's items are its file level).
    own_items: Option<&'a [ast::Item]>,
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
    r.hir.lattices = target.lattices.clone();
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
    /// Relations whose declaration failed (and was reported): uses of them are not reported again.
    pub broken: BTreeSet<Symbol>,
    /// Pure functions declared in this module, by name (LANGUAGE §16.1).
    pub fns: BTreeMap<Symbol, HFnId>,
    /// Generic functions (and functions with function parameters) declared in this module: their templates.
    pub generic_fns: BTreeMap<Symbol, usize>,
    /// Trees declared in this module, by name (docs/design/SUGAR.md §3).
    pub trees: BTreeMap<Symbol, sugar::TreeInfo>,
    /// Fragments declared in this module, by name (SUGAR.md §4).
    pub fragments: BTreeMap<Symbol, &'t ast::FragmentItem>,
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
            broken: BTreeSet::new(),
            fns: BTreeMap::new(),
            generic_fns: BTreeMap::new(),
            trees: BTreeMap::new(),
            fragments: BTreeMap::new(),
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
    Recovered,
    LocalTick,
    Halt,
    NodeDir,
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
    /// Structs and enums already interned, by home (defining file and module body, `types::Home::body_id`) and name.
    pub nominal: BTreeMap<(FileKey, usize, Symbol), TypeId>,
    /// The user lattices being resolved, by home and name: one met again contains itself.
    pub lattices_resolving: BTreeSet<(FileKey, usize, Symbol)>,
    /// The `impl` items already resolved (by address): an item is resolved once, however many scopes reach it.
    pub impls_done: BTreeSet<usize>,
    pub rel_spans: BTreeMap<HRelId, Span>,
    /// Frontend bugs met while resolving (a lookup of an id the resolver minted that fails).
    pub bugs: Vec<InternalError>,
    /// What a missing scope reads as.
    pub empty: ModScope<'t>,
    /// Spec mode, when resolving a spec's views.
    pub spec: Option<SpecMode>,
    /// The deployment's values of deploy-time parameters (LANG-010), by name; a `param` it does not bind takes its
    /// default.
    pub param_bindings: BTreeMap<String, crate::api::ParamBinding>,
    /// The parameters declared, to report bindings of names that are not parameters.
    pub params_declared: BTreeSet<String>,
    /// Generic functions' templates (LANGUAGE §16.1), instantiated per call.
    pub templates: Vec<generic::Template<'t>>,
    /// The functions each generic function instance calls, for the recursion check (BLS0213).
    pub instance_calls: BTreeMap<HFnId, BTreeSet<HFnId>>,
    /// The templates being instantiated, innermost last: a template met again is recursive.
    pub instantiating: Vec<usize>,
    /// The fragments being expanded, innermost last: one met again calls itself (BLS0433).
    pub fragments_expanding: Vec<Symbol>,
    /// Templates already reported recursive.
    pub recursive: BTreeSet<usize>,
    /// Whether the instance bound was reported (once).
    pub instances_capped: bool,
    /// How many handlers of each module carry each label (a `resolve prefer` name must label exactly one).
    pub handler_labels: BTreeMap<(ScopeIdx, Symbol), u32>,
    /// The (table, handler label) pairs of `next`/`upsert` writes into tables with `resolve prefer`.
    pub prefer_writers: BTreeSet<(HRelId, Symbol)>,
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
                lattices: Vec::new(),
                roles: Vec::new(),
                rels: Vec::new(),
                handlers: Vec::new(),
                views: Vec::new(),
                facts: Vec::new(),
                invariants: Vec::new(),
                guards: Vec::new(),
                fns: Vec::new(),
                methods: Vec::new(),
                streams: Vec::new(),
                scopes: Vec::new(),
                var_types: Vec::new(),
            },
            scopes: Vec::new(),
            builtins: BTreeMap::new(),
            members: BTreeMap::new(),
            nominal: BTreeMap::new(),
            lattices_resolving: BTreeSet::new(),
            impls_done: BTreeSet::new(),
            rel_spans: BTreeMap::new(),
            bugs: Vec::new(),
            empty: ModScope::empty(FileKey::Root),
            spec: None,
            param_bindings: BTreeMap::new(),
            params_declared: BTreeSet::new(),
            templates: Vec::new(),
            instance_calls: BTreeMap::new(),
            instantiating: Vec::new(),
            fragments_expanding: Vec::new(),
            recursive: BTreeSet::new(),
            instances_capped: false,
            handler_labels: BTreeMap::new(),
            prefer_writers: BTreeSet::new(),
        }
    }

    /// The HIR, unless a bug or an error was reported.
    pub(crate) fn finish(mut self) -> Result<Option<Hir>, InternalError> {
        self.node_local_conns();
        if let Some(bug) = self.bugs.into_iter().next() {
            return Err(bug);
        }
        if self.diags.has_errors() {
            return Ok(None);
        }
        Ok(Some(self.hir))
    }

    /// A `Conn` names a connection of one node's incarnation (FOREIGN-PROTOCOLS §1.1), so it may not reach another
    /// node or outlive the incarnation: a channel or a durable relation that holds one is BLS0315.
    fn node_local_conns(&mut self) {
        let mut bad = Vec::new();
        for rel in &self.hir.rels {
            let crosses = matches!(rel.kind, HRelKind::Channel(_));
            if !(crosses || rel.durable) {
                continue;
            }
            if let Some(c) = rel.cols.iter().find(|c| {
                c.ty.is_some_and(|t| holds_conn(&self.hir.types, t, &mut BTreeSet::new()))
            }) {
                let what = if crosses { "a channel" } else { "a durable relation" };
                bad.push((
                    rel.span,
                    format!(
                        "column `{}` of {what} `{}` holds a `Conn`, which names a connection of this node's \
                         incarnation only",
                        c.name.as_str(),
                        rel.name
                    ),
                ));
            }
        }
        for (span, msg) in bad {
            self.error(code!("BLS0315"), span, msg);
        }
        // A blob's bytes live in its node's store: a channel would carry the handle without them, and a host input
        // would name bytes the store never got. Neither is built yet (LANG-028).
        let mut blobs = Vec::new();
        for rel in &self.hir.rels {
            let what = match rel.kind {
                HRelKind::Channel(_) => "a channel",
                HRelKind::Input { root: true } => "a host input",
                _ => continue,
            };
            if rel.cols.iter().any(|c| {
                c.ty.is_some_and(|t| {
                    holds(
                        &self.hir.types,
                        t,
                        &|d| matches!(d, TypeDef::Blob),
                        &mut BTreeSet::new(),
                    )
                })
            }) {
                blobs.push((
                    rel.span,
                    format!("a `Blob` in {what} (`{}`): blobs do not leave their node", rel.name),
                ));
            }
        }
        for (span, what) in blobs {
            self.unsupported("LANG-028", &what, span);
        }
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
            BuiltinRel::Recovered => ("recovered", HRelKind::Recovered),
            BuiltinRel::LocalTick => ("localtick", HRelKind::LocalTick),
            BuiltinRel::Halt => ("halt", HRelKind::Halt),
            BuiltinRel::NodeDir => ("node_dir", HRelKind::NodeDir),
        };
        let cols = match which {
            BuiltinRel::Halt => vec![HCol {
                name: Symbol::intern("kill"),
                ty: Some(self.intern_type(TypeDef::Bool, span)),
            }],
            BuiltinRel::NodeDir => {
                let node = self.node_type(None);
                let string = self.intern_type(TypeDef::Str, span);
                let principal = self.intern_type(TypeDef::Principal, span);
                [
                    ("node", node),
                    ("addr", string),
                    ("principal", principal),
                    ("role", string),
                ]
                .into_iter()
                .map(|(n, t)| HCol {
                    name: Symbol::intern(n),
                    ty: Some(t),
                })
                .collect()
            }
            _ => Vec::new(),
        };
        let id = self.add_rel(HRel {
            name: QualName::single(Symbol::intern(name)),
            kind,
            cols,
            key: None,
            durable: false,
            cell: false,
            resolve: None,
            prefer: None,
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
            cell: false,
            resolve: None,
            prefer: None,
            role: None,
            span,
        });
        if let Some(spec) = self.spec.as_mut() {
            spec.crashed = Some(id);
        }
        id
    }

    /// The trace relation of target relation `name` at `time` (`None`: the evaluation point), created on first use:
    /// the target's columns after a `node: Node` column. A `Blob` column is its reference (its content's hash and
    /// length, as `Bytes`): a blob's bytes stay in its node's store, but which blob a tuple holds the spec may compare.
    pub fn trace_rel(&mut self, name: Ident, time: Option<u64>) -> Option<HRelId> {
        let target_cols = self.spec.as_ref()?.targets.get(&name.name)?.clone();
        let bytes = self.intern_type(TypeDef::Bytes, name.span);
        let cols: Vec<HCol> = target_cols
            .into_iter()
            .map(|c| match c.ty.and_then(|t| self.hir.types.get(t)) {
                Some(TypeDef::Blob) => HCol {
                    name: c.name,
                    ty: Some(bytes),
                },
                _ => c,
            })
            .collect();
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
            cell: false,
            resolve: None,
            prefer: None,
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
            cell: false,
            resolve: None,
            prefer: None,
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
        self.acls(s, items);
        self.imports(s, items, placement);
        // After the imports, so a guard can name an instance's member (and be told it is not a view or table).
        self.timer_guards(s, items);
        self.trees(s, items);
        self.functions(s, items);
        self.rules(s, items, placement);
        self.check_prefer(s);
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
                    match self.rel_decl(s, d, if shared { None } else { placement }, root) {
                        Some(id) => self.bind_rel(s, d.name, id),
                        None => {
                            self.scope_mut(s).broken.insert(d.name.name);
                        }
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
                        cell: false,
                        resolve: None,
                        prefer: None,
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
                ItemKind::Stream { name, kind } => {
                    if has_roles && !in_at {
                        self.error(
                            code!("BLS0408"),
                            name.span,
                            "in a module with roles, streams go inside `at`",
                        );
                        continue;
                    }
                    self.stream(s, *name, *kind, placement, item.span);
                }
                ItemKind::Interpose(_) => {
                    // Declared in `imports`, once the instance exists.
                }
                _ => {}
            }
        }
    }

    /// Pass 2b: the `while` guards of the timers declared in `items` (LANGUAGE §15.2), once every relation of the
    /// scope is known: a view or table placed where the timer is (the node observes it after each tick).
    /// Pass: tree declarations (docs/design/SUGAR.md §3): each role's relation, its columns in the role's order, and
    /// their shapes (BLS0434).
    fn trees(&mut self, s: ScopeIdx, items: &'t [ast::Item]) {
        for item in items {
            if let ItemKind::Fragment(f) = &item.kind {
                if self.scope(s).fragments.contains_key(&f.name.name) {
                    self.error(
                        code!("BLS0201"),
                        f.name.span,
                        format!("`{}` is declared twice", f.name.as_str()),
                    );
                } else {
                    self.scope_mut(s).fragments.insert(f.name.name, f);
                }
                for (p, _) in &f.params {
                    if !p.as_str().starts_with(|c: char| c.is_ascii_lowercase() || c == '_') || p.as_str() == "_" {
                        self.error(
                            code!("BLS0436"),
                            p.span,
                            format!(
                                "`{}` is not a variable name: a fragment's parameters start with a lowercase letter \
                                 or `_`",
                                p.as_str()
                            ),
                        );
                    }
                }
                continue;
            }
            let ItemKind::Tree(t) = &item.kind else { continue };
            let mut node = None;
            let mut props = None;
            let mut content = None;
            let mut ok = true;
            for r in &t.roles {
                let arity = match r.role.as_str() {
                    "node" => 4,
                    "props" => 3,
                    "content" => 2,
                    other => {
                        self.error(
                            code!("BLS0434"),
                            r.role.span,
                            format!("a tree's roles are `node`, `props` and `content`, not `{other}`"),
                        );
                        ok = false;
                        continue;
                    }
                };
                let Some(rel) = self.lookup_rel(s, &r.rel) else {
                    let name: Vec<&str> = r.rel.iter().map(ast::Ident::as_str).collect();
                    self.error(code!("BLS0434"), r.span, format!("no relation `{}`", name.join(".")));
                    ok = false;
                    continue;
                };
                let cols: Vec<Symbol> = self.rel_of(rel).cols.iter().map(|c| c.name).collect();
                if r.cols.len() != arity || cols.len() != arity {
                    self.error(
                        code!("BLS0434"),
                        r.span,
                        format!(
                            "a tree's `{}` relation has {arity} columns, named here in the role's order",
                            r.role.as_str()
                        ),
                    );
                    ok = false;
                    continue;
                }
                let mut places = Vec::new();
                for c in &r.cols {
                    match cols.iter().position(|x| *x == c.name) {
                        Some(i) => places.push(i),
                        None => {
                            self.error(
                                code!("BLS0434"),
                                c.span,
                                format!("the relation has no column `{}`", c.as_str()),
                            );
                            ok = false;
                        }
                    }
                }
                let role = sugar::Role {
                    path: r.rel.clone(),
                    rel,
                    cols: places,
                };
                let slot = match r.role.as_str() {
                    "node" => &mut node,
                    "props" => &mut props,
                    _ => &mut content,
                };
                if slot.is_some() {
                    self.error(code!("BLS0434"), r.span, "a role given twice");
                    ok = false;
                }
                *slot = Some(role);
            }
            let Some(node) = node else {
                if ok {
                    self.error(code!("BLS0434"), t.span, "a tree needs a `node` relation");
                }
                continue;
            };
            // The ids are one type throughout; a position is an integer.
            let ty = |me: &mut Self, role: &sugar::Role, place: usize| {
                let rel = me.rel_of(role.rel);
                role.cols.get(place).and_then(|c| rel.cols.get(*c)).and_then(|c| c.ty)
            };
            let id = ty(self, &node, 0);
            let pos = ty(self, &node, 2);
            let mut shapes_ok =
                ty(self, &node, 1) == id && matches!(pos.and_then(|t| self.hir.types.get(t)), Some(TypeDef::Int(_)));
            for role in props.iter().chain(content.iter()) {
                shapes_ok &= ty(self, role, 0) == id;
            }
            if !shapes_ok {
                self.error(
                    code!("BLS0434"),
                    t.span,
                    "a tree's ids (the node's id and parent, the props' and content's ids) are one type, and its \
                     position an integer",
                );
                continue;
            }
            if ok {
                self.scope_mut(s)
                    .trees
                    .insert(t.name.name, sugar::TreeInfo { node, props, content });
            }
        }
    }

    fn timer_guards(&mut self, s: ScopeIdx, items: &'t [ast::Item]) {
        for item in items {
            match &item.kind {
                ItemKind::At { items: inner, .. } => self.timer_guards(s, inner),
                ItemKind::Timer(t) => {
                    let Some(path) = &t.guard else { continue };
                    // A timer that failed to declare was reported.
                    let Some(timer) = self.scope(s).rels.get(&t.name.name).copied() else {
                        continue;
                    };
                    let span = path.last().map_or(t.span, |i| i.span);
                    let Some(g) = self.lookup_rel(s, path) else {
                        let name: Vec<&str> = path.iter().map(Ident::as_str).collect();
                        self.error(
                            code!("BLS0200"),
                            span,
                            format!("unknown relation `{}` in the timer's `while` guard", name.join(".")),
                        );
                        continue;
                    };
                    // Both were declared (`lookup_rel` and the scope only hold declared relations).
                    let (Some((gk, grole)), Some(trole)) = (
                        self.hir.rels.get(g.index()).map(|r| (r.kind.clone(), r.role)),
                        self.hir.rels.get(timer.index()).map(|r| r.role),
                    ) else {
                        continue;
                    };
                    if !matches!(gk, HRelKind::View | HRelKind::Table) {
                        self.error(
                            code!("BLS0412"),
                            span,
                            "a timer's `while` guard must be a view or a table (the node checks it after each tick)",
                        );
                        continue;
                    }
                    if grole.is_some() && grole != trole {
                        self.error(
                            code!("BLS0412"),
                            span,
                            "a timer's `while` guard must be placed at the timer's role",
                        );
                        continue;
                    }
                    if let Some(HRelKind::Timer { guard, .. }) =
                        self.hir.rels.get_mut(timer.index()).map(|r| &mut r.kind)
                    {
                        *guard = Some(g);
                    }
                }
                _ => {}
            }
        }
    }

    /// Pass 2b: the explicit ACLs of the channels declared in `items` (LANGUAGE §18.3), once every relation and
    /// role of the scope is known.
    fn acls(&mut self, s: ScopeIdx, items: &'t [ast::Item]) {
        for item in items {
            match &item.kind {
                ItemKind::At { items: inner, .. } => self.acls(s, inner),
                ItemKind::Rel(d) if d.kind == RelKind::Channel => {
                    let mut accepts = item.attrs.iter().filter(|a| a.name.as_str() == "accept");
                    let Some(first) = accepts.next() else { continue };
                    for extra in accepts {
                        self.error(
                            code!("BLS0210"),
                            extra.span,
                            "a channel takes at most one `#[accept(…)]`",
                        );
                    }
                    // A duplicate or failed declaration was reported; only the channel this item declared gets the
                    // ACL.
                    let Some(id) = self.scope(s).rels.get(&d.name.name).copied() else {
                        continue;
                    };
                    if self.rel_of(id).span != d.name.span {
                        continue;
                    }
                    if let Some(acl) = self.accept(s, id, first)
                        && let Some(HRel {
                            kind: HRelKind::Channel(ch),
                            ..
                        }) = self.hir.rels.get_mut(id.index())
                    {
                        ch.acl = Some(acl);
                    }
                }
                _ => {}
            }
        }
    }

    /// `#[accept(sources…[, principal in REL])]` on the channel `id`: its sources are role names and `external`
    /// (client sessions of the channel's external source role); `REL` is a unary `static` or `table` relation of
    /// type `Principal` at the receiving role.
    fn accept(&mut self, s: ScopeIdx, id: HRelId, a: &ast::Attr) -> Option<HAcl> {
        let rel = self.rel_of(id);
        let HRelKind::Channel(ch) = &rel.kind else {
            return None;
        };
        let shape = "`#[accept(…)]` takes role names, `external` and `principal in REL`";
        if a.value.is_some() {
            self.error(code!("BLS0210"), a.span, shape);
            return None;
        }
        let src = ch.direction.map(|(src, _)| src);
        let receiver = match (ch.direction, ch.dest_col) {
            (Some((_, dst)), _) => Some(dst),
            (None, Some(d)) => rel
                .cols
                .get(d)
                .and_then(|c| c.ty)
                .and_then(|t| match self.hir.types.get(t) {
                    Some(blossom_value::TypeDef::Node(Some(r))) => Some(HRoleId(r.raw())),
                    _ => None,
                }),
            (None, None) => None,
        };
        if let Some(dst) = receiver
            && self.role_of(dst).kind == RoleKind::External
        {
            self.error(
                code!("BLS0210"),
                a.span,
                format!(
                    "`{}` is sent to an external role: an ACL admits what a node receives, and client sessions receive nothing through one",
                    rel.name
                ),
            );
            return None;
        }
        let mut ok = true;
        let mut roles: Vec<HRoleId> = Vec::new();
        let mut external = false;
        let mut principal: Option<(Ident, Span)> = None;
        for arg in &a.args {
            let ast::Arg::Pos(e) = arg else {
                self.error(code!("BLS0210"), arg.span(), shape);
                ok = false;
                continue;
            };
            if principal.is_some() {
                self.error(
                    code!("BLS0210"),
                    e.span,
                    "`principal in REL` comes last in `#[accept(…)]`",
                );
                ok = false;
                continue;
            }
            if let ast::ExprKind::Binary {
                op: ast::BinOp::In,
                lhs,
                rhs,
            } = &e.kind
                && crate::ast::attrs::word(lhs) == Some("principal")
            {
                match &rhs.kind {
                    ast::ExprKind::Path(p, t) if t.is_empty() && p.len() == 1 => {
                        principal = p.first().map(|n| (*n, e.span));
                    }
                    _ => {
                        self.error(
                            code!("BLS0210"),
                            rhs.span,
                            "`principal in` names a relation of this module by its name",
                        );
                        ok = false;
                    }
                }
                continue;
            }
            let Some(word) = crate::ast::attrs::word(e) else {
                self.error(code!("BLS0210"), e.span, shape);
                ok = false;
                continue;
            };
            if word == "external" {
                if external {
                    self.error(code!("BLS0210"), e.span, "`external` is named twice");
                    ok = false;
                }
                external = true;
                match src {
                    Some(r) if self.role_of(r).kind == RoleKind::External => {}
                    _ => {
                        let why = match src {
                            Some(r) => format!("its source `{}` is not an external role", self.role_of(r).name),
                            None => "it has no source role".to_owned(),
                        };
                        self.error(
                            code!("BLS0404"),
                            e.span,
                            format!(
                                "`external` admits client sessions of the channel's external source role, but `{}` has none: {why}",
                                rel.name
                            ),
                        );
                        ok = false;
                    }
                }
                continue;
            }
            let Some(r) = self.scope(s).roles.get(&Symbol::intern(word)).copied() else {
                self.error(code!("BLS0200"), e.span, format!("unknown role `{word}`"));
                ok = false;
                continue;
            };
            let role = self.role_of(r);
            if role.kind == RoleKind::External {
                self.error(
                    code!("BLS0210"),
                    e.span,
                    format!(
                        "`{}` is an external role, whose clients are sessions, not nodes: admit them with `external`",
                        role.name
                    ),
                );
                ok = false;
                continue;
            }
            if let Some(src) = src
                && src != r
            {
                let src_name = self.role_of(src).name;
                self.error(
                    code!("BLS0404"),
                    e.span,
                    format!(
                        "`{}` never sends on `{}`: its source role is `{src_name}`",
                        role.name, rel.name
                    ),
                );
                ok = false;
                continue;
            }
            if roles.contains(&r) {
                self.error(code!("BLS0210"), e.span, format!("`{}` is named twice", role.name));
                ok = false;
                continue;
            }
            roles.push(r);
        }
        if roles.is_empty() && !external && ok {
            self.error(
                code!("BLS0210"),
                a.span,
                "`#[accept(…)]` names at least one source: a role or `external`",
            );
            return None;
        }
        let principal_in = match principal {
            None => None,
            Some((name, span)) => self.principal_relation(s, name, span, receiver),
        };
        if !ok || principal.is_some() && principal_in.is_none() {
            return None;
        }
        Some(HAcl {
            roles,
            external,
            principal_in,
            span: a.span,
        })
    }

    /// The relation of `principal in REL`: a unary `static` or `table` relation of type `Principal` that the
    /// receiving role (`None`: every node) can read.
    fn principal_relation(
        &mut self,
        s: ScopeIdx,
        name: Ident,
        span: Span,
        receiver: Option<HRoleId>,
    ) -> Option<HRelId> {
        let Some(id) = self.scope(s).rels.get(&name.name).copied() else {
            if !self.scope(s).broken.contains(&name.name) {
                self.error(
                    code!("BLS0200"),
                    name.span,
                    format!("unknown relation `{}`", name.as_str()),
                );
            }
            return None;
        };
        let r = self.rel_of(id);
        if !matches!(r.kind, HRelKind::Static | HRelKind::Table) {
            self.error(
                code!("BLS0210"),
                name.span,
                format!(
                    "`principal in` reads a `static` or `table` relation; `{}` is neither",
                    r.name
                ),
            );
            return None;
        }
        if r.cols.len() != 1 {
            self.error(
                code!("BLS0301"),
                name.span,
                format!(
                    "`principal in` reads a unary relation; `{}` has {} columns",
                    r.name,
                    r.cols.len()
                ),
            );
            return None;
        }
        let principal = r
            .cols
            .first()
            .and_then(|c| c.ty)
            .is_some_and(|t| matches!(self.hir.types.get(t), Some(blossom_value::TypeDef::Principal)));
        if !principal {
            self.error(
                code!("BLS0300"),
                name.span,
                format!(
                    "`principal in` compares principals; the column of `{}` is not a `Principal`",
                    r.name
                ),
            );
            return None;
        }
        if r.role.is_some() && r.role != receiver {
            let at = |this: &mut Self, role: Option<HRoleId>| match role {
                Some(x) => format!("`{}`", this.role_of(x).name),
                None => "every node".to_owned(),
            };
            let (here, there) = (at(self, r.role), at(self, receiver));
            self.error(
                code!("BLS0404"),
                span,
                format!(
                    "`principal in {}` is read where the channel is received ({there}), but `{}` lives at {here}",
                    name.as_str(),
                    r.name
                ),
            );
            return None;
        }
        Some(id)
    }

    /// A timer: `timer name every d;` declares the event relation `name(count: u64, at: Instant)` (LANGUAGE §7.14).
    /// `stream name: listen|connect;` (FOREIGN-PROTOCOLS §1.1): its relations, read and written as `name.rel` like an
    /// instance's interface — the events (`opened`, `data`, `closed`, a connect stream's `failed`) are read, the
    /// requests to the host (`write`, `close`, `pause`, `resume`, a connect stream's `dial`) are sent.
    fn stream(&mut self, s: ScopeIdx, name: Ident, kind: Ident, placement: Option<HRoleId>, span: Span) {
        use blossom_ir::core::{HostOp, StreamEvent, StreamKind};
        let kind = match kind.as_str() {
            "listen" => StreamKind::Listen,
            "connect" => StreamKind::Connect,
            other => {
                self.error(
                    code!("BLS0200"),
                    kind.span,
                    format!("unknown stream kind `{other}`: a stream is `listen` or `connect`"),
                );
                return;
            }
        };
        let taken = self.scope(s).instances.contains_key(&name.name)
            || self.scope(s).rels.contains_key(&name.name)
            || self.scope(s).fns.contains_key(&name.name);
        if taken {
            self.error(
                code!("BLS0201"),
                name.span,
                format!("`{}` is declared twice", name.as_str()),
            );
            return;
        }
        let t = |r: &mut Self, d: TypeDef| r.intern_type(d, span);
        let conn = t(self, TypeDef::Conn);
        let u64t = t(self, TypeDef::Int(blossom_value::types::IntTy::U64));
        let text = t(self, TypeDef::Str);
        let instant = t(self, TypeDef::Instant);
        let bytes = t(self, TypeDef::Bytes);
        let part = self.part_type(span);
        let parts = t(self, TypeDef::Vec(part));
        let mut interface = BTreeMap::new();
        let mut make = |r: &mut Self, rel: &str, what: HStreamRel, cols: &[(&str, TypeId)]| -> HRelId {
            let mut segs = r.scope(s).prefix.clone();
            segs.push(name.name);
            segs.push(Symbol::intern(rel));
            let id = r.add_rel(HRel {
                name: QualName::new(segs),
                kind: HRelKind::Stream(what),
                cols: cols
                    .iter()
                    .map(|(n, ty)| HCol {
                        name: Symbol::intern(n),
                        ty: Some(*ty),
                    })
                    .collect(),
                key: None,
                durable: false,
                cell: false,
                resolve: None,
                prefer: None,
                role: placement,
                span,
            });
            r.rel_spans.insert(id, span);
            // An instance interface's flag is `true` for what the importer writes.
            interface.insert(Symbol::intern(rel), (id, matches!(what, HStreamRel::Host(_))));
            id
        };
        let opened_cols: Vec<(&str, TypeId)> = match kind {
            StreamKind::Listen => vec![("c", conn), ("peer", text), ("at", instant)],
            StreamKind::Connect => vec![("c", conn), ("req", u64t), ("peer", text), ("at", instant)],
        };
        let opened = make(self, "opened", HStreamRel::Event(StreamEvent::Opened), &opened_cols);
        let data = make(
            self,
            "data",
            HStreamRel::Event(StreamEvent::Data),
            &[("c", conn), ("seq", u64t), ("bytes", bytes)],
        );
        let closed = make(
            self,
            "closed",
            HStreamRel::Event(StreamEvent::Closed),
            &[("c", conn), ("reason", text)],
        );
        let write = make(
            self,
            "write",
            HStreamRel::Host(HostOp::Write),
            &[("c", conn), ("seq", u64t), ("parts", parts)],
        );
        let close = make(self, "close", HStreamRel::Host(HostOp::Close), &[("c", conn)]);
        let pause = make(self, "pause", HStreamRel::Host(HostOp::Pause), &[("c", conn)]);
        let resume = make(self, "resume", HStreamRel::Host(HostOp::Resume), &[("c", conn)]);
        let (failed, dial) = match kind {
            StreamKind::Listen => (None, None),
            StreamKind::Connect => (
                Some(make(
                    self,
                    "failed",
                    HStreamRel::Event(StreamEvent::Failed),
                    &[("req", u64t), ("reason", text)],
                )),
                Some(make(
                    self,
                    "dial",
                    HStreamRel::Host(HostOp::Dial),
                    &[("req", u64t), ("addr", text)],
                )),
            ),
        };
        self.scope_mut(s).instances.insert(name.name, Instance { interface });
        self.hir.streams.push(HStream {
            name: self.qual(s, name.name),
            kind,
            role: placement,
            opened,
            data,
            closed,
            failed,
            write,
            close,
            pause,
            resume,
            dial,
            span,
        });
    }

    fn timer(&mut self, s: ScopeIdx, t: &'t ast::TimerDecl, placement: Option<HRoleId>) -> Option<HRelId> {
        let words: Vec<&str> = t.words.iter().map(Ident::as_str).collect();
        // The parser accepts `every E [ticks] [times E]` and `once [after E]` only.
        let (schedule, used) = match words.as_slice() {
            ["every"] => (
                TimerSchedule::Every {
                    period: self.timer_period(s, t, 0)?,
                    times: None,
                },
                1,
            ),
            ["every", "times"] => (
                TimerSchedule::Every {
                    period: self.timer_period(s, t, 0)?,
                    times: Some(self.timer_count(s, t, 1, "a timer's `times`")?),
                },
                2,
            ),
            ["every", "ticks"] => (
                TimerSchedule::Ticks {
                    every: self.timer_count(s, t, 0, "a logical timer's number of ticks")?,
                    times: None,
                },
                1,
            ),
            ["every", "ticks", "times"] => (
                TimerSchedule::Ticks {
                    every: self.timer_count(s, t, 0, "a logical timer's number of ticks")?,
                    times: Some(self.timer_count(s, t, 1, "a timer's `times`")?),
                },
                2,
            ),
            ["once"] => (TimerSchedule::Once, 0),
            ["once", "after"] => (TimerSchedule::OnceAfter(self.timer_period(s, t, 0)?), 1),
            _ => {
                self.bugs.push(blossom_base::internal_error!(
                    "a timer declaration of the words {words:?} parsed"
                ));
                return None;
            }
        };
        if t.exprs.len() != used {
            self.bugs.push(blossom_base::internal_error!(
                "a timer declaration with {} expressions parsed",
                t.exprs.len()
            ));
            return None;
        }
        if schedule == TimerSchedule::Once && t.guard.is_some() {
            self.error(
                code!("BLS0412"),
                t.span,
                "a `once` timer fires in the boot tick, before any guard can hold: it takes no `while`",
            );
            return None;
        }
        let u64_ty = self.intern_type(TypeDef::Int(blossom_value::types::IntTy::U64), t.span);
        let instant = self.intern_type(TypeDef::Instant, t.span);
        Some(self.add_rel(HRel {
            name: self.qual(s, t.name.name),
            // A `while` guard is resolved once every relation of the scope is known (`timer_guards`).
            kind: HRelKind::Timer { schedule, guard: None },
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
            cell: false,
            resolve: None,
            prefer: None,
            role: placement,
            span: t.name.span,
        }))
    }

    /// The positive `Duration` that expression `i` of a timer declaration is (its period, or `once after`'s delay), in
    /// nanoseconds.
    fn timer_period(&mut self, s: ScopeIdx, t: &'t ast::TimerDecl, i: usize) -> Option<u128> {
        let e = t.exprs.get(i)?;
        match self.const_value(s, e, None) {
            Some((Value::Duration(d), _)) if d.as_nanos() > 0 => Some(d.as_nanos() as u128),
            Some((Value::Duration(_), _)) => {
                self.error(code!("BLS0300"), e.span, "a timer period must be positive");
                None
            }
            Some(_) => {
                self.error(code!("BLS0300"), e.span, "a timer period must be a Duration");
                None
            }
            None => None,
        }
    }

    /// The positive integer that expression `i` of a timer declaration is (`what`: a logical timer's ticks, or
    /// `times`).
    fn timer_count(&mut self, s: ScopeIdx, t: &'t ast::TimerDecl, i: usize, what: &str) -> Option<u64> {
        let e = t.exprs.get(i)?;
        let u64_ty = self.intern_type(TypeDef::Int(blossom_value::types::IntTy::U64), e.span);
        match self.const_value(s, e, Some(u64_ty)) {
            Some((Value::Int(blossom_value::value::IntValue::U64(n)), _)) if n > 0 => Some(n),
            Some((Value::Int(_), _)) => {
                self.error(code!("BLS0300"), e.span, format!("{what} must be positive"));
                None
            }
            Some(_) => {
                self.error(code!("BLS0300"), e.span, format!("{what} must be an integer"));
                None
            }
            None => None,
        }
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
        let is_lattice = |r: &Self, c: &HCol| c.ty.is_some_and(|t| r.hir.lattice_of(t).is_some());
        if d.mods.cell && !cols.first().is_some_and(|c| is_lattice(self, c)) {
            self.error(
                code!("BLS0300"),
                d.span,
                "a cell holds a lattice value: declare it `cell name: L`",
            );
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
                        Some(i) if cols.get(i).is_some_and(|c| is_lattice(self, c)) => self.error(
                            code!("BLS0304"),
                            n.span,
                            format!("`{}` is a lattice column and cannot be a key", n.as_str()),
                        ),
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
        let (resolve, prefer) = match &d.resolve {
            None => (None, None),
            Some((ast::RelPolicy::Prefer(rules), span)) => {
                (None, self.prefer_policy(&cols, key.as_deref(), rules, *span))
            }
            Some((policy, span)) => (self.resolve_policy(d, &cols, key.as_deref(), policy, *span), None),
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
                    acl: None,
                })
            }
        };
        Some(self.add_rel(HRel {
            name: self.qual(s, d.name.name),
            kind,
            cols,
            key,
            durable: d.mods.durable,
            cell: d.mods.cell,
            resolve,
            prefer,
            role: placement,
            span: d.name.span,
        }))
    }

    /// `resolve prefer(rule, …)` (LANGUAGE §10.7): on a keyed table of plain values, each handler named once.
    fn prefer_policy(
        &mut self,
        cols: &[HCol],
        key: Option<&[usize]>,
        rules: &[Ident],
        span: Span,
    ) -> Option<Vec<(Symbol, Span)>> {
        if key.is_none() {
            self.error(
                code!("BLS0106"),
                span,
                "`resolve prefer` arbitrates writes to one key: declare `key(…)`",
            );
            return None;
        }
        if cols
            .iter()
            .any(|c| c.ty.is_some_and(|t| holds_lattice(&self.hir.types, t)))
        {
            self.unsupported("LANG-117", "`resolve prefer` on a table holding lattice values", span);
            return None;
        }
        if rules.is_empty() {
            self.error(code!("BLS0411"), span, "`resolve prefer` names no handler");
            return None;
        }
        let mut seen = BTreeSet::new();
        for r in rules {
            if !seen.insert(r.name) {
                self.error(
                    code!("BLS0411"),
                    r.span,
                    format!("`{}` is named twice in `resolve prefer`", r.as_str()),
                );
                return None;
            }
        }
        Some(rules.iter().map(|r| (r.name, r.span)).collect())
    }

    /// After a module's rules: each handler a `resolve prefer` names writes the table with `next` or `upsert`.
    fn check_prefer(&mut self, s: ScopeIdx) {
        let rels: Vec<HRelId> = self.scope(s).rels.values().copied().collect();
        for id in rels {
            let r = self.rel_of(id);
            for (name, span) in r.prefer.iter().flatten() {
                let labelled = self.handler_labels.get(&(s, *name)).copied().unwrap_or(0);
                if labelled > 1 {
                    self.error(
                        code!("BLS0411"),
                        *span,
                        format!(
                            "`resolve prefer` names `{}`, which labels {labelled} handlers",
                            name.as_str()
                        ),
                    );
                } else if !self.prefer_writers.contains(&(id, *name)) {
                    self.error(
                        code!("BLS0411"),
                        *span,
                        format!(
                            "`resolve prefer` names `{}`, which is not a handler writing `{}` with `next` or `upsert`",
                            name.as_str(),
                            r.name
                        ),
                    );
                }
            }
        }
    }

    /// A relation-level `resolve P` (LANGUAGE §10.7): only on a keyed table.
    fn resolve_policy(
        &mut self,
        d: &ast::RelDecl,
        cols: &[HCol],
        key: Option<&[usize]>,
        policy: &ast::RelPolicy,
        span: Span,
    ) -> Option<HResolve> {
        let Some(key) = key else {
            self.error(
                code!("BLS0106"),
                span,
                "`resolve` chooses among tuples with one key: declare `key(…)`",
            );
            return None;
        };
        let is_lattice = |r: &Self, c: &HCol| c.ty.is_some_and(|t| r.hir.lattice_of(t).is_some());
        let policy = match policy {
            ast::RelPolicy::Prefer(_) => {
                self.bugs.push(blossom_base::internal_error!(
                    "`resolve prefer` reached the resolution policies"
                ));
                return None;
            }
            ast::RelPolicy::Choose { sticky: false } => HPolicy::Choose,
            ast::RelPolicy::Choose { sticky: true } => {
                self.unsupported("LANG-115", "`resolve choose sticky`", span);
                return None;
            }
            ast::RelPolicy::ChooseRand { .. } => {
                self.unsupported("LANG-117", "`resolve choose_rand`", span);
                return None;
            }
            ast::RelPolicy::Merge => {
                let all_lattice = cols
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !key.contains(i))
                    .all(|(_, c)| is_lattice(self, c));
                if !all_lattice {
                    self.error(
                        code!("BLS0106"),
                        span,
                        "`resolve merge` needs every non-key column to be a lattice",
                    );
                }
                // Merging is what a lattice-valued relation does anyway.
                return None;
            }
            ast::RelPolicy::Least(e) | ast::RelPolicy::Most(e) => {
                let most = matches!(policy, ast::RelPolicy::Most(_));
                let col = match &e.kind {
                    ast::ExprKind::Path(p, t) if t.is_empty() && p.len() == 1 => p
                        .first()
                        .and_then(|n| d.cols.iter().position(|c| c.name.name == n.name)),
                    _ => None,
                };
                let Some(col) = col else {
                    self.unsupported("LANG-117", "a resolution cost other than one column", e.span);
                    return None;
                };
                HPolicy::Extreme { col, most }
            }
        };
        if cols.iter().any(|c| is_lattice(self, c)) {
            self.unsupported("LANG-117", "`resolve` on a lattice-valued relation", span);
            return None;
        }
        Some(HResolve { policy, span })
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
                    self.handler(s, h, placement);
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
                // Declared by `trees`.
                ItemKind::Tree(_) | ItemKind::Fragment(_) => {}
                ItemKind::Format(f) => self.bugs.push(blossom_base::internal_error!(
                    "format `{}` reached name resolution (formats are expanded when files load)",
                    f.name.as_str()
                )),
                ItemKind::Rel(d) if d.guard.is_some() => self.persist_guard(s, d, placement),
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
                | ItemKind::Fn(_)
                | ItemKind::ExternFn(_)
                | ItemKind::Stream { .. }
                | ItemKind::Lattice(_)
                | ItemKind::Impl(_)
                | ItemKind::Unsupported { .. } => {}
                ItemKind::Param { .. } => {}
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
            broken: Default::default(),
            fns: BTreeMap::new(),
            generic_fns: BTreeMap::new(),
            trees: BTreeMap::new(),
            fragments: BTreeMap::new(),
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
            broken: Default::default(),
            fns: BTreeMap::new(),
            generic_fns: BTreeMap::new(),
            trees: BTreeMap::new(),
            fragments: BTreeMap::new(),
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
                    "recovered" => Some(self.builtin(BuiltinRel::Recovered, name.span)),
                    "localtick" => Some(self.builtin(BuiltinRel::LocalTick, name.span)),
                    "halt" => Some(self.builtin(BuiltinRel::Halt, name.span)),
                    "node_dir" => Some(self.builtin(BuiltinRel::NodeDir, name.span)),
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

    /// Folds the `const` and `param` items of the scope's module and file, in dependency order: a constant may use
    /// a parameter and a parameter's default a constant (LANGUAGE §6.4). A deploy-time parameter is a constant of
    /// the deployment, its binding or else its default (LANG-010; the program is compiled per deployment). The
    /// deployment binds the root program's parameters by name and a module's by its qualified name
    /// (`module.NAME`), so a module's parameter never takes a binding meant for another's.
    fn fold_consts(&mut self, s: ScopeIdx, items: &'t [ast::Item]) {
        let decls: Vec<&'t ast::Item> = items
            .iter()
            .filter(|i| matches!(i.kind, ItemKind::Const { .. } | ItemKind::Param { .. }))
            .collect();
        let name_of = |i: &ast::Item| match &i.kind {
            ItemKind::Const { name, .. } | ItemKind::Param { name, .. } => Some(*name),
            _ => None,
        };
        let index: BTreeMap<Symbol, usize> = decls
            .iter()
            .enumerate()
            .filter_map(|(k, i)| name_of(i).map(|n| (n.name, k)))
            .collect();
        let deps: Vec<BTreeSet<usize>> = decls
            .iter()
            .map(|i| {
                let mut names = BTreeSet::new();
                match &i.kind {
                    ItemKind::Const { value, .. } => const_deps(value, &mut names),
                    ItemKind::Param { default: Some(e), .. } => const_deps(e, &mut names),
                    _ => {}
                }
                names.iter().filter_map(|n| index.get(n).copied()).collect()
            })
            .collect();
        let mut done = vec![false; decls.len()];
        let mut order = Vec::new();
        loop {
            let ready: Vec<usize> = (0..decls.len())
                .filter(|&k| !done.get(k).copied().unwrap_or(true))
                .filter(|&k| {
                    deps.get(k)
                        .is_some_and(|d| d.iter().all(|&j| done.get(j).copied().unwrap_or(false)))
                })
                .collect();
            if ready.is_empty() {
                break;
            }
            for k in ready {
                if let Some(d) = done.get_mut(k) {
                    *d = true;
                }
                order.push(k);
            }
        }
        for (k, item) in decls.iter().enumerate() {
            if !done.get(k).copied().unwrap_or(true)
                && let Some(name) = name_of(item)
            {
                self.error(
                    code!("BLS0200"),
                    name.span,
                    format!("`{}` is defined in terms of itself", name.as_str()),
                );
            }
        }
        for k in order {
            let Some(item) = decls.get(k) else { continue };
            match &item.kind {
                ItemKind::Const { name, ty, value } => {
                    let Some(t) = self.resolve_type(s, ty) else { continue };
                    if let Some(v) = self.const_value(s, value, Some(t)) {
                        self.define_value(s, name, v);
                    }
                }
                ItemKind::Param { name, ty, default } => {
                    let prefix = self.module_path(s);
                    let key = if prefix.segments().is_empty() {
                        name.as_str().to_owned()
                    } else {
                        format!("{prefix}.{}", name.as_str())
                    };
                    self.params_declared.insert(key.clone());
                    let Some(t) = self.resolve_type(s, ty) else { continue };
                    let value = match self.param_bindings.get(&key).cloned() {
                        Some(b) => self.bound_param(name, t, &b),
                        None => match default {
                            Some(e) => self.const_value(s, e, Some(t)),
                            None => {
                                self.error(
                                    code!("BLS0205"),
                                    name.span,
                                    format!("the parameter `{key}` has no default and the deployment does not bind it"),
                                );
                                None
                            }
                        },
                    };
                    if let Some(v) = value {
                        self.define_value(s, name, v);
                    }
                }
                _ => {}
            }
        }
    }

    fn define_value(&mut self, s: ScopeIdx, name: &Ident, v: (Value, TypeId)) {
        if self.scope(s).values.contains_key(&name.name) {
            self.error(
                code!("BLS0201"),
                name.span,
                format!("`{}` is defined twice", name.as_str()),
            );
        }
        self.scope_mut(s).values.insert(name.name, v);
    }

    /// A deployment's binding of parameter `name` of type `t`.
    fn bound_param(&mut self, name: &Ident, t: TypeId, b: &crate::api::ParamBinding) -> Option<(Value, TypeId)> {
        use crate::api::ParamBinding as B;
        let def = self.hir.types.get(t).cloned();
        let v = match (&def, b) {
            (Some(TypeDef::Int(ity)), B::Int(n)) => blossom_value::value::IntValue::from_i128(*ity, *n).map(Value::Int),
            (Some(TypeDef::Bool), B::Bool(x)) => Some(Value::Bool(*x)),
            (Some(TypeDef::Str), B::Text(x)) => Some(Value::Str(x.as_str().into())),
            (Some(TypeDef::Duration), B::Text(x)) => crate::api::parse_duration(x).map(Value::Duration),
            _ => None,
        };
        match v {
            Some(v) => Some((v, t)),
            None => {
                self.error(
                    code!("BLS0205"),
                    name.span,
                    format!(
                        "the deployment's value {b:?} for `{}` is not a {} (bindings are integers, bools, strings and \
                         durations such as \"150ms\")",
                        name.as_str(),
                        def.map_or("known type".to_string(), |d| format!("{d:?}"))
                    ),
                );
                None
            }
        }
    }
}

/// The names a constant expression refers to (the forms `const_value` folds).
fn const_deps(e: &ast::Expr, out: &mut BTreeSet<Symbol>) {
    match &e.kind {
        ast::ExprKind::Path(path, targs) if targs.is_empty() => {
            if let [name] = path.as_slice() {
                out.insert(name.name);
            }
        }
        ast::ExprKind::Binary { lhs, rhs, .. } => {
            const_deps(lhs, out);
            const_deps(rhs, out);
        }
        ast::ExprKind::Prefix { arg, .. } => const_deps(arg, out),
        _ => {}
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

/// Whether a value of type `t` can contain a `Conn`.
fn holds_conn(types: &blossom_value::TypeTable, t: TypeId, seen: &mut BTreeSet<TypeId>) -> bool {
    holds(types, t, &|d| matches!(d, TypeDef::Conn), seen)
}

/// Whether a value of type `t` can contain a value of a type `leaf` accepts.
fn holds(
    types: &blossom_value::TypeTable,
    t: TypeId,
    leaf: &dyn Fn(&TypeDef) -> bool,
    seen: &mut BTreeSet<TypeId>,
) -> bool {
    if !seen.insert(t) {
        return false;
    }
    match types.get(t) {
        Some(d) if leaf(d) => true,
        Some(TypeDef::Tuple(ts)) => ts.iter().any(|x| holds(types, *x, leaf, seen)),
        Some(TypeDef::Option(x) | TypeDef::Vec(x) | TypeDef::Set(x)) => holds(types, *x, leaf, seen),
        Some(TypeDef::Map(k, v)) => holds(types, *k, leaf, seen) || holds(types, *v, leaf, seen),
        Some(TypeDef::Struct(d)) => d.fields.iter().any(|f| holds(types, f.ty, leaf, seen)),
        Some(TypeDef::Enum(d)) => d
            .variants
            .iter()
            .any(|v| v.payload.iter().any(|f| holds(types, f.ty, leaf, seen))),
        _ => false,
    }
}
