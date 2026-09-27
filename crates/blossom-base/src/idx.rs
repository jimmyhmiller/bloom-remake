//! Dense `u32` id newtypes and the vector they index (ARCHITECTURE §2.1).
//!
//! Every id is created by [`define_idx!`]: a `Copy` newtype over `u32` that orders numerically, prints as
//! `RelId(3)` and serializes as a plain integer. Ids are positions in an [`IndexVec`], so they are dense and cheap.
//! Their numeric values carry no meaning: nothing observable may depend on them (the IR digest relabels them
//! canonically, ARCHITECTURE §2.10).
//!
//! Lookups never panic: [`IndexVec::get`] returns an `Option`, and [`IndexVec::get_or_bug`] turns a missing id into
//! an [`InternalError`](crate::InternalError) located at the caller, for code where a missing id is a violated
//! invariant.

use std::fmt;
use std::hash::Hash;
use std::marker::PhantomData;

use serde::de::{SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::InternalError;

// Ids are `u32`; widening one to `usize` must be lossless on every supported target.
const _: () = assert!(
    usize::BITS >= 32,
    "blossom requires a target whose usize has at least 32 bits"
);

/// A dense index type: a `u32` newtype usable as the key of an [`IndexVec`].
pub trait Idx: Copy + Eq + Ord + Hash + fmt::Debug + Send + Sync + 'static {
    /// The id with the given raw value.
    fn from_raw(raw: u32) -> Self;

    /// The raw `u32` value.
    fn raw(self) -> u32;

    /// The id as a `usize` position.
    fn index(self) -> usize {
        // Lossless: usize has at least 32 bits (checked above).
        self.raw() as usize
    }

    /// The id at position `index`, or an error if `index` does not fit in 32 bits.
    fn try_from_usize(index: usize) -> Result<Self, IdxOverflow> {
        u32::try_from(index).map(Self::from_raw).map_err(|_| IdxOverflow {
            id_type: std::any::type_name::<Self>(),
            index,
        })
    }
}

impl Idx for u32 {
    fn from_raw(raw: u32) -> Self {
        raw
    }
    fn raw(self) -> u32 {
        self
    }
}

/// A position that does not fit in a 32-bit id: more than 2³² elements in one [`IndexVec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("index {index} does not fit in the 32-bit id type {id_type}")]
pub struct IdxOverflow {
    /// The id type that overflowed.
    pub id_type: &'static str,
    /// The position that did not fit.
    pub index: usize,
}

/// Defines dense `u32` id newtypes implementing [`Idx`].
///
/// ```
/// blossom_base::define_idx! {
///     /// A widget in the widget table.
///     pub struct WidgetId;
/// }
/// use blossom_base::Idx;
/// let w = WidgetId::from_raw(3);
/// assert_eq!(w.index(), 3);
/// assert_eq!(format!("{w:?}"), "WidgetId(3)");
/// ```
#[macro_export]
macro_rules! define_idx {
    ($( $(#[$meta:meta])* $vis:vis struct $name:ident; )*) => {$(
        $(#[$meta])*
        #[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(transparent)]
        $vis struct $name(u32);

        impl $name {
            /// The id with the given raw value.
            #[inline]
            pub const fn from_raw(raw: u32) -> Self {
                Self(raw)
            }

            /// The raw `u32` value.
            #[inline]
            pub const fn raw(self) -> u32 {
                self.0
            }

            /// The id as a `usize` position.
            #[inline]
            pub const fn index(self) -> usize {
                // Lossless: blossom_base::idx checks at compile time that usize has at least 32 bits.
                self.0 as usize
            }
        }

        impl $crate::idx::Idx for $name {
            #[inline]
            fn from_raw(raw: u32) -> Self {
                Self(raw)
            }
            #[inline]
            fn raw(self) -> u32 {
                self.0
            }
        }

        impl ::core::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                ::core::write!(f, "{}({})", ::core::stringify!($name), self.0)
            }
        }

        impl $crate::__private::serde::Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> ::core::result::Result<S::Ok, S::Error>
            where
                S: $crate::__private::serde::Serializer,
            {
                serializer.serialize_u32(self.0)
            }
        }

        impl<'de> $crate::__private::serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> ::core::result::Result<Self, D::Error>
            where
                D: $crate::__private::serde::Deserializer<'de>,
            {
                <u32 as $crate::__private::serde::Deserialize<'de>>::deserialize(deserializer).map(Self)
            }
        }
    )*};
}

