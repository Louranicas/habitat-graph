//! Aggregate a set of per-file extractions into a single interned graph.
//!
//! # Dangling-edge policy
//!
//! An edge whose `source` or `target` label does not appear in any [`RawNode`] across the full
//! set of extractions is silently **dropped**. This happens when an extractor emits a relationship
//! referencing a symbol that was never extracted as a node — for example because that symbol lives
//! in an un-scanned file or was filtered out. Callers that need to preserve such edges should
//! ensure every referenced label appears in at least one extraction.

use indexmap::{map::Entry, IndexMap};

use habitat_graph_core::{Edge, Extraction, Graph, Node, NodeId};

/// Assembles per-file extractions into a deterministic [`Graph`].
///
/// The algorithm proceeds in four phases:
///
/// 1. **Label interning** — every distinct [`RawNode`](habitat_graph_core::RawNode) label
///    encountered across all extractions (in iteration order: extraction, then node within
///    extraction) is assigned a monotonically increasing [`NodeId`] starting at `0`. First
///    occurrence wins; later extractions that contain the same label are skipped for that label,
///    preserving the original `source_file` and `span`.
///
/// 2. **Edge resolution** — each [`RawEdge`](habitat_graph_core::RawEdge)'s `source` and `target`
///    labels are looked up in the intern table. When both resolve the edge is kept; if either
///    endpoint label is absent the edge is silently dropped (see the *dangling-edge policy* in the
///    module-level documentation).
///
/// 3. **Deduplication** — [`crate::dedup`] removes duplicate nodes (same id) and duplicate edges
///    (same `source`/`target`/`relation`); first occurrence wins.
///
/// 4. **Sorting** — [`Graph::sorted`] canonicalises collection order so serialized output is
///    deterministic and diffs are minimal (invariant R4).
///
/// The resulting graph's `communities` and `manifest` are at their default (empty/default) values;
/// higher-level build stages are responsible for populating them.
#[must_use]
// The `Vec<Extraction>` signature is the public API contract (callers transfer ownership and the
// type may later be consumed rather than borrowed).  Changing it to `&[Extraction]` would be a
// breaking API change, so we suppress the pedantic lint here.
#[allow(clippy::needless_pass_by_value)]
pub fn assemble(extractions: Vec<Extraction>) -> Graph {
    // Phase 1: intern RawNode labels → NodeId.
    // IndexMap preserves insertion order, giving the same id sequence for the same input.
    let mut interner: IndexMap<String, NodeId> = IndexMap::new();
    let mut graph = Graph::new();
    let mut next_index: u32 = 0;

    for extraction in &extractions {
        for raw_node in &extraction.nodes {
            // The Entry API avoids a double-lookup: `entry` checks and potentially inserts in
            // one operation.  Occupied → duplicate label, skip silently.
            if let Entry::Vacant(slot) = interner.entry(raw_node.label.clone()) {
                let id = NodeId::new(next_index);
                next_index += 1;
                slot.insert(id);
                graph.nodes.push(Node {
                    id,
                    label: raw_node.label.clone(),
                    source_file: raw_node.source_file.clone(),
                    source_location: raw_node.span,
                });
            }
            // Occupied: first-occurrence already interned; this extraction's data is discarded.
        }
    }

    // Phase 2: resolve edge endpoints; drop dangling edges (either endpoint label absent).
    for extraction in &extractions {
        for raw_edge in &extraction.edges {
            if let Some(&src_id) = interner.get(raw_edge.source.as_str()) {
                if let Some(&tgt_id) = interner.get(raw_edge.target.as_str()) {
                    graph.edges.push(Edge {
                        source: src_id,
                        target: tgt_id,
                        relation: raw_edge.relation.clone(),
                        confidence: raw_edge.confidence,
                    });
                }
                // target label absent → dangling edge dropped (policy documented above)
            }
            // source label absent → dangling edge dropped (policy documented above)
        }
    }

    // Phases 3 & 4: dedup then sort for canonical R4 output.
    crate::dedup(graph).sorted()
}

#[cfg(test)]
mod tests {
    use habitat_graph_core::{Confidence, Extraction, RawEdge, RawNode, Span, SCHEMA_VERSION};

    use super::assemble;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn raw_node(label: &str, file: &str) -> RawNode {
        RawNode {
            label: label.to_owned(),
            source_file: file.to_owned(),
            span: Span::new(0, 10, 1, 2),
        }
    }

    fn raw_node_at(label: &str, file: &str, span: Span) -> RawNode {
        RawNode {
            label: label.to_owned(),
            source_file: file.to_owned(),
            span,
        }
    }

