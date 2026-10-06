//! Pure functions (LANGUAGE §16.1) and the built-in library (Appendix B) in the engine. Written independently of the
//! oracle's (ARCH-16).
//!
//! A call evaluates in a frame of the callee's own variables, its parameters first. A `let` or a closure application
//! binds its variables in the current frame and restores what they held when its body ends (`Frame`), so no binding
//! is visible outside its scope and a closure re-applied by a combinator never sees its previous application's
//! bindings; the frame is never copied.

use std::sync::Arc;

use blossom_base::FnId;
use blossom_ir::core::{BuiltinFn, Expr, FnBody, FnRef, LibFn, Pattern};
use blossom_value::Value;
use blossom_value::float;
use blossom_value::types::IntTy;
use blossom_value::value::IntValue;

use crate::expr::{Ctx, ExprError, ExprResult, Frame, bug, eval_in, truth};

pub(crate) fn call(cx: &Ctx<'_>, env: &mut Frame<'_>, f: FnId, args: &[Expr]) -> ExprResult<Value> {
    let Some(work) = cx.fn_work else {
        return call_unprofiled(cx, env, f, args);
    };
    // The steps of this call, those of the calls inside it, and its own (the difference).
    let (before, outer) = (cx.steps.get(), cx.callee_steps.replace(0));
    let out = call_unprofiled(cx, env, f, args);
    let total = cx.steps.get() - before;
    let inner = cx.callee_steps.replace(outer + total);
    let mut work = work.borrow_mut();
    let w = work.entry(f).or_default();
    w.calls += 1;
    w.steps += total;
    w.self_steps += total.saturating_sub(inner);
    out
}

