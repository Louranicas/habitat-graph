//! Duplicate removal for nodes and edges.
//!
//! The algorithm is a single-pass O(n) filter: for each node/edge a key is inserted into a
//! [`HashSet`]; the item is kept only when the insertion returns `true` (first occurrence).
//! Communities and the manifest are never modified.

use std::collections::HashSet;

use habitat_graph_core::{Graph, NodeId};

/// Returns the graph with duplicate nodes (same `id`) and duplicate edges
/// (same `source`/`target`/`relation`) removed — first occurrence wins.
///
/// Order among surviving items is the order of their first appearance in the input.
/// Call [`Graph::sorted`](habitat_graph_core::Graph::sorted) afterwards when canonical
/// (R4) order is required.
///
/// Communities and the manifest are copied through to the output unchanged.
///
/// The operation is O(n) in the total number of nodes and edges, using two [`HashSet`]s
/// to track seen keys.
///
/// # Guarantees
/// - Deterministic: equal inputs produce equal outputs.
/// - No allocation beyond the output `Vec`s and the two transient sets.
/// - `forbid(unsafe_code)` — no unsafe is used.
#[must_use]
pub fn dedup(graph: Graph) -> Graph {
    let mut seen_nodes: HashSet<NodeId> = HashSet::with_capacity(graph.nodes.len());
    // Edge key: (source, target, relation).  NodeId is Copy; relation is cloned once per unique
    // edge and once for each duplicate (the duplicate is then dropped by filter).
    let mut seen_edges: HashSet<(NodeId, NodeId, String)> =
        HashSet::with_capacity(graph.edges.len());

    let nodes = graph
        .nodes
        .into_iter()
        .filter(|n| seen_nodes.insert(n.id))
        .collect();

    let edges = graph
        .edges
        .into_iter()
        .filter(|e| seen_edges.insert((e.source, e.target, e.relation.clone())))
        .collect();

    let mut node_content_ids = graph.node_content_ids;
    node_content_ids.retain(|id, _| seen_nodes.contains(id));

    Graph {
        schema: graph.schema,
        nodes,
        node_content_ids,
        edges,
        communities: graph.communities,
        manifest: graph.manifest,
    }
}

#[cfg(test)]
mod tests {
    use habitat_graph_core::{
        Community, CommunityId, Confidence, Edge, Graph, Node, NodeId, Span, SCHEMA_VERSION,
    };

