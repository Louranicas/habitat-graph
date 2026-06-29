//! Model Context Protocol (MCP) transport — a pure JSON-RPC 2.0 handler that exposes a loaded
//! [`Graph`] as MCP tools, so habitat-graph is a live knowledge-graph organ a Claude Code or
//! orchestrator client can call.
//!
//! The surface is three methods — `initialize`, `tools/list`, `tools/call` — plus notifications
//! (requests without an `id`, e.g. `notifications/initialized`), which yield no response. Everything
//! here is a pure `&str -> String` transform over a borrowed [`Graph`], so the whole protocol is
//! unit-tested with no transport, runtime, or I/O. The CLI's `mcp` subcommand wraps
//! [`handle_jsonrpc`] in a line-oriented stdio loop.
//!
//! Tool output funnels every node label through [`display_safe`] — a graph extracted from untrusted
//! source must not deliver a Trojan-Source escape to the calling model's terminal.

use std::fmt::Write as _;

use habitat_graph_core::{display_safe, Graph, NodeId};
use serde_json::{json, Value};

use crate::query::{find_by_label, shortest_path};

/// The MCP protocol version this server speaks.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Server name advertised in the `initialize` handshake.
pub const SERVER_NAME: &str = "habitat-graph";

/// Maximum node matches rendered by `graph_query` before truncation (with an explicit note — never
/// a silent cap).
pub const MAX_QUERY_RESULTS: usize = 50;

/// Maximum accepted `graph_query` needle length in bytes — a defense-in-depth cap so an untrusted
/// client cannot force pathological per-request work with an enormous query string.
pub const MAX_QUERY_LEN: usize = 256;

/// JSON-RPC parse-error code.
const PARSE_ERROR: i64 = -32700;
/// JSON-RPC method-not-found code.
const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC invalid-params code.
const INVALID_PARAMS: i64 = -32602;

/// Handles one JSON-RPC 2.0 request line against `graph`, returning the response line.
///
/// Returns an **empty string** for notifications (requests without an `id`, e.g.
/// `notifications/initialized`): the caller sends nothing in reply. A malformed line yields a
/// JSON-RPC parse error with a `null` id.
#[must_use]
pub fn handle_jsonrpc(graph: &Graph, request: &str) -> String {
    let value: Value = match serde_json::from_str(request) {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                &Value::Null,
                PARSE_ERROR,
                "parse error: request was not valid JSON",
            )
        }
    };
    let id = match value.get("id") {
        Some(id) => id.clone(),
        None => return String::new(), // notification — no response
    };
    let method = value.get("method").and_then(Value::as_str).unwrap_or_default();
    match method {
        "initialize" => initialize_response(graph, &id),
        "tools/list" => tools_list_response(&id),
        "tools/call" => tools_call(graph, &id, value.get("params")),
        "resources/list" => ok_response(&id, crate::resources::resources_list(graph)),
        "resources/read" => resources_read_response(graph, &id, value.get("params")),
        other => error_response(&id, METHOD_NOT_FOUND, &format!("method not found: {other}")),
    }
}

/// Dispatches `resources/read`: extracts the `uri` param and renders the resource (or an error).
fn resources_read_response(graph: &Graph, id: &Value, params: Option<&Value>) -> String {
    let Some(uri) = params.and_then(|p| p.get("uri")).and_then(Value::as_str) else {
        return error_response(
            id,
            INVALID_PARAMS,
            "invalid params: resources/read requires a string `uri`",
        );
    };
    match crate::resources::resources_read(graph, uri) {
        Ok(result) => ok_response(id, result),
        Err(msg) => error_response(id, INVALID_PARAMS, &format!("invalid params: {msg}")),
    }
}

/// Builds a JSON-RPC success response echoing `id`.
fn ok_response(id: &Value, result: Value) -> String {
    // Build via a map so `result` is genuinely moved in (json! borrows, tripping needless_pass_by_value).
    Value::Object(serde_json::Map::from_iter([
        ("jsonrpc".to_owned(), Value::from("2.0")),
        ("id".to_owned(), id.clone()),
        ("result".to_owned(), result),
    ]))
    .to_string()
}

