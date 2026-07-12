//! Surprising connections — trusted edges that bridge *different* communities (PD analytics, FO-9).
//!
//! A "surprising" connection is a high-confidence ([`Confidence::Extracted`]) edge whose endpoints
//! belong to two **distinct** Leiden communities — a cross-cluster bridge that an agent would not
//! expect from the local community structure alone.  These bridges signal unexpected coupling between
//! subsystems and are the primary input to cross-community arc-coherence analysis (S1008620).
//!
//! ## Inclusion criteria (all must hold)
//!
//! 1. The edge confidence is [`Confidence::Extracted`] (i.e. [`Confidence::is_trusted`] returns
//!    `true`).  `INFERRED` and `AMBIGUOUS` edges are excluded unconditionally.
//! 2. The source node is a member of at least one community in `graph.communities`.
//! 3. The target node is a member of at least one community in `graph.communities`.
//! 4. The source community and target community are **not the same**
//!    (`source_community ≠ target_community`).
//!
//! ## Output ordering (R4)
//!
//! Results appear in **`graph.edges` insertion order**.  No additional sort is applied so the
//! output is byte-identical on equivalent inputs and never leaks `HashMap` iteration order into the
//! returned slice.
//!
//! ## Relationship to other analytics
//!
//! [`surprising_connections`] is consumed by [`suggested_questions`] to generate exploration
//! prompts for agents, and its output feeds the PV2 arc-coherence gauge introduced in S1008620.
//!
//! [`Confidence::Extracted`]: habitat_graph_core::Confidence::Extracted
//! [`Confidence::is_trusted`]: habitat_graph_core::Confidence::is_trusted
//! [`suggested_questions`]: crate::questions::suggested_questions

use std::collections::HashMap;

use habitat_graph_core::{CommunityId, Graph, NodeId};

/// A trusted edge bridging two distinct communities.
///
/// Both [`source_community`](Self::source_community) and
/// [`target_community`](Self::target_community) are always [`Some`] in values returned by
/// [`surprising_connections`]; the `Option` wrapper preserves API flexibility for callers that
/// extend the type in derived analyses without a breaking change.
#[allow(clippy::module_name_repetitions)] // name fixed by contract (FO-9 public API)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurprisingConnection {
    /// Source node identifier.
    pub source: NodeId,
    /// Target node identifier.
    pub target: NodeId,
    /// The edge relation label (e.g. `"calls"`, `"imports"`, `"defines"`).
    pub relation: String,
    /// Community the source node belongs to.
    ///
    /// Always [`Some`] in results produced by [`surprising_connections`].
    pub source_community: Option<CommunityId>,
    /// Community the target node belongs to.
    ///
    /// Always [`Some`] in results produced by [`surprising_connections`].
    pub target_community: Option<CommunityId>,
}

/// Builds a `NodeId -> CommunityId` lookup from `graph.communities`.
///
/// When a `NodeId` appears in multiple community member lists (a structurally inconsistent graph)
/// the community that occurs last in `graph.communities` wins for that node, providing a
/// deterministic but arbitrary tie-break.
fn community_of(graph: &Graph) -> HashMap<NodeId, CommunityId> {
    graph
        .communities
        .iter()
        .flat_map(|c| c.members.iter().map(move |&m| (m, c.id)))
        .collect()
}

