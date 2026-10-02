//! Pure functions (LANGUAGE §16.1) and the built-in library (Appendix B) in the oracle: calls, `let`, closures, and
//! the library's functions and combinators. Every library function is total: a position past the end is `None`.
//!
//! Scoping is by copying: a call gets a fresh environment of the callee's variables, and a `let` body or a closure
//! application evaluates in a copy of the enclosing environment with its bindings written in. So a binding never
//! outlives its scope, and a closure applied again (a fold's step) starts from the same enclosing environment.

use std::sync::Arc;

use blossom_base::FnId;
use blossom_base::internal_error;
use blossom_ir::core::{BuiltinFn, Expr, FnBody, FnRef, LibFn, Pattern};
use blossom_value::{Value, types::IntTy, value::IntValue};

use crate::expr::{ExprError, ExprResult, Scope, eval, truth};

fn bug(msg: String) -> ExprError {
    ExprError::Oracle(internal_error!("{msg}").into())
}

/// A call of the pure function `id`.
pub(crate) fn call(scope: &Scope<'_>, env: &[Option<Value>], id: FnId, args: &[Expr]) -> ExprResult<Value> {
    let decl = scope
        .program
        .fns
        .get(id)
        .ok_or_else(|| bug(format!("unknown function {id:?}")))?;
    if args.len() != decl.params.len() {
        return Err(bug(format!("`{}` called with {} arguments", decl.name, args.len())));
    }
    let body = match &decl.body {
        FnBody::Ir(body) => body,
        FnBody::Extern { path, .. } => {
            let f = scope
                .oracle
                .host_fn(path)
                .ok_or_else(|| bug(format!("host function {path} was not bound")))?;
            let mut vs = Vec::with_capacity(args.len());
            for a in args {
                vs.push(eval(scope, env, a)?);
            }
            return f.call(&vs).map_err(|e| match e {
                blossom_value::ExternError::Failed(m) => ExprError::Refused(format!("{}: {m}", decl.name)),
                blossom_value::ExternError::InvalidArguments(m) => {
                    bug(format!("{} called with the wrong arguments: {m}", decl.name))
                }
                blossom_value::ExternError::Unimplemented(u) => ExprError::Oracle(u.into()),
            });
        }
        other => {
            return Err(ExprError::Oracle(
                blossom_base::unimplemented_error!("LANG-183", "calls of `{}` ({other:?}) in the oracle", decl.name)
                    .into(),
            ));
        }
    };
    let mut local: Vec<Option<Value>> = vec![None; decl.vars.len()];
    for (slot, a) in local.iter_mut().zip(args) {
        *slot = Some(eval(scope, env, a)?);
    }
    let saved = scope.fuel.enter(decl.props.metered);
    let out = eval(scope, &local, body);
    scope.fuel.exit(saved);
    out
}

/// `let pat = value; body`.
pub(crate) fn let_in(
    scope: &Scope<'_>,
    env: &[Option<Value>],
    pat: &Pattern,
    value: &Expr,
    body: &Expr,
) -> ExprResult<Value> {
    let v = eval(scope, env, value)?;
    let mut local = env.to_vec();
    bind(&mut local, pat, v)?;
    eval(scope, &local, body)
}

/// Binds an irrefutable pattern, overwriting its variables.
fn bind(env: &mut [Option<Value>], pat: &Pattern, v: Value) -> ExprResult<()> {
    match (pat, v) {
        (Pattern::Wild, _) => Ok(()),
        (Pattern::Var(x), v) => {
            let slot = env
                .get_mut(x.index())
                .ok_or_else(|| bug(format!("variable {x:?} out of range")))?;
            *slot = Some(v);
            Ok(())
        }
        (Pattern::Tuple(ps), Value::Tuple(fs)) if ps.len() == fs.len() => {
            for (p, f) in ps.iter().zip(fs.iter()) {
                bind(env, p, f.clone())?;
            }
            Ok(())
        }
        (p, v) => Err(bug(format!("the `let` pattern {p:?} does not bind {v:?}"))),
    }
}

