//! Formats: byte layouts declared once, from which the compiler derives a struct, a decoder and an encoder
//! (LANGUAGE §16.7, EXTENSIONS 2.5).
//!
//! A record `format Name(params) { [field:] element [if cond] [= default], … }` becomes, before name resolution:
//!
//! - `struct Name { field: T, … }`, one field per named element, of the element's value type;
//! - `fn Name::decode(b: Bytes, p: u64, params…) -> Option<(Name, u64)>`: the value at `p` and the position after
//!   it, `None` when the bytes run out or break the layout (never a runtime error: a hostile length or count is
//!   checked against the bytes left before anything uses it);
//! - `fn Name::encode(x: Name, params…) -> Bytes`.
//!
//! An alias `format name(F, …) = element;` names an element, its parameters replaced where it is used. Compound
//! elements (prefixed values, arrays, nullable ones) get their own generated functions (`Name$d1`, `Name$e1`, …),
//! and every scope with a record gets the shared helpers (`format$…`); `$` keeps all of these apart from any name a
//! program can write. Everything generated is ordinary Blossom (with `?`, closures in combinators and one generic
//! helper), so both evaluators run it unchanged.
//!
//! Element arguments (a nested format's arguments, `bytes(n)`'s length, `constant`'s value) may read the record's
//! parameters; a field's condition may also read the fields before it.

use std::collections::BTreeMap;

use blossom_base::{Diagnostic, Diagnostics, Span, Symbol, code};
use blossom_value::types::IntTy;

use super::*;

/// Replaces the formats among `items` — a file's or a module's, includes expanded — by their structs and functions,
/// recursively into modules. A format inside an `at` section, a protocol or an interposition is BLS0110.
pub(crate) fn expand(items: &mut Vec<Item>, diags: &mut Diagnostics) {
    for item in items.iter_mut() {
        match &mut item.kind {
            ItemKind::Module(m) => expand(&mut m.items, diags),
            ItemKind::At { items: inner, .. }
            | ItemKind::Protocol(ProtocolItem { items: inner, .. })
            | ItemKind::Interpose(Interpose { items: inner, .. }) => {
                inner.retain(|i| match &i.kind {
                    ItemKind::Format(f) => {
                        diags.push(
                            Diagnostic::new(
                                code!("BLS0110"),
                                "a `format` belongs at the top of a file or module, not in an `at` section, a \
                                 protocol or an interposition",
                            )
                            .with_primary(f.span),
                        );
                        false
                    }
                    _ => true,
                });
            }
            _ => {}
        }
    }
    if !items.iter().any(|i| matches!(i.kind, ItemKind::Format(_))) {
        return;
    }
    let mut formats = Vec::new();
    let mut rest = Vec::new();
    for item in std::mem::take(items) {
        match item.kind {
            ItemKind::Format(f) => formats.push((item.is_pub, f)),
            kind => rest.push(Item { kind, ..item }),
        }
    }
    let mut env = Env::default();
    for (_, f) in &formats {
        let clash = env.aliases.contains_key(&f.name.name) || env.records.contains_key(&f.name.name);
        if clash || is_builtin(f.name.as_str()) {
            diags.push(
                Diagnostic::new(
                    code!("BLS0201"),
                    format!(
                        "`{}` is declared twice, or is a built-in format element",
                        f.name.as_str()
                    ),
                )
                .with_primary(f.name.span),
            );
            continue;
        }
        match &f.body {
            FormatBody::Alias(e) => {
                if let Some(span) = unbounded(e) {
                    diags.push(
                        Diagnostic::new(
                            code!("BLS0301"),
                            "a format's element arguments take no closure and no `range` (they are evaluated for \
                             every value, outside the step budget); call a function, which is metered",
                        )
                        .with_primary(span),
                    );
                    continue;
                }
                env.aliases
                    .insert(f.name.name, (f.params.iter().map(|p| p.0).collect(), e.clone()));
            }
            FormatBody::Record(_) => {
                if let FormatBody::Record(fields) = &f.body {
                    env.records.insert(f.name.name, (f.params.len(), fields.clone()));
                }
            }
        }
    }
    let mut any_record = false;
    for (is_pub, f) in &formats {
        if let FormatBody::Record(fields) = &f.body {
            any_record = true;
            if let Some(out) = record(f, fields, &env, diags) {
                rest.extend(out.into_iter().map(|kind| Item {
                    attrs: Vec::new(),
                    is_pub: *is_pub,
                    kind,
                    span: f.span,
                }));
            }
        } else if let Some((p, _)) = f.params.iter().find(|p| p.1.is_some()) {
            diags.push(
                Diagnostic::new(
                    code!("BLS0301"),
                    "an alias's parameters are elements, written without types",
                )
                .with_primary(p.span),
            );
        }
    }
    if any_record && let Some((_, f)) = formats.first() {
        rest.extend(helpers(f.span, diags).into_iter().map(|kind| Item {
            attrs: Vec::new(),
            is_pub: false,
            kind,
            span: f.span,
        }));
    }
    *items = rest;
}

/// The scope's aliases (their parameters and element) and records (their number of parameters).
#[derive(Default)]
struct Env {
    aliases: BTreeMap<Symbol, (Vec<Ident>, Expr)>,
    /// Each record's number of parameters and fields (for the widths of the elements that hold it).
    records: BTreeMap<Symbol, (usize, Vec<FormatField>)>,
}

fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "bool"
            | "uvarint"
            | "varint"
            | "rest"
            | "utf8"
            | "tags"
            | "bytes"
            | "prefixed"
            | "array"
            | "nullable"
            | "constant"
            | "ignored"
            | "select"
    ) || int_named(name).is_some()
}

/// The integer elements: big-endian, as the byte accessors read them.
fn int_named(name: &str) -> Option<IntTy> {
    Some(match name {
        "u8" => IntTy::U8,
        "i8" => IntTy::I8,
        "u16" => IntTy::U16,
        "i16" => IntTy::I16,
        "u32" => IntTy::U32,
        "i32" => IntTy::I32,
        "u64" => IntTy::U64,
        "i64" => IntTy::I64,
        _ => return None,
    })
}

/// A length's encoding.
#[derive(Clone, Copy, Debug)]
enum Len {
    Int(IntTy),
    Uvarint,
    Varint,
}

impl Len {
    fn signed(self) -> bool {
        match self {
            Len::Int(t) => t.is_signed(),
            Len::Uvarint => false,
            Len::Varint => true,
        }
    }
}

/// An element, aliases expanded.
#[derive(Clone, Debug)]
enum Elem {
    Int(IntTy),
    Bool,
    Uvarint,
    Varint,
    /// Exactly `n` bytes.
    Bytes(Expr),
    /// The rest of the buffer, as bytes.
    Rest,
    /// The rest of the buffer, as a string.
    Utf8,
    /// A length (minus `bias`), then a value taking exactly that many bytes.
    Prefixed {
        len: Len,
        bias: u64,
        inner: Box<Elem>,
    },
    /// A count (minus `bias`), then that many items.
    Array {
        len: Len,
        bias: u64,
        item: Box<Elem>,
    },
    /// A prefixed value or array (`inner`, whose length is `len` with `bias`) whose length may be the null value
    /// (`bias - 1`): `None`.
    Nullable {
        len: Len,
        bias: u64,
        inner: Box<Elem>,
    },
    /// A record format, with its arguments.
    Format {
        name: Ident,
        args: Vec<Expr>,
    },
    /// An element whose value is fixed: written on encode, checked on decode. No field.
    Constant {
        elem: Box<Elem>,
        value: Expr,
    },
    /// An element read past and dropped on decode; `value` is written on encode. No field.
    Ignored {
        elem: Box<Elem>,
        value: Expr,
    },
    /// A flexible-version tagged-field section: none written, every one skipped (unknown tags are ignored). No field.
    Tags,
    /// Elements in sequence; the value is the tuple of the valued ones (the one itself, when only one is).
    Tuple(Vec<Elem>),
    /// `select(cond, A, B)`: `A` where the condition (over the format's parameters) holds, `B`
    /// otherwise; both have the same value (a protocol's encoding that changes with its version).
    Select {
        cond: Expr,
        then: Box<Elem>,
        els: Box<Elem>,
    },
}

impl Elem {
    /// Whether the element has a value (a field), rather than only a position.
    fn valued(&self) -> bool {
        match self {
            Elem::Constant { .. } | Elem::Ignored { .. } | Elem::Tags => false,
            Elem::Tuple(xs) => xs.iter().any(Elem::valued),
            Elem::Select { then, .. } => then.valued(),
            _ => true,
        }
    }
}

/// The deepest alias expansion: deeper is an alias that expands into itself.
const MAX_ALIAS_DEPTH: u32 = 64;
/// The largest element an alias may expand to (an alias that duplicates its argument grows exponentially).
const MAX_ALIAS_SIZE: usize = 10_000;

