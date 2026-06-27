//! Pure endpoint logic (sync, fully testable) — the daemon's HTTP handlers reduce to these.
//!
//! All three functions are intentionally sync and accept a plain `&Graph` so they can be exercised
//! in unit tests without spinning up an HTTP server. The [`crate::server`] module calls them from
//! within async axum handlers after extracting state.

use habitat_graph_core::Graph;
use serde_json::{json, Value};

/// `GET /health` body: tool version + graph `(nodes, edges, communities)` counts.
///
/// Returns a JSON object with four keys:
/// - `"version"` — the crate version from `CARGO_PKG_VERSION`
/// - `"nodes"` — total node count
/// - `"edges"` — total edge count
/// - `"communities"` — total detected-community count
#[must_use]
pub fn health_json(graph: &Graph) -> Value {
    let (nodes, edges, communities) = graph.counts();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "nodes": nodes,
        "edges": edges,
        "communities": communities,
    })
}

/// `GET /query?q=` body: the nodes whose label contains `q`.
///
/// Delegates to [`habitat_graph_serve::find_by_label`] for case-insensitive substring matching.
/// An empty `q` matches every node.
///
/// Returns a JSON object with three keys:
/// - `"query"` — the raw query string
/// - `"count"` — total number of matching nodes
/// - `"matches"` — array of `{"id", "label", "source_file"}` objects, one per match
///
/// # Errors
/// This function is infallible; it always returns a valid `Value`.
#[must_use]
pub fn query_json(graph: &Graph, q: &str) -> Value {
    let matches = habitat_graph_serve::find_by_label(graph, q);
    let match_array: Vec<Value> = matches
        .iter()
        .map(|n| {
            json!({
                "id": n.id.get(),
                "label": n.label,
                "source_file": n.source_file,
            })
        })
        .collect();
    let count = match_array.len();
    json!({
        "query": q,
        "count": count,
        "matches": match_array,
    })
}

