//! Immutable byte values with shared hash material for large payloads.
//!
//! Clones and large slices share backing bytes; each view hashes lazily, at most once. Small values
//! retain their single `Arc` allocation. The
//! digest is only an accelerator for hashed collections: equality still checks bytes on a digest collision,
//! canonical order remains lexicographic, and wire/serde encodings contain only the original bytes.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::{Deref, Range};
use std::sync::{Arc, OnceLock};

const HASH_CACHE_MIN: usize = 128;

#[derive(Clone)]
enum Repr {
    Small(Arc<[u8]>),
    Large(Arc<Cached>),
}

struct Cached {
    bytes: Arc<[u8]>,
    // Private constructors validate this range against the backing allocation.
    range: Range<usize>,
    digest: OnceLock<[u8; 32]>,
    materialized: OnceLock<Arc<[u8]>>,
}

impl Cached {
    #[allow(clippy::indexing_slicing)] // The private range is validated on construction and never mutated.
    fn view(&self) -> &[u8] {
        &self.bytes[self.range.clone()]
    }
    fn digest(&self) -> &[u8; 32] {
        self.digest.get_or_init(|| *blake3::hash(self.view()).as_bytes())
    }
    fn shared(&self) -> &Arc<[u8]> {
        if self.range == (0..self.bytes.len()) {
            &self.bytes
        } else {
            self.materialized.get_or_init(|| Arc::from(self.view()))
        }
    }
}

/// Immutable, cheaply cloned bytes. Large payloads are hashed once per shared allocation, rather than once per
/// relation insertion, removal or lookup. A keyed collection's hasher mixes the cached BLAKE3 digest.
///
/// The hash representation differs from `[u8]`, so this type deliberately does not implement `Borrow<[u8]>`.
#[derive(Clone)]
pub struct BytesValue(Repr);

impl BytesValue {
    /// An exact shared allocation for these bytes. A partial view materializes only when an owner
    /// requires an `Arc<[u8]>`; ordinary reads, comparisons, hashing and slicing keep the view.
    pub fn shared(&self) -> &Arc<[u8]> {
        match &self.0 {
            Repr::Small(bytes) => bytes,
            Repr::Large(cached) => cached.shared(),
        }
    }

    /// A checked byte slice. Large slices share the backing allocation. Small slices copy at most
    /// 127 bytes so a tiny header does not keep an arbitrarily large request alive.
    pub fn slice(&self, range: Range<usize>) -> Option<Self> {
        let view = self.get(range.clone())?;
        if range == (0..self.len()) {
            return Some(self.clone());
        }
        if view.len() < HASH_CACHE_MIN {
            return Some(view.into());
        }
        let Repr::Large(cached) = &self.0 else {
            return Some(view.into());
        };
        Some(Self(Repr::Large(Arc::new(Cached {
            bytes: cached.bytes.clone(),
            range: cached.range.start + range.start..cached.range.start + range.end,
            digest: OnceLock::new(),
            materialized: OnceLock::new(),
        }))))
    }

    fn same_view(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Repr::Small(a), Repr::Small(b)) => Arc::ptr_eq(a, b),
            (Repr::Large(a), Repr::Large(b)) => {
                Arc::ptr_eq(a, b) || (a.range == b.range && Arc::ptr_eq(&a.bytes, &b.bytes))
            }
            _ => false,
        }
    }

    /// The content address of these bytes, reusing a large payload's digest.
    pub fn blob_ref(&self) -> crate::BlobRef {
        crate::BlobRef {
            hash: match &self.0 {
                Repr::Small(bytes) => *blake3::hash(bytes).as_bytes(),
                Repr::Large(cached) => *cached.digest(),
            },
            len: self.len() as u64,
        }
    }
}

impl From<Arc<[u8]>> for BytesValue {
    fn from(bytes: Arc<[u8]>) -> Self {
        if bytes.len() < HASH_CACHE_MIN {
            Self(Repr::Small(bytes))
        } else {
            let range = 0..bytes.len();
            Self(Repr::Large(Arc::new(Cached {
                bytes,
                range,
                digest: OnceLock::new(),
                materialized: OnceLock::new(),
            })))
        }
    }
}

impl From<Vec<u8>> for BytesValue {
    fn from(bytes: Vec<u8>) -> Self {
        Arc::<[u8]>::from(bytes).into()
    }
}

impl From<&[u8]> for BytesValue {
    fn from(bytes: &[u8]) -> Self {
        Arc::<[u8]>::from(bytes).into()
    }
}

impl FromIterator<u8> for BytesValue {
    fn from_iter<T: IntoIterator<Item = u8>>(iter: T) -> Self {
        iter.into_iter().collect::<Vec<_>>().into()
    }
}

