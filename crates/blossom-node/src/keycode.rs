//! Order-preserving key encoding of values (docs/design/DATABASE.md §3): what a node's database orders its rows by.
//!
//! Each value is a tag byte, then its body. The encoding is injective and self-delimiting (so a row's leading columns
//! are a byte prefix of its key, and a value is never a prefix of another), and, within a column's type, the bytes
//! order as the values do for the types a program compares: `bool`, the integers (big-endian, the sign bit flipped for
//! signed ones), `f64` (IEEE 754 totalOrder), strings and bytes (escaped: `00` is written `00 FF`, and `00 01` ends
//! them), `Duration` and `Instant`, nodes (a deployment's by name, which is their order; a client member by its
//! server's name and serial), and tuples, structs, enums, `Vec` and `Option` of those. A value holding anything else
//! (sets, maps, lattice and group values, blobs, extern values, …) is encoded whole in the caller's canonical codec,
//! escaped: equal values give equal bytes, distinct ones distinct bytes, in no meaningful order.

use std::sync::Arc;

use blossom_value::Value;
use blossom_value::time::NodeId;
use blossom_value::value::IntValue;

const UNIT: u8 = 0x01;
const BOOL: u8 = 0x02;
const INT: u8 = 0x03;
const F64: u8 = 0x04;
const STR: u8 = 0x05;
const BYTES: u8 = 0x06;
const PRINCIPAL: u8 = 0x07;
const DURATION: u8 = 0x08;
const INSTANT: u8 = 0x09;
const NODE: u8 = 0x0a;
const TUPLE: u8 = 0x0b;
const STRUCT: u8 = 0x0c;
const ENUM: u8 = 0x0d;
const VEC: u8 = 0x0e;
const NONE: u8 = 0x0f;
const SOME: u8 = 0x10;
const OTHER: u8 = 0x7f;

/// Bytes escaped so they order as themselves and end where they end: `00` → `00 FF`, then `00 01`.
pub fn put_escaped(out: &mut Vec<u8>, bytes: &[u8]) {
    for &b in bytes {
        out.push(b);
        if b == 0 {
            out.push(0xff);
        }
    }
    out.extend_from_slice(&[0x00, 0x01]);
}

/// Whether the value is ordered by this encoding (not encoded whole by the fallback).
pub fn ordered(v: &Value) -> bool {
    match v {
        Value::Unit
        | Value::Bool(_)
        | Value::Int(_)
        | Value::F64(_)
        | Value::Str(_)
        | Value::Bytes(_)
        | Value::Principal(_)
        | Value::Duration(_)
        | Value::Instant(_)
        | Value::Node(_) => true,
        Value::Tuple(xs) | Value::Struct(xs) | Value::Vec(xs) => xs.iter().all(ordered),
        Value::Enum { fields, .. } => fields.iter().all(ordered),
        Value::Option(o) => o.as_deref().is_none_or(ordered),
        _ => false,
    }
}

fn int(out: &mut Vec<u8>, i: &IntValue) {
    // The integer type first (a column holds one), then its bits ordered as its values.
    match *i {
        IntValue::U8(x) => {
            out.push(0);
            out.extend_from_slice(&x.to_be_bytes());
        }
        IntValue::U16(x) => {
            out.push(1);
            out.extend_from_slice(&x.to_be_bytes());
        }
        IntValue::U32(x) => {
            out.push(2);
            out.extend_from_slice(&x.to_be_bytes());
        }
        IntValue::U64(x) => {
            out.push(3);
            out.extend_from_slice(&x.to_be_bytes());
        }
        IntValue::U128(x) => {
            out.push(4);
            out.extend_from_slice(&x.to_be_bytes());
        }
        IntValue::I8(x) => {
            out.push(5);
            out.extend_from_slice(&((x as u8) ^ 0x80).to_be_bytes());
        }
        IntValue::I16(x) => {
            out.push(6);
            out.extend_from_slice(&((x as u16) ^ 0x8000).to_be_bytes());
        }
        IntValue::I32(x) => {
            out.push(7);
            out.extend_from_slice(&((x as u32) ^ 0x8000_0000).to_be_bytes());
        }
        IntValue::I64(x) => {
            out.push(8);
            out.extend_from_slice(&((x as u64) ^ (1 << 63)).to_be_bytes());
        }
        IntValue::I128(x) => {
            out.push(9);
            out.extend_from_slice(&((x as u128) ^ (1 << 127)).to_be_bytes());
        }
    }
}

