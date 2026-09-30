//! Expression evaluation over `Value` (LANGUAGE §9, §15.1). Arithmetic is checked: overflow and division by zero are
//! the runtime hard error BLSR004; division truncates toward zero. Written independently of the oracle (ARCH-16): the
//! differential suite compares the two.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_base::{ParamId, RoleId, internal_error};
use blossom_ir::core::{
    BinOp, BuiltinFn, BuiltinScalar, CollKind, Expr, FnRef, GenSource, LatOpRef, MajorityDomain, Pattern, Program,
    RangeKind, Term, UnOp,
};
use blossom_ir::tick::EvalError;
use blossom_lattice::{Kind, LatticeError};
use blossom_value::time::{Instant, NodeId, Tick};
use blossom_value::value::{IntValue, LatValue};
use blossom_value::{Seed, TypeDef, Value};

/// A runtime hard error of an expression, before it is attributed to a rule and a tick.
#[derive(Debug)]
pub(crate) enum ExprError {
    /// BLSR004.
    Arithmetic(String),
    /// BLSR006: an `LPoint` conflict.
    Conflict(String),
    /// Anything else: a missing feature or a bug.
    Eval(EvalError),
}

impl ExprError {
    /// A copy of a program error (the kind a valuation raises; an evaluator error is never repeated).
    pub(crate) fn duplicate(&self) -> ExprError {
        match self {
            ExprError::Arithmetic(m) => ExprError::Arithmetic(m.clone()),
            ExprError::Conflict(m) => ExprError::Conflict(m.clone()),
            ExprError::Eval(e) => bug(format!("an evaluator error repeated per valuation: {e}")),
        }
    }
}

impl From<EvalError> for ExprError {
    fn from(e: EvalError) -> ExprError {
        ExprError::Eval(e)
    }
}

impl From<LatticeError> for ExprError {
    fn from(e: LatticeError) -> ExprError {
        match e {
            LatticeError::Conflict(..) => ExprError::Conflict(e.to_string()),
            LatticeError::Arithmetic(m) | LatticeError::Domain(m) => ExprError::Arithmetic(m),
            LatticeError::Shape(m) => bug(format!("a lattice operation on the wrong values: {m}")),
        }
    }
}

pub(crate) type ExprResult<T> = Result<T, ExprError>;

pub(crate) fn bug(msg: String) -> ExprError {
    ExprError::Eval(internal_error!("{msg}").into())
}

/// What an expression reads besides its variables: the program, the node and the tick.
pub(crate) struct Ctx<'a> {
    pub program: &'a Program,
    pub node: NodeId,
    pub incarnation: u64,
    pub tick: Tick,
    pub now: Instant,
    pub shared: &'a Shared,
}

/// Per-program facts every tick's expressions read.
pub(crate) struct Shared {
    pub params: BTreeMap<ParamId, Value>,
    /// The choice seed σc (SEM-084).
    pub choice: Option<Seed>,
    /// Each node's seed σn, by node id.
    pub node_seeds: Vec<Seed>,
    /// Each node's role, by node id.
    pub roles: Vec<Option<RoleId>>,
    /// The built-in lattice of each declared lattice, by lattice id.
    pub kinds: Vec<Option<Kind>>,
}

impl Shared {
    pub fn role_size(&self, r: RoleId) -> u64 {
        self.roles.iter().filter(|x| **x == Some(r)).count() as u64
    }
}

pub(crate) fn term(cx: &Ctx<'_>, env: &[Option<Value>], t: &Term) -> ExprResult<Value> {
    match t {
        Term::Var(v) => env
            .get(v.index())
            .cloned()
            .flatten()
            .ok_or_else(|| bug(format!("variable {v:?} read before it is bound"))),
        Term::Const(c) => cx
            .program
            .consts
            .get(*c)
            .cloned()
            .ok_or_else(|| bug(format!("unknown constant {c:?}"))),
        Term::Wild => Err(bug("`_` evaluated as a value".into())),
    }
}

pub(crate) fn truth(v: &Value) -> ExprResult<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        other => Err(bug(format!("a condition evaluated to {other:?}"))),
    }
}

/// A missing feature, as an expression error.
macro_rules! unimplemented {
    ($feature:literal, $what:expr) => {
        ExprError::Eval(blossom_base::unimplemented_error!($feature, "{} in the engine", $what).into())
    };
}

