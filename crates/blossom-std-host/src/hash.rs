//! Host functions of `blossom_std::hash` (FOREIGN-PROTOCOLS §4, LANGUAGE §16.2): SHA-256 and BLAKE3 digests.

use std::sync::Arc;

use blossom_value::Value;
use blossom_value::externs::ExternError;
use sha2::Digest;

use crate::host::{bytes_arg, register_std};

pub fn register(reg: &mut blossom_value::ExternRegistry) -> Result<(), blossom_value::error::ValueError> {
    register_std(reg, "blossom_std::hash::sha256", |args: &[Value]| {
        let [b] = args else {
            return Err(ExternError::InvalidArguments("sha256 takes one Bytes".into()));
        };
        Ok(Value::Bytes(Arc::from(sha2::Sha256::digest(bytes_arg(b)?).as_slice())))
    })?;
    register_std(reg, "blossom_std::hash::blake3", |args: &[Value]| {
        let [b] = args else {
            return Err(ExternError::InvalidArguments("blake3 takes one Bytes".into()));
        };
        Ok(Value::Bytes(Arc::from(
            blake3::hash(bytes_arg(b)?).as_bytes().as_slice(),
        )))
    })?;
    Ok(())
}