fn signed(out: &mut Vec<u8>, x: i64) {
    out.extend_from_slice(&((x as u64) ^ (1 << 63)).to_be_bytes());
}

/// Appends `v`'s key encoding: ordered where [`ordered`] says so, else `fallback(v)` (its canonical codec bytes)
/// escaped. `names` names a deployment's nodes (`NodeId(i)` is `names[i]`).
pub fn encode<E>(
    v: &Value,
    names: &[Arc<str>],
    fallback: &dyn Fn(&Value) -> Result<Vec<u8>, E>,
    out: &mut Vec<u8>,
) -> Result<(), E>
where
    E: From<String>,
{
    if !ordered(v) {
        out.push(OTHER);
        put_escaped(out, &fallback(v)?);
        return Ok(());
    }
    match v {
        Value::Unit => out.push(UNIT),
        Value::Bool(b) => {
            out.push(BOOL);
            out.push(u8::from(*b));
        }
        Value::Int(i) => {
            out.push(INT);
            int(out, i);
        }
        Value::F64(f) => {
            out.push(F64);
            let bits = f.to_bits();
            let ordered = if bits >> 63 == 1 { !bits } else { bits ^ (1 << 63) };
            out.extend_from_slice(&ordered.to_be_bytes());
        }
        Value::Str(s) => {
            out.push(STR);
            put_escaped(out, s.as_bytes());
        }
        Value::Bytes(b) => {
            out.push(BYTES);
            put_escaped(out, b);
        }
        Value::Principal(p) => {
            out.push(PRINCIPAL);
            put_escaped(out, p.as_bytes());
        }
        Value::Duration(d) => {
            out.push(DURATION);
            signed(out, d.0);
        }
        Value::Instant(t) => {
            out.push(INSTANT);
            signed(out, t.0);
        }
        Value::Node(n) => {
            out.push(NODE);
            let name = |id: NodeId| {
                names
                    .get(id.0 as usize)
                    .ok_or_else(|| E::from(format!("node {} is not in the deployment", id.0)))
            };
            match n.client_parts() {
                None => {
                    out.push(0);
                    put_escaped(out, name(*n)?.as_bytes());
                }
                Some((server, serial)) => {
                    out.push(1);
                    put_escaped(out, name(server)?.as_bytes());
                    out.extend_from_slice(&serial.to_be_bytes());
                }
            }
        }
        Value::Tuple(xs) | Value::Struct(xs) => {
            out.push(if matches!(v, Value::Tuple(_)) { TUPLE } else { STRUCT });
            for x in xs.iter() {
                encode(x, names, fallback, out)?;
            }
        }
        Value::Enum { variant, fields } => {
            out.push(ENUM);
            out.extend_from_slice(&variant.to_be_bytes());
            for x in fields.iter() {
                encode(x, names, fallback, out)?;
            }
        }
        Value::Vec(xs) => {
            out.push(VEC);
            for x in xs.iter() {
                out.push(1);
                encode(x, names, fallback, out)?;
            }
            out.push(0);
        }
        Value::Option(None) => out.push(NONE),
        Value::Option(Some(x)) => {
            out.push(SOME);
            encode(x, names, fallback, out)?;
        }
        other => return Err(E::from(format!("{other:?} is not ordered, yet was not encoded whole"))),
    }
    Ok(())
}