define_idx! {
    /// A source file in a [`SourceDb`](crate::SourceDb).
    pub struct FileId;
    /// A type in a `TypeTable` (blossom-value).
    pub struct TypeId;
    /// A lattice type declaration (`LatticeDef`).
    pub struct LatticeTypeId;
    /// A group or ring type declaration (`GroupDef`).
    pub struct GroupTypeId;
    /// A folded constant or literal of a program.
    pub struct ConstId;
    /// A deploy-time parameter.
    pub struct ParamId;
    /// A pure, extern or table function.
    pub struct FnId;
    /// A user-defined aggregate.
    pub struct UdaId;
    /// An async service declaration.
    pub struct ServiceId;
    /// A role of a (choreographic) program.
    pub struct RoleId;
    /// A relation.
    pub struct RelId;
    /// A rule.
    pub struct RuleId;
    /// A variable of one rule.
    pub struct VarId;
    /// A seeded or order-sensitive operator site.
    pub struct SiteId;
    /// A construct (a surface feature with native and expansion forms, ARCHITECTURE §2.6).
    pub struct ConstructId;
    /// A stratum.
    pub struct StratumId;
    /// A column position within a relation's schema.
    pub struct ColIdx;
    /// A runtime invariant declaration.
    pub struct InvariantId;
    /// A body occurrence (atom or lookup) of a rule.
    pub struct OccId;
    /// A native operator instance of a physical plan.
    pub struct NativeId;
    /// An aggregation table of a physical plan.
    pub struct AggTableId;
    /// A tick-local buffer of a physical plan.
    pub struct BufferId;
    /// An index of a relation in a physical plan.
    pub struct IndexId;
    /// An operator node of an `OpTree` in a physical plan.
    pub struct OpId;
    /// A goal (a fact that held at a node and tick) of a provenance graph.
    pub struct GoalId;
    /// A rule firing of a provenance graph.
    pub struct FiringId;
}

/// A vector indexed by a dense id type `I` instead of `usize`.
///
/// It never holds more than 2³² elements, so every position is a valid id. Ordering of elements carries no meaning
/// beyond the ids themselves.
pub struct IndexVec<I: Idx, T> {
    raw: Vec<T>,
    _marker: PhantomData<fn(&I)>,
}

impl<I: Idx, T> IndexVec<I, T> {
    /// An empty vector.
    pub const fn new() -> Self {
        Self {
            raw: Vec::new(),
            _marker: PhantomData,
        }
    }

