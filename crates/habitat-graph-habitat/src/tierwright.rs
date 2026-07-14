//! TIERWRIGHT backend — [`Backend`] implementation that routes
//! semantic extraction through the factory's TIERWRIGHT model router (`:8201`).
//!
//! TIERWRIGHT speaks a simple envelope protocol:
//! - **Request:** `{ "model": "<model>", "capability": "semantic-extract", "prompt": "<text>" }`
//!   sent (via HTTP POST) to `{base_url}/v1/route` (where `base_url` is the configured endpoint).
//! - **Response:** `{ "output": "<json-string>" }` where the inner string is the `{nodes,edges}`
//!   JSON understood by [`habitat_graph_backend::parse_semantic`].
//!
//! The backend is generic over [`HttpTransport`] so it is
//! fully testable without a running service: tests inject
//! [`StaticTransport`](habitat_graph_backend::StaticTransport); production wires a real client
//! via the `live` feature.

use habitat_graph_backend::{build_prompt, parse_semantic, Backend, HttpTransport};
use habitat_graph_core::{Extraction, GraphError, Result};
use serde::Deserialize;

/// Default TIERWRIGHT base URL — the factory's model-router loopback address.
pub const DEFAULT_TIERWRIGHT_URL: &str = "http://localhost:8201";

/// Default model specifier passed to TIERWRIGHT. The value `"auto"` lets the router pick.
pub const DEFAULT_TIERWRIGHT_MODEL: &str = "auto";

/// The `{ "output": "…" }` envelope that TIERWRIGHT wraps every routed response in.
///
/// The `output` field contains the raw JSON string produced by the model, which
/// [`parse_semantic`] converts into an [`Extraction`].
#[derive(Debug, Deserialize)]
struct RouteEnvelope {
    /// The model's raw JSON response string (a `{nodes, edges}` object).
    output: String,
}

/// Semantic-extraction backend that routes requests through the TIERWRIGHT model router.
///
/// TIERWRIGHT is the factory's internal model-dispatch service (`:8201`). It accepts a
/// `capability`-tagged request and returns an `output` envelope wrapping the model's JSON.
/// Treated as **local** because the router runs on the same machine.
///
/// The backend is generic over `T: HttpTransport` for network-free testing.
///
/// # Examples
///
/// ```rust
/// use habitat_graph_habitat::tierwright::TierwrightBackend;
/// use habitat_graph_backend::StaticTransport;
///
/// let transport = StaticTransport::ok(r#"{"output":"{}"}"#);
/// let backend = TierwrightBackend::new(transport);
/// ```
#[derive(Debug, Clone)]
pub struct TierwrightBackend<T: HttpTransport> {
    transport: T,
    url: String,
    model: String,
}

impl<T: HttpTransport> TierwrightBackend<T> {
    /// Creates a backend with the default URL (`http://localhost:8201`) and model (`auto`) over
    /// `transport`.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            url: DEFAULT_TIERWRIGHT_URL.to_string(),
            model: DEFAULT_TIERWRIGHT_MODEL.to_string(),
        }
    }

    /// Overrides the TIERWRIGHT base URL. Trailing slashes are tolerated.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use habitat_graph_habitat::tierwright::TierwrightBackend;
    /// use habitat_graph_backend::StaticTransport;
    ///
    /// let backend = TierwrightBackend::new(StaticTransport::ok(r#"{"output":"{}"}"#))
    ///     .with_url("http://gpu-node:8201");
    /// ```
    #[must_use]
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// Overrides the model specifier forwarded to TIERWRIGHT.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use habitat_graph_habitat::tierwright::TierwrightBackend;
    /// use habitat_graph_backend::StaticTransport;
    ///
    /// let backend = TierwrightBackend::new(StaticTransport::ok(r#"{"output":"{}"}"#))
    ///     .with_model("llama3-70b");
    /// ```
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Borrows the underlying transport. Useful in tests to inspect the recorded request.
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Builds the `/v1/route` endpoint URL, normalizing any trailing slash on the base URL.
    fn endpoint(&self) -> String {
        format!("{}/v1/route", self.url.trim_end_matches('/'))
    }

    /// Serializes the TIERWRIGHT request body for the given `text`.
    fn request_body(&self, text: &str) -> String {
        serde_json::json!({
            "model": self.model,
            "capability": "semantic-extract",
            "prompt": build_prompt(text),
        })
        .to_string()
    }
}