/// Returns all trusted edges that bridge two distinct communities, in [`Graph`]`::edges` order.
///
/// A "surprising" connection is a [`Confidence::Extracted`] edge whose source and target nodes
/// belong to **different** communities.  Edges where either endpoint has no community assignment,
/// where both endpoints share the same community, or whose confidence is not
/// [`Confidence::Extracted`] are silently excluded.
///
/// The output is deterministic (R4): iteration follows `graph.edges` insertion order and no
/// `HashMap` iteration order leaks into the result.
///
/// # Examples
///
/// ```
/// use habitat_graph_analyze::surprising_connections;
/// use habitat_graph_core::{Community, CommunityId, Confidence, Edge, Graph, NodeId};
///
/// let mut g = Graph::new();
/// g.communities.push(Community {
///     id: CommunityId::new(0),
///     label: "parser".into(),
///     members: vec![NodeId::new(1)],
/// });
/// g.communities.push(Community {
///     id: CommunityId::new(1),
///     label: "backend".into(),
///     members: vec![NodeId::new(2)],
/// });
/// g.edges.push(Edge {
///     source: NodeId::new(1),
///     target: NodeId::new(2),
///     relation: "calls".into(),
///     confidence: Confidence::Extracted,
/// });
///
/// let bridges = surprising_connections(&g);
/// assert_eq!(bridges.len(), 1);
/// assert_eq!(bridges[0].source, NodeId::new(1));
/// assert_eq!(bridges[0].target, NodeId::new(2));
/// assert_eq!(bridges[0].source_community, Some(CommunityId::new(0)));
/// assert_eq!(bridges[0].target_community, Some(CommunityId::new(1)));
/// ```
#[allow(clippy::module_name_repetitions)] // name fixed by contract (FO-9 public API)
#[must_use]
pub fn surprising_connections(graph: &Graph) -> Vec<SurprisingConnection> {
    let comm = community_of(graph);
    graph
        .edges
        .iter()
        .filter(|e| e.confidence.is_trusted())
        .filter_map(|e| {
            let sc = comm.get(&e.source).copied();
            let tc = comm.get(&e.target).copied();
            // Surprising only when both endpoints are assigned to *different* communities.
            match (sc, tc) {
                (Some(a), Some(b)) if a != b => Some(SurprisingConnection {
                    source: e.source,
                    target: e.target,
                    relation: e.relation.clone(),
                    source_community: sc,
                    target_community: tc,
                }),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use habitat_graph_core::{Community, CommunityId, Confidence, Edge, Graph, NodeId};

    use super::surprising_connections;

    // ── Helpers ───────────────────────────────────────────────────────────────────────────────

    /// Build a directed edge with the default relation `"calls"`.
    fn edge(s: u32, t: u32, c: Confidence) -> Edge {
        Edge {
            source: NodeId::new(s),
            target: NodeId::new(t),
            relation: "calls".into(),
            confidence: c,
        }
    }

    /// Build a directed edge with an explicit relation string.
    fn edge_rel(s: u32, t: u32, rel: &str, c: Confidence) -> Edge {
        Edge {
            source: NodeId::new(s),
            target: NodeId::new(t),
            relation: rel.into(),
            confidence: c,
        }
    }

    /// Build a community from a slice of raw node ids.
    fn comm(id: u32, members: &[u32]) -> Community {
        Community {
            id: CommunityId::new(id),
            label: format!("c{id}"),
            members: members.iter().copied().map(NodeId::new).collect(),
        }
    }

    fn nid(n: u32) -> NodeId {
        NodeId::new(n)
    }

    fn cid(c: u32) -> CommunityId {
        CommunityId::new(c)
    }

    // ── Group 1: Basic inclusion / exclusion ──────────────────────────────────────────────────

    /// No communities → no surprising connections, regardless of edges present.
    #[test]
    fn no_communities_yields_nothing() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, Confidence::Extracted));
        assert!(surprising_connections(&g).is_empty());
    }

    /// A trusted (`EXTRACTED`) edge between nodes in different communities is surprising.
    #[test]
    fn cross_community_trusted_edge_is_surprising() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        assert_eq!(surprising_connections(&g).len(), 1);
    }

    /// An extracted edge whose endpoints share a community is not surprising.
    #[test]
    fn same_community_edge_is_not_surprising() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1, 2]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        assert!(surprising_connections(&g).is_empty());
    }

    /// An `INFERRED` cross-community edge is excluded (only `EXTRACTED` is trusted).
    #[test]
    fn inferred_cross_community_edge_is_excluded() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges.push(edge(1, 2, Confidence::Inferred));
        assert!(surprising_connections(&g).is_empty());
    }

    /// An `AMBIGUOUS` cross-community edge is excluded.
    #[test]
    fn ambiguous_cross_community_edge_is_excluded() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges.push(edge(1, 2, Confidence::Ambiguous));
        assert!(surprising_connections(&g).is_empty());
    }

    /// Three edges with all confidence levels on the same cross-community pair: only `EXTRACTED`
    /// makes it into the output.
    #[test]
    fn all_three_confidences_only_extracted_returned() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        g.edges.push(edge(1, 2, Confidence::Inferred));
        g.edges.push(edge(1, 2, Confidence::Ambiguous));
        let result = surprising_connections(&g);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_community, Some(cid(0)));
        assert_eq!(result[0].target_community, Some(cid(1)));
    }

    // ── Group 2: Missing community membership ─────────────────────────────────────────────────

    /// Source endpoint has no community assignment → excluded even for trusted edges.
    #[test]
    fn source_missing_from_communities_excluded() {
        let mut g = Graph::new();
        // Only node 2 has a community; node 1 does not.
        g.communities.push(comm(0, &[2]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        assert!(surprising_connections(&g).is_empty());
    }

    /// Target endpoint has no community assignment → excluded.
    #[test]
    fn target_missing_from_communities_excluded() {
        let mut g = Graph::new();
        // Only node 1 has a community; node 2 does not.
        g.communities.push(comm(0, &[1]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        assert!(surprising_connections(&g).is_empty());
    }

    /// Both endpoints have no community assignment → excluded.
    #[test]
    fn both_endpoints_missing_from_communities_excluded() {
        let mut g = Graph::new();
        // Communities exist but neither node 1 nor node 2 is a member.
        g.communities.push(comm(0, &[99]));
        g.communities.push(comm(1, &[100]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        assert!(surprising_connections(&g).is_empty());
    }

    // ── Group 3: Empty / minimal graph scenarios ──────────────────────────────────────────────

    /// An entirely empty graph produces no output.
    #[test]
    fn empty_graph_yields_nothing() {
        assert!(surprising_connections(&Graph::new()).is_empty());
    }

    /// Communities present but no edges → no output.
    #[test]
    fn communities_only_no_edges_yields_nothing() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1, 2]));
        g.communities.push(comm(1, &[3]));
        assert!(surprising_connections(&g).is_empty());
    }

    /// Edges present but no communities → no output (no membership to classify against).
    #[test]
    fn edges_only_no_communities_yields_nothing() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, Confidence::Extracted));
        g.edges.push(edge(3, 4, Confidence::Extracted));
        assert!(surprising_connections(&g).is_empty());
    }

    /// A single community with members but zero edges → no output.
    #[test]
    fn single_community_no_edges_yields_nothing() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[42]));
        assert!(surprising_connections(&g).is_empty());
    }

    // ── Group 4: Output field correctness ─────────────────────────────────────────────────────

    /// The `source` field matches the edge source id exactly.
    #[test]
    fn result_preserves_source_id() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[7]));
        g.communities.push(comm(1, &[13]));
        g.edges.push(edge(7, 13, Confidence::Extracted));
        let result = surprising_connections(&g);
        assert_eq!(result[0].source, nid(7));
    }

    /// The `target` field matches the edge target id exactly (not the source).
    #[test]
    fn result_preserves_target_id() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[7]));
        g.communities.push(comm(1, &[13]));
        g.edges.push(edge(7, 13, Confidence::Extracted));
        let result = surprising_connections(&g);
        assert_eq!(result[0].target, nid(13));
    }

    /// Source and target ids are never swapped in the output.
    #[test]
    fn source_and_target_not_swapped() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[5]));
        g.communities.push(comm(1, &[9]));
        g.edges.push(edge(5, 9, Confidence::Extracted));
        let result = surprising_connections(&g);
        assert_eq!(
            result[0].source,
            nid(5),
            "source must be 5, not swapped with target"
        );
        assert_eq!(
            result[0].target,
            nid(9),
            "target must be 9, not swapped with source"
        );
    }

    /// The `relation` field is cloned verbatim from the edge's relation string.
    #[test]
    fn result_preserves_relation_string() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges
            .push(edge_rel(1, 2, "imports", Confidence::Extracted));
        let result = surprising_connections(&g);
        assert_eq!(result[0].relation, "imports");
    }

    /// `source_community` holds the community id that the source node belongs to.
    #[test]
    fn source_community_matches_actual_community_id() {
        let mut g = Graph::new();
        g.communities.push(comm(10, &[1]));
        g.communities.push(comm(20, &[2]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        let result = surprising_connections(&g);
        assert_eq!(result[0].source_community, Some(cid(10)));
    }

    /// `target_community` holds the community id that the target node belongs to.
    #[test]
    fn target_community_matches_actual_community_id() {
        let mut g = Graph::new();
        g.communities.push(comm(10, &[1]));
        g.communities.push(comm(20, &[2]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        let result = surprising_connections(&g);
        assert_eq!(result[0].target_community, Some(cid(20)));
    }

    /// `source_community` is always `Some` in returned results (never `None`).
    #[test]
    fn source_community_is_some_in_output() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        let result = surprising_connections(&g);
        assert!(result[0].source_community.is_some());
    }

    /// `target_community` is always `Some` in returned results (never `None`).
    #[test]
    fn target_community_is_some_in_output() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        let result = surprising_connections(&g);
        assert!(result[0].target_community.is_some());
    }

    // ── Group 5: Ordering and determinism (R4) ────────────────────────────────────────────────

    /// Output preserves `graph.edges` insertion order — no implicit sorting by node id.
    #[test]
    fn output_preserves_edges_insertion_order() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.communities.push(comm(2, &[3]));
        // Insert edges with *descending* source ids; output must mirror this order.
        g.edges.push(edge(3, 2, Confidence::Extracted)); // comms 2→1
        g.edges.push(edge(1, 2, Confidence::Extracted)); // comms 0→1
        let result = surprising_connections(&g);
        assert_eq!(result.len(), 2);
        assert_eq!(
            result[0].source,
            nid(3),
            "first result must match first edge"
        );
        assert_eq!(
            result[1].source,
            nid(1),
            "second result must match second edge"
        );
    }

    /// Non-surprising edges interleaved with surprising ones do not shift the order of bridges.
    #[test]
    fn interleaved_surprising_and_non_surprising_order_preserved() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1, 2]));
        g.communities.push(comm(1, &[3]));
        // Interleave: surprising, same-community, surprising.
        g.edges.push(edge(1, 3, Confidence::Extracted)); // bridge (index 0 in result)
        g.edges.push(edge(1, 2, Confidence::Extracted)); // same community, filtered out
        g.edges.push(edge(2, 3, Confidence::Extracted)); // bridge (index 1 in result)
        let result = surprising_connections(&g);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].source, nid(1));
        assert_eq!(result[1].source, nid(2));
    }

    /// Calling `surprising_connections` twice on the same graph yields byte-identical output.
    #[test]
    fn result_is_deterministic_across_repeated_calls() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1, 2]));
        g.communities.push(comm(1, &[3, 4]));
        g.communities.push(comm(2, &[5]));
        g.edges.push(edge(1, 3, Confidence::Extracted));
        g.edges.push(edge(2, 4, Confidence::Inferred));
        g.edges.push(edge(3, 5, Confidence::Extracted));
        g.edges.push(edge(4, 2, Confidence::Extracted));
        let first = surprising_connections(&g);
        let second = surprising_connections(&g);
        assert_eq!(
            first, second,
            "surprising_connections must be deterministic"
        );
    }

    // ── Group 6: Self-loop handling ───────────────────────────────────────────────────────────

    /// A trusted self-loop (`a → a`) where node `a` is in a community is never surprising because
    /// source and target resolve to the same community.
    #[test]
    fn trusted_self_loop_in_community_not_surprising() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.edges.push(edge(1, 1, Confidence::Extracted));
        assert!(surprising_connections(&g).is_empty());
    }

    /// A trusted self-loop on a node that belongs to no community is excluded.
    #[test]
    fn trusted_self_loop_no_community_excluded() {
        let mut g = Graph::new();
        // No communities: self-loop on an uncommitted node.
        g.edges.push(edge(7, 7, Confidence::Extracted));
        assert!(surprising_connections(&g).is_empty());
    }

    // ── Group 7: Multiple bridges ─────────────────────────────────────────────────────────────

    /// Two trusted edges bridging the same community pair are both returned.
    #[test]
    fn two_bridges_between_two_communities_both_returned() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1, 2]));
        g.communities.push(comm(1, &[3, 4]));
        g.edges.push(edge(1, 3, Confidence::Extracted));
        g.edges.push(edge(2, 4, Confidence::Extracted));
        assert_eq!(surprising_connections(&g).len(), 2);
    }

    /// Three trusted cross-community edges return three results.
    #[test]
    fn three_cross_community_edges_all_returned() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.communities.push(comm(2, &[3]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        g.edges.push(edge(2, 3, Confidence::Extracted));
        g.edges.push(edge(1, 3, Confidence::Extracted));
        assert_eq!(surprising_connections(&g).len(), 3);
    }

    /// A hub node in its own community with trusted edges to four other communities → four bridges.
    #[test]
    fn star_hub_bridging_four_communities() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[10]));
        for i in 1_u32..=4 {
            g.communities.push(comm(i, &[i * 100]));
            g.edges.push(edge(10, i * 100, Confidence::Extracted));
        }
        assert_eq!(surprising_connections(&g).len(), 4);
    }

    /// Three communities in a ring (each pair connected by a bridge) → three bridges.
    #[test]
    fn triangle_of_communities_all_bridges() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.communities.push(comm(2, &[3]));
        g.edges.push(edge(1, 2, Confidence::Extracted)); // comms 0→1
        g.edges.push(edge(2, 3, Confidence::Extracted)); // comms 1→2
        g.edges.push(edge(3, 1, Confidence::Extracted)); // comms 2→0
        assert_eq!(surprising_connections(&g).len(), 3);
    }

    // ── Group 8: Large communities ────────────────────────────────────────────────────────────

    /// Many internal edges within a large community plus one bridge → exactly one surprising result.
    #[test]
    fn large_community_internal_edges_not_surprising() {
        let mut g = Graph::new();
        let members_a: Vec<u32> = (1..=10).collect();
        g.communities.push(comm(0, &members_a));
        g.communities.push(comm(1, &[99]));
        // All internal edges within community 0.
        for i in 1_u32..10 {
            g.edges.push(edge(i, i + 1, Confidence::Extracted));
        }
        // One cross-community bridge.
        g.edges.push(edge(5, 99, Confidence::Extracted));
        let result = surprising_connections(&g);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source, nid(5));
        assert_eq!(result[0].target, nid(99));
    }

    /// Two communities with multiple internal edges and exactly two bridges.
    #[test]
    fn two_communities_many_internal_two_bridges() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1, 2, 3]));
        g.communities.push(comm(1, &[4, 5, 6]));
        // Internal edges only.
        for (s, t) in [(1_u32, 2_u32), (2, 3), (4, 5), (5, 6)] {
            g.edges.push(edge(s, t, Confidence::Extracted));
        }
        // Two bridges.
        g.edges.push(edge(3, 4, Confidence::Extracted));
        g.edges.push(edge(1, 5, Confidence::Extracted));
        assert_eq!(surprising_connections(&g).len(), 2);
    }

    // ── Group 9: Relation string variety ─────────────────────────────────────────────────────

    /// `"imports"` relation is cloned verbatim.
    #[test]
    fn relation_imports_preserved() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges
            .push(edge_rel(1, 2, "imports", Confidence::Extracted));
        assert_eq!(surprising_connections(&g)[0].relation, "imports");
    }

    /// `"defines"` relation is cloned verbatim.
    #[test]
    fn relation_defines_preserved() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges
            .push(edge_rel(1, 2, "defines", Confidence::Extracted));
        assert_eq!(surprising_connections(&g)[0].relation, "defines");
    }

    /// An empty string relation is preserved verbatim.
    #[test]
    fn relation_empty_string_preserved() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges.push(edge_rel(1, 2, "", Confidence::Extracted));
        assert_eq!(surprising_connections(&g)[0].relation, "");
    }

    /// A relation with Unicode / punctuation characters is preserved verbatim.
    #[test]
    fn relation_unicode_characters_preserved() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges
            .push(edge_rel(1, 2, "calls → via::trait", Confidence::Extracted));
        assert_eq!(surprising_connections(&g)[0].relation, "calls → via::trait");
    }

    // ── Group 10: Mixed trusted/untrusted ─────────────────────────────────────────────────────

    /// Trusted cross-community edge plus `INFERRED` cross-community edge → only trusted returned.
    #[test]
    fn mix_trusted_and_inferred_only_trusted_returned() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        g.edges.push(edge(1, 2, Confidence::Inferred));
        let result = surprising_connections(&g);
        assert_eq!(
            result.len(),
            1,
            "only the EXTRACTED edge is a surprising connection"
        );
    }

    /// Trusted cross-community edge plus `AMBIGUOUS` cross-community edge → only trusted returned.
    #[test]
    fn mix_trusted_and_ambiguous_only_trusted_returned() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        g.edges.push(edge(1, 2, Confidence::Ambiguous));
        assert_eq!(surprising_connections(&g).len(), 1);
    }

    /// Extracted same-community edges plus untrusted cross-community edges → nothing returned.
    #[test]
    fn in_community_extracted_plus_cross_community_untrusted_yields_nothing() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1, 2]));
        g.communities.push(comm(1, &[3]));
        g.edges.push(edge(1, 2, Confidence::Extracted)); // same community: not surprising
        g.edges.push(edge(1, 3, Confidence::Inferred)); // cross-community but untrusted
        assert!(surprising_connections(&g).is_empty());
    }

    // ── Group 11: Direction ───────────────────────────────────────────────────────────────────

    /// The reverse-direction edge (target→source) is also surprising if both nodes are in
    /// different communities and the edge is trusted.
    #[test]
    fn reverse_direction_edge_also_surprising() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges.push(edge(1, 2, Confidence::Extracted)); // forward bridge
        g.edges.push(edge(2, 1, Confidence::Extracted)); // reverse bridge
        let result = surprising_connections(&g);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].source, nid(1));
        assert_eq!(result[1].source, nid(2));
    }

    // ── Group 12: Community id correctness ───────────────────────────────────────────────────

    /// Both community ids in the output match the actual community assignments precisely.
    #[test]
    fn community_ids_correctly_assigned() {
        let mut g = Graph::new();
        g.communities.push(comm(42, &[1]));
        g.communities.push(comm(99, &[2]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        let result = surprising_connections(&g);
        assert_eq!(result[0].source_community, Some(cid(42)));
        assert_eq!(result[0].target_community, Some(cid(99)));
    }

    /// Community ids near `u32::MAX` are handled correctly without overflow.
    #[test]
    fn large_community_ids_work() {
        let large = u32::MAX - 1;
        let mut g = Graph::new();
        g.communities.push(Community {
            id: CommunityId::new(large),
            label: "big-a".into(),
            members: vec![nid(1)],
        });
        g.communities.push(Community {
            id: CommunityId::new(u32::MAX),
            label: "big-b".into(),
            members: vec![nid(2)],
        });
        g.edges.push(edge(1, 2, Confidence::Extracted));
        let result = surprising_connections(&g);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_community, Some(CommunityId::new(large)));
        assert_eq!(result[0].target_community, Some(CommunityId::new(u32::MAX)));
    }

    /// `CommunityId(0)` is a valid community identifier.
    #[test]
    fn community_id_zero_works() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        let result = surprising_connections(&g);
        assert_eq!(result[0].source_community, Some(cid(0)));
    }

    // ── Group 13: Node id edge cases ──────────────────────────────────────────────────────────

    /// `NodeId(0)` is a valid node identifier as both source and target.
    #[test]
    fn node_id_zero_works() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[0]));
        g.communities.push(comm(1, &[1]));
        g.edges.push(edge(0, 1, Confidence::Extracted));
        let result = surprising_connections(&g);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source, nid(0));
        assert_eq!(result[0].target, nid(1));
    }

    /// `NodeId` values near `u32::MAX` are handled correctly.
    #[test]
    fn large_node_ids_work() {
        let big = u32::MAX;
        let mut g = Graph::new();
        g.communities.push(comm(0, &[big]));
        g.communities.push(comm(1, &[big - 1]));
        g.edges.push(Edge {
            source: nid(big),
            target: nid(big - 1),
            relation: "link".into(),
            confidence: Confidence::Extracted,
        });
        let result = surprising_connections(&g);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source, nid(big));
        assert_eq!(result[0].target, nid(big - 1));
    }

    // ── Group 14: Result length / count invariants ────────────────────────────────────────────

    /// Result length equals exactly the count of trusted cross-community edges.
    #[test]
    fn result_length_equals_trusted_cross_community_count() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1, 2]));
        g.communities.push(comm(1, &[3, 4]));
        // 2 trusted bridges + 1 untrusted bridge + 2 same-community trusted.
        g.edges.push(edge(1, 3, Confidence::Extracted)); // bridge
        g.edges.push(edge(2, 4, Confidence::Extracted)); // bridge
        g.edges.push(edge(1, 4, Confidence::Inferred)); // untrusted → excluded
        g.edges.push(edge(1, 2, Confidence::Extracted)); // same community → not surprising
        g.edges.push(edge(3, 4, Confidence::Extracted)); // same community → not surprising
        assert_eq!(surprising_connections(&g).len(), 2);
    }

    /// A single community means no cross-community edge is possible → no bridges.
    #[test]
    fn single_community_produces_no_bridges() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1, 2, 3, 4]));
        for (s, t) in [(1_u32, 2_u32), (2, 3), (3, 4), (4, 1)] {
            g.edges.push(edge(s, t, Confidence::Extracted));
        }
        assert!(surprising_connections(&g).is_empty());
    }

    /// All edges trusted and all cross-community → all are surprising.
    #[test]
    fn all_trusted_cross_community_all_surprising() {
        let mut g = Graph::new();
        for i in 0_u32..5 {
            g.communities.push(comm(i, &[i * 10]));
        }
        // Chain: 0→10, 10→20, 20→30, 30→40 (all cross-community).
        for i in 0_u32..4 {
            g.edges
                .push(edge(i * 10, (i + 1) * 10, Confidence::Extracted));
        }
        assert_eq!(surprising_connections(&g).len(), 4);
    }

    /// No trusted edges at all → no surprising connections.
    #[test]
    fn no_trusted_edges_yields_nothing() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges.push(edge(1, 2, Confidence::Inferred));
        g.edges.push(edge(1, 2, Confidence::Ambiguous));
        assert!(surprising_connections(&g).is_empty());
    }

    // ── Group 15: Multi-relation parallel edges ───────────────────────────────────────────────

    /// Multiple parallel trusted edges (different relations) between the same cross-community pair
    /// are all included, preserving insertion order and relation strings.
    #[test]
    fn multiple_parallel_trusted_edges_all_returned() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        for rel in ["calls", "imports", "inherits"] {
            g.edges.push(edge_rel(1, 2, rel, Confidence::Extracted));
        }
        let result = surprising_connections(&g);
        assert_eq!(result.len(), 3);
        let relations: Vec<&str> = result.iter().map(|c| c.relation.as_str()).collect();
        assert_eq!(relations, vec!["calls", "imports", "inherits"]);
    }

    // ── Group 16: Clone / equality on `SurprisingConnection` ─────────────────────────────────

    /// `SurprisingConnection` clones correctly and `PartialEq` holds between clone and original.
    #[test]
    fn surprising_connection_clone_eq() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        let result = surprising_connections(&g);
        let cloned = result.clone();
        assert_eq!(result, cloned);
    }

    /// Two `SurprisingConnection` values with different community ids compare unequal.
    #[test]
    fn surprising_connection_neq_on_different_community() {
        let mut g1 = Graph::new();
        g1.communities.push(comm(0, &[1]));
        g1.communities.push(comm(1, &[2]));
        g1.edges.push(edge(1, 2, Confidence::Extracted));

        let mut g2 = Graph::new();
        g2.communities.push(comm(0, &[1]));
        g2.communities.push(comm(5, &[2])); // different community id for node 2
        g2.edges.push(edge(1, 2, Confidence::Extracted));

        assert_ne!(surprising_connections(&g1), surprising_connections(&g2));
    }

    // ── Group 17: Community with empty member list ────────────────────────────────────────────

    /// An empty community (no members) contributes no mappings and does not cause a panic.
    #[test]
    fn empty_community_member_list_no_crash() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[])); // empty: contributes nothing
        g.communities.push(comm(1, &[1]));
        g.communities.push(comm(2, &[2]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        // Nodes 1 and 2 are in communities 1 and 2 → one bridge.
        assert_eq!(surprising_connections(&g).len(), 1);
    }

    // ── Group 18: Nodes with community but no edges ───────────────────────────────────────────

    /// Multiple communities and members but zero edges → no surprising connections.
    #[test]
    fn communited_nodes_no_edges_yields_nothing() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1, 2, 3]));
        g.communities.push(comm(1, &[4, 5]));
        assert!(surprising_connections(&g).is_empty());
    }

    // ── Group 19: Many singleton communities ──────────────────────────────────────────────────

    /// Ten singleton communities, one edge per adjacent pair → nine bridges.
    #[test]
    fn ten_singleton_communities_chain_yields_nine_bridges() {
        let mut g = Graph::new();
        for i in 0_u32..10 {
            g.communities.push(comm(i, &[i]));
        }
        for i in 0_u32..9 {
            g.edges.push(edge(i, i + 1, Confidence::Extracted));
        }
        assert_eq!(surprising_connections(&g).len(), 9);
    }

    // ── Group 20: Debug representation ───────────────────────────────────────────────────────

    /// `SurprisingConnection` implements `Debug` without panicking and includes the type name.
    #[test]
    fn surprising_connection_debug_does_not_panic() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2]));
        g.edges.push(edge(1, 2, Confidence::Extracted));
        let result = surprising_connections(&g);
        let debug_str = format!("{:?}", result[0]);
        assert!(debug_str.contains("SurprisingConnection"));
    }

    // ── Group 21: Community membership at maximum id values ───────────────────────────────────

    /// The result count never over-counts when both endpoints are in the same high-id community.
    #[test]
    fn high_id_same_community_not_surprising() {
        let max = u32::MAX;
        let mut g = Graph::new();
        g.communities.push(Community {
            id: CommunityId::new(max),
            label: "top".into(),
            members: vec![nid(1), nid(2)],
        });
        g.edges.push(edge(1, 2, Confidence::Extracted));
        assert!(surprising_connections(&g).is_empty());
    }

    /// Mixed graph: some in-community, some untrusted bridges, some genuine bridges.
    /// The count must match precisely.
    #[test]
    fn comprehensive_mixed_graph_exact_count() {
        let mut g = Graph::new();
        g.communities.push(comm(0, &[1, 2]));
        g.communities.push(comm(1, &[3, 4]));
        g.communities.push(comm(2, &[5]));
        // Same-community (not surprising): 3 edges
        g.edges.push(edge(1, 2, Confidence::Extracted));
        g.edges.push(edge(3, 4, Confidence::Extracted));
        g.edges.push(edge(3, 4, Confidence::Inferred));
        // Cross-community untrusted (excluded): 2 edges
        g.edges.push(edge(1, 3, Confidence::Inferred));
        g.edges.push(edge(2, 5, Confidence::Ambiguous));
        // Cross-community trusted (surprising): 3 edges
        g.edges.push(edge(1, 3, Confidence::Extracted));
        g.edges.push(edge(2, 4, Confidence::Extracted));
        g.edges.push(edge(4, 5, Confidence::Extracted));
        // One endpoint missing from any community: excluded
        g.edges.push(edge(99, 3, Confidence::Extracted)); // node 99 has no community
        assert_eq!(surprising_connections(&g).len(), 3);
    }
}
