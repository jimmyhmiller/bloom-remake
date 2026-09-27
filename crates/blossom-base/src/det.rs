//! Deterministic hashed collections (ARCHITECTURE §12.1, ARCH-18, ARCH-19).
//!
//! [`DetMap`] and [`DetSet`] are the only hashed collections in Blossom; `std` and `hashbrown` maps are banned by
//! `clippy.toml`. They are built on [`hashbrown::HashTable`] with an explicitly keyed folded-multiply hasher
//! ([`DetState`]):
//!
//! - [`DetState::fixed`] uses a constant key, so hashing is the same in every process (use it for compiler data);
//! - [`DetState::from_nonce`] derives the key from a per-incarnation boot nonce, so remote clients cannot predict
//!   it (use it for tables fed by network input, ARCHITECTURE §4.1).
//!
//! **Iteration order must never be observable.** It depends on the key, on capacity and on insertion history.
//! Anything that leaves a component (dumps, traces, messages, diagnostics, digests) must go through a canonical
//! order: use [`DetMap::sorted`]/[`DetSet::sorted`] or a `BTreeMap`. `Debug` and `serde` output of these collections
//! is sorted for that reason, and requires `Ord` keys.

use std::borrow::Borrow;
use std::fmt;
use std::hash::{BuildHasher, Hash, Hasher};
use std::iter::FusedIterator;
use std::marker::PhantomData;

use hashbrown::HashTable;
use hashbrown::hash_table;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Digits of π, used as fixed key material.
const PI: [u64; 4] = [
    0x243f_6a88_85a3_08d3,
    0x1319_8a2e_0370_7344,
    0xa409_3822_299f_31d0,
    0x082e_fa98_ec4e_6c89,
];

/// The key of a [`DetHasher`]: a [`BuildHasher`] for [`DetMap`] and [`DetSet`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct DetState {
    k0: u64,
    k1: u64,
}

impl DetState {
    /// The fixed key: hashing is identical in every process and on every platform.
    pub const fn fixed() -> DetState {
        DetState {
            k0: PI[0],
            k1: PI[1] | 1,
        }
    }

    /// A key derived from a boot nonce (OS entropy recorded for replay, DIST-033).
    pub const fn from_nonce(nonce: u64) -> DetState {
        let k0 = splitmix64(nonce ^ PI[2]);
        let k1 = splitmix64(k0 ^ PI[3]) | 1;
        DetState { k0, k1 }
    }
}

impl Default for DetState {
    fn default() -> DetState {
        DetState::fixed()
    }
}

impl BuildHasher for DetState {
    type Hasher = DetHasher;

    #[inline]
    fn build_hasher(&self) -> DetHasher {
        DetHasher {
            acc: self.k0,
            mul: self.k1,
        }
    }
}

/// The SplitMix64 finalizer (a bijection on `u64`).
const fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The full 128-bit product folded to 64 bits: both halves of the result depend on every input bit.
#[inline]
const fn folded_multiply(x: u64, y: u64) -> u64 {
    let full = (x as u128).wrapping_mul(y as u128);
    (full as u64) ^ ((full >> 64) as u64)
}

/// A keyed folded-multiply hasher. Created by [`DetState`]; byte order is fixed (little-endian), so hashes are
/// the same on every platform.
#[derive(Clone, Debug)]
pub struct DetHasher {
    acc: u64,
    mul: u64,
}

impl Hasher for DetHasher {
    #[inline]
    fn write_u64(&mut self, x: u64) {
        self.acc = folded_multiply(self.acc ^ x, self.mul);
    }

    #[inline]
    fn write_u8(&mut self, x: u8) {
        self.write_u64(u64::from(x));
    }

    #[inline]
    fn write_u16(&mut self, x: u16) {
        self.write_u64(u64::from(x));
    }

    #[inline]
    fn write_u32(&mut self, x: u32) {
        self.write_u64(u64::from(x));
    }

    #[inline]
    fn write_u128(&mut self, x: u128) {
        self.write_u64(x as u64);
        self.write_u64((x >> 64) as u64);
    }