const REST_LAST: &str = "`rest` and `utf8` read every byte left: such an element comes last";

/// The fewest bytes an element takes (0 for an element whose size depends on its value or parameters).
fn min_width(e: &Elem, env: &Env, depth: u32) -> u64 {
    match e {
        Elem::Int(t) => int_access(*t).2 as u64,
        Elem::Bool | Elem::Uvarint | Elem::Varint | Elem::Tags => 1,
        Elem::Bytes(n) => match &n.kind {
            ExprKind::Lit(LitValue::Int { value, .. }) => u64::try_from(*value).unwrap_or(u64::MAX),
            _ => 0,
        },
        Elem::Rest | Elem::Utf8 => 0,
        Elem::Prefixed { len, .. } | Elem::Array { len, .. } | Elem::Nullable { len, .. } => match len {
            Len::Int(t) => int_access(*t).2 as u64,
            Len::Uvarint | Len::Varint => 1,
        },
        Elem::Constant { elem, .. } | Elem::Ignored { elem, .. } => min_width(elem, env, depth),
        Elem::Tuple(xs) => xs.iter().map(|x| min_width(x, env, depth)).fold(0, u64::saturating_add),
        Elem::Select { then, els, .. } => min_width(then, env, depth).min(min_width(els, env, depth)),
        Elem::Format { name, .. } => record_elems(name.name, env, depth)
            .iter()
            .filter(|(conditional, _)| !conditional)
            .map(|(_, x)| min_width(x, env, depth + 1))
            .fold(0, u64::saturating_add),
    }
}

/// Whether an element reads every byte left.
fn consumes_all(e: &Elem, env: &Env, depth: u32) -> bool {
    match e {
        Elem::Rest | Elem::Utf8 => true,
        Elem::Constant { elem, .. } | Elem::Ignored { elem, .. } => consumes_all(elem, env, depth),
        Elem::Tuple(xs) => xs.last().is_some_and(|x| consumes_all(x, env, depth)),
        Elem::Select { then, els, .. } => consumes_all(then, env, depth) || consumes_all(els, env, depth),
        Elem::Format { name, .. } => record_elems(name.name, env, depth)
            .last()
            .is_some_and(|(_, x)| consumes_all(x, env, depth + 1)),
        _ => false,
    }
}

/// A record's elements (and whether each is conditional), read without reporting (its own expansion reports); none
/// past the alias depth (a record that holds itself is reported as recursive where its decoder is resolved).
fn record_elems(name: Symbol, env: &Env, depth: u32) -> Vec<(bool, Elem)> {
    let Some((_, fields)) = env.records.get(&name) else {
        return Vec::new();
    };
    if depth > MAX_ALIAS_DEPTH {
        return Vec::new();
    }
    let mut quiet = Diagnostics::new();
    fields
        .iter()
        .filter_map(|f| elem(&f.elem, env, depth + 1, &mut quiet).map(|e| (f.cond.is_some(), e)))
        .collect()
}

/// An expression's number of nodes.
fn size(e: &Expr) -> usize {
    let mut n = 1;
    for c in children(e) {
        n += size(c);
    }
    n
}

/// The first closure or `range` call in `e`, if any: work an expression does on its own, not bounded by its size.
fn unbounded(e: &Expr) -> Option<Span> {
    match &e.kind {
        ExprKind::Closure { .. } => return Some(e.span),
        ExprKind::Call { callee, .. } if matches!(&callee.kind, ExprKind::Path(p, _) if matches!(p.as_slice(), [n] if n.as_str() == "range")) =>
        {
            return Some(e.span);
        }
        _ => {}
    }
    children(e).into_iter().find_map(unbounded)
}

/// Whether two written types are the same, spans aside.
fn same_type(a: &Type, b: &Type) -> bool {
    match (a, b) {
        (Type::Named { path: pa, args: aa, .. }, Type::Named { path: pb, args: ab, .. }) => {
            pa.len() == pb.len()
                && pa.iter().zip(pb).all(|(x, y)| x.name == y.name)
                && aa.len() == ab.len()
                && aa.iter().zip(ab).all(|(x, y)| same_type(x, y))
        }
        (Type::Tuple { elems: ea, .. }, Type::Tuple { elems: eb, .. }) => {
            ea.len() == eb.len() && ea.iter().zip(eb).all(|(x, y)| same_type(x, y))
        }
        (Type::Unsafe { inner: ia, .. }, Type::Unsafe { inner: ib, .. }) => same_type(ia, ib),
        (
            Type::Fn {
                params: pa, ret: ra, ..
            },
            Type::Fn {
                params: pb, ret: rb, ..
            },
        ) => pa.len() == pb.len() && pa.iter().zip(pb).all(|(x, y)| same_type(x, y)) && same_type(ra, rb),
        _ => false,
    }
}

/// An expression's direct subexpressions (an aggregate's clauses aside).
fn children(e: &Expr) -> Vec<&Expr> {
    fn args(args: &[Arg]) -> Vec<&Expr> {
        args.iter()
            .flat_map(|a| match a {
                Arg::Pos(x) | Arg::Named(_, x) | Arg::Spread(Spread::Expr(x, _)) => vec![x],
                Arg::Spread(Spread::Record(fields, _)) => fields.iter().map(|(_, x)| x).collect(),
                Arg::Rest(_) | Arg::Star(_) => Vec::new(),
            })
            .collect()
    }
    match &e.kind {
        ExprKind::Lit(_) | ExprKind::Path(..) | ExprKind::Wildcard | ExprKind::SelfNode => Vec::new(),
        ExprKind::Call { callee, args: a } => std::iter::once(&**callee).chain(args(a)).collect(),
        ExprKind::Method { receiver, args: a, .. } => std::iter::once(&**receiver).chain(args(a)).collect(),
        ExprKind::Bang { args: a, .. } => args(a),
        ExprKind::Field { base, .. } | ExprKind::TupleIndex { base, .. } => vec![base],
        ExprKind::Index { base, index } => vec![base, index],
        ExprKind::Binary { lhs, rhs, .. } => vec![lhs, rhs],
        ExprKind::Prefix { arg, .. }
        | ExprKind::Cast { expr: arg, .. }
        | ExprKind::Ascribe { expr: arg, .. }
        | ExprKind::Try(arg) => vec![arg],
        ExprKind::Tuple(xs) | ExprKind::Vec(xs) | ExprKind::Set(xs) => xs.iter().collect(),
        ExprKind::Map(kvs) => kvs.iter().flat_map(|(k, v)| [k, v]).collect(),
        ExprKind::If { cond, then, els } => [&**cond, &**then].into_iter().chain(els.as_deref()).collect(),
        ExprKind::Match { scrut, arms } => std::iter::once(&**scrut)
            .chain(
                arms.iter()
                    .flat_map(|a| [&a.pat, &a.body].into_iter().chain(a.guard.as_ref())),
            )
            .collect(),
        ExprKind::StructLit { fields, base, .. } => fields
            .iter()
            .filter_map(|(_, v)| v.as_ref())
            .chain(base.iter().map(|x| &**x))
            .collect(),
        ExprKind::Block { lets, result } => lets
            .iter()
            .flat_map(|l| [&l.pat, &l.value])
            .chain(std::iter::once(&**result))
            .collect(),
        ExprKind::Closure { body, .. } => vec![body],
    }
}

