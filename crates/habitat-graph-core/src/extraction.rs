//! Pre-interning extraction output (interface contract §2).
//!
//! A language extractor yields `{nodes, edges}` for one file using **labels** as identities; the
//! build layer interns those labels into [`NodeId`](crate::NodeId)s and produces the final
//! [`Graph`](crate::Graph). Keeping this type in `core` (vocabulary) lets both the extract crate and
//! the backend crate depend on it without a cycle.

use crate::{Confidence, Span};

/// A node before id-interning — identified by `label` within `source_file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawNode {
    /// Human-readable label (also the pre-interning identity within a file).
    pub label: String,
    /// Normalized source path the node was extracted from.
    pub source_file: String,
    /// Location within `source_file`.
    pub span: Span,
}

/// An edge before id-interning — references its endpoints by label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEdge {
    /// Source node label.
    pub source: String,
    /// Target node label.
    pub target: String,
    /// Relationship kind (e.g. `"calls"`, `"defines"`).
    pub relation: String,
    /// How the relationship was derived.
    pub confidence: Confidence,
}

/// The complete `{nodes, edges}` produced by extracting a single file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extraction {
    /// Extracted nodes.
    pub nodes: Vec<RawNode>,
    /// Extracted edges.
    pub edges: Vec<RawEdge>,
}

impl Extraction {
    /// Creates an empty extraction.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if neither a node nor an edge was extracted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.edges.is_empty()
    }

    /// Returns `(node_count, edge_count)`.
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        (self.nodes.len(), self.edges.len())
    }

    /// Appends another extraction's nodes and edges into this one.
    pub fn merge(&mut self, other: Extraction) {
        self.nodes.extend(other.nodes);
        self.edges.extend(other.edges);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(label: &str) -> RawNode {
        RawNode {
            label: label.into(),
            source_file: "a.rs".into(),
            span: Span::new(0, 1, 1, 1),
        }
    }
    fn edge(s: &str, t: &str) -> RawEdge {
        RawEdge {
            source: s.into(),
            target: t.into(),
            relation: "calls".into(),
            confidence: Confidence::Inferred,
        }
    }

    #[test]
    fn new_is_empty() {
        let e = Extraction::new();
        assert!(e.is_empty());
        assert_eq!(e.counts(), (0, 0));
    }

    #[test]
    fn counts_reflect_contents() {
        let mut e = Extraction::new();
        e.nodes.push(node("f"));
        e.edges.push(edge("f", "g"));
        assert_eq!(e.counts(), (1, 1));
        assert!(!e.is_empty());
    }

    #[test]
    fn merge_concatenates() {
        let mut a = Extraction::new();
        a.nodes.push(node("f"));
        let mut b = Extraction::new();
        b.nodes.push(node("g"));
        b.edges.push(edge("f", "g"));
        a.merge(b);
        assert_eq!(a.counts(), (2, 1));
    }

    #[test]
    fn merge_empty_is_noop() {
        let mut a = Extraction::new();
        a.nodes.push(node("f"));
        a.merge(Extraction::new());
        assert_eq!(a.counts(), (1, 0));
    }

    #[test]
    fn raw_node_carries_span_and_file() {
        let n = node("foo");
        assert_eq!(n.label, "foo");
        assert_eq!(n.source_file, "a.rs");
        assert_eq!(n.span.start_line, 1);
    }

    #[test]
    fn raw_edge_carries_confidence() {
        let e = edge("a", "b");
        assert_eq!(e.confidence, Confidence::Inferred);
        assert_eq!(e.relation, "calls");
    }

    #[test]
    fn equality_is_structural() {
        assert_eq!(node("x"), node("x"));
        assert_ne!(node("x"), node("y"));
    }

    #[test]
    fn default_matches_new() {
        assert_eq!(Extraction::default(), Extraction::new());
    }
}