    #[inline]
    fn write_usize(&mut self, x: usize) {
        // Hash usize as u64 so 32- and 64-bit targets agree.
        self.write_u64(x as u64);
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(word));
        }
        let mut tail = [0u8; 8];
        for (dst, src) in tail.iter_mut().zip(chunks.remainder()) {
            *dst = *src;
        }
        // The length keeps inputs that differ only in trailing zero bytes apart.
        self.write_u64(u64::from_le_bytes(tail) ^ ((bytes.len() as u64) << 56));
    }

    #[inline]
    fn finish(&self) -> u64 {
        folded_multiply(self.acc ^ PI[2], self.mul ^ PI[3]).rotate_left(23)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// DetMap
// ---------------------------------------------------------------------------------------------------------------

/// A hash map with a deterministic, explicitly keyed hasher. See the [module docs](self) for the iteration rule.
pub struct DetMap<K, V> {
    table: HashTable<(K, V)>,
    state: DetState,
}

impl<K, V> DetMap<K, V> {
    /// An empty map with the fixed key.
    pub const fn new() -> Self {
        DetMap {
            table: HashTable::new(),
            state: DetState::fixed(),
        }
    }

    /// An empty map with the given key.
    pub const fn with_state(state: DetState) -> Self {
        DetMap {
            table: HashTable::new(),
            state,
        }
    }

    /// An empty map with the fixed key and room for `capacity` entries.
    pub fn with_capacity(capacity: usize) -> Self {
        DetMap::with_capacity_and_state(capacity, DetState::fixed())
    }

    /// An empty map with the given key and room for `capacity` entries.
    pub fn with_capacity_and_state(capacity: usize, state: DetState) -> Self {
        DetMap {
            table: HashTable::with_capacity(capacity),
            state,
        }
    }

    /// The hasher key.
    pub fn state(&self) -> DetState {
        self.state
    }

    /// How many entries the map holds without reallocating.
    pub fn capacity(&self) -> usize {
        self.table.capacity()
    }

    /// Removes every entry, yielding them in an unspecified order that must never be observable. The allocation is
    /// kept.
    pub fn drain(&mut self) -> Drain<'_, K, V> {
        Drain(self.table.drain())
    }

    /// The number of entries.
    pub fn len(&self) -> usize {
        self.table.len()
    }

    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// Removes every entry, keeping the allocation.
    pub fn clear(&mut self) {
        self.table.clear();
    }

    /// Iterates in an unspecified order that must never be observable (see the [module docs](self)).
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter(self.table.iter())
    }

    /// Iterates mutably in an unspecified order that must never be observable.
    pub fn iter_mut(&mut self) -> IterMut<'_, K, V> {
        IterMut(self.table.iter_mut())
    }

    /// The keys, in an unspecified order that must never be observable.
    pub fn keys(&self) -> impl Iterator<Item = &K> + '_ {
        self.table.iter().map(|(k, _)| k)
    }

    /// The values, in an unspecified order that must never be observable.
    pub fn values(&self) -> impl Iterator<Item = &V> + '_ {
        self.table.iter().map(|(_, v)| v)
    }

    /// The values, mutably, in an unspecified order that must never be observable.
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> + '_ {
        self.table.iter_mut().map(|(_, v)| v)
    }

    /// Keeps only the entries for which `keep` returns true.
    pub fn retain(&mut self, mut keep: impl FnMut(&K, &mut V) -> bool) {
        self.table.retain(|(k, v)| keep(k, v));
    }

    /// The entries sorted by key: the canonical order for anything observable.
    pub fn sorted(&self) -> Vec<(&K, &V)>
    where
        K: Ord,
    {
        let mut out: Vec<(&K, &V)> = self.iter().collect();
        out.sort_unstable_by(|a, b| a.0.cmp(b.0));
        out
    }

    /// The entries sorted by key, by value.
    pub fn into_sorted_vec(self) -> Vec<(K, V)>
    where
        K: Ord,
    {
        let mut out: Vec<(K, V)> = self.table.into_iter().collect();
        out.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

impl<K: Hash + Eq, V> DetMap<K, V> {
    fn hash_of<Q: Hash + ?Sized>(&self, key: &Q) -> u64 {
        self.state.hash_one(key)
    }

    /// Reserves room for `additional` more entries.
    pub fn reserve(&mut self, additional: usize) {
        let state = self.state;
        self.table.reserve(additional, |(k, _)| state.hash_one(k));
    }

    /// Inserts an entry, returning the previous value for the key.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        match self.entry(key) {
            Entry::Occupied(mut e) => Some(e.insert(value)),
            Entry::Vacant(e) => {
                e.insert(value);
                None
            }
        }
    }

    /// The value for `key`.
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.get_key_value(key).map(|(_, v)| v)
    }

    /// The stored key and value for `key`.
    pub fn get_key_value<Q>(&self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hash_of(key);
        self.table.find(hash, |(k, _)| k.borrow() == key).map(|(k, v)| (k, v))
    }

    /// The value for `key`, mutably.
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hash_of(key);
        self.table.find_mut(hash, |(k, _)| k.borrow() == key).map(|(_, v)| v)
    }

    /// Whether the map has an entry for `key`.
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.get_key_value(key).is_some()
    }

    /// Removes the entry for `key`, returning its value.
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.remove_entry(key).map(|(_, v)| v)
    }

    /// Removes the entry for `key`, returning it.
    pub fn remove_entry<Q>(&mut self, key: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hash_of(key);
        match self.table.find_entry(hash, |(k, _)| k.borrow() == key) {
            Ok(entry) => Some(entry.remove().0),
            Err(_) => None,
        }
    }

    /// The entry for `key`, for in-place insertion or update.
    pub fn entry(&mut self, key: K) -> Entry<'_, K, V> {
        let state = self.state;
        let hash = state.hash_one(&key);
        match self.table.entry(hash, |(k, _)| *k == key, |(k, _)| state.hash_one(k)) {
            hash_table::Entry::Occupied(inner) => Entry::Occupied(OccupiedEntry { inner }),
            hash_table::Entry::Vacant(inner) => Entry::Vacant(VacantEntry { inner, key }),
        }
    }
}