/// Reads an element from its expression.
fn elem(e: &Expr, env: &Env, depth: u32, diags: &mut Diagnostics) -> Option<Elem> {
    let fail = |diags: &mut Diagnostics, msg: String| {
        diags.push(Diagnostic::new(code!("BLS0301"), msg).with_primary(e.span));
        None
    };
    if depth > MAX_ALIAS_DEPTH {
        return fail(diags, "format aliases that expand into themselves".into());
    }
    if let ExprKind::Tuple(xs) = &e.kind {
        let mut out = Vec::new();
        for x in xs {
            out.push(elem(x, env, depth + 1, diags)?);
        }
        if out.iter().rev().skip(1).any(|x| consumes_all(x, env, 0)) {
            return fail(diags, REST_LAST.into());
        }
        let t = Elem::Tuple(out);
        if !t.valued() {
            return fail(diags, "a tuple element holds at least one element with a value".into());
        }
        return Some(t);
    }
    let (name, args): (Ident, &[Arg]) = match &e.kind {
        ExprKind::Path(path, targs) if targs.is_empty() && path.len() == 1 => (*path.first()?, &[]),
        ExprKind::Call { callee, args } => match &callee.kind {
            ExprKind::Path(path, targs) if targs.is_empty() && path.len() == 1 => (*path.first()?, args.as_slice()),
            _ => return fail(diags, "a format element is a name or a call of one".into()),
        },
        _ => return fail(diags, "a format element is a name or a call of one".into()),
    };
    let mut pos = Vec::new();
    for a in args {
        match a {
            Arg::Pos(x) => pos.push(x),
            _ => return fail(diags, "a format element's arguments are positional".into()),
        }
    }
    let arity = |diags: &mut Diagnostics, n: usize| -> bool {
        if pos.len() == n {
            true
        } else {
            diags.push(
                Diagnostic::new(
                    code!("BLS0301"),
                    format!("`{}` takes {n} argument(s), {} given", name.as_str(), pos.len()),
                )
                .with_primary(e.span),
            );
            false
        }
    };
    let text = name.as_str();
    if let Some(t) = int_named(text) {
        return arity(diags, 0).then_some(Elem::Int(t));
    }
    match text {
        "bool" => return arity(diags, 0).then_some(Elem::Bool),
        "uvarint" => return arity(diags, 0).then_some(Elem::Uvarint),
        "varint" => return arity(diags, 0).then_some(Elem::Varint),
        "rest" => return arity(diags, 0).then_some(Elem::Rest),
        "utf8" => return arity(diags, 0).then_some(Elem::Utf8),
        "tags" => return arity(diags, 0).then_some(Elem::Tags),
        "bytes" => {
            if !arity(diags, 1) {
                return None;
            }
            return Some(Elem::Bytes((*pos.first()?).clone()));
        }
        "prefixed" | "array" => {
            if !arity(diags, 3) {
                return None;
            }
            let len = match elem(pos.first()?, env, depth + 1, diags)? {
                Elem::Int(t) => Len::Int(t),
                Elem::Uvarint => Len::Uvarint,
                Elem::Varint => Len::Varint,
                _ => return fail(diags, format!("`{text}`'s length is an integer or a varint")),
            };
            let bias = match &pos.get(1)?.kind {
                ExprKind::Lit(LitValue::Int { value, suffix: None }) => *value,
                _ => return fail(diags, format!("`{text}`'s bias is an unsuffixed integer literal")),
            };
            // The bias is the raw length of an empty value: it must be one the length's type can hold.
            let max: u128 = match len {
                Len::Int(t) if t.is_signed() => (1u128 << (t.bits() - 1)) - 1,
                Len::Int(t) => (1u128 << t.bits()) - 1,
                Len::Uvarint => u128::from(u64::MAX),
                Len::Varint => u128::from(i64::MAX as u64),
            };
            if bias > max {
                return fail(diags, format!("`{text}`'s bias {bias} does not fit its length's type"));
            }
            let bias = u64::try_from(bias).ok()?;
            let inner = elem(pos.get(2)?, env, depth + 1, diags)?;
            if !inner.valued() {
                return fail(diags, format!("`{text}` holds an element with a value"));
            }
            if text == "array" && (min_width(&inner, env, 0) == 0 || consumes_all(&inner, env, 0)) {
                return fail(
                    diags,
                    "an array's items must each take at least one byte, and not every byte left (so a count is \
                     checked against the bytes left, and decoding gives back what encoding wrote)"
                        .into(),
                );
            }
            let inner = Box::new(inner);
            return Some(if text == "prefixed" {
                Elem::Prefixed { len, bias, inner }
            } else {
                Elem::Array { len, bias, item: inner }
            });
        }
        "nullable" => {
            if !arity(diags, 1) {
                return None;
            }
            let inner = elem(pos.first()?, env, depth + 1, diags)?;
            let (len, bias) = match &inner {
                Elem::Prefixed { len, bias, .. } | Elem::Array { len, bias, .. } => (*len, *bias),
                _ => return fail(diags, "`nullable` applies to a prefixed value or an array".into()),
            };
            if !len.signed() && bias == 0 {
                return fail(
                    diags,
                    "`nullable` over an unsigned length needs a bias of at least 1 (its null is the bias less one)"
                        .into(),
                );
            }
            return Some(Elem::Nullable {
                len,
                bias,
                inner: Box::new(inner),
            });
        }
        "select" => {
            if !arity(diags, 3) {
                return None;
            }
            let then = elem(pos.get(1)?, env, depth + 1, diags)?;
            let els = elem(pos.get(2)?, env, depth + 1, diags)?;
            if then.valued() != els.valued() {
                return fail(
                    diags,
                    "`select`'s two elements both have a value, or neither does".into(),
                );
            }
            return Some(Elem::Select {
                cond: (*pos.first()?).clone(),
                then: Box::new(then),
                els: Box::new(els),
            });
        }
        "constant" | "ignored" => {
            if !arity(diags, 2) {
                return None;
            }
            let inner = elem(pos.first()?, env, depth + 1, diags)?;
            if !inner.valued() {
                return fail(diags, format!("`{text}` takes an element with a value"));
            }
            let (elem, value) = (Box::new(inner), (*pos.get(1)?).clone());
            return Some(if text == "constant" {
                Elem::Constant { elem, value }
            } else {
                Elem::Ignored { elem, value }
            });
        }
        _ => {}
    }
    if let Some((params, body)) = env.aliases.get(&name.name) {
        if !arity(diags, params.len()) {
            return None;
        }
        let map: BTreeMap<Symbol, &Expr> = params.iter().map(|p| p.name).zip(pos.iter().copied()).collect();
        let expanded = substitute(body, &map);
        if size(&expanded) > MAX_ALIAS_SIZE {
            return fail(
                diags,
                format!("an alias expands to more than {MAX_ALIAS_SIZE} expression nodes"),
            );
        }
        return elem(&expanded, env, depth + 1, diags);
    }
    if let Some((n, _)) = env.records.get(&name.name) {
        if !arity(diags, *n) {
            return None;
        }
        return Some(Elem::Format {
            name,
            args: pos.into_iter().cloned().collect(),
        });
    }
    fail(diags, format!("`{text}` is not a format element"))
}

/// An alias's element with its parameters replaced, wherever they occur (an argument may be any expression, such
/// as `bytes(n * 2)`). A name a closure, a `let` or a match arm binds is not a parameter inside it.
fn substitute(e: &Expr, map: &BTreeMap<Symbol, &Expr>) -> Expr {
    let span = e.span;
    let sub = |x: &Expr| substitute(x, map);
    let sub_args = |args: &[Arg]| -> Vec<Arg> {
        args.iter()
            .map(|a| match a {
                Arg::Pos(x) => Arg::Pos(substitute(x, map)),
                Arg::Named(n, x) => Arg::Named(*n, substitute(x, map)),
                other => other.clone(),
            })
            .collect()
    };
    let without = |names: &[Symbol]| -> BTreeMap<Symbol, &Expr> {
        map.iter()
            .filter(|(k, _)| !names.contains(k))
            .map(|(k, v)| (*k, *v))
            .collect()
    };
    let kind = match &e.kind {
        ExprKind::Path(path, targs) if targs.is_empty() && path.len() == 1 => {
            match path.first().and_then(|n| map.get(&n.name)) {
                Some(x) => return (*x).clone(),
                None => e.kind.clone(),
            }
        }
        ExprKind::Lit(_) | ExprKind::Path(..) | ExprKind::Wildcard | ExprKind::SelfNode | ExprKind::Bang { .. } => {
            e.kind.clone()
        }
        ExprKind::Call { callee, args } => ExprKind::Call {
            callee: Box::new(sub(callee)),
            args: sub_args(args),
        },
        ExprKind::Method { receiver, name, args } => ExprKind::Method {
            receiver: Box::new(sub(receiver)),
            name: *name,
            args: sub_args(args),
        },
        ExprKind::Field { base, name } => ExprKind::Field {
            base: Box::new(sub(base)),
            name: *name,
        },
        ExprKind::TupleIndex { base, index } => ExprKind::TupleIndex {
            base: Box::new(sub(base)),
            index: *index,
        },
        ExprKind::Index { base, index } => ExprKind::Index {
            base: Box::new(sub(base)),
            index: Box::new(sub(index)),
        },
        ExprKind::Binary { op, lhs, rhs } => ExprKind::Binary {
            op: *op,
            lhs: Box::new(sub(lhs)),
            rhs: Box::new(sub(rhs)),
        },
        ExprKind::Prefix { op, arg } => ExprKind::Prefix {
            op: *op,
            arg: Box::new(sub(arg)),
        },
        ExprKind::Cast { expr, ty } => ExprKind::Cast {
            expr: Box::new(sub(expr)),
            ty: ty.clone(),
        },
        ExprKind::Ascribe { expr, ty } => ExprKind::Ascribe {
            expr: Box::new(sub(expr)),
            ty: ty.clone(),
        },
        ExprKind::Try(x) => ExprKind::Try(Box::new(sub(x))),
        ExprKind::Tuple(xs) => ExprKind::Tuple(xs.iter().map(sub).collect()),
        ExprKind::Vec(xs) => ExprKind::Vec(xs.iter().map(sub).collect()),
        ExprKind::Set(xs) => ExprKind::Set(xs.iter().map(sub).collect()),
        ExprKind::Map(kvs) => ExprKind::Map(kvs.iter().map(|(k, v)| (sub(k), sub(v))).collect()),
        ExprKind::If { cond, then, els } => ExprKind::If {
            cond: Box::new(sub(cond)),
            then: Box::new(sub(then)),
            els: els.as_ref().map(|x| Box::new(sub(x))),
        },
        ExprKind::StructLit { path, fields, base } => ExprKind::StructLit {
            path: path.clone(),
            fields: fields.iter().map(|(n, v)| (*n, v.as_ref().map(sub))).collect(),
            base: base.as_ref().map(|x| Box::new(sub(x))),
        },
        ExprKind::Closure { params, body } => {
            let inner = without(&params.iter().map(|p| p.name).collect::<Vec<_>>());
            ExprKind::Closure {
                params: params.clone(),
                body: Box::new(substitute(body, &inner)),
            }
        }
        ExprKind::Match { scrut, arms } => ExprKind::Match {
            scrut: Box::new(sub(scrut)),
            arms: arms
                .iter()
                .map(|a| {
                    let inner = without(&bound_names(&a.pat));
                    MatchArm {
                        pat: a.pat.clone(),
                        guard: a.guard.as_ref().map(|g| substitute(g, &inner)),
                        body: substitute(&a.body, &inner),
                    }
                })
                .collect(),
        },
        ExprKind::Block { lets, result } => {
            let mut bound = Vec::new();
            let mut out = Vec::new();
            for l in lets {
                let inner = without(&bound);
                out.push(BlockLet {
                    pat: l.pat.clone(),
                    ty: l.ty.clone(),
                    value: substitute(&l.value, &inner),
                    span: l.span,
                });
                bound.extend(bound_names(&l.pat));
            }
            ExprKind::Block {
                lets: out,
                result: Box::new(substitute(result, &without(&bound))),
            }
        }
    };
    Expr::new(kind, span)
}

