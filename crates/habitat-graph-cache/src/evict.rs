//! Retention policy (interface contract §6 / ADR-04 §8.4).
//!
//! The policy is *pluggable*: the OSS core ships [`Lru`]; the habitat crate injects a POVM-weighted
//! policy that retains by Hebbian co-activation weight instead of recency — without the core ever
//! depending on a live habitat substrate.

use std::collections::VecDeque;

use crate::key::CacheKey;

/// Decides which cached entry to evict when the store is over capacity.
pub trait RetentionPolicy {
    /// Records that `key` was read (a hit).
    fn note_access(&mut self, key: &CacheKey);
    /// Records that `key` was inserted.
    fn note_insert(&mut self, key: &CacheKey);
    /// Records that `key` was removed (e.g. evicted).
    fn note_remove(&mut self, key: &CacheKey);
    /// Returns the next eviction candidate, if any.
    #[must_use]
    fn evict_candidate(&self) -> Option<CacheKey>;
}

/// Least-recently-used retention. Front of the queue is the eviction candidate.
#[derive(Debug, Default)]
pub struct Lru {
    order: VecDeque<CacheKey>,
}

impl Lru {
    /// Creates an empty LRU policy.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn touch(&mut self, key: &CacheKey) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(*key);
    }
}

impl RetentionPolicy for Lru {
    fn note_access(&mut self, key: &CacheKey) {
        self.touch(key);
    }

    fn note_insert(&mut self, key: &CacheKey) {
        self.touch(key);
    }

    fn note_remove(&mut self, key: &CacheKey) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
    }

    fn evict_candidate(&self) -> Option<CacheKey> {
        self.order.front().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> CacheKey {
        CacheKey::derive("k", &[&[n]], b"")
    }

    #[test]
    fn empty_policy_has_no_candidate() {
        assert!(Lru::new().evict_candidate().is_none());
    }

    #[test]
    fn first_inserted_is_first_candidate() {
        let mut lru = Lru::new();
        lru.note_insert(&key(1));
        lru.note_insert(&key(2));
        assert_eq!(lru.evict_candidate(), Some(key(1)));
    }

    #[test]
    fn access_promotes_recency() {
        let mut lru = Lru::new();
        lru.note_insert(&key(1));
        lru.note_insert(&key(2));
        lru.note_access(&key(1)); // 1 now most-recent → 2 is the candidate
        assert_eq!(lru.evict_candidate(), Some(key(2)));
    }

    #[test]
    fn remove_drops_from_order() {
        let mut lru = Lru::new();
        lru.note_insert(&key(1));
        lru.note_insert(&key(2));
        lru.note_remove(&key(1));
        assert_eq!(lru.evict_candidate(), Some(key(2)));
    }

    #[test]
    fn reinsert_does_not_duplicate() {
        let mut lru = Lru::new();
        lru.note_insert(&key(1));
        lru.note_insert(&key(1));
        lru.note_remove(&key(1));
        assert!(
            lru.evict_candidate().is_none(),
            "key(1) should be fully gone"
        );
    }

    #[test]
    fn candidate_is_stable_without_mutation() {
        let mut lru = Lru::new();
        lru.note_insert(&key(7));
        assert_eq!(lru.evict_candidate(), lru.evict_candidate());
    }
}
