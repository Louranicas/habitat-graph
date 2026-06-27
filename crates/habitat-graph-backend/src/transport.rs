//! HTTP transport abstraction (dependency inversion) so network backends are testable without a
//! network. The default build carries **no** network dependency; the real client lives behind the
//! `net` feature.

use habitat_graph_core::{GraphError, Result};
use std::sync::Mutex;

/// A minimal blocking HTTP transport: send a JSON `body` to `url` via `POST`, return the response.
///
/// Backends are generic over this trait so tests inject a deterministic [`StaticTransport`] and
/// production wires a real client (the `net`-feature [`UreqTransport`], or the habitat crate's
/// TIERWRIGHT-routed transport).
pub trait HttpTransport: Send + Sync {
    /// Sends an HTTP `POST` of `body` to `url` with `Content-Type: application/json` plus any extra
    /// `headers` (e.g. `("Authorization", "Bearer …")`), returning the raw response body.
    ///
    /// # Errors
    /// Returns [`GraphError::Backend`] if the request fails or the response is not a success status.
    fn post_json(&self, url: &str, body: &str, headers: &[(&str, &str)]) -> Result<String>;
}

/// A request captured by [`StaticTransport`], for assertions in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    /// The endpoint the backend sent the request to.
    pub url: String,
    /// The JSON request body the backend built.
    pub body: String,
    /// Extra headers the backend attached (owned copies).
    pub headers: Vec<(String, String)>,
}

/// A deterministic [`HttpTransport`] double: returns a preset response (or error) and records the
/// last request. Available in every build (it has no dependencies) so downstream crates — e.g. the
/// habitat TIERWRIGHT backend's tests — can reuse it.
#[derive(Debug)]
pub struct StaticTransport {
    response: std::result::Result<String, String>,
    last: Mutex<Option<RecordedRequest>>,
}

impl StaticTransport {
    /// A transport that always returns `response` as the body.
    #[must_use]
    pub fn ok(response: impl Into<String>) -> Self {
        Self {
            response: Ok(response.into()),
            last: Mutex::new(None),
        }
    }

    /// A transport that always fails with `message` (modelling a network/HTTP error).
    #[must_use]
    pub fn failing(message: impl Into<String>) -> Self {
        Self {
            response: Err(message.into()),
            last: Mutex::new(None),
        }
    }

    /// Returns the most recent request the transport received, if any.
    #[must_use]
    pub fn last_request(&self) -> Option<RecordedRequest> {
        self.last.lock().ok().and_then(|g| g.clone())
    }
}

impl HttpTransport for StaticTransport {
    fn post_json(&self, url: &str, body: &str, headers: &[(&str, &str)]) -> Result<String> {
        if let Ok(mut guard) = self.last.lock() {
            *guard = Some(RecordedRequest {
                url: url.to_string(),
                body: body.to_string(),
                headers: headers
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
            });
        }
        match &self.response {
            Ok(body) => Ok(body.clone()),
            Err(message) => Err(GraphError::Backend(message.clone())),
        }
    }
}

/// A concrete blocking transport backed by [`ureq`]. Only compiled with the `net` feature.
#[cfg(feature = "net")]
#[derive(Debug, Clone)]
pub struct UreqTransport {
    timeout_secs: u64,
}

#[cfg(feature = "net")]
impl Default for UreqTransport {
    fn default() -> Self {
        Self { timeout_secs: 30 }
    }
}

#[cfg(feature = "net")]
impl UreqTransport {
    /// A transport with the default 30-second timeout.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the per-request timeout in seconds.
    #[must_use]
    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }
}

#[cfg(feature = "net")]
impl HttpTransport for UreqTransport {
    fn post_json(&self, url: &str, body: &str, headers: &[(&str, &str)]) -> Result<String> {
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .build();
        let mut req = agent.post(url).set("Content-Type", "application/json");
        for (k, v) in headers {
            req = req.set(k, v);
        }
        match req.send_string(body) {
            Ok(resp) => resp
                .into_string()
                .map_err(|e| GraphError::Backend(format!("could not read response body: {e}"))),
            Err(e) => Err(GraphError::Backend(format!("http request to {url} failed: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HttpTransport, StaticTransport};

    #[test]
    fn ok_transport_returns_preset_body() {
        let t = StaticTransport::ok("hello");
        assert_eq!(t.post_json("http://x", "{}", &[]).expect("ok"), "hello");
    }

    #[test]
    fn failing_transport_yields_backend_error() {
        let t = StaticTransport::failing("connection refused");
        let err = t.post_json("http://x", "{}", &[]).expect_err("must fail");
        assert_eq!(err.kind(), "backend");
        assert!(err.to_string().contains("connection refused"));
    }

    #[test]
    fn records_the_url_and_body() {
        let t = StaticTransport::ok("r");
        let _ = t.post_json("http://h/api", "{\"a\":1}", &[]);
        let rec = t.last_request().expect("recorded");
        assert_eq!(rec.url, "http://h/api");
        assert_eq!(rec.body, "{\"a\":1}");
    }

    #[test]
    fn records_headers_as_owned_pairs() {
        let t = StaticTransport::ok("r");
        let _ = t.post_json("http://h", "{}", &[("Authorization", "Bearer k")]);
        let rec = t.last_request().expect("recorded");
        assert_eq!(rec.headers, vec![("Authorization".into(), "Bearer k".into())]);
    }

    #[test]
    fn last_request_is_none_before_any_call() {
        let t = StaticTransport::ok("r");
        assert!(t.last_request().is_none());
    }

    #[test]
    fn last_request_tracks_the_most_recent() {
        let t = StaticTransport::ok("r");
        let _ = t.post_json("http://one", "1", &[]);
        let _ = t.post_json("http://two", "2", &[]);
        assert_eq!(t.last_request().expect("rec").url, "http://two");
    }

    #[test]
    fn transport_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StaticTransport>();
    }
}
