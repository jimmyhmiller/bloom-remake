//! Host functions of `blossom_std::checksum` (FOREIGN-PROTOCOLS §4): CRC-32C (Castagnoli) and CRC-32 (IEEE).

use blossom_value::Value;
use blossom_value::externs::ExternError;
use blossom_value::value::IntValue;

use crate::host::{bytes_arg, register_std};

pub fn register(reg: &mut blossom_value::ExternRegistry) -> Result<(), blossom_value::error::ValueError> {
    register_std(reg, "blossom_std::checksum::crc32c", |args: &[Value]| {
        let [b] = args else {
            return Err(ExternError::InvalidArguments("crc32c takes one Bytes".into()));
        };
        Ok(Value::Int(IntValue::U32(crc32c::crc32c(bytes_arg(b)?))))
    })?;
    register_std(reg, "blossom_std::checksum::crc32", |args: &[Value]| {
        let [b] = args else {
            return Err(ExternError::InvalidArguments("crc32 takes one Bytes".into()));
        };
        Ok(Value::Int(IntValue::U32(crc32fast::hash(bytes_arg(b)?))))
    })?;
    Ok(())
}