/// The names a pattern binds: its one-segment paths.
fn bound_names(p: &Expr) -> Vec<Symbol> {
    match &p.kind {
        ExprKind::Path(path, _) if path.len() == 1 => path.iter().map(|n| n.name).collect(),
        _ => children(p).into_iter().flat_map(bound_names).collect(),
    }
}

/// Builds expressions, types and items at one span.
#[derive(Clone, Copy)]
struct B {
    span: Span,
}

impl B {
    fn id(self, s: &str) -> Ident {
        Ident {
            name: Symbol::intern(s),
            span: self.span,
        }
    }
    fn e(self, kind: ExprKind) -> Expr {
        Expr::new(kind, self.span)
    }
    fn var(self, s: &str) -> Expr {
        self.e(ExprKind::Path(s.split("::").map(|x| self.id(x)).collect(), Vec::new()))
    }
    fn call(self, f: &str, args: Vec<Expr>) -> Expr {
        self.e(ExprKind::Call {
            callee: Box::new(self.var(f)),
            args: args.into_iter().map(Arg::Pos).collect(),
        })
    }
    fn m(self, recv: Expr, name: &str, args: Vec<Expr>) -> Expr {
        self.e(ExprKind::Method {
            receiver: Box::new(recv),
            name: self.id(name),
            args: args.into_iter().map(Arg::Pos).collect(),
        })
    }
    fn int(self, v: u128, suffix: Option<&str>) -> Expr {
        self.e(ExprKind::Lit(LitValue::Int {
            value: v,
            suffix: suffix.map(Symbol::intern),
        }))
    }
    /// An integer literal of type `t`, possibly negative.
    fn int_of(self, v: i128, t: &str) -> Expr {
        let lit = self.int(v.unsigned_abs(), Some(t));
        if v < 0 {
            self.e(ExprKind::Prefix {
                op: PrefixOp::Neg,
                arg: Box::new(lit),
            })
        } else {
            lit
        }
    }
    fn bin(self, op: BinOp, a: Expr, b: Expr) -> Expr {
        self.e(ExprKind::Binary {
            op,
            lhs: Box::new(a),
            rhs: Box::new(b),
        })
    }
    fn tup(self, xs: Vec<Expr>) -> Expr {
        self.e(ExprKind::Tuple(xs))
    }
    fn some(self, x: Expr) -> Expr {
        self.call("Some", vec![x])
    }
    fn none(self) -> Expr {
        self.var("None")
    }
    fn ifx(self, c: Expr, t: Expr, f: Expr) -> Expr {
        self.e(ExprKind::If {
            cond: Box::new(c),
            then: Box::new(t),
            els: Some(Box::new(f)),
        })
    }
    fn mat(self, scrut: Expr, arms: Vec<(Expr, Expr)>) -> Expr {
        self.e(ExprKind::Match {
            scrut: Box::new(scrut),
            arms: arms
                .into_iter()
                .map(|(pat, body)| MatchArm { pat, guard: None, body })
                .collect(),
        })
    }
    fn clos(self, params: &[&str], body: Expr) -> Expr {
        self.e(ExprKind::Closure {
            params: params.iter().map(|p| self.id(p)).collect(),
            body: Box::new(body),
        })
    }
    fn tidx(self, base: Expr, i: u32) -> Expr {
        self.e(ExprKind::TupleIndex {
            base: Box::new(base),
            index: i,
        })
    }
    fn field(self, base: Expr, name: Ident) -> Expr {
        self.e(ExprKind::Field {
            base: Box::new(base),
            name,
        })
    }
    fn cast(self, x: Expr, t: &str) -> Expr {
        self.e(ExprKind::Cast {
            expr: Box::new(x),
            ty: self.ty(t),
        })
    }
    fn try_(self, x: Expr) -> Expr {
        self.e(ExprKind::Try(Box::new(x)))
    }
    fn tpat(self, names: &[&str]) -> Expr {
        self.tup(names.iter().map(|n| self.var(n)).collect())
    }
    fn block(self, lets: Vec<(Expr, Expr)>, result: Expr) -> Expr {
        if lets.is_empty() {
            return result;
        }
        self.e(ExprKind::Block {
            lets: lets
                .into_iter()
                .map(|(pat, value)| BlockLet {
                    pat,
                    ty: None,
                    value,
                    span: self.span,
                })
                .collect(),
            result: Box::new(result),
        })
    }
    fn ty(self, name: &str) -> Type {
        self.ty_app(name, Vec::new())
    }
    fn ty_app(self, name: &str, args: Vec<Type>) -> Type {
        Type::Named {
            path: vec![self.id(name)],
            args,
            span: self.span,
        }
    }
    fn ty_tuple(self, elems: Vec<Type>) -> Type {
        Type::Tuple { elems, span: self.span }
    }
    /// A function item; its `?`s are desugared as a written function's are.
    fn func(
        self,
        name: &str,
        generics: &[&str],
        params: Vec<(Ident, Type)>,
        ret: Type,
        body: Expr,
        diags: &mut Diagnostics,
    ) -> ItemKind {
        let mut item = FnItem {
            name: self.id(name),
            generics: generics
                .iter()
                .map(|g| GenericParam {
                    name: self.id(g),
                    bounds: Vec::new(),
                })
                .collect(),
            params,
            ret,
            body,
            span: self.span,
            metered: false,
        };
        super::desugar::fn_body(&mut item, diags);
        ItemKind::Fn(item)
    }
}

/// The method reading an integer element at a position, and the function writing one.
fn int_access(t: IntTy) -> (String, String, u128) {
    let width = match t {
        IntTy::U8 | IntTy::I8 => 1,
        IntTy::U16 | IntTy::I16 => 2,
        IntTy::U32 | IntTy::I32 => 4,
        _ => 8,
    };
    let name = if width == 1 {
        t.name().to_string()
    } else {
        format!("{}_be", t.name())
    };
    (format!("{name}_at"), format!("Bytes::from_{name}"), width)
}

