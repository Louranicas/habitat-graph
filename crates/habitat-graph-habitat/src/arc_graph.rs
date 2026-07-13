//! Arc-graph — producer→consumer arc extraction + severed-ear diff (S1008620 bidi-wiring).
//!
//! This module serves the bidi-wiring arc-coherence gauge: given a [`habitat_graph_core::Graph`]
//! it can extract all producer→consumer [`Arc`]s for a chosen set of relation kinds, and diff
//! them against an expected baseline to produce a [`SeveredEarReport`] that names severed ears
//! and their coherence score.
//!
//! The module is **pure**: no I/O, no network, no allocation beyond normal collection operations.
//! Consequently it is tested without any live service, file system, or network dependency.

use std::collections::HashMap;
use std::collections::HashSet;

use habitat_graph_core::Graph;
use habitat_graph_core::NodeId;

// ─── Core type ───────────────────────────────────────────────────────────────

/// A directed producer→consumer arc, parameterized by a relation kind.
///
/// Arcs are derived from graph edges by resolving [`NodeId`]s to human-readable labels. The
/// derived ordering is lexicographic on `(producer, consumer, relation)`, which gives canonical
/// deterministic output suitable for diffs (R4).
///
/// # Example
///
/// ```rust
/// use habitat_graph_habitat::arc_graph::Arc;
///
/// let arc = Arc {
///     producer: "parser".to_owned(),
///     consumer: "lexer".to_owned(),
///     relation: "calls".to_owned(),
/// };
/// assert_eq!(arc.producer, "parser");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Arc {
    /// Label of the producing (source) node.
    pub producer: String,
    /// Label of the consuming (target) node.
    pub consumer: String,
    /// The relationship kind (e.g. `"calls"`, `"imports_from"`).
    pub relation: String,
}

// ─── Relation catalogue ──────────────────────────────────────────────────────

/// Returns the default relation kinds used for arc extraction in the bidi-wiring analysis.
///
/// The canonical set is `["calls", "imports_from", "method", "defines"]`.  Pass this slice to
/// [`extract_arcs`] to obtain the standard arc view used by the S1008620 severed-ear gauge.
#[must_use]
pub fn default_arc_relations() -> &'static [&'static str] {
    &["calls", "imports_from", "method", "defines"]
}

// ─── Extraction ──────────────────────────────────────────────────────────────

/// Extracts all arcs from `graph` whose relation kind appears in `relations`.
///
/// Each graph edge is resolved to an [`Arc`] by looking up the source [`NodeId`] → `producer`
/// label and target [`NodeId`] → `consumer` label in the graph's node list.  Edges whose source
/// or target id has no corresponding node are **silently dropped** — a partially-loaded or
/// corrupted graph must not panic the caller.
///
/// The output is canonically sorted and deduplicated (R4 determinism requirement): the same graph
/// produces byte-identical output on every call regardless of internal iteration order.
#[must_use]
pub fn extract_arcs(graph: &Graph, relations: &[&str]) -> Vec<Arc> {
    // Build NodeId → label lookup in a single pass over nodes.
    let id_map: HashMap<NodeId, &str> = graph
        .nodes
        .iter()
        .map(|n| (n.id, n.label.as_str()))
        .collect();

    // Materialise the relation filter into a set for O(1) membership.
    let relation_set: HashSet<&str> = relations.iter().copied().collect();

    let mut arcs: Vec<Arc> = graph
        .edges
        .iter()
        .filter(|e| relation_set.contains(e.relation.as_str()))
        .filter_map(|e| {
            // `.copied()` collapses Option<&&str> → Option<&str>.
            let producer = id_map.get(&e.source).copied()?;
            let consumer = id_map.get(&e.target).copied()?;
            Some(Arc {
                producer: producer.to_owned(),
                consumer: consumer.to_owned(),
                relation: e.relation.clone(),
            })
        })
        .collect();

    // Sort then dedup achieves canonical sorted-unique output.
    arcs.sort_unstable();
    arcs.dedup();
    arcs
}

// ─── Severed-ear diff ────────────────────────────────────────────────────────

