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
use blossom_value::{Value, value::IntValue};

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
    let FnBody::Ir(body) = &decl.body else {
        return Err(ExprError::Oracle(
            blossom_base::unimplemented_error!(
                "LANG-181",
                "calls of `{}` (not an IR function) in the oracle",
                decl.name
            )
            .into(),
        ));
    };
    if args.len() != decl.params.len() {
        return Err(bug(format!("`{}` called with {} arguments", decl.name, args.len())));
    }
    let mut local: Vec<Option<Value>> = vec![None; decl.vars.len()];
    for (slot, a) in local.iter_mut().zip(args) {
        *slot = Some(eval(scope, env, a)?);
    }
    eval(scope, &local, body)
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
    })
}