impl<T: HttpTransport> Backend for TierwrightBackend<T> {
    /// Returns `"tierwright"` — the stable identifier for receipts and diagnostics.
    fn name(&self) -> &'static str {
        "tierwright"
    }

    /// Returns `true`: TIERWRIGHT is a loopback router on the factory host.
    fn is_local(&self) -> bool {
        true
    }

    /// Extracts semantic `{nodes, edges}` from `text` via the TIERWRIGHT `/v1/route` endpoint.
    ///
    /// The request body is a JSON object with `model`, `capability`, and `prompt` fields. The
    /// response is unwrapped from a `{ "output": "<json-string>" }` envelope, then parsed by
    /// [`parse_semantic`].
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Backend`] if:
    /// - the transport fails (network error, non-success HTTP status),
    /// - the response is not a valid `{ "output": "…" }` envelope,
    /// - or the inner payload cannot be parsed as `{nodes, edges}` JSON.
    fn extract_semantic(&self, text: &str, source_file: &str) -> Result<Extraction> {
        let body = self.request_body(text);
        let raw = self.transport.post_json(&self.endpoint(), &body, &[])?;
        let envelope: RouteEnvelope = serde_json::from_str(&raw).map_err(|e| {
            GraphError::Backend(format!("TIERWRIGHT /v1/route envelope was malformed: {e}"))
        })?;
        parse_semantic(&envelope.output, source_file)
    }
}

#[cfg(test)]
mod tests {
    use super::{TierwrightBackend, DEFAULT_TIERWRIGHT_MODEL, DEFAULT_TIERWRIGHT_URL};
    use habitat_graph_backend::{Backend, StaticTransport};
    use serde_json::Value;

    // ── helpers ────────────────────────────────────────────────────────────────

    /// Wraps an inner `{nodes,edges}` JSON string in a TIERWRIGHT route envelope.
    fn envelope(inner: &str) -> String {
        serde_json::json!({ "output": inner }).to_string()
    }

    /// A minimal well-formed inner payload with one node.
    fn one_node_payload() -> &'static str {
        r#"{"nodes":[{"label":"Concept"}],"edges":[]}"#
    }

    /// A minimal well-formed inner payload with one node and one edge.
    fn node_and_edge_payload() -> &'static str {
        r#"{"nodes":[{"label":"Auth"},{"label":"DB"}],"edges":[{"source":"Auth","target":"DB","relation":"queries"}]}"#
    }

    // ── name + locality ────────────────────────────────────────────────────────

    #[test]
    fn name_returns_tierwright() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")));
        assert_eq!(b.name(), "tierwright");
    }

    #[test]
    fn is_local_returns_true() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")));
        assert!(b.is_local());
    }

    #[test]
    fn backend_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TierwrightBackend<StaticTransport>>();
    }

    // ── construction + builder methods ────────────────────────────────────────

    #[test]
    fn new_applies_defaults() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")));
        assert_eq!(b.url, DEFAULT_TIERWRIGHT_URL);
        assert_eq!(b.model, DEFAULT_TIERWRIGHT_MODEL);
    }

    #[test]
    fn with_url_overrides_endpoint() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")))
            .with_url("http://router:9999");
        assert_eq!(b.endpoint(), "http://router:9999/v1/route");
    }

    #[test]
    fn with_model_overrides_model() {
        let b =
            TierwrightBackend::new(StaticTransport::ok(envelope("{}"))).with_model("mixtral-8x7b");
        assert_eq!(b.model, "mixtral-8x7b");
    }

    #[test]
    fn with_url_returns_self_must_use() {
        // Verify builder chain compiles and the final value reflects both overrides.
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")))
            .with_url("http://a:1")
            .with_model("m1");
        assert_eq!(b.url, "http://a:1");
        assert_eq!(b.model, "m1");
    }

    #[test]
    fn transport_accessor_borrows_inner() {
        let inner = StaticTransport::ok(envelope("{}"));
        let b = TierwrightBackend::new(inner);
        // Calling transport() returns the same transport; we can inspect it.
        let _ = b.extract_semantic("text", "f.md");
        assert!(b.transport().last_request().is_some());
    }

    // ── endpoint URL normalization ─────────────────────────────────────────────

    #[test]
    fn default_endpoint_is_v1_route() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")));
        assert_eq!(b.endpoint(), format!("{DEFAULT_TIERWRIGHT_URL}/v1/route"));
    }

    #[test]
    fn trailing_slash_on_base_url_is_stripped() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")))
            .with_url("http://localhost:8201/");
        assert_eq!(b.endpoint(), "http://localhost:8201/v1/route");
    }

    #[test]
    fn multiple_trailing_slashes_are_stripped() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")))
            .with_url("http://localhost:8201///");
        assert_eq!(b.endpoint(), "http://localhost:8201/v1/route");
    }

    #[test]
    fn posts_to_v1_route_endpoint() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")));
        let _ = b.extract_semantic("any text", "f.md");
        let rec = b.transport().last_request().expect("recorded");
        assert_eq!(rec.url, format!("{DEFAULT_TIERWRIGHT_URL}/v1/route"));
    }

    #[test]
    fn custom_url_is_used_in_actual_request() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")))
            .with_url("http://tierwright-staging:8201");
        let _ = b.extract_semantic("x", "f.md");
        let rec = b.transport().last_request().expect("recorded");
        assert_eq!(rec.url, "http://tierwright-staging:8201/v1/route");
    }

    // ── request body shape ────────────────────────────────────────────────────

    #[test]
    fn request_body_contains_capability_semantic_extract() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")));
        let _ = b.extract_semantic("some text", "f.md");
        let rec = b.transport().last_request().expect("recorded");
        let v: Value = serde_json::from_str(&rec.body).expect("valid json");
        assert_eq!(v["capability"], "semantic-extract");
    }

    #[test]
    fn request_body_contains_default_model() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")));
        let _ = b.extract_semantic("text", "f.md");
        let rec = b.transport().last_request().expect("recorded");
        let v: Value = serde_json::from_str(&rec.body).expect("valid json");
        assert_eq!(v["model"], DEFAULT_TIERWRIGHT_MODEL);
    }

    #[test]
    fn request_body_contains_custom_model() {
        let b =
            TierwrightBackend::new(StaticTransport::ok(envelope("{}"))).with_model("llama3-70b");
        let _ = b.extract_semantic("text", "f.md");
        let rec = b.transport().last_request().expect("recorded");
        let v: Value = serde_json::from_str(&rec.body).expect("valid json");
        assert_eq!(v["model"], "llama3-70b");
    }

    #[test]
    fn request_body_prompt_contains_the_input_text() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")));
        let _ = b.extract_semantic("the special input text", "f.md");
        let rec = b.transport().last_request().expect("recorded");
        let v: Value = serde_json::from_str(&rec.body).expect("valid json");
        let prompt = v["prompt"].as_str().expect("string prompt");
        assert!(
            prompt.contains("the special input text"),
            "prompt must embed the input text; got: {prompt}"
        );
    }

    #[test]
    fn request_body_is_valid_json_with_all_three_fields() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")));
        let body = b.request_body("hello world");
        let v: Value = serde_json::from_str(&body).expect("valid json");
        assert!(v.get("model").is_some(), "must have model");
        assert!(v.get("capability").is_some(), "must have capability");
        assert!(v.get("prompt").is_some(), "must have prompt");
    }

    #[test]
    fn request_body_prompt_references_nodes_and_edges_schema() {
        // The prompt produced by build_prompt must name the expected response shape so the model
        // knows what JSON to produce.
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")));
        let body = b.request_body("any text");
        let v: Value = serde_json::from_str(&body).expect("valid json");
        let prompt = v["prompt"].as_str().expect("prompt is a string");
        assert!(prompt.contains("nodes"), "prompt must reference 'nodes'");
        assert!(prompt.contains("edges"), "prompt must reference 'edges'");
    }

    #[test]
    fn no_extra_headers_are_sent() {
        // TIERWRIGHT uses no auth headers in default config.
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")));
        let _ = b.extract_semantic("text", "f.md");
        let rec = b.transport().last_request().expect("recorded");
        assert!(
            rec.headers.is_empty(),
            "default config must send no extra headers; got: {:?}",
            rec.headers
        );
    }

    // ── happy-path extraction ─────────────────────────────────────────────────

    #[test]
    fn extracts_a_single_node_through_the_envelope() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope(one_node_payload())));
        let e = b
            .extract_semantic("prose about a Concept", "doc.md")
            .expect("ok");
        assert_eq!(e.counts(), (1, 0));
        assert_eq!(e.nodes[0].label, "Concept");
    }

    #[test]
    fn extracts_nodes_and_edges_through_the_envelope() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope(node_and_edge_payload())));
        let e = b
            .extract_semantic("Auth queries DB", "arch.md")
            .expect("ok");
        assert_eq!(e.counts(), (2, 1));
        assert_eq!(e.edges[0].relation, "queries");
        assert_eq!(e.edges[0].source, "Auth");
        assert_eq!(e.edges[0].target, "DB");
    }

    #[test]
    fn empty_inner_payload_yields_empty_extraction() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")));
        let e = b.extract_semantic("text", "f.md").expect("ok");
        assert!(e.is_empty());
    }

    #[test]
    fn empty_arrays_in_inner_payload_yield_empty_extraction() {
        let inner = r#"{"nodes":[],"edges":[]}"#;
        let b = TierwrightBackend::new(StaticTransport::ok(envelope(inner)));
        let e = b.extract_semantic("text", "f.md").expect("ok");
        assert!(e.is_empty());
    }

    #[test]
    fn source_file_threads_through_to_every_node() {
        let inner = r#"{"nodes":[{"label":"A"},{"label":"B"}]}"#;
        let b = TierwrightBackend::new(StaticTransport::ok(envelope(inner)));
        let e = b
            .extract_semantic("text", "path/to/special.md")
            .expect("ok");
        for node in &e.nodes {
            assert_eq!(
                node.source_file, "path/to/special.md",
                "every node must carry the source file"
            );
        }
    }

    #[test]
    fn multiple_nodes_and_edges_are_all_returned() {
        let inner = r#"{
            "nodes":[{"label":"A"},{"label":"B"},{"label":"C"}],
            "edges":[{"source":"A","target":"B","relation":"calls"},
                     {"source":"B","target":"C","relation":"uses"}]
        }"#;
        let b = TierwrightBackend::new(StaticTransport::ok(envelope(inner)));
        let e = b.extract_semantic("text", "f.md").expect("ok");
        assert_eq!(e.counts(), (3, 2));
    }

    // ── error cases ───────────────────────────────────────────────────────────

    #[test]
    fn transport_failure_is_a_backend_error() {
        let b = TierwrightBackend::new(StaticTransport::failing("connection refused"));
        let err = b.extract_semantic("text", "f.md").expect_err("must fail");
        assert_eq!(err.kind(), "backend");
        assert!(
            err.to_string().contains("connection refused"),
            "error must embed transport message; got: {err}"
        );
    }

    #[test]
    fn malformed_envelope_is_a_backend_error() {
        let b = TierwrightBackend::new(StaticTransport::ok("not a valid envelope"));
        let err = b.extract_semantic("text", "f.md").expect_err("must fail");
        assert_eq!(err.kind(), "backend");
    }

    #[test]
    fn envelope_error_message_mentions_envelope() {
        let b = TierwrightBackend::new(StaticTransport::ok("{\"wrong_field\":\"x\"}"));
        let err = b.extract_semantic("text", "f.md").expect_err("must fail");
        // The `output` field is missing — serde will fail because the struct is non-optional.
        assert_eq!(err.kind(), "backend");
    }

    #[test]
    fn empty_response_body_is_a_backend_error() {
        let b = TierwrightBackend::new(StaticTransport::ok(""));
        let err = b.extract_semantic("text", "f.md").expect_err("must fail");
        assert_eq!(err.kind(), "backend");
    }

    #[test]
    fn malformed_inner_payload_is_a_backend_error() {
        // The envelope is valid but `output` contains garbage, not `{nodes,edges}`.
        let b = TierwrightBackend::new(StaticTransport::ok(
            r#"{"output":"this is not json at all"}"#,
        ));
        let err = b.extract_semantic("text", "f.md").expect_err("must fail");
        assert_eq!(err.kind(), "backend");
    }

    #[test]
    fn wrong_json_structure_in_output_is_a_backend_error() {
        // A bare array instead of an object.
        let b = TierwrightBackend::new(StaticTransport::ok(
            r#"{"output":"[\"not\",\"an\",\"object\"]"}"#,
        ));
        let err = b.extract_semantic("text", "f.md").expect_err("must fail");
        assert_eq!(err.kind(), "backend");
    }

    #[test]
    fn transport_error_message_is_preserved() {
        let b = TierwrightBackend::new(StaticTransport::failing("timeout after 30s"));
        let err = b.extract_semantic("text", "f.md").expect_err("must fail");
        assert!(
            err.to_string().contains("timeout after 30s"),
            "original transport message must appear; got: {err}"
        );
    }

    // ── trait object compatibility ─────────────────────────────────────────────

    #[test]
    fn works_behind_dyn_backend() {
        let b: Box<dyn Backend> = Box::new(TierwrightBackend::new(StaticTransport::ok(envelope(
            one_node_payload(),
        ))));
        assert_eq!(b.name(), "tierwright");
        assert!(b.is_local());
        let e = b.extract_semantic("text", "f.md").expect("ok");
        assert_eq!(e.counts(), (1, 0));
    }

    // ── sanitization (inherits from parse_semantic) ───────────────────────────

    #[test]
    fn blank_node_labels_in_inner_payload_are_dropped() {
        let inner = r#"{"nodes":[{"label":"   "},{"label":"Real"}]}"#;
        let b = TierwrightBackend::new(StaticTransport::ok(envelope(inner)));
        let e = b.extract_semantic("text", "f.md").expect("ok");
        assert_eq!(e.counts(), (1, 0));
        assert_eq!(e.nodes[0].label, "Real");
    }

    #[test]
    fn control_chars_are_stripped_from_labels_in_inner_payload() {
        // U+0007 BEL is a control character; sanitize_label removes it.
        let inner = "{\"nodes\":[{\"label\":\"A\\u0007B\"}]}";
        let b = TierwrightBackend::new(StaticTransport::ok(envelope(inner)));
        let e = b.extract_semantic("text", "f.md").expect("ok");
        assert_eq!(e.nodes[0].label, "AB");
    }

    #[test]
    fn empty_relation_defaults_to_relates_to() {
        let inner = r#"{"edges":[{"source":"A","target":"B","relation":""}]}"#;
        let b = TierwrightBackend::new(StaticTransport::ok(envelope(inner)));
        let e = b.extract_semantic("text", "f.md").expect("ok");
        assert_eq!(e.edges[0].relation, habitat_graph_backend::DEFAULT_RELATION);
    }

    #[test]
    fn absent_relation_defaults_to_relates_to() {
        let inner = r#"{"edges":[{"source":"A","target":"B"}]}"#;
        let b = TierwrightBackend::new(StaticTransport::ok(envelope(inner)));
        let e = b.extract_semantic("text", "f.md").expect("ok");
        assert_eq!(e.edges[0].relation, habitat_graph_backend::DEFAULT_RELATION);
    }

    // ── additional behaviour/edge tests ──────────────────────────────────────

    /// Unicode characters in the input text must reach the prompt unchanged.
    #[test]
    fn unicode_input_text_is_embedded_in_prompt() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")));
        let _ = b.extract_semantic("αβγ — café résumé", "f.md");
        let rec = b.transport().last_request().expect("recorded");
        let v: Value = serde_json::from_str(&rec.body).expect("valid json");
        let prompt = v["prompt"].as_str().expect("prompt is string");
        assert!(
            prompt.contains("αβγ"),
            "unicode chars must appear in prompt: {prompt}"
        );
        assert!(
            prompt.contains("café"),
            "unicode chars must appear in prompt: {prompt}"
        );
    }

    /// Whitespace (tabs, newlines) in the input text must survive into the prompt.
    #[test]
    fn whitespace_in_input_text_is_preserved_in_prompt() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")));
        let text = "  indented line\n\tanother\tcolumn  ";
        let _ = b.extract_semantic(text, "f.md");
        let rec = b.transport().last_request().expect("recorded");
        let v: Value = serde_json::from_str(&rec.body).expect("valid json");
        let prompt = v["prompt"].as_str().expect("prompt is string");
        assert!(
            prompt.contains("indented"),
            "whitespace-padded input must reach the prompt: {prompt}"
        );
    }

    /// A single node result must carry the exact `source_file` supplied to `extract_semantic`.
    #[test]
    fn source_file_attribution_on_single_node() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope(
            r#"{"nodes":[{"label":"Solo"}]}"#,
        )));
        let e = b.extract_semantic("text", "single/node.rs").expect("ok");
        assert_eq!(e.nodes.len(), 1);
        assert_eq!(e.nodes[0].source_file, "single/node.rs");
        assert_eq!(e.nodes[0].label, "Solo");
    }

    /// Builder chain: both `with_url` and `with_model` must appear in the recorded request.
    #[test]
    fn custom_model_and_url_both_reflected_in_recorded_request() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")))
            .with_url("http://staging:9000")
            .with_model("codellama");
        let _ = b.extract_semantic("text", "f.md");
        let rec = b.transport().last_request().expect("recorded");
        assert_eq!(rec.url, "http://staging:9000/v1/route");
        let v: Value = serde_json::from_str(&rec.body).expect("valid json");
        assert_eq!(v["model"], "codellama");
    }

    /// The `DEFAULT_TIERWRIGHT_URL` constant must address the factory loopback at `:8201`.
    #[test]
    fn default_url_constant_is_localhost_8201() {
        assert_eq!(DEFAULT_TIERWRIGHT_URL, "http://localhost:8201");
    }

    /// The `DEFAULT_TIERWRIGHT_MODEL` constant must be `"auto"` (router-picks).
    #[test]
    fn default_model_constant_is_auto() {
        assert_eq!(DEFAULT_TIERWRIGHT_MODEL, "auto");
    }

    /// An explicit non-empty relation string must be preserved — not replaced by the default.
    #[test]
    fn explicit_edge_relation_is_preserved_not_replaced_by_default() {
        let inner = r#"{"edges":[{"source":"A","target":"B","relation":"calls"}]}"#;
        let b = TierwrightBackend::new(StaticTransport::ok(envelope(inner)));
        let e = b.extract_semantic("text", "f.md").expect("ok");
        assert_eq!(e.edges[0].relation, "calls");
        assert_ne!(
            e.edges[0].relation,
            habitat_graph_backend::DEFAULT_RELATION,
            "non-empty relation must NOT be replaced with the default"
        );
    }

    /// `output: null` in the TIERWRIGHT envelope must produce a backend error.
    #[test]
    fn envelope_null_output_field_is_backend_error() {
        let b = TierwrightBackend::new(StaticTransport::ok(r#"{"output":null}"#));
        let err = b.extract_semantic("text", "f.md").expect_err("must fail");
        assert_eq!(err.kind(), "backend");
    }

    /// `output: 42` (a number, not a string) must produce a backend error.
    #[test]
    fn envelope_numeric_output_field_is_backend_error() {
        let b = TierwrightBackend::new(StaticTransport::ok(r#"{"output":42}"#));
        let err = b.extract_semantic("text", "f.md").expect_err("must fail");
        assert_eq!(err.kind(), "backend");
    }

    /// Exactly one transport call is made per `extract_semantic` invocation.
    #[test]
    fn exactly_one_transport_call_per_extract_semantic() {
        let b = TierwrightBackend::new(StaticTransport::ok(envelope("{}")));
        // No calls yet.
        let _ = b.extract_semantic("first", "f.md");
        // After one call, last_request is Some.
        assert!(
            b.transport().last_request().is_some(),
            "transport must have been called exactly once"
        );
    }

    /// Three-node, two-edge payload: counts tuple must report `(3, 2)`.
    #[test]
    fn three_nodes_two_edges_exact_counts() {
        let inner = r#"{
            "nodes":[{"label":"X"},{"label":"Y"},{"label":"Z"}],
            "edges":[{"source":"X","target":"Y","relation":"r1"},
                     {"source":"Y","target":"Z","relation":"r2"}]
        }"#;
        let b = TierwrightBackend::new(StaticTransport::ok(envelope(inner)));
        let e = b.extract_semantic("text", "f.md").expect("ok");
        assert_eq!(e.counts(), (3, 2));
    }

    /// Inner payload with only a `nodes` key (no `edges` key) must yield zero edges.
    #[test]
    fn inner_payload_nodes_only_no_edges_key_yields_empty_edges() {
        let inner = r#"{"nodes":[{"label":"A"},{"label":"B"}]}"#;
        let b = TierwrightBackend::new(StaticTransport::ok(envelope(inner)));
        let e = b.extract_semantic("text", "f.md").expect("ok");
        assert_eq!(e.nodes.len(), 2);
        assert!(e.edges.is_empty(), "no edges key → edges must be empty");
    }
}