/// A compound element, the ones generated functions decode and encode: its parts.
#[derive(Clone, Copy)]
enum Compound<'e> {
    Prefixed(Len, u64, &'e Elem),
    Array(Len, u64, &'e Elem),
    Nullable(Len, u64, &'e Elem),
    Tuple(&'e [Elem]),
}

/// One record's generation: its sub-functions accumulate in `out`, and what desugaring their bodies reports in
/// `diags` (nothing, unless the generator itself is wrong).
struct Gen<'a> {
    b: B,
    name: &'a str,
    params: &'a [(Ident, Type)],
    out: Vec<ItemKind>,
    next: u32,
    diags: Diagnostics,
}

impl Gen<'_> {
    fn fresh(&mut self, kind: char) -> String {
        self.next += 1;
        format!("{}${kind}{}", self.name, self.next)
    }

    fn param_decls(&self) -> Vec<(Ident, Type)> {
        self.params.to_vec()
    }

    fn param_args(&self) -> Vec<Expr> {
        self.params.iter().map(|(p, _)| self.b.var(p.as_str())).collect()
    }

    /// An element's value type.
    fn value_ty(&self, e: &Elem) -> Type {
        let b = self.b;
        match e {
            Elem::Int(t) => b.ty(t.name()),
            Elem::Bool => b.ty("bool"),
            Elem::Uvarint => b.ty("u64"),
            Elem::Varint => b.ty("i64"),
            Elem::Bytes(_) | Elem::Rest => b.ty("Bytes"),
            Elem::Utf8 => b.ty("String"),
            Elem::Prefixed { inner, .. } => self.value_ty(inner),
            Elem::Array { item, .. } => b.ty_app("Vec", vec![self.value_ty(item)]),
            Elem::Nullable { inner, .. } => b.ty_app("Option", vec![self.value_ty(inner)]),
            Elem::Format { name, .. } => b.ty(name.as_str()),
            Elem::Tuple(xs) => {
                let mut tys: Vec<Type> = xs.iter().filter(|x| x.valued()).map(|x| self.value_ty(x)).collect();
                if tys.len() == 1 {
                    tys.pop().unwrap_or_else(|| b.ty_tuple(Vec::new()))
                } else {
                    b.ty_tuple(tys)
                }
            }
            // Both branches have one value type (`select_mismatch`).
            Elem::Select { then, .. } => self.value_ty(then),
            // Valueless elements have no field; callers check `valued` first.
            Elem::Constant { .. } | Elem::Ignored { .. } | Elem::Tags => b.ty_tuple(Vec::new()),
        }
    }

    /// The first `select` in `e` whose two elements' values differ in type.
    fn select_mismatch<'e>(&self, e: &'e Elem) -> Option<&'e Expr> {
        match e {
            Elem::Select { cond, then, els } => {
                if then.valued() && !same_type(&self.value_ty(then), &self.value_ty(els)) {
                    return Some(cond);
                }
                self.select_mismatch(then).or_else(|| self.select_mismatch(els))
            }
            Elem::Prefixed { inner, .. } | Elem::Nullable { inner, .. } => self.select_mismatch(inner),
            Elem::Array { item, .. } => self.select_mismatch(item),
            Elem::Constant { elem, .. } | Elem::Ignored { elem, .. } => self.select_mismatch(elem),
            Elem::Tuple(xs) => xs.iter().find_map(|x| self.select_mismatch(x)),
            _ => None,
        }
    }

    /// The value an absent conditional field decodes to: its type's zero, or `None` for a format (which needs a
    /// declared default).
    fn zero(&self, e: &Elem) -> Option<Expr> {
        let b = self.b;
        Some(match e {
            Elem::Int(t) => b.int(0, Some(t.name())),
            Elem::Bool => b.e(ExprKind::Lit(LitValue::Bool(false))),
            Elem::Uvarint => b.int(0, Some("u64")),
            Elem::Varint => b.int(0, Some("i64")),
            Elem::Bytes(_) | Elem::Rest => b.call("Bytes::empty", Vec::new()),
            Elem::Utf8 => b.e(ExprKind::Lit(LitValue::Str(String::new()))),
            Elem::Prefixed { inner, .. } => return self.zero(inner),
            Elem::Array { .. } => b.e(ExprKind::Vec(Vec::new())),
            Elem::Nullable { .. } => b.none(),
            Elem::Tuple(xs) => {
                let mut zs = Vec::new();
                for x in xs.iter().filter(|x| x.valued()) {
                    zs.push(self.zero(x)?);
                }
                if zs.len() == 1 {
                    return zs.pop();
                }
                b.tup(zs)
            }
            Elem::Select { then, .. } => return self.zero(then),
            Elem::Format { .. } | Elem::Constant { .. } | Elem::Ignored { .. } | Elem::Tags => return None,
        })
    }

    /// Decoding `e` from `buf` at `pos`: an `Option<(T, u64)>` for a valued element, an `Option<u64>` otherwise.
    fn dec(&mut self, e: &Elem, buf: &Expr, pos: &Expr) -> Expr {
        let b = self.b;
        match e {
            Elem::Int(t) => {
                let (at, _, width) = int_access(*t);
                let read = b.m(buf.clone(), &at, vec![pos.clone()]);
                let next = b.bin(BinOp::Add, pos.clone(), b.int(width, None));
                b.m(read, "map", vec![b.clos(&["fmt$v"], b.tup(vec![b.var("fmt$v"), next]))])
            }
            Elem::Bool => {
                let read = b.m(buf.clone(), "u8_at", vec![pos.clone()]);
                let truth = b.bin(BinOp::Ne, b.var("fmt$v"), b.int(0, Some("u8")));
                let next = b.bin(BinOp::Add, pos.clone(), b.int(1, None));
                b.m(read, "map", vec![b.clos(&["fmt$v"], b.tup(vec![truth, next]))])
            }
            Elem::Uvarint => b.m(buf.clone(), "uvarint_at", vec![pos.clone()]),
            Elem::Varint => b.m(buf.clone(), "varint_at", vec![pos.clone()]),
            Elem::Bytes(n) => b.call("format$bytes", vec![buf.clone(), pos.clone(), n.clone()]),
            Elem::Rest => {
                let left = b.call("format$left", vec![buf.clone(), pos.clone()]);
                b.call("format$bytes", vec![buf.clone(), pos.clone(), left])
            }
            Elem::Utf8 => {
                let left = b.call("format$left", vec![buf.clone(), pos.clone()]);
                b.call("format$utf8", vec![buf.clone(), pos.clone(), left])
            }
            Elem::Prefixed { len, bias, inner } => self.dec_call(e, Compound::Prefixed(*len, *bias, inner), buf, pos),
            Elem::Array { len, bias, item } => self.dec_call(e, Compound::Array(*len, *bias, item), buf, pos),
            Elem::Nullable { len, bias, inner } => self.dec_call(e, Compound::Nullable(*len, *bias, inner), buf, pos),
            Elem::Tuple(xs) => self.dec_call(e, Compound::Tuple(xs), buf, pos),
            Elem::Format { name, args } => {
                let mut xs = vec![buf.clone(), pos.clone()];
                xs.extend(args.iter().cloned());
                b.call(&format!("{}::decode", name.as_str()), xs)
            }
            Elem::Constant { elem, value } => {
                let read = self.dec(elem, buf, pos);
                let check = b.ifx(
                    b.bin(BinOp::Eq, b.tidx(b.var("fmt$c"), 0), value.clone()),
                    b.some(b.tidx(b.var("fmt$c"), 1)),
                    b.none(),
                );
                b.m(read, "and_then", vec![b.clos(&["fmt$c"], check)])
            }
            Elem::Ignored { elem, .. } => {
                let read = self.dec(elem, buf, pos);
                b.m(read, "map", vec![b.clos(&["fmt$c"], b.tidx(b.var("fmt$c"), 1))])
            }
            Elem::Tags => b.call("format$skip_tags", vec![buf.clone(), pos.clone()]),
            Elem::Select { cond, then, els } => {
                let (t, f) = (self.dec(then, buf, pos), self.dec(els, buf, pos));
                b.ifx(cond.clone(), t, f)
            }
        }
    }

    /// The length a prefixed value or array declares, from its raw value: `None` for a malformed one.
    fn len_of(&self, len: Len, bias: u64, raw: Expr) -> Expr {
        let b = self.b;
        if len.signed() {
            b.call(
                "format$ilen",
                vec![b.cast(raw, "i64"), b.int(u128::from(bias), Some("i64"))],
            )
        } else {
            b.call(
                "format$ulen",
                vec![b.cast(raw, "u64"), b.int(u128::from(bias), Some("u64"))],
            )
        }
    }

    /// The raw length value that stands for null: the bias less one, of the length's type.
    fn null_raw(&self, len: Len, bias: u64) -> Expr {
        let b = self.b;
        let v = i128::from(bias) - 1;
        match len {
            Len::Int(t) => b.int_of(v, t.name()),
            Len::Uvarint => b.int_of(v, "u64"),
            Len::Varint => b.int_of(v, "i64"),
        }
    }

    /// The length element of a prefixed value or array.
    fn len_elem(len: Len) -> Elem {
        match len {
            Len::Int(t) => Elem::Int(t),
            Len::Uvarint => Elem::Uvarint,
            Len::Varint => Elem::Varint,
        }
    }

    /// A call of compound element `e`'s generated decoder (`dec_fn`).
    fn dec_call(&mut self, e: &Elem, c: Compound<'_>, buf: &Expr, pos: &Expr) -> Expr {
        let f = self.dec_fn(e, c);
        let mut args = vec![buf.clone(), pos.clone()];
        args.extend(self.param_args());
        self.b.call(&f, args)
    }

    /// The generated decoder of a compound element: `(b: Bytes, p: u64, params…) -> Option<(T, u64)>`.
    fn dec_fn(&mut self, e: &Elem, c: Compound<'_>) -> String {
        let b = self.b;
        let name = self.fresh('d');
        let (buf, pos) = (b.var("fmt$b"), b.var("fmt$p"));
        let body = match c {
            Compound::Prefixed(len, bias, inner) => {
                let raw = self.dec(&Self::len_elem(len), &buf, &pos);
                let n = self.len_of(len, bias, b.var("fmt$raw"));
                let slice = b.call("format$bytes", vec![buf.clone(), b.var("fmt$q"), b.var("fmt$n")]);
                let whole = match inner {
                    Elem::Utf8 => b.m(b.var("fmt$s"), "from_utf8", Vec::new()),
                    Elem::Rest => b.some(b.var("fmt$s")),
                    other => {
                        let read = self.dec(other, &b.var("fmt$s"), &b.int(0, Some("u64")));
                        let all = b.ifx(
                            b.bin(
                                BinOp::Eq,
                                b.tidx(b.var("fmt$w"), 1),
                                b.m(b.var("fmt$s"), "len", Vec::new()),
                            ),
                            b.some(b.tidx(b.var("fmt$w"), 0)),
                            b.none(),
                        );
                        b.m(read, "and_then", vec![b.clos(&["fmt$w"], all)])
                    }
                };
                b.block(
                    vec![
                        (b.tpat(&["fmt$raw", "fmt$q"]), b.try_(raw)),
                        (b.var("fmt$n"), b.try_(n)),
                        (b.tpat(&["fmt$s", "fmt$e"]), b.try_(slice)),
                        (b.var("fmt$v"), b.try_(whole)),
                    ],
                    b.some(b.tpat(&["fmt$v", "fmt$e"])),
                )
            }
            Compound::Array(len, bias, item) => {
                let raw = self.dec(&Self::len_elem(len), &buf, &pos);
                let n = self.len_of(len, bias, b.var("fmt$raw"));
                // No item takes fewer than one byte (checked when the array is read, `min_width`), so a count beyond the bytes left is malformed: a hostile count
                // costs nothing.
                let fits = b.call(
                    "format$check",
                    vec![b.bin(
                        BinOp::Le,
                        b.var("fmt$n"),
                        b.call("format$left", vec![buf.clone(), b.var("fmt$q")]),
                    )],
                );
                let step = {
                    let read = self.dec(item, &buf, &b.tidx(b.var("fmt$a"), 1));
                    let wrap = b.clos(
                        &["fmt$r"],
                        b.tup(vec![
                            b.e(ExprKind::Vec(vec![b.tidx(b.var("fmt$r"), 0)])),
                            b.tidx(b.var("fmt$r"), 1),
                        ]),
                    );
                    let next = b.m(read, "map", vec![wrap]);
                    b.clos(
                        &["fmt$acc", "_fmt_i"],
                        b.m(b.var("fmt$acc"), "and_then", vec![b.clos(&["fmt$a"], next)]),
                    )
                };
                let init = b.some(b.tup(vec![b.e(ExprKind::Vec(Vec::new())), b.var("fmt$q")]));
                let steps = b.m(
                    b.call("range", vec![b.int(0, Some("u64")), b.var("fmt$n")]),
                    "scan",
                    vec![init, step],
                );
                b.block(
                    vec![
                        (b.tpat(&["fmt$raw", "fmt$q"]), b.try_(raw)),
                        (b.var("fmt$n"), b.try_(n)),
                        (b.var("_fmt_fits"), b.try_(fits)),
                        (b.var("fmt$steps"), steps),
                    ],
                    b.call("format$gather", vec![b.var("fmt$steps"), b.var("fmt$q")]),
                )
            }
            Compound::Nullable(len, bias, inner) => {
                let raw = self.dec(&Self::len_elem(len), &buf, &pos);
                let present = {
                    let read = self.dec(inner, &buf, &pos);
                    let wrap = b.clos(
                        &["fmt$r"],
                        b.tup(vec![b.some(b.tidx(b.var("fmt$r"), 0)), b.tidx(b.var("fmt$r"), 1)]),
                    );
                    b.m(read, "map", vec![wrap])
                };
                b.block(
                    vec![(b.tpat(&["fmt$raw", "fmt$q"]), b.try_(raw))],
                    b.ifx(
                        b.bin(BinOp::Eq, b.var("fmt$raw"), self.null_raw(len, bias)),
                        b.some(b.tup(vec![b.none(), b.var("fmt$q")])),
                        present,
                    ),
                )
            }
            Compound::Tuple(xs) => {
                let mut lets = Vec::new();
                let mut values = Vec::new();
                let mut at = "fmt$p".to_string();
                for (i, x) in xs.iter().enumerate() {
                    let next = format!("fmt$q{i}");
                    let read = self.dec(x, &buf, &b.var(&at));
                    if x.valued() {
                        let v = format!("fmt$t{i}");
                        lets.push((b.tpat(&[&v, &next]), b.try_(read)));
                        values.push(b.var(&v));
                    } else {
                        lets.push((b.var(&next), b.try_(read)));
                    }
                    at = next;
                }
                let value = if values.len() == 1 {
                    values.remove(0)
                } else {
                    b.tup(values)
                };
                b.block(lets, b.some(b.tup(vec![value, b.var(&at)])))
            }
        };
        let mut params = vec![(b.id("fmt$b"), b.ty("Bytes")), (b.id("fmt$p"), b.ty("u64"))];
        params.extend(self.param_decls());
        let ret = b.ty_app("Option", vec![b.ty_tuple(vec![self.value_ty(e), b.ty("u64")])]);
        let item = b.func(&name, &[], params, ret, body, &mut self.diags);
        self.out.push(item);
        name
    }

    /// Encoding the value `v` of `e` (for a valueless element, its fixed bytes).
    fn enc(&mut self, e: &Elem, v: &Expr) -> Expr {
        let b = self.b;
        match e {
            Elem::Int(t) => {
                let (_, from, _) = int_access(*t);
                b.call(&from, vec![v.clone()])
            }
            Elem::Bool => b.call(
                "Bytes::from_u8",
                vec![b.ifx(v.clone(), b.int(1, Some("u8")), b.int(0, Some("u8")))],
            ),
            Elem::Uvarint => b.call("Bytes::uvarint", vec![v.clone()]),
            Elem::Varint => b.call("Bytes::varint", vec![v.clone()]),
            Elem::Bytes(n) => b.call("format$exact", vec![v.clone(), n.clone()]),
            Elem::Rest => v.clone(),
            Elem::Utf8 => b.m(v.clone(), "to_utf8", Vec::new()),
            Elem::Prefixed { len, bias, inner } => self.enc_call(e, Compound::Prefixed(*len, *bias, inner), v),
            Elem::Array { len, bias, item } => self.enc_call(e, Compound::Array(*len, *bias, item), v),
            Elem::Nullable { len, bias, inner } => self.enc_call(e, Compound::Nullable(*len, *bias, inner), v),
            Elem::Tuple(xs) => self.enc_call(e, Compound::Tuple(xs), v),
            Elem::Format { name, args } => {
                let mut xs = vec![v.clone()];
                xs.extend(args.iter().cloned());
                b.call(&format!("{}::encode", name.as_str()), xs)
            }
            Elem::Constant { elem, value } | Elem::Ignored { elem, value } => self.enc(elem, value),
            Elem::Tags => b.call("Bytes::uvarint", vec![b.int(0, Some("u64"))]),
            Elem::Select { cond, then, els } => {
                let (t, f) = (self.enc(then, v), self.enc(els, v));
                b.ifx(cond.clone(), t, f)
            }
        }
    }

    /// Encoding a length `n` (a `u64`) plus the bias, as `len` writes it.
    fn len_enc(&self, len: Len, bias: u64, n: Expr) -> Expr {
        let b = self.b;
        let raw = b.bin(BinOp::Add, n, b.int(u128::from(bias), Some("u64")));
        match len {
            Len::Uvarint => b.call("Bytes::uvarint", vec![raw]),
            Len::Varint => b.call("Bytes::varint", vec![b.cast(raw, "i64")]),
            Len::Int(t) => {
                let (_, from, _) = int_access(t);
                b.call(&from, vec![b.cast(raw, t.name())])
            }
        }
    }

    /// A call of compound element `e`'s generated encoder (`enc_fn`).
    fn enc_call(&mut self, e: &Elem, c: Compound<'_>, v: &Expr) -> Expr {
        let f = self.enc_fn(e, c);
        let mut args = vec![v.clone()];
        args.extend(self.param_args());
        self.b.call(&f, args)
    }

    /// The generated encoder of a compound element: `(v: T, params…) -> Bytes`.
    fn enc_fn(&mut self, e: &Elem, c: Compound<'_>) -> String {
        let b = self.b;
        let name = self.fresh('e');
        let v = b.var("fmt$v");
        let body = match c {
            Compound::Prefixed(len, bias, inner) => {
                let bytes = match inner {
                    Elem::Utf8 => b.m(v.clone(), "to_utf8", Vec::new()),
                    Elem::Rest => v.clone(),
                    other => self.enc(other, &v),
                };
                let head = self.len_enc(len, bias, b.m(b.var("fmt$i"), "len", Vec::new()));
                b.block(vec![(b.var("fmt$i"), bytes)], b.m(head, "concat", vec![b.var("fmt$i")]))
            }
            Compound::Array(len, bias, item) => {
                let head = self.len_enc(len, bias, b.m(v.clone(), "len", Vec::new()));
                let each = self.enc(item, &b.var("fmt$x"));
                let items = b.call(
                    "Bytes::join",
                    vec![b.m(v.clone(), "map", vec![b.clos(&["fmt$x"], each)])],
                );
                b.m(head, "concat", vec![items])
            }
            Compound::Nullable(len, bias, inner) => {
                let present = self.enc(inner, &b.var("fmt$x"));
                let null = match len {
                    Len::Uvarint => b.call("Bytes::uvarint", vec![self.null_raw(len, bias)]),
                    Len::Varint => b.call("Bytes::varint", vec![self.null_raw(len, bias)]),
                    Len::Int(t) => b.call(&int_access(t).1, vec![self.null_raw(len, bias)]),
                };
                b.mat(v.clone(), vec![(b.some(b.var("fmt$x")), present), (b.none(), null)])
            }
            Compound::Tuple(xs) => {
                let valued = xs.iter().filter(|x| x.valued()).count();
                let mut k = 0;
                let mut parts = Vec::new();
                for x in xs {
                    if x.valued() {
                        let part = if valued == 1 { v.clone() } else { b.tidx(v.clone(), k) };
                        k += 1;
                        parts.push(self.enc(x, &part));
                    } else {
                        parts.push(self.enc(x, &b.tup(Vec::new())));
                    }
                }
                b.call("Bytes::join", vec![b.e(ExprKind::Vec(parts))])
            }
        };
        let mut params = vec![(b.id("fmt$v"), self.value_ty(e))];
        params.extend(self.param_decls());
        let item = b.func(&name, &[], params, b.ty("Bytes"), body, &mut self.diags);
        self.out.push(item);
        name
    }
}

