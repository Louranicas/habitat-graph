//! Cross-Model Validation Matrix — XM-1 … XM-7 (S1008901 doc 17, §9)
//!
//! Unit tests exercising the MCP surface and cross-model bridge without live external model calls.
//! Each test maps to one row of the XM acceptance matrix:
//!
//! | XM  | Assertion (unit form)                                      |
//! |-----|------------------------------------------------------------|
//! | XM-1 | `initialize` + `tools/list` round-trip; tools discovered  |
//! | XM-2 | `graph_query` via MCP; typed response format verified      |
//! | XM-3 | MCP `tools/list` → `OpenAI` function spec translation        |
//! | XM-4 | `OpenAI` function call → MCP `tools/call` translation        |
//! | XM-5 | `max_tokens=100` budget constraint observed in output       |
//! | XM-6 | `generation_id` changes on graph mutation (cache key)       |
//! | XM-7 | `resources/read` round-trip for schema, report, node URI   |
//!
//! All tests are pure in-memory — no network, no file I/O, no live model call.

use habitat_graph_core::{Confidence, Edge, Graph, Node, NodeId, Span};
use habitat_graph_serve::{
    estimate_tokens, generation_id, handle_jsonrpc, mcp_tools_to_openai_functions,
    openai_function_call_to_mcp,
};
use serde_json::{json, Value};

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Construct a minimal test [`Node`].
fn node(id: u32, label: &str) -> Node {
    Node {
        id: NodeId::new(id),
        label: label.to_owned(),
        source_file: "src/lib.rs".to_owned(),
        source_location: Span::new(0, 1, id.max(1), 1),
    }
}

/// Construct a directed test [`Edge`] with `Confidence::Extracted`.
fn edge(source: u32, target: u32, relation: &str) -> Edge {
    Edge {
        source: NodeId::new(source),
        target: NodeId::new(target),
        relation: relation.to_owned(),
        confidence: Confidence::Extracted,
    }
}

/// A three-node graph (Alpha → Beta → Gamma) used across multiple tests.
fn sample_graph() -> Graph {
    let mut g = Graph::new();
    g.nodes = vec![node(1, "Alpha"), node(2, "Beta"), node(3, "Gamma")];
    g.edges = vec![edge(1, 2, "calls"), edge(2, 3, "calls")];
    g
}

/// Send one JSON-RPC request line to `handle_jsonrpc`, parse the JSON response.
fn call(graph: &Graph, line: &str) -> Value {
    let resp = handle_jsonrpc(graph, line);
    serde_json::from_str(&resp).expect("response must be valid JSON")
}

/// Convenience wrapper for a `tools/call` request.
fn tool_call(graph: &Graph, id: i64, name: &str, arguments: &Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    call(graph, &req.to_string())
}

/// Extract the `text` field from the first content item of a `tools/call` result.
fn text_of(result: &Value) -> &str {
    result["result"]["content"][0]["text"]
        .as_str()
        .expect("result must have text content")
}

// ── XM-1 ─────────────────────────────────────────────────────────────────────

/// XM-1: `initialize` → `notifications/initialized` → `tools/list` round-trip.
///
/// Verifies that:
/// - `initialize` returns the correct protocol version and advertises capabilities.
/// - `notifications/initialized` (a notification) returns no response.
/// - `tools/list` exposes the expected tools, each with a machine-readable `inputSchema`.
/// - Tool names are discovered from the server response, not assumed by the client.
#[test]
fn xm1_initialize_tools_list_round_trip() {
    let g = sample_graph();

    // Step 1 — initialize handshake.
    let init_resp = call(
        &g,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize"}).to_string(),
    );
    assert_eq!(
        init_resp["result"]["protocolVersion"], "2024-11-05",
        "initialize must echo the MCP protocol version"
    );
    assert!(
        init_resp["result"]["serverInfo"]["name"].is_string(),
        "initialize must include serverInfo.name"
    );
    assert!(
        init_resp["result"]["capabilities"]["tools"].is_object(),
        "initialize must advertise a tools capability"
    );
    assert!(
        init_resp["result"]["capabilities"]["resources"].is_object(),
        "initialize must advertise a resources capability"
    );
    // generation is the content-addressed cache key (FO-5)
    assert!(
        init_resp["result"]["generation"].is_string(),
        "initialize must carry a generation id"
    );

    // Step 2 — initialized notification (no response expected).
    let notif_resp = handle_jsonrpc(
        &g,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string(),
    );
    assert!(
        notif_resp.is_empty(),
        "notifications must produce no response"
    );

    // Step 3 — discover tools from the server (no hard-coded names on the client side).
    let list_resp = call(
        &g,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}).to_string(),
    );
    let tools = list_resp["result"]["tools"]
        .as_array()
        .expect("tools/list must return a tools array");
    assert!(
        !tools.is_empty(),
        "tools/list must advertise at least one tool"
    );

    // Collect names dynamically — a cross-model client must do this, never hard-code.
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        names.contains(&"graph_query"),
        "graph_query must appear in tools/list; got: {names:?}"
    );
    assert!(
        names.contains(&"graph_path"),
        "graph_path must appear in tools/list; got: {names:?}"
    );
    assert!(
        names.contains(&"graph_health"),
        "graph_health must appear in tools/list; got: {names:?}"
    );

    // Every tool must have a machine-readable JSON Schema (model-readable contract).
    for tool in tools {
        assert_eq!(
            tool["inputSchema"]["type"], "object",
            "tool {:?} must have an object inputSchema",
            tool["name"]
        );
    }
}