impl From<BytesValue> for Arc<[u8]> {
    fn from(bytes: BytesValue) -> Self {
        match bytes.0 {
            Repr::Small(bytes) => bytes,
            Repr::Large(cached) => cached.shared().clone(),
        }
    }
}

impl Deref for BytesValue {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match &self.0 {
            Repr::Small(bytes) => bytes,
            Repr::Large(cached) => cached.view(),
        }
    }
}

impl AsRef<[u8]> for BytesValue {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl fmt::Debug for BytesValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl PartialEq for BytesValue {
    fn eq(&self, other: &Self) -> bool {
        if self.same_view(other) {
            return true;
        }
        if let (Repr::Large(a), Repr::Large(b)) = (&self.0, &other.0)
            && let (Some(a), Some(b)) = (a.digest.get(), b.digest.get())
            && a != b
        {
            return false;
        }
        self.as_ref() == other.as_ref()
    }
}

impl Eq for BytesValue {}

impl Ord for BytesValue {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.same_view(other) {
            Ordering::Equal
        } else {
            self.as_ref().cmp(other.as_ref())
        }
    }
}

impl PartialOrd for BytesValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for BytesValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.0 {
            Repr::Small(bytes) => bytes.hash(state),
            Repr::Large(cached) => {
                cached.range.len().hash(state);
                state.write(cached.digest());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::hash::BuildHasher;

    use blossom_base::DetState;
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn large_clones_share_the_digest_and_its_lifetime() {
        let bytes = BytesValue::from(vec![7; 1024]);
        let clone = bytes.clone();
        let (Repr::Large(a), Repr::Large(b)) = (&bytes.0, &clone.0) else {
            panic!("large bytes")
        };
        assert!(Arc::ptr_eq(a, b));
        let hash = DetState::from_nonce(5).hash_one(&bytes);
        assert_eq!(hash, DetState::from_nonce(5).hash_one(&clone));
        assert_ne!(hash, DetState::from_nonce(6).hash_one(&clone));
        let weak = Arc::downgrade(a);
        drop(bytes);
        assert!(weak.upgrade().is_some());
        drop(clone);
        assert!(weak.upgrade().is_none(), "the cache must die with its last live value");
    }

    #[test]
    fn a_digest_collision_never_makes_distinct_bytes_equal() {
        let collide = |byte| {
            BytesValue(Repr::Large(Arc::new(Cached {
                bytes: vec![byte; 1024].into(),
                range: 0..1024,
                digest: OnceLock::from([0; 32]),
                materialized: OnceLock::new(),
            })))
        };
        let a = collide(1);
        let b = collide(2);
        assert_eq!(DetState::fixed().hash_one(&a), DetState::fixed().hash_one(&b));
        assert_ne!(a, b);
        assert!(a < b);
        let mut map = blossom_base::DetMap::new();
        map.insert(a.clone(), 1);
        map.insert(b.clone(), 2);
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(&a), Some(&1));
        assert_eq!(map.get(&b), Some(&2));
    }

    #[test]
    fn collection_hash_work_is_bounded_independent_of_payload_size() {
        #[derive(Default)]
        struct Count {
            bytes: usize,
        }
        impl Hasher for Count {
            fn finish(&self) -> u64 {
                0
            }
            fn write(&mut self, bytes: &[u8]) {
                self.bytes += bytes.len();
            }
        }
        for size in [HASH_CACHE_MIN, 1024, 128 * 1024] {
            let value = BytesValue::from(vec![7; size]);
            let mut hasher = Count::default();
            value.hash(&mut hasher);
            assert!(
                hasher.bytes <= 40,
                "hashed {} bytes for a {size}-byte payload",
                hasher.bytes
            );
        }
    }

    #[test]
    fn cached_content_addresses_match_the_blob_hash() {
        for size in [0, HASH_CACHE_MIN - 1, HASH_CACHE_MIN, 1024] {
            let value = BytesValue::from(vec![7; size]);
            assert_eq!(value.blob_ref(), crate::BlobRef::of(&value));
        }
    }

