//! Wiki exporter — a per-node Markdown article set plus an `index.md`.
//!
//! This exporter produces a plain-Markdown wiki suitable for any renderer that understands
//! standard `[text](url)` links (unlike [`crate::obsidian`] which uses `[[wikilinks]]`).
//!
//! # Output shape
//!
//! - One article per node at stable filename `node-{id}.md`.  The [`NodeId`] integer is unique
//!   and stable, so all cross-article links always resolve without disambiguation.
//! - `index.md` — lists every article with a plain Markdown link.
//!
//! # Security (STRIDE-T)
//!
//! Every attacker-influenced string (node label, source path, relation) is routed through
//! [`display_safe`](habitat_graph_core::display_safe) before embedding in the output.
//! Labels used inside `[text](url)` link text are additionally bracket-escaped (`]` → `\]`,
//! `[` → `\[`) to prevent link-injection attacks such as `](evil)` breaking out of the intended
//! link target.
//!
//! # Determinism (R4)
//!
//! Output is byte-identical across calls for the same [`Graph`].  The function iterates
//! `graph.nodes` / `graph.edges` in the given order and uses [`BTreeMap`] for auxiliary lookups
//! whose key traversal order contributes to the rendered text.  No `HashMap` iteration order
//! leaks into the output.

use std::collections::BTreeMap;
use std::fmt::Write as FmtWrite;

use habitat_graph_core::{display_safe, sanitize_label, Graph, NodeId};

use crate::escape::redact_public_text;

/// Renders `graph` as a plain-Markdown wiki: one article per node plus an `index.md`.
///
/// Returns a [`Vec`] of `(filename, content)` pairs (same shape as
/// [`render_vault`](crate::obsidian::render_vault)):
///
/// - `index.md` — one `- [{label}](node-{id}.md)` bullet per node in `graph.nodes` order.
/// - `node-{id}.md` — article per node: H1 title, source location, `## Outbound` section
///   (outgoing edges rendered as Markdown links with relation in parentheses), and `## Inbound`
///   section (incoming edges).
///
/// **Link closure guarantee:** every `](node-{id}.md)` link target in the returned content is
/// present as a filename in the returned [`Vec`].  Edge endpoints that are absent from
/// `graph.nodes` are silently omitted from link lists to preserve this invariant.
///
/// The result follows `graph.nodes` order with `index.md` appended last.  Callers that need
/// lexicographic file-tree order should sort the returned vec themselves.
///
/// # Security
///
/// All labels, paths, and relations pass through [`sanitize_label`] and [`display_safe`].
/// Labels embedded in Markdown link text also pass through the internal bracket-escaper so
/// that `]` inside a label cannot close the link text early.
#[must_use]
pub fn render_wiki(graph: &Graph) -> Vec<(String, String)> {
    // NodeId → raw label (&str borrowed from graph).  BTreeMap for deterministic key traversal.
    let label_map: BTreeMap<NodeId, &str> = graph
        .nodes
        .iter()
        .map(|n| (n.id, n.label.as_str()))
        .collect();

    // Outbound adjacency: source NodeId → sorted [(relation, target NodeId)].
    // Inbound adjacency:  target NodeId → sorted [(relation, source NodeId)].
    // Only edges where BOTH endpoints exist in label_map are included (link-closure invariant).
    let mut outbound: BTreeMap<NodeId, Vec<(String, NodeId)>> = BTreeMap::new();
    let mut inbound: BTreeMap<NodeId, Vec<(String, NodeId)>> = BTreeMap::new();

    for edge in &graph.edges {
        if label_map.contains_key(&edge.source) && label_map.contains_key(&edge.target) {
            outbound
                .entry(edge.source)
                .or_default()
                .push((edge.relation.clone(), edge.target));
            inbound
                .entry(edge.target)
                .or_default()
                .push((edge.relation.clone(), edge.source));
        }
    }

    // Sort each adjacency list by (relation, peer_id) for determinism (R4).
    for list in outbound.values_mut() {
        list.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    }
    for list in inbound.values_mut() {
        list.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    }

    // Render one article per node, in graph.nodes order.
    let mut result: Vec<(String, String)> = graph
        .nodes
        .iter()
        .map(|node| {
            let filename = node_filename(node.id);
            let content = render_node_article(
                node.id,
                &node.label,
                &node.source_file,
                node.source_location.start_line,
                &outbound,
                &inbound,
                &label_map,
            );
            (filename, content)
        })
        .collect();

    // Append the index last.
    result.push(("index.md".to_owned(), render_index(graph, &label_map)));

    result
}

/// Returns the stable article filename for a node: `node-{id}.md`.
///
/// The [`NodeId`] integer is unique and stable so cross-article links always resolve without
/// any disambiguation logic.
#[must_use]
fn node_filename(id: NodeId) -> String {
    format!("node-{}.md", id.get())
}

/// Escapes `label` for safe embedding as Markdown inline link text `[text](url)`.
///
/// First applies [`display_safe`] (neutralises Trojan-Source bidi overrides and control
/// characters), then replaces `[` with `\[` and `]` with `\]` so that a crafted label cannot
/// close the link text bracket early and redirect the link destination (link-injection defense).
#[must_use]
fn md_link_text(label: &str) -> String {
    let redacted = redact_public_text(label);
    let safe = display_safe(&redacted);
    // Capacity hint: add a small margin for potential escapes.
    let mut out = String::with_capacity(safe.len().saturating_add(8));
    for ch in safe.chars() {
        match ch {
            '[' => out.push_str("\\["),
            ']' => out.push_str("\\]"),
            c => out.push(c),
        }
    }
    out
}

