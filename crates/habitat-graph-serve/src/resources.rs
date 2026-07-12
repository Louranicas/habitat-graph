//! MCP resources + templates (FO-1) — the agent-readable views of the graph.
//!
//! Exposes two concrete resources and two URI templates over the MCP `resources/list` and
//! `resources/read` verbs. All rendering is purely in-memory — no filesystem, network, or
//! process I/O occurs here. Every label that reaches a render boundary is passed through
//! [`display_safe`] to prevent Trojan-Source / bidirectional-control-character injection
//! (CVE-2021-42574).
//!
//! The URI `{label}` / `{id}` path segments are **logical keys** used to filter the in-memory
//! [`Graph`]; they never influence file paths, system calls, or process spawning.

use std::fmt::Write as _;

use habitat_graph_core::{display_safe, CommunityId, Graph, NodeId, SCHEMA_VERSION};
use serde_json::{json, Value};

use crate::query::find_by_label;

// ── URI constants ────────────────────────────────────────────────────────────

/// Exact URI of the schema resource.
const SCHEMA_URI: &str = "habitat-graph://schema";
/// Exact URI of the report resource.
const REPORT_URI: &str = "habitat-graph://report";
/// URI prefix for node template instances (`habitat-graph://node/{label}`).
const NODE_PREFIX: &str = "habitat-graph://node/";
/// URI prefix for community template instances (`habitat-graph://community/{id}`).
const COMMUNITY_PREFIX: &str = "habitat-graph://community/";

/// Maximum node labels included in the report's sample section.
const REPORT_SAMPLE_SIZE: usize = 20;

// ── Public API ───────────────────────────────────────────────────────────────

/// Returns the MCP `resources/list` payload — concrete resources + URI templates.
///
/// Returns a JSON object with two keys:
/// - `resources`: concrete, fully-resolved URIs the client may read immediately.
/// - `resourceTemplates`: parameterised URI patterns the client instantiates.
///
/// The `report` description embeds live counts from `graph` so the client sees
/// the graph shape before issuing a full `resources/read`.
#[must_use]
pub fn resources_list(graph: &Graph) -> Value {
    let (n, e, c) = graph.counts();
    json!({
        "resources": [
            {
                "uri": REPORT_URI,
                "name": "report",
                "description": format!("Graph summary ({n} nodes, {e} edges, {c} communities)"),
                "mimeType": "text/markdown"
            },
            {
                "uri": SCHEMA_URI,
                "name": "schema",
                "description": "The graph.json schema version + envelope shape",
                "mimeType": "application/json"
            }
        ],
        "resourceTemplates": [
            {
                "uriTemplate": "habitat-graph://node/{label}",
                "name": "node",
                "description": "A node's detail by label",
                "mimeType": "text/markdown"
            },
            {
                "uriTemplate": "habitat-graph://community/{id}",
                "name": "community",
                "description": "A community's members by id",
                "mimeType": "text/markdown"
            }
        ]
    })
}

/// Returns the MCP `resources/read` `contents` payload for `uri`.
///
/// Recognised forms:
/// - `habitat-graph://schema` — JSON describing the schema version and envelope shape.
/// - `habitat-graph://report` — Markdown graph summary (counts + label sample).
/// - `habitat-graph://node/{label}` — Markdown detail for every node whose label
///   contains `{label}` (case-insensitive substring match).
/// - `habitat-graph://community/{id}` — Markdown listing of a community's members.
///
/// The `{label}` and `{id}` segments are **logical keys** only; no filesystem or
/// network access occurs. Every label in rendered output passes through [`display_safe`].
///
/// # Errors
///
/// Returns `Err(message)` when:
/// - The URI does not match any known resource or template pattern.
/// - The `{label}` segment is empty.
/// - The `{id}` segment is not a valid `u32` integer.
/// - The community id does not exist in `graph`.
/// - No node in `graph` matches the supplied label.
pub fn resources_read(graph: &Graph, uri: &str) -> Result<Value, String> {
    if uri == SCHEMA_URI {
        render_schema()
    } else if uri == REPORT_URI {
        Ok(render_report(graph))
    } else if let Some(label) = uri.strip_prefix(NODE_PREFIX) {
        render_node(graph, label)
    } else if let Some(id_str) = uri.strip_prefix(COMMUNITY_PREFIX) {
        render_community(graph, id_str)
    } else {
        Err(format!("unknown resource: {uri}"))
    }
}

// ── Private rendering ────────────────────────────────────────────────────────