pub(crate) fn eval(cx: &Ctx<'_>, env: &[Option<Value>], e: &Expr) -> ExprResult<Value> {
    match e {
        Expr::Term(t) => term(cx, env, t),
        Expr::Param(p) => param(cx, *p),
        Expr::Scalar(s) => match s {
            BuiltinScalar::SelfNode => Ok(Value::Node(cx.node)),
            BuiltinScalar::Tick => Ok(Value::Int(IntValue::U64(cx.tick.0))),
            BuiltinScalar::Now => Ok(Value::Instant(cx.now)),
            other => Err(unimplemented!("LANG-180", &format!("`${other:?}`"))),
        },
        Expr::Unary { op, arg } => {
            let v = eval(cx, env, arg)?;
            match (op, v) {
                (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                (UnOp::Neg, Value::Int(i)) => negate(i).map(Value::Int),
                (UnOp::BitNot, _) => Err(unimplemented!("LANG-084", "bit operations")),
                (op, v) => Err(bug(format!("{op:?} applied to {v:?}"))),
            }
        }
        Expr::Binary { op, lhs, rhs } => match op {
            BinOp::And => {
                if !truth(&eval(cx, env, lhs)?)? {
                    return Ok(Value::Bool(false));
                }
                Ok(Value::Bool(truth(&eval(cx, env, rhs)?)?))
            }
            BinOp::Or => {
                if truth(&eval(cx, env, lhs)?)? {
                    return Ok(Value::Bool(true));
                }
                Ok(Value::Bool(truth(&eval(cx, env, rhs)?)?))
            }
            _ => {
                let l = eval(cx, env, lhs)?;
                let r = eval(cx, env, rhs)?;
                binary(op, l, r)
            }
        },
        Expr::If { cond, then, els } => {
            if truth(&eval(cx, env, cond)?)? {
                eval(cx, env, then)
            } else {
                eval(cx, env, els)
            }
        }
        Expr::Construct { ty, variant, fields } => {
            let mut vs = Vec::with_capacity(fields.len());
            for f in fields {
                vs.push(eval(cx, env, f)?);
            }
            match (cx.program.types.get(*ty), variant) {
                (Some(TypeDef::Option(_)), Some(1)) => match <[Value; 1]>::try_from(vs) {
                    Ok([v]) => Ok(Value::Option(Some(Arc::new(v)))),
                    Err(_) => Err(bug("`Some` with the wrong number of values".into())),
                },
                (Some(TypeDef::Option(_)), Some(0)) => Ok(Value::Option(None)),
                (Some(TypeDef::Tuple(_)), None) => Ok(Value::Tuple(vs.into())),
                (Some(TypeDef::Struct(_)), None) => Ok(Value::Struct(vs.into())),
                (Some(TypeDef::Enum(_)), Some(v)) => Ok(Value::Enum {
                    variant: *v,
                    fields: vs.into(),
                }),
                (other, v) => Err(bug(format!("constructing {other:?} variant {v:?}"))),
            }
        }
        Expr::Field { base, index } => match eval(cx, env, base)? {
            Value::Tuple(fs) | Value::Struct(fs) => fs
                .get(*index as usize)
                .cloned()
                .ok_or_else(|| bug(format!("field {index} out of range"))),
            other => Err(bug(format!("field {index} of {other:?}"))),
        },
        Expr::Match { scrut, arms } => {
            let v = eval(cx, env, scrut)?;
            for (pat, guard, body) in arms {
                // An arm's bindings are local to it.
                let mut local = env.to_vec();
                if !matches(cx, &mut local, pat, &v, &mut Vec::new())? {
                    continue;
                }
                if let Some(g) = guard
                    && !truth(&eval(cx, &local, g)?)?
                {
                    continue;
                }
                return eval(cx, &local, body);
            }
            Err(bug(format!("no match arm matched {v:?}")))
        }
        Expr::Call {
            f: FnRef::Builtin(BuiltinFn::Lib(f)),
            args,
        } => crate::func::library(cx, env, *f, args),
        Expr::Call { f: FnRef::Builtin(f), args } => builtin(cx, env, f, args),
        Expr::Call { f: FnRef::Fn(f), args } => crate::func::call(cx, env, *f, args),
        Expr::Collection { kind, elems } => {
            let mut vs = Vec::with_capacity(elems.len());
            for x in elems {
                vs.push(eval(cx, env, x)?);
            }
            Ok(match kind {
                CollKind::Vec => Value::Vec(vs.into()),
                CollKind::Set => Value::Set(Arc::new(vs.into_iter().collect::<BTreeSet<Value>>())),
                CollKind::Map => {
                    let mut m = BTreeMap::new();
                    for pair in vs {
                        let Value::Tuple(kv) = pair else {
                            return Err(bug(format!("a map entry {pair:?}")));
                        };
                        let [k, v] = &*kv else {
                            return Err(bug(format!("a map entry {kv:?}")));
                        };
                        m.insert(k.clone(), v.clone());
                    }
                    Value::Map(Arc::new(m))
                }
            })
        }
        Expr::Lattice { op, args } => {
            let (kind, lop) = lattice_op(cx, op)?;
            let mut vs = Vec::with_capacity(args.len());
            for a in args {
                vs.push(eval(cx, env, a)?);
            }
            Ok(kind.eval(lop, &vs)?)
        }
        Expr::Let { pat, value, body } => crate::func::let_expr(cx, env, pat, value, body),
        Expr::Closure { .. } => Err(bug("a closure evaluated outside a combinator".into())),
    }
}

fn param(cx: &Ctx<'_>, p: ParamId) -> ExprResult<Value> {
    if let Some(v) = cx.shared.params.get(&p) {
        return Ok(v.clone());
    }
    let decl = cx
        .program
        .params
        .get(p)
        .ok_or_else(|| bug(format!("parameter {p:?} is not declared")))?;
    let Some(c) = decl.default else {
        return Err(ExprError::Eval(EvalError::Unbound(decl.name.to_string())));
    };
    cx.program
        .consts
        .get(c)
        .cloned()
        .ok_or_else(|| bug(format!("the default of {} is not a constant", decl.name)))
}

fn lattice_op<'s>(cx: &'s Ctx<'_>, op: &LatOpRef) -> ExprResult<(&'s Kind, blossom_lattice::Op)> {
    let kind = cx
        .shared
        .kinds
        .get(op.lattice.index())
        .and_then(Option::as_ref)
        .ok_or_else(|| unimplemented!("LANG-124", &format!("lattice {:?}", op.lattice)))?;
    let lop = blossom_lattice::Op::from_name(kind, op.op.as_str())
        .ok_or_else(|| bug(format!("`{}` is not an operation of {kind:?}", op.op)))?;
    Ok((kind, lop))
}

