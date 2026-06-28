//! `GraphML` exporter — `Gephi`/`yEd`-compatible XML (the graphify `--graphml` analogue).
//!
//! [`render_graphml`] produces a standards-conformant `GraphML` document with `<key>` declarations
//! for node and edge attributes, one `<node>` per graph node, and one `<edge>` per graph edge.
//! Every attacker-influenced string (label, `source_file`, relation) is routed through
//! [`crate::escape::xml_escape`] before embedding in the XML (STRIDE-T injection guard).

use std::fmt::Write as FmtWrite;

use habitat_graph_core::{sanitize_label, Graph};

use crate::escape::xml_escape;

/// `GraphML` attribute key identifier for a node's `label` field.
const KEY_LABEL: &str = "d_label";
/// `GraphML` attribute key identifier for a node's `source_file` field.
const KEY_SOURCE_FILE: &str = "d_source_file";
/// `GraphML` attribute key identifier for an edge's `relation` field.
const KEY_RELATION: &str = "d_relation";
/// `GraphML` attribute key identifier for an edge's `confidence` field.
const KEY_CONFIDENCE: &str = "d_confidence";

/// Renders `graph` as a `Gephi`/`yEd`-importable `GraphML` XML document.
///
/// ## Document structure
///
/// ```text
/// <?xml version="1.0" encoding="UTF-8"?>
/// <graphml xmlns="http://graphml.graphdrawing.org/xmlns"
///          xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
///          xsi:schemaLocation="…">
///   <key id="d_label"       for="node" attr.name="label"       attr.type="string"/>
///   <key id="d_source_file" for="node" attr.name="source_file" attr.type="string"/>
///   <key id="d_relation"    for="edge" attr.name="relation"    attr.type="string"/>
///   <key id="d_confidence"  for="edge" attr.name="confidence"  attr.type="string"/>
///   <graph id="G" edgedefault="directed">
///     <node id="n{id}">
///       <data key="d_label">{xml_escaped label}</data>
///       <data key="d_source_file">{xml_escaped source_file}</data>
///     </node>
///     …
///     <edge source="n{src}" target="n{tgt}">
///       <data key="d_relation">{xml_escaped relation}</data>
///       <data key="d_confidence">{confidence}</data>
///     </edge>
///     …
///   </graph>
/// </graphml>
/// ```
///
/// ## Determinism (R4)
///
/// Output is byte-identical across runs: nodes and edges are emitted in the order they appear in
/// `graph.nodes` / `graph.edges`. Call [`Graph::sorted`](habitat_graph_core::Graph::sorted) first
/// to obtain the canonical ascending-id ordering required by the git merge driver.
///
/// ## Security (STRIDE-T)
///
/// Every attacker-influenced string is passed through [`xml_escape`](crate::escape::xml_escape)
/// before embedding. Node labels and edge relations are additionally preprocessed with
/// [`sanitize_label`](habitat_graph_core::sanitize_label), which strips C0 control characters and
/// caps length at 256 code points. A label like `</node><evil>` or a path containing `&` cannot
/// break out of its enclosing XML element.
///
/// The function is infallible: `write!` on `String` only panics on OOM.
#[allow(clippy::module_name_repetitions)]
#[must_use]
pub fn render_graphml(graph: &Graph) -> String {
    let mut out = String::new();

    // XML declaration.
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");

    // Root <graphml> element with canonical namespace + schema-location attributes.
    out.push_str("<graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\"\n");
    out.push_str("         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\n");
    out.push_str(
        "         xsi:schemaLocation=\"http://graphml.graphdrawing.org/xmlns\n",
    );
    out.push_str(
        "           http://graphml.graphdrawing.org/xmlns/1.0/graphml.xsd\">\n",
    );

    // Key declarations — node attributes (label, source_file).
    let _ = writeln!(
        out,
        "  <key id=\"{KEY_LABEL}\" for=\"node\" attr.name=\"label\" attr.type=\"string\"/>"
    );
    let _ = writeln!(
        out,
        "  <key id=\"{KEY_SOURCE_FILE}\" for=\"node\" attr.name=\"source_file\" attr.type=\"string\"/>"
    );

    // Key declarations — edge attributes (relation, confidence).
    let _ = writeln!(
        out,
        "  <key id=\"{KEY_RELATION}\" for=\"edge\" attr.name=\"relation\" attr.type=\"string\"/>"
    );
    let _ = writeln!(
        out,
        "  <key id=\"{KEY_CONFIDENCE}\" for=\"edge\" attr.name=\"confidence\" attr.type=\"string\"/>"
    );

    // <graph> container.
    out.push_str("  <graph id=\"G\" edgedefault=\"directed\">\n");

    // One <node> element per graph node; deterministic (follows graph.nodes order).
    for node in &graph.nodes {
        let id = node.id.get();
        // sanitize_label strips C0 controls / caps length; xml_escape escapes metacharacters.
        let label = xml_escape(&sanitize_label(&node.label));
        let source_file = xml_escape(&node.source_file);

        let _ = writeln!(out, "    <node id=\"n{id}\">");
        let _ = writeln!(out, "      <data key=\"{KEY_LABEL}\">{label}</data>");
        let _ = writeln!(
            out,
            "      <data key=\"{KEY_SOURCE_FILE}\">{source_file}</data>"
        );
        out.push_str("    </node>\n");
    }

    // One <edge> element per graph edge; deterministic (follows graph.edges order).
    for edge in &graph.edges {
        let src = edge.source.get();
        let tgt = edge.target.get();
        let relation = xml_escape(&sanitize_label(&edge.relation));
        // Confidence values are always ASCII uppercase identifiers; xml_escape is a no-op here
        // but is applied for belt-and-suspenders STRIDE-T compliance.
        let confidence = xml_escape(edge.confidence.as_str());

        let _ = writeln!(out, "    <edge source=\"n{src}\" target=\"n{tgt}\">");
        let _ = writeln!(out, "      <data key=\"{KEY_RELATION}\">{relation}</data>");
        let _ = writeln!(
            out,
            "      <data key=\"{KEY_CONFIDENCE}\">{confidence}</data>"
        );
        out.push_str("    </edge>\n");
    }

    out.push_str("  </graph>\n");
    out.push_str("</graphml>\n");
    out
}