/// The result of comparing declared arcs against observed arcs.
///
/// `present` and `severed` are both sorted (R4) and their union exactly covers `expected`
/// (element by element, preserving duplicates if the caller passes any).
/// `coherence ∈ [0.0, 1.0]`.
#[derive(Clone, Debug, PartialEq)]
pub struct SeveredEarReport {
    /// Expected arcs that are present in the actual arc set.
    pub present: Vec<Arc>,
    /// Expected arcs that are absent from the actual arc set — the "severed ears".
    pub severed: Vec<Arc>,
    /// Arc-coherence score: `present.len() / expected.len()`, or `1.0` when `expected` is empty.
    pub coherence: f64,
}

/// Diffs `expected` declared arcs against `actual` observed arcs.
///
/// Returns a [`SeveredEarReport`] partitioning every element of `expected` into either `present`
/// (the arc exists in `actual`) or `severed` (the arc is absent).  Arcs in `actual` that are not
/// in `expected` are ignored — this function measures *declared coverage*, not over-wiring.
///
/// Coherence is `1.0` when `expected` is empty (no ears declared → no ears severed).
#[must_use]
pub fn diff_arcs(expected: &[Arc], actual: &[Arc]) -> SeveredEarReport {
    if expected.is_empty() {
        return SeveredEarReport {
            present: Vec::new(),
            severed: Vec::new(),
            coherence: 1.0,
        };
    }

    let actual_set: HashSet<&Arc> = actual.iter().collect();

    let mut present = Vec::with_capacity(expected.len());
    let mut severed = Vec::new();

    for arc in expected {
        if actual_set.contains(arc) {
            present.push(arc.clone());
        } else {
            severed.push(arc.clone());
        }
    }

    present.sort_unstable();
    severed.sort_unstable();

    // Coherence ratio. Production arc counts are many orders of magnitude below f64's 53-bit
    // mantissa precision-loss threshold, so the cast is safe in practice.
    #[allow(clippy::cast_precision_loss)]
    let coherence = present.len() as f64 / expected.len() as f64;

    SeveredEarReport {
        present,
        severed,
        coherence,
    }
}

// ─── Convenience accessor ────────────────────────────────────────────────────