/// A view into one entry of a [`DetMap`].
pub enum Entry<'a, K, V> {
    /// The key is present.
    Occupied(OccupiedEntry<'a, K, V>),
    /// The key is absent.
    Vacant(VacantEntry<'a, K, V>),
}

/// A present entry of a [`DetMap`].
pub struct OccupiedEntry<'a, K, V> {
    inner: hash_table::OccupiedEntry<'a, (K, V)>,
}

/// An absent entry of a [`DetMap`].
pub struct VacantEntry<'a, K, V> {
    inner: hash_table::VacantEntry<'a, (K, V)>,
    key: K,
}

impl<'a, K, V> Entry<'a, K, V> {
    /// The value, inserting `value` first if absent.
    pub fn or_insert(self, value: V) -> &'a mut V {
        match self {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(value),
        }
    }

    /// The value, inserting `make()` first if absent.
    pub fn or_insert_with(self, make: impl FnOnce() -> V) -> &'a mut V {
        match self {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(make()),
        }
    }

    /// The value, inserting `V::default()` first if absent.
    pub fn or_default(self) -> &'a mut V
    where
        V: Default,
    {
        self.or_insert_with(V::default)
    }

    /// Applies `f` to the value if present.
    pub fn and_modify(self, f: impl FnOnce(&mut V)) -> Self {
        match self {
            Entry::Occupied(mut e) => {
                f(e.get_mut());
                Entry::Occupied(e)
            }
            vacant @ Entry::Vacant(_) => vacant,
        }
    }

    /// The entry's key.
    pub fn key(&self) -> &K {
        match self {
            Entry::Occupied(e) => e.key(),
            Entry::Vacant(e) => e.key(),
        }
    }
}

impl<'a, K, V> OccupiedEntry<'a, K, V> {
    /// The key.
    pub fn key(&self) -> &K {
        &self.inner.get().0
    }

    /// The value.
    pub fn get(&self) -> &V {
        &self.inner.get().1
    }

    /// The value, mutably.
    pub fn get_mut(&mut self) -> &mut V {
        &mut self.inner.get_mut().1
    }

