//! Warm label index (FO-3) — eliminates the O(n·m) substring scan over node labels.
//!
//! A [`LabelIndex`] is built once from a [`Graph`] and answers case-insensitive substring
//! queries in sub-linear time (for needles ≥ 3 chars) via a **trigram inverted index**:
//!
//! * **Build:** each node label is lowercased; overlapping char-based 3-grams are extracted and
//!   inserted into a `HashMap<[char; 3], Vec<NodeId>>` posting list. Each posting list is sorted
//!   ascending and deduplicated.
//! * **Find (needle ≥ 3 chars):** extract the needle's trigrams, intersect their posting lists
//!   (sorted-merge, O(k·p) where k = trigram count and p = shortest list length), then verify
//!   each candidate against the stored lowercase label — trigrams *over-approximate* containment
//!   (a label can contain all of the needle's trigrams yet not contain the needle in order), so
//!   verification makes the result exact.
//! * **Find (needle < 3 chars, including empty):** trigrams are not informative for very short
//!   needles, so the index falls back to a full scan over the stored lowercase labels. Empty needle
//!   matches every node (a degenerate-but-correct full-scan shortcut).
//!
//! **Trigram granularity:** trigrams are char-based (not byte-based). A multi-byte Unicode scalar
//! is a single unit; trigrams never split code points. This means Unicode labels and Unicode
//! needles are handled correctly without any additional normalisation.
//!
//! **Correctness invariant (R4 / FO-3):** for every graph and every needle,
//! `LabelIndex::build(g).find(n)` returns *exactly* the same id-set as the naïve
//! `node.label.to_lowercase().contains(needle.to_lowercase())` scan, in ascending [`NodeId`]
//! order, with no duplicates.

use std::cmp::Ordering;
use std::collections::HashMap;

use habitat_graph_core::{Graph, NodeId};

// ── private helpers ───────────────────────────────────────────────────────────

/// Extracts overlapping char-based 3-grams from `s`.
///
/// Returns an empty `Vec` for strings shorter than 3 chars.
/// Because iteration is over `char`s (not bytes), multi-byte Unicode scalars are each one unit
/// and trigrams never split code points.
fn extract_trigrams(s: &str) -> Vec<[char; 3]> {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < 3 {
        return Vec::new();
    }
    chars
        .windows(3)
        .map(|w| [w[0], w[1], w[2]])
        .collect()
}

/// Merge-intersects two sorted, deduplicated [`NodeId`] slices.
///
/// Returns a sorted, deduplicated `Vec<NodeId>` containing only ids present in both `a` and `b`.
/// Both inputs must be sorted ascending with no duplicates; the output inherits those properties.
/// Time complexity: O(|a| + |b|).
fn intersect_sorted(a: &[NodeId], b: &[NodeId]) -> Vec<NodeId> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0_usize, 0_usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
            Ordering::Less => i += 1,
            Ordering::Greater => j += 1,
        }
    }
    out
}

// ── public API ────────────────────────────────────────────────────────────────

/// A prebuilt trigram inverted index over node labels for fast case-insensitive substring lookup.
///
/// # Building
///
/// Call [`LabelIndex::build`] once per graph version. For the warm daemon (FO-6) the index is
/// held across queries so the per-query cost drops from O(n·m) scanning to sub-linear (for
/// needles ≥ 3 chars), with an exact-containment verification step for trigram candidates.
///
/// # Querying
///
/// [`LabelIndex::find`] returns a sorted `Vec<NodeId>` of matching nodes (ascending, distinct).
/// The returned id-set is *identical* to the brute-force scan result for every input.
///
/// # Determinism
///
/// The index is fully deterministic (R4): `build` + `find` produce byte-identical outputs on
/// repeated calls with the same inputs, regardless of internal `HashMap` insertion order.
#[derive(Debug, Clone, Default)]
pub struct LabelIndex {
    /// Trigram → sorted (ascending), deduplicated posting list of [`NodeId`]s.
    trigram_index: HashMap<[char; 3], Vec<NodeId>>,
    /// Lowercased label per node, keyed by [`NodeId`].
    ///
    /// Used for (a) the short-needle fallback scan and (b) exact-containment verification of
    /// trigram candidates (the trigram index over-approximates; this map makes it exact).
    lower_labels: HashMap<NodeId, String>,
    /// All indexed [`NodeId`]s in ascending order.
    ///
    /// Cloned directly as the result for an empty-needle query (match-all fast path).
    all_ids: Vec<NodeId>,
}