    fn raw_edge(src: &str, tgt: &str, rel: &str, conf: Confidence) -> RawEdge {
        RawEdge {
            source: src.to_owned(),
            target: tgt.to_owned(),
            relation: rel.to_owned(),
            confidence: conf,
        }
    }

    fn extraction(nodes: &[RawNode], edges: &[RawEdge]) -> Extraction {
        Extraction {
            nodes: nodes.to_vec(),
            edges: edges.to_vec(),
        }
    }

    // ── 1. empty Vec ─────────────────────────────────────────────────────────

    #[test]
    fn empty_input_gives_empty_graph() {
        let g = assemble(vec![]);
        assert_eq!(g.counts(), (0, 0, 0));
        assert_eq!(g.schema, SCHEMA_VERSION);
    }

    // ── 2. single empty extraction ────────────────────────────────────────────

    #[test]
    fn single_empty_extraction_gives_empty_graph() {
        let g = assemble(vec![Extraction::new()]);
        assert_eq!(g.counts(), (0, 0, 0));
    }

    // ── 3. basic: 2 nodes + 1 edge in one extraction ─────────────────────────

    #[test]
    fn single_extraction_two_nodes_one_edge() {
        let e = extraction(
            &[raw_node("A", "src/a.rs"), raw_node("B", "src/b.rs")],
            &[raw_edge("A", "B", "calls", Confidence::Extracted)],
        );
        let g = assemble(vec![e]);
        assert_eq!(g.counts(), (2, 1, 0));
    }

    // ── 4. duplicate label across two extractions → interned once ─────────────

    #[test]
    fn duplicate_label_across_extractions_interned_once() {
        let e1 = extraction(&[raw_node("Alpha", "file1.rs")], &[]);
        let e2 = extraction(&[raw_node("Alpha", "file2.rs")], &[]);
        let g = assemble(vec![e1, e2]);
        assert_eq!(
            g.nodes.len(),
            1,
            "duplicate label must produce exactly one node"
        );
        assert_eq!(g.nodes[0].label, "Alpha");
    }

    // ── 5. first-occurrence source_file wins ──────────────────────────────────

    #[test]
    fn first_extraction_source_file_wins() {
        let e1 = extraction(&[raw_node("X", "first.rs")], &[]);
        let e2 = extraction(&[raw_node("X", "second.rs")], &[]);
        let g = assemble(vec![e1, e2]);
        assert_eq!(g.nodes[0].source_file, "first.rs");
    }

    // ── 6. first-occurrence span wins ─────────────────────────────────────────

    #[test]
    fn first_extraction_span_wins() {
        let span1 = Span::new(0, 5, 1, 1);
        let span2 = Span::new(10, 20, 3, 4);
        let e1 = extraction(&[raw_node_at("Y", "a.rs", span1)], &[]);
        let e2 = extraction(&[raw_node_at("Y", "b.rs", span2)], &[]);
        let g = assemble(vec![e1, e2]);
        assert_eq!(g.nodes[0].source_location, span1);
    }

    // ── 7. NodeId assigned in first-seen order ────────────────────────────────

    #[test]
    fn node_ids_assigned_in_first_seen_order() {
        let e1 = extraction(
            &[raw_node("First", "a.rs"), raw_node("Second", "b.rs")],
            &[],
        );
        let e2 = extraction(&[raw_node("Third", "c.rs")], &[]);
        let g = assemble(vec![e1, e2]);
        // sorted() orders by id; ids must be 0 → First, 1 → Second, 2 → Third.
        let pairs: Vec<(u32, &str)> = g
            .nodes
            .iter()
            .map(|n| (n.id.get(), n.label.as_str()))
            .collect();
        assert_eq!(pairs, vec![(0, "First"), (1, "Second"), (2, "Third")]);
    }

    // ── 8. edge with unknown target is dropped ────────────────────────────────

    #[test]
    fn edge_with_unknown_target_dropped() {
        let e = extraction(
            &[raw_node("Known", "a.rs")],
            &[raw_edge("Known", "Ghost", "calls", Confidence::Extracted)],
        );
        let g = assemble(vec![e]);
        assert_eq!(
            g.edges.len(),
            0,
            "dangling target must cause edge to be dropped"
        );
        assert_eq!(g.nodes.len(), 1, "valid node must survive");
    }

    // ── 9. edge with unknown source is dropped ────────────────────────────────

