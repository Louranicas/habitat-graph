//! A generic `OpenAI`-compatible backend — speaks the `/v1/chat/completions` protocol used by
//! `OpenAI`, by many local inference servers, and by gateway proxies.

use crate::api::Backend;
use crate::protocol::{build_prompt, parse_semantic};
use crate::transport::HttpTransport;
use habitat_graph_core::{Extraction, GraphError, Result};
use serde::Deserialize;

/// Default model name (overridable; the right value depends on the server).
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";

/// Semantic-extraction backend speaking the `OpenAI` `/v1/chat/completions` protocol.
///
/// Generic over an [`HttpTransport`] for network-free testing. [`Backend::is_local`] is derived from
/// the endpoint host: a loopback URL is local, anything else is not — so the local-first audit
/// reports honestly whether a configured server leaves the machine.
#[derive(Debug, Clone)]
pub struct OpenAiCompatBackend<T: HttpTransport> {
    transport: T,
    url: String,
    model: String,
    api_key: Option<String>,
}

/// The `/v1/chat/completions` response: the model text is in `choices[0].message.content`.
#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: String,
}

impl<T: HttpTransport> OpenAiCompatBackend<T> {
    /// Creates a backend pointed at `base_url` (e.g. `http://localhost:8000`) over `transport`.
    pub fn new(transport: T, base_url: impl Into<String>) -> Self {
        Self {
            transport,
            url: base_url.into(),
            model: DEFAULT_OPENAI_MODEL.to_string(),
            api_key: None,
        }
    }

    /// Overrides the model name.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Sets the bearer API key sent as `Authorization: Bearer …`.
    #[must_use]
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Borrows the underlying transport (lets callers inspect the recorded request in tests).
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// The chat-completions endpoint, normalizing a trailing slash on the base URL.
    fn endpoint(&self) -> String {
        format!("{}/v1/chat/completions", self.url.trim_end_matches('/'))
    }

    /// Builds the chat-completions request body (temperature 0 + JSON response format).
    fn request_body(&self, text: &str) -> String {
        serde_json::json!({
            "model": self.model,
            "messages": [{ "role": "user", "content": build_prompt(text) }],
            "temperature": 0,
            "response_format": { "type": "json_object" },
        })
        .to_string()
    }

    /// Returns `true` when the endpoint host is loopback.
    fn url_is_loopback(&self) -> bool {
        let host = self
            .url
            .split("://")
            .nth(1)
            .unwrap_or(&self.url)
            .split('/')
            .next()
            .unwrap_or_default();
        host.starts_with("localhost") || host.starts_with("127.0.0.1") || host.starts_with("[::1]")
    }
}

impl<T: HttpTransport> Backend for OpenAiCompatBackend<T> {
    fn name(&self) -> &'static str {
        "openai-compat"
    }

    fn is_local(&self) -> bool {
        self.url_is_loopback()
    }

    fn extract_semantic(&self, text: &str, source_file: &str) -> Result<Extraction> {
        let body = self.request_body(text);
        let auth;
        let headers: &[(&str, &str)] = match &self.api_key {
            Some(key) => {
                auth = format!("Bearer {key}");
                &[("Authorization", auth.as_str())]
            }
            None => &[],
        };
        let raw = self.transport.post_json(&self.endpoint(), &body, headers)?;
        let envelope: ChatResponse = serde_json::from_str(&raw).map_err(|e| {
            GraphError::Backend(format!("chat-completions envelope was malformed: {e}"))
        })?;
        let content = envelope
            .choices
            .first()
            .map(|c| c.message.content.as_str())
            .ok_or_else(|| {
                GraphError::Backend("chat-completions returned no choices".to_string())
            })?;
        parse_semantic(content, source_file)
    }
}

#[cfg(test)]
mod tests {
    use super::{OpenAiCompatBackend, DEFAULT_OPENAI_MODEL};
    use crate::api::Backend;
    use crate::transport::StaticTransport;
    use serde_json::Value;

