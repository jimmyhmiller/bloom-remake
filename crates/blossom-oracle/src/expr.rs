//! Expression evaluation over `Value`. Arithmetic is checked: overflow and division by zero are the runtime hard
//! error BLSR004 (ARCHITECTURE §12.1). Division truncates toward zero.

use blossom_base::{code, internal_error};
use blossom_ir::core::{BinOp, BuiltinScalar, Expr, Program, Term, UnOp};
use blossom_value::{
    Value,
    time::{NodeId, Tick},
    value::IntValue,
};

use crate::OracleError;

/// What an expression can read besides its variables.
pub(crate) struct Scope<'a> {
    pub program: &'a Program,
    pub node: NodeId,
    pub tick: Tick,
}

/// A runtime hard error found while evaluating an expression, before it is attributed to a rule and tick.
#[derive(Debug)]
pub(crate) enum ExprError {
    /// BLSR004: arithmetic overflow or division by zero.
    Arithmetic(String),
    Oracle(OracleError),
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
        Expr::Scalar(s @ (BuiltinScalar::Now | BuiltinScalar::Incarnation | BuiltinScalar::Host)) => {
            Err(ExprError::Oracle(
                blossom_base::unimplemented_error!("LANG-180", "`${s:?}` in the oracle (WP M4.1)").into(),
            ))
        }
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
        Expr::Param(_) => Err(ExprError::Oracle(
            blossom_base::unimplemented_error!("LANG-010", "deploy-time parameters in the oracle (WP M4.1)").into(),
        )),
        Expr::Call { .. }
        | Expr::Construct { .. }
        | Expr::Field { .. }
        | Expr::Match { .. }
        | Expr::Collection { .. } => Err(ExprError::Oracle(
            blossom_base::unimplemented_error!(
                "LANG-084",
                "calls, constructors, fields, match and collection literals in the oracle (WP M4.1)"
            )
            .into(),
        )),
        Expr::Lattice { .. } => Err(ExprError::Oracle(
            blossom_base::unimplemented_error!("LANG-123", "lattice operations in the oracle (WP M4.1)").into(),
        )),
        Expr::Let { .. } | Expr::Closure { .. } => Err(ExprError::Oracle(
            internal_error!("`let` and closures appear only in function bodies").into(),
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
            let (Value::Int(a), Value::Int(b)) = (&l, &r) else {
                return Err(ExprError::Oracle(internal_error!("ordering {l:?} {op:?} {r:?}").into()));
            };
            if a.ty() != b.ty() {
                return Err(ExprError::Oracle(internal_error!("comparing {a:?} with {b:?}").into()));
            }
            // Integers of one type order numerically under `Value`'s canonical order.
            Ok(Value::Bool(match op {
                Lt => l < r,
                Le => l <= r,
                Gt => l > r,
                _ => l >= r,
            }))
        }
        Add | Sub | Mul | Div | Rem => {
            let (Value::Int(a), Value::Int(b)) = (l, r) else {
                return Err(ExprError::Oracle(
                    internal_error!("arithmetic {op:?} on non-integers").into(),
                ));
            };
            int_arith(op, a, b).map(Value::Int)
        }
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

/// BLSR007's code.
pub(crate) fn fixpoint_code() -> &'static str {
    code!("BLSR007").as_str()
}
