//! Load a node-link `graph.json` back into a queryable [`Graph`](habitat_graph_core::Graph).

use std::collections::BTreeMap;

use habitat_graph_core::{
    Community, CommunityId, Confidence, Edge, Graph, GraphError, Manifest, Node, NodeId, Result,
    Span, SCHEMA_VERSION,
};

/// Parses `NetworkX` node-link JSON (the envelope written by `habitat-graph-export`) into a
/// [`Graph`].
///
/// Reconstructs nodes (`id`, `label`, `source_file`, `community`) and links (`source`, `target`,
/// `relation`, `confidence`). `source_location` is rebuilt from the `"Lnn"` line marker only —
/// byte offsets are not present in node-link form, so the span carries the line on both axes.
///
/// Community membership is reconstructed by grouping all node ids sharing the same `"community"`
/// `u32` value; nodes whose `"community"` field is `null` (or absent) belong to no community.
///
/// The returned graph is **sorted** (nodes by id, edges by `(source, target, relation)`,
/// communities by id with members ascending), satisfying the determinism invariant R4.
///
/// # Errors
/// Returns [`GraphError::Schema`](habitat_graph_core::GraphError::Schema) if `json` is not a valid
/// node-link document (missing `nodes`/`links`, wrong field types, unknown confidence value, etc.).
pub fn from_node_link(json: &str) -> Result<Graph> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| GraphError::Schema(format!("invalid JSON: {e}")))?;

    let nodes_arr = root
        .get("nodes")
        .ok_or_else(|| GraphError::Schema("missing 'nodes' key".to_owned()))?
        .as_array()
        .ok_or_else(|| GraphError::Schema("'nodes' must be an array".to_owned()))?;

    let links_arr = root
        .get("links")
        .ok_or_else(|| GraphError::Schema("missing 'links' key".to_owned()))?
        .as_array()
        .ok_or_else(|| GraphError::Schema("'links' must be an array".to_owned()))?;

    // community_id → member NodeIds (encounter order; sorted before building Community).
    let mut community_map: BTreeMap<u32, Vec<NodeId>> = BTreeMap::new();
    let mut nodes: Vec<Node> = Vec::with_capacity(nodes_arr.len());
    for (i, node_val) in nodes_arr.iter().enumerate() {
        let (node, opt_cid) = parse_node(i, node_val)?;
        if let Some(cid) = opt_cid {
            community_map.entry(cid).or_default().push(node.id);
        }
        nodes.push(node);
    }

    let mut edges: Vec<Edge> = Vec::with_capacity(links_arr.len());
    for (i, link_val) in links_arr.iter().enumerate() {
        edges.push(parse_edge(i, link_val)?);
    }

    // BTreeMap iteration is ascending by key, so communities are already ordered by id.
    let communities: Vec<Community> = community_map
        .into_iter()
        .map(|(cid, mut members)| {
            members.sort_unstable();
            Community {
                id: CommunityId::new(cid),
                label: format!("community-{cid}"),
                members,
            }
        })
        .collect();

    // Canonical sort: R4 determinism.
    nodes.sort_by_key(|n| n.id);
    edges.sort_by(|a, b| {
        (a.source, a.target, a.relation.as_str()).cmp(&(b.source, b.target, b.relation.as_str()))
    });

    Ok(Graph {
        schema: SCHEMA_VERSION.to_owned(),
        nodes,
        edges,
        communities,
        manifest: Manifest::default(),
    })
}

// ── private helpers ───────────────────────────────────────────────────────────

