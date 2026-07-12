//! Cross-model bridge (`FO-11`) — translate the MCP surface to/from the `OpenAI`
//! function-call format.
//!
//! `Claude 4.8`+ mounts the organ via MCP directly; `GPT-5.5`+ and other function-calling
//! models speak the `OpenAI` tool/function schema. This module is the pure, deterministic
//! translation seam: MCP tool descriptors → `OpenAI` function specs, and an `OpenAI`
//! function call → an MCP `tools/call` params object. The live cross-model proof (the
//! `XM-1..7` matrix against real `GPT-5.5`+) is separate + gated; this is the format
//! bridge it runs over.
//!
//! # Graceful-degrade contract
//!
//! When [`openai_function_call_to_mcp`] receives an `arguments` value whose inner JSON
//! string cannot be decoded (malformed JSON), it substitutes an empty object `{}`. This
//! is intentional: the bridge always produces a structurally valid MCP params object so
//! the caller can route the call without special-casing decode failures. Callers may treat
//! `arguments == {}` as a signal that the model's output was unparseable.

use serde_json::{json, Value};

/// MCP protocol version this bridge targets.
pub const BRIDGE_MCP_VERSION: &str = "2024-11-05";

/// `OpenAI` function-calling API version supported by this bridge.
pub const BRIDGE_OPENAI_VERSION: &str = "2024-10-01";

/// Semver string of this bridge implementation, injected at compile time via
/// `CARGO_PKG_VERSION`.
pub const BRIDGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Describes the cross-model bridge's protocol capabilities.
///
/// Produced by [`capabilities`]; also available as a typed struct for callers that need
/// to inspect individual fields rather than parse JSON. Use [`Capabilities::to_json`] to
/// obtain the `Value` representation suitable for embedding in a `JSON-RPC` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Version of the MCP protocol this bridge targets.
    pub mcp_protocol_version: &'static str,
    /// `OpenAI` function-calling API version supported.
    pub openai_function_calling_version: &'static str,
    /// Semver string of this bridge implementation.
    pub bridge_version: &'static str,
    /// Human-readable labels for the supported translation directions.
    pub supported_translations: Vec<&'static str>,
}

impl Capabilities {
    /// Serialises this capability descriptor to a [`Value`] suitable for embedding in a
    /// `JSON-RPC` response or capability-negotiation exchange.
    ///
    /// The returned object is byte-stable (`R4`): field order is deterministic across
    /// calls with identical inputs.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let translations = Value::Array(
            self.supported_translations
                .iter()
                .map(|s| Value::String((*s).to_owned()))
                .collect(),
        );
        json!({
            "mcp_protocol_version": self.mcp_protocol_version,
            "openai_function_calling_version": self.openai_function_calling_version,
            "bridge_version": self.bridge_version,
            "supported_translations": translations,
        })
    }
}

/// Returns the capability descriptor for this cross-model bridge.
///
/// Describes the MCP and `OpenAI` protocol versions supported and the available
/// translation directions. The returned [`Value`] is byte-stable (`R4`): identical
/// across calls, no interior randomness. Callers that need structured access can
/// construct a [`Capabilities`] value directly using the `BRIDGE_*` constants.
#[must_use]
pub fn capabilities() -> Value {
    Capabilities {
        mcp_protocol_version: BRIDGE_MCP_VERSION,
        openai_function_calling_version: BRIDGE_OPENAI_VERSION,
        bridge_version: BRIDGE_VERSION,
        supported_translations: vec![
            "mcp_tools_to_openai_functions",
            "openai_function_call_to_mcp",
        ],
    }
    .to_json()
}

