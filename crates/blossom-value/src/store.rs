//! How every oracle-free part of the system sees values: the [`ValueStore`] (ARCHITECTURE §4.1).
//!
//! The engine's interner (`blossom-kernel::intern`) implements [`ValueStore`]; [`RefValueStore`] is the simple,
//! complete reference implementation over [`Value`]s used by oracle-free tests, the wire tests and host code
//! (implemented by WP M2.1).

use std::cmp::Ordering;
use std::sync::Arc;

use blossom_base::{TypeId, unimplemented_feature};
use smallvec::SmallVec;

use crate::error::ValueError;
use crate::fp::Fingerprint;
use crate::types::TypeTable;
use crate::value::Value;
use crate::word::{BytesWord, StrWord, Word};

/// Something that turns a record's child words into the record's word: a [`ValueStore`] or a
/// [`WordSink`](crate::WordSink)'s ingest arena.
pub trait RecordTarget {
    /// Interns (or stores) the record of type `ty` with the given children, returning its word.
    fn finish_record(&mut self, ty: TypeId, children: &[Word]) -> Result<Word, ValueError>;
}

/// Builds a record bottom-up from its children's words (tuples, structs, enum payloads, collections in canonical
/// order).
pub struct RecordBuilder<'a> {
    target: &'a mut dyn RecordTarget,
    ty: TypeId,
    children: SmallVec<[Word; 8]>,
}

impl<'a> RecordBuilder<'a> {
    /// A builder for a record of type `ty` finished into `target`.
    pub fn new(target: &'a mut dyn RecordTarget, ty: TypeId) -> RecordBuilder<'a> {
        RecordBuilder {
            target,
            ty,
            children: SmallVec::new(),
        }
    }

    /// The record's type.
    pub fn ty(&self) -> TypeId {
        self.ty
    }

    /// The number of children pushed so far.
    pub fn len(&self) -> usize {
        self.children.len()
    }

    /// Whether no child was pushed.
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// Appends a child word.
    pub fn push(&mut self, child: Word) -> &mut Self {
        self.children.push(child);
        self
    }

    /// Appends child words.
    pub fn extend(&mut self, children: &[Word]) -> &mut Self {
        self.children.extend_from_slice(children);
        self
    }

    /// Finishes the record, returning its word.
    pub fn finish(self) -> Result<Word, ValueError> {
        self.target.finish_record(self.ty, &self.children)
    }
}

/// The value store interface (ARCHITECTURE §4.1).
pub trait ValueStore: RecordTarget {
    /// Interns bytes of type `ty`, probing by slice and copying only on a miss.
    fn intern_bytes(&mut self, ty: TypeId, bytes: &[u8]) -> Result<Word, ValueError>;
    /// Interns a string of type `ty`.
    fn intern_str(&mut self, ty: TypeId, s: &str) -> Result<Word, ValueError>;
    /// A builder for a record of type `ty`, built bottom-up from child words.
    fn record(&mut self, ty: TypeId) -> RecordBuilder<'_>;
    /// Interns a whole value (the oracle, the REPL, the dynamic host API and dumps).
    fn intern_value(&mut self, ty: TypeId, v: &Value) -> Result<Word, ValueError>;
    /// The fingerprint of a word of type `ty`.
    fn fingerprint(&self, ty: TypeId, w: Word) -> Result<Fingerprint, ValueError>;
    /// The canonical order of two words of type `ty` (SEM-088).
    fn cmp_canonical(&self, ty: TypeId, a: Word, b: Word) -> Result<Ordering, ValueError>;
    /// The value a word of type `ty` denotes.
    fn to_value(&self, ty: TypeId, w: Word) -> Result<Value, ValueError>;
    /// The text of a string word.
    fn str_of(&self, w: StrWord) -> Result<&str, ValueError>;
    /// The bytes of a bytes word.
    fn bytes_of(&self, w: BytesWord) -> Result<&[u8], ValueError>;
}

/// The reference [`ValueStore`]: interns [`Value`]s, compares through the canonical `Value` order and computes
/// fingerprints from values. Implemented by WP M2.1.
pub struct RefValueStore {
    types: Arc<TypeTable>,
}

impl RefValueStore {
    /// A store for values of the types in `types`.
    pub fn new(types: Arc<TypeTable>) -> RefValueStore {
        RefValueStore { types }
    }

