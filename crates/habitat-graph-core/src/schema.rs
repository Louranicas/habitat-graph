//! The canonical internal graph model (interface contract §1).
//!
//! Serialization is **deterministic**: [`Graph::sorted`] canonicalizes collection order so diffs are
//! minimal and the git merge driver stays conflict-free (R4). Paths are stored as normalized
//! `String`s (forward-slash) for cross-platform wire stability rather than `PathBuf`.
//!
//! This model may contain raw attacker-influenced strings. Its serde representation is used for
//! owner-only incremental state; public `graph.json` is the redacted node-link projection emitted
//! by `habitat-graph-export`, which preserves IDs and topology while replacing screened strings.

use serde::{Deserialize, Serialize};

use crate::{CommunityId, Confidence, NodeId, Span};

/// Schema version embedded in every serialized [`Graph`].
pub const SCHEMA_VERSION: &str = "habitat-graph.graph.v0";

/// A node in the knowledge graph (a symbol, file, concept, …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Stable identifier.
    pub id: NodeId,
    /// Raw human-readable label; public exporters apply deterministic redaction.
    pub label: String,
    /// Raw normalized source path; public exporters apply deterministic redaction.
    pub source_file: String,
    /// Location within `source_file`.
    pub source_location: Span,
}

/// A directed, typed relationship between two nodes, carrying a [`Confidence`] trust-signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    /// Source node.
    pub source: NodeId,
    /// Target node.
    pub target: NodeId,
    /// Raw relationship kind (e.g. `"calls"`, `"imports"`, `"defines"`); public exporters redact it.
    pub relation: String,
    /// How the relationship was derived.
    pub confidence: Confidence,
}

/// A detected community (a Leiden cluster of related nodes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Community {
    /// Stable identifier.
    pub id: CommunityId,
    /// Human-readable label for the cluster.
    pub label: String,
    /// Member node ids.
    pub members: Vec<NodeId>,
}

/// A record of one processed input, for provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRecord {
    /// Normalized input path.
    pub path: String,
    /// Content hash (hex) of the input at extraction time.
    pub content_hash: String,
}

/// Manifest describing how the graph was produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Inputs that contributed to the graph.
    pub inputs: Vec<InputRecord>,
    /// Version of the tool that produced the graph.
    pub tool_version: String,
    /// Optional timestamp; omitted under deterministic/parity mode (the only non-deterministic field).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub generated_at: Option<String>,
}

/// The complete internal knowledge graph, including raw strings and provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Graph {
    /// Schema version tag (see [`SCHEMA_VERSION`]).
    pub schema: String,
    /// All nodes (canonically sorted by id after [`Graph::sorted`]).
    pub nodes: Vec<Node>,
    /// All edges (canonically sorted by `(source, target, relation)`).
    pub edges: Vec<Edge>,
    /// Detected communities (sorted by id).
    pub communities: Vec<Community>,
    /// Provenance manifest.
    pub manifest: Manifest,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            inputs: Vec::new(),
            tool_version: env!("CARGO_PKG_VERSION").to_owned(),
            generated_at: None,
        }
    }
}

impl Default for Graph {
    fn default() -> Self {
        Self {
            schema: SCHEMA_VERSION.to_owned(),
            nodes: Vec::new(),
            edges: Vec::new(),
            communities: Vec::new(),
            manifest: Manifest::default(),
        }
    }
}