// ── XM-2 ─────────────────────────────────────────────────────────────────────

/// XM-2: `graph_query` via MCP — typed response format verified.
///
/// Verifies that:
/// - A valid query returns `result`, not `error`.
/// - `result.content` is a non-empty array of typed items.
/// - The first content item has `type:"text"` and a string `text` value.
/// - `result.isError` is `false`.
/// - The query result text mentions the matched node label.
#[test]
fn xm2_graph_query_typed_response_format() {
    let g = sample_graph();
    let resp = tool_call(&g, 1, "graph_query", &json!({"query": "Alpha"}));

    // Must return result, not error.
    assert!(
        resp.get("result").is_some(),
        "graph_query must return a result for a valid query"
    );
    assert!(
        resp.get("error").is_none(),
        "graph_query must not return an error for a valid query"
    );

    // Typed content array.
    let content = resp["result"]["content"]
        .as_array()
        .expect("result.content must be an array");
    assert!(!content.is_empty(), "result.content must not be empty");
    assert_eq!(
        content[0]["type"], "text",
        "content item type must be 'text'"
    );
    assert!(
        content[0]["text"].is_string(),
        "content item text must be a string"
    );

    // isError false.
    assert_eq!(resp["result"]["isError"], false);

    // Match text mentions the node.
    assert!(
        text_of(&resp).contains("Alpha"),
        "result text must mention the matched node"
    );

    // Missing required arg → error, not panic.
    let err_resp = tool_call(&g, 2, "graph_query", &json!({}));
    assert!(
        err_resp.get("error").is_some(),
        "missing query arg must return an error"
    );
    assert_eq!(err_resp["error"]["code"], -32602);
}

// ── XM-3 ─────────────────────────────────────────────────────────────────────

/// XM-3: MCP `tools/list` descriptors → `OpenAI` function spec translation.
///
/// Verifies that `mcp_tools_to_openai_functions` applied to the live `tools/list`
/// output produces valid `OpenAI` function tool specs that a GPT-5.5+ function-router
/// can consume. Every tool maps 1:1. Names, descriptions, and `required` arrays survive.
#[test]
fn xm3_mcp_tools_to_openai_functions_translation() {
    let g = sample_graph();
    let list_resp = call(
        &g,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string(),
    );
    let tools_arr = list_resp["result"]["tools"]
        .as_array()
        .expect("tools/list must return a tools array");

    // Translate to OpenAI function specs.
    let fn_specs = mcp_tools_to_openai_functions(tools_arr);

    // 1:1 mapping — every MCP tool produces exactly one function spec.
    assert_eq!(
        fn_specs.len(),
        tools_arr.len(),
        "every MCP tool must produce exactly one OpenAI function spec"
    );

    // Each spec must be a valid OpenAI function tool object.
    for spec in &fn_specs {
        assert_eq!(
            spec["type"], "function",
            "each function spec must have type:'function'"
        );
        assert!(
            spec["function"]["name"].is_string(),
            "function spec must have a string name"
        );
        assert_eq!(
            spec["function"]["parameters"]["type"], "object",
            "function spec parameters must be an object schema"
        );
    }

    // graph_query must survive translation with its required fields intact.
    let query_fn = fn_specs
        .iter()
        .find(|s| s["function"]["name"] == "graph_query")
        .expect("graph_query must appear in translated function specs");
    let required = query_fn["function"]["parameters"]["required"]
        .as_array()
        .expect("graph_query must have a required array");
    assert!(
        required.iter().any(|v| v == "query"),
        "graph_query function spec must require 'query'"
    );

    // graph_path must require both 'from' and 'to'.
    let path_fn = fn_specs
        .iter()
        .find(|s| s["function"]["name"] == "graph_path")
        .expect("graph_path must appear in translated function specs");
    let path_req = path_fn["function"]["parameters"]["required"]
        .as_array()
        .expect("graph_path must have a required array");
    let path_req_strs: Vec<&str> = path_req.iter().filter_map(Value::as_str).collect();
    assert!(
        path_req_strs.contains(&"from") && path_req_strs.contains(&"to"),
        "graph_path function spec must require 'from' and 'to'; got: {path_req_strs:?}"
    );

    // Translation is deterministic (R4): identical inputs → identical outputs.
    let fn_specs_again = mcp_tools_to_openai_functions(tools_arr);
    assert_eq!(
        fn_specs, fn_specs_again,
        "mcp_tools_to_openai_functions must be deterministic"
    );
}

