//! Stable, interned identifiers for graph entities (interface contract §1).
//!
//! Each id is a `u32` newtype: cheap, `Copy`, totally ordered (so collections sort deterministically,
//! satisfying the R4 minimal-diff invariant), and serde-stable.

use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(u32);

        impl $name {
            #[doc = concat!("Creates a new `", stringify!($name), "` from a raw value.")]
            #[must_use]
            pub const fn new(value: u32) -> Self {
                Self(value)
            }

            /// Returns the underlying numeric value.
            #[must_use]
            pub const fn get(self) -> u32 {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}{}", $prefix, self.0)
            }
        }

        impl From<u32> for $name {
            fn from(value: u32) -> Self {
                Self(value)
            }
        }
    };
}

id_type!(
    /// A stable identifier for a graph node.
    NodeId,
    "n"
);
id_type!(
    /// A stable identifier for a graph edge.
    EdgeId,
    "e"
);
id_type!(
    /// A stable identifier for a detected community.
    CommunityId,
    "c"
);

/// Derives a content-addressed `u32` id from a node label (the first 4 bytes of `blake3(label)`,
/// little-endian).
///
/// Content-addressing makes a node's id a pure function of its label, so adding or removing one
/// symbol does **not** renumber the others — `graph.json` diffs stay minimal (R4) and the 3-way
/// merge driver stays stable across rebuilds. Renaming the label changes this id. Public exporters
/// preserve the original id when redacting a label, which retains topology but lets a reader test
/// low-entropy label guesses against the published id; redaction is not anonymization. Two distinct
/// labels can (astronomically rarely, for in-scope graph sizes) collide in `u32`; the assembler
/// resolves a collision by probing forward deterministically, so the function itself need not be
/// injective.
#[must_use]
pub fn content_id(label: &str) -> u32 {
    let hash = blake3::hash(label.as_bytes());
    let b = hash.as_bytes();
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_id_is_deterministic() {
        assert_eq!(content_id("HttpClient"), content_id("HttpClient"));
    }

    #[test]
    fn content_id_distinguishes_labels() {
        // Different labels almost never collide; these two do not.
        assert_ne!(content_id("alpha"), content_id("beta"));
    }

    #[test]
    fn content_id_is_label_order_independent() {
        // The id depends only on the label, not on when it was first seen — the FO-4 property.
        let a = content_id("zzz_last");
        let b = content_id("aaa_first");
        assert_eq!(a, content_id("zzz_last"));
        assert_eq!(b, content_id("aaa_first"));
    }

    #[test]
    fn content_id_empty_label_is_stable() {
        assert_eq!(content_id(""), content_id(""));
    }

    #[test]
    fn new_and_get_roundtrip() {
        assert_eq!(NodeId::new(7).get(), 7);
        assert_eq!(EdgeId::new(0).get(), 0);
        assert_eq!(CommunityId::new(u32::MAX).get(), u32::MAX);
    }

    #[test]
    fn from_u32_matches_new() {
        assert_eq!(NodeId::from(3), NodeId::new(3));
    }

    #[test]
    fn display_has_prefix() {
        assert_eq!(NodeId::new(12).to_string(), "n12");
        assert_eq!(EdgeId::new(5).to_string(), "e5");
        assert_eq!(CommunityId::new(9).to_string(), "c9");
    }

    #[test]
    fn ordering_is_numeric() {
        let mut v = vec![NodeId::new(3), NodeId::new(1), NodeId::new(2)];
        v.sort_unstable();
        assert_eq!(v, vec![NodeId::new(1), NodeId::new(2), NodeId::new(3)]);
    }

    #[test]
    fn serde_is_transparent_integer() {
        // R2: ids serialize as bare integers, not wrapped objects.
        assert_eq!(serde_json::to_string(&NodeId::new(42)).unwrap(), "42");
        let back: NodeId = serde_json::from_str("42").unwrap();
        assert_eq!(back, NodeId::new(42));
    }

    #[test]
    fn distinct_id_types_do_not_unify() {
        // Compile-time guarantee that NodeId and EdgeId are not interchangeable is implicit;
        // here we assert their Display prefixes differ so receipts stay unambiguous.
        assert_ne!(NodeId::new(1).to_string(), EdgeId::new(1).to_string());
    }

    #[test]
    fn copy_semantics() {
        let a = NodeId::new(1);
        let b = a; // Copy
        assert_eq!(a, b);
    }

    #[test]
    fn hashable_in_set() {
        use std::collections::HashSet;
        let mut s = HashSet::new();
        s.insert(NodeId::new(1));
        s.insert(NodeId::new(1));
        s.insert(NodeId::new(2));
        assert_eq!(s.len(), 2);
    }
}
