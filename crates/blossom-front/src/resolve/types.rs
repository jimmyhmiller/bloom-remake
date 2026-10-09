//! Types, constants and module lookup (LANGUAGE §5, §6.4).
//!
//! Lookups go from the scope outwards: the instance's bound type and value parameters, the module body's own items,
//! the items of the file the module is defined in, and the names that file brings in with `use`.

use blossom_base::TypeId;
use blossom_base::{QualName, Span, Symbol, code};
use blossom_value::time::Duration;
use blossom_value::types::{EnumDef, FieldDef, IntTy, StructDef, VariantDef};
use blossom_value::value::IntValue;
use blossom_value::{TypeDef, Value};

use super::{FileKey, Resolver, ScopeIdx};
use crate::ast::{self, BinOp, ExprKind, Ident, ItemKind, LitValue, PrefixOp};
use crate::hir::HRoleId;

/// Where a definition is written: its file, and the module body it is an item of (`None`: the file's top level).
/// The names a definition's own types and constants use are looked up from there.
#[derive(Clone)]
pub(crate) struct Home<'t> {
    pub file: FileKey,
    pub body: Option<&'t [ast::Item]>,
}

impl Home<'_> {
    /// Identifies the module body (0 for a file's top level): bodies are borrowed from the module tree, which outlives
    /// resolution, so a body's address names it.
    fn body_id(&self) -> usize {
        self.body.map_or(0, |b| b.as_ptr() as usize)
    }

    /// Whether `items` (a scope's module body) is this home's body.
    fn is_body(&self, items: Option<&[ast::Item]>) -> bool {
        match (self.body, items) {
            (None, None) => true,
            (Some(a), Some(b)) => std::ptr::eq(a, b),
            _ => false,
        }
    }
}

/// A named definition found by a lookup.
pub(crate) enum Def<'t> {
    Alias(&'t ast::Type, Home<'t>),
    Struct(&'t ast::StructItem, Home<'t>),
    Enum(&'t ast::EnumItem, Home<'t>),
    Module(&'t ast::ModuleItem, Home<'t>),
    Protocol(&'t ast::ProtocolItem, Home<'t>),
    Const(&'t ast::Type, &'t ast::Expr, Home<'t>),
    Lattice(&'t ast::LatticeItem, Home<'t>),
}

