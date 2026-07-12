//! Concept explanation (PE) — a local-first structural summary of a concept's neighbourhood.
//!
//! [`explain`] answers "what is `<concept>`?" by summarising the nodes whose label matches it and
//! their immediate graph neighbourhood (outbound/inbound relations, community membership).
//! It is **local-first** (R3): a pure, deterministic structural summary with **no LLM or network
//! call**. Deterministic ordering throughout satisfies R4.

use std::fmt::Write as _;

use habitat_graph_core::{display_safe, Community, Edge, Graph, Node, NodeId};

use crate::query::find_by_label;

/// Maximum number of matched nodes summarised in one explanation.
const MAX_CONCEPTS: usize = 10;

/// Maximum outbound or inbound edges listed per matched node (cap with explicit note).
const MAX_EDGES_PER_NODE: usize = 8;

/// Returns a deterministic, local-first structural summary of `concept` and its graph neighbourhood.
///
/// Nodes whose label contains `concept` (case-insensitive substring) are found via
/// [`find_by_label`] in ascending [`NodeId`] order. Up to `MAX_CONCEPTS` are rendered;
/// when more match, an explicit truncation note is appended (never a silent cap).
///
/// For each matched node the summary includes:
/// - outbound edges: sorted by `(target_id, relation)`, capped at `MAX_EDGES_PER_NODE` with a
///   note when truncated, showing the relation kind and the target node's label;
/// - inbound edges: sorted by `(source_id, relation)`, same cap, showing the relation kind and
///   the source node's label;
/// - community membership: the label(s) of any
///   [`Community`] the node belongs to, sorted by community id.
///
/// Every label and relation string embedded in the output is passed through [`display_safe`] to
/// neutralise Trojan-Source / bidi-override injection from attacker-controlled source.
///
/// When no node matches, returns a human-readable "no match" line rather than an error — an
/// explanation of nothing is a valid, informative answer.
///
/// # Guarantees
/// - **No LLM call / no network I/O** (local-first, R3).
/// - **Deterministic** across repeated calls with the same inputs (R4).
#[must_use]
pub fn explain(graph: &Graph, concept: &str) -> String {
    let matches = find_by_label(graph, concept);

    if matches.is_empty() {
        return format!("No nodes match \"{}\".\n", display_safe(concept));
    }

    let total = matches.len();
    let mut out = String::new();

    if total > MAX_CONCEPTS {
        let _ = writeln!(
            out,
            "{total} node(s) match \"{}\" (showing first {MAX_CONCEPTS}):",
            display_safe(concept)
        );
    } else {
        let _ = writeln!(out, "{total} node(s) match \"{}\":", display_safe(concept));
    }

    for node in matches.iter().take(MAX_CONCEPTS) {
        summarise_node(graph, node, &mut out);
    }

    if total > MAX_CONCEPTS {
        let _ = writeln!(
            out,
            "\n\u{2026} {} more node(s) not shown.",
            total - MAX_CONCEPTS
        );
    }

    out
}

/// Renders the neighbourhood section for a single `node` into `out`.
///
/// Writes a header, then the outbound-edge list, inbound-edge list, and community membership.
/// All user-controlled strings are passed through [`display_safe`] before being written.
fn summarise_node(graph: &Graph, node: &Node, out: &mut String) {
    let _ = writeln!(
        out,
        "\nNode: `{}` (id {})",
        display_safe(&node.label),
        node.id.get()
    );

    render_outbound(graph, node, out);
    render_inbound(graph, node, out);
    render_community(graph, node, out);
}

/// Writes the outbound-edge subsection for `node` into `out`.
fn render_outbound(graph: &Graph, node: &Node, out: &mut String) {
    let mut outbound: Vec<&Edge> = graph.edges.iter().filter(|e| e.source == node.id).collect();
    // Deterministic: primary key = target id (ascending), secondary = relation string.
    outbound.sort_by(|a, b| (a.target, a.relation.as_str()).cmp(&(b.target, b.relation.as_str())));

    if outbound.is_empty() {
        let _ = writeln!(out, "  Outbound: (none)");
        return;
    }

    let outbound_count = outbound.len();
    let _ = writeln!(out, "  Outbound ({outbound_count} relation(s)):");
    for e in outbound.iter().take(MAX_EDGES_PER_NODE) {
        let tgt = label_of(graph, e.target);
        let _ = writeln!(out, "    --[{}]--> {}", display_safe(&e.relation), tgt);
    }
    if outbound_count > MAX_EDGES_PER_NODE {
        let _ = writeln!(
            out,
            "    \u{2026} {} more outbound (showing first {MAX_EDGES_PER_NODE})",
            outbound_count - MAX_EDGES_PER_NODE
        );
    }
}

