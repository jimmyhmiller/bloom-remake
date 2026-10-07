//! The field-numbered tuple codec (ARCHITECTURE §5.4): values of the program's types, and rows of its relations, in
//! the byte form messages, WAL records and checkpoints share.
//!
//! ```text
//! tuple := nfields:varint field{nfields}                 fields in ascending field-number order
//! field := key:varint (= field_no << 3 | wt) value
//! wt    := 0 varint | 1 zz | 2 fixed64 | 3 bytes(len, data) | 4 nested(len, payload) | 5 lattice(len, kind:u8, payload)
//!        | 6 variant(number:varint, nested)
//! ```
//!
//! A row's field numbers are its columns' (`#n`), or the column's position plus one. A value inside a composite is
//! written as `wt value` without a key: a tuple or struct is a nested tuple (fields numbered from 1), a `Vec` or
//! `Set` a count and its elements, a `Map` a count and its pairs, an `Option` a count (0 or 1) and the element.
//! Lattice payloads are the lattice's own shape: an element, a sorted element list, or sorted (key, lattice) pairs.
//!
//! `Node` values are dense node ids on the wire and stable node names on disk ([`NodeEncoding`]), so durable state
//! survives renumbering the deployment.
//!
//! Decoding checks everything against the type: integer ranges, UTF-8, counts against the remaining bytes (before
//! allocating), nesting depth. Unknown fields of a tuple are skipped, so a newer sender's extra fields are ignored.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_base::{LatticeTypeId, TypeId};
use blossom_ir::core::{Column, LatticeCtor, Program};
use blossom_value::time::{Duration, Instant, NodeId};
use blossom_value::types::IntTy;
use blossom_value::value::{ConnId, IntValue, LatValue, SessionId};
use blossom_value::{TypeDef, Value};

/// Why bytes did not encode or decode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// The input ended inside a value.
    #[error("truncated input: {0}")]
    Truncated(&'static str),
    /// The input is not a value of the expected type.
    #[error("malformed input: {0}")]
    Malformed(String),
    /// A limit of [`WireLimits`] was exceeded.
    #[error("limit exceeded: {0}")]
    Limit(&'static str),
    /// A value the codec does not carry yet.
    #[error("unsupported on the wire: {0}")]
    Unsupported(String),
}

/// Bounds applied while decoding (ARCHITECTURE §5.4).
#[derive(Clone, Copy, Debug)]
pub struct WireLimits {
    /// The largest frame, in bytes.
    pub max_frame: usize,
    /// The most tuples one batch may carry.
    pub max_tuples_per_batch: usize,
    /// The deepest nesting of composite values.
    pub max_nesting: u32,
}

impl Default for WireLimits {
    fn default() -> WireLimits {
        WireLimits {
            max_frame: 16 << 20,
            max_tuples_per_batch: 1 << 20,
            max_nesting: 64,
        }
    }
}

/// How `Node` values are written.
#[derive(Clone, Debug)]
pub enum NodeEncoding {
    /// The dense node id (messages: both ends checked the same directory).
    Dense,
    /// The node's stable name, `names[id]` (durable state).
    ByName(Arc<[Arc<str>]>),
}

const WT_VARINT: u8 = 0;
const WT_ZZ: u8 = 1;
const WT_FIXED64: u8 = 2;
const WT_BYTES: u8 = 3;
const WT_NESTED: u8 = 4;
const WT_LATTICE: u8 = 5;
const WT_VARIANT: u8 = 6;

const LAT_BOTTOM: u8 = 0;
const LAT_TOP: u8 = 1;
const LAT_BOOL: u8 = 2;
const LAT_ELEM: u8 = 3;
const LAT_SET: u8 = 4;
const LAT_MAP: u8 = 5;
/// A product's fields, each length-prefixed (LANGUAGE §11.8).
const LAT_SEQ: u8 = 6;

/// Appends `v` as LEB128.
pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Reads a LEB128 value.
pub fn get_varint(input: &mut &[u8]) -> Result<u64, WireError> {
    let mut v: u64 = 0;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = input.split_first().ok_or(WireError::Truncated("varint"))?;
        *input = rest;
        let bits = u64::from(byte & 0x7f);
        if shift == 63 && bits > 1 {
            return Err(WireError::Malformed("varint overflows u64".into()));
        }
        v |= bits << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(WireError::Malformed("varint longer than ten bytes".into()))
}

fn put_zz(out: &mut Vec<u8>, v: i64) {
    put_varint(out, ((v << 1) ^ (v >> 63)) as u64);
}

fn get_zz(input: &mut &[u8]) -> Result<i64, WireError> {
    let u = get_varint(input)?;
    Ok(((u >> 1) as i64) ^ -((u & 1) as i64))
}

fn put_bytes(out: &mut Vec<u8>, data: &[u8]) {
    put_varint(out, data.len() as u64);
    out.extend_from_slice(data);
}

/// Takes `n` bytes.
pub fn take<'a>(input: &mut &'a [u8], n: usize, what: &'static str) -> Result<&'a [u8], WireError> {
    if input.len() < n {
        return Err(WireError::Truncated(what));
    }
    let (head, rest) = input.split_at(n);
    *input = rest;
    Ok(head)
}

fn get_bytes<'a>(input: &mut &'a [u8], what: &'static str) -> Result<&'a [u8], WireError> {
    let n = usize::try_from(get_varint(input)?).map_err(|_| WireError::Limit("length"))?;
    take(input, n, what)
}

/// A count, checked against the bytes that remain (every element takes at least one byte).
fn get_count(input: &mut &[u8]) -> Result<usize, WireError> {
    let n = usize::try_from(get_varint(input)?).map_err(|_| WireError::Limit("count"))?;
    if n > input.len() {
        return Err(WireError::Limit("count larger than the remaining input"));
    }
    Ok(n)
}

/// Encodes and decodes the values of one program's types.
#[derive(Clone)]
pub struct Codec<'p> {
    program: &'p Program,
    nodes: NodeEncoding,
    limits: WireLimits,
}

