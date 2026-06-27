//! Reduce our [`Graph`](habitat_graph_core::Graph) to a [`NormalizedGraph`](crate::NormalizedGraph).
//!
//! The conversion is label-keyed and endpoint-safe:
//!
//! * Every node contributes its `label` to the node set.
//! * Every edge becomes a `(source_label, target_label, relation)` triple, resolved through a
//!   `NodeId`→label index built in a single pass. Edges whose source or target id has no
//!   corresponding node entry are **silently skipped** so the result is always well-formed.
//! * Both output collections are [`BTreeSet`](std::collections::BTreeSet), which guarantees
//!   deterministic (lexicographically sorted) iteration — required by the R4 minimal-diff
//!   invariant and the parity harness.

use std::collections::HashMap;

use habitat_graph_core::{Graph, NodeId};

use crate::NormalizedGraph;

/// Reduces a habitat-graph [`Graph`] to label-keyed sets suitable for cross-implementation
/// parity comparison.
///
/// The node set contains every distinct `node.label` present in the graph (two nodes that share
/// the same label collapse to one set entry, which is intentional — the Python extractor sets
/// `label` to graphify's qualified node id, e.g. `"exceptions_httperror"`, making labels the
/// authoritative comparison key).
///
/// Each edge is resolved to `(source_label, target_label, relation)`. Edges referencing an
/// unknown `NodeId` on either end are dropped rather than causing an error, since sparse or
/// incremental graphs are a valid state during incremental builds.
///
/// The output is **deterministic**: [`BTreeSet`](std::collections::BTreeSet) imposes a total
/// lexicographic order on both collections independent of insertion order.
#[must_use]
pub fn from_core(graph: &Graph) -> NormalizedGraph {
    // Build NodeId → &str label index in O(n) — we borrow from `graph` so no cloning here.
    let id_to_label: HashMap<NodeId, &str> = graph
        .nodes
        .iter()
        .map(|n| (n.id, n.label.as_str()))
        .collect();

    // Collect every node label into the BTreeSet (duplicates collapse automatically).
    let nodes = graph.nodes.iter().map(|n| n.label.clone()).collect();

    // Map each edge to (src_label, tgt_label, relation), dropping edges with unknown endpoints.
    let edges = graph
        .edges
        .iter()
        .filter_map(|e| {
            let src = *id_to_label.get(&e.source)?;
            let tgt = *id_to_label.get(&e.target)?;
            Some((src.to_owned(), tgt.to_owned(), e.relation.clone()))
        })
        .collect();

    NormalizedGraph { nodes, edges }
}

#[cfg(test)]
mod tests {
    use habitat_graph_core::{
        Confidence, Edge, Graph, Manifest, Node, NodeId, Span, SCHEMA_VERSION,
    };

    use super::from_core;
    use crate::NormalizedGraph;

    // ── helpers ────────────────────────────────────────────────────────────────────

    fn span() -> Span {
        Span::new(0, 1, 1, 1)
    }

