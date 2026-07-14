//! Load a graphify committed node-link golden into a [`NormalizedGraph`].

use std::collections::BTreeSet;

use habitat_graph_core::{GraphError, Result};

use crate::NormalizedGraph;

/// Parses a graphify node-link `graph.json` (committed golden) into id-keyed sets.
///
/// The graphify format stores a top-level `"nodes"` array — each node must carry a string `"id"`
/// field (e.g. `"exceptions_httperror"`) — and a top-level `"links"` array — each link must carry
/// string `"source"`, `"target"`, and `"relation"` fields that reference node ids directly.
///
/// The resulting [`NormalizedGraph`] contains:
/// * `nodes` — the set of every node's `"id"` value.
/// * `edges` — the set of `(source, target, relation)` tuples from the links array.
///
/// # Errors
///
/// Returns [`GraphError::Schema`] if:
/// * `json` is not syntactically valid JSON.
/// * the top-level value does not carry a `"nodes"` array.
/// * the top-level value does not carry a `"links"` array.
/// * any node element is missing a string `"id"` field.
/// * any link element is missing a string `"source"`, `"target"`, or `"relation"` field.
pub fn from_golden(json: &str) -> Result<NormalizedGraph> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| GraphError::Schema(format!("invalid JSON: {e}")))?;

    // --- nodes ------------------------------------------------------------
    let nodes_arr = root
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            GraphError::Schema("top-level 'nodes' key is absent or not an array".to_owned())
        })?;

    let mut nodes: BTreeSet<String> = BTreeSet::new();
    for (idx, node_val) in nodes_arr.iter().enumerate() {
        let id = node_val
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| GraphError::Schema(format!("node[{idx}] missing string field 'id'")))?;
        nodes.insert(id.to_owned());
    }

    // --- links ------------------------------------------------------------
    let links_arr = root
        .get("links")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            GraphError::Schema("top-level 'links' key is absent or not an array".to_owned())
        })?;

    let mut edges: BTreeSet<(String, String, String)> = BTreeSet::new();
    for (idx, link_val) in links_arr.iter().enumerate() {
        let source = link_val
            .get("source")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                GraphError::Schema(format!("link[{idx}] missing string field 'source'"))
            })?;
        let target = link_val
            .get("target")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                GraphError::Schema(format!("link[{idx}] missing string field 'target'"))
            })?;
        let relation = link_val
            .get("relation")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                GraphError::Schema(format!("link[{idx}] missing string field 'relation'"))
            })?;
        edges.insert((source.to_owned(), target.to_owned(), relation.to_owned()));
    }

    Ok(NormalizedGraph { nodes, edges })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ─────────────────────────────────────────────────────────────

    /// Loads the real vendored httpx golden from the fixtures directory.
    fn load_httpx_golden() -> String {
        let path = format!(
            "{}/../../tests/fixtures/goldens/httpx/graph.json",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read httpx golden at {path}: {e}"))
    }

    /// Inline minimal node-link document with two nodes and one link.
    const TWO_NODE_ONE_LINK: &str = r#"
    {
        "directed": false,
        "multigraph": false,
        "graph": {},
        "nodes": [
            {"id": "alpha", "label": "Alpha"},
            {"id": "beta",  "label": "Beta"}
        ],
        "links": [
            {"source": "alpha", "target": "beta", "relation": "contains"}
        ]
    }
    "#;

    // ── golden corpus tests ─────────────────────────────────────────────────

    #[test]
    fn golden_parses_without_error() {
        let json = load_httpx_golden();
        assert!(
            from_golden(&json).is_ok(),
            "httpx golden must parse cleanly"
        );
    }

    #[test]
    fn golden_node_count_is_144() {
        let json = load_httpx_golden();
        let g = from_golden(&json).expect("httpx golden parses");
        assert_eq!(
            g.nodes.len(),
            144,
            "httpx golden has exactly 144 unique node ids; got {}",
            g.nodes.len()
        );
    }

    #[test]
    fn golden_edge_count_is_330() {
        let json = load_httpx_golden();
        let g = from_golden(&json).expect("httpx golden parses");
        assert_eq!(
            g.edges.len(),
            330,
            "httpx golden has exactly 330 unique (source,target,relation) tuples; got {}",
            g.edges.len()
        );
    }

    #[test]
    fn golden_has_exceptions_node_id() {
        let json = load_httpx_golden();
        let g = from_golden(&json).expect("httpx golden parses");
        assert!(
            g.nodes.contains("exceptions"),
            "node id 'exceptions' must appear in the node set"
        );
    }

    #[test]
    fn golden_has_exceptions_httperror_node_id() {
        let json = load_httpx_golden();
        let g = from_golden(&json).expect("httpx golden parses");
        assert!(
            g.nodes.contains("exceptions_httperror"),
            "node id 'exceptions_httperror' must appear in the node set"
        );
    }

    #[test]
    fn golden_has_contains_edge_exceptions_to_httperror() {
        let json = load_httpx_golden();
        let g = from_golden(&json).expect("httpx golden parses");
        let edge = (
            "exceptions".to_owned(),
            "exceptions_httperror".to_owned(),
            "contains".to_owned(),
        );
        assert!(
            g.edges.contains(&edge),
            "edge (exceptions, exceptions_httperror, contains) must be present"
        );
    }

    #[test]
    fn golden_has_at_least_one_inherits_edge() {
        let json = load_httpx_golden();
        let g = from_golden(&json).expect("httpx golden parses");
        let inherits = g.edges_with_relation("inherits");
        assert!(
            !inherits.is_empty(),
            "golden must contain at least one 'inherits' edge"
        );
    }

    #[test]
    fn golden_has_at_least_one_method_edge() {
        let json = load_httpx_golden();
        let g = from_golden(&json).expect("httpx golden parses");
        let methods = g.edges_with_relation("method");
        assert!(
            !methods.is_empty(),
            "golden must contain at least one 'method' edge"
        );
    }

    #[test]
    fn golden_node_set_is_sorted_deterministically() {
        // BTreeSet guarantees sorted order; two parses of the same input must yield identical sets.
        let json = load_httpx_golden();
        let g1 = from_golden(&json).expect("first parse");
        let g2 = from_golden(&json).expect("second parse");
        assert_eq!(g1.nodes, g2.nodes, "node set must be deterministic");
        assert_eq!(g1.edges, g2.edges, "edge set must be deterministic");
    }

    // ── minimal hand-written literal tests ──────────────────────────────────

    #[test]
    fn minimal_two_node_one_link_parses() {
        let g = from_golden(TWO_NODE_ONE_LINK).expect("minimal doc must parse");
        assert_eq!(g.nodes.len(), 2);
        assert!(g.nodes.contains("alpha"));
        assert!(g.nodes.contains("beta"));
        assert_eq!(g.edges.len(), 1);
        let edge = ("alpha".to_owned(), "beta".to_owned(), "contains".to_owned());
        assert!(g.edges.contains(&edge));
    }

    #[test]
    fn empty_arrays_produce_empty_sets() {
        let json = r#"{"nodes": [], "links": []}"#;
        let g = from_golden(json).expect("empty arrays are valid");
        assert!(g.nodes.is_empty(), "nodes must be empty");
        assert!(g.edges.is_empty(), "edges must be empty");
    }

    #[test]
    fn non_json_input_returns_schema_error() {
        let err = from_golden("this is not json at all!!!").unwrap_err();
        assert_eq!(err.kind(), "schema");
    }

    #[test]
    fn missing_nodes_key_returns_schema_error() {
        // "links" present but "nodes" absent
        let json = r#"{"links": []}"#;
        let err = from_golden(json).unwrap_err();
        assert_eq!(err.kind(), "schema");
        assert!(
            err.to_string().contains("nodes"),
            "error must mention the missing key; got: {err}"
        );
    }

    #[test]
    fn missing_links_key_returns_schema_error() {
        // "nodes" present but "links" absent
        let json = r#"{"nodes": []}"#;
        let err = from_golden(json).unwrap_err();
        assert_eq!(err.kind(), "schema");
        assert!(
            err.to_string().contains("links"),
            "error must mention the missing key; got: {err}"
        );
    }

    #[test]
    fn nodes_not_array_returns_schema_error() {
        let json = r#"{"nodes": "should-be-array", "links": []}"#;
        let err = from_golden(json).unwrap_err();
        assert_eq!(err.kind(), "schema");
    }

    #[test]
    fn node_missing_id_field_returns_schema_error() {
        // Node has a "label" but no "id"
        let json = r#"{"nodes": [{"label": "Orphan"}], "links": []}"#;
        let err = from_golden(json).unwrap_err();
        assert_eq!(err.kind(), "schema");
        assert!(
            err.to_string().contains("id"),
            "error must mention 'id'; got: {err}"
        );
    }

    #[test]
    fn node_id_not_string_returns_schema_error() {
        // "id" is a number, not a string
        let json = r#"{"nodes": [{"id": 42}], "links": []}"#;
        let err = from_golden(json).unwrap_err();
        assert_eq!(err.kind(), "schema");
    }

    #[test]
    fn link_missing_source_returns_schema_error() {
        let json = r#"{"nodes": [{"id": "a"}], "links": [{"target": "a", "relation": "x"}]}"#;
        let err = from_golden(json).unwrap_err();
        assert_eq!(err.kind(), "schema");
        assert!(
            err.to_string().contains("source"),
            "error must mention 'source'; got: {err}"
        );
    }

    #[test]
    fn link_missing_target_returns_schema_error() {
        let json = r#"{"nodes": [{"id": "a"}], "links": [{"source": "a", "relation": "x"}]}"#;
        let err = from_golden(json).unwrap_err();
        assert_eq!(err.kind(), "schema");
        assert!(
            err.to_string().contains("target"),
            "error must mention 'target'; got: {err}"
        );
    }

    #[test]
    fn link_missing_relation_returns_schema_error() {
        let json = r#"{"nodes": [{"id": "a"}], "links": [{"source": "a", "target": "a"}]}"#;
        let err = from_golden(json).unwrap_err();
        assert_eq!(err.kind(), "schema");
        assert!(
            err.to_string().contains("relation"),
            "error must mention 'relation'; got: {err}"
        );
    }

    #[test]
    fn duplicate_links_are_deduplicated_into_set() {
        // The same (source, target, relation) triple repeated twice → one edge
        let json = r#"{
            "nodes": [{"id": "x"}, {"id": "y"}],
            "links": [
                {"source": "x", "target": "y", "relation": "calls"},
                {"source": "x", "target": "y", "relation": "calls"}
            ]
        }"#;
        let g = from_golden(json).expect("valid doc");
        assert_eq!(g.edges.len(), 1, "duplicate links collapse to one edge");
    }

    #[test]
    fn additional_node_fields_are_ignored() {
        // Extra fields beyond "id" must not cause failures
        let json = r#"{
            "nodes": [
                {"id": "n1", "label": "Node1", "community": 0, "extra_field": true}
            ],
            "links": []
        }"#;
        let g = from_golden(json).expect("extra node fields are harmless");
        assert!(g.nodes.contains("n1"));
    }

    #[test]
    fn additional_link_fields_are_ignored() {
        // Extra fields on links (confidence, weight, source_file, …) must not cause failures
        let json = r#"{
            "nodes": [{"id": "a"}, {"id": "b"}],
            "links": [
                {
                    "source": "a", "target": "b", "relation": "imports_from",
                    "confidence": "EXTRACTED", "weight": 1.0,
                    "_src": "a", "_tgt": "b", "source_file": "a.py"
                }
            ]
        }"#;
        let g = from_golden(json).expect("extra link fields are harmless");
        let edge = ("a".to_owned(), "b".to_owned(), "imports_from".to_owned());
        assert!(g.edges.contains(&edge));
    }

    #[test]
    fn json_object_at_root_with_no_keys_returns_schema_error() {
        // {} has neither "nodes" nor "links"
        let err = from_golden("{}").unwrap_err();
        assert_eq!(err.kind(), "schema");
    }
}