    /// The value, mutably, for the map's lifetime.
    pub fn into_mut(self) -> &'a mut V {
        &mut self.inner.into_mut().1
    }

    /// Replaces the value, returning the old one.
    pub fn insert(&mut self, value: V) -> V {
        std::mem::replace(self.get_mut(), value)
    }

    /// Removes the entry, returning its value.
    pub fn remove(self) -> V {
        self.inner.remove().0.1
    }
}

impl<'a, K, V> VacantEntry<'a, K, V> {
    /// The key.
    pub fn key(&self) -> &K {
        &self.key
    }

    /// Gives the key back.
    pub fn into_key(self) -> K {
        self.key
    }

    /// Inserts the value.
    pub fn insert(self, value: V) -> &'a mut V {
        &mut self.inner.insert((self.key, value)).into_mut().1
    }
}

// The iterators wrap hashbrown's, so hashbrown stays out of this frozen crate's public API.

/// Iterator over a [`DetMap`]; its order must never be observable.
pub struct Iter<'a, K, V>(hash_table::Iter<'a, (K, V)>);

impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(k, v)| (k, v))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<K, V> ExactSizeIterator for Iter<'_, K, V> {}
impl<K, V> FusedIterator for Iter<'_, K, V> {}

impl<K, V> Clone for Iter<'_, K, V> {
    fn clone(&self) -> Self {
        Iter(self.0.clone())
    }
}

/// Mutable iterator over a [`DetMap`]; its order must never be observable.
pub struct IterMut<'a, K, V>(hash_table::IterMut<'a, (K, V)>);

impl<'a, K, V> Iterator for IterMut<'a, K, V> {
    type Item = (&'a K, &'a mut V);
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(k, v)| (&*k, v))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<K, V> ExactSizeIterator for IterMut<'_, K, V> {}
impl<K, V> FusedIterator for IterMut<'_, K, V> {}

/// Owning iterator over a [`DetMap`]; its order must never be observable.
pub struct IntoIter<K, V>(hash_table::IntoIter<(K, V)>);

impl<K, V> Iterator for IntoIter<K, V> {
    type Item = (K, V);
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<K, V> ExactSizeIterator for IntoIter<K, V> {}
impl<K, V> FusedIterator for IntoIter<K, V> {}

/// Draining iterator over a [`DetMap`]; its order must never be observable.
pub struct Drain<'a, K, V>(hash_table::Drain<'a, (K, V)>);

impl<K, V> Iterator for Drain<'_, K, V> {
    type Item = (K, V);
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<K, V> ExactSizeIterator for Drain<'_, K, V> {}
impl<K, V> FusedIterator for Drain<'_, K, V> {}

impl<'a, K, V> IntoIterator for &'a DetMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<K, V> IntoIterator for DetMap<K, V> {
    type Item = (K, V);
    type IntoIter = IntoIter<K, V>;
    /// Consumes the map in an unspecified order that must never be observable.
    fn into_iter(self) -> Self::IntoIter {
        IntoIter(self.table.into_iter())
    }
}

impl<K: Hash + Eq, V> Extend<(K, V)> for DetMap<K, V> {
    fn extend<T: IntoIterator<Item = (K, V)>>(&mut self, iter: T) {
        for (k, v) in iter {
            self.insert(k, v);
        }
    }
}

impl<K: Hash + Eq, V> FromIterator<(K, V)> for DetMap<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut map = DetMap::new();
        map.extend(iter);
        map
    }
}

impl<K, V> Default for DetMap<K, V> {
    fn default() -> Self {
        DetMap::new()
    }
}

impl<K: Clone, V: Clone> Clone for DetMap<K, V> {
    fn clone(&self) -> Self {
        DetMap {
            table: self.table.clone(),
            state: self.state,
        }
    }
}

impl<K: Hash + Eq, V: PartialEq> PartialEq for DetMap<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}

impl<K: Hash + Eq, V: Eq> Eq for DetMap<K, V> {}

/// Prints the entries sorted by key, so the output never depends on hashing.
impl<K: fmt::Debug + Ord, V: fmt::Debug> fmt::Debug for DetMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.sorted()).finish()
    }
}