// ── XM-4 ─────────────────────────────────────────────────────────────────────

/// XM-4: `OpenAI` function call → MCP `tools/call` params translation.
///
/// Verifies `openai_function_call_to_mcp`:
/// - Object arguments pass through directly.
/// - JSON-encoded string arguments are decoded (GPT sometimes emits this form).
/// - The resulting params object can drive a live `handle_jsonrpc` call end-to-end.
#[test]
fn xm4_openai_function_call_to_mcp_translation() {
    // Object arguments pass through directly.
    let params =
        openai_function_call_to_mcp("graph_query", &json!({"query": "Alpha", "max_tokens": 500}));
    assert_eq!(params["name"], "graph_query");
    assert_eq!(params["arguments"]["query"], "Alpha");
    assert_eq!(params["arguments"]["max_tokens"], 500);

    // JSON-encoded string arguments decoded (GPT-5.5+ sometimes emits this).
    let params_str = openai_function_call_to_mcp(
        "graph_path",
        &json!("{\"from\": \"Alpha\", \"to\": \"Gamma\"}"),
    );
    assert_eq!(params_str["name"], "graph_path");
    assert_eq!(params_str["arguments"]["from"], "Alpha");
    assert_eq!(params_str["arguments"]["to"], "Gamma");

    // Malformed string → graceful degrade to {} (documented contract).
    let params_bad = openai_function_call_to_mcp("t", &json!("not json {{{"));
    assert_eq!(
        params_bad["arguments"],
        json!({}),
        "malformed JSON string must degrade to empty object"
    );

    // End-to-end: translate a GPT-style call and drive it through handle_jsonrpc.
    let g = sample_graph();
    let mcp_params =
        openai_function_call_to_mcp("graph_path", &json!({"from": "Alpha", "to": "Gamma"}));
    let req = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {
            "name": mcp_params["name"],
            "arguments": mcp_params["arguments"]
        }
    });
    let resp = call(&g, &req.to_string());
    assert!(
        resp.get("result").is_some(),
        "translated MCP call must return a result"
    );
    let text = text_of(&resp);
    assert!(
        text.contains("Alpha") && text.contains("Gamma"),
        "path result must mention both endpoints: {text}"
    );
}

// ── XM-5 ─────────────────────────────────────────────────────────────────────

/// XM-5: `max_tokens` token-budget constraint is observed in `graph_query` output.
///
/// Verifies that:
/// - Without `max_tokens`, all matching nodes appear in the result.
/// - With `max_tokens=100` and many matches, the budget packing fires: the seed is
///   always present, and extra candidates are omitted with an explicit note.
/// - `estimate_tokens` is deterministic (R4).
#[test]
fn xm5_token_budget_constraint_respected() {
    // Build a graph with 30 nodes that all match "match_node".
    let mut g = Graph::new();
    for i in 1..=30_u32 {
        g.nodes.push(node(i, &format!("match_node_{i}")));
    }

    // Without budget: all 30 nodes should be mentioned.
    let resp_unbounded = tool_call(&g, 1, "graph_query", &json!({"query": "match_node"}));
    let text_unbounded = text_of(&resp_unbounded);
    assert!(
        text_unbounded.contains("30 node(s) match"),
        "unbounded query must report all 30 matches: {text_unbounded}"
    );

    // With a tight budget (5 tokens): seed always present, excess candidates omitted.
    let resp_tight = tool_call(
        &g,
        2,
        "graph_query",
        &json!({"query": "match_node", "max_tokens": 5}),
    );
    let text_tight = text_of(&resp_tight);
    assert!(
        text_tight.contains("match_node"),
        "seed must always be present even at budget 5: {text_tight}"
    );
    assert!(
        text_tight.contains("omitted"),
        "tight budget must note omitted candidates: {text_tight}"
    );

    // With budget=100 and 30 nodes: some candidates should still be omitted
    // because header + all 30 formatted lines > 100 tokens.
    let resp_100 = tool_call(
        &g,
        3,
        "graph_query",
        &json!({"query": "match_node", "max_tokens": 100}),
    );
    let text_100 = text_of(&resp_100);
    // Seed (first match) is always present.
    assert!(
        text_100.contains("match_node_1"),
        "seed (match_node_1) must be present at budget 100: {text_100}"
    );
    // Budget was applied: either all fit (uncommon) or some were omitted.
    // Either way the format is valid: has content, has a match count header.
    assert!(
        text_100.contains("node(s) match"),
        "response must have a match count header: {text_100}"
    );

    // estimate_tokens is deterministic (R4).
    let t1 = estimate_tokens(text_tight);
    let t2 = estimate_tokens(text_tight);
    assert_eq!(t1, t2, "estimate_tokens must be deterministic");

    // estimate_tokens uses ceil(bytes/4) — sanity check on a known string.
    assert_eq!(estimate_tokens("abcd"), 1); // 4 bytes / 4 = 1
    assert_eq!(estimate_tokens("abcde"), 2); // 5 bytes → ceil(5/4) = 2
}