/// Renders the `index.md` listing every node as a plain Markdown link.
#[must_use]
fn render_index(graph: &Graph, label_map: &BTreeMap<NodeId, &str>) -> String {
    let mut out = String::from("# Index\n\n");
    for node in &graph.nodes {
        let label = label_map.get(&node.id).copied().unwrap_or("");
        let link_text = md_link_text(&sanitize_label(label));
        // write! on String is infallible (the only failure mode is OOM, which aborts).
        let _ = writeln!(out, "- [{link_text}](node-{}.md)", node.id.get());
    }
    out
}

/// Renders the Markdown article for one node.
///
/// Sections:
/// - `# {safe_label}` — H1 heading with [`display_safe`] + [`sanitize_label`] applied.
/// - Source location block.
/// - `## Outbound` — one `- [target](node-{id}.md) (relation)` bullet per outgoing edge.
/// - `## Inbound`  — one bullet per incoming edge.
///
/// Sections with no edges render `_none_`.
#[must_use]
fn render_node_article(
    id: NodeId,
    label: &str,
    source_file: &str,
    start_line: u32,
    outbound: &BTreeMap<NodeId, Vec<(String, NodeId)>>,
    inbound: &BTreeMap<NodeId, Vec<(String, NodeId)>>,
    label_map: &BTreeMap<NodeId, &str>,
) -> String {
    let redacted_label = redact_public_text(label);
    let redacted_file = redact_public_text(source_file);
    let safe_label = display_safe(&sanitize_label(&redacted_label));
    let safe_file = display_safe(&redacted_file);

    let mut out = String::new();
    // H1 title.
    let _ = writeln!(out, "# {safe_label}\n");
    // Source location.
    let _ = writeln!(out, "Source: `{safe_file}` line {start_line}\n");

    // Outbound section.
    out.push_str("## Outbound\n\n");
    let ob_edges = outbound
        .get(&id)
        .map_or(&[] as &[(String, NodeId)], Vec::as_slice);
    if ob_edges.is_empty() {
        out.push_str("_none_\n");
    } else {
        for (relation, target_id) in ob_edges {
            let target_label = label_map.get(target_id).copied().unwrap_or("");
            let link_text = md_link_text(&sanitize_label(target_label));
            let redacted_relation = redact_public_text(relation);
            let safe_rel = display_safe(&sanitize_label(&redacted_relation));
            let _ = writeln!(
                out,
                "- [{link_text}](node-{}.md) ({safe_rel})",
                target_id.get()
            );
        }
    }

    out.push('\n');

    // Inbound section.
    out.push_str("## Inbound\n\n");
    let ib_edges = inbound
        .get(&id)
        .map_or(&[] as &[(String, NodeId)], Vec::as_slice);
    if ib_edges.is_empty() {
        out.push_str("_none_\n");
    } else {
        for (relation, source_id) in ib_edges {
            let source_label = label_map.get(source_id).copied().unwrap_or("");
            let link_text = md_link_text(&sanitize_label(source_label));
            let redacted_relation = redact_public_text(relation);
            let safe_rel = display_safe(&sanitize_label(&redacted_relation));
            let _ = writeln!(
                out,
                "- [{link_text}](node-{}.md) ({safe_rel})",
                source_id.get()
            );
        }
    }

    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use habitat_graph_core::{Community, CommunityId, Confidence, Edge, Graph, Node, NodeId, Span};

    use super::{md_link_text, node_filename, render_wiki};

    // ── Fixtures ──────────────────────────────────────────────────────────────

    fn span() -> Span {
        Span::new(0, 1, 1, 1)
    }

    fn span_at(line: u32) -> Span {
        Span::new(0, 1, line, line)
    }

    fn make_node(id: u32, label: &str, file: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: file.to_owned(),
            source_location: span(),
        }
    }

    fn make_node_at(id: u32, label: &str, file: &str, line: u32) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: file.to_owned(),
            source_location: span_at(line),
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

    fn graph_nodes(nodes: Vec<Node>) -> Graph {
        Graph {
            nodes,
            ..Graph::default()
        }
    }

    /// Collect all `](...)` link destinations from a Markdown string.
    fn extract_md_link_targets(content: &str) -> Vec<&str> {
        let mut targets = Vec::new();
        let mut rest = content;
        while let Some(pos) = rest.find("](") {
            let after = &rest[pos + 2..];
            if let Some(end) = after.find(')') {
                targets.push(&after[..end]);
            }
            rest = &rest[pos + 1..];
        }
        targets
    }

    /// Build a `HashSet` of filenames from the returned pairs.
    fn filenames(pages: &[(String, String)]) -> std::collections::HashSet<&str> {
        pages.iter().map(|(f, _)| f.as_str()).collect()
    }

    /// Find the content for a given filename, panicking if absent.
    fn content_of<'a>(pages: &'a [(String, String)], fname: &str) -> &'a str {
        pages.iter().find(|(f, _)| f == fname).map_or_else(
            || panic!("file '{fname}' not found in result"),
            |(_, c)| c.as_str(),
        )
    }

    // ── T01: empty graph yields only index.md ─────────────────────────────────

    #[test]
    fn empty_graph_yields_only_index() {
        let pages = render_wiki(&Graph::default());
        assert_eq!(pages.len(), 1, "expected exactly 1 file (index.md)");
        assert_eq!(pages[0].0, "index.md");
    }

    // ── T02: single node yields two files ─────────────────────────────────────

    #[test]
    fn single_node_yields_two_files() {
        let g = graph_nodes(vec![make_node(1, "alpha", "a.rs")]);
        let pages = render_wiki(&g);
        assert_eq!(pages.len(), 2, "expected node article + index = 2 files");
    }

    // ── T03: N nodes yields N + 1 files ───────────────────────────────────────

    #[test]
    fn n_nodes_yields_n_plus_one_files() {
        let nodes: Vec<Node> = (1..=7_u32)
            .map(|i| make_node(i, &format!("n{i}"), "src.rs"))
            .collect();
        let g = graph_nodes(nodes);
        let pages = render_wiki(&g);
        assert_eq!(pages.len(), 8, "7 nodes + index = 8 files");
    }

    // ── T04: index filename is exactly "index.md" ─────────────────────────────

    #[test]
    fn index_filename_is_index_md() {
        let g = graph_nodes(vec![make_node(5, "x", "x.rs")]);
        let pages = render_wiki(&g);
        assert!(
            pages.iter().any(|(f, _)| f == "index.md"),
            "index.md missing from result: {pages:?}"
        );
    }

    // ── T05: node filename format is "node-{id}.md" ───────────────────────────

    #[test]
    fn node_filename_format_id_based() {
        let g = graph_nodes(vec![make_node(42, "thing", "t.rs")]);
        let pages = render_wiki(&g);
        let fnames: Vec<&str> = pages.iter().map(|(f, _)| f.as_str()).collect();
        assert!(
            fnames.contains(&"node-42.md"),
            "expected node-42.md in {fnames:?}"
        );
    }

    // ── T06: node_filename helper produces correct string ─────────────────────

    #[test]
    fn node_filename_helper_id_zero() {
        assert_eq!(node_filename(NodeId::new(0)), "node-0.md");
    }

    // ── T07: node_filename with large id ──────────────────────────────────────

    #[test]
    fn node_filename_helper_large_id() {
        assert_eq!(node_filename(NodeId::new(999_999)), "node-999999.md");
    }

    // ── T08: index always present even with nodes ──────────────────────────────

    #[test]
    fn index_always_present_with_nodes() {
        let g = graph_nodes(vec![make_node(1, "a", "a.rs"), make_node(2, "b", "b.rs")]);
        let pages = render_wiki(&g);
        assert!(
            pages.iter().any(|(f, _)| f == "index.md"),
            "index.md missing when nodes present"
        );
    }

    // ── T09: index lists every node ───────────────────────────────────────────

    #[test]
    fn index_lists_all_nodes() {
        let g = graph_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
            make_node(3, "gamma", "c.rs"),
        ]);
        let pages = render_wiki(&g);
        let index = content_of(&pages, "index.md");
        assert!(
            index.contains("node-1.md"),
            "node-1.md missing from index: {index}"
        );
        assert!(
            index.contains("node-2.md"),
            "node-2.md missing from index: {index}"
        );
        assert!(
            index.contains("node-3.md"),
            "node-3.md missing from index: {index}"
        );
    }

    // ── T10: index link format is "- [label](node-{id}.md)" ──────────────────

    #[test]
    fn index_link_format() {
        let g = graph_nodes(vec![make_node(7, "myfunc", "src.rs")]);
        let pages = render_wiki(&g);
        let index = content_of(&pages, "index.md");
        assert!(
            index.contains("- [myfunc](node-7.md)"),
            "expected '- [myfunc](node-7.md)' in index: {index}"
        );
    }

    // ── T11: index has a title heading ────────────────────────────────────────

    #[test]
    fn index_has_title_heading() {
        let pages = render_wiki(&Graph::default());
        let index = content_of(&pages, "index.md");
        assert!(
            index.contains("# Index"),
            "expected '# Index' heading in index: {index}"
        );
    }

    // ── T12: index entry count matches node count ──────────────────────────────

    #[test]
    fn index_has_correct_entry_count() {
        let nodes: Vec<Node> = (1..=5_u32)
            .map(|i| make_node(i, &format!("n{i}"), "f.rs"))
            .collect();
        let g = graph_nodes(nodes);
        let pages = render_wiki(&g);
        let index = content_of(&pages, "index.md");
        let bullet_count = index.lines().filter(|l| l.starts_with("- [")).count();
        assert_eq!(
            bullet_count, 5,
            "expected 5 index bullets; got {bullet_count}"
        );
    }

    // ── T13: all index link targets resolve ───────────────────────────────────

    #[test]
    fn all_index_link_targets_resolve() {
        let nodes: Vec<Node> = (1..=4_u32)
            .map(|i| make_node(i, &format!("n{i}"), "f.rs"))
            .collect();
        let g = graph_nodes(nodes);
        let pages = render_wiki(&g);
        let file_set = filenames(&pages);
        let index = content_of(&pages, "index.md");
        for target in extract_md_link_targets(index) {
            assert!(
                file_set.contains(target),
                "index link target '{target}' not present in result"
            );
        }
    }

    // ── T14: all outbound link targets resolve ────────────────────────────────

    #[test]
    fn all_outbound_link_targets_resolve() {
        let mut g = graph_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
            make_node(3, "gamma", "c.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "calls"));
        g.edges.push(make_edge(1, 3, "imports"));
        let pages = render_wiki(&g);
        let file_set = filenames(&pages);
        let article = content_of(&pages, "node-1.md");
        for target in extract_md_link_targets(article) {
            assert!(
                file_set.contains(target),
                "outbound link target '{target}' not in result"
            );
        }
    }

    // ── T15: all inbound link targets resolve ─────────────────────────────────

    #[test]
    fn all_inbound_link_targets_resolve() {
        let mut g = graph_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "calls"));
        let pages = render_wiki(&g);
        let file_set = filenames(&pages);
        // beta (node-2.md) should have inbound link back to node-1.md.
        let article = content_of(&pages, "node-2.md");
        for target in extract_md_link_targets(article) {
            assert!(
                file_set.contains(target),
                "inbound link target '{target}' not in result"
            );
        }
    }

    // ── T16: comprehensive link-closure check across all files ────────────────

    #[test]
    fn all_links_in_all_files_resolve() {
        let mut g = graph_nodes(vec![
            make_node(1, "hub", "h.rs"),
            make_node(2, "a", "a.rs"),
            make_node(3, "b", "b.rs"),
            make_node(4, "c", "c.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "calls"));
        g.edges.push(make_edge(2, 3, "imports"));
        g.edges.push(make_edge(3, 4, "uses"));
        g.edges.push(make_edge(4, 1, "returns"));
        let pages = render_wiki(&g);
        let file_set = filenames(&pages);
        for (fname, content) in &pages {
            for target in extract_md_link_targets(content) {
                assert!(
                    file_set.contains(target),
                    "broken link '{target}' found in '{fname}'"
                );
            }
        }
    }

    // ── T17: node article has H1 heading ──────────────────────────────────────

    #[test]
    fn node_article_has_h1_title() {
        let g = graph_nodes(vec![make_node(1, "myfn", "src.rs")]);
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        assert!(
            article.starts_with("# myfn"),
            "article must start with H1: {article:?}"
        );
    }

    // ── T18: H1 title matches the node label ──────────────────────────────────

    #[test]
    fn node_article_title_matches_label() {
        let g = graph_nodes(vec![make_node(3, "MyStruct", "lib.rs")]);
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-3.md");
        assert!(
            article.contains("# MyStruct"),
            "expected '# MyStruct' in article: {article}"
        );
    }

    // ── T19: article has source location block ────────────────────────────────

    #[test]
    fn node_article_has_source_location() {
        let g = graph_nodes(vec![make_node(1, "foo", "crates/bar/src/lib.rs")]);
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        assert!(
            article.contains("crates/bar/src/lib.rs"),
            "source file missing from article: {article}"
        );
    }

    // ── T20: article source location shows line number ────────────────────────

    #[test]
    fn node_article_source_location_line_number() {
        let mut g = Graph::default();
        g.nodes.push(make_node_at(1, "MyFn", "src.rs", 42));
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        assert!(
            article.contains("line 42"),
            "expected 'line 42' in article: {article}"
        );
    }

    // ── T21: article has ## Outbound section ──────────────────────────────────

    #[test]
    fn node_article_has_outbound_section() {
        let g = graph_nodes(vec![make_node(1, "solo", "s.rs")]);
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        assert!(
            article.contains("## Outbound"),
            "## Outbound missing: {article}"
        );
    }

    // ── T22: article has ## Inbound section ───────────────────────────────────

    #[test]
    fn node_article_has_inbound_section() {
        let g = graph_nodes(vec![make_node(1, "solo", "s.rs")]);
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        assert!(
            article.contains("## Inbound"),
            "## Inbound missing: {article}"
        );
    }

    // ── T23: outbound section lists the outgoing edge ─────────────────────────

    #[test]
    fn outbound_section_lists_edge() {
        let mut g = graph_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "calls"));
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        // Should contain a link to node-2.md in the Outbound section.
        assert!(
            article.contains("node-2.md"),
            "outbound link to node-2.md missing: {article}"
        );
        assert!(
            article.contains("[beta]"),
            "outbound link text 'beta' missing: {article}"
        );
    }

    // ── T24: outbound section shows relation in parentheses ───────────────────

    #[test]
    fn outbound_section_shows_relation() {
        let mut g = graph_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "imports"));
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        assert!(
            article.contains("(imports)"),
            "relation 'imports' missing from outbound: {article}"
        );
    }

    // ── T25: inbound section lists the incoming edge ──────────────────────────

    #[test]
    fn inbound_section_lists_edge() {
        let mut g = graph_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "calls"));
        let pages = render_wiki(&g);
        // beta (node-2.md) receives the edge from alpha (node-1.md).
        let article = content_of(&pages, "node-2.md");
        assert!(
            article.contains("node-1.md"),
            "inbound link to node-1.md missing from beta: {article}"
        );
        assert!(
            article.contains("[alpha]"),
            "inbound link text 'alpha' missing: {article}"
        );
    }

    // ── T26: inbound section shows relation in parentheses ────────────────────

    #[test]
    fn inbound_section_shows_relation() {
        let mut g = graph_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "defines"));
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-2.md");
        assert!(
            article.contains("(defines)"),
            "relation 'defines' missing from inbound: {article}"
        );
    }

    // ── T27: no outbound edges renders _none_ ─────────────────────────────────

    #[test]
    fn outbound_none_when_no_outgoing_edges() {
        let g = graph_nodes(vec![
            make_node(1, "source", "s.rs"),
            make_node(2, "sink", "k.rs"),
        ]);
        // edge goes FROM node 1 TO node 2 — node 2 has no outbound edges.
        let mut g2 = g;
        g2.edges.push(make_edge(1, 2, "calls"));
        let pages = render_wiki(&g2);
        let article = content_of(&pages, "node-2.md");
        // Must say _none_ after ## Outbound.
        let ob_pos = article.find("## Outbound").expect("## Outbound missing");
        let ib_pos = article.find("## Inbound").expect("## Inbound missing");
        let ob_section = &article[ob_pos..ib_pos];
        assert!(
            ob_section.contains("_none_"),
            "expected '_none_' in Outbound section: {ob_section}"
        );
    }

    // ── T28: no inbound edges renders _none_ ──────────────────────────────────

    #[test]
    fn inbound_none_when_no_incoming_edges() {
        let mut g = graph_nodes(vec![
            make_node(1, "source", "s.rs"),
            make_node(2, "sink", "k.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "calls"));
        let pages = render_wiki(&g);
        // node 1 (source) has no inbound edges.
        let article = content_of(&pages, "node-1.md");
        let ib_pos = article.find("## Inbound").expect("## Inbound missing");
        let ib_section = &article[ib_pos..];
        assert!(
            ib_section.contains("_none_"),
            "expected '_none_' in Inbound section: {ib_section}"
        );
    }

    // ── T29: isolated node shows _none_ in both sections ─────────────────────

    #[test]
    fn isolated_node_none_in_both_sections() {
        let g = graph_nodes(vec![make_node(1, "island", "i.rs")]);
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        // Count _none_ occurrences — should be at least 2 (one per section).
        let count = article.matches("_none_").count();
        assert!(
            count >= 2,
            "expected >= 2 '_none_' placeholders for isolated node; got {count}: {article}"
        );
    }

    // ── T30: self-loop appears in both outbound and inbound ───────────────────

    #[test]
    fn self_loop_in_both_outbound_and_inbound() {
        let mut g = graph_nodes(vec![make_node(1, "recursive", "r.rs")]);
        g.edges.push(make_edge(1, 1, "recurses"));
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        let ob_pos = article.find("## Outbound").expect("## Outbound missing");
        let ib_pos = article.find("## Inbound").expect("## Inbound missing");
        let ob_section = &article[ob_pos..ib_pos];
        let ib_section = &article[ib_pos..];
        assert!(
            ob_section.contains("node-1.md"),
            "self-loop missing from Outbound: {ob_section}"
        );
        assert!(
            ib_section.contains("node-1.md"),
            "self-loop missing from Inbound: {ib_section}"
        );
    }

    // ── T31: multiple outbound edges all listed ────────────────────────────────

    #[test]
    fn multiple_outbound_edges_all_listed() {
        let mut g = graph_nodes(vec![
            make_node(1, "hub", "h.rs"),
            make_node(2, "a", "a.rs"),
            make_node(3, "b", "b.rs"),
            make_node(4, "c", "c.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "calls"));
        g.edges.push(make_edge(1, 3, "imports"));
        g.edges.push(make_edge(1, 4, "uses"));
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        let ob_pos = article.find("## Outbound").expect("## Outbound missing");
        let ob_section = &article[ob_pos..];
        assert!(
            ob_section.contains("node-2.md"),
            "link to node-2.md missing: {ob_section}"
        );
        assert!(
            ob_section.contains("node-3.md"),
            "link to node-3.md missing: {ob_section}"
        );
        assert!(
            ob_section.contains("node-4.md"),
            "link to node-4.md missing: {ob_section}"
        );
    }

    // ── T32: multiple inbound edges all listed ────────────────────────────────

    #[test]
    fn multiple_inbound_edges_all_listed() {
        let mut g = graph_nodes(vec![
            make_node(1, "sink", "s.rs"),
            make_node(2, "x", "x.rs"),
            make_node(3, "y", "y.rs"),
        ]);
        g.edges.push(make_edge(2, 1, "calls"));
        g.edges.push(make_edge(3, 1, "imports"));
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        let ib_pos = article.find("## Inbound").expect("## Inbound missing");
        let ib_section = &article[ib_pos..];
        assert!(
            ib_section.contains("node-2.md"),
            "inbound from node-2.md missing: {ib_section}"
        );
        assert!(
            ib_section.contains("node-3.md"),
            "inbound from node-3.md missing: {ib_section}"
        );
    }

    // ── T33: outbound link URL is the target's article filename ───────────────

    #[test]
    fn outbound_link_url_is_target_filename() {
        let mut g = graph_nodes(vec![
            make_node(10, "src", "s.rs"),
            make_node(20, "tgt", "t.rs"),
        ]);
        g.edges.push(make_edge(10, 20, "uses"));
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-10.md");
        assert!(
            article.contains("](node-20.md)"),
            "expected outbound link '](node-20.md)': {article}"
        );
    }

    // ── T34: inbound link URL is the source's article filename ────────────────

    #[test]
    fn inbound_link_url_is_source_filename() {
        let mut g = graph_nodes(vec![
            make_node(10, "src", "s.rs"),
            make_node(20, "tgt", "t.rs"),
        ]);
        g.edges.push(make_edge(10, 20, "uses"));
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-20.md");
        assert!(
            article.contains("](node-10.md)"),
            "expected inbound link '](node-10.md)': {article}"
        );
    }

    // ── T35: deterministic — empty graph ──────────────────────────────────────

    #[test]
    fn deterministic_empty_graph() {
        let first = render_wiki(&Graph::default());
        let second = render_wiki(&Graph::default());
        assert_eq!(
            first, second,
            "render_wiki must be deterministic on empty graph"
        );
    }

    // ── T36: deterministic — with nodes and edges ─────────────────────────────

    #[test]
    fn deterministic_with_nodes_and_edges() {
        let build = || {
            let mut g = graph_nodes(vec![
                make_node(1, "alpha", "a.rs"),
                make_node(2, "beta", "b.rs"),
                make_node(3, "gamma", "c.rs"),
            ]);
            g.edges.push(make_edge(1, 2, "calls"));
            g.edges.push(make_edge(2, 3, "imports"));
            g.edges.push(make_edge(3, 1, "returns"));
            g
        };
        assert_eq!(
            render_wiki(&build()),
            render_wiki(&build()),
            "render_wiki must be deterministic"
        );
    }

    // ── T37: deterministic — many nodes (exercises BTreeMap order) ────────────

    #[test]
    fn deterministic_many_nodes() {
        let nodes: Vec<Node> = (1..=10_u32)
            .map(|i| make_node(i, &format!("node{i}"), "src.rs"))
            .collect();
        let g = graph_nodes(nodes);
        let first = render_wiki(&g);
        let second = render_wiki(&g);
        assert_eq!(first, second, "must be deterministic with many nodes");
    }

    // ── T38: injection — close-bracket in label escaped in link text ──────────

    #[test]
    fn label_injection_close_bracket_escaped_in_link_text() {
        // A label containing ']' could close the link text bracket early.
        let mut g = graph_nodes(vec![
            make_node(1, "src", "s.rs"),
            make_node(2, "evil]label", "e.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "calls"));
        let pages = render_wiki(&g);
        // Check both the index and the outbound link in the source article.
        let index = content_of(&pages, "index.md");
        let article = content_of(&pages, "node-1.md");
        // The raw ']' must not appear inside '[…]' link text (only outside as the link bracket).
        // We check that the escaped form '\]' is present when the label contains ']'.
        assert!(
            index.contains("\\]"),
            "raw ']' in label must be escaped in index: {index}"
        );
        assert!(
            article.contains("\\]"),
            "raw ']' in label must be escaped in outbound section: {article}"
        );
    }

    // ── T39: injection — open-bracket in label escaped in link text ───────────

    #[test]
    fn label_injection_open_bracket_escaped_in_link_text() {
        let mut g = graph_nodes(vec![
            make_node(1, "src", "s.rs"),
            make_node(2, "[nested[label", "n.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "uses"));
        let pages = render_wiki(&g);
        let index = content_of(&pages, "index.md");
        assert!(
            index.contains("\\["),
            "raw '[' in label must be escaped in index: {index}"
        );
    }

    // ── T40: injection — ](evil.com) label cannot redirect link destination ───

    #[test]
    fn label_injection_bracket_url_neutralized() {
        // Label "](evil.com)" would redirect the link if `]` were not escaped.
        // Without escaping: `[](evil.com)](node-2.md)` → empty link text, destination = evil.com.
        // With escaping:    `[\](evil.com)](node-2.md)` → link text = `](evil.com)`, destination = node-2.md.
        let mut g = graph_nodes(vec![
            make_node(1, "src", "s.rs"),
            make_node(2, "](evil.com)", "e.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "calls"));
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        // The injection form `[](evil.com)` (empty link text, evil.com as target) must not appear.
        assert!(
            !article.contains("[](evil.com)"),
            "injection form '[](evil.com)' leaked into output: {article}"
        );
        // The correct link destination must still be node-2.md.
        assert!(
            article.contains("](node-2.md)"),
            "correct link destination node-2.md missing: {article}"
        );
        // The `]` from the label must be bracket-escaped.
        assert!(
            article.contains("\\]"),
            "bracket-escaped ']' must appear in article: {article}"
        );
    }

    // ── T41: injection — newline in label is stripped before reaching output ────

    #[test]
    fn label_injection_newline_stripped() {
        // Newline is a control char. `sanitize_label` strips it before `display_safe` sees it.
        // The combined pipeline guarantees no raw newline appears in any link text.
        let label = "line1\nline2";
        let g = graph_nodes(vec![make_node(1, label, "f.rs")]);
        let pages = render_wiki(&g);
        let index = content_of(&pages, "index.md");
        // The raw "line1\nline2" must not appear in the index link text.
        assert!(
            !index.contains("line1\nline2"),
            "raw newline in label must not appear in index: {index}"
        );
        // The sanitised form (newline stripped) must appear in the index.
        // sanitize_label("line1\nline2") = "line1line2"; display_safe preserves it.
        assert!(
            index.contains("line1line2"),
            "sanitized label 'line1line2' must appear in index: {index}"
        );
    }

    // ── T42: security — bidi override U+202E in label escaped ────────────────

    #[test]
    fn bidi_override_in_label_escaped() {
        // U+202E = RIGHT-TO-LEFT OVERRIDE (classic Trojan-Source payload).
        let evil_label = "fn\u{202E}safe";
        let g = graph_nodes(vec![make_node(1, evil_label, "src.rs")]);
        let pages = render_wiki(&g);
        // Must not appear raw in any output file.
        for (fname, content) in &pages {
            assert!(
                !content.contains('\u{202E}'),
                "raw U+202E leaked into '{fname}': {content:?}"
            );
        }
        // Must appear escaped.
        let article = content_of(&pages, "node-1.md");
        assert!(
            article.contains("\\u{202E}"),
            "escaped form missing from article: {article:?}"
        );
    }

    // ── T43: security — bidi override in relation escaped ─────────────────────

    #[test]
    fn bidi_override_in_relation_escaped() {
        let mut g = graph_nodes(vec![make_node(1, "a", "a.rs"), make_node(2, "b", "b.rs")]);
        g.edges.push(make_edge(1, 2, "calls\u{202E}evil"));
        let pages = render_wiki(&g);
        for (fname, content) in &pages {
            assert!(
                !content.contains('\u{202E}'),
                "raw U+202E in relation leaked into '{fname}'"
            );
        }
        let article = content_of(&pages, "node-1.md");
        assert!(
            article.contains("\\u{202E}"),
            "relation bidi override not escaped: {article:?}"
        );
    }

    // ── T44: security — bidi override in source_file escaped ─────────────────

    #[test]
    fn bidi_override_in_source_file_escaped() {
        let evil_path = "src/\u{202E}evil.rs";
        let g = graph_nodes(vec![make_node(1, "n", evil_path)]);
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        assert!(
            !article.contains('\u{202E}'),
            "raw U+202E in source_file leaked into article: {article:?}"
        );
        assert!(
            article.contains("\\u{202E}"),
            "escaped form missing from article source line: {article:?}"
        );
    }

    // ── T45: security — cypher injection in relation neutralized ──────────────

    #[test]
    fn cypher_injection_in_relation_neutralized() {
        // Classic Cypher injection: "' DETACH DELETE n //" must not break structure.
        let evil_rel = "' DETACH DELETE n //";
        let mut g = graph_nodes(vec![make_node(1, "a", "a.rs"), make_node(2, "b", "b.rs")]);
        g.edges.push(make_edge(1, 2, evil_rel));
        let pages = render_wiki(&g);
        // The raw string is plain text in Markdown — display_safe leaves non-bidi chars alone.
        // The key guard is that the relation appears only in parentheses, not as a link URL.
        let article = content_of(&pages, "node-1.md");
        // The link URL must still be node-2.md (the relation is in parens after the link).
        assert!(
            article.contains("](node-2.md)"),
            "link URL must be node-2.md even with injection relation: {article}"
        );
    }

    // ── T46: security — pipe in label is safe in Markdown ────────────────────

    #[test]
    fn pipe_in_label_safe_outside_tables() {
        // Pipe is Markdown-special only in tables; in links and headings it is literal.
        let g = graph_nodes(vec![make_node(1, "a|b", "f.rs")]);
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        // The heading should contain the label with pipe.
        assert!(
            article.contains("a|b"),
            "pipe in label should appear literally in H1: {article}"
        );
    }

    // ── T47: edges to unknown node IDs produce no broken links ────────────────

    #[test]
    fn edge_to_unknown_node_no_broken_link() {
        // Node 99 is referenced by an edge but has no entry in graph.nodes.
        let mut g = graph_nodes(vec![make_node(1, "known", "k.rs")]);
        g.edges.push(make_edge(1, 99, "calls")); // 99 is a ghost node
        let pages = render_wiki(&g);
        let file_set = filenames(&pages);
        // node-99.md must NOT be generated (ghost node).
        assert!(
            !file_set.contains("node-99.md"),
            "ghost node file should not exist: {file_set:?}"
        );
        // All links in all files must resolve.
        for (fname, content) in &pages {
            for target in extract_md_link_targets(content) {
                assert!(
                    file_set.contains(target),
                    "broken link '{target}' in '{fname}'"
                );
            }
        }
    }

    // ── T48: outbound edges sorted by (relation, target_id) ───────────────────

    #[test]
    fn outbound_edges_sorted_by_relation_then_id() {
        let mut g = graph_nodes(vec![
            make_node(1, "hub", "h.rs"),
            make_node(2, "b", "b.rs"),
            make_node(3, "c", "c.rs"),
            make_node(4, "d", "d.rs"),
        ]);
        // Two edges with same relation → sorted by target id; one with different relation.
        g.edges.push(make_edge(1, 4, "calls")); // calls / 4
        g.edges.push(make_edge(1, 2, "calls")); // calls / 2  → should come first
        g.edges.push(make_edge(1, 3, "imports")); // imports / 3 → last
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        let ob_pos = article.find("## Outbound").expect("## Outbound missing");
        let ib_pos = article.find("## Inbound").expect("## Inbound missing");
        let ob_section = &article[ob_pos..ib_pos];
        // "calls … node-2.md" must appear before "calls … node-4.md".
        let pos2 = ob_section.find("node-2.md").unwrap_or(usize::MAX);
        let pos4 = ob_section.find("node-4.md").unwrap_or(usize::MAX);
        let pos3 = ob_section.find("node-3.md").unwrap_or(usize::MAX);
        assert!(
            pos2 < pos4,
            "node-2.md must come before node-4.md (same relation, lower id)"
        );
        assert!(
            pos4 < pos3,
            "calls edges must come before imports edge (alpha order)"
        );
    }

    // ── T49: inbound edges sorted by (relation, source_id) ────────────────────

    #[test]
    fn inbound_edges_sorted_by_relation_then_id() {
        let mut g = graph_nodes(vec![
            make_node(1, "sink", "s.rs"),
            make_node(2, "x", "x.rs"),
            make_node(3, "y", "y.rs"),
            make_node(4, "z", "z.rs"),
        ]);
        // Two sources with same relation → lower id first.
        g.edges.push(make_edge(4, 1, "calls")); // calls / 4
        g.edges.push(make_edge(2, 1, "calls")); // calls / 2 → first
        g.edges.push(make_edge(3, 1, "imports")); // imports / 3 → last
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        let ib_pos = article.find("## Inbound").expect("## Inbound missing");
        let ib_section = &article[ib_pos..];
        let pos2 = ib_section.find("node-2.md").unwrap_or(usize::MAX);
        let pos4 = ib_section.find("node-4.md").unwrap_or(usize::MAX);
        let pos3 = ib_section.find("node-3.md").unwrap_or(usize::MAX);
        assert!(
            pos2 < pos4,
            "inbound from node-2 before node-4 (same relation)"
        );
        assert!(pos4 < pos3, "calls before imports in inbound");
    }

    // ── T50: file set is exactly nodes plus index ─────────────────────────────

    #[test]
    fn file_set_is_exactly_nodes_plus_index() {
        let mut g = graph_nodes(vec![
            make_node(5, "five", "5.rs"),
            make_node(10, "ten", "10.rs"),
            make_node(15, "fifteen", "15.rs"),
        ]);
        g.edges.push(make_edge(5, 10, "calls"));
        let pages = render_wiki(&g);
        let names: std::collections::HashSet<&str> =
            pages.iter().map(|(f, _)| f.as_str()).collect();
        let expected: std::collections::HashSet<&str> =
            ["node-5.md", "node-10.md", "node-15.md", "index.md"]
                .iter()
                .copied()
                .collect();
        assert_eq!(names, expected, "unexpected file set: {names:?}");
    }

    // ── T51: index entry order follows graph.nodes order ─────────────────────

    #[test]
    fn index_entry_order_follows_graph_nodes_order() {
        // Nodes given in non-monotonic id order; index must follow that same order.
        let g = graph_nodes(vec![
            make_node(3, "gamma", "c.rs"),
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
        ]);
        let pages = render_wiki(&g);
        let index = content_of(&pages, "index.md");
        let positions: Vec<Option<usize>> = ["node-3.md", "node-1.md", "node-2.md"]
            .iter()
            .map(|n| index.find(n))
            .collect();
        assert!(
            positions[0] < positions[1] && positions[1] < positions[2],
            "index order must follow graph.nodes order (3, 1, 2); got: {positions:?}"
        );
    }

    // ── T52: md_link_text escapes brackets only ───────────────────────────────

    #[test]
    fn md_link_text_helper_escapes_brackets() {
        assert_eq!(md_link_text("a]b[c"), "a\\]b\\[c");
    }

    // ── T53: md_link_text passes ordinary alphanumeric text unchanged ─────────

    #[test]
    fn md_link_text_helper_plain_text_unchanged() {
        assert_eq!(md_link_text("MyFunction"), "MyFunction");
    }

    // ── T54: md_link_text escapes bidi override via display_safe ─────────────

    #[test]
    fn md_link_text_helper_bidi_override_escaped() {
        let out = md_link_text("x\u{202E}y");
        assert!(
            !out.contains('\u{202E}'),
            "raw bidi in md_link_text output: {out:?}"
        );
        assert!(out.contains("\\u{202E}"), "escaped form missing: {out:?}");
    }

    // ── T55: outbound link text matches the target node's label ──────────────

    #[test]
    fn outbound_link_text_matches_target_label() {
        let mut g = graph_nodes(vec![
            make_node(1, "source_fn", "s.rs"),
            make_node(2, "TargetStruct", "t.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "uses"));
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        assert!(
            article.contains("[TargetStruct](node-2.md)"),
            "outbound link text must match target label: {article}"
        );
    }

    // ── T56: inbound link text matches the source node's label ───────────────

    #[test]
    fn inbound_link_text_matches_source_label() {
        let mut g = graph_nodes(vec![
            make_node(1, "Producer", "p.rs"),
            make_node(2, "consumer_fn", "c.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "feeds"));
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-2.md");
        assert!(
            article.contains("[Producer](node-1.md)"),
            "inbound link text must match source label: {article}"
        );
    }

    // ── T57: all control chars in label stripped by sanitize_label ────────────

    #[test]
    fn secret_patterns_are_redacted_in_articles_index_and_edges() {
        let mut g = graph_nodes(vec![
            make_node(1, "api_key_assignment_refused", "src/api_key.rs"),
            make_node(2, "safe", "safe.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "Authorization: Bearer token"));
        let pages = render_wiki(&g);
        let joined = pages
            .iter()
            .map(|(_, content)| content.as_str())
            .collect::<String>();
        assert!(!joined.contains("api_key_assignment_refused"));
        assert!(!joined.contains("src/api_key.rs"));
        assert!(!joined.contains("Authorization: Bearer token"));
        assert!(joined.contains("[REDACTED:api_key]"));
        assert!(joined.contains("[REDACTED:bearer_token]"));
    }

    #[test]
    fn label_with_control_chars_sanitized() {
        // Control chars are stripped by sanitize_label before display_safe.
        let label = "ab\x07\x0Bcd";
        let g = graph_nodes(vec![make_node(1, label, "f.rs")]);
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        // BEL (0x07) and VT (0x0B) must not appear raw.
        assert!(
            !article.contains('\x07'),
            "BEL control char leaked into article"
        );
        assert!(
            !article.contains('\x0B'),
            "VT control char leaked into article"
        );
    }

    // ── T58: communities field does not affect output (wiki is community-agnostic) ──

    #[test]
    fn communities_do_not_affect_file_set() {
        let mut g = graph_nodes(vec![make_node(1, "a", "a.rs"), make_node(2, "b", "b.rs")]);
        g.communities.push(make_community(0, &[1, 2]));
        let pages_with = render_wiki(&g);
        g.communities.clear();
        let pages_without = render_wiki(&g);
        // File count must be the same (communities don't add files in the wiki exporter).
        assert_eq!(
            pages_with.len(),
            pages_without.len(),
            "communities must not affect file count"
        );
    }

    // ── T59: large graph — link closure holds for all pages ───────────────────

    #[test]
    fn large_graph_link_closure() {
        let nodes: Vec<Node> = (1..=20_u32)
            .map(|i| make_node(i, &format!("node{i}"), "src.rs"))
            .collect();
        let mut g = graph_nodes(nodes);
        // Chain of edges 1→2→3→…→20→1.
        for i in 1..=20_u32 {
            let next = if i == 20 { 1 } else { i + 1 };
            g.edges.push(make_edge(i, next, "next"));
        }
        let pages = render_wiki(&g);
        let file_set = filenames(&pages);
        for (fname, content) in &pages {
            for target in extract_md_link_targets(content) {
                assert!(
                    file_set.contains(target),
                    "broken link '{target}' in '{fname}'"
                );
            }
        }
        // Exactly 21 files: 20 node articles + index.
        assert_eq!(pages.len(), 21, "expected 20 articles + index = 21 files");
    }

    // ── T60: empty label node — article still has H1 (may be empty) ──────────

    #[test]
    fn empty_label_node_article_has_sections() {
        // Empty label → sanitize_label("") = "", display_safe("") = "".
        let g = graph_nodes(vec![make_node(1, "", "f.rs")]);
        let pages = render_wiki(&g);
        let article = content_of(&pages, "node-1.md");
        // H1 must be present even if content is empty.
        assert!(
            article.starts_with("# \n") || article.starts_with("# "),
            "H1 must be present for empty label: {article:?}"
        );
        assert!(
            article.contains("## Outbound"),
            "## Outbound must be present: {article}"
        );
        assert!(
            article.contains("## Inbound"),
            "## Inbound must be present: {article}"
        );
    }
}
