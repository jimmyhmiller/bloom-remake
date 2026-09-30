//! Shared plumbing of the standard host functions: registration under the catalog's signature, and argument
//! decoding.

use blossom_value::error::ValueError;
use blossom_value::externs::{ExternError, ExternFn};
use blossom_value::value::IntValue;
use blossom_value::{ExternRegistry, Value};

/// Registers `f` at `path` with the signature `blossom_value::STD_EXTERNS` gives it. A path the catalog lacks is
/// refused, so the host library and the compiler's catalog cannot drift apart.
pub(crate) fn register_std(
    reg: &mut ExternRegistry,
    path: &'static str,
    f: impl ExternFn + 'static,
) -> Result<(), ValueError> {
    let Some(entry) = blossom_value::std_extern(path) else {
        return Err(ValueError::ExternSignature {
            path: path.into(),
            reason: "not in the standard catalog (blossom_value::STD_EXTERNS)".into(),
        });
    };
    reg.register_typed_fn(path, entry.params.to_vec(), entry.ret, f)
}

pub(crate) fn bytes_arg(v: &Value) -> Result<&[u8], ExternError> {
    match v {
        Value::Bytes(b) => Ok(b),
        other => Err(ExternError::InvalidArguments(
            format!("expected Bytes, got {other:?}").into(),
        )),
    }
}

pub(crate) fn u64_arg(v: &Value) -> Result<u64, ExternError> {
    match v {
        Value::Int(IntValue::U64(n)) => Ok(*n),
        other => Err(ExternError::InvalidArguments(
            format!("expected a u64, got {other:?}").into(),
        )),
    }
}

pub(crate) fn u8_arg(v: &Value) -> Result<u8, ExternError> {
    match v {
        Value::Int(IntValue::U8(n)) => Ok(*n),
        other => Err(ExternError::InvalidArguments(
            format!("expected a u8, got {other:?}").into(),
        )),
    }
}
