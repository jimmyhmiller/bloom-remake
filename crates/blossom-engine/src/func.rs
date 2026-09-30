//! Pure functions (LANGUAGE §16.1) and the built-in library (Appendix B) in the engine. Written independently of the
//! oracle's (ARCH-16).
//!
//! A call evaluates in a frame of the callee's own variables, its parameters first. A `let` or a closure application
//! evaluates its body in a copy of the current frame with the new bindings set, so no binding is visible outside its
//! scope and a closure re-applied by a combinator never sees its previous application's bindings.

use std::sync::Arc;

use blossom_base::FnId;
use blossom_ir::core::{BuiltinFn, Expr, FnBody, FnRef, LibFn, Pattern};
use blossom_value::Value;
use blossom_value::value::IntValue;

use crate::expr::{Ctx, ExprError, ExprResult, bug, eval, truth};

pub(crate) fn call(cx: &Ctx<'_>, env: &[Option<Value>], f: FnId, args: &[Expr]) -> ExprResult<Value> {
    let Some(decl) = cx.program.fns.get(f) else {
        return Err(bug(format!("call of undeclared function {f:?}")));
    };
    let body = match &decl.body {
        FnBody::Ir(body) => body,
        _ => {
            return Err(ExprError::Eval(
                blossom_base::unimplemented_error!("LANG-181", "calls of `{}` in the engine", decl.name).into(),
            ));
        }
    };
    let mut frame: Vec<Option<Value>> = Vec::with_capacity(decl.vars.len());
    for a in args {
        frame.push(Some(eval(cx, env, a)?));
    }
    if frame.len() != decl.params.len() {
        return Err(bug(format!(
            "`{}` takes {} arguments, given {}",
            decl.name,
            decl.params.len(),
            frame.len()
        )));
    }
    frame.resize(decl.vars.len(), None);
    eval(cx, &frame, body)
}

pub(crate) fn let_expr(
    cx: &Ctx<'_>,
    env: &[Option<Value>],
    pat: &Pattern,
    value: &Expr,
    body: &Expr,
) -> ExprResult<Value> {
    let v = eval(cx, env, value)?;
    let mut frame = env.to_vec();
    set(&mut frame, pat, &v)?;
    eval(cx, &frame, body)
}

/// Writes an irrefutable pattern's bindings into `frame`.
fn set(frame: &mut [Option<Value>], pat: &Pattern, v: &Value) -> ExprResult<()> {
    match pat {
        Pattern::Wild => Ok(()),
        Pattern::Var(x) => match frame.get_mut(x.index()) {
            Some(slot) => {
                *slot = Some(v.clone());
                Ok(())
            }
            None => Err(bug(format!("binding {x:?} outside the frame"))),
        },
        Pattern::Tuple(ps) => {
            let Value::Tuple(fs) = v else {
                return Err(bug(format!("a tuple pattern over {v:?}")));
            };
            if ps.len() != fs.len() {
                return Err(bug(format!("a {}-tuple pattern over {v:?}", ps.len())));
            }
            ps.iter().zip(fs.iter()).try_for_each(|(p, f)| set(frame, p, f))
        }
        other => Err(bug(format!("a refutable `let` pattern {other:?}"))),
    }
}

/// A closure argument, applied by a combinator.
struct Closure<'e> {
    params: &'e [blossom_base::VarId],
    body: &'e Expr,
}