impl Graph {
    /// Creates an empty graph with the current schema version.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `(nodes, edges, communities)` counts — the shape reported by `/health`.
    #[must_use]
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.nodes.len(), self.edges.len(), self.communities.len())
    }

    /// Returns the graph with every collection in canonical (deterministic) order.
    ///
    /// Required for minimal diffs and the conflict-free `graph.json` merge driver (R4). Sorting is
    /// total and stable: nodes by id, edges by `(source, target, relation)`, communities by id, and
    /// each community's members ascending.
    #[must_use]
    pub fn sorted(mut self) -> Self {
        self.nodes.sort_by_key(|n| n.id);
        self.edges.sort_by(|a, b| {
            (a.source, a.target, a.relation.as_str()).cmp(&(
                b.source,
                b.target,
                b.relation.as_str(),
            ))
        });
        self.communities.sort_by_key(|c| c.id);
        for community in &mut self.communities {
            community.members.sort_unstable();
        }
        self
    }

    /// Serializes the complete, unredacted internal graph to canonical pretty JSON.
    ///
    /// This representation is suitable for owner-only state, not a public artifact. Use
    /// `habitat_graph_export::to_node_link` at public output boundaries.
    ///
    /// # Errors
    /// Returns [`GraphError::Schema`](crate::GraphError::Schema) if serialization fails.
    pub fn to_json(&self) -> crate::Result<String> {
        serde_json::to_string_pretty(self).map_err(|e| crate::GraphError::Schema(e.to_string()))
    }

    /// Parses the complete internal graph representation from JSON.
    ///
    /// This is not the graphify-compatible node-link parser used for public `graph.json` files.
    ///
    /// # Errors
    /// Returns [`GraphError::Schema`](crate::GraphError::Schema) if the input is not a valid graph.
    pub fn from_json(text: &str) -> crate::Result<Self> {
        serde_json::from_str(text).map_err(|e| crate::GraphError::Schema(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u32, file: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: format!("node{id}"),
            source_file: file.to_owned(),
            source_location: Span::new(0, 1, 1, 1),
        }
    }

    fn edge(s: u32, t: u32, rel: &str, c: Confidence) -> Edge {
        Edge {
            source: NodeId::new(s),
            target: NodeId::new(t),
            relation: rel.to_owned(),
            confidence: c,
        }
    }

    #[test]
    fn new_graph_is_empty_with_schema() {
        let g = Graph::new();
        assert_eq!(g.schema, SCHEMA_VERSION);
        assert_eq!(g.counts(), (0, 0, 0));
    }

    #[test]
    fn counts_reflect_contents() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "a.rs"));
        g.nodes.push(node(2, "a.rs"));
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        assert_eq!(g.counts(), (2, 1, 0));
    }

    #[test]
    fn sorted_orders_nodes_by_id() {
        let mut g = Graph::new();
        g.nodes.push(node(3, "a"));
        g.nodes.push(node(1, "a"));
        g.nodes.push(node(2, "a"));
        let g = g.sorted();
        let ids: Vec<u32> = g.nodes.iter().map(|n| n.id.get()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn sorted_orders_edges_by_tuple() {
        let mut g = Graph::new();
        g.edges.push(edge(2, 1, "calls", Confidence::Extracted));
        g.edges.push(edge(1, 2, "imports", Confidence::Inferred));
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        let g = g.sorted();
        let got: Vec<(u32, u32, &str)> = g
            .edges
            .iter()
            .map(|e| (e.source.get(), e.target.get(), e.relation.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![(1, 2, "calls"), (1, 2, "imports"), (2, 1, "calls")]
        );
    }

    #[test]
    fn sorted_orders_community_members() {
        let mut g = Graph::new();
        g.communities.push(Community {
            id: CommunityId::new(0),
            label: "c".into(),
            members: vec![NodeId::new(3), NodeId::new(1), NodeId::new(2)],
        });
        let g = g.sorted();
        let members: Vec<u32> = g.communities[0].members.iter().map(|n| n.get()).collect();
        assert_eq!(members, vec![1, 2, 3]);
    }

    #[test]
    fn sorted_is_idempotent() {
        let mut g = Graph::new();
        g.nodes.push(node(2, "a"));
        g.nodes.push(node(1, "a"));
        let once = g.sorted();
        let twice = once.clone().sorted();
        assert_eq!(once, twice);
    }

    #[test]
    fn json_roundtrip_preserves_graph() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "a.rs"));
        g.edges.push(edge(1, 1, "self", Confidence::Ambiguous));
        let g = g.sorted();
        let json = g.to_json().unwrap();
        let back = Graph::from_json(&json).unwrap();
        assert_eq!(g, back);
    }

    #[test]
    fn deterministic_serialization_omits_timestamp() {
        let g = Graph::new();
        let json = g.to_json().unwrap();
        assert!(
            !json.contains("generated_at"),
            "deterministic graph must omit generated_at"
        );
    }

    #[test]
    fn timestamp_serializes_when_present() {
        let mut g = Graph::new();
        g.manifest.generated_at = Some("2026-06-27T00:00:00Z".into());
        let json = g.to_json().unwrap();
        assert!(json.contains("generated_at"));
    }

    #[test]
    fn schema_version_present_in_json() {
        let json = Graph::new().to_json().unwrap();
        assert!(json.contains(SCHEMA_VERSION));
    }

    #[test]
    fn from_json_rejects_garbage() {
        assert!(Graph::from_json("{ not json").is_err());
    }

    #[test]
    fn edge_confidence_serializes_as_uppercase() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        let json = g.to_json().unwrap();
        assert!(json.contains("EXTRACTED"));
    }

    #[test]
    fn two_graphs_same_content_serialize_identically() {
        // The core parity-transparency guarantee at the schema level: equal graphs → equal bytes.
        let build = || {
            let mut g = Graph::new();
            g.nodes.push(node(2, "b"));
            g.nodes.push(node(1, "a"));
            g.edges.push(edge(2, 1, "x", Confidence::Inferred));
            g.edges.push(edge(1, 2, "y", Confidence::Extracted));
            g.sorted()
        };
        assert_eq!(build().to_json().unwrap(), build().to_json().unwrap());
    }
}
