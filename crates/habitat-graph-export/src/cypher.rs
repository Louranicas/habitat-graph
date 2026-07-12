//! Cypher exporter — Neo4j `MERGE` statements (the graphify `--neo4j` analogue).
//!
//! Every attacker-influenced string (`label`, `source_file`, `relation`) is routed through
//! [`crate::escape::cypher_escape`] before embedding in any single-quoted Cypher literal
//! (STRIDE-T injection guard).  The relationship type is always the fixed literal `:REL`;
//! `relation` is stored as a *property*, never interpolated into the type-identifier position —
//! a label like `' DETACH DELETE n //` is neutralised to `\' DETACH DELETE n //` so it cannot
//! terminate the string literal or inject a new clause.

use std::fmt::Write as FmtWrite;

use habitat_graph_core::Graph;

use crate::escape::{cypher_escape, redact_public_text, PublicRelationProjector};

/// Renders `graph` as a Neo4j Cypher import script (deterministic, infallible).
///
/// The output contains:
///
/// - A header comment line with node and edge counts.
/// - One `MERGE` statement per node, creating or matching a `:Symbol` node:
///   ```text
///   MERGE (n42:Symbol {id: 42, label: 'MyFn', source_file: 'src/lib.rs', line: 10});
///   ```
/// - One `MATCH`/`MERGE` pair per edge, locating endpoints by `id` and merging a `[:REL]`
///   relationship carrying `relation` and `confidence` as properties:
///   ```text
///   MATCH (a {id: 1}),(b {id: 2}) MERGE (a)-[:REL {relation: 'calls', confidence: 'EXTRACTED'}]->(b);
///   ```
///
/// All attacker-influenced strings (`label`, `source_file`, `relation`) are escaped through
/// [`cypher_escape`](crate::escape::cypher_escape).  `confidence` is a bounded enum value
/// produced by this library's own code; it is passed through `cypher_escape` for uniformity.
///
/// Output order follows `graph.nodes` and `graph.edges` in the order they appear.  For
/// byte-identical canonical output across runs, call
/// [`Graph::sorted`](habitat_graph_core::Graph::sorted) before passing the graph (R4).
///
/// # Security
///
/// The relationship type is **always** the fixed identifier `:REL`.  An attacker-controlled
/// `relation` value is stored only as a property string, never in the `[:TYPE]` position —
/// this prevents identifier injection regardless of the relation's content.
#[allow(clippy::module_name_repetitions)]
#[must_use]
pub fn render_cypher(graph: &Graph) -> String {
    let (n_nodes, n_edges, _) = graph.counts();

    // Pre-size: header ~65 bytes; ~130 bytes/node; ~110 bytes/edge (generous but cheap).
    let capacity = 65_usize
        .saturating_add(n_nodes.saturating_mul(130))
        .saturating_add(n_edges.saturating_mul(110));
    let mut out = String::with_capacity(capacity);

    let _ = writeln!(
        out,
        "// habitat-graph cypher export — {n_nodes} nodes, {n_edges} edges"
    );

    for node in &graph.nodes {
        let id = node.id.get();
        let redacted_label = redact_public_text(&node.label);
        let redacted_source_file = redact_public_text(&node.source_file);
        let label = cypher_escape(&redacted_label);
        let source_file = cypher_escape(&redacted_source_file);
        let line = node.source_location.start_line;
        // STRIDE-T: both `label` and `source_file` are attacker-influenced; cypher_escape
        // neutralises single-quote termination, backslash injection, and control characters.
        let _ = writeln!(
            out,
            "MERGE (n{id}:Symbol {{id: {id}, label: '{label}', source_file: '{source_file}', line: {line}}});"
        );
    }

    let mut relation_projector = PublicRelationProjector::new();
    for edge in &graph.edges {
        let src = edge.source.get();
        let tgt = edge.target.get();
        // `relation` is attacker-influenced — escaped AND stored as a property, never as the
        // relationship-type identifier (which is always the fixed literal :REL).
        let projected_relation =
            relation_projector.project(edge.source, edge.target, &edge.relation);
        let relation = cypher_escape(&projected_relation);
        // `confidence` is a bounded enum string from this library's own code; cypher_escape
        // is applied for uniformity (the canonical strings contain no escapable characters).
        let confidence = cypher_escape(edge.confidence.as_str());
        let _ = writeln!(
            out,
            "MATCH (a {{id: {src}}}),(b {{id: {tgt}}}) MERGE (a)-[:REL {{relation: '{relation}', confidence: '{confidence}'}}]->(b);"
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use habitat_graph_core::{Confidence, Edge, Graph, Node, NodeId, Span};

    use super::render_cypher;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn span(start_line: u32) -> Span {
        Span::new(0, 10, start_line, start_line)
    }

    fn node(id: u32, label: &str, file: &str, line: u32) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: file.to_owned(),
            source_location: span(line),
        }
    }

    fn edge(src: u32, tgt: u32, rel: &str, conf: Confidence) -> Edge {
        Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: rel.to_owned(),
            confidence: conf,
        }
    }

    fn one_node(id: u32, label: &str, file: &str, line: u32) -> Graph {
        let mut g = Graph::new();
        g.nodes.push(node(id, label, file, line));
        g
    }

    // ── 1: empty graph has a header comment ──────────────────────────────────

    #[test]
    fn empty_graph_has_header() {
        let cy = render_cypher(&Graph::new());
        assert!(
            cy.starts_with("// habitat-graph cypher export"),
            "header missing: {cy:?}"
        );
    }

    // ── 2: empty graph header reports zero nodes ──────────────────────────────

    #[test]
    fn empty_graph_header_reports_zero_nodes() {
        let cy = render_cypher(&Graph::new());
        assert!(cy.contains("0 nodes"), "zero-node count missing: {cy:?}");
    }

    // ── 3: empty graph header reports zero edges ──────────────────────────────

    #[test]
    fn empty_graph_header_reports_zero_edges() {
        let cy = render_cypher(&Graph::new());
        assert!(cy.contains("0 edges"), "zero-edge count missing: {cy:?}");
    }

    // ── 4: empty graph has no MERGE statement ────────────────────────────────

    #[test]
    fn empty_graph_has_no_merge() {
        let cy = render_cypher(&Graph::new());
        assert!(
            !cy.contains("MERGE"),
            "unexpected MERGE in empty-graph output: {cy:?}"
        );
    }

    // ── 5: empty graph is deterministic ──────────────────────────────────────

    #[test]
    fn empty_graph_is_deterministic() {
        assert_eq!(
            render_cypher(&Graph::new()),
            render_cypher(&Graph::new()),
            "render_cypher must be deterministic for an empty graph"
        );
    }

    // ── 6: single node produces exactly one MERGE statement ──────────────────

    #[test]
    fn single_node_produces_one_merge() {
        let g = one_node(1, "Alpha", "src/a.rs", 5);
        let cy = render_cypher(&g);
        let count = cy.lines().filter(|l| l.starts_with("MERGE")).count();
        assert_eq!(count, 1, "expected 1 MERGE line, got {count}:\n{cy}");
    }

    // ── 7: single node MERGE contains the node id ────────────────────────────

    #[test]
    fn single_node_merge_contains_id() {
        let g = one_node(42, "Fn", "src/a.rs", 1);
        let cy = render_cypher(&g);
        // The id appears both in the variable `n42` and the property `id: 42`.
        assert!(cy.contains("n42"), "variable n42 missing: {cy:?}");
        assert!(cy.contains("id: 42"), "property id: 42 missing: {cy:?}");
    }

    // ── 8: single node MERGE uses :Symbol type label ─────────────────────────

    #[test]
    fn single_node_merge_uses_symbol_type() {
        let g = one_node(1, "Fn", "src/a.rs", 1);
        let cy = render_cypher(&g);
        assert!(cy.contains(":Symbol"), ":Symbol type missing: {cy:?}");
    }

    // ── 9: single node MERGE contains the label property ─────────────────────

    #[test]
    fn single_node_merge_contains_label_property() {
        let g = one_node(1, "MyFunc", "src/lib.rs", 10);
        let cy = render_cypher(&g);
        assert!(
            cy.contains("label: 'MyFunc'"),
            "label property missing: {cy:?}"
        );
    }

    // ── 10: single node MERGE contains source_file property ──────────────────

    #[test]
    fn single_node_merge_contains_source_file_property() {
        let g = one_node(1, "X", "crates/core/src/lib.rs", 1);
        let cy = render_cypher(&g);
        assert!(
            cy.contains("source_file: 'crates/core/src/lib.rs'"),
            "source_file property missing: {cy:?}"
        );
    }

    // ── 11: single node MERGE contains line property ──────────────────────────

    #[test]
    fn single_node_merge_contains_line_property() {
        let g = one_node(1, "X", "f.rs", 99);
        let cy = render_cypher(&g);
        assert!(cy.contains("line: 99"), "line property missing: {cy:?}");
    }

    // ── 12: node MERGE statement ends with ';' ────────────────────────────────

    #[test]
    fn node_statement_ends_with_semicolon() {
        let g = one_node(1, "X", "f.rs", 1);
        let cy = render_cypher(&g);
        let stmt = cy
            .lines()
            .find(|l| l.starts_with("MERGE"))
            .expect("no MERGE line");
        assert!(stmt.ends_with(';'), "MERGE must end with ';': {stmt:?}");
    }

    // ── 13: header reports correct node count ────────────────────────────────

    #[test]
    fn header_reports_correct_node_count() {
        let mut g = Graph::new();
        for i in 0..7_u32 {
            g.nodes.push(node(i, "N", "f.rs", 1));
        }
        let cy = render_cypher(&g);
        assert!(
            cy.contains("7 nodes"),
            "7 nodes missing from header: {cy:?}"
        );
    }

    // ── 14: header reports correct edge count ────────────────────────────────

    #[test]
    fn header_reports_correct_edge_count() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        g.edges.push(edge(2, 3, "imports", Confidence::Inferred));
        g.edges.push(edge(3, 1, "uses", Confidence::Ambiguous));
        let cy = render_cypher(&g);
        assert!(
            cy.contains("3 edges"),
            "3 edges missing from header: {cy:?}"
        );
    }

    // ── 15: N nodes → exactly N MERGE statements ─────────────────────────────

    #[test]
    fn n_nodes_yields_n_merges() {
        let mut g = Graph::new();
        for i in 0..5_u32 {
            g.nodes.push(node(i, &format!("N{i}"), "f.rs", i + 1));
        }
        let cy = render_cypher(&g);
        let count = cy.lines().filter(|l| l.starts_with("MERGE")).count();
        assert_eq!(count, 5, "expected 5 MERGEs, got {count}:\n{cy}");
    }

    // ── 16: edge produces a MATCH + MERGE line ───────────────────────────────

    #[test]
    fn edge_produces_match_merge_line() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        let cy = render_cypher(&g);
        let stmt = cy
            .lines()
            .find(|l| l.starts_with("MATCH"))
            .expect("no MATCH line");
        assert!(
            stmt.contains("MERGE"),
            "edge statement must contain MERGE: {stmt:?}"
        );
    }

    // ── 17: edge statement ends with ';' ─────────────────────────────────────

    #[test]
    fn edge_statement_ends_with_semicolon() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        let cy = render_cypher(&g);
        let stmt = cy
            .lines()
            .find(|l| l.starts_with("MATCH"))
            .expect("no MATCH line");
        assert!(stmt.ends_with(';'), "edge stmt must end with ';': {stmt:?}");
    }

    // ── 18: relationship type is always the fixed :REL identifier ────────────

    #[test]
    fn edge_relationship_type_is_always_rel() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        let cy = render_cypher(&g);
        assert!(cy.contains("[:REL"), "[:REL missing: {cy:?}");
    }

    // ── 19: relation value is a property, not the type identifier ────────────
    //
    // Even if the relation is named something that looks like a Cypher keyword,
    // it must never appear in the `[:TYPE]` position — only inside `{relation: '...'}`.

    #[test]
    fn relation_is_property_not_type_identifier() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        let cy = render_cypher(&g);
        assert!(
            !cy.contains("[:calls"),
            "relation appeared as type identifier: {cy:?}"
        );
        assert!(cy.contains("[:REL"), "fixed :REL type missing: {cy:?}");
    }

    // ── 20: EXTRACTED confidence in edge property ─────────────────────────────

    #[test]
    fn edge_confidence_extracted_in_property() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        let cy = render_cypher(&g);
        assert!(
            cy.contains("confidence: 'EXTRACTED'"),
            "EXTRACTED confidence property missing: {cy:?}"
        );
    }

    // ── 21: INFERRED confidence in edge property ──────────────────────────────

    #[test]
    fn edge_confidence_inferred_in_property() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "uses", Confidence::Inferred));
        let cy = render_cypher(&g);
        assert!(
            cy.contains("confidence: 'INFERRED'"),
            "INFERRED confidence property missing: {cy:?}"
        );
    }

    // ── 22: AMBIGUOUS confidence in edge property ─────────────────────────────

    #[test]
    fn edge_confidence_ambiguous_in_property() {
        let mut g = Graph::new();
        g.edges.push(edge(3, 4, "maybe", Confidence::Ambiguous));
        let cy = render_cypher(&g);
        assert!(
            cy.contains("confidence: 'AMBIGUOUS'"),
            "AMBIGUOUS confidence property missing: {cy:?}"
        );
    }

    // ── 23: edge src/tgt IDs appear in the MATCH clause ─────────────────────

    #[test]
    fn edge_src_tgt_ids_appear_in_match() {
        let mut g = Graph::new();
        g.edges.push(edge(7, 13, "calls", Confidence::Extracted));
        let cy = render_cypher(&g);
        let stmt = cy
            .lines()
            .find(|l| l.starts_with("MATCH"))
            .expect("no MATCH line");
        assert!(
            stmt.contains("id: 7"),
            "src id 7 missing from MATCH: {stmt:?}"
        );
        assert!(
            stmt.contains("id: 13"),
            "tgt id 13 missing from MATCH: {stmt:?}"
        );
    }

    // ── 24: self-loop edge renders with the same id on both sides ────────────

    #[test]
    fn self_loop_edge_renders() {
        let mut g = Graph::new();
        g.edges.push(edge(5, 5, "recurses", Confidence::Extracted));
        let cy = render_cypher(&g);
        let stmt = cy
            .lines()
            .find(|l| l.starts_with("MATCH"))
            .expect("no MATCH line for self-loop");
        // Both the (a) and (b) slots must reference id 5.
        let count = stmt.matches("id: 5").count();
        assert!(
            count >= 2,
            "self-loop: both ends should be id 5 (found {count}): {stmt:?}"
        );
    }

    // ── 25: N edges → exactly N MATCH/MERGE statements ───────────────────────

    #[test]
    fn n_edges_yields_n_match_statements() {
        let mut g = Graph::new();
        for i in 0..4_u32 {
            g.edges.push(edge(i, i + 1, "e", Confidence::Extracted));
        }
        let cy = render_cypher(&g);
        let count = cy.lines().filter(|l| l.starts_with("MATCH")).count();
        assert_eq!(count, 4, "expected 4 MATCH lines, got {count}:\n{cy}");
    }

    // ── 26: label single quote is escaped as \' ──────────────────────────────

    #[test]
    fn label_single_quote_is_escaped() {
        // "it's" → after cypher_escape → it\'s
        let g = one_node(1, "it's", "f.rs", 1);
        let cy = render_cypher(&g);
        // The full property must contain the escaped form.
        assert!(
            cy.contains(r"label: 'it\'s'"),
            "escaped label not found: {cy:?}"
        );
    }

    // ── 27: label backslash is doubled ───────────────────────────────────────

    #[test]
    fn label_backslash_is_doubled() {
        // r"a\b" is the Rust string `a\b` (3 chars: a, \, b).
        // After cypher_escape: a\\b (4 chars: a, \, \, b).
        let g = one_node(1, r"a\b", "f.rs", 1);
        let cy = render_cypher(&g);
        // "a\\\\b" in a Rust regular literal is the 4-char string a\\b.
        assert!(cy.contains("a\\\\b"), "backslash not doubled: {cy:?}");
    }

    // ── 28: label newline is escaped as \n ───────────────────────────────────

    #[test]
    fn label_newline_is_escaped() {
        let g = one_node(1, "line1\nline2", "f.rs", 1);
        let cy = render_cypher(&g);
        // The escape sequence \n must appear in the output.
        assert!(cy.contains("\\n"), "\\n escape missing: {cy:?}");
        // The raw newline must not appear inside the single-line MERGE statement.
        let stmt = cy
            .lines()
            .find(|l| l.starts_with("MERGE"))
            .expect("no MERGE line");
        assert!(
            !stmt.contains('\n'),
            "raw newline in MERGE statement: {stmt:?}"
        );
    }

    // ── 29: label tab is escaped as \t ───────────────────────────────────────

    #[test]
    fn label_tab_is_escaped() {
        let g = one_node(1, "a\tb", "f.rs", 1);
        let cy = render_cypher(&g);
        assert!(cy.contains("\\t"), "\\t escape missing: {cy:?}");
    }

    // ── 30: label carriage-return is escaped as \r ────────────────────────────

    #[test]
    fn label_carriage_return_is_escaped() {
        let g = one_node(1, "a\rb", "f.rs", 1);
        let cy = render_cypher(&g);
        assert!(cy.contains("\\r"), "\\r escape missing: {cy:?}");
    }

    // ── 31: label Cypher clause injection is neutralised (STRIDE-T gate) ─────
    //
    // `' DETACH DELETE n //` would terminate the string literal then inject a destructive
    // clause if the leading `'` is not escaped.  The injection succeeds only when the output
    // contains two adjacent unescaped quotes: `label: ''` (first closes the literal, second
    // starts the injected statement).  With `cypher_escape`, the leading `'` becomes `\'`,
    // so the property looks like `label: '\' DETACH DELETE n //'` — safe Cypher.

    #[test]
    fn label_clause_injection_neutralised() {
        let evil = "' DETACH DELETE n //";
        let g = one_node(1, evil, "f.rs", 1);
        let cy = render_cypher(&g);
        // The injection pattern: two adjacent unescaped quotes that close then reopen a literal.
        assert!(
            !cy.contains("label: '' DETACH DELETE n //"),
            "injection pattern (premature string termination) found: {cy:?}"
        );
        // The correctly-escaped form must be present: backslash before the leading quote.
        assert!(
            cy.contains(r"label: '\' DETACH DELETE n //"),
            "escaped form missing from output: {cy:?}"
        );
    }

    // ── 32: label with embedded quote yields correct escaped literal ──────────

    #[test]
    fn label_embedded_quote_yields_escaped_literal() {
        // "x'y" → x\'y → the property looks like: label: 'x\'y'
        let g = one_node(1, "x'y", "f.rs", 1);
        let cy = render_cypher(&g);
        assert!(cy.contains(r"x\'y"), "escaped x\\'y form missing: {cy:?}");
    }

    // ── 33: source_file single quote is escaped ───────────────────────────────

    #[test]
    fn source_file_single_quote_is_escaped() {
        let g = one_node(1, "X", "path/to/'evil'.rs", 1);
        let cy = render_cypher(&g);
        // The escaped form must be present.
        assert!(
            cy.contains(r"path/to/\'evil\'"),
            "source_file escaping missing: {cy:?}"
        );
    }

    // ── 34: source_file backslash is doubled ─────────────────────────────────

    #[test]
    fn source_file_backslash_is_doubled() {
        // Windows-style path: src\lib.rs — the backslash must be doubled.
        let g = one_node(1, "X", r"src\lib.rs", 1);
        let cy = render_cypher(&g);
        assert!(
            cy.contains("src\\\\lib.rs"),
            "source_file backslash not doubled: {cy:?}"
        );
    }

    // ── 35: source_file clause injection neutralised ──────────────────────────
    //
    // Same guard as label: the leading `'` must be escaped so the literal cannot be
    // prematurely closed.  Injection succeeds only when `source_file: ''` appears.

    #[test]
    fn source_file_clause_injection_neutralised() {
        let evil = "' DETACH DELETE n //";
        let g = one_node(1, "X", evil, 1);
        let cy = render_cypher(&g);
        // Injection pattern: two adjacent unescaped quotes.
        assert!(
            !cy.contains("source_file: '' DETACH DELETE n //"),
            "source_file injection pattern found: {cy:?}"
        );
        // The correctly-escaped form must be present.
        assert!(
            cy.contains(r"source_file: '\' DETACH DELETE n //"),
            "source_file escaped form missing: {cy:?}"
        );
    }

    // ── 36: relation single quote is escaped ─────────────────────────────────

    #[test]
    fn relation_single_quote_is_escaped() {
        let mut g = Graph::new();
        g.edges
            .push(edge(1, 2, "it's-related", Confidence::Extracted));
        let cy = render_cypher(&g);
        assert!(
            cy.contains(r"it\'s-related"),
            "escaped relation quote missing: {cy:?}"
        );
    }

    // ── 37: relation backslash is doubled ────────────────────────────────────

    #[test]
    fn relation_backslash_is_doubled() {
        let mut g = Graph::new();
        g.edges
            .push(edge(1, 2, r"call\back", Confidence::Extracted));
        let cy = render_cypher(&g);
        assert!(
            cy.contains("call\\\\back"),
            "relation backslash not doubled: {cy:?}"
        );
    }

    // ── 38: relation clause injection neutralised (STRIDE-T gate) ────────────
    //
    // Same guard as label/source_file.  The injection fires only if `relation: ''` appears
    // (premature termination).  With escaping, the output is `relation: '\' DETACH ...`.

    #[test]
    fn relation_clause_injection_neutralised() {
        let evil = "' DETACH DELETE n //";
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, evil, Confidence::Extracted));
        let cy = render_cypher(&g);
        // Injection pattern: two adjacent unescaped quotes that close the literal early.
        assert!(
            !cy.contains("relation: '' DETACH DELETE n //"),
            "relation injection pattern found: {cy:?}"
        );
        // The correctly-escaped form must be present.
        assert!(
            cy.contains(r"relation: '\' DETACH DELETE n //"),
            "relation escaped form missing: {cy:?}"
        );
    }

    // ── 39: hostile relation is never the [:TYPE] identifier ─────────────────
    //
    // Even a relation name that looks like a valid Cypher identifier (e.g. `DETACH_DELETE`)
    // must never appear in the relationship-type position `[:DETACH_DELETE]`.

    #[test]
    fn hostile_relation_never_in_type_identifier() {
        let hostile = "DETACH_DELETE";
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, hostile, Confidence::Extracted));
        let cy = render_cypher(&g);
        assert!(
            !cy.contains(&format!("[:{hostile}")),
            "hostile relation appeared as type identifier: {cy:?}"
        );
        // The fixed :REL type must be present.
        assert!(cy.contains("[:REL"), "[:REL missing: {cy:?}");
        // The relation value must still appear — as a property.
        assert!(
            cy.contains(hostile),
            "relation property value missing: {cy:?}"
        );
    }

    // ── 40: deterministic with nodes ─────────────────────────────────────────

    #[test]
    fn deterministic_with_nodes() {
        let build = || {
            let mut g = Graph::new();
            g.nodes.push(node(1, "Alpha", "a.rs", 10));
            g.nodes.push(node(2, "Beta", "b.rs", 20));
            g
        };
        assert_eq!(
            render_cypher(&build()),
            render_cypher(&build()),
            "render_cypher must be deterministic"
        );
    }

    // ── 41: deterministic with edges ─────────────────────────────────────────

    #[test]
    fn deterministic_with_edges() {
        let build = || {
            let mut g = Graph::new();
            g.nodes.push(node(1, "A", "a.rs", 1));
            g.nodes.push(node(2, "B", "b.rs", 2));
            g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
            g.edges.push(edge(2, 1, "imports", Confidence::Inferred));
            g
        };
        assert_eq!(
            render_cypher(&build()),
            render_cypher(&build()),
            "edge rendering must be deterministic"
        );
    }

    // ── 42: node output order follows graph.nodes order ──────────────────────

    #[test]
    fn node_output_order_follows_graph_order() {
        let mut g = Graph::new();
        // Deliberately unsorted: 3, 1, 2 — output must follow this order, not sort.
        g.nodes.push(node(3, "C", "c.rs", 3));
        g.nodes.push(node(1, "A", "a.rs", 1));
        g.nodes.push(node(2, "B", "b.rs", 2));
        let cy = render_cypher(&g);
        let merges: Vec<&str> = cy.lines().filter(|l| l.starts_with("MERGE")).collect();
        assert_eq!(merges.len(), 3, "expected 3 MERGE lines");
        assert!(
            merges[0].contains("n3"),
            "first MERGE should be n3: {:?}",
            merges[0]
        );
        assert!(
            merges[1].contains("n1"),
            "second MERGE should be n1: {:?}",
            merges[1]
        );
        assert!(
            merges[2].contains("n2"),
            "third MERGE should be n2: {:?}",
            merges[2]
        );
    }

    // ── 43: edge output order follows graph.edges order ──────────────────────

    #[test]
    fn edge_output_order_follows_graph_order() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "first", Confidence::Extracted));
        g.edges.push(edge(3, 4, "second", Confidence::Inferred));
        let cy = render_cypher(&g);
        let matches: Vec<&str> = cy.lines().filter(|l| l.starts_with("MATCH")).collect();
        assert_eq!(matches.len(), 2, "expected 2 MATCH lines");
        assert!(
            matches[0].contains("id: 1") && matches[0].contains("id: 2"),
            "first MATCH should have ids 1,2: {:?}",
            matches[0]
        );
        assert!(
            matches[1].contains("id: 3") && matches[1].contains("id: 4"),
            "second MATCH should have ids 3,4: {:?}",
            matches[1]
        );
    }

    // ── 44: sorted graph renders deterministically ────────────────────────────

    #[test]
    fn sorted_graph_renders_deterministically() {
        let mut g = Graph::new();
        g.nodes.push(node(3, "C", "c.rs", 3));
        g.nodes.push(node(1, "A", "a.rs", 1));
        g.edges.push(edge(3, 1, "calls", Confidence::Extracted));
        let g1 = g.clone().sorted();
        let g2 = g.sorted();
        assert_eq!(
            render_cypher(&g1),
            render_cypher(&g2),
            "sorted graph must render identically"
        );
    }

    // ── 45: large node id renders correctly ──────────────────────────────────

    #[test]
    fn large_node_id_renders_correctly() {
        let g = one_node(u32::MAX, "BigId", "f.rs", 1);
        let cy = render_cypher(&g);
        let max_str = u32::MAX.to_string();
        assert!(cy.contains(&max_str), "u32::MAX id missing: {cy:?}");
    }

    // ── 46: large line number renders correctly ───────────────────────────────

    #[test]
    fn large_line_number_renders_correctly() {
        let g = one_node(1, "X", "f.rs", 999_999);
        let cy = render_cypher(&g);
        assert!(
            cy.contains("line: 999999"),
            "large line number missing: {cy:?}"
        );
    }

    // ── 47: empty label renders as empty string literal ───────────────────────

    #[test]
    fn empty_label_renders_as_empty_literal() {
        let g = one_node(1, "", "f.rs", 1);
        let cy = render_cypher(&g);
        assert!(
            cy.contains("label: ''"),
            "empty label literal missing: {cy:?}"
        );
    }

    // ── 48: empty source_file renders as empty string literal ─────────────────

    #[test]
    fn empty_source_file_renders_as_empty_literal() {
        let g = one_node(1, "X", "", 1);
        let cy = render_cypher(&g);
        assert!(
            cy.contains("source_file: ''"),
            "empty source_file literal missing: {cy:?}"
        );
    }

    // ── 49: empty relation renders as empty string literal ────────────────────

    #[test]
    fn empty_relation_renders_as_empty_literal() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "", Confidence::Extracted));
        let cy = render_cypher(&g);
        assert!(
            cy.contains("relation: ''"),
            "empty relation literal missing: {cy:?}"
        );
    }

    // ── 50: safe unicode label passes through unmodified ─────────────────────

    #[test]
    fn unicode_label_passes_through() {
        let g = one_node(1, "café_Ω_δ", "f.rs", 1);
        let cy = render_cypher(&g);
        assert!(
            cy.contains("café_Ω_δ"),
            "unicode label missing from output: {cy:?}"
        );
    }

    // ── 51: safe unicode source_file passes through unmodified ───────────────

    #[test]
    fn unicode_source_file_passes_through() {
        let g = one_node(1, "X", "src/répertoire/lib.rs", 1);
        let cy = render_cypher(&g);
        assert!(
            cy.contains("src/répertoire/lib.rs"),
            "unicode path missing from output: {cy:?}"
        );
    }

    // ── 52: all three confidence variants render correctly ────────────────────

    #[test]
    fn all_confidence_variants_render() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "a", Confidence::Extracted));
        g.edges.push(edge(2, 3, "b", Confidence::Inferred));
        g.edges.push(edge(3, 4, "c", Confidence::Ambiguous));
        let cy = render_cypher(&g);
        assert!(cy.contains("'EXTRACTED'"), "EXTRACTED missing: {cy:?}");
        assert!(cy.contains("'INFERRED'"), "INFERRED missing: {cy:?}");
        assert!(cy.contains("'AMBIGUOUS'"), "AMBIGUOUS missing: {cy:?}");
    }

    // ── 53: relation value appears in property map with correct quoting ───────

    #[test]
    fn relation_value_in_property_map() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "imports", Confidence::Extracted));
        let cy = render_cypher(&g);
        assert!(
            cy.contains("relation: 'imports'"),
            "relation property missing: {cy:?}"
        );
    }

    // ── 54: node variable uses the n{id} naming pattern ──────────────────────

    #[test]
    fn node_variable_uses_n_prefix_with_id() {
        let g = one_node(17, "X", "f.rs", 1);
        let cy = render_cypher(&g);
        assert!(
            cy.contains("(n17:Symbol"),
            "node variable n17 missing: {cy:?}"
        );
    }

    // ── 55: multiple nodes all produce distinct, correct MERGE statements ─────

    #[test]
    fn multiple_nodes_have_distinct_merges() {
        let mut g = Graph::new();
        g.nodes.push(node(10, "Ten", "a.rs", 1));
        g.nodes.push(node(20, "Twenty", "b.rs", 2));
        g.nodes.push(node(30, "Thirty", "c.rs", 3));
        let cy = render_cypher(&g);
        assert!(cy.contains("(n10:Symbol"), "n10 MERGE missing: {cy:?}");
        assert!(cy.contains("(n20:Symbol"), "n20 MERGE missing: {cy:?}");
        assert!(cy.contains("(n30:Symbol"), "n30 MERGE missing: {cy:?}");
    }

    // ── 56: line 1 renders correctly (lowest valid value) ────────────────────

    #[test]
    fn line_one_renders_correctly() {
        let g = one_node(1, "Root", "lib.rs", 1);
        let cy = render_cypher(&g);
        assert!(cy.contains("line: 1"), "line: 1 missing: {cy:?}");
    }

    // ── 57: mixed graph produces correct total count of statements ────────────

    #[test]
    fn mixed_graph_correct_statement_counts() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "A", "a.rs", 1));
        g.nodes.push(node(2, "B", "b.rs", 2));
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        g.edges.push(edge(2, 1, "uses", Confidence::Inferred));
        let cy = render_cypher(&g);
        let merges = cy.lines().filter(|l| l.starts_with("MERGE")).count();
        let matches = cy.lines().filter(|l| l.starts_with("MATCH")).count();
        assert_eq!(merges, 2, "expected 2 node MERGEs: {cy}");
        assert_eq!(matches, 2, "expected 2 edge MATCHes: {cy}");
    }

    // ── 58: C0 control characters (below 0x20) are dropped from labels ────────

    #[test]
    fn c0_control_chars_dropped_from_label() {
        // cypher_escape drops C0 controls (< 0x20) except the whitespace ones it rewrites.
        // U+0001 (SOH) must be silently dropped.
        let g = one_node(1, "a\x01b", "f.rs", 1);
        let cy = render_cypher(&g);
        let stmt = cy
            .lines()
            .find(|l| l.starts_with("MERGE"))
            .expect("no MERGE");
        assert!(
            !stmt.contains('\x01'),
            "C0 control char leaked into output: {stmt:?}"
        );
        // The remaining chars a and b must still be present in the label.
        assert!(cy.contains("label: 'ab'"), "surrounding chars lost: {cy:?}");
    }

    // ── 59: nodes-only graph has no MATCH lines ──────────────────────────────

    #[test]
    fn secret_patterns_are_redacted_before_cypher_escaping() {
        let mut g = Graph::new();
        g.nodes
            .push(node(1, "api_key_assignment_refused", "src/api_key.rs", 1));
        g.edges.push(edge(
            1,
            1,
            "Authorization: Bearer token",
            Confidence::Extracted,
        ));
        let cypher = render_cypher(&g);
        assert!(!cypher.contains("api_key_assignment_refused"));
        assert!(!cypher.contains("src/api_key.rs"));
        assert!(!cypher.contains("Authorization: Bearer token"));
        assert!(cypher.contains("[REDACTED:api_key]"));
        assert!(cypher.contains("[REDACTED:bearer_token]"));
    }

    #[test]
    fn distinct_redacted_relations_keep_distinct_merge_identity() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "A", "a.rs", 1));
        g.nodes.push(node(2, "B", "b.rs", 1));
        g.edges
            .push(edge(1, 2, "api_key=alpha", Confidence::Extracted));
        g.edges
            .push(edge(1, 2, "api_key=beta", Confidence::Extracted));

        let cypher = render_cypher(&g);
        let relationships: Vec<&str> = cypher
            .lines()
            .filter(|line| line.starts_with("MATCH"))
            .collect();
        assert_eq!(relationships.len(), 2);
        assert_ne!(relationships[0], relationships[1]);
        assert!(relationships
            .iter()
            .all(|line| line.contains("[REDACTED:api_key]#e")));
    }

    #[test]
    fn nodes_only_graph_has_no_match_lines() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "A", "a.rs", 1));
        g.nodes.push(node(2, "B", "b.rs", 2));
        let cy = render_cypher(&g);
        let match_count = cy.lines().filter(|l| l.starts_with("MATCH")).count();
        assert_eq!(
            match_count, 0,
            "nodes-only graph must have 0 MATCH lines: {cy}"
        );
    }

    // ── 60: edges-only graph has no node MERGE lines ─────────────────────────

    #[test]
    fn edges_only_graph_has_no_node_merge_lines() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        let cy = render_cypher(&g);
        // All MERGE lines must be inside MATCH statements (edge merges), not standalone.
        let standalone_merges = cy.lines().filter(|l| l.starts_with("MERGE")).count();
        assert_eq!(
            standalone_merges, 0,
            "edges-only graph must have 0 standalone MERGE lines: {cy}"
        );
    }
}
