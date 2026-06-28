//! Token-benchmark — estimated token cost of the export formats (PD analytics, FO-9).
//!
//! Gives an agent a budget-aware sense of how expensive each serialization of the graph is, so it
//! can pick a format that fits its context window. [`estimate_tokens`] uses a deterministic
//! heuristic (≈ one token per 4 bytes — the common rule of thumb) rather than a real BPE
//! tokenizer, so it pulls no dependency and stays byte-stable (R4). Swapping in a real tokenizer
//! later is a localised change to this file only.

use habitat_graph_core::Graph;

/// Estimated token counts for the graph rendered in each export format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenBenchmark {
    /// Estimated tokens of the node-link `graph.json`.
    pub node_link: usize,
    /// Estimated tokens of the human-facing `GRAPH_REPORT.md`.
    pub report: usize,
}

/// Estimates the token count of `text` as `ceil(bytes / 4)` — a deterministic approximation of a
/// BPE tokenizer's output, with no external dependency.
///
/// The heuristic counts UTF-8 **bytes**, not Unicode scalar values, matching the behaviour of
/// typical subword tokenizers (e.g., GPT-family) that operate on raw byte sequences. Multibyte
/// characters therefore count proportionally more than ASCII:
/// a 4-byte emoji contributes the same as four ASCII letters.
///
/// The function is infallible and side-effect-free. Two calls on identical input always return
/// identical values (R4 determinism).
///
/// # Examples
///
/// ```
/// use habitat_graph_export::benchmark::estimate_tokens;
/// assert_eq!(estimate_tokens(""), 0);
/// assert_eq!(estimate_tokens("abcd"), 1);   // 4 ASCII bytes  → ceil(4/4) = 1
/// assert_eq!(estimate_tokens("abcde"), 2);  // 5 ASCII bytes  → ceil(5/4) = 2
/// assert_eq!(estimate_tokens("😀"), 1);      // 4 UTF-8 bytes  → ceil(4/4) = 1
/// assert_eq!(estimate_tokens("中"), 1);      // 3 UTF-8 bytes  → ceil(3/4) = 1
/// ```
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Computes the [`TokenBenchmark`] for `graph` across the available export formats.
///
/// The node-link estimate falls back to `0` if serialization fails (it cannot in practice for a
/// well-formed [`Graph`]); the report estimate is always available.
///
/// Both estimates are deterministic: identical graphs produce identical [`TokenBenchmark`] values
/// (R4 invariant). Call [`Graph::sorted`](habitat_graph_core::Graph::sorted) before this function
/// to obtain the canonical ordering.
///
/// # Examples
///
/// ```
/// use habitat_graph_core::Graph;
/// use habitat_graph_export::benchmark::token_benchmark;
/// let tb = token_benchmark(&Graph::new());
/// assert!(tb.report > 0);   // render always emits at least a title section
/// ```
#[must_use]
pub fn token_benchmark(graph: &Graph) -> TokenBenchmark {
    // `to_node_link` only errors on a serde failure, which cannot occur for a well-formed `Graph`;
    // the `0` fallback is an unreachable documented degrade, not a swallowed real error.
    let node_link = crate::to_node_link(graph).map_or(0, |s| estimate_tokens(&s));
    let report = estimate_tokens(&crate::render_report(graph));
    TokenBenchmark { node_link, report }
}

#[cfg(test)]
mod tests {
    use habitat_graph_core::{
        Community, CommunityId, Confidence, Edge, Graph, Node, NodeId, Span,
    };

    use super::{estimate_tokens, token_benchmark, TokenBenchmark};

    // ── Shared fixtures ───────────────────────────────────────────────────────

    const SPAN: Span = Span::new(0, 1, 1, 1);

