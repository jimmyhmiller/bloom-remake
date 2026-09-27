//! Stable xxh3 fingerprints over canonical, typed Merkle encodings.
use crate::bounded::DepthGuard;
use crate::error::ValueError;
use crate::value::{GroupValue, LatValue, Value};
use serde::{Deserialize, Serialize};

pub const ENCODING_VERSION: u16 = 1;
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Fingerprint(pub u64);
fn hash(tag: u8, bytes: &[u8]) -> Fingerprint {
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    h.update(&ENCODING_VERSION.to_le_bytes());
    h.update(&[tag]);
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
    Fingerprint(h.digest())
}
fn children(
    tag: u8,
    values: impl IntoIterator<Item = Result<Fingerprint, ValueError>>,
) -> Result<Fingerprint, ValueError> {
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    h.update(&ENCODING_VERSION.to_le_bytes());
    h.update(&[tag]);
    let mut n = 0u64;
    for fp in values {
        h.update(&fp?.0.to_le_bytes());
        n += 1;
    }
    h.update(&n.to_le_bytes());
    Ok(Fingerprint(h.digest()))
}
fn primitive<T: serde::Serialize>(tag: u8, v: &T) -> Result<Fingerprint, ValueError> {
    let bytes = postcard::to_allocvec(v).map_err(|e| ValueError::InvalidValue(format!("canonical encoding: {e}")))?;
    Ok(hash(tag, &bytes))
}
/// Fingerprint from the value's canonical typed shape, never from an intern id.
pub fn fingerprint(value: &Value) -> Result<Fingerprint, ValueError> {
    let _depth = DepthGuard::enter(|| ValueError::InvalidValue("value fingerprint nesting exceeds 128".into()))?;
    match value {
        Value::Unit => Ok(hash(0, &[])),
        Value::Bool(v) => primitive(1, v),
        Value::Int(v) => primitive(2, v),
        Value::F64(v) => Ok(hash(3, &v.to_bits().to_le_bytes())),
        Value::Str(v) => Ok(hash(4, v.as_bytes())),
        Value::Bytes(v) => Ok(hash(5, v)),
        Value::Duration(v) => primitive(6, v),
        Value::Instant(v) => primitive(7, v),
        Value::Mod(v) => primitive(8, v),
        Value::Blob(v) => primitive(9, v),
        Value::Session(v) => primitive(10, v),
        Value::Principal(v) => Ok(hash(11, v.as_bytes())),
        Value::Node(v) => primitive(12, v),
        Value::Tuple(v) => children(13, v.iter().map(fingerprint)),
        Value::Struct(v) => children(14, v.iter().map(fingerprint)),
        Value::Enum { variant, fields } => {
            let mut f = vec![Ok(hash(15, &variant.to_le_bytes()))];
            f.extend(fields.iter().map(fingerprint));
            children(16, f)
        }
        Value::UnknownVariant {
            variant,
            wire_number,
            payload,
        } => {
            let mut b = Vec::new();
            b.extend_from_slice(&variant.to_le_bytes());
            b.extend_from_slice(&wire_number.to_le_bytes());
            b.extend_from_slice(payload);
            Ok(hash(17, &b))
        }
        Value::Vec(v) => children(18, v.iter().map(fingerprint)),
        Value::Set(v) => children(19, v.iter().map(fingerprint)),
        Value::Map(v) => children(
            20,
            v.iter()
                .map(|(k, val)| children(21, [fingerprint(k), fingerprint(val)])),
        ),
        Value::Option(None) => Ok(hash(22, &[])),
        Value::Option(Some(v)) => children(23, [fingerprint(v)]),
        Value::Lattice(v) => lattice_fp(v),
        Value::Group(v) => group_fp(v),
        Value::Extern { codec, bytes } => children(26, [primitive(27, codec), Ok(hash(28, bytes))]),
    }
}
fn lattice_fp(value: &LatValue) -> Result<Fingerprint, ValueError> {
    let _depth = DepthGuard::enter(|| ValueError::InvalidValue("lattice fingerprint nesting exceeds 128".into()))?;
    match value {
        LatValue::Bottom => Ok(hash(30, &[])),
        LatValue::Top => Ok(hash(31, &[])),
        LatValue::Bool(v) => primitive(32, v),
        LatValue::Elem(v) => children(33, [fingerprint(v)]),
        LatValue::Set(v) => children(34, v.iter().map(fingerprint)),
        LatValue::Map(v) => children(
            35,
            v.iter().map(|(k, lat)| children(36, [fingerprint(k), lattice_fp(lat)])),
        ),
        LatValue::Bag(v) => children(
            37,
            v.iter().map(|(k, n)| children(38, [fingerprint(k), primitive(39, n)])),
        ),
        LatValue::Seq(v) => children(40, v.iter().map(lattice_fp)),
        LatValue::Extern { codec, bytes } => children(41, [primitive(42, codec), Ok(hash(43, bytes))]),
    }
}
fn group_fp(value: &GroupValue) -> Result<Fingerprint, ValueError> {
    let _depth = DepthGuard::enter(|| ValueError::InvalidValue("group fingerprint nesting exceeds 128".into()))?;
    match value {
        GroupValue::Z(n) => primitive(44, n),
        GroupValue::Zn(n) => primitive(45, n),
        GroupValue::ZSet(v) => children(
            46,
            v.iter().map(|(k, n)| children(47, [fingerprint(k), primitive(48, n)])),
        ),
        GroupValue::Tuple(v) => children(49, v.iter().map(group_fp)),
        GroupValue::Map(v) => children(50, v.iter().map(|(k, g)| children(51, [fingerprint(k), group_fp(g)]))),
        GroupValue::User(v) => children(52, [fingerprint(v)]),
    }
}
/// Row fingerprint, with a distinct domain from tuple values.
pub fn fingerprint_row(values: &[Value]) -> Result<Fingerprint, ValueError> {
    children(29, values.iter().map(fingerprint))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprint_kat() {
        let cases = [
            (Value::Unit, 0x048f5eafa5b911cf),
            (Value::Bool(true), 0x0ddead44231e7d0e),
            (Value::i64(-1), 0xf044fef21605ed61),
            (Value::str("hé"), 0x79a8b2b8bdefa19d),
            (Value::tuple([Value::u64(7), Value::Bool(false)]), 0xf3435927f678e19c),
        ];
        for (value, expected) in cases {
            assert_eq!(fingerprint(&value).unwrap().0, expected);
        }
    }
    #[test]
    fn fingerprint_merkle_order() {
        let a = Value::tuple([Value::u64(1), Value::str("a")]);
        let b = Value::tuple([Value::str("a"), Value::u64(1)]);
        assert_ne!(fingerprint(&a).unwrap(), fingerprint(&b).unwrap());
        assert_eq!(
            fingerprint_row(&[Value::u64(1), Value::str("a")]).unwrap(),
            fingerprint_row(&[Value::u64(1), Value::str("a")]).unwrap()
        );
        assert_ne!(
            fingerprint_row(&[Value::u64(1)]).unwrap(),
            fingerprint(&Value::tuple([Value::u64(1)])).unwrap()
        );
    }
}
