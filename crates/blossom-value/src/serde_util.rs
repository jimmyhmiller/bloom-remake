//! Serde helpers that keep every value format lossless and strict.
//!
//! - `f64_bits`: an `f64` as its IEEE bit pattern, so NaN payloads and `-0.0` survive formats such as JSON;
//! - `set_strict`: a set as a sequence, rejecting duplicate elements instead of merging them;
//! - `map_pairs`: a map as a sequence of `(key, value)` pairs (JSON object keys must be strings), rejecting
//!   duplicate keys instead of keeping the last one;
//! - `nonempty_tuple`: a tuple's elements, rejecting an empty sequence, whose canonical form is `()` (`Unit`).

pub(crate) mod f64_bits {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(v: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(v.to_bits())
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        u64::deserialize(deserializer).map(f64::from_bits)
    }
}

pub(crate) mod set_strict {
    use std::collections::BTreeSet;
    use std::fmt;
    use std::marker::PhantomData;
    use std::sync::Arc;

    use serde::de::{SeqAccess, Visitor};
    use serde::ser::SerializeSeq;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<T: Serialize, S: Serializer>(
        set: &Arc<BTreeSet<T>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(set.len()))?;
        for item in set.iter() {
            seq.serialize_element(item)?;
        }
        seq.end()
    }

    pub(crate) fn deserialize<'de, T, D>(deserializer: D) -> Result<Arc<BTreeSet<T>>, D::Error>
    where
        T: Deserialize<'de> + Ord,
        D: Deserializer<'de>,
    {
        struct SetVisitor<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de> + Ord> Visitor<'de> for SetVisitor<T> {
            type Value = Arc<BTreeSet<T>>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a sequence of distinct set elements")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut set = BTreeSet::new();
                while let Some(item) = seq.next_element()? {
                    if !set.insert(item) {
                        return Err(serde::de::Error::custom("duplicate element in a set"));
                    }
                }
                Ok(Arc::new(set))
            }
        }
        deserializer.deserialize_seq(SetVisitor(PhantomData))
    }
}

pub(crate) mod map_pairs {
    use std::collections::BTreeMap;
    use std::fmt;
    use std::marker::PhantomData;
    use std::sync::Arc;

    use serde::de::{SeqAccess, Visitor};
    use serde::ser::SerializeSeq;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<K, V, S>(map: &Arc<BTreeMap<K, V>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        K: Serialize,
        V: Serialize,
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(map.len()))?;
        for pair in map.iter() {
            seq.serialize_element(&pair)?;
        }
        seq.end()
    }

    pub(crate) fn deserialize<'de, K, V, D>(deserializer: D) -> Result<Arc<BTreeMap<K, V>>, D::Error>
    where
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        struct PairsVisitor<K, V>(PhantomData<(K, V)>);
        impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>> Visitor<'de> for PairsVisitor<K, V> {
            type Value = Arc<BTreeMap<K, V>>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a sequence of (key, value) pairs with distinct keys")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut map = BTreeMap::new();
                while let Some((k, v)) = seq.next_element::<(K, V)>()? {
                    if map.insert(k, v).is_some() {
                        return Err(serde::de::Error::custom("duplicate key in a map"));
                    }
                }
                Ok(Arc::new(map))
            }
        }
        deserializer.deserialize_seq(PairsVisitor(PhantomData))
    }
}

pub(crate) mod nonempty_tuple {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer};

    pub(crate) fn deserialize<'de, T, D>(deserializer: D) -> Result<Arc<[T]>, D::Error>
    where
        T: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        let items = Vec::<T>::deserialize(deserializer)?;
        if items.is_empty() {
            return Err(serde::de::Error::custom(
                "an empty tuple is not canonical: the empty tuple is `()` (Unit)",
            ));
        }
        Ok(items.into())
    }
}
