//! `habitat-graph-fixtures` — the parity harness (dev-only).
//!
//! Compares habitat-graph output to graphify's committed node-link goldens by reducing both to
//! **label-keyed sets** (the cross-implementation common denominator, since the two assign different
//! node ids). [`from_golden`] loads a graphify golden; [`from_core`] reduces our
//! [`Graph`](habitat_graph_core::Graph); [`classify`]
//! produces a [`ParityReport`]. Parity is CONTENT-equivalence on the node set + structural relations
//! (`contains`/`method`/`inherits`/`imports_from`); `calls`/`uses` are heuristic and reported separately.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::BTreeSet;

pub mod community_golden;
pub mod diff;
pub mod golden;
pub mod normalize;

pub use community_golden::communities_from_golden;
pub use diff::{classify, ParityReport};
pub use golden::from_golden;
pub use normalize::from_core;

/// A graph reduced to label-keyed sets for cross-implementation comparison.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NormalizedGraph {
    /// The set of node labels.
    pub nodes: BTreeSet<String>,
    /// Edges as `(source_label, target_label, relation)`.
    pub edges: BTreeSet<(String, String, String)>,
}

impl NormalizedGraph {
    /// Edges restricted to the given relation kind.
    #[must_use]
    pub fn edges_with_relation(&self, relation: &str) -> BTreeSet<(String, String, String)> {
        self.edges
            .iter()
            .filter(|(_, _, r)| r == relation)
            .cloned()
            .collect()
    }
}
