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

#[cfg(test)]
mod tests {
    use super::*;

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