    /// An empty vector with room for `capacity` elements.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            raw: Vec::with_capacity(capacity),
            _marker: PhantomData,
        }
    }

    /// Wraps a plain vector; fails if it has more elements than `I` can index.
    pub fn from_raw(raw: Vec<T>) -> Result<Self, IdxOverflow> {
        check_len::<I>(raw.len())?;
        Ok(Self {
            raw,
            _marker: PhantomData,
        })
    }

    /// Collects an iterator; fails if it yields more elements than `I` can index.
    pub fn try_from_iter(iter: impl IntoIterator<Item = T>) -> Result<Self, IdxOverflow> {
        let mut out = Self::new();
        for item in iter {
            out.push(item)?;
        }
        Ok(out)
    }

    /// The number of elements.
    pub fn len(&self) -> usize {
        self.raw.len()
    }

    /// Whether the vector is empty.
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// The id the next [`push`](Self::push) will return.
    pub fn next_idx(&self) -> Result<I, IdxOverflow> {
        I::try_from_usize(self.raw.len())
    }

    /// Appends an element and returns its id.
    pub fn push(&mut self, value: T) -> Result<I, IdxOverflow> {
        let id = self.next_idx()?;
        self.raw.push(value);
        Ok(id)
    }

    /// The element with id `id`, if any.
    pub fn get(&self, id: I) -> Option<&T> {
        self.raw.get(id.index())
    }

    /// The element with id `id`, mutably, if any.
    pub fn get_mut(&mut self, id: I) -> Option<&mut T> {
        self.raw.get_mut(id.index())
    }

    /// The element with id `id`, where a missing id is a violated internal invariant: returns an
    /// [`InternalError`] located at the caller (and panics in debug builds, like [`bug!`](crate::bug)).
    #[track_caller]
    pub fn get_or_bug(&self, id: I) -> Result<&T, InternalError> {
        let len = self.raw.len();
        match self.raw.get(id.index()) {
            Some(v) => Ok(v),
            None => Err(missing_id(id, len)),
        }
    }

    /// Mutable form of [`get_or_bug`](Self::get_or_bug).
    #[track_caller]
    pub fn get_mut_or_bug(&mut self, id: I) -> Result<&mut T, InternalError> {
        let len = self.raw.len();
        match self.raw.get_mut(id.index()) {
            Some(v) => Ok(v),
            None => Err(missing_id(id, len)),
        }
    }

    /// Mutable references to two distinct elements at once; `None` if the ids are equal or either is missing.
    pub fn pick2_mut(&mut self, a: I, b: I) -> Option<(&mut T, &mut T)> {
        let (ai, bi) = (a.index(), b.index());
        if ai == bi || ai >= self.raw.len() || bi >= self.raw.len() {
            return None;
        }
        let (lo, hi) = (ai.min(bi), ai.max(bi));
        let (head, tail) = self.raw.split_at_mut(hi);
        let low = head.get_mut(lo)?;
        let high = tail.first_mut()?;
        Some(if ai < bi { (low, high) } else { (high, low) })
    }

    /// Reserves room for `additional` more elements.
    pub fn reserve(&mut self, additional: usize) {
        self.raw.reserve(additional);
    }

    /// Removes and returns the last element with its id.
    pub fn pop(&mut self) -> Option<(I, T)> {
        let id = self.last_idx()?;
        self.raw.pop().map(|v| (id, v))
    }

    /// Keeps the first `len` elements (every id below `len`), dropping the rest.
    pub fn truncate(&mut self, len: usize) {
        self.raw.truncate(len);
    }

    /// Removes every element.
    pub fn clear(&mut self) {
        self.raw.clear();
    }

    /// Grows or shrinks to `len` elements, filling new positions with `fill()`; fails if `len` exceeds what `I`
    /// can index.
    pub fn resize_with(&mut self, len: usize, fill: impl FnMut() -> T) -> Result<(), IdxOverflow> {
        check_len::<I>(len)?;
        self.raw.resize_with(len, fill);
        Ok(())
    }

    /// Appends every element of `iter`; fails (keeping the elements appended so far) if `I` runs out of ids.
    pub fn try_extend(&mut self, iter: impl IntoIterator<Item = T>) -> Result<(), IdxOverflow> {
        for item in iter {
            self.push(item)?;
        }
        Ok(())
    }

    /// Whether `id` is a valid position.
    pub fn contains_idx(&self, id: I) -> bool {
        id.index() < self.raw.len()
    }

    /// The id of the last element, if any.
    pub fn last_idx(&self) -> Option<I> {
        self.raw.len().checked_sub(1).map(|i| I::from_raw(position_to_raw(i)))
    }

    /// Iterates over the elements in id order.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.raw.iter()
    }

    /// Iterates mutably over the elements in id order.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.raw.iter_mut()
    }

    /// Iterates over `(id, element)` pairs in id order.
    pub fn iter_enumerated(&self) -> impl DoubleEndedIterator<Item = (I, &T)> + ExactSizeIterator + '_ {
        self.raw
            .iter()
            .enumerate()
            .map(|(i, v)| (I::from_raw(position_to_raw(i)), v))
    }

    /// Iterates mutably over `(id, element)` pairs in id order.
    pub fn iter_enumerated_mut(&mut self) -> impl DoubleEndedIterator<Item = (I, &mut T)> + ExactSizeIterator + '_ {
        self.raw
            .iter_mut()
            .enumerate()
            .map(|(i, v)| (I::from_raw(position_to_raw(i)), v))
    }

    /// Iterates over every valid id in order.
    pub fn indices(&self) -> impl DoubleEndedIterator<Item = I> + ExactSizeIterator + 'static {
        (0..self.raw.len()).map(|i| I::from_raw(position_to_raw(i)))
    }

    /// The elements as a slice, in id order.
    pub fn as_slice(&self) -> &[T] {
        &self.raw
    }

    /// The elements as a mutable slice, in id order.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.raw
    }

    /// Unwraps into the plain vector.
    pub fn into_raw(self) -> Vec<T> {
        self.raw
    }

    /// Maps every element, keeping ids.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> IndexVec<I, U> {
        IndexVec {
            raw: self.raw.into_iter().map(f).collect(),
            _marker: PhantomData,
        }
    }
}

