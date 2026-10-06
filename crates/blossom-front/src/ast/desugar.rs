//! Desugaring of function bodies before name resolution (EXTENSIONS 2.1).
//!
//! `e?` in a function returning `Option` is `e`'s value when it is `Some`; otherwise the function returns `None`. The
//! body is rewritten into nested `match`es: each `?` in a strict position (one evaluated whenever its `let` or the
//! result is) is hoisted into a `match` on its operand, in evaluation order, whose `Some` arm binds a fresh name the
//! expression then uses, and whose `None` arm is the function's `None`:
//!
//! ```text
//! let (a, c) = f(g(x)?)?; rest      ⇒   match g(x) { Some(t1) => match f(t1) { Some(t2) => { let (a, c) = t2; rest }
//!                                                                             None => None }
//!                                                    None => None }
//! ```
//!
//! A `?` under a branch (`if`, a `match` arm, the right of `&&`/`||`), in a nested block or a closure cannot return
//! early this way (hoisting it would evaluate it when the branch is not taken, or outside the names it uses): it is
//! BLS0218, as is a `?` in a function whose result is not an `Option`.

use blossom_base::{Diagnostic, Diagnostics, Span, Symbol, code};

use super::*;

/// Rewrites the `?`s of a function's body into `match`es, reporting those that cannot be rewritten.
pub(super) fn fn_body(item: &mut FnItem, diags: &mut Diagnostics) {
    if !has_try(&item.body) {
        return;
    }
    let returns_option =
        matches!(&item.ret, Type::Named { path, .. } if path.last().is_some_and(|p| p.as_str() == "Option"));
    if !returns_option {
        for span in try_spans(&item.body) {
            diags.push(
                Diagnostic::new(
                    code!("BLS0218"),
                    "`?` returns `None` from its function early: this function's result is not an `Option`",
                )
                .with_primary(span),
            );
        }
        return;
    }
    let body = std::mem::replace(&mut item.body, Expr::new(ExprKind::Wildcard, item.span));
    let mut d = Desugar { fresh: 0, diags };
    item.body = d.body(body);
}

struct Desugar<'d> {
    fresh: u32,
    diags: &'d mut Diagnostics,
}