/// `GET /path?from=&to=` body: the shortest-path node labels.
///
/// Delegates to [`habitat_graph_serve::shortest_path`] for BFS over the (undirected) graph.
/// Each [`habitat_graph_core::NodeId`] in the returned path is resolved to its node's label
/// string. If a node id has no corresponding entry in `graph.nodes` it is silently skipped, so
/// the path array may be shorter than the id sequence in pathological graphs.
///
/// Returns a JSON object with four keys:
/// - `"from"` — the source label
/// - `"to"` — the destination label
/// - `"found"` — `true` if a path exists
/// - `"path"` — array of label strings along the path (empty when `found` is `false`)
///
/// # Errors
/// This function is infallible; it always returns a valid `Value`.
#[must_use]
pub fn path_json(graph: &Graph, from: &str, to: &str) -> Value {
    if let Some(path) = habitat_graph_serve::shortest_path(graph, from, to) {
        let labels: Vec<&str> = path
            .iter()
            .filter_map(|node_id| {
                graph
                    .nodes
                    .iter()
                    .find(|n| n.id == *node_id)
                    .map(|n| n.label.as_str())
            })
            .collect();
        json!({
            "from": from,
            "to": to,
            "found": true,
            "path": labels,
        })
    } else {
        json!({
            "from": from,
            "to": to,
            "found": false,
            "path": Vec::<&str>::new(),
        })
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{health_json, path_json, query_json};
    use habitat_graph_core::{
        Community, CommunityId, Confidence, Edge, Graph, Manifest, Node, NodeId, Span,
        SCHEMA_VERSION,
    };

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_node(id: u32, label: &str, source_file: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: source_file.to_owned(),
            source_location: Span::new(0, 1, 1, 1),
        }
    }

    fn make_edge(src: u32, tgt: u32) -> Edge {
        Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: "depends".to_owned(),
            confidence: Confidence::Extracted,
        }
    }

    fn make_graph(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        Graph {
            schema: SCHEMA_VERSION.to_owned(),
            nodes,
            edges,
            communities: Vec::new(),
            manifest: Manifest::default(),
        }
    }

    // ── health_json ───────────────────────────────────────────────────────────

    /// Empty graph produces zero counts and the "version" key is present and non-empty.
    #[test]
    fn health_empty_graph_all_zero_counts() {
        let g = Graph::new();
        let v = health_json(&g);
        assert_eq!(v["nodes"], 0);
        assert_eq!(v["edges"], 0);
        assert_eq!(v["communities"], 0);
    }

    /// The `health_json` response carries a "version" field that is a non-empty string.
    #[test]
    fn health_version_is_nonempty_string() {
        let g = Graph::new();
        let v = health_json(&g);
        let version = v["version"].as_str().expect("version must be a string");
        assert!(!version.is_empty(), "CARGO_PKG_VERSION must not be empty");
    }

    /// `health_json` version matches the compile-time `CARGO_PKG_VERSION`.
    #[test]
    fn health_version_matches_cargo_pkg_version() {
        let g = Graph::new();
        let v = health_json(&g);
        assert_eq!(v["version"].as_str().unwrap(), env!("CARGO_PKG_VERSION"));
    }

    /// `health_json` counts reflect graph contents accurately.
    #[test]
    fn health_counts_reflect_graph_contents() {
        let mut g = make_graph(
            vec![
                make_node(1, "alpha", "a.rs"),
                make_node(2, "beta", "b.rs"),
                make_node(3, "gamma", "c.rs"),
            ],
            vec![make_edge(1, 2), make_edge(2, 3)],
        );
        g.communities.push(Community {
            id: CommunityId::new(0),
            label: "cluster".to_owned(),
            members: vec![NodeId::new(1), NodeId::new(2)],
        });
        let v = health_json(&g);
        assert_eq!(v["nodes"], 3);
        assert_eq!(v["edges"], 2);
        assert_eq!(v["communities"], 1);
    }

    /// `health_json` output is a JSON object (not null / array / scalar).
    #[test]
    fn health_output_is_object() {
        let v = health_json(&Graph::new());
        assert!(v.is_object());
    }

    // ── query_json ────────────────────────────────────────────────────────────

    /// `query_json` with a matching prefix returns count >= 1 and the matching node in matches.
    #[test]
    fn query_prefix_match_returns_result() {
        let g = make_graph(vec![make_node(1, "alpha", "a.rs")], vec![]);
        let v = query_json(&g, "alp");
        assert!(v["count"].as_u64().unwrap() >= 1);
        let matches = v["matches"].as_array().unwrap();
        let labels: Vec<&str> = matches
            .iter()
            .map(|m| m["label"].as_str().unwrap())
            .collect();
        assert!(
            labels.contains(&"alpha"),
            "\"alpha\" must appear in matches"
        );
    }

    /// `query_json` with no matching needle returns count=0 and empty matches array.
    #[test]
    fn query_no_match_returns_zero_count_and_empty_array() {
        let g = make_graph(
            vec![make_node(1, "alpha", "a.rs"), make_node(2, "beta", "b.rs")],
            vec![],
        );
        let v = query_json(&g, "zzz_not_present");
        assert_eq!(v["count"], 0);
        assert_eq!(v["matches"].as_array().unwrap().len(), 0);
    }

    /// `query_json` echoes the query string in the "query" key.
    #[test]
    fn query_echoes_needle_in_output() {
        let g = make_graph(vec![make_node(1, "foo", "f.rs")], vec![]);
        let v = query_json(&g, "my_query");
        assert_eq!(v["query"].as_str().unwrap(), "my_query");
    }

    /// Each match object contains `"id"`, `"label"`, and `"source_file"` fields.
    #[test]
    fn query_match_objects_have_required_fields() {
        let g = make_graph(vec![make_node(7, "node_seven", "seven.rs")], vec![]);
        let v = query_json(&g, "seven");
        let matches = v["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        let m = &matches[0];
        assert_eq!(m["id"].as_u64().unwrap(), 7_u64);
        assert_eq!(m["label"].as_str().unwrap(), "node_seven");
        assert_eq!(m["source_file"].as_str().unwrap(), "seven.rs");
    }

    /// The `count` field equals the actual length of the matches array.
    #[test]
    fn query_count_equals_matches_len() {
        let g = make_graph(
            vec![
                make_node(1, "prefix_one", "a.rs"),
                make_node(2, "prefix_two", "b.rs"),
                make_node(3, "other", "c.rs"),
            ],
            vec![],
        );
        let v = query_json(&g, "prefix");
        let count = usize::try_from(v["count"].as_u64().unwrap()).unwrap();
        let arr_len = v["matches"].as_array().unwrap().len();
        assert_eq!(count, arr_len);
        assert_eq!(count, 2);
    }

    /// Empty needle matches all nodes in the graph.
    #[test]
    fn query_empty_needle_matches_all_nodes() {
        let g = make_graph(
            vec![
                make_node(1, "alpha", "a.rs"),
                make_node(2, "beta", "b.rs"),
                make_node(3, "gamma", "c.rs"),
            ],
            vec![],
        );
        let v = query_json(&g, "");
        assert_eq!(v["count"].as_u64().unwrap(), 3_u64);
    }

    /// `query_json` on an empty graph always returns count=0.
    #[test]
    fn query_empty_graph_returns_zero() {
        let g = Graph::new();
        let v = query_json(&g, "anything");
        assert_eq!(v["count"], 0);
        assert!(v["matches"].as_array().unwrap().is_empty());
    }

    // ── path_json ─────────────────────────────────────────────────────────────

    /// Connected pair produces `found=true` and a non-empty path array containing both labels.
    #[test]
    fn path_connected_pair_found_true_nonempty() {
        let g = make_graph(
            vec![make_node(1, "alpha", "a.rs"), make_node(2, "beta", "b.rs")],
            vec![make_edge(1, 2)],
        );
        let v = path_json(&g, "alpha", "beta");
        assert_eq!(v["found"], true);
        assert_eq!(v["from"].as_str().unwrap(), "alpha");
        assert_eq!(v["to"].as_str().unwrap(), "beta");
        let path = v["path"].as_array().unwrap();
        assert!(!path.is_empty());
    }

    /// Path labels for a direct edge are exactly `["alpha", "beta"]`.
    #[test]
    fn path_direct_edge_labels_correct() {
        let g = make_graph(
            vec![make_node(1, "alpha", "a.rs"), make_node(2, "beta", "b.rs")],
            vec![make_edge(1, 2)],
        );
        let v = path_json(&g, "alpha", "beta");
        let labels: Vec<&str> = v["path"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(labels, vec!["alpha", "beta"]);
    }

    /// Disconnected nodes: `found=false` and path is an empty array.
    #[test]
    fn path_disconnected_found_false_empty_path() {
        let g = make_graph(
            vec![
                make_node(1, "island_a", "a.rs"),
                make_node(2, "island_b", "b.rs"),
            ],
            vec![], // no edges
        );
        let v = path_json(&g, "island_a", "island_b");
        assert_eq!(v["found"], false);
        assert!(v["path"].as_array().unwrap().is_empty());
    }

    /// Missing `"from"` label: `found=false` and path is empty.
    #[test]
    fn path_missing_from_label_found_false() {
        let g = make_graph(vec![make_node(1, "existing", "e.rs")], vec![]);
        let v = path_json(&g, "ghost", "existing");
        assert_eq!(v["found"], false);
        assert!(v["path"].as_array().unwrap().is_empty());
    }

    /// Missing `"to"` label: `found=false` and path is empty.
    #[test]
    fn path_missing_to_label_found_false() {
        let g = make_graph(vec![make_node(1, "existing", "e.rs")], vec![]);
        let v = path_json(&g, "existing", "ghost");
        assert_eq!(v["found"], false);
        assert!(v["path"].as_array().unwrap().is_empty());
    }

    /// When `from == to` and the node exists, `found=true` with a single-element path.
    #[test]
    fn path_same_label_single_element_path() {
        let g = make_graph(vec![make_node(5, "self_ref", "s.rs")], vec![]);
        let v = path_json(&g, "self_ref", "self_ref");
        assert_eq!(v["found"], true);
        let path = v["path"].as_array().unwrap();
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].as_str().unwrap(), "self_ref");
    }

    /// `path_json` echoes `"from"` and `"to"` regardless of success.
    #[test]
    fn path_echoes_from_and_to_on_failure() {
        let g = Graph::new();
        let v = path_json(&g, "A", "B");
        assert_eq!(v["from"].as_str().unwrap(), "A");
        assert_eq!(v["to"].as_str().unwrap(), "B");
    }

    /// Multi-hop path through an intermediate node: labels appear in order.
    #[test]
    fn path_multi_hop_labels_in_order() {
        let g = make_graph(
            vec![
                make_node(1, "start", "a.rs"),
                make_node(2, "middle", "b.rs"),
                make_node(3, "end", "c.rs"),
            ],
            vec![make_edge(1, 2), make_edge(2, 3)],
        );
        let v = path_json(&g, "start", "end");
        assert_eq!(v["found"], true);
        let labels: Vec<&str> = v["path"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(labels, vec!["start", "middle", "end"]);
    }

    /// `path_json` on a completely empty graph returns `found=false` and empty path.
    #[test]
    fn path_empty_graph_found_false() {
        let g = Graph::new();
        let v = path_json(&g, "any", "thing");
        assert_eq!(v["found"], false);
        assert!(v["path"].as_array().unwrap().is_empty());
    }

    /// Reverse traversal: path found even against the directed edge direction (BFS is undirected).
    #[test]
    fn path_reverse_direction_found() {
        // Directed A(1)→B(2), query B→A: BFS treats edges as undirected.
        let g = make_graph(
            vec![make_node(1, "src", "s.rs"), make_node(2, "dst", "d.rs")],
            vec![make_edge(1, 2)],
        );
        let v = path_json(&g, "dst", "src");
        assert_eq!(v["found"], true);
        let labels: Vec<&str> = v["path"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(labels, vec!["dst", "src"]);
    }
}