/// Serializes the entries sorted by key, so the output never depends on hashing.
impl<K: Serialize + Ord, V: Serialize> Serialize for DetMap<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (k, v) in self.sorted() {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

/// Rejects duplicate keys instead of keeping the last one.
impl<'de, K: Deserialize<'de> + Hash + Eq, V: Deserialize<'de>> Deserialize<'de> for DetMap<K, V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MapVisitor<K, V>(PhantomData<(K, V)>);
        impl<'de, K: Deserialize<'de> + Hash + Eq, V: Deserialize<'de>> Visitor<'de> for MapVisitor<K, V> {
            type Value = DetMap<K, V>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map without duplicate keys")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let mut map = DetMap::with_capacity(access.size_hint().unwrap_or(0).min(4096));
                while let Some((k, v)) = access.next_entry()? {
                    if map.insert(k, v).is_some() {
                        return Err(serde::de::Error::custom("duplicate key in DetMap"));
                    }
                }
                Ok(map)
            }
        }
        deserializer.deserialize_map(MapVisitor(PhantomData))
    }
}

// ---------------------------------------------------------------------------------------------------------------
// DetSet
// ---------------------------------------------------------------------------------------------------------------

/// A hash set with a deterministic, explicitly keyed hasher. See the [module docs](self) for the iteration rule.
pub struct DetSet<T> {
    table: HashTable<T>,
    state: DetState,
}

impl<T> DetSet<T> {
    /// An empty set with the fixed key.
    pub const fn new() -> Self {
        DetSet {
            table: HashTable::new(),
            state: DetState::fixed(),
        }
    }

    /// An empty set with the given key.
    pub const fn with_state(state: DetState) -> Self {
        DetSet {
            table: HashTable::new(),
            state,
        }
    }

    /// An empty set with the fixed key and room for `capacity` elements.
    pub fn with_capacity(capacity: usize) -> Self {
        DetSet::with_capacity_and_state(capacity, DetState::fixed())
    }

    /// An empty set with the given key and room for `capacity` elements.
    pub fn with_capacity_and_state(capacity: usize, state: DetState) -> Self {
        DetSet {
            table: HashTable::with_capacity(capacity),
            state,
        }
    }

    /// The hasher key.
    pub fn state(&self) -> DetState {
        self.state
    }

    /// How many elements the set holds without reallocating.
    pub fn capacity(&self) -> usize {
        self.table.capacity()
    }

    /// Removes every element, yielding them in an unspecified order that must never be observable. The allocation
    /// is kept.
    pub fn drain(&mut self) -> SetDrain<'_, T> {
        SetDrain(self.table.drain())
    }

    /// The number of elements.
    pub fn len(&self) -> usize {
        self.table.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// Removes every element, keeping the allocation.
    pub fn clear(&mut self) {
        self.table.clear();
    }

    /// Iterates in an unspecified order that must never be observable (see the [module docs](self)).
    pub fn iter(&self) -> SetIter<'_, T> {
        SetIter(self.table.iter())
    }

    /// Keeps only the elements for which `keep` returns true.
    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        self.table.retain(|t| keep(t));
    }

    /// The elements in ascending order: the canonical order for anything observable.
    pub fn sorted(&self) -> Vec<&T>
    where
        T: Ord,
    {
        let mut out: Vec<&T> = self.iter().collect();
        out.sort_unstable();
        out
    }

    /// The elements in ascending order, by value.
    pub fn into_sorted_vec(self) -> Vec<T>
    where
        T: Ord,
    {
        let mut out: Vec<T> = self.table.into_iter().collect();
        out.sort_unstable();
        out
    }
}

impl<T: Hash + Eq> DetSet<T> {
    /// Reserves room for `additional` more elements.
    pub fn reserve(&mut self, additional: usize) {
        let state = self.state;
        self.table.reserve(additional, |t| state.hash_one(t));
    }

    /// Adds an element; returns whether it was new.
    pub fn insert(&mut self, value: T) -> bool {
        let state = self.state;
        let hash = state.hash_one(&value);
        match self.table.entry(hash, |t| *t == value, |t| state.hash_one(t)) {
            hash_table::Entry::Occupied(_) => false,
            hash_table::Entry::Vacant(e) => {
                e.insert(value);
                true
            }
        }
    }

