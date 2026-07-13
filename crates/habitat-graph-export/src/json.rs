//! `NetworkX` node-link JSON export (graphify-compatible envelope; see `ai_docs/06_PARITY_INTEL`).

use std::collections::HashMap;

use habitat_graph_core::{
    display_safe, is_canonical_redaction_marker, sanitize_label, Graph, GraphError, NodeId, Result,
    SCHEMA_VERSION,
};
use serde_json::Value;

use crate::escape::{project_public_edges, redact_public_text};

/// Renders `graph` as `NetworkX` node-link JSON.
///
/// Envelope:
/// ```json
/// { "directed": true, "multigraph": false, "graph": {}, "nodes": [...], "links": [...] }
/// ```
///
/// Each node entry:
/// ```json
/// { "id": <u32>, "content_id": <optional u32 for displaced redacted nodes>,
///   "label": <str>, "source_file": <str>, "source_location": "L<n>",
///   "community": <u32 | null> }
/// ```
///
/// Each link entry:
/// ```json
/// { "source": <u32>, "target": <u32>, "relation": <str>,
///   "confidence": "EXTRACTED"|"INFERRED"|"AMBIGUOUS", "weight": 1.0|0.8 }
/// ```
///
/// `weight` is `1.0` when `confidence.is_trusted()` (`EXTRACTED`), otherwise `0.8`.
/// `community` is the [`CommunityId`](habitat_graph_core::CommunityId) integer when the node
/// appears in `graph.communities`, or JSON `null` otherwise.
///
/// Output is fully deterministic: nodes follow `graph.nodes`, while links are ordered by source,
/// target, projected relation, and confidence before redacted relation ordinals are assigned.
/// Call [`Graph::sorted`](habitat_graph_core::Graph::sorted) first to obtain canonical node and
/// community ordering. String fields are sanitised with
/// [`sanitize_label`], redacted with the shared public-output policy, and render-escaped with
/// [`display_safe`] before embedding. Redaction changes display fields only; node ids and topology
/// remain untouched.
///
/// # Errors
///
/// Returns [`GraphError::Schema`] if
/// [`serde_json`] serialization fails (in practice, only if a [`serde_json::Number`] is
/// non-finite, which cannot occur here).
pub fn to_node_link(graph: &Graph) -> Result<String> {
    // Build NodeId → community-id lookup from community membership lists.
    // If a node appears in multiple communities the last one wins — the schema treats
    // communities as disjoint partitions so this edge case does not arise in practice.
    let community_map: HashMap<NodeId, u32> = graph
        .communities
        .iter()
        .flat_map(|c| {
            let cid = c.id.get();
            c.members.iter().map(move |&nid| (nid, cid))
        })
        .collect();

    // Build nodes array.
    let nodes: Vec<Value> = graph
        .nodes
        .iter()
        .map(|node| {
            let community: Value = community_map
                .get(&node.id)
                .copied()
                .map_or(Value::Null, Value::from);
            let redacted_label = redact_public_text(&node.label);
            let redacted_source_file = redact_public_text(&node.source_file);
            let mut value = serde_json::json!({
                "id": node.id.get(),
                "label": display_safe(&sanitize_label(&redacted_label)),
                "source_file": display_safe(&redacted_source_file),
                "source_location": format!("L{}", node.source_location.start_line),
                "community": community,
            });
            let content_id = graph.node_content_id(node.id);
            if content_id != node.id && is_canonical_redaction_marker(&redacted_label) {
                if let Value::Object(fields) = &mut value {
                    fields.insert("content_id".to_owned(), Value::from(content_id.get()));
                }
            }
            value
        })
        .collect();

    // Build links array.
    let links: Vec<Value> = project_public_edges(graph)
        .into_iter()
        .map(|projected| {
            let edge = projected.edge;
            // weight: 1.0 for EXTRACTED (trusted), 0.8 for INFERRED / AMBIGUOUS.
            let weight: f64 = if edge.confidence.is_trusted() {
                1.0
            } else {
                0.8
            };
            serde_json::json!({
                "source": edge.source.get(),
                "target": edge.target.get(),
                "relation": display_safe(&sanitize_label(&projected.relation)),
                "confidence": edge.confidence.as_str(),
                "weight": weight,
            })
        })
        .collect();

    // `schema_version` rides in the NetworkX graph-attributes object (P1-G12): it lets `update`
    // detect a taxonomy mismatch against an older graph.json instead of silently merging.
    let envelope = serde_json::json!({
        "directed": true,
        "multigraph": false,
        "graph": { "schema_version": SCHEMA_VERSION },
        "nodes": nodes,
        "links": links,
    });

    serde_json::to_string_pretty(&envelope).map_err(|e| GraphError::Schema(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::to_node_link;
    use habitat_graph_core::{
        Community, CommunityId, Confidence, Edge, Graph, Node, NodeId, Span, SCHEMA_VERSION,
    };

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

    fn community(id: u32, members: Vec<u32>) -> Community {
        Community {
            id: CommunityId::new(id),
            label: format!("cluster-{id}"),
            members: members.into_iter().map(NodeId::new).collect(),
        }
    }

    fn parse(json: &str) -> serde_json::Value {
        serde_json::from_str(json).expect("output must be valid JSON")
    }

    // ── 1: empty graph produces valid JSON ───────────────────────────────────

    #[test]
    fn empty_graph_is_valid_json() {
        let result = to_node_link(&Graph::new());
        assert!(result.is_ok(), "empty graph must serialize: {result:?}");
        let v = serde_json::from_str::<serde_json::Value>(&result.unwrap());
        assert!(v.is_ok(), "output must re-parse as JSON");
    }

    // ── 2: empty nodes array ─────────────────────────────────────────────────

    #[test]
    fn empty_graph_has_empty_nodes_array() {
        let json = to_node_link(&Graph::new()).unwrap();
        let v = parse(&json);
        assert_eq!(v["nodes"].as_array().expect("nodes must be array").len(), 0);
    }

    // ── 3: empty links array ─────────────────────────────────────────────────

    #[test]
    fn empty_graph_has_empty_links_array() {
        let json = to_node_link(&Graph::new()).unwrap();
        let v = parse(&json);
        assert_eq!(v["links"].as_array().expect("links must be array").len(), 0);
    }

    // ── 4: all top-level keys present ────────────────────────────────────────

    #[test]
    fn top_level_keys_present() {
        let json = to_node_link(&Graph::new()).unwrap();
        let v = parse(&json);
        for key in ["directed", "multigraph", "graph", "nodes", "links"] {
            assert!(v.get(key).is_some(), "missing top-level key: {key}");
        }
    }

    // ── 5: directed == true ──────────────────────────────────────────────────

    #[test]
    fn directed_is_true() {
        let v = parse(&to_node_link(&Graph::new()).unwrap());
        assert_eq!(v["directed"], serde_json::Value::Bool(true));
    }

    // ── 6: multigraph == false ───────────────────────────────────────────────

    #[test]
    fn multigraph_is_false() {
        let v = parse(&to_node_link(&Graph::new()).unwrap());
        assert_eq!(v["multigraph"], serde_json::Value::Bool(false));
    }

    // ── 7: graph metadata carries the schema_version (P1-G12) ────────────────

    #[test]
    fn graph_metadata_carries_schema_version() {
        let v = parse(&to_node_link(&Graph::new()).unwrap());
        assert_eq!(
            v["graph"],
            serde_json::json!({ "schema_version": SCHEMA_VERSION })
        );
        assert_eq!(v["graph"]["schema_version"], SCHEMA_VERSION);
    }

    // ── 8: 2-node 1-edge → correct counts ───────────────────────────────────

    #[test]
    fn two_node_one_edge_correct_shape() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "Alpha", "src/alpha.rs", 10));
        g.nodes.push(node(2, "Beta", "src/beta.rs", 20));
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));

        let v = parse(&to_node_link(&g).unwrap());
        assert_eq!(v["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(v["links"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn collision_displaced_node_emits_original_content_id() {
        let mut g = Graph::new();
        g.nodes
            .push(node(2, "[REDACTED:api_key]", "src/alpha.rs", 10));
        g.node_content_ids.insert(NodeId::new(2), NodeId::new(1));

        let v = parse(&to_node_link(&g).unwrap());

        assert_eq!(v["nodes"][0]["id"], 2);
        assert_eq!(v["nodes"][0]["content_id"], 1);
    }

    // ── 9: source_location renders "L<n>" ────────────────────────────────────

    #[test]
    fn source_location_renders_l_prefix() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "Thing", "src/thing.rs", 42));
        let v = parse(&to_node_link(&g).unwrap());
        assert_eq!(v["nodes"][0]["source_location"], "L42");
    }

    #[test]
    fn source_location_line_one() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "Root", "lib.rs", 1));
        let v = parse(&to_node_link(&g).unwrap());
        assert_eq!(v["nodes"][0]["source_location"], "L1");
    }

    // ── 10: EXTRACTED → weight 1.0 ──────────────────────────────────────────

    #[test]
    fn extracted_confidence_yields_weight_one() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        let v = parse(&to_node_link(&g).unwrap());
        let w = v["links"][0]["weight"].as_f64().unwrap();
        assert!((w - 1.0_f64).abs() < f64::EPSILON, "expected 1.0, got {w}");
    }

    // ── 11: INFERRED → weight 0.8 ───────────────────────────────────────────

    #[test]
    fn inferred_confidence_yields_weight_point_eight() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "uses", Confidence::Inferred));
        let v = parse(&to_node_link(&g).unwrap());
        let w = v["links"][0]["weight"].as_f64().unwrap();
        assert!((w - 0.8_f64).abs() < 1e-10, "expected 0.8, got {w}");
    }

    // ── 12: AMBIGUOUS → weight 0.8 (not trusted) ────────────────────────────

    #[test]
    fn ambiguous_confidence_yields_weight_point_eight() {
        let mut g = Graph::new();
        g.edges.push(edge(3, 4, "maybe", Confidence::Ambiguous));
        let v = parse(&to_node_link(&g).unwrap());
        let w = v["links"][0]["weight"].as_f64().unwrap();
        assert!((w - 0.8_f64).abs() < 1e-10, "expected 0.8, got {w}");
    }

    // ── 13: community id on a clustered node ────────────────────────────────

    #[test]
    fn clustered_node_has_community_id() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "Node1", "src/a.rs", 1));
        g.communities.push(community(7, vec![1]));

        let v = parse(&to_node_link(&g).unwrap());
        assert_eq!(v["nodes"][0]["community"], serde_json::json!(7_u32));
    }

    // ── 14: unclustered node → community null ────────────────────────────────

    #[test]
    fn unclustered_node_has_null_community() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "Lone", "lone.rs", 5));
        let v = parse(&to_node_link(&g).unwrap());
        assert_eq!(v["nodes"][0]["community"], serde_json::Value::Null);
    }

    // ── 15: output re-parses as valid JSON ───────────────────────────────────

    #[test]
    fn output_re_parses_as_valid_json() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "Alpha", "src/a.rs", 1));
        g.nodes.push(node(2, "Beta", "src/b.rs", 2));
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));

        let json = to_node_link(&g).unwrap();
        let result = serde_json::from_str::<serde_json::Value>(&json);
        assert!(result.is_ok(), "output must re-parse as JSON: {json}");
    }

    // ── 16: deterministic — same graph → identical string ───────────────────

    #[test]
    fn same_graph_produces_identical_output() {
        let build = || {
            let mut g = Graph::new();
            g.nodes.push(node(1, "Alpha", "a.rs", 10));
            g.nodes.push(node(2, "Beta", "b.rs", 20));
            g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
            g.edges.push(edge(2, 1, "imports", Confidence::Inferred));
            g.communities.push(community(0, vec![1, 2]));
            g
        };
        let out1 = to_node_link(&build()).unwrap();
        let out2 = to_node_link(&build()).unwrap();
        assert_eq!(out1, out2, "to_node_link must be deterministic");
    }

    // ── 17: all node fields map correctly ────────────────────────────────────

    #[test]
    fn node_fields_map_correctly() {
        let mut g = Graph::new();
        g.nodes.push(node(42, "MyFunc", "src/lib.rs", 99));
        let v = parse(&to_node_link(&g).unwrap());
        let n = &v["nodes"][0];
        assert_eq!(n["id"].as_u64().unwrap(), 42);
        assert_eq!(n["label"], "MyFunc");
        assert_eq!(n["source_file"], "src/lib.rs");
        assert_eq!(n["source_location"], "L99");
        assert_eq!(n["community"], serde_json::Value::Null);
    }

    // ── 18: all link fields map correctly ────────────────────────────────────

    #[test]
    fn link_fields_map_correctly() {
        let mut g = Graph::new();
        g.edges.push(edge(3, 7, "imports", Confidence::Inferred));
        let v = parse(&to_node_link(&g).unwrap());
        let lnk = &v["links"][0];
        assert_eq!(lnk["source"].as_u64().unwrap(), 3);
        assert_eq!(lnk["target"].as_u64().unwrap(), 7);
        assert_eq!(lnk["relation"], "imports");
        assert_eq!(lnk["confidence"], "INFERRED");
        let w = lnk["weight"].as_f64().unwrap();
        assert!((w - 0.8_f64).abs() < 1e-10);
    }

    // ── 19: confidence strings are canonical wire values ─────────────────────

    #[test]
    fn confidence_strings_are_canonical() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "a", Confidence::Extracted));
        g.edges.push(edge(2, 3, "b", Confidence::Inferred));
        g.edges.push(edge(3, 4, "c", Confidence::Ambiguous));
        let v = parse(&to_node_link(&g).unwrap());
        let confs: Vec<&str> = v["links"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["confidence"].as_str().unwrap())
            .collect();
        assert!(confs.contains(&"EXTRACTED"), "missing EXTRACTED");
        assert!(confs.contains(&"INFERRED"), "missing INFERRED");
        assert!(confs.contains(&"AMBIGUOUS"), "missing AMBIGUOUS");
    }

    // ── 20: multiple nodes, mixed community membership ───────────────────────

    #[test]
    fn mixed_community_membership() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "A", "a.rs", 1));
        g.nodes.push(node(2, "B", "b.rs", 2));
        g.nodes.push(node(3, "C", "c.rs", 3)); // no community
        g.communities.push(community(5, vec![1, 2]));

        let v = parse(&to_node_link(&g).unwrap());
        let nodes = v["nodes"].as_array().unwrap();

        let find = |id: u64| {
            nodes
                .iter()
                .find(|n| n["id"].as_u64() == Some(id))
                .expect("node missing")
        };

        assert_eq!(find(1)["community"], serde_json::json!(5_u32));
        assert_eq!(find(2)["community"], serde_json::json!(5_u32));
        assert_eq!(find(3)["community"], serde_json::Value::Null);
    }

    // ── 21: render-dangerous label characters are escaped ────────────────────

    #[test]
    fn render_dangerous_label_is_escaped() {
        let mut g = Graph::new();
        // U+202E = RIGHT-TO-LEFT OVERRIDE — Trojan-Source payload
        g.nodes.push(node(1, "fn\u{202E}safe", "src/a.rs", 1));
        let v = parse(&to_node_link(&g).unwrap());
        let label = v["nodes"][0]["label"].as_str().unwrap();
        assert!(
            !label.contains('\u{202E}'),
            "raw bidi override must not appear in output label: {label:?}"
        );
        assert!(
            label.contains("\\u{202E}"),
            "bidi override must be escaped: {label:?}"
        );
    }

    // ── 22: large source_location line number formats correctly ──────────────

    #[test]
    fn large_line_number_formats_correctly() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "Big", "huge.rs", 999_999));
        let v = parse(&to_node_link(&g).unwrap());
        assert_eq!(v["nodes"][0]["source_location"], "L999999");
    }

    // ── 23: two edges with opposite confidence → distinct weights ────────────

    #[test]
    fn extracted_and_inferred_edges_have_distinct_weights() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        g.edges.push(edge(2, 3, "maybe", Confidence::Inferred));
        let v = parse(&to_node_link(&g).unwrap());
        let w0 = v["links"][0]["weight"].as_f64().unwrap();
        let w1 = v["links"][1]["weight"].as_f64().unwrap();
        assert!((w0 - 1.0_f64).abs() < f64::EPSILON);
        assert!((w1 - 0.8_f64).abs() < 1e-10);
        assert!((w0 - w1).abs() > 1e-10, "weights must differ");
    }

    // ── 24: community with multiple members, id zero ─────────────────────────

    #[test]
    fn community_id_zero_is_valid() {
        let mut g = Graph::new();
        g.nodes.push(node(10, "X", "x.rs", 1));
        g.communities.push(community(0, vec![10]));
        let v = parse(&to_node_link(&g).unwrap());
        assert_eq!(v["nodes"][0]["community"], serde_json::json!(0_u32));
    }

    // ── 25: no communities → all nodes have null community ───────────────────

    #[test]
    fn no_communities_all_null() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "A", "a.rs", 1));
        g.nodes.push(node(2, "B", "b.rs", 2));
        let v = parse(&to_node_link(&g).unwrap());
        for n in v["nodes"].as_array().unwrap() {
            assert_eq!(n["community"], serde_json::Value::Null);
        }
    }

    // ── 26: node count matches graph.nodes.len() ─────────────────────────────

    #[test]
    fn node_count_matches_graph() {
        let mut g = Graph::new();
        for i in 0..10_u32 {
            g.nodes.push(node(i, &format!("n{i}"), "src.rs", i + 1));
        }
        let v = parse(&to_node_link(&g).unwrap());
        assert_eq!(v["nodes"].as_array().unwrap().len(), 10);
    }

    // ── 27: link count matches graph.edges.len() ─────────────────────────────

    #[test]
    fn link_count_matches_graph() {
        let mut g = Graph::new();
        for i in 0..5_u32 {
            g.edges.push(edge(i, i + 1, "e", Confidence::Extracted));
        }
        let v = parse(&to_node_link(&g).unwrap());
        assert_eq!(v["links"].as_array().unwrap().len(), 5);
    }

    // ── 28: sorted graph → output order matches sort ─────────────────────────

    #[test]
    fn sorted_graph_output_follows_sort_order() {
        let mut g = Graph::new();
        g.nodes.push(node(3, "C", "c.rs", 3));
        g.nodes.push(node(1, "A", "a.rs", 1));
        g.nodes.push(node(2, "B", "b.rs", 2));
        let g = g.sorted();
        let v = parse(&to_node_link(&g).unwrap());
        let ids: Vec<u64> = v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![1, 2, 3], "nodes must appear in sort order");
    }

    // ── security: edge relation passes through display_safe (judge gap) ──────────

    #[test]
    fn node_label_secret_pattern_is_redacted_without_changing_id() {
        let mut g = Graph::new();
        g.nodes
            .push(node(41, "api_key_assignment_refused", "src/privacy.rs", 1));
        let v = parse(&to_node_link(&g).unwrap());
        assert_eq!(v["nodes"][0]["id"], 41);
        assert_eq!(v["nodes"][0]["label"], "[REDACTED:api_key]");
        assert!(!to_node_link(&g)
            .unwrap()
            .contains("api_key_assignment_refused"));
    }

    #[test]
    fn source_file_and_relation_secret_patterns_are_redacted() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "A", "src/api_key/private.rs", 1));
        g.edges.push(edge(
            1,
            1,
            "Authorization: Bearer token",
            Confidence::Extracted,
        ));
        let v = parse(&to_node_link(&g).unwrap());
        assert_eq!(v["nodes"][0]["source_file"], "[REDACTED:api_key]");
        let relation = v["links"][0]["relation"].as_str().expect("relation");
        assert_eq!(relation, "[REDACTED:bearer_token]#e00000000000000000000");
    }

    #[test]
    fn distinct_redacted_relations_keep_stable_non_secret_identity() {
        let mut g = Graph::new();
        g.edges
            .push(edge(1, 2, "api_key=alpha", Confidence::Extracted));
        g.edges
            .push(edge(1, 2, "api_key=beta", Confidence::Extracted));
        let first = parse(&to_node_link(&g).unwrap());
        let relations: Vec<&str> = first["links"]
            .as_array()
            .expect("links")
            .iter()
            .filter_map(|link| link["relation"].as_str())
            .collect();
        assert_eq!(relations.len(), 2);
        assert_ne!(relations[0], relations[1]);
        assert!(relations
            .iter()
            .all(|relation| relation.starts_with("[REDACTED:api_key]#e")));
        assert!(!to_node_link(&g).unwrap().contains("api_key=alpha"));
    }

    #[test]
    fn redacted_relation_order_uses_only_public_edge_fields() {
        let mut first = Graph::new();
        first
            .edges
            .push(edge(1, 2, "api_key=alpha", Confidence::Ambiguous));
        first
            .edges
            .push(edge(1, 2, "api_key=zulu", Confidence::Extracted));

        let mut swapped = Graph::new();
        swapped
            .edges
            .push(edge(1, 2, "api_key=alpha", Confidence::Extracted));
        swapped
            .edges
            .push(edge(1, 2, "api_key=zulu", Confidence::Ambiguous));

        let first_public = to_node_link(&first).unwrap();
        let swapped_public = to_node_link(&swapped).unwrap();
        assert_eq!(first_public, swapped_public);
        let value = parse(&first_public);
        assert_eq!(value["links"][0]["confidence"], "EXTRACTED");
        assert_eq!(value["links"][1]["confidence"], "AMBIGUOUS");
    }

    #[test]
    fn projected_relation_identity_is_idempotent() {
        let mut g = Graph::new();
        g.edges
            .push(edge(1, 2, "api_key=alpha", Confidence::Extracted));
        let first = to_node_link(&g).unwrap();
        let projected = parse(&first)["links"][0]["relation"]
            .as_str()
            .expect("relation")
            .to_owned();
        g.edges[0].relation = projected;
        assert_eq!(to_node_link(&g).unwrap(), first);
    }

    #[test]
    fn edge_relation_bidi_override_is_render_safe() {
        // A relation carrying a Trojan-Source bidi override (U+202E) must be escaped, never raw, in
        // the serialized output — the same render-boundary guard applied to node labels.
        let mut g = Graph::new();
        g.nodes.push(node(1, "A", "a.rs", 1));
        g.nodes.push(node(2, "B", "b.rs", 2));
        g.edges
            .push(edge(1, 2, "calls\u{202E}evil", Confidence::Inferred));
        let json = to_node_link(&g).unwrap();
        assert!(
            !json.contains('\u{202E}'),
            "raw RLO must not appear in serialized output"
        );
        let v = parse(&json);
        let rel = v["links"][0]["relation"]
            .as_str()
            .expect("relation is a string");
        assert!(
            rel.contains("\\u{202E}"),
            "relation must be display_safe-escaped: {rel}"
        );
    }
}
