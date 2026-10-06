//! CST → [`File`](super::File).
//!
//! The parser has already reported every syntax error; [`convert`] runs only on trees without errors, so a missing
//! child here is a construct the grammar allows but this build does not represent. Each such construct is reported
//! as BLS0908 (not implemented in this build) with its span, and replaced by a placeholder that later phases skip
//! because the program is rejected anyway.

use blossom_base::{Diagnostic, Diagnostics, FeatureId, FileId, Span, Symbol};
use blossom_syntax::{SyntaxKind, SyntaxKind::*, SyntaxNode, SyntaxToken};

use super::*;

/// Converts a parsed file. Diagnostics go to `diags`.
pub fn convert(file: FileId, root: &SyntaxNode, diags: &mut Diagnostics) -> File {
    let mut cx = Cx { file, diags };
    let header = root
        .children()
        .find(|n| n.kind() == PROGRAMHEADER)
        .and_then(|h| cx.header(&h));
    let items = cx.items(root);
    let mut inner_attrs = Vec::new();
    for a in children_of(root, INNERATTR) {
        cx.attr(&a, &mut inner_attrs);
    }
    File {
        inner_attrs,
        header,
        items,
        span: cx.span(root),
    }
}

struct Cx<'d> {
    file: FileId,
    diags: &'d mut Diagnostics,
}

/// The non-trivia tokens that are direct children of `node`.
fn tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia())
}

fn has_token(node: &SyntaxNode, kind: SyntaxKind) -> bool {
    tokens(node).any(|t| t.kind() == kind)
}

fn has_word(node: &SyntaxNode, word: &str) -> bool {
    tokens(node).any(|t| t.kind() == IDENT && t.text() == word)
}

fn children_of(node: &SyntaxNode, kind: SyntaxKind) -> impl Iterator<Item = SyntaxNode> {
    node.children().filter(move |n| n.kind() == kind)
}

/// A function's name, its parameters with their types, and its result type.
type FnSig = (Ident, Vec<GenericParam>, Vec<(Ident, Type)>, Type);

fn child_of(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    children_of(node, kind).next()
}

fn is_expr(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        LITERALEXPR
            | PATHEXPR
            | CALLEXPR
            | METHODCALLEXPR
            | TRYEXPR
            | BANGCALLEXPR
            | FIELDEXPR
            | TUPLEINDEXEXPR
            | INDEXEXPR
            | BINARYEXPR
            | PREFIXEXPR
            | CASTEXPR
            | PARENEXPR
            | TUPLEEXPR
            | VECEXPR
            | SETEXPR
            | MAPEXPR
            | FOLDEXPR
            | IFEXPR
            | MATCHEXPR
            | STRUCTLITEXPR
            | CLOSUREEXPR
            | WILDCARD
            | SELFEXPR
            | FSTRINGEXPR
    )
}

fn expr_children(node: &SyntaxNode) -> impl Iterator<Item = SyntaxNode> {
    node.children().filter(|n| is_expr(n.kind()))
}

fn is_literal(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        NOTLIT
            | LETLIT
            | OUTERLIT
            | INSERTEDLIT
            | DELETEDLIT
            | SEALEDLIT
            | FINALLIT
            | PERLIT
            | ANYLIT
            | FORALLLIT
            | EVERLIT
            | SENTLIT
            | QUORUMLIT
            | ATOMLIT
    )
}

const LANG_002: FeatureId = FeatureId("LANG-002");