fn builtin(cx: &Ctx<'_>, env: &[Option<Value>], f: &BuiltinFn, args: &[Expr]) -> ExprResult<Value> {
    let arg = |i: usize| -> ExprResult<Value> {
        let e = args.get(i).ok_or_else(|| bug(format!("{f:?} is missing argument {i}")))?;
        eval(cx, env, e)
    };
    match f {
        BuiltinFn::Len => {
            let n = match arg(0)? {
                Value::Str(s) => s.len(),
                Value::Bytes(b) => b.len(),
                Value::Vec(v) => v.len(),
                Value::Set(s) => s.len(),
                Value::Map(m) => m.len(),
                other => return Err(bug(format!("`len` of {other:?}"))),
            };
            Ok(Value::Int(IntValue::U64(n as u64)))
        }
        BuiltinFn::Size { role } => Ok(Value::Int(IntValue::U64(cx.shared.role_size(*role)))),
        BuiltinFn::IntCast(to) => match arg(0)? {
            Value::Int(i) => {
                let wide = match i {
                    IntValue::U128(u) => i128::try_from(u).ok(),
                    other => other.to_i128(),
                };
                let cast = match (i, to) {
                    // A u128 above i128::MAX fits only a u128.
                    (IntValue::U128(u), blossom_value::types::IntTy::U128) => Some(IntValue::U128(u)),
                    _ => wide.and_then(|w| IntValue::from_i128(*to, w)),
                };
                cast.map(Value::Int)
                    .ok_or_else(|| ExprError::Arithmetic(format!("{i:?} as {} is out of range", to.name())))
            }
            other => Err(bug(format!("an integer cast of {other:?}"))),
        },
        BuiltinFn::Concat => match (arg(0)?, arg(1)?) {
            (Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{a}{b}").into())),
            (Value::Bytes(a), Value::Bytes(b)) => Ok(Value::Bytes(a.iter().chain(b.iter()).copied().collect())),
            (Value::Vec(a), Value::Vec(b)) => Ok(Value::Vec(a.iter().chain(b.iter()).cloned().collect())),
            (a, b) => Err(bug(format!("`++` on {a:?} and {b:?}"))),
        },
        BuiltinFn::Contains => {
            let (c, x) = (arg(0)?, arg(1)?);
            Ok(Value::Bool(match &c {
                Value::Set(s) => s.contains(&x),
                Value::Vec(v) => v.contains(&x),
                Value::Map(m) => m.contains_key(&x),
                other => return Err(bug(format!("`contains` on {other:?}"))),
            }))
        }
        BuiltinFn::Prio { site } => {
            // $prio(site, X̄, Ȳ) = (PRF_σc(site, fp(X̄), fp(Ȳ)), Ȳ) (SEM-084).
            let (x, y) = (arg(0)?, arg(1)?);
            let seed = cx
                .shared
                .choice
                .as_ref()
                .ok_or_else(|| bug("a seeded choice, but the engine was given no seed".into()))?;
            let key = cx
                .program
                .sites
                .get(*site)
                .map(|s| s.key)
                .ok_or_else(|| bug(format!("unknown site {site:?}")))?;
            let p = blossom_value::prf::prf(seed, "prio", &[fingerprint(&x)?, fingerprint(&y)?], &[key])
                .map_err(|e| bug(format!("the PRF: {e}")))?;
            Ok(Value::Tuple(vec![Value::Int(IntValue::U64(p)), y].into()))
        }
        BuiltinFn::RandRange => {
            let (lo, hi) = (arg(0)?, arg(1)?);
            let mut key = Vec::new();
            for i in 2..args.len() {
                key.push(arg(i)?);
            }
            rand_range(cx, &lo, &hi, &key)
        }
        BuiltinFn::Majority { domain } => {
            let MajorityDomain::Role(role) = domain else {
                return Err(unimplemented!("LANG-113", "`majority` over a relation"));
            };
            let members = match arg(0)? {
                Value::Lattice(LatValue::Set(xs)) | Value::Set(xs) => xs
                    .iter()
                    .filter(|v| {
                        matches!(v, Value::Node(n) if cx.shared.roles.get(n.0 as usize).copied().flatten() == Some(*role))
                    })
                    .count() as u64,
                Value::Lattice(LatValue::Bottom) => 0,
                other => return Err(bug(format!("`majority` of {other:?}"))),
            };
            Ok(Value::Bool(members > cx.shared.role_size(*role) / 2))
        }
        other => Err(unimplemented!("LANG-180", &format!("the built-in {other:?}"))),
    }
}

fn fingerprint(v: &Value) -> ExprResult<blossom_value::fp::Fingerprint> {
    blossom_value::fp::fingerprint(v).map_err(|e| bug(format!("fingerprinting {v:?}: {e}")))
}

/// `lo + PRF_σn("rand", fp(k̄), incarnation, tick, attempt) mod span`, redrawing from the incomplete last span so the
/// result is unbiased (LANGUAGE §15.1).
fn rand_range(cx: &Ctx<'_>, lo: &Value, hi: &Value, key: &[Value]) -> ExprResult<Value> {
    let seed = cx
        .shared
        .node_seeds
        .get(cx.node.0 as usize)
        .copied()
        .ok_or_else(|| bug(format!("a `rand` draw on node {}, which has no seed", cx.node.0)))?;
    let fp = blossom_value::fp::fingerprint_row(key).map_err(|e| bug(format!("fingerprinting a rand key: {e}")))?;
    let draw = |span: u128| {
        blossom_value::prf::uniform_below(&seed, "rand", &[fp], &[cx.incarnation, cx.tick.0], span)
            .map_err(|e| bug(format!("rand: {e}")))
    };
    let empty = |l: &dyn std::fmt::Display, h: &dyn std::fmt::Display| {
        ExprError::Arithmetic(format!("rand_range: the range [{l}, {h}) is empty"))
    };
    match (lo, hi) {
        // `u128` bounds may exceed `i128`: they draw in `u128`.
        (Value::Int(IntValue::U128(l)), Value::Int(IntValue::U128(h))) => {
            if h <= l {
                return Err(empty(l, h));
            }
            Ok(Value::Int(IntValue::U128(l + draw(h - l)?)))
        }
        (Value::Duration(_), Value::Duration(_)) | (Value::Int(_), Value::Int(_)) => {
            let bound = |v: &Value| match v {
                Value::Duration(d) => Some(i128::from(d.as_nanos())),
                Value::Int(i) => i.to_i128(),
                _ => None,
            };
            let (Some(l), Some(h)) = (bound(lo), bound(hi)) else {
                return Err(bug(format!("`rand_range` over {lo:?} and {hi:?}")));
            };
            if h <= l {
                return Err(empty(&l, &h));
            }
            // An `i128` span may exceed `i128::MAX`; in two's complement it is exact as a `u128`, and so is the result.
            let span = h.cast_unsigned().wrapping_sub(l.cast_unsigned());
            let v = l.cast_unsigned().wrapping_add(draw(span)?).cast_signed();
            match lo {
                Value::Duration(_) => Ok(Value::Duration(blossom_value::time::Duration::from_nanos(
                    i64::try_from(v).map_err(|_| bug("a duration out of range".into()))?,
                ))),
                Value::Int(i) => IntValue::from_i128(i.ty(), v)
                    .map(Value::Int)
                    .ok_or_else(|| bug("a rand_range result out of its type".into())),
                _ => Err(bug(format!("`rand_range` over {lo:?}"))),
            }
        }
        (a, b) => Err(bug(format!("`rand_range` over {a:?} and {b:?}"))),
    }
}

fn negate(i: IntValue) -> ExprResult<IntValue> {
    let r = match i {
        IntValue::I8(x) => x.checked_neg().map(IntValue::I8),
        IntValue::I16(x) => x.checked_neg().map(IntValue::I16),
        IntValue::I32(x) => x.checked_neg().map(IntValue::I32),
        IntValue::I64(x) => x.checked_neg().map(IntValue::I64),
        IntValue::I128(x) => x.checked_neg().map(IntValue::I128),
        unsigned => return Err(bug(format!("negating the unsigned {unsigned:?}"))),
    };
    r.ok_or_else(|| ExprError::Arithmetic(format!("-{i:?} overflows")))
}

fn binary(op: &BinOp, l: Value, r: Value) -> ExprResult<Value> {
    use BinOp::*;
    match op {
        Eq => Ok(Value::Bool(l == r)),
        Ne => Ok(Value::Bool(l != r)),
        CanonLt => Ok(Value::Bool(l < r)),
        CanonLe => Ok(Value::Bool(l <= r)),
        Lt | Le | Gt | Ge => {
            // Values of one ordered kind compare by the canonical order, which is numeric within a kind.
            let comparable = match (&l, &r) {
                (Value::Int(a), Value::Int(b)) => a.ty() == b.ty(),
                (Value::Duration(_), Value::Duration(_))
                | (Value::Instant(_), Value::Instant(_))
                | (Value::Str(_), Value::Str(_))
                | (Value::Bytes(_), Value::Bytes(_))
                | (Value::Node(_), Value::Node(_)) => true,
                _ => false,
            };
            if !comparable {
                return Err(bug(format!("ordering {l:?} {op:?} {r:?}")));
            }
            Ok(Value::Bool(match op {
                Lt => l < r,
                Le => l <= r,
                Gt => l > r,
                _ => l >= r,
            }))
        }
        Add | Sub | Mul | Div | Rem => arithmetic(op, l, r),
        And | Or => Err(bug("`&&`/`||` evaluated strictly".into())),
        BitAnd | BitOr | BitXor | Shl | Shr => Err(unimplemented!("LANG-084", "bit operations")),
    }
}

/// An arithmetic operator as written.
fn op_text(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        _ => "?",
    }
}

