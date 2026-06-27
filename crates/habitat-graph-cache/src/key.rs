//! Content-addressed cache keys (interface contract §3).
//!
//! `key = blake3( stage_id ‖ Σ length-prefixed inputs ‖ config )`. Length-prefixing inputs and
//! inserting domain separators prevents concatenation ambiguity (so `["ab","c"]` ≠ `["a","bc"]`).

/// A 256-bit content-addressed cache key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct CacheKey([u8; 32]);

impl CacheKey {
    /// Derives a key from a stage id, its inputs, and a config fingerprint.
    ///
    /// Deterministic: identical `(stage, inputs, config)` always yield the same key, and any change
    /// to any component yields a different key with overwhelming probability.
    #[must_use]
    pub fn derive(stage: &str, inputs: &[&[u8]], config: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(stage.as_bytes());
        hasher.update(b"\x1fstage\x1f");
        for input in inputs {
            let len = u64::try_from(input.len()).unwrap_or(u64::MAX);
            hasher.update(&len.to_le_bytes());
            hasher.update(input);
        }
        hasher.update(b"\x1fconfig\x1f");
        hasher.update(config);
        Self(*hasher.finalize().as_bytes())
    }

    /// Returns the raw 32-byte digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Returns the lowercase hex encoding of the key.
    #[must_use]
    pub fn to_hex(&self) -> String {
        blake3::Hash::from(self.0).to_hex().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_inputs_yield_identical_keys() {
        let a = CacheKey::derive("extract", &[b"fn main(){}"], b"v1");
        let b = CacheKey::derive("extract", &[b"fn main(){}"], b"v1");
        assert_eq!(a, b);
    }

    #[test]
    fn different_stage_changes_key() {
        let a = CacheKey::derive("extract", &[b"x"], b"v1");
        let b = CacheKey::derive("build", &[b"x"], b"v1");
        assert_ne!(a, b);
    }

    #[test]
    fn different_input_changes_key() {
        let a = CacheKey::derive("extract", &[b"x"], b"v1");
        let b = CacheKey::derive("extract", &[b"y"], b"v1");
        assert_ne!(a, b);
    }

    #[test]
    fn different_config_changes_key() {
        let a = CacheKey::derive("extract", &[b"x"], b"v1");
        let b = CacheKey::derive("extract", &[b"x"], b"v2");
        assert_ne!(a, b);
    }

    #[test]
    fn input_boundaries_are_unambiguous() {
        // Length-prefixing must distinguish these — a naive concat would collide.
        let a = CacheKey::derive("s", &[b"ab", b"c"], b"");
        let b = CacheKey::derive("s", &[b"a", b"bc"], b"");
        assert_ne!(a, b);
    }

    #[test]
    fn empty_inputs_are_stable() {
        assert_eq!(
            CacheKey::derive("s", &[], b""),
            CacheKey::derive("s", &[], b"")
        );
    }

    #[test]
    fn hex_is_64_chars() {
        let hex = CacheKey::derive("s", &[b"x"], b"").to_hex();
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn as_bytes_matches_hex() {
        use std::fmt::Write as _;
        let key = CacheKey::derive("s", &[b"x"], b"");
        let mut from_bytes = String::new();
        for byte in key.as_bytes() {
            let _ = write!(from_bytes, "{byte:02x}");
        }
        assert_eq!(from_bytes, key.to_hex());
    }

    #[test]
    fn usable_as_map_key() {
        use std::collections::HashMap;
        let mut m = HashMap::new();
        m.insert(CacheKey::derive("s", &[b"x"], b""), 1);
        assert_eq!(m.get(&CacheKey::derive("s", &[b"x"], b"")), Some(&1));
    }
}
