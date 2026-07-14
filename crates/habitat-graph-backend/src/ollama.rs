//! The Ollama backend — a local LLM server speaking the `/api/generate` protocol.

use crate::api::Backend;
use crate::protocol::{build_prompt, parse_semantic};
use crate::transport::HttpTransport;
use habitat_graph_core::{Extraction, GraphError, Result};
use serde::Deserialize;

/// Default Ollama endpoint (the local server's default bind).
pub const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
/// Default model name.
pub const DEFAULT_OLLAMA_MODEL: &str = "llama3";

/// Semantic-extraction backend backed by a local [Ollama](https://ollama.com) server.
///
/// Generic over an [`HttpTransport`] so it is fully testable without a network: production passes a
/// real client (e.g. `UreqTransport`), tests pass `StaticTransport`. Treated as **local**
/// ([`Backend::is_local`] is `true`) because Ollama runs on the same host.
#[derive(Debug, Clone)]
pub struct OllamaBackend<T: HttpTransport> {
    transport: T,
    url: String,
    model: String,
}

/// The `/api/generate` response envelope (non-streaming). The model's text is in `response`.
#[derive(Debug, Deserialize)]
struct GenerateResponse {
    response: String,
}

impl<T: HttpTransport> OllamaBackend<T> {
    /// Creates a backend with the default URL and model over `transport`.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            url: DEFAULT_OLLAMA_URL.to_string(),
            model: DEFAULT_OLLAMA_MODEL.to_string(),
        }
    }

    /// Overrides the endpoint URL (trailing slash tolerated).
    #[must_use]
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// Overrides the model name.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// The configured model name.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Borrows the underlying transport (lets callers inspect the recorded request in tests).
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Builds the `/api/generate` request body with `format: "json"` so Ollama constrains output.
    fn request_body(&self, text: &str) -> String {
        serde_json::json!({
            "model": self.model,
            "prompt": build_prompt(text),
            "format": "json",
            "stream": false,
        })
        .to_string()
    }

    /// The full generate endpoint, normalizing a trailing slash on the base URL.
    fn endpoint(&self) -> String {
        format!("{}/api/generate", self.url.trim_end_matches('/'))
    }
}

impl<T: HttpTransport> Backend for OllamaBackend<T> {
    fn name(&self) -> &'static str {
        "ollama"
    }

    fn is_local(&self) -> bool {
        true
    }

    fn extract_semantic(&self, text: &str, source_file: &str) -> Result<Extraction> {
        let body = self.request_body(text);
        let raw = self.transport.post_json(&self.endpoint(), &body, &[])?;
        let envelope: GenerateResponse = serde_json::from_str(&raw).map_err(|e| {
            GraphError::Backend(format!("ollama generate envelope was malformed: {e}"))
        })?;
        parse_semantic(&envelope.response, source_file)
    }
}

#[cfg(test)]
mod tests {
    use super::{OllamaBackend, DEFAULT_OLLAMA_MODEL, DEFAULT_OLLAMA_URL};
    use crate::api::Backend;
    use crate::transport::StaticTransport;
    use serde_json::Value;

    /// Wrap an inner `{nodes,edges}` JSON string as an Ollama `/api/generate` envelope.
    fn envelope(inner: &str) -> String {
        serde_json::json!({ "response": inner, "done": true }).to_string()
    }

    #[test]
    fn name_and_locality() {
        let b = OllamaBackend::new(StaticTransport::ok("{}"));
        assert_eq!(b.name(), "ollama");
        assert!(b.is_local());
    }

    #[test]
    fn defaults_are_applied() {
        let b = OllamaBackend::new(StaticTransport::ok("{}"));
        assert_eq!(b.model(), DEFAULT_OLLAMA_MODEL);
    }

    #[test]
    fn extracts_through_the_envelope() {
        let inner = r#"{"nodes":[{"label":"Concept"}]}"#;
        let b = OllamaBackend::new(StaticTransport::ok(envelope(inner)));
        let e = b.extract_semantic("some prose", "doc.md").expect("ok");
        assert_eq!(e.counts(), (1, 0));
        assert_eq!(e.nodes[0].label, "Concept");
    }

    #[test]
    fn posts_to_the_generate_endpoint() {
        let b = OllamaBackend::new(StaticTransport::ok(envelope("{}")));
        let _ = b.extract_semantic("x", "f");
        let rec = b.transport().last_request().expect("recorded");
        assert_eq!(rec.url, format!("{DEFAULT_OLLAMA_URL}/api/generate"));
        // The request body the server actually received is well-formed JSON.
        let v: serde_json::Value = serde_json::from_str(&rec.body).expect("json body");
        assert_eq!(v["stream"], false);
    }

    #[test]
    fn request_body_is_valid_json_with_expected_fields() {
        let b = OllamaBackend::new(StaticTransport::ok(envelope("{}")));
        let body = b.request_body("the text");
        let v: Value = serde_json::from_str(&body).expect("valid json");
        assert_eq!(v["model"], DEFAULT_OLLAMA_MODEL);
        assert_eq!(v["format"], "json");
        assert_eq!(v["stream"], false);
        assert!(v["prompt"].as_str().expect("str").contains("the text"));
    }

    #[test]
    fn custom_url_and_model_are_used() {
        let b = OllamaBackend::new(StaticTransport::ok(envelope("{}")))
            .with_url("http://gpu:11434")
            .with_model("mistral");
        assert_eq!(b.model(), "mistral");
        assert_eq!(b.endpoint(), "http://gpu:11434/api/generate");
        let v: Value = serde_json::from_str(&b.request_body("x")).expect("json");
        assert_eq!(v["model"], "mistral");
    }

    #[test]
    fn trailing_slash_on_url_is_normalized() {
        let b = OllamaBackend::new(StaticTransport::ok(envelope("{}"))).with_url("http://h:11434/");
        assert_eq!(b.endpoint(), "http://h:11434/api/generate");
    }

    #[test]
    fn transport_failure_propagates_as_backend_error() {
        let b = OllamaBackend::new(StaticTransport::failing("connection refused"));
        let err = b.extract_semantic("x", "f").expect_err("must fail");
        assert_eq!(err.kind(), "backend");
        assert!(err.to_string().contains("connection refused"));
    }

    #[test]
    fn malformed_envelope_is_a_backend_error() {
        let b = OllamaBackend::new(StaticTransport::ok("not an envelope"));
        let err = b.extract_semantic("x", "f").expect_err("must fail");
        assert_eq!(err.kind(), "backend");
        assert!(err.to_string().contains("envelope"));
    }

    #[test]
    fn malformed_inner_payload_is_a_backend_error() {
        // Envelope is valid; the model's `response` text is not the expected JSON.
        let b = OllamaBackend::new(StaticTransport::ok(envelope("garbage")));
        assert!(b.extract_semantic("x", "f").is_err());
    }

    #[test]
    fn empty_inner_payload_yields_empty_extraction() {
        let b = OllamaBackend::new(StaticTransport::ok(envelope("{}")));
        assert!(b.extract_semantic("x", "f").expect("ok").is_empty());
    }

    #[test]
    fn source_file_threads_through() {
        let b = OllamaBackend::new(StaticTransport::ok(envelope(
            r#"{"nodes":[{"label":"A"}]}"#,
        )));
        let e = b.extract_semantic("x", "path/to.md").expect("ok");
        assert_eq!(e.nodes[0].source_file, "path/to.md");
    }
}