impl<'t> Resolver<'t, '_> {
    /// Finds a definition named `name` among `items`, whose home is `home` (not descending into modules; `at`
    /// sections hold no types).
    fn find_in(items: &'t [ast::Item], name: Symbol, home: &Home<'t>) -> Option<Def<'t>> {
        for item in items {
            let found = match &item.kind {
                ItemKind::TypeAlias { name: n, ty, generics } if n.name == name && generics.is_empty() => {
                    Some(Def::Alias(ty, home.clone()))
                }
                ItemKind::Struct(s) if s.name.name == name => Some(Def::Struct(s, home.clone())),
                ItemKind::Enum(e) if e.name.name == name => Some(Def::Enum(e, home.clone())),
                ItemKind::Module(m) if m.name.name == name => Some(Def::Module(m, home.clone())),
                ItemKind::Protocol(p) if p.name.name == name => Some(Def::Protocol(p, home.clone())),
                ItemKind::Const { name: n, ty, value } if n.name == name => Some(Def::Const(ty, value, home.clone())),
                ItemKind::Lattice(l) if l.name.name == name => Some(Def::Lattice(l, home.clone())),
                _ => None,
            };
            if found.is_some() {
                return found;
            }
        }
        None
    }

    /// Looks `name` up in the file `file` and through its `use` items.
    fn find_in_file(&self, file: &FileKey, name: Symbol, depth: u32) -> Option<Def<'t>> {
        let items = self.file_items(file);
        let home = Home {
            file: file.clone(),
            body: None,
        };
        if let Some(d) = Self::find_in(items, name, &home) {
            return Some(d);
        }
        if depth > 8 {
            return None;
        }
        for item in items {
            let ItemKind::Use(tree) = &item.kind else { continue };
            for path in &tree.paths {
                let Some(last) = path.last() else { continue };
                if last.name != name {
                    continue;
                }
                let target = match path.as_slice() {
                    [module, _] => FileKey::Module(module.as_str().to_owned()),
                    _ => continue,
                };
                if let Some(d) = self.find_in_file(&target, name, depth + 1) {
                    return Some(d);
                }
            }
        }
        None
    }

    /// Looks up a definition by name from scope `s`.
    pub fn find_def(&self, s: ScopeIdx, name: Symbol) -> Option<Def<'t>> {
        let scope = self.scope(s);
        if let Some(items) = scope.own_items {
            let home = Home {
                file: scope.file.clone(),
                body: Some(items),
            };
            if let Some(d) = Self::find_in(items, name, &home) {
                return Some(d);
            }
        }
        self.find_in_file(&scope.file, name, 0)
    }

    pub fn find_module(&self, s: ScopeIdx, path: &[Ident]) -> Option<(FileKey, &'t ast::ModuleItem)> {
        let def = match path {
            [name] => self.find_def(s, name.name),
            [module, name] => self.find_in_file(&FileKey::Module(module.as_str().to_owned()), name.name, 0),
            _ => None,
        };
        match def {
            Some(Def::Module(m, home)) => Some((home.file, m)),
            _ => None,
        }
    }

    pub fn find_protocol(&self, s: ScopeIdx, path: &[Ident]) -> Option<(FileKey, &'t ast::ProtocolItem)> {
        let def = match path {
            [name] => self.find_def(s, name.name),
            [module, name] => self.find_in_file(&FileKey::Module(module.as_str().to_owned()), name.name, 0),
            _ => None,
        };
        match def {
            Some(Def::Protocol(p, home)) => Some((home.file, p)),
            _ => None,
        }
    }

    /// A scope over a definition's home (no instance parameters): for resolving the types and constants a type or
    /// constant definition uses, from where it is written.
    fn home_scope(&mut self, home: &Home<'t>) -> ScopeIdx {
        if let Some(i) = self
            .scopes
            .iter()
            .position(|sc| sc.file == home.file && sc.prefix.is_empty() && home.is_body(sc.own_items))
        {
            return ScopeIdx(i);
        }
        self.new_scope_for_home(home)
    }

    /// The scope to resolve a definition found from scope `s` in: `s` itself when the definition is written in `s`'s
    /// body, else its home's.
    fn def_scope(&mut self, s: ScopeIdx, home: &Home<'t>) -> ScopeIdx {
        let scope = self.scope(s);
        if scope.file == home.file && home.is_body(scope.own_items) {
            s
        } else {
            self.home_scope(home)
        }
    }

    fn new_scope_for_home(&mut self, home: &Home<'t>) -> ScopeIdx {
        self.scopes.push(super::ModScope {
            file: home.file.clone(),
            prefix: Vec::new(),
            generics: Default::default(),
            values: Default::default(),
            rels: Default::default(),
            instances: Default::default(),
            roles: Default::default(),
            role_template: None,
            has_roles: false,
            write_redirect: Default::default(),
            own_items: home.body,
            broken: Default::default(),
            role_rels: Default::default(),
            fns: Default::default(),
            outer_fns: Default::default(),
            generic_fns: Default::default(),
            trees: Default::default(),
            fragments: Default::default(),
        });
        ScopeIdx(self.scopes.len() - 1)
    }

    /// Resolves a written type.
    pub fn resolve_type(&mut self, s: ScopeIdx, ty: &ast::Type) -> Option<TypeId> {
        match ty {
            ast::Type::Tuple { elems, span } => {
                if elems.is_empty() {
                    return Some(self.intern_type(TypeDef::Unit, *span));
                }
                if elems.len() == 1 {
                    return elems.first().and_then(|e| self.resolve_type(s, e));
                }
                let mut ids = Vec::new();
                for e in elems {
                    ids.push(self.resolve_type(s, e)?);
                }
                Some(self.intern_type(TypeDef::Tuple(ids), *span))
            }
            ast::Type::Named { path, args, span } => self.named_type(s, path, args, *span),
            ast::Type::Unsafe { inner, span } => match &**inner {
                ast::Type::Named { path, .. }
                    if path.len() == 1 && path.first().is_some_and(|n| n.as_str() == "DomPair") =>
                {
                    self.unsupported("LANG-136", "`unsafe DomPair<K, V>`", *span);
                    None
                }
                _ => {
                    self.error(code!("BLS0300"), *span, "`unsafe` applies only to `DomPair<K, V>`");
                    None
                }
            },
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

    fn named_type(&mut self, s: ScopeIdx, path: &[Ident], args: &[ast::Type], span: Span) -> Option<TypeId> {
        let [name] = path else {
            self.unsupported("LANG-001", "qualified type paths", span);
            return None;
        };
        let text = name.as_str();
        let arity = |r: &mut Self, n: usize| -> bool {
            if args.len() != n {
                r.error(
                    code!("BLS0301"),
                    span,
                    format!("`{text}` takes {n} type argument(s), {} given", args.len()),
                );
                false
            } else {
                true
            }
        };
        let scalar = match text {
            "bool" => Some(TypeDef::Bool),
            "f64" => Some(TypeDef::F64),
            "String" => Some(TypeDef::Str),
            "Bytes" => Some(TypeDef::Bytes),
            "Blob" => Some(TypeDef::Blob),
            "Duration" => Some(TypeDef::Duration),
            "Instant" => Some(TypeDef::Instant),
            "Session" => Some(TypeDef::Session),
            "Conn" => Some(TypeDef::Conn),
            "Principal" => Some(TypeDef::Principal),
            _ => IntTy::ALL.iter().find(|t| t.name() == text).map(|t| TypeDef::Int(*t)),
        };
        if let Some(def) = scalar {
            if !arity(self, 0) {
                return None;
            }
            return Some(self.intern_type(def, span));
        }
        match text {
            "Part" if self.find_def(s, name.name).is_none() => {
                if !arity(self, 0) {
                    return None;
                }
                return Some(self.part_type(span));
            }
            "Node" => {
                if args.is_empty() {
                    return Some(self.node_type(None));
                }
                if !arity(self, 1) {
                    return None;
                }
                let role = match args.first() {
                    Some(ast::Type::Named { path, args: a, .. }) if a.is_empty() && path.len() == 1 => {
                        path.first().and_then(|p| self.scope(s).roles.get(&p.name).copied())
                    }
                    _ => None,
                };
                let Some(role) = role else {
                    self.error(code!("BLS0200"), span, "`Node<R>` needs a role in scope");
                    return None;
                };
                return Some(self.node_type(Some(role)));
            }
            "Option" | "Vec" | "Set" => {
                if !arity(self, 1) {
                    return None;
                }
                let inner = self.resolve_type(s, args.first()?)?;
                let def = match text {
                    "Option" => TypeDef::Option(inner),
                    "Vec" => TypeDef::Vec(inner),
                    _ => TypeDef::Set(inner),
                };
                return Some(self.intern_type(def, span));
            }
            "Map" => {
                if !arity(self, 2) {
                    return None;
                }
                let k = self.resolve_type(s, args.first()?)?;
                let v = self.resolve_type(s, args.get(1)?)?;
                return Some(self.intern_type(TypeDef::Map(k, v), span));
            }
            _ => {}
        }
        if let Some(t) = self.scope(s).generics.get(&name.name).copied() {
            if !arity(self, 0) {
                return None;
            }
            return Some(t);
        }
        if let Some(role) = self.scope(s).roles.get(&name.name).copied() {
            if !arity(self, 0) {
                return None;
            }
            return Some(self.node_type(Some(role)));
        }
        if is_lattice_name(text) {
            return self.lattice_type(s, text, args, span);
        }
        match text {
            // Prelude aliases (LANGUAGE §11.5).
            "VClock" if self.find_def(s, name.name).is_none() => {
                if !arity(self, 0) {
                    return None;
                }
                let node = self.node_type(None);
                let u64t = self.intern_type(TypeDef::Int(IntTy::U64), span);
                let max = self.intern_lattice_or_bug(blossom_ir::core::LatticeCtor::Max(u64t))?;
                let (inner, _) = self.hir.lattice_of(max)?;
                return self.intern_lattice_or_bug(blossom_ir::core::LatticeCtor::Map(node, inner));
            }
            "Ballot" | "Lww" if self.find_def(s, name.name).is_none() => {
                self.unsupported("LANG-131", &format!("the prelude lattice `{text}` (a `Lex`)"), span);
                return None;
            }
            "DomPair" if self.find_def(s, name.name).is_none() => {
                self.error(
                    code!("BLS0706"),
                    span,
                    "`DomPair` is not associative (CR-25): it exists only as `unsafe DomPair<K, V>`",
                );
                return None;
            }
            _ => {}
        }
        match self.find_def(s, name.name) {
            Some(Def::Alias(t, home)) => {
                if !arity(self, 0) {
                    return None;
                }
                let fs = self.def_scope(s, &home);
                self.resolve_type(fs, t)
            }
            Some(Def::Struct(st, home)) => {
                if !arity(self, 0) || !st.generics.is_empty() {
                    if !st.generics.is_empty() {
                        self.unsupported("LANG-023", "generic structs", span);
                    }
                    return None;
                }
                self.struct_type(st, home)
            }
            Some(Def::Enum(en, home)) => {
                if !arity(self, 0) || !en.generics.is_empty() {
                    if !en.generics.is_empty() {
                        self.unsupported("LANG-023", "generic enums", span);
                    }
                    return None;
                }
                self.enum_type(en, home)
            }
            Some(Def::Lattice(l, home)) => {
                if !arity(self, 0) || !l.generics.is_empty() {
                    if !l.generics.is_empty() {
                        self.unsupported("LANG-023", "generic lattices", span);
                    }
                    return None;
                }
                self.user_lattice(l, home, span)
            }
            _ => {
                self.error(code!("BLS0200"), name.span, format!("unknown type `{text}`"));
                None
            }
        }
    }

    fn struct_type(&mut self, st: &'t ast::StructItem, home: Home<'t>) -> Option<TypeId> {
        let key = (home.file.clone(), home.body_id(), st.name.name);
        if let Some(t) = self.nominal.get(&key) {
            return Some(*t);
        }
        let fs = self.home_scope(&home);
        let mut fields = Vec::new();
        for (i, f) in st.fields.iter().enumerate() {
            let ty = self.resolve_type(fs, &f.ty)?;
            let name = f.name.map_or_else(|| Symbol::intern(&i.to_string()), |n| n.name);
            fields.push(field(name, ty));
        }
        let t = self.intern_type(
            TypeDef::Struct(StructDef {
                name: QualName::single(st.name.name),
                fields,
                reserved: Vec::new(),
            }),
            st.name.span,
        );
        self.nominal.insert(key, t);
        Some(t)
    }

    /// A user-defined lattice (LANGUAGE §11.8): an alias names another lattice type; a product is nominal, its fields
    /// lattices.
    fn user_lattice(&mut self, l: &'t ast::LatticeItem, home: Home<'t>, use_span: Span) -> Option<TypeId> {
        let key = (home.file.clone(), home.body_id(), l.name.name);
        if let Some(t) = self.nominal.get(&key) {
            return Some(*t);
        }
        if !self.lattices_resolving.insert(key.clone()) {
            self.error(
                code!("BLS0300"),
                use_span,
                format!("the lattice `{}` contains itself", l.name.as_str()),
            );
            return None;
        }
        let fs = self.home_scope(&home);
        let t = match &l.body {
            ast::LatticeBody::Alias(target) => {
                let t = self.resolve_type(fs, target);
                match t {
                    Some(t) if self.hir.lattice_of(t).is_some() => Some(t),
                    Some(_) => {
                        self.error(
                            code!("BLS0300"),
                            target.span(),
                            format!(
                                "`lattice {} = …` names a lattice type; for a plain type write `type`",
                                l.name.as_str()
                            ),
                        );
                        None
                    }
                    None => None,
                }
            }
            ast::LatticeBody::Product(fields) => self.product_type(fs, l, fields),
        };
        self.lattices_resolving.remove(&key);
        let t = t?;
        self.nominal.insert(key, t);
        Some(t)
    }

    fn product_type(&mut self, fs: ScopeIdx, l: &'t ast::LatticeItem, fields: &'t [ast::FieldDecl]) -> Option<TypeId> {
        let mut out = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut ok = true;
        for f in fields {
            let Some(name) = f.name else {
                ok = false;
                continue;
            };
            if !seen.insert(name.name) {
                self.error(
                    code!("BLS0201"),
                    name.span,
                    format!("field `{}` is declared twice", name.as_str()),
                );
                ok = false;
                continue;
            }
            if let Some(a) = f.attrs.first() {
                self.error(
                    code!("BLS0210"),
                    a.span,
                    "a field of a product lattice takes no attributes",
                );
                ok = false;
            }
            match self.resolve_type(fs, &f.ty) {
                Some(t) => match self.hir.lattice_of(t) {
                    Some((id, _)) => out.push((name.name, id)),
                    None => {
                        self.error(
                            code!("BLS0300"),
                            f.ty.span(),
                            format!(
                                "a field of the product lattice `{}` is a lattice: `{}` is not one",
                                l.name.as_str(),
                                crate::typeck::type_name(&self.hir.types, t)
                            ),
                        );
                        ok = false;
                    }
                },
                None => ok = false,
            }
        }
        if out.is_empty() && ok {
            self.error(
                code!("BLS0300"),
                l.name.span,
                format!("the product lattice `{}` has no fields", l.name.as_str()),
            );
            return None;
        }
        if !ok {
            return None;
        }
        self.intern_lattice_or_bug(blossom_ir::core::LatticeCtor::Product {
            name: QualName::single(l.name.name),
            fields: out,
        })
    }

    fn enum_type(&mut self, en: &'t ast::EnumItem, home: Home<'t>) -> Option<TypeId> {
        let key = (home.file.clone(), home.body_id(), en.name.name);
        if let Some(t) = self.nominal.get(&key) {
            return Some(*t);
        }
        let fs = self.home_scope(&home);
        let mut variants = Vec::new();
        let mut unknown = None;
        for (i, v) in en.variants.iter().enumerate() {
            let number = u32::try_from(i).unwrap_or(u32::MAX);
            if v.attrs.iter().any(|a| a.name.as_str() == "unknown") {
                if unknown.is_some() {
                    self.error(
                        code!("BLS0308"),
                        v.name.span,
                        "an enum has at most one `#[unknown]` variant",
                    );
                }
                unknown = Some(number);
            }
            let mut fields = Vec::new();
            for (j, f) in v.fields.iter().enumerate() {
                let ty = self.resolve_type(fs, &f.ty)?;
                let name = f.name.map_or_else(|| Symbol::intern(&j.to_string()), |n| n.name);
                fields.push(field(name, ty));
            }
            variants.push(VariantDef {
                name: v.name.name,
                number,
                payload: fields,
                since: None,
            });
        }
        let t = self.intern_type(
            TypeDef::Enum(EnumDef {
                name: QualName::single(en.name.name),
                variants,
                unknown,
                reserved: Vec::new(),
            }),
            en.name.span,
        );
        self.nominal.insert(key, t);
        Some(t)
    }

    /// An enum type named `name` in scope, for variant paths `E::V`.
    /// The built-in `Part` enum (FOREIGN-PROTOCOLS §1.1): what a stream write sends. `Part::Bytes(b)` is literal
    /// bytes; blob ranges come with blobs (§5).
    pub fn part_type(&mut self, span: Span) -> TypeId {
        let bytes = self.intern_type(TypeDef::Bytes, span);
        let blob = self.intern_type(TypeDef::Blob, span);
        let u64t = self.intern_type(TypeDef::Int(blossom_value::types::IntTy::U64), span);
        self.intern_type(
            TypeDef::Enum(EnumDef {
                name: QualName::single(Symbol::intern(blossom_ir::PART_TYPE)),
                variants: vec![
                    VariantDef {
                        name: Symbol::intern("Bytes"),
                        number: 0,
                        payload: vec![field(Symbol::intern("0"), bytes)],
                        since: None,
                    },
                    // Bytes `lo..hi` of a stored blob, sent without passing through the engine (FOREIGN-PROTOCOLS §5).
                    VariantDef {
                        name: Symbol::intern("Blob"),
                        number: 1,
                        payload: vec![
                            field(Symbol::intern("0"), blob),
                            field(Symbol::intern("1"), u64t),
                            field(Symbol::intern("2"), u64t),
                        ],
                        since: None,
                    },
                ],
                unknown: None,
                reserved: Vec::new(),
            }),
            span,
        )
    }

    pub fn enum_named(&mut self, s: ScopeIdx, name: Ident) -> Option<TypeId> {
        if name.as_str() == "Part" && self.find_def(s, name.name).is_none() {
            return Some(self.part_type(name.span));
        }
        match self.find_def(s, name.name) {
            Some(Def::Enum(en, home)) if en.generics.is_empty() => self.enum_type(en, home),
            Some(Def::Alias(..)) => {
                let t = self.named_type(s, &[name], &[], name.span)?;
                matches!(self.hir.types.get(t), Some(TypeDef::Enum(_))).then_some(t)
            }
            _ => None,
        }
    }

    /// A struct type named `name` in scope, for struct literals.
    pub fn struct_named(&mut self, s: ScopeIdx, name: Ident) -> Option<TypeId> {
        match self.find_def(s, name.name) {
            Some(Def::Struct(st, home)) if st.generics.is_empty() => self.struct_type(st, home),
            _ => None,
        }
    }

    /// The product lattice named `name` (directly, or through a lattice alias), for its literals.
    pub fn product_named(&mut self, s: ScopeIdx, name: Ident) -> Option<TypeId> {
        let ty = match self.find_def(s, name.name) {
            Some(Def::Lattice(l, home)) if l.generics.is_empty() => self.user_lattice(l, home, name.span)?,
            _ => return None,
        };
        matches!(
            self.hir.lattice_of(ty),
            Some((_, blossom_ir::core::LatticeCtor::Product { .. }))
        )
        .then_some(ty)
    }

    /// A constant or value parameter named `name`, folded.
    pub fn lookup_value(&mut self, s: ScopeIdx, name: Symbol) -> Option<(Value, TypeId)> {
        if let Some(v) = self.scope(s).values.get(&name) {
            return Some(v.clone());
        }
        match self.find_def(s, name) {
            Some(Def::Const(ty, value, home)) => {
                let fs = self.def_scope(s, &home);
                let t = self.resolve_type(fs, ty)?;
                let v = self.const_value(fs, value, Some(t))?;
                self.scope_mut(fs).values.insert(name, v.clone());
                Some(v)
            }
            _ => None,
        }
    }

    pub fn role_named(&self, s: ScopeIdx, name: Symbol) -> Option<HRoleId> {
        self.scope(s).roles.get(&name).copied()
    }

    /// Folds a constant expression (LANGUAGE §6.4). `expected` types unsuffixed integer literals.
    pub fn const_value(&mut self, s: ScopeIdx, e: &ast::Expr, expected: Option<TypeId>) -> Option<(Value, TypeId)> {
        let expected_def = expected.and_then(|t| self.hir.types.get(t).cloned());
        let (v, t) = match &e.kind {
            ExprKind::Lit(lit) => match lit {
                LitValue::Int { value, suffix } => {
                    let ity = match (suffix, &expected_def) {
                        (Some(sfx), _) => IntTy::ALL.iter().copied().find(|t| t.name() == sfx.as_str())?,
                        (None, Some(TypeDef::Int(t))) => t.to_owned(),
                        (None, _) => IntTy::I64,
                    };
                    let v = int_value(*value, ity, false).or_else(|| {
                        self.error(
                            code!("BLS0300"),
                            e.span,
                            format!("{value} does not fit in {}", ity.name()),
                        );
                        None
                    })?;
                    (v, self.intern_type(TypeDef::Int(ity), e.span))
                }
                LitValue::Duration(ns) => {
                    let Some(d) = i64::try_from(*ns).ok().map(Duration::from_nanos) else {
                        self.error(code!("BLS0300"), e.span, "duration out of range");
                        return None;
                    };
                    (Value::Duration(d), self.intern_type(TypeDef::Duration, e.span))
                }
                LitValue::Str(s) => (Value::Str(s.as_str().into()), self.intern_type(TypeDef::Str, e.span)),
                LitValue::Bytes(b) => (
                    Value::Bytes(b.as_slice().into()),
                    self.intern_type(TypeDef::Bytes, e.span),
                ),
                LitValue::Bool(b) => (Value::Bool(*b), self.intern_type(TypeDef::Bool, e.span)),
                LitValue::Float(f) => (
                    Value::F64(blossom_value::float::canonical(*f)),
                    self.intern_type(TypeDef::F64, e.span),
                ),
            },
            ExprKind::Prefix { op: PrefixOp::Neg, arg } => {
                if let ExprKind::Lit(LitValue::Int { value, suffix }) = &arg.kind {
                    let ity = match (suffix, &expected_def) {
                        (Some(sfx), _) => IntTy::ALL.iter().copied().find(|t| t.name() == sfx.as_str())?,
                        (None, Some(TypeDef::Int(t))) => t.to_owned(),
                        (None, _) => IntTy::I64,
                    };
                    let Some(v) = int_value(*value, ity, true) else {
                        self.error(
                            code!("BLS0300"),
                            e.span,
                            format!("-{value} does not fit in {}", ity.name()),
                        );
                        return None;
                    };
                    (v, self.intern_type(TypeDef::Int(ity), e.span))
                } else if let ExprKind::Lit(LitValue::Float(f)) = &arg.kind {
                    (
                        Value::F64(blossom_value::float::canonical(-*f)),
                        self.intern_type(TypeDef::F64, e.span),
                    )
                } else {
                    self.unsupported("LANG-010", "this constant expression", e.span);
                    return None;
                }
            }
            ExprKind::Path(path, targs) if targs.is_empty() && path.len() == 1 => {
                let name = path.first()?;
                match self.lookup_value(s, name.name) {
                    Some(v) => v,
                    None => {
                        self.error(
                            code!("BLS0200"),
                            name.span,
                            format!("unknown constant `{}`", name.as_str()),
                        );
                        return None;
                    }
                }
            }
            ExprKind::Binary { op, lhs, rhs } if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div) => {
                let (a, ta) = self.const_value(s, lhs, expected)?;
                let (b, _) = self.const_value(s, rhs, Some(ta))?;
                let v = match (a, b) {
                    (Value::Int(x), Value::Int(y)) => {
                        let (Some(x), Some(y)) = (x.to_i128(), y.to_i128()) else {
                            self.unsupported("LANG-010", "u128 constants above i128::MAX", e.span);
                            return None;
                        };
                        let r = match op {
                            BinOp::Add => x.checked_add(y),
                            BinOp::Sub => x.checked_sub(y),
                            BinOp::Mul => x.checked_mul(y),
                            _ => x.checked_div(y),
                        };
                        let ity = match self.hir.types.get(ta) {
                            Some(TypeDef::Int(t)) => *t,
                            _ => return None,
                        };
                        match r.and_then(|r| IntValue::from_i128(ity, r)) {
                            Some(v) => Value::Int(v),
                            None => {
                                self.error(
                                    code!("BLS0300"),
                                    e.span,
                                    "constant arithmetic overflows or divides by zero",
                                );
                                return None;
                            }
                        }
                    }
                    // IEEE, canonical (LANGUAGE §5.1).
                    (Value::F64(x), Value::F64(y)) => Value::F64(blossom_value::float::canonical(match op {
                        BinOp::Add => x + y,
                        BinOp::Sub => x - y,
                        BinOp::Mul => x * y,
                        _ => x / y,
                    })),
                    (Value::Duration(x), Value::Duration(y)) if matches!(op, BinOp::Add | BinOp::Sub) => {
                        let r = if *op == BinOp::Add {
                            x.checked_add(y)
                        } else {
                            x.checked_sub(y)
                        };
                        match r {
                            Some(d) => Value::Duration(d),
                            None => {
                                self.error(code!("BLS0300"), e.span, "constant duration arithmetic overflows");
                                return None;
                            }
                        }
                    }
                    _ => {
                        self.unsupported("LANG-010", "this constant expression", e.span);
                        return None;
                    }
                };
                (v, ta)
            }
            ExprKind::Tuple(xs) if !xs.is_empty() => {
                let want = match &expected_def {
                    Some(TypeDef::Tuple(ts)) if ts.len() == xs.len() => Some(ts.clone()),
                    _ => None,
                };
                let mut values = Vec::with_capacity(xs.len());
                let mut types = Vec::with_capacity(xs.len());
                for (i, x) in xs.iter().enumerate() {
                    let (v, t) = self.const_value(s, x, want.as_ref().and_then(|w| w.get(i).copied()))?;
                    values.push(v);
                    types.push(t);
                }
                (
                    Value::Tuple(values.into()),
                    self.intern_type(TypeDef::Tuple(types), e.span),
                )
            }
            ExprKind::Vec(xs) | ExprKind::Set(xs) => {
                let mut elem = match &expected_def {
                    Some(TypeDef::Vec(t) | TypeDef::Set(t)) => Some(*t),
                    _ => None,
                };
                let mut values = Vec::with_capacity(xs.len());
                for x in xs {
                    // The first element types the rest (or the declared element type, when there is one).
                    let (v, t) = self.const_value(s, x, elem)?;
                    elem.get_or_insert(t);
                    values.push(v);
                }
                let Some(elem) = elem else {
                    self.error(
                        code!("BLS0300"),
                        e.span,
                        "an empty constant collection needs a declared type",
                    );
                    return None;
                };
                if matches!(e.kind, ExprKind::Vec(_)) {
                    (Value::Vec(values.into()), self.intern_type(TypeDef::Vec(elem), e.span))
                } else {
                    (
                        Value::Set(std::sync::Arc::new(values.into_iter().collect())),
                        self.intern_type(TypeDef::Set(elem), e.span),
                    )
                }
            }
            ExprKind::Map(entries) => {
                let (mut kt, mut vt) = match &expected_def {
                    Some(TypeDef::Map(k, v)) => (Some(*k), Some(*v)),
                    _ => (None, None),
                };
                let mut map = std::collections::BTreeMap::new();
                for (k, v) in entries {
                    let (kv, k_ty) = self.const_value(s, k, kt)?;
                    let (vv, v_ty) = self.const_value(s, v, vt)?;
                    kt.get_or_insert(k_ty);
                    vt.get_or_insert(v_ty);
                    if map.insert(kv, vv).is_some() {
                        self.error(code!("BLS0300"), k.span, "a constant map gives this key twice");
                        return None;
                    }
                }
                let (Some(kt), Some(vt)) = (kt, vt) else {
                    self.error(
                        code!("BLS0300"),
                        e.span,
                        "an empty constant collection needs a declared type",
                    );
                    return None;
                };
                (
                    Value::Map(std::sync::Arc::new(map)),
                    self.intern_type(TypeDef::Map(kt, vt), e.span),
                )
            }
            _ => {
                self.unsupported("LANG-010", "this constant expression", e.span);
                return None;
            }
        };
        if let Some(want) = expected
            && want != t
        {
            self.error(
                code!("BLS0300"),
                e.span,
                format!(
                    "expected a value of type {:?}, found {:?}",
                    self.hir.types.get(want),
                    self.hir.types.get(t)
                ),
            );
            return None;
        }
        Some((v, t))
    }
}

