//! Height-scoped MVCC key/value store for stateful rules.
//!
//! Every write is tagged with a height, so rollback is a range delete. Pool
//! reserves are the canonical `state = "kv"` use case; everything else stays
//! stateless.

use std::collections::BTreeMap;

/// Height-scoped key/value store.
pub trait Kv: Send + Sync {
    /// Read the newest value for `key` at or below `height`.
    fn get(&self, key: &str, height: u64) -> Option<Vec<u8>>;

    /// Write `key` at `height`.
    fn put(&mut self, key: String, height: u64, value: Vec<u8>);

    /// Delete every write above `height` (reorg rollback).
    fn rollback_above(&mut self, height: u64);

    /// Highest height with any write, if any.
    fn max_height(&self) -> Option<u64>;
}

/// In-memory MVCC map: `(key, height) -> value`.
#[derive(Debug, Default)]
pub struct MemoryKv {
    entries: BTreeMap<(String, u64), Vec<u8>>,
}

impl MemoryKv {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored versions (keys × heights).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no versions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Kv for MemoryKv {
    fn get(&self, key: &str, height: u64) -> Option<Vec<u8>> {
        self.entries
            .range(..=(key.to_owned(), height))
            .rev()
            .find(|((k, _), _)| k == key)
            .map(|(_, v)| v.clone())
    }

    fn put(&mut self, key: String, height: u64, value: Vec<u8>) {
        self.entries.insert((key, height), value);
    }

    fn rollback_above(&mut self, height: u64) {
        self.entries.retain(|(_, h), _| *h <= height);
    }

    fn max_height(&self) -> Option<u64> {
        self.entries.keys().map(|(_, h)| *h).max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn reads_see_newest_write_at_or_below_height() {
        let mut kv = MemoryKv::new();
        kv.put("pool".to_owned(), 10, b"r10".to_vec());
        kv.put("pool".to_owned(), 20, b"r20".to_vec());
        assert_eq!(kv.get("pool", 9), None);
        assert_eq!(kv.get("pool", 10), Some(b"r10".to_vec()));
        assert_eq!(kv.get("pool", 15), Some(b"r10".to_vec()));
        assert_eq!(kv.get("pool", 20), Some(b"r20".to_vec()));
        assert_eq!(kv.get("other", 20), None);
    }

    #[test]
    fn rollback_is_a_range_delete() {
        let mut kv = MemoryKv::new();
        assert!(kv.is_empty());
        assert_eq!(kv.len(), 0);
        kv.put("a".to_owned(), 10, b"1".to_vec());
        kv.put("a".to_owned(), 30, b"2".to_vec());
        kv.put("b".to_owned(), 25, b"3".to_vec());
        assert!(!kv.is_empty());
        assert_eq!(kv.len(), 3);
        kv.rollback_above(20);
        assert_eq!(kv.len(), 1);
        assert_eq!(kv.get("a", 30), Some(b"1".to_vec()));
        assert_eq!(kv.get("b", 30), None);
        assert_eq!(kv.max_height(), Some(10));
    }

    proptest! {
        #[test]
        fn reads_never_see_the_future(
            writes in proptest::collection::vec((0u64..100, 0u64..100), 0..20),
            at in 0u64..100,
        ) {
            let mut kv = MemoryKv::new();
            for (key, height) in &writes {
                kv.put(key.to_string(), *height, vec![*height as u8]);
            }
            // Every visible write is at or below `at`.
            if kv.get("0", at).is_some() {
                prop_assert!(writes.iter().any(|(k, h)| k.to_string() == "0" && *h <= at));
            }
        }
    }
}