/// Renders `habitat-graph://schema` as a JSON-text contents envelope.
fn render_schema() -> Result<Value, String> {
    let payload = json!({
        "schema_version": SCHEMA_VERSION,
        "envelope": "NetworkX node-link: directed,multigraph,graph,nodes,links"
    });
    let text =
        serde_json::to_string(&payload).map_err(|e| format!("schema serialisation failed: {e}"))?;
    Ok(contents(SCHEMA_URI, "application/json", &text))
}

/// Renders `habitat-graph://report` as a Markdown contents envelope.
///
/// Includes node/edge/community counts and up to [`REPORT_SAMPLE_SIZE`] node labels
/// sorted by [`NodeId`] (R4 determinism). All labels pass through [`display_safe`].
fn render_report(graph: &Graph) -> Value {
    let (n, e, c) = graph.counts();
    let mut buf = String::new();
    let _ = writeln!(buf, "# habitat-graph — Graph Report\n");
    let _ = writeln!(buf, "| Metric | Count |");
    let _ = writeln!(buf, "| --- | --- |");
    let _ = writeln!(buf, "| Nodes | {n} |");
    let _ = writeln!(buf, "| Edges | {e} |");
    let _ = writeln!(buf, "| Communities | {c} |");
    let _ = writeln!(buf);

    if n == 0 {
        let _ = writeln!(buf, "_Graph is empty._");
    } else {
        let _ = writeln!(buf, "## Node Sample (up to {REPORT_SAMPLE_SIZE})\n");
        // Sort by NodeId for R4 determinism.
        let mut nodes: Vec<_> = graph.nodes.iter().collect();
        nodes.sort_by_key(|nd| nd.id);
        for node in nodes.into_iter().take(REPORT_SAMPLE_SIZE) {
            let safe_label = display_safe(&node.label);
            let _ = writeln!(
                buf,
                "- `{safe_label}` (`{}:{}`)",
                display_safe(&node.source_file),
                node.source_location.start_line
            );
        }
        if n > REPORT_SAMPLE_SIZE {
            let _ = writeln!(buf, "\n_… and {} more node(s)._", n - REPORT_SAMPLE_SIZE);
        }
    }

    contents(REPORT_URI, "text/markdown", &buf)
}

/// Renders `habitat-graph://node/{label}` as a Markdown contents envelope.
///
/// Calls [`find_by_label`] (case-insensitive substring) and renders each matched
/// node's source location plus outbound/inbound edges. All labels and edge relations
/// pass through [`display_safe`].
fn render_node(graph: &Graph, label: &str) -> Result<Value, String> {
    if label.is_empty() {
        return Err("node template requires a non-empty label segment".to_owned());
    }

    let uri = format!("{NODE_PREFIX}{label}");
    let matches = find_by_label(graph, label);

    if matches.is_empty() {
        return Err(format!("no node found matching label: {label:?}"));
    }

    let mut buf = String::new();
    let _ = writeln!(buf, "# Nodes matching `{}`\n", display_safe(label));

    for &node in &matches {
        let safe_label = display_safe(&node.label);
        let _ = writeln!(buf, "## `{safe_label}`\n");
        let _ = writeln!(
            buf,
            "- **File:** `{}:{}`",
            display_safe(&node.source_file),
            node.source_location.start_line
        );

        // Outbound: source == node.id.  Sort by (target, relation) for R4 determinism.
        let mut outbound: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.source == node.id)
            .collect();
        outbound
            .sort_by(|a, b| (a.target, a.relation.as_str()).cmp(&(b.target, b.relation.as_str())));

        if outbound.is_empty() {
            let _ = writeln!(buf, "- **Outbound edges:** none");
        } else {
            let _ = writeln!(buf, "- **Outbound edges:**");
            for edge in &outbound {
                let _ = writeln!(
                    buf,
                    "  - `{}` \u{2192} `{}`",
                    display_safe(&edge.relation),
                    display_safe(&node_label(graph, edge.target))
                );
            }
        }

        // Inbound: target == node.id.  Sort by (source, relation) for R4 determinism.
        let mut inbound: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.target == node.id)
            .collect();
        inbound
            .sort_by(|a, b| (a.source, a.relation.as_str()).cmp(&(b.source, b.relation.as_str())));

        if inbound.is_empty() {
            let _ = writeln!(buf, "- **Inbound edges:** none");
        } else {
            let _ = writeln!(buf, "- **Inbound edges:**");
            for edge in &inbound {
                let _ = writeln!(
                    buf,
                    "  - `{}` \u{2190} `{}`",
                    display_safe(&edge.relation),
                    display_safe(&node_label(graph, edge.source))
                );
            }
        }
        let _ = writeln!(buf);
    }

    Ok(contents(&uri, "text/markdown", &buf))
}