/// Writes the inbound-edge subsection for `node` into `out`.
fn render_inbound(graph: &Graph, node: &Node, out: &mut String) {
    let mut inbound: Vec<&Edge> = graph.edges.iter().filter(|e| e.target == node.id).collect();
    // Deterministic: primary key = source id (ascending), secondary = relation string.
    inbound.sort_by(|a, b| (a.source, a.relation.as_str()).cmp(&(b.source, b.relation.as_str())));

    if inbound.is_empty() {
        let _ = writeln!(out, "  Inbound: (none)");
        return;
    }

    let inbound_count = inbound.len();
    let _ = writeln!(out, "  Inbound ({inbound_count} relation(s)):");
    for e in inbound.iter().take(MAX_EDGES_PER_NODE) {
        let src = label_of(graph, e.source);
        let _ = writeln!(out, "    <--[{}]-- {}", display_safe(&e.relation), src);
    }
    if inbound_count > MAX_EDGES_PER_NODE {
        let _ = writeln!(
            out,
            "    \u{2026} {} more inbound (showing first {MAX_EDGES_PER_NODE})",
            inbound_count - MAX_EDGES_PER_NODE
        );
    }
}

/// Writes the community-membership subsection for `node` into `out`.
fn render_community(graph: &Graph, node: &Node, out: &mut String) {
    let mut communities: Vec<&Community> = graph
        .communities
        .iter()
        .filter(|c| c.members.contains(&node.id))
        .collect();
    // Deterministic: sort by community id (ascending).
    communities.sort_by_key(|c| c.id);

    if communities.is_empty() {
        let _ = writeln!(out, "  Community: (none)");
        return;
    }

    let labels: Vec<String> = communities.iter().map(|c| display_safe(&c.label)).collect();
    let _ = writeln!(out, "  Community: {}", labels.join(", "));
}

