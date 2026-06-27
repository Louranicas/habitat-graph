//! The content-addressed store (interface contract §3).

use std::collections::HashMap;

use crate::evict::{Lru, RetentionPolicy};
use crate::key::CacheKey;

/// A content-addressed `key → bytes` store.
///
/// `get` takes `&mut self` because a read updates retention order (e.g. LRU recency).
pub trait ContentStore {
    /// Returns the stored bytes for `key`, recording the access.
    fn get(&mut self, key: &CacheKey) -> Option<Vec<u8>>;
    /// Inserts `value` under `key`, evicting if over capacity.
    fn put(&mut self, key: CacheKey, value: Vec<u8>);
    /// Returns `true` if `key` is present (read-only; does not record an access).
    #[must_use]
    fn contains(&self, key: &CacheKey) -> bool;
    /// Number of entries currently held.
    #[must_use]
    fn len(&self) -> usize;
    /// Returns `true` if the store holds no entries.
    #[must_use]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An in-memory content-addressed store with a capacity bound and a pluggable [`RetentionPolicy`].
///
/// `capacity == 0` means unbounded. Defaults to [`Lru`].
#[derive(Debug, Default)]
pub struct MemStore<P: RetentionPolicy = Lru> {
    map: HashMap<CacheKey, Vec<u8>>,
    policy: P,
    capacity: usize,
}

impl MemStore<Lru> {
    /// Creates a bounded LRU store (`capacity == 0` → unbounded).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self::with_policy(capacity, Lru::new())
    }

    /// Creates an unbounded LRU store.
    #[must_use]
    pub fn unbounded() -> Self {
        Self::new(0)
    }
}

impl<P: RetentionPolicy> MemStore<P> {
    /// Creates a store with an explicit retention policy.
    #[must_use]
    pub fn with_policy(capacity: usize, policy: P) -> Self {
        Self {
            map: HashMap::new(),
            policy,
            capacity,
        }
    }

    /// Returns the configured capacity (0 = unbounded).
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }
}

impl<P: RetentionPolicy> ContentStore for MemStore<P> {
    fn get(&mut self, key: &CacheKey) -> Option<Vec<u8>> {
        let hit = self.map.get(key).cloned();
        if hit.is_some() {
            self.policy.note_access(key);
        }
        hit
    }

    fn put(&mut self, key: CacheKey, value: Vec<u8>) {
        self.map.insert(key, value);
        self.policy.note_insert(&key);
        if self.capacity > 0 {
            while self.map.len() > self.capacity {
                let Some(victim) = self.policy.evict_candidate() else {
                    break;
                };
                self.map.remove(&victim);
                self.policy.note_remove(&victim);
            }
        }
    }

    fn contains(&self, key: &CacheKey) -> bool {
        self.map.contains_key(key)
    }

    fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> CacheKey {
        CacheKey::derive("k", &[&[n]], b"")
    }

    #[test]
    fn put_then_get_roundtrips() {
        let mut s = MemStore::unbounded();
        s.put(key(1), b"value".to_vec());
        assert_eq!(s.get(&key(1)), Some(b"value".to_vec()));
    }

    #[test]
    fn missing_key_returns_none() {
        let mut s = MemStore::unbounded();
        assert_eq!(s.get(&key(9)), None);
    }

    #[test]
    fn len_and_is_empty_track_contents() {
        let mut s = MemStore::unbounded();
        assert!(s.is_empty());
        s.put(key(1), b"a".to_vec());
        s.put(key(2), b"b".to_vec());
        assert_eq!(s.len(), 2);
        assert!(!s.is_empty());
    }

    #[test]
    fn contains_does_not_require_mut() {
        let mut s = MemStore::unbounded();
        s.put(key(1), b"a".to_vec());
        assert!(s.contains(&key(1)));
        assert!(!s.contains(&key(2)));
    }

    #[test]
    fn capacity_evicts_lru() {
        let mut s = MemStore::new(2);
        s.put(key(1), b"1".to_vec());
        s.put(key(2), b"2".to_vec());
        s.put(key(3), b"3".to_vec()); // evicts key(1), the LRU
        assert_eq!(s.len(), 2);
        assert!(!s.contains(&key(1)));
        assert!(s.contains(&key(2)));
        assert!(s.contains(&key(3)));
    }

    #[test]
    fn access_protects_from_eviction() {
        let mut s = MemStore::new(2);
        s.put(key(1), b"1".to_vec());
        s.put(key(2), b"2".to_vec());
        let _ = s.get(&key(1)); // key(1) now most-recent
        s.put(key(3), b"3".to_vec()); // evicts key(2) instead
        assert!(s.contains(&key(1)));
        assert!(!s.contains(&key(2)));
    }

    #[test]
    fn unbounded_never_evicts() {
        let mut s = MemStore::unbounded();
        for n in 0..50 {
            s.put(key(n), vec![n]);
        }
        assert_eq!(s.len(), 50);
    }

    #[test]
    fn capacity_reports_configured_bound() {
        assert_eq!(MemStore::new(7).capacity(), 7);
        assert_eq!(MemStore::unbounded().capacity(), 0);
    }

    #[test]
    fn overwrite_same_key_keeps_one_entry() {
        let mut s = MemStore::unbounded();
        s.put(key(1), b"old".to_vec());
        s.put(key(1), b"new".to_vec());
        assert_eq!(s.len(), 1);
        assert_eq!(s.get(&key(1)), Some(b"new".to_vec()));
    }
}
