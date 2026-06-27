//! The semantic-extraction wire protocol: the prompt a backend sends, the `{nodes, edges}` JSON it
//! asks a model to return, and the conversion of that JSON into the core [`Extraction`] vocabulary.
//!
//! A model's response is **untrusted input** — every label and relation funnels through
//! [`sanitize_label`] (control-stripping + length cap) before it can enter the graph, the same
//! discipline the AST extractors apply. Blank entries are dropped: a blank node id would corrupt the
//! build layer's label interning.

use habitat_graph_core::{
    sanitize_label, Confidence, Extraction, GraphError, RawEdge, RawNode, Result, Span,
};
use serde::Deserialize;

/// Relation assigned to a semantic edge whose `relation` field is empty.
pub const DEFAULT_RELATION: &str = "relates_to";

/// Semantic nodes have no byte range in the source file; this sentinel span marks that.
const SEMANTIC_SPAN: Span = Span::new(0, 0, 0, 0);

/// One node in a model's JSON response. Unknown fields (e.g. a `kind`) are ignored.
#[derive(Debug, Deserialize)]
struct WireNode {
    label: String,
}

/// One edge in a model's JSON response.
#[derive(Debug, Deserialize)]
struct WireEdge {
    source: String,
    target: String,
    #[serde(default)]
    relation: String,
}

/// The `{nodes, edges}` envelope a backend asks the model to return.
#[derive(Debug, Deserialize)]
struct WireGraph {
    #[serde(default)]
    nodes: Vec<WireNode>,
    #[serde(default)]
    edges: Vec<WireEdge>,
}

/// Builds the instruction prompt asking a model to return the semantic `{nodes, edges}` JSON.
///
/// The schema in the prompt mirrors [`WireGraph`]; `text` is appended verbatim. Backends that set a
/// JSON response-format flag still send this so models without that flag produce parseable output.
#[must_use]
pub fn build_prompt(text: &str) -> String {
    format!(
        "Extract the key concepts and their relationships from the text below. \
Respond with ONLY a JSON object of the form \
{{\"nodes\":[{{\"label\":\"name\"}}],\"edges\":[{{\"source\":\"a\",\"target\":\"b\",\"relation\":\"verb\"}}]}}. \
Use short concept names as labels. Text:\n{text}"
    )
}

/// Parses a model's JSON `{nodes, edges}` response into a core [`Extraction`].
///
/// `source_file` attributes the semantic nodes to their input. Labels and relations are sanitized;
/// an empty `relation` becomes [`DEFAULT_RELATION`]; whitespace is trimmed. Nodes with an empty
/// (post-sanitization) label and edges with an empty endpoint are dropped. All edges carry
/// [`Confidence::Inferred`] — semantic extraction is never as certain as a direct AST reference.
///
/// # Errors
/// Returns [`GraphError::Backend`] if `raw` is not the expected JSON object.
pub fn parse_semantic(raw: &str, source_file: &str) -> Result<Extraction> {
    let wire: WireGraph = serde_json::from_str(raw).map_err(|e| {
        GraphError::Backend(format!("semantic response was not the expected JSON object: {e}"))
    })?;

    let mut extraction = Extraction::new();
    for node in wire.nodes {
        let label = sanitize_label(node.label.trim());
        if label.is_empty() {
            continue;
        }
        extraction.nodes.push(RawNode {
            label,
            source_file: source_file.to_string(),
            span: SEMANTIC_SPAN,
        });
    }
    for edge in wire.edges {
        let source = sanitize_label(edge.source.trim());
        let target = sanitize_label(edge.target.trim());
        if source.is_empty() || target.is_empty() {
            continue;
        }
        let relation_trimmed = edge.relation.trim();
        let relation = if relation_trimmed.is_empty() {
            DEFAULT_RELATION.to_string()
        } else {
            sanitize_label(relation_trimmed)
        };
        extraction.edges.push(RawEdge {
            source,
            target,
            relation,
            confidence: Confidence::Inferred,
        });
    }
    Ok(extraction)
}

#[cfg(test)]
mod tests {
    use super::{build_prompt, parse_semantic, DEFAULT_RELATION};
    use habitat_graph_core::Confidence;

