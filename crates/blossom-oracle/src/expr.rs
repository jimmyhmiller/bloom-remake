//! Expression evaluation over `Value`. Arithmetic is checked: overflow and division by zero are the runtime hard
//! error BLSR004 (ARCHITECTURE §12.1). Division truncates toward zero.

use blossom_base::{code, internal_error};
use blossom_ir::core::{
    BinOp, BuiltinFn, BuiltinScalar, CollKind, Expr, FnRef, GenSource, LatOpRef, Pattern, Program, RangeKind, Term,
    UnOp,
};
use blossom_value::{
    TypeDef, Value,
    time::{Instant, NodeId, Tick},
    value::{IntValue, LatValue},
};

use crate::{Oracle, OracleError};

/// What an expression can read besides its variables.
pub(crate) struct Scope<'a> {
    pub program: &'a Program,
    pub node: NodeId,
    pub tick: Tick,
    pub now: Instant,
    pub oracle: &'a Oracle,
}

/// A runtime hard error found while evaluating an expression, before it is attributed to a rule and tick.
#[derive(Debug)]
pub(crate) enum ExprError {
    /// BLSR004: arithmetic overflow or division by zero.
    Arithmetic(String),
    /// BLSR006: two different values merged into an `LPoint`.
    Conflict(String),
    Oracle(OracleError),
}

impl From<blossom_lattice::LatticeError> for ExprError {
    fn from(e: blossom_lattice::LatticeError) -> Self {
        use blossom_lattice::LatticeError as L;
        match e {
            L::Conflict(..) => ExprError::Conflict(e.to_string()),
            // A negative number in an `LPSet` is an out-of-range value, like an out-of-range cast.
            L::Arithmetic(m) | L::Domain(m) => ExprError::Arithmetic(m),
            L::Shape(m) => ExprError::Oracle(internal_error!("a lattice operation on the wrong values: {m}").into()),
        }
    }
}

impl From<crate::cells::MergeError> for ExprError {
    fn from(e: crate::cells::MergeError) -> Self {
        match e {
            crate::cells::MergeError::Lattice(l) => l.into(),
            crate::cells::MergeError::Internal(i) => ExprError::Oracle(i.into()),
        }
    }
}

impl From<OracleError> for ExprError {
    fn from(e: OracleError) -> Self {
        ExprError::Oracle(e)
    }
}

pub(crate) type ExprResult<T> = Result<T, ExprError>;

pub(crate) fn term(scope: &Scope<'_>, env: &[Option<Value>], t: &Term) -> ExprResult<Value> {
    match t {
        Term::Var(v) => env
            .get(v.index())
            .cloned()
            .flatten()
            .ok_or_else(|| ExprError::Oracle(internal_error!("variable {v:?} read before it is bound").into())),
        Term::Const(c) => scope
            .program
            .consts
            .get(*c)
            .cloned()
            .ok_or_else(|| ExprError::Oracle(internal_error!("unknown constant {c:?}").into())),
        Term::Wild => Err(ExprError::Oracle(internal_error!("`_` evaluated as a value").into())),
    }
}