/// Resolves a [`NodeId`] to its display-safe label, falling back to `<id N>` for unknown nodes.
fn label_of(graph: &Graph, id: NodeId) -> String {
    graph
        .nodes
        .iter()
        .find(|n| n.id == id)
        .map_or_else(|| format!("<id {}>", id.get()), |n| display_safe(&n.label))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use habitat_graph_core::{
        Community, CommunityId, Confidence, Edge, Graph, Manifest, Node, NodeId, Span,
        SCHEMA_VERSION,
    };

    use super::{explain, MAX_CONCEPTS, MAX_EDGES_PER_NODE};

    // ── helpers ──────────────────────────────────────────────────────────────

    fn node(id: u32, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: "a.rs".to_owned(),
            source_location: Span::new(0, 1, 1, 1),
        }
    }

    fn edge(src: u32, tgt: u32, relation: &str) -> Edge {
        Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: relation.to_owned(),
            confidence: Confidence::Extracted,
        }
    }

    fn community(id: u32, label: &str, members: &[u32]) -> Community {
        Community {
            id: CommunityId::new(id),
            label: label.to_owned(),
            members: members.iter().copied().map(NodeId::new).collect(),
        }
    }

    fn build_graph(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        Graph {
            schema: SCHEMA_VERSION.to_owned(),
            nodes,
            edges,
            communities: Vec::new(),
            manifest: Manifest::default(),
        }
    }

    fn build_graph_with_communities(
        nodes: Vec<Node>,
        edges: Vec<Edge>,
        communities: Vec<Community>,
    ) -> Graph {
        Graph {
            schema: SCHEMA_VERSION.to_owned(),
            nodes,
            edges,
            communities,
            manifest: Manifest::default(),
        }
    }

    // ═══ Group 1: No-match cases ═════════════════════════════════════════════

    #[test]
    fn no_match_empty_graph() {
        let s = explain(&Graph::new(), "anything");
        assert!(
            s.contains("No nodes match"),
            "empty graph must report no-match: {s}"
        );
    }

    #[test]
    fn no_match_concept_not_in_any_label() {
        let g = build_graph(vec![node(1, "alpha"), node(2, "beta")], vec![]);
        let s = explain(&g, "zzz");
        assert!(
            s.contains("No nodes match"),
            "absent concept must report no-match: {s}"
        );
        assert!(
            !s.contains("alpha"),
            "non-matching node must not appear: {s}"
        );
    }

    #[test]
    fn no_match_concept_display_safe_escaped() {
        // Bidi override in the concept itself must be escaped in the no-match message.
        let g = Graph::new();
        let s = explain(&g, "evil\u{202E}label");
        assert!(!s.contains('\u{202E}'), "bidi must be escaped: {s}");
        assert!(s.contains("\\u{202E}"), "escaped form must appear: {s}");
    }

    // ═══ Group 2: Single-match basics ════════════════════════════════════════

    #[test]
    fn single_match_count_header() {
        let g = build_graph(vec![node(1, "alpha")], vec![]);
        let s = explain(&g, "alpha");
        assert!(s.contains("1 node(s) match"), "header count wrong: {s}");
    }

    #[test]
    fn single_match_label_in_output() {
        let g = build_graph(vec![node(1, "alpha")], vec![]);
        let s = explain(&g, "alpha");
        assert!(s.contains("alpha"), "matched label must appear: {s}");
    }

    #[test]
    fn single_match_node_id_in_output() {
        let g = build_graph(vec![node(42, "hub")], vec![]);
        let s = explain(&g, "hub");
        assert!(s.contains("42"), "node id must appear: {s}");
    }

    #[test]
    fn isolated_node_outbound_none() {
        let g = build_graph(vec![node(1, "solo")], vec![]);
        let s = explain(&g, "solo");
        assert!(
            s.contains("Outbound: (none)"),
            "isolated node must show no outbound: {s}"
        );
    }

    #[test]
    fn isolated_node_inbound_none() {
        let g = build_graph(vec![node(1, "solo")], vec![]);
        let s = explain(&g, "solo");
        assert!(
            s.contains("Inbound: (none)"),
            "isolated node must show no inbound: {s}"
        );
    }

    #[test]
    fn isolated_node_community_none() {
        let g = build_graph(vec![node(1, "solo")], vec![]);
        let s = explain(&g, "solo");
        assert!(
            s.contains("Community: (none)"),
            "isolated node must show no community: {s}"
        );
    }

    #[test]
    fn single_match_with_community() {
        let g = build_graph_with_communities(
            vec![node(1, "hub")],
            vec![],
            vec![community(0, "cluster-A", &[1])],
        );
        let s = explain(&g, "hub");
        assert!(s.contains("cluster-A"), "community label must appear: {s}");
        assert!(
            !s.contains("Community: (none)"),
            "community section must not show none: {s}"
        );
    }

    #[test]
    fn single_match_output_has_section_headers() {
        let g = build_graph(vec![node(1, "alpha")], vec![]);
        let s = explain(&g, "alpha");
        assert!(
            s.contains("Outbound"),
            "Outbound header must be present: {s}"
        );
        assert!(s.contains("Inbound"), "Inbound header must be present: {s}");
        assert!(
            s.contains("Community"),
            "Community header must be present: {s}"
        );
    }

    // ═══ Group 3: Multiple matches ════════════════════════════════════════════

    #[test]
    fn multi_match_count_header() {
        let g = build_graph(
            vec![node(1, "foo_a"), node(2, "foo_b"), node(3, "foo_c")],
            vec![],
        );
        let s = explain(&g, "foo");
        assert!(s.contains("3 node(s) match"), "count must be 3: {s}");
    }

    #[test]
    fn two_matches_both_labels_present() {
        let g = build_graph(vec![node(1, "alpha"), node(2, "alphabeta")], vec![]);
        let s = explain(&g, "alpha");
        assert!(s.contains("alpha"), "alpha must appear: {s}");
        assert!(s.contains("alphabeta"), "alphabeta must appear: {s}");
    }

    #[test]
    fn multi_match_ascending_id_order_first_before_second() {
        // Node id 1 (label "aa_1") must appear before node id 2 (label "aa_2").
        let g = build_graph(vec![node(2, "aa_2"), node(1, "aa_1")], vec![]);
        let s = explain(&g, "aa");
        let pos1 = s.find("aa_1").expect("aa_1 must appear");
        let pos2 = s.find("aa_2").expect("aa_2 must appear");
        assert!(pos1 < pos2, "id 1 must precede id 2 in output: {s}");
    }

    // ═══ Group 4: Outbound edges ══════════════════════════════════════════════

    #[test]
    fn outbound_single_edge_relation_shown() {
        let g = build_graph(
            vec![node(1, "src"), node(2, "tgt")],
            vec![edge(1, 2, "calls")],
        );
        let s = explain(&g, "src");
        assert!(s.contains("calls"), "relation 'calls' must appear: {s}");
    }

    #[test]
    fn outbound_single_edge_neighbour_label() {
        let g = build_graph(
            vec![node(1, "source_node"), node(2, "target_node")],
            vec![edge(1, 2, "calls")],
        );
        let s = explain(&g, "source_node");
        assert!(
            s.contains("target_node"),
            "neighbour label must appear: {s}"
        );
    }

    #[test]
    fn outbound_count_correct() {
        let g = build_graph(
            vec![node(1, "hub"), node(2, "a"), node(3, "b")],
            vec![edge(1, 2, "calls"), edge(1, 3, "imports")],
        );
        let s = explain(&g, "hub");
        assert!(
            s.contains("Outbound (2 relation(s))"),
            "outbound count must be 2: {s}"
        );
    }

    #[test]
    fn outbound_multiple_edges_sorted_by_target_id() {
        // Edges from hub to node 3 and node 1; node 1 (smaller id) must appear first.
        let g = build_graph(
            vec![node(1, "hub"), node(2, "first"), node(3, "second")],
            vec![edge(1, 3, "calls"), edge(1, 2, "calls")],
        );
        let s = explain(&g, "hub");
        let pos_first = s.find("first").expect("first must appear");
        let pos_second = s.find("second").expect("second must appear");
        assert!(
            pos_first < pos_second,
            "lower target-id (first) must be listed first: {s}"
        );
    }

    #[test]
    fn outbound_two_edges_same_target_sorted_by_relation_alphabetically() {
        // Same target, two relation kinds: "calls" < "imports" alphabetically.
        let g = build_graph(
            vec![node(1, "hub"), node(2, "peer")],
            vec![edge(1, 2, "imports"), edge(1, 2, "calls")],
        );
        let s = explain(&g, "hub");
        let pos_calls = s.find("calls").expect("calls must appear");
        let pos_imports = s.find("imports").expect("imports must appear");
        assert!(
            pos_calls < pos_imports,
            "'calls' must precede 'imports': {s}"
        );
    }

    #[test]
    fn outbound_arrow_notation_present() {
        let g = build_graph(
            vec![node(1, "src"), node(2, "tgt")],
            vec![edge(1, 2, "calls")],
        );
        let s = explain(&g, "src");
        assert!(
            s.contains("--[calls]-->"),
            "arrow notation must be present: {s}"
        );
    }

    // ═══ Group 5: Inbound edges ═══════════════════════════════════════════════

    #[test]
    fn inbound_single_edge_relation_shown() {
        let g = build_graph(
            vec![node(1, "caller"), node(2, "callee")],
            vec![edge(1, 2, "calls")],
        );
        let s = explain(&g, "callee");
        assert!(
            s.contains("calls"),
            "inbound relation 'calls' must appear: {s}"
        );
    }

    #[test]
    fn inbound_single_edge_source_label() {
        let g = build_graph(
            vec![node(1, "caller_node"), node(2, "callee_node")],
            vec![edge(1, 2, "calls")],
        );
        let s = explain(&g, "callee_node");
        assert!(
            s.contains("caller_node"),
            "source label must appear in inbound: {s}"
        );
    }

    #[test]
    fn inbound_count_correct() {
        let g = build_graph(
            vec![node(1, "sink"), node(2, "a"), node(3, "b")],
            vec![edge(2, 1, "calls"), edge(3, 1, "calls")],
        );
        let s = explain(&g, "sink");
        assert!(
            s.contains("Inbound (2 relation(s))"),
            "inbound count must be 2: {s}"
        );
    }

    #[test]
    fn inbound_multiple_edges_sorted_by_source_id() {
        // Two inbound edges from node 3 and node 1; node 1 (smaller id) must appear first.
        let g = build_graph(
            vec![node(1, "early"), node(2, "sink"), node(3, "late")],
            vec![edge(3, 2, "calls"), edge(1, 2, "calls")],
        );
        let s = explain(&g, "sink");
        let pos_early = s.find("early").expect("early must appear");
        let pos_late = s.find("late").expect("late must appear");
        assert!(
            pos_early < pos_late,
            "lower source-id (early) must be listed first: {s}"
        );
    }

    #[test]
    fn inbound_and_outbound_both_shown_for_middle_node() {
        // middle receives from source and sends to target.
        let g = build_graph(
            vec![node(1, "source"), node(2, "middle"), node(3, "target_node")],
            vec![edge(1, 2, "calls"), edge(2, 3, "imports")],
        );
        let s = explain(&g, "middle");
        assert!(
            s.contains("Outbound (1 relation(s))"),
            "outbound section: {s}"
        );
        assert!(
            s.contains("Inbound (1 relation(s))"),
            "inbound section: {s}"
        );
    }

    #[test]
    fn inbound_arrow_notation_present() {
        let g = build_graph(
            vec![node(1, "caller"), node(2, "callee")],
            vec![edge(1, 2, "calls")],
        );
        let s = explain(&g, "callee");
        assert!(
            s.contains("<--[calls]--"),
            "inbound arrow notation must appear: {s}"
        );
    }

    // ═══ Group 6: Community membership ═══════════════════════════════════════

    #[test]
    fn single_community_label_shown() {
        let g = build_graph_with_communities(
            vec![node(1, "member")],
            vec![],
            vec![community(0, "cluster-X", &[1])],
        );
        let s = explain(&g, "member");
        assert!(s.contains("cluster-X"), "community label must appear: {s}");
    }

    #[test]
    fn no_community_shows_none() {
        let g = build_graph_with_communities(
            vec![node(1, "orphan")],
            vec![],
            vec![community(0, "cluster-X", &[99])], // node 1 not a member
        );
        let s = explain(&g, "orphan");
        assert!(
            s.contains("Community: (none)"),
            "non-member must show none: {s}"
        );
    }

    #[test]
    fn multiple_communities_all_shown() {
        let g = build_graph_with_communities(
            vec![node(1, "shared")],
            vec![],
            vec![
                community(0, "cluster-A", &[1]),
                community(1, "cluster-B", &[1]),
            ],
        );
        let s = explain(&g, "shared");
        assert!(s.contains("cluster-A"), "cluster-A must appear: {s}");
        assert!(s.contains("cluster-B"), "cluster-B must appear: {s}");
    }

    #[test]
    fn communities_sorted_by_id_lower_first() {
        // Community ids: 5 then 2; id 2 must appear first in output.
        let g = build_graph_with_communities(
            vec![node(1, "multi")],
            vec![],
            vec![
                community(5, "cluster-late", &[1]),
                community(2, "cluster-early", &[1]),
            ],
        );
        let s = explain(&g, "multi");
        let pos_early = s.find("cluster-early").expect("cluster-early must appear");
        let pos_late = s.find("cluster-late").expect("cluster-late must appear");
        assert!(
            pos_early < pos_late,
            "lower community-id must be listed first: {s}"
        );
    }

    // ═══ Group 7: Caps (concept cap + edge cap) ════════════════════════════════

    #[test]
    fn max_concepts_exact_no_truncation_note() {
        // Exactly MAX_CONCEPTS matches → no truncation note.
        let nodes: Vec<Node> = (1..=MAX_CONCEPTS)
            .map(|i| node(u32::try_from(i).expect("i fits u32"), &format!("item_{i}")))
            .collect();
        let g = build_graph(nodes, vec![]);
        let s = explain(&g, "item");
        assert!(
            !s.contains("more node(s) not shown"),
            "no truncation note for exact cap: {s}"
        );
    }

    #[test]
    fn max_concepts_plus_one_shows_truncation_note() {
        // MAX_CONCEPTS + 1 matches → truncation note present.
        let nodes: Vec<Node> = (1..=u32::try_from(MAX_CONCEPTS + 1).expect("fits"))
            .map(|i| node(i, &format!("item_{i}")))
            .collect();
        let g = build_graph(nodes, vec![]);
        let s = explain(&g, "item");
        assert!(
            s.contains("more node(s) not shown"),
            "truncation note must appear: {s}"
        );
    }

    #[test]
    fn truncation_note_count_is_correct() {
        // 13 nodes with "item" label → 13 - MAX_CONCEPTS = 3 hidden.
        let extra = 3_usize;
        let total = MAX_CONCEPTS + extra;
        let nodes: Vec<Node> = (1..=u32::try_from(total).expect("fits"))
            .map(|i| node(i, &format!("item_{i}")))
            .collect();
        let g = build_graph(nodes, vec![]);
        let s = explain(&g, "item");
        let note = format!("{extra} more node(s) not shown");
        assert!(s.contains(&note), "truncation note count wrong: {s}");
    }

    #[test]
    fn outbound_edge_cap_truncation_note_present() {
        // MAX_EDGES_PER_NODE + 1 outbound edges → truncation note.
        let mut nodes = vec![node(1, "hub")];
        let mut edges = Vec::new();
        for i in 2..=u32::try_from(MAX_EDGES_PER_NODE + 2).expect("fits") {
            nodes.push(node(i, &format!("peer_{i}")));
            edges.push(edge(1, i, "calls"));
        }
        let g = build_graph(nodes, edges);
        let s = explain(&g, "hub");
        assert!(
            s.contains("more outbound"),
            "outbound cap note must appear: {s}"
        );
    }

    #[test]
    fn inbound_edge_cap_truncation_note_present() {
        // MAX_EDGES_PER_NODE + 1 inbound edges → truncation note.
        let mut nodes = vec![node(1, "sink")];
        let mut edges = Vec::new();
        for i in 2..=u32::try_from(MAX_EDGES_PER_NODE + 2).expect("fits") {
            nodes.push(node(i, &format!("peer_{i}")));
            edges.push(edge(i, 1, "calls"));
        }
        let g = build_graph(nodes, edges);
        let s = explain(&g, "sink");
        assert!(
            s.contains("more inbound"),
            "inbound cap note must appear: {s}"
        );
    }

    #[test]
    fn outbound_cap_note_count_correct() {
        // 10 outbound edges; cap is MAX_EDGES_PER_NODE = 8 → 2 hidden.
        let hidden = 2_usize;
        let total_edges = MAX_EDGES_PER_NODE + hidden;
        let mut nodes = vec![node(1, "hub")];
        let mut edges = Vec::new();
        for i in 2..=u32::try_from(total_edges + 1).expect("fits") {
            nodes.push(node(i, &format!("peer_{i}")));
            edges.push(edge(1, i, "calls"));
        }
        let g = build_graph(nodes, edges);
        let s = explain(&g, "hub");
        let note = format!("{hidden} more outbound");
        assert!(s.contains(&note), "outbound cap count wrong: {s}");
    }

    #[test]
    fn inbound_cap_note_count_correct() {
        let hidden = 2_usize;
        let total_edges = MAX_EDGES_PER_NODE + hidden;
        let mut nodes = vec![node(1, "sink")];
        let mut edges = Vec::new();
        for i in 2..=u32::try_from(total_edges + 1).expect("fits") {
            nodes.push(node(i, &format!("peer_{i}")));
            edges.push(edge(i, 1, "calls"));
        }
        let g = build_graph(nodes, edges);
        let s = explain(&g, "sink");
        let note = format!("{hidden} more inbound");
        assert!(s.contains(&note), "inbound cap count wrong: {s}");
    }

    // ═══ Group 8: Security / display_safe ════════════════════════════════════

    #[test]
    fn label_bidi_override_neutralised() {
        let g = build_graph(vec![node(1, "evil\u{202E}node")], vec![]);
        let s = explain(&g, "evil");
        assert!(
            !s.contains('\u{202E}'),
            "bidi in label must be escaped: {s}"
        );
        assert!(s.contains("\\u{202E}"), "escaped form must appear: {s}");
    }

    #[test]
    fn concept_bidi_override_neutralised_in_header() {
        let g = build_graph(vec![node(1, "concept\u{202E}safe")], vec![]);
        // Query with bidi in concept: bidi must be escaped in the header.
        let s = explain(&g, "concept\u{202E}safe");
        assert!(
            !s.contains('\u{202E}'),
            "bidi in concept must be escaped: {s}"
        );
    }

    #[test]
    fn relation_bidi_override_neutralised() {
        let g = build_graph(
            vec![node(1, "src"), node(2, "tgt")],
            vec![Edge {
                source: NodeId::new(1),
                target: NodeId::new(2),
                relation: "evil\u{202E}rel".to_owned(),
                confidence: Confidence::Extracted,
            }],
        );
        let s = explain(&g, "src");
        assert!(
            !s.contains('\u{202E}'),
            "bidi in relation must be escaped: {s}"
        );
        assert!(s.contains("\\u{202E}"), "escaped form must appear: {s}");
    }

    #[test]
    fn neighbour_label_bidi_neutralised() {
        let g = build_graph(
            vec![node(1, "src"), node(2, "evil\u{202E}neighbour")],
            vec![edge(1, 2, "calls")],
        );
        let s = explain(&g, "src");
        assert!(
            !s.contains('\u{202E}'),
            "bidi in neighbour label must be escaped: {s}"
        );
    }

    #[test]
    fn source_label_bidi_neutralised() {
        let g = build_graph(
            vec![node(1, "evil\u{202E}caller"), node(2, "callee")],
            vec![edge(1, 2, "calls")],
        );
        let s = explain(&g, "callee");
        assert!(
            !s.contains('\u{202E}'),
            "bidi in source label must be escaped: {s}"
        );
    }

    #[test]
    fn community_label_bidi_neutralised() {
        let g = build_graph_with_communities(
            vec![node(1, "member")],
            vec![],
            vec![community(0, "evil\u{202E}cluster", &[1])],
        );
        let s = explain(&g, "member");
        assert!(
            !s.contains('\u{202E}'),
            "bidi in community label must be escaped: {s}"
        );
        assert!(s.contains("\\u{202E}"), "escaped form must appear: {s}");
    }

    #[test]
    fn control_char_in_label_neutralised() {
        // ANSI ESC (U+001B) in a label must not reach the output raw.
        let g = build_graph(vec![node(1, "bad\u{001B}[31mlabel")], vec![]);
        let s = explain(&g, "bad");
        assert!(!s.contains('\u{001B}'), "ANSI ESC must be escaped: {s}");
        assert!(s.contains("\\u{001B}"), "escaped form must appear: {s}");
    }

    // ═══ Group 9: Determinism ═════════════════════════════════════════════════

    #[test]
    fn determinism_same_output_on_repeated_calls() {
        let g = build_graph(
            vec![node(1, "alpha"), node(2, "beta"), node(3, "gamma")],
            vec![edge(1, 2, "calls"), edge(3, 1, "imports")],
        );
        let first = explain(&g, "a");
        let second = explain(&g, "a");
        assert_eq!(first, second, "output must be identical across calls");
    }

    #[test]
    fn determinism_outbound_order_is_target_id_then_relation() {
        // Two outbound edges: (1→3, "imports") and (1→2, "calls").
        // Target id 2 < 3, so calls-to-node-2 must come first regardless of insertion order.
        let g = build_graph(
            vec![node(1, "hub"), node(2, "low_id"), node(3, "high_id")],
            vec![edge(1, 3, "imports"), edge(1, 2, "calls")],
        );
        let s = explain(&g, "hub");
        let pos_low = s.find("low_id").expect("low_id must appear");
        let pos_high = s.find("high_id").expect("high_id must appear");
        assert!(
            pos_low < pos_high,
            "lower target id must be listed first: {s}"
        );
    }

    #[test]
    fn determinism_inbound_order_is_source_id_then_relation() {
        // Two inbound edges: (3→1, "imports") and (2→1, "calls").
        // Source id 2 < 3, so calls-from-node-2 must come first.
        let g = build_graph(
            vec![node(1, "sink"), node(2, "low_src"), node(3, "high_src")],
            vec![edge(3, 1, "imports"), edge(2, 1, "calls")],
        );
        let s = explain(&g, "sink");
        let pos_low = s.find("low_src").expect("low_src must appear");
        let pos_high = s.find("high_src").expect("high_src must appear");
        assert!(
            pos_low < pos_high,
            "lower source id must be listed first: {s}"
        );
    }

    // ═══ Group 10: Edge cases ════════════════════════════════════════════════

    #[test]
    fn self_loop_appears_in_outbound_and_inbound() {
        // A self-loop (source == target) must appear in both outbound and inbound sections.
        let g = build_graph(vec![node(1, "recursive")], vec![edge(1, 1, "self_calls")]);
        let s = explain(&g, "recursive");
        // self_calls must appear at least twice (once outbound, once inbound).
        let count = s.matches("self_calls").count();
        assert!(count >= 2, "self-loop must appear in both sections: {s}");
    }

    #[test]
    fn relation_types_preserved_calls_vs_imports() {
        let g = build_graph(
            vec![node(1, "source"), node(2, "target_a"), node(3, "target_b")],
            vec![edge(1, 2, "calls"), edge(1, 3, "imports")],
        );
        let s = explain(&g, "source");
        assert!(
            s.contains("--[calls]-->"),
            "calls relation must appear: {s}"
        );
        assert!(
            s.contains("--[imports]-->"),
            "imports relation must appear: {s}"
        );
    }

    #[test]
    fn empty_concept_matches_all_nodes_in_small_graph() {
        // Empty concept = match all; graph has 3 nodes, all are shown.
        let g = build_graph(
            vec![node(1, "alpha"), node(2, "beta"), node(3, "gamma")],
            vec![],
        );
        let s = explain(&g, "");
        assert!(s.contains("3 node(s) match"), "all 3 nodes must match: {s}");
        assert!(s.contains("alpha"), "alpha must appear: {s}");
        assert!(s.contains("beta"), "beta must appear: {s}");
        assert!(s.contains("gamma"), "gamma must appear: {s}");
    }

    #[test]
    fn no_match_message_ends_with_newline() {
        let s = explain(&Graph::new(), "nope");
        assert!(s.ends_with('\n'), "no-match must end with newline: {s:?}");
    }

    #[test]
    fn full_output_ends_with_newline() {
        let g = build_graph(vec![node(1, "alpha")], vec![]);
        let s = explain(&g, "alpha");
        assert!(
            s.ends_with('\n'),
            "full output must end with newline: {s:?}"
        );
    }

    #[test]
    fn node_with_only_outbound_shows_inbound_none() {
        let g = build_graph(
            vec![node(1, "source"), node(2, "target")],
            vec![edge(1, 2, "calls")],
        );
        let s = explain(&g, "source");
        assert!(
            s.contains("Inbound: (none)"),
            "source node must show inbound none: {s}"
        );
    }

    #[test]
    fn node_with_only_inbound_shows_outbound_none() {
        let g = build_graph(
            vec![node(1, "source"), node(2, "target")],
            vec![edge(1, 2, "calls")],
        );
        let s = explain(&g, "target");
        assert!(
            s.contains("Outbound: (none)"),
            "target node must show outbound none: {s}"
        );
    }

    #[test]
    fn concept_match_is_case_insensitive() {
        // Concept "ALPHA" should match label "alpha_module".
        let g = build_graph(vec![node(1, "alpha_module")], vec![]);
        let s = explain(&g, "ALPHA");
        assert!(
            s.contains("alpha_module"),
            "case-insensitive match must work: {s}"
        );
    }

    #[test]
    fn concept_partial_substring_match() {
        // Concept "mod" matches label "my_module_name".
        let g = build_graph(vec![node(1, "my_module_name"), node(2, "other")], vec![]);
        let s = explain(&g, "mod");
        assert!(s.contains("my_module_name"), "partial match must work: {s}");
        assert!(
            !s.contains("other"),
            "non-matching node must not appear: {s}"
        );
    }

    // Extra tests to push well past 50 ────────────────────────────────────────

    #[test]
    fn outbound_shows_missing_target_as_fallback_id() {
        // Edge to a node id not in graph.nodes → fallback "<id N>".
        let g = Graph {
            schema: SCHEMA_VERSION.to_owned(),
            nodes: vec![node(1, "hub")],
            edges: vec![edge(1, 99, "calls")], // node 99 absent
            communities: Vec::new(),
            manifest: Manifest::default(),
        };
        let s = explain(&g, "hub");
        assert!(
            s.contains("<id 99>"),
            "fallback for missing node must appear: {s}"
        );
    }

    #[test]
    fn inbound_shows_missing_source_as_fallback_id() {
        let g = Graph {
            schema: SCHEMA_VERSION.to_owned(),
            nodes: vec![node(1, "sink")],
            edges: vec![edge(88, 1, "calls")], // node 88 absent
            communities: Vec::new(),
            manifest: Manifest::default(),
        };
        let s = explain(&g, "sink");
        assert!(
            s.contains("<id 88>"),
            "fallback for missing source must appear: {s}"
        );
    }

    #[test]
    fn truncation_header_includes_max_concepts_count() {
        // When truncated, header must state how many are shown.
        let nodes: Vec<Node> = (1..=u32::try_from(MAX_CONCEPTS + 1).expect("fits"))
            .map(|i| node(i, &format!("item_{i}")))
            .collect();
        let g = build_graph(nodes, vec![]);
        let s = explain(&g, "item");
        let expected = format!("showing first {MAX_CONCEPTS}");
        assert!(
            s.contains(&expected),
            "header must state showing-first count: {s}"
        );
    }

    #[test]
    fn multiple_relation_types_on_inbound_all_shown() {
        // Two different callers with different relations to the same node.
        let g = build_graph(
            vec![
                node(1, "alpha_caller"),
                node(2, "beta_caller"),
                node(3, "sink_node"),
            ],
            vec![edge(1, 3, "calls"), edge(2, 3, "imports")],
        );
        let s = explain(&g, "sink_node");
        assert!(s.contains("alpha_caller"), "first caller must appear: {s}");
        assert!(s.contains("beta_caller"), "second caller must appear: {s}");
    }

    #[test]
    fn community_not_shown_for_different_member_only() {
        // Community exists but its only member is a different node.
        let g = build_graph_with_communities(
            vec![node(1, "loner"), node(2, "joiner")],
            vec![],
            vec![community(0, "clique", &[2])],
        );
        let s = explain(&g, "loner");
        assert!(
            s.contains("Community: (none)"),
            "non-member must not see community: {s}"
        );
    }

    #[test]
    fn edge_self_loop_outbound_neighbour_is_self() {
        // Self-loop: outbound target label == node's own label.
        let g = build_graph(vec![node(1, "self_ref")], vec![edge(1, 1, "loops")]);
        let s = explain(&g, "self_ref");
        // "self_ref" must appear more than once: in the header + in both outbound/inbound.
        let count = s.matches("self_ref").count();
        assert!(count >= 3, "self-ref label must appear multiple times: {s}");
    }
}