/// Converts MCP `tools/list` tool descriptors to `OpenAI` function-tool specs.
///
/// Each MCP tool `{name, description, inputSchema}` becomes
/// `{"type":"function","function":{name, description, parameters: <inputSchema>}}`. Tools
/// without a string `name` are skipped (a malformed descriptor cannot produce a valid
/// function). Order is preserved (`R4`).
///
/// # Defaults applied
///
/// - `description` absent or non-string → empty string `""`.
/// - `inputSchema` absent → `{"type": "object", "properties": {}}`.
/// - `inputSchema` present (including `null`) → passed through as-is.
#[must_use]
pub fn mcp_tools_to_openai_functions(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?;
            let description = tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let parameters = tool
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
            Some(json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": parameters
                }
            }))
        })
        .collect()
}

/// Converts an `OpenAI` function call into an MCP `tools/call` params object.
///
/// `arguments` is the model's function arguments in one of two forms:
///
/// - **[`Value::Object`]** — used as-is.
/// - **[`Value::String`]** — decoded as a JSON value. On decode failure the result
///   defaults to `{}` (see [Graceful-degrade contract] in the module documentation).
///   This is intentional: the bridge always produces a structurally valid MCP params
///   object.
/// - **Any other variant** (`null`, boolean, number, array) — defaults to `{}`.
///
/// Produces `{"name": <name>, "arguments": <value>}` suitable as the `params` of an MCP
/// `tools/call` request.
///
/// [Graceful-degrade contract]: self
#[must_use]
pub fn openai_function_call_to_mcp(name: &str, arguments: &Value) -> Value {
    // Models sometimes emit arguments as a JSON-encoded string; decode it if so.
    // On any decode failure, default to an empty object — see the module-level
    // "Graceful-degrade contract" documentation. This default is intentional and
    // documented, not a silently swallowed error.
    let args = match arguments {
        Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or_else(|_| json!({})),
        Value::Object(_) => arguments.clone(),
        _ => json!({}),
    };
    json!({ "name": name, "arguments": args })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        capabilities, mcp_tools_to_openai_functions, openai_function_call_to_mcp, Capabilities,
        BRIDGE_MCP_VERSION, BRIDGE_OPENAI_VERSION,
    };

    // ── mcp_tools_to_openai_functions — basic shape ──────────────────────────

    #[test]
    fn single_tool_produces_one_function_entry() {
        let tools = vec![json!({ "name": "graph_query", "description": "search" })];
        assert_eq!(mcp_tools_to_openai_functions(&tools).len(), 1);
    }

    #[test]
    fn single_tool_type_field_is_function() {
        let tools = vec![json!({ "name": "graph_query" })];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(fns[0]["type"], "function");
    }

    #[test]
    fn single_tool_function_name_matches() {
        let tools = vec![json!({ "name": "graph_query" })];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(fns[0]["function"]["name"], "graph_query");
    }

    #[test]
    fn single_tool_function_description_matches() {
        let tools = vec![json!({ "name": "t", "description": "my desc" })];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(fns[0]["function"]["description"], "my desc");
    }

    #[test]
    fn single_tool_parameters_match_input_schema() {
        let schema = json!({ "type": "object", "properties": { "q": { "type": "string" } } });
        let tools = vec![json!({ "name": "t", "inputSchema": schema.clone() })];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(fns[0]["function"]["parameters"], schema);
    }

    // ── mcp_tools_to_openai_functions — multiple tools, order ────────────────

    #[test]
    fn multiple_tools_all_converted() {
        let tools = vec![
            json!({ "name": "a" }),
            json!({ "name": "b" }),
            json!({ "name": "c" }),
        ];
        assert_eq!(mcp_tools_to_openai_functions(&tools).len(), 3);
    }

    #[test]
    fn multiple_tools_order_preserved_first() {
        let tools = vec![json!({ "name": "first" }), json!({ "name": "second" })];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(fns[0]["function"]["name"], "first");
    }

    #[test]
    fn multiple_tools_order_preserved_second() {
        let tools = vec![json!({ "name": "first" }), json!({ "name": "second" })];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(fns[1]["function"]["name"], "second");
    }

    #[test]
    fn multiple_tools_order_preserved_last() {
        let tools = vec![
            json!({ "name": "alpha" }),
            json!({ "name": "beta" }),
            json!({ "name": "gamma" }),
        ];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(fns[2]["function"]["name"], "gamma");
    }

    // ── mcp_tools_to_openai_functions — name filtering ───────────────────────

    #[test]
    fn nameless_tool_skipped() {
        let tools = vec![json!({ "description": "no name here" })];
        assert!(mcp_tools_to_openai_functions(&tools).is_empty());
    }

    #[test]
    fn null_named_tool_skipped() {
        let tools = vec![json!({ "name": null, "description": "null name" })];
        assert!(mcp_tools_to_openai_functions(&tools).is_empty());
    }

    #[test]
    fn numeric_named_tool_skipped() {
        // `name` must be a JSON string; a number is not a valid string name.
        let tools = vec![json!({ "name": 42, "description": "numeric name" })];
        assert!(mcp_tools_to_openai_functions(&tools).is_empty());
    }

    #[test]
    fn bool_named_tool_skipped() {
        let tools = vec![json!({ "name": true })];
        assert!(mcp_tools_to_openai_functions(&tools).is_empty());
    }

    #[test]
    fn empty_string_name_included() {
        // An empty string IS a valid string — it passes the name filter.
        let tools = vec![json!({ "name": "" })];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0]["function"]["name"], "");
    }

    // ── mcp_tools_to_openai_functions — description defaults ─────────────────

    #[test]
    fn missing_description_gives_empty_string() {
        let tools = vec![json!({ "name": "t" })];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(fns[0]["function"]["description"], "");
    }

    #[test]
    fn null_description_gives_empty_string() {
        let tools = vec![json!({ "name": "t", "description": null })];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(fns[0]["function"]["description"], "");
    }

    #[test]
    fn numeric_description_gives_empty_string() {
        // A non-string description value is treated as absent.
        let tools = vec![json!({ "name": "t", "description": 99 })];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(fns[0]["function"]["description"], "");
    }

    // ── mcp_tools_to_openai_functions — inputSchema defaults + passthrough ───

    #[test]
    fn missing_input_schema_gives_default_object_schema() {
        let tools = vec![json!({ "name": "t" })];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(fns[0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn missing_input_schema_default_has_empty_properties() {
        let tools = vec![json!({ "name": "t" })];
        let fns = mcp_tools_to_openai_functions(&tools);
        let props = fns[0]["function"]["parameters"]["properties"]
            .as_object()
            .expect("properties must be an object");
        assert!(props.is_empty());
    }

    #[test]
    fn required_array_preserved_through() {
        let schema = json!({ "type": "object", "properties": {}, "required": ["query"] });
        let tools = vec![json!({ "name": "t", "inputSchema": schema })];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(fns[0]["function"]["parameters"]["required"][0], "query");
    }

    #[test]
    fn required_multiple_fields_preserved() {
        let schema = json!({
            "type": "object",
            "properties": {},
            "required": ["from", "to"]
        });
        let tools = vec![json!({ "name": "t", "inputSchema": schema })];
        let fns = mcp_tools_to_openai_functions(&tools);
        let req = fns[0]["function"]["parameters"]["required"]
            .as_array()
            .expect("required must be an array");
        assert_eq!(req.len(), 2);
        assert_eq!(req[0], "from");
        assert_eq!(req[1], "to");
    }

    #[test]
    fn null_input_schema_passes_through_as_null() {
        // An explicit `null` key is different from an absent key — it passes through as-is.
        let tools = vec![json!({ "name": "t", "inputSchema": null })];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert!(fns[0]["function"]["parameters"].is_null());
    }

    #[test]
    fn input_schema_with_nested_properties_preserved() {
        let schema = json!({
            "type": "object",
            "properties": {
                "opts": {
                    "type": "object",
                    "properties": { "limit": { "type": "integer" } }
                }
            }
        });
        let tools = vec![json!({ "name": "t", "inputSchema": schema })];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(
            fns[0]["function"]["parameters"]["properties"]["opts"]["type"],
            "object"
        );
    }

    // ── mcp_tools_to_openai_functions — edge cases ───────────────────────────

    #[test]
    fn empty_tool_list_returns_empty_vec() {
        assert!(mcp_tools_to_openai_functions(&[]).is_empty());
    }

    #[test]
    fn mix_valid_and_nameless_tools_only_valid_included() {
        let tools = vec![
            json!({ "name": "good" }),
            json!({ "description": "no name" }),
            json!({ "name": "also_good" }),
        ];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(fns.len(), 2);
        assert_eq!(fns[0]["function"]["name"], "good");
        assert_eq!(fns[1]["function"]["name"], "also_good");
    }

    #[test]
    fn tool_with_only_name_field_has_empty_desc_and_default_schema() {
        let tools = vec![json!({ "name": "minimal" })];
        let fns = mcp_tools_to_openai_functions(&tools);
        assert_eq!(fns[0]["function"]["description"], "");
        assert_eq!(fns[0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn determinism_mcp_to_openai_identical_calls() {
        let tools = vec![
            json!({ "name": "a", "description": "A", "inputSchema": { "type": "object" } }),
            json!({ "name": "b", "description": "B" }),
        ];
        let a = mcp_tools_to_openai_functions(&tools);
        let b = mcp_tools_to_openai_functions(&tools);
        assert_eq!(a, b);
    }

    // ── openai_function_call_to_mcp — object arguments ───────────────────────

    #[test]
    fn function_call_object_args_name_field_correct() {
        let mcp = openai_function_call_to_mcp("graph_query", &json!({ "query": "x" }));
        assert_eq!(mcp["name"], "graph_query");
    }

    #[test]
    fn function_call_object_args_arguments_preserved() {
        let mcp = openai_function_call_to_mcp("graph_query", &json!({ "query": "x" }));
        assert_eq!(mcp["arguments"]["query"], "x");
    }

    #[test]
    fn function_call_object_args_multiple_fields_preserved() {
        let args = json!({ "from": "A", "to": "B", "max_depth": 5 });
        let mcp = openai_function_call_to_mcp("graph_path", &args);
        assert_eq!(mcp["arguments"]["from"], "A");
        assert_eq!(mcp["arguments"]["to"], "B");
        assert_eq!(mcp["arguments"]["max_depth"], 5);
    }

    #[test]
    fn function_call_empty_object_args_produces_empty_arguments() {
        let mcp = openai_function_call_to_mcp("graph_health", &json!({}));
        let obj = mcp["arguments"]
            .as_object()
            .expect("arguments must be an object");
        assert!(obj.is_empty());
    }

    #[test]
    fn function_call_nested_object_args_preserved() {
        let args = json!({ "opts": { "limit": 10, "offset": 0 } });
        let mcp = openai_function_call_to_mcp("t", &args);
        assert_eq!(mcp["arguments"]["opts"]["limit"], 10);
        assert_eq!(mcp["arguments"]["opts"]["offset"], 0);
    }

    // ── openai_function_call_to_mcp — JSON-string arguments ──────────────────

    #[test]
    fn function_call_json_string_args_decoded_name() {
        let mcp = openai_function_call_to_mcp("graph_query", &json!("{\"query\":\"y\"}"));
        assert_eq!(mcp["name"], "graph_query");
    }

    #[test]
    fn function_call_json_string_args_decoded_value() {
        let mcp = openai_function_call_to_mcp("graph_query", &json!("{\"query\":\"y\"}"));
        assert_eq!(mcp["arguments"]["query"], "y");
    }

    #[test]
    fn function_call_json_string_empty_object_decoded() {
        let mcp = openai_function_call_to_mcp("t", &json!("{}"));
        let obj = mcp["arguments"]
            .as_object()
            .expect("decoded empty-object string must yield an object");
        assert!(obj.is_empty());
    }

    #[test]
    fn function_call_json_string_nested_obj_decoded() {
        let mcp = openai_function_call_to_mcp("t", &json!("{\"a\":{\"b\":42}}"));
        assert_eq!(mcp["arguments"]["a"]["b"], 42);
    }

    #[test]
    fn function_call_json_string_array_passes_through_decoded() {
        // A JSON-encoded array string decodes successfully and is stored as-is in arguments.
        let mcp = openai_function_call_to_mcp("t", &json!("[1, 2, 3]"));
        assert!(
            mcp["arguments"].is_array(),
            "a successfully decoded array should be preserved as-is"
        );
    }

    #[test]
    fn function_call_json_string_with_whitespace_decoded() {
        // serde_json tolerates surrounding whitespace in JSON strings.
        let mcp = openai_function_call_to_mcp("t", &json!("  { \"x\": 1 }  "));
        assert_eq!(mcp["arguments"]["x"], 1);
    }

    #[test]
    fn function_call_json_string_unicode_value_decoded() {
        let mcp = openai_function_call_to_mcp("t", &json!("{\"label\":\"中文\"}"));
        assert_eq!(mcp["arguments"]["label"], "中文");
    }

    // ── openai_function_call_to_mcp — malformed / fallback arguments ──────────

    #[test]
    fn function_call_malformed_string_args_gives_empty_object() {
        // Non-JSON string: graceful degrade (module-level contract).
        let mcp = openai_function_call_to_mcp("t", &json!("not json {"));
        let obj = mcp["arguments"]
            .as_object()
            .expect("malformed JSON string must degrade to an object");
        assert!(obj.is_empty());
    }

    #[test]
    fn function_call_empty_string_args_gives_empty_object() {
        // An empty string is not valid JSON; degrade to `{}`.
        let mcp = openai_function_call_to_mcp("t", &json!(""));
        assert_eq!(mcp["arguments"], json!({}));
    }

    #[test]
    fn function_call_null_args_gives_empty_object() {
        let mcp = openai_function_call_to_mcp("t", &json!(null));
        assert_eq!(mcp["arguments"], json!({}));
    }

    #[test]
    fn function_call_bool_true_args_gives_empty_object() {
        let mcp = openai_function_call_to_mcp("t", &json!(true));
        assert_eq!(mcp["arguments"], json!({}));
    }

    #[test]
    fn function_call_bool_false_args_gives_empty_object() {
        let mcp = openai_function_call_to_mcp("t", &json!(false));
        assert_eq!(mcp["arguments"], json!({}));
    }

    #[test]
    fn function_call_integer_args_gives_empty_object() {
        let mcp = openai_function_call_to_mcp("t", &json!(42));
        assert_eq!(mcp["arguments"], json!({}));
    }

    #[test]
    fn function_call_float_args_gives_empty_object() {
        // Use a float that is not a named constant to avoid the approx_constant lint.
        let mcp = openai_function_call_to_mcp("t", &json!(1.5));
        assert_eq!(mcp["arguments"], json!({}));
    }

    #[test]
    fn function_call_array_args_gives_empty_object() {
        // A bare array (not string-encoded) is not an object → degrade to `{}`.
        let mcp = openai_function_call_to_mcp("t", &json!([1, 2, 3]));
        assert_eq!(mcp["arguments"], json!({}));
    }

    // ── openai_function_call_to_mcp — object argument content ────────────────

    #[test]
    fn function_call_object_args_null_value_preserved() {
        let mcp = openai_function_call_to_mcp("t", &json!({ "optional_field": null }));
        assert!(mcp["arguments"]["optional_field"].is_null());
    }

    #[test]
    fn function_call_object_args_array_value_preserved() {
        let mcp = openai_function_call_to_mcp("t", &json!({ "items": [1, "two", 3] }));
        assert_eq!(mcp["arguments"]["items"][1], "two");
    }

    #[test]
    fn function_call_unicode_in_object_args_preserved() {
        let mcp = openai_function_call_to_mcp("t", &json!({ "label": "αβγ", "emoji": "🦀" }));
        assert_eq!(mcp["arguments"]["label"], "αβγ");
        assert_eq!(mcp["arguments"]["emoji"], "🦀");
    }

    #[test]
    fn function_call_empty_name_produces_name_field() {
        // An empty function name is still a valid string name field in the output.
        let mcp = openai_function_call_to_mcp("", &json!({}));
        assert_eq!(mcp["name"], "");
    }

    #[test]
    fn function_call_name_with_special_chars_preserved() {
        let mcp = openai_function_call_to_mcp("ns::tool/v2", &json!({}));
        assert_eq!(mcp["name"], "ns::tool/v2");
    }

    // ── capabilities — shape invariants ──────────────────────────────────────

    #[test]
    fn capabilities_returns_non_null() {
        assert!(!capabilities().is_null());
    }

    #[test]
    fn capabilities_is_object() {
        assert!(capabilities().is_object());
    }

    #[test]
    fn capabilities_has_mcp_protocol_version_key() {
        assert!(capabilities()["mcp_protocol_version"].is_string());
    }

    #[test]
    fn capabilities_mcp_version_matches_constant() {
        assert_eq!(capabilities()["mcp_protocol_version"], BRIDGE_MCP_VERSION);
    }

    #[test]
    fn capabilities_has_openai_function_calling_version_key() {
        assert!(capabilities()["openai_function_calling_version"].is_string());
    }

    #[test]
    fn capabilities_openai_version_matches_constant() {
        assert_eq!(
            capabilities()["openai_function_calling_version"],
            BRIDGE_OPENAI_VERSION
        );
    }

    #[test]
    fn capabilities_has_bridge_version_key() {
        assert!(capabilities()["bridge_version"].is_string());
    }

    #[test]
    fn capabilities_supported_translations_is_array() {
        assert!(capabilities()["supported_translations"].is_array());
    }

    #[test]
    fn capabilities_supported_translations_includes_mcp_to_openai() {
        let cap = capabilities();
        let arr = cap["supported_translations"]
            .as_array()
            .expect("supported_translations must be an array");
        assert!(arr.iter().any(|v| v == "mcp_tools_to_openai_functions"));
    }

    #[test]
    fn capabilities_supported_translations_includes_openai_to_mcp() {
        let cap = capabilities();
        let arr = cap["supported_translations"]
            .as_array()
            .expect("supported_translations must be an array");
        assert!(arr.iter().any(|v| v == "openai_function_call_to_mcp"));
    }

    #[test]
    fn capabilities_is_deterministic() {
        assert_eq!(capabilities(), capabilities());
    }

    // ── capabilities — struct API ─────────────────────────────────────────────

    #[test]
    fn capabilities_struct_mcp_version_roundtrips_to_json() {
        let cap = Capabilities {
            mcp_protocol_version: BRIDGE_MCP_VERSION,
            openai_function_calling_version: BRIDGE_OPENAI_VERSION,
            bridge_version: "0.0.0",
            supported_translations: vec![],
        };
        assert_eq!(cap.to_json()["mcp_protocol_version"], BRIDGE_MCP_VERSION);
    }

    #[test]
    fn capabilities_struct_openai_version_roundtrips_to_json() {
        let cap = Capabilities {
            mcp_protocol_version: "v-test",
            openai_function_calling_version: BRIDGE_OPENAI_VERSION,
            bridge_version: "0.0.0",
            supported_translations: vec![],
        };
        assert_eq!(
            cap.to_json()["openai_function_calling_version"],
            BRIDGE_OPENAI_VERSION
        );
    }

    #[test]
    fn capabilities_struct_supported_translations_length_preserved() {
        let cap = Capabilities {
            mcp_protocol_version: "v1",
            openai_function_calling_version: "v2",
            bridge_version: "0.1.0",
            supported_translations: vec!["one", "two", "three"],
        };
        let json = cap.to_json();
        assert_eq!(
            json["supported_translations"].as_array().map(Vec::len),
            Some(3)
        );
    }

    #[test]
    fn capabilities_struct_supported_translations_values_preserved() {
        let cap = Capabilities {
            mcp_protocol_version: "v1",
            openai_function_calling_version: "v2",
            bridge_version: "0.1.0",
            supported_translations: vec!["alpha", "beta"],
        };
        let json = cap.to_json();
        let arr = json["supported_translations"].as_array().expect("array");
        assert_eq!(arr[0], "alpha");
        assert_eq!(arr[1], "beta");
    }

    #[test]
    fn capabilities_struct_to_json_is_deterministic() {
        let cap = Capabilities {
            mcp_protocol_version: "v1",
            openai_function_calling_version: "v2",
            bridge_version: "0.0.1",
            supported_translations: vec!["x"],
        };
        assert_eq!(cap.to_json(), cap.to_json());
    }

    #[test]
    fn capabilities_struct_empty_translations_yields_empty_array() {
        let cap = Capabilities {
            mcp_protocol_version: "v1",
            openai_function_calling_version: "v2",
            bridge_version: "0.0.1",
            supported_translations: vec![],
        };
        let json = cap.to_json();
        let arr = json["supported_translations"].as_array().expect("array");
        assert!(arr.is_empty());
    }

    // ── round-trip + determinism ──────────────────────────────────────────────

    #[test]
    fn round_trip_name_preserved() {
        // MCP tool → `OpenAI` function → call back to MCP: name must survive.
        let tools = vec![json!({ "name": "graph_query", "description": "search" })];
        let fns = mcp_tools_to_openai_functions(&tools);
        let fn_name = fns[0]["function"]["name"].as_str().expect("name str");
        let mcp_call = openai_function_call_to_mcp(fn_name, &json!({ "query": "x" }));
        assert_eq!(mcp_call["name"], "graph_query");
    }

    #[test]
    fn round_trip_args_shape_consistent() {
        // Arguments that were valid before remain structurally valid after round-trip.
        let mcp_call = openai_function_call_to_mcp("graph_query", &json!({ "query": "foo" }));
        assert!(mcp_call["arguments"].is_object());
        assert_eq!(mcp_call["arguments"]["query"], "foo");
    }

    #[test]
    fn determinism_openai_to_mcp_identical_calls() {
        let args = json!({ "query": "hello", "max_tokens": 100 });
        let a = openai_function_call_to_mcp("graph_query", &args);
        let b = openai_function_call_to_mcp("graph_query", &args);
        assert_eq!(a, b);
    }

    #[test]
    fn determinism_mcp_to_openai_large_tool_set() {
        let tools: Vec<serde_json::Value> = (0..20_u32)
            .map(|i| {
                json!({
                    "name": format!("tool_{i}"),
                    "description": format!("Tool number {i}"),
                    "inputSchema": { "type": "object", "properties": {} }
                })
            })
            .collect();
        let a = mcp_tools_to_openai_functions(&tools);
        let b = mcp_tools_to_openai_functions(&tools);
        assert_eq!(a, b);
    }

    #[test]
    fn all_non_string_name_variants_skipped() {
        // Comprehensive check: null, number, bool, object, array — all skip.
        let tools = vec![
            json!({ "name": null }),
            json!({ "name": 0 }),
            json!({ "name": true }),
            json!({ "name": false }),
            json!({ "name": {} }),
            json!({ "name": [] }),
        ];
        assert!(mcp_tools_to_openai_functions(&tools).is_empty());
    }

    #[test]
    fn all_non_object_non_string_arg_variants_give_empty_object() {
        // Comprehensive check across multiple fallback-path variants.
        for args in [json!(null), json!(true), json!(false), json!(0), json!([])] {
            let mcp = openai_function_call_to_mcp("t", &args);
            assert_eq!(mcp["arguments"], json!({}), "expected {{}} for args={args}");
        }
    }
}