pub(crate) fn eval(scope: &Scope<'_>, env: &[Option<Value>], e: &Expr) -> ExprResult<Value> {
    match e {
        Expr::Term(t) => term(scope, env, t),
        Expr::Scalar(BuiltinScalar::SelfNode) => Ok(Value::Node(scope.node)),
        Expr::Scalar(BuiltinScalar::Tick) => Ok(Value::Int(IntValue::U64(scope.tick.0))),
        Expr::Scalar(BuiltinScalar::Now) => Ok(Value::Instant(scope.now)),
        Expr::Scalar(s @ (BuiltinScalar::Incarnation | BuiltinScalar::Host)) => Err(ExprError::Oracle(
            blossom_base::unimplemented_error!("LANG-180", "`${s:?}` in the oracle (WP M4.1)").into(),
        )),
        Expr::Unary { op, arg } => {
            let v = eval(scope, env, arg)?;
            unary(op.clone(), v)
        }
        Expr::Binary { op, lhs, rhs } => {
            // `&&` and `||` short-circuit.
            if matches!(op, BinOp::And | BinOp::Or) {
                let l = truth(&eval(scope, env, lhs)?)?;
                return match (op, l) {
                    (BinOp::And, false) => Ok(Value::Bool(false)),
                    (BinOp::Or, true) => Ok(Value::Bool(true)),
                    _ => Ok(Value::Bool(truth(&eval(scope, env, rhs)?)?)),
                };
            }
            let l = eval(scope, env, lhs)?;
            let r = eval(scope, env, rhs)?;
            binary(op.clone(), l, r)
        }
        Expr::If { cond, then, els } => {
            if truth(&eval(scope, env, cond)?)? {
                eval(scope, env, then)
            } else {
                eval(scope, env, els)
            }
        }
        Expr::Param(p) => scope.oracle.param(*p).map_err(ExprError::Oracle),
        Expr::Construct { ty, variant, fields } => {
            let mut vs = Vec::with_capacity(fields.len());
            for f in fields {
                vs.push(eval(scope, env, f)?);
            }
            construct(scope, *ty, *variant, vs)
        }
        Expr::Field { base, index } => {
            let v = eval(scope, env, base)?;
            let fields = match &v {
                Value::Tuple(fs) | Value::Struct(fs) => fs,
                other => {
                    return Err(ExprError::Oracle(internal_error!("field {index} of {other:?}").into()));
                }
            };
            fields
                .get(*index as usize)
                .cloned()
                .ok_or_else(|| ExprError::Oracle(internal_error!("field {index} out of range").into()))
        }
        Expr::Match { scrut, arms } => {
            let v = eval(scope, env, scrut)?;
            // Arm bindings live in a copy of the environment: they are local to the arm.
            for (pat, guard, body) in arms {
                let mut local = env.to_vec();
                let mut newly = Vec::new();
                if !matches(scope, &mut local, pat, &v, &mut newly)? {
                    continue;
                }
                if let Some(g) = guard
                    && !truth(&eval(scope, &local, g)?)?
                {
                    continue;
                }
                return eval(scope, &local, body);
            }
            Err(ExprError::Oracle(internal_error!("no match arm matched {v:?}").into()))
        }
        Expr::Call {
            f: FnRef::Builtin(BuiltinFn::Len),
            args,
        } => {
            let [a] = args.as_slice() else {
                return Err(ExprError::Oracle(internal_error!("`len` takes one argument").into()));
            };
            let n = match eval(scope, env, a)? {
                Value::Str(s) => s.len(),
                Value::Bytes(b) => b.len(),
                Value::Vec(v) => v.len(),
                Value::Set(s) => s.len(),
                Value::Map(m) => m.len(),
                other => return Err(ExprError::Oracle(internal_error!("`len` of {other:?}").into())),
            };
            Ok(Value::Int(IntValue::U64(n as u64)))
        }
        Expr::Call {
            f: FnRef::Builtin(BuiltinFn::Size { role }),
            ..
        } => Ok(Value::Int(IntValue::U64(scope.oracle.role_size(*role)))),
        Expr::Call {
            f: FnRef::Builtin(BuiltinFn::Concat),
            args,
        } => {
            let [a, b] = args.as_slice() else {
                return Err(ExprError::Oracle(internal_error!("`++` takes two operands").into()));
            };
            Ok(match (eval(scope, env, a)?, eval(scope, env, b)?) {
                (Value::Str(x), Value::Str(y)) => Value::Str(format!("{x}{y}").into()),
                (Value::Bytes(x), Value::Bytes(y)) => Value::Bytes(x.iter().chain(y.iter()).copied().collect()),
                (Value::Vec(x), Value::Vec(y)) => Value::Vec(x.iter().chain(y.iter()).cloned().collect()),
                (x, y) => return Err(ExprError::Oracle(internal_error!("`++` on {x:?} and {y:?}").into())),
            })
        }
        Expr::Call {
            f: FnRef::Builtin(BuiltinFn::Contains),
            args,
        } => {
            let [c, x] = args.as_slice() else {
                return Err(ExprError::Oracle(
                    internal_error!("`contains` takes two arguments").into(),
                ));
            };
            let (c, x) = (eval(scope, env, c)?, eval(scope, env, x)?);
            Ok(Value::Bool(match &c {
                Value::Set(s) => s.contains(&x),
                Value::Vec(v) => v.contains(&x),
                Value::Map(m) => m.contains_key(&x),
                other => return Err(ExprError::Oracle(internal_error!("`contains` on {other:?}").into())),
            }))
        }
        Expr::Call {
            f: FnRef::Builtin(BuiltinFn::Prio { site }),
            args,
        } => {
            // $prio(site, X̄, Ȳ) = (PRF_σc(site, fp(X̄), fp(Ȳ)), Ȳ) (SEM-084).
            let [x, y] = args.as_slice() else {
                return Err(ExprError::Oracle(internal_error!("`$prio` takes two arguments").into()));
            };
            let (x, y) = (eval(scope, env, x)?, eval(scope, env, y)?);
            let seed = scope.oracle.choice.as_ref().ok_or_else(|| {
                ExprError::Oracle(internal_error!("a seeded choice, but the oracle was given no seed").into())
            })?;
            let key = scope
                .program
                .sites
                .get(*site)
                .map(|s| s.key)
                .ok_or_else(|| ExprError::Oracle(internal_error!("unknown site {site:?}").into()))?;
            let fp = |v: &Value| {
                blossom_value::fp::fingerprint(v)
                    .map_err(|e| ExprError::Oracle(internal_error!("fingerprinting {v:?}: {e}").into()))
            };
            let p = blossom_value::prf::prf(seed, "prio", &[fp(&x)?, fp(&y)?], &[key])
                .map_err(|e| ExprError::Oracle(internal_error!("the PRF: {e}").into()))?;
            Ok(Value::Tuple(vec![Value::Int(IntValue::U64(p)), y].into()))
        }
        Expr::Call { .. } => Err(ExprError::Oracle(
            blossom_base::unimplemented_error!("LANG-180", "function calls in the oracle (WP M4.1)").into(),
        )),
        Expr::Collection { kind, elems } => {
            let mut vs = Vec::with_capacity(elems.len());
            for x in elems {
                vs.push(eval(scope, env, x)?);
            }
            Ok(match kind {
                CollKind::Vec => Value::Vec(vs.into()),
                CollKind::Set => Value::Set(std::sync::Arc::new(vs.into_iter().collect())),
                CollKind::Map => {
                    let mut m = std::collections::BTreeMap::new();
                    for pair in vs {
                        match pair {
                            Value::Tuple(kv) => match &*kv {
                                [k, v] => {
                                    m.insert(k.clone(), v.clone());
                                }
                                _ => {
                                    return Err(ExprError::Oracle(internal_error!("a map entry {kv:?}").into()));
                                }
                            },
                            other => return Err(ExprError::Oracle(internal_error!("a map entry {other:?}").into())),
                        }
                    }
                    Value::Map(std::sync::Arc::new(m))
                }
            })
        }
        Expr::Lattice { op, args } => {
            let (kind, lop) = lattice_op(scope, op)?;
            let mut vs = Vec::with_capacity(args.len());
            for a in args {
                vs.push(eval(scope, env, a)?);
            }
            Ok(kind.eval(lop, &vs)?)
        }
        Expr::Let { .. } | Expr::Closure { .. } => Err(ExprError::Oracle(
            internal_error!("`let` and closures appear only in function bodies").into(),
        )),
    }
}