impl Cx<'_> {
    /// A node's span without its leading and trailing trivia (the parser attaches whitespace and comments to the
    /// node that follows them).
    fn span(&self, node: &SyntaxNode) -> Span {
        let r = node.text_range();
        let mut tokens = node
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .filter(|t| !t.kind().is_trivia());
        let first = tokens.next();
        let last = tokens.last().or_else(|| first.clone());
        match (first, last) {
            (Some(a), Some(b)) => Span::new(self.file, a.text_range().start().into(), b.text_range().end().into()),
            _ => Span::new(self.file, r.start().into(), r.end().into()),
        }
    }

    fn token_span(&self, t: &SyntaxToken) -> Span {
        let r = t.text_range();
        Span::new(self.file, r.start().into(), r.end().into())
    }

    /// Reports a construct this build does not represent.
    fn unsupported(&mut self, feature: &'static str, what: &str, span: Span) {
        self.diags.push(
            Diagnostic::not_implemented(FeatureId(feature), what, "the Blossom frontend (slice 2)").with_primary(span),
        );
    }

    /// Reports a tree shape the converter did not expect (an error-free parse never produces one).
    /// A closure's parameters: names, or `_` for one it ignores.
    fn closure_params(&mut self, params: &[SyntaxNode]) -> Vec<Ident> {
        let mut names = Vec::new();
        for p in params {
            let span = self.span(p);
            match self.expr(p).kind {
                ExprKind::Path(path, targs) if targs.is_empty() && path.len() == 1 => names.extend(path),
                ExprKind::Wildcard => names.push(Ident {
                    name: Symbol::intern("_"),
                    span,
                }),
                _ => self.malformed("a closure parameter that is not a name", span),
            }
        }
        names
    }

    fn malformed(&mut self, what: &str, span: Span) {
        self.diags.push(
            Diagnostic::not_implemented(
                LANG_002,
                &format!("syntax shape: {what}"),
                "the Blossom frontend (slice 2)",
            )
            .with_primary(span),
        );
    }

    fn ident(&mut self, node: &SyntaxNode) -> Ident {
        let span = self.span(node);
        let text = tokens(node).next().map(|t| t.text().to_owned()).unwrap_or_default();
        let text = text.strip_prefix("r#").unwrap_or(&text).to_owned();
        Ident {
            name: Symbol::intern(&text),
            span,
        }
    }

    fn names(&mut self, node: &SyntaxNode) -> Vec<Ident> {
        children_of(node, NAME).map(|n| self.ident(&n)).collect()
    }

    fn first_name(&mut self, node: &SyntaxNode) -> Option<Ident> {
        child_of(node, NAME).map(|n| self.ident(&n))
    }

    fn need_name(&mut self, node: &SyntaxNode) -> Ident {
        match self.first_name(node) {
            Some(n) => n,
            None => {
                let span = self.span(node);
                self.malformed("a missing name", span);
                Ident {
                    name: Symbol::intern("<missing>"),
                    span,
                }
            }
        }
    }

    fn header(&mut self, node: &SyntaxNode) -> Option<ProgramHeader> {
        let name = self.first_name(node)?;
        let ints: Vec<SyntaxToken> = tokens(node).filter(|t| t.kind() == INT_LIT).collect();
        let version = ints.first().and_then(|t| parse_int(t.text()).ok()).map(|(v, _)| v);
        let edition = ints.get(1).and_then(|t| parse_int(t.text()).ok()).map(|(v, _)| v);
        let span = self.span(node);
        let Some(version) = version.and_then(|v| u32::try_from(v).ok()) else {
            self.malformed("a program version that is not a u32", span);
            return None;
        };
        let edition = match edition {
            None => 1,
            Some(e) => match u16::try_from(e) {
                Ok(e) => e,
                Err(_) => {
                    self.malformed("an edition that is not a u16", span);
                    1
                }
            },
        };
        Some(ProgramHeader {
            name,
            version,
            edition,
            span,
        })
    }

    fn attrs(&mut self, node: &SyntaxNode) -> Vec<Attr> {
        let mut out = Vec::new();
        for a in children_of(node, ATTR) {
            self.attr(&a, &mut out);
        }
        out
    }

    /// One `#[…]` or `#![…]`: each comma-separated body `path`, `path(args…)` or `path = e`, in order. Inside the
    /// parentheses an argument is `name = e`, a keyword (kept as a one-segment path, `#[accept(external)]`), or an
    /// expression.
    fn attr(&mut self, node: &SyntaxNode, out: &mut Vec<Attr>) {
        let span = self.span(node);
        let elems: Vec<_> = node.children_with_tokens().filter(|e| !e.kind().is_trivia()).collect();
        // Past the opening `#[` or `#![`.
        let mut i = 1;
        loop {
            let mut path = Vec::new();
            while let Some(e) = elems.get(i) {
                match (e.kind(), e.as_node()) {
                    (NAME, Some(n)) => path.push(self.ident(n)),
                    (COLON2, _) => {}
                    _ => break,
                }
                i += 1;
            }
            let (Some(first), Some(last)) = (path.first().copied(), path.last().copied()) else {
                self.malformed("an attribute without a name", span);
                return;
            };
            let name = if path.len() == 1 {
                first
            } else {
                let text: Vec<&str> = path.iter().map(Ident::as_str).collect();
                Ident {
                    name: Symbol::intern(&text.join("::")),
                    span: first.span.to(last.span).unwrap_or(first.span),
                }
            };
            let mut end = last.span;
            let mut args = Vec::new();
            let mut value = None;
            match elems.get(i).map(|e| e.kind()) {
                Some(L_PAREN) => {
                    i += 1;
                    loop {
                        let Some(e) = elems.get(i) else {
                            self.malformed("an unclosed attribute argument list", span);
                            return;
                        };
                        i += 1;
                        match (e.kind(), e.as_node(), e.as_token()) {
                            (R_PAREN, _, Some(t)) => {
                                end = self.token_span(t);
                                break;
                            }
                            (COMMA, _, _) => {}
                            (NAME, Some(n), _) => {
                                let n = self.ident(n);
                                if elems.get(i).is_some_and(|e| e.kind() == EQ) {
                                    match elems.get(i + 1).and_then(|e| e.as_node()).filter(|x| is_expr(x.kind())) {
                                        Some(x) => args.push(Arg::Named(n, self.expr(x))),
                                        None => {
                                            self.malformed("an attribute argument `name =` without a value", n.span);
                                            return;
                                        }
                                    }
                                    i += 2;
                                } else {
                                    args.push(Arg::Pos(Expr {
                                        kind: ExprKind::Path(vec![n], Vec::new()),
                                        span: n.span,
                                    }));
                                }
                            }
                            (k, Some(x), _) if is_expr(k) => args.push(Arg::Pos(self.expr(x))),
                            (k, _, _) => {
                                self.malformed(&format!("attribute argument {k:?}"), span);
                                return;
                            }
                        }
                    }
                }
                Some(EQ) => {
                    match elems.get(i + 1).and_then(|e| e.as_node()).filter(|x| is_expr(x.kind())) {
                        Some(x) => {
                            let e = self.expr(x);
                            end = e.span;
                            value = Some(e);
                        }
                        None => {
                            self.malformed("an attribute `name =` without a value", span);
                            return;
                        }
                    }
                    i += 2;
                }
                _ => {}
            }
            out.push(Attr {
                name,
                args,
                value,
                span: first.span.to(end).unwrap_or(first.span),
            });
            match elems.get(i).map(|e| e.kind()) {
                Some(COMMA) if elems.get(i + 1).is_some_and(|e| e.kind() != R_BRACK) => i += 1,
                Some(COMMA | R_BRACK) => return,
                other => {
                    self.malformed(&format!("attribute continuation {other:?}"), span);
                    return;
                }
            }
        }
    }

    fn items(&mut self, node: &SyntaxNode) -> Vec<Item> {
        let mut out = Vec::new();
        for child in node.children() {
            if matches!(
                child.kind(),
                PROGRAMHEADER | INNERATTR | NAME | ATTR | GENERICS | MODPARAMS | TYPE | RELPATH | BLOCK | BODY
            ) {
                continue;
            }
            if let Some(item) = self.item(&child) {
                out.push(item);
            }
        }
        out
    }

    fn item(&mut self, node: &SyntaxNode) -> Option<Item> {
        let span = self.span(node);
        let attrs = self.attrs(node);
        let is_pub = has_token(node, PUB_KW);
        let kind = match node.kind() {
            USEITEM => ItemKind::Use(self.use_item(node)),
            IMPORTITEM => ItemKind::Import(self.import(node)),
            INCLUDEITEM => ItemKind::Include(self.include(node)),
            CONSTITEM => {
                let name = self.need_name(node);
                let ty = self.need_type(node);
                let value = self.need_expr(node);
                ItemKind::Const { name, ty, value }
            }
            PARAMITEM => {
                let name = self.need_name(node);
                let ty = self.need_type(node);
                let default = expr_children(node).next().map(|e| self.expr(&e));
                ItemKind::Param { name, ty, default }
            }
            TYPEALIAS => {
                let name = self.need_name(node);
                let generics = self.generics(node);
                let ty = self.need_type(node);
                ItemKind::TypeAlias { name, generics, ty }
            }
            STRUCTITEM => ItemKind::Struct(self.struct_item(node)),
            ENUMITEM => ItemKind::Enum(self.enum_item(node)),
            MODULEITEM => ItemKind::Module(self.module(node)),
            PROTOCOLITEM => {
                let name = self.need_name(node);
                let generics = self.generics(node);
                let items = self.items(node);
                ItemKind::Protocol(ProtocolItem { name, generics, items })
            }
            FORMATITEM => ItemKind::Format(self.format_item(node, span)?),
            TREEITEM => ItemKind::Tree(self.tree_item(node, span)?),
            STREAMITEM => {
                let names = self.names(node);
                let (Some(name), Some(kind)) = (names.first().copied(), names.get(1).copied()) else {
                    self.malformed("a stream without a name or a kind", span);
                    return None;
                };
                ItemKind::Stream { name, kind }
            }
            ROLEITEM => {
                let names = self.names(node);
                let Some(name) = names.first().copied() else {
                    self.malformed("a role without a name", span);
                    return None;
                };
                ItemKind::Role {
                    name,
                    kind: names.get(1).copied(),
                }
            }
            ATSECTION => {
                let role = self.need_name(node);
                let items = self.items(node);
                ItemKind::At { role, items }
            }
            RELDECL => ItemKind::Rel(self.rel_decl(node)),
            TIMERDECL => {
                let name = self.need_name(node);
                let words = tokens(node)
                    .filter(|t| t.kind() == IDENT && t.text() != "timer")
                    .map(|t| Ident {
                        name: Symbol::intern(t.text()),
                        span: self.token_span(&t),
                    })
                    .collect();
                let exprs = expr_children(node).map(|e| self.expr(&e)).collect();
                let guard = child_of(node, RELPATH).map(|r| self.names(&r));
                ItemKind::Timer(TimerDecl {
                    name,
                    words,
                    exprs,
                    guard,
                    span,
                })
            }
            VIEWDECL => ItemKind::View(self.view(node)),
            HANDLERITEM => ItemKind::Handler(self.handler(node)),
            BOOTSTRAPITEM => {
                let fresh = has_word(node, "fresh");
                let block = self.need_block(node);
                ItemKind::Bootstrap { fresh, block }
            }
            FACTITEM => ItemKind::Fact(self.fact(node)),
            INVARIANTITEM => ItemKind::Invariant(self.invariant(node)),
            INTERPOSEITEM => {
                let target = child_of(node, RELPATH).map(|r| self.names(&r)).unwrap_or_default();
                let names = self.names(node);
                let (Some(outside), Some(inside)) = (names.first().copied(), names.get(1).copied()) else {
                    self.malformed("an interposition without its two names", span);
                    return None;
                };
                let items = self.items(node);
                ItemKind::Interpose(Interpose {
                    target,
                    outside,
                    inside,
                    items,
                })
            }
            SPECITEM => ItemKind::Spec(self.spec(node)),
            FNITEM => ItemKind::Fn(self.fn_item(node, span)?),
            EXTERNITEM => self.extern_item(node, span)?,
            IMPLITEM => self.unsupported_item("LANG-180", "impl blocks", span),
            LATTICETYPEITEM => self.unsupported_item("LANG-135", "user-defined lattices", span),
            AGGREGATEITEM => self.unsupported_item("LANG-105", "user-defined aggregates", span),
            SERVICEITEM => self.unsupported_item("LANG-184", "async services", span),
            BLOCKITEM => self.unsupported_item("LANG-007", "named blocks", span),
            OVERRIDEITEM => self.unsupported_item("LANG-007", "override", span),
            ACLITEM => self.unsupported_item("LANG-242", "ACL items", span),
            CELLDECL => ItemKind::Rel(self.cell_decl(node)),
            MIGRATEITEM => self.unsupported_item("LANG-262", "migrations", span),
            TRANSLATEITEM => self.unsupported_item("LANG-263", "channel translations", span),
            SNAPSHOTITEM => self.unsupported_item("LANG-139", "progressive snapshots", span),
            other => {
                self.malformed(&format!("item {other:?}"), span);
                return None;
            }
        };
        Some(Item {
            attrs,
            is_pub,
            kind,
            span,
        })
    }

    fn unsupported_item(&mut self, feature: &'static str, what: &'static str, span: Span) -> ItemKind {
        self.unsupported(feature, what, span);
        ItemKind::Unsupported { what }
    }

    fn use_item(&mut self, node: &SyntaxNode) -> UseTree {
        let mut paths = Vec::new();
        if let Some(tree) = child_of(node, USETREE) {
            self.use_tree(&tree, &mut Vec::new(), &mut paths);
        }
        UseTree { paths }
    }

    fn use_tree(&mut self, node: &SyntaxNode, prefix: &mut Vec<Ident>, out: &mut Vec<Vec<Ident>>) {
        let names = self.names(node);
        let depth = prefix.len();
        prefix.extend(names);
        let subtrees: Vec<SyntaxNode> = children_of(node, USETREE).collect();
        if subtrees.is_empty() {
            out.push(prefix.clone());
        } else {
            for s in subtrees {
                self.use_tree(&s, prefix, out);
            }
        }
        prefix.truncate(depth);
    }

    fn import(&mut self, node: &SyntaxNode) -> Import {
        let mut module = Vec::new();
        let mut alias = None;
        let mut args = Vec::new();
        let mut roles = Vec::new();
        let mut type_args = Vec::new();
        let mut after_as = false;
        let mut after_with = false;
        for e in node.children_with_tokens() {
            if let Some(t) = e.as_token() {
                if t.kind() == AS_KW {
                    after_as = true;
                } else if t.kind() == IDENT && t.text() == "with" {
                    after_with = true;
                }
            }
            if let Some(n) = e.into_node() {
                match n.kind() {
                    NAME if !after_as => module.push(self.ident(&n)),
                    NAME => alias = Some(self.ident(&n)),
                    GENERICARGS => type_args = self.generic_args(&n),
                    ARG if after_with => {
                        let span = self.span(&n);
                        match self.arg(&n) {
                            Arg::Named(role, value) => match &value.kind {
                                ExprKind::Path(p, _) if p.len() == 1 => {
                                    if let Some(target) = p.first() {
                                        roles.push((role, *target));
                                    }
                                }
                                _ => self.malformed("a role binding that is not `Role = Role`", span),
                            },
                            _ => self.malformed("a role binding that is not `Role = Role`", span),
                        }
                    }
                    ARG => args.push(self.arg(&n)),
                    _ => {}
                }
            }
        }
        let alias = alias.unwrap_or_else(|| {
            let span = self.span(node);
            self.malformed("an import without `as`", span);
            Ident {
                name: Symbol::intern("<missing>"),
                span,
            }
        });
        Import {
            module,
            type_args,
            args,
            alias,
            roles,
        }
    }

    fn include(&mut self, node: &SyntaxNode) -> IncludeTarget {
        if let Some(s) = tokens(node).find(|t| t.kind() == STRING_LIT) {
            let span = self.token_span(&s);
            return IncludeTarget::File(self.string(s.text(), span));
        }
        IncludeTarget::Module(self.names(node))
    }

    fn generics(&mut self, node: &SyntaxNode) -> Vec<GenericParam> {
        let Some(g) = child_of(node, GENERICS) else {
            return Vec::new();
        };
        children_of(&g, GENERICPARAM)
            .map(|p| GenericParam {
                name: self.need_name(&p),
                bounds: children_of(&p, TYPE).map(|t| self.ty(&t)).collect(),
            })
            .collect()
    }

    fn generic_args(&mut self, node: &SyntaxNode) -> Vec<Type> {
        let mut out = Vec::new();
        for a in children_of(node, GENERICARG) {
            match child_of(&a, TYPE) {
                Some(t) => out.push(self.ty(&t)),
                None => {
                    let span = self.span(&a);
                    self.unsupported("LANG-021", "named generic arguments", span);
                }
            }
        }
        out
    }

    fn need_type(&mut self, node: &SyntaxNode) -> Type {
        match child_of(node, TYPE) {
            Some(t) => self.ty(&t),
            None => {
                let span = self.span(node);
                self.malformed("a missing type", span);
                Type::Tuple {
                    elems: Vec::new(),
                    span,
                }
            }
        }
    }

    fn ty(&mut self, node: &SyntaxNode) -> Type {
        let span = self.span(node);
        if tokens(node)
            .next()
            .is_some_and(|t| t.kind() == IDENT && t.text() == "unsafe")
            && let Some(inner) = child_of(node, TYPE)
        {
            return Type::Unsafe {
                inner: Box::new(self.ty(&inner)),
                span,
            };
        }
        if has_token(node, FN_KW) {
            // `fn(A, B) -> R`: the parameter types, then the result type (the last type child).
            let mut tys: Vec<Type> = children_of(node, TYPE).map(|t| self.ty(&t)).collect();
            let ret = tys.pop().unwrap_or(Type::Tuple {
                elems: Vec::new(),
                span,
            });
            return Type::Fn {
                params: tys,
                ret: Box::new(ret),
                span,
            };
        }
        let path = self.names(node);
        if path.is_empty() {
            let elems = children_of(node, TYPE).map(|t| self.ty(&t)).collect();
            return Type::Tuple { elems, span };
        }
        let args = child_of(node, GENERICARGS)
            .map(|g| self.generic_args(&g))
            .unwrap_or_default();
        Type::Named { path, args, span }
    }

    fn field_decls(&mut self, node: &SyntaxNode) -> (Vec<FieldDecl>, bool) {
        let named: Vec<SyntaxNode> = children_of(node, FIELDDECL).collect();
        if !named.is_empty() {
            let fields = named
                .iter()
                .map(|f| FieldDecl {
                    attrs: self.attrs(f),
                    name: self.first_name(f),
                    ty: self.need_type(f),
                    span: self.span(f),
                })
                .collect();
            return (fields, false);
        }
        let positional: Vec<FieldDecl> = children_of(node, TYPE)
            .map(|t| {
                let span = self.span(&t);
                FieldDecl {
                    attrs: Vec::new(),
                    name: None,
                    ty: self.ty(&t),
                    span,
                }
            })
            .collect();
        let tuple = !positional.is_empty() || has_token(node, L_PAREN);
        (positional, tuple)
    }

    fn struct_item(&mut self, node: &SyntaxNode) -> StructItem {
        let name = self.need_name(node);
        let generics = self.generics(node);
        let (fields, tuple) = self.field_decls(node);
        StructItem {
            name,
            generics,
            fields,
            tuple,
        }
    }

    fn enum_item(&mut self, node: &SyntaxNode) -> EnumItem {
        let name = self.need_name(node);
        let generics = self.generics(node);
        let variants = children_of(node, VARIANT)
            .map(|v| {
                let (fields, tuple) = self.field_decls(&v);
                Variant {
                    attrs: self.attrs(&v),
                    name: self.need_name(&v),
                    fields,
                    tuple,
                    span: self.span(&v),
                }
            })
            .collect();
        EnumItem {
            name,
            generics,
            variants,
        }
    }

    fn module(&mut self, node: &SyntaxNode) -> ModuleItem {
        let name = self.need_name(node);
        let choreography = has_token(node, CHOREOGRAPHY_KW);
        let generics = self.generics(node);
        let mut params = Vec::new();
        if let Some(ps) = child_of(node, MODPARAMS) {
            for p in children_of(&ps, MODPARAM) {
                let span = self.span(&p);
                let pname = self.need_name(&p);
                let kind = if let Some(list) = child_of(&p, PARAMLIST) {
                    let cols = children_of(&list, PARAM)
                        .map(|c| (self.need_name(&c), self.need_type(&c)))
                        .collect();
                    ModParamKind::Rel { cols }
                } else {
                    ModParamKind::Value {
                        ty: self.need_type(&p),
                        default: expr_children(&p).next().map(|e| self.expr(&e)),
                    }
                };
                params.push(ModParam {
                    name: pname,
                    kind,
                    span,
                });
            }
        }
        let protocols = children_of(node, TYPE).map(|t| self.ty(&t)).collect();
        let items = self.items(node);
        ModuleItem {
            name,
            choreography,
            generics,
            params,
            protocols,
            items,
        }
    }

    fn rel_decl(&mut self, node: &SyntaxNode) -> RelDecl {
        let span = self.span(node);
        let name = self.need_name(node);
        let kind = tokens(node).find_map(|t| match t.kind() {
            TABLE_KW => Some(RelKind::Table),
            SCRATCH_KW => Some(RelKind::Scratch),
            CHANNEL_KW => Some(RelKind::Channel),
            INPUT_KW => Some(RelKind::Input),
            OUTPUT_KW => Some(RelKind::Output),
            STATIC_KW => Some(RelKind::Static),
            LOOPBACK_KW => Some(RelKind::Loopback),
            _ => None,
        });
        let kind = kind.unwrap_or_else(|| {
            self.malformed("a relation declaration without a kind", span);
            RelKind::Scratch
        });
        let mut mods = RelMods::default();
        for t in tokens(node) {
            match t.text() {
                "durable" => mods.durable = true,
                "soft" => mods.soft = true,
                "sealed" => mods.sealed = true,
                "zset" => mods.zset = true,
                "bag" => mods.bag = true,
                "final" => mods.final_ = true,
                _ => {}
            }
        }
        let cols = children_of(node, COLDECL)
            .map(|c| ColDecl {
                attrs: self.attrs(&c),
                dest: has_token(&c, AT),
                name: self.need_name(&c),
                ty: self.need_type(&c),
                default: expr_children(&c).next().map(|e| self.expr(&e)),
                span: self.span(&c),
            })
            .collect();
        let like = child_of(node, RELPATH).map(|r| self.names(&r));
        let mut key = None;
        let mut direction = None;
        let mut resolve = None;
        let mut guard = None;
        let mut other_clauses = Vec::new();
        for c in node.children() {
            let cspan = self.span(&c);
            match c.kind() {
                KEYCLAUSE => key = Some((self.names(&c), cspan)),
                DIRECTIONCLAUSE => {
                    let names = self.names(&c);
                    match (names.first(), names.get(1)) {
                        (Some(a), Some(b)) => direction = Some((*a, *b)),
                        _ => self.malformed("a direction without two roles", cspan),
                    }
                }
                TTLCLAUSE => other_clauses.push(("ttl", cspan)),
                MAXCLAUSE => other_clauses.push(("max", cspan)),
                RANGECLAUSE => other_clauses.push(("range", cspan)),
                RESOLVECLAUSE => resolve = child_of(&c, POLICY).map(|p| (self.rel_policy(&p), cspan)),
                PARTITIONCLAUSE => other_clauses.push(("partition by", cspan)),
                SEALEDBYCLAUSE => other_clauses.push(("sealed by", cspan)),
                EXACTLYONCECLAUSE => other_clauses.push(("exactly_once", cspan)),
                WHILECLAUSE => match child_of(&c, BODY) {
                    Some(b) => guard = Some(self.body(&b)),
                    None => self.malformed("a `while` clause without a body", cspan),
                },
                _ => {}
            }
        }
        RelDecl {
            name,
            kind,
            mods,
            cols,
            like,
            key,
            direction,
            resolve,
            other_clauses,
            guard,
            span,
        }
    }

    fn rel_policy(&mut self, node: &SyntaxNode) -> RelPolicy {
        let sticky = has_word(node, "sticky");
        let expr = || expr_children(node).next();
        if has_word(node, "prefer") {
            RelPolicy::Prefer(self.names(node))
        } else if has_word(node, "choose_rand") {
            RelPolicy::ChooseRand { sticky }
        } else if has_word(node, "choose_least") {
            match expr() {
                Some(e) => RelPolicy::Least(self.expr(&e)),
                None => {
                    self.malformed("`choose_least` without its cost", self.span(node));
                    RelPolicy::Choose { sticky: false }
                }
            }
        } else if has_word(node, "choose_most") {
            match expr() {
                Some(e) => RelPolicy::Most(self.expr(&e)),
                None => {
                    self.malformed("`choose_most` without its cost", self.span(node));
                    RelPolicy::Choose { sticky: false }
                }
            }
        } else if has_word(node, "merge") {
            RelPolicy::Merge
        } else {
            RelPolicy::Choose { sticky }
        }
    }

    /// `[durable] [scratch] cell name: L;` as the relation `name(value: L)` (LANGUAGE §7.13).
    fn cell_decl(&mut self, node: &SyntaxNode) -> RelDecl {
        let span = self.span(node);
        let name = self.need_name(node);
        let kind = if has_token(node, SCRATCH_KW) {
            RelKind::Scratch
        } else {
            RelKind::Table
        };
        let mods = RelMods {
            durable: tokens(node).any(|t| t.text() == "durable"),
            cell: true,
            ..RelMods::default()
        };
        let ty = self.need_type(node);
        RelDecl {
            name,
            kind,
            mods,
            cols: vec![ColDecl {
                attrs: Vec::new(),
                dest: false,
                name: Ident {
                    name: Symbol::intern("value"),
                    span,
                },
                ty,
                default: None,
                span,
            }],
            like: None,
            key: None,
            direction: None,
            resolve: None,
            other_clauses: Vec::new(),
            guard: None,
            span,
        }
    }

    fn view(&mut self, node: &SyntaxNode) -> ViewDecl {
        let name = self.need_name(node);
        let monotone = has_word(node, "monotone");
        let cols = children_of(node, VIEWCOL)
            .map(|c| ViewCol {
                name: self.need_name(&c),
                ty: child_of(&c, TYPE).map(|t| self.ty(&t)),
                agg: expr_children(&c).next().map(|e| self.expr(&e)),
                span: self.span(&c),
            })
            .collect();
        let alternatives = children_of(node, BODY).map(|b| self.body(&b)).collect();
        ViewDecl {
            name,
            monotone,
            cols,
            alternatives,
            span: self.span(node),
        }
    }

    fn handler(&mut self, node: &SyntaxNode) -> Handler {
        let label = self.first_name(node);
        let monotone = has_word(node, "monotone");
        let trigger = if has_token(node, WHILE_KW) {
            Trigger::While
        } else {
            Trigger::On
        };
        let header = match child_of(node, BODY) {
            Some(b) => self.body(&b),
            None => {
                let span = self.span(node);
                self.malformed("a handler without a header", span);
                Body {
                    lits: Vec::new(),
                    guards: Vec::new(),
                    span,
                }
            }
        };
        let block = self.need_block(node);
        Handler {
            label,
            monotone,
            trigger,
            header,
            block,
            span: self.span(node),
        }
    }

    fn need_block(&mut self, node: &SyntaxNode) -> Block {
        match child_of(node, BLOCK) {
            Some(b) => self.block(&b),
            None => {
                let span = self.span(node);
                self.malformed("a missing block", span);
                Block {
                    stmts: Vec::new(),
                    span,
                }
            }
        }
    }

    fn block(&mut self, node: &SyntaxNode) -> Block {
        let mut stmts = Vec::new();
        for s in node.children() {
            if let Some(stmt) = self.stmt(&s) {
                stmts.push(stmt);
            }
        }
        Block {
            stmts,
            span: self.span(node),
        }
    }

    fn stmt(&mut self, node: &SyntaxNode) -> Option<Stmt> {
        let span = self.span(node);
        match node.kind() {
            VERBSTMT => {
                let verb = tokens(node).find_map(|t| match t.kind() {
                    EMIT_KW => Some(Verb::Emit),
                    NEXT_KW => Some(Verb::Next),
                    SEND_KW => Some(Verb::Send),
                    DELETE_KW => Some(Verb::Delete),
                    UPSERT_KW => Some(Verb::Upsert),
                    SEAL_KW => Some(Verb::Seal),
                    _ => None,
                });
                let Some(verb) = verb else {
                    self.malformed("a statement without a verb", span);
                    return None;
                };
                let (head, tree) = match (child_of(node, HEAD), child_of(node, TREENAME), child_of(node, ELEMENT)) {
                    (Some(h), _, _) => (self.head(&h), None),
                    (None, Some(t), Some(e)) => {
                        let tspan = self.span(&t);
                        let rel = match child_of(&t, RELPATH) {
                            Some(r) => self.names(&r),
                            None => self.names(&t),
                        };
                        let head = Head {
                            rel,
                            args: Vec::new(),
                            span: tspan,
                        };
                        (head, Some(Box::new(self.element(&e))))
                    }
                    _ => {
                        self.malformed("a statement without a head", span);
                        return None;
                    }
                };
                let children = child_of(node, CHILDREN).map(|c| self.children(&c)).unwrap_or_default();
                let extra = expr_children(node).next().map(|e| self.expr(&e));
                let (to, weight) = match verb {
                    Verb::Send | Verb::Seal => (extra, None),
                    _ => (None, extra),
                };
                let resolve = child_of(node, POLICY).map(|p| self.policy(&p));
                Some(Stmt::Verb(Box::new(VerbStmt {
                    attrs: self.attrs(node),
                    verb,
                    head,
                    to,
                    weight,
                    resolve,
                    children,
                    tree,
                    tag: None,
                    span,
                })))
            }
            IFSTMT => {
                let cond = match child_of(node, BODY) {
                    Some(b) => self.body(&b),
                    None => {
                        self.malformed("an `if` without a condition", span);
                        return None;
                    }
                };
                let blocks: Vec<SyntaxNode> = children_of(node, BLOCK).collect();
                let Some(then) = blocks.first().map(|b| self.block(b)) else {
                    self.malformed("an `if` without a block", span);
                    return None;
                };
                let els = if let Some(b) = blocks.get(1) {
                    Some(Box::new(Else::Block(self.block(b))))
                } else if let Some(nested) = child_of(node, IFSTMT) {
                    self.stmt(&nested).map(|s| Box::new(Else::If(Box::new(s))))
                } else {
                    None
                };
                Some(Stmt::If {
                    attrs: self.attrs(node),
                    cond,
                    then,
                    els,
                    span,
                })
            }
            FORSTMT => {
                let cond = match child_of(node, BODY) {
                    Some(b) => self.body(&b),
                    None => {
                        self.malformed("a `for` without a condition", span);
                        return None;
                    }
                };
                let block = self.need_block(node);
                Some(Stmt::For {
                    attrs: self.attrs(node),
                    cond,
                    block,
                    span,
                })
            }
            ATTR => None,
            other => {
                self.malformed(&format!("statement {other:?}"), span);
                None
            }
        }
    }

    fn head(&mut self, node: &SyntaxNode) -> Head {
        let rel = child_of(node, RELPATH).map(|r| self.names(&r)).unwrap_or_default();
        let args = children_of(node, ARG).map(|a| self.arg(&a)).collect();
        Head {
            rel,
            args,
            span: self.span(node),
        }
    }

    fn policy(&mut self, node: &SyntaxNode) -> Policy {
        Policy {
            name: self.need_name(node),
            arg: expr_children(node).next().map(|e| self.expr(&e)),
            span: self.span(node),
        }
    }

    fn fact(&mut self, node: &SyntaxNode) -> Fact {
        let span = self.span(node);
        let head = match child_of(node, HEAD) {
            Some(h) => self.head(&h),
            None => {
                self.malformed("a fact without a head", span);
                Head {
                    rel: Vec::new(),
                    args: Vec::new(),
                    span,
                }
            }
        };
        // Each expression follows its keyword: `@ n`, `from s`, `at tick k`.
        let (mut at, mut from, mut tick) = (None, None, None);
        let mut slot = 0;
        for e in node.children_with_tokens() {
            if let Some(t) = e.as_token() {
                if t.kind() == AT {
                    slot = 1;
                } else if t.kind() == IDENT && t.text() == "from" {
                    slot = 2;
                } else if t.kind() == IDENT && t.text() == "tick" {
                    slot = 3;
                }
                continue;
            }
            let Some(n) = e.into_node() else { continue };
            if !is_expr(n.kind()) {
                continue;
            }
            let x = self.expr(&n);
            match slot {
                1 => at = Some(x),
                2 => from = Some(x),
                3 => tick = Some(x),
                _ => self.malformed("an expression in a fact outside `@`, `from` and `at tick`", span),
            }
        }
        Fact {
            head,
            at,
            from,
            tick,
            span,
        }
    }

    fn invariant(&mut self, node: &SyntaxNode) -> Invariant {
        let span = self.span(node);
        let message = tokens(node).find(|t| t.kind() == STRING_LIT).map(|t| {
            let s = self.token_span(&t);
            self.string(t.text(), s)
        });
        let body = match child_of(node, BODY) {
            Some(b) => self.body(&b),
            None => {
                self.malformed("an invariant without a body", span);
                Body {
                    lits: Vec::new(),
                    guards: Vec::new(),
                    span,
                }
            }
        };
        Invariant {
            name: self.need_name(node),
            message,
            body,
            span,
        }
    }

    fn body(&mut self, node: &SyntaxNode) -> Body {
        let mut lits = Vec::new();
        let mut guards = Vec::new();
        for c in node.children() {
            if is_literal(c.kind()) {
                if let Some(l) = self.lit(&c) {
                    lits.push(l);
                }
            } else if is_expr(c.kind()) {
                guards.push(self.expr(&c));
            }
        }
        Body {
            lits,
            guards,
            span: self.span(node),
        }
    }

    fn atom_lit_of(&mut self, node: &SyntaxNode) -> Option<AtomLit> {
        match child_of(node, ATOMLIT) {
            Some(a) => Some(self.atom_lit(&a)),
            None => {
                let span = self.span(node);
                self.malformed("a literal without its atom", span);
                None
            }
        }
    }

    fn lit(&mut self, node: &SyntaxNode) -> Option<Lit> {
        let span = self.span(node);
        Some(match node.kind() {
            ATOMLIT => Lit::Plain(self.atom_lit(node)),
            NOTLIT => {
                if let Some(b) = child_of(node, BODY) {
                    Lit::NotBody(self.body(&b), span)
                } else {
                    let inner = node.children().find(|c| is_literal(c.kind()));
                    match inner {
                        Some(i) => Lit::Not(Box::new(self.lit(&i)?), span),
                        None => {
                            self.malformed("`not` without an operand", span);
                            return None;
                        }
                    }
                }
            }
            LETLIT => {
                let mut es = expr_children(node);
                let (Some(p), Some(v)) = (es.next(), es.next()) else {
                    self.malformed("`let` without a pattern and a value", span);
                    return None;
                };
                Lit::Let {
                    pat: self.expr(&p),
                    value: self.expr(&v),
                    span,
                }
            }
            OUTERLIT => Lit::Outer(self.atom_lit_of(node)?),
            INSERTEDLIT => Lit::Inserted(self.atom_lit_of(node)?),
            DELETEDLIT => Lit::Deleted(self.atom_lit_of(node)?),
            SEALEDLIT => Lit::Sealed(self.atom_lit_of(node)?),
            FINALLIT => Lit::Final(self.atom_lit_of(node)?),
            PERLIT => Lit::Per(self.atom_lit_of(node)?),
            ANYLIT => Lit::Any(children_of(node, BODY).map(|b| self.body(&b)).collect(), span),
            FORALLLIT => {
                let domain = self.atom_lit_of(node)?;
                let body = match child_of(node, BODY) {
                    Some(b) => self.body(&b),
                    None => {
                        self.malformed("`forall` without a body", span);
                        return None;
                    }
                };
                Lit::Forall { domain, body, span }
            }
            EVERLIT => Lit::Spec(SpecLit::Ever(self.atom_lit_of(node)?)),
            SENTLIT => Lit::Spec(SpecLit::Sent(self.atom_lit_of(node)?)),
            QUORUMLIT => {
                let var = self.need_name(node);
                let role = child_of(node, RELPATH).map(|r| self.names(&r)).unwrap_or_default();
                let body = match child_of(node, BODY) {
                    Some(b) => self.body(&b),
                    None => {
                        self.malformed("`quorum` without a body", span);
                        return None;
                    }
                };
                Lit::Spec(SpecLit::Quorum { var, role, body, span })
            }
            other => {
                self.malformed(&format!("literal {other:?}"), span);
                return None;
            }
        })
    }

    fn atom_lit(&mut self, node: &SyntaxNode) -> AtomLit {
        let span = self.span(node);
        let expr = match expr_children(node).next() {
            Some(e) => self.expr(&e),
            None => {
                self.malformed("a literal without an expression", span);
                Expr {
                    kind: ExprKind::Wildcard,
                    span,
                }
            }
        };
        let mut lit = AtomLit {
            expr,
            from: None,
            principal: None,
            weight: None,
            at: None,
            at_tick: None,
            span,
        };
        for s in node.children() {
            let value = expr_children(&s).next().map(|e| self.expr(&e));
            match s.kind() {
                FROMSUFFIX => lit.from = value,
                PRINCIPALSUFFIX => lit.principal = value,
                WEIGHTSUFFIX => lit.weight = value,
                ATSUFFIX => lit.at = value,
                ATTICKSUFFIX => lit.at_tick = value,
                _ => {}
            }
        }
        lit
    }

    fn arg(&mut self, node: &SyntaxNode) -> Arg {
        let span = self.span(node);
        if has_token(node, RANGE) {
            if let Some(r) = child_of(node, RECORDLIT) {
                let fields = children_of(&r, ARG)
                    .filter_map(|f| {
                        let fspan = self.span(&f);
                        let name = self.prop_name(&f);
                        let value = expr_children(&f).next().map(|e| self.expr(&e));
                        match (name, value) {
                            (Some(n), Some(v)) => Some((n, v)),
                            _ => {
                                self.malformed("a spread field without a name and a value", fspan);
                                None
                            }
                        }
                    })
                    .collect();
                return Arg::Spread(Spread::Record(fields, span));
            }
            if let Some(e) = expr_children(node).next() {
                return Arg::Spread(Spread::Expr(self.expr(&e), span));
            }
            return Arg::Rest(span);
        }
        if has_token(node, STAR) && expr_children(node).next().is_none() {
            return Arg::Star(span);
        }
        let name = self.prop_name(node);
        let value = expr_children(node).next().map(|e| self.expr(&e));
        match (name, value) {
            (Some(n), Some(v)) => Arg::Named(n, v),
            (None, Some(v)) => Arg::Pos(v),
            (Some(n), None) => Arg::Pos(Expr {
                kind: ExprKind::Path(vec![n], Vec::new()),
                span: n.span,
            }),
            (None, None) => {
                self.malformed("an empty argument", span);
                Arg::Rest(span)
            }
        }
    }

    /// An argument's name: a plain name, or a property name (dashed, or a string: SUGAR.md §3).
    fn prop_name(&mut self, node: &SyntaxNode) -> Option<Ident> {
        if let Some(p) = child_of(node, PROPNAME) {
            let span = self.span(&p);
            let toks: Vec<SyntaxToken> = tokens(&p).collect();
            let text = match toks.as_slice() {
                [t] if t.kind() == STRING_LIT => self.string(t.text(), span),
                _ => toks.iter().map(|t| t.text()).collect::<String>(),
            };
            return Some(Ident {
                name: Symbol::intern(&text),
                span,
            });
        }
        self.first_name(node)
    }

    /// An element's name: a relation path (`a.b`) or one kind, dashes kept (`font-face`).
    fn elem_name(&mut self, node: &SyntaxNode) -> Vec<Ident> {
        let mut out: Vec<Ident> = Vec::new();
        let mut joined = false;
        for t in tokens(node) {
            let span = self.token_span(&t);
            match t.kind() {
                DOT => joined = false,
                MINUS => joined = true,
                _ => match out.last_mut() {
                    Some(last) if joined => {
                        let text = format!("{}-{}", last.as_str(), t.text());
                        last.name = Symbol::intern(&text);
                        last.span = last.span.to(span).unwrap_or(span);
                        joined = false;
                    }
                    _ => out.push(Ident {
                        name: Symbol::intern(t.text()),
                        span,
                    }),
                },
            }
        }
        out
    }

    fn element(&mut self, node: &SyntaxNode) -> Element {
        let span = self.span(node);
        let name = match child_of(node, ELEMNAME) {
            Some(n) => self.elem_name(&n),
            None => Vec::new(),
        };
        let meta = match child_of(node, META) {
            Some(m) => children_of(&m, ARG).map(|a| self.arg(&a)).collect(),
            None => Vec::new(),
        };
        let args = children_of(node, ARG).map(|a| self.arg(&a)).collect();
        let children = child_of(node, CHILDREN).map(|c| self.children(&c)).unwrap_or_default();
        Element {
            name,
            meta,
            args,
            children,
            span,
        }
    }

    fn children(&mut self, node: &SyntaxNode) -> Vec<Child> {
        let mut out = Vec::new();
        for c in node.children() {
            if let Some(child) = self.child(&c) {
                out.push(child);
            }
        }
        out
    }

    fn child(&mut self, node: &SyntaxNode) -> Option<Child> {
        let span = self.span(node);
        match node.kind() {
            ELEMENT => Some(Child::Element(self.element(node))),
            CONTENT => match expr_children(node).next() {
                Some(e) => Some(Child::Content(self.expr(&e))),
                None => {
                    self.malformed("content without an expression", span);
                    None
                }
            },
            IFCHILD => {
                let Some(b) = child_of(node, BODY) else {
                    self.malformed("an `if` without a condition", span);
                    return None;
                };
                let cond = self.body(&b);
                let blocks: Vec<SyntaxNode> = children_of(node, CHILDREN).collect();
                let then = blocks.first().map(|c| self.children(c)).unwrap_or_default();
                let els = match (blocks.get(1), child_of(node, IFCHILD)) {
                    (Some(c), _) => Some(Box::new(ChildElse::Children(self.children(c)))),
                    (None, Some(i)) => self.child(&i).map(|c| Box::new(ChildElse::If(Box::new(c)))),
                    (None, None) => None,
                };
                Some(Child::If { cond, then, els, span })
            }
            FORCHILD => {
                let Some(b) = child_of(node, BODY) else {
                    self.malformed("a `for` without a condition", span);
                    return None;
                };
                let cond = self.body(&b);
                let children = child_of(node, CHILDREN).map(|c| self.children(&c)).unwrap_or_default();
                Some(Child::For { cond, children, span })
            }
            _ => None,
        }
    }

    fn tree_item(&mut self, node: &SyntaxNode, span: Span) -> Option<TreeDecl> {
        let Some(name) = self.first_name(node) else {
            self.malformed("a tree without a name", span);
            return None;
        };
        let mut roles = Vec::new();
        for r in children_of(node, TREEROLE) {
            let rspan = self.span(&r);
            let names: Vec<Ident> = children_of(&r, NAME).map(|n| self.ident(&n)).collect();
            let rel = child_of(&r, RELPATH).map(|p| self.names(&p)).unwrap_or_default();
            let Some((role, cols)) = names.split_first() else {
                self.malformed("a tree role without a name", rspan);
                continue;
            };
            roles.push(TreeRole {
                role: *role,
                rel,
                cols: cols.to_vec(),
                span: rspan,
            });
        }
        Some(TreeDecl { name, roles, span })
    }

    fn args(&mut self, node: &SyntaxNode) -> Vec<Arg> {
        children_of(node, ARG).map(|a| self.arg(&a)).collect()
    }

    fn need_expr(&mut self, node: &SyntaxNode) -> Expr {
        match expr_children(node).next() {
            Some(e) => self.expr(&e),
            None => {
                let span = self.span(node);
                self.malformed("a missing expression", span);
                Expr {
                    kind: ExprKind::Wildcard,
                    span,
                }
            }
        }
    }

    fn expr(&mut self, node: &SyntaxNode) -> Expr {
        let span = self.span(node);
        let kind = self.expr_kind(node, span);
        Expr { kind, span }
    }

    fn boxed(&mut self, node: Option<SyntaxNode>, span: Span) -> Box<Expr> {
        Box::new(match node {
            Some(n) => self.expr(&n),
            None => {
                self.malformed("a missing operand", span);
                Expr {
                    kind: ExprKind::Wildcard,
                    span,
                }
            }
        })
    }

    fn expr_kind(&mut self, node: &SyntaxNode, span: Span) -> ExprKind {
        match node.kind() {
            LITERALEXPR => match self.literal(node, span) {
                Some(v) => ExprKind::Lit(v),
                None => ExprKind::Wildcard,
            },
            PATHEXPR => {
                let names = self.names(node);
                let args = child_of(node, GENERICARGS)
                    .map(|g| self.generic_args(&g))
                    .unwrap_or_default();
                ExprKind::Path(names, args)
            }
            CALLEXPR => {
                let callee = expr_children(node).next();
                ExprKind::Call {
                    callee: self.boxed(callee, span),
                    args: self.args(node),
                }
            }
            METHODCALLEXPR => {
                let receiver = expr_children(node).next();
                let receiver = self.boxed(receiver, span);
                // A banged method (`d.value!()`) keeps its `!` in the name.
                let name = match tokens(node).find(|t| t.kind() == BANG_IDENT) {
                    Some(t) => Ident {
                        name: Symbol::intern(t.text()),
                        span: self.token_span(&t),
                    },
                    None => self.need_name(node),
                };
                ExprKind::Method {
                    receiver,
                    name,
                    args: self.args(node),
                }
            }
            TRYEXPR => {
                let inner = expr_children(node).next();
                ExprKind::Try(self.boxed(inner, span))
            }
            BANGCALLEXPR => {
                let text = tokens(node)
                    .find(|t| t.kind() == BANG_IDENT)
                    .map(|t| (t.text().trim_end_matches('!').to_owned(), self.token_span(&t)));
                let (text, nspan) = text.unwrap_or_else(|| (String::new(), span));
                let name = Ident {
                    name: Symbol::intern(&text),
                    span: nspan,
                };
                let clauses = children_of(node, BANGCLAUSE).map(|c| self.bang_clause(&c)).collect();
                ExprKind::Bang {
                    name,
                    args: self.args(node),
                    clauses,
                }
            }
            FIELDEXPR => {
                let base = expr_children(node).next();
                let base = self.boxed(base, span);
                ExprKind::Field {
                    base,
                    name: self.need_name(node),
                }
            }
            TUPLEINDEXEXPR => {
                let base = expr_children(node).next();
                let base = self.boxed(base, span);
                let index = tokens(node)
                    .find(|t| t.kind() == INT_LIT)
                    .and_then(|t| t.text().parse::<u32>().ok());
                match index {
                    Some(index) => ExprKind::TupleIndex { base, index },
                    None => {
                        self.malformed("a tuple index that is not a number", span);
                        ExprKind::Wildcard
                    }
                }
            }
            INDEXEXPR => {
                let mut es = expr_children(node);
                let (b, i) = (es.next(), es.next());
                ExprKind::Index {
                    base: self.boxed(b, span),
                    index: self.boxed(i, span),
                }
            }
            BINARYEXPR => {
                let op = tokens(node).find_map(|t| bin_op(t.kind()));
                let mut es = expr_children(node);
                let (l, r) = (es.next(), es.next());
                let lhs = self.boxed(l, span);
                let rhs = self.boxed(r, span);
                match op {
                    Some(op) => ExprKind::Binary { op, lhs, rhs },
                    None => {
                        self.malformed("a binary expression without a known operator", span);
                        ExprKind::Wildcard
                    }
                }
            }
            PREFIXEXPR => {
                let op = tokens(node).find_map(|t| match t.kind() {
                    NOT_KW => Some(PrefixOp::Not),
                    MINUS => Some(PrefixOp::Neg),
                    TILDE => Some(PrefixOp::BitNot),
                    _ => None,
                });
                let arg = expr_children(node).next();
                let arg = self.boxed(arg, span);
                match op {
                    Some(op) => ExprKind::Prefix { op, arg },
                    None => {
                        self.malformed("a prefix expression without a known operator", span);
                        ExprKind::Wildcard
                    }
                }
            }
            CASTEXPR => {
                let e = expr_children(node).next();
                let expr = self.boxed(e, span);
                ExprKind::Cast {
                    expr,
                    ty: self.need_type(node),
                }
            }
            PARENEXPR => match expr_children(node).next() {
                Some(e) => self.expr(&e).kind,
                None => ExprKind::Tuple(Vec::new()),
            },
            TUPLEEXPR => ExprKind::Tuple(expr_children(node).map(|e| self.expr(&e)).collect()),
            VECEXPR => ExprKind::Vec(expr_children(node).map(|e| self.expr(&e)).collect()),
            SETEXPR => ExprKind::Set(expr_children(node).map(|e| self.expr(&e)).collect()),
            MAPEXPR => {
                let es: Vec<Expr> = expr_children(node).map(|e| self.expr(&e)).collect();
                if !es.len().is_multiple_of(2) {
                    self.malformed("a map literal with an odd number of expressions", span);
                    return ExprKind::Wildcard;
                }
                let mut pairs = Vec::new();
                let mut it = es.into_iter();
                while let (Some(k), Some(v)) = (it.next(), it.next()) {
                    pairs.push((k, v));
                }
                ExprKind::Map(pairs)
            }
            IFEXPR => {
                let cond = expr_children(node).next();
                let cond = self.boxed(cond, span);
                let blocks: Vec<SyntaxNode> = children_of(node, BLOCKEXPR).collect();
                let then = match blocks.first() {
                    Some(b) => self.block_expr(b),
                    None => {
                        self.malformed("an `if` expression without a block", span);
                        return ExprKind::Wildcard;
                    }
                };
                // `else { … }` is a second block; `else if …` is a nested `if` expression after the condition.
                let els = match blocks.get(1) {
                    Some(b) => Some(self.block_expr(b)),
                    None => expr_children(node).nth(1).map(|e| self.expr(&e)),
                };
                ExprKind::If {
                    cond,
                    then: Box::new(then),
                    els: els.map(Box::new),
                }
            }
            MATCHEXPR => {
                let scrut = expr_children(node).next();
                let scrut = self.boxed(scrut, span);
                let mut arms = Vec::new();
                for a in children_of(node, MATCHARM) {
                    let aspan = self.span(&a);
                    let mut es: Vec<Expr> = expr_children(&a).map(|e| self.expr(&e)).collect();
                    // A block body follows the pattern and guard.
                    if let Some(block) = child_of(&a, BLOCKEXPR) {
                        es.push(self.block_expr(&block));
                    }
                    let mut it = es.into_iter();
                    match (it.next(), it.next(), it.next()) {
                        (Some(pat), Some(body), None) => arms.push(MatchArm { pat, guard: None, body }),
                        (Some(pat), Some(guard), Some(body)) => arms.push(MatchArm {
                            pat,
                            guard: Some(guard),
                            body,
                        }),
                        _ => self.malformed("a match arm without a pattern and a body", aspan),
                    }
                }
                ExprKind::Match { scrut, arms }
            }
            STRUCTLITEXPR => {
                let path = match child_of(node, PATHEXPR) {
                    Some(p) => self.names(&p),
                    None => self.names(node),
                };
                let mut fields = Vec::new();
                let mut base = None;
                for f in children_of(node, FIELDINIT) {
                    let fspan = self.span(&f);
                    if has_token(&f, RANGE) {
                        match expr_children(&f).next() {
                            Some(e) if base.is_none() => base = Some(Box::new(self.expr(&e))),
                            Some(_) => self.malformed("a struct literal with two `..base`s", fspan),
                            None => self.malformed("`..` without a base", fspan),
                        }
                        continue;
                    }
                    if base.is_some() {
                        self.malformed("a field after `..base` (the base comes last)", fspan);
                        continue;
                    }
                    let name = self.first_name(&f);
                    let value = expr_children(&f).next().map(|e| self.expr(&e));
                    match name {
                        Some(n) => fields.push((n, value)),
                        None => self.malformed("a struct field without a name", fspan),
                    }
                }
                ExprKind::StructLit { path, fields, base }
            }
            FSTRINGEXPR => self.fstring(node, span).kind,
            WILDCARD => ExprKind::Wildcard,
            SELFEXPR => ExprKind::SelfNode,
            FOLDEXPR => {
                self.unsupported("LANG-123", "lattice folds in expressions", span);
                ExprKind::Wildcard
            }
            CLOSUREEXPR => {
                // `|a, b| body`: the parameters are path expressions; the body is a block, or else the last expression.
                let es: Vec<SyntaxNode> = expr_children(node).collect();
                if let Some(block) = child_of(node, BLOCKEXPR) {
                    let names = self.closure_params(&es);
                    return ExprKind::Closure {
                        params: names,
                        body: Box::new(self.block_expr(&block)),
                    };
                }
                let Some((body, params)) = es.split_last() else {
                    self.malformed("a closure without a body", span);
                    return ExprKind::Wildcard;
                };
                let names = self.closure_params(params);
                ExprKind::Closure {
                    params: names,
                    body: Box::new(self.expr(body)),
                }
            }
            other => {
                self.malformed(&format!("expression {other:?}"), span);
                ExprKind::Wildcard
            }
        }
    }

    fn block_expr(&mut self, node: &SyntaxNode) -> Expr {
        let span = self.span(node);
        let mut lets = Vec::new();
        for l in children_of(node, LETLIT) {
            let lspan = self.span(&l);
            let es: Vec<SyntaxNode> = expr_children(&l).collect();
            let ty = child_of(&l, TYPE).map(|t| self.ty(&t));
            match es.as_slice() {
                [pat, value] => lets.push(BlockLet {
                    pat: self.expr(pat),
                    ty,
                    value: self.expr(value),
                    span: lspan,
                }),
                _ => self.malformed("a `let` without a pattern and a value", lspan),
            }
        }
        let result = match expr_children(node).last() {
            Some(e) => self.expr(&e),
            None => Expr {
                kind: ExprKind::Tuple(Vec::new()),
                span,
            },
        };
        if lets.is_empty() {
            return result;
        }
        Expr {
            kind: ExprKind::Block {
                lets,
                result: Box::new(result),
            },
            span,
        }
    }

    /// `extern fn name(params) -> ret = "path";` (LANGUAGE §16.2). Table functions, host types and host lattices
    /// are not built yet.
    fn extern_item(&mut self, node: &SyntaxNode, span: Span) -> Option<ItemKind> {
        let kinds: Vec<SyntaxKind> = tokens(node).map(|t| t.kind()).collect();
        if kinds.contains(&TABLE_KW) {
            return Some(self.unsupported_item("LANG-183", "extern table functions", span));
        }
        if kinds.contains(&TYPE_KW) || kinds.contains(&LATTICE_KW) {
            return Some(self.unsupported_item("LANG-027", "host types and lattices", span));
        }
        let (name, generics, params, ret) = self.fn_signature(node, span)?;
        if !generics.is_empty() {
            return Some(self.unsupported_item("LANG-181", "generic host functions", span));
        }
        let Some(path) = tokens(node).find(|t| t.kind() == STRING_LIT) else {
            self.malformed("an extern function without its host path", span);
            return None;
        };
        let path_span = self.token_span(&path);
        let path = self.string(path.text(), path_span);
        Some(ItemKind::ExternFn(ExternFnItem {
            name,
            params,
            ret,
            path,
            path_span,
            span,
        }))
    }

    /// A function's name, parameters and result type (`fn` and `extern fn` items).
    fn fn_signature(&mut self, node: &SyntaxNode, span: Span) -> Option<FnSig> {
        let Some(sig) = child_of(node, FNSIG) else {
            self.malformed("a function without a signature", span);
            return None;
        };
        let generics = self.generics(&sig);
        if let Some(bound) = generics.iter().flat_map(|g| &g.bounds).next() {
            self.unsupported(
                "LANG-180",
                "bounds and defaults on a function's type parameters",
                bound.span(),
            );
            return None;
        }
        // A class prefix (`monotone fn`, `threshold fn`, …) is a direct token of the item (LANGUAGE §16.1).
        if let Some(class) = tokens(node).find(|t| t.kind() == IDENT) {
            self.unsupported(
                "LANG-182",
                &format!("function classes (`{} fn`)", class.text()),
                self.token_span(&class),
            );
            return None;
        }
        let name = self.need_name(&sig);
        let mut params = Vec::new();
        for p in children_of(&sig, FNPARAM) {
            let pspan = self.span(&p);
            let pat = expr_children(&p).next().map(|e| self.expr(&e));
            let ty = child_of(&p, TYPE).map(|t| self.ty(&t));
            match (pat.map(|e| e.kind), ty) {
                (Some(ExprKind::Path(path, targs)), Some(ty)) if targs.is_empty() && path.len() == 1 => {
                    params.extend(path.into_iter().map(|n| (n, ty.clone())));
                }
                (None, None) => {
                    self.unsupported("LANG-180", "methods (`self` parameters)", pspan);
                    return None;
                }
                _ => {
                    self.malformed("a function parameter that is not `name: Type`", pspan);
                    return None;
                }
            }
        }
        // The return type is the signature's last direct type child.
        let Some(ret) = children_of(&sig, TYPE).last().map(|t| self.ty(&t)) else {
            self.malformed("a function without a return type", span);
            return None;
        };
        Some((name, generics, params, ret))
    }

    /// `format …` (LANGUAGE §16.7).
    fn format_item(&mut self, node: &SyntaxNode, span: Span) -> Option<FormatItem> {
        let name = self.need_name(node);
        let params = children_of(node, FORMATPARAM)
            .map(|p| (self.need_name(&p), child_of(&p, TYPE).map(|t| self.ty(&t))))
            .collect();
        let fields: Vec<SyntaxNode> = children_of(node, FORMATFIELD).collect();
        let body = if has_token(node, EQ) && fields.is_empty() {
            let Some(e) = expr_children(node).next() else {
                self.malformed("a format alias without its element", span);
                return None;
            };
            FormatBody::Alias(self.expr(&e))
        } else {
            let mut out = Vec::new();
            for f in fields {
                let fspan = self.span(&f);
                let Some(elem) = expr_children(&f).next().map(|e| self.expr(&e)) else {
                    self.malformed("a format field without its element", fspan);
                    continue;
                };
                let sub = |this: &mut Self, kind| {
                    child_of(&f, kind)
                        .and_then(|c| expr_children(&c).next())
                        .map(|e| this.expr(&e))
                };
                let cond = sub(self, FORMATCOND);
                let default = sub(self, FORMATDEFAULT);
                out.push(FormatField {
                    name: self.first_name(&f),
                    elem,
                    cond,
                    default,
                    span: fspan,
                });
            }
            FormatBody::Record(out)
        };
        Some(FormatItem {
            name,
            params,
            body,
            span,
        })
    }

    /// `fn name(params) -> ret { body }`.
    fn fn_item(&mut self, node: &SyntaxNode, span: Span) -> Option<FnItem> {
        let (name, generics, params, ret) = self.fn_signature(node, span)?;
        let Some(body) = child_of(node, BLOCKEXPR).map(|b| self.block_expr(&b)) else {
            self.malformed("a function without a body", span);
            return None;
        };
        let mut item = FnItem {
            name,
            generics,
            params,
            ret,
            body,
            span,
            metered: true,
        };
        desugar::fn_body(&mut item, self.diags);
        Some(item)
    }

    fn bang_clause(&mut self, node: &SyntaxNode) -> BangClause {
        let span = self.span(node);
        let keyword = tokens(node)
            .find(|t| t.kind() == IDENT)
            .map(|t| Ident {
                name: Symbol::intern(t.text()),
                span: self.token_span(&t),
            })
            .unwrap_or(Ident {
                name: Symbol::intern(""),
                span,
            });
        let exprs = expr_children(node).map(|e| self.expr(&e)).collect();
        let mut order = Vec::new();
        if let Some(keys) = child_of(node, ORDERKEYS) {
            for k in children_of(&keys, ORDERKEY) {
                let desc = has_word(&k, "desc");
                let kspan = self.span(&k);
                match expr_children(&k).next() {
                    Some(e) => order.push((self.expr(&e), desc)),
                    None => self.malformed("an order key without an expression", kspan),
                }
            }
        }
        BangClause {
            keyword,
            exprs,
            order,
            span,
        }
    }

    fn literal(&mut self, node: &SyntaxNode, span: Span) -> Option<LitValue> {
        let t = tokens(node).next()?;
        let text = t.text();
        match t.kind() {
            INT_LIT => match parse_int(text) {
                Ok((value, suffix)) => Some(LitValue::Int {
                    value,
                    suffix: suffix.map(Symbol::intern),
                }),
                Err(e) => {
                    self.malformed(&e, span);
                    None
                }
            },
            FLOAT_LIT => {
                let digits: String = text.trim_end_matches("f64").chars().filter(|c| *c != '_').collect();
                match digits.parse::<f64>() {
                    Ok(f) => Some(LitValue::Float(f)),
                    Err(_) => {
                        self.malformed("a malformed float literal", span);
                        None
                    }
                }
            }
            DURATION_LIT => match parse_duration(text) {
                Some(ns) => Some(LitValue::Duration(ns)),
                None => {
                    self.malformed("a duration literal that does not fit", span);
                    None
                }
            },
            STRING_LIT => Some(LitValue::Str(self.string(text, span))),
            RAW_STRING_LIT => Some(LitValue::Str(raw_string(text))),
            BYTES_LIT => {
                if text.starts_with("br") {
                    Some(LitValue::Bytes(raw_string(text.trim_start_matches('b')).into_bytes()))
                } else {
                    let s = self.string(text.trim_start_matches('b'), span);
                    Some(LitValue::Bytes(s.into_bytes()))
                }
            }
            TRUE_KW => Some(LitValue::Bool(true)),
            FALSE_KW => Some(LitValue::Bool(false)),
            MOD_LIT => {
                self.unsupported("LANG-026", "modular integer literals", span);
                None
            }
            other => {
                self.malformed(&format!("literal token {other:?}"), span);
                None
            }
        }
    }

    /// `f"a{x}b{y:.2}"` (LANGUAGE §2.4): its text and holes joined with `++`, each hole converted with `to_string` (or,
    /// with a spec `.N`, `to_fixed(N)`): `"a" ++ x.to_string() ++ "b" ++ y.to_fixed(2)`.
    fn fstring(&mut self, node: &SyntaxNode, span: Span) -> Expr {
        let mut parts: Vec<Expr> = Vec::new();
        for el in node.children_with_tokens() {
            if let Some(t) = el.as_token().filter(|t| t.kind() == FSTRING_TEXT) {
                let tspan = self.token_span(t);
                let text = self.fstring_text(t.text(), tspan);
                parts.push(Expr {
                    kind: ExprKind::Lit(LitValue::Str(text)),
                    span: tspan,
                });
            } else if let Some(h) = el.as_node().filter(|h| h.kind() == FSTRINGHOLE) {
                let hspan = self.span(h);
                let Some(e) = expr_children(h).next() else {
                    self.malformed("an interpolation hole without an expression", hspan);
                    continue;
                };
                let value = self.expr(&e);
                let spec = h
                    .children_with_tokens()
                    .filter_map(|x| x.into_token())
                    .find(|t| t.kind() == FSTRING_SPEC);
                let (name, args) = match spec {
                    None => ("to_string", Vec::new()),
                    Some(t) => {
                        let digits = t.text().strip_prefix('.').and_then(|d| d.parse::<u128>().ok());
                        match digits {
                            Some(n) => (
                                "to_fixed",
                                vec![Arg::Pos(Expr {
                                    kind: ExprKind::Lit(LitValue::Int {
                                        value: n,
                                        suffix: Some(Symbol::intern("u64")),
                                    }),
                                    span: self.token_span(&t),
                                })],
                            ),
                            None => {
                                self.diags.push(
                                    Diagnostic::new(
                                        blossom_base::code!("BLS0435"),
                                        format!(
                                            "the format spec `{}`: an interpolation hole takes `.N` (N digits \
                                                 after the point, for an f64)",
                                            t.text()
                                        ),
                                    )
                                    .with_primary(self.token_span(&t)),
                                );
                                ("to_string", Vec::new())
                            }
                        }
                    }
                };
                parts.push(Expr {
                    kind: ExprKind::Method {
                        receiver: Box::new(value),
                        name: Ident {
                            name: Symbol::intern(name),
                            span: hspan,
                        },
                        args,
                    },
                    span: hspan,
                });
            }
        }
        let mut parts = parts.into_iter();
        let Some(first) = parts.next() else {
            return Expr {
                kind: ExprKind::Lit(LitValue::Str(String::new())),
                span,
            };
        };
        // A lone hole is still a String: `f"{n}"` is `"" ++ n.to_string()`'s value, `n.to_string()`.
        let mut out = first;
        for p in parts {
            out = Expr {
                kind: ExprKind::Binary {
                    op: BinOp::Concat,
                    lhs: Box::new(out),
                    rhs: Box::new(p),
                },
                span,
            };
        }
        out.span = span;
        out
    }

    /// An interpolated string's text run: its escapes decoded, `{{` and `}}` as single braces.
    fn fstring_text(&mut self, text: &str, span: Span) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(c) = rest.chars().next() {
            if let Some(r) = rest.strip_prefix("{{") {
                out.push('{');
                rest = r;
            } else if let Some(r) = rest.strip_prefix("}}") {
                out.push('}');
                rest = r;
            } else if c == '\\' {
                // One escape: `\u{…}` up to its `}`, any other two characters.
                let len = if rest.starts_with("\\u{") {
                    rest.find('}').map_or(rest.len(), |i| i + 1)
                } else {
                    rest.chars().take(2).map(char::len_utf8).sum()
                };
                let (esc, r) = rest.split_at_checked(len).unwrap_or((rest, ""));
                out.push_str(&self.string(&format!("\"{esc}\""), span));
                rest = r;
            } else {
                out.push(c);
                rest = rest.get(c.len_utf8()..).unwrap_or("");
            }
        }
        out
    }

    /// The value of a (lexically valid) quoted string.
    fn string(&mut self, text: &str, span: Span) -> String {
        let inner = text.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(text);
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('0') => out.push('\0'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('u') => {
                    let hex: String = chars.by_ref().skip(1).take_while(|c| *c != '}').collect();
                    match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        Some(ch) => out.push(ch),
                        None => self.malformed("an invalid Unicode escape", span),
                    }
                }
                _ => self.malformed("an unknown string escape", span),
            }
        }
        out
    }
}