/// Applies a closure argument to `args`.
fn apply(scope: &Scope<'_>, env: &[Option<Value>], closure: &Expr, args: Vec<Value>) -> ExprResult<Value> {
    let Expr::Closure { params, body } = closure else {
        return Err(bug(format!("a combinator's argument is not a closure: {closure:?}")));
    };
    if params.len() != args.len() {
        return Err(bug(format!(
            "a closure of {} parameters applied to {}",
            params.len(),
            args.len()
        )));
    }
    scope.fuel.spend(1)?;
    let mut local = env.to_vec();
    for (p, a) in params.iter().zip(args) {
        let slot = local
            .get_mut(p.index())
            .ok_or_else(|| bug(format!("closure parameter {p:?} out of range")))?;
        *slot = Some(a);
    }
    eval(scope, &local, body)
}

fn u64_of(v: &Value) -> ExprResult<u64> {
    match v {
        Value::Int(IntValue::U64(n)) => Ok(*n),
        other => Err(bug(format!("expected a u64, got {other:?}"))),
    }
}

/// The elements a combinator walks: a vector, or a `range(lo, hi)` that is never built.
enum Seq {
    Vals(Arc<[Value]>),
    Range(u64, u64),
}

impl Seq {
    fn iter(&self) -> Box<dyn Iterator<Item = Value> + '_> {
        match self {
            Seq::Vals(v) => Box::new(v.iter().cloned()),
            Seq::Range(lo, hi) => Box::new((*lo..*hi).map(|i| Value::Int(IntValue::U64(i)))),
        }
    }
}

fn seq(scope: &Scope<'_>, env: &[Option<Value>], e: &Expr) -> ExprResult<Seq> {
    if let Expr::Call {
        f: FnRef::Builtin(BuiltinFn::Lib(LibFn::Range)),
        args,
    } = e
    {
        let [lo, hi] = args.as_slice() else {
            return Err(bug("`range` takes two arguments".into()));
        };
        return Ok(Seq::Range(
            u64_of(&eval(scope, env, lo)?)?,
            u64_of(&eval(scope, env, hi)?)?,
        ));
    }
    match eval(scope, env, e)? {
        Value::Vec(v) => Ok(Seq::Vals(v)),
        other => Err(bug(format!("a vector combinator on {other:?}"))),
    }
}

fn option(v: Value) -> ExprResult<Option<Value>> {
    match v {
        Value::Option(o) => Ok(o.map(|x| (*x).clone())),
        other => Err(bug(format!("expected an option, got {other:?}"))),
    }
}

fn opt(v: Option<Value>) -> Value {
    match v {
        Some(x) => Value::some(x),
        None => Value::none(),
    }
}