/// The smallest byte string greater than every one starting with `prefix` (`None`: there is none, all `FF`).
pub fn successor(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut out = prefix.to_vec();
    while let Some(last) = out.pop() {
        if last < 0xff {
            out.push(last + 1);
            return Some(out);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use blossom_value::time::{Duration, Instant};

    fn enc(v: &Value) -> Vec<u8> {
        let names: Vec<Arc<str>> = vec![Arc::from("a"), Arc::from("b"), Arc::from("c")];
        let fallback = |v: &Value| -> Result<Vec<u8>, String> { Ok(format!("{v:?}").into_bytes()) };
        let mut out = Vec::new();
        encode(v, &names, &fallback, &mut out).unwrap();
        out
    }

    /// Values of one type, in their order.
    fn families() -> Vec<Vec<Value>> {
        let ints = |xs: &[i64]| xs.iter().map(|x| Value::Int(IntValue::I64(*x))).collect::<Vec<_>>();
        vec![
            vec![Value::Bool(false), Value::Bool(true)],
            ints(&[i64::MIN, -300, -1, 0, 1, 2, 255, 256, i64::MAX]),
            [0u64, 1, 255, 256, 65_536, u64::MAX]
                .iter()
                .map(|x| Value::Int(IntValue::U64(*x)))
                .collect(),
            [-1.0e300, -2.5, -0.0, 0.0, 1.0e-300, 3.0, f64::INFINITY]
                .iter()
                .map(|x| Value::F64(*x))
                .collect(),
            ["", "\0", "\0\0", "a", "a\0", "a\0b", "ab", "b", "\u{e9}"]
                .iter()
                .map(|s| Value::str(s))
                .collect(),
            [-5i64, 0, 7].iter().map(|x| Value::Instant(Instant(*x))).collect(),
            [-5i64, 0, 7].iter().map(|x| Value::Duration(Duration(*x))).collect(),
            vec![Value::Node(NodeId(0)), Value::Node(NodeId(1)), Value::Node(NodeId(2))],
            vec![
                Value::Option(None),
                Value::Option(Some(Arc::new(Value::str("")))),
                Value::Option(Some(Arc::new(Value::str("z")))),
            ],
            vec![
                Value::Tuple(Arc::from(vec![Value::str("a"), Value::Int(IntValue::I64(9))])),
                Value::Tuple(Arc::from(vec![Value::str("a\0"), Value::Int(IntValue::I64(1))])),
                Value::Tuple(Arc::from(vec![Value::str("b"), Value::Int(IntValue::I64(0))])),
            ],
            vec![
                Value::Vec(Arc::from(Vec::<Value>::new())),
                Value::Vec(Arc::from(vec![Value::str("a")])),
                Value::Vec(Arc::from(vec![Value::str("a"), Value::str("a")])),
                Value::Vec(Arc::from(vec![Value::str("b")])),
            ],
        ]
    }

    #[test]
    fn bytes_order_as_values_do_and_no_encoding_is_a_prefix_of_another() {
        for family in families() {
            for a in &family {
                for b in &family {
                    let (ea, eb) = (enc(a), enc(b));
                    assert_eq!(ea.cmp(&eb), a.cmp(b), "{a:?} vs {b:?}");
                    if a != b {
                        assert!(!eb.starts_with(&ea), "{a:?} is a prefix of {b:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn values_outside_the_ordered_types_are_encoded_whole_and_injectively() {
        let set = |xs: &[i64]| Value::Set(Arc::new(xs.iter().map(|x| Value::Int(IntValue::I64(*x))).collect()));
        let (a, b) = (set(&[1, 2]), set(&[1, 3]));
        assert!(!ordered(&a));
        assert_ne!(enc(&a), enc(&b));
        assert_eq!(enc(&a), enc(&set(&[2, 1])));
        // A client member orders after the deployment's nodes, by its server and serial.
        let c = |s: u32| Value::Node(NodeId::client(NodeId(1), s).unwrap());
        assert!(enc(&c(1)) < enc(&c(2)));
        assert!(enc(&Value::Node(NodeId(2))) < enc(&c(0)));
    }

    #[test]
    fn successor_is_past_every_extension() {
        assert_eq!(successor(b"ab"), Some(b"ac".to_vec()));
        assert_eq!(successor(b"a\xff"), Some(b"b".to_vec()));
        assert_eq!(successor(b"\xff\xff"), None);
        let s = successor(b"key").unwrap();
        for ext in [&b""[..], b"\x00", b"\xff\xff\xff"] {
            let mut k = b"key".to_vec();
            k.extend_from_slice(ext);
            assert!(k < s);
        }
    }
}