impl LabelIndex {
    /// Builds a [`LabelIndex`] from `graph`'s node labels.
    ///
    /// Each label is lowercased; overlapping char 3-grams are inserted into the trigram posting
    /// lists. Posting lists are sorted ascending and deduplicated. The index is ready for
    /// [`Self::find`] immediately after construction.
    #[must_use]
    pub fn build(graph: &Graph) -> Self {
        let mut lower_labels: HashMap<NodeId, String> =
            HashMap::with_capacity(graph.nodes.len());
        let mut trigram_index: HashMap<[char; 3], Vec<NodeId>> = HashMap::new();

        for node in &graph.nodes {
            let lower = node.label.to_lowercase();
            for tg in extract_trigrams(&lower) {
                trigram_index.entry(tg).or_default().push(node.id);
            }
            lower_labels.insert(node.id, lower);
        }

        // Sort and dedup every posting list so `intersect_sorted` is valid.
        for list in trigram_index.values_mut() {
            list.sort_unstable();
            list.dedup();
        }

        // Build sorted all_ids for the empty-needle match-all case.
        let mut all_ids: Vec<NodeId> = lower_labels.keys().copied().collect();
        all_ids.sort_unstable();

        Self {
            trigram_index,
            lower_labels,
            all_ids,
        }
    }

    /// Returns the ids of nodes whose label contains `needle` (case-insensitive), ascending by id.
    ///
    /// * **Empty `needle`** matches every node (a degenerate substring of any string).
    /// * **Needle < 3 chars** — the index falls back to a full scan over stored lowercased labels
    ///   because trigrams are not informative for very short patterns.
    /// * **Needle ≥ 3 chars** — the trigram inverted index is used: candidate node ids are
    ///   obtained by intersecting the posting lists of the needle's trigrams, then each candidate's
    ///   stored label is verified for exact containment (trigrams *over-approximate*).
    ///
    /// The returned `Vec<NodeId>` is always sorted ascending and contains no duplicates.
    #[must_use]
    pub fn find(&self, needle: &str) -> Vec<NodeId> {
        let nl = needle.to_lowercase();
        let needle_chars: Vec<char> = nl.chars().collect();

        if needle_chars.is_empty() {
            // Empty needle matches every node — return all ids directly.
            return self.all_ids.clone();
        }

        if needle_chars.len() < 3 {
            // Short-needle fallback: full scan over stored lowercased labels.
            let mut out: Vec<NodeId> = self
                .lower_labels
                .iter()
                .filter(|(_, label)| label.contains(nl.as_str()))
                .map(|(id, _)| *id)
                .collect();
            out.sort_unstable();
            return out;
        }

        // Needle ≥ 3 chars: trigram intersection path.
        let tgs: Vec<[char; 3]> = needle_chars
            .windows(3)
            .map(|w| [w[0], w[1], w[2]])
            .collect();

        // Start with the posting list for the first trigram.
        let Some(first) = self.trigram_index.get(&tgs[0]) else {
            return Vec::new();
        };
        let mut candidates: Vec<NodeId> = first.clone();

        // Intersect remaining trigram posting lists (short-circuit on empty).
        for tg in &tgs[1..] {
            if candidates.is_empty() {
                return Vec::new();
            }
            match self.trigram_index.get(tg) {
                Some(list) => candidates = intersect_sorted(&candidates, list),
                None => return Vec::new(),
            }
        }

        // Verify: retain only candidates whose stored label actually contains the needle.
        // The trigram index over-approximates (all trigrams present does not guarantee the needle
        // appears in order), so this step is required for correctness.
        candidates.retain(|id| {
            self.lower_labels
                .get(id)
                .is_some_and(|label| label.contains(nl.as_str()))
        });

        // `candidates` is already sorted ascending (posting lists were sorted; sorted-merge
        // intersection preserves order; `retain` preserves order).
        candidates
    }

