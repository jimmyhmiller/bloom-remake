//! The S16 sugar (docs/design/SUGAR.md): child heads, spreads and tree literals, expanded into plain statements and
//! `if`/`for` blocks before they are resolved. Nothing here reaches the IR: every row a sugared statement writes is
//! an ordinary statement of the same verb, in the block the sugar sits in.

use std::collections::BTreeMap;

use blossom_base::{Span, Symbol, code};
use blossom_value::types::TypeDef;

use super::Resolver;
use super::body::RuleCx;
use crate::ast::{
    Arg, AtomLit, BinOp, Block, Body, Child, ChildElse, Element, Expr, ExprKind, FragmentItem, Head, Ident, Lit,
    LitValue, Spread, Stmt, VerbStmt,
};
use crate::hir::HRelId;

/// A `tree` declaration, resolved: each role's relation (by the path it was declared with) and the positions of its
/// columns in the role's order.
#[derive(Clone, Debug)]
pub(crate) struct TreeInfo {
    pub node: Role,
    pub props: Option<Role>,
    pub content: Option<Role>,
}

#[derive(Clone, Debug)]
pub(crate) struct Role {
    pub path: Vec<Ident>,
    pub rel: HRelId,
    /// The relation's column for each of the role's places (node: id, parent, position, kind; props: id, name,
    /// value; content: id, value).
    pub cols: Vec<usize>,
}

/// An enclosing head of a child head: its relation's columns, by name, with the expression each was given.
type Given = BTreeMap<Symbol, (Option<blossom_base::TypeId>, Expr)>;

/// Where an element sits while a tree is expanded.
struct Place {
    /// The parent's id (the mount point's: `""`).
    parent: Expr,
    /// The path of ordinals from the root, for derived ids and statement tags.
    path: String,
    /// Inside a `for` block (an element there needs an id or a key, BLS0430).
    in_for: bool,
}

fn lit_str(s: &str, span: Span) -> Expr {
    Expr {
        kind: ExprKind::Lit(LitValue::Str(s.to_owned())),
        span,
    }
}

/// `a ++ b`, joining two string literals at once.
fn concat(a: Expr, b: Expr, span: Span) -> Expr {
    match (&a.kind, &b.kind) {
        (ExprKind::Lit(LitValue::Str(x)), ExprKind::Lit(LitValue::Str(y))) => lit_str(&format!("{x}{y}"), span),
        _ => Expr {
            kind: ExprKind::Binary {
                op: BinOp::Concat,
                lhs: Box::new(a),
                rhs: Box::new(b),
            },
            span,
        },
    }
}

/// `e.to_string()` (a string literal as it is).
fn to_string(e: Expr) -> Expr {
    if matches!(e.kind, ExprKind::Lit(LitValue::Str(_))) {
        return e;
    }
    let span = e.span;
    Expr {
        kind: ExprKind::Method {
            receiver: Box::new(e),
            name: Ident {
                name: Symbol::intern("to_string"),
                span,
            },
            args: Vec::new(),
        },
        span,
    }
}

fn path_expr(name: &str, span: Span) -> Expr {
    Expr {
        kind: ExprKind::Path(
            vec![Ident {
                name: Symbol::intern(name),
                span,
            }],
            Vec::new(),
        ),
        span,
    }
}