/// Renders `habitat-graph://community/{id}` as a Markdown contents envelope.
///
/// Parses `id_str` as a `u32`, locates the matching community, and lists its
/// members in ascending [`NodeId`] order (R4 determinism). All labels pass through
/// [`display_safe`].
fn render_community(graph: &Graph, id_str: &str) -> Result<Value, String> {
    let raw: u32 = id_str
        .trim()
        .parse()
        .map_err(|_| format!("community id must be a non-negative integer, got: {id_str:?}"))?;
    let cid = CommunityId::new(raw);
    let community = graph
        .communities
        .iter()
        .find(|c| c.id == cid)
        .ok_or_else(|| format!("community {raw} not found"))?;

    let uri = format!("{COMMUNITY_PREFIX}{raw}");
    let safe_label = display_safe(&community.label);
    let mut buf = String::new();
    let _ = writeln!(buf, "# Community {raw}: `{safe_label}`\n");

    // Sort members for R4 determinism (the stored Vec may be unsorted).
    let mut members: Vec<NodeId> = community.members.clone();
    members.sort_unstable();

    if members.is_empty() {
        let _ = writeln!(buf, "_No members._");
    } else {
        let _ = writeln!(buf, "## Members\n");
        for &mid in &members {
            let _ = writeln!(
                buf,
                "- `{}` (node {})",
                display_safe(&node_label(graph, mid)),
                mid.get()
            );
        }
    }

    Ok(contents(&uri, "text/markdown", &buf))
}

/// Returns a node's label by [`NodeId`], or a placeholder when the node is absent.
fn node_label(graph: &Graph, id: NodeId) -> String {
    graph
        .nodes
        .iter()
        .find(|n| n.id == id)
        .map_or_else(|| format!("<unknown:{}>", id.get()), |n| n.label.clone())
}