/// Parses a single node object from the `"nodes"` array.
///
/// Returns `(Node, Option<community_id>)`.  `i` is the zero-based array index used in error
/// messages.
///
/// # Errors
/// Returns [`GraphError::Schema`] on any type mismatch or missing required field.
fn parse_node(i: usize, v: &serde_json::Value) -> Result<(Node, Option<u32>)> {
    let raw_id = v
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| GraphError::Schema(format!("node[{i}]: missing or invalid 'id'")))?;
    let id = u32::try_from(raw_id)
        .map_err(|_| GraphError::Schema(format!("node[{i}]: 'id' {raw_id} exceeds u32 range")))?;

    let label = v
        .get("label")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| GraphError::Schema(format!("node[{i}]: missing or invalid 'label'")))?
        .to_owned();

    let source_file = v
        .get("source_file")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| GraphError::Schema(format!("node[{i}]: missing or invalid 'source_file'")))?
        .to_owned();

    let loc_str = v
        .get("source_location")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            GraphError::Schema(format!("node[{i}]: missing or invalid 'source_location'"))
        })?;

    // "Lnn" → strip leading 'L', parse to u32 (default 1 on failure).
    let line: u32 = loc_str
        .strip_prefix('L')
        .unwrap_or(loc_str)
        .parse::<u32>()
        .unwrap_or(1);

    let opt_cid = match v.get("community") {
        None | Some(serde_json::Value::Null) => None,
        Some(cid_val) => {
            let community_u64 = cid_val.as_u64().ok_or_else(|| {
                GraphError::Schema(format!(
                    "node[{i}]: 'community' must be a u32 integer or null"
                ))
            })?;
            let cid = u32::try_from(community_u64).map_err(|_| {
                GraphError::Schema(format!(
                    "node[{i}]: 'community' {community_u64} exceeds u32 range"
                ))
            })?;
            Some(cid)
        }
    };

    let node = Node {
        id: NodeId::new(id),
        label,
        source_file,
        source_location: Span::new(0, 0, line, line),
    };
    Ok((node, opt_cid))
}