fn raw_string(text: &str) -> String {
    let t = text.trim_start_matches('r');
    let hashes = t.chars().take_while(|c| *c == '#').count();
    let t = t.get(hashes..).unwrap_or("");
    let t = t.strip_prefix('"').unwrap_or(t);
    let end = t.len().saturating_sub(1 + hashes);
    t.get(..end).unwrap_or("").to_owned()
}

fn bin_op(kind: SyntaxKind) -> Option<BinOp> {
    Some(match kind {
        PLUS => BinOp::Add,
        MINUS => BinOp::Sub,
        STAR => BinOp::Mul,
        SLASH => BinOp::Div,
        PERCENT => BinOp::Rem,
        CONCAT => BinOp::Concat,
        EQ2 => BinOp::Eq,
        NEQ => BinOp::Ne,
        LT => BinOp::Lt,
        LE => BinOp::Le,
        GT => BinOp::Gt,
        GE => BinOp::Ge,
        AND2 => BinOp::And,
        OR2 => BinOp::Or,
        AMP => BinOp::BitAnd,
        PIPE => BinOp::BitOr,
        CARET => BinOp::BitXor,
        SHL => BinOp::Shl,
        SHR => BinOp::Shr,
        IN_KW => BinOp::In,
        RANGE => BinOp::Range,
        RANGE_EQ => BinOp::RangeEq,
        OPEN_RANGE => BinOp::OpenRange,
        OPEN_RANGE_EQ => BinOp::OpenRangeEq,
        _ => return None,
    })
}

