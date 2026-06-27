//! Cached/uncached partitioning — what an incremental rebuild can reuse vs must recompute.
//!
//! (This is the responsibility that moved out of `source` into the cache crate, ADR-04: a single
//! cache truth.)

use crate::cas::ContentStore;
use crate::key::CacheKey;

/// Splits `items` into `(cached, uncached)` by whether `key_of(item)` is already present in `store`.
///
/// Read-only against the store. The `uncached` set is exactly what an incremental rebuild must
/// (re)compute; the `cached` set is reused.
pub fn partition<'a, T, S, K>(store: &S, items: &'a [T], key_of: K) -> (Vec<&'a T>, Vec<&'a T>)
where
    S: ContentStore,
    K: Fn(&T) -> CacheKey,
{
    let mut cached = Vec::new();
    let mut uncached = Vec::new();
    for item in items {
        if store.contains(&key_of(item)) {
            cached.push(item);
        } else {
            uncached.push(item);
        }
    }
    (cached, uncached)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cas::MemStore;

    fn key_of(item: &&str) -> CacheKey {
        CacheKey::derive("f", &[item.as_bytes()], b"")
    }

    #[test]
    fn all_uncached_when_store_empty() {
        let store = MemStore::unbounded();
        let items = ["a", "b", "c"];
        let (cached, uncached) = partition(&store, &items, key_of);
        assert!(cached.is_empty());
        assert_eq!(uncached.len(), 3);
    }

    #[test]
    fn splits_by_presence() {
        let mut store = MemStore::unbounded();
        store.put(CacheKey::derive("f", &[b"b"], b""), b"x".to_vec());
        let items = ["a", "b", "c"];
        let (cached, uncached) = partition(&store, &items, key_of);
        assert_eq!(cached, vec![&"b"]);
        assert_eq!(uncached, vec![&"a", &"c"]);
    }

    #[test]
    fn all_cached_when_present() {
        let mut store = MemStore::unbounded();
        for s in ["a", "b"] {
            store.put(CacheKey::derive("f", &[s.as_bytes()], b""), b"v".to_vec());
        }
        let items = ["a", "b"];
        let (cached, uncached) = partition(&store, &items, key_of);
        assert_eq!(cached.len(), 2);
        assert!(uncached.is_empty());
    }

    #[test]
    fn empty_items_yields_two_empty_sets() {
        let store = MemStore::unbounded();
        let items: [&str; 0] = [];
        let (cached, uncached) = partition(&store, &items, key_of);
        assert!(cached.is_empty());
        assert!(uncached.is_empty());
    }

    #[test]
    fn preserves_order_within_each_partition() {
        let mut store = MemStore::unbounded();
        store.put(CacheKey::derive("f", &[b"b"], b""), b"v".to_vec());
        let items = ["a", "b", "c", "d"];
        let (_, uncached) = partition(&store, &items, key_of);
        assert_eq!(uncached, vec![&"a", &"c", &"d"]);
    }
}