impl<'p> Codec<'p> {
    pub fn new(program: &'p Program, nodes: NodeEncoding, limits: WireLimits) -> Codec<'p> {
        Codec { program, nodes, limits }
    }

    fn def(&self, ty: TypeId) -> Result<&'p TypeDef, WireError> {
        self.program
            .types
            .get(ty)
            .ok_or_else(|| WireError::Malformed(format!("unknown type {ty:?}")))
    }

    fn lattice(&self, id: LatticeTypeId) -> Result<&'p LatticeCtor, WireError> {
        self.program
            .lattices
            .get(id)
            .map(|l| &l.ctor)
            .ok_or_else(|| WireError::Malformed(format!("unknown lattice {id:?}")))
    }

    /// The wire type of values of `ty`.
    fn wire_type(&self, ty: TypeId) -> Result<u8, WireError> {
        Ok(match self.def(ty)? {
            TypeDef::Bool | TypeDef::Session | TypeDef::Conn | TypeDef::Unit => WT_VARINT,
            TypeDef::Int(t) if !signed(*t) && !matches!(t, IntTy::U128) => WT_VARINT,
            TypeDef::Int(t) if signed(*t) && !matches!(t, IntTy::I128) => WT_ZZ,
            TypeDef::Int(_) => WT_BYTES,
            TypeDef::Duration | TypeDef::Instant => WT_ZZ,
            TypeDef::F64 => WT_FIXED64,
            TypeDef::Str | TypeDef::Bytes | TypeDef::Principal | TypeDef::Blob => WT_BYTES,
            TypeDef::Node(_) => match self.nodes {
                NodeEncoding::Dense => WT_VARINT,
                NodeEncoding::ByName(_) => WT_BYTES,
            },
            TypeDef::Tuple(_)
            | TypeDef::Struct(_)
            | TypeDef::Vec(_)
            | TypeDef::Set(_)
            | TypeDef::Map(..)
            | TypeDef::Option(_) => WT_NESTED,
            TypeDef::Enum(_) => WT_VARIANT,
            TypeDef::Lattice(_) => WT_LATTICE,
            other => return Err(WireError::Unsupported(format!("values of type {other:?}"))),
        })
    }

    /// Appends `v`, a value of `ty`, without its wire type.
    pub fn encode_value(&self, ty: TypeId, v: &Value, out: &mut Vec<u8>) -> Result<(), WireError> {
        let mismatch = || WireError::Malformed(format!("{v:?} is not a value of type {ty:?}"));
        match (self.def(ty)?, v) {
            (TypeDef::Unit, Value::Unit) => put_varint(out, 0),
            (TypeDef::Bool, Value::Bool(b)) => put_varint(out, u64::from(*b)),
            (TypeDef::Int(t), Value::Int(i)) if i.ty() == *t => match *i {
                IntValue::U128(u) => put_bytes(out, &u.to_le_bytes()),
                IntValue::I128(s) => put_bytes(out, &s.to_le_bytes()),
                IntValue::U8(u) => put_varint(out, u64::from(u)),
                IntValue::U16(u) => put_varint(out, u64::from(u)),
                IntValue::U32(u) => put_varint(out, u64::from(u)),
                IntValue::U64(u) => put_varint(out, u),
                IntValue::I8(s) => put_zz(out, i64::from(s)),
                IntValue::I16(s) => put_zz(out, i64::from(s)),
                IntValue::I32(s) => put_zz(out, i64::from(s)),
                IntValue::I64(s) => put_zz(out, s),
            },
            (TypeDef::F64, Value::F64(f)) => out.extend_from_slice(&f.to_bits().to_le_bytes()),
            (TypeDef::Duration, Value::Duration(d)) => put_zz(out, d.as_nanos()),
            (TypeDef::Instant, Value::Instant(t)) => put_zz(out, t.0),
            (TypeDef::Str, Value::Str(s)) => put_bytes(out, s.as_bytes()),
            (TypeDef::Bytes, Value::Bytes(b)) => put_bytes(out, b),
            (TypeDef::Principal, Value::Principal(p)) => put_bytes(out, p.as_bytes()),
            (TypeDef::Session, Value::Session(s)) => put_varint(out, s.0),
            (TypeDef::Conn, Value::Conn(c)) => put_varint(out, c.0),
            // A blob handle, not its bytes: the hash, then the length (little-endian).
            (TypeDef::Blob, Value::Blob(b)) => {
                let mut h = b.hash.to_vec();
                h.extend_from_slice(&b.len.to_le_bytes());
                put_bytes(out, &h);
            }
            (TypeDef::Node(_), Value::Node(n)) => match &self.nodes {
                NodeEncoding::Dense => put_varint(out, u64::from(n.0)),
                // A client member (docs/design/CLIENTS.md §2) is written `#serial@server`, its admitting node by name.
                NodeEncoding::ByName(names) => match n.client_parts() {
                    Some((server, serial)) => {
                        let name = names
                            .get(server.0 as usize)
                            .ok_or_else(|| WireError::Malformed(format!("node {} has no name", server.0)))?;
                        put_bytes(out, format!("#{serial}@{name}").as_bytes());
                    }
                    None => {
                        let name = names
                            .get(n.0 as usize)
                            .ok_or_else(|| WireError::Malformed(format!("node {} has no name", n.0)))?;
                        put_bytes(out, name.as_bytes());
                    }
                },
            },
            (TypeDef::Tuple(ts), Value::Tuple(vs)) => {
                let mut inner = Vec::new();
                self.encode_fields(ts.iter().copied().zip(vs.iter()), vs.len(), ts.len(), &mut inner)?;
                put_bytes(out, &inner);
            }
            (TypeDef::Struct(s), Value::Struct(vs)) => {
                let mut inner = Vec::new();
                let tys: Vec<TypeId> = s.fields.iter().map(|f| f.ty).collect();
                self.encode_fields(tys.iter().copied().zip(vs.iter()), vs.len(), tys.len(), &mut inner)?;
                put_bytes(out, &inner);
            }
            (TypeDef::Vec(t), Value::Vec(vs)) => {
                let mut inner = Vec::new();
                put_varint(&mut inner, vs.len() as u64);
                for x in vs.iter() {
                    self.encode_value(*t, x, &mut inner)?;
                }
                put_bytes(out, &inner);
            }
            (TypeDef::Set(t), Value::Set(vs)) => {
                let mut inner = Vec::new();
                put_varint(&mut inner, vs.len() as u64);
                for x in vs.iter() {
                    self.encode_value(*t, x, &mut inner)?;
                }
                put_bytes(out, &inner);
            }
            (TypeDef::Map(k, t), Value::Map(m)) => {
                let mut inner = Vec::new();
                put_varint(&mut inner, m.len() as u64);
                for (a, b) in m.iter() {
                    self.encode_value(*k, a, &mut inner)?;
                    self.encode_value(*t, b, &mut inner)?;
                }
                put_bytes(out, &inner);
            }
            (TypeDef::Option(t), Value::Option(o)) => {
                let mut inner = Vec::new();
                match o {
                    None => put_varint(&mut inner, 0),
                    Some(x) => {
                        put_varint(&mut inner, 1);
                        self.encode_value(*t, x, &mut inner)?;
                    }
                }
                put_bytes(out, &inner);
            }
            (TypeDef::Enum(e), Value::Enum { variant, fields }) => {
                let var = e
                    .variants
                    .iter()
                    .find(|x| x.number == *variant)
                    .ok_or_else(|| WireError::Malformed(format!("enum variant #{variant} is not declared")))?;
                put_varint(out, u64::from(*variant));
                let tys: Vec<TypeId> = var.payload.iter().map(|f| f.ty).collect();
                let mut inner = Vec::new();
                self.encode_fields(
                    tys.iter().copied().zip(fields.iter()),
                    fields.len(),
                    tys.len(),
                    &mut inner,
                )?;
                put_bytes(out, &inner);
            }
            (
                TypeDef::Enum(_),
                Value::UnknownVariant {
                    wire_number, payload, ..
                },
            ) => {
                // A newer version's variant re-encodes exactly as it arrived (LANG-261).
                put_varint(out, u64::from(*wire_number));
                put_bytes(out, payload);
            }
            (TypeDef::Lattice(id), Value::Lattice(l)) => {
                let ctor = self.lattice(*id)?.clone();
                let mut inner = Vec::new();
                self.encode_lattice(&ctor, l, &mut inner)?;
                put_bytes(out, &inner);
            }
            _ => return Err(mismatch()),
        }
        Ok(())
    }

    fn encode_fields<'v>(
        &self,
        fields: impl Iterator<Item = (TypeId, &'v Value)>,
        nv: usize,
        nt: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), WireError> {
        if nv != nt {
            return Err(WireError::Malformed(format!("{nv} values for {nt} fields")));
        }
        put_varint(out, nv as u64);
        for (i, (t, v)) in fields.enumerate() {
            put_varint(out, ((i as u64 + 1) << 3) | u64::from(self.wire_type(t)?));
            self.encode_value(t, v, out)?;
        }
        Ok(())
    }

    fn encode_lattice(&self, ctor: &LatticeCtor, l: &LatValue, out: &mut Vec<u8>) -> Result<(), WireError> {
        match (ctor, l) {
            (_, LatValue::Bottom) => out.push(LAT_BOTTOM),
            (_, LatValue::Top) => out.push(LAT_TOP),
            (LatticeCtor::Bool, LatValue::Bool(b)) => {
                out.push(LAT_BOOL);
                put_varint(out, u64::from(*b));
            }
            (LatticeCtor::Max(t) | LatticeCtor::Min(t) | LatticeCtor::Point(t), LatValue::Elem(x)) => {
                out.push(LAT_ELEM);
                self.encode_value(*t, x, out)?;
            }
            (LatticeCtor::Set(t) | LatticeCtor::PSet(t), LatValue::Set(xs)) => {
                out.push(LAT_SET);
                put_varint(out, xs.len() as u64);
                for x in xs.iter() {
                    self.encode_value(*t, x, out)?;
                }
            }
            (LatticeCtor::Map(k, inner), LatValue::Map(m)) => {
                let inner = self.lattice(*inner)?.clone();
                out.push(LAT_MAP);
                put_varint(out, m.len() as u64);
                for (a, b) in m.iter() {
                    self.encode_value(*k, a, out)?;
                    let mut nested = Vec::new();
                    self.encode_lattice(&inner, b, &mut nested)?;
                    put_bytes(out, &nested);
                }
            }
            (LatticeCtor::Product { fields, .. }, LatValue::Seq(vs)) if fields.len() == vs.len() => {
                out.push(LAT_SEQ);
                put_varint(out, vs.len() as u64);
                for ((_, id), v) in fields.iter().zip(vs.iter()) {
                    let inner = self.lattice(*id)?.clone();
                    let mut nested = Vec::new();
                    self.encode_lattice(&inner, v, &mut nested)?;
                    put_bytes(out, &nested);
                }
            }
            (c, v) => return Err(WireError::Unsupported(format!("the lattice value {v:?} of {c:?}"))),
        }
        Ok(())
    }

    /// Reads a value of `ty` (without its wire type).
    pub fn decode_value(&self, ty: TypeId, input: &mut &[u8]) -> Result<Value, WireError> {
        self.decode_at(ty, input, 0)
    }

    fn decode_at(&self, ty: TypeId, input: &mut &[u8], depth: u32) -> Result<Value, WireError> {
        if depth > self.limits.max_nesting {
            return Err(WireError::Limit("nesting"));
        }
        let int = |t: blossom_value::types::IntTy, n: i128| {
            IntValue::from_i128(t, n)
                .map(Value::Int)
                .ok_or_else(|| WireError::Malformed(format!("{n} does not fit {}", t.name())))
        };
        Ok(match self.def(ty)? {
            TypeDef::Unit => match get_varint(input)? {
                0 => Value::Unit,
                n => return Err(WireError::Malformed(format!("unit encoded as {n}"))),
            },
            TypeDef::Bool => match get_varint(input)? {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                n => return Err(WireError::Malformed(format!("bool encoded as {n}"))),
            },
            TypeDef::Int(t) => match self.wire_type(ty)? {
                WT_VARINT => int(*t, i128::from(get_varint(input)?))?,
                WT_ZZ => int(*t, i128::from(get_zz(input)?))?,
                _ => {
                    let b = get_bytes(input, "a 128-bit integer")?;
                    let arr: [u8; 16] = b
                        .try_into()
                        .map_err(|_| WireError::Malformed("a 128-bit integer is not 16 bytes".into()))?;
                    match t {
                        IntTy::U128 => Value::Int(IntValue::U128(u128::from_le_bytes(arr))),
                        _ => Value::Int(IntValue::I128(i128::from_le_bytes(arr))),
                    }
                }
            },
            TypeDef::F64 => {
                let b = take(input, 8, "f64")?;
                let arr: [u8; 8] = b.try_into().map_err(|_| WireError::Truncated("f64"))?;
                Value::F64(f64::from_bits(u64::from_le_bytes(arr)))
            }
            TypeDef::Duration => Value::Duration(Duration::from_nanos(get_zz(input)?)),
            TypeDef::Instant => Value::Instant(Instant(get_zz(input)?)),
            TypeDef::Str => Value::Str(Arc::from(utf8(get_bytes(input, "a string")?)?)),
            TypeDef::Bytes => Value::Bytes(Arc::from(get_bytes(input, "bytes")?)),
            TypeDef::Principal => Value::Principal(Arc::from(utf8(get_bytes(input, "a principal")?)?)),
            TypeDef::Session => Value::Session(SessionId(get_varint(input)?)),
            TypeDef::Conn => Value::Conn(ConnId(get_varint(input)?)),
            TypeDef::Blob => {
                let b = get_bytes(input, "a blob handle")?;
                let (hash, len) = (b.get(..32), b.get(32..));
                match (hash, len) {
                    (Some(h), Some(l)) if l.len() == 8 => Value::Blob(blossom_value::value::BlobRef {
                        hash: h.try_into().map_err(|_| WireError::Malformed("a blob hash".into()))?,
                        len: u64::from_le_bytes(
                            l.try_into().map_err(|_| WireError::Malformed("a blob length".into()))?,
                        ),
                    }),
                    _ => return Err(WireError::Malformed("a blob handle is 40 bytes".into())),
                }
            }
            TypeDef::Node(_) => match &self.nodes {
                NodeEncoding::Dense => Value::Node(NodeId(
                    u32::try_from(get_varint(input)?).map_err(|_| WireError::Malformed("node id".into()))?,
                )),
                NodeEncoding::ByName(names) => {
                    let name = utf8(get_bytes(input, "a node name")?)?;
                    let index = |name: &str| -> Result<NodeId, WireError> {
                        let i = names
                            .iter()
                            .position(|n| &**n == name)
                            .ok_or_else(|| WireError::Malformed(format!("no node named `{name}` in the directory")))?;
                        Ok(NodeId(u32::try_from(i).map_err(|_| WireError::Limit("node id"))?))
                    };
                    match name.strip_prefix('#').and_then(|rest| rest.split_once('@')) {
                        Some((serial, server)) => {
                            let serial: u32 = serial
                                .parse()
                                .map_err(|_| WireError::Malformed(format!("`{name}` is not a client member")))?;
                            Value::Node(
                                NodeId::client(index(server)?, serial)
                                    .ok_or_else(|| WireError::Malformed(format!("`{name}` is out of range")))?,
                            )
                        }
                        None => Value::Node(index(name)?),
                    }
                }
            },
            TypeDef::Tuple(ts) => {
                let mut inner = get_bytes(input, "a tuple")?;
                Value::Tuple(Arc::from(self.decode_fields(ts, &mut inner, depth)?))
            }
            TypeDef::Struct(s) => {
                let tys: Vec<TypeId> = s.fields.iter().map(|f| f.ty).collect();
                let mut inner = get_bytes(input, "a struct")?;
                Value::Struct(Arc::from(self.decode_fields(&tys, &mut inner, depth)?))
            }
            TypeDef::Vec(t) => {
                let mut inner = get_bytes(input, "a vector")?;
                let n = get_count(&mut inner)?;
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    out.push(self.decode_at(*t, &mut inner, depth + 1)?);
                }
                done(inner, "a vector")?;
                Value::Vec(Arc::from(out))
            }
            TypeDef::Set(t) => {
                let mut inner = get_bytes(input, "a set")?;
                let n = get_count(&mut inner)?;
                let mut out = BTreeSet::new();
                for _ in 0..n {
                    out.insert(self.decode_at(*t, &mut inner, depth + 1)?);
                }
                done(inner, "a set")?;
                Value::Set(Arc::new(out))
            }
            TypeDef::Map(k, t) => {
                let mut inner = get_bytes(input, "a map")?;
                let n = get_count(&mut inner)?;
                let mut out = BTreeMap::new();
                for _ in 0..n {
                    let a = self.decode_at(*k, &mut inner, depth + 1)?;
                    let b = self.decode_at(*t, &mut inner, depth + 1)?;
                    out.insert(a, b);
                }
                done(inner, "a map")?;
                Value::Map(Arc::new(out))
            }
            TypeDef::Option(t) => {
                let mut inner = get_bytes(input, "an option")?;
                let v = match get_varint(&mut inner)? {
                    0 => Value::none(),
                    1 => Value::some(self.decode_at(*t, &mut inner, depth + 1)?),
                    n => return Err(WireError::Malformed(format!("an option with {n} elements"))),
                };
                done(inner, "an option")?;
                v
            }
            TypeDef::Enum(e) => {
                let number = u32::try_from(get_varint(input)?).map_err(|_| WireError::Malformed("variant".into()))?;
                let payload = get_bytes(input, "a variant")?;
                match e.variants.iter().find(|v| v.number == number) {
                    Some(var) => {
                        let tys: Vec<TypeId> = var.payload.iter().map(|f| f.ty).collect();
                        let mut inner = payload;
                        let fields = self.decode_fields(&tys, &mut inner, depth)?;
                        Value::Enum {
                            variant: number,
                            fields: Arc::from(fields),
                        }
                    }
                    // A newer version's variant: the local `#[unknown]` one, keeping its bytes (LANG-261).
                    None => match e.unknown {
                        Some(unknown) => Value::UnknownVariant {
                            variant: unknown,
                            wire_number: number,
                            payload: Arc::from(payload),
                        },
                        None => return Err(WireError::Malformed(format!("enum variant #{number} is not declared"))),
                    },
                }
            }
            TypeDef::Lattice(id) => {
                let ctor = self.lattice(*id)?.clone();
                let mut inner = get_bytes(input, "a lattice value")?;
                let l = self.decode_lattice(&ctor, &mut inner, depth + 1)?;
                done(inner, "a lattice value")?;
                Value::Lattice(l)
            }
            other => return Err(WireError::Unsupported(format!("values of type {other:?}"))),
        })
    }

    /// A tuple's fields, by field number (1-based); unknown fields are skipped, missing ones are an error.
    fn decode_fields(&self, tys: &[TypeId], input: &mut &[u8], depth: u32) -> Result<Vec<Value>, WireError> {
        let numbers: Vec<u32> = (1..=tys.len() as u32).collect();
        self.decode_numbered(tys, &numbers, input, depth)
    }

    fn decode_numbered(
        &self,
        tys: &[TypeId],
        numbers: &[u32],
        input: &mut &[u8],
        depth: u32,
    ) -> Result<Vec<Value>, WireError> {
        let n = get_count(input)?;
        let mut slots: Vec<Option<Value>> = vec![None; tys.len()];
        for _ in 0..n {
            let key = get_varint(input)?;
            let wt = (key & 7) as u8;
            let number = u32::try_from(key >> 3).map_err(|_| WireError::Malformed("field number".into()))?;
            match numbers.iter().position(|x| *x == number) {
                Some(i) => {
                    let ty = *tys.get(i).ok_or_else(|| WireError::Malformed("field index".into()))?;
                    let want = self.wire_type(ty)?;
                    if want != wt {
                        return Err(WireError::Malformed(format!(
                            "field #{number} has wire type {wt}, expected {want}"
                        )));
                    }
                    let v = self.decode_at(ty, input, depth + 1)?;
                    if let Some(slot) = slots.get_mut(i) {
                        if slot.is_some() {
                            return Err(WireError::Malformed(format!("field #{number} appears twice")));
                        }
                        *slot = Some(v);
                    }
                }
                None => skip(wt, input)?,
            }
        }
        slots
            .into_iter()
            .zip(numbers)
            .map(|(v, n)| v.ok_or_else(|| WireError::Malformed(format!("field #{n} is missing"))))
            .collect()
    }

    fn decode_lattice(&self, ctor: &LatticeCtor, input: &mut &[u8], depth: u32) -> Result<LatValue, WireError> {
        if depth > self.limits.max_nesting {
            return Err(WireError::Limit("nesting"));
        }
        let (&kind, rest) = input.split_first().ok_or(WireError::Truncated("a lattice kind"))?;
        *input = rest;
        Ok(match (kind, ctor) {
            (LAT_BOTTOM, _) => LatValue::Bottom,
            (LAT_TOP, _) => LatValue::Top,
            (LAT_BOOL, LatticeCtor::Bool) => LatValue::Bool(get_varint(input)? != 0),
            (LAT_ELEM, LatticeCtor::Max(t) | LatticeCtor::Min(t) | LatticeCtor::Point(t)) => {
                LatValue::Elem(Arc::new(self.decode_at(*t, input, depth)?))
            }
            (LAT_SET, LatticeCtor::Set(t) | LatticeCtor::PSet(t)) => {
                let n = get_count(input)?;
                let mut out = BTreeSet::new();
                for _ in 0..n {
                    out.insert(self.decode_at(*t, input, depth)?);
                }
                LatValue::Set(Arc::new(out))
            }
            (LAT_MAP, LatticeCtor::Map(k, inner)) => {
                let inner = self.lattice(*inner)?.clone();
                let n = get_count(input)?;
                let mut out = BTreeMap::new();
                for _ in 0..n {
                    let key = self.decode_at(*k, input, depth)?;
                    let mut nested = get_bytes(input, "a lattice map entry")?;
                    let v = self.decode_lattice(&inner, &mut nested, depth + 1)?;
                    done(nested, "a lattice map entry")?;
                    out.insert(key, v);
                }
                LatValue::Map(Arc::new(out))
            }
            (LAT_SEQ, LatticeCtor::Product { fields, .. }) => {
                let n = get_count(input)?;
                if n != fields.len() {
                    return Err(WireError::Malformed(format!(
                        "{n} fields for a product of {}",
                        fields.len()
                    )));
                }
                let mut out = Vec::with_capacity(n);
                for (_, id) in fields {
                    let inner = self.lattice(*id)?.clone();
                    let mut nested = get_bytes(input, "a product field")?;
                    out.push(self.decode_lattice(&inner, &mut nested, depth + 1)?);
                    done(nested, "a product field")?;
                }
                LatValue::Seq(out.into())
            }
            (k, c) => return Err(WireError::Malformed(format!("lattice kind {k} for {c:?}"))),
        })
    }

    /// The field numbers of `cols`: declared (`#n`), or position plus one.
    fn numbers(cols: &[Column]) -> Vec<u32> {
        cols.iter()
            .enumerate()
            .map(|(i, c)| c.field_no.map_or(i as u32 + 1, |f| f.0))
            .collect()
    }

    /// Appends a row of `cols` as a tuple.
    pub fn encode_row(&self, cols: &[Column], row: &[Value], out: &mut Vec<u8>) -> Result<(), WireError> {
        if cols.len() != row.len() {
            return Err(WireError::Malformed(format!(
                "{} values for {} columns",
                row.len(),
                cols.len()
            )));
        }
        let numbers = Self::numbers(cols);
        let mut order: Vec<usize> = (0..cols.len()).collect();
        order.sort_by_key(|i| numbers.get(*i).copied());
        put_varint(out, cols.len() as u64);
        for i in order {
            let (Some(c), Some(v), Some(n)) = (cols.get(i), row.get(i), numbers.get(i)) else {
                return Err(WireError::Malformed("column index".into()));
            };
            put_varint(out, (u64::from(*n) << 3) | u64::from(self.wire_type(c.ty)?));
            self.encode_value(c.ty, v, out)?;
        }
        Ok(())
    }

    /// Reads a row of `cols`.
    pub fn decode_row(&self, cols: &[Column], input: &mut &[u8]) -> Result<Vec<Value>, WireError> {
        let tys: Vec<TypeId> = cols.iter().map(|c| c.ty).collect();
        self.decode_numbered(&tys, &Self::numbers(cols), input, 0)
    }
}

fn signed(t: IntTy) -> bool {
    matches!(t, IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::I128)
}

fn utf8(b: &[u8]) -> Result<&str, WireError> {
    std::str::from_utf8(b).map_err(|_| WireError::Malformed("invalid UTF-8".into()))
}

fn done(rest: &[u8], what: &'static str) -> Result<(), WireError> {
    if rest.is_empty() {
        Ok(())
    } else {
        Err(WireError::Malformed(format!(
            "{} trailing bytes after {what}",
            rest.len()
        )))
    }
}

/// Skips a value of wire type `wt` (an unknown field).
fn skip(wt: u8, input: &mut &[u8]) -> Result<(), WireError> {
    match wt {
        WT_VARINT | WT_ZZ => {
            get_varint(input)?;
        }
        WT_FIXED64 => {
            take(input, 8, "fixed64")?;
        }
        WT_BYTES | WT_NESTED | WT_LATTICE => {
            get_bytes(input, "a skipped field")?;
        }
        WT_VARIANT => {
            get_varint(input)?;
            get_bytes(input, "a skipped variant")?;
        }
        other => return Err(WireError::Malformed(format!("unknown wire type {other}"))),
    }
    Ok(())
}