    /// Whether the set contains `value`.
    pub fn contains<Q>(&self, value: &Q) -> bool
    where
        T: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.get(value).is_some()
    }

    /// The stored element equal to `value`.
    pub fn get<Q>(&self, value: &Q) -> Option<&T>
    where
        T: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.state.hash_one(value);
        self.table.find(hash, |t| t.borrow() == value)
    }

    /// Removes `value`; returns whether it was present.
    pub fn remove<Q>(&mut self, value: &Q) -> bool
    where
        T: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.take(value).is_some()
    }

    /// Removes and returns the stored element equal to `value`.
    pub fn take<Q>(&mut self, value: &Q) -> Option<T>
    where
        T: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.state.hash_one(value);
        match self.table.find_entry(hash, |t| t.borrow() == value) {
            Ok(entry) => Some(entry.remove().0),
            Err(_) => None,
        }
    }
}

/// Iterator over a [`DetSet`]; its order must never be observable.
pub struct SetIter<'a, T>(hash_table::Iter<'a, T>);

impl<'a, T> Iterator for SetIter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<T> ExactSizeIterator for SetIter<'_, T> {}
impl<T> FusedIterator for SetIter<'_, T> {}

impl<T> Clone for SetIter<'_, T> {
    fn clone(&self) -> Self {
        SetIter(self.0.clone())
    }
}

/// Owning iterator over a [`DetSet`]; its order must never be observable.
pub struct SetIntoIter<T>(hash_table::IntoIter<T>);

impl<T> Iterator for SetIntoIter<T> {
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<T> ExactSizeIterator for SetIntoIter<T> {}
impl<T> FusedIterator for SetIntoIter<T> {}

/// Draining iterator over a [`DetSet`]; its order must never be observable.
pub struct SetDrain<'a, T>(hash_table::Drain<'a, T>);

impl<T> Iterator for SetDrain<'_, T> {
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<T> ExactSizeIterator for SetDrain<'_, T> {}
impl<T> FusedIterator for SetDrain<'_, T> {}

impl<'a, T> IntoIterator for &'a DetSet<T> {
    type Item = &'a T;
    type IntoIter = SetIter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T> IntoIterator for DetSet<T> {
    type Item = T;
    type IntoIter = SetIntoIter<T>;
    /// Consumes the set in an unspecified order that must never be observable.
    fn into_iter(self) -> Self::IntoIter {
        SetIntoIter(self.table.into_iter())
    }
}

impl<T: Hash + Eq> Extend<T> for DetSet<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for t in iter {
            self.insert(t);
        }
    }
}

impl<T: Hash + Eq> FromIterator<T> for DetSet<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut set = DetSet::new();
        set.extend(iter);
        set
    }
}

impl<T> Default for DetSet<T> {
    fn default() -> Self {
        DetSet::new()
    }
}

impl<T: Clone> Clone for DetSet<T> {
    fn clone(&self) -> Self {
        DetSet {
            table: self.table.clone(),
            state: self.state,
        }
    }
}

impl<T: Hash + Eq> PartialEq for DetSet<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().all(|t| other.contains(t))
    }
}

impl<T: Hash + Eq> Eq for DetSet<T> {}

/// Prints the elements in ascending order, so the output never depends on hashing.
impl<T: fmt::Debug + Ord> fmt::Debug for DetSet<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.sorted()).finish()
    }
}

/// Serializes the elements in ascending order, so the output never depends on hashing.
impl<T: Serialize + Ord> Serialize for DetSet<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.len()))?;
        for t in self.sorted() {
            seq.serialize_element(t)?;
        }
        seq.end()
    }
}