fn field(name: Symbol, ty: TypeId) -> FieldDef {
    FieldDef {
        name,
        ty,
        field_no: None,
        default: None,
        since: None,
        deprecated: None,
        renamed_from: None,
    }
}

impl Resolver<'_, '_> {
    /// A built-in lattice type (LANGUAGE §11.5): `LBool`, `LMax<T>`, `LMin<T>`, `LSet<T>`, `LPSet<T>`, `LMap<K, L>`,
    /// `LPoint<T>`. The others are not implemented in this build.
    fn lattice_type(&mut self, s: ScopeIdx, text: &str, args: &[ast::Type], span: Span) -> Option<TypeId> {
        use blossom_ir::core::LatticeCtor;
        let want = match text {
            "LBool" => 0,
            "LMap" => 2,
            "LMax" | "LMin" | "LSet" | "LPSet" | "LPoint" => 1,
            other => {
                self.unsupported("LANG-124", &format!("the lattice `{other}`"), span);
                return None;
            }
        };
        if args.len() != want {
            self.error(
                code!("BLS0301"),
                span,
                format!("`{text}` takes {want} type argument(s), {} given", args.len()),
            );
            return None;
        }
        let mut tys = Vec::new();
        for a in args {
            tys.push(self.resolve_type(s, a)?);
        }
        let elem = tys.first().copied();
        let ctor = match (text, elem) {
            ("LBool", _) => LatticeCtor::Bool,
            ("LMax", Some(t)) | ("LMin", Some(t)) => {
                if matches!(self.hir.types.get(t), Some(TypeDef::F64)) {
                    self.error(code!("BLS0312"), span, "`f64` is not the element of `LMax`/`LMin`");
                    return None;
                }
                if text == "LMax" {
                    LatticeCtor::Max(t)
                } else {
                    LatticeCtor::Min(t)
                }
            }
            ("LSet", Some(t)) => LatticeCtor::Set(t),
            ("LPSet", Some(t)) => LatticeCtor::PSet(t),
            ("LPoint", Some(t)) => LatticeCtor::Point(t),
            ("LMap", Some(k)) => {
                let v = tys.get(1).copied()?;
                let Some((inner, _)) = self.hir.lattice_of(v) else {
                    self.error(code!("BLS0300"), span, "the values of `LMap<K, L>` are a lattice");
                    return None;
                };
                LatticeCtor::Map(k, inner)
            }
            _ => return None,
        };
        self.intern_lattice_or_bug(ctor)
    }