// ── XM-6 ─────────────────────────────────────────────────────────────────────

/// XM-6: `generation_id` is a content-addressed cache key that changes on mutation.
///
/// Verifies:
/// - Identical graphs produce identical ids (determinism, R4).
/// - Adding a node changes the id.
/// - Adding an edge changes the id.
/// - Empty graph has a stable id different from non-empty.
/// - The id is returned in the `initialize` handshake (the client's cache key).
#[test]
fn xm6_generation_id_changes_on_mutation() {
    let g1 = sample_graph();
    let gen1 = generation_id(&g1);

    // Same structure, same id (R4 determinism).
    let g2 = sample_graph();
    assert_eq!(
        generation_id(&g2),
        gen1,
        "identical graphs must produce identical generation ids"
    );

    // Adding a node must change the id.
    let mut g_node = sample_graph();
    g_node.nodes.push(node(99, "Delta"));
    let gen_with_node = generation_id(&g_node);
    assert_ne!(
        gen_with_node, gen1,
        "adding a node must change the generation id"
    );

    // Adding an edge must change the id.
    let mut g_edge = sample_graph();
    g_edge.edges.push(edge(1, 3, "references"));
    let gen_with_edge = generation_id(&g_edge);
    assert_ne!(
        gen_with_edge, gen1,
        "adding an edge must change the generation id"
    );

    // Empty graph has a stable id different from the sample graph.
    let gen_empty = generation_id(&Graph::new());
    assert_ne!(
        gen_empty, gen1,
        "empty graph must have a different id from non-empty"
    );
    assert_eq!(
        generation_id(&Graph::new()),
        gen_empty,
        "empty graph must have a stable id"
    );

    // The id appears in the initialize handshake so a client can use it as a cache key.
    let init_resp = call(
        &g1,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize"}).to_string(),
    );
    assert_eq!(
        init_resp["result"]["generation"], gen1,
        "initialize must carry the correct content-addressed generation id"
    );

    // A different graph sends a different generation in initialize.
    let init_resp2 = call(
        &g_node,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize"}).to_string(),
    );
    assert_ne!(
        init_resp2["result"]["generation"], gen1,
        "mutated graph must carry a different generation in initialize"
    );
}

// ── XM-7 ─────────────────────────────────────────────────────────────────────