    use super::dedup;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn node(id: u32, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: "src/a.rs".to_owned(),
            source_location: Span::new(0, 10, 1, 2),
        }
    }

    fn edge(src: u32, tgt: u32, rel: &str, confidence: Confidence) -> Edge {
        Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: rel.to_owned(),
            confidence,
        }
    }

    fn community(id: u32, members: &[u32]) -> Community {
        Community {
            id: CommunityId::new(id),
            label: format!("cluster-{id}"),
            members: members.iter().copied().map(NodeId::new).collect(),
        }
    }

    fn ids(g: &Graph) -> Vec<u32> {
        g.nodes.iter().map(|n| n.id.get()).collect()
    }

    fn edge_tuples(g: &Graph) -> Vec<(u32, u32, &str)> {
        g.edges
            .iter()
            .map(|e| (e.source.get(), e.target.get(), e.relation.as_str()))
            .collect()
    }

    // ── node tests ────────────────────────────────────────────────────────────

    #[test]
    fn empty_graph_returns_empty() {
        let result = dedup(Graph::new());
        assert_eq!(result.counts(), (0, 0, 0));
    }

    #[test]
    fn single_node_passes_through() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "Foo"));
        let result = dedup(g);
        assert_eq!(ids(&result), vec![1]);
    }

    #[test]
    fn duplicate_nodes_collapsed_to_one() {
        let mut g = Graph::new();
        g.nodes.push(node(42, "Alpha"));
        g.nodes.push(node(42, "AlphaDuplicate"));
        let result = dedup(g);
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].id, NodeId::new(42));
    }

    #[test]
    fn first_node_label_wins_on_id_collision() {
        // When two nodes share an id the *first* occurrence must survive intact.
        let mut g = Graph::new();
        g.nodes.push(node(7, "Original"));
        g.nodes.push(node(7, "Clobber"));
        let result = dedup(g);
        assert_eq!(result.nodes[0].label, "Original");
    }

    #[test]
    fn distinct_nodes_all_kept() {
        let mut g = Graph::new();
        for id in [10u32, 20, 30] {
            g.nodes.push(node(id, &id.to_string()));
        }
        let result = dedup(g);
        assert_eq!(result.nodes.len(), 3);
    }

    #[test]
    fn node_insertion_order_preserved() {
        // Nodes with ids 3, 1, 2 must survive in that order (first-seen, not sorted).
        let mut g = Graph::new();
        g.nodes.push(node(3, "c"));
        g.nodes.push(node(1, "a"));
        g.nodes.push(node(2, "b"));
        // Introduce a duplicate between 1 and 2 — order should be 3, 1, 2.
        g.nodes.push(node(1, "a-dup"));
        let result = dedup(g);
        assert_eq!(ids(&result), vec![3, 1, 2]);
    }

    #[test]
    fn many_duplicate_nodes_collapse_to_one() {
        let mut g = Graph::new();
        for _ in 0..50 {
            g.nodes.push(node(99, "repeat"));
        }
        let result = dedup(g);
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].id.get(), 99);
    }

    // ── edge tests ────────────────────────────────────────────────────────────

    #[test]
    fn single_edge_passes_through() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        let result = dedup(g);
        assert_eq!(result.edges.len(), 1);
    }

    #[test]
    fn duplicate_edges_collapsed_to_one() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        g.edges.push(edge(1, 2, "calls", Confidence::Inferred));
        let result = dedup(g);
        assert_eq!(result.edges.len(), 1);
    }

    #[test]
    fn first_edge_confidence_wins_on_collision() {
        // The first edge's confidence must survive, not the duplicate's.
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        g.edges.push(edge(1, 2, "calls", Confidence::Ambiguous));
        let result = dedup(g);
        assert_eq!(result.edges[0].confidence, Confidence::Extracted);
    }

    #[test]
    fn edges_differ_by_relation_both_kept() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        g.edges.push(edge(1, 2, "imports", Confidence::Extracted));
        let result = dedup(g);
        assert_eq!(result.edges.len(), 2);
    }

    #[test]
    fn edges_differ_by_source_both_kept() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 3, "calls", Confidence::Extracted));
        g.edges.push(edge(2, 3, "calls", Confidence::Extracted));
        let result = dedup(g);
        assert_eq!(result.edges.len(), 2);
    }

    #[test]
    fn edges_differ_by_target_both_kept() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        g.edges.push(edge(1, 3, "calls", Confidence::Extracted));
        let result = dedup(g);
        assert_eq!(result.edges.len(), 2);
    }

    #[test]
    fn edge_insertion_order_preserved() {
        let mut g = Graph::new();
        g.edges.push(edge(5, 6, "defines", Confidence::Extracted));
        g.edges.push(edge(1, 2, "calls", Confidence::Inferred));
        g.edges.push(edge(3, 4, "imports", Confidence::Ambiguous));
        // Add a duplicate of the first in position 4 — should not shift others.
        g.edges.push(edge(5, 6, "defines", Confidence::Inferred));
        let result = dedup(g);
        assert_eq!(
            edge_tuples(&result),
            vec![(5, 6, "defines"), (1, 2, "calls"), (3, 4, "imports")]
        );
    }

    #[test]
    fn self_edge_deduplicated_correctly() {
        let mut g = Graph::new();
        g.edges.push(edge(7, 7, "self-ref", Confidence::Ambiguous));
        g.edges.push(edge(7, 7, "self-ref", Confidence::Extracted));
        let result = dedup(g);
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].confidence, Confidence::Ambiguous);
    }

    // ── communities / manifest passthrough ────────────────────────────────────

    #[test]
    fn communities_passed_through_unchanged() {
        let mut g = Graph::new();
        g.communities.push(community(0, &[1, 2, 3]));
        g.communities.push(community(1, &[4, 5]));
        let result = dedup(g);
        assert_eq!(result.communities.len(), 2);
        assert_eq!(result.communities[0].id, CommunityId::new(0));
        assert_eq!(result.communities[0].members.len(), 3);
        assert_eq!(result.communities[1].id, CommunityId::new(1));
    }

    #[test]
    fn manifest_inputs_passed_through_unchanged() {
        use habitat_graph_core::InputRecord;
        let mut g = Graph::new();
        g.manifest.inputs.push(InputRecord {
            path: "src/main.rs".to_owned(),
            content_hash: "deadbeef".to_owned(),
        });
        g.manifest.tool_version = "1.2.3".to_owned();
        let result = dedup(g);
        assert_eq!(result.manifest.inputs.len(), 1);
        assert_eq!(result.manifest.inputs[0].path, "src/main.rs");
        assert_eq!(result.manifest.tool_version, "1.2.3");
    }

    #[test]
    fn schema_string_passed_through_unchanged() {
        let g = Graph::new();
        let result = dedup(g);
        assert_eq!(result.schema, SCHEMA_VERSION);
    }

    // ── combined / mixed scenarios ────────────────────────────────────────────

    #[test]
    fn mixed_duplicates_and_uniques_all_correct() {
        let mut g = Graph::new();
        // Nodes: 1 unique, 2 dup, 3 unique, 2 dup again
        g.nodes.push(node(1, "A"));
        g.nodes.push(node(2, "B"));
        g.nodes.push(node(3, "C"));
        g.nodes.push(node(2, "B2")); // dup
                                     // Edges: (1,2,"x"), dup, (1,3,"x"), (2,3,"y")
        g.edges.push(edge(1, 2, "x", Confidence::Extracted));
        g.edges.push(edge(1, 2, "x", Confidence::Inferred)); // dup
        g.edges.push(edge(1, 3, "x", Confidence::Inferred));
        g.edges.push(edge(2, 3, "y", Confidence::Ambiguous));
        let result = dedup(g);
        assert_eq!(ids(&result), vec![1, 2, 3]);
        assert_eq!(
            edge_tuples(&result),
            vec![(1, 2, "x"), (1, 3, "x"), (2, 3, "y")]
        );
    }

    #[test]
    fn all_nodes_distinct_none_dropped() {
        let mut g = Graph::new();
        for i in 0u32..100 {
            g.nodes.push(node(i, &format!("n{i}")));
        }
        let result = dedup(g);
        assert_eq!(result.nodes.len(), 100);
    }

    #[test]
    fn all_edges_distinct_none_dropped() {
        let mut g = Graph::new();
        for i in 0u32..50 {
            g.edges.push(edge(i, i + 1, "next", Confidence::Extracted));
        }
        let result = dedup(g);
        assert_eq!(result.edges.len(), 50);
    }

    #[test]
    fn dedup_does_not_sort_output() {
        // dedup preserves insertion order; Graph::sorted() is a separate step.
        let mut g = Graph::new();
        g.nodes.push(node(30, "z"));
        g.nodes.push(node(10, "a"));
        g.nodes.push(node(20, "m"));
        let result = dedup(g);
        // Expect first-seen order (30, 10, 20), NOT numeric order.
        assert_eq!(ids(&result), vec![30, 10, 20]);
    }

    #[test]
    fn manifest_generated_at_preserved() {
        let mut g = Graph::new();
        g.manifest.generated_at = Some("2026-06-27T00:00:00Z".to_owned());
        let result = dedup(g);
        assert_eq!(
            result.manifest.generated_at.as_deref(),
            Some("2026-06-27T00:00:00Z")
        );
    }

    #[test]
    fn dedup_is_idempotent() {
        // Applying dedup twice must give the same result as applying it once.
        let mut g = Graph::new();
        g.nodes.push(node(1, "A"));
        g.nodes.push(node(1, "A-dup"));
        g.nodes.push(node(2, "B"));
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        g.edges.push(edge(1, 2, "calls", Confidence::Inferred));
        let once = dedup(g.clone());
        let twice = dedup(once.clone());
        assert_eq!(once.nodes, twice.nodes);
        assert_eq!(once.edges, twice.edges);
    }
}