const INT_SUFFIXES: [&str; 10] = ["u128", "u16", "u32", "u64", "u8", "i128", "i16", "i32", "i64", "i8"];

/// An integer literal's value and suffix.
fn parse_int(text: &str) -> Result<(u128, Option<&'static str>), String> {
    let mut body = text;
    let mut suffix = None;
    for s in INT_SUFFIXES {
        if let Some(b) = text.strip_suffix(s)
            && !b.is_empty()
        {
            body = b;
            suffix = Some(s);
            break;
        }
    }
    let digits: String = body.chars().filter(|c| *c != '_').collect();
    let parsed = if let Some(h) = digits.strip_prefix("0x") {
        u128::from_str_radix(h, 16)
    } else if let Some(b) = digits.strip_prefix("0b") {
        u128::from_str_radix(b, 2)
    } else {
        digits.parse::<u128>()
    };
    parsed
        .map(|v| (v, suffix))
        .map_err(|_| format!("an integer literal `{text}` that does not fit in 128 bits"))
}

/// A duration literal in nanoseconds.
fn parse_duration(text: &str) -> Option<u128> {
    let split = text.find(|c: char| c.is_ascii_alphabetic())?;
    let (num, unit) = text.split_at(split);
    let n: u128 = num.chars().filter(|c| *c != '_').collect::<String>().parse().ok()?;
    let scale: u128 = match unit {
        "ns" => 1,
        "us" => 1_000,
        "ms" => 1_000_000,
        "s" => 1_000_000_000,
        "m" => 60_000_000_000,
        "h" => 3_600_000_000_000,
        "d" => 86_400_000_000_000,
        _ => return None,
    };
    n.checked_mul(scale)
}