fn call_unprofiled(cx: &Ctx<'_>, env: &mut Frame<'_>, f: FnId, args: &[Expr]) -> ExprResult<Value> {
    let Some(decl) = cx.program.fns.get(f) else {
        return Err(bug(format!("call of undeclared function {f:?}")));
    };
    let body = match &decl.body {
        FnBody::Ir(body) => body,
        FnBody::Extern { path, .. } => {
            let Some(host) = cx.shared.externs.lookup_fn(path) else {
                return Err(bug(format!(
                    "host function {path} was not bound when the engine was built"
                )));
            };
            let mut vs = Vec::with_capacity(args.len());
            for a in args {
                vs.push(eval_in(cx, env, a)?);
            }
            return host.call(&vs).map_err(|e| match e {
                blossom_value::ExternError::Failed(m) => ExprError::Refused(format!("{}: {m}", decl.name)),
                blossom_value::ExternError::InvalidArguments(m) => {
                    bug(format!("{} was called with the wrong arguments: {m}", decl.name))
                }
                blossom_value::ExternError::Unimplemented(u) => ExprError::Eval(u.into()),
            });
        }
        other => {
            return Err(ExprError::Eval(
                blossom_base::unimplemented_error!("LANG-183", "calls of `{}` ({other:?}) in the engine", decl.name)
                    .into(),
            ));
        }
    };
    let mut frame: Vec<Option<Value>> = Vec::with_capacity(decl.vars.len());
    for a in args {
        frame.push(Some(eval_in(cx, env, a)?));
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
    let saved = cx.fuel.enter(decl.props.metered);
    let out = eval_in(cx, &mut Frame::owned(frame), body);
    cx.fuel.exit(saved);
    out
}

pub(crate) fn let_expr(
    cx: &Ctx<'_>,
    env: &mut Frame<'_>,
    pat: &Pattern,
    value: &Expr,
    body: &Expr,
) -> ExprResult<Value> {
    let v = eval_in(cx, env, value)?;
    let mark = env.mark();
    let out = set(env, pat, v).and_then(|()| eval_in(cx, env, body));
    env.restore(mark);
    out
}

/// Binds an irrefutable pattern's variables in `frame` (undone by `Frame::restore`), taking `v`.
fn set(frame: &mut Frame<'_>, pat: &Pattern, v: Value) -> ExprResult<()> {
    match pat {
        Pattern::Wild => Ok(()),
        Pattern::Var(x) => frame.bind(x.index(), v),
        Pattern::Tuple(ps) => match v {
            Value::Tuple(fs) if fs.len() == ps.len() => {
                ps.iter().zip(fs.iter()).try_for_each(|(p, f)| set(frame, p, f.clone()))
            }
            Value::Tuple(fs) => Err(bug(format!("a {}-tuple pattern over {:?}", ps.len(), Value::Tuple(fs)))),
            other => Err(bug(format!("a tuple pattern over {other:?}"))),
        },
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

    /// Applies the closure to `args`, which its parameters take (moved, not copied).
    fn call<const N: usize>(&self, cx: &Ctx<'_>, env: &mut Frame<'_>, args: [Value; N]) -> ExprResult<Value> {
        if N != self.params.len() {
            return Err(bug(format!("a {}-parameter closure given {N}", self.params.len())));
        }
        cx.fuel.spend(1)?;
        let mark = env.mark();
        let out = self
            .params
            .iter()
            .zip(args)
            .try_for_each(|(p, a)| env.bind(p.index(), a))
            .and_then(|()| eval_in(cx, env, self.body));
        env.restore(mark);
        out
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
fn each<'f>(
    cx: &Ctx<'_>,
    env: &mut Frame<'f>,
    recv: &Expr,
    mut step: impl FnMut(&mut Frame<'f>, Value) -> ExprResult<bool>,
) -> ExprResult<()> {
    if let Expr::Call {
        f: FnRef::Builtin(BuiltinFn::Lib(LibFn::Range)),
        args,
    } = recv
    {
        let (Some(lo), Some(hi)) = (args.first(), args.get(1)) else {
            return Err(bug("`range` without its bounds".into()));
        };
        let (lo, hi) = (as_u64(eval_in(cx, env, lo)?)?, as_u64(eval_in(cx, env, hi)?)?);
        let mut i = lo;
        while i < hi {
            if !step(env, Value::Int(IntValue::U64(i)))? {
                break;
            }
            i += 1;
        }
        return Ok(());
    }
    let Value::Vec(xs) = eval_in(cx, env, recv)? else {
        return Err(bug("a vector combinator on a non-vector".into()));
    };
    for x in xs.iter() {
        if !step(env, x.clone())? {
            break;
        }
    }
    Ok(())
}

/// A library call's argument `i`, evaluated.
fn arg_value(cx: &Ctx<'_>, env: &mut Frame<'_>, f: LibFn, args: &[Expr], i: usize) -> ExprResult<Value> {
    let e = args
        .get(i)
        .ok_or_else(|| bug(format!("{f:?} is missing argument {i}")))?;
    eval_in(cx, env, e)
}

/// A library call's argument `i`, evaluated to a vector.
fn arg_vector(cx: &Ctx<'_>, env: &mut Frame<'_>, f: LibFn, args: &[Expr], i: usize) -> ExprResult<Arc<[Value]>> {
    match arg_value(cx, env, f, args, i)? {
        Value::Vec(v) => Ok(v),
        other => Err(bug(format!("{f:?} expects a vector, found {other:?}"))),
    }
}

pub(crate) fn library(cx: &Ctx<'_>, env: &mut Frame<'_>, f: LibFn, args: &[Expr]) -> ExprResult<Value> {
    let expr = |i: usize| args.get(i).ok_or_else(|| bug(format!("{f:?} is missing argument {i}")));
    let optional = |v: Value| -> ExprResult<Option<Value>> {
        match v {
            Value::Option(o) => Ok(o.as_deref().cloned()),
            other => Err(bug(format!("{f:?} expects an option, found {other:?}"))),
        }
    };
    match f {
        LibFn::Range => {
            let (lo, hi) = (
                as_u64(arg_value(cx, env, f, args, 0)?)?,
                as_u64(arg_value(cx, env, f, args, 1)?)?,
            );
            let n = hi.saturating_sub(lo);
            cx.fuel.spend(n)?;
            let mut out = Vec::with_capacity(usize::try_from(n).unwrap_or(0).min(1 << 16));
            let mut i = lo;
            while i < hi {
                out.push(Value::Int(IntValue::U64(i)));
                i += 1;
            }
            Ok(Value::Vec(out.into()))
        }
        LibFn::VecGet => {
            let xs = arg_vector(cx, env, f, args, 0)?;
            let i = as_u64(arg_value(cx, env, f, args, 1)?)?;
            Ok(some_or_none(usize::try_from(i).ok().and_then(|i| xs.get(i).cloned())))
        }
        LibFn::VecFirst => Ok(some_or_none(arg_vector(cx, env, f, args, 0)?.first().cloned())),
        LibFn::VecLast => Ok(some_or_none(arg_vector(cx, env, f, args, 0)?.last().cloned())),
        LibFn::VecPush => {
            let xs = arg_vector(cx, env, f, args, 0)?;
            let mut out = xs.to_vec();
            out.push(arg_value(cx, env, f, args, 1)?);
            Ok(Value::Vec(out.into()))
        }
        LibFn::VecConcat => {
            let mut out = arg_vector(cx, env, f, args, 0)?.to_vec();
            out.extend(arg_vector(cx, env, f, args, 1)?.iter().cloned());
            Ok(Value::Vec(out.into()))
        }
        LibFn::VecIsEmpty => Ok(Value::Bool(arg_vector(cx, env, f, args, 0)?.is_empty())),
        LibFn::VecReverse => {
            let mut out = arg_vector(cx, env, f, args, 0)?.to_vec();
            out.reverse();
            Ok(Value::Vec(out.into()))
        }
        LibFn::VecFlatten => {
            let mut out = Vec::new();
            for inner in arg_vector(cx, env, f, args, 0)?.iter() {
                match inner {
                    Value::Vec(xs) => out.extend(xs.iter().cloned()),
                    other => return Err(bug(format!("`flatten` of a vector holding {other:?}"))),
                }
            }
            Ok(Value::Vec(out.into()))
        }
        LibFn::VecEnumerate => {
            let xs = arg_vector(cx, env, f, args, 0)?;
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
            each(cx, env, expr(0)?, |env, x| {
                match f {
                    LibFn::VecMap => out.push(c.call(cx, env, [x])?),
                    LibFn::VecFilter => {
                        if truth(&c.call(cx, env, [x.clone()])?)? {
                            out.push(x);
                        }
                    }
                    _ => {
                        if let Some(y) = optional(c.call(cx, env, [x])?)? {
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
            each(cx, env, expr(0)?, |env, x| {
                all = truth(&c.call(cx, env, [x])?)?;
                Ok(all)
            })?;
            Ok(Value::Bool(all))
        }
        LibFn::VecAny => {
            let c = Closure::of(expr(1)?)?;
            let mut any = false;
            each(cx, env, expr(0)?, |env, x| {
                any = truth(&c.call(cx, env, [x])?)?;
                Ok(!any)
            })?;
            Ok(Value::Bool(any))
        }
        LibFn::VecFold => {
            // Arguments evaluate left to right, the receiver first: the initial value is evaluated after the
            // receiver's bounds (at the first element, or after an empty receiver), as the reference does.
            let c = Closure::of(expr(2)?)?;
            let mut acc: Option<Value> = None;
            each(cx, env, expr(0)?, |env, x| {
                let prev = match acc.take() {
                    Some(a) => a,
                    None => arg_value(cx, env, f, args, 1)?,
                };
                acc = Some(c.call(cx, env, [prev, x])?);
                Ok(true)
            })?;
            match acc {
                Some(a) => Ok(a),
                None => arg_value(cx, env, f, args, 1),
            }
        }
        LibFn::VecScan => {
            // As `fold`: the initial value is evaluated at the first element, or after an empty receiver.
            let c = Closure::of(expr(2)?)?;
            let mut acc: Option<Value> = None;
            let mut out = Vec::new();
            each(cx, env, expr(0)?, |env, x| {
                let prev = match acc.take() {
                    Some(a) => a,
                    None => arg_value(cx, env, f, args, 1)?,
                };
                let next = c.call(cx, env, [prev, x])?;
                out.push(next.clone());
                acc = Some(next);
                Ok(true)
            })?;
            if acc.is_none() {
                arg_value(cx, env, f, args, 1)?;
            }
            Ok(Value::Vec(out.into()))
        }
        LibFn::VecScanWhile => {
            // As `scan`: the initial value is evaluated at the first element, or after an empty receiver.
            let c = Closure::of(expr(2)?)?;
            let mut acc: Option<Value> = None;
            let mut started = false;
            let mut out = Vec::new();
            each(cx, env, expr(0)?, |env, x| {
                let prev = match acc.take() {
                    Some(a) => a,
                    None => arg_value(cx, env, f, args, 1)?,
                };
                started = true;
                match c.call(cx, env, [prev, x])? {
                    Value::Option(Some(next)) => {
                        let next = (*next).clone();
                        out.push(next.clone());
                        acc = Some(next);
                        Ok(true)
                    }
                    Value::Option(None) => Ok(false),
                    other => Err(bug(format!("a scan_while step returned {other:?}"))),
                }
            })?;
            if !started {
                arg_value(cx, env, f, args, 1)?;
            }
            Ok(Value::Vec(out.into()))
        }
        LibFn::VecToSet => Ok(Value::Set(Arc::new(
            arg_vector(cx, env, f, args, 0)?.iter().cloned().collect(),
        ))),
        LibFn::VecToMap => {
            let mut m = std::collections::BTreeMap::new();
            for pair in arg_vector(cx, env, f, args, 0)?.iter() {
                match pair {
                    Value::Tuple(kv) if kv.len() == 2 => {
                        if let (Some(k), Some(v)) = (kv.first(), kv.get(1)) {
                            m.insert(k.clone(), v.clone());
                        }
                    }
                    other => return Err(bug(format!("`to_map` of a vector holding {other:?}"))),
                }
            }
            Ok(Value::Map(Arc::new(m)))
        }
        LibFn::MapGet => match arg_value(cx, env, f, args, 0)? {
            Value::Map(m) => {
                let k = arg_value(cx, env, f, args, 1)?;
                Ok(match m.get(&k) {
                    Some(v) => Value::some(v.clone()),
                    None => Value::none(),
                })
            }
            other => Err(bug(format!("`get` on {other:?}"))),
        },
        LibFn::OptIsSome => Ok(Value::Bool(optional(arg_value(cx, env, f, args, 0)?)?.is_some())),
        LibFn::OptIsNone => Ok(Value::Bool(optional(arg_value(cx, env, f, args, 0)?)?.is_none())),
        LibFn::OptUnwrapOr => match optional(arg_value(cx, env, f, args, 0)?)? {
            Some(x) => Ok(x),
            None => arg_value(cx, env, f, args, 1),
        },
        LibFn::OptMap | LibFn::OptAndThen => {
            let Some(x) = optional(arg_value(cx, env, f, args, 0)?)? else {
                return Ok(Value::Option(None));
            };
            let r = Closure::of(expr(1)?)?.call(cx, env, [x])?;
            Ok(if f == LibFn::OptMap {
                Value::Option(Some(Arc::new(r)))
            } else {
                r
            })
        }
        LibFn::BytesSlice => {
            let Value::Bytes(b) = arg_value(cx, env, f, args, 0)? else {
                return Err(bug("`slice` of a non-Bytes value".into()));
            };
            let (lo, hi) = (
                as_u64(arg_value(cx, env, f, args, 1)?)?,
                as_u64(arg_value(cx, env, f, args, 2)?)?,
            );
            let len = b.len() as u64;
            if lo > hi || hi > len {
                return Ok(Value::Option(None));
            }
            let (lo, hi) = (lo as usize, hi as usize);
            Ok(some_or_none(b.get(lo..hi).map(|s| Value::Bytes(Arc::from(s)))))
        }
        LibFn::BytesConcat => match (arg_value(cx, env, f, args, 0)?, arg_value(cx, env, f, args, 1)?) {
            (Value::Bytes(a), Value::Bytes(b)) => {
                let mut out = Vec::with_capacity(a.len() + b.len());
                out.extend_from_slice(&a);
                out.extend_from_slice(&b);
                Ok(Value::Bytes(out.into()))
            }
            (a, b) => Err(bug(format!("`concat` of {a:?} and {b:?}"))),
        },
        LibFn::StrSplitWhitespace => match arg_value(cx, env, f, args, 0)? {
            Value::Str(s) => Ok(Value::Vec(
                s.split_whitespace().map(|w| Value::Str(Arc::from(w))).collect(),
            )),
            other => Err(bug(format!("`split_whitespace` of {other:?}"))),
        },
        LibFn::StrToLowercase => match arg_value(cx, env, f, args, 0)? {
            Value::Str(s) => Ok(Value::Str(Arc::from(s.to_lowercase()))),
            other => Err(bug(format!("`to_lowercase` of {other:?}"))),
        },
        LibFn::StrTrim => match arg_value(cx, env, f, args, 0)? {
            Value::Str(s) => Ok(Value::Str(Arc::from(s.trim()))),
            other => Err(bug(format!("`trim` of {other:?}"))),
        },
        LibFn::IntToString => match arg_value(cx, env, f, args, 0)? {
            Value::Int(i) => Ok(Value::Str(Arc::from(i.to_string()))),
            other => Err(bug(format!("`to_string` of {other:?}"))),
        },
        LibFn::FloatToString => match arg_value(cx, env, f, args, 0)? {
            Value::F64(x) => Ok(Value::Str(Arc::from(float::to_string(x)))),
            other => Err(bug(format!("`to_string` of {other:?}"))),
        },
        LibFn::Abs => num(float::abs(&arg_value(cx, env, f, args, 0)?)),
        LibFn::Min | LibFn::Max => {
            let (a, b) = (arg_value(cx, env, f, args, 0)?, arg_value(cx, env, f, args, 1)?);
            num(float::min_max(&a, &b, f == LibFn::Max))
        }
        LibFn::Clamp => {
            let x = arg_value(cx, env, f, args, 0)?;
            let (lo, hi) = (arg_value(cx, env, f, args, 1)?, arg_value(cx, env, f, args, 2)?);
            num(float::clamp(&x, &lo, &hi))
        }
        LibFn::FloatSqrt => num(float::method(float::Method::Sqrt, &arg_value(cx, env, f, args, 0)?)),
        LibFn::FloatFloor => num(float::method(float::Method::Floor, &arg_value(cx, env, f, args, 0)?)),
        LibFn::FloatCeil => num(float::method(float::Method::Ceil, &arg_value(cx, env, f, args, 0)?)),
        LibFn::FloatRound => num(float::method(float::Method::Round, &arg_value(cx, env, f, args, 0)?)),
        LibFn::FloatTrunc => num(float::method(float::Method::Trunc, &arg_value(cx, env, f, args, 0)?)),
        LibFn::DurationFromMillis => match arg_value(cx, env, f, args, 0)? {
            Value::Int(IntValue::I64(n)) => n
                .checked_mul(1_000_000)
                .map(|x| Value::Duration(blossom_value::time::Duration(x)))
                .ok_or_else(|| ExprError::Arithmetic(format!("Duration::from_millis({n}) overflows"))),
            other => Err(bug(format!("`from_millis` of {other:?}"))),
        },
        LibFn::DurationAsMillis => match arg_value(cx, env, f, args, 0)? {
            Value::Duration(d) => Ok(Value::Int(IntValue::I64(d.0 / 1_000_000))),
            other => Err(bug(format!("`as_millis` of {other:?}"))),
        },
        LibFn::InstantAsMillis => match arg_value(cx, env, f, args, 0)? {
            Value::Instant(t) => Ok(Value::Int(IntValue::I64(t.0 / 1_000_000))),
            other => Err(bug(format!("`as_millis` of {other:?}"))),
        },
        LibFn::StrParseI64 => match arg_value(cx, env, f, args, 0)? {
            Value::Str(s) => Ok(some_or_none(
                s.parse::<i64>().ok().map(|n| Value::Int(IntValue::I64(n))),
            )),
            other => Err(bug(format!("`parse_i64` of {other:?}"))),
        },
        LibFn::StrToUtf8 => match arg_value(cx, env, f, args, 0)? {
            Value::Str(s) => Ok(Value::Bytes(Arc::from(s.as_bytes()))),
            other => Err(bug(format!("`to_utf8` of {other:?}"))),
        },
        LibFn::BytesFromUtf8 => match arg_value(cx, env, f, args, 0)? {
            Value::Bytes(b) => Ok(some_or_none(
                String::from_utf8(b.to_vec()).ok().map(|s| Value::Str(Arc::from(s))),
            )),
            other => Err(bug(format!("`from_utf8` of {other:?}"))),
        },
        LibFn::BytesRead(it) => {
            let b = bytes_arg(arg_value(cx, env, f, args, 0)?)?;
            let at = as_u64(arg_value(cx, env, f, args, 1)?)?;
            Ok(some_or_none(read_be(&b, at, it)?))
        }
        LibFn::BytesPut(it) => {
            let b = bytes_arg(arg_value(cx, env, f, args, 0)?)?;
            let at = as_u64(arg_value(cx, env, f, args, 1)?)?;
            let enc = be_bytes(it, &arg_value(cx, env, f, args, 2)?)?;
            let Ok(start) = usize::try_from(at) else {
                return Ok(Value::Option(None));
            };
            if start > b.len() || b.len() - start < enc.len() {
                return Ok(Value::Option(None));
            }
            let mut out = Vec::with_capacity(b.len());
            out.extend(b.iter().take(start));
            out.extend_from_slice(&enc);
            out.extend(b.iter().skip(start + enc.len()));
            Ok(Value::Option(Some(Arc::new(Value::Bytes(out.into())))))
        }
        LibFn::BytesFrom(it) => Ok(Value::Bytes(be_bytes(it, &arg_value(cx, env, f, args, 0)?)?.into())),
        LibFn::BytesUvarintAt | LibFn::BytesVarintAt => {
            let b = bytes_arg(arg_value(cx, env, f, args, 0)?)?;
            let at = as_u64(arg_value(cx, env, f, args, 1)?)?;
            let Some((raw, next)) = usize::try_from(at).ok().and_then(|s| read_uvarint(&b, s)) else {
                return Ok(Value::Option(None));
            };
            let decoded = if f == LibFn::BytesVarintAt {
                // Zigzag: 0, -1, 1, -2, … are 0, 1, 2, 3, …
                let magnitude = (raw >> 1) as i64;
                Value::Int(IntValue::I64(if raw & 1 == 0 { magnitude } else { !magnitude }))
            } else {
                Value::Int(IntValue::U64(raw))
            };
            let next = u64::try_from(next).map_err(|_| bug("a position beyond u64".into()))?;
            Ok(Value::Option(Some(Arc::new(Value::Tuple(Arc::from([
                decoded,
                Value::Int(IntValue::U64(next)),
            ]))))))
        }
        LibFn::BytesUvarint => Ok(Value::Bytes(
            write_uvarint(as_u64(arg_value(cx, env, f, args, 0)?)?).into(),
        )),
        LibFn::BytesVarint => match arg_value(cx, env, f, args, 0)? {
            Value::Int(IntValue::I64(x)) => {
                let zz = if x >= 0 {
                    (x as u64) << 1
                } else {
                    ((!x as u64) << 1) | 1
                };
                Ok(Value::Bytes(write_uvarint(zz).into()))
            }
            other => Err(bug(format!("`Bytes::varint` of {other:?}"))),
        },
        LibFn::BytesEmpty => Ok(Value::Bytes(Arc::from(Vec::new()))),
        LibFn::BlobOf => {
            let Value::Bytes(b) = arg_value(cx, env, f, args, 0)? else {
                return Err(bug("`Blob::of` of a non-Bytes value".into()));
            };
            let r = blossom_value::BlobRef::of(&b);
            cx.new_blobs.borrow_mut().entry(r).or_insert(b);
            Ok(Value::Blob(r))
        }
        LibFn::BlobRead => {
            let Value::Blob(r) = arg_value(cx, env, f, args, 0)? else {
                return Err(bug("`read` of a non-Blob value".into()));
            };
            let (lo, hi) = (
                as_u64(arg_value(cx, env, f, args, 1)?)?,
                as_u64(arg_value(cx, env, f, args, 2)?)?,
            );
            // Handles are made only from their bytes: a missing blob is a host bug.
            let b = cx
                .blob(&r)
                .ok_or_else(|| bug(format!("the bytes of blob {} are not available", r.hex())))?;
            if lo > hi || hi > b.len() as u64 {
                return Ok(Value::Option(None));
            }
            let (lo, hi) = (lo as usize, hi as usize);
            Ok(some_or_none(b.get(lo..hi).map(|s| Value::Bytes(Arc::from(s)))))
        }
        LibFn::BytesJoin => {
            let parts = arg_vector(cx, env, f, args, 0)?;
            let mut out = Vec::new();
            for p in parts.iter() {
                let Value::Bytes(b) = p else {
                    return Err(bug(format!("`Bytes::join` of {p:?}")));
                };
                out.extend_from_slice(b);
            }
            Ok(Value::Bytes(out.into()))
        }
    }
}

fn bytes_arg(v: Value) -> ExprResult<Arc<[u8]>> {
    match v {
        Value::Bytes(b) => Ok(b),
        other => Err(bug(format!("Bytes expected, found {other:?}"))),
    }
}

/// The big-endian integer of type `it` starting at byte `at`, or `None` if it runs past the end.
fn read_be(b: &[u8], at: u64, it: IntTy) -> ExprResult<Option<Value>> {
    let n = match it {
        IntTy::U8 | IntTy::I8 => 1,
        IntTy::U16 | IntTy::I16 => 2,
        IntTy::U32 | IntTy::I32 => 4,
        IntTy::U64 | IntTy::I64 => 8,
        other => return Err(bug(format!("a byte read of {}", other.name()))),
    };
    let Ok(start) = usize::try_from(at) else {
        return Ok(None);
    };
    if start > b.len() || b.len() - start < n {
        return Ok(None);
    }
    let mut word = [0u8; 8];
    for (dst, src) in word.iter_mut().skip(8 - n).zip(b.iter().skip(start)) {
        *dst = *src;
    }
    let u = u64::from_be_bytes(word);
    Ok(Some(Value::Int(match it {
        IntTy::U8 => IntValue::U8(u as u8),
        IntTy::I8 => IntValue::I8(u as u8 as i8),
        IntTy::U16 => IntValue::U16(u as u16),
        IntTy::I16 => IntValue::I16(u as u16 as i16),
        IntTy::U32 => IntValue::U32(u as u32),
        IntTy::I32 => IntValue::I32(u as u32 as i32),
        IntTy::U64 => IntValue::U64(u),
        _ => IntValue::I64(u as i64),
    })))
}

/// The big-endian bytes of `v`, which must be an integer of type `it`.
fn be_bytes(it: IntTy, v: &Value) -> ExprResult<Vec<u8>> {
    Ok(match (it, v) {
        (IntTy::U8, Value::Int(IntValue::U8(x))) => x.to_be_bytes().to_vec(),
        (IntTy::I8, Value::Int(IntValue::I8(x))) => x.to_be_bytes().to_vec(),
        (IntTy::U16, Value::Int(IntValue::U16(x))) => x.to_be_bytes().to_vec(),
        (IntTy::I16, Value::Int(IntValue::I16(x))) => x.to_be_bytes().to_vec(),
        (IntTy::U32, Value::Int(IntValue::U32(x))) => x.to_be_bytes().to_vec(),
        (IntTy::I32, Value::Int(IntValue::I32(x))) => x.to_be_bytes().to_vec(),
        (IntTy::U64, Value::Int(IntValue::U64(x))) => x.to_be_bytes().to_vec(),
        (IntTy::I64, Value::Int(IntValue::I64(x))) => x.to_be_bytes().to_vec(),
        (it, v) => return Err(bug(format!("{} bytes of {v:?}", it.name()))),
    })
}

/// An unsigned LEB128 varint starting at `start`: its value and the index after it.
fn read_uvarint(b: &[u8], start: usize) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    let mut shift = 0u32;
    let mut at = start;
    loop {
        let byte = *b.get(at)?;
        at += 1;
        let chunk = u64::from(byte & 0x7f);
        // At shift 63 only one bit is left in a u64.
        if shift == 63 && chunk > 1 {
            return None;
        }
        value |= chunk << shift;
        if byte & 0x80 == 0 {
            return Some((value, at));
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

fn write_uvarint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    while n >= 0x80 {
        out.push((n as u8) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
    out
}

/// A numeric library call's value: its runtime error is BLSR004, a mistyped call an engine bug.
fn num(r: Result<Value, float::NumError>) -> ExprResult<Value> {
    r.map_err(|e| match e {
        float::NumError::Arithmetic(m) => ExprError::Arithmetic(m),
        float::NumError::Type(m) => bug(m),
    })
}