/// Converts a position of an `IndexVec` into a raw id.
///
/// Exact: every `IndexVec` is built through `check_len`/`next_idx`, so it holds at most 2³² elements and every
/// position is below 2³².
#[inline]
fn position_to_raw(position: usize) -> u32 {
    position as u32
}

fn check_len<I: Idx>(len: usize) -> Result<(), IdxOverflow> {
    match len.checked_sub(1) {
        Some(last) => I::try_from_usize(last).map(|_| ()),
        None => Ok(()),
    }
}

#[track_caller]
fn missing_id<I: Idx>(id: I, len: usize) -> InternalError {
    let location = std::panic::Location::caller();
    InternalError::at(
        format!("id {id:?} is out of range for an IndexVec of length {len}"),
        location.file(),
        location.line(),
        cfg!(debug_assertions),
    )
}

impl<I: Idx, T> Default for IndexVec<I, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I: Idx, T: Clone> Clone for IndexVec<I, T> {
    fn clone(&self) -> Self {
        Self {
            raw: self.raw.clone(),
            _marker: PhantomData,
        }
    }
}

impl<I: Idx, T: fmt::Debug> fmt::Debug for IndexVec<I, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter_enumerated()).finish()
    }
}

impl<I: Idx, T: PartialEq> PartialEq for IndexVec<I, T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<I: Idx, T: Eq> Eq for IndexVec<I, T> {}

impl<I: Idx, T: Hash> Hash for IndexVec<I, T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl<'a, I: Idx, T> IntoIterator for &'a IndexVec<I, T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.raw.iter()
    }
}

impl<I: Idx, T> IntoIterator for IndexVec<I, T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.raw.into_iter()
    }
}

impl<I: Idx, T: Serialize> Serialize for IndexVec<I, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.raw.len()))?;
        for item in &self.raw {
            seq.serialize_element(item)?;
        }
        seq.end()
    }
}