/// A record format's struct, decoder, encoder and sub-functions.
fn record(f: &FormatItem, fields: &[FormatField], env: &Env, diags: &mut Diagnostics) -> Option<Vec<ItemKind>> {
    let b = B { span: f.span };
    let mut params = Vec::new();
    for (p, t) in &f.params {
        match t {
            Some(t) => params.push((*p, t.clone())),
            None => {
                diags.push(
                    Diagnostic::new(
                        code!("BLS0301"),
                        "a record format's parameters are values: `name: Type`",
                    )
                    .with_primary(p.span),
                );
                return None;
            }
        }
    }
    let mut elems = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for p in &params {
        names.insert(p.0.name);
    }
    let mut ok = true;
    for fd in fields {
        let Some(e) = elem(&fd.elem, env, 0, diags) else {
            ok = false;
            continue;
        };
        match (&fd.name, e.valued()) {
            (Some(n), true) => {
                if !names.insert(n.name) {
                    diags.push(
                        Diagnostic::new(
                            code!("BLS0201"),
                            format!("`{}` is a field or parameter of this format twice", n.as_str()),
                        )
                        .with_primary(n.span),
                    );
                    ok = false;
                }
            }
            (Some(n), false) => {
                diags.push(
                    Diagnostic::new(
                        code!("BLS0301"),
                        "`constant(…)`, `ignored(…)` and `tags` have no value to keep in a field",
                    )
                    .with_primary(n.span),
                );
                ok = false;
            }
            (None, true) => {
                diags.push(
                    Diagnostic::new(
                        code!("BLS0301"),
                        "an element with a value needs a field name (only `constant(…)`, `ignored(…)` and `tags` go without)",
                    )
                    .with_primary(fd.span),
                );
                ok = false;
            }
            (None, false) => {}
        }
        // The generated functions are not metered (LANGUAGE §16.7): an expression of the program's in a field (an
        // element's argument, a condition, a default) is evaluated per value, so it may not loop on its own.
        if let Some(span) = [Some(&fd.elem), fd.cond.as_ref(), fd.default.as_ref()]
            .into_iter()
            .flatten()
            .find_map(unbounded)
        {
            diags.push(
                Diagnostic::new(
                    code!("BLS0301"),
                    "a format's element arguments, conditions and defaults take no closure and no `range` (they \
                     are evaluated for every value, outside the step budget); call a function, which is metered",
                )
                .with_primary(span),
            );
            ok = false;
        }
        if fd.default.is_some() && (fd.cond.is_none() || !e.valued()) {
            diags.push(
                Diagnostic::new(
                    code!("BLS0301"),
                    "only a conditional field has a default (its value when the condition does not hold)",
                )
                .with_primary(fd.span),
            );
            ok = false;
        }
        elems.push((fd, e));
    }
    if let Some((fd, _)) = elems.iter().rev().skip(1).find(|(_, e)| consumes_all(e, env, 0)) {
        diags.push(Diagnostic::new(code!("BLS0301"), REST_LAST).with_primary(fd.span));
        ok = false;
    }
    if !ok {
        return None;
    }
    let name = f.name.as_str();
    let mut g = Gen {
        b,
        name,
        params: &params,
        out: Vec::new(),
        next: 0,
        diags: Diagnostics::new(),
    };
    if let Some(cond) = elems.iter().find_map(|(_, e)| g.select_mismatch(e)) {
        diags.push(
            Diagnostic::new(
                code!("BLS0301"),
                "`select`'s two elements have values of different types",
            )
            .with_primary(cond.span),
        );
        return None;
    }
    // The struct.
    let mut struct_fields = Vec::new();
    for (fd, e) in &elems {
        if let Some(n) = fd.name {
            struct_fields.push(FieldDecl {
                attrs: Vec::new(),
                name: Some(n),
                ty: g.value_ty(e),
                span: fd.span,
            });
        }
    }
    // The decoder: each element at the position the one before it ended.
    let buf = b.var("fmt$b");
    let mut lets = Vec::new();
    let mut pos = "fmt$p0".to_string();
    for (i, (fd, e)) in elems.iter().enumerate() {
        let next = format!("fmt$p{}", i + 1);
        let read = g.dec(e, &buf, &b.var(&pos));
        let absent = if e.valued() {
            let value = match (&fd.default, g.zero(e)) {
                (Some(d), _) => d.clone(),
                (None, Some(z)) => z,
                (None, None) => {
                    if fd.cond.is_some() {
                        diags.push(
                            Diagnostic::new(
                                code!("BLS0301"),
                                "a conditional field of a format type needs `= default`",
                            )
                            .with_primary(fd.span),
                        );
                        return None;
                    }
                    b.none()
                }
            };
            b.some(b.tup(vec![value, b.var(&pos)]))
        } else {
            b.some(b.var(&pos))
        };
        let value = match &fd.cond {
            Some(c) => b.ifx(c.clone(), read, absent),
            None => read,
        };
        let pat = match fd.name {
            Some(n) => b.tup(vec![b.var(n.as_str()), b.var(&next)]),
            None => b.var(&next),
        };
        lets.push((pat, b.try_(value)));
        pos = next;
    }
    let lit = b.e(ExprKind::StructLit {
        base: None,
        path: vec![f.name],
        fields: elems
            .iter()
            .filter_map(|(fd, _)| fd.name.map(|n| (n, Some(b.var(n.as_str())))))
            .collect(),
    });
    let decode_body = b.block(lets, b.some(b.tup(vec![lit, b.var(&pos)])));
    // The encoder: the fields' bytes in order, a conditional one only when its condition holds.
    let x = b.var("fmt$x");
    let mut enc_lets = Vec::new();
    let mut parts = Vec::new();
    for (fd, e) in &elems {
        let value = match fd.name {
            Some(n) => {
                enc_lets.push((b.var(n.as_str()), b.field(x.clone(), n)));
                b.var(n.as_str())
            }
            None => b.tup(Vec::new()),
        };
        let bytes = g.enc(e, &value);
        parts.push(match &fd.cond {
            Some(c) => b.ifx(c.clone(), bytes, b.call("Bytes::empty", Vec::new())),
            None => bytes,
        });
    }
    let encode_body = b.block(enc_lets, b.call("Bytes::join", vec![b.e(ExprKind::Vec(parts))]));
    let mut dparams = vec![(b.id("fmt$b"), b.ty("Bytes")), (b.id("fmt$p0"), b.ty("u64"))];
    dparams.extend(params.iter().cloned());
    let mut eparams = vec![(b.id("fmt$x"), b.ty(name))];
    eparams.extend(params.iter().cloned());
    let decode = b.func(
        &format!("{name}::decode"),
        &[],
        dparams,
        b.ty_app("Option", vec![b.ty_tuple(vec![b.ty(name), b.ty("u64")])]),
        decode_body,
        diags,
    );
    let encode = b.func(
        &format!("{name}::encode"),
        &[],
        eparams,
        b.ty("Bytes"),
        encode_body,
        diags,
    );
    let mut out = vec![
        ItemKind::Struct(StructItem {
            name: f.name,
            generics: Vec::new(),
            fields: struct_fields,
            tuple: false,
        }),
        decode,
        encode,
    ];
    out.append(&mut g.out);
    for d in g.diags.into_vec() {
        diags.push(d);
    }
    Some(out)
}