    fn make_node(id: u32, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: "test.rs".to_owned(),
            source_location: span(),
        }
    }

    fn make_edge(source: u32, target: u32, relation: &str) -> Edge {
        Edge {
            source: NodeId::new(source),
            target: NodeId::new(target),
            relation: relation.to_owned(),
            confidence: Confidence::Extracted,
        }
    }

    fn make_graph(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        Graph {
            schema: SCHEMA_VERSION.to_owned(),
            nodes,
            edges,
            communities: Vec::new(),
            manifest: Manifest::default(),
        }
    }

    /// Convenience: build a `NormalizedGraph` from string slices for readable assertions.
    fn norm(nodes: &[&str], edges: &[(&str, &str, &str)]) -> NormalizedGraph {
        NormalizedGraph {
            nodes: nodes.iter().map(|s| (*s).to_owned()).collect(),
            edges: edges
                .iter()
                .map(|(s, t, r)| ((*s).to_owned(), (*t).to_owned(), (*r).to_owned()))
                .collect(),
        }
    }

    // ── Test 1: fundamental contract ───────────────────────────────────────────────

    #[test]
    fn basic_two_nodes_one_edge() {
        // Contract from spec: nodes "a","b" + edge a->b rel "contains"
        // → nodes={"a","b"}, edges={("a","b","contains")}.
        let g = make_graph(
            vec![make_node(1, "a"), make_node(2, "b")],
            vec![make_edge(1, 2, "contains")],
        );
        assert_eq!(from_core(&g), norm(&["a", "b"], &[("a", "b", "contains")]));
    }

    // ── Test 2: empty graph ────────────────────────────────────────────────────────

    #[test]
    fn empty_graph_yields_empty_sets() {
        assert_eq!(from_core(&Graph::default()), norm(&[], &[]));
    }

    // ── Test 3: dangling source ────────────────────────────────────────────────────

    #[test]
    fn dangling_source_edge_skipped() {
        // Node id 99 does not exist; the edge must be silently dropped.
        let g = make_graph(vec![make_node(1, "a")], vec![make_edge(99, 1, "calls")]);
        let got = from_core(&g);
        assert_eq!(got.nodes.len(), 1, "node set unaffected by dangling edge");
        assert!(
            got.edges.is_empty(),
            "edge with missing source must be skipped"
        );
    }

    // ── Test 4: dangling target ────────────────────────────────────────────────────

    #[test]
    fn dangling_target_edge_skipped() {
        let g = make_graph(vec![make_node(1, "a")], vec![make_edge(1, 99, "calls")]);
        let got = from_core(&g);
        assert!(
            got.edges.is_empty(),
            "edge with missing target must be skipped"
        );
    }

    // ── Test 5: both endpoints missing ────────────────────────────────────────────

    #[test]
    fn both_endpoints_missing_edge_skipped() {
        let g = make_graph(vec![make_node(1, "x")], vec![make_edge(50, 60, "inherits")]);
        assert!(
            from_core(&g).edges.is_empty(),
            "edge with both endpoints missing must be skipped"
        );
    }

    // ── Test 6: relation preserved verbatim ───────────────────────────────────────

    #[test]
    fn relation_string_preserved_verbatim() {
        let g = make_graph(
            vec![make_node(1, "a"), make_node(2, "b")],
            vec![make_edge(1, 2, "imports_from")],
        );
        let got = from_core(&g);
        let relations: Vec<&str> = got.edges.iter().map(|(_, _, r)| r.as_str()).collect();
        assert_eq!(relations, vec!["imports_from"]);
    }

    // ── Test 7: multiple edges all present ────────────────────────────────────────

    #[test]
    fn multiple_edges_all_present() {
        let g = make_graph(
            vec![make_node(1, "a"), make_node(2, "b"), make_node(3, "c")],
            vec![
                make_edge(1, 2, "calls"),
                make_edge(2, 3, "contains"),
                make_edge(1, 3, "imports_from"),
            ],
        );
        let expected = norm(
            &["a", "b", "c"],
            &[
                ("a", "b", "calls"),
                ("a", "c", "imports_from"),
                ("b", "c", "contains"),
            ],
        );
        assert_eq!(from_core(&g), expected);
    }

    // ── Test 8: duplicate labels collapse ─────────────────────────────────────────

    #[test]
    fn duplicate_labels_collapse_in_node_set() {
        // Two nodes with the same label → only one entry in the BTreeSet.
        let g = make_graph(vec![make_node(1, "x"), make_node(2, "x")], vec![]);
        let got = from_core(&g);
        assert_eq!(got.nodes.len(), 1, "duplicate labels must collapse");
        assert!(got.nodes.contains("x"));
    }

    // ── Test 9: self-loop ─────────────────────────────────────────────────────────

    #[test]
    fn self_loop_edge_preserved() {
        let g = make_graph(vec![make_node(1, "m")], vec![make_edge(1, 1, "recurses")]);
        let got = from_core(&g);
        assert!(got
            .edges
            .contains(&("m".to_owned(), "m".to_owned(), "recurses".to_owned())));
    }

    // ── Test 10: determinism ──────────────────────────────────────────────────────

    #[test]
    fn deterministic_repeated_calls() {
        // Calling from_core twice on the same graph must yield identical results.
        let g = make_graph(
            vec![make_node(3, "c"), make_node(1, "a"), make_node(2, "b")],
            vec![make_edge(3, 1, "calls"), make_edge(1, 2, "contains")],
        );
        assert_eq!(
            from_core(&g),
            from_core(&g),
            "from_core must be deterministic"
        );
    }

    // ── Test 11: nodes only, no edges ─────────────────────────────────────────────

    #[test]
    fn nodes_only_no_edges() {
        let g = make_graph(vec![make_node(1, "alpha"), make_node(2, "beta")], vec![]);
        let got = from_core(&g);
        assert_eq!(got.nodes.len(), 2);
        assert!(got.edges.is_empty());
    }

    // ── Test 12: mixed valid and dangling edges ───────────────────────────────────

    #[test]
    fn mixed_valid_and_dangling_edges() {
        let g = make_graph(
            vec![make_node(1, "a"), make_node(2, "b")],
            vec![
                make_edge(1, 2, "calls"),
                make_edge(1, 99, "ghost_target"),
                make_edge(99, 2, "ghost_source"),
            ],
        );
        let got = from_core(&g);
        assert_eq!(got.edges.len(), 1);
        assert!(got
            .edges
            .contains(&("a".to_owned(), "b".to_owned(), "calls".to_owned())));
    }

    // ── Test 13: parallel edges, distinct relations ───────────────────────────────

    #[test]
    fn parallel_edges_different_relations_both_present() {
        let g = make_graph(
            vec![make_node(1, "a"), make_node(2, "b")],
            vec![make_edge(1, 2, "calls"), make_edge(1, 2, "imports")],
        );
        let got = from_core(&g);
        assert_eq!(
            got.edges.len(),
            2,
            "parallel edges with distinct relations must both appear"
        );
    }

    // ── Test 14: parallel edges, same relation collapse ───────────────────────────

    #[test]
    fn parallel_edges_same_relation_collapse_in_set() {
        // The same (src, tgt, relation) triple → one entry in the BTreeSet.
        let g = make_graph(
            vec![make_node(1, "a"), make_node(2, "b")],
            vec![
                make_edge(1, 2, "calls"),
                make_edge(1, 2, "calls"), // duplicate
            ],
        );
        let got = from_core(&g);
        assert_eq!(got.edges.len(), 1, "identical edges must collapse in set");
    }

    // ── Test 15: node set is sorted ───────────────────────────────────────────────

    #[test]
    fn node_set_is_sorted_lexicographically() {
        // BTreeSet guarantees sorted iteration; insertion order must not matter.
        let g = make_graph(
            vec![
                make_node(3, "zebra"),
                make_node(1, "apple"),
                make_node(2, "mango"),
            ],
            vec![],
        );
        let got = from_core(&g);
        let sorted: Vec<&str> = got.nodes.iter().map(String::as_str).collect();
        assert_eq!(sorted, vec!["apple", "mango", "zebra"]);
    }

    // ── Test 16: edge set is sorted ───────────────────────────────────────────────

    #[test]
    fn edge_set_is_sorted_lexicographically() {
        // ("a","b","calls") < ("b","a","calls") — BTreeSet must honour tuple ordering.
        let g = make_graph(
            vec![make_node(1, "b"), make_node(2, "a")],
            vec![
                make_edge(1, 2, "calls"), // ("b","a","calls")
                make_edge(2, 1, "calls"), // ("a","b","calls")
            ],
        );
        let got = from_core(&g);
        let edges: Vec<_> = got.edges.iter().collect();
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].0, "a", "('a','b') must sort before ('b','a')");
        assert_eq!(edges[1].0, "b");
    }

    // ── Test 17: graphify qualified-id label style ────────────────────────────────

    #[test]
    fn qualified_label_style_preserved() {
        // Labels like "exceptions_httperror" (graphify qualified ids) are taken verbatim.
        let g = make_graph(
            vec![
                make_node(1, "exceptions_httperror"),
                make_node(2, "exceptions_baseerror"),
            ],
            vec![make_edge(1, 2, "inherits")],
        );
        let got = from_core(&g);
        assert!(got.nodes.contains("exceptions_httperror"));
        assert!(got.nodes.contains("exceptions_baseerror"));
        assert!(got.edges.contains(&(
            "exceptions_httperror".to_owned(),
            "exceptions_baseerror".to_owned(),
            "inherits".to_owned()
        )));
    }

    // ── Test 18: empty label is legal ─────────────────────────────────────────────

    #[test]
    fn empty_label_is_legal_and_not_skipped() {
        // An empty string is a valid label; from_core must not panic or skip it.
        let g = make_graph(
            vec![make_node(1, ""), make_node(2, "x")],
            vec![make_edge(1, 2, "calls")],
        );
        let got = from_core(&g);
        assert!(
            got.nodes.contains(""),
            "empty-string label must appear in node set"
        );
        assert!(got
            .edges
            .contains(&(String::new(), "x".to_owned(), "calls".to_owned())));
    }

    // ── Test 19: dangling edge does not corrupt valid edge collection ─────────────

    #[test]
    fn dangling_edge_does_not_corrupt_valid_edges() {
        // A ghost edge in the middle of the list must not suppress subsequent valid edges.
        let g = make_graph(
            vec![make_node(5, "p"), make_node(6, "q")],
            vec![
                make_edge(5, 6, "method"),
                make_edge(5, 0, "ghost"), // 0 not in graph
                make_edge(6, 5, "inverse"),
            ],
        );
        let got = from_core(&g);
        assert_eq!(got.edges.len(), 2);
        assert!(got
            .edges
            .contains(&("p".to_owned(), "q".to_owned(), "method".to_owned())));
        assert!(got
            .edges
            .contains(&("q".to_owned(), "p".to_owned(), "inverse".to_owned())));
    }
}