    #[test]
    fn cached_bytes_keep_the_serialized_value_and_typed_fingerprint() {
        let raw = vec![7u8; 1024];
        let value = crate::Value::Bytes(raw.clone().into());
        let fp = crate::fp::fingerprint(&value).unwrap();
        DetState::fixed().hash_one(&value);
        assert_eq!(crate::fp::fingerprint(&value).unwrap(), fp);
        assert_eq!(serde_json::to_value(&value).unwrap(), serde_json::json!({"Bytes": raw}));
        let encoded = postcard::to_allocvec(&value).unwrap();
        // The existing Value serde enum's Bytes tag (5), the slice length (1024), then the unchanged bytes.
        let mut original = vec![5, 0x80, 0x08];
        original.extend_from_slice(&raw);
        assert_eq!(encoded, original);
        assert_eq!(postcard::from_bytes::<crate::Value>(&encoded).unwrap(), value);
        // This variant stays smaller than an existing payload, so it cannot widen every Value/row slot.
        assert!(std::mem::size_of::<BytesValue>() <= std::mem::size_of::<crate::BlobRef>());
    }

    #[test]
    fn hashing_shared_bytes_is_safe_on_multiple_threads() {
        let bytes = BytesValue::from(vec![7; 128 * 1024]);
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let bytes = bytes.clone();
                std::thread::spawn(move || DetState::fixed().hash_one(&bytes))
            })
            .collect();
        let hash = DetState::fixed().hash_one(&bytes);
        for thread in threads {
            assert_eq!(thread.join().unwrap(), hash);
        }
    }

    #[test]
    fn slicing_shares_large_backing_and_hashes_only_on_demand() {
        let raw: Vec<_> = (0..4096).map(|i| (i % 251) as u8).collect();
        let whole = BytesValue::from(raw.clone());
        let view = whole.slice(512..3072).unwrap();
        let nested = view.slice(128..1024).unwrap();
        let (Repr::Large(a), Repr::Large(b), Repr::Large(c)) = (&whole.0, &view.0, &nested.0) else {
            panic!("large slices must share backing")
        };
        assert!(Arc::ptr_eq(&a.bytes, &b.bytes));
        assert!(Arc::ptr_eq(&b.bytes, &c.bytes));
        assert!(a.digest.get().is_none());
        assert!(b.digest.get().is_none());
        assert!(c.digest.get().is_none());
        assert!(c.materialized.get().is_none());
        assert_eq!(nested.as_ref(), &raw[640..1536]);
        let copied = BytesValue::from(&raw[640..1536]);
        assert_eq!(nested, copied);
        assert_eq!(nested.cmp(&copied), Ordering::Equal);
        assert!(c.digest.get().is_none(), "comparison must not force hashing");
        assert_eq!(DetState::fixed().hash_one(&nested), DetState::fixed().hash_one(&copied));
        assert!(c.digest.get().is_some());
        assert!(a.digest.get().is_none());
        assert_eq!(nested.blob_ref(), crate::BlobRef::of(&raw[640..1536]));
        assert_eq!(nested.shared().as_ref(), &raw[640..1536]);
        assert_eq!(Arc::<[u8]>::from(nested.clone()).as_ref(), &raw[640..1536]);
        assert_eq!(
            serde_json::to_value(crate::Value::Bytes(nested)).unwrap(),
            serde_json::to_value(crate::Value::Bytes(copied)).unwrap()
        );
        let tiny = view.slice(10..20).unwrap();
        assert!(matches!(tiny.0, Repr::Small(_)));
        assert_eq!(tiny.as_ref(), &raw[522..532]);
        assert!(view.slice(0..usize::MAX).is_none());
        assert!(view.slice(Range { start: 20, end: 10 }).is_none());
    }

    proptest! {
        #[test]
        fn nested_views_match_owned_bytes(raw in proptest::collection::vec(any::<u8>(), 0..2048),
                                         start in 0usize..2200, end in 0usize..2200) {
            let bytes = BytesValue::from(raw.clone());
            let view = bytes.slice(start..end);
            prop_assert_eq!(view.as_deref(), raw.get(start..end));
            if let Some(view) = view {
                let owned = BytesValue::from(view.as_ref());
                prop_assert_eq!(view.cmp(&owned), Ordering::Equal);
                prop_assert_eq!(DetState::fixed().hash_one(&view), DetState::fixed().hash_one(&owned));
                prop_assert_eq!(view.blob_ref(), owned.blob_ref());
                prop_assert_eq!(view.slice(0..view.len()).unwrap(), owned);
            }
        }
        #[test]
        fn bytes_keep_slice_order_and_equal_hashes(a in proptest::collection::vec(any::<u8>(), 0..1024),
                                                 b in proptest::collection::vec(any::<u8>(), 0..1024)) {
            let av = BytesValue::from(a.clone());
            let bv = BytesValue::from(b.clone());
            prop_assert_eq!(av.cmp(&bv), a.cmp(&b));
            prop_assert_eq!(av == bv, a == b);
            let another = BytesValue::from(a.as_slice());
            prop_assert_eq!(DetState::fixed().hash_one(&av), DetState::fixed().hash_one(&another));
        }
    }
}
