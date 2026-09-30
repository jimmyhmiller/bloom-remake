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
use blossom_value::types::IntTy;
use blossom_value::value::IntValue;

use crate::expr::{Ctx, ExprError, ExprResult, bug, eval, truth};

pub(crate) fn call(cx: &Ctx<'_>, env: &[Option<Value>], f: FnId, args: &[Expr]) -> ExprResult<Value> {
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
                vs.push(eval(cx, env, a)?);
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
    cx.fuel.enter();
    let out = eval(cx, &frame, body);
    cx.fuel.exit();
    out
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
        cx.fuel.spend(1)?;
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
            // Arguments evaluate left to right, the receiver first: the initial value is evaluated after the
            // receiver's bounds (at the first element, or after an empty receiver), as the reference does.
            let c = Closure::of(expr(2)?)?;
            let mut acc: Option<Value> = None;
            each(cx, env, expr(0)?, |x| {
                let prev = match acc.take() {
                    Some(a) => a,
                    None => value(1)?,
                };
                acc = Some(c.call(cx, env, &[prev, x])?);
                Ok(true)
            })?;
            match acc {
                Some(a) => Ok(a),
                None => value(1),
            }
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
        LibFn::DurationFromMillis => match value(0)? {
            Value::Int(IntValue::I64(n)) => n
                .checked_mul(1_000_000)
                .map(|x| Value::Duration(blossom_value::time::Duration(x)))
                .ok_or_else(|| ExprError::Arithmetic(format!("Duration::from_millis({n}) overflows"))),
            other => Err(bug(format!("`from_millis` of {other:?}"))),
        },
        LibFn::DurationAsMillis => match value(0)? {
            Value::Duration(d) => Ok(Value::Int(IntValue::I64(d.0 / 1_000_000))),
            other => Err(bug(format!("`as_millis` of {other:?}"))),
        },
        LibFn::StrParseI64 => match value(0)? {
            Value::Str(s) => Ok(some_or_none(s.parse::<i64>().ok().map(|n| Value::Int(IntValue::I64(n))))),
            other => Err(bug(format!("`parse_i64` of {other:?}"))),
        },
        LibFn::StrToUtf8 => match value(0)? {
            Value::Str(s) => Ok(Value::Bytes(Arc::from(s.as_bytes()))),
            other => Err(bug(format!("`to_utf8` of {other:?}"))),
        },
        LibFn::BytesFromUtf8 => match value(0)? {
            Value::Bytes(b) => Ok(some_or_none(
                String::from_utf8(b.to_vec()).ok().map(|s| Value::Str(Arc::from(s))),
            )),
            other => Err(bug(format!("`from_utf8` of {other:?}"))),
        },
        LibFn::BytesRead(it) => {
            let b = bytes_arg(value(0)?)?;
            let at = as_u64(value(1)?)?;
            Ok(some_or_none(read_be(&b, at, it)?))
        }
        LibFn::BytesPut(it) => {
            let b = bytes_arg(value(0)?)?;
            let at = as_u64(value(1)?)?;
            let enc = be_bytes(it, &value(2)?)?;
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
        LibFn::BytesFrom(it) => Ok(Value::Bytes(be_bytes(it, &value(0)?)?.into())),
        LibFn::BytesUvarintAt | LibFn::BytesVarintAt => {
            let b = bytes_arg(value(0)?)?;
            let at = as_u64(value(1)?)?;
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
        LibFn::BytesUvarint => Ok(Value::Bytes(write_uvarint(as_u64(value(0)?)?).into())),
        LibFn::BytesVarint => match value(0)? {
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
            let Value::Bytes(b) = value(0)? else {
                return Err(bug("`Blob::of` of a non-Bytes value".into()));
            };
            let r = blossom_value::BlobRef::of(&b);
            cx.new_blobs.borrow_mut().entry(r).or_insert(b);
            Ok(Value::Blob(r))
        }
        LibFn::BlobRead => {
            let Value::Blob(r) = value(0)? else {
                return Err(bug("`read` of a non-Blob value".into()));
            };
            let (lo, hi) = (as_u64(value(1)?)?, as_u64(value(2)?)?);
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
            let parts = vector(0)?;
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