/// The lattice and operation an IR operation reference names.
fn lattice_op<'s>(scope: &'s Scope<'_>, op: &LatOpRef) -> ExprResult<(&'s blossom_lattice::Kind, blossom_lattice::Op)> {
    let kind = scope
        .oracle
        .kinds
        .get(op.lattice.index())
        .and_then(Option::as_ref)
        .ok_or_else(|| {
            ExprError::Oracle(
                blossom_base::unimplemented_error!("LANG-124", "lattice {:?} in the oracle", op.lattice).into(),
            )
        })?;
    let lop = blossom_lattice::Op::from_name(kind, op.op.as_str()).ok_or_else(|| {
        ExprError::Oracle(internal_error!("lattice operation `{}` is not in {kind:?}'s catalogue", op.op).into())
    })?;
    Ok((kind, lop))
}

/// A constructed value of type `ty`.
fn construct(scope: &Scope<'_>, ty: blossom_base::TypeId, variant: Option<u32>, vs: Vec<Value>) -> ExprResult<Value> {
    match (scope.program.types.get(ty), variant) {
        (Some(TypeDef::Option(_)), Some(1)) => match <[Value; 1]>::try_from(vs) {
            Ok([v]) => Ok(Value::some(v)),
            Err(_) => Err(ExprError::Oracle(internal_error!("`Some` takes one value").into())),
        },
        (Some(TypeDef::Option(_)), Some(0)) => Ok(Value::none()),
        (Some(TypeDef::Tuple(_)), None) => Ok(Value::Tuple(vs.into())),
        (Some(TypeDef::Struct(_)), None) => Ok(Value::Struct(vs.into())),
        (Some(TypeDef::Enum(_)), Some(v)) => Ok(Value::Enum {
            variant: v,
            fields: vs.into(),
        }),
        (other, v) => Err(ExprError::Oracle(
            internal_error!("constructing {other:?} variant {v:?}").into(),
        )),
    }
}