/// A call of the library function `f`; the receiver is `args[0]`, a closure the last argument.
pub(crate) fn lib(scope: &Scope<'_>, env: &[Option<Value>], f: LibFn, args: &[Expr]) -> ExprResult<Value> {
    let arg = |i: usize| args.get(i).ok_or_else(|| bug(format!("{f:?}: missing argument {i}")));
    let val = |i: usize| eval(scope, env, arg(i)?);
    let vec_of = |v: Value| match v {
        Value::Vec(v) => Ok(v),
        other => Err(bug(format!("{f:?} on {other:?}"))),
    };
    let bytes_of = |v: Value| match v {
        Value::Bytes(b) => Ok(b),
        other => Err(bug(format!("{f:?} on {other:?}"))),
    };
    let str_of = |v: Value| match v {
        Value::Str(s) => Ok(s),
        other => Err(bug(format!("{f:?} on {other:?}"))),
    };
    Ok(match f {
        LibFn::Range => {
            let (lo, hi) = (u64_of(&val(0)?)?, u64_of(&val(1)?)?);
            scope.fuel.spend(hi.saturating_sub(lo))?;
            Value::Vec((lo..hi).map(|i| Value::Int(IntValue::U64(i))).collect())
        }
        LibFn::VecGet => {
            let v = vec_of(val(0)?)?;
            let i = u64_of(&val(1)?)?;
            opt(usize::try_from(i).ok().and_then(|i| v.get(i)).cloned())
        }
        LibFn::VecFirst => opt(vec_of(val(0)?)?.first().cloned()),
        LibFn::VecLast => opt(vec_of(val(0)?)?.last().cloned()),
        LibFn::VecPush => {
            let v = vec_of(val(0)?)?;
            let x = val(1)?;
            Value::Vec(v.iter().cloned().chain(std::iter::once(x)).collect())
        }
        LibFn::VecConcat => {
            let (a, b) = (vec_of(val(0)?)?, vec_of(val(1)?)?);
            Value::Vec(a.iter().chain(b.iter()).cloned().collect())
        }
        LibFn::VecIsEmpty => Value::Bool(vec_of(val(0)?)?.is_empty()),
        LibFn::VecReverse => Value::Vec(vec_of(val(0)?)?.iter().rev().cloned().collect()),
        LibFn::VecFlatten => {
            let mut out = Vec::new();
            for inner in vec_of(val(0)?)?.iter() {
                out.extend(vec_of(inner.clone())?.iter().cloned());
            }
            Value::Vec(out.into())
        }
        LibFn::VecEnumerate => Value::Vec(
            vec_of(val(0)?)?
                .iter()
                .enumerate()
                .map(|(i, x)| Value::Tuple(vec![Value::Int(IntValue::U64(i as u64)), x.clone()].into()))
                .collect(),
        ),
        LibFn::VecMap => {
            let s = seq(scope, env, arg(0)?)?;
            let c = arg(1)?;
            let mut out = Vec::new();
            for x in s.iter() {
                out.push(apply(scope, env, c, vec![x])?);
            }
            Value::Vec(out.into())
        }
        LibFn::VecFilter => {
            let s = seq(scope, env, arg(0)?)?;
            let c = arg(1)?;
            let mut out = Vec::new();
            for x in s.iter() {
                if truth(&apply(scope, env, c, vec![x.clone()])?)? {
                    out.push(x);
                }
            }
            Value::Vec(out.into())
        }
        LibFn::VecFilterMap => {
            let s = seq(scope, env, arg(0)?)?;
            let c = arg(1)?;
            let mut out = Vec::new();
            for x in s.iter() {
                if let Some(y) = option(apply(scope, env, c, vec![x])?)? {
                    out.push(y);
                }
            }
            Value::Vec(out.into())
        }
        LibFn::VecAll | LibFn::VecAny => {
            let s = seq(scope, env, arg(0)?)?;
            let c = arg(1)?;
            // Both stop at the first element that decides them.
            let want = f == LibFn::VecAny;
            let mut hit = false;
            for x in s.iter() {
                if truth(&apply(scope, env, c, vec![x])?)? == want {
                    hit = true;
                    break;
                }
            }
            Value::Bool(if want { hit } else { !hit })
        }
        LibFn::VecFold => {
            let s = seq(scope, env, arg(0)?)?;
            let mut acc = val(1)?;
            let c = arg(2)?;
            for x in s.iter() {
                acc = apply(scope, env, c, vec![acc, x])?;
            }
            acc
        }
        LibFn::VecScan => {
            let s = seq(scope, env, arg(0)?)?;
            let mut acc = val(1)?;
            let c = arg(2)?;
            let mut out = Vec::new();
            for x in s.iter() {
                acc = apply(scope, env, c, vec![acc, x])?;
                out.push(acc.clone());
            }
            Value::Vec(out.into())
        }
        LibFn::VecToSet => Value::Set(Arc::new(vec_of(val(0)?)?.iter().cloned().collect())),
        LibFn::VecToMap => {
            let mut m = std::collections::BTreeMap::new();
            for pair in vec_of(val(0)?)?.iter() {
                match pair {
                    Value::Tuple(kv) if kv.len() == 2 => {
                        if let (Some(k), Some(v)) = (kv.first(), kv.get(1)) {
                            m.insert(k.clone(), v.clone());
                        }
                    }
                    other => return Err(bug(format!("`to_map` of a vector holding {other:?}"))),
                }
            }
            Value::Map(Arc::new(m))
        }
        LibFn::MapGet => match val(0)? {
            Value::Map(m) => opt(m.get(&val(1)?).cloned()),
            other => return Err(bug(format!("`get` on {other:?}"))),
        },
        LibFn::OptIsSome => Value::Bool(option(val(0)?)?.is_some()),
        LibFn::OptIsNone => Value::Bool(option(val(0)?)?.is_none()),
        LibFn::OptUnwrapOr => match option(val(0)?)? {
            Some(x) => x,
            None => val(1)?,
        },
        LibFn::OptMap => match option(val(0)?)? {
            Some(x) => Value::some(apply(scope, env, arg(1)?, vec![x])?),
            None => Value::none(),
        },
        LibFn::OptAndThen => match option(val(0)?)? {
            Some(x) => apply(scope, env, arg(1)?, vec![x])?,
            None => Value::none(),
        },
        LibFn::BytesSlice => {
            let b = bytes_of(val(0)?)?;
            let (lo, hi) = (u64_of(&val(1)?)?, u64_of(&val(2)?)?);
            let range = usize::try_from(lo).ok().zip(usize::try_from(hi).ok());
            opt(range
                .filter(|(lo, hi)| lo <= hi)
                .and_then(|(lo, hi)| b.get(lo..hi))
                .map(|s| Value::Bytes(s.into())))
        }
        LibFn::BytesConcat => {
            let (a, b) = (bytes_of(val(0)?)?, bytes_of(val(1)?)?);
            Value::Bytes(a.iter().chain(b.iter()).copied().collect())
        }
        LibFn::StrSplitWhitespace => Value::Vec(
            str_of(val(0)?)?
                .split_whitespace()
                .map(|w| Value::Str(w.into()))
                .collect(),
        ),
        LibFn::StrToLowercase => Value::Str(str_of(val(0)?)?.to_lowercase().into()),
        LibFn::StrToUtf8 => Value::Bytes(str_of(val(0)?)?.as_bytes().into()),
        LibFn::StrParseI64 => opt(str_of(val(0)?)?.parse::<i64>().ok().map(|n| Value::Int(IntValue::I64(n)))),
        LibFn::BytesFromUtf8 => {
            let b = bytes_of(val(0)?)?;
            opt(std::str::from_utf8(&b).ok().map(|s| Value::Str(s.into())))
        }
        LibFn::BytesRead(it) => {
            let b = bytes_of(val(0)?)?;
            let pos = u64_of(&val(1)?)?;
            opt(read_int(&b, pos, it)?)
        }
        LibFn::BytesPut(it) => {
            let b = bytes_of(val(0)?)?;
            let pos = u64_of(&val(1)?)?;
            let enc = int_bytes(it, &val(2)?)?;
            let at = usize::try_from(pos).ok();
            let end = at.and_then(|a| a.checked_add(enc.len())).filter(|e| *e <= b.len());
            opt(at.zip(end).map(|(a, e)| {
                let mut out = b.to_vec();
                if let Some(dst) = out.get_mut(a..e) {
                    dst.copy_from_slice(&enc);
                }
                Value::Bytes(out.into())
            }))
        }
        LibFn::BytesFrom(it) => Value::Bytes(int_bytes(it, &val(0)?)?.into()),
        LibFn::BytesUvarintAt | LibFn::BytesVarintAt => {
            let b = bytes_of(val(0)?)?;
            let pos = u64_of(&val(1)?)?;
            let got = usize::try_from(pos).ok().and_then(|p| uvarint(&b, p));
            opt(got.map(|(n, next)| {
                let v = if f == LibFn::BytesUvarintAt {
                    Value::Int(IntValue::U64(n))
                } else {
                    Value::Int(IntValue::I64(((n >> 1) as i64) ^ -((n & 1) as i64)))
                };
                Value::Tuple(vec![v, Value::Int(IntValue::U64(next as u64))].into())
            }))
        }
        LibFn::BytesUvarint => Value::Bytes(uvarint_bytes(u64_of(&val(0)?)?).into()),
        LibFn::BytesVarint => {
            let x = match val(0)? {
                Value::Int(IntValue::I64(x)) => x,
                other => return Err(bug(format!("`Bytes::varint` of {other:?}"))),
            };
            Value::Bytes(uvarint_bytes(((x << 1) ^ (x >> 63)) as u64).into())
        }
        LibFn::BytesEmpty => Value::Bytes(Arc::from(&[][..])),
        LibFn::DurationFromMillis => match val(0)? {
            Value::Int(IntValue::I64(n)) => Value::Duration(blossom_value::time::Duration(
                n.checked_mul(1_000_000)
                    .ok_or_else(|| ExprError::Arithmetic(format!("Duration::from_millis({n}) overflows")))?,
            )),
            other => return Err(bug(format!("`from_millis` of {other:?}"))),
        },
        LibFn::DurationAsMillis => match val(0)? {
            Value::Duration(d) => Value::Int(IntValue::I64(d.0 / 1_000_000)),
            other => return Err(bug(format!("`as_millis` of {other:?}"))),
        },
        LibFn::InstantAsMillis => match val(0)? {
            Value::Instant(t) => Value::Int(IntValue::I64(t.0 / 1_000_000)),
            other => return Err(bug(format!("`as_millis` of {other:?}"))),
        },
        LibFn::BlobOf => {
            let b = bytes_of(val(0)?)?;
            let r = blossom_value::BlobRef::of(&b);
            scope.new_blobs.borrow_mut().entry(r).or_insert_with(|| Arc::from(&b[..]));
            Value::Blob(r)
        }
        LibFn::BlobRead => {
            let Value::Blob(r) = val(0)? else {
                return Err(bug("`read` of a non-Blob".into()));
            };
            let (lo, hi) = (u64_of(&val(1)?)?, u64_of(&val(2)?)?);
            // A handle is only ever made from its bytes, so a missing blob is a host bug, never the program's.
            let b = scope
                .blob(&r)
                .ok_or_else(|| bug(format!("the bytes of blob {} are not available", r.hex())))?;
            let range = usize::try_from(lo).ok().zip(usize::try_from(hi).ok());
            opt(range
                .filter(|(lo, hi)| lo <= hi)
                .and_then(|(lo, hi)| b.get(lo..hi))
                .map(|s| Value::Bytes(s.into())))
        }
        LibFn::BytesJoin => {
            let mut out = Vec::new();
            for x in vec_of(val(0)?)?.iter() {
                match x {
                    Value::Bytes(b) => out.extend_from_slice(b),
                    other => return Err(bug(format!("`Bytes::join` of {other:?}"))),
                }
            }
            Value::Bytes(out.into())
        }
    })
}

