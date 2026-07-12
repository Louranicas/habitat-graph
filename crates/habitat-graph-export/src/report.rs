//! `GRAPH_REPORT.md` generation — the human-facing summary.

use std::collections::HashMap;
use std::fmt::Write as _;

use habitat_graph_analyze::degree_centrality;
use habitat_graph_core::{display_safe, Graph, NodeId};

use crate::escape::redact_public_text;

/// Renders a deterministic Markdown report: node/edge/community counts, the top hubs (by
/// [`degree_centrality`](habitat_graph_analyze::degree_centrality)), a per-community summary, and a
/// few suggested queries. Infallible (returns the Markdown string).
///
/// ## Sections
///
/// | Section | Content |
/// |---|---|
/// | Counts | Node, edge, and community totals from [`Graph::counts`]. |
/// | Top Hubs | Up to 10 highest-degree nodes — label (resolved from `graph.nodes`) and degree. Node ids absent from `graph.nodes` are silently skipped. |
/// | Communities | One bullet per community showing its member count, sorted by community id. |
/// | Suggested Queries | 2–3 natural-language starters referencing the top hub label(s). |
///
/// Hubs are ranked by degree (descending), then by [`NodeId`](habitat_graph_core::NodeId)
/// (ascending) on a tie, matching [`degree_centrality`](habitat_graph_analyze::degree_centrality)'s
/// guarantee. All labels pass through [`display_safe`](habitat_graph_core::display_safe) before
/// appearing in the output, providing Trojan-Source and bidi protection.
#[allow(clippy::module_name_repetitions)]
#[must_use]
pub fn render_report(graph: &Graph) -> String {
    // Build NodeId → label lookup; borrows string data from `graph.nodes` for the call's duration.
    let node_labels: HashMap<NodeId, &str> = graph
        .nodes
        .iter()
        .map(|n| (n.id, n.label.as_str()))
        .collect();

    let (n_nodes, n_edges, n_communities) = graph.counts();

    let mut out = String::new();

    // ── Title ────────────────────────────────────────────────────────────────
    out.push_str("# Graph Report\n\n");

    // ── Counts ───────────────────────────────────────────────────────────────
    let _ = write!(
        out,
        "- Nodes: {n_nodes}\n- Edges: {n_edges}\n- Communities: {n_communities}\n\n"
    );

    // ── Top Hubs — up to 10, node ids absent from graph.nodes are skipped ───
    out.push_str("## Top Hubs\n\n");
    let ranked = degree_centrality(graph);
    let top_hubs: Vec<(&str, usize)> = ranked
        .iter()
        .filter_map(|&(node_id, degree)| node_labels.get(&node_id).map(|&label| (label, degree)))
        .take(10)
        .collect();

    if top_hubs.is_empty() {
        out.push_str("_none_\n");
    } else {
        for &(label, degree) in &top_hubs {
            let redacted_label = redact_public_text(label);
            let safe_label = display_safe(&redacted_label);
            let _ = writeln!(out, "- {safe_label} ({degree})");
        }
    }
    out.push('\n');

    // ── Communities — sorted by id for determinism regardless of insertion order ─
    out.push_str("## Communities\n\n");
    let mut communities = graph.communities.clone();
    communities.sort_by_key(|c| c.id);

    if communities.is_empty() {
        out.push_str("_none_\n");
    } else {
        for community in &communities {
            let community_id = community.id.get();
            let n_members = community.members.len();
            let _ = writeln!(out, "- community {community_id}: {n_members} members");
        }
    }
    out.push('\n');

    // ── Suggested Queries ────────────────────────────────────────────────────
    out.push_str("## Suggested Queries\n\n");
    push_suggested_queries(&mut out, &top_hubs);

    out
}