impl Cx<'_> {
    fn spec(&mut self, node: &SyntaxNode) -> SpecItem {
        let span = self.span(node);
        let names: Vec<Ident> = self.names(node);
        let has_for = has_token(node, FOR_KW);
        let name = names.first().copied();
        let target = if has_for { names.get(1).map(|n| vec![*n]) } else { None };
        let mut members = Vec::new();
        let mut member_attrs = Vec::new();
        let mut target_args = Vec::new();
        for c in node.children() {
            let cspan = self.span(&c);
            member_attrs.extend(self.attrs(&c));
            let m = match c.kind() {
                NAME => continue,
                ARG if has_for => {
                    target_args.push(self.arg(&c));
                    continue;
                }
                NODESMEMBER => SpecMember::Nodes(self.names(&c)),
                ASSIGNMEMBER => {
                    let names = self.names(&c);
                    let Some((role, nodes)) = names.split_first() else {
                        self.malformed("an `assign` without a role", cspan);
                        continue;
                    };
                    SpecMember::Assign {
                        role: *role,
                        nodes: nodes.to_vec(),
                        span: cspan,
                    }
                }
                FAULTSMEMBER => SpecMember::Faults(self.opt_block(&c), cspan),
                INCLUDEITEM => SpecMember::Include(self.names(&c), cspan),
                CHECKMEMBER => {
                    let names = self.names(&c);
                    let Some(kind) = names.first().copied() else {
                        self.malformed("a `check` without a kind", cspan);
                        continue;
                    };
                    let expect = if has_word(&c, "expect") {
                        names.last().copied().filter(|_| names.len() > 1)
                    } else {
                        None
                    };
                    SpecMember::Check {
                        kind,
                        options: self.opt_block(&c),
                        expect,
                        span: cspan,
                    }
                }
                FACTITEM => SpecMember::Fact(self.fact(&c)),
                VIEWDECL => SpecMember::View(self.view(&c)),
                INVARIANTITEM => SpecMember::Invariant(self.invariant(&c)),
                CONSTITEM => SpecMember::Const {
                    name: self.need_name(&c),
                    ty: self.need_type(&c),
                    value: self.need_expr(&c),
                },
                // Checked by tools this build does not have; the spec compiler reports them as not run.
                LIVENESSMEMBER => SpecMember::Unsupported {
                    what: "liveness",
                    span: cspan,
                },
                PROVEMEMBER => SpecMember::Unsupported {
                    what: "prove",
                    span: cspan,
                },
                EXPECTMEMBER => SpecMember::Unsupported {
                    what: "expect",
                    span: cspan,
                },
                other => {
                    self.malformed(&format!("spec member {other:?}"), cspan);
                    continue;
                }
            };
            members.push(m);
        }
        SpecItem {
            name,
            target,
            target_args,
            members,
            member_attrs,
            span,
        }
    }

    fn opt_block(&mut self, node: &SyntaxNode) -> Vec<(Ident, Expr)> {
        let Some(b) = child_of(node, OPTBLOCK) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for f in children_of(&b, OPTFIELD) {
            let name = self.need_name(&f);
            let value = self.need_expr(&f);
            out.push((name, value));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_duration, parse_int};

    #[test]
    fn integers() {
        assert_eq!(parse_int("42"), Ok((42, None)));
        assert_eq!(parse_int("1_000u64"), Ok((1000, Some("u64"))));
        assert_eq!(parse_int("0xffu8"), Ok((255, Some("u8"))));
        assert_eq!(parse_int("0b101"), Ok((5, None)));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("1s"), Some(1_000_000_000));
        assert_eq!(parse_duration("500ms"), Some(500_000_000));
        assert_eq!(parse_duration("2m"), Some(120_000_000_000));
    }
}