    fn make_node(id: u32, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: "test.rs".into(),
            source_location: SPAN,
        }
    }

    fn make_edge(src: u32, tgt: u32, rel: &str) -> Edge {
        Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: rel.to_owned(),
            confidence: Confidence::Extracted,
        }
    }

    fn make_community(id: u32, members: &[u32]) -> Community {
        Community {
            id: CommunityId::new(id),
            label: format!("cluster-{id}"),
            members: members.iter().copied().map(NodeId::new).collect(),
        }
    }

    // ════════════════════════════════════════════════════════════════════════════
    // Group 1 — estimate_tokens: boundary conditions
    // ════════════════════════════════════════════════════════════════════════════

    #[test]
    fn estimate_empty_string_is_zero() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_one_byte_is_one() {
        assert_eq!(estimate_tokens("a"), 1);
    }

    #[test]
    fn estimate_two_bytes_is_one() {
        assert_eq!(estimate_tokens("ab"), 1);
    }

    #[test]
    fn estimate_three_bytes_is_one() {
        assert_eq!(estimate_tokens("abc"), 1);
    }

    #[test]
    fn estimate_four_bytes_is_one_exact_boundary() {
        // ceil(4/4) = 1 — exact divisor boundary, no rounding.
        assert_eq!(estimate_tokens("abcd"), 1);
    }

    #[test]
    fn estimate_five_bytes_is_two_just_over_boundary() {
        // ceil(5/4) = 2 — first value that crosses the boundary.
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn estimate_seven_bytes_is_two() {
        assert_eq!(estimate_tokens("abcdefg"), 2);
    }

    #[test]
    fn estimate_eight_bytes_is_two_second_exact_boundary() {
        // ceil(8/4) = 2 — second exact boundary.
        assert_eq!(estimate_tokens("abcdefgh"), 2);
    }

    #[test]
    fn estimate_nine_bytes_is_three() {
        assert_eq!(estimate_tokens("abcdefghi"), 3);
    }

    #[test]
    fn estimate_twelve_bytes_is_three() {
        assert_eq!(estimate_tokens("abcdefghijkl"), 3);
    }

    #[test]
    fn estimate_thirteen_bytes_is_four() {
        assert_eq!(estimate_tokens("abcdefghijklm"), 4);
    }

    #[test]
    fn estimate_sixteen_bytes_is_four() {
        // ceil(16/4) = 4 — fourth exact boundary.
        assert_eq!(estimate_tokens("abcdefghijklmnop"), 4);
    }

    #[test]
    fn estimate_seventeen_bytes_is_five() {
        assert_eq!(estimate_tokens("abcdefghijklmnopq"), 5);
    }

    // ════════════════════════════════════════════════════════════════════════════
    // Group 2 — estimate_tokens: larger inputs
    // ════════════════════════════════════════════════════════════════════════════

    #[test]
    fn estimate_hundred_bytes_is_twenty_five() {
        let s = "a".repeat(100);
        assert_eq!(estimate_tokens(&s), 25);
    }

    #[test]
    fn estimate_hundred_one_bytes_is_twenty_six() {
        // Boundary: 100 → 25; 101 → 26 (first token of next block).
        let s = "a".repeat(101);
        assert_eq!(estimate_tokens(&s), 26);
    }

    #[test]
    fn estimate_thousand_bytes_is_two_hundred_fifty() {
        let s = "a".repeat(1000);
        assert_eq!(estimate_tokens(&s), 250);
    }

    #[test]
    fn estimate_thousand_one_bytes_is_two_hundred_fifty_one() {
        let s = "a".repeat(1001);
        assert_eq!(estimate_tokens(&s), 251);
    }

    // ════════════════════════════════════════════════════════════════════════════
    // Group 3 — estimate_tokens: whitespace and special ASCII
    // ════════════════════════════════════════════════════════════════════════════

    #[test]
    fn estimate_four_spaces_is_one() {
        // Whitespace bytes count; a 4-space indent is exactly 1 token.
        assert_eq!(estimate_tokens("    "), 1);
    }

    #[test]
    fn estimate_four_newlines_is_one() {
        assert_eq!(estimate_tokens("\n\n\n\n"), 1);
    }

    #[test]
    fn estimate_five_newlines_is_two() {
        assert_eq!(estimate_tokens("\n\n\n\n\n"), 2);
    }

    // ════════════════════════════════════════════════════════════════════════════
    // Group 4 — estimate_tokens: multibyte UTF-8 (counts BYTES, not scalar values)
    // ════════════════════════════════════════════════════════════════════════════

    #[test]
    fn estimate_two_byte_utf8_char_counts_two_bytes() {
        // U+00E9 LATIN SMALL LETTER E WITH ACUTE encodes to 2 UTF-8 bytes.
        // ceil(2/4) = 1 token.
        assert_eq!("é".len(), 2, "precondition: é must encode to 2 bytes");
        assert_eq!(estimate_tokens("é"), 1);
    }

    #[test]
    fn estimate_three_byte_utf8_char_counts_three_bytes() {
        // U+4E2D CJK UNIFIED IDEOGRAPH encodes to 3 UTF-8 bytes.
        // ceil(3/4) = 1 token.
        assert_eq!("中".len(), 3, "precondition: 中 must encode to 3 bytes");
        assert_eq!(estimate_tokens("中"), 1);
    }

    #[test]
    fn estimate_two_three_byte_utf8_chars_sum_to_six_bytes() {
        // "中文" = 3 + 3 = 6 bytes → ceil(6/4) = 2 tokens.
        assert_eq!("中文".len(), 6, "precondition: 中文 must encode to 6 bytes");
        assert_eq!(estimate_tokens("中文"), 2);
    }

    #[test]
    fn estimate_four_byte_utf8_char_counts_four_bytes() {
        // U+1F600 GRINNING FACE encodes to 4 UTF-8 bytes → ceil(4/4) = 1 token.
        assert_eq!("😀".len(), 4, "precondition: 😀 must encode to 4 bytes");
        assert_eq!(estimate_tokens("😀"), 1);
    }

    #[test]
    fn estimate_four_byte_utf8_plus_ascii_is_five_bytes_two_tokens() {
        // "😀a" = 4 + 1 = 5 bytes → ceil(5/4) = 2 tokens.
        // Confirms the heuristic counts bytes, not Unicode scalar values.
        assert_eq!("😀a".len(), 5, "precondition: 😀a must encode to 5 bytes");
        assert_eq!(estimate_tokens("😀a"), 2);
    }

    #[test]
    fn estimate_mixed_ascii_and_multibyte_counts_total_bytes() {
        // "abcé" = 3 + 2 = 5 bytes → ceil(5/4) = 2 tokens, not ceil(4/4) = 1.
        assert_eq!("abcé".len(), 5, "precondition: abcé must encode to 5 bytes");
        assert_eq!(estimate_tokens("abcé"), 2);
    }

    #[test]
    fn estimate_emoji_and_four_ascii_yield_same_token_count() {
        // A 4-byte emoji has the same byte count as 4 ASCII characters.
        // Both should produce 1 token, proving the byte-counting behaviour.
        assert_eq!(estimate_tokens("😀"), estimate_tokens("abcd"));
    }

    // ════════════════════════════════════════════════════════════════════════════
    // Group 5 — estimate_tokens: monotonicity and formula consistency
    // ════════════════════════════════════════════════════════════════════════════

    #[test]
    fn estimate_is_monotonically_nondecreasing() {
        // Adding any character must never reduce the token count.
        let mut prev = estimate_tokens("");
        for n in 1..=32_usize {
            let s = "x".repeat(n);
            let cur = estimate_tokens(&s);
            assert!(
                cur >= prev,
                "estimate_tokens must be monotonic; decreased at n={n}: {prev} → {cur}"
            );
            prev = cur;
        }
    }

    #[test]
    fn estimate_doubling_at_exact_boundary_doubles_tokens() {
        // 8 bytes → 2 tokens; 16 bytes → 4 tokens. Ratio is preserved at exact boundaries.
        let s8 = "a".repeat(8);
        let s16 = "a".repeat(16);
        assert_eq!(
            estimate_tokens(&s16),
            2 * estimate_tokens(&s8),
            "doubling bytes at exact boundaries must double tokens"
        );
    }

    #[test]
    fn estimate_matches_div_ceil_formula_for_range() {
        // Exhaustive check of the formula against every byte length 0..=64.
        for n in 0..=64_usize {
            let s = "x".repeat(n);
            let expected = n.div_ceil(4);
            assert_eq!(
                estimate_tokens(&s),
                expected,
                "formula mismatch at n={n}: expected {expected}"
            );
        }
    }

    // ════════════════════════════════════════════════════════════════════════════
    // Group 6 — token_benchmark: empty graph invariants
    // ════════════════════════════════════════════════════════════════════════════

    #[test]
    fn benchmark_empty_graph_does_not_panic() {
        let _ = token_benchmark(&Graph::new());
    }

    #[test]
    fn benchmark_empty_graph_report_is_positive() {
        // render_report always emits at least a title and section headings.
        assert!(
            token_benchmark(&Graph::new()).report > 0,
            "empty-graph report must be > 0 (render always has a title)"
        );
    }

    #[test]
    fn benchmark_empty_graph_node_link_is_positive() {
        // to_node_link always produces a non-empty JSON envelope.
        assert!(
            token_benchmark(&Graph::new()).node_link > 0,
            "empty-graph node_link must be > 0 (JSON envelope is always non-empty)"
        );
    }

    #[test]
    fn benchmark_empty_graph_is_deterministic() {
        let g = Graph::new();
        assert_eq!(
            token_benchmark(&g),
            token_benchmark(&g),
            "token_benchmark must be deterministic on empty graph"
        );
    }

    // ════════════════════════════════════════════════════════════════════════════
    // Group 7 — token_benchmark: agreement with manual estimate_tokens call
    // ════════════════════════════════════════════════════════════════════════════

    #[test]
    fn benchmark_node_link_matches_manual_estimate_on_empty_graph() {
        let g = Graph::new();
        let expected = estimate_tokens(&crate::to_node_link(&g).unwrap());
        assert_eq!(token_benchmark(&g).node_link, expected);
    }

    #[test]
    fn benchmark_report_matches_manual_estimate_on_empty_graph() {
        let g = Graph::new();
        let expected = estimate_tokens(&crate::render_report(&g));
        assert_eq!(token_benchmark(&g).report, expected);
    }

    #[test]
    fn benchmark_node_link_matches_manual_estimate_with_content() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "alpha"));
        g.nodes.push(make_node(2, "beta"));
        g.edges.push(make_edge(1, 2, "calls"));
        let expected = estimate_tokens(&crate::to_node_link(&g).unwrap());
        assert_eq!(token_benchmark(&g).node_link, expected);
    }

    #[test]
    fn benchmark_report_matches_manual_estimate_with_content() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "alpha"));
        g.nodes.push(make_node(2, "beta"));
        g.edges.push(make_edge(1, 2, "calls"));
        let expected = estimate_tokens(&crate::render_report(&g));
        assert_eq!(token_benchmark(&g).report, expected);
    }

    // ════════════════════════════════════════════════════════════════════════════
    // Group 8 — token_benchmark: determinism with graph content
    // ════════════════════════════════════════════════════════════════════════════

    #[test]
    fn benchmark_deterministic_with_nodes() {
        let mut g = Graph::new();
        for i in 0..5_u32 {
            g.nodes.push(make_node(i, &format!("symbol_{i}")));
        }
        assert_eq!(
            token_benchmark(&g),
            token_benchmark(&g),
            "token_benchmark must be deterministic given identical nodes"
        );
    }

    #[test]
    fn benchmark_deterministic_with_edges() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "a"));
        g.nodes.push(make_node(2, "b"));
        g.edges.push(make_edge(1, 2, "calls"));
        g.edges.push(make_edge(2, 1, "imports"));
        assert_eq!(
            token_benchmark(&g),
            token_benchmark(&g),
            "token_benchmark must be deterministic given identical edges"
        );
    }

    #[test]
    fn benchmark_deterministic_with_communities() {
        let mut g = Graph::new();
        for i in 0..4_u32 {
            g.nodes.push(make_node(i, &format!("m{i}")));
        }
        g.communities.push(make_community(0, &[0, 1]));
        g.communities.push(make_community(1, &[2, 3]));
        assert_eq!(
            token_benchmark(&g),
            token_benchmark(&g),
            "token_benchmark must be deterministic given identical communities"
        );
    }

    // ════════════════════════════════════════════════════════════════════════════
    // Group 9 — token_benchmark: monotonicity with graph size
    // ════════════════════════════════════════════════════════════════════════════

    #[test]
    fn node_link_grows_monotonically_as_nodes_added() {
        let mut g = Graph::new();
        let mut prev = token_benchmark(&g).node_link;
        for i in 0..10_u32 {
            g.nodes.push(make_node(i, &format!("node_label_{i}")));
            let cur = token_benchmark(&g).node_link;
            assert!(
                cur >= prev,
                "node_link must not decrease after adding node {i}: {prev} → {cur}"
            );
            prev = cur;
        }
    }

    #[test]
    fn report_grows_monotonically_as_nodes_added() {
        let mut g = Graph::new();
        let mut prev = token_benchmark(&g).report;
        for i in 0..10_u32 {
            g.nodes.push(make_node(i, &format!("node_label_{i}")));
            let cur = token_benchmark(&g).report;
            assert!(
                cur >= prev,
                "report must not decrease after adding node {i}: {prev} → {cur}"
            );
            prev = cur;
        }
    }

    #[test]
    fn node_link_grows_monotonically_as_edges_added() {
        let mut g = Graph::new();
        for i in 0..12_u32 {
            g.nodes.push(make_node(i, &format!("n{i}")));
        }
        let mut prev = token_benchmark(&g).node_link;
        for i in 0..10_u32 {
            g.edges.push(make_edge(i, i + 1, "calls"));
            let cur = token_benchmark(&g).node_link;
            assert!(
                cur >= prev,
                "node_link must not decrease after adding edge {i}: {prev} → {cur}"
            );
            prev = cur;
        }
    }

    #[test]
    fn short_community_id_can_shrink_node_link_versus_null() {
        // to_node_link embeds the community ID per-node (not in a separate array).
        // A single-digit ID ("0", 1 byte) is shorter than JSON `null` (4 bytes), so embedding
        // it can decrease node_link — this is expected and documents the per-node encoding.
        let mut g = Graph::new();
        g.nodes.push(make_node(0, "alpha"));
        g.nodes.push(make_node(1, "beta"));
        let before = token_benchmark(&g).node_link;
        // Community ID 0 → "0" (1 byte/node) replacing null (4 bytes/node): net −6 bytes total.
        g.communities.push(make_community(0, &[0, 1]));
        let after = token_benchmark(&g).node_link;
        // After is ≤ before because node entries got shorter.
        assert!(
            after <= before,
            "single-digit community ID should produce smaller or equal node_link: {before} → {after}"
        );
        // The value must remain positive regardless.
        assert!(after > 0, "node_link must stay positive after embedding community: {after}");
    }

    #[test]
    fn large_community_id_increases_node_link_versus_null() {
        // A 5-digit community ID ("99999", 5 bytes) is longer than JSON `null` (4 bytes),
        // so embedding it in two node entries adds bytes and can increase node_link.
        let mut g = Graph::new();
        g.nodes.push(make_node(0, "alpha"));
        g.nodes.push(make_node(1, "beta"));
        let before = token_benchmark(&g).node_link;
        // Community ID 99999 → "99999" (5 bytes/node) vs null (4 bytes/node): net +2 bytes total.
        g.communities.push(make_community(99_999, &[0, 1]));
        let after = token_benchmark(&g).node_link;
        assert!(
            after >= before,
            "5-digit community ID must produce >= node_link: {before} → {after}"
        );
    }

    #[test]
    fn report_grows_monotonically_as_communities_added() {
        let mut g = Graph::new();
        for i in 0..10_u32 {
            g.nodes.push(make_node(i, &format!("n{i}")));
        }
        let mut prev = token_benchmark(&g).report;
        for cid in 0..5_u32 {
            g.communities
                .push(make_community(cid, &[cid * 2, cid * 2 + 1]));
            let cur = token_benchmark(&g).report;
            assert!(
                cur >= prev,
                "report must not decrease after adding community {cid}: {prev} → {cur}"
            );
            prev = cur;
        }
    }

    // ════════════════════════════════════════════════════════════════════════════
    // Group 10 — token_benchmark: larger graph exceeds smaller graph
    // ════════════════════════════════════════════════════════════════════════════

    #[test]
    fn ten_node_graph_node_link_exceeds_one_node_graph() {
        let mut g1 = Graph::new();
        g1.nodes.push(make_node(1, "solo_symbol"));

        let mut g10 = Graph::new();
        for i in 0..10_u32 {
            g10.nodes.push(make_node(i, &format!("sym_{i}")));
        }
        assert!(
            token_benchmark(&g10).node_link >= token_benchmark(&g1).node_link,
            "10-node graph must have >= node_link tokens than 1-node graph"
        );
    }

    #[test]
    fn ten_node_graph_report_exceeds_one_node_graph() {
        let mut g1 = Graph::new();
        g1.nodes.push(make_node(1, "solo_symbol"));

        let mut g10 = Graph::new();
        for i in 0..10_u32 {
            g10.nodes.push(make_node(i, &format!("sym_{i}")));
        }
        assert!(
            token_benchmark(&g10).report >= token_benchmark(&g1).report,
            "10-node graph must have >= report tokens than 1-node graph"
        );
    }

    // ════════════════════════════════════════════════════════════════════════════
    // Group 11 — TokenBenchmark struct properties
    // ════════════════════════════════════════════════════════════════════════════

    #[test]
    fn token_benchmark_struct_is_clone() {
        let tb = token_benchmark(&Graph::new());
        #[allow(clippy::clone_on_copy)]
        let tb2 = tb.clone();
        assert_eq!(tb, tb2, "Clone must produce an equal value");
    }

    #[test]
    fn token_benchmark_struct_is_copy() {
        let tb = token_benchmark(&Graph::new());
        let tb2 = tb; // Copy semantics: tb remains accessible.
        assert_eq!(tb.node_link, tb2.node_link, "Copy: original field accessible after copy");
        assert_eq!(tb.report, tb2.report, "Copy: original field accessible after copy");
    }

    #[test]
    fn token_benchmark_eq_when_fields_equal() {
        let tb1 = TokenBenchmark { node_link: 10, report: 5 };
        let tb2 = TokenBenchmark { node_link: 10, report: 5 };
        assert_eq!(tb1, tb2);
    }

    #[test]
    fn token_benchmark_ne_when_node_link_differs() {
        let tb1 = TokenBenchmark { node_link: 10, report: 5 };
        let tb2 = TokenBenchmark { node_link: 11, report: 5 };
        assert_ne!(tb1, tb2);
    }

    #[test]
    fn token_benchmark_ne_when_report_differs() {
        let tb1 = TokenBenchmark { node_link: 10, report: 5 };
        let tb2 = TokenBenchmark { node_link: 10, report: 6 };
        assert_ne!(tb1, tb2);
    }

    #[test]
    fn token_benchmark_debug_contains_both_field_names() {
        let tb = TokenBenchmark { node_link: 7, report: 3 };
        let dbg = format!("{tb:?}");
        assert!(dbg.contains("node_link"), "Debug output must mention node_link: {dbg}");
        assert!(dbg.contains("report"), "Debug output must mention report: {dbg}");
    }

    #[test]
    fn token_benchmark_zero_fallback_is_valid_construct() {
        // Documents the infallible-fallback path: node_link=0 is the degrade when serialization
        // fails (unreachable in practice for well-formed graphs, but the API allows it).
        let tb = TokenBenchmark { node_link: 0, report: 0 };
        assert_eq!(tb.node_link, 0);
        assert_eq!(tb.report, 0);
    }

    // ════════════════════════════════════════════════════════════════════════════
    // Group 12 — token_benchmark: robustness
    // ════════════════════════════════════════════════════════════════════════════

    #[test]
    fn dangerous_bidi_override_label_does_not_panic() {
        // U+202E RIGHT-TO-LEFT OVERRIDE is a Trojan-Source payload. The export layer should
        // sanitise it without panicking.
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "evil\u{202E}label"));
        g.edges.push(make_edge(1, 1, "calls"));
        let tb = token_benchmark(&g);
        assert!(tb.node_link > 0, "node_link must be > 0 even with dangerous label");
        assert!(tb.report > 0, "report must be > 0 even with dangerous label");
    }

    #[test]
    fn large_graph_both_fields_are_positive() {
        let mut g = Graph::new();
        for i in 0..100_u32 {
            g.nodes.push(make_node(i, &format!("function_{i}_implementation")));
        }
        for i in 0..99_u32 {
            g.edges.push(make_edge(i, i + 1, "calls"));
        }
        for c in 0..10_u32 {
            let members: Vec<u32> = (c * 10..(c + 1) * 10).collect();
            g.communities.push(make_community(c, &members));
        }
        let tb = token_benchmark(&g);
        assert!(tb.node_link > 0, "node_link must be positive for a 100-node graph");
        assert!(tb.report > 0, "report must be positive for a 100-node graph");
    }

    #[test]
    fn all_three_confidence_variants_produce_valid_benchmark() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "src"));
        g.nodes.push(make_node(2, "mid"));
        g.nodes.push(make_node(3, "dst"));
        g.edges.push(Edge {
            source: NodeId::new(1),
            target: NodeId::new(2),
            relation: "extracted_rel".into(),
            confidence: Confidence::Extracted,
        });
        g.edges.push(Edge {
            source: NodeId::new(2),
            target: NodeId::new(3),
            relation: "inferred_rel".into(),
            confidence: Confidence::Inferred,
        });
        g.edges.push(Edge {
            source: NodeId::new(3),
            target: NodeId::new(1),
            relation: "ambiguous_rel".into(),
            confidence: Confidence::Ambiguous,
        });
        let tb = token_benchmark(&g);
        assert!(tb.node_link > 0);
        assert!(tb.report > 0);
    }

    // ════════════════════════════════════════════════════════════════════════════
    // Group 13 — token_benchmark: relative size of node_link vs report
    // ════════════════════════════════════════════════════════════════════════════

    #[test]
    fn node_link_exceeds_or_equals_report_for_nontrivial_graph() {
        // The node-link JSON carries structural weight (weights, source_file, source_location,
        // confidence strings, JSON envelope overhead) that is absent from the Markdown summary,
        // so for a non-trivial graph node_link should be at least as large as report.
        let mut g = Graph::new();
        for i in 0..10_u32 {
            g.nodes.push(make_node(i, &format!("module_function_{i}")));
        }
        for i in 0..9_u32 {
            g.edges.push(make_edge(i, i + 1, "calls"));
        }
        let tb = token_benchmark(&g);
        assert!(
            tb.node_link >= tb.report,
            "node_link ({}) must be >= report ({}) for a 10-node/9-edge graph",
            tb.node_link,
            tb.report
        );
    }
}