    /// Wrap inner `{nodes,edges}` JSON as a chat-completions envelope.
    fn envelope(inner: &str) -> String {
        serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": inner } }]
        })
        .to_string()
    }

    #[test]
    fn name_is_openai_compat() {
        let b = OpenAiCompatBackend::new(StaticTransport::ok("{}"), "http://localhost:8000");
        assert_eq!(b.name(), "openai-compat");
    }

    #[test]
    fn loopback_url_is_local() {
        let b = OpenAiCompatBackend::new(StaticTransport::ok("{}"), "http://localhost:8000");
        assert!(b.is_local());
    }

    #[test]
    fn loopback_ip_is_local() {
        let b = OpenAiCompatBackend::new(StaticTransport::ok("{}"), "http://127.0.0.1:8080");
        assert!(b.is_local());
    }

    #[test]
    fn remote_url_is_not_local() {
        let b = OpenAiCompatBackend::new(StaticTransport::ok("{}"), "https://api.openai.com");
        assert!(!b.is_local());
    }

    #[test]
    fn endpoint_is_chat_completions() {
        let b = OpenAiCompatBackend::new(StaticTransport::ok("{}"), "http://h:8000");
        assert_eq!(b.endpoint(), "http://h:8000/v1/chat/completions");
    }

    #[test]
    fn trailing_slash_normalized() {
        let b = OpenAiCompatBackend::new(StaticTransport::ok("{}"), "http://h:8000/");
        assert_eq!(b.endpoint(), "http://h:8000/v1/chat/completions");
    }

    #[test]
    fn request_body_has_expected_shape() {
        let b = OpenAiCompatBackend::new(StaticTransport::ok("{}"), "http://localhost:8000");
        let v: Value = serde_json::from_str(&b.request_body("body text")).expect("json");
        assert_eq!(v["model"], DEFAULT_OPENAI_MODEL);
        assert_eq!(v["temperature"], 0);
        assert_eq!(v["response_format"]["type"], "json_object");
        let content = v["messages"][0]["content"].as_str().expect("str");
        assert!(content.contains("body text"));
    }

    #[test]
    fn custom_model_is_used() {
        let b = OpenAiCompatBackend::new(StaticTransport::ok("{}"), "http://h").with_model("qwen2");
        let v: Value = serde_json::from_str(&b.request_body("x")).expect("json");
        assert_eq!(v["model"], "qwen2");
    }

    #[test]
    fn extracts_through_the_envelope() {
        let inner = r#"{"nodes":[{"label":"X"},{"label":"Y"}],"edges":[{"source":"X","target":"Y","relation":"links"}]}"#;
        let b = OpenAiCompatBackend::new(
            StaticTransport::ok(envelope(inner)),
            "http://localhost:8000",
        );
        let e = b.extract_semantic("prose", "n.md").expect("ok");
        assert_eq!(e.counts(), (2, 1));
        assert_eq!(e.edges[0].relation, "links");
    }

    #[test]
    fn api_key_becomes_authorization_header() {
        let b = OpenAiCompatBackend::new(
            StaticTransport::ok(envelope("{}")),
            "https://api.openai.com",
        )
        .with_api_key("secret");
        let _ = b.extract_semantic("x", "f");
        let rec = b.transport().last_request().expect("recorded");
        assert!(rec
            .headers
            .contains(&("Authorization".to_string(), "Bearer secret".to_string())));
    }

    #[test]
    fn without_api_key_sends_no_auth_header() {
        let b =
            OpenAiCompatBackend::new(StaticTransport::ok(envelope("{}")), "http://localhost:8000");
        let _ = b.extract_semantic("x", "f");
        let rec = b.transport().last_request().expect("recorded");
        assert!(rec.headers.is_empty());
    }

    #[test]
    fn no_choices_is_a_backend_error() {
        let empty = serde_json::json!({ "choices": [] }).to_string();
        let b = OpenAiCompatBackend::new(StaticTransport::ok(empty), "http://localhost:8000");
        let err = b.extract_semantic("x", "f").expect_err("must fail");
        assert_eq!(err.kind(), "backend");
        assert!(err.to_string().contains("no choices"));
    }

    #[test]
    fn malformed_envelope_is_a_backend_error() {
        let b = OpenAiCompatBackend::new(StaticTransport::ok("nope"), "http://localhost:8000");
        assert!(b.extract_semantic("x", "f").is_err());
    }

    #[test]
    fn transport_failure_propagates() {
        let b = OpenAiCompatBackend::new(
            StaticTransport::failing("502 bad gateway"),
            "https://api.openai.com",
        );
        let err = b.extract_semantic("x", "f").expect_err("must fail");
        assert!(err.to_string().contains("502"));
    }

    #[test]
    fn malformed_inner_payload_is_rejected() {
        let b = OpenAiCompatBackend::new(
            StaticTransport::ok(envelope("not json")),
            "http://localhost",
        );
        assert!(b.extract_semantic("x", "f").is_err());
    }
}