    /// The type table.
    pub fn types(&self) -> &TypeTable {
        &self.types
    }
}

impl RecordTarget for RefValueStore {
    fn finish_record(&mut self, ty: TypeId, children: &[Word]) -> Result<Word, ValueError> {
        let _ = (ty, children);
        unimplemented_feature!("LANG-023", "records in the reference value store (WP M2.1)")
    }
}

impl ValueStore for RefValueStore {
    fn intern_bytes(&mut self, ty: TypeId, bytes: &[u8]) -> Result<Word, ValueError> {
        let _ = (ty, bytes);
        unimplemented_feature!("LANG-023", "interning bytes in the reference value store (WP M2.1)")
    }

    fn intern_str(&mut self, ty: TypeId, s: &str) -> Result<Word, ValueError> {
        let _ = (ty, s);
        unimplemented_feature!("LANG-023", "interning strings in the reference value store (WP M2.1)")
    }

    fn record(&mut self, ty: TypeId) -> RecordBuilder<'_> {
        RecordBuilder::new(self, ty)
    }

    fn intern_value(&mut self, ty: TypeId, v: &Value) -> Result<Word, ValueError> {
        let _ = (ty, v);
        unimplemented_feature!("LANG-023", "interning values in the reference value store (WP M2.1)")
    }

    fn fingerprint(&self, ty: TypeId, w: Word) -> Result<Fingerprint, ValueError> {
        let _ = (ty, w);
        unimplemented_feature!("ENG-032", "fingerprints in the reference value store (WP M2.1)")
    }

    fn cmp_canonical(&self, ty: TypeId, a: Word, b: Word) -> Result<Ordering, ValueError> {
        let _ = (ty, a, b);
        unimplemented_feature!(
            "LANG-023",
            "canonical comparison in the reference value store (WP M2.1)"
        )
    }

    fn to_value(&self, ty: TypeId, w: Word) -> Result<Value, ValueError> {
        let _ = (ty, w);
        unimplemented_feature!("LANG-023", "decoding words in the reference value store (WP M2.1)")
    }

    fn str_of(&self, w: StrWord) -> Result<&str, ValueError> {
        let _ = w;
        unimplemented_feature!("LANG-023", "string lookup in the reference value store (WP M2.1)")
    }

    fn bytes_of(&self, w: BytesWord) -> Result<&[u8], ValueError> {
        let _ = w;
        unimplemented_feature!("LANG-023", "bytes lookup in the reference value store (WP M2.1)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A target that numbers records by their children, to exercise the builder.
    struct Counting(Vec<Vec<Word>>);

    impl RecordTarget for Counting {
        fn finish_record(&mut self, _ty: TypeId, children: &[Word]) -> Result<Word, ValueError> {
            self.0.push(children.to_vec());
            Ok(Word(self.0.len() as u64 - 1))
        }
    }

    #[test]
    fn record_builder_collects_children() {
        let mut target = Counting(Vec::new());
        let mut b = RecordBuilder::new(&mut target, TypeId::from_raw(3));
        assert!(b.is_empty());
        b.push(Word(1)).extend(&[Word(2), Word(3)]);
        assert_eq!((b.len(), b.ty()), (3, TypeId::from_raw(3)));
        assert_eq!(b.finish().unwrap(), Word(0));
        assert_eq!(target.0, vec![vec![Word(1), Word(2), Word(3)]]);
    }

    #[test]
    fn ref_store_works_or_is_unimplemented_until_m2_1() {
        let mut store = RefValueStore::new(Arc::new(TypeTable::new()));
        assert!(store.types().is_empty());
        match store.intern_str(TypeId::from_raw(0), "x") {
            Ok(_) => {}
            Err(ValueError::Unimplemented(u)) => assert_eq!(u.feature.as_str(), "LANG-023"),
            Err(ValueError::UnknownType(_)) => {}
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
}