/// The helpers every scope with a record gets.
fn helpers(span: Span, diags: &mut Diagnostics) -> Vec<ItemKind> {
    let b = B { span };
    let (bv, p, n) = (b.var("b"), b.var("p"), b.var("n"));
    let pb = |name: &str, t: &str| (b.id(name), b.ty(t));
    let opt = |t: Type| b.ty_app("Option", vec![t]);
    let len = b.m(bv.clone(), "len", Vec::new());
    let mut out = Vec::new();
    // The bytes left after `p` (none past the end).
    out.push(b.func(
        "format$left",
        &[],
        vec![pb("b", "Bytes"), pb("p", "u64")],
        b.ty("u64"),
        b.ifx(
            b.bin(BinOp::Gt, p.clone(), len.clone()),
            b.int(0, Some("u64")),
            b.bin(BinOp::Sub, len.clone(), p.clone()),
        ),
        diags,
    ));
    // The `n` bytes at `p`, and the position after them; `n` is checked against the bytes left first.
    let p_n = b.bin(BinOp::Add, p.clone(), n.clone());
    out.push(b.func(
        "format$bytes",
        &[],
        vec![pb("b", "Bytes"), pb("p", "u64"), pb("n", "u64")],
        opt(b.ty_tuple(vec![b.ty("Bytes"), b.ty("u64")])),
        b.ifx(
            b.bin(BinOp::Gt, n.clone(), b.call("format$left", vec![bv.clone(), p.clone()])),
            b.none(),
            b.m(
                b.m(bv.clone(), "slice", vec![p.clone(), p_n.clone()]),
                "map",
                vec![b.clos(&["s"], b.tup(vec![b.var("s"), p_n.clone()]))],
            ),
        ),
        diags,
    ));
    // The `n` bytes at `p` as a string.
    out.push(b.func(
        "format$utf8",
        &[],
        vec![pb("b", "Bytes"), pb("p", "u64"), pb("n", "u64")],
        opt(b.ty_tuple(vec![b.ty("String"), b.ty("u64")])),
        b.block(
            vec![
                (
                    b.tpat(&["s", "e"]),
                    b.try_(b.call("format$bytes", vec![bv.clone(), p.clone(), n.clone()])),
                ),
                (b.var("t"), b.try_(b.m(b.var("s"), "from_utf8", Vec::new()))),
            ],
            b.some(b.tpat(&["t", "e"])),
        ),
        diags,
    ));
    out.push(b.func(
        "format$check",
        &[],
        vec![pb("ok", "bool")],
        opt(b.ty("bool")),
        b.ifx(b.var("ok"), b.some(b.e(ExprKind::Lit(LitValue::Bool(true)))), b.none()),
        diags,
    ));
    // A declared length less its bias; below the bias, malformed.
    for (name, t, cast) in [("format$ulen", "u64", false), ("format$ilen", "i64", true)] {
        let diff = b.bin(BinOp::Sub, b.var("raw"), b.var("bias"));
        out.push(b.func(
            name,
            &[],
            vec![pb("raw", t), pb("bias", t)],
            opt(b.ty("u64")),
            b.ifx(
                b.bin(BinOp::Lt, b.var("raw"), b.var("bias")),
                b.none(),
                b.some(if cast { b.cast(diff, "u64") } else { diff }),
            ),
            diags,
        ));
    }
    // A fixed-size element's value must have its size: anything else is a program error, never a short write.
    out.push(b.func(
        "format$exact",
        &[],
        vec![pb("v", "Bytes"), pb("n", "u64")],
        b.ty("Bytes"),
        b.ifx(
            b.bin(BinOp::Eq, b.m(b.var("v"), "len", Vec::new()), n.clone()),
            b.var("v"),
            b.call(
                "error",
                vec![b.e(ExprKind::Lit(LitValue::Str(
                    "format: a fixed-size element given a value of another size".into(),
                )))],
            ),
        ),
        diags,
    ));
    // A tagged-field section: a count, then per field its tag, its size and its bytes, all skipped.
    {
        let skip_one = b.m(
            b.m(bv.clone(), "uvarint_at", vec![b.var("x")]),
            "and_then",
            vec![b.clos(
                &["t"],
                b.m(
                    b.m(bv.clone(), "uvarint_at", vec![b.tidx(b.var("t"), 1)]),
                    "and_then",
                    vec![b.clos(
                        &["m"],
                        b.m(
                            b.call(
                                "format$bytes",
                                vec![bv.clone(), b.tidx(b.var("m"), 1), b.tidx(b.var("m"), 0)],
                            ),
                            "map",
                            vec![b.clos(&["r"], b.tidx(b.var("r"), 1))],
                        ),
                    )],
                ),
            )],
        );
        let step = b.clos(
            &["acc", "_i"],
            b.m(b.var("acc"), "and_then", vec![b.clos(&["x"], skip_one)]),
        );
        let steps = b.m(
            b.call("range", vec![b.int(0, Some("u64")), b.var("k")]),
            "scan",
            vec![b.some(b.var("q")), step],
        );
        let body = b.block(
            vec![
                (
                    b.tpat(&["k", "q"]),
                    b.try_(b.m(bv.clone(), "uvarint_at", vec![p.clone()])),
                ),
                (
                    b.var("_fits"),
                    b.try_(b.call(
                        "format$check",
                        vec![b.bin(
                            BinOp::Le,
                            b.var("k"),
                            b.call("format$left", vec![bv.clone(), b.var("q")]),
                        )],
                    )),
                ),
                (b.var("steps"), steps),
            ],
            b.mat(
                b.m(b.var("steps"), "last", Vec::new()),
                vec![(b.some(b.var("end")), b.var("end")), (b.none(), b.some(b.var("q")))],
            ),
        );
        out.push(b.func(
            "format$skip_tags",
            &[],
            vec![pb("b", "Bytes"), pb("p", "u64")],
            opt(b.ty("u64")),
            body,
            diags,
        ));
    }
    // An array's steps (the accumulator after each item): every item, and the position after the last.
    {
        let t = b.ty("T");
        let step_ty = opt(b.ty_tuple(vec![b.ty_app("Vec", vec![t.clone()]), b.ty("u64")]));
        let steps = b.var("steps");
        let any_none = b.m(
            steps.clone(),
            "any",
            vec![b.clos(&["s"], b.m(b.var("s"), "is_none", Vec::new()))],
        );
        let items = b.m(
            b.m(
                steps.clone(),
                "filter_map",
                vec![b.clos(
                    &["s"],
                    b.m(b.var("s"), "map", vec![b.clos(&["x"], b.tidx(b.var("x"), 0))]),
                )],
            ),
            "flatten",
            Vec::new(),
        );
        let last = b.mat(
            b.m(steps.clone(), "last", Vec::new()),
            vec![
                (
                    b.some(b.some(b.var("s"))),
                    b.some(b.tup(vec![b.var("items"), b.tidx(b.var("s"), 1)])),
                ),
                (
                    b.e(ExprKind::Wildcard),
                    b.some(b.tup(vec![b.var("items"), b.var("start")])),
                ),
            ],
        );
        out.push(b.func(
            "format$gather",
            &["T"],
            vec![
                (b.id("steps"), b.ty_app("Vec", vec![step_ty.clone()])),
                pb("start", "u64"),
            ],
            step_ty,
            b.ifx(any_none, b.none(), b.block(vec![(b.var("items"), items)], last)),
            diags,
        ));
    }
    out
}