#[cfg(test)]
mod tests {
    use habitat_graph_core::{Confidence, Edge, Graph, Node, NodeId, Span};

    use super::render_graphml;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn span() -> Span {
        Span::new(0, 10, 1, 1)
    }

    fn node(id: u32, label: &str, file: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: file.to_owned(),
            source_location: span(),
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

    /// Counts non-overlapping occurrences of `needle` in `haystack`.
    fn count_occurrences(haystack: &str, needle: &str) -> usize {
        let mut count = 0_usize;
        let mut start = 0_usize;
        while let Some(pos) = haystack[start..].find(needle) {
            count += 1;
            start += pos + needle.len();
        }
        count
    }

    // ── 1: XML declaration present ───────────────────────────────────────────

    #[test]
    fn empty_graph_has_xml_declaration() {
        let xml = render_graphml(&Graph::new());
        assert!(
            xml.contains("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"),
            "XML declaration missing: {xml}"
        );
    }

    // ── 2: <graphml> opening tag present ────────────────────────────────────

    #[test]
    fn empty_graph_has_graphml_opening_tag() {
        let xml = render_graphml(&Graph::new());
        assert!(xml.contains("<graphml "), "opening <graphml> tag missing");
    }

    // ── 3: </graphml> closing tag present ───────────────────────────────────

    #[test]
    fn empty_graph_has_graphml_closing_tag() {
        let xml = render_graphml(&Graph::new());
        assert!(xml.contains("</graphml>"), "closing </graphml> tag missing");
    }

    // ── 4: directed attribute on <graph> ────────────────────────────────────

    #[test]
    fn empty_graph_has_directed_attribute() {
        let xml = render_graphml(&Graph::new());
        assert!(
            xml.contains("edgedefault=\"directed\""),
            "edgedefault=\"directed\" missing: {xml}"
        );
    }

    // ── 5: <graph> container present ────────────────────────────────────────

    #[test]
    fn empty_graph_has_graph_element() {
        let xml = render_graphml(&Graph::new());
        assert!(xml.contains("<graph "), "<graph> element missing");
        assert!(xml.contains("</graph>"), "</graph> closing tag missing");
    }

    // ── 6: graphml namespace is canonical ───────────────────────────────────

    #[test]
    fn graphml_namespace_is_correct() {
        let xml = render_graphml(&Graph::new());
        assert!(
            xml.contains("xmlns=\"http://graphml.graphdrawing.org/xmlns\""),
            "canonical GraphML namespace missing: {xml}"
        );
    }

    // ── 7: no <node> elements for empty graph ────────────────────────────────

    #[test]
    fn empty_graph_has_no_node_elements() {
        let xml = render_graphml(&Graph::new());
        assert!(
            !xml.contains("<node "),
            "empty graph must have no <node> elements: {xml}"
        );
    }

    // ── 8: no <edge> elements for empty graph ────────────────────────────────

    #[test]
    fn empty_graph_has_no_edge_elements() {
        let xml = render_graphml(&Graph::new());
        assert!(
            !xml.contains("<edge "),
            "empty graph must have no <edge> elements: {xml}"
        );
    }

    // ── 9: key d_label declared for node ────────────────────────────────────

    #[test]
    fn key_label_declared_for_node() {
        let xml = render_graphml(&Graph::new());
        assert!(
            xml.contains("id=\"d_label\" for=\"node\""),
            "key d_label for node missing: {xml}"
        );
    }

    // ── 10: key d_source_file declared for node ──────────────────────────────

    #[test]
    fn key_source_file_declared_for_node() {
        let xml = render_graphml(&Graph::new());
        assert!(
            xml.contains("id=\"d_source_file\" for=\"node\""),
            "key d_source_file for node missing: {xml}"
        );
    }

    // ── 11: key d_relation declared for edge ─────────────────────────────────

    #[test]
    fn key_relation_declared_for_edge() {
        let xml = render_graphml(&Graph::new());
        assert!(
            xml.contains("id=\"d_relation\" for=\"edge\""),
            "key d_relation for edge missing: {xml}"
        );
    }

    // ── 12: key d_confidence declared for edge ───────────────────────────────

    #[test]
    fn key_confidence_declared_for_edge() {
        let xml = render_graphml(&Graph::new());
        assert!(
            xml.contains("id=\"d_confidence\" for=\"edge\""),
            "key d_confidence for edge missing: {xml}"
        );
    }

    // ── 13: key attr.type="string" for d_label ──────────────────────────────

    #[test]
    fn key_label_attr_type_is_string() {
        let xml = render_graphml(&Graph::new());
        // Verify that the d_label key has attr.type="string"
        assert!(
            xml.contains("attr.name=\"label\" attr.type=\"string\""),
            "d_label key must have attr.type=string: {xml}"
        );
    }

    // ── 14: key attr.type="string" for d_source_file ────────────────────────

    #[test]
    fn key_source_file_attr_type_is_string() {
        let xml = render_graphml(&Graph::new());
        assert!(
            xml.contains("attr.name=\"source_file\" attr.type=\"string\""),
            "d_source_file key must have attr.type=string: {xml}"
        );
    }

    // ── 15: key attr.type="string" for d_relation ───────────────────────────

    #[test]
    fn key_relation_attr_type_is_string() {
        let xml = render_graphml(&Graph::new());
        assert!(
            xml.contains("attr.name=\"relation\" attr.type=\"string\""),
            "d_relation key must have attr.type=string: {xml}"
        );
    }

    // ── 16: key attr.type="string" for d_confidence ─────────────────────────

    #[test]
    fn key_confidence_attr_type_is_string() {
        let xml = render_graphml(&Graph::new());
        assert!(
            xml.contains("attr.name=\"confidence\" attr.type=\"string\""),
            "d_confidence key must have attr.type=string: {xml}"
        );
    }

    // ── 17: single node → exactly one <node> element ────────────────────────

    #[test]
    fn single_node_produces_one_node_element() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "Alpha", "src/alpha.rs"));
        let xml = render_graphml(&g);
        assert_eq!(
            count_occurrences(&xml, "<node "),
            1,
            "expected exactly 1 <node> element: {xml}"
        );
    }

    // ── 18: node id uses "n{id}" format ─────────────────────────────────────

    #[test]
    fn single_node_id_uses_n_prefix() {
        let mut g = Graph::new();
        g.nodes.push(node(42, "Thing", "t.rs"));
        let xml = render_graphml(&g);
        assert!(
            xml.contains("id=\"n42\""),
            "node id=\"n42\" missing: {xml}"
        );
    }

    // ── 19: node d_label data element present ───────────────────────────────

    #[test]
    fn single_node_label_data_key_correct() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "MyFunc", "src/lib.rs"));
        let xml = render_graphml(&g);
        assert!(
            xml.contains("key=\"d_label\""),
            "node data key d_label missing: {xml}"
        );
    }

    // ── 20: node d_source_file data element present ──────────────────────────

    #[test]
    fn single_node_source_file_data_key_correct() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "MyFunc", "src/lib.rs"));
        let xml = render_graphml(&g);
        assert!(
            xml.contains("key=\"d_source_file\""),
            "node data key d_source_file missing: {xml}"
        );
    }

    // ── 21: node label value appears in <data> content ──────────────────────

    #[test]
    fn single_node_label_value_correct() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "MyFunc", "src/lib.rs"));
        let xml = render_graphml(&g);
        assert!(
            xml.contains(">MyFunc<"),
            "label value 'MyFunc' missing from <data>: {xml}"
        );
    }

    // ── 22: node source_file value appears in <data> content ────────────────

    #[test]
    fn single_node_source_file_value_correct() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "MyFunc", "src/lib.rs"));
        let xml = render_graphml(&g);
        assert!(
            xml.contains(">src/lib.rs<"),
            "source_file value missing from <data>: {xml}"
        );
    }

    // ── 23: node count matches graph.nodes.len() ─────────────────────────────

    #[test]
    fn node_count_matches_graph() {
        let mut g = Graph::new();
        for i in 0..7_u32 {
            g.nodes.push(node(i, &format!("n{i}"), "f.rs"));
        }
        let xml = render_graphml(&g);
        assert_eq!(
            count_occurrences(&xml, "<node "),
            7,
            "expected 7 <node> elements: {xml}"
        );
    }

    // ── 24: edge count matches graph.edges.len() ─────────────────────────────

    #[test]
    fn edge_count_matches_graph() {
        let mut g = Graph::new();
        for i in 0..5_u32 {
            g.edges.push(edge(i, i + 1, "calls", Confidence::Extracted));
        }
        let xml = render_graphml(&g);
        assert_eq!(
            count_occurrences(&xml, "<edge "),
            5,
            "expected 5 <edge> elements: {xml}"
        );
    }

    // ── 25: both nodes appear when two are added ─────────────────────────────

    #[test]
    fn two_nodes_both_have_node_elements() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "A", "a.rs"));
        g.nodes.push(node(2, "B", "b.rs"));
        let xml = render_graphml(&g);
        assert!(xml.contains("id=\"n1\""), "node n1 missing: {xml}");
        assert!(xml.contains("id=\"n2\""), "node n2 missing: {xml}");
    }

    // ── 26: edge source attribute correct ───────────────────────────────────

    #[test]
    fn edge_source_attribute_correct() {
        let mut g = Graph::new();
        g.edges.push(edge(3, 7, "calls", Confidence::Extracted));
        let xml = render_graphml(&g);
        assert!(
            xml.contains("source=\"n3\""),
            "edge source=\"n3\" missing: {xml}"
        );
    }

    // ── 27: edge target attribute correct ───────────────────────────────────

    #[test]
    fn edge_target_attribute_correct() {
        let mut g = Graph::new();
        g.edges.push(edge(3, 7, "calls", Confidence::Extracted));
        let xml = render_graphml(&g);
        assert!(
            xml.contains("target=\"n7\""),
            "edge target=\"n7\" missing: {xml}"
        );
    }

    // ── 28: edge relation data correct ──────────────────────────────────────

    #[test]
    fn edge_relation_data_correct() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "imports", Confidence::Inferred));
        let xml = render_graphml(&g);
        assert!(
            xml.contains(">imports<"),
            "relation value 'imports' missing from <data>: {xml}"
        );
    }

    // ── 29: EXTRACTED confidence renders correctly ───────────────────────────

    #[test]
    fn edge_confidence_extracted_value() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        let xml = render_graphml(&g);
        assert!(
            xml.contains(">EXTRACTED<"),
            "EXTRACTED confidence missing from <data>: {xml}"
        );
    }

    // ── 30: INFERRED confidence renders correctly ────────────────────────────

    #[test]
    fn edge_confidence_inferred_value() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "uses", Confidence::Inferred));
        let xml = render_graphml(&g);
        assert!(
            xml.contains(">INFERRED<"),
            "INFERRED confidence missing from <data>: {xml}"
        );
    }

    // ── 31: AMBIGUOUS confidence renders correctly ───────────────────────────

    #[test]
    fn edge_confidence_ambiguous_value() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "maybe", Confidence::Ambiguous));
        let xml = render_graphml(&g);
        assert!(
            xml.contains(">AMBIGUOUS<"),
            "AMBIGUOUS confidence missing from <data>: {xml}"
        );
    }

    // ── 32: all three confidence values produce distinct strings ─────────────

    #[test]
    fn all_three_confidence_values_distinct() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "a", Confidence::Extracted));
        g.edges.push(edge(2, 3, "b", Confidence::Inferred));
        g.edges.push(edge(3, 4, "c", Confidence::Ambiguous));
        let xml = render_graphml(&g);
        assert!(xml.contains(">EXTRACTED<"), "EXTRACTED missing");
        assert!(xml.contains(">INFERRED<"), "INFERRED missing");
        assert!(xml.contains(">AMBIGUOUS<"), "AMBIGUOUS missing");
    }

    // ── 33: self-loop edge renders correctly ─────────────────────────────────

    #[test]
    fn self_loop_edge_renders_correctly() {
        let mut g = Graph::new();
        g.nodes.push(node(5, "Recursive", "r.rs"));
        g.edges.push(edge(5, 5, "recurses", Confidence::Extracted));
        let xml = render_graphml(&g);
        assert!(
            xml.contains("source=\"n5\" target=\"n5\""),
            "self-loop source=target missing: {xml}"
        );
    }

    // ── 34: node id zero renders as "n0" ────────────────────────────────────

    #[test]
    fn node_id_zero_format() {
        let mut g = Graph::new();
        g.nodes.push(node(0, "Root", "root.rs"));
        let xml = render_graphml(&g);
        assert!(xml.contains("id=\"n0\""), "id=\"n0\" missing: {xml}");
    }

    // ── 35: large node id renders correctly ──────────────────────────────────

    #[test]
    fn large_node_id_format() {
        let id = u32::MAX - 1;
        let mut g = Graph::new();
        g.nodes.push(node(id, "Huge", "h.rs"));
        let xml = render_graphml(&g);
        let expected = format!("id=\"n{id}\"");
        assert!(xml.contains(&expected), "large node id missing: {xml}");
    }

    // ── SECURITY: XML tag injection in label ─────────────────────────────────

    // ── 36: closing tag + opening tag in label cannot break XML structure ────

    #[test]
    fn xml_tag_injection_in_label_neutralised() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "</node><evil>", "f.rs"));
        let xml = render_graphml(&g);
        // Raw < and > must not appear inside the data element's text content.
        // After the XML declaration, the only < characters must be tag openers.
        // Check: no raw </node> appears after the declarations.
        assert!(
            !xml.contains("</node><evil>"),
            "raw injection string must not appear in output: {xml}"
        );
        // The escaped form must be present.
        assert!(
            xml.contains("&lt;/node&gt;&lt;evil&gt;"),
            "escaped injection missing: {xml}"
        );
    }

    // ── 37: full script tag injection in label ───────────────────────────────

    #[test]
    fn script_tag_injection_in_label() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "</text><script>alert(1)</script>", "f.rs"));
        let xml = render_graphml(&g);
        assert!(
            !xml.contains("</text>"),
            "raw </text> must not appear: {xml}"
        );
        assert!(
            !xml.contains("<script>"),
            "raw <script> must not appear: {xml}"
        );
        assert!(xml.contains("&lt;"), "escaped &lt; must appear: {xml}");
        assert!(xml.contains("&gt;"), "escaped &gt; must appear: {xml}");
    }

    // ── 38: ampersand in label neutralised ───────────────────────────────────

    #[test]
    fn ampersand_in_label_neutralised() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "foo & bar", "f.rs"));
        let xml = render_graphml(&g);
        // Inside <data> text, raw & is illegal XML — it must be escaped.
        // We look for &amp; as the escaped form.
        assert!(
            xml.contains("foo &amp; bar"),
            "ampersand must be &amp;-escaped: {xml}"
        );
    }

    // ── 39: double quote in label neutralised ───────────────────────────────

    #[test]
    fn double_quote_in_label_neutralised() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "say \"hello\"", "f.rs"));
        let xml = render_graphml(&g);
        assert!(
            xml.contains("say &quot;hello&quot;"),
            "double quote must be &quot;-escaped: {xml}"
        );
    }

    // ── 40: single quote in label neutralised ───────────────────────────────

    #[test]
    fn single_quote_in_label_neutralised() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "it's fine", "f.rs"));
        let xml = render_graphml(&g);
        assert!(
            xml.contains("it&apos;s fine"),
            "single quote must be &apos;-escaped: {xml}"
        );
    }

    // ── 41: XML tag injection in source_file ─────────────────────────────────

    #[test]
    fn xml_tag_injection_in_source_file_neutralised() {
        let mut g = Graph::new();
        g.nodes
            .push(node(1, "X", "</data><evil attr=\"x\">path.rs</evil>"));
        let xml = render_graphml(&g);
        assert!(
            !xml.contains("</data><evil"),
            "raw injection in source_file must not appear: {xml}"
        );
        assert!(
            xml.contains("&lt;/data&gt;"),
            "source_file injection must be escaped: {xml}"
        );
    }

    // ── 42: ampersand in source_file neutralised ─────────────────────────────

    #[test]
    fn ampersand_in_source_file_neutralised() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "X", "src/a&b.rs"));
        let xml = render_graphml(&g);
        assert!(
            xml.contains("src/a&amp;b.rs"),
            "ampersand in source_file must be escaped: {xml}"
        );
    }

    // ── 43: XML tag injection in relation ────────────────────────────────────

    #[test]
    fn xml_tag_injection_in_relation_neutralised() {
        let mut g = Graph::new();
        g.edges.push(edge(
            1,
            2,
            "</data><inject/>",
            Confidence::Extracted,
        ));
        let xml = render_graphml(&g);
        assert!(
            !xml.contains("</data><inject/>"),
            "raw injection in relation must not appear: {xml}"
        );
        assert!(
            xml.contains("&lt;/data&gt;"),
            "relation injection must be escaped: {xml}"
        );
    }

    // ── 44: full five-metacharacter injection in label ────────────────────────

    #[test]
    fn combined_xml_metacharacters_in_label() {
        let label = "&<>\"'";
        let mut g = Graph::new();
        g.nodes.push(node(1, label, "f.rs"));
        let xml = render_graphml(&g);
        // Each XML metacharacter must appear as its entity — not raw.
        assert!(xml.contains("&amp;"), "& must be escaped as &amp;: {xml}");
        assert!(xml.contains("&lt;"), "< must be escaped as &lt;: {xml}");
        assert!(xml.contains("&gt;"), "> must be escaped as &gt;: {xml}");
        assert!(xml.contains("&quot;"), "\" must be escaped as &quot;: {xml}");
        assert!(xml.contains("&apos;"), "' must be escaped as &apos;: {xml}");
        // The raw unescaped label must not appear as a contiguous substring.
        assert!(
            !xml.contains(label),
            "raw injection label must not appear unescaped: {xml}"
        );
        // Crucially: no raw < inside the <data> text content (after the opening tag).
        let data_text = xml
            .split("<data key=\"d_label\">")
            .nth(1)
            .and_then(|s| s.split("</data>").next())
            .unwrap_or("");
        // The data text must be "&amp;&lt;&gt;&quot;&apos;" — no raw < or bare > outside entities.
        assert!(
            !data_text.contains('<'),
            "raw < must not appear inside data text: {data_text}"
        );
    }

    // ── 45: C0 control chars in label are dropped ────────────────────────────

    #[test]
    fn c0_control_in_label_dropped() {
        let mut g = Graph::new();
        // NUL (U+0000) and BEL (U+0007) are illegal in XML 1.0 and must be dropped.
        g.nodes.push(node(1, "fn\x00bad\x07name", "f.rs"));
        let xml = render_graphml(&g);
        assert!(
            !xml.contains('\x00'),
            "NUL control char must be dropped: {xml:?}"
        );
        assert!(
            !xml.contains('\x07'),
            "BEL control char must be dropped: {xml:?}"
        );
        // Surviving text (legal chars) must still be present.
        assert!(
            xml.contains("fnbadname"),
            "remaining label chars must survive: {xml}"
        );
    }

    // ── 46: C0 control chars in source_file are dropped ──────────────────────

    #[test]
    fn c0_control_in_source_file_dropped() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "X", "src/\x01evil\x02.rs"));
        let xml = render_graphml(&g);
        assert!(
            !xml.contains('\x01'),
            "SOH control char in source_file must be dropped: {xml:?}"
        );
        assert!(
            !xml.contains('\x02'),
            "STX control char in source_file must be dropped: {xml:?}"
        );
    }

    // ── 47: C0 control chars in relation are dropped ─────────────────────────

    #[test]
    fn c0_control_in_relation_dropped() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "cal\x03ls", Confidence::Extracted));
        let xml = render_graphml(&g);
        assert!(
            !xml.contains('\x03'),
            "ETX in relation must be dropped: {xml:?}"
        );
        assert!(xml.contains(">calls<"), "relation text must survive: {xml}");
    }

    // ── 48: Cypher/SQL injection string in label is XML-escaped ──────────────

    #[test]
    fn cypher_injection_string_in_label_is_xml_escaped() {
        // "' DETACH DELETE n //" — a Cypher injection payload.
        // In GraphML context it's an XML embedding; single-quote must be &apos;.
        let mut g = Graph::new();
        g.nodes.push(node(1, "' DETACH DELETE n //", "f.rs"));
        let xml = render_graphml(&g);
        // The data text must not start with a raw single quote.
        let data_text = xml
            .split("<data key=\"d_label\">")
            .nth(1)
            .and_then(|s| s.split("</data>").next())
            .unwrap_or("");
        assert!(
            !data_text.starts_with('\''),
            "raw leading quote in data: {data_text}"
        );
        assert!(
            data_text.starts_with("&apos;"),
            "quote must be &apos;-escaped: {data_text}"
        );
    }

    // ── 49: deterministic — empty graph same output twice ────────────────────

    #[test]
    fn deterministic_empty_graph() {
        let out1 = render_graphml(&Graph::new());
        let out2 = render_graphml(&Graph::new());
        assert_eq!(out1, out2, "render_graphml must be deterministic on empty graph");
    }

    // ── 50: deterministic — single-node graph ────────────────────────────────

    #[test]
    fn deterministic_single_node() {
        let build = || {
            let mut g = Graph::new();
            g.nodes.push(node(1, "Alpha", "a.rs"));
            g
        };
        assert_eq!(
            render_graphml(&build()),
            render_graphml(&build()),
            "render_graphml must be byte-identical for same single-node graph"
        );
    }

    // ── 51: deterministic — multi-node, multi-edge graph ─────────────────────

    #[test]
    fn deterministic_with_multiple_nodes_and_edges() {
        let build = || {
            let mut g = Graph::new();
            g.nodes.push(node(1, "Alpha", "a.rs"));
            g.nodes.push(node(2, "Beta", "b.rs"));
            g.nodes.push(node(3, "Gamma", "c.rs"));
            g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
            g.edges.push(edge(2, 3, "imports", Confidence::Inferred));
            g.edges.push(edge(3, 1, "depends", Confidence::Ambiguous));
            g
        };
        assert_eq!(
            render_graphml(&build()),
            render_graphml(&build()),
            "render_graphml must be deterministic for multi-node graph"
        );
    }

    // ── 52: output starts with XML declaration ───────────────────────────────

    #[test]
    fn output_starts_with_xml_declaration() {
        let xml = render_graphml(&Graph::new());
        assert!(
            xml.starts_with("<?xml"),
            "output must start with <?xml: {xml}"
        );
    }

    // ── 53: output ends with </graphml> newline ──────────────────────────────

    #[test]
    fn output_ends_with_closing_graphml_tag() {
        let xml = render_graphml(&Graph::new());
        assert!(
            xml.trim_end().ends_with("</graphml>"),
            "output must end with </graphml>: {xml}"
        );
    }

    // ── 54: key declarations appear before <graph> element ───────────────────

    #[test]
    fn keys_appear_before_graph_element() {
        let xml = render_graphml(&Graph::new());
        let key_pos = xml.find("<key ").expect("<key> must appear in output");
        let graph_pos = xml.find("<graph ").expect("<graph> must appear in output");
        assert!(
            key_pos < graph_pos,
            "key declarations must precede <graph>: key@{key_pos} graph@{graph_pos}"
        );
    }

    // ── 55: nodes appear inside <graph> element ──────────────────────────────

    #[test]
    fn nodes_appear_inside_graph_element() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "X", "x.rs"));
        let xml = render_graphml(&g);
        let graph_open = xml.find("<graph ").expect("<graph> must appear");
        let graph_close = xml.find("</graph>").expect("</graph> must appear");
        let node_pos = xml.find("<node ").expect("<node> must appear");
        assert!(
            node_pos > graph_open && node_pos < graph_close,
            "<node> must be inside <graph>: graph@{graph_open}..{graph_close}, node@{node_pos}"
        );
    }

    // ── 56: edges appear inside <graph> element ───────────────────────────────

    #[test]
    fn edges_appear_inside_graph_element() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        let xml = render_graphml(&g);
        let graph_open = xml.find("<graph ").expect("<graph> must appear");
        let graph_close = xml.find("</graph>").expect("</graph> must appear");
        let edge_pos = xml.find("<edge ").expect("<edge> must appear");
        assert!(
            edge_pos > graph_open && edge_pos < graph_close,
            "<edge> must be inside <graph>: graph@{graph_open}..{graph_close}, edge@{edge_pos}"
        );
    }

    // ── 57: unicode label is preserved unchanged ──────────────────────────────

    #[test]
    fn unicode_label_preserved() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "café — Ω — 日本語", "f.rs"));
        let xml = render_graphml(&g);
        assert!(
            xml.contains("café — Ω — 日本語"),
            "unicode label must pass through unchanged: {xml}"
        );
    }

    // ── 58: ampersand in source_file not double-escaped ──────────────────────

    #[test]
    fn ampersand_in_source_file_not_double_escaped() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "X", "src/a&b.rs"));
        let xml = render_graphml(&g);
        // Must contain exactly &amp; — not &amp;amp;
        assert!(
            !xml.contains("&amp;amp;"),
            "double-escape must not occur: {xml}"
        );
        assert!(
            xml.contains("&amp;"),
            "single escape of & must appear: {xml}"
        );
    }

    // ── 59: edge data keys are d_relation and d_confidence ───────────────────

    #[test]
    fn edge_data_keys_are_correct() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        let xml = render_graphml(&g);
        assert!(
            xml.contains("key=\"d_relation\""),
            "edge data key d_relation missing: {xml}"
        );
        assert!(
            xml.contains("key=\"d_confidence\""),
            "edge data key d_confidence missing: {xml}"
        );
    }

    // ── 60: multiple edges all appear in output ───────────────────────────────

    #[test]
    fn multiple_edges_all_appear() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        g.edges.push(edge(2, 3, "imports", Confidence::Inferred));
        g.edges.push(edge(3, 1, "depends", Confidence::Ambiguous));
        let xml = render_graphml(&g);
        assert_eq!(
            count_occurrences(&xml, "<edge "),
            3,
            "expected 3 <edge> elements: {xml}"
        );
        assert!(xml.contains(">calls<"), "calls relation missing");
        assert!(xml.contains(">imports<"), "imports relation missing");
        assert!(xml.contains(">depends<"), "depends relation missing");
    }

    // ── 61: 10-node graph has correct node count ─────────────────────────────

    #[test]
    fn ten_node_graph_correct_count() {
        let mut g = Graph::new();
        for i in 0..10_u32 {
            g.nodes.push(node(i, &format!("node{i}"), "src.rs"));
        }
        let xml = render_graphml(&g);
        assert_eq!(
            count_occurrences(&xml, "<node "),
            10,
            "expected 10 <node> elements"
        );
    }

    // ── 62: sorted graph — output order matches sort order ───────────────────

    #[test]
    fn sorted_graph_output_order_matches() {
        let mut g = Graph::new();
        g.nodes.push(node(3, "C", "c.rs"));
        g.nodes.push(node(1, "A", "a.rs"));
        g.nodes.push(node(2, "B", "b.rs"));
        let g = g.sorted();
        let xml = render_graphml(&g);
        // n1 must appear before n2 before n3 in the output.
        let pos1 = xml.find("id=\"n1\"").expect("n1 missing");
        let pos2 = xml.find("id=\"n2\"").expect("n2 missing");
        let pos3 = xml.find("id=\"n3\"").expect("n3 missing");
        assert!(pos1 < pos2 && pos2 < pos3, "nodes out of sort order in output");
    }

    // ── 63: graph id attribute is "G" ────────────────────────────────────────

    #[test]
    fn graph_element_has_id_g() {
        let xml = render_graphml(&Graph::new());
        assert!(
            xml.contains("id=\"G\""),
            "<graph> element must have id=\"G\": {xml}"
        );
    }

    // ── 64: source_file path with spaces is xml-escaped correctly ─────────────

    #[test]
    fn source_file_with_special_chars_escaped() {
        let mut g = Graph::new();
        g.nodes
            .push(node(1, "X", "path/with <brackets> & ampersand.rs"));
        let xml = render_graphml(&g);
        assert!(
            !xml.contains("<brackets>"),
            "raw brackets in source_file must not appear: {xml}"
        );
        assert!(
            xml.contains("&lt;brackets&gt;"),
            "brackets must be escaped: {xml}"
        );
    }

    // ── 65: node data elements are children of their <node> ──────────────────

    #[test]
    fn node_data_appears_between_node_tags() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "MyNode", "src.rs"));
        let xml = render_graphml(&g);
        // Find the <node id="n1"> ... </node> slice.
        let node_open = xml.find("<node id=\"n1\">").expect("<node id=\"n1\"> missing");
        let node_close = xml[node_open..].find("</node>").expect("</node> missing");
        let node_slice = &xml[node_open..node_open + node_close];
        assert!(
            node_slice.contains("key=\"d_label\""),
            "d_label data must be inside <node>: {node_slice}"
        );
        assert!(
            node_slice.contains("key=\"d_source_file\""),
            "d_source_file data must be inside <node>: {node_slice}"
        );
    }
}