/// The byte width of an integer type byte access supports.
fn width(it: IntTy) -> ExprResult<usize> {
    match it.bits() {
        8 | 16 | 32 | 64 => Ok(it.bits() as usize / 8),
        _ => Err(bug(format!("byte access of {}", it.name()))),
    }
}

/// The big-endian integer of type `it` at `pos`, if it lies inside `b`.
fn read_int(b: &[u8], pos: u64, it: IntTy) -> ExprResult<Option<Value>> {
    let w = width(it)?;
    let Some(bytes) = usize::try_from(pos).ok().and_then(|p| b.get(p..p.checked_add(w)?)) else {
        return Ok(None);
    };
    let raw = bytes.iter().fold(0u128, |acc, x| (acc << 8) | u128::from(*x));
    let bits = it.bits();
    let v = if it.is_signed() && raw >> (bits - 1) == 1 {
        raw as i128 - (1i128 << bits)
    } else {
        raw as i128
    };
    IntValue::from_i128(it, v)
        .map(|i| Some(Value::Int(i)))
        .ok_or_else(|| bug(format!("{v} read as {}", it.name())))
}

/// The big-endian bytes of the integer `v` of type `it`.
fn int_bytes(it: IntTy, v: &Value) -> ExprResult<Vec<u8>> {
    let w = width(it)?;
    let x = match v {
        Value::Int(i) if i.ty() == it => i.to_i128().ok_or_else(|| bug(format!("{i:?} as i128")))?,
        other => return Err(bug(format!("{} bytes of {other:?}", it.name()))),
    };
    let all = (x as u128).to_be_bytes();
    all.get(16 - w..)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| bug(format!("a {w}-byte integer")))
}

/// An unsigned LEB128 varint at `pos` and the position after it.
fn uvarint(b: &[u8], pos: usize) -> Option<(u64, usize)> {
    let mut v: u64 = 0;
    for i in 0..10 {
        let byte = *b.get(pos.checked_add(i)?)?;
        // The tenth byte holds bit 63 only: anything more overflows a u64, or continues past ten bytes.
        if i == 9 && byte > 1 {
            return None;
        }
        v |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((v, pos + i + 1));
        }
    }
    None
}

fn uvarint_bytes(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let low = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(low);
            return out;
        }
        out.push(low | 0x80);
    }
}