    #[test]
    fn edge_with_unknown_source_dropped() {
        let e = extraction(
            &[raw_node("Known", "a.rs")],
            &[raw_edge("Ghost", "Known", "calls", Confidence::Extracted)],
        );
        let g = assemble(vec![e]);
        assert_eq!(
            g.edges.len(),
            0,
            "dangling source must cause edge to be dropped"
        );
    }

    // ── 10. edge with both endpoints unknown is dropped ───────────────────────

    #[test]
    fn edge_with_both_endpoints_unknown_dropped() {
        let e = extraction(
            &[],
            &[raw_edge("Ghost1", "Ghost2", "calls", Confidence::Ambiguous)],
        );
        let g = assemble(vec![e]);
        assert_eq!(g.edges.len(), 0);
        assert_eq!(g.nodes.len(), 0);
    }

    // ── 11. Extracted confidence preserved ───────────────────────────────────

    #[test]
    fn edge_confidence_extracted_preserved() {
        let e = extraction(
            &[raw_node("A", "a.rs"), raw_node("B", "b.rs")],
            &[raw_edge("A", "B", "calls", Confidence::Extracted)],
        );
        let g = assemble(vec![e]);
        assert_eq!(g.edges[0].confidence, Confidence::Extracted);
    }

    // ── 12. Inferred confidence preserved ────────────────────────────────────

    #[test]
    fn edge_confidence_inferred_preserved() {
        let e = extraction(
            &[raw_node("A", "a.rs"), raw_node("B", "b.rs")],
            &[raw_edge("A", "B", "uses", Confidence::Inferred)],
        );
        let g = assemble(vec![e]);
        assert_eq!(g.edges[0].confidence, Confidence::Inferred);
    }

    // ── 13. Ambiguous confidence preserved ───────────────────────────────────

    #[test]
    fn edge_confidence_ambiguous_preserved() {
        let e = extraction(
            &[raw_node("A", "a.rs"), raw_node("B", "b.rs")],
            &[raw_edge("A", "B", "maybe", Confidence::Ambiguous)],
        );
        let g = assemble(vec![e]);
        assert_eq!(g.edges[0].confidence, Confidence::Ambiguous);
    }

    // ── 14. output nodes sorted by id ────────────────────────────────────────

    #[test]
    fn output_nodes_sorted_by_id() {
        // Three separate extractions → ids 0,1,2 in first-seen order; sorted() keeps them that way.
        let e1 = extraction(&[raw_node("C", "c.rs")], &[]);
        let e2 = extraction(&[raw_node("A", "a.rs")], &[]);
        let e3 = extraction(&[raw_node("B", "b.rs")], &[]);
        let g = assemble(vec![e1, e2, e3]);
        let ids: Vec<u32> = g.nodes.iter().map(|n| n.id.get()).collect();
        assert_eq!(ids, vec![0, 1, 2]);
    }

    // ── 15. output edges sorted by (source, target, relation) ─────────────────

    #[test]
    fn output_edges_sorted_by_tuple() {
        // Nodes: A=0, B=1, C=2.  Edges emitted in reverse sort order.
        let e = extraction(
            &[
                raw_node("A", "a.rs"),
                raw_node("B", "b.rs"),
                raw_node("C", "c.rs"),
            ],
            &[
                raw_edge("B", "C", "uses", Confidence::Extracted),
                raw_edge("A", "C", "calls", Confidence::Extracted),
                raw_edge("A", "B", "defines", Confidence::Extracted),
            ],
        );
        let g = assemble(vec![e]);
        // Canonical order: (0,1,"defines"), (0,2,"calls"), (1,2,"uses")
        let tuples: Vec<(u32, u32, &str)> = g
            .edges
            .iter()
            .map(|e| (e.source.get(), e.target.get(), e.relation.as_str()))
            .collect();
        assert_eq!(
            tuples,
            vec![(0, 1, "defines"), (0, 2, "calls"), (1, 2, "uses")]
        );
    }

    // ── 16. multiple extractions accumulate distinct nodes ────────────────────

    #[test]
    fn multiple_extractions_accumulate_all_nodes() {
        let extractions: Vec<Extraction> = (0..10u32)
            .map(|i| extraction(&[raw_node(&format!("node{i}"), "f.rs")], &[]))
            .collect();
        let g = assemble(extractions);
        assert_eq!(g.nodes.len(), 10);
    }

    // ── 17. schema version preserved ─────────────────────────────────────────

    #[test]
    fn graph_schema_version_preserved() {
        let g = assemble(vec![]);
        assert_eq!(g.schema, SCHEMA_VERSION);
    }