/// Builds a JSON-RPC error response echoing `id`.
fn error_response(id: &Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

/// The `initialize` handshake result.
fn initialize_response(graph: &Graph, id: &Value) -> String {
    ok_response(
        id,
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": { "tools": {}, "resources": {} },
            "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
            // FO-5: content-addressed generation id — the client's cache key for this graph state.
            "generation": crate::generation::generation_id(graph),
        }),
    )
}

/// The `tools/list` result — the three graph tools and their input schemas.
fn tools_list_response(id: &Value) -> String {
    ok_response(id, json!({ "tools": tool_descriptors() }))
}

/// The advertised tool descriptors (name + description + JSON-Schema for arguments).
fn tool_descriptors() -> Value {
    json!([
        {
            "name": "graph_query",
            "description": "Search graph nodes whose label contains a substring (case-insensitive).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "substring to match against node labels" },
                    "max_tokens": { "type": "integer", "description": "optional token budget; packs the most relevant matches within it (top match never dropped)" }
                },
                "required": ["query"]
            }
        },
        {
            "name": "graph_path",
            "description": "Find the shortest undirected path between two node labels.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "source node label (exact match)" },
                    "to": { "type": "string", "description": "target node label (exact match)" }
                },
                "required": ["from", "to"]
            }
        },
        {
            "name": "graph_health",
            "description": "Report graph statistics: node, edge, and community counts.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

/// Dispatches a `tools/call` to the named tool.
fn tools_call(graph: &Graph, id: &Value, params: Option<&Value>) -> String {
    let Some(params) = params else {
        return error_response(id, INVALID_PARAMS, "invalid params: missing params object");
    };
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    match name {
        "graph_query" => tool_query(graph, id, &args),
        "graph_path" => tool_path(graph, id, &args),
        "graph_health" => tool_health(graph, id),
        other => error_response(id, INVALID_PARAMS, &format!("invalid params: unknown tool {other}")),
    }
}

/// `graph_query` tool: substring search over node labels.
fn tool_query(graph: &Graph, id: &Value, args: &Value) -> String {
    let Some(needle) = args.get("query").and_then(Value::as_str) else {
        return error_response(
            id,
            INVALID_PARAMS,
            "invalid params: graph_query requires a string `query`",
        );
    };
    if needle.len() > MAX_QUERY_LEN {
        return error_response(
            id,
            INVALID_PARAMS,
            &format!("invalid params: query exceeds {MAX_QUERY_LEN} bytes"),
        );
    }
    let matches = find_by_label(graph, needle);
    if matches.is_empty() {
        return tool_text_result(id, &format!("no nodes match {needle:?}"));
    }
    let total = matches.len();
    let header = format!("{total} node(s) match {needle:?}:\n");
    let lines: Vec<String> = matches
        .iter()
        .take(MAX_QUERY_RESULTS)
        .map(|node| {
            format!(
                "  - {} [{}:{}]\n",
                display_safe(&node.label),
                node.source_file,
                node.source_location.start_line
            )
        })
        .collect();

    // FO-2: when the caller passes a token budget, pack the most-relevant matches within it
    // (the top match is the seed and is never dropped); otherwise use the fixed result cap.
    let text = if let Some(max_tokens) = args.get("max_tokens").and_then(Value::as_u64) {
        let budget = usize::try_from(max_tokens).unwrap_or(usize::MAX);
        let seed = lines.first().map(String::as_str);
        let rest = if lines.is_empty() { &[][..] } else { &lines[1..] };
        crate::budget::pack(&header, seed, rest, budget)
    } else {
        let mut out = header;
        for line in &lines {
            out.push_str(line);
        }
        if total > MAX_QUERY_RESULTS {
            let _ = writeln!(
                out,
                "  … {} more (showing first {MAX_QUERY_RESULTS})",
                total - MAX_QUERY_RESULTS
            );
        }
        out
    };
    tool_text_result(id, &text)
}

/// `graph_path` tool: shortest undirected path between two labels.
fn tool_path(graph: &Graph, id: &Value, args: &Value) -> String {
    let (Some(from), Some(to)) = (
        args.get("from").and_then(Value::as_str),
        args.get("to").and_then(Value::as_str),
    ) else {
        return error_response(
            id,
            INVALID_PARAMS,
            "invalid params: graph_path requires string `from` and `to`",
        );
    };
    let text = match shortest_path(graph, from, to) {
        Some(path) => {
            let labels: Vec<String> = path.iter().map(|nid| label_for(graph, *nid)).collect();
            format!(
                "path ({} hop(s)): {}",
                path.len().saturating_sub(1),
                labels.join(" -> ")
            )
        }
        None => format!("no path between {from:?} and {to:?}"),
    };
    tool_text_result(id, &text)
}

/// `graph_health` tool: graph statistics.
fn tool_health(graph: &Graph, id: &Value) -> String {
    let (nodes, edges, communities) = graph.counts();
    tool_text_result(
        id,
        &format!("nodes={nodes} edges={edges} communities={communities} schema={}", graph.schema),
    )
}

/// Resolves a [`NodeId`] to its render-safe label, falling back to the id if the node is absent.
fn label_for(graph: &Graph, nid: NodeId) -> String {
    graph
        .nodes
        .iter()
        .find(|n| n.id == nid)
        .map_or_else(|| format!("{nid:?}"), |n| display_safe(&n.label))
}

/// Wraps `text` as an MCP `tools/call` text-content result.
fn tool_text_result(id: &Value, text: &str) -> String {
    ok_response(
        id,
        json!({
            "content": [ { "type": "text", "text": text } ],
            "isError": false,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::{handle_jsonrpc, MCP_PROTOCOL_VERSION, SERVER_NAME};
    use habitat_graph_core::{Confidence, Edge, Graph, Node, NodeId, Span};
    use serde_json::{json, Value};

    fn node(id: u32, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: "src/lib.rs".to_owned(),
            source_location: Span::new(0, 1, id.max(1), 1),
        }
    }

    fn edge(s: u32, t: u32) -> Edge {
        Edge {
            source: NodeId::new(s),
            target: NodeId::new(t),
            relation: "calls".to_owned(),
            confidence: Confidence::Extracted,
        }
    }

    fn sample_graph() -> Graph {
        let mut g = Graph::new();
        g.nodes = vec![node(1, "Alpha"), node(2, "Beta"), node(3, "Gamma")];
        g.edges = vec![edge(1, 2), edge(2, 3)];
        g
    }

    fn call(graph: &Graph, line: &str) -> Value {
        let resp = handle_jsonrpc(graph, line);
        serde_json::from_str(&resp).expect("response is valid JSON")
    }

    fn tool_call(graph: &Graph, id: i64, name: &str, arguments: Value) -> Value {
        let params = Value::Object(serde_json::Map::from_iter([
            ("name".to_owned(), Value::from(name)),
            ("arguments".to_owned(), arguments),
        ]));
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params });
        call(graph, &req.to_string())
    }

    fn text_of(result: &Value) -> String {
        result["result"]["content"][0]["text"]
            .as_str()
            .expect("text content")
            .to_owned()
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let resp = call(&sample_graph(), "not json");
        assert_eq!(resp["error"]["code"], -32700);
        assert!(resp["id"].is_null());
        assert_eq!(resp["jsonrpc"], "2.0");
    }

    #[test]
    fn notification_without_id_yields_empty_string() {
        let g = sample_graph();
        let req = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert_eq!(handle_jsonrpc(&g, &req.to_string()), "");
    }

    #[test]
    fn notification_is_not_confused_with_null_id() {
        // An explicit null id IS a request and must be answered.
        let g = sample_graph();
        let req = json!({ "jsonrpc": "2.0", "id": null, "method": "initialize" });
        let resp = handle_jsonrpc(&g, &req.to_string());
        assert!(!resp.is_empty());
    }

    #[test]
    fn initialize_reports_protocol_and_server() {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" });
        let resp = call(&sample_graph(), &req.to_string());
        assert_eq!(resp["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(resp["result"]["serverInfo"]["name"], SERVER_NAME);
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn initialize_echoes_numeric_id() {
        let req = json!({ "jsonrpc": "2.0", "id": 42, "method": "initialize" });
        assert_eq!(call(&sample_graph(), &req.to_string())["id"], 42);
    }

    #[test]
    fn initialize_echoes_string_id() {
        let req = json!({ "jsonrpc": "2.0", "id": "abc", "method": "initialize" });
        assert_eq!(call(&sample_graph(), &req.to_string())["id"], "abc");
    }

    #[test]
    fn tools_list_advertises_three_tools() {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
        let resp = call(&sample_graph(), &req.to_string());
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, ["graph_query", "graph_path", "graph_health"]);
    }

    #[test]
    fn each_tool_has_an_input_schema() {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
        let resp = call(&sample_graph(), &req.to_string());
        for tool in resp["result"]["tools"].as_array().expect("arr") {
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn query_and_path_declare_required_args() {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
        let resp = call(&sample_graph(), &req.to_string());
        let tools = resp["result"]["tools"].as_array().expect("arr");
        let q = tools.iter().find(|t| t["name"] == "graph_query").expect("query");
        assert_eq!(q["inputSchema"]["required"][0], "query");
        let p = tools.iter().find(|t| t["name"] == "graph_path").expect("path");
        let req_args: Vec<&str> = p["inputSchema"]["required"].as_array().expect("arr").iter().filter_map(Value::as_str).collect();
        assert!(req_args.contains(&"from") && req_args.contains(&"to"));
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "frobnicate" });
        let resp = call(&sample_graph(), &req.to_string());
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn tools_call_without_params_is_invalid_params() {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call" });
        let resp = call(&sample_graph(), &req.to_string());
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[test]
    fn unknown_tool_is_invalid_params() {
        let resp = tool_call(&sample_graph(), 1, "no_such_tool", json!({}));
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[test]
    fn graph_query_finds_a_node() {
        let resp = tool_call(&sample_graph(), 1, "graph_query", json!({ "query": "Alph" }));
        let text = text_of(&resp);
        assert!(text.contains("Alpha"), "{text}");
        assert!(text.contains("1 node(s) match"));
        assert_eq!(resp["result"]["isError"], false);
    }

    #[test]
    fn graph_query_is_case_insensitive() {
        let resp = tool_call(&sample_graph(), 1, "graph_query", json!({ "query": "beta" }));
        assert!(text_of(&resp).contains("Beta"));
    }

    #[test]
    fn graph_query_no_match_reports_so() {
        let resp = tool_call(&sample_graph(), 1, "graph_query", json!({ "query": "zzz" }));
        assert!(text_of(&resp).contains("no nodes match"));
    }

    #[test]
    fn graph_query_missing_arg_is_invalid_params() {
        let resp = tool_call(&sample_graph(), 1, "graph_query", json!({}));
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[test]
    fn graph_query_empty_needle_matches_all() {
        let resp = tool_call(&sample_graph(), 1, "graph_query", json!({ "query": "" }));
        assert!(text_of(&resp).contains("3 node(s) match"));
    }

    #[test]
    fn graph_query_escapes_bidi_label() {
        let mut g = Graph::new();
        g.nodes = vec![node(1, "ev\u{202e}il")];
        let resp = tool_call(&g, 1, "graph_query", json!({ "query": "ev" }));
        let text = text_of(&resp);
        assert!(!text.contains('\u{202e}'), "bidi override must be escaped");
        assert!(text.contains("\\u{202E}"));
    }

    #[test]
    fn graph_query_caps_and_notes_truncation() {
        let mut g = Graph::new();
        // 60 matches > MAX_QUERY_RESULTS (50): list is capped, total reported honestly.
        for i in 0u32..60 {
            g.nodes.push(node(i + 1, &format!("Item{i}")));
        }
        let resp = tool_call(&g, 1, "graph_query", json!({ "query": "Item" }));
        let text = text_of(&resp);
        assert!(text.contains("more (showing first"), "{text}");
        assert!(text.contains("60 node(s) match"));
    }

    #[test]
    fn graph_path_reports_a_path() {
        let resp = tool_call(&sample_graph(), 1, "graph_path", json!({ "from": "Alpha", "to": "Gamma" }));
        let text = text_of(&resp);
        assert!(text.contains("Alpha -> Beta -> Gamma"), "{text}");
        assert!(text.contains("2 hop(s)"));
    }

    #[test]
    fn graph_path_same_node_is_zero_hops() {
        let resp = tool_call(&sample_graph(), 1, "graph_path", json!({ "from": "Beta", "to": "Beta" }));
        assert!(text_of(&resp).contains("0 hop(s)"));
    }

    #[test]
    fn graph_path_no_path_reports_so() {
        let mut g = sample_graph();
        g.nodes.push(node(9, "Island"));
        let resp = tool_call(&g, 1, "graph_path", json!({ "from": "Alpha", "to": "Island" }));
        assert!(text_of(&resp).contains("no path"));
    }

    #[test]
    fn graph_path_missing_args_is_invalid_params() {
        let resp = tool_call(&sample_graph(), 1, "graph_path", json!({ "from": "Alpha" }));
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[test]
    fn graph_health_reports_counts() {
        let resp = tool_call(&sample_graph(), 1, "graph_health", json!({}));
        let text = text_of(&resp);
        assert!(text.contains("nodes=3"));
        assert!(text.contains("edges=2"));
        assert!(text.contains("communities=0"));
    }

    #[test]
    fn responses_always_carry_jsonrpc_version() {
        for line in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            "bad",
        ] {
            assert_eq!(call(&sample_graph(), line)["jsonrpc"], "2.0");
        }
    }

    #[test]
    fn success_and_error_are_mutually_exclusive() {
        let ok = call(&sample_graph(), r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        assert!(ok.get("result").is_some() && ok.get("error").is_none());
        let err = call(&sample_graph(), r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#);
        assert!(err.get("error").is_some() && err.get("result").is_none());
    }

    #[test]
    fn tools_call_echoes_id() {
        let resp = tool_call(&sample_graph(), 7, "graph_health", json!({}));
        assert_eq!(resp["id"], 7);
    }

    #[test]
    fn empty_graph_health_is_all_zero() {
        let resp = tool_call(&Graph::new(), 1, "graph_health", json!({}));
        let text = text_of(&resp);
        assert!(text.contains("nodes=0 edges=0 communities=0"));
    }

    #[test]
    fn arguments_default_to_empty_when_omitted() {
        // tools/call with name but no arguments → health needs none, should still work.
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "graph_health" } });
        let resp = call(&sample_graph(), &req.to_string());
        assert_eq!(resp["result"]["isError"], false);
    }

    // ── A0 front door: resources + token-budget wiring (integration) ──────────

    #[test]
    fn initialize_advertises_resources_capability() {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" });
        let resp = call(&sample_graph(), &req.to_string());
        assert!(
            resp["result"]["capabilities"].get("resources").is_some(),
            "initialize must advertise the resources capability"
        );
    }

    #[test]
    fn resources_list_advertises_report_and_schema() {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "resources/list" });
        let s = call(&sample_graph(), &req.to_string()).to_string();
        assert!(s.contains("habitat-graph://report"), "report resource missing: {s}");
        assert!(s.contains("habitat-graph://schema"), "schema resource missing: {s}");
    }

    #[test]
    fn resources_read_schema_succeeds() {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "resources/read", "params": { "uri": "habitat-graph://schema" } });
        let s = call(&sample_graph(), &req.to_string()).to_string();
        assert!(s.contains("schema_version"), "schema text missing: {s}");
    }

    #[test]
    fn resources_read_unknown_uri_is_error() {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "resources/read", "params": { "uri": "habitat-graph://nope" } });
        let resp = call(&sample_graph(), &req.to_string());
        assert!(resp.get("error").is_some(), "unknown resource must error");
    }

    #[test]
    fn resources_read_missing_uri_is_invalid_params() {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "resources/read", "params": {} });
        let resp = call(&sample_graph(), &req.to_string());
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[test]
    fn graph_query_with_max_tokens_packs_within_budget() {
        // Many matches + a tiny budget → the packed output notes the omissions (FO-2 wiring).
        let mut g = Graph::new();
        for i in 1..=30_u32 {
            g.nodes.push(node(i, &format!("match_node_{i}")));
        }
        let resp = tool_call(&g, 1, "graph_query", json!({ "query": "match_node", "max_tokens": 5 }));
        let s = resp.to_string();
        assert!(s.contains("match"), "seed/header must be present: {s}");
        assert!(s.contains("omitted"), "a tiny budget must omit candidates: {s}");
    }

    #[test]
    fn graph_query_over_length_needle_is_invalid_params() {
        // DoS defense-in-depth: an oversized query string is rejected, not processed.
        let huge = "x".repeat(super::MAX_QUERY_LEN + 1);
        let resp = tool_call(&sample_graph(), 1, "graph_query", json!({ "query": huge }));
        // tool-level invalid-args surfaces as a JSON-RPC error.
        assert!(resp.get("error").is_some(), "over-length query must be rejected: {resp}");
    }
}
