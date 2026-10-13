//! Immutable byte values with shared hash material for large payloads.
//!
//! Clones share both the bytes and their cached digest. Small values retain their single `Arc` allocation. The
//! digest is only an accelerator for hashed collections: equality still checks bytes on a digest collision,
//! canonical order remains lexicographic, and wire/serde encodings contain only the original bytes.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

const HASH_CACHE_MIN: usize = 128;

#[derive(Clone)]
enum Repr {
    Small(Arc<[u8]>),
    Large(Arc<Cached>),
}

struct Cached {
    bytes: Arc<[u8]>,
    digest: [u8; 32],
}

/// Immutable, cheaply cloned bytes. Large payloads are hashed once per shared allocation, rather than once per
/// relation insertion, removal or lookup. A keyed collection's hasher mixes the cached BLAKE3 digest.
///
/// The hash representation differs from `[u8]`, so this type deliberately does not implement `Borrow<[u8]>`.
#[derive(Clone)]
pub struct BytesValue(Repr);

impl BytesValue {
    /// The shared byte allocation, without copying its contents.
    pub fn shared(&self) -> &Arc<[u8]> {
        match &self.0 {
            Repr::Small(bytes) => bytes,
            Repr::Large(cached) => &cached.bytes,
        }
    }

    /// The content address of these bytes, reusing a large payload's digest.
    pub fn blob_ref(&self) -> crate::BlobRef {
        crate::BlobRef {
            hash: match &self.0 {
                Repr::Small(bytes) => *blake3::hash(bytes).as_bytes(),
                Repr::Large(cached) => cached.digest,
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
            let digest = *blake3::hash(&bytes).as_bytes();
            Self(Repr::Large(Arc::new(Cached { bytes, digest })))
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
            Repr::Large(cached) => cached.bytes.clone(),
        }
    }
}

impl Deref for BytesValue {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.shared()
    }
}

impl AsRef<[u8]> for BytesValue {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl fmt::Debug for BytesValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.shared().fmt(f)
    }
}

impl PartialEq for BytesValue {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(self.shared(), other.shared()) {
            return true;
        }
        if let (Repr::Large(a), Repr::Large(b)) = (&self.0, &other.0)
            && a.digest != b.digest
        {
            return false;
        }
        self.as_ref() == other.as_ref()
    }
}

impl Eq for BytesValue {}

impl Ord for BytesValue {
    fn cmp(&self, other: &Self) -> Ordering {
        if Arc::ptr_eq(self.shared(), other.shared()) {
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
                cached.bytes.len().hash(state);
                state.write(&cached.digest);
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
                digest: [0; 32],
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

    proptest! {
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