/// Appends 2–3 natural-language query suggestion lines, referencing the top hub label(s) when
/// present.
fn push_suggested_queries(out: &mut String, top_hubs: &[(&str, usize)]) {
    let first_label = top_hubs.first().map(|h| h.0);
    let second_label = top_hubs.get(1).map(|h| h.0);

    match (first_label, second_label) {
        (None, _) => {
            // No hub nodes — emit generic discovery queries.
            out.push_str("- List all nodes in the graph\n");
            out.push_str("- Show all edges between nodes\n");
            out.push_str("- Find isolated nodes with no connections\n");
        }
        (Some(first), None) => {
            // Single hub — two specific queries + a generic path query.
            let redacted = redact_public_text(first);
            let safe = display_safe(&redacted);
            let _ = writeln!(out, "- Show all edges connected to `{safe}`");
            let _ = writeln!(out, "- Which nodes does `{safe}` depend on?");
            out.push_str("- Find the shortest path between any two nodes\n");
        }
        (Some(first), Some(second)) => {
            // Two or more hubs — three queries referencing the top two.
            let redacted_first = redact_public_text(first);
            let redacted_second = redact_public_text(second);
            let safe_first = display_safe(&redacted_first);
            let safe_second = display_safe(&redacted_second);
            let _ = writeln!(out, "- Show all edges connected to `{safe_first}`");
            let _ = writeln!(out, "- Which community contains `{safe_second}`?");
            let _ = writeln!(
                out,
                "- Find the path between `{safe_first}` and `{safe_second}`"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::render_report;
    use habitat_graph_core::{Community, CommunityId, Confidence, Edge, Graph, Node, NodeId, Span};

    // ── Test fixtures ────────────────────────────────────────────────────────

    const SPAN: Span = Span::new(0, 1, 1, 1);

    #[must_use]
    fn make_node(id: u32, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: "test.rs".into(),
            source_location: SPAN,
        }
    }

    #[must_use]
    fn make_edge(src: u32, tgt: u32) -> Edge {
        Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: "calls".into(),
            confidence: Confidence::Extracted,
        }
    }

    #[must_use]
    fn make_community(id: u32, members: &[u32]) -> Community {
        Community {
            id: CommunityId::new(id),
            label: format!("cluster-{id}"),
            members: members.iter().copied().map(NodeId::new).collect(),
        }
    }

    /// Returns the text between two section headings (exclusive of both headings).
    #[must_use]
    fn extract_section<'a>(report: &'a str, start_heading: &str, end_heading: &str) -> &'a str {
        let start = report
            .find(start_heading)
            .map_or(0, |i| i + start_heading.len());
        let end = report[start..]
            .find(end_heading)
            .map_or(report.len(), |i| start + i);
        &report[start..end]
    }

    /// Returns the text from a section heading to end-of-string.
    #[must_use]
    fn extract_section_to_end<'a>(report: &'a str, start_heading: &str) -> &'a str {
        let start = report
            .find(start_heading)
            .map_or(0, |i| i + start_heading.len());
        &report[start..]
    }

    // ── 1. Title ─────────────────────────────────────────────────────────────

    #[test]
    fn report_starts_with_title() {
        let report = render_report(&Graph::new());
        assert!(
            report.starts_with("# Graph Report\n\n"),
            "expected '# Graph Report\\n\\n' at start, got: {report:?}"
        );
    }

    // ── 2. All four headings (empty graph) ───────────────────────────────────

    #[test]
    fn all_four_headings_present_empty_graph() {
        let report = render_report(&Graph::new());
        for heading in &[
            "# Graph Report",
            "## Top Hubs",
            "## Communities",
            "## Suggested Queries",
        ] {
            assert!(report.contains(heading), "missing heading: {heading}");
        }
    }

    // ── 3. All four headings (non-empty graph) ───────────────────────────────

    #[test]
    fn all_four_headings_present_with_content() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "alpha"));
        g.edges.push(make_edge(1, 1));
        let report = render_report(&g);
        for heading in &[
            "# Graph Report",
            "## Top Hubs",
            "## Communities",
            "## Suggested Queries",
        ] {
            assert!(report.contains(heading), "missing heading: {heading}");
        }
    }

    // ── 4. Counts: empty graph → all zero ────────────────────────────────────

    #[test]
    fn empty_graph_counts_all_zero() {
        let report = render_report(&Graph::new());
        assert!(report.contains("Nodes: 0"), "expected 'Nodes: 0'");
        assert!(report.contains("Edges: 0"), "expected 'Edges: 0'");
        assert!(
            report.contains("Communities: 0"),
            "expected 'Communities: 0'"
        );
    }

    // ── 5. Counts: reflect actual sizes ──────────────────────────────────────

    #[test]
    fn counts_reflect_actual_sizes() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "a"));
        g.nodes.push(make_node(2, "b"));
        g.nodes.push(make_node(3, "c"));
        g.edges.push(make_edge(1, 2));
        g.edges.push(make_edge(2, 3));
        g.communities.push(make_community(0, &[1, 2]));
        let report = render_report(&g);
        assert!(report.contains("Nodes: 3"), "expected 'Nodes: 3'");
        assert!(report.contains("Edges: 2"), "expected 'Edges: 2'");
        assert!(
            report.contains("Communities: 1"),
            "expected 'Communities: 1'"
        );
    }

    // ── 6. Top hub label appears in Top Hubs section ─────────────────────────

    #[test]
    fn hub_label_appears_under_top_hubs() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "alpha"));
        g.nodes.push(make_node(2, "beta"));
        g.edges.push(make_edge(1, 2));
        g.edges.push(make_edge(1, 1)); // self-loop: alpha degree 3, beta degree 1
        let report = render_report(&g);
        let hubs = extract_section(&report, "## Top Hubs", "## Communities");
        assert!(
            hubs.contains("alpha"),
            "expected 'alpha' in Top Hubs section, got: {hubs}"
        );
    }

    // ── 7. Hub entry format: "- label (degree)" ───────────────────────────────

    #[test]
    fn hub_entry_format_is_label_then_degree_in_parens() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "gamma"));
        g.nodes.push(make_node(2, "delta"));
        g.edges.push(make_edge(1, 2));
        g.edges.push(make_edge(1, 2)); // parallel edge: gamma degree 2, delta degree 2; tie → gamma first (id 1 < 2)
        let report = render_report(&g);
        let hubs = extract_section(&report, "## Top Hubs", "## Communities");
        assert!(
            hubs.contains("- gamma (2)"),
            "expected '- gamma (2)' in Top Hubs, got: {hubs}"
        );
    }

    // ── 8. >10 nodes → exactly 10 hub bullets ────────────────────────────────

    #[test]
    fn more_than_ten_nodes_produces_at_most_ten_hub_entries() {
        let mut g = Graph::new();
        for id in 1..=15_u32 {
            g.nodes.push(make_node(id, &format!("node{id}")));
        }
        for id in 1..15_u32 {
            g.edges.push(make_edge(id, id + 1));
        }
        let report = render_report(&g);
        let hubs = extract_section(&report, "## Top Hubs", "## Communities");
        let bullet_count = hubs.lines().filter(|l| l.starts_with("- ")).count();
        assert_eq!(
            bullet_count, 10,
            "expected exactly 10 hub entries, got {bullet_count}\n{hubs}"
        );
    }

    // ── 9. Hub labels resolved, not raw NodeId display strings ───────────────

    #[test]
    fn hub_labels_not_raw_node_ids() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "my_function"));
        g.edges.push(make_edge(1, 1));
        let report = render_report(&g);
        let hubs = extract_section(&report, "## Top Hubs", "## Communities");
        assert!(
            hubs.contains("my_function"),
            "resolved label should appear; got: {hubs}"
        );
        assert!(
            !hubs.contains("- n1"),
            "raw NodeId 'n1' must not appear as the hub entry; got: {hubs}"
        );
    }

    // ── 10. Edge-only node ids (absent from graph.nodes) are skipped ─────────

    #[test]
    fn edge_endpoint_absent_from_nodes_is_skipped() {
        // Node 99 is an edge target but not in graph.nodes → must not appear as a hub bullet.
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "declared"));
        g.edges.push(make_edge(1, 99));
        let report = render_report(&g);
        let hubs = extract_section(&report, "## Top Hubs", "## Communities");
        assert!(
            hubs.contains("declared"),
            "declared node should appear; got: {hubs}"
        );
        let bullet_count = hubs.lines().filter(|l| l.starts_with("- ")).count();
        assert_eq!(
            bullet_count, 1,
            "only 1 hub bullet expected (node 99 has no label); got {bullet_count}\n{hubs}"
        );
    }

    // ── 11. Empty graph → _none_ placeholder in Top Hubs ─────────────────────

    #[test]
    fn empty_graph_top_hubs_shows_none_placeholder() {
        let report = render_report(&Graph::new());
        let hubs = extract_section(&report, "## Top Hubs", "## Communities");
        assert!(
            hubs.contains("_none_"),
            "expected '_none_' placeholder; got: {hubs}"
        );
    }

    // ── 12. Hub with highest degree is listed first ───────────────────────────

    #[test]
    fn hub_ordering_highest_degree_first() {
        let mut g = Graph::new();
        for id in 1..=3_u32 {
            g.nodes.push(make_node(id, &format!("n{id}")));
        }
        // n2: self-loop (+2) + in-edge from n1 (+1) = degree 3; n1 and n3 each get degree 1.
        g.edges.push(make_edge(1, 2));
        g.edges.push(make_edge(2, 2));
        g.edges.push(make_edge(2, 3));
        let report = render_report(&g);
        let hubs = extract_section(&report, "## Top Hubs", "## Communities");
        let first_bullet = hubs.lines().find(|l| l.starts_with("- ")).unwrap_or("");
        assert!(
            first_bullet.contains("n2"),
            "highest-degree hub (n2) must be first; first bullet was: {first_bullet}"
        );
    }

    // ── 13. Community bullet contains id and member count ────────────────────

    #[test]
    fn community_bullet_contains_id_and_member_count() {
        let mut g = Graph::new();
        for id in 1..=3_u32 {
            g.nodes.push(make_node(id, &format!("n{id}")));
        }
        g.communities.push(make_community(5, &[1, 2, 3]));
        let report = render_report(&g);
        let comm = extract_section(&report, "## Communities", "## Suggested Queries");
        assert!(
            comm.contains("community 5: 3 members"),
            "expected 'community 5: 3 members'; got: {comm}"
        );
    }

    // ── 14. Multiple communities all appear ──────────────────────────────────

    #[test]
    fn multiple_communities_all_appear() {
        let mut g = Graph::new();
        for id in 1..=4_u32 {
            g.nodes.push(make_node(id, &format!("n{id}")));
        }
        g.communities.push(make_community(1, &[1, 2]));
        g.communities.push(make_community(2, &[3, 4]));
        let report = render_report(&g);
        let comm = extract_section(&report, "## Communities", "## Suggested Queries");
        assert!(
            comm.contains("community 1:"),
            "community 1 missing; got: {comm}"
        );
        assert!(
            comm.contains("community 2:"),
            "community 2 missing; got: {comm}"
        );
    }

    // ── 15. Empty communities → _none_ placeholder ───────────────────────────

    #[test]
    fn empty_communities_shows_none_placeholder() {
        let report = render_report(&Graph::new());
        let comm = extract_section(&report, "## Communities", "## Suggested Queries");
        assert!(
            comm.contains("_none_"),
            "expected '_none_' in empty communities; got: {comm}"
        );
    }

    // ── 16. Communities sorted by id ascending ────────────────────────────────

    #[test]
    fn community_ordering_is_by_id_ascending() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "n1"));
        // Insert communities in descending order; output must be ascending.
        g.communities.push(make_community(30, &[1]));
        g.communities.push(make_community(10, &[1]));
        g.communities.push(make_community(20, &[1]));
        let report = render_report(&g);
        let comm = extract_section(&report, "## Communities", "## Suggested Queries");
        let pos: Vec<usize> = [10_u32, 20, 30]
            .iter()
            .map(|id| comm.find(&format!("community {id}:")).unwrap_or(usize::MAX))
            .collect();
        assert!(
            pos[0] < pos[1] && pos[1] < pos[2],
            "communities must be sorted by id asc (10 < 20 < 30); got: {comm}"
        );
    }

    // ── 17. Suggested Queries reference the top hub label ────────────────────

    #[test]
    fn suggested_queries_reference_top_hub_label() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "hub_node"));
        g.nodes.push(make_node(2, "leaf"));
        g.edges.push(make_edge(1, 2));
        g.edges.push(make_edge(1, 1)); // hub_node is top hub
        let report = render_report(&g);
        let queries = extract_section_to_end(&report, "## Suggested Queries");
        assert!(
            queries.contains("hub_node"),
            "top hub label must appear in Suggested Queries; got: {queries}"
        );
    }

    // ── 18. Empty graph uses generic (label-free) suggested queries ───────────

    #[test]
    fn empty_graph_uses_generic_suggested_queries() {
        let report = render_report(&Graph::new());
        let queries = extract_section_to_end(&report, "## Suggested Queries");
        // At least one bullet.
        assert!(
            queries.contains("- "),
            "expected at least one query bullet; got: {queries}"
        );
        // Generic queries must not reference any specific node label (no backtick-wrapped name).
        assert!(
            !queries.contains('`'),
            "generic queries should not contain backtick-wrapped labels; got: {queries}"
        );
    }

    // ── 19. Two hubs → second hub label also in Suggested Queries ────────────

    #[test]
    fn two_hubs_both_referenced_in_suggested_queries() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "first_hub"));
        g.nodes.push(make_node(2, "second_hub"));
        g.nodes.push(make_node(3, "leaf"));
        // first_hub: self-loop (+2) → degree 2; second_hub → leaf (+1) → degree 1 (tiebreak: id 2 < 3)
        g.edges.push(make_edge(1, 1));
        g.edges.push(make_edge(2, 3));
        let report = render_report(&g);
        let queries = extract_section_to_end(&report, "## Suggested Queries");
        assert!(
            queries.contains("second_hub"),
            "second hub must appear in Suggested Queries; got: {queries}"
        );
    }

    // ── 20. Determinism — same output on every call ───────────────────────────

    #[test]
    fn render_is_deterministic() {
        let mut g = Graph::new();
        for id in [3_u32, 1, 5, 2, 4] {
            g.nodes.push(make_node(id, &format!("node{id}")));
        }
        g.edges.push(make_edge(3, 1));
        g.edges.push(make_edge(1, 5));
        g.edges.push(make_edge(5, 2));
        g.communities.push(make_community(0, &[1, 3]));
        g.communities.push(make_community(1, &[2, 4, 5]));
        let r1 = render_report(&g);
        let r2 = render_report(&g);
        assert_eq!(r1, r2, "render_report must be deterministic");
    }

    // ── 21. display_safe applied: dangerous chars are escaped ─────────────────

    #[test]
    fn dangerous_label_chars_are_escaped_in_output() {
        let mut g = Graph::new();
        // Label contains a Trojan-Source bidi-override character (U+202E).
        g.nodes.push(make_node(1, "evil\u{202E}label"));
        g.edges.push(make_edge(1, 1)); // ensure it appears as a hub
        let report = render_report(&g);
        assert!(
            !report.contains('\u{202E}'),
            "raw U+202E must not appear in report; got: {report:?}"
        );
        assert!(
            report.contains("\\u{202E}"),
            "expected escaped form '\\u{{202E}}'; got: {report:?}"
        );
    }

    // ── 22. Single-node graph: one hub listed, not truncated ─────────────────

    #[test]
    fn secret_pattern_hub_is_redacted_in_hub_and_suggestions() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "api_key_assignment_refused"));
        g.edges.push(make_edge(1, 1));
        let report = render_report(&g);
        assert!(!report.contains("api_key_assignment_refused"));
        assert!(report.contains("[REDACTED:api_key]"));
    }

    #[test]
    fn single_node_produces_one_hub_entry() {
        let mut g = Graph::new();
        g.nodes.push(make_node(7, "solo"));
        let report = render_report(&g);
        let hubs = extract_section(&report, "## Top Hubs", "## Communities");
        let bullets: Vec<&str> = hubs.lines().filter(|l| l.starts_with("- ")).collect();
        assert_eq!(bullets.len(), 1, "expected exactly 1 hub; got: {hubs}");
        assert!(
            bullets[0].contains("solo"),
            "hub entry should contain 'solo'; got: {}",
            bullets[0]
        );
    }

    // ── 23. Exactly 10 nodes → all 10 hubs listed ────────────────────────────

    #[test]
    fn exactly_ten_nodes_lists_all_ten() {
        let mut g = Graph::new();
        for id in 1..=10_u32 {
            g.nodes.push(make_node(id, &format!("n{id}")));
        }
        let report = render_report(&g);
        let hubs = extract_section(&report, "## Top Hubs", "## Communities");
        let bullet_count = hubs.lines().filter(|l| l.starts_with("- ")).count();
        assert_eq!(
            bullet_count, 10,
            "all 10 nodes should appear as hubs; got {bullet_count}\n{hubs}"
        );
    }

    // ── 24. Community member count = 0 when members list is empty ────────────

    #[test]
    fn community_with_zero_members_listed() {
        let mut g = Graph::new();
        g.communities.push(make_community(0, &[]));
        let report = render_report(&g);
        let comm = extract_section(&report, "## Communities", "## Suggested Queries");
        assert!(
            comm.contains("community 0: 0 members"),
            "expected 'community 0: 0 members'; got: {comm}"
        );
    }

    // ── 25. Self-loop hub dominates the top-hubs list ────────────────────────

    #[test]
    fn self_loop_hub_appears_first_with_correct_degree() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "loop_node"));
        g.nodes.push(make_node(2, "plain_node"));
        g.edges.push(make_edge(1, 1)); // self-loop: degree 2
        g.edges.push(make_edge(2, 1)); // plain_node: degree 1; loop_node: degree 3 total
        let report = render_report(&g);
        let hubs = extract_section(&report, "## Top Hubs", "## Communities");
        let first_bullet = hubs.lines().find(|l| l.starts_with("- ")).unwrap_or("");
        assert!(
            first_bullet.contains("loop_node"),
            "self-loop hub must be first; got: {first_bullet}"
        );
        assert!(
            first_bullet.contains("(3)"),
            "self-loop hub degree must be 3; got: {first_bullet}"
        );
    }
}