    // ── 18. counts reflect assembled state ───────────────────────────────────

    #[test]
    fn counts_reflect_final_state() {
        let e = extraction(
            &[
                raw_node("N1", "a.rs"),
                raw_node("N2", "b.rs"),
                raw_node("N3", "c.rs"),
            ],
            &[
                raw_edge("N1", "N2", "calls", Confidence::Extracted),
                raw_edge("N2", "N3", "imports", Confidence::Inferred),
            ],
        );
        let g = assemble(vec![e]);
        assert_eq!(g.counts(), (3, 2, 0));
    }

    // ── 19. duplicate edges deduplicated (dedup called internally) ────────────

    #[test]
    fn duplicate_edges_across_extractions_deduplicated() {
        // Same (source, target, relation) emitted by two extractions → one edge in output.
        let nodes = [raw_node("A", "a.rs"), raw_node("B", "b.rs")];
        let e1 = extraction(
            &nodes,
            &[raw_edge("A", "B", "calls", Confidence::Extracted)],
        );
        let e2 = extraction(&nodes, &[raw_edge("A", "B", "calls", Confidence::Inferred)]);
        let g = assemble(vec![e1, e2]);
        assert_eq!(
            g.edges.len(),
            1,
            "dedup must collapse identical (src,tgt,rel) tuples"
        );
    }

    // ── 20. node source_file carried through ─────────────────────────────────

    #[test]
    fn node_source_file_carried_through() {
        let e = extraction(&[raw_node("Foo", "src/foo.rs")], &[]);
        let g = assemble(vec![e]);
        assert_eq!(g.nodes[0].source_file, "src/foo.rs");
    }

    // ── 21. node span carried through ────────────────────────────────────────

    #[test]
    fn node_span_carried_through() {
        let span = Span::new(100, 200, 5, 8);
        let e = extraction(&[raw_node_at("Bar", "src/bar.rs", span)], &[]);
        let g = assemble(vec![e]);
        assert_eq!(g.nodes[0].source_location, span);
    }

    // ── 22. cross-extraction edge resolved when both labels are interned ───────

    #[test]
    fn cross_extraction_edge_resolved_when_both_interned() {
        // Node "M" in extraction 1, node "N" in extraction 2, edge M→N in extraction 2.
        let e1 = extraction(&[raw_node("M", "m.rs")], &[]);
        let e2 = extraction(
            &[raw_node("N", "n.rs")],
            &[raw_edge("M", "N", "links", Confidence::Extracted)],
        );
        let g = assemble(vec![e1, e2]);
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].relation, "links");
    }

    // ── 23. self-edge (source == target) is kept when the node exists ─────────

    #[test]
    fn self_edge_kept_when_node_exists() {
        let e = extraction(
            &[raw_node("Recursive", "r.rs")],
            &[raw_edge(
                "Recursive",
                "Recursive",
                "recurse",
                Confidence::Inferred,
            )],
        );
        let g = assemble(vec![e]);
        assert_eq!(g.edges.len(), 1);
        assert_eq!(
            g.edges[0].source, g.edges[0].target,
            "self-edge source and target must be the same NodeId"
        );
    }

    // ── 24. mix of dangling and valid edges: only valid edge survives ──────────

    #[test]
    fn dangling_edges_dropped_valid_edges_kept() {
        let e = extraction(
            &[raw_node("P", "p.rs"), raw_node("Q", "q.rs")],
            &[
                raw_edge("P", "Q", "ok", Confidence::Extracted), // valid
                raw_edge("P", "Z", "dangle1", Confidence::Extracted), // Z missing → dropped
                raw_edge("Z", "Q", "dangle2", Confidence::Extracted), // Z missing → dropped
            ],
        );
        let g = assemble(vec![e]);
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].relation, "ok");
    }

    // ── 25. zero-based monotone id assignment confirmed via label map ──────────

    #[test]
    fn node_ids_are_zero_based_monotone() {
        let e = extraction(
            &[
                raw_node("Z", "z.rs"),
                raw_node("Y", "y.rs"),
                raw_node("X", "x.rs"),
            ],
            &[],
        );
        let g = assemble(vec![e]);
        let by_label: std::collections::HashMap<&str, u32> = g
            .nodes
            .iter()
            .map(|n| (n.label.as_str(), n.id.get()))
            .collect();
        assert_eq!(by_label["Z"], 0, "first seen label gets id 0");
        assert_eq!(by_label["Y"], 1, "second seen label gets id 1");
        assert_eq!(by_label["X"], 2, "third seen label gets id 2");
    }
}
