//! Classify the difference between our output and a golden.

use crate::NormalizedGraph;

/// The outcome of comparing our [`NormalizedGraph`] to a golden's.
///
/// Nodes/edges present in the golden but missing from ours are **REGRESSION**s; those present in
/// ours but not the golden are **extra** (informational — a richer or differently-scoped extraction).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParityReport {
    /// Node labels present in both.
    pub nodes_matched: usize,
    /// Node labels in the golden but missing from ours (REGRESSION).
    pub nodes_missing: Vec<String>,
    /// Node labels in ours but not the golden.
    pub nodes_extra: Vec<String>,
    /// Edges `(src,tgt,relation)` present in both.
    pub edges_matched: usize,
    /// Edges in the golden but missing from ours (REGRESSION).
    pub edges_missing: Vec<(String, String, String)>,
    /// Edges in ours but not the golden.
    pub edges_extra: Vec<(String, String, String)>,
}

impl ParityReport {
    /// `true` if there is no node or edge REGRESSION (the golden is fully covered).
    #[must_use]
    pub fn is_regression_free(&self) -> bool {
        self.nodes_missing.is_empty() && self.edges_missing.is_empty()
    }
}

/// Compares `ours` against `golden`, producing a [`ParityReport`].
///
/// * `nodes_matched` — count of golden node labels present in `ours`.
/// * `nodes_missing` — golden node labels absent from `ours`; sorted (REGRESSION indicators).
/// * `nodes_extra`   — node labels present in `ours` but not in `golden`; sorted (informational).
/// * The three `edges_*` fields follow the same semantics for `(src, tgt, relation)` triples.
///
/// Output Vecs are **deterministically sorted** because the inputs are [`BTreeSet`]s and
/// [`BTreeSet::difference`] / [`BTreeSet::intersection`] iterate in ascending order.
///
/// [`BTreeSet`]: std::collections::BTreeSet
#[must_use]
pub fn classify(ours: &NormalizedGraph, golden: &NormalizedGraph) -> ParityReport {
    // ── nodes ──────────────────────────────────────────────────────────────
    let nodes_matched = ours.nodes.intersection(&golden.nodes).count();
    // golden \ ours  →  items golden expects that we are missing (REGRESSION)
    let nodes_missing: Vec<String> = golden.nodes.difference(&ours.nodes).cloned().collect();
    // ours \ golden  →  items we produce that the golden does not (extra / richer scope)
    let nodes_extra: Vec<String> = ours.nodes.difference(&golden.nodes).cloned().collect();

    // ── edges ──────────────────────────────────────────────────────────────
    let edges_matched = ours.edges.intersection(&golden.edges).count();
    let edges_missing: Vec<(String, String, String)> =
        golden.edges.difference(&ours.edges).cloned().collect();
    let edges_extra: Vec<(String, String, String)> =
        ours.edges.difference(&golden.edges).cloned().collect();

    ParityReport {
        nodes_matched,
        nodes_missing,
        nodes_extra,
        edges_matched,
        edges_missing,
        edges_extra,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{classify, ParityReport};
    use crate::NormalizedGraph;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn nodes(labels: &[&str]) -> BTreeSet<String> {
        labels.iter().map(|s| (*s).to_owned()).collect()
    }

    fn edges(triples: &[(&str, &str, &str)]) -> BTreeSet<(String, String, String)> {
        triples
            .iter()
            .map(|(a, b, c)| ((*a).to_owned(), (*b).to_owned(), (*c).to_owned()))
            .collect()
    }

    fn graph(ns: &[&str], es: &[(&str, &str, &str)]) -> NormalizedGraph {
        NormalizedGraph {
            nodes: nodes(ns),
            edges: edges(es),
        }
    }

    // ── 1. empty vs empty ─────────────────────────────────────────────────────

    #[test]
    fn empty_vs_empty_is_regression_free() {
        let r = classify(&graph(&[], &[]), &graph(&[], &[]));
        assert_eq!(r.nodes_matched, 0);
        assert!(r.nodes_missing.is_empty());
        assert!(r.nodes_extra.is_empty());
        assert_eq!(r.edges_matched, 0);
        assert!(r.edges_missing.is_empty());
        assert!(r.edges_extra.is_empty());
        assert!(r.is_regression_free());
    }

    // ── 2. identical non-empty graphs ─────────────────────────────────────────

    #[test]
    fn identical_graphs_all_matched_none_missing() {
        let ns = &["alpha", "beta", "gamma"];
        let es = &[("alpha", "beta", "calls"), ("beta", "gamma", "imports")];
        let ours = graph(ns, es);
        let golden = graph(ns, es);
        let r = classify(&ours, &golden);
        assert_eq!(r.nodes_matched, 3);
        assert_eq!(r.edges_matched, 2);
        assert!(r.nodes_missing.is_empty());
        assert!(r.nodes_extra.is_empty());
        assert!(r.edges_missing.is_empty());
        assert!(r.edges_extra.is_empty());
        assert!(r.is_regression_free());
    }

    // ── 3. golden has a node ours lacks → regression ─────────────────────────

    #[test]
    fn golden_has_node_ours_lacks_appears_in_missing() {
        let ours = graph(&["alpha"], &[]);
        let golden = graph(&["alpha", "missing_node"], &[]);
        let r = classify(&ours, &golden);
        assert_eq!(r.nodes_missing, vec!["missing_node".to_owned()]);
        assert_eq!(r.nodes_matched, 1);
        assert!(r.nodes_extra.is_empty());
        assert!(!r.is_regression_free());
    }

    // ── 4. ours has extra node → nodes_extra, still regression-free ───────────

    #[test]
    fn ours_has_extra_node_no_regression() {
        let ours = graph(&["alpha", "extra_node"], &[]);
        let golden = graph(&["alpha"], &[]);
        let r = classify(&ours, &golden);
        assert_eq!(r.nodes_extra, vec!["extra_node".to_owned()]);
        assert_eq!(r.nodes_matched, 1);
        assert!(r.nodes_missing.is_empty());
        // nodes regression-free; edges regression-free
        assert!(r.is_regression_free());
    }

    // ── 5. golden has an edge ours lacks → edges_missing, not regression-free ─

    #[test]
    fn golden_edge_absent_in_ours_is_regression() {
        let ours = graph(&["a", "b"], &[]);
        let golden = graph(&["a", "b"], &[("a", "b", "calls")]);
        let r = classify(&ours, &golden);
        assert_eq!(
            r.edges_missing,
            vec![("a".to_owned(), "b".to_owned(), "calls".to_owned())]
        );
        assert_eq!(r.edges_matched, 0);
        assert!(r.edges_extra.is_empty());
        assert!(!r.is_regression_free());
    }

    // ── 6. ours has an extra edge → edges_extra, regression-free if golden covered

    #[test]
    fn ours_has_extra_edge_still_regression_free() {
        let ours = graph(&["a", "b"], &[("a", "b", "calls"), ("a", "b", "imports")]);
        let golden = graph(&["a", "b"], &[("a", "b", "calls")]);
        let r = classify(&ours, &golden);
        assert_eq!(r.edges_matched, 1);
        assert_eq!(
            r.edges_extra,
            vec![("a".to_owned(), "b".to_owned(), "imports".to_owned())]
        );
        assert!(r.edges_missing.is_empty());
        assert!(r.is_regression_free());
    }

    // ── 7. nodes_missing is sorted ────────────────────────────────────────────

    #[test]
    fn nodes_missing_output_is_sorted() {
        let ours = graph(&["common"], &[]);
        // Insert in reverse alphabetical order to confirm BTreeSet sorts output.
        let golden = graph(&["common", "zebra", "alpha", "mango"], &[]);
        let r = classify(&ours, &golden);
        assert_eq!(
            r.nodes_missing,
            vec!["alpha".to_owned(), "mango".to_owned(), "zebra".to_owned()]
        );
    }

    // ── 8. nodes_extra is sorted ──────────────────────────────────────────────

    #[test]
    fn nodes_extra_output_is_sorted() {
        let ours = graph(&["common", "zulu", "apple", "mango"], &[]);
        let golden = graph(&["common"], &[]);
        let r = classify(&ours, &golden);
        assert_eq!(
            r.nodes_extra,
            vec!["apple".to_owned(), "mango".to_owned(), "zulu".to_owned()]
        );
    }

    // ── 9. edges_missing is sorted ────────────────────────────────────────────

    #[test]
    fn edges_missing_output_is_sorted() {
        let ours = graph(&["a", "b", "c"], &[]);
        let golden = graph(
            &["a", "b", "c"],
            &[
                ("c", "a", "imports"),
                ("a", "b", "calls"),
                ("b", "c", "contains"),
            ],
        );
        let r = classify(&ours, &golden);
        // BTreeSet orders tuples lexicographically: (a,b,calls) < (b,c,contains) < (c,a,imports)
        let expected = vec![
            ("a".to_owned(), "b".to_owned(), "calls".to_owned()),
            ("b".to_owned(), "c".to_owned(), "contains".to_owned()),
            ("c".to_owned(), "a".to_owned(), "imports".to_owned()),
        ];
        assert_eq!(r.edges_missing, expected);
    }

    // ── 10. edges_extra is sorted ─────────────────────────────────────────────

    #[test]
    fn edges_extra_output_is_sorted() {
        let ours = graph(
            &["a", "b", "c"],
            &[("c", "b", "uses"), ("a", "c", "uses"), ("a", "b", "calls")],
        );
        let golden = graph(&["a", "b", "c"], &[]);
        let r = classify(&ours, &golden);
        let expected = vec![
            ("a".to_owned(), "b".to_owned(), "calls".to_owned()),
            ("a".to_owned(), "c".to_owned(), "uses".to_owned()),
            ("c".to_owned(), "b".to_owned(), "uses".to_owned()),
        ];
        assert_eq!(r.edges_extra, expected);
    }

    // ── 11. partial node overlap — matched count correct ──────────────────────

    #[test]
    fn partial_node_overlap_matched_count() {
        let ours = graph(&["alpha", "beta", "extra"], &[]);
        let golden = graph(&["alpha", "beta", "gamma"], &[]);
        let r = classify(&ours, &golden);
        assert_eq!(r.nodes_matched, 2);
        assert_eq!(r.nodes_missing, vec!["gamma".to_owned()]);
        assert_eq!(r.nodes_extra, vec!["extra".to_owned()]);
        assert!(!r.is_regression_free());
    }

    // ── 12. partial edge overlap — matched count correct ──────────────────────

    #[test]
    fn partial_edge_overlap_matched_count() {
        let shared = ("x", "y", "calls");
        let ours = graph(&["x", "y"], &[shared, ("x", "y", "imports")]);
        let golden = graph(&["x", "y"], &[shared, ("x", "y", "inherits")]);
        let r = classify(&ours, &golden);
        assert_eq!(r.edges_matched, 1);
        assert_eq!(
            r.edges_missing,
            vec![("x".to_owned(), "y".to_owned(), "inherits".to_owned())]
        );
        assert_eq!(
            r.edges_extra,
            vec![("x".to_owned(), "y".to_owned(), "imports".to_owned())]
        );
        assert!(!r.is_regression_free());
    }

    // ── 13. is_regression_free false when nodes missing ───────────────────────

    #[test]
    fn regression_free_false_missing_nodes_only() {
        let ours = graph(&[], &[]);
        let golden = graph(&["phantom"], &[]);
        let r = classify(&ours, &golden);
        assert!(!r.nodes_missing.is_empty());
        assert!(r.edges_missing.is_empty());
        assert!(!r.is_regression_free());
    }

    // ── 14. is_regression_free false when edges missing ───────────────────────

    #[test]
    fn regression_free_false_missing_edges_only() {
        let ours = graph(&["a", "b"], &[]);
        let golden = graph(&["a", "b"], &[("a", "b", "calls")]);
        let r = classify(&ours, &golden);
        assert!(r.nodes_missing.is_empty(), "nodes fully covered");
        assert!(!r.edges_missing.is_empty());
        assert!(!r.is_regression_free());
    }

    // ── 15. both missing nodes and edges → not regression-free ────────────────

    #[test]
    fn regression_free_false_both_missing() {
        let ours = graph(&["a"], &[]);
        let golden = graph(&["a", "b"], &[("a", "b", "calls")]);
        let r = classify(&ours, &golden);
        assert_eq!(r.nodes_missing, vec!["b".to_owned()]);
        assert!(!r.edges_missing.is_empty());
        assert!(!r.is_regression_free());
    }

    // ── 16. golden empty, ours non-empty → all extra, regression-free ─────────

    #[test]
    fn golden_empty_ours_full_all_extra_regression_free() {
        let ours = graph(&["x", "y"], &[("x", "y", "defines")]);
        let golden = graph(&[], &[]);
        let r = classify(&ours, &golden);
        assert_eq!(r.nodes_matched, 0);
        assert_eq!(r.edges_matched, 0);
        assert!(r.nodes_missing.is_empty());
        assert!(r.edges_missing.is_empty());
        assert_eq!(r.nodes_extra.len(), 2);
        assert_eq!(r.edges_extra.len(), 1);
        assert!(r.is_regression_free());
    }

    // ── 17. default ParityReport is regression-free ───────────────────────────

    #[test]
    fn default_parity_report_is_regression_free() {
        let r = ParityReport::default();
        assert!(r.is_regression_free());
        assert_eq!(r.nodes_matched, 0);
        assert_eq!(r.edges_matched, 0);
    }

    // ── 18. only edges differ — nodes regression-free but overall not ──────────

    #[test]
    fn nodes_regression_free_but_edge_regression_fails_overall() {
        let ours = graph(&["a", "b"], &[("a", "b", "calls")]);
        let golden = graph(&["a", "b"], &[("a", "b", "calls"), ("a", "b", "contains")]);
        let r = classify(&ours, &golden);
        assert!(r.nodes_missing.is_empty(), "all golden nodes matched");
        assert_eq!(r.nodes_matched, 2);
        assert_eq!(
            r.edges_missing,
            vec![("a".to_owned(), "b".to_owned(), "contains".to_owned())]
        );
        assert!(!r.is_regression_free());
    }

    // ── 19. multiple golden nodes all missing (empty ours) ────────────────────

    #[test]
    fn all_golden_nodes_missing_when_ours_empty() {
        let golden = graph(&["alpha", "beta", "gamma"], &[]);
        let ours = graph(&[], &[]);
        let r = classify(&ours, &golden);
        assert_eq!(r.nodes_matched, 0);
        assert_eq!(
            r.nodes_missing,
            vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()]
        );
        assert!(r.nodes_extra.is_empty());
    }

    // ── 20. relation type distinguishes edges with same endpoints ─────────────

    #[test]
    fn different_relation_same_endpoints_counted_separately() {
        let ours = graph(&["a", "b"], &[("a", "b", "calls")]);
        let golden = graph(
            &["a", "b"],
            &[
                ("a", "b", "calls"),
                ("a", "b", "imports"),
                ("a", "b", "inherits"),
            ],
        );
        let r = classify(&ours, &golden);
        assert_eq!(r.edges_matched, 1);
        assert_eq!(r.edges_missing.len(), 2);
        assert!(r
            .edges_missing
            .contains(&("a".to_owned(), "b".to_owned(), "imports".to_owned())));
        assert!(r
            .edges_missing
            .contains(&("a".to_owned(), "b".to_owned(), "inherits".to_owned())));
        assert!(!r.is_regression_free());
    }
}