/// Rejects duplicate elements instead of silently merging them.
impl<'de, T: Deserialize<'de> + Hash + Eq> Deserialize<'de> for DetSet<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SetVisitor<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de> + Hash + Eq> Visitor<'de> for SetVisitor<T> {
            type Value = DetSet<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a sequence without duplicate elements")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut set = DetSet::with_capacity(seq.size_hint().unwrap_or(0).min(4096));
                while let Some(t) = seq.next_element()? {
                    if !set.insert(t) {
                        return Err(serde::de::Error::custom("duplicate element in DetSet"));
                    }
                }
                Ok(set)
            }
        }
        deserializer.deserialize_seq(SetVisitor(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn build(keys: &[u64], state: DetState) -> DetMap<u64, u64> {
        let mut map = DetMap::with_state(state);
        for &k in keys {
            *map.entry(k).or_default() += 1;
        }
        map
    }

    #[test]
    fn det_map_deterministic() {
        let keys: Vec<u64> = (0..2000u64)
            .map(|i| i.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 7)
            .collect();
        // Same key material and insertion history: the same (unobservable) iteration order, in every process.
        let a = build(&keys, DetState::fixed());
        let b = build(&keys, DetState::fixed());
        assert_eq!(a.iter().collect::<Vec<_>>(), b.iter().collect::<Vec<_>>());
        // Hashing does not depend on anything ambient.
        assert_eq!(
            DetState::fixed().hash_one(12345u64),
            DetState::fixed().hash_one(12345u64)
        );
        assert_eq!(
            DetState::from_nonce(7).hash_one("abc"),
            DetState::from_nonce(7).hash_one("abc")
        );
        assert_ne!(
            DetState::from_nonce(7).hash_one("abc"),
            DetState::from_nonce(8).hash_one("abc")
        );
        // A different insertion order or key gives the same contents and the same canonical (sorted) view.
        let mut reversed = keys.clone();
        reversed.reverse();
        let c = build(&reversed, DetState::from_nonce(99));
        assert_eq!(a, c);
        assert_eq!(a.sorted(), c.sorted());
        assert_eq!(format!("{a:?}"), format!("{c:?}"));
        assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&c).unwrap());
    }

    #[test]
    fn det_map_platform_independent_hash() {
        // Pinned values: the hasher's output is part of no observable result, but a fixed key must mean the same
        // hash on every platform (little-endian byte feeding, usize hashed as u64).
        let fixed = DetState::fixed();
        let h1 = fixed.hash_one(0u64);
        let h2 = fixed.hash_one(usize::MAX as u64);
        assert_eq!(fixed.hash_one(0usize), h1);
        assert_eq!(fixed.hash_one(usize::MAX), h2);
        let mut bytes_hasher = fixed.build_hasher();
        bytes_hasher.write(&[1, 2, 3]);
        let mut padded = fixed.build_hasher();
        padded.write(&[1, 2, 3, 0]);
        assert_ne!(
            bytes_hasher.finish(),
            padded.finish(),
            "length must separate trailing zeros"
        );
    }

    #[test]
    fn det_map_basic_operations() {
        let mut m: DetMap<String, u32> = DetMap::new();
        assert!(m.is_empty());
        assert_eq!(m.insert("a".into(), 1), None);
        assert_eq!(m.insert("a".into(), 2), Some(1));
        m.insert("b".into(), 3);
        assert_eq!(m.get("a"), Some(&2));
        assert!(m.contains_key("b") && !m.contains_key("c"));
        *m.get_mut("b").unwrap() += 10;
        assert_eq!(m.get_key_value("b"), Some((&"b".to_string(), &13)));
        m.entry("a".into()).and_modify(|v| *v *= 100).or_insert(0);
        assert_eq!(m.get("a"), Some(&200));
        assert_eq!(*m.entry("z".into()).or_insert_with(|| 26), 26);
        match m.entry("z".into()) {
            Entry::Occupied(e) => assert_eq!(e.remove(), 26),
            Entry::Vacant(_) => panic!("z was inserted"),
        }
        assert_eq!(m.remove("a"), Some(200));
        assert_eq!(m.remove_entry("b"), Some(("b".into(), 13)));
        assert!(m.is_empty());
        let m: DetMap<u8, u8> = [(3, 30), (1, 10), (2, 20)].into_iter().collect();
        assert_eq!(m.into_sorted_vec(), vec![(1, 10), (2, 20), (3, 30)]);
    }

    #[test]
    fn det_iterators_cover_every_entry() {
        let m: DetMap<u32, u32> = (0..50).map(|k| (k, k * 2)).collect();
        let it = m.iter();
        assert_eq!(it.len(), 50);
        let mut keys: Vec<u32> = it.clone().map(|(k, _)| *k).collect();
        keys.sort_unstable();
        assert_eq!(keys, (0..50).collect::<Vec<_>>());
        let mut bumped = m.clone();
        for (_, v) in bumped.iter_mut() {
            *v += 1;
        }
        assert_eq!(bumped.get(&3), Some(&7));
        let owned = m.into_iter();
        assert_eq!(owned.len(), 50);
        let mut pairs: Vec<(u32, u32)> = owned.collect();
        pairs.sort_unstable();
        assert_eq!(pairs, (0..50).map(|k| (k, k * 2)).collect::<Vec<_>>());
        let s: DetSet<u32> = (0..20).collect();
        assert_eq!(s.iter().len(), 20);
        assert_eq!((&s).into_iter().count(), 20);
        let mut elems: Vec<u32> = s.into_iter().collect();
        elems.sort_unstable();
        assert_eq!(elems, (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn det_drain_and_capacity() {
        let state = DetState::from_nonce(5);
        let mut m: DetMap<u8, u8> = DetMap::with_capacity_and_state(16, state);
        assert!(m.capacity() >= 16);
        assert_eq!(m.state(), state);
        m.extend([(1, 10), (2, 20)]);
        let cap = m.capacity();
        let mut drained: Vec<(u8, u8)> = m.drain().collect();
        drained.sort_unstable();
        assert_eq!(drained, vec![(1, 10), (2, 20)]);
        assert!(m.is_empty() && m.capacity() == cap);
        let mut s: DetSet<u8> = DetSet::with_capacity_and_state(4, state);
        s.reserve(32);
        assert!(s.capacity() >= 32);
        s.extend([3, 1]);
        let mut out: Vec<u8> = s.drain().collect();
        out.sort_unstable();
        assert_eq!(out, vec![1, 3]);
        assert!(s.is_empty());
    }

    #[test]
    fn det_map_serde_rejects_duplicate_keys() {
        let json = r#"{"a":1,"b":2}"#;
        let m: DetMap<String, u8> = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_string(&m).unwrap(), json);
        assert!(serde_json::from_str::<DetMap<String, u8>>(r#"{"a":1,"a":2}"#).is_err());
    }

    #[test]
    fn det_set_basic_operations() {
        let mut s: DetSet<&str> = DetSet::with_state(DetState::from_nonce(1));
        assert!(s.insert("x"));
        assert!(!s.insert("x"));
        assert!(s.insert("y"));
        assert!(s.contains("x"));
        assert_eq!(s.get("y"), Some(&"y"));
        assert_eq!(s.sorted(), vec![&"x", &"y"]);
        assert_eq!(format!("{s:?}"), r#"{"x", "y"}"#);
        assert!(s.remove("x"));
        assert!(!s.remove("x"));
        assert_eq!(s.take("y"), Some("y"));
        assert!(s.is_empty());
        let json = serde_json::to_string(&[3u8, 1, 2].into_iter().collect::<DetSet<u8>>()).unwrap();
        assert_eq!(json, "[1,2,3]");
        assert!(serde_json::from_str::<DetSet<u8>>("[1,1]").is_err());
    }

    proptest! {
        #[test]
        fn det_map_matches_btree_model(ops in proptest::collection::vec((0u8..3, 0u16..64, any::<u32>()), 0..300)) {
            let mut map: DetMap<u16, u32> = DetMap::new();
            let mut model = std::collections::BTreeMap::new();
            for (op, k, v) in ops {
                match op {
                    0 => prop_assert_eq!(map.insert(k, v), model.insert(k, v)),
                    1 => prop_assert_eq!(map.remove(&k), model.remove(&k)),
                    _ => prop_assert_eq!(map.get(&k), model.get(&k)),
                }
            }
            prop_assert_eq!(map.len(), model.len());
            prop_assert_eq!(map.into_sorted_vec(), model.into_iter().collect::<Vec<_>>());
        }
    }
}
