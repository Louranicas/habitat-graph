//! Confidence gate — trusted/untrusted edge partitioning for the habitat-graph pipeline.
//!
//! This module is the T1/F12 filter (S1008901). The pipeline calls [`trusted_subgraph`] *before*
//! [`detect_communities`](crate::cluster::detect_communities) so that `INFERRED`/`AMBIGUOUS`
//! deep-mode edges never corrupt the Leiden community topology. The complement view produced by
//! [`untrusted_subgraph`] is preserved for human-export and review workflows.
//!
//! # Partition guarantee
//!
//! [`trusted_subgraph`] and [`untrusted_subgraph`] produce a **strict partition** of the edge set:
//!
//! - Their union equals the original edge set (no edges are lost).
//! - Their intersection is empty (no edge appears in both).
//! - `trusted_subgraph(g).edges.len() + untrusted_subgraph(g).edges.len() == g.edges.len()`.
//!
//! Nodes, communities, and the manifest are **cloned unchanged** by both operations — only the
//! edge collection is filtered.
//!
//! # Determinism (R4)
//!
//! All three public functions are pure and deterministic. They use only `clone` + `retain` (or
//! in-order iteration), never hash-based structures. Calling any function twice on the same input
//! yields byte-identical results.

use habitat_graph_core::{Confidence, Graph};

/// Per-confidence edge histogram returned by [`confidence_counts`].
///
/// The three fields count edges by their [`Confidence`] tier. Their sum always equals the total
/// edge count:
///
/// ```text
/// counts.extracted + counts.inferred + counts.ambiguous == graph.edges.len()
/// ```
///
/// See [`ConfidenceCounts::total`] for the convenience aggregation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfidenceCounts {
    /// Number of [`Confidence::Extracted`] edges.
    pub extracted: usize,
    /// Number of [`Confidence::Inferred`] edges.
    pub inferred: usize,
    /// Number of [`Confidence::Ambiguous`] edges.
    pub ambiguous: usize,
}

impl ConfidenceCounts {
    /// Returns `extracted + inferred + ambiguous`.
    ///
    /// This value always equals `graph.edges.len()` for the graph passed to [`confidence_counts`].
    #[must_use]
    pub fn total(self) -> usize {
        self.extracted + self.inferred + self.ambiguous
    }
}

/// Returns a copy of `graph` retaining only load-bearing [`Confidence::Extracted`] edges.
///
/// Nodes, communities, and manifest are **cloned unchanged**. Only the edge collection is
/// filtered. This is the subgraph that should be passed to
/// [`detect_communities`](crate::cluster::detect_communities): `INFERRED` and `AMBIGUOUS` deep-mode
/// edges inflate node degree and can merge Leiden communities that should remain separate,
/// corrupting the community → PV2-sphere topology.
///
/// # Idempotency
///
/// `trusted_subgraph(trusted_subgraph(g)) == trusted_subgraph(g)` — filtering an
/// already-trusted subgraph is a no-op.
///
/// # Determinism
///
/// The relative order of surviving edges is preserved from the input.
#[must_use]
pub fn trusted_subgraph(graph: &Graph) -> Graph {
    filter_edges(graph, Confidence::is_trusted)
}

/// Returns a copy of `graph` retaining only [`Confidence::Inferred`] and
/// [`Confidence::Ambiguous`] edges — the complement of [`trusted_subgraph`].
///
/// Nodes, communities, and manifest are **cloned unchanged**. Only the edge collection is
/// filtered. This subgraph contains the deep-mode exploration edges that are unsuitable for
/// automated topology analysis but are valuable for human review.
///
/// Together with [`trusted_subgraph`], this function partitions the edge set: every edge in the
/// input appears in exactly one of the two subgraphs and in neither the other.
///
/// # Idempotency
///
/// `untrusted_subgraph(untrusted_subgraph(g)) == untrusted_subgraph(g)`.
///
/// # Determinism
///
/// The relative order of surviving edges is preserved from the input.
#[must_use]
pub fn untrusted_subgraph(graph: &Graph) -> Graph {
    filter_edges(graph, |c| !c.is_trusted())
}