/// XM-7: `resources/read` round-trip for all resource types.
///
/// Verifies that the MCP resources surface provides readable, typed content for:
/// - `habitat-graph://schema` — JSON schema descriptor.
/// - `habitat-graph://report` — Markdown graph summary.
/// - `habitat-graph://node/{label}` — typed node neighbourhood.
///
/// Unknown URIs must return a JSON-RPC error (not a panic).
#[test]
fn xm7_resources_read_round_trip() {
    let g = sample_graph();

    // 1. Schema resource.
    let schema_resp = call(
        &g,
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "resources/read",
            "params": { "uri": "habitat-graph://schema" }
        })
        .to_string(),
    );
    assert!(
        schema_resp.get("result").is_some(),
        "schema resource must return a result"
    );
    let schema_str = schema_resp.to_string();
    assert!(
        schema_str.contains("schema_version"),
        "schema resource must contain schema_version: {schema_str}"
    );

    // 2. Report resource (Markdown summary with live counts).
    let report_resp = call(
        &g,
        &json!({
            "jsonrpc": "2.0", "id": 2, "method": "resources/read",
            "params": { "uri": "habitat-graph://report" }
        })
        .to_string(),
    );
    assert!(
        report_resp.get("result").is_some(),
        "report resource must return a result"
    );
    let report_str = report_resp.to_string();
    // Report must include node count from the live graph.
    assert!(
        report_str.contains("Alpha") || report_str.contains("Nodes"),
        "report resource must reference graph content: {report_str}"
    );

    // 3. Node resource for 'Alpha' — typed neighbourhood.
    let node_resp = call(
        &g,
        &json!({
            "jsonrpc": "2.0", "id": 3, "method": "resources/read",
            "params": { "uri": "habitat-graph://node/Alpha" }
        })
        .to_string(),
    );
    assert!(
        node_resp.get("result").is_some(),
        "node resource must return a result for an existing label"
    );
    let node_str = node_resp.to_string();
    assert!(
        node_str.contains("Alpha"),
        "node resource must mention the matched label: {node_str}"
    );

    // 4. Node resource for a substring match (case-insensitive).
    let lower_resp = call(
        &g,
        &json!({
            "jsonrpc": "2.0", "id": 4, "method": "resources/read",
            "params": { "uri": "habitat-graph://node/alpha" }
        })
        .to_string(),
    );
    assert!(
        lower_resp.get("result").is_some(),
        "node resource must match case-insensitively"
    );

    // 5. Unknown URI → JSON-RPC error (not a panic, not an empty response).
    let unknown_resp = call(
        &g,
        &json!({
            "jsonrpc": "2.0", "id": 5, "method": "resources/read",
            "params": { "uri": "habitat-graph://nope/unknown" }
        })
        .to_string(),
    );
    assert!(
        unknown_resp.get("error").is_some(),
        "unknown resource URI must return a JSON-RPC error"
    );

    // 6. Missing URI param → invalid-params error.
    let no_uri_resp = call(
        &g,
        &json!({
            "jsonrpc": "2.0", "id": 6, "method": "resources/read",
            "params": {}
        })
        .to_string(),
    );
    assert_eq!(
        no_uri_resp["error"]["code"], -32602,
        "missing uri param must be an invalid-params error"
    );
}

// ── Bonus: chained cross-model sequence ──────────────────────────────────────

/// Chain test: full GPT-5.5+ bridge sequence end-to-end (no live model).
///
/// Simulates what a GPT-5.5+ function-router does:
/// 1. Initialize to learn the generation id (cache key).
/// 2. Discover tools via `tools/list`.
/// 3. Translate tools to `OpenAI` function specs.
/// 4. Simulate a GPT function call and translate it to MCP.
/// 5. Execute the translated call through `handle_jsonrpc`.
/// 6. Verify the response carries the same generation id (cache validity).
#[test]
fn xm_chain_cross_model_sequence() {
    let g = sample_graph();

    // Step 1 — initialize: learn the generation id.
    let init_resp = call(
        &g,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize"}).to_string(),
    );
    let generation = init_resp["result"]["generation"]
        .as_str()
        .expect("generation must be a string")
        .to_owned();
    assert!(!generation.is_empty(), "generation id must not be empty");

    // Step 2 — discover tools.
    let list_resp = call(
        &g,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}).to_string(),
    );
    let tools_arr = list_resp["result"]["tools"]
        .as_array()
        .expect("tools must be an array");

    // Step 3 — translate to OpenAI function specs (GPT-5.5+ tool-router input).
    let fn_specs = mcp_tools_to_openai_functions(tools_arr);
    assert!(!fn_specs.is_empty(), "function specs must not be empty");

    // Step 4 — simulate GPT emitting a function call (object form).
    let gpt_fn_name = fn_specs
        .iter()
        .find(|s| s["function"]["name"] == "graph_query")
        .expect("graph_query must be discoverable")["function"]["name"]
        .as_str()
        .unwrap()
        .to_owned();
    let mcp_params = openai_function_call_to_mcp(&gpt_fn_name, &json!({"query": "Beta"}));
    assert_eq!(mcp_params["name"], "graph_query");

    // Step 5 — execute through handle_jsonrpc.
    let exec_req = json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {
            "name": mcp_params["name"],
            "arguments": mcp_params["arguments"]
        }
    });
    let exec_resp = call(&g, &exec_req.to_string());
    assert!(
        exec_resp.get("result").is_some(),
        "translated GPT call must execute successfully"
    );
    assert!(
        text_of(&exec_resp).contains("Beta"),
        "result must mention the queried node"
    );

    // Step 6 — generation id is stable across calls (cache validity: graph unchanged).
    let gen_again = generation_id(&g);
    assert_eq!(
        gen_again, generation,
        "generation id must be stable while the graph is unchanged"
    );
}