/// Wraps `text` in the MCP `resources/read` contents envelope.
fn contents(uri: &str, mime: &str, text: &str) -> Value {
    json!({ "contents": [ { "uri": uri, "mimeType": mime, "text": text } ] })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use habitat_graph_core::{
        Community, CommunityId, Confidence, Edge, Graph, Manifest, Node, NodeId, Span,
        SCHEMA_VERSION,
    };
    use serde_json::Value;

    use super::{
        resources_list, resources_read, COMMUNITY_PREFIX, NODE_PREFIX, REPORT_SAMPLE_SIZE,
        SCHEMA_URI,
    };

    // ── helpers ───────────────────────────────────────────────────────────────

    fn node(id: u32, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: "src/lib.rs".to_owned(),
            source_location: Span::new(0, 10, id.max(1), id.max(1)),
        }
    }

    fn edge(src: u32, tgt: u32, rel: &str) -> Edge {
        Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: rel.to_owned(),
            confidence: Confidence::Extracted,
        }
    }

    fn community(id: u32, label: &str, members: Vec<u32>) -> Community {
        Community {
            id: CommunityId::new(id),
            label: label.to_owned(),
            members: members.into_iter().map(NodeId::new).collect(),
        }
    }

    fn sample_graph() -> Graph {
        let mut g = Graph {
            schema: SCHEMA_VERSION.to_owned(),
            nodes: vec![node(1, "Alpha"), node(2, "Beta"), node(3, "Gamma")],
            edges: vec![edge(1, 2, "calls"), edge(2, 3, "imports")],
            communities: Vec::new(),
            manifest: Manifest::default(),
        };
        g.communities.push(community(0, "Cluster", vec![1, 2]));
        g
    }

    fn text_of(v: &Value) -> &str {
        v["contents"][0]["text"].as_str().expect("text field")
    }

    fn mime_of(v: &Value) -> &str {
        v["contents"][0]["mimeType"]
            .as_str()
            .expect("mimeType field")
    }

    fn uri_of(v: &Value) -> &str {
        v["contents"][0]["uri"].as_str().expect("uri field")
    }

    // ── resources_list: shape ─────────────────────────────────────────────────

    #[test]
    fn list_has_resources_key() {
        assert!(resources_list(&Graph::new())["resources"].is_array());
    }

    #[test]
    fn list_has_resource_templates_key() {
        assert!(resources_list(&Graph::new())["resourceTemplates"].is_array());
    }

    #[test]
    fn list_resources_has_exactly_two_entries() {
        assert_eq!(
            resources_list(&Graph::new())["resources"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn list_templates_has_exactly_two_entries() {
        assert_eq!(
            resources_list(&Graph::new())["resourceTemplates"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn list_advertises_report_and_schema() {
        let v = resources_list(&Graph::new());
        let uris: Vec<&str> = v["resources"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["uri"].as_str())
            .collect();
        assert!(uris.contains(&"habitat-graph://report"));
        assert!(uris.contains(&"habitat-graph://schema"));
    }

    #[test]
    fn list_advertises_node_and_community_templates() {
        let v = resources_list(&Graph::new());
        let tpls: Vec<&str> = v["resourceTemplates"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["uriTemplate"].as_str())
            .collect();
        assert!(tpls.iter().any(|t| t.contains("node/{label}")));
        assert!(tpls.iter().any(|t| t.contains("community/{id}")));
    }

    #[test]
    fn list_report_mime_is_text_markdown() {
        let v = resources_list(&Graph::new());
        let report = v["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["uri"] == "habitat-graph://report")
            .expect("report resource");
        assert_eq!(report["mimeType"], "text/markdown");
    }

    #[test]
    fn list_schema_mime_is_application_json() {
        let v = resources_list(&Graph::new());
        let schema_res = v["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["uri"] == SCHEMA_URI)
            .expect("schema resource");
        assert_eq!(schema_res["mimeType"], "application/json");
    }

    #[test]
    fn list_node_template_mime_is_text_markdown() {
        let v = resources_list(&Graph::new());
        let tpl = v["resourceTemplates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["uriTemplate"].as_str().unwrap_or("").contains("node/"))
            .expect("node template");
        assert_eq!(tpl["mimeType"], "text/markdown");
    }

    #[test]
    fn list_community_template_mime_is_text_markdown() {
        let v = resources_list(&Graph::new());
        let tpl = v["resourceTemplates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| {
                r["uriTemplate"]
                    .as_str()
                    .unwrap_or("")
                    .contains("community/")
            })
            .expect("community template");
        assert_eq!(tpl["mimeType"], "text/markdown");
    }

    #[test]
    fn list_description_includes_node_count() {
        let g = sample_graph(); // 3 nodes
        let v = resources_list(&g);
        let desc = v["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["uri"] == "habitat-graph://report")
            .expect("report")["description"]
            .as_str()
            .expect("description");
        assert!(desc.contains("3 nodes"), "{desc}");
    }

    #[test]
    fn list_description_reflects_empty_graph() {
        let v = resources_list(&Graph::new());
        let desc = v["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["uri"] == "habitat-graph://report")
            .expect("report")["description"]
            .as_str()
            .expect("description");
        assert!(desc.contains("0 nodes"), "{desc}");
    }

    // ── resources_read: schema ────────────────────────────────────────────────

    #[test]
    fn read_schema_returns_ok() {
        assert!(resources_read(&Graph::new(), "habitat-graph://schema").is_ok());
    }

    #[test]
    fn read_schema_contents_envelope_has_one_entry() {
        let v = resources_read(&Graph::new(), "habitat-graph://schema").unwrap();
        assert_eq!(v["contents"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn read_schema_uri_in_envelope_matches() {
        let v = resources_read(&Graph::new(), "habitat-graph://schema").unwrap();
        assert_eq!(uri_of(&v), "habitat-graph://schema");
    }

    #[test]
    fn read_schema_mime_is_application_json() {
        let v = resources_read(&Graph::new(), "habitat-graph://schema").unwrap();
        assert_eq!(mime_of(&v), "application/json");
    }

    #[test]
    fn read_schema_text_contains_schema_version() {
        let v = resources_read(&Graph::new(), "habitat-graph://schema").unwrap();
        assert!(
            text_of(&v).contains(SCHEMA_VERSION),
            "SCHEMA_VERSION missing"
        );
        assert!(text_of(&v).contains("habitat-graph.graph"));
    }

    #[test]
    fn read_schema_text_has_envelope_key() {
        let v = resources_read(&Graph::new(), "habitat-graph://schema").unwrap();
        let text = text_of(&v);
        assert!(text.contains("envelope"), "envelope key missing");
        assert!(text.contains("NetworkX"), "NetworkX missing");
    }

    #[test]
    fn read_schema_text_is_valid_json() {
        let v = resources_read(&Graph::new(), "habitat-graph://schema").unwrap();
        let parsed: serde_json::Result<Value> = serde_json::from_str(text_of(&v));
        assert!(parsed.is_ok(), "schema text is not valid JSON");
    }

    // ── resources_read: report ────────────────────────────────────────────────

    #[test]
    fn read_report_returns_ok() {
        assert!(resources_read(&Graph::new(), "habitat-graph://report").is_ok());
    }

    #[test]
    fn read_report_contents_envelope_has_one_entry() {
        let v = resources_read(&Graph::new(), "habitat-graph://report").unwrap();
        assert_eq!(v["contents"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn read_report_uri_in_envelope_matches() {
        let v = resources_read(&Graph::new(), "habitat-graph://report").unwrap();
        assert_eq!(uri_of(&v), "habitat-graph://report");
    }

    #[test]
    fn read_report_mime_is_text_markdown() {
        let v = resources_read(&Graph::new(), "habitat-graph://report").unwrap();
        assert_eq!(mime_of(&v), "text/markdown");
    }

    #[test]
    fn read_report_includes_node_metric() {
        let v = resources_read(&sample_graph(), "habitat-graph://report").unwrap();
        let text = text_of(&v);
        assert!(
            text.contains("| Nodes | 3 |"),
            "node count row missing: {text}"
        );
    }

    #[test]
    fn read_report_includes_edge_metric() {
        let v = resources_read(&sample_graph(), "habitat-graph://report").unwrap();
        let text = text_of(&v);
        assert!(
            text.contains("| Edges | 2 |"),
            "edge count row missing: {text}"
        );
    }

    #[test]
    fn read_report_includes_community_metric() {
        let v = resources_read(&sample_graph(), "habitat-graph://report").unwrap();
        let text = text_of(&v);
        assert!(
            text.contains("| Communities | 1 |"),
            "community row missing: {text}"
        );
    }

    #[test]
    fn read_report_empty_graph_shows_empty_message() {
        let v = resources_read(&Graph::new(), "habitat-graph://report").unwrap();
        assert!(text_of(&v).contains("empty"), "empty-graph message missing");
    }

    #[test]
    fn read_report_includes_sample_labels() {
        let v = resources_read(&sample_graph(), "habitat-graph://report").unwrap();
        let text = text_of(&v);
        assert!(text.contains("Alpha"), "Alpha missing from report");
        assert!(text.contains("Beta"), "Beta missing from report");
        assert!(text.contains("Gamma"), "Gamma missing from report");
    }

    #[test]
    fn read_report_bidi_label_escaped() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "ev\u{202E}il"));
        let v = resources_read(&g, "habitat-graph://report").unwrap();
        let text = text_of(&v);
        assert!(!text.contains('\u{202E}'), "raw RLO leaked into report");
        assert!(text.contains("\\u{202E}"), "bidi not escaped");
    }

    #[test]
    fn read_report_control_char_escaped() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "label\u{001B}escape"));
        let v = resources_read(&g, "habitat-graph://report").unwrap();
        assert!(
            !text_of(&v).contains('\u{001B}'),
            "raw ESC leaked into report"
        );
    }

    #[test]
    fn read_report_truncates_at_sample_size() {
        let mut g = Graph::new();
        for i in 0..(REPORT_SAMPLE_SIZE + 5) {
            g.nodes.push(node(
                u32::try_from(i + 1).expect("id fits u32"),
                &format!("Node{i}"),
            ));
        }
        let v = resources_read(&g, "habitat-graph://report").unwrap();
        assert!(text_of(&v).contains("more node"), "truncation note missing");
    }

    #[test]
    fn read_report_no_truncation_note_when_at_limit() {
        let mut g = Graph::new();
        for i in 0..REPORT_SAMPLE_SIZE {
            g.nodes.push(node(
                u32::try_from(i + 1).expect("id fits u32"),
                &format!("Node{i}"),
            ));
        }
        let v = resources_read(&g, "habitat-graph://report").unwrap();
        // Exactly REPORT_SAMPLE_SIZE nodes — no "more" note should appear.
        assert!(
            !text_of(&v).contains("more node"),
            "spurious truncation note"
        );
    }

    #[test]
    fn read_report_deterministic() {
        let g = sample_graph();
        let a = resources_read(&g, "habitat-graph://report")
            .unwrap()
            .to_string();
        let b = resources_read(&g, "habitat-graph://report")
            .unwrap()
            .to_string();
        assert_eq!(a, b);
    }

    // ── resources_read: node ──────────────────────────────────────────────────

    #[test]
    fn read_node_single_match_returns_ok() {
        assert!(resources_read(&sample_graph(), "habitat-graph://node/Alpha").is_ok());
    }

    #[test]
    fn read_node_contents_envelope_has_one_entry() {
        let v = resources_read(&sample_graph(), "habitat-graph://node/Alpha").unwrap();
        assert_eq!(v["contents"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn read_node_uri_in_envelope_matches_requested() {
        let v = resources_read(&sample_graph(), "habitat-graph://node/Alpha").unwrap();
        assert_eq!(uri_of(&v), format!("{NODE_PREFIX}Alpha"));
    }

    #[test]
    fn read_node_mime_is_text_markdown() {
        let v = resources_read(&sample_graph(), "habitat-graph://node/Alpha").unwrap();
        assert_eq!(mime_of(&v), "text/markdown");
    }

    #[test]
    fn read_node_shows_label_in_output() {
        let v = resources_read(&sample_graph(), "habitat-graph://node/Alpha").unwrap();
        assert!(text_of(&v).contains("Alpha"));
    }

    #[test]
    fn read_node_shows_source_file_and_line() {
        let v = resources_read(&sample_graph(), "habitat-graph://node/Alpha").unwrap();
        let text = text_of(&v);
        assert!(text.contains("src/lib.rs"), "source file missing");
        assert!(text.contains("src/lib.rs:1"), "line number missing");
    }

    #[test]
    fn source_file_bidi_and_control_are_sanitised_in_node_and_report() {
        // source_file is attacker-influenced (load.rs accepts arbitrary graph.json) and is rendered
        // into agent-facing markdown — it MUST be display_safe'd like labels (Trojan-Source / ANSI).
        use habitat_graph_core::{Graph, Node, NodeId, Span};
        let mut g = Graph::new();
        g.nodes.push(Node {
            id: NodeId::new(1),
            label: "Alpha".to_owned(),
            source_file: "src/\u{202e}evil\u{1b}[31m.rs".to_owned(),
            source_location: Span::new(0, 1, 1, 1),
        });
        let node_v = resources_read(&g, "habitat-graph://node/Alpha").unwrap();
        let node_text = text_of(&node_v);
        assert!(
            !node_text.contains('\u{202e}'),
            "bidi must be sanitised in node view"
        );
        assert!(
            !node_text.contains('\u{1b}'),
            "ESC must be sanitised in node view"
        );
        let report_v = resources_read(&g, "habitat-graph://report").unwrap();
        let report_text = text_of(&report_v);
        assert!(
            !report_text.contains('\u{202e}'),
            "bidi must be sanitised in report"
        );
        assert!(
            !report_text.contains('\u{1b}'),
            "ESC must be sanitised in report"
        );
    }

    #[test]
    fn read_node_outbound_edges_listed() {
        // Alpha(1) --calls--> Beta(2)
        let v = resources_read(&sample_graph(), "habitat-graph://node/Alpha").unwrap();
        let text = text_of(&v);
        assert!(text.contains("calls"), "outbound relation missing");
        assert!(text.contains("Beta"), "outbound neighbour missing");
    }

    #[test]
    fn read_node_inbound_edges_listed() {
        // Beta(2) receives "calls" from Alpha(1)
        let v = resources_read(&sample_graph(), "habitat-graph://node/Beta").unwrap();
        let text = text_of(&v);
        assert!(text.contains("calls"), "inbound relation missing");
        assert!(text.contains("Alpha"), "inbound neighbour missing");
    }

    #[test]
    fn read_node_no_edges_shows_none() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "Isolated"));
        let v = resources_read(&g, "habitat-graph://node/Isolated").unwrap();
        assert!(
            text_of(&v).contains("none"),
            "none message missing for no edges"
        );
    }

    #[test]
    fn read_node_multiple_matches_all_present() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "ModA"));
        g.nodes.push(node(2, "ModB"));
        let v = resources_read(&g, "habitat-graph://node/Mod").unwrap();
        let text = text_of(&v);
        assert!(text.contains("ModA"), "ModA missing");
        assert!(text.contains("ModB"), "ModB missing");
    }

    #[test]
    fn read_node_not_found_is_err() {
        let v = resources_read(&sample_graph(), "habitat-graph://node/zzz_missing");
        assert!(v.is_err(), "expected Err for missing node");
    }

    #[test]
    fn read_node_empty_label_is_err() {
        // The URI "habitat-graph://node/" has an empty label segment.
        let v = resources_read(&sample_graph(), "habitat-graph://node/");
        assert!(v.is_err(), "expected Err for empty label");
    }

    #[test]
    fn read_node_bidi_label_in_header_escaped() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "ev\u{202E}il"));
        let v = resources_read(&g, "habitat-graph://node/ev").unwrap();
        assert!(!text_of(&v).contains('\u{202E}'), "raw bidi in node header");
    }

    #[test]
    fn read_node_bidi_label_in_neighbour_escaped() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "Source"));
        g.nodes.push(node(2, "ev\u{202E}il"));
        g.edges.push(edge(1, 2, "calls"));
        let v = resources_read(&g, "habitat-graph://node/Source").unwrap();
        assert!(
            !text_of(&v).contains('\u{202E}'),
            "raw bidi in neighbour label"
        );
    }

    #[test]
    fn read_node_case_insensitive_match() {
        let v = resources_read(&sample_graph(), "habitat-graph://node/alpha");
        assert!(v.is_ok(), "case-insensitive match failed");
        assert!(text_of(&v.unwrap()).contains("Alpha"));
    }

    #[test]
    fn read_node_search_term_display_safe_in_header() {
        // The search term itself (from the URI) must be display_safe'd in the header.
        let mut g = Graph::new();
        g.nodes.push(node(1, "ev\u{202E}il"));
        // Use the raw bidi char in the search term (via URI).
        // find_by_label will match it; the header must escape it.
        let v = resources_read(&g, "habitat-graph://node/ev\u{202E}il").unwrap();
        assert!(!text_of(&v).contains('\u{202E}'), "search term bidi leaked");
    }

    // ── resources_read: community ─────────────────────────────────────────────

    #[test]
    fn read_community_found_returns_ok() {
        let v = resources_read(&sample_graph(), "habitat-graph://community/0");
        assert!(v.is_ok(), "{v:?}");
    }

    #[test]
    fn read_community_contents_envelope_has_one_entry() {
        let v = resources_read(&sample_graph(), "habitat-graph://community/0").unwrap();
        assert_eq!(v["contents"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn read_community_uri_in_envelope_matches() {
        let v = resources_read(&sample_graph(), "habitat-graph://community/0").unwrap();
        assert_eq!(uri_of(&v), format!("{COMMUNITY_PREFIX}0"));
    }

    #[test]
    fn read_community_mime_is_text_markdown() {
        let v = resources_read(&sample_graph(), "habitat-graph://community/0").unwrap();
        assert_eq!(mime_of(&v), "text/markdown");
    }

    #[test]
    fn read_community_members_listed() {
        // sample_graph has community 0 with members [1, 2] → Alpha, Beta
        let v = resources_read(&sample_graph(), "habitat-graph://community/0").unwrap();
        let text = text_of(&v);
        assert!(text.contains("Alpha"), "Alpha missing from community");
        assert!(text.contains("Beta"), "Beta missing from community");
    }

    #[test]
    fn read_community_member_labels_display_safe() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "ev\u{202E}il"));
        g.communities.push(community(0, "C", vec![1]));
        let v = resources_read(&g, "habitat-graph://community/0").unwrap();
        assert!(
            !text_of(&v).contains('\u{202E}'),
            "bidi leaked into community member"
        );
    }

    #[test]
    fn read_community_bidi_in_community_label_escaped() {
        let mut g = Graph::new();
        g.communities.push(community(0, "C\u{202E}luster", vec![]));
        let v = resources_read(&g, "habitat-graph://community/0").unwrap();
        assert!(
            !text_of(&v).contains('\u{202E}'),
            "bidi leaked into community label"
        );
    }

    #[test]
    fn read_community_empty_members_shows_message() {
        let mut g = Graph::new();
        g.communities.push(community(0, "Empty", vec![]));
        let v = resources_read(&g, "habitat-graph://community/0").unwrap();
        // Expect "_No members._" in the output.
        assert!(
            text_of(&v).contains("No members"),
            "empty-members message missing"
        );
    }

    #[test]
    fn read_community_not_found_is_err() {
        let v = resources_read(&Graph::new(), "habitat-graph://community/99");
        assert!(v.is_err(), "expected Err for unknown community");
    }

    #[test]
    fn read_community_garbage_id_is_err() {
        let v = resources_read(&Graph::new(), "habitat-graph://community/not_a_number");
        assert!(v.is_err(), "expected Err for non-integer id");
    }

    #[test]
    fn read_community_negative_id_is_err() {
        // "-1" is not a valid u32.
        let v = resources_read(&Graph::new(), "habitat-graph://community/-1");
        assert!(v.is_err(), "expected Err for negative id");
    }

    #[test]
    fn read_community_zero_id_works() {
        let mut g = Graph::new();
        g.communities.push(community(0, "ZeroCluster", vec![]));
        assert!(resources_read(&g, "habitat-graph://community/0").is_ok());
    }

    #[test]
    fn read_community_members_sorted_deterministically() {
        // Members stored in reverse order; rendered output must be in ascending NodeId order.
        let mut g = Graph::new();
        g.nodes.push(node(1, "Alpha"));
        g.nodes.push(node(2, "Beta"));
        g.nodes.push(node(3, "Gamma"));
        g.communities.push(community(0, "C", vec![3, 1, 2]));
        let v = resources_read(&g, "habitat-graph://community/0").unwrap();
        let text = text_of(&v);
        let alpha_pos = text.find("Alpha").unwrap_or(usize::MAX);
        let beta_pos = text.find("Beta").unwrap_or(usize::MAX);
        let gamma_pos = text.find("Gamma").unwrap_or(usize::MAX);
        assert!(
            alpha_pos < beta_pos && beta_pos < gamma_pos,
            "members not in NodeId order"
        );
    }

    #[test]
    fn read_community_unknown_node_id_shows_placeholder() {
        let mut g = Graph::new();
        // Community refers to node 42 which is absent from graph.nodes.
        g.communities.push(community(0, "C", vec![42]));
        let v = resources_read(&g, "habitat-graph://community/0").unwrap();
        // Must render a placeholder rather than panic.
        assert!(
            text_of(&v).contains("42"),
            "unknown node id not represented"
        );
    }

    // ── resources_read: unknown / invalid URIs ────────────────────────────────

    #[test]
    fn read_unknown_uri_is_err() {
        assert!(resources_read(&Graph::new(), "habitat-graph://nope").is_err());
    }

    #[test]
    fn read_bare_scheme_is_err() {
        assert!(resources_read(&Graph::new(), "habitat-graph://").is_err());
    }

    #[test]
    fn read_empty_string_is_err() {
        assert!(resources_read(&Graph::new(), "").is_err());
    }

    #[test]
    fn read_http_uri_is_err() {
        assert!(resources_read(&Graph::new(), "http://example.com/graph").is_err());
    }

    #[test]
    fn read_node_prefix_alone_is_err() {
        // Empty label segment: "habitat-graph://node/"
        assert!(resources_read(&Graph::new(), "habitat-graph://node/").is_err());
    }

    #[test]
    fn read_community_prefix_alone_is_err() {
        // Empty id segment parses as error (not a u32).
        assert!(resources_read(&Graph::new(), "habitat-graph://community/").is_err());
    }

    // ── contents envelope invariants ──────────────────────────────────────────

    #[test]
    fn envelope_schema_has_uri_mime_text() {
        let v = resources_read(&Graph::new(), "habitat-graph://schema").unwrap();
        let item = &v["contents"][0];
        assert!(item.get("uri").is_some(), "missing uri");
        assert!(item.get("mimeType").is_some(), "missing mimeType");
        assert!(item.get("text").is_some(), "missing text");
    }

    #[test]
    fn envelope_report_has_uri_mime_text() {
        let v = resources_read(&Graph::new(), "habitat-graph://report").unwrap();
        let item = &v["contents"][0];
        assert!(item.get("uri").is_some());
        assert!(item.get("mimeType").is_some());
        assert!(item.get("text").is_some());
    }

    #[test]
    fn envelope_node_has_uri_mime_text() {
        let v = resources_read(&sample_graph(), "habitat-graph://node/Alpha").unwrap();
        let item = &v["contents"][0];
        assert!(item.get("uri").is_some());
        assert!(item.get("mimeType").is_some());
        assert!(item.get("text").is_some());
    }

    #[test]
    fn envelope_community_has_uri_mime_text() {
        let v = resources_read(&sample_graph(), "habitat-graph://community/0").unwrap();
        let item = &v["contents"][0];
        assert!(item.get("uri").is_some());
        assert!(item.get("mimeType").is_some());
        assert!(item.get("text").is_some());
    }

    #[test]
    fn read_node_deterministic_across_calls() {
        let g = sample_graph();
        let a = resources_read(&g, "habitat-graph://node/Beta")
            .unwrap()
            .to_string();
        let b = resources_read(&g, "habitat-graph://node/Beta")
            .unwrap()
            .to_string();
        assert_eq!(a, b);
    }

    #[test]
    fn read_community_deterministic_across_calls() {
        let g = sample_graph();
        let a = resources_read(&g, "habitat-graph://community/0")
            .unwrap()
            .to_string();
        let b = resources_read(&g, "habitat-graph://community/0")
            .unwrap()
            .to_string();
        assert_eq!(a, b);
    }
}
