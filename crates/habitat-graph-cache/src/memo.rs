//! Demand-driven memoization (interface contract §2/§3).

use crate::cas::ContentStore;
use crate::key::CacheKey;

/// Returns the cached bytes for `key`, or computes them with `compute`, stores, and returns them.
///
/// **Parity-transparency invariant** (interface contract §3, asserted again at the G5 gate): the
/// returned value equals `compute()` whether it came from cache or a fresh computation. A cache hit
/// never recomputes and never differs from a recompute.
pub fn memoize<S, F>(store: &mut S, key: CacheKey, compute: F) -> Vec<u8>
where
    S: ContentStore,
    F: FnOnce() -> Vec<u8>,
{
    if let Some(cached) = store.get(&key) {
        return cached;
    }
    let value = compute();
    store.put(key, value.clone());
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cas::MemStore;

    fn produce() -> Vec<u8> {
        b"OUTPUT".to_vec()
    }

    #[test]
    fn miss_computes_and_caches() {
        let mut s = MemStore::unbounded();
        let k = CacheKey::derive("stage", &[b"in"], b"cfg");
        assert!(!s.contains(&k));
        let v = memoize(&mut s, k, produce);
        assert_eq!(v, produce());
        assert!(s.contains(&k));
    }

    #[test]
    fn hit_does_not_recompute() {
        let mut s = MemStore::unbounded();
        let k = CacheKey::derive("stage", &[b"in"], b"cfg");
        let _ = memoize(&mut s, k, produce); // prime
        let v = memoize(&mut s, k, || unreachable!("cache hit must not recompute"));
        assert_eq!(v, produce());
    }

    #[test]
    fn parity_transparency_hit_equals_recompute() {
        // THE invariant (interface contract §3 / G5): a hit is byte-identical to a recompute.
        let mut s = MemStore::unbounded();
        let k = CacheKey::derive("extract", &[b"fn main(){}"], b"v1");
        let cold = memoize(&mut s, k, produce); // miss → computes
        let warm = memoize(&mut s, k, || unreachable!("hit")); // hit → cached
        assert_eq!(cold, warm);
        assert_eq!(warm, produce());
    }

    #[test]
    fn distinct_keys_are_independent() {
        let mut s = MemStore::unbounded();
        let k1 = CacheKey::derive("s", &[b"a"], b"");
        let k2 = CacheKey::derive("s", &[b"b"], b"");
        assert_eq!(memoize(&mut s, k1, || b"A".to_vec()), b"A");
        assert_eq!(memoize(&mut s, k2, || b"B".to_vec()), b"B");
        assert_eq!(memoize(&mut s, k1, || unreachable!()), b"A");
    }

    #[test]
    fn eviction_forces_recompute_to_same_value() {
        // capacity 1: a second key evicts the first; recomputing yields the SAME value (transparency).
        let mut s = MemStore::new(1);
        let k1 = CacheKey::derive("s", &[b"a"], b"");
        let k2 = CacheKey::derive("s", &[b"b"], b"");
        let first = memoize(&mut s, k1, || b"A".to_vec());
        let _ = memoize(&mut s, k2, || b"B".to_vec()); // evicts k1
        assert!(!s.contains(&k1));
        let recomputed = memoize(&mut s, k1, || b"A".to_vec());
        assert_eq!(first, recomputed);
    }

    #[test]
    fn empty_output_is_cached_not_treated_as_miss() {
        let mut s = MemStore::unbounded();
        let k = CacheKey::derive("s", &[b"x"], b"");
        let v = memoize(&mut s, k, Vec::new); // legitimately-empty output
        assert!(v.is_empty());
        assert!(s.contains(&k), "empty value must still be cached");
        let again = memoize(&mut s, k, || {
            unreachable!("empty result was a real cache entry")
        });
        assert!(again.is_empty());
    }
}