impl<'e> Closure<'e> {
    fn of(e: &'e Expr) -> ExprResult<Closure<'e>> {
        match e {
            Expr::Closure { params, body } => Ok(Closure { params, body }),
            other => Err(bug(format!("a combinator given {other:?} for its closure"))),
        }
    }

    fn call(&self, cx: &Ctx<'_>, env: &[Option<Value>], args: &[Value]) -> ExprResult<Value> {
        if args.len() != self.params.len() {
            return Err(bug(format!(
                "a {}-parameter closure given {}",
                self.params.len(),
                args.len()
            )));
        }
        let mut frame = env.to_vec();
        for (p, a) in self.params.iter().zip(args) {
            let Some(slot) = frame.get_mut(p.index()) else {
                return Err(bug(format!("closure parameter {p:?} outside the frame")));
            };
            *slot = Some(a.clone());
        }
        eval(cx, &frame, self.body)
    }
}

fn as_u64(v: Value) -> ExprResult<u64> {
    match v {
        Value::Int(IntValue::U64(n)) => Ok(n),
        other => Err(bug(format!("a u64 expected, found {other:?}"))),
    }
}

fn some_or_none(v: Option<Value>) -> Value {
    Value::Option(v.map(Arc::new))
}

/// Walks a combinator's receiver: `range(lo, hi)` directly (it is never built), or a vector.
fn each(
    cx: &Ctx<'_>,
    env: &[Option<Value>],
    recv: &Expr,
    mut step: impl FnMut(Value) -> ExprResult<bool>,
) -> ExprResult<()> {
    if let Expr::Call {
        f: FnRef::Builtin(BuiltinFn::Lib(LibFn::Range)),
        args,
    } = recv
    {
        let (Some(lo), Some(hi)) = (args.first(), args.get(1)) else {
            return Err(bug("`range` without its bounds".into()));
        };
        let (lo, hi) = (as_u64(eval(cx, env, lo)?)?, as_u64(eval(cx, env, hi)?)?);
        let mut i = lo;
        while i < hi {
            if !step(Value::Int(IntValue::U64(i)))? {
                break;
            }
            i += 1;
        }
        return Ok(());
    }
    let Value::Vec(xs) = eval(cx, env, recv)? else {
        return Err(bug("a vector combinator on a non-vector".into()));
    };
    for x in xs.iter() {
        if !step(x.clone())? {
            break;
        }
    }
    Ok(())
}

pub(crate) fn library(cx: &Ctx<'_>, env: &[Option<Value>], f: LibFn, args: &[Expr]) -> ExprResult<Value> {
    let expr = |i: usize| args.get(i).ok_or_else(|| bug(format!("{f:?} is missing argument {i}")));
    let value = |i: usize| -> ExprResult<Value> { eval(cx, env, expr(i)?) };
    let vector = |i: usize| -> ExprResult<Arc<[Value]>> {
        match value(i)? {
            Value::Vec(v) => Ok(v),
            other => Err(bug(format!("{f:?} expects a vector, found {other:?}"))),
        }
    };
    let optional = |v: Value| -> ExprResult<Option<Value>> {
        match v {
            Value::Option(o) => Ok(o.as_deref().cloned()),
            other => Err(bug(format!("{f:?} expects an option, found {other:?}"))),
        }
    };
    match f {
        LibFn::Range => {
            let (lo, hi) = (as_u64(value(0)?)?, as_u64(value(1)?)?);
            let n = hi.saturating_sub(lo);
            let mut out = Vec::with_capacity(usize::try_from(n).unwrap_or(0).min(1 << 16));
            let mut i = lo;
            while i < hi {
                out.push(Value::Int(IntValue::U64(i)));
                i += 1;
            }
            Ok(Value::Vec(out.into()))
        }
        LibFn::VecGet => {
            let xs = vector(0)?;
            let i = as_u64(value(1)?)?;
            Ok(some_or_none(usize::try_from(i).ok().and_then(|i| xs.get(i).cloned())))
        }
        LibFn::VecFirst => Ok(some_or_none(vector(0)?.first().cloned())),
        LibFn::VecLast => Ok(some_or_none(vector(0)?.last().cloned())),
        LibFn::VecPush => {
            let xs = vector(0)?;
            let mut out = xs.to_vec();
            out.push(value(1)?);
            Ok(Value::Vec(out.into()))
        }
        LibFn::VecConcat => {
            let mut out = vector(0)?.to_vec();
            out.extend(vector(1)?.iter().cloned());
            Ok(Value::Vec(out.into()))
        }
        LibFn::VecIsEmpty => Ok(Value::Bool(vector(0)?.is_empty())),
        LibFn::VecReverse => {
            let mut out = vector(0)?.to_vec();
            out.reverse();
            Ok(Value::Vec(out.into()))
        }
        LibFn::VecEnumerate => {
            let xs = vector(0)?;
            let mut out = Vec::with_capacity(xs.len());
            for (i, x) in xs.iter().enumerate() {
                let i = u64::try_from(i).map_err(|_| bug("a vector longer than u64".into()))?;
                out.push(Value::Tuple(Arc::from([Value::Int(IntValue::U64(i)), x.clone()])));
            }
            Ok(Value::Vec(out.into()))
        }
        LibFn::VecMap | LibFn::VecFilter | LibFn::VecFilterMap => {
            let c = Closure::of(expr(1)?)?;
            let mut out = Vec::new();
            each(cx, env, expr(0)?, |x| {
                let r = c.call(cx, env, std::slice::from_ref(&x))?;
                match f {
                    LibFn::VecMap => out.push(r),
                    LibFn::VecFilter => {
                        if truth(&r)? {
                            out.push(x);
                        }
                    }
                    _ => {
                        if let Some(y) = optional(r)? {
                            out.push(y);
                        }
                    }
                }
                Ok(true)
            })?;
            Ok(Value::Vec(out.into()))
        }
        LibFn::VecAll => {
            let c = Closure::of(expr(1)?)?;
            let mut all = true;
            each(cx, env, expr(0)?, |x| {
                all = truth(&c.call(cx, env, &[x])?)?;
                Ok(all)
            })?;
            Ok(Value::Bool(all))
        }
        LibFn::VecAny => {
            let c = Closure::of(expr(1)?)?;
            let mut any = false;
            each(cx, env, expr(0)?, |x| {
                any = truth(&c.call(cx, env, &[x])?)?;
                Ok(!any)
            })?;
            Ok(Value::Bool(any))
        }
        LibFn::VecFold => {
            let c = Closure::of(expr(2)?)?;
            let mut acc = Some(value(1)?);
            each(cx, env, expr(0)?, |x| {
                let prev = acc.take().ok_or_else(|| bug("a fold lost its accumulator".into()))?;
                acc = Some(c.call(cx, env, &[prev, x])?);
                Ok(true)
            })?;
            acc.ok_or_else(|| bug("a fold lost its accumulator".into()))
        }
        LibFn::OptIsSome => Ok(Value::Bool(optional(value(0)?)?.is_some())),
        LibFn::OptIsNone => Ok(Value::Bool(optional(value(0)?)?.is_none())),
        LibFn::OptUnwrapOr => match optional(value(0)?)? {
            Some(x) => Ok(x),
            None => value(1),
        },
        LibFn::OptMap | LibFn::OptAndThen => {
            let Some(x) = optional(value(0)?)? else {
                return Ok(Value::Option(None));
            };
            let r = Closure::of(expr(1)?)?.call(cx, env, &[x])?;
            Ok(if f == LibFn::OptMap {
                Value::Option(Some(Arc::new(r)))
            } else {
                r
            })
        }
        LibFn::BytesSlice => {
            let Value::Bytes(b) = value(0)? else {
                return Err(bug("`slice` of a non-Bytes value".into()));
            };
            let (lo, hi) = (as_u64(value(1)?)?, as_u64(value(2)?)?);
            let len = b.len() as u64;
            if lo > hi || hi > len {
                return Ok(Value::Option(None));
            }
            let (lo, hi) = (lo as usize, hi as usize);
            Ok(some_or_none(b.get(lo..hi).map(|s| Value::Bytes(Arc::from(s)))))
        }
        LibFn::BytesConcat => match (value(0)?, value(1)?) {
            (Value::Bytes(a), Value::Bytes(b)) => {
                let mut out = Vec::with_capacity(a.len() + b.len());
                out.extend_from_slice(&a);
                out.extend_from_slice(&b);
                Ok(Value::Bytes(out.into()))
            }
            (a, b) => Err(bug(format!("`concat` of {a:?} and {b:?}"))),
        },
        LibFn::StrSplitWhitespace => match value(0)? {
            Value::Str(s) => Ok(Value::Vec(
                s.split_whitespace().map(|w| Value::Str(Arc::from(w))).collect(),
            )),
            other => Err(bug(format!("`split_whitespace` of {other:?}"))),
        },
        LibFn::StrToLowercase => match value(0)? {
            Value::Str(s) => Ok(Value::Str(Arc::from(s.to_lowercase()))),
            other => Err(bug(format!("`to_lowercase` of {other:?}"))),
        },
    }
}