impl Desugar<'_> {
    fn body(&mut self, e: Expr) -> Expr {
        let span = e.span;
        match e.kind {
            ExprKind::Block { lets, result } => self.block(lets, *result, span),
            kind => self.block(Vec::new(), Expr::new(kind, span), span),
        }
    }

    /// `let`s in order, then the result; each `let` (and the result) under the `match`es of its `?`s.
    fn block(&mut self, mut lets: Vec<BlockLet>, result: Expr, span: Span) -> Expr {
        if lets.is_empty() {
            let mut binds = Vec::new();
            let r = self.extract(result, &mut binds);
            return wrap(binds, r);
        }
        let first = lets.remove(0);
        let mut binds = Vec::new();
        let value = self.extract(first.value, &mut binds);
        let rest = self.block(lets, result, span);
        let inner = Expr::new(
            ExprKind::Block {
                lets: vec![BlockLet {
                    pat: first.pat,
                    ty: first.ty,
                    value,
                    span: first.span,
                }],
                result: Box::new(rest),
            },
            span,
        );
        wrap(binds, inner)
    }

    fn fresh(&mut self, span: Span) -> Ident {
        self.fresh += 1;
        Ident {
            name: Symbol::intern(&format!("try${}", self.fresh)),
            span,
        }
    }

    /// `e` with each `?` in a strict position replaced by a fresh name, the names and their operands appended to
    /// `binds` in evaluation order.
    fn extract(&mut self, e: Expr, binds: &mut Vec<(Ident, Expr)>) -> Expr {
        let span = e.span;
        let kind = match e.kind {
            ExprKind::Try(inner) => {
                let inner = self.extract(*inner, binds);
                let name = self.fresh(span);
                binds.push((name, inner));
                ExprKind::Path(vec![name], Vec::new())
            }
            k @ (ExprKind::Lit(_) | ExprKind::Path(..) | ExprKind::Wildcard | ExprKind::SelfNode) => k,
            ExprKind::Call { callee, args } => {
                let callee = Box::new(self.extract(*callee, binds));
                ExprKind::Call {
                    callee,
                    args: self.args(args, binds),
                }
            }
            ExprKind::Method { receiver, name, args } => {
                let receiver = Box::new(self.extract(*receiver, binds));
                ExprKind::Method {
                    receiver,
                    name,
                    args: self.args(args, binds),
                }
            }
            ExprKind::Field { base, name } => ExprKind::Field {
                base: Box::new(self.extract(*base, binds)),
                name,
            },
            ExprKind::TupleIndex { base, index } => ExprKind::TupleIndex {
                base: Box::new(self.extract(*base, binds)),
                index,
            },
            ExprKind::Index { base, index } => {
                let base = Box::new(self.extract(*base, binds));
                ExprKind::Index {
                    base,
                    index: Box::new(self.extract(*index, binds)),
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let lhs = Box::new(self.extract(*lhs, binds));
                // The right of `&&` and `||` is evaluated only sometimes.
                let rhs = if matches!(op, BinOp::And | BinOp::Or) {
                    Box::new(self.forbid(*rhs))
                } else {
                    Box::new(self.extract(*rhs, binds))
                };
                ExprKind::Binary { op, lhs, rhs }
            }
            ExprKind::Prefix { op, arg } => ExprKind::Prefix {
                op,
                arg: Box::new(self.extract(*arg, binds)),
            },
            ExprKind::Cast { expr, ty } => ExprKind::Cast {
                expr: Box::new(self.extract(*expr, binds)),
                ty,
            },
            ExprKind::Tuple(xs) => ExprKind::Tuple(self.all(xs, binds)),
            ExprKind::Vec(xs) => ExprKind::Vec(self.all(xs, binds)),
            ExprKind::Set(xs) => ExprKind::Set(self.all(xs, binds)),
            ExprKind::Map(kvs) => ExprKind::Map(
                kvs.into_iter()
                    .map(|(k, v)| {
                        let k = self.extract(k, binds);
                        (k, self.extract(v, binds))
                    })
                    .collect(),
            ),
            ExprKind::StructLit { path, fields, base } => ExprKind::StructLit {
                path,
                fields: fields
                    .into_iter()
                    .map(|(n, v)| (n, v.map(|v| self.extract(v, binds))))
                    .collect(),
                base: base.map(|x| Box::new(self.extract(*x, binds))),
            },
            ExprKind::If { cond, then, els } => {
                let cond = Box::new(self.extract(*cond, binds));
                ExprKind::If {
                    cond,
                    then: Box::new(self.forbid(*then)),
                    els: els.map(|e| Box::new(self.forbid(*e))),
                }
            }
            ExprKind::Match { scrut, arms } => {
                let scrut = Box::new(self.extract(*scrut, binds));
                ExprKind::Match {
                    scrut,
                    arms: arms
                        .into_iter()
                        .map(|a| MatchArm {
                            pat: self.forbid(a.pat),
                            guard: a.guard.map(|g| self.forbid(g)),
                            body: self.forbid(a.body),
                        })
                        .collect(),
                }
            }
            k @ (ExprKind::Bang { .. } | ExprKind::Block { .. } | ExprKind::Closure { .. }) => {
                self.forbid(Expr::new(k, span)).kind
            }
        };
        Expr::new(kind, span)
    }

    fn all(&mut self, xs: Vec<Expr>, binds: &mut Vec<(Ident, Expr)>) -> Vec<Expr> {
        xs.into_iter().map(|x| self.extract(x, binds)).collect()
    }

    fn args(&mut self, args: Vec<Arg>, binds: &mut Vec<(Ident, Expr)>) -> Vec<Arg> {
        args.into_iter()
            .map(|a| match a {
                Arg::Pos(e) => Arg::Pos(self.extract(e, binds)),
                Arg::Named(n, e) => Arg::Named(n, self.extract(e, binds)),
                other => other,
            })
            .collect()
    }

    /// A position that is evaluated only sometimes, or under names of its own: a `?` in it is reported (and left,
    /// so name resolution sees no hole).
    fn forbid(&mut self, e: Expr) -> Expr {
        for span in try_spans(&e) {
            self.diags.push(
                Diagnostic::new(
                    code!("BLS0218"),
                    "`?` here cannot return early: it is under a branch, the right of `&&`/`||`, a nested block or a \
                     closure (bind it with a `let` before)",
                )
                .with_primary(span),
            );
        }
        e
    }
}