fn arithmetic(op: &BinOp, l: Value, r: Value) -> ExprResult<Value> {
    use BinOp::*;
    let overflow = |l: &dyn std::fmt::Debug, r: &dyn std::fmt::Debug| {
        ExprError::Arithmetic(format!("{l:?} {} {r:?} overflows or divides by zero", op_text(op)))
    };
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => int_op(op, a, b).map(Value::Int),
        (Value::Duration(a), Value::Duration(b)) if matches!(op, Add | Sub) => {
            let v = if *op == Add { a.checked_add(b) } else { a.checked_sub(b) };
            v.map(Value::Duration).ok_or_else(|| overflow(&a, &b))
        }
        (Value::Instant(a), Value::Duration(d)) if matches!(op, Add | Sub) => {
            let v = if *op == Add { a.checked_add(d) } else { a.checked_sub(d) };
            v.map(Value::Instant).ok_or_else(|| overflow(&a, &d))
        }
        (Value::Duration(d), Value::Instant(a)) if *op == Add => {
            a.checked_add(d).map(Value::Instant).ok_or_else(|| overflow(&d, &a))
        }
        (Value::Instant(a), Value::Instant(b)) if *op == Sub => {
            a.checked_since(b).map(Value::Duration).ok_or_else(|| overflow(&a, &b))
        }
        (l, r) => Err(bug(format!("arithmetic {op:?} on {l:?} and {r:?}"))),
    }
}