/// Returns the per-confidence edge histogram for `graph`.
///
/// The returned [`ConfidenceCounts`] has:
///
/// - `extracted` — count of [`Confidence::Extracted`] edges.
/// - `inferred` — count of [`Confidence::Inferred`] edges.
/// - `ambiguous` — count of [`Confidence::Ambiguous`] edges.
///
/// The three fields always sum to `graph.edges.len()`. This feeds pipeline diagnostics, the
/// `/health` endpoint edge-breakdown field, and the `graph.json` export report.
///
/// # Determinism
///
/// The result depends only on edge confidence values and is order-insensitive.
#[must_use]
pub fn confidence_counts(graph: &Graph) -> ConfidenceCounts {
    let mut extracted = 0_usize;
    let mut inferred = 0_usize;
    let mut ambiguous = 0_usize;

    for edge in &graph.edges {
        match edge.confidence {
            Confidence::Extracted => extracted += 1,
            Confidence::Inferred => inferred += 1,
            Confidence::Ambiguous => ambiguous += 1,
        }
    }

    ConfidenceCounts {
        extracted,
        inferred,
        ambiguous,
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────────────────────────

/// Clones `graph` and retains only edges whose [`Confidence`] satisfies `pred`.
///
/// All other graph fields are cloned verbatim. The relative order of surviving edges is
/// preserved. Never allocates beyond the clone.
fn filter_edges(graph: &Graph, pred: impl Fn(Confidence) -> bool) -> Graph {
    let mut out = graph.clone();
    out.edges.retain(|e| pred(e.confidence));
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use habitat_graph_core::{
        Community, CommunityId, Confidence, Edge, Graph, InputRecord, Node, NodeId, Span,
        SCHEMA_VERSION,
    };

    use super::{confidence_counts, trusted_subgraph, untrusted_subgraph, ConfidenceCounts};

    // ── Test helpers ─────────────────────────────────────────────────────────────────────────────

    const SPAN: Span = Span::new(0, 1, 1, 1);

    fn n(id: u32) -> Node {
        Node {
            id: NodeId::new(id),
            label: format!("n{id}"),
            source_file: "test.rs".into(),
            source_location: SPAN,
        }
    }

    fn e(src: u32, tgt: u32, c: Confidence) -> Edge {
        Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: "rel".into(),
            confidence: c,
        }
    }

    fn e_rel(src: u32, tgt: u32, rel: &str, c: Confidence) -> Edge {
        Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: rel.into(),
            confidence: c,
        }
    }

    fn comm(id: u32, members: &[u32]) -> Community {
        Community {
            id: CommunityId::new(id),
            label: format!("community-{id}"),
            members: members.iter().copied().map(NodeId::new).collect(),
        }
    }

    /// Build a bare graph (no nodes, no communities) with the given edges.
    fn bare_graph(edges: impl IntoIterator<Item = Edge>) -> Graph {
        let mut g = Graph::new();
        g.edges.extend(edges);
        g
    }

    /// Sort edges into a canonical order for set-equality comparisons.
    fn sorted_edges(mut v: Vec<Edge>) -> Vec<Edge> {
        v.sort_by(|a, b| {
            (a.source, a.target, a.relation.as_str(), a.confidence).cmp(&(
                b.source,
                b.target,
                b.relation.as_str(),
                b.confidence,
            ))
        });
        v
    }

    // ── 1. trusted_subgraph — basic filtering ─────────────────────────────────────────────────────

    #[test]
    fn trusted_empty_graph_stays_empty() {
        let g = Graph::new();
        let out = trusted_subgraph(&g);
        assert_eq!(out.edges.len(), 0);
    }

    #[test]
    fn trusted_all_extracted_all_kept() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Extracted),
            e(3, 4, Confidence::Extracted),
        ]);
        let out = trusted_subgraph(&g);
        assert_eq!(out.edges.len(), 3);
        assert!(out
            .edges
            .iter()
            .all(|e| e.confidence == Confidence::Extracted));
    }

    #[test]
    fn trusted_all_inferred_none_kept() {
        let g = bare_graph([e(1, 2, Confidence::Inferred), e(2, 3, Confidence::Inferred)]);
        let out = trusted_subgraph(&g);
        assert_eq!(out.edges.len(), 0);
    }

    #[test]
    fn trusted_all_ambiguous_none_kept() {
        let g = bare_graph([
            e(1, 2, Confidence::Ambiguous),
            e(2, 3, Confidence::Ambiguous),
        ]);
        let out = trusted_subgraph(&g);
        assert_eq!(out.edges.len(), 0);
    }

    #[test]
    fn trusted_mixed_extracted_and_inferred_keeps_only_extracted() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Extracted),
            e(4, 5, Confidence::Inferred),
        ]);
        let out = trusted_subgraph(&g);
        assert_eq!(out.edges.len(), 2);
        assert!(out.edges.iter().all(|e| e.confidence.is_trusted()));
    }

    #[test]
    fn trusted_mixed_extracted_and_ambiguous_keeps_only_extracted() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Ambiguous),
            e(3, 4, Confidence::Extracted),
        ]);
        let out = trusted_subgraph(&g);
        assert_eq!(out.edges.len(), 2);
        assert!(out.edges.iter().all(|e| e.confidence.is_trusted()));
    }

    #[test]
    fn trusted_all_three_confidences_keeps_only_extracted() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Ambiguous),
            e(4, 5, Confidence::Extracted),
            e(5, 6, Confidence::Inferred),
            e(6, 7, Confidence::Ambiguous),
        ]);
        let out = trusted_subgraph(&g);
        assert_eq!(out.edges.len(), 2, "only extracted edges should survive");
        assert!(out
            .edges
            .iter()
            .all(|e| e.confidence == Confidence::Extracted));
    }

    #[test]
    fn trusted_single_extracted_edge_kept() {
        let g = bare_graph([e(1, 2, Confidence::Extracted)]);
        assert_eq!(trusted_subgraph(&g).edges.len(), 1);
    }

    #[test]
    fn trusted_single_inferred_edge_dropped() {
        let g = bare_graph([e(1, 2, Confidence::Inferred)]);
        assert_eq!(trusted_subgraph(&g).edges.len(), 0);
    }

    #[test]
    fn trusted_single_ambiguous_edge_dropped() {
        let g = bare_graph([e(1, 2, Confidence::Ambiguous)]);
        assert_eq!(trusted_subgraph(&g).edges.len(), 0);
    }

    // ── 2. untrusted_subgraph — basic filtering ───────────────────────────────────────────────────

    #[test]
    fn untrusted_empty_graph_stays_empty() {
        let g = Graph::new();
        assert_eq!(untrusted_subgraph(&g).edges.len(), 0);
    }

    #[test]
    fn untrusted_all_inferred_all_kept() {
        let g = bare_graph([
            e(1, 2, Confidence::Inferred),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Inferred),
        ]);
        let out = untrusted_subgraph(&g);
        assert_eq!(out.edges.len(), 3);
        assert!(out.edges.iter().all(|e| !e.confidence.is_trusted()));
    }

    #[test]
    fn untrusted_all_ambiguous_all_kept() {
        let g = bare_graph([
            e(1, 2, Confidence::Ambiguous),
            e(2, 3, Confidence::Ambiguous),
        ]);
        let out = untrusted_subgraph(&g);
        assert_eq!(out.edges.len(), 2);
    }

    #[test]
    fn untrusted_all_extracted_none_kept() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Extracted),
        ]);
        assert_eq!(untrusted_subgraph(&g).edges.len(), 0);
    }

    #[test]
    fn untrusted_mixed_keeps_inferred_drops_extracted() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Extracted),
            e(4, 5, Confidence::Inferred),
        ]);
        let out = untrusted_subgraph(&g);
        assert_eq!(out.edges.len(), 2);
        assert!(out.edges.iter().all(|e| !e.confidence.is_trusted()));
    }

    #[test]
    fn untrusted_mixed_keeps_ambiguous_drops_extracted() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Ambiguous),
            e(3, 4, Confidence::Ambiguous),
        ]);
        let out = untrusted_subgraph(&g);
        assert_eq!(out.edges.len(), 2);
        assert!(out
            .edges
            .iter()
            .all(|e| e.confidence == Confidence::Ambiguous));
    }

    #[test]
    fn untrusted_all_three_keeps_inferred_and_ambiguous() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Ambiguous),
            e(4, 5, Confidence::Extracted),
        ]);
        let out = untrusted_subgraph(&g);
        assert_eq!(out.edges.len(), 2);
        assert!(out.edges.iter().all(|e| !e.confidence.is_trusted()));
    }

    #[test]
    fn untrusted_single_extracted_dropped() {
        let g = bare_graph([e(1, 2, Confidence::Extracted)]);
        assert_eq!(untrusted_subgraph(&g).edges.len(), 0);
    }

    #[test]
    fn untrusted_single_inferred_retained() {
        let g = bare_graph([e(1, 2, Confidence::Inferred)]);
        assert_eq!(untrusted_subgraph(&g).edges.len(), 1);
    }

    #[test]
    fn untrusted_single_ambiguous_retained() {
        let g = bare_graph([e(1, 2, Confidence::Ambiguous)]);
        assert_eq!(untrusted_subgraph(&g).edges.len(), 1);
    }

    // ── 3. confidence_counts ──────────────────────────────────────────────────────────────────────

    #[test]
    fn counts_empty_graph_all_zero() {
        let g = Graph::new();
        assert_eq!(
            confidence_counts(&g),
            ConfidenceCounts {
                extracted: 0,
                inferred: 0,
                ambiguous: 0,
            }
        );
    }

    #[test]
    fn counts_only_extracted() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Extracted),
        ]);
        let c = confidence_counts(&g);
        assert_eq!(c.extracted, 2);
        assert_eq!(c.inferred, 0);
        assert_eq!(c.ambiguous, 0);
    }

    #[test]
    fn counts_only_inferred() {
        let g = bare_graph([
            e(1, 2, Confidence::Inferred),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Inferred),
        ]);
        let c = confidence_counts(&g);
        assert_eq!(c.extracted, 0);
        assert_eq!(c.inferred, 3);
        assert_eq!(c.ambiguous, 0);
    }

    #[test]
    fn counts_only_ambiguous() {
        let g = bare_graph([e(1, 2, Confidence::Ambiguous)]);
        let c = confidence_counts(&g);
        assert_eq!(c.extracted, 0);
        assert_eq!(c.inferred, 0);
        assert_eq!(c.ambiguous, 1);
    }

    #[test]
    fn counts_one_of_each() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Ambiguous),
        ]);
        let c = confidence_counts(&g);
        assert_eq!(c.extracted, 1);
        assert_eq!(c.inferred, 1);
        assert_eq!(c.ambiguous, 1);
    }

    #[test]
    fn counts_mixed_multiple() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Extracted),
            e(3, 4, Confidence::Inferred),
            e(4, 5, Confidence::Ambiguous),
            e(5, 6, Confidence::Ambiguous),
        ]);
        let c = confidence_counts(&g);
        assert_eq!(c.extracted, 2);
        assert_eq!(c.inferred, 1);
        assert_eq!(c.ambiguous, 2);
    }

    #[test]
    fn counts_sum_equals_total_edge_count() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Ambiguous),
            e(4, 5, Confidence::Extracted),
            e(5, 6, Confidence::Inferred),
        ]);
        let c = confidence_counts(&g);
        assert_eq!(c.extracted + c.inferred + c.ambiguous, g.edges.len());
    }

    #[test]
    fn counts_total_method_matches_field_sum() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Ambiguous),
        ]);
        let c = confidence_counts(&g);
        assert_eq!(c.total(), c.extracted + c.inferred + c.ambiguous);
        assert_eq!(c.total(), 3);
    }

    #[test]
    fn counts_total_zero_for_empty_graph() {
        assert_eq!(confidence_counts(&Graph::new()).total(), 0);
    }

    #[test]
    fn counts_large_mixed_graph() {
        // 60 extracted, 30 inferred, 10 ambiguous = 100 edges total.
        let mut g = Graph::new();
        for i in 0..60_u32 {
            g.edges.push(e(i, i + 1, Confidence::Extracted));
        }
        for i in 60..90_u32 {
            g.edges.push(e(i, i + 1, Confidence::Inferred));
        }
        for i in 90..100_u32 {
            g.edges.push(e(i, i + 1, Confidence::Ambiguous));
        }
        let c = confidence_counts(&g);
        assert_eq!(c.extracted, 60);
        assert_eq!(c.inferred, 30);
        assert_eq!(c.ambiguous, 10);
        assert_eq!(c.total(), 100);
    }

    // ── 4. Partition invariants ───────────────────────────────────────────────────────────────────

    #[test]
    fn partition_edge_count_sums_to_total() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Ambiguous),
            e(4, 5, Confidence::Extracted),
            e(5, 6, Confidence::Inferred),
        ]);
        let t = trusted_subgraph(&g);
        let u = untrusted_subgraph(&g);
        assert_eq!(
            t.edges.len() + u.edges.len(),
            g.edges.len(),
            "partition: trusted + untrusted edge counts must sum to total"
        );
    }

    #[test]
    fn partition_all_edges_covered_set_equality() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Ambiguous),
            e(4, 5, Confidence::Extracted),
        ]);
        let t = trusted_subgraph(&g);
        let u = untrusted_subgraph(&g);
        let mut combined = t.edges.clone();
        combined.extend(u.edges.clone());
        assert_eq!(
            sorted_edges(combined),
            sorted_edges(g.edges.clone()),
            "trusted ∪ untrusted must equal the original edge set"
        );
    }

    #[test]
    fn partition_disjoint_by_confidence() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Ambiguous),
        ]);
        let t = trusted_subgraph(&g);
        let u = untrusted_subgraph(&g);
        // No edge in t should also be in u (disjoint sets).
        for te in &t.edges {
            assert!(
                !u.edges.contains(te),
                "trusted and untrusted edge sets must be disjoint"
            );
        }
    }

    #[test]
    fn partition_empty_graph_both_empty() {
        let g = Graph::new();
        assert_eq!(trusted_subgraph(&g).edges.len(), 0);
        assert_eq!(untrusted_subgraph(&g).edges.len(), 0);
    }

    #[test]
    fn partition_all_extracted_trusted_full_untrusted_empty() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Extracted),
        ]);
        assert_eq!(trusted_subgraph(&g).edges.len(), 2);
        assert_eq!(untrusted_subgraph(&g).edges.len(), 0);
    }

    #[test]
    fn partition_all_inferred_trusted_empty_untrusted_full() {
        let g = bare_graph([e(1, 2, Confidence::Inferred), e(2, 3, Confidence::Inferred)]);
        assert_eq!(trusted_subgraph(&g).edges.len(), 0);
        assert_eq!(untrusted_subgraph(&g).edges.len(), 2);
    }

    #[test]
    fn partition_all_ambiguous_trusted_empty_untrusted_full() {
        let g = bare_graph([
            e(1, 2, Confidence::Ambiguous),
            e(2, 3, Confidence::Ambiguous),
        ]);
        assert_eq!(trusted_subgraph(&g).edges.len(), 0);
        assert_eq!(untrusted_subgraph(&g).edges.len(), 2);
    }

    #[test]
    fn partition_large_mixed_graph_completeness() {
        // 50 extracted + 30 inferred + 20 ambiguous = 100 total edges.
        let mut g = Graph::new();
        for i in 0..50_u32 {
            g.edges.push(e(i, i + 1, Confidence::Extracted));
        }
        for i in 50..80_u32 {
            g.edges.push(e(i, i + 1, Confidence::Inferred));
        }
        for i in 80..100_u32 {
            g.edges.push(e(i, i + 1, Confidence::Ambiguous));
        }
        let t = trusted_subgraph(&g);
        let u = untrusted_subgraph(&g);
        assert_eq!(t.edges.len(), 50);
        assert_eq!(u.edges.len(), 50);
        assert_eq!(t.edges.len() + u.edges.len(), g.edges.len());
        let mut combined = t.edges.clone();
        combined.extend(u.edges.clone());
        assert_eq!(sorted_edges(combined), sorted_edges(g.edges.clone()));
    }

    // ── 5. Structural preservation ────────────────────────────────────────────────────────────────

    #[test]
    fn trusted_preserves_nodes() {
        let mut g = Graph::new();
        g.nodes.extend([n(1), n(2), n(3)]);
        g.edges.push(e(1, 2, Confidence::Inferred));
        let out = trusted_subgraph(&g);
        assert_eq!(out.nodes, g.nodes, "trusted_subgraph must preserve nodes");
    }

    #[test]
    fn trusted_preserves_communities() {
        let mut g = Graph::new();
        g.nodes.extend([n(1), n(2)]);
        g.communities.push(comm(0, &[1, 2]));
        g.edges.push(e(1, 2, Confidence::Inferred));
        let out = trusted_subgraph(&g);
        assert_eq!(
            out.communities, g.communities,
            "trusted_subgraph must preserve communities"
        );
    }

    #[test]
    fn trusted_preserves_manifest() {
        let mut g = Graph::new();
        g.manifest.inputs.push(InputRecord {
            path: "src/main.rs".into(),
            content_hash: "abc123".into(),
        });
        g.manifest.tool_version = "0.99.0".into();
        g.edges.push(e(1, 2, Confidence::Inferred));
        let out = trusted_subgraph(&g);
        assert_eq!(
            out.manifest, g.manifest,
            "trusted_subgraph must preserve manifest"
        );
    }

    #[test]
    fn trusted_preserves_schema_field() {
        let g = Graph::new();
        let out = trusted_subgraph(&g);
        assert_eq!(out.schema, SCHEMA_VERSION);
    }

    #[test]
    fn untrusted_preserves_nodes() {
        let mut g = Graph::new();
        g.nodes.extend([n(10), n(20)]);
        g.edges.push(e(10, 20, Confidence::Extracted));
        let out = untrusted_subgraph(&g);
        assert_eq!(out.nodes, g.nodes, "untrusted_subgraph must preserve nodes");
    }

    #[test]
    fn untrusted_preserves_communities() {
        let mut g = Graph::new();
        g.nodes.extend([n(1), n(2), n(3)]);
        g.communities.push(comm(0, &[1, 2, 3]));
        g.edges.push(e(1, 2, Confidence::Extracted));
        let out = untrusted_subgraph(&g);
        assert_eq!(
            out.communities, g.communities,
            "untrusted_subgraph must preserve communities"
        );
    }

    #[test]
    fn untrusted_preserves_manifest() {
        let mut g = Graph::new();
        g.manifest.inputs.push(InputRecord {
            path: "lib.rs".into(),
            content_hash: "deadbeef".into(),
        });
        g.manifest.generated_at = Some("2026-06-29T00:00:00Z".into());
        let out = untrusted_subgraph(&g);
        assert_eq!(
            out.manifest, g.manifest,
            "untrusted_subgraph must preserve manifest"
        );
    }

    #[test]
    fn untrusted_preserves_schema_field() {
        let g = Graph::new();
        let out = untrusted_subgraph(&g);
        assert_eq!(out.schema, SCHEMA_VERSION);
    }

    // ── 6. Idempotency ────────────────────────────────────────────────────────────────────────────

    #[test]
    fn trusted_is_idempotent() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Ambiguous),
        ]);
        let once = trusted_subgraph(&g);
        let twice = trusted_subgraph(&once);
        assert_eq!(once, twice, "trusted_subgraph must be idempotent");
    }

    #[test]
    fn untrusted_is_idempotent() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Ambiguous),
        ]);
        let once = untrusted_subgraph(&g);
        let twice = untrusted_subgraph(&once);
        assert_eq!(once, twice, "untrusted_subgraph must be idempotent");
    }

    #[test]
    fn trusted_of_untrusted_has_no_edges() {
        // untrusted contains only INFERRED+AMBIGUOUS — trusted(untrusted(g)) must be empty.
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Ambiguous),
        ]);
        let inner = untrusted_subgraph(&g);
        let outer = trusted_subgraph(&inner);
        assert_eq!(
            outer.edges.len(),
            0,
            "trusted(untrusted(g)) must have no edges since untrusted has no EXTRACTED"
        );
    }

    #[test]
    fn untrusted_of_trusted_has_no_edges() {
        // trusted contains only EXTRACTED — untrusted(trusted(g)) must be empty.
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Ambiguous),
        ]);
        let inner = trusted_subgraph(&g);
        let outer = untrusted_subgraph(&inner);
        assert_eq!(
            outer.edges.len(),
            0,
            "untrusted(trusted(g)) must have no edges since trusted has no INFERRED/AMBIGUOUS"
        );
    }

    // ── 7. Determinism ────────────────────────────────────────────────────────────────────────────

    #[test]
    fn trusted_is_deterministic() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Extracted),
            e(4, 5, Confidence::Ambiguous),
        ]);
        assert_eq!(
            trusted_subgraph(&g),
            trusted_subgraph(&g),
            "trusted_subgraph must be deterministic"
        );
    }

    #[test]
    fn untrusted_is_deterministic() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Ambiguous),
        ]);
        assert_eq!(
            untrusted_subgraph(&g),
            untrusted_subgraph(&g),
            "untrusted_subgraph must be deterministic"
        );
    }

    #[test]
    fn counts_is_deterministic() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Ambiguous),
        ]);
        assert_eq!(
            confidence_counts(&g),
            confidence_counts(&g),
            "confidence_counts must be deterministic"
        );
    }

    // ── 8. Edge ordering preserved ────────────────────────────────────────────────────────────────

    #[test]
    fn trusted_preserves_relative_order_of_extracted_edges() {
        // Insert extracted edges in a specific order; they must survive in the same relative order.
        let g = bare_graph([
            e(3, 4, Confidence::Extracted), // will be at index 0 after filter
            e(1, 2, Confidence::Inferred),  // dropped
            e(1, 3, Confidence::Extracted), // will be at index 1 after filter
            e(2, 4, Confidence::Ambiguous), // dropped
            e(5, 6, Confidence::Extracted), // will be at index 2 after filter
        ]);
        let out = trusted_subgraph(&g);
        assert_eq!(out.edges.len(), 3);
        assert_eq!(out.edges[0], e(3, 4, Confidence::Extracted));
        assert_eq!(out.edges[1], e(1, 3, Confidence::Extracted));
        assert_eq!(out.edges[2], e(5, 6, Confidence::Extracted));
    }

    #[test]
    fn untrusted_preserves_relative_order_of_nontrusted_edges() {
        // Insert non-trusted edges in a specific order; they must survive in the same relative order.
        let g = bare_graph([
            e(3, 4, Confidence::Inferred),  // index 0 after filter
            e(1, 2, Confidence::Extracted), // dropped
            e(1, 3, Confidence::Ambiguous), // index 1 after filter
            e(2, 4, Confidence::Extracted), // dropped
            e(5, 6, Confidence::Inferred),  // index 2 after filter
        ]);
        let out = untrusted_subgraph(&g);
        assert_eq!(out.edges.len(), 3);
        assert_eq!(out.edges[0], e(3, 4, Confidence::Inferred));
        assert_eq!(out.edges[1], e(1, 3, Confidence::Ambiguous));
        assert_eq!(out.edges[2], e(5, 6, Confidence::Inferred));
    }

    // ── 9. Self-loops ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn trusted_keeps_extracted_self_loop() {
        let g = bare_graph([e(7, 7, Confidence::Extracted)]);
        let out = trusted_subgraph(&g);
        assert_eq!(out.edges.len(), 1);
        assert_eq!(out.edges[0].source, out.edges[0].target);
        assert_eq!(out.edges[0].confidence, Confidence::Extracted);
    }

    #[test]
    fn trusted_drops_inferred_self_loop() {
        let g = bare_graph([e(7, 7, Confidence::Inferred)]);
        assert_eq!(trusted_subgraph(&g).edges.len(), 0);
    }

    #[test]
    fn untrusted_keeps_inferred_self_loop() {
        let g = bare_graph([e(7, 7, Confidence::Inferred)]);
        let out = untrusted_subgraph(&g);
        assert_eq!(out.edges.len(), 1);
        assert_eq!(out.edges[0].source, out.edges[0].target);
    }

    #[test]
    fn untrusted_drops_extracted_self_loop() {
        let g = bare_graph([e(7, 7, Confidence::Extracted)]);
        assert_eq!(untrusted_subgraph(&g).edges.len(), 0);
    }

    #[test]
    fn self_loop_ambiguous_goes_to_untrusted_not_trusted() {
        let g = bare_graph([e(3, 3, Confidence::Ambiguous)]);
        assert_eq!(trusted_subgraph(&g).edges.len(), 0);
        assert_eq!(untrusted_subgraph(&g).edges.len(), 1);
    }

    // ── 10. Parallel / multi-relation edges ───────────────────────────────────────────────────────

    #[test]
    fn parallel_edges_same_pair_different_confidence_split_correctly() {
        // Same (src, tgt) pair with three different confidences and three different relations.
        let g = bare_graph([
            e_rel(1, 2, "calls", Confidence::Extracted),
            e_rel(1, 2, "imports", Confidence::Inferred),
            e_rel(1, 2, "uses", Confidence::Ambiguous),
        ]);
        let t = trusted_subgraph(&g);
        let u = untrusted_subgraph(&g);
        assert_eq!(t.edges.len(), 1);
        assert_eq!(t.edges[0].relation, "calls");
        assert_eq!(u.edges.len(), 2);
        let rels: Vec<&str> = u.edges.iter().map(|e| e.relation.as_str()).collect();
        assert!(rels.contains(&"imports"));
        assert!(rels.contains(&"uses"));
    }

    // ── 11. counts ∩ partition cross-validation ───────────────────────────────────────────────────

    #[test]
    fn counts_extracted_matches_trusted_subgraph_len() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Extracted),
            e(4, 5, Confidence::Ambiguous),
        ]);
        let c = confidence_counts(&g);
        assert_eq!(
            c.extracted,
            trusted_subgraph(&g).edges.len(),
            "extracted count must equal trusted subgraph edge count"
        );
    }

    #[test]
    fn counts_nontrusted_matches_untrusted_subgraph_len() {
        let g = bare_graph([
            e(1, 2, Confidence::Extracted),
            e(2, 3, Confidence::Inferred),
            e(3, 4, Confidence::Ambiguous),
            e(4, 5, Confidence::Inferred),
        ]);
        let c = confidence_counts(&g);
        assert_eq!(
            c.inferred + c.ambiguous,
            untrusted_subgraph(&g).edges.len(),
            "inferred+ambiguous count must equal untrusted subgraph edge count"
        );
    }

    // ── 12. Communities unaffected ────────────────────────────────────────────────────────────────

    #[test]
    fn trusted_preserves_multi_community_structure() {
        let mut g = Graph::new();
        g.nodes.extend([n(1), n(2), n(3), n(4)]);
        g.communities.push(comm(0, &[1, 2]));
        g.communities.push(comm(1, &[3, 4]));
        g.edges.push(e(1, 2, Confidence::Inferred));
        g.edges.push(e(3, 4, Confidence::Extracted));
        let out = trusted_subgraph(&g);
        assert_eq!(out.communities.len(), 2);
        assert_eq!(out.communities, g.communities);
    }

    #[test]
    fn untrusted_preserves_multi_community_structure() {
        let mut g = Graph::new();
        g.nodes.extend([n(1), n(2), n(3)]);
        g.communities.push(comm(0, &[1]));
        g.communities.push(comm(1, &[2, 3]));
        g.edges.push(e(1, 2, Confidence::Extracted));
        g.edges.push(e(2, 3, Confidence::Inferred));
        let out = untrusted_subgraph(&g);
        assert_eq!(out.communities.len(), 2);
        assert_eq!(out.communities, g.communities);
    }

    // ── 13. Manifest with generated_at ────────────────────────────────────────────────────────────

    #[test]
    fn trusted_preserves_generated_at_in_manifest() {
        let mut g = Graph::new();
        g.manifest.generated_at = Some("2026-06-29T12:00:00Z".into());
        g.edges.push(e(1, 2, Confidence::Inferred));
        let out = trusted_subgraph(&g);
        assert_eq!(
            out.manifest.generated_at,
            Some("2026-06-29T12:00:00Z".into())
        );
    }

    #[test]
    fn untrusted_preserves_generated_at_in_manifest() {
        let mut g = Graph::new();
        g.manifest.generated_at = Some("2026-06-29T12:00:00Z".into());
        g.edges.push(e(1, 2, Confidence::Extracted));
        let out = untrusted_subgraph(&g);
        assert_eq!(
            out.manifest.generated_at,
            Some("2026-06-29T12:00:00Z".into())
        );
    }

    // ── 14. Only-edges-differ invariant ───────────────────────────────────────────────────────────

    #[test]
    fn trusted_differs_from_original_only_in_edges() {
        let mut g = Graph::new();
        g.nodes.extend([n(1), n(2), n(3)]);
        g.communities.push(comm(0, &[1, 2, 3]));
        g.edges.push(e(1, 2, Confidence::Extracted));
        g.edges.push(e(2, 3, Confidence::Inferred));
        let out = trusted_subgraph(&g);
        assert_eq!(out.nodes, g.nodes);
        assert_eq!(out.communities, g.communities);
        assert_eq!(out.schema, g.schema);
        assert_eq!(out.manifest, g.manifest);
        assert_ne!(out.edges, g.edges); // edges differ
    }

    #[test]
    fn untrusted_differs_from_original_only_in_edges() {
        let mut g = Graph::new();
        g.nodes.extend([n(10), n(20)]);
        g.communities.push(comm(0, &[10, 20]));
        g.edges.push(e(10, 20, Confidence::Extracted));
        g.edges.push(e(20, 10, Confidence::Ambiguous));
        let out = untrusted_subgraph(&g);
        assert_eq!(out.nodes, g.nodes);
        assert_eq!(out.communities, g.communities);
        assert_eq!(out.schema, g.schema);
        assert_eq!(out.manifest, g.manifest);
        assert_ne!(out.edges, g.edges);
    }

    // ── 15. ConfidenceCounts field equality ───────────────────────────────────────────────────────

    #[test]
    fn confidence_counts_struct_equality_is_field_wise() {
        let a = ConfidenceCounts {
            extracted: 3,
            inferred: 2,
            ambiguous: 1,
        };
        let b = ConfidenceCounts {
            extracted: 3,
            inferred: 2,
            ambiguous: 1,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn confidence_counts_struct_inequality_on_extracted() {
        let a = ConfidenceCounts {
            extracted: 3,
            inferred: 2,
            ambiguous: 1,
        };
        let b = ConfidenceCounts {
            extracted: 4,
            inferred: 2,
            ambiguous: 1,
        };
        assert_ne!(a, b);
    }
}