impl<'t> Resolver<'t, '_> {
    /// Whether a statement uses the S16 sugar.
    pub(super) fn sugared(v: &VerbStmt) -> bool {
        v.tree.is_some() || !v.children.is_empty() || v.head.args.iter().any(|a| matches!(a, Arg::Spread(_)))
    }

    /// A sugared statement as plain statements (SUGAR.md §§2, 3, 5); `None` after reporting an error.
    pub(super) fn expand(&mut self, cx: &RuleCx, v: &VerbStmt) -> Option<Vec<Stmt>> {
        if let Some(root) = &v.tree {
            let info = self.tree_named(cx, &v.head.rel, v.head.span)?;
            let mut out = Vec::new();
            let mut ordinal = 0;
            let place = Place {
                parent: lit_str("", v.head.span),
                path: String::new(),
                in_for: false,
            };
            self.element(cx, v, &info, root, &place, &mut ordinal, &mut out)?;
            return Some(out);
        }
        let mut out = Vec::new();
        self.child_head(cx, v, &v.head, &v.children, &[], &mut out, "")?;
        Some(out)
    }

    /// A plain statement of `v`'s verb writing `head`, tagged so its rule's label is its own.
    fn stmt_of(v: &VerbStmt, head: Head, span: Span, tag: String) -> Stmt {
        Stmt::Verb(Box::new(VerbStmt {
            attrs: v.attrs.clone(),
            verb: v.verb,
            head,
            to: v.to.clone(),
            weight: v.weight.clone(),
            resolve: v.resolve.clone(),
            children: Vec::new(),
            tree: None,
            tag: Some(tag),
            span,
        }))
    }

    /// The statements of one head (with its spread expanded) and of its children, each child inheriting by name what
    /// it does not give from its enclosing heads (nearest first).
    #[allow(clippy::too_many_arguments)]
    fn child_head(
        &mut self,
        cx: &RuleCx,
        v: &VerbStmt,
        head: &Head,
        children: &[Child],
        ancestors: &[Given],
        out: &mut Vec<Stmt>,
        tag: &str,
    ) -> Option<()> {
        let rel = self.lookup_rel(cx.ms, &head.rel);
        let Some(rel) = rel else {
            // `verb_stmt` reports the unknown relation, with its own wording.
            out.push(Self::stmt_of(v, head.clone(), head.span, format!("{tag}head")));
            return Some(());
        };
        let cols: Vec<(Symbol, Option<blossom_base::TypeId>)> =
            self.rel_of(rel).cols.iter().map(|c| (c.name, c.ty)).collect();
        // What this head gives, by column: positional arguments in order, named ones by name.
        let mut given = Given::new();
        let mut args = Vec::new();
        let mut spread = None;
        for (i, a) in head.args.iter().enumerate() {
            match a {
                Arg::Pos(e) => {
                    if let Some((name, ty)) = cols.get(i) {
                        given.insert(*name, (*ty, e.clone()));
                    }
                    args.push(a.clone());
                }
                Arg::Named(n, e) => {
                    if let Some((_, ty)) = cols.iter().find(|(c, _)| *c == n.name) {
                        given.insert(n.name, (*ty, e.clone()));
                    }
                    args.push(a.clone());
                }
                Arg::Spread(s) if i + 1 == head.args.len() => spread = Some(s),
                Arg::Spread(_) => {
                    self.error(code!("BLS0303"), a.span(), "a spread is a head's last argument");
                    return None;
                }
                Arg::Rest(_) | Arg::Star(_) => args.push(a.clone()),
            }
        }
        // Inherited columns: a child head is given what it lacks from its ancestors, by name and type.
        if !ancestors.is_empty() {
            let named = head.args.iter().any(|a| matches!(a, Arg::Named(..)));
            if !named && head.args.iter().any(|a| matches!(a, Arg::Pos(_))) {
                self.error(
                    code!("BLS0303"),
                    head.span,
                    "a child head names its arguments (`col: value`): the others come from its parents",
                );
                return None;
            }
            let spread_cols = if spread.is_some() { 2 } else { 0 };
            for (c, ty) in cols.iter().take(cols.len().saturating_sub(spread_cols)) {
                if given.contains_key(c) {
                    continue;
                }
                if let Some((_, e)) = ancestors.iter().rev().find_map(|g| g.get(c).filter(|(t, _)| t == ty)) {
                    let span = head.span;
                    args.push(Arg::Named(Ident { name: *c, span }, e.clone()));
                    given.insert(*c, (*ty, e.clone()));
                }
            }
        }
        match spread {
            None => out.push(Self::stmt_of(
                v,
                Head {
                    rel: head.rel.clone(),
                    args,
                    span: head.span,
                },
                head.span,
                format!("{tag}head"),
            )),
            Some(s) => self.spread(cx, v, head, rel, args, s, out, tag)?,
        }
        if children.is_empty() {
            return Some(());
        }
        let mut chain = ancestors.to_vec();
        chain.push(given);
        let stmts = self.child_heads(cx, v, children, &chain, tag)?;
        out.extend(stmts);
        Some(())
    }

    fn child_heads(
        &mut self,
        cx: &RuleCx,
        v: &VerbStmt,
        children: &[Child],
        chain: &[Given],
        tag: &str,
    ) -> Option<Vec<Stmt>> {
        let mut out = Vec::new();
        for (i, c) in children.iter().enumerate() {
            let ctag = format!("{tag}{i}/");
            match c {
                Child::Element(e) => {
                    if !e.meta.is_empty() {
                        self.error(
                            code!("BLS0303"),
                            e.span,
                            "`[…]` belongs to a tree element, not a child head",
                        );
                        return None;
                    }
                    let head = Head {
                        rel: e.name.clone(),
                        args: e.args.clone(),
                        span: e.span,
                    };
                    self.child_head(cx, v, &head, &e.children, chain, &mut out, &ctag)?;
                }
                Child::If { cond, then, els, span } => {
                    let then = self.child_heads(cx, v, then, chain, &format!("{ctag}then/"))?;
                    let els = match els.as_deref() {
                        None => None,
                        Some(ChildElse::Children(cs)) => Some(Box::new(crate::ast::Else::Block(Block {
                            stmts: self.child_heads(cx, v, cs, chain, &format!("{ctag}else/"))?,
                            span: *span,
                        }))),
                        Some(ChildElse::If(c)) => {
                            let mut inner =
                                self.child_heads(cx, v, std::slice::from_ref(&**c), chain, &format!("{ctag}else/"))?;
                            match inner.pop() {
                                Some(s @ Stmt::If { .. }) if inner.is_empty() => {
                                    Some(Box::new(crate::ast::Else::If(Box::new(s))))
                                }
                                _ => None,
                            }
                        }
                    };
                    out.push(Stmt::If {
                        attrs: Vec::new(),
                        cond: cond.clone(),
                        then: Block {
                            stmts: then,
                            span: *span,
                        },
                        els,
                        span: *span,
                    });
                }
                Child::For { cond, children, span } => {
                    let inner = self.child_heads(cx, v, children, chain, &ctag)?;
                    out.push(Stmt::For {
                        attrs: Vec::new(),
                        cond: cond.clone(),
                        block: Block {
                            stmts: inner,
                            span: *span,
                        },
                        span: *span,
                    });
                }
                Child::Content(e) => {
                    self.error(
                        code!("BLS0303"),
                        e.span,
                        "content belongs to a tree element, not a child head",
                    );
                    return None;
                }
                Child::Stmt(s) => out.push((**s).clone()),
            }
        }
        Some(out)
    }

    /// A head ending in a spread: its relation's last two columns (a name, a value) from each field of a record, or
    /// from each entry of a map (a `for` over it).
    #[allow(clippy::too_many_arguments)]
    fn spread(
        &mut self,
        cx: &RuleCx,
        v: &VerbStmt,
        head: &Head,
        rel: HRelId,
        args: Vec<Arg>,
        s: &Spread,
        out: &mut Vec<Stmt>,
        tag: &str,
    ) -> Option<()> {
        let _ = cx;
        let cols = self.rel_of(rel).cols.clone();
        let n = cols.len();
        let (Some(name_col), Some(value_col)) = (n.checked_sub(2).and_then(|i| cols.get(i)), cols.last()) else {
            self.error(
                code!("BLS0303"),
                head.span,
                "a spread fills a relation's last two columns (a name, a value)",
            );
            return None;
        };
        if !matches!(name_col.ty.and_then(|t| self.hir.types.get(t)), Some(TypeDef::Str)) {
            self.error(
                code!("BLS0303"),
                head.span,
                format!(
                    "a spread names its rows: the column `{}` is not a String",
                    name_col.name
                ),
            );
            return None;
        }
        let string_value = matches!(value_col.ty.and_then(|t| self.hir.types.get(t)), Some(TypeDef::Str));
        // The other arguments give the columns before the spread's two.
        let mut lead = Vec::new();
        for a in &args {
            match a {
                Arg::Named(c, _) if c.name == name_col.name || c.name == value_col.name => {
                    self.error(code!("BLS0303"), a.span(), "the spread gives this column");
                    return None;
                }
                Arg::Named(..) => lead.push(a.clone()),
                Arg::Pos(e) => lead.push(Arg::Pos(e.clone())),
                _ => lead.push(a.clone()),
            }
        }
        let named = lead.iter().any(|a| matches!(a, Arg::Named(..)));
        let fill = |key: Expr, value: Expr| -> Vec<Arg> {
            let mut out = lead.clone();
            let value = if string_value { to_string(value) } else { value };
            if named {
                out.push(Arg::Named(
                    Ident {
                        name: name_col.name,
                        span: key.span,
                    },
                    key,
                ));
                out.push(Arg::Named(
                    Ident {
                        name: value_col.name,
                        span: value.span,
                    },
                    value,
                ));
            } else {
                out.push(Arg::Pos(key));
                out.push(Arg::Pos(value));
            }
            out
        };
        match s {
            Spread::Record(fields, _) => {
                for (f, e) in fields {
                    let head = Head {
                        rel: head.rel.clone(),
                        args: fill(lit_str(f.as_str(), f.span), e.clone()),
                        span: head.span,
                    };
                    out.push(Self::stmt_of(v, head, f.span, format!("{tag}spread:{}", f.as_str())));
                }
            }
            Spread::Expr(m, span) => {
                // `for (k, v) in m { … }`: names no program can write, so they capture nothing.
                let (k, val) = (format!("k$spread{}", span.lo), format!("v$spread{}", span.lo));
                let pattern = Expr {
                    kind: ExprKind::Tuple(vec![path_expr(&k, *span), path_expr(&val, *span)]),
                    span: *span,
                };
                let generator = Expr {
                    kind: ExprKind::Binary {
                        op: BinOp::In,
                        lhs: Box::new(pattern),
                        rhs: Box::new(m.clone()),
                    },
                    span: *span,
                };
                let head = Head {
                    rel: head.rel.clone(),
                    args: fill(path_expr(&k, *span), path_expr(&val, *span)),
                    span: head.span,
                };
                out.push(Stmt::For {
                    attrs: Vec::new(),
                    cond: Body {
                        lits: vec![Lit::Plain(AtomLit {
                            expr: generator,
                            from: None,
                            principal: None,
                            weight: None,
                            at: None,
                            at_tick: None,
                            span: *span,
                        })],
                        guards: Vec::new(),
                        span: *span,
                    },
                    block: Block {
                        stmts: vec![Self::stmt_of(v, head, *span, format!("{tag}spread"))],
                        span: *span,
                    },
                    span: *span,
                });
            }
        }
        Some(())
    }

    /// The tree a statement names (declared in this module, SUGAR.md §3).
    fn tree_named(&mut self, cx: &RuleCx, path: &[Ident], span: Span) -> Option<TreeInfo> {
        let found = match path {
            [name] => self.scope(cx.ms).trees.get(&name.name).cloned(),
            _ => None,
        };
        if found.is_none() {
            let name: Vec<&str> = path.iter().map(Ident::as_str).collect();
            self.error(code!("BLS0200"), span, format!("no tree `{}`", name.join(".")));
        }
        found
    }

    /// One element of a tree, and its subtree: its node row, a props row per property, its content row; its children
    /// with their slots (an `if`'s elements in line, a `for`'s once each).
    #[allow(clippy::too_many_arguments)]
    fn element(
        &mut self,
        cx: &RuleCx,
        v: &VerbStmt,
        info: &TreeInfo,
        e: &Element,
        place: &Place,
        ordinal: &mut u64,
        out: &mut Vec<Stmt>,
    ) -> Option<()> {
        let slot = *ordinal;
        *ordinal += 1;
        let [kind] = e.name.as_slice() else {
            self.error(
                code!("BLS0303"),
                e.span,
                "a tree element's kind is one name (`rect`, `font-face`)",
            );
            return None;
        };
        let (mut id, mut key, mut pos) = (None, None, None);
        for m in &e.meta {
            match m {
                Arg::Named(n, x) if n.as_str() == "id" && id.is_none() => id = Some(x.clone()),
                Arg::Named(n, x) if n.as_str() == "key" && key.is_none() => key = Some(x.clone()),
                Arg::Named(n, x) if n.as_str() == "pos" && pos.is_none() => pos = Some(x.clone()),
                other => {
                    self.error(
                        code!("BLS0303"),
                        other.span(),
                        "an element's `[…]` takes `id:`, `key:` and `pos:`, each once",
                    );
                    return None;
                }
            }
        }
        if place.in_for && id.is_none() && key.is_none() {
            self.error(
                code!("BLS0430"),
                e.span,
                "an element inside a `for` needs an id or a key: its rows would repeat one id",
            );
            return None;
        }
        if id.is_some() && key.is_some() {
            self.error(code!("BLS0431"), e.span, "an element with an id takes no key");
            return None;
        }
        let path = format!("{}/{}.{slot}", place.path, kind.as_str());
        let id = match id {
            Some(x) => to_string(x),
            None => {
                let mut d = concat(
                    place.parent.clone(),
                    lit_str(&format!("/{}.{slot}", kind.as_str()), e.span),
                    e.span,
                );
                if let Some(k) = key {
                    d = concat(d, lit_str("[", e.span), e.span);
                    d = concat(d, to_string(k), e.span);
                    d = concat(d, lit_str("]", e.span), e.span);
                }
                d
            }
        };
        let pos = match pos {
            Some(p) => p,
            None => {
                let ty = self
                    .rel_of(info.node.rel)
                    .cols
                    .get(info.node.cols.get(2).copied().unwrap_or(0))
                    .and_then(|c| c.ty);
                let suffix = match ty.and_then(|t| self.hir.types.get(t)) {
                    Some(TypeDef::Int(it)) => Some(Symbol::intern(it.name())),
                    _ => None,
                };
                Expr {
                    kind: ExprKind::Lit(LitValue::Int {
                        value: u128::from(slot),
                        suffix,
                    }),
                    span: e.span,
                }
            }
        };
        let node_args = self.role_args(
            &info.node,
            vec![id.clone(), place.parent.clone(), pos, lit_str(kind.as_str(), kind.span)],
        );
        out.push(Self::stmt_of(
            v,
            Head {
                rel: info.node.path.clone(),
                args: node_args,
                span: e.span,
            },
            e.span,
            format!("{path}:node"),
        ));
        // Properties: one row each; a spread's fields too.
        for a in &e.args {
            let rows: Vec<(Ident, Expr)> = match a {
                Arg::Named(n, x) => vec![(*n, x.clone())],
                Arg::Spread(Spread::Record(fields, _)) => fields.clone(),
                Arg::Spread(Spread::Expr(..)) => {
                    self.unsupported("LANG-084", "a map spread among a tree element's properties", a.span());
                    return None;
                }
                other => {
                    self.error(
                        code!("BLS0303"),
                        other.span(),
                        "a tree element's properties are named (`width: 10`)",
                    );
                    return None;
                }
            };
            let Some(props) = &info.props else {
                self.error(code!("BLS0434"), a.span(), "this tree declares no `props` relation");
                return None;
            };
            for (n, x) in rows {
                let string_value = self.string_col(props, 2);
                let value = if string_value { to_string(x) } else { x };
                let args = self.role_args(props, vec![id.clone(), lit_str(n.as_str(), n.span), value]);
                out.push(Self::stmt_of(
                    v,
                    Head {
                        rel: props.path.clone(),
                        args,
                        span: n.span,
                    },
                    n.span,
                    format!("{path}:prop:{}", n.as_str()),
                ));
            }
        }
        // Children: elements by slot, `if`/`for` blocks around theirs, and at most one content.
        // Below an element its id tells its descendants apart: inside a `for`, it has an id or a key of its own.
        let inner = Place {
            parent: id.clone(),
            path,
            in_for: false,
        };
        let mut child_ordinal = 0;
        let mut content_seen = false;
        let stmts = self.tree_children(cx, v, info, &e.children, &inner, &mut child_ordinal, &mut content_seen)?;
        out.extend(stmts);
        Some(())
    }

    #[allow(clippy::too_many_arguments)]
    fn tree_children(
        &mut self,
        cx: &RuleCx,
        v: &VerbStmt,
        info: &TreeInfo,
        children: &[Child],
        place: &Place,
        ordinal: &mut u64,
        content_seen: &mut bool,
    ) -> Option<Vec<Stmt>> {
        let mut out = Vec::new();
        for c in children {
            match c {
                Child::Element(e) if self.fragment_named(cx, e).is_some() => {
                    let Some(frag) = self.fragment_named(cx, e) else {
                        continue;
                    };
                    if v.to.is_some() {
                        self.unsupported("LANG-084", "a fragment call in a tree statement with `to`", e.span);
                        return None;
                    }
                    if self.fragments_expanding.contains(&frag.name.name) {
                        self.error(
                            code!("BLS0433"),
                            e.span,
                            format!("the fragment `{}` calls itself", frag.name.as_str()),
                        );
                        return None;
                    }
                    // The call's elements are the enclosing element's children: its id is a hidden parameter.
                    let hidden = format!("parent$frag@{}", e.span.lo);
                    let inner = Place {
                        parent: path_expr(&hidden, e.span),
                        path: place.path.clone(),
                        in_for: place.in_for,
                    };
                    self.fragments_expanding.push(frag.name.name);
                    let body = self.tree_children(cx, v, info, &frag.body, &inner, ordinal, content_seen);
                    self.fragments_expanding.pop();
                    let body = body?;
                    out.push(self.fragment_block(cx, frag, e, vec![(hidden, place.parent.clone())], body)?);
                }
                Child::Element(e) => self.element(cx, v, info, e, place, ordinal, &mut out)?,
                Child::Stmt(s) => out.push((**s).clone()),
                Child::Content(x) => {
                    let Some(content) = &info.content else {
                        self.error(code!("BLS0434"), x.span, "this tree declares no `content` relation");
                        return None;
                    };
                    if *content_seen {
                        self.error(code!("BLS0303"), x.span, "an element has one content");
                        return None;
                    }
                    *content_seen = true;
                    let value = if self.string_col(content, 1) {
                        to_string(x.clone())
                    } else {
                        x.clone()
                    };
                    let args = self.role_args(content, vec![place.parent.clone(), value]);
                    out.push(Self::stmt_of(
                        v,
                        Head {
                            rel: content.path.clone(),
                            args,
                            span: x.span,
                        },
                        x.span,
                        format!("{}:content", place.path),
                    ));
                }
                Child::If { cond, then, els, span } => {
                    let then = self.tree_children(cx, v, info, then, place, ordinal, content_seen)?;
                    let els = match els.as_deref() {
                        None => None,
                        Some(ChildElse::Children(cs)) => Some(Box::new(crate::ast::Else::Block(Block {
                            stmts: self.tree_children(cx, v, info, cs, place, ordinal, content_seen)?,
                            span: *span,
                        }))),
                        Some(ChildElse::If(c)) => {
                            let mut inner = self.tree_children(
                                cx,
                                v,
                                info,
                                std::slice::from_ref(&**c),
                                place,
                                ordinal,
                                content_seen,
                            )?;
                            match inner.pop() {
                                Some(s @ Stmt::If { .. }) if inner.is_empty() => {
                                    Some(Box::new(crate::ast::Else::If(Box::new(s))))
                                }
                                _ => None,
                            }
                        }
                    };
                    out.push(Stmt::If {
                        attrs: Vec::new(),
                        cond: cond.clone(),
                        then: Block {
                            stmts: then,
                            span: *span,
                        },
                        els,
                        span: *span,
                    });
                }
                Child::For { cond, children, span } => {
                    let inner_place = Place {
                        parent: place.parent.clone(),
                        path: place.path.clone(),
                        in_for: true,
                    };
                    let inner = self.tree_children(cx, v, info, children, &inner_place, ordinal, content_seen)?;
                    out.push(Stmt::For {
                        attrs: Vec::new(),
                        cond: cond.clone(),
                        block: Block {
                            stmts: inner,
                            span: *span,
                        },
                        span: *span,
                    });
                }
            }
        }
        Some(out)
    }

    /// The fragment an element (or a call statement) names, if it names one.
    fn fragment_named(&self, cx: &RuleCx, e: &Element) -> Option<&'t FragmentItem> {
        match e.name.as_slice() {
            [name] => self.scope(cx.ms).fragments.get(&name.name).copied(),
            _ => None,
        }
    }

    /// `frag(args);` among statements: the fragment's statements in a block of their own (SUGAR.md §4).
    pub(super) fn fragment_call(&mut self, cx: &RuleCx, e: &Element) -> Option<Stmt> {
        let Some(frag) = self.fragment_named(cx, e) else {
            let name: Vec<&str> = e.name.iter().map(Ident::as_str).collect();
            self.error(code!("BLS0200"), e.span, format!("no fragment `{}`", name.join(".")));
            return None;
        };
        if !e.meta.is_empty() || !e.children.is_empty() {
            self.error(
                code!("BLS0303"),
                e.span,
                "a fragment call takes arguments only: `name(args);`",
            );
            return None;
        }
        let body = self.fragment_stmts(cx, &frag.body)?;
        self.fragment_block(cx, frag, e, Vec::new(), body)
    }

    /// A fragment's items as statements: a call outside a tree has no element to put tree elements under (BLS0432).
    fn fragment_stmts(&mut self, cx: &RuleCx, items: &[Child]) -> Option<Vec<Stmt>> {
        let mut out = Vec::new();
        for c in items {
            match c {
                Child::Stmt(s) => out.push((**s).clone()),
                Child::Element(e) if self.fragment_named(cx, e).is_some() => out.push(Stmt::Call(e.clone())),
                Child::Element(e) => {
                    self.error(
                        code!("BLS0432"),
                        e.span,
                        "a fragment's elements need a tree around the call (`emit html … { call(…); }`)",
                    );
                    return None;
                }
                Child::If { cond, then, els, span } => {
                    let then = self.fragment_stmts(cx, then)?;
                    let els = match els.as_deref() {
                        None => None,
                        Some(ChildElse::Children(cs)) => Some(Box::new(crate::ast::Else::Block(Block {
                            stmts: self.fragment_stmts(cx, cs)?,
                            span: *span,
                        }))),
                        Some(ChildElse::If(c)) => {
                            let mut inner = self.fragment_stmts(cx, std::slice::from_ref(&**c))?;
                            match inner.pop() {
                                Some(s @ Stmt::If { .. }) if inner.is_empty() => {
                                    Some(Box::new(crate::ast::Else::If(Box::new(s))))
                                }
                                _ => None,
                            }
                        }
                    };
                    out.push(Stmt::If {
                        attrs: Vec::new(),
                        cond: cond.clone(),
                        then: Block {
                            stmts: then,
                            span: *span,
                        },
                        els,
                        span: *span,
                    });
                }
                Child::For { cond, children, span } => {
                    let inner = self.fragment_stmts(cx, children)?;
                    out.push(Stmt::For {
                        attrs: Vec::new(),
                        cond: cond.clone(),
                        block: Block {
                            stmts: inner,
                            span: *span,
                        },
                        span: *span,
                    });
                }
                Child::Content(x) => {
                    self.error(
                        code!("BLS0432"),
                        x.span,
                        "a fragment's content or elements need a tree around the call",
                    );
                    return None;
                }
            }
        }
        Some(out)
    }

    /// A call's block: its arguments' values bound in the caller's scope (with `hidden` ones, a tree's context), then
    /// the parameters, typed, from them, for `body` to see alone.
    fn fragment_block(
        &mut self,
        cx: &RuleCx,
        frag: &FragmentItem,
        call: &Element,
        hidden: Vec<(String, Expr)>,
        body: Vec<Stmt>,
    ) -> Option<Stmt> {
        let _ = cx;
        let span = call.span;
        let mut values = Vec::new();
        for a in &call.args {
            match a {
                Arg::Pos(x) => values.push(x.clone()),
                other => {
                    self.error(
                        code!("BLS0303"),
                        other.span(),
                        "a fragment's arguments are positional, in its parameters' order",
                    );
                    return None;
                }
            }
        }
        if values.len() != frag.params.len() {
            self.error(
                code!("BLS0301"),
                span,
                format!(
                    "`{}` takes {} argument(s), {} given",
                    frag.name.as_str(),
                    frag.params.len(),
                    values.len()
                ),
            );
            return None;
        }
        let let_lit = |pat: &str, value: Expr| Lit::Let {
            pat: path_expr(pat, span),
            value,
            span,
        };
        let mut args = Vec::new();
        let mut params = Vec::new();
        for (i, ((p, ty), value)) in frag.params.iter().zip(values).enumerate() {
            let temp = format!("arg${}${i}@{}", p.as_str(), span.lo);
            args.push(let_lit(&temp, value));
            params.push(let_lit(
                p.as_str(),
                Expr {
                    kind: ExprKind::Ascribe {
                        expr: Box::new(path_expr(&temp, span)),
                        ty: ty.clone(),
                    },
                    span,
                },
            ));
        }
        for (name, value) in hidden {
            let temp = format!("arg${name}");
            args.push(let_lit(&temp, value));
            params.push(let_lit(&name, path_expr(&temp, span)));
        }
        let body_of = |lits: Vec<Lit>| Body {
            lits,
            guards: Vec::new(),
            span,
        };
        Some(Stmt::Fragment {
            name: frag.name,
            args: body_of(args),
            params: body_of(params),
            body: Block { stmts: body, span },
            text: self.normalized(span),
            span,
        })
    }

    /// The arguments of a role's row, in its relation's column order, from the values in the role's order.
    fn role_args(&mut self, role: &Role, values: Vec<Expr>) -> Vec<Arg> {
        let n = self.rel_of(role.rel).cols.len();
        let mut slots: Vec<Option<Expr>> = vec![None; n];
        for (place, value) in role.cols.iter().zip(values) {
            if let Some(s) = slots.get_mut(*place) {
                *s = Some(value);
            }
        }
        slots.into_iter().flatten().map(Arg::Pos).collect()
    }

    /// Whether a role's place holds a `String` column.
    fn string_col(&mut self, role: &Role, place: usize) -> bool {
        let rel = self.rel_of(role.rel);
        role.cols
            .get(place)
            .and_then(|c| rel.cols.get(*c))
            .is_some_and(|c| matches!(c.ty.and_then(|t| self.hir.types.get(t)), Some(TypeDef::Str)))
    }
}
