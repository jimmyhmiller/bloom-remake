//! `f64` semantics (LANGUAGE §5.1, LANG-022), shared by every evaluator so they agree bit for bit.
//!
//! Floats are IEEE 754 binary64 with round-to-nearest-even, and every float a program computes is canonical: zero has
//! one sign and NaN one bit pattern. Rust's `+ - * / %`, `sqrt`, `floor`, `ceil`, `round` and `trunc` are correctly
//! rounded on every platform; only the NaN bits differ between them, which [`canonical`] erases.

use crate::Value;
use crate::types::IntTy;
use crate::value::IntValue;

/// Why a numeric library call has no value: a hard runtime error of the program (BLSR004), or arguments its typing
/// rules out (an evaluator bug).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NumError {
    Arithmetic(String),
    Type(String),
}

/// The one NaN: the quiet NaN with no payload and a clear sign.
pub const NAN: f64 = f64::from_bits(0x7ff8_0000_0000_0000);

/// `x`, canonical: `-0.0` is `0.0`, and every NaN is [`NAN`].
pub fn canonical(x: f64) -> f64 {
    if x.is_nan() {
        NAN
    } else if x == 0.0 {
        0.0
    } else {
        x
    }
}

/// The nearest double to an integer (`n as f64`).
pub fn from_int(n: IntValue) -> f64 {
    match n {
        IntValue::U128(u) => u as f64,
        other => match other.to_i128() {
            Some(i) => i as f64,
            // Only a `u128` above `i128::MAX` has no `i128`, and it is the arm above.
            None => NAN,
        },
    }
}

/// `x as T` for an integer type `T`: `x` truncated toward zero, unless it is NaN, infinite or out of `T`'s range.
pub fn to_int(x: f64, ty: IntTy) -> Option<IntValue> {
    if !x.is_finite() {
        return None;
    }
    let t = x.trunc();
    // 2^127 and 2^128 are exact doubles; a truncated double inside these bounds converts exactly.
    let two_127 = 170_141_183_460_469_231_731_687_303_715_884_105_728.0_f64;
    if (-two_127..two_127).contains(&t) {
        IntValue::from_i128(ty, t as i128)
    } else if ty == IntTy::U128 && t >= 0.0 && t < 2.0 * two_127 {
        Some(IntValue::U128(t as u128))
    } else {
        None
    }
}

/// `x.to_string()`: the shortest decimal that reads back as `x`, without an exponent (`1`, `0.1`, `-2.5`, `NaN`,
/// `inf`, `-inf`).
pub fn to_string(x: f64) -> String {
    canonical(x).to_string()
}

/// A float in `[0, 1)` from 64 random bits: the top 53, as a multiple of 2^-53 (every such double equally likely).
pub fn unit_from_bits(bits: u64) -> f64 {
    (bits >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
}

/// `abs(x)` on an integer (an error when `-x` does not fit its type) or an `f64`.
pub fn abs(x: &Value) -> Result<Value, NumError> {
    match x {
        Value::F64(f) => Ok(Value::F64(canonical(f.abs()))),
        Value::Int(IntValue::U128(_) | IntValue::U64(_) | IntValue::U32(_) | IntValue::U16(_) | IntValue::U8(_)) => {
            Ok(x.clone())
        }
        Value::Int(i) => i
            .to_i128()
            .and_then(i128::checked_abs)
            .and_then(|a| IntValue::from_i128(i.ty(), a))
            .map(Value::Int)
            .ok_or_else(|| NumError::Arithmetic(format!("abs({i}) overflows {}", i.ty().name()))),
        other => Err(NumError::Type(format!("abs of {other:?}"))),
    }
}

/// Whether `a` and `b` are numbers of one type.
fn same_number(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::F64(_), Value::F64(_)) => true,
        (Value::Int(x), Value::Int(y)) => x.ty() == y.ty(),
        _ => false,
    }
}

/// `min(a, b)` (`max` when `most`): in the values' order (for floats, IEEE `totalOrder` on canonical values).
pub fn min_max(a: &Value, b: &Value, most: bool) -> Result<Value, NumError> {
    if !same_number(a, b) {
        return Err(NumError::Type(format!("min/max of {a:?} and {b:?}")));
    }
    let a_first = if most { a >= b } else { a <= b };
    Ok(if a_first { a.clone() } else { b.clone() })
}

/// `clamp(x, lo, hi)`: `max(lo, min(x, hi))`, an error when `lo > hi`.
pub fn clamp(x: &Value, lo: &Value, hi: &Value) -> Result<Value, NumError> {
    if !same_number(x, lo) || !same_number(x, hi) {
        return Err(NumError::Type(format!("clamp of {x:?}, {lo:?}, {hi:?}")));
    }
    if lo > hi {
        return Err(NumError::Arithmetic(format!(
            "clamp between {lo:?} and {hi:?}: the low bound is above the high"
        )));
    }
    min_max(&min_max(x, hi, false)?, lo, true)
}

/// A method of an `f64` that maps it to an `f64`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Sqrt,
    Floor,
    Ceil,
    /// To the nearest integer, half away from zero.
    Round,
    Trunc,
}