    /// Number of indexed nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.all_ids.len()
    }

    /// Whether the index contains no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.all_ids.is_empty()
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use habitat_graph_core::{Graph, Node, NodeId, Span};

    use super::{extract_trigrams, intersect_sorted, LabelIndex};

    // ── helpers ──────────────────────────────────────────────────────────────

    fn node(id: u32, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: "test.rs".to_owned(),
            source_location: Span::new(0, 1, 1, 1),
        }
    }

    fn graph(labels: &[(u32, &str)]) -> Graph {
        let mut g = Graph::new();
        for (id, l) in labels {
            g.nodes.push(node(*id, l));
        }
        g
    }

    /// Brute-force oracle: the naïve O(n·m) scan that `LabelIndex` must match exactly.
    fn brute_force_find(g: &Graph, needle: &str) -> Vec<NodeId> {
        let nl = needle.to_lowercase();
        let mut ids: Vec<NodeId> = g
            .nodes
            .iter()
            .filter(|n| n.label.to_lowercase().contains(&nl))
            .map(|n| n.id)
            .collect();
        ids.sort_unstable();
        ids
    }

    fn ids_of(v: &[NodeId]) -> Vec<u32> {
        v.iter().map(|n| n.get()).collect()
    }

    // ── empty / minimal ───────────────────────────────────────────────────────

    #[test]
    fn empty_graph_is_empty() {
        let idx = LabelIndex::build(&Graph::new());
        assert!(idx.is_empty());
    }

    #[test]
    fn empty_graph_len_is_zero() {
        assert_eq!(LabelIndex::build(&Graph::new()).len(), 0);
    }

    #[test]
    fn empty_graph_find_returns_empty() {
        let idx = LabelIndex::build(&Graph::new());
        assert!(idx.find("anything").is_empty());
    }

    #[test]
    fn empty_needle_on_empty_graph_returns_empty() {
        assert!(LabelIndex::build(&Graph::new()).find("").is_empty());
    }

    // ── single-node ───────────────────────────────────────────────────────────

    #[test]
    fn single_node_exact_label_match() {
        let idx = LabelIndex::build(&graph(&[(1, "HttpClient")]));
        assert_eq!(ids_of(&idx.find("HttpClient")), vec![1]);
    }

    #[test]
    fn single_node_partial_substring_hit() {
        let idx = LabelIndex::build(&graph(&[(1, "HttpClient")]));
        assert_eq!(ids_of(&idx.find("Client")), vec![1]);
    }

    #[test]
    fn single_node_no_match_returns_empty() {
        let idx = LabelIndex::build(&graph(&[(1, "foo")]));
        assert!(idx.find("zzz").is_empty());
    }

    // ── case-insensitivity ────────────────────────────────────────────────────

    #[test]
    fn lowercase_needle_uppercase_label() {
        let idx = LabelIndex::build(&graph(&[(1, "FOOBAR")]));
        assert_eq!(ids_of(&idx.find("foobar")), vec![1]);
    }

    #[test]
    fn uppercase_needle_lowercase_label() {
        let idx = LabelIndex::build(&graph(&[(1, "foobar")]));
        assert_eq!(ids_of(&idx.find("FOOBAR")), vec![1]);
    }

    #[test]
    fn mixed_case_both_sides() {
        let idx = LabelIndex::build(&graph(&[(1, "FoO_BaR")]));
        assert_eq!(ids_of(&idx.find("fOo_bAr")), vec![1]);
    }

    #[test]
    fn case_insensitive_partial_match() {
        let idx = LabelIndex::build(&graph(&[(1, "HttpClient"), (2, "TcpServer")]));
        assert_eq!(ids_of(&idx.find("HTTP")), vec![1]);
    }

    // ── empty needle (match-all) ──────────────────────────────────────────────

    #[test]
    fn empty_needle_matches_all_nodes() {
        let idx = LabelIndex::build(&graph(&[(3, "gamma"), (1, "alpha"), (2, "beta")]));
        assert_eq!(ids_of(&idx.find("")), vec![1, 2, 3]);
    }

    #[test]
    fn empty_needle_returns_ascending_ids() {
        let idx = LabelIndex::build(&graph(&[(10, "z"), (5, "y"), (1, "x")]));
        assert_eq!(ids_of(&idx.find("")), vec![1, 5, 10]);
    }

    // ── ascending-id contract ─────────────────────────────────────────────────

    #[test]
    fn multiple_matches_ascending_id_not_label_order() {
        // IDs out of label-alphabetic order; result must sort by id, not label.
        let idx = LabelIndex::build(&graph(&[
            (30, "node_c"),
            (10, "node_a"),
            (20, "node_b"),
        ]));
        assert_eq!(ids_of(&idx.find("node")), vec![10, 20, 30]);
    }

    #[test]
    fn ascending_order_three_char_trigram_path() {
        let idx = LabelIndex::build(&graph(&[(3, "alpha"), (1, "alpha2"), (2, "alpha3")]));
        assert_eq!(ids_of(&idx.find("alpha")), vec![1, 2, 3]);
    }

    // ── needle length boundary (fallback vs trigram path) ─────────────────────

    #[test]
    fn one_char_needle_fallback_hit() {
        let idx = LabelIndex::build(&graph(&[(1, "ax"), (2, "by")]));
        assert_eq!(ids_of(&idx.find("a")), vec![1]);
    }

    #[test]
    fn one_char_needle_fallback_miss() {
        let idx = LabelIndex::build(&graph(&[(1, "foo")]));
        assert!(idx.find("z").is_empty());
    }

    #[test]
    fn two_char_needle_fallback_hit() {
        let idx = LabelIndex::build(&graph(&[(1, "module"), (2, "mode")]));
        // Both contain "mo"; "module" also contains it.
        assert_eq!(ids_of(&idx.find("mo")), vec![1, 2]);
    }

    #[test]
    fn two_char_needle_fallback_miss() {
        let idx = LabelIndex::build(&graph(&[(1, "foo"), (2, "bar")]));
        assert!(idx.find("zz").is_empty());
    }

    #[test]
    fn three_char_needle_uses_trigram_path_hit() {
        // Exact 3-char needle — single trigram in the index.
        let idx = LabelIndex::build(&graph(&[(1, "foobar"), (2, "baz")]));
        assert_eq!(ids_of(&idx.find("foo")), vec![1]);
    }

    #[test]
    fn three_char_needle_trigram_path_miss() {
        let idx = LabelIndex::build(&graph(&[(1, "abc")]));
        assert!(idx.find("xyz").is_empty());
    }

    // ── substring position variants ───────────────────────────────────────────

    #[test]
    fn substring_at_start() {
        let idx = LabelIndex::build(&graph(&[(1, "prefixlong")]));
        assert_eq!(ids_of(&idx.find("prefix")), vec![1]);
    }

    #[test]
    fn substring_at_end() {
        let idx = LabelIndex::build(&graph(&[(1, "longsuffix")]));
        assert_eq!(ids_of(&idx.find("suffix")), vec![1]);
    }

    #[test]
    fn substring_in_middle() {
        let idx = LabelIndex::build(&graph(&[(1, "xmiddley")]));
        assert_eq!(ids_of(&idx.find("middle")), vec![1]);
    }

    // ── needle longer than label ───────────────────────────────────────────────

    #[test]
    fn needle_longer_than_any_label_returns_empty() {
        let idx = LabelIndex::build(&graph(&[(1, "hi")]));
        assert!(idx.find("hello_world_this_is_very_long").is_empty());
    }

    // ── duplicate labels, different ids ───────────────────────────────────────

    #[test]
    fn duplicate_labels_both_ids_returned() {
        let idx = LabelIndex::build(&graph(&[(1, "alpha"), (2, "alpha")]));
        assert_eq!(ids_of(&idx.find("alpha")), vec![1, 2]);
    }

    #[test]
    fn duplicate_labels_ascending_id_order() {
        let idx = LabelIndex::build(&graph(&[(5, "dup"), (3, "dup"), (1, "dup")]));
        assert_eq!(ids_of(&idx.find("dup")), vec![1, 3, 5]);
    }

    // ── trigram false-positive filtering (verification step) ─────────────────

    #[test]
    fn trigram_false_positive_is_filtered() {
        // "xabcybcd" contains trigrams "abc" and "bcd" (which are also trigrams of "abcd"),
        // but "xabcybcd".contains("abcd") == false.  The verification step must remove it.
        // "has_abcd" does contain "abcd" and must be returned.
        let idx = LabelIndex::build(&graph(&[
            (1, "xabcybcd"), // false positive — must be filtered
            (2, "has_abcd"), // true positive
        ]));
        assert_eq!(ids_of(&idx.find("abcd")), vec![2]);
    }

    // ── len / is_empty ────────────────────────────────────────────────────────

    #[test]
    fn len_matches_node_count() {
        let g = graph(&[(1, "a"), (2, "b"), (3, "c")]);
        assert_eq!(LabelIndex::build(&g).len(), 3);
    }

    #[test]
    fn is_empty_false_when_nodes_present() {
        assert!(!LabelIndex::build(&graph(&[(1, "x")])).is_empty());
    }

    // ── determinism ──────────────────────────────────────────────────────────

    #[test]
    fn deterministic_repeated_find_calls() {
        let idx = LabelIndex::build(&graph(&[(1, "Alpha"), (2, "beta"), (3, "Gamma")]));
        let first = idx.find("a");
        let second = idx.find("a");
        assert_eq!(first, second);
    }

    #[test]
    fn deterministic_independent_builds() {
        let g = graph(&[(3, "foo"), (1, "foobar"), (2, "baz")]);
        let a = LabelIndex::build(&g).find("foo");
        let b = LabelIndex::build(&g).find("foo");
        assert_eq!(a, b);
    }

    // ── NodeId edge cases ─────────────────────────────────────────────────────

    #[test]
    fn node_id_zero_is_indexed() {
        let idx = LabelIndex::build(&graph(&[(0, "zero_label")]));
        assert_eq!(ids_of(&idx.find("zero")), vec![0]);
    }

    #[test]
    fn node_id_u32_max_is_indexed() {
        let idx = LabelIndex::build(&graph(&[(u32::MAX, "maxid")]));
        assert_eq!(ids_of(&idx.find("maxid")), vec![u32::MAX]);
    }

    // ── label content edge cases ───────────────────────────────────────────────

    #[test]
    fn empty_string_label_matches_empty_needle_only() {
        let idx = LabelIndex::build(&graph(&[(1, ""), (2, "hello")]));
        // Empty needle matches all.
        assert_eq!(ids_of(&idx.find("")), vec![1, 2]);
        // Non-empty needle does not match the empty label.
        assert_eq!(ids_of(&idx.find("hello")), vec![2]);
    }

    #[test]
    fn label_with_spaces_matched_by_substring() {
        let idx = LabelIndex::build(&graph(&[(1, "foo bar baz")]));
        assert_eq!(ids_of(&idx.find("bar")), vec![1]);
    }

    #[test]
    fn label_with_underscores_matched() {
        let idx = LabelIndex::build(&graph(&[(1, "my_func_name")]));
        assert_eq!(ids_of(&idx.find("func")), vec![1]);
    }

    #[test]
    fn needle_all_same_chars() {
        // "aaa" as a needle; label "aaaa" contains it.
        let idx = LabelIndex::build(&graph(&[(1, "aaaa"), (2, "aab")]));
        assert_eq!(ids_of(&idx.find("aaa")), vec![1]);
    }

    // ── Unicode (char-based trigrams) ─────────────────────────────────────────

    #[test]
    fn unicode_label_matched_by_ascii_needle() {
        // Café → lowercase "café".  Substring "caf" (3 chars, all ASCII) must match.
        let idx = LabelIndex::build(&graph(&[(1, "Café")]));
        assert_eq!(ids_of(&idx.find("caf")), vec![1]);
    }

    #[test]
    fn unicode_needle_matches_unicode_label() {
        // Label contains the Greek word "λόγος"; needle is a substring.
        let idx = LabelIndex::build(&graph(&[(1, "λόγος"), (2, "plain")]));
        assert_eq!(ids_of(&idx.find("λόγ")), vec![1]);
    }

    #[test]
    fn unicode_needle_no_match() {
        let idx = LabelIndex::build(&graph(&[(1, "hello")]));
        assert!(idx.find("λόγ").is_empty());
    }

    #[test]
    fn multibyte_char_trigrams_correct() {
        // "αβγδ" has char-trigrams ["αβγ","βγδ"].  Needle "αβγ" must match.
        let idx = LabelIndex::build(&graph(&[(1, "αβγδ")]));
        assert_eq!(ids_of(&idx.find("αβγ")), vec![1]);
    }

    #[test]
    fn unicode_two_char_needle_fallback() {
        // "αβ" is 2 chars → fallback path.
        let idx = LabelIndex::build(&graph(&[(1, "αβγ"), (2, "xyz")]));
        assert_eq!(ids_of(&idx.find("αβ")), vec![1]);
    }

    // ── large graph ───────────────────────────────────────────────────────────

    #[test]
    fn large_graph_only_matching_ids_returned() {
        // 200 nodes: even ids have "module", odd ids have "service".
        let pairs: Vec<(u32, String)> = (0_u32..200)
            .map(|i| {
                let label = if i % 2 == 0 {
                    format!("module_{i}")
                } else {
                    format!("service_{i}")
                };
                (i, label)
            })
            .collect();
        let g: Graph = {
            let mut g = Graph::new();
            for (id, label) in &pairs {
                g.nodes.push(node(*id, label));
            }
            g
        };
        let idx = LabelIndex::build(&g);
        let result = idx.find("module");
        // Must contain only even ids (0, 2, 4, …, 198), ascending.
        let expected: Vec<u32> = (0_u32..200).step_by(2).collect();
        assert_eq!(ids_of(&result), expected);
    }

    // ── brute-force oracle equivalence ────────────────────────────────────────

    fn check_oracle(g: &Graph, needle: &str) {
        let got = LabelIndex::build(g).find(needle);
        let want = brute_force_find(g, needle);
        assert_eq!(
            got, want,
            "LabelIndex diverged from oracle for needle={needle:?}"
        );
    }

    #[test]
    fn oracle_single_node_various_needles() {
        let g = graph(&[(1, "HttpClient")]);
        for needle in ["", "h", "ht", "htt", "http", "Http", "HTTP", "client", "zzz"] {
            check_oracle(&g, needle);
        }
    }

    #[test]
    fn oracle_multi_node_short_needles() {
        let g = graph(&[(1, "alpha"), (2, "beta"), (3, "gamma"), (4, "delta")]);
        for needle in ["", "a", "al", "be", "et", "mm", "zz"] {
            check_oracle(&g, needle);
        }
    }

    #[test]
    fn oracle_multi_node_three_plus_char_needles() {
        let g = graph(&[
            (1, "parse_token"),
            (2, "tokeniser"),
            (3, "emit_token"),
            (4, "scanner"),
        ]);
        for needle in ["tok", "token", "parse", "emit", "scan", "xyz", "oken"] {
            check_oracle(&g, needle);
        }
    }

    #[test]
    fn oracle_unicode_labels() {
        let g = graph(&[(1, "λόγος"), (2, "Café"), (3, "hello"), (4, "Ünit")]);
        for needle in ["", "λ", "λό", "λόγ", "caf", "Caf", "ell", "nit", "ünit", "Ü"] {
            check_oracle(&g, needle);
        }
    }

    #[test]
    fn oracle_duplicate_labels() {
        let g = graph(&[(1, "dup"), (2, "dup"), (3, "other")]);
        for needle in ["", "d", "du", "dup", "other", "zzz"] {
            check_oracle(&g, needle);
        }
    }

    #[test]
    fn oracle_empty_label_node() {
        let g = graph(&[(1, ""), (2, "abc"), (3, "def")]);
        for needle in ["", "a", "ab", "abc", "zzz"] {
            check_oracle(&g, needle);
        }
    }

    #[test]
    fn oracle_large_graph_many_needles() {
        let pairs: Vec<(u32, &str)> = vec![
            (1, "parse_expr"),
            (2, "emit_asm"),
            (3, "HttpRequest"),
            (4, "http_response"),
            (5, "DataStore"),
            (6, "data_loader"),
            (7, "TokenStream"),
            (8, "token"),
            (9, "Config"),
            (10, "ConfigLoader"),
        ];
        let g = graph(&pairs);
        for needle in [
            "", "a", "ht", "htt", "http", "HTTP", "parse", "emit", "token", "Token", "data",
            "config", "Config", "load", "store", "xyz", "expr", "asm",
        ] {
            check_oracle(&g, needle);
        }
    }

    #[test]
    fn oracle_false_positive_scenario() {
        // Node 1 has trigrams of "abcd" but not the substring → must be filtered.
        let g = graph(&[(1, "xabcybcd"), (2, "has_abcd"), (3, "nothing")]);
        check_oracle(&g, "abcd");
    }

    // ── private helper unit tests ─────────────────────────────────────────────

    #[test]
    fn extract_trigrams_empty_string() {
        assert!(extract_trigrams("").is_empty());
    }

    #[test]
    fn extract_trigrams_one_char() {
        assert!(extract_trigrams("a").is_empty());
    }

    #[test]
    fn extract_trigrams_two_chars() {
        assert!(extract_trigrams("ab").is_empty());
    }

    #[test]
    fn extract_trigrams_exactly_three_chars() {
        assert_eq!(extract_trigrams("abc"), vec![['a', 'b', 'c']]);
    }

    #[test]
    fn extract_trigrams_four_chars_gives_two_overlapping() {
        assert_eq!(
            extract_trigrams("abcd"),
            vec![['a', 'b', 'c'], ['b', 'c', 'd']]
        );
    }

    #[test]
    fn extract_trigrams_unicode_chars_not_bytes() {
        // "αβγ" is 3 chars (each 2 bytes) → exactly one trigram.
        assert_eq!(extract_trigrams("αβγ"), vec![['α', 'β', 'γ']]);
    }

    #[test]
    fn intersect_sorted_both_empty() {
        assert!(intersect_sorted(&[], &[]).is_empty());
    }

    #[test]
    fn intersect_sorted_left_empty() {
        assert!(intersect_sorted(&[], &[NodeId::new(1), NodeId::new(2)]).is_empty());
    }

    #[test]
    fn intersect_sorted_right_empty() {
        assert!(intersect_sorted(&[NodeId::new(1)], &[]).is_empty());
    }

    #[test]
    fn intersect_sorted_no_common_elements() {
        let a = [NodeId::new(1), NodeId::new(3)];
        let b = [NodeId::new(2), NodeId::new(4)];
        assert!(intersect_sorted(&a, &b).is_empty());
    }

    #[test]
    fn intersect_sorted_full_overlap() {
        let a = [NodeId::new(1), NodeId::new(2), NodeId::new(3)];
        assert_eq!(intersect_sorted(&a, &a), a.to_vec());
    }

    #[test]
    fn intersect_sorted_partial_overlap() {
        let a = [NodeId::new(1), NodeId::new(2), NodeId::new(4)];
        let b = [NodeId::new(2), NodeId::new(3), NodeId::new(4)];
        assert_eq!(
            intersect_sorted(&a, &b),
            vec![NodeId::new(2), NodeId::new(4)]
        );
    }

    #[test]
    fn intersect_sorted_preserves_ascending_order() {
        let a: Vec<NodeId> = (0_u32..10).map(NodeId::new).collect();
        let b: Vec<NodeId> = (5_u32..15).map(NodeId::new).collect();
        let result = intersect_sorted(&a, &b);
        let expected: Vec<NodeId> = (5_u32..10).map(NodeId::new).collect();
        assert_eq!(result, expected);
    }
}