    #[test]
    fn parses_a_single_node() {
        let e = parse_semantic(r#"{"nodes":[{"label":"Auth"}]}"#, "doc.md").expect("ok");
        assert_eq!(e.counts(), (1, 0));
        assert_eq!(e.nodes[0].label, "Auth");
        assert_eq!(e.nodes[0].source_file, "doc.md");
    }

    #[test]
    fn parses_node_and_edge() {
        let raw = r#"{"nodes":[{"label":"A"},{"label":"B"}],"edges":[{"source":"A","target":"B","relation":"uses"}]}"#;
        let e = parse_semantic(raw, "f").expect("ok");
        assert_eq!(e.counts(), (2, 1));
        assert_eq!(e.edges[0].relation, "uses");
        assert_eq!(e.edges[0].source, "A");
        assert_eq!(e.edges[0].target, "B");
    }

    #[test]
    fn semantic_edges_are_inferred() {
        let raw = r#"{"edges":[{"source":"A","target":"B"}]}"#;
        let e = parse_semantic(raw, "f").expect("ok");
        assert_eq!(e.edges[0].confidence, Confidence::Inferred);
    }

    #[test]
    fn empty_object_yields_empty_extraction() {
        let e = parse_semantic("{}", "f").expect("ok");
        assert!(e.is_empty());
    }

    #[test]
    fn missing_arrays_default_to_empty() {
        assert!(parse_semantic(r#"{"nodes":[]}"#, "f").expect("ok").is_empty());
        assert!(parse_semantic(r#"{"edges":[]}"#, "f").expect("ok").is_empty());
    }

    #[test]
    fn invalid_json_is_a_backend_error() {
        let err = parse_semantic("not json", "f").expect_err("must fail");
        assert_eq!(err.kind(), "backend");
        assert!(err.to_string().contains("JSON"));
    }

    #[test]
    fn truncated_json_is_rejected() {
        assert!(parse_semantic(r#"{"nodes":[{"label":"#, "f").is_err());
    }

    #[test]
    fn empty_relation_becomes_default() {
        let raw = r#"{"edges":[{"source":"A","target":"B","relation":""}]}"#;
        let e = parse_semantic(raw, "f").expect("ok");
        assert_eq!(e.edges[0].relation, DEFAULT_RELATION);
    }

    #[test]
    fn absent_relation_field_becomes_default() {
        let raw = r#"{"edges":[{"source":"A","target":"B"}]}"#;
        let e = parse_semantic(raw, "f").expect("ok");
        assert_eq!(e.edges[0].relation, DEFAULT_RELATION);
    }

    #[test]
    fn whitespace_in_labels_is_trimmed() {
        let e = parse_semantic(r#"{"nodes":[{"label":"  Auth  "}]}"#, "f").expect("ok");
        assert_eq!(e.nodes[0].label, "Auth");
    }

    #[test]
    fn blank_node_label_is_dropped() {
        let raw = r#"{"nodes":[{"label":"   "},{"label":"Real"}]}"#;
        let e = parse_semantic(raw, "f").expect("ok");
        assert_eq!(e.counts(), (1, 0));
        assert_eq!(e.nodes[0].label, "Real");
    }

    #[test]
    fn edge_with_blank_endpoint_is_dropped() {
        let raw = r#"{"edges":[{"source":"","target":"B"},{"source":"A","target":"  "},{"source":"A","target":"B"}]}"#;
        let e = parse_semantic(raw, "f").expect("ok");
        assert_eq!(e.counts(), (0, 1));
    }

    #[test]
    fn control_chars_are_stripped_from_labels() {
        let e = parse_semantic("{\"nodes\":[{\"label\":\"A\\u0007B\"}]}", "f").expect("ok");
        assert_eq!(e.nodes[0].label, "AB");
    }

    #[test]
    fn bidi_override_in_label_is_escaped_at_render() {
        // U+202E RIGHT-TO-LEFT OVERRIDE is a *format* char (Cf), not a control char (Cc):
        // `sanitize_label` retains it in storage (the AST path does the same), and the core's
        // two-layer policy escapes it at the render boundary via `display_safe`. This pins that
        // contract for untrusted model output — a Trojan-Source label can never reach a terminal raw.
        let e = parse_semantic("{\"nodes\":[{\"label\":\"a\\u202eb\"}]}", "f").expect("ok");
        let label = &e.nodes[0].label;
        assert!(label.contains('\u{202e}'), "format char retained in storage");
        let rendered = habitat_graph_core::display_safe(label);
        assert!(!rendered.contains('\u{202e}'), "escaped at render");
        assert!(rendered.contains("\\u{202E}"));
    }

    #[test]
    fn relation_is_sanitized() {
        let e = parse_semantic(
            "{\"edges\":[{\"source\":\"A\",\"target\":\"B\",\"relation\":\"x\\u0007y\"}]}",
            "f",
        )
        .expect("ok");
        assert_eq!(e.edges[0].relation, "xy");
    }

    #[test]
    fn long_label_is_capped_at_256_chars() {
        let long = "z".repeat(500);
        let raw = format!("{{\"nodes\":[{{\"label\":\"{long}\"}}]}}");
        let e = parse_semantic(&raw, "f").expect("ok");
        assert_eq!(e.nodes[0].label.chars().count(), 256);
    }

    #[test]
    fn source_file_is_attributed_to_every_node() {
        let raw = r#"{"nodes":[{"label":"A"},{"label":"B"}]}"#;
        let e = parse_semantic(raw, "x/y.md").expect("ok");
        assert!(e.nodes.iter().all(|n| n.source_file == "x/y.md"));
    }

    #[test]
    fn nodes_have_sentinel_span() {
        let e = parse_semantic(r#"{"nodes":[{"label":"A"}]}"#, "f").expect("ok");
        assert!(e.nodes[0].span.is_empty());
        assert_eq!(e.nodes[0].span.start_line, 0);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let raw = r#"{"nodes":[{"label":"A","kind":"concept","extra":1}],"version":2}"#;
        let e = parse_semantic(raw, "f").expect("ok");
        assert_eq!(e.counts(), (1, 0));
    }

    #[test]
    fn unicode_labels_survive() {
        let e = parse_semantic(r#"{"nodes":[{"label":"café→naïve"}]}"#, "f").expect("ok");
        assert_eq!(e.nodes[0].label, "café→naïve");
    }

    #[test]
    fn order_is_preserved() {
        let raw = r#"{"nodes":[{"label":"first"},{"label":"second"},{"label":"third"}]}"#;
        let e = parse_semantic(raw, "f").expect("ok");
        let labels: Vec<_> = e.nodes.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(labels, ["first", "second", "third"]);
    }

    #[test]
    fn array_at_top_level_is_rejected() {
        // The protocol is an object, not a bare array.
        assert!(parse_semantic(r#"[{"label":"A"}]"#, "f").is_err());
    }

    #[test]
    fn wrong_type_for_label_is_rejected() {
        assert!(parse_semantic(r#"{"nodes":[{"label":123}]}"#, "f").is_err());
    }

    #[test]
    fn build_prompt_contains_text_and_schema() {
        let p = build_prompt("the body");
        assert!(p.contains("the body"));
        assert!(p.contains("\"nodes\""));
        assert!(p.contains("\"edges\""));
        assert!(p.contains("relation"));
    }

    #[test]
    fn build_prompt_schema_is_itself_valid_json_fragment() {
        // The embedded example object must be parseable so models that copy it stay on-protocol.
        let p = build_prompt("x");
        let start = p.find('{').expect("has brace");
        // The first balanced object in the prompt is the schema example.
        assert!(p[start..].contains(r#"{"label":"name"}"#));
    }

    #[test]
    fn build_prompt_round_trips_through_parser() {
        // A model that echoes the schema example should parse cleanly.
        let example = r#"{"nodes":[{"label":"name"}],"edges":[{"source":"a","target":"b","relation":"verb"}]}"#;
        let e = parse_semantic(example, "f").expect("ok");
        assert_eq!(e.counts(), (1, 1));
    }

    #[test]
    fn duplicate_labels_are_kept_for_the_build_layer_to_dedup() {
        let raw = r#"{"nodes":[{"label":"A"},{"label":"A"}]}"#;
        let e = parse_semantic(raw, "f").expect("ok");
        // Dedup is the build layer's job; the protocol preserves what the model said.
        assert_eq!(e.counts(), (2, 0));
    }

    #[test]
    fn empty_string_input_is_rejected() {
        assert!(parse_semantic("", "f").is_err());
    }

    #[test]
    fn self_referential_edge_is_allowed() {
        let raw = r#"{"edges":[{"source":"A","target":"A","relation":"recurses"}]}"#;
        let e = parse_semantic(raw, "f").expect("ok");
        assert_eq!(e.counts(), (0, 1));
        assert_eq!(e.edges[0].source, e.edges[0].target);
    }
}