/// `x.sqrt()`, `x.floor()`, … (correctly rounded, canonical).
pub fn method(m: Method, x: &Value) -> Result<Value, NumError> {
    let Value::F64(f) = x else {
        return Err(NumError::Type(format!("{m:?} of {x:?}")));
    };
    let r = match m {
        Method::Sqrt => f.sqrt(),
        Method::Floor => f.floor(),
        Method::Ceil => f.ceil(),
        Method::Round => f.round(),
        Method::Trunc => f.trunc(),
    };
    Ok(Value::F64(canonical(r)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_canonical() {
        assert_eq!(canonical(-0.0).to_bits(), 0.0_f64.to_bits());
        assert_eq!(
            canonical(f64::from_bits(0xfff8_0000_dead_beef)).to_bits(),
            NAN.to_bits()
        );
        assert_eq!(canonical(-2.5), -2.5);
        let (zero, minus_one) = (0.0_f64, -1.0_f64);
        assert_eq!(canonical(zero * minus_one).to_bits(), 0.0_f64.to_bits());
    }

    #[test]
    fn casts_truncate_and_refuse_what_does_not_fit() {
        assert_eq!(to_int(2.9, IntTy::I64), Some(IntValue::I64(2)));
        assert_eq!(to_int(-2.9, IntTy::I64), Some(IntValue::I64(-2)));
        assert_eq!(to_int(-0.5, IntTy::U8), Some(IntValue::U8(0)));
        assert_eq!(to_int(-1.0, IntTy::U8), None);
        assert_eq!(to_int(256.0, IntTy::U8), None);
        assert_eq!(to_int(255.9, IntTy::U8), Some(IntValue::U8(255)));
        assert_eq!(to_int(f64::NAN, IntTy::I64), None);
        assert_eq!(to_int(f64::INFINITY, IntTy::U128), None);
        assert_eq!(to_int(9.223_372_036_854_776e18, IntTy::I64), None);
        assert_eq!(
            to_int(-9.223_372_036_854_776e18, IntTy::I64),
            Some(IntValue::I64(i64::MIN))
        );
        assert_eq!(
            to_int(3.0e38, IntTy::U128),
            Some(IntValue::U128(300_000_000_000_000_012_135_895_401_846_682_943_488))
        );
        assert_eq!(to_int(3.0e38, IntTy::I128), None);
        assert_eq!(from_int(IntValue::U128(u128::MAX)), 3.402_823_669_209_385e38);
        assert_eq!(from_int(IntValue::I64(-3)), -3.0);
        assert_eq!(from_int(IntValue::U64(9_007_199_254_740_993)), 9_007_199_254_740_992.0);
    }

    #[test]
    fn the_numeric_library() {
        let f = Value::F64;
        let i = |n: i64| Value::Int(IntValue::I64(n));
        assert_eq!(abs(&i(-3)), Ok(i(3)));
        assert!(matches!(abs(&i(i64::MIN)), Err(NumError::Arithmetic(_))));
        assert_eq!(abs(&Value::Int(IntValue::U8(200))), Ok(Value::Int(IntValue::U8(200))));
        assert_eq!(
            abs(&f(-0.0)).map(|v| matches!(v, Value::F64(z) if z.to_bits() == 0)),
            Ok(true)
        );
        assert_eq!(min_max(&i(2), &i(-5), false), Ok(i(-5)));
        assert_eq!(min_max(&f(2.0), &f(-5.5), true), Ok(f(2.0)));
        assert!(matches!(min_max(&i(2), &f(1.0), true), Err(NumError::Type(_))));
        assert_eq!(clamp(&i(15), &i(0), &i(10)), Ok(i(10)));
        assert_eq!(clamp(&f(-1.0), &f(0.0), &f(1.0)), Ok(f(0.0)));
        assert!(matches!(clamp(&i(1), &i(5), &i(0)), Err(NumError::Arithmetic(_))));
        assert_eq!(method(Method::Floor, &f(-1.5)), Ok(f(-2.0)));
        assert_eq!(method(Method::Sqrt, &f(9.0)), Ok(f(3.0)));
        assert_eq!(
            method(Method::Sqrt, &f(-1.0)).map(|v| matches!(v, Value::F64(n) if n.to_bits() == NAN.to_bits())),
            Ok(true)
        );
        assert_eq!(
            method(Method::Ceil, &f(-0.5)).map(|v| matches!(v, Value::F64(z) if z.to_bits() == 0)),
            Ok(true)
        );
    }

    #[test]
    fn strings_rounding_and_random_floats() {
        assert_eq!(to_string(1.0), "1");
        assert_eq!(to_string(0.1), "0.1");
        assert_eq!(to_string(-2.5), "-2.5");
        assert_eq!(to_string(-0.0), "0");
        assert_eq!(to_string(f64::NAN), "NaN");
        assert_eq!(to_string(f64::NEG_INFINITY), "-inf");
        assert_eq!(method(Method::Round, &Value::F64(2.5)), Ok(Value::F64(3.0)));
        assert_eq!(method(Method::Round, &Value::F64(-2.5)), Ok(Value::F64(-3.0)));
        assert!(matches!(method(Method::Round, &Value::F64(-0.4)), Ok(Value::F64(z)) if z.to_bits() == 0));
        assert_eq!(unit_from_bits(0), 0.0);
        assert!(unit_from_bits(u64::MAX) < 1.0);
        assert_eq!(unit_from_bits(1 << 63), 0.5);
    }
}