/// Parses a single link object from the `"links"` array into an [`Edge`].
///
/// `i` is the zero-based array index used in error messages.
///
/// # Errors
/// Returns [`GraphError::Schema`] on any type mismatch, missing required field, or unknown
/// confidence string.
fn parse_edge(i: usize, v: &serde_json::Value) -> Result<Edge> {
    let raw_src = v
        .get("source")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| GraphError::Schema(format!("link[{i}]: missing or invalid 'source'")))?;
    let source_id = u32::try_from(raw_src).map_err(|_| {
        GraphError::Schema(format!("link[{i}]: 'source' {raw_src} exceeds u32 range"))
    })?;

    let raw_tgt = v
        .get("target")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| GraphError::Schema(format!("link[{i}]: missing or invalid 'target'")))?;
    let target_id = u32::try_from(raw_tgt).map_err(|_| {
        GraphError::Schema(format!("link[{i}]: 'target' {raw_tgt} exceeds u32 range"))
    })?;

    let relation = v
        .get("relation")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| GraphError::Schema(format!("link[{i}]: missing or invalid 'relation'")))?
        .to_owned();

    let conf_str = v
        .get("confidence")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| GraphError::Schema(format!("link[{i}]: missing or invalid 'confidence'")))?;

    let confidence = match conf_str {
        "EXTRACTED" => Confidence::Extracted,
        "INFERRED" => Confidence::Inferred,
        "AMBIGUOUS" => Confidence::Ambiguous,
        other => {
            return Err(GraphError::Schema(format!(
                "link[{i}]: unknown confidence {other:?}; \
                 expected EXTRACTED | INFERRED | AMBIGUOUS"
            )));
        }
    };

    // `weight` is in the wire format but is not part of the core Edge model.

    Ok(Edge {
        source: NodeId::new(source_id),
        target: NodeId::new(target_id),
        relation,
        confidence,
    })
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use habitat_graph_core::{Confidence, NodeId, Span, SCHEMA_VERSION};

    use super::from_node_link;

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Minimal valid 2-node / 1-link document used as the base fixture.
    fn minimal_json() -> &'static str {
        r#"{
            "directed": true,
            "multigraph": false,
            "graph": {},
            "nodes": [
                {"id": 1, "label": "Alpha", "source_file": "a.rs", "source_location": "L1",  "community": null},
                {"id": 2, "label": "Beta",  "source_file": "b.rs", "source_location": "L20", "community": null}
            ],
            "links": [
                {"source": 1, "target": 2, "relation": "calls", "confidence": "EXTRACTED", "weight": 1.0}
            ]
        }"#
    }

    // ── core parsing ─────────────────────────────────────────────────────────

    #[test]
    fn minimal_2node_1link_parses_to_correct_counts() {
        let g = from_node_link(minimal_json()).expect("minimal doc must parse");
        assert_eq!(g.nodes.len(), 2, "expected 2 nodes");
        assert_eq!(g.edges.len(), 1, "expected 1 edge");
    }

    #[test]
    fn schema_version_set_on_output() {
        let g = from_node_link(minimal_json()).expect("parse");
        assert_eq!(g.schema, SCHEMA_VERSION);
    }

    #[test]
    fn node_id_preserved() {
        let g = from_node_link(minimal_json()).expect("parse");
        let ids: Vec<u32> = g.nodes.iter().map(|n| n.id.get()).collect();
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
    }

    #[test]
    fn label_preserved() {
        let g = from_node_link(minimal_json()).expect("parse");
        let labels: Vec<&str> = g.nodes.iter().map(|n| n.label.as_str()).collect();
        assert!(labels.contains(&"Alpha"));
        assert!(labels.contains(&"Beta"));
    }

    #[test]
    fn source_file_preserved() {
        let g = from_node_link(minimal_json()).expect("parse");
        let files: Vec<&str> = g.nodes.iter().map(|n| n.source_file.as_str()).collect();
        assert!(files.contains(&"a.rs"));
        assert!(files.contains(&"b.rs"));
    }

    // ── source_location / Span ────────────────────────────────────────────────

    #[test]
    fn source_location_l20_sets_line_20() {
        let g = from_node_link(minimal_json()).expect("parse");
        // Node id=2 has "L20"
        let n = g
            .nodes
            .iter()
            .find(|n| n.id == NodeId::new(2))
            .expect("node 2");
        assert_eq!(n.source_location.start_line, 20);
        assert_eq!(n.source_location.end_line, 20);
    }

    #[test]
    fn source_location_byte_offsets_are_zero() {
        let g = from_node_link(minimal_json()).expect("parse");
        for n in &g.nodes {
            assert_eq!(n.source_location.start_byte, 0);
            assert_eq!(n.source_location.end_byte, 0);
        }
    }

    #[test]
    fn source_location_l1_produces_span_1_1() {
        let g = from_node_link(minimal_json()).expect("parse");
        let n = g
            .nodes
            .iter()
            .find(|n| n.id == NodeId::new(1))
            .expect("node 1");
        assert_eq!(n.source_location, Span::new(0, 0, 1, 1));
    }

    #[test]
    fn source_location_without_l_prefix_is_parsed_directly() {
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [{"id": 5, "label": "X", "source_file": "x.rs", "source_location": "42", "community": null}],
            "links": []
        }"#;
        let g = from_node_link(json).expect("parse");
        // "42" has no 'L' prefix — strip_prefix returns None, unwrap_or gives "42" → 42.
        assert_eq!(g.nodes[0].source_location.start_line, 42);
    }

    #[test]
    fn source_location_garbage_defaults_to_line_1() {
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [{"id": 9, "label": "X", "source_file": "x.rs", "source_location": "Lnotanumber", "community": null}],
            "links": []
        }"#;
        let g = from_node_link(json).expect("parse");
        assert_eq!(g.nodes[0].source_location.start_line, 1);
    }

    // ── confidence mapping ────────────────────────────────────────────────────

    #[test]
    fn confidence_extracted_parsed() {
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [
                {"id": 1, "label": "A", "source_file": "a.rs", "source_location": "L1", "community": null},
                {"id": 2, "label": "B", "source_file": "b.rs", "source_location": "L2", "community": null}
            ],
            "links": [{"source": 1, "target": 2, "relation": "r", "confidence": "EXTRACTED", "weight": 0.9}]
        }"#;
        let g = from_node_link(json).expect("parse");
        assert_eq!(g.edges[0].confidence, Confidence::Extracted);
    }

    #[test]
    fn confidence_inferred_parsed() {
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [
                {"id": 1, "label": "A", "source_file": "a.rs", "source_location": "L1", "community": null},
                {"id": 2, "label": "B", "source_file": "b.rs", "source_location": "L2", "community": null}
            ],
            "links": [{"source": 1, "target": 2, "relation": "r", "confidence": "INFERRED", "weight": 0.5}]
        }"#;
        let g = from_node_link(json).expect("parse");
        assert_eq!(g.edges[0].confidence, Confidence::Inferred);
    }

    #[test]
    fn confidence_ambiguous_parsed() {
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [
                {"id": 1, "label": "A", "source_file": "a.rs", "source_location": "L1", "community": null},
                {"id": 2, "label": "B", "source_file": "b.rs", "source_location": "L2", "community": null}
            ],
            "links": [{"source": 1, "target": 2, "relation": "r", "confidence": "AMBIGUOUS", "weight": 0.1}]
        }"#;
        let g = from_node_link(json).expect("parse");
        assert_eq!(g.edges[0].confidence, Confidence::Ambiguous);
    }

    #[test]
    fn unknown_confidence_returns_schema_error() {
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [
                {"id": 1, "label": "A", "source_file": "a.rs", "source_location": "L1", "community": null},
                {"id": 2, "label": "B", "source_file": "b.rs", "source_location": "L2", "community": null}
            ],
            "links": [{"source": 1, "target": 2, "relation": "r", "confidence": "MAYBE", "weight": 0.5}]
        }"#;
        let err = from_node_link(json).expect_err("unknown confidence must fail");
        assert!(
            matches!(err, habitat_graph_core::GraphError::Schema(_)),
            "expected Schema error, got: {err}"
        );
    }

    // ── community reconstruction ──────────────────────────────────────────────

    #[test]
    fn community_grouping_reconstructs_communities() {
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [
                {"id": 10, "label": "A", "source_file": "a.rs", "source_location": "L1", "community": 0},
                {"id": 20, "label": "B", "source_file": "b.rs", "source_location": "L2", "community": 0},
                {"id": 30, "label": "C", "source_file": "c.rs", "source_location": "L3", "community": 1}
            ],
            "links": []
        }"#;
        let g = from_node_link(json).expect("parse");
        assert_eq!(g.communities.len(), 2);
        let c0 = g
            .communities
            .iter()
            .find(|c| c.id.get() == 0)
            .expect("community 0");
        assert_eq!(c0.members.len(), 2);
        assert!(c0.members.contains(&NodeId::new(10)));
        assert!(c0.members.contains(&NodeId::new(20)));
        let c1 = g
            .communities
            .iter()
            .find(|c| c.id.get() == 1)
            .expect("community 1");
        assert_eq!(c1.members, vec![NodeId::new(30)]);
    }

    #[test]
    fn community_label_format() {
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [
                {"id": 1, "label": "A", "source_file": "a.rs", "source_location": "L1", "community": 7}
            ],
            "links": []
        }"#;
        let g = from_node_link(json).expect("parse");
        assert_eq!(g.communities[0].label, "community-7");
    }

    #[test]
    fn null_community_excludes_node_from_communities() {
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [
                {"id": 1, "label": "A", "source_file": "a.rs", "source_location": "L1", "community": null},
                {"id": 2, "label": "B", "source_file": "b.rs", "source_location": "L2", "community": 0}
            ],
            "links": []
        }"#;
        let g = from_node_link(json).expect("parse");
        // Only node 2 is in a community.
        assert_eq!(g.communities.len(), 1);
        let c = &g.communities[0];
        assert!(!c.members.contains(&NodeId::new(1)));
        assert!(c.members.contains(&NodeId::new(2)));
    }

    #[test]
    fn absent_community_field_excludes_node() {
        // Nodes without the 'community' key at all should not appear in any community.
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [
                {"id": 5, "label": "A", "source_file": "a.rs", "source_location": "L1"}
            ],
            "links": []
        }"#;
        let g = from_node_link(json).expect("parse");
        assert!(g.communities.is_empty());
    }

    #[test]
    fn community_members_sorted_ascending() {
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [
                {"id": 30, "label": "C", "source_file": "c.rs", "source_location": "L3", "community": 0},
                {"id": 10, "label": "A", "source_file": "a.rs", "source_location": "L1", "community": 0},
                {"id": 20, "label": "B", "source_file": "b.rs", "source_location": "L2", "community": 0}
            ],
            "links": []
        }"#;
        let g = from_node_link(json).expect("parse");
        let members: Vec<u32> = g.communities[0].members.iter().map(|n| n.get()).collect();
        assert_eq!(members, vec![10, 20, 30]);
    }

    #[test]
    fn multiple_communities_sorted_by_id() {
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [
                {"id": 1, "label": "A", "source_file": "a.rs", "source_location": "L1", "community": 5},
                {"id": 2, "label": "B", "source_file": "b.rs", "source_location": "L2", "community": 2},
                {"id": 3, "label": "C", "source_file": "c.rs", "source_location": "L3", "community": 9}
            ],
            "links": []
        }"#;
        let g = from_node_link(json).expect("parse");
        let cids: Vec<u32> = g.communities.iter().map(|c| c.id.get()).collect();
        assert_eq!(cids, vec![2, 5, 9]);
    }

    // ── error cases ───────────────────────────────────────────────────────────

    #[test]
    fn non_json_returns_schema_error() {
        let err = from_node_link("this is not json").expect_err("must fail");
        assert!(matches!(err, habitat_graph_core::GraphError::Schema(_)));
    }

    #[test]
    fn missing_nodes_key_returns_schema_error() {
        let json = r#"{"directed": false, "multigraph": false, "graph": {}, "links": []}"#;
        let err = from_node_link(json).expect_err("must fail");
        assert!(matches!(err, habitat_graph_core::GraphError::Schema(_)));
    }

    #[test]
    fn missing_links_key_returns_schema_error() {
        let json = r#"{"directed": false, "multigraph": false, "graph": {}, "nodes": []}"#;
        let err = from_node_link(json).expect_err("must fail");
        assert!(matches!(err, habitat_graph_core::GraphError::Schema(_)));
    }

    #[test]
    fn empty_nodes_and_links_returns_empty_graph() {
        let json =
            r#"{"directed": false, "multigraph": false, "graph": {}, "nodes": [], "links": []}"#;
        let g = from_node_link(json).expect("empty doc must parse");
        assert_eq!(g.nodes.len(), 0);
        assert_eq!(g.edges.len(), 0);
        assert_eq!(g.communities.len(), 0);
    }

    #[test]
    fn missing_node_id_returns_schema_error() {
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [{"label": "A", "source_file": "a.rs", "source_location": "L1", "community": null}],
            "links": []
        }"#;
        let err = from_node_link(json).expect_err("must fail on missing id");
        assert!(matches!(err, habitat_graph_core::GraphError::Schema(_)));
    }

    // ── ordering / determinism ────────────────────────────────────────────────

    #[test]
    fn nodes_sorted_by_id_ascending() {
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [
                {"id": 30, "label": "C", "source_file": "c.rs", "source_location": "L3", "community": null},
                {"id": 10, "label": "A", "source_file": "a.rs", "source_location": "L1", "community": null},
                {"id": 20, "label": "B", "source_file": "b.rs", "source_location": "L2", "community": null}
            ],
            "links": []
        }"#;
        let g = from_node_link(json).expect("parse");
        let ids: Vec<u32> = g.nodes.iter().map(|n| n.id.get()).collect();
        assert_eq!(ids, vec![10, 20, 30]);
    }

    #[test]
    fn edges_sorted_by_source_target_relation() {
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [
                {"id": 1, "label": "A", "source_file": "a.rs", "source_location": "L1", "community": null},
                {"id": 2, "label": "B", "source_file": "b.rs", "source_location": "L2", "community": null}
            ],
            "links": [
                {"source": 2, "target": 1, "relation": "uses",    "confidence": "INFERRED",  "weight": 0.5},
                {"source": 1, "target": 2, "relation": "imports",  "confidence": "EXTRACTED", "weight": 1.0},
                {"source": 1, "target": 2, "relation": "calls",    "confidence": "AMBIGUOUS", "weight": 0.3}
            ]
        }"#;
        let g = from_node_link(json).expect("parse");
        let got: Vec<(u32, u32, &str)> = g
            .edges
            .iter()
            .map(|e| (e.source.get(), e.target.get(), e.relation.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![(1, 2, "calls"), (1, 2, "imports"), (2, 1, "uses")]
        );
    }

    // ── edge fields ───────────────────────────────────────────────────────────

    #[test]
    fn edge_source_and_target_preserved() {
        let g = from_node_link(minimal_json()).expect("parse");
        assert_eq!(g.edges[0].source, NodeId::new(1));
        assert_eq!(g.edges[0].target, NodeId::new(2));
    }

    #[test]
    fn edge_relation_preserved() {
        let g = from_node_link(minimal_json()).expect("parse");
        assert_eq!(g.edges[0].relation, "calls");
    }

    #[test]
    fn weight_field_present_but_ignored() {
        // The wire format carries a `weight` float; the core Edge has no weight field.
        // Parsing must still succeed.
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [
                {"id": 1, "label": "A", "source_file": "a.rs", "source_location": "L1", "community": null},
                {"id": 2, "label": "B", "source_file": "b.rs", "source_location": "L2", "community": null}
            ],
            "links": [{"source": 1, "target": 2, "relation": "r", "confidence": "EXTRACTED", "weight": 0.987654321}]
        }"#;
        let g = from_node_link(json).expect("weight must not block parsing");
        assert_eq!(g.edges.len(), 1);
    }

    #[test]
    fn extra_envelope_fields_ignored() {
        // `directed`, `multigraph`, `graph` and any future top-level keys are silently ignored.
        let json = r#"{
            "directed": true,
            "multigraph": true,
            "graph": {"name": "test"},
            "extra_field": 99,
            "nodes": [],
            "links": []
        }"#;
        let g = from_node_link(json).expect("extra envelope fields must not block parsing");
        assert_eq!(g.nodes.len(), 0);
    }

    #[test]
    fn all_three_confidences_in_one_graph() {
        let json = r#"{
            "directed": false, "multigraph": false, "graph": {},
            "nodes": [
                {"id": 1, "label": "A", "source_file": "a.rs", "source_location": "L1", "community": null},
                {"id": 2, "label": "B", "source_file": "b.rs", "source_location": "L2", "community": null},
                {"id": 3, "label": "C", "source_file": "c.rs", "source_location": "L3", "community": null}
            ],
            "links": [
                {"source": 1, "target": 2, "relation": "r", "confidence": "EXTRACTED", "weight": 1.0},
                {"source": 2, "target": 3, "relation": "r", "confidence": "INFERRED",  "weight": 0.7},
                {"source": 1, "target": 3, "relation": "r", "confidence": "AMBIGUOUS", "weight": 0.2}
            ]
        }"#;
        let g = from_node_link(json).expect("parse");
        let confs: std::collections::HashSet<Confidence> =
            g.edges.iter().map(|e| e.confidence).collect();
        assert!(confs.contains(&Confidence::Extracted));
        assert!(confs.contains(&Confidence::Inferred));
        assert!(confs.contains(&Confidence::Ambiguous));
    }

    // ── round-trip with export (the primary contract) + error paths ─────────────

    #[test]
    fn round_trips_through_export() {
        // from_node_link must invert export::to_node_link (the format the CLI writes). Build a
        // graph, export to node-link, load it back, assert structural equality (line-lossy span).
        use habitat_graph_core::{Community, CommunityId, Edge, Graph, Node};
        let mut g = Graph::new();
        g.nodes.push(Node {
            id: NodeId::new(0),
            label: "alpha".to_owned(),
            source_file: "a.rs".to_owned(),
            source_location: Span::new(0, 0, 3, 3),
        });
        g.nodes.push(Node {
            id: NodeId::new(1),
            label: "beta".to_owned(),
            source_file: "b.rs".to_owned(),
            source_location: Span::new(0, 0, 7, 7),
        });
        g.edges.push(Edge {
            source: NodeId::new(0),
            target: NodeId::new(1),
            relation: "calls".to_owned(),
            confidence: Confidence::Inferred,
        });
        g.communities.push(Community {
            id: CommunityId::new(0),
            label: "community-0".to_owned(),
            members: vec![NodeId::new(0), NodeId::new(1)],
        });
        let g = g.sorted();

        let json = habitat_graph_export::to_node_link(&g).expect("export");
        let loaded = from_node_link(&json).expect("load");

        assert_eq!(loaded.nodes.len(), g.nodes.len());
        for (a, b) in g.nodes.iter().zip(loaded.nodes.iter()) {
            assert_eq!(
                (a.id, a.label.as_str(), a.source_file.as_str()),
                (b.id, b.label.as_str(), b.source_file.as_str())
            );
            assert_eq!(a.source_location.start_line, b.source_location.start_line);
        }
        assert_eq!(loaded.edges.len(), g.edges.len());
        for (a, b) in g.edges.iter().zip(loaded.edges.iter()) {
            assert_eq!(
                (a.source, a.target, a.relation.as_str(), a.confidence),
                (b.source, b.target, b.relation.as_str(), b.confidence)
            );
        }
        assert_eq!(loaded.communities, g.communities);
    }

    #[test]
    fn nodes_not_an_array_is_schema_error() {
        let json = r#"{"nodes": "oops", "links": []}"#;
        assert!(matches!(
            from_node_link(json),
            Err(habitat_graph_core::GraphError::Schema(_))
        ));
    }

    #[test]
    fn node_missing_label_is_schema_error() {
        let json = r#"{"nodes": [{"id": 0, "source_file": "a.rs", "source_location": "L1"}], "links": []}"#;
        assert!(
            from_node_link(json).is_err(),
            "a node without a label must fail"
        );
    }
}