/// Returns the arc-coherence score from a [`SeveredEarReport`].
///
/// Convenience accessor — range is `[0.0, 1.0]` where `1.0` means all declared arcs are present
/// and `0.0` means every declared arc is severed.
#[must_use]
pub fn arc_coherence(report: &SeveredEarReport) -> f64 {
    report.coherence
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use habitat_graph_core::Confidence;
    use habitat_graph_core::Edge;
    use habitat_graph_core::Graph;
    use habitat_graph_core::Manifest;
    use habitat_graph_core::Node;
    use habitat_graph_core::NodeId;
    use habitat_graph_core::Span;

    use super::arc_coherence;
    use super::default_arc_relations;
    use super::diff_arcs;
    use super::extract_arcs;
    use super::Arc;
    use super::SeveredEarReport;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn span() -> Span {
        Span::new(0, 1, 1, 1)
    }

    fn node(id: u32, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: "test.rs".to_owned(),
            source_location: span(),
        }
    }

    fn edge(src: u32, tgt: u32, rel: &str) -> Edge {
        Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: rel.to_owned(),
            confidence: Confidence::Extracted,
        }
    }

    fn graph(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        Graph {
            schema: habitat_graph_core::SCHEMA_VERSION.to_owned(),
            nodes,
            node_content_ids: Default::default(),
            edges,
            communities: Vec::new(),
            manifest: Manifest::default(),
        }
    }

    fn arc(producer: &str, consumer: &str, relation: &str) -> Arc {
        Arc {
            producer: producer.to_owned(),
            consumer: consumer.to_owned(),
            relation: relation.to_owned(),
        }
    }

    /// Epsilon comparison for f64 coherence values.
    fn assert_coherence(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < f64::EPSILON * 16.0,
            "coherence mismatch: expected {expected:.15}, got {actual:.15}"
        );
    }

    // ── Arc struct ───────────────────────────────────────────────────────────

    #[test]
    fn arc_eq_same_fields() {
        assert_eq!(arc("A", "B", "calls"), arc("A", "B", "calls"));
    }

    #[test]
    fn arc_ne_different_producer() {
        assert_ne!(arc("A", "B", "calls"), arc("X", "B", "calls"));
    }

    #[test]
    fn arc_ne_different_consumer() {
        assert_ne!(arc("A", "B", "calls"), arc("A", "Y", "calls"));
    }

    #[test]
    fn arc_ne_different_relation() {
        assert_ne!(arc("A", "B", "calls"), arc("A", "B", "defines"));
    }

    #[test]
    fn arc_clone_equals_original() {
        let a = arc("producer", "consumer", "method");
        assert_eq!(a.clone(), a);
    }

    #[test]
    fn arc_debug_is_nonempty() {
        let a = arc("p", "c", "r");
        let s = format!("{a:?}");
        assert!(!s.is_empty());
        assert!(s.contains("Arc"));
    }

    #[test]
    fn arc_ord_by_producer_first() {
        let a = arc("alpha", "z", "z");
        let b = arc("beta", "a", "a");
        assert!(a < b);
    }

    #[test]
    fn arc_ord_by_consumer_second() {
        let a = arc("same", "alpha", "z");
        let b = arc("same", "beta", "a");
        assert!(a < b);
    }

    #[test]
    fn arc_ord_by_relation_third() {
        let a = arc("same", "same", "calls");
        let b = arc("same", "same", "defines");
        assert!(a < b);
    }

    #[test]
    fn arc_hash_dedup_in_hashset() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(arc("A", "B", "calls"));
        set.insert(arc("A", "B", "calls")); // duplicate
        set.insert(arc("A", "B", "defines"));
        assert_eq!(set.len(), 2);
    }

    // ── default_arc_relations ────────────────────────────────────────────────

    #[test]
    fn default_arc_relations_length() {
        assert_eq!(default_arc_relations().len(), 4);
    }

    #[test]
    fn default_arc_relations_contains_calls() {
        assert!(default_arc_relations().contains(&"calls"));
    }

    #[test]
    fn default_arc_relations_contains_imports_from() {
        assert!(default_arc_relations().contains(&"imports_from"));
    }

    #[test]
    fn default_arc_relations_contains_method() {
        assert!(default_arc_relations().contains(&"method"));
    }

    #[test]
    fn default_arc_relations_contains_defines() {
        assert!(default_arc_relations().contains(&"defines"));
    }

    // ── extract_arcs ─────────────────────────────────────────────────────────

    #[test]
    fn extract_empty_graph_returns_empty() {
        let g = graph(vec![], vec![]);
        assert!(extract_arcs(&g, default_arc_relations()).is_empty());
    }

    #[test]
    fn extract_empty_relations_filter_returns_empty() {
        let g = graph(vec![node(1, "A"), node(2, "B")], vec![edge(1, 2, "calls")]);
        assert!(extract_arcs(&g, &[]).is_empty());
    }

    #[test]
    fn extract_single_matching_edge() {
        let g = graph(
            vec![node(1, "parser"), node(2, "lexer")],
            vec![edge(1, 2, "calls")],
        );
        let arcs = extract_arcs(&g, &["calls"]);
        assert_eq!(arcs, vec![arc("parser", "lexer", "calls")]);
    }

    #[test]
    fn extract_label_resolved_from_node_map() {
        // Nodes have non-sequential ids; labels must come from the node, not the id.
        let g = graph(
            vec![node(10, "frontend"), node(99, "backend")],
            vec![edge(10, 99, "calls")],
        );
        let arcs = extract_arcs(&g, &["calls"]);
        assert_eq!(arcs[0].producer, "frontend");
        assert_eq!(arcs[0].consumer, "backend");
    }

    #[test]
    fn extract_relation_filter_excludes_non_matching() {
        let g = graph(
            vec![node(1, "A"), node(2, "B")],
            vec![edge(1, 2, "inherits"), edge(1, 2, "calls")],
        );
        let arcs = extract_arcs(&g, &["calls"]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].relation, "calls");
    }

    #[test]
    fn extract_self_loop_arc_producer_equals_consumer() {
        // A node can have an edge back to itself; the arc must survive, not panic.
        let g = graph(vec![node(1, "recursive")], vec![edge(1, 1, "calls")]);
        let arcs = extract_arcs(&g, &["calls"]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].producer, "recursive");
        assert_eq!(arcs[0].consumer, "recursive");
    }

    #[test]
    fn extract_dangling_source_is_skipped_not_panicked() {
        // Source id 99 has no node.
        let g = graph(vec![node(2, "B")], vec![edge(99, 2, "calls")]);
        let arcs = extract_arcs(&g, &["calls"]);
        assert!(arcs.is_empty());
    }

    #[test]
    fn extract_dangling_target_is_skipped_not_panicked() {
        // Target id 99 has no node.
        let g = graph(vec![node(1, "A")], vec![edge(1, 99, "calls")]);
        let arcs = extract_arcs(&g, &["calls"]);
        assert!(arcs.is_empty());
    }

    #[test]
    fn extract_both_endpoints_dangling_is_skipped() {
        let g = graph(vec![], vec![edge(7, 8, "calls")]);
        let arcs = extract_arcs(&g, &["calls"]);
        assert!(arcs.is_empty());
    }

    #[test]
    fn extract_deduplicates_identical_edges() {
        // Two identical edges must produce exactly one arc.
        let g = graph(
            vec![node(1, "A"), node(2, "B")],
            vec![edge(1, 2, "calls"), edge(1, 2, "calls")],
        );
        let arcs = extract_arcs(&g, &["calls"]);
        assert_eq!(arcs.len(), 1);
    }

    #[test]
    fn extract_output_is_sorted() {
        let g = graph(
            vec![node(1, "Z"), node(2, "A"), node(3, "M")],
            vec![
                edge(1, 2, "calls"),
                edge(3, 2, "calls"),
                edge(2, 3, "calls"),
            ],
        );
        let arcs = extract_arcs(&g, &["calls"]);
        let sorted = {
            let mut v = arcs.clone();
            v.sort_unstable();
            v
        };
        assert_eq!(arcs, sorted);
    }

    #[test]
    fn extract_deterministic_regardless_of_edge_insertion_order() {
        let nodes = vec![node(1, "alpha"), node(2, "beta"), node(3, "gamma")];
        let edges_a = vec![
            edge(1, 2, "calls"),
            edge(3, 1, "imports_from"),
            edge(2, 3, "defines"),
        ];
        let mut edges_b = edges_a.clone();
        edges_b.reverse();

        let arcs_a = extract_arcs(&graph(nodes.clone(), edges_a), default_arc_relations());
        let arcs_b = extract_arcs(&graph(nodes, edges_b), default_arc_relations());
        assert_eq!(arcs_a, arcs_b);
    }

    #[test]
    fn extract_multiple_relation_types_all_pass() {
        let g = graph(
            vec![node(1, "A"), node(2, "B")],
            vec![
                edge(1, 2, "calls"),
                edge(1, 2, "imports_from"),
                edge(1, 2, "method"),
                edge(1, 2, "defines"),
            ],
        );
        let arcs = extract_arcs(&g, default_arc_relations());
        assert_eq!(arcs.len(), 4);
    }

    #[test]
    fn extract_unknown_relation_excluded() {
        let g = graph(
            vec![node(1, "A"), node(2, "B")],
            vec![edge(1, 2, "inherits")],
        );
        assert!(extract_arcs(&g, default_arc_relations()).is_empty());
    }

    #[test]
    fn extract_three_node_chain() {
        // A calls B, B imports C.
        let g = graph(
            vec![node(1, "A"), node(2, "B"), node(3, "C")],
            vec![edge(1, 2, "calls"), edge(2, 3, "imports_from")],
        );
        let arcs = extract_arcs(&g, default_arc_relations());
        assert_eq!(arcs.len(), 2);
        // Sorted: ("A","B","calls") < ("B","C","imports_from")
        assert_eq!(arcs[0], arc("A", "B", "calls"));
        assert_eq!(arcs[1], arc("B", "C", "imports_from"));
    }

    #[test]
    fn extract_mixed_matching_and_non_matching_edges() {
        let g = graph(
            vec![node(1, "A"), node(2, "B"), node(3, "C")],
            vec![
                edge(1, 2, "calls"),
                edge(2, 3, "inherits"), // not in filter
                edge(3, 1, "imports_from"),
            ],
        );
        let arcs = extract_arcs(&g, default_arc_relations());
        assert_eq!(arcs.len(), 2);
        let relations: Vec<&str> = arcs.iter().map(|a| a.relation.as_str()).collect();
        assert!(relations.contains(&"calls"));
        assert!(relations.contains(&"imports_from"));
        assert!(!relations.contains(&"inherits"));
    }

    #[test]
    fn extract_all_edges_dangling_returns_empty() {
        // None of the edge ids (100, 200, 300) exist as nodes.
        let g = graph(
            vec![node(1, "A")],
            vec![edge(100, 200, "calls"), edge(300, 100, "defines")],
        );
        assert!(extract_arcs(&g, default_arc_relations()).is_empty());
    }

    #[test]
    fn extract_idempotent_same_graph_twice() {
        let nodes = vec![node(1, "X"), node(2, "Y")];
        let edges = vec![edge(1, 2, "calls"), edge(2, 1, "defines")];
        let g = graph(nodes, edges);
        let first = extract_arcs(&g, default_arc_relations());
        let second = extract_arcs(&g, default_arc_relations());
        assert_eq!(first, second);
    }

    // ── diff_arcs ────────────────────────────────────────────────────────────

    #[test]
    fn diff_empty_expected_returns_coherence_one() {
        let report = diff_arcs(&[], &[arc("A", "B", "calls")]);
        assert_coherence(report.coherence, 1.0);
        assert!(report.present.is_empty());
        assert!(report.severed.is_empty());
    }

    #[test]
    fn diff_both_empty_returns_coherence_one() {
        let report = diff_arcs(&[], &[]);
        assert_coherence(report.coherence, 1.0);
    }

    #[test]
    fn diff_all_present_coherence_one() {
        let expected = vec![arc("A", "B", "calls"), arc("B", "C", "defines")];
        let report = diff_arcs(&expected, &expected);
        assert_coherence(report.coherence, 1.0);
        assert!(report.severed.is_empty());
        assert_eq!(report.present.len(), 2);
    }

    #[test]
    fn diff_none_present_coherence_zero() {
        let expected = vec![arc("A", "B", "calls")];
        let actual: Vec<Arc> = vec![];
        let report = diff_arcs(&expected, &actual);
        assert_coherence(report.coherence, 0.0);
        assert!(report.present.is_empty());
        assert_eq!(report.severed.len(), 1);
    }

    #[test]
    fn diff_finds_specific_severed_ear() {
        let expected = vec![arc("A", "B", "calls"), arc("B", "C", "defines")];
        let actual = vec![arc("A", "B", "calls")]; // "B→C defines" is severed
        let report = diff_arcs(&expected, &actual);
        assert_eq!(report.present, vec![arc("A", "B", "calls")]);
        assert_eq!(report.severed, vec![arc("B", "C", "defines")]);
    }

    #[test]
    fn diff_half_present_coherence_half() {
        let expected = vec![arc("A", "B", "calls"), arc("C", "D", "method")];
        let actual = vec![arc("A", "B", "calls")];
        let report = diff_arcs(&expected, &actual);
        assert_coherence(report.coherence, 0.5);
    }

    #[test]
    fn diff_extra_actual_arcs_are_ignored() {
        let expected = vec![arc("A", "B", "calls")];
        let actual = vec![
            arc("A", "B", "calls"),
            arc("X", "Y", "defines"), // not in expected — must be ignored
        ];
        let report = diff_arcs(&expected, &actual);
        assert_coherence(report.coherence, 1.0);
        assert_eq!(report.present.len(), 1);
        assert!(report.severed.is_empty());
    }

    #[test]
    fn diff_present_list_is_sorted() {
        let expected = vec![
            arc("Z", "A", "calls"),
            arc("A", "Z", "defines"),
            arc("M", "M", "method"),
        ];
        let actual = expected.clone();
        let report = diff_arcs(&expected, &actual);
        let mut sorted_expected = report.present.clone();
        sorted_expected.sort_unstable();
        assert_eq!(report.present, sorted_expected);
    }

    #[test]
    fn diff_severed_list_is_sorted() {
        let expected = vec![
            arc("Z", "A", "calls"),
            arc("A", "Z", "defines"),
            arc("M", "M", "method"),
        ];
        let report = diff_arcs(&expected, &[]);
        let mut sorted_severed = report.severed.clone();
        sorted_severed.sort_unstable();
        assert_eq!(report.severed, sorted_severed);
    }

    #[test]
    fn diff_present_plus_severed_covers_expected() {
        let expected = vec![
            arc("A", "B", "calls"),
            arc("C", "D", "defines"),
            arc("E", "F", "method"),
        ];
        let actual = vec![arc("A", "B", "calls"), arc("E", "F", "method")];
        let report = diff_arcs(&expected, &actual);
        // Every expected arc must appear in exactly one of present or severed.
        assert_eq!(report.present.len() + report.severed.len(), expected.len());
    }

    #[test]
    fn diff_single_arc_present() {
        let a = arc("p", "c", "calls");
        let report = diff_arcs(std::slice::from_ref(&a), std::slice::from_ref(&a));
        assert_eq!(report.present, vec![a]);
        assert!(report.severed.is_empty());
        assert_coherence(report.coherence, 1.0);
    }

    #[test]
    fn diff_single_arc_severed() {
        let a = arc("p", "c", "calls");
        let report = diff_arcs(std::slice::from_ref(&a), &[]);
        assert!(report.present.is_empty());
        assert_eq!(report.severed, vec![a]);
        assert_coherence(report.coherence, 0.0);
    }

    #[test]
    fn diff_empty_actual_all_severed() {
        let expected: Vec<Arc> = vec![arc("A", "B", "calls"), arc("C", "D", "defines")];
        let report = diff_arcs(&expected, &[]);
        assert_eq!(report.severed.len(), expected.len());
        assert!(report.present.is_empty());
        assert_coherence(report.coherence, 0.0);
    }

    #[test]
    fn diff_coherence_is_not_nan_with_non_empty_expected() {
        let expected = vec![arc("A", "B", "calls")];
        let report = diff_arcs(&expected, &[]);
        assert!(!report.coherence.is_nan(), "coherence must not be NaN");
    }

    #[test]
    fn diff_two_expected_one_present_one_severed() {
        let a1 = arc("A", "B", "calls");
        let a2 = arc("C", "D", "defines");
        let report = diff_arcs(&[a1.clone(), a2.clone()], std::slice::from_ref(&a1));
        assert_eq!(report.present, vec![a1]);
        assert_eq!(report.severed, vec![a2]);
        assert_coherence(report.coherence, 0.5);
    }

    #[test]
    fn diff_does_not_mutate_expected_slice() {
        // diff_arcs takes &[Arc]; the caller's data must be unchanged.
        let expected = vec![arc("A", "B", "calls"), arc("X", "Y", "defines")];
        let snapshot = expected.clone();
        let _ = diff_arcs(&expected, &[arc("A", "B", "calls")]);
        assert_eq!(expected, snapshot);
    }

    #[test]
    fn diff_does_not_mutate_actual_slice() {
        let actual = vec![arc("A", "B", "calls"), arc("X", "Y", "defines")];
        let snapshot = actual.clone();
        let _ = diff_arcs(&[arc("A", "B", "calls")], &actual);
        assert_eq!(actual, snapshot);
    }

    // ── arc_coherence accessor ────────────────────────────────────────────────

    #[test]
    fn arc_coherence_returns_report_coherence_field() {
        let report = SeveredEarReport {
            present: vec![],
            severed: vec![],
            coherence: 0.75,
        };
        assert_coherence(arc_coherence(&report), 0.75);
    }

    #[test]
    fn arc_coherence_zero_when_all_severed() {
        let expected = vec![arc("A", "B", "calls")];
        let report = diff_arcs(&expected, &[]);
        assert_coherence(arc_coherence(&report), 0.0);
    }

    #[test]
    fn arc_coherence_one_when_all_present() {
        let expected = vec![arc("A", "B", "calls"), arc("B", "C", "defines")];
        let report = diff_arcs(&expected, &expected);
        assert_coherence(arc_coherence(&report), 1.0);
    }

    #[test]
    fn arc_coherence_one_when_expected_empty() {
        let report = diff_arcs(&[], &[]);
        assert_coherence(arc_coherence(&report), 1.0);
    }
}
