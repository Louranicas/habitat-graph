//! Degree centrality over the graph.
//!
//! This module computes the *total degree* (in-degree + out-degree) for every node in a
//! [`Graph`], returning a ranked vector suitable for hub identification.
//!
//! ## Self-loop convention
//!
//! An edge where `source == target` (a self-loop `a → a`) contributes **2** to the degree of
//! node `a`, because `a` appears at **both** the source and the target endpoint of the same
//! edge.  This matches the standard graph-theory treatment of a self-loop when lifting a
//! directed graph into its underlying undirected form, and is explicitly documented here so
//! callers are not surprised.

use std::collections::HashMap;

use habitat_graph_core::{Graph, NodeId};

/// Returns the total degree of every node in `graph`, sorted **descending by degree** then
/// **ascending by [`NodeId`]** (a fully deterministic, parity-safe ranking).
///
/// ## Degree semantics
///
/// The degree of a node is the count of *edge endpoints* that reference it:
///
/// - A directed edge `a → b` (where `a ≠ b`) adds **1** to `a`'s degree and **1** to `b`'s.
/// - A self-loop `a → a` adds **2** to `a`'s degree (the node occupies both the source and
///   target slot of the same edge — see the module-level note for the rationale).
///
/// ## Isolation guarantee
///
/// Every node declared in `graph.nodes` appears in the output, even when it has no incident
/// edges (degree `0`).  Edge endpoints that reference node ids absent from `graph.nodes` are
/// also included (malformed-graph tolerance — no panic on structural inconsistency).
///
/// ## Complexity
///
/// *O*((N + E) log N) where N is the node count and E is the edge count.
///
/// ## Examples
///
/// ```
/// use habitat_graph_analyze::degree_centrality;
/// use habitat_graph_core::{Confidence, Edge, Graph, Node, NodeId, Span};
///
/// let mut g = Graph::new();
/// let span = Span::new(0, 1, 1, 1);
/// g.nodes.push(Node { id: NodeId::new(1), label: "a".into(), source_file: "f.rs".into(), source_location: span });
/// g.nodes.push(Node { id: NodeId::new(2), label: "b".into(), source_file: "f.rs".into(), source_location: span });
/// g.edges.push(Edge { source: NodeId::new(1), target: NodeId::new(2), relation: "calls".into(), confidence: Confidence::Extracted });
/// let ranked = degree_centrality(&g);
/// // Both nodes have degree 1; NodeId 1 < NodeId 2 so node 1 sorts first.
/// assert_eq!(ranked, vec![(NodeId::new(1), 1), (NodeId::new(2), 1)]);
/// ```
#[must_use]
pub fn degree_centrality(graph: &Graph) -> Vec<(NodeId, usize)> {
    // Seed every declared node at degree 0 so isolated nodes appear in the output.
    let mut degrees: HashMap<NodeId, usize> = graph.nodes.iter().map(|n| (n.id, 0_usize)).collect();

    for edge in &graph.edges {
        if edge.source == edge.target {
            // Self-loop a → a: the node occupies both endpoints, so it gains 2.
            *degrees.entry(edge.source).or_insert(0) += 2;
        } else {
            *degrees.entry(edge.source).or_insert(0) += 1;
            *degrees.entry(edge.target).or_insert(0) += 1;
        }
    }

    let mut ranked: Vec<(NodeId, usize)> = degrees.into_iter().collect();
    // Primary sort: degree descending.  Tiebreak: NodeId ascending (deterministic).
    ranked.sort_by(|(id_a, deg_a), (id_b, deg_b)| deg_b.cmp(deg_a).then_with(|| id_a.cmp(id_b)));
    ranked
}

#[cfg(test)]
mod tests {
    use super::degree_centrality;
    use habitat_graph_core::{Confidence, Edge, Graph, Node, NodeId, Span};

    // ---------------------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------------------

    const SPAN: Span = Span::new(0, 1, 1, 1);

    fn n(id: u32) -> Node {
        Node {
            id: NodeId::new(id),
            label: format!("n{id}"),
            source_file: "t.rs".into(),
            source_location: SPAN,
        }
    }

