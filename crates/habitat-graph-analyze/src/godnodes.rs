//! God-nodes — the highest-connectivity hubs of the graph (PD analytics, FO-9).
//!
//! Surfaces the already-computed [`degree_centrality`](crate::degree_centrality) as labelled,
//! ranked hubs. The primary sort key is total degree (descending); the tiebreak is
//! [`NodeId`] ascending, giving a deterministic total order (R4).
//!
//! ## Label resolution
//!
//! Labels come from [`Graph::nodes`](habitat_graph_core::Graph). If an edge references a node id
//! that is absent from `graph.nodes`, the label field in the returned [`GodNode`] is an empty
//! `String` — the function never panics on a structurally inconsistent graph.
//!
//! ## Self-loop convention
//!
//! An edge `a → a` contributes **2** to node `a`'s degree, consistent with the treatment in
//! [`degree_centrality`](crate::degree_centrality). This means a node with a single self-loop
//! has degree `2`, not `1`.

use std::collections::HashMap;

use habitat_graph_core::{Graph, NodeId};

/// A high-connectivity hub node: its stable identifier, human-readable label, and total degree.
///
/// Returned by [`god_nodes`] in descending-degree order with ascending [`NodeId`] as the
/// tiebreak (R4 deterministic total order). The `label` field is empty when the node id is
/// referenced by an edge but is absent from [`Graph::nodes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GodNode {
    /// The hub node's stable identifier.
    pub id: NodeId,
    /// The hub node's human-readable label.
    ///
    /// Empty when the `id` is referenced by an edge but absent from
    /// [`Graph::nodes`](habitat_graph_core::Graph).
    pub label: String,
    /// Total degree (in-edges + out-edges) of the node.
    ///
    /// Self-loops (`a → a`) count as **2** — the node occupies both source and target slots.
    pub degree: usize,
}