/// Matches `v` against `pat`, binding the pattern's unbound variables in `env` (recorded in `newly` so the caller
/// can undo them). A bound variable is an equality test.
pub(crate) fn matches(
    scope: &Scope<'_>,
    env: &mut [Option<Value>],
    pat: &Pattern,
    v: &Value,
    newly: &mut Vec<usize>,
) -> ExprResult<bool> {
    match pat {
        Pattern::Wild => Ok(true),
        Pattern::Const(c) => Ok(term(scope, env, &Term::Const(*c))? == *v),
        Pattern::Var(var) => match env.get_mut(var.index()) {
            Some(slot @ None) => {
                *slot = Some(v.clone());
                newly.push(var.index());
                Ok(true)
            }
            Some(Some(existing)) => Ok(existing == v),
            None => Err(ExprError::Oracle(
                internal_error!("variable {var:?} out of range").into(),
            )),
        },
        Pattern::Tuple(ps) => {
            let Value::Tuple(fs) = v else { return Ok(false) };
            if fs.len() != ps.len() {
                return Ok(false);
            }
            for (p, f) in ps.iter().zip(fs.iter()) {
                if !matches(scope, env, p, f, newly)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Pattern::Variant { ty, number, fields } => {
            let payload: Vec<Value> = match (scope.program.types.get(*ty), v) {
                (Some(TypeDef::Option(_)), Value::Option(o)) => match (number, o) {
                    (1, Some(x)) => vec![(**x).clone()],
                    (0, None) => Vec::new(),
                    _ => return Ok(false),
                },
                (_, Value::Enum { variant, fields: fs }) => {
                    if variant != number {
                        return Ok(false);
                    }
                    fs.to_vec()
                }
                _ => return Ok(false),
            };
            if payload.len() != fields.len() {
                return Ok(false);
            }
            for (p, f) in fields.iter().zip(&payload) {
                if !matches(scope, env, p, f, newly)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Pattern::Struct { fields, .. } => {
            let Value::Struct(fs) = v else { return Ok(false) };
            for (i, p) in fields {
                let Some(f) = fs.get(*i as usize) else { return Ok(false) };
                if !matches(scope, env, p, f, newly)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
}

/// The values a generator ranges over, in canonical order.
pub(crate) fn generate(scope: &Scope<'_>, env: &[Option<Value>], src: &GenSource) -> ExprResult<Vec<Value>> {
    match src {
        GenSource::Range {
            lo,
            hi,
            kind,
            ring_bits: None,
        } => {
            let (Value::Int(a), Value::Int(b)) = (eval(scope, env, lo)?, eval(scope, env, hi)?) else {
                return Err(ExprError::Oracle(internal_error!("a range over non-integers").into()));
            };
            let ty = a.ty();
            let (Some(a), Some(b)) = (a.to_i128(), b.to_i128()) else {
                return Err(ExprError::Oracle(
                    blossom_base::unimplemented_error!("LANG-092", "ranges beyond i128 in the oracle").into(),
                ));
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
                let v = IntValue::from_i128(ty, i)
                    .ok_or_else(|| ExprError::Oracle(internal_error!("a range value out of its type").into()))?;
                out.push(Value::Int(v));
                i += 1;
            }
            Ok(out)
        }
        GenSource::Value(e) => Ok(match eval(scope, env, e)? {
            Value::Vec(v) => v.to_vec(),
            Value::Set(s) => s.iter().cloned().collect(),
            Value::Map(m) => m
                .iter()
                .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()].into()))
                .collect(),
            other => return Err(ExprError::Oracle(internal_error!("a generator over {other:?}").into())),
        }),
        // A set-like lattice yields its elements, a map lattice its (key, value) pairs (LANG-123).
        GenSource::Lattice(e) => Ok(match eval(scope, env, e)? {
            Value::Lattice(LatValue::Set(s)) => s.iter().cloned().collect(),
            Value::Lattice(LatValue::Map(m)) => m
                .iter()
                .map(|(k, v)| Value::Tuple(vec![k.clone(), Value::Lattice(v.clone())].into()))
                .collect(),
            other => {
                return Err(ExprError::Oracle(
                    internal_error!("a lattice generator over {other:?}").into(),
                ));
            }
        }),
        _ => Err(ExprError::Oracle(
            blossom_base::unimplemented_error!("LANG-088", "this generator in the oracle").into(),
        )),
    }
}

pub(crate) fn truth(v: &Value) -> ExprResult<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        other => Err(ExprError::Oracle(
            internal_error!("a condition evaluated to {other:?}").into(),
        )),
    }
}

fn unary(op: UnOp, v: Value) -> ExprResult<Value> {
    match (op, v) {
        (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
        (UnOp::Neg, Value::Int(i)) => int_neg(i).map(Value::Int),
        (UnOp::BitNot, _) => Err(ExprError::Oracle(
            blossom_base::unimplemented_error!("LANG-084", "bit operations in the oracle (WP M4.1)").into(),
        )),
        (op, v) => Err(ExprError::Oracle(internal_error!("{op:?} applied to {v:?}").into())),
    }
}

fn binary(op: BinOp, l: Value, r: Value) -> ExprResult<Value> {
    use BinOp::*;
    match op {
        Eq => Ok(Value::Bool(l == r)),
        Ne => Ok(Value::Bool(l != r)),
        CanonLt => Ok(Value::Bool(l < r)),
        CanonLe => Ok(Value::Bool(l <= r)),
        Lt | Le | Gt | Ge => {
            // Integers of one type, durations, instants, strings and bytes order by `Value`'s canonical order, which
            // is numeric (lexicographic by bytes for strings) within one kind.
            let same_kind = match (&l, &r) {
                (Value::Int(a), Value::Int(b)) => a.ty() == b.ty(),
                (Value::Duration(_), Value::Duration(_))
                | (Value::Instant(_), Value::Instant(_))
                | (Value::Str(_), Value::Str(_))
                | (Value::Bytes(_), Value::Bytes(_))
                | (Value::Node(_), Value::Node(_)) => true,
                _ => false,
            };
            if !same_kind {
                return Err(ExprError::Oracle(internal_error!("ordering {l:?} {op:?} {r:?}").into()));
            }
            Ok(Value::Bool(match op {
                Lt => l < r,
                Le => l <= r,
                Gt => l > r,
                _ => l >= r,
            }))
        }
        Add | Sub | Mul | Div | Rem => match (l, r) {
            (Value::Int(a), Value::Int(b)) => int_arith(op, a, b).map(Value::Int),
            (Value::Duration(a), Value::Duration(b)) if matches!(op, Add | Sub) => {
                let r = if op == Add { a.checked_add(b) } else { a.checked_sub(b) };
                r.map(Value::Duration)
                    .ok_or_else(|| ExprError::Arithmetic(format!("{a:?} {} {b:?} overflows", op_text(op))))
            }
            (Value::Instant(a), Value::Duration(d)) if matches!(op, Add | Sub) => {
                let r = if op == Add { a.checked_add(d) } else { a.checked_sub(d) };
                r.map(Value::Instant)
                    .ok_or_else(|| ExprError::Arithmetic(format!("{a:?} {} {d:?} overflows", op_text(op))))
            }
            (Value::Duration(d), Value::Instant(a)) if op == Add => a
                .checked_add(d)
                .map(Value::Instant)
                .ok_or_else(|| ExprError::Arithmetic(format!("{d:?} + {a:?} overflows"))),
            (Value::Instant(a), Value::Instant(b)) if op == Sub => a
                .checked_since(b)
                .map(Value::Duration)
                .ok_or_else(|| ExprError::Arithmetic(format!("{a:?} - {b:?} overflows"))),
            (l, r) => Err(ExprError::Oracle(
                internal_error!("arithmetic {op:?} on {l:?} and {r:?}").into(),
            )),
        },
        And | Or => Err(ExprError::Oracle(
            internal_error!("`&&`/`||` reached the strict path").into(),
        )),
        BitAnd | BitOr | BitXor | Shl | Shr => Err(ExprError::Oracle(
            blossom_base::unimplemented_error!("LANG-084", "bit operations in the oracle (WP M4.1)").into(),
        )),
    }
}

macro_rules! int_ops {
    ($op:expr, $a:expr, $b:expr, $($variant:ident),*) => {
        match ($a, $b) {
            $((IntValue::$variant(x), IntValue::$variant(y)) => {
                let r = match $op {
                    BinOp::Add => x.checked_add(y),
                    BinOp::Sub => x.checked_sub(y),
                    BinOp::Mul => x.checked_mul(y),
                    BinOp::Div => x.checked_div(y),
                    BinOp::Rem => x.checked_rem(y),
                    _ => None,
                };
                r.map(IntValue::$variant)
                    .ok_or_else(|| ExprError::Arithmetic(format!("{x} {} {y} overflows or divides by zero", op_text($op))))
            })*
            (a, b) => Err(ExprError::Oracle(internal_error!("arithmetic on {a:?} and {b:?}").into())),
        }
    };
}

fn int_arith(op: BinOp, a: IntValue, b: IntValue) -> ExprResult<IntValue> {
    int_ops!(op, a, b, U8, U16, U32, U64, U128, I8, I16, I32, I64, I128)
}

fn int_neg(i: IntValue) -> ExprResult<IntValue> {
    let r = match i {
        IntValue::I8(x) => x.checked_neg().map(IntValue::I8),
        IntValue::I16(x) => x.checked_neg().map(IntValue::I16),
        IntValue::I32(x) => x.checked_neg().map(IntValue::I32),
        IntValue::I64(x) => x.checked_neg().map(IntValue::I64),
        IntValue::I128(x) => x.checked_neg().map(IntValue::I128),
        unsigned => {
            return Err(ExprError::Oracle(
                internal_error!("negating the unsigned {unsigned:?}").into(),
            ));
        }
    };
    r.ok_or_else(|| ExprError::Arithmetic(format!("-{i:?} overflows")))
}

fn op_text(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        _ => "?",
    }
}

/// The checked sum of integers of one type (BLSR004 on overflow).
pub(crate) fn int_sum<'a>(mut values: impl Iterator<Item = &'a Value>) -> ExprResult<Value> {
    let Some(first) = values.next() else {
        return Err(ExprError::Oracle(internal_error!("sum over an empty group").into()));
    };
    let Value::Int(mut acc) = first.clone() else {
        return Err(ExprError::Oracle(internal_error!("sum over {first:?}").into()));
    };
    for v in values {
        let Value::Int(x) = v else {
            return Err(ExprError::Oracle(internal_error!("sum over {v:?}").into()));
        };
        acc = int_arith(BinOp::Add, acc, *x)?;
    }
    Ok(Value::Int(acc))
}

/// BLSR004's code.
pub(crate) fn arithmetic_code() -> &'static str {
    code!("BLSR004").as_str()
}

/// BLSR006's code.
pub(crate) fn conflict_code() -> &'static str {
    code!("BLSR006").as_str()
}

/// BLSR007's code.
pub(crate) fn fixpoint_code() -> &'static str {
    code!("BLSR007").as_str()
}