    fn e(src: u32, tgt: u32) -> Edge {
        Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: "rel".into(),
            confidence: Confidence::Extracted,
        }
    }

    fn nid(id: u32) -> NodeId {
        NodeId::new(id)
    }

    // ---------------------------------------------------------------------------
    // 1. Empty graph → empty result
    // ---------------------------------------------------------------------------
    #[test]
    fn empty_graph_returns_empty_vec() {
        let g = Graph::new();
        assert_eq!(degree_centrality(&g), vec![]);
    }

    // ---------------------------------------------------------------------------
    // 2. Single isolated node → degree 0, present in result
    // ---------------------------------------------------------------------------
    #[test]
    fn isolated_node_has_degree_zero() {
        let mut g = Graph::new();
        g.nodes.push(n(1));
        let result = degree_centrality(&g);
        assert_eq!(result, vec![(nid(1), 0)]);
    }

    // ---------------------------------------------------------------------------
    // 3. Multiple isolated nodes → all degree 0, tiebreak by NodeId ASC
    // ---------------------------------------------------------------------------
    #[test]
    fn multiple_isolated_nodes_sorted_by_id_ascending() {
        let mut g = Graph::new();
        g.nodes.push(n(5));
        g.nodes.push(n(2));
        g.nodes.push(n(9));
        let result = degree_centrality(&g);
        assert_eq!(
            result,
            vec![(nid(2), 0), (nid(5), 0), (nid(9), 0)],
            "equal-degree nodes must be ordered by NodeId ASC"
        );
    }

    // ---------------------------------------------------------------------------
    // 4. Simple directed edge a→b: both endpoints get degree 1
    // ---------------------------------------------------------------------------
    #[test]
    fn simple_edge_gives_both_endpoints_degree_one() {
        let mut g = Graph::new();
        g.nodes.push(n(1));
        g.nodes.push(n(2));
        g.edges.push(e(1, 2));
        let result = degree_centrality(&g);
        assert_eq!(result, vec![(nid(1), 1), (nid(2), 1)]);
    }

    // ---------------------------------------------------------------------------
    // 5. Hub node: a→b, a→c, a→d → a has degree 3; b, c, d have degree 1
    // ---------------------------------------------------------------------------
    #[test]
    fn hub_node_has_highest_degree() {
        let mut g = Graph::new();
        for id in [1, 2, 3, 4] {
            g.nodes.push(n(id));
        }
        g.edges.push(e(1, 2));
        g.edges.push(e(1, 3));
        g.edges.push(e(1, 4));
        let result = degree_centrality(&g);
        // Node 1 is the hub with degree 3.
        assert_eq!(result[0], (nid(1), 3));
        // The remaining nodes (2, 3, 4) each have degree 1, sorted by id.
        assert_eq!(result[1..], [(nid(2), 1), (nid(3), 1), (nid(4), 1)]);
    }

    // ---------------------------------------------------------------------------
    // 6. Self-loop a→a contributes exactly 2 to a's degree
    // ---------------------------------------------------------------------------
    #[test]
    fn self_loop_contributes_two() {
        let mut g = Graph::new();
        g.nodes.push(n(7));
        g.edges.push(e(7, 7));
        let result = degree_centrality(&g);
        assert_eq!(result, vec![(nid(7), 2)]);
    }

    // ---------------------------------------------------------------------------
    // 7. Self-loop degree is 2, not 1 — explicit guard against off-by-one
    // ---------------------------------------------------------------------------
    #[test]
    fn self_loop_degree_is_not_one() {
        let mut g = Graph::new();
        g.nodes.push(n(3));
        g.edges.push(e(3, 3));
        let result = degree_centrality(&g);
        assert_ne!(result[0].1, 1, "self-loop must count as 2, not 1");
        assert_eq!(result[0].1, 2);
    }

    // ---------------------------------------------------------------------------
    // 8. Sort order: DESC degree, ASC NodeId tiebreak (verify full vector order)
    // ---------------------------------------------------------------------------
    #[test]
    fn sort_is_desc_degree_then_asc_nodeid() {
        let mut g = Graph::new();
        // Nodes 1, 2, 3, 4, 5
        for id in 1..=5 {
            g.nodes.push(n(id));
        }
        // Node 3 will be the hub: degree 3 via edges 3→1, 3→2, 3→4
        g.edges.push(e(3, 1));
        g.edges.push(e(3, 2));
        g.edges.push(e(3, 4));
        // Node 5 is isolated.
        // Degrees: 1→1, 2→1, 3→3, 4→1, 5→0
        let result = degree_centrality(&g);
        let ids: Vec<u32> = result.iter().map(|(id, _)| id.get()).collect();
        let degs: Vec<usize> = result.iter().map(|(_, d)| *d).collect();
        assert_eq!(ids, vec![3, 1, 2, 4, 5]);
        assert_eq!(degs, vec![3, 1, 1, 1, 0]);
    }

    // ---------------------------------------------------------------------------
    // 9. Node referenced only as edge target still appears in output
    // ---------------------------------------------------------------------------
    #[test]
    fn target_only_node_in_nodes_list_still_counted() {
        let mut g = Graph::new();
        g.nodes.push(n(10));
        g.nodes.push(n(20));
        // Edge 10→20: node 20 appears as target only.
        g.edges.push(e(10, 20));
        let result = degree_centrality(&g);
        // Both must appear; 20 has degree 1.
        let twenty = result.iter().find(|(id, _)| *id == nid(20));
        assert_eq!(twenty, Some(&(nid(20), 1)));
    }

    // ---------------------------------------------------------------------------
    // 10. Node referenced only as edge endpoint but NOT in graph.nodes is tolerated
    // ---------------------------------------------------------------------------
    #[test]
    fn edge_target_absent_from_nodes_list_is_tolerated() {
        let mut g = Graph::new();
        g.nodes.push(n(1));
        // Edge 1→99 where node 99 is not declared.
        g.edges.push(e(1, 99));
        let result = degree_centrality(&g);
        // Node 99 must still appear with degree 1.
        let ninety_nine = result.iter().find(|(id, _)| *id == nid(99));
        assert_eq!(ninety_nine, Some(&(nid(99), 1)));
    }

    // ---------------------------------------------------------------------------
    // 11. Multiple self-loops on same node accumulate correctly
    // ---------------------------------------------------------------------------
    #[test]
    fn multiple_self_loops_accumulate() {
        let mut g = Graph::new();
        g.nodes.push(n(4));
        g.edges.push(e(4, 4));
        g.edges.push(e(4, 4));
        // Two self-loops → degree 4 (2 per loop).
        let result = degree_centrality(&g);
        assert_eq!(result, vec![(nid(4), 4)]);
    }

    // ---------------------------------------------------------------------------
    // 12. Chain a→b, b→c: b has degree 2 (both in and out), a and c have degree 1
    // ---------------------------------------------------------------------------
    #[test]
    fn chain_middle_node_has_highest_degree() {
        let mut g = Graph::new();
        g.nodes.push(n(1));
        g.nodes.push(n(2));
        g.nodes.push(n(3));
        g.edges.push(e(1, 2));
        g.edges.push(e(2, 3));
        let result = degree_centrality(&g);
        assert_eq!(result[0], (nid(2), 2));
        // Nodes 1 and 3 both have degree 1, ordered by id asc.
        assert_eq!(result[1], (nid(1), 1));
        assert_eq!(result[2], (nid(3), 1));
    }

    // ---------------------------------------------------------------------------
    // 13. Parallel edges between same pair accumulate per-edge
    // ---------------------------------------------------------------------------
    #[test]
    fn parallel_edges_accumulate_degree() {
        let mut g = Graph::new();
        g.nodes.push(n(1));
        g.nodes.push(n(2));
        // Three edges from 1 to 2 (different relations, same pair).
        for rel in ["calls", "imports", "uses"] {
            g.edges.push(Edge {
                source: nid(1),
                target: nid(2),
                relation: rel.into(),
                confidence: Confidence::Extracted,
            });
        }
        let result = degree_centrality(&g);
        // Each of the 3 edges contributes 1 to each node → degree 3 apiece.
        assert_eq!(result, vec![(nid(1), 3), (nid(2), 3)]);
    }

    // ---------------------------------------------------------------------------
    // 14. Self-loop + regular edge: combined count is correct
    // ---------------------------------------------------------------------------
    #[test]
    fn self_loop_plus_regular_edge_combined() {
        let mut g = Graph::new();
        g.nodes.push(n(1));
        g.nodes.push(n(2));
        g.edges.push(e(1, 1)); // self-loop: +2 to node 1
        g.edges.push(e(1, 2)); // regular: +1 to node 1, +1 to node 2
                               // Node 1 degree = 3, node 2 degree = 1.
        let result = degree_centrality(&g);
        assert_eq!(result[0], (nid(1), 3));
        assert_eq!(result[1], (nid(2), 1));
    }

    // ---------------------------------------------------------------------------
    // 15. Fan-in: many nodes pointing to one hub
    // ---------------------------------------------------------------------------
    #[test]
    fn fan_in_hub_gets_high_degree() {
        let mut g = Graph::new();
        // Nodes 1..=6; node 6 is the fan-in hub.
        for id in 1..=6 {
            g.nodes.push(n(id));
        }
        // Nodes 1–5 all point to node 6.
        for src in 1..=5 {
            g.edges.push(e(src, 6));
        }
        let result = degree_centrality(&g);
        // Node 6 has degree 5 (one incoming from each of 1–5).
        assert_eq!(result[0], (nid(6), 5));
        // Nodes 1–5 each have degree 1.
        for (id, deg) in result.iter().skip(1) {
            assert_eq!(*deg, 1, "node {} should have degree 1", id.get());
        }
    }

    // ---------------------------------------------------------------------------
    // 16. All same degree: verify NodeId ascending tiebreak is exact
    // ---------------------------------------------------------------------------
    #[test]
    fn equal_degree_tiebreak_is_nodeid_asc() {
        let mut g = Graph::new();
        // Four nodes in a 2-cycle pair: 1↔2 and 3↔4 — each node degree 1.
        for id in [4, 3, 2, 1] {
            g.nodes.push(n(id));
        }
        g.edges.push(e(1, 2));
        g.edges.push(e(3, 4));
        let result = degree_centrality(&g);
        let ids: Vec<u32> = result.iter().map(|(id, _)| id.get()).collect();
        assert_eq!(ids, vec![1, 2, 3, 4]);
    }

    // ---------------------------------------------------------------------------
    // 17. Degree result is deterministic across repeated calls (no HashMap ordering leak)
    // ---------------------------------------------------------------------------
    #[test]
    fn result_is_deterministic_across_calls() {
        let mut g = Graph::new();
        for id in [7, 3, 1, 9, 5] {
            g.nodes.push(n(id));
        }
        g.edges.push(e(1, 3));
        g.edges.push(e(9, 3));
        g.edges.push(e(3, 7));
        let first = degree_centrality(&g);
        let second = degree_centrality(&g);
        assert_eq!(first, second, "degree_centrality must be deterministic");
    }

    // ---------------------------------------------------------------------------
    // 18. No edges but multiple nodes: all degree 0, ids strictly ascending
    // ---------------------------------------------------------------------------
    #[test]
    fn no_edges_many_nodes_all_zero_asc() {
        let mut g = Graph::new();
        for id in [100, 50, 10, 200] {
            g.nodes.push(n(id));
        }
        let result = degree_centrality(&g);
        let ids: Vec<u32> = result.iter().map(|(id, _)| id.get()).collect();
        assert_eq!(ids, vec![10, 50, 100, 200]);
        assert!(result.iter().all(|(_, d)| *d == 0));
    }

    // ---------------------------------------------------------------------------
    // 19. Source-only node (all edges emanate from it, none target it) is counted
    // ---------------------------------------------------------------------------
    #[test]
    fn source_only_node_counted() {
        let mut g = Graph::new();
        g.nodes.push(n(1));
        g.nodes.push(n(2));
        g.nodes.push(n(3));
        // Node 1 is pure source.
        g.edges.push(e(1, 2));
        g.edges.push(e(1, 3));
        let result = degree_centrality(&g);
        let one = result.iter().find(|(id, _)| *id == nid(1));
        assert_eq!(one, Some(&(nid(1), 2)));
    }

    // ---------------------------------------------------------------------------
    // 20. Graph with only a self-loop (no non-self edges): result length equals nodes
    // ---------------------------------------------------------------------------
    #[test]
    fn only_self_loop_result_covers_all_nodes() {
        let mut g = Graph::new();
        g.nodes.push(n(1));
        g.nodes.push(n(2));
        // Self-loop on node 1 only; node 2 is isolated.
        g.edges.push(e(1, 1));
        let result = degree_centrality(&g);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], (nid(1), 2));
        assert_eq!(result[1], (nid(2), 0));
    }
}