/// Returns the top-`top_n` highest-degree nodes as [`GodNode`]s, in a fully deterministic order.
///
/// Results are sorted **descending by degree** then **ascending by [`NodeId`]** — satisfying the
/// R4 byte-identical-output invariant. When `top_n` is `0`, or when the graph is empty, the
/// returned `Vec` is empty.
///
/// ## Label resolution
///
/// Each result's `label` is taken from `graph.nodes`; if the node id is absent (an edge
/// references a ghost node), the label is an empty [`String`].
///
/// ## Degree semantics
///
/// Degree is computed by [`degree_centrality`](crate::degree_centrality):
/// - a directed edge `a → b` (with `a ≠ b`) adds **1** to both `a`'s and `b`'s degree;
/// - a self-loop `a → a` adds **2** to `a`'s degree.
///
/// ## Examples
///
/// ```
/// use habitat_graph_analyze::god_nodes;
/// use habitat_graph_core::{Confidence, Edge, Graph, Node, NodeId, Span};
///
/// let span = Span::new(0, 1, 1, 1);
/// let mut g = Graph::new();
/// g.nodes.push(Node {
///     id: NodeId::new(1),
///     label: "hub".into(),
///     source_file: "a.rs".into(),
///     source_location: span,
/// });
/// g.nodes.push(Node {
///     id: NodeId::new(2),
///     label: "spoke".into(),
///     source_file: "a.rs".into(),
///     source_location: span,
/// });
/// g.edges.push(Edge {
///     source: NodeId::new(1),
///     target: NodeId::new(2),
///     relation: "calls".into(),
///     confidence: Confidence::Extracted,
/// });
/// let hubs = god_nodes(&g, 1);
/// assert_eq!(hubs.len(), 1);
/// assert_eq!(hubs[0].id, NodeId::new(1));
/// assert_eq!(hubs[0].label, "hub");
/// assert_eq!(hubs[0].degree, 1);
/// ```
#[must_use]
pub fn god_nodes(graph: &Graph, top_n: usize) -> Vec<GodNode> {
    if top_n == 0 {
        return Vec::new();
    }
    let label_of: HashMap<NodeId, &str> = graph
        .nodes
        .iter()
        .map(|n| (n.id, n.label.as_str()))
        .collect();
    // `degree_centrality` already returns DESC degree / ASC NodeId, but we sort again here
    // to make `god_nodes`'s own ordering contract explicit and independent of the delegate.
    let mut ranked = crate::degree_centrality(graph);
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked
        .into_iter()
        .take(top_n)
        .map(|(id, degree)| GodNode {
            id,
            label: label_of.get(&id).copied().unwrap_or("").to_owned(),
            degree,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use habitat_graph_core::{Confidence, Edge, Graph, Node, NodeId, Span};

    use super::{god_nodes, GodNode};

    // -----------------------------------------------------------------------
    // Shared helpers
    // -----------------------------------------------------------------------

    const SPAN: Span = Span::new(0, 1, 1, 1);

    fn node(id: u32) -> Node {
        Node {
            id: NodeId::new(id),
            label: format!("n{id}"),
            source_file: "t.rs".into(),
            source_location: SPAN,
        }
    }

    fn node_labelled(id: u32, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: "t.rs".into(),
            source_location: SPAN,
        }
    }

    fn edge(s: u32, t: u32) -> Edge {
        Edge {
            source: NodeId::new(s),
            target: NodeId::new(t),
            relation: "calls".into(),
            confidence: Confidence::Extracted,
        }
    }

    fn nid(id: u32) -> NodeId {
        NodeId::new(id)
    }

    // -----------------------------------------------------------------------
    // Group 1: Empty / boundary cases
    // -----------------------------------------------------------------------

    /// An empty graph with `top_n` > 0 returns an empty vec.
    #[test]
    fn empty_graph_top_n_5_is_empty() {
        assert!(god_nodes(&Graph::new(), 5).is_empty());
    }

    /// An empty graph with `top_n` = 0 returns an empty vec.
    #[test]
    fn empty_graph_top_n_0_is_empty() {
        assert!(god_nodes(&Graph::new(), 0).is_empty());
    }

    /// `top_n` = 0 on a non-empty graph returns an empty vec — no nodes exposed.
    #[test]
    fn top_n_zero_nonempty_graph_returns_empty() {
        let mut g = Graph::new();
        for i in 1..=3 {
            g.nodes.push(node(i));
        }
        g.edges.push(edge(1, 2));
        assert!(god_nodes(&g, 0).is_empty());
    }

    /// When `top_n` exceeds the number of nodes, all nodes are returned.
    #[test]
    fn top_n_exceeds_node_count_returns_all() {
        let mut g = Graph::new();
        g.nodes.push(node(1));
        g.nodes.push(node(2));
        g.edges.push(edge(1, 2));
        let result = god_nodes(&g, 100);
        assert_eq!(result.len(), 2);
    }

    /// With exactly one node and no edges, `top_n` = 1 returns that single node with degree 0.
    #[test]
    fn single_node_no_edges_top_1() {
        let mut g = Graph::new();
        g.nodes.push(node(7));
        let result = god_nodes(&g, 1);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, nid(7));
        assert_eq!(result[0].label, "n7");
        assert_eq!(result[0].degree, 0);
    }

    /// With exactly one node and no edges, `top_n` = 0 returns empty.
    #[test]
    fn single_node_no_edges_top_0() {
        let mut g = Graph::new();
        g.nodes.push(node(3));
        assert!(god_nodes(&g, 0).is_empty());
    }

    /// `usize::MAX` for `top_n` on a small graph returns all nodes, not a panic.
    #[test]
    fn top_n_usize_max_small_graph() {
        let mut g = Graph::new();
        g.nodes.push(node(1));
        g.nodes.push(node(2));
        g.edges.push(edge(1, 2));
        let result = god_nodes(&g, usize::MAX);
        assert_eq!(result.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Group 2: Degree correctness (in + out)
    // -----------------------------------------------------------------------

    /// A directed edge `a → b` gives both endpoints degree 1.
    #[test]
    fn simple_edge_both_endpoints_degree_1() {
        let mut g = Graph::new();
        g.nodes.push(node(1));
        g.nodes.push(node(2));
        g.edges.push(edge(1, 2));
        let result = god_nodes(&g, 2);
        // Both nodes have degree 1; tiebreak by id: node 1 first.
        assert_eq!(result[0].degree, 1);
        assert_eq!(result[1].degree, 1);
    }

    /// A self-loop `a → a` contributes exactly 2 to the node's degree.
    #[test]
    fn self_loop_contributes_exactly_2() {
        let mut g = Graph::new();
        g.nodes.push(node(5));
        g.edges.push(edge(5, 5));
        let result = god_nodes(&g, 1);
        assert_eq!(result[0].degree, 2);
    }

    /// A node receiving an in-edge has degree 1.
    #[test]
    fn in_edge_adds_to_degree() {
        let mut g = Graph::new();
        g.nodes.push(node(10));
        g.nodes.push(node(20));
        g.edges.push(edge(10, 20));
        let result = god_nodes(&g, 2);
        let n20 = result
            .iter()
            .find(|x| x.id == nid(20))
            .expect("node 20 missing");
        assert_eq!(n20.degree, 1);
    }

    /// A node emitting an out-edge has degree 1.
    #[test]
    fn out_edge_adds_to_degree() {
        let mut g = Graph::new();
        g.nodes.push(node(10));
        g.nodes.push(node(20));
        g.edges.push(edge(10, 20));
        let result = god_nodes(&g, 2);
        let n10 = result
            .iter()
            .find(|x| x.id == nid(10))
            .expect("node 10 missing");
        assert_eq!(n10.degree, 1);
    }

    /// The middle node of a chain `a → b → c` has degree 2 (one in, one out).
    #[test]
    fn chain_middle_node_has_degree_2() {
        let mut g = Graph::new();
        for i in 1..=3 {
            g.nodes.push(node(i));
        }
        g.edges.push(edge(1, 2));
        g.edges.push(edge(2, 3));
        let result = god_nodes(&g, 3);
        assert_eq!(result[0].id, nid(2));
        assert_eq!(result[0].degree, 2);
    }

    /// A fan-out node `a → b, a → c, a → d` has degree 3.
    #[test]
    fn fan_out_hub_degree_3() {
        let mut g = Graph::new();
        for i in 1..=4 {
            g.nodes.push(node(i));
        }
        g.edges.push(edge(1, 2));
        g.edges.push(edge(1, 3));
        g.edges.push(edge(1, 4));
        let result = god_nodes(&g, 1);
        assert_eq!(result[0].id, nid(1));
        assert_eq!(result[0].degree, 3);
    }

    /// Multiple parallel edges between the same pair accumulate independently.
    #[test]
    fn parallel_edges_accumulate_degree() {
        let mut g = Graph::new();
        g.nodes.push(node(1));
        g.nodes.push(node(2));
        // Three distinct-relation edges from 1 to 2.
        for rel in ["calls", "imports", "references"] {
            g.edges.push(Edge {
                source: nid(1),
                target: nid(2),
                relation: rel.into(),
                confidence: Confidence::Extracted,
            });
        }
        let result = god_nodes(&g, 2);
        // Each edge contributes 1 to each node → degree 3 apiece, tiebreak by id.
        assert_eq!(result[0].id, nid(1));
        assert_eq!(result[0].degree, 3);
        assert_eq!(result[1].degree, 3);
    }

    /// Self-loop + regular edge: degrees combine correctly.
    #[test]
    fn self_loop_plus_regular_edge_combined() {
        let mut g = Graph::new();
        g.nodes.push(node(1));
        g.nodes.push(node(2));
        g.edges.push(edge(1, 1)); // self-loop: +2 to node 1
        g.edges.push(edge(1, 2)); // regular: +1 to node 1, +1 to node 2
        let result = god_nodes(&g, 2);
        assert_eq!(result[0].id, nid(1));
        assert_eq!(result[0].degree, 3);
        assert_eq!(result[1].id, nid(2));
        assert_eq!(result[1].degree, 1);
    }

    /// A declared node with no incident edges has degree 0 and appears in results.
    #[test]
    fn isolated_node_has_degree_zero_and_appears() {
        let mut g = Graph::new();
        g.nodes.push(node(1));
        g.nodes.push(node(2));
        g.edges.push(edge(1, 2));
        g.nodes.push(node(99)); // isolated
        let result = god_nodes(&g, 3);
        let n99 = result
            .iter()
            .find(|x| x.id == nid(99))
            .expect("isolated node missing");
        assert_eq!(n99.degree, 0);
    }

    // -----------------------------------------------------------------------
    // Group 3: Ordering / ranking
    // -----------------------------------------------------------------------

    /// The node with the highest degree is always first.
    #[test]
    fn highest_degree_node_is_first() {
        let mut g = Graph::new();
        for i in 1..=3 {
            g.nodes.push(node(i));
        }
        // Node 1: out-degree 2 → total degree 2.
        g.edges.push(edge(1, 2));
        g.edges.push(edge(1, 3));
        let result = god_nodes(&g, 1);
        assert_eq!(result[0].id, nid(1));
    }

    /// In a fan-out star, the center must be rank 1.
    #[test]
    fn star_center_fan_out_is_rank_1() {
        let mut g = Graph::new();
        let center = 1_u32;
        for i in 1..=6 {
            g.nodes.push(node(i));
        }
        for spoke in 2..=6 {
            g.edges.push(edge(center, spoke));
        }
        let result = god_nodes(&g, 1);
        assert_eq!(result[0].id, nid(center));
        assert_eq!(result[0].degree, 5);
    }

    /// In a fan-in star (all spokes point to center), center must be rank 1.
    #[test]
    fn star_center_fan_in_is_rank_1() {
        let mut g = Graph::new();
        let center = 10_u32;
        for i in [10, 20, 30, 40, 50] {
            g.nodes.push(node(i));
        }
        for spoke in [20, 30, 40, 50] {
            g.edges.push(edge(spoke, center));
        }
        let result = god_nodes(&g, 1);
        assert_eq!(result[0].id, nid(center));
        assert_eq!(result[0].degree, 4);
    }

    /// Nodes with degree 0 appear after all connected nodes.
    #[test]
    fn zero_degree_nodes_rank_below_connected() {
        let mut g = Graph::new();
        for i in 1..=4 {
            g.nodes.push(node(i));
        }
        g.edges.push(edge(1, 2));
        // Nodes 3 and 4 are isolated.
        let result = god_nodes(&g, 4);
        assert!(result[0].degree >= 1, "first node must be connected");
        assert!(result[1].degree >= 1, "second node must be connected");
        assert_eq!(result[2].degree, 0);
        assert_eq!(result[3].degree, 0);
    }

    /// The result vector is strictly non-ascending in degree.
    #[test]
    fn result_is_non_ascending_by_degree() {
        let mut g = Graph::new();
        for i in 1..=5 {
            g.nodes.push(node(i));
        }
        // Node 3 has degree 3: 3→1, 3→2, 3→4.
        g.edges.push(edge(3, 1));
        g.edges.push(edge(3, 2));
        g.edges.push(edge(3, 4));
        let result = god_nodes(&g, 5);
        for w in result.windows(2) {
            assert!(
                w[0].degree >= w[1].degree,
                "degree order violated: {} then {}",
                w[0].degree,
                w[1].degree
            );
        }
    }

    /// `top_n` = 1 returns exactly the highest-degree node.
    #[test]
    fn top_n_1_returns_only_highest() {
        let mut g = Graph::new();
        for i in 1..=5 {
            g.nodes.push(node(i));
        }
        g.edges.push(edge(2, 1));
        g.edges.push(edge(3, 1));
        g.edges.push(edge(4, 1));
        let result = god_nodes(&g, 1);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, nid(1));
        assert_eq!(result[0].degree, 3);
    }

    /// `top_n` = 2 returns the two highest-degree nodes in order.
    #[test]
    fn top_n_2_returns_two_highest() {
        let mut g = Graph::new();
        for i in 1..=4 {
            g.nodes.push(node(i));
        }
        // Node 1: degree 3; node 2: degree 2; nodes 3, 4: degree 1 each.
        g.edges.push(edge(1, 2));
        g.edges.push(edge(1, 3));
        g.edges.push(edge(1, 4));
        g.edges.push(edge(2, 3));
        let result = god_nodes(&g, 2);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id, nid(1));
        assert_eq!(result[1].id, nid(2));
    }

    // -----------------------------------------------------------------------
    // Group 4: Tie-break by NodeId ascending
    // -----------------------------------------------------------------------

    /// When two nodes have the same degree, the one with the lower id is first.
    #[test]
    fn tie_break_lower_id_sorts_first() {
        let mut g = Graph::new();
        // Nodes 5 and 3 — inserted in descending id order, each with degree 1.
        g.nodes.push(node(5));
        g.nodes.push(node(3));
        g.edges.push(edge(3, 5));
        let result = god_nodes(&g, 2);
        // Both degree 1; node 3 < node 5 so node 3 must be first.
        assert_eq!(result[0].id, nid(3));
        assert_eq!(result[1].id, nid(5));
    }

    /// A three-way tie is resolved by ascending id.
    #[test]
    fn three_way_tie_resolved_by_id_asc() {
        let mut g = Graph::new();
        // Three 2-cycles: 10↔20, 30↔40, 50↔60 — each node has degree 1.
        for id in [60, 10, 50, 20, 40, 30] {
            g.nodes.push(node(id));
        }
        g.edges.push(edge(10, 20));
        g.edges.push(edge(30, 40));
        g.edges.push(edge(50, 60));
        let result = god_nodes(&g, 6);
        let ids: Vec<u32> = result.iter().map(|x| x.id.get()).collect();
        assert_eq!(ids, vec![10, 20, 30, 40, 50, 60]);
    }

    /// All isolated nodes (degree 0) are sorted ascending by id.
    #[test]
    fn all_isolated_sorted_by_id_asc() {
        let mut g = Graph::new();
        for id in [9, 3, 7, 1, 5] {
            g.nodes.push(node(id));
        }
        let result = god_nodes(&g, 5);
        let ids: Vec<u32> = result.iter().map(|x| x.id.get()).collect();
        assert_eq!(ids, vec![1, 3, 5, 7, 9]);
    }

    /// Tie-break uses numeric ordering, not string ordering (e.g. 2 < 10 numerically).
    #[test]
    fn tie_break_is_numeric_not_lexicographic() {
        let mut g = Graph::new();
        // Nodes 2 and 10 — both get degree 1; numerically 2 < 10.
        g.nodes.push(node(2));
        g.nodes.push(node(10));
        g.edges.push(edge(2, 10));
        let result = god_nodes(&g, 2);
        assert_eq!(result[0].id, nid(2));
        assert_eq!(result[1].id, nid(10));
    }

    /// Node insertion order does not affect the tie-break outcome.
    #[test]
    fn tie_break_independent_of_insertion_order() {
        let make = |insert_order: &[u32]| {
            let mut g = Graph::new();
            for &id in insert_order {
                g.nodes.push(node(id));
            }
            // Isolated nodes, all degree 0.
            god_nodes(&g, insert_order.len())
        };
        let a = make(&[5, 2, 8, 1]);
        let b = make(&[1, 8, 2, 5]);
        let ids_a: Vec<u32> = a.iter().map(|x| x.id.get()).collect();
        let ids_b: Vec<u32> = b.iter().map(|x| x.id.get()).collect();
        assert_eq!(ids_a, ids_b);
    }

    /// Ten nodes all with degree 1 are sorted by ascending id.
    #[test]
    fn ten_nodes_all_degree_1_sorted_by_id() {
        let mut g = Graph::new();
        // Nodes 1–10, edges forming a ring where all degrees are 1
        // (actually a directed ring: each node has out-degree 1 + in-degree 1 = degree 2,
        // so let's use a matching: pair up nodes so all get exactly degree 1).
        // Node i → node i+5 (for i in 1..=5) gives nodes 1-5 out-degree 1 and 6-10 in-degree 1.
        for i in 1..=10 {
            g.nodes.push(node(i));
        }
        for i in 1..=5 {
            g.edges.push(edge(i, i + 5));
        }
        let result = god_nodes(&g, 10);
        let ids: Vec<u32> = result.iter().map(|x| x.id.get()).collect();
        // All have degree 1; sorted ascending by id.
        assert_eq!(ids, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    // -----------------------------------------------------------------------
    // Group 5: Label join
    // -----------------------------------------------------------------------

    /// The hub node's label matches its `Node::label` field exactly.
    #[test]
    fn label_matches_node_label_for_hub() {
        let mut g = Graph::new();
        g.nodes.push(node_labelled(1, "MyStruct::new"));
        g.nodes.push(node_labelled(2, "helper"));
        g.edges.push(edge(1, 2));
        g.edges.push(edge(2, 1));
        // Node 1 and 2 both have degree 2; node 1 wins tiebreak.
        let result = god_nodes(&g, 1);
        assert_eq!(result[0].label, "MyStruct::new");
    }

    /// A spoke node's label is also correct.
    #[test]
    fn label_correct_for_spoke() {
        let mut g = Graph::new();
        g.nodes.push(node_labelled(1, "hub_fn"));
        g.nodes.push(node_labelled(2, "spoke_fn"));
        g.nodes.push(node_labelled(3, "spoke_fn_b"));
        g.edges.push(edge(1, 2));
        g.edges.push(edge(1, 3));
        let result = god_nodes(&g, 3);
        let n2 = result
            .iter()
            .find(|x| x.id == nid(2))
            .expect("node 2 missing");
        assert_eq!(n2.label, "spoke_fn");
    }

    /// A ghost node (referenced by an edge but absent from `graph.nodes`) gets an empty label.
    #[test]
    fn ghost_node_label_is_empty_string() {
        let mut g = Graph::new();
        g.nodes.push(node(1));
        // Node 99 is not declared in graph.nodes.
        g.edges.push(edge(1, 99));
        let result = god_nodes(&g, 2);
        let ghost = result
            .iter()
            .find(|x| x.id == nid(99))
            .expect("ghost node missing");
        assert_eq!(ghost.label, "");
    }

    /// Labels are not mixed up between nodes when many are present.
    #[test]
    fn labels_not_mixed_across_nodes() {
        let mut g = Graph::new();
        let entries: &[(u32, &str)] = &[
            (1, "alpha"),
            (2, "beta"),
            (3, "gamma"),
            (4, "delta"),
            (5, "epsilon"),
        ];
        for &(id, lbl) in entries {
            g.nodes.push(node_labelled(id, lbl));
        }
        g.edges.push(edge(3, 1));
        g.edges.push(edge(3, 2));
        g.edges.push(edge(3, 4));
        let result = god_nodes(&g, 5);
        for gn in &result {
            // Find the original label from our entries array.
            let expected = entries
                .iter()
                .find(|&&(id, _)| id == gn.id.get())
                .map_or("", |&(_, lbl)| lbl);
            assert_eq!(gn.label, expected, "label mismatch for id {}", gn.id.get());
        }
    }

    /// A custom multi-segment label is preserved verbatim.
    #[test]
    fn custom_label_preserved_verbatim() {
        let mut g = Graph::new();
        g.nodes.push(node_labelled(42, "my::module::SomeType<T>"));
        let result = god_nodes(&g, 1);
        assert_eq!(result[0].label, "my::module::SomeType<T>");
    }

    /// A node whose declared label is `""` returns `""` in the result.
    #[test]
    fn empty_label_in_node_is_preserved() {
        let mut g = Graph::new();
        g.nodes.push(node_labelled(1, ""));
        let result = god_nodes(&g, 1);
        assert_eq!(result[0].label, "");
    }

    /// Labels are resolved independently of the order nodes were pushed.
    #[test]
    fn label_resolution_order_independent() {
        let mut g = Graph::new();
        // Push nodes in reverse id order.
        g.nodes.push(node_labelled(3, "c"));
        g.nodes.push(node_labelled(1, "a"));
        g.nodes.push(node_labelled(2, "b"));
        g.edges.push(edge(1, 2));
        g.edges.push(edge(1, 3));
        let result = god_nodes(&g, 3);
        let n1 = result
            .iter()
            .find(|x| x.id == nid(1))
            .expect("node 1 missing");
        assert_eq!(n1.label, "a");
        let n2 = result
            .iter()
            .find(|x| x.id == nid(2))
            .expect("node 2 missing");
        assert_eq!(n2.label, "b");
        let n3 = result
            .iter()
            .find(|x| x.id == nid(3))
            .expect("node 3 missing");
        assert_eq!(n3.label, "c");
    }

    // -----------------------------------------------------------------------
    // Group 6: Structural edge cases
    // -----------------------------------------------------------------------

    /// A single node with a self-loop has degree 2 and is the sole result.
    #[test]
    fn single_node_self_loop_degree_2() {
        let mut g = Graph::new();
        g.nodes.push(node(1));
        g.edges.push(edge(1, 1));
        let result = god_nodes(&g, 1);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].degree, 2);
    }

    /// A node with a self-loop ranks above an isolated node (degree 2 vs. degree 0).
    #[test]
    fn self_loop_node_ranks_above_isolated() {
        let mut g = Graph::new();
        g.nodes.push(node(10)); // isolated
        g.nodes.push(node(1)); // self-loop
        g.edges.push(edge(1, 1));
        let result = god_nodes(&g, 2);
        assert_eq!(result[0].id, nid(1));
        assert_eq!(result[0].degree, 2);
        assert_eq!(result[1].id, nid(10));
        assert_eq!(result[1].degree, 0);
    }

    /// A bidirectional edge pair (`a → b` and `b → a`) gives both nodes degree 2.
    #[test]
    fn bidirectional_pair_both_degree_2() {
        let mut g = Graph::new();
        g.nodes.push(node(1));
        g.nodes.push(node(2));
        g.edges.push(edge(1, 2));
        g.edges.push(edge(2, 1));
        let result = god_nodes(&g, 2);
        assert_eq!(result[0].degree, 2);
        assert_eq!(result[1].degree, 2);
        // Tiebreak: node 1 < node 2.
        assert_eq!(result[0].id, nid(1));
    }

    /// Edges with any `Confidence` level are counted for degree purposes.
    #[test]
    fn all_confidence_levels_count_for_degree() {
        let mut g = Graph::new();
        for i in 1..=4 {
            g.nodes.push(node(i));
        }
        g.edges.push(Edge {
            source: nid(1),
            target: nid(2),
            relation: "x".into(),
            confidence: Confidence::Extracted,
        });
        g.edges.push(Edge {
            source: nid(1),
            target: nid(3),
            relation: "y".into(),
            confidence: Confidence::Inferred,
        });
        g.edges.push(Edge {
            source: nid(1),
            target: nid(4),
            relation: "z".into(),
            confidence: Confidence::Ambiguous,
        });
        let result = god_nodes(&g, 1);
        assert_eq!(result[0].id, nid(1));
        assert_eq!(result[0].degree, 3);
    }

    /// A graph with only edges and no declared nodes returns ghost nodes with empty labels.
    #[test]
    fn no_declared_nodes_ghost_nodes_get_empty_label() {
        let mut g = Graph::new();
        // ghost nodes: 1 and 2 — neither declared in graph.nodes.
        g.edges.push(edge(1, 2));
        let result = god_nodes(&g, 2);
        // Both ghost nodes must appear.
        assert_eq!(result.len(), 2);
        for gn in &result {
            assert_eq!(gn.label, "", "ghost node label must be empty");
        }
    }

    /// A 10-spoke star graph: the center must be rank 1 with degree 10.
    #[test]
    fn large_star_center_is_rank_1() {
        let center = 1_u32;
        let mut g = Graph::new();
        g.nodes.push(node(center));
        for spoke in 2..=11 {
            g.nodes.push(node(spoke));
            g.edges.push(edge(center, spoke));
        }
        let result = god_nodes(&g, 1);
        assert_eq!(result[0].id, nid(center));
        assert_eq!(result[0].degree, 10);
    }

    /// Endpoint nodes of a long chain have the lowest degree.
    #[test]
    fn chain_endpoints_rank_last() {
        // Chain: 1 → 2 → 3 → 4 → 5; degrees: 1:1, 2:2, 3:2, 4:2, 5:1.
        let mut g = Graph::new();
        for i in 1..=5 {
            g.nodes.push(node(i));
        }
        for i in 1..=4 {
            g.edges.push(edge(i, i + 1));
        }
        let result = god_nodes(&g, 5);
        // Last two entries must have degree 1 (the endpoints).
        assert_eq!(result[3].degree, 1);
        assert_eq!(result[4].degree, 1);
    }

    // -----------------------------------------------------------------------
    // Group 7: Determinism (R4)
    // -----------------------------------------------------------------------

    /// Calling `god_nodes` twice on the same graph produces identical results.
    #[test]
    fn determinism_same_output_on_repeated_calls() {
        let mut g = Graph::new();
        for id in [7, 3, 1, 9, 5] {
            g.nodes.push(node(id));
        }
        g.edges.push(edge(1, 3));
        g.edges.push(edge(9, 3));
        g.edges.push(edge(3, 7));
        let first = god_nodes(&g, 5);
        let second = god_nodes(&g, 5);
        assert_eq!(first, second);
    }

    /// Nodes inserted in different order produce the same result (no `HashMap` order leak).
    #[test]
    fn determinism_independent_of_node_insertion_order() {
        let build = |order: &[u32]| {
            let mut g = Graph::new();
            for &id in order {
                g.nodes.push(node(id));
            }
            g.edges.push(edge(2, 1));
            g.edges.push(edge(3, 1));
            god_nodes(&g, 3)
        };
        let a = build(&[1, 2, 3]);
        let b = build(&[3, 1, 2]);
        assert_eq!(a, b);
    }

    /// Edges added in different order produce the same result.
    #[test]
    fn determinism_independent_of_edge_insertion_order() {
        let build = |edge_order: &[(u32, u32)]| {
            let mut g = Graph::new();
            for i in 1..=4 {
                g.nodes.push(node(i));
            }
            for &(s, t) in edge_order {
                g.edges.push(edge(s, t));
            }
            god_nodes(&g, 4)
        };
        let pairs = [(1, 2), (1, 3), (2, 4), (3, 4)];
        let mut rev_pairs = pairs;
        rev_pairs.reverse();
        assert_eq!(build(&pairs), build(&rev_pairs));
    }

    /// A 50-node graph produces identical output on back-to-back calls.
    #[test]
    fn determinism_large_graph() {
        let mut g = Graph::new();
        for i in 1..=50 {
            g.nodes.push(node(i));
        }
        // Create a pseudo-random-ish edge set deterministically.
        for i in 1..=49 {
            g.edges.push(edge(i, i + 1));
            if i % 7 == 0 {
                g.edges.push(edge(i, 1));
            }
        }
        let first = god_nodes(&g, 10);
        let second = god_nodes(&g, 10);
        assert_eq!(first, second);
    }

    // -----------------------------------------------------------------------
    // Group 8: GodNode struct behaviour
    // -----------------------------------------------------------------------

    /// Two `GodNode`s with identical fields are equal.
    #[test]
    fn godnode_equality() {
        let a = GodNode {
            id: nid(1),
            label: "foo".into(),
            degree: 3,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    /// Differing `id` makes `GodNode`s unequal.
    #[test]
    fn godnode_ne_different_id() {
        let a = GodNode {
            id: nid(1),
            label: "x".into(),
            degree: 1,
        };
        let b = GodNode {
            id: nid(2),
            label: "x".into(),
            degree: 1,
        };
        assert_ne!(a, b);
    }

    /// Differing `label` makes `GodNode`s unequal.
    #[test]
    fn godnode_ne_different_label() {
        let a = GodNode {
            id: nid(1),
            label: "alpha".into(),
            degree: 1,
        };
        let b = GodNode {
            id: nid(1),
            label: "beta".into(),
            degree: 1,
        };
        assert_ne!(a, b);
    }

    /// Differing `degree` makes `GodNode`s unequal.
    #[test]
    fn godnode_ne_different_degree() {
        let a = GodNode {
            id: nid(1),
            label: "x".into(),
            degree: 2,
        };
        let b = GodNode {
            id: nid(1),
            label: "x".into(),
            degree: 5,
        };
        assert_ne!(a, b);
    }

    /// `Clone` produces a deep-equal, independent copy.
    #[test]
    fn godnode_clone_is_equal() {
        let a = GodNode {
            id: nid(42),
            label: "cloned".into(),
            degree: 7,
        };
        let b = a.clone();
        assert_eq!(a, b);
        assert_eq!(b.id, nid(42));
        assert_eq!(b.label, "cloned");
        assert_eq!(b.degree, 7);
    }

    // -----------------------------------------------------------------------
    // Group 9: Additional / composite scenarios
    // -----------------------------------------------------------------------

    /// In a 100-node graph, `top_n` = 1 returns exactly one element.
    #[test]
    fn top_n_1_from_large_graph_returns_one() {
        let mut g = Graph::new();
        for i in 1..=100 {
            g.nodes.push(node(i));
        }
        // Node 1 is a super-hub pointing to nodes 2–50.
        for i in 2..=50 {
            g.edges.push(edge(1, i));
        }
        let result = god_nodes(&g, 1);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, nid(1));
    }

    /// When all nodes have degree 0 (no edges), `top_n` of them are returned, sorted by id.
    #[test]
    fn all_degree_zero_returns_top_n_by_id() {
        let mut g = Graph::new();
        for i in [5, 2, 8, 1, 9, 3] {
            g.nodes.push(node(i));
        }
        let result = god_nodes(&g, 3);
        assert_eq!(result.len(), 3);
        // All degree 0, so must be the three smallest ids in ascending order.
        let ids: Vec<u32> = result.iter().map(|x| x.id.get()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
        assert!(result.iter().all(|x| x.degree == 0));
    }

    /// A node with 3 incoming and 2 outgoing edges has total degree 5.
    #[test]
    fn in_plus_out_degree_correct() {
        let mut g = Graph::new();
        for i in 1..=6 {
            g.nodes.push(node(i));
        }
        // Node 1 gets 3 in-edges (from 2, 3, 4) and 2 out-edges (to 5, 6).
        g.edges.push(edge(2, 1));
        g.edges.push(edge(3, 1));
        g.edges.push(edge(4, 1));
        g.edges.push(edge(1, 5));
        g.edges.push(edge(1, 6));
        let result = god_nodes(&g, 1);
        assert_eq!(result[0].id, nid(1));
        assert_eq!(result[0].degree, 5);
    }

    /// Two nodes with the same out-degree but zero in-degree tie on degree;
    /// lower id wins the tiebreak.
    #[test]
    fn symmetric_pure_sources_tie_break() {
        let mut g = Graph::new();
        // Nodes 1 and 2 both emit one edge (no in-edges to each other).
        g.nodes.push(node(1));
        g.nodes.push(node(2));
        g.nodes.push(node(3));
        g.nodes.push(node(4));
        g.edges.push(edge(1, 3)); // node 1: degree 1
        g.edges.push(edge(2, 4)); // node 2: degree 1
        let result = god_nodes(&g, 2);
        // Nodes 1 and 2 tie at degree 1; node 1 wins tiebreak.
        assert_eq!(result[0].id, nid(1));
        assert_eq!(result[1].id, nid(2));
    }

    /// A 50-spoke star graph: center label is correct and center is always first.
    #[test]
    fn large_star_center_label_and_rank() {
        let center = 100_u32;
        let mut g = Graph::new();
        g.nodes.push(node_labelled(center, "super_hub"));
        for spoke in 1..=50 {
            g.nodes.push(node(spoke));
            g.edges.push(edge(center, spoke));
        }
        let result = god_nodes(&g, 1);
        assert_eq!(result[0].id, nid(center));
        assert_eq!(result[0].label, "super_hub");
        assert_eq!(result[0].degree, 50);
    }

    /// `top_n` exactly equal to node count returns all nodes.
    #[test]
    fn top_n_exactly_node_count_returns_all() {
        let mut g = Graph::new();
        for i in 1..=5 {
            g.nodes.push(node(i));
        }
        g.edges.push(edge(1, 2));
        let result = god_nodes(&g, 5);
        assert_eq!(result.len(), 5);
    }

    /// Two calls with the same graph but using a clone produce the same result.
    #[test]
    fn determinism_clone_graph_same_result() {
        let mut g = Graph::new();
        for i in [4, 2, 7, 1, 9] {
            g.nodes.push(node(i));
        }
        g.edges.push(edge(1, 2));
        g.edges.push(edge(2, 7));
        g.edges.push(edge(7, 9));
        let g2 = g.clone();
        assert_eq!(god_nodes(&g, 5), god_nodes(&g2, 5));
    }
}
