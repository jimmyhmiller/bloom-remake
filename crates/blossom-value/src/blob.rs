//! Blobs (LANGUAGE §16.6, FOREIGN-PROTOCOLS §5): large immutable byte payloads kept out of the engine, named by their
//! content. A [`BlobRef`] is the BLAKE3 hash of the bytes and their length, so creating one is a pure function of the
//! bytes; the bytes themselves live in a [`BlobSource`] the host provides.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::value::BlobRef;

impl BlobRef {
    /// The handle of `bytes`.
    pub fn of(bytes: &[u8]) -> BlobRef {
        BlobRef {
            hash: *blake3::hash(bytes).as_bytes(),
            len: bytes.len() as u64,
        }
    }

    /// The hash as lowercase hex (a blob's file name in a store).
    pub fn hex(&self) -> String {
        self.hash.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Where an evaluator reads the bytes of blobs created before the tick it evaluates.
pub trait BlobSource: Send + Sync + std::fmt::Debug {
    /// The bytes of `b`, if the source has them.
    fn get(&self, b: &BlobRef) -> Option<Arc<[u8]>>;
}

/// A source with no blobs: for programs and runs that create every blob they read in the same tick.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoBlobs;

impl BlobSource for NoBlobs {
    fn get(&self, _b: &BlobRef) -> Option<Arc<[u8]>> {
        None
    }
}

/// Blobs held in memory: a simulation's store, or blobs created in earlier ticks and not yet made durable.
#[derive(Clone, Debug, Default)]
pub struct BlobMap(pub BTreeMap<BlobRef, Arc<[u8]>>);

impl BlobSource for BlobMap {
    fn get(&self, b: &BlobRef) -> Option<Arc<[u8]>> {
        self.0.get(b).cloned()
    }
}

/// The blobs a value holds, anywhere inside it.
pub fn blobs_in(v: &crate::Value, out: &mut std::collections::BTreeSet<BlobRef>) {
    use crate::Value;
    match v {
        Value::Blob(b) => {
            out.insert(*b);
        }
        Value::Tuple(xs) | Value::Struct(xs) | Value::Vec(xs) => xs.iter().for_each(|x| blobs_in(x, out)),
        Value::Enum { fields, .. } => fields.iter().for_each(|x| blobs_in(x, out)),
        Value::Option(Some(x)) => blobs_in(x, out),
        Value::Set(s) => s.iter().for_each(|x| blobs_in(x, out)),
        Value::Map(m) => m.iter().for_each(|(k, x)| {
            blobs_in(k, out);
            blobs_in(x, out);
        }),
        Value::Lattice(l) => lattice_blobs(l, out),
        _ => {}
    }
}

fn lattice_blobs(l: &crate::LatValue, out: &mut std::collections::BTreeSet<BlobRef>) {
    use crate::LatValue as L;
    match l {
        L::Elem(x) => blobs_in(x, out),
        L::Set(s) => s.iter().for_each(|x| blobs_in(x, out)),
        L::Map(m) => m.iter().for_each(|(k, x)| {
            blobs_in(k, out);
            lattice_blobs(x, out);
        }),
        L::Bag(m) => m.keys().for_each(|k| blobs_in(k, out)),
        L::Seq(xs) => xs.iter().for_each(|x| lattice_blobs(x, out)),
        L::Bottom | L::Top | L::Bool(_) | L::Extern { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_is_the_content_hash_and_length() {
        let a = BlobRef::of(b"hello");
        assert_eq!(a, BlobRef::of(b"hello"));
        assert_ne!(a, BlobRef::of(b"hellp"));
        assert_eq!(a.len, 5);
        // BLAKE3("hello"), the reference implementation's answer.
        assert_eq!(a.hex(), "ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f");
    }
}