macro_rules! same_width {
    ($op:expr, $a:expr, $b:expr, $($v:ident),*) => {
        match ($a, $b) {
            $((IntValue::$v(x), IntValue::$v(y)) => {
                let r = match $op {
                    BinOp::Add => x.checked_add(y),
                    BinOp::Sub => x.checked_sub(y),
                    BinOp::Mul => x.checked_mul(y),
                    BinOp::Div => x.checked_div(y),
                    BinOp::Rem => x.checked_rem(y),
                    _ => None,
                };
                r.map(IntValue::$v)
                    .ok_or_else(|| ExprError::Arithmetic(format!("{x} {} {y} overflows or divides by zero", op_text($op))))
            })*
            (a, b) => Err(bug(format!("arithmetic on {a:?} and {b:?}"))),
        }
    };
}

pub(crate) fn int_op(op: &BinOp, a: IntValue, b: IntValue) -> ExprResult<IntValue> {
    same_width!(op, a, b, U8, U16, U32, U64, U128, I8, I16, I32, I64, I128)
}

/// Matches `v` against `pat`, binding the pattern's unbound variables (recorded in `newly`); a bound variable is an
/// equality test.
pub(crate) fn matches(
    cx: &Ctx<'_>,
    env: &mut [Option<Value>],
    pat: &Pattern,
    v: &Value,
    newly: &mut Vec<usize>,
) -> ExprResult<bool> {
    match pat {
        Pattern::Wild => Ok(true),
        Pattern::Const(c) => Ok(term(cx, env, &Term::Const(*c))? == *v),
        Pattern::Var(var) => match env.get_mut(var.index()) {
            Some(slot @ None) => {
                *slot = Some(v.clone());
                newly.push(var.index());
                Ok(true)
            }
            Some(Some(existing)) => Ok(existing == v),
            None => Err(bug(format!("variable {var:?} out of range"))),
        },
        Pattern::Tuple(ps) => {
            let Value::Tuple(fs) = v else { return Ok(false) };
            if fs.len() != ps.len() {
                return Ok(false);
            }
            for (p, f) in ps.iter().zip(fs.iter()) {
                if !matches(cx, env, p, f, newly)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Pattern::Variant { ty, number, fields } => {
            let payload: Vec<Value> = match (cx.program.types.get(*ty), v) {
                (Some(TypeDef::Option(_)), Value::Option(o)) => match (number, o) {
                    (1, Some(x)) => vec![(**x).clone()],
                    (0, None) => Vec::new(),
                    _ => return Ok(false),
                },
                (_, Value::Enum { variant, fields: fs }) if variant == number => fs.to_vec(),
                _ => return Ok(false),
            };
            if payload.len() != fields.len() {
                return Ok(false);
            }
            for (p, f) in fields.iter().zip(&payload) {
                if !matches(cx, env, p, f, newly)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Pattern::Struct { fields, .. } => {
            let Value::Struct(fs) = v else { return Ok(false) };
            for (i, p) in fields {
                let Some(f) = fs.get(*i as usize) else { return Ok(false) };
                if !matches(cx, env, p, f, newly)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
}

/// The values a generator ranges over, in canonical order.
pub(crate) fn generate(cx: &Ctx<'_>, env: &[Option<Value>], src: &GenSource) -> ExprResult<Vec<Value>> {
    match src {
        GenSource::Range {
            lo,
            hi,
            kind,
            ring_bits: None,
        } => {
            let (Value::Int(a), Value::Int(b)) = (eval(cx, env, lo)?, eval(cx, env, hi)?) else {
                return Err(bug("a range over non-integers".into()));
            };
            let ty = a.ty();
            let (Some(a), Some(b)) = (a.to_i128(), b.to_i128()) else {
                return Err(unimplemented!("LANG-092", "ranges beyond i128"));
            };
            let (start, end) = match kind {
                RangeKind::HalfOpen => (a, b),
                RangeKind::Closed => (a, b.saturating_add(1)),
                RangeKind::OpenOpen => (a.saturating_add(1), b),
                RangeKind::OpenClosed => (a.saturating_add(1), b.saturating_add(1)),
            };
            let mut out = Vec::new();
            let mut i = start;
            while i < end {
                out.push(Value::Int(
                    IntValue::from_i128(ty, i).ok_or_else(|| bug("a range value out of its type".into()))?,
                ));
                i += 1;
            }
            Ok(out)
        }
        GenSource::Range { ring_bits: Some(_), .. } => Err(unimplemented!("LANG-026", "ring-interval generators")),
        GenSource::Value(e) => Ok(match eval(cx, env, e)? {
            Value::Vec(v) => v.to_vec(),
            Value::Set(s) => s.iter().cloned().collect(),
            Value::Map(m) => m
                .iter()
                .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()].into()))
                .collect(),
            other => return Err(bug(format!("a generator over {other:?}"))),
        }),
        GenSource::Lattice(e) => Ok(match eval(cx, env, e)? {
            Value::Lattice(LatValue::Set(s)) => s.iter().cloned().collect(),
            Value::Lattice(LatValue::Map(m)) => m
                .iter()
                .map(|(k, v)| Value::Tuple(vec![k.clone(), Value::Lattice(v.clone())].into()))
                .collect(),
            other => return Err(bug(format!("a lattice generator over {other:?}"))),
        }),
        GenSource::TableFn { .. } => Err(unimplemented!("LANG-183", "table-function generators")),
    }
}

/// The checked sum of integers of one type.
pub(crate) fn int_sum<'a>(mut values: impl Iterator<Item = &'a Value>) -> ExprResult<Value> {
    let Some(Value::Int(mut acc)) = values.next().cloned() else {
        return Err(bug("a sum over an empty group or non-integers".into()));
    };
    for v in values {
        let Value::Int(x) = v else {
            return Err(bug(format!("a sum over {v:?}")));
        };
        acc = int_op(&BinOp::Add, acc, *x)?;
    }
    Ok(Value::Int(acc))
}

/// Whether an expression reads a time-varying scalar (LANGUAGE §15.1, ARCHITECTURE §3.4.2): its value can change from
/// tick to tick with no relation changing, so a rule reading one is re-evaluated at every tick.
pub(crate) fn time_varying(e: &Expr) -> bool {
    match e {
        Expr::Scalar(BuiltinScalar::Now | BuiltinScalar::Tick | BuiltinScalar::Incarnation) => true,
        Expr::Scalar(_) | Expr::Term(_) | Expr::Param(_) => false,
        Expr::Call { f, args } => {
            matches!(
                f,
                FnRef::Builtin(BuiltinFn::Rand | BuiltinFn::RandFloat | BuiltinFn::RandRange | BuiltinFn::RandPrio { .. })
            ) || args.iter().any(time_varying)
        }
        Expr::Unary { arg, .. } => time_varying(arg),
        Expr::Binary { lhs, rhs, .. } => time_varying(lhs) || time_varying(rhs),
        Expr::Construct { fields, .. } => fields.iter().any(time_varying),
        Expr::Field { base, .. } => time_varying(base),
        Expr::If { cond, then, els } => time_varying(cond) || time_varying(then) || time_varying(els),
        Expr::Match { scrut, arms } => {
            time_varying(scrut)
                || arms
                    .iter()
                    .any(|(_, g, b)| g.as_ref().is_some_and(time_varying) || time_varying(b))
        }
        Expr::Collection { elems, .. } => elems.iter().any(time_varying),
        Expr::Lattice { args, .. } => args.iter().any(time_varying),
        Expr::Let { value, body, .. } => time_varying(value) || time_varying(body),
        Expr::Closure { body, .. } => time_varying(body),
    }
}