/// `inner` under a `match` per bound `?`, the first outermost.
fn wrap(binds: Vec<(Ident, Expr)>, inner: Expr) -> Expr {
    binds.into_iter().rev().fold(inner, |body, (name, scrut)| {
        let span = scrut.span;
        let path = |s: &str| {
            Expr::new(
                ExprKind::Path(
                    vec![Ident {
                        name: Symbol::intern(s),
                        span,
                    }],
                    Vec::new(),
                ),
                span,
            )
        };
        let some = Expr::new(
            ExprKind::Call {
                callee: Box::new(path("Some")),
                args: vec![Arg::Pos(Expr::new(ExprKind::Path(vec![name], Vec::new()), span))],
            },
            span,
        );
        Expr::new(
            ExprKind::Match {
                scrut: Box::new(scrut),
                arms: vec![
                    MatchArm {
                        pat: some,
                        guard: None,
                        body,
                    },
                    MatchArm {
                        pat: path("None"),
                        guard: None,
                        body: path("None"),
                    },
                ],
            },
            span,
        )
    })
}

fn has_try(e: &Expr) -> bool {
    !try_spans(e).is_empty()
}

/// The spans of every `?` in `e`, closures and blocks included.
fn try_spans(e: &Expr) -> Vec<Span> {
    let mut out = Vec::new();
    collect(e, &mut out);
    out
}

fn collect(e: &Expr, out: &mut Vec<Span>) {
    let mut each = |x: &Expr| collect(x, out);
    match &e.kind {
        ExprKind::Try(inner) => {
            out.push(e.span);
            collect(inner, out);
        }
        ExprKind::Lit(_) | ExprKind::Path(..) | ExprKind::Wildcard | ExprKind::SelfNode => {}
        ExprKind::Call { callee, args } => {
            each(callee);
            args_of(args).for_each(each);
        }
        ExprKind::Method { receiver, args, .. } => {
            each(receiver);
            args_of(args).for_each(each);
        }
        ExprKind::Bang { args, clauses, .. } => {
            args_of(args).for_each(&mut each);
            for c in clauses {
                c.exprs.iter().for_each(&mut each);
                c.order.iter().for_each(|(x, _)| each(x));
            }
        }
        ExprKind::Field { base, .. } | ExprKind::TupleIndex { base, .. } => each(base),
        ExprKind::Index { base, index } => {
            each(base);
            each(index);
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            each(lhs);
            each(rhs);
        }
        ExprKind::Prefix { arg, .. } => each(arg),
        ExprKind::Cast { expr, .. } => each(expr),
        ExprKind::Tuple(xs) | ExprKind::Vec(xs) | ExprKind::Set(xs) => xs.iter().for_each(each),
        ExprKind::Map(kvs) => {
            for (k, v) in kvs {
                each(k);
                each(v);
            }
        }
        ExprKind::StructLit { fields, base, .. } => {
            fields.iter().filter_map(|(_, v)| v.as_ref()).for_each(&mut each);
            base.iter().for_each(|x| each(x));
        }
        ExprKind::If { cond, then, els } => {
            each(cond);
            each(then);
            if let Some(e) = els {
                each(e);
            }
        }
        ExprKind::Match { scrut, arms } => {
            each(scrut);
            for a in arms {
                each(&a.pat);
                if let Some(g) = &a.guard {
                    each(g);
                }
                each(&a.body);
            }
        }
        ExprKind::Block { lets, result } => {
            for l in lets {
                each(&l.pat);
                each(&l.value);
            }
            each(result);
        }
        ExprKind::Closure { body, .. } => each(body),
    }
}

fn args_of(args: &[Arg]) -> impl Iterator<Item = &Expr> {
    args.iter().flat_map(|a| match a {
        Arg::Pos(e) | Arg::Named(_, e) | Arg::Spread(Spread::Expr(e, _)) => vec![e],
        Arg::Spread(Spread::Record(fields, _)) => fields.iter().map(|(_, e)| e).collect(),
        Arg::Rest(_) | Arg::Star(_) => Vec::new(),
    })
}