    fn intern_lattice_or_bug(&mut self, ctor: blossom_ir::core::LatticeCtor) -> Option<TypeId> {
        match self.hir.intern_lattice(ctor) {
            Ok(t) => Some(t),
            Err(e) => {
                self.bugs.push(e);
                None
            }
        }
    }
}

/// The built-in lattice type constructors (LANGUAGE §11.5).
fn is_lattice_name(text: &str) -> bool {
    matches!(
        text,
        "LBool"
            | "LMax"
            | "LMin"
            | "LSet"
            | "LPSet"
            | "LBag"
            | "LMap"
            | "LPair"
            | "Lex"
            | "LDom"
            | "LPoint"
            | "LConflict"
            | "LWithBot"
            | "LWithTop"
            | "LUnit"
            | "LVec"
            | "LUnionFind"
            | "LTombSet"
            | "LTombMap"
            | "Causal"
    )
}

/// An integer literal as a value of `ty` (negated when `neg`), or `None` when it does not fit.
pub(crate) fn int_value(v: u128, ty: IntTy, neg: bool) -> Option<Value> {
    let signed: i128 = if neg {
        if v > i128::MAX as u128 + 1 {
            return None;
        }
        if v == i128::MAX as u128 + 1 {
            i128::MIN
        } else {
            -(v as i128)
        }
    } else {
        i128::try_from(v).ok().unwrap_or(-1)
    };
    let iv = match ty {
        IntTy::U8 if !neg => IntValue::U8(u8::try_from(v).ok()?),
        IntTy::U16 if !neg => IntValue::U16(u16::try_from(v).ok()?),
        IntTy::U32 if !neg => IntValue::U32(u32::try_from(v).ok()?),
        IntTy::U64 if !neg => IntValue::U64(u64::try_from(v).ok()?),
        IntTy::U128 if !neg => IntValue::U128(v),
        IntTy::I8 => IntValue::I8(i8::try_from(signed).ok()?),
        IntTy::I16 => IntValue::I16(i16::try_from(signed).ok()?),
        IntTy::I32 => IntValue::I32(i32::try_from(signed).ok()?),
        IntTy::I64 => IntValue::I64(i64::try_from(signed).ok()?),
        IntTy::I128 => {
            if !neg && v > i128::MAX as u128 {
                return None;
            }
            IntValue::I128(signed)
        }
        _ => return None,
    };
    Some(Value::Int(iv))
}