impl<'de, I: Idx, T: Deserialize<'de>> Deserialize<'de> for IndexVec<I, T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SeqVisitor<I, T>(PhantomData<fn(&I) -> T>);
        impl<'de, I: Idx, T: Deserialize<'de>> Visitor<'de> for SeqVisitor<I, T> {
            type Value = IndexVec<I, T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a sequence of IndexVec elements")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = IndexVec::with_capacity(seq.size_hint().unwrap_or(0).min(4096));
                while let Some(item) = seq.next_element()? {
                    out.push(item).map_err(serde::de::Error::custom)?;
                }
                Ok(out)
            }
        }
        deserializer.deserialize_seq(SeqVisitor(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idx_roundtrip_and_debug() {
        let r = RelId::from_raw(7);
        assert_eq!(r.raw(), 7);
        assert_eq!(r.index(), 7);
        assert_eq!(format!("{r:?}"), "RelId(7)");
        assert_eq!(<RelId as Idx>::try_from_usize(7), Ok(r));
        assert!(RelId::from_raw(1) < RelId::from_raw(2));
    }

    #[test]
    fn idx_try_from_usize_overflow() {
        let too_big = usize::try_from(u64::from(u32::MAX) + 1).unwrap();
        let err = <RuleId as Idx>::try_from_usize(too_big).unwrap_err();
        assert_eq!(err.index, too_big);
        assert!(err.id_type.ends_with("RuleId"), "{}", err.id_type);
    }

    #[test]
    fn idx_serde_is_a_plain_integer() {
        let json = serde_json::to_string(&TypeId::from_raw(42)).unwrap();
        assert_eq!(json, "42");
        let back: TypeId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TypeId::from_raw(42));
    }

    #[test]
    fn idx_vec_push_get_enumerate() {
        let mut v: IndexVec<RelId, &str> = IndexVec::new();
        let a = v.push("a").unwrap();
        let b = v.push("b").unwrap();
        assert_eq!((a, b), (RelId::from_raw(0), RelId::from_raw(1)));
        assert_eq!(v.get(b), Some(&"b"));
        assert_eq!(v.get(RelId::from_raw(2)), None);
        assert_eq!(v.last_idx(), Some(b));
        assert!(v.contains_idx(a) && !v.contains_idx(RelId::from_raw(5)));
        let pairs: Vec<_> = v.iter_enumerated().map(|(i, s)| (i.raw(), *s)).collect();
        assert_eq!(pairs, vec![(0, "a"), (1, "b")]);
        assert_eq!(v.indices().collect::<Vec<_>>(), vec![a, b]);
        *v.get_mut(a).unwrap() = "z";
        assert_eq!(v.get_or_bug(a).unwrap(), &"z");
        assert_eq!(v.next_idx().unwrap(), RelId::from_raw(2));
        assert_eq!(format!("{v:?}"), r#"{RelId(0): "z", RelId(1): "b"}"#);
    }

    #[test]
    fn idx_vec_serde_roundtrip() {
        let v = IndexVec::<RuleId, u8>::try_from_iter([3, 1, 2]).unwrap();
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, "[3,1,2]");
        let back: IndexVec<RuleId, u8> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
        assert_eq!(back.map(u32::from).into_raw(), vec![3u32, 1, 2]);
    }

    #[test]
    fn idx_vec_mutation_helpers() {
        let mut v = IndexVec::<RelId, u32>::try_from_iter([10, 20, 30]).unwrap();
        let (a, c) = (RelId::from_raw(0), RelId::from_raw(2));
        let (x, z) = v.pick2_mut(c, a).unwrap();
        std::mem::swap(x, z);
        assert_eq!(v.as_slice(), &[30, 20, 10]);
        assert!(v.pick2_mut(a, a).is_none());
        assert!(v.pick2_mut(a, RelId::from_raw(3)).is_none());
        assert_eq!(v.pop(), Some((c, 10)));
        v.try_extend([40, 50]).unwrap();
        assert_eq!(v.as_slice(), &[30, 20, 40, 50]);
        v.truncate(1);
        v.resize_with(3, || 7).unwrap();
        assert_eq!(v.as_slice(), &[30, 7, 7]);
        v.reserve(8);
        v.clear();
        assert_eq!(v.pop(), None);
    }

    #[test]
    fn idx_vec_empty() {
        let v: IndexVec<SiteId, ()> = IndexVec::default();
        assert!(v.is_empty());
        assert_eq!(v.last_idx(), None);
        assert_eq!(v.indices().count(), 0);
    }

    // A missing id is a bug: it panics in debug builds and is returned as an InternalError in release builds.
    #[test]
    #[cfg_attr(debug_assertions, should_panic(expected = "out of range"))]
    fn idx_vec_get_or_bug_missing_id() {
        let v: IndexVec<RelId, u8> = IndexVec::new();
        let err = v.get_or_bug(RelId::from_raw(3)).unwrap_err();
        assert!(err.what.contains("RelId(3)"), "{err}");
        assert!(err.file.ends_with("idx.rs"));
    }
}
