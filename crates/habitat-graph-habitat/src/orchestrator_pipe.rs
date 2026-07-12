//! Orchestrator `cc-pipe` verb-dispatch — `map.scope`, `map.health`, `map.query` ACK/NACK schema.
//!
//! # Overview
//!
//! The orchestrator pipe protocol is a newline-delimited JSON RPC over the `cc-pipe` bus.
//! This module owns the wire schema ([`PipeRequest`], [`PipeResponse`]), the single
//! dispatcher ([`handle_request`]), the codec pair ([`parse_request`] /
//! [`serialize_response`]), and the [`PipeTransport`] boundary trait.
//!
//! ## Transports
//!
//! | Transport | Feature | Remarks |
//! |---|---|---|
//! | [`LoopbackTransport`] | always | in-process round-trip; zero network/spawn |
//! | [`live::ProcessTransport`] | `live` | shells out to `cc-pipe` |
//!
//! Tests exclusively use [`LoopbackTransport`] — zero I/O, zero process spawning.
//!
//! ## Validation order in [`handle_request`]
//!
//! 1. `scope.trim()` is empty → `NACK_SCHEMA_INVALID` (structural check first).
//! 2. `verb` not in the supported set → `NACK_UNKNOWN_VERB` (semantic routing check second).
//! 3. Otherwise → `Ack` with render-safe strings.

use habitat_graph_core::{display_safe, sanitize_label, GraphError, Result};
use serde::{Deserialize, Serialize};

/// Verb strings accepted by [`handle_request`] without a `NACK_UNKNOWN_VERB` response.
const SUPPORTED_VERBS: &[&str] = &["map.scope", "map.health", "map.query"];

/// An incoming request on the orchestrator `cc-pipe` bus.
///
/// Serializes to / deserializes from newline-delimited JSON. Use [`parse_request`] for
/// decoding and [`serialize_response`] on the matching [`PipeResponse`].
///
/// # JSON example
///
/// ```json
/// {"verb":"map.scope","scope":"habitat-graph-habitat","payload":null}
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipeRequest {
    /// The dispatch verb (e.g. `"map.scope"`, `"map.health"`, `"map.query"`).
    pub verb: String,
    /// The target scope (crate name, cluster name, file path …).
    ///
    /// Must be non-empty after Unicode-whitespace trimming; otherwise [`handle_request`]
    /// returns a `NACK_SCHEMA_INVALID` response.
    pub scope: String,
    /// Verb-specific payload; pass [`serde_json::Value::Null`] when unused.
    pub payload: serde_json::Value,
}

/// The response produced by [`handle_request`] or a [`PipeTransport`] round-trip.
///
/// Uses an internally-tagged serde representation with the discriminant key `"type"`:
///
/// ```json
/// {"type":"Ack","verb":"map.scope","detail":"dispatched map.scope for scope \"x\""}
/// {"type":"Nack","code":"NACK_UNKNOWN_VERB","reason":"verb \"foo\" is not supported; …"}
/// ```
///
/// The `"type"` key makes the JSON self-describing and round-trippable without a wrapper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PipeResponse {
    /// The request was accepted and dispatched.
    Ack {
        /// Echo of the dispatched verb (render-safe).
        verb: String,
        /// Human-readable detail describing what was dispatched (render-safe).
        detail: String,
    },
    /// The request was rejected before dispatch.
    Nack {
        /// Machine-readable rejection code.
        ///
        /// Known codes: `"NACK_UNKNOWN_VERB"`, `"NACK_SCHEMA_INVALID"`.
        code: String,
        /// Human-readable explanation (render-safe).
        reason: String,
    },
}

/// Dispatches a [`PipeRequest`], returning an [`Ack`](PipeResponse::Ack) or
/// [`Nack`](PipeResponse::Nack) — never fails.
///
/// Validation order (first failing check wins):
///
/// 1. `scope.trim()` empty → `Nack { code: "NACK_SCHEMA_INVALID" }`.
/// 2. `verb` not in `["map.scope", "map.health", "map.query"]` →
///    `Nack { code: "NACK_UNKNOWN_VERB" }`.
/// 3. Otherwise → `Ack { verb, detail }` with render-safe strings built via
///    [`sanitize_label`] + [`display_safe`].
#[must_use]
pub fn handle_request(req: &PipeRequest) -> PipeResponse {
    // — Structural check: scope must be non-empty after whitespace trim —
    if req.scope.trim().is_empty() {
        return PipeResponse::Nack {
            code: "NACK_SCHEMA_INVALID".to_string(),
            reason: "field `scope` must be non-empty after trimming".to_string(),
        };
    }

    let safe_verb = display_safe(&sanitize_label(&req.verb));
    let safe_scope = display_safe(&sanitize_label(&req.scope));

    // — Semantic check: verb must be one of the supported set —
    if !SUPPORTED_VERBS.contains(&req.verb.as_str()) {
        return PipeResponse::Nack {
            code: "NACK_UNKNOWN_VERB".to_string(),
            reason: format!(
                "verb {safe_verb:?} is not supported; accepted: {}",
                SUPPORTED_VERBS.join(", ")
            ),
        };
    }

    // Compute detail before moving safe_verb into the struct field.
    let detail = format!("dispatched {safe_verb} for scope {safe_scope:?}");
    PipeResponse::Ack {
        verb: safe_verb,
        detail,
    }
}

/// Parses a JSON string into a [`PipeRequest`].
///
/// # Errors
///
/// Returns [`GraphError::Schema`] when `json` is not valid JSON or does not match the
/// [`PipeRequest`] schema (e.g. missing required fields).
#[must_use = "the parsed request must be used to dispatch a verb"]
pub fn parse_request(json: &str) -> Result<PipeRequest> {
    serde_json::from_str(json)
        .map_err(|e| GraphError::Schema(format!("invalid PipeRequest JSON: {e}")))
}

/// Serializes a [`PipeResponse`] to a compact JSON string.
///
/// # Errors
///
/// Returns [`GraphError::Schema`] if serialization fails. In practice this is unreachable
/// for well-formed [`PipeResponse`] values, but the error path exists for forward-compatibility.
#[must_use = "the serialized response must be sent to the caller"]
pub fn serialize_response(r: &PipeResponse) -> Result<String> {
    serde_json::to_string(r)
        .map_err(|e| GraphError::Schema(format!("could not serialize PipeResponse: {e}")))
}

/// A line-oriented transport over the orchestrator `cc-pipe` bus.
///
/// Each call to [`send`](PipeTransport::send) carries exactly one JSON-encoded
/// [`PipeRequest`] and returns a JSON-encoded [`PipeResponse`].
pub trait PipeTransport {
    /// Sends `line` (a JSON-encoded [`PipeRequest`]) and returns a JSON-encoded
    /// [`PipeResponse`] as a [`String`].
    ///
    /// # Errors
    ///
    /// Returns [`GraphError`] if `line` cannot be delivered, or if the response cannot be
    /// received or decoded.
    fn send(&self, line: &str) -> Result<String>;
}

/// An in-process, zero-I/O [`PipeTransport`] for use in tests and offline tooling.
///
/// Each [`send`](PipeTransport::send) call performs a full in-process round-trip:
///
/// 1. Parses `line` as a [`PipeRequest`] via [`parse_request`].
/// 2. Dispatches through [`handle_request`].
/// 3. Serializes the response via [`serialize_response`].
///
/// No network, no filesystem, no process spawning — safe in every build mode.
#[derive(Debug, Default, Clone, Copy)]
pub struct LoopbackTransport;

impl LoopbackTransport {
    /// Creates a new [`LoopbackTransport`].
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl PipeTransport for LoopbackTransport {
    fn send(&self, line: &str) -> Result<String> {
        let req = parse_request(line)?;
        let resp = handle_request(&req);
        serialize_response(&resp)
    }
}

/// Live adapters that shell out to the `cc-pipe` binary. Only compiled with the `live` feature.
#[cfg(feature = "live")]
pub mod live {
    use std::io::Write as _;

    use habitat_graph_core::{GraphError, Result};

    use super::PipeTransport;

    /// A [`PipeTransport`] that invokes the `cc-pipe` binary in a child process.
    ///
    /// Each [`send`](PipeTransport::send) call spawns one subprocess, writes the request line
    /// to its stdin, and reads the response from stdout.
    ///
    /// The binary must accept a JSON-encoded [`super::PipeRequest`] on stdin and write a
    /// JSON-encoded [`super::PipeResponse`] to stdout before exiting with status 0.
    #[derive(Debug, Clone)]
    pub struct ProcessTransport {
        binary: String,
    }

    impl Default for ProcessTransport {
        fn default() -> Self {
            Self {
                binary: "cc-pipe".to_string(),
            }
        }
    }

    impl ProcessTransport {
        /// Creates a transport using the `cc-pipe` binary located on `PATH`.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Creates a transport using a custom binary path instead of the default `cc-pipe`.
        #[must_use]
        pub fn with_binary(binary: impl Into<String>) -> Self {
            Self {
                binary: binary.into(),
            }
        }
    }

    impl PipeTransport for ProcessTransport {
        fn send(&self, line: &str) -> Result<String> {
            let mut child = std::process::Command::new(&self.binary)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| GraphError::Io(format!("failed to spawn {}: {e}", self.binary)))?;

            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(line.as_bytes())
                    .map_err(|e| GraphError::Io(format!("write to cc-pipe stdin: {e}")))?;
                stdin
                    .write_all(b"\n")
                    .map_err(|e| GraphError::Io(format!("write newline to cc-pipe stdin: {e}")))?;
            }

            let output = child
                .wait_with_output()
                .map_err(|e| GraphError::Io(format!("waiting for cc-pipe output: {e}")))?;

            if !output.status.success() {
                return Err(GraphError::Daemon(format!(
                    "cc-pipe exited with status {}",
                    output.status
                )));
            }

            String::from_utf8(output.stdout)
                .map_err(|e| GraphError::Io(format!("cc-pipe stdout is not valid UTF-8: {e}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::{
        handle_request, parse_request, serialize_response, LoopbackTransport, PipeRequest,
        PipeResponse, PipeTransport, SUPPORTED_VERBS,
    };

    // ── test helpers ──────────────────────────────────────────────────────────

    fn req(verb: &str, scope: &str) -> PipeRequest {
        PipeRequest {
            verb: verb.to_string(),
            scope: scope.to_string(),
            payload: Value::Null,
        }
    }

    fn req_json(verb: &str, scope: &str) -> String {
        serde_json::to_string(&json!({
            "verb": verb,
            "scope": scope,
            "payload": null,
        }))
        .expect("helper serialisation never fails")
    }

    fn loopback() -> LoopbackTransport {
        LoopbackTransport::new()
    }

    // ── handle_request — ACK paths ────────────────────────────────────────────

    #[test]
    fn ack_on_valid_map_scope() {
        let resp = handle_request(&req("map.scope", "habitat-graph-habitat"));
        assert!(
            matches!(resp, PipeResponse::Ack { .. }),
            "expected Ack, got {resp:?}"
        );
    }

    #[test]
    fn ack_on_map_health() {
        let resp = handle_request(&req("map.health", "some-service"));
        assert!(
            matches!(resp, PipeResponse::Ack { .. }),
            "expected Ack, got {resp:?}"
        );
    }

    #[test]
    fn ack_on_map_query() {
        let resp = handle_request(&req("map.query", "pattern-x"));
        assert!(
            matches!(resp, PipeResponse::Ack { .. }),
            "expected Ack, got {resp:?}"
        );
    }

    #[test]
    fn ack_verb_echoed_in_response() {
        let resp = handle_request(&req("map.scope", "my-crate"));
        if let PipeResponse::Ack { verb, .. } = resp {
            assert_eq!(verb, "map.scope");
        } else {
            panic!("expected Ack");
        }
    }

    #[test]
    fn ack_detail_contains_scope() {
        let resp = handle_request(&req("map.scope", "my-crate"));
        if let PipeResponse::Ack { detail, .. } = resp {
            assert!(
                detail.contains("my-crate"),
                "detail missing scope: {detail}"
            );
        } else {
            panic!("expected Ack");
        }
    }

    #[test]
    fn ack_detail_contains_verb() {
        for verb in SUPPORTED_VERBS {
            let resp = handle_request(&req(verb, "some-scope"));
            if let PipeResponse::Ack { detail, .. } = resp {
                assert!(
                    detail.contains(verb),
                    "detail missing verb {verb}: {detail}"
                );
            } else {
                panic!("expected Ack for verb {verb}");
            }
        }
    }

    #[test]
    fn ack_detail_is_render_safe_bidi_override_in_scope() {
        // RLO (U+202E) in scope must be escaped before reaching the Ack detail.
        let evil_scope = "crate\u{202E}name";
        let resp = handle_request(&req("map.scope", evil_scope));
        if let PipeResponse::Ack { detail, .. } = resp {
            assert!(
                !detail.contains('\u{202E}'),
                "raw RLO leaked into detail: {detail:?}"
            );
        } else {
            panic!("expected Ack for non-empty scope");
        }
    }

    #[test]
    fn scope_with_leading_trailing_spaces_is_accepted() {
        // Non-empty after trim → ACK (trim is only used for the emptiness check).
        let resp = handle_request(&req("map.scope", "  my-scope  "));
        assert!(matches!(resp, PipeResponse::Ack { .. }));
    }

    #[test]
    fn unicode_scope_is_accepted() {
        let resp = handle_request(&req("map.scope", "crate_café→graph"));
        assert!(
            matches!(resp, PipeResponse::Ack { .. }),
            "Unicode scope should produce Ack"
        );
    }

    // ── handle_request — NACK_SCHEMA_INVALID paths ───────────────────────────

    #[test]
    fn nack_schema_invalid_on_empty_scope() {
        let resp = handle_request(&req("map.scope", ""));
        match resp {
            PipeResponse::Nack { ref code, .. } => assert_eq!(code, "NACK_SCHEMA_INVALID"),
            other @ PipeResponse::Ack { .. } => panic!("expected Nack, got {other:?}"),
        }
    }

    #[test]
    fn nack_schema_invalid_on_whitespace_only_scope() {
        let resp = handle_request(&req("map.scope", "   "));
        match resp {
            PipeResponse::Nack { ref code, .. } => assert_eq!(code, "NACK_SCHEMA_INVALID"),
            other @ PipeResponse::Ack { .. } => panic!("expected Nack, got {other:?}"),
        }
    }

    #[test]
    fn nack_schema_invalid_on_tab_only_scope() {
        let resp = handle_request(&req("map.health", "\t\t\t"));
        match resp {
            PipeResponse::Nack { ref code, .. } => assert_eq!(code, "NACK_SCHEMA_INVALID"),
            other @ PipeResponse::Ack { .. } => panic!("expected Nack, got {other:?}"),
        }
    }

    #[test]
    fn nack_schema_invalid_on_newline_scope() {
        let resp = handle_request(&req("map.query", "\n"));
        match resp {
            PipeResponse::Nack { ref code, .. } => assert_eq!(code, "NACK_SCHEMA_INVALID"),
            other @ PipeResponse::Ack { .. } => panic!("expected Nack, got {other:?}"),
        }
    }

    #[test]
    fn scope_validated_before_verb_when_both_invalid() {
        // Both scope is empty AND verb is unknown — scope (structural) check fires first.
        let resp = handle_request(&req("frobnicate", ""));
        match resp {
            PipeResponse::Nack { ref code, .. } => assert_eq!(code, "NACK_SCHEMA_INVALID"),
            other @ PipeResponse::Ack { .. } => {
                panic!("expected NACK_SCHEMA_INVALID, got {other:?}")
            }
        }
    }

    // ── handle_request — NACK_UNKNOWN_VERB paths ─────────────────────────────

    #[test]
    fn nack_unknown_verb_on_frobnicate() {
        let resp = handle_request(&req("frobnicate", "some-scope"));
        match resp {
            PipeResponse::Nack { ref code, .. } => assert_eq!(code, "NACK_UNKNOWN_VERB"),
            other @ PipeResponse::Ack { .. } => panic!("expected Nack, got {other:?}"),
        }
    }

    #[test]
    fn nack_unknown_verb_on_empty_verb() {
        // Empty verb string is not in the supported set.
        let resp = handle_request(&req("", "some-scope"));
        match resp {
            PipeResponse::Nack { ref code, .. } => assert_eq!(code, "NACK_UNKNOWN_VERB"),
            other @ PipeResponse::Ack { .. } => panic!("expected Nack, got {other:?}"),
        }
    }

    #[test]
    fn nack_reason_mentions_bad_verb() {
        let resp = handle_request(&req("frobnicate", "scope"));
        if let PipeResponse::Nack { reason, .. } = resp {
            assert!(
                reason.contains("frobnicate"),
                "reason missing verb: {reason}"
            );
        } else {
            panic!("expected Nack");
        }
    }

    #[test]
    fn nack_unknown_verb_reason_lists_supported_verbs() {
        let resp = handle_request(&req("bad.verb", "scope"));
        if let PipeResponse::Nack { reason, .. } = resp {
            assert!(
                reason.contains("map.scope"),
                "reason missing map.scope: {reason}"
            );
            assert!(
                reason.contains("map.health"),
                "reason missing map.health: {reason}"
            );
            assert!(
                reason.contains("map.query"),
                "reason missing map.query: {reason}"
            );
        } else {
            panic!("expected Nack");
        }
    }

    // ── parse_request ─────────────────────────────────────────────────────────

    #[test]
    fn parse_request_valid_json() {
        let json = r#"{"verb":"map.scope","scope":"habitat","payload":null}"#;
        let r = parse_request(json).expect("valid JSON");
        assert_eq!(r.verb, "map.scope");
        assert_eq!(r.scope, "habitat");
        assert_eq!(r.payload, Value::Null);
    }

    #[test]
    fn parse_request_rejects_bad_json() {
        let err = parse_request("not json at all").expect_err("bad JSON must fail");
        assert_eq!(err.kind(), "schema");
    }

    #[test]
    fn parse_request_rejects_empty_string() {
        assert!(parse_request("").is_err());
    }

    #[test]
    fn parse_request_rejects_missing_verb() {
        let json = r#"{"scope":"habitat","payload":null}"#;
        assert!(
            parse_request(json).is_err(),
            "missing `verb` must be rejected"
        );
    }

    #[test]
    fn parse_request_rejects_missing_scope() {
        let json = r#"{"verb":"map.scope","payload":null}"#;
        assert!(
            parse_request(json).is_err(),
            "missing `scope` must be rejected"
        );
    }

    #[test]
    fn parse_request_accepts_null_payload() {
        let json = r#"{"verb":"map.health","scope":"svc","payload":null}"#;
        let r = parse_request(json).expect("valid");
        assert_eq!(r.payload, Value::Null);
    }

    #[test]
    fn parse_request_accepts_object_payload() {
        let json = r#"{"verb":"map.query","scope":"svc","payload":{"k":"v"}}"#;
        let r = parse_request(json).expect("valid");
        assert_eq!(r.payload, json!({"k": "v"}));
    }

    #[test]
    fn parse_request_accepts_array_payload() {
        let json = r#"{"verb":"map.scope","scope":"x","payload":[1,2,3]}"#;
        let r = parse_request(json).expect("valid");
        assert_eq!(r.payload, json!([1, 2, 3]));
    }

    #[test]
    fn parse_request_round_trips_through_serde() {
        let original = PipeRequest {
            verb: "map.scope".to_string(),
            scope: "my-crate".to_string(),
            payload: json!({"filters": ["a", "b"]}),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed = parse_request(&json).expect("deserialize");
        assert_eq!(original, parsed);
    }

    #[test]
    fn parse_request_ignores_extra_fields() {
        // serde deny_unknown_fields is NOT set — extra fields are silently ignored.
        let json = r#"{"verb":"map.scope","scope":"x","payload":null,"extra":"ignored"}"#;
        let r = parse_request(json).expect("extra fields should be ignored");
        assert_eq!(r.verb, "map.scope");
    }

    #[test]
    fn parse_request_error_kind_is_schema() {
        let err = parse_request("{bad}").expect_err("bad JSON");
        assert_eq!(
            err.kind(),
            "schema",
            "expected schema error kind, got {err:?}"
        );
    }

    // ── serialize_response ────────────────────────────────────────────────────

    #[test]
    fn serialize_response_ack_has_type_key() {
        let resp = PipeResponse::Ack {
            verb: "map.scope".to_string(),
            detail: "ok".to_string(),
        };
        let json = serialize_response(&resp).expect("serialize");
        let v: Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(v["type"], "Ack");
    }

    #[test]
    fn serialize_response_nack_has_type_key() {
        let resp = PipeResponse::Nack {
            code: "NACK_UNKNOWN_VERB".to_string(),
            reason: "bad".to_string(),
        };
        let json = serialize_response(&resp).expect("serialize");
        let v: Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(v["type"], "Nack");
    }

    #[test]
    fn serialize_response_ack_contains_verb() {
        let resp = PipeResponse::Ack {
            verb: "map.health".to_string(),
            detail: "dispatched".to_string(),
        };
        let json = serialize_response(&resp).expect("serialize");
        assert!(json.contains("map.health"), "verb missing from: {json}");
    }

    #[test]
    fn serialize_response_nack_contains_code() {
        let resp = PipeResponse::Nack {
            code: "NACK_SCHEMA_INVALID".to_string(),
            reason: "empty scope".to_string(),
        };
        let json = serialize_response(&resp).expect("serialize");
        assert!(
            json.contains("NACK_SCHEMA_INVALID"),
            "code missing from: {json}"
        );
    }

    // ── PipeResponse round-trips ──────────────────────────────────────────────

    #[test]
    fn pipe_response_ack_round_trips() {
        let original = PipeResponse::Ack {
            verb: "map.scope".to_string(),
            detail: r#"dispatched map.scope for scope "my-crate""#.to_string(),
        };
        let json = serialize_response(&original).expect("serialize");
        let parsed: PipeResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, parsed);
    }

    #[test]
    fn pipe_response_nack_round_trips() {
        let original = PipeResponse::Nack {
            code: "NACK_UNKNOWN_VERB".to_string(),
            reason: "verb \"xyz\" is not supported; accepted: map.scope, map.health, map.query"
                .to_string(),
        };
        let json = serialize_response(&original).expect("serialize");
        let parsed: PipeResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, parsed);
    }

    #[test]
    fn pipe_response_type_discriminant_always_present() {
        for resp in [
            PipeResponse::Ack {
                verb: "v".to_string(),
                detail: "d".to_string(),
            },
            PipeResponse::Nack {
                code: "c".to_string(),
                reason: "r".to_string(),
            },
        ] {
            let json = serialize_response(&resp).expect("serialize");
            let v: Value = serde_json::from_str(&json).expect("valid JSON");
            assert!(
                v.get("type").is_some(),
                "no 'type' discriminant key in: {json}"
            );
        }
    }

    // ── LoopbackTransport ─────────────────────────────────────────────────────

    #[test]
    fn loopback_end_to_end_ack() {
        let t = loopback();
        let line = req_json("map.scope", "habitat-graph-habitat");
        let resp_str = t.send(&line).expect("loopback must succeed");
        let resp: PipeResponse = serde_json::from_str(&resp_str).expect("parse response");
        assert!(matches!(resp, PipeResponse::Ack { .. }));
    }

    #[test]
    fn loopback_nack_unknown_verb() {
        let t = loopback();
        let line = req_json("frobnicate", "some-scope");
        let resp_str = t.send(&line).expect("loopback must succeed");
        let resp: PipeResponse = serde_json::from_str(&resp_str).expect("parse response");
        match resp {
            PipeResponse::Nack { code, .. } => assert_eq!(code, "NACK_UNKNOWN_VERB"),
            other @ PipeResponse::Ack { .. } => panic!("expected Nack, got {other:?}"),
        }
    }

    #[test]
    fn loopback_nack_empty_scope() {
        let t = loopback();
        let line = req_json("map.scope", "");
        let resp_str = t.send(&line).expect("loopback must succeed");
        let resp: PipeResponse = serde_json::from_str(&resp_str).expect("parse response");
        match resp {
            PipeResponse::Nack { code, .. } => assert_eq!(code, "NACK_SCHEMA_INVALID"),
            other @ PipeResponse::Ack { .. } => panic!("expected Nack, got {other:?}"),
        }
    }

    #[test]
    fn loopback_nack_whitespace_scope() {
        let t = loopback();
        let line = req_json("map.health", "   \t  ");
        let resp_str = t.send(&line).expect("loopback must succeed");
        let resp: PipeResponse = serde_json::from_str(&resp_str).expect("parse response");
        match resp {
            PipeResponse::Nack { code, .. } => assert_eq!(code, "NACK_SCHEMA_INVALID"),
            other @ PipeResponse::Ack { .. } => panic!("expected Nack, got {other:?}"),
        }
    }

    #[test]
    fn loopback_bad_json_returns_schema_error() {
        let t = loopback();
        let err = t.send("not json").expect_err("bad JSON must fail");
        assert_eq!(err.kind(), "schema");
    }

    #[test]
    fn loopback_response_is_always_valid_json() {
        let t = loopback();
        for (verb, scope) in [("map.scope", "x"), ("frobnicate", "y"), ("map.health", "")] {
            let line = req_json(verb, scope);
            let resp_str = t.send(&line).expect("loopback must succeed");
            let _: Value = serde_json::from_str(&resp_str).unwrap_or_else(|_| {
                panic!("loopback returned non-JSON for ({verb},{scope}): {resp_str}")
            });
        }
    }

    #[test]
    fn loopback_map_health_end_to_end() {
        let t = loopback();
        let line = req_json("map.health", "pane-vortex-v2");
        let resp_str = t.send(&line).expect("loopback must succeed");
        let resp: PipeResponse = serde_json::from_str(&resp_str).expect("parse response");
        assert!(matches!(resp, PipeResponse::Ack { .. }));
    }

    #[test]
    fn loopback_map_query_end_to_end() {
        let t = loopback();
        let line = req_json("map.query", "community-leader-nodes");
        let resp_str = t.send(&line).expect("loopback must succeed");
        let resp: PipeResponse = serde_json::from_str(&resp_str).expect("parse response");
        assert!(matches!(resp, PipeResponse::Ack { .. }));
    }

    #[test]
    fn loopback_ack_detail_contains_scope_value() {
        let t = loopback();
        let line = req_json("map.scope", "my-crate");
        let resp_str = t.send(&line).expect("loopback must succeed");
        let v: Value = serde_json::from_str(&resp_str).expect("valid JSON");
        let detail = v["detail"].as_str().expect("detail must be a string");
        assert!(
            detail.contains("my-crate"),
            "detail missing scope: {detail}"
        );
    }

    #[test]
    fn loopback_new_and_default_produce_same_output() {
        let a = LoopbackTransport::new();
        let b = LoopbackTransport;
        let line = req_json("map.scope", "x");
        assert_eq!(
            a.send(&line).expect("a"),
            b.send(&line).expect("b"),
            "LoopbackTransport::new() and Default must be equivalent"
        );
    }

    // ── supported verbs / code constants ─────────────────────────────────────

    #[test]
    fn supported_verbs_has_exactly_three_entries() {
        assert_eq!(SUPPORTED_VERBS.len(), 3);
    }

    #[test]
    fn supported_verbs_contains_map_scope() {
        assert!(SUPPORTED_VERBS.contains(&"map.scope"));
    }

    #[test]
    fn supported_verbs_contains_map_health() {
        assert!(SUPPORTED_VERBS.contains(&"map.health"));
    }

    #[test]
    fn supported_verbs_contains_map_query() {
        assert!(SUPPORTED_VERBS.contains(&"map.query"));
    }

    #[test]
    fn nack_unknown_verb_code_is_exact_string() {
        let resp = handle_request(&req("bad", "scope"));
        if let PipeResponse::Nack { code, .. } = resp {
            assert_eq!(code, "NACK_UNKNOWN_VERB");
        } else {
            panic!("expected Nack");
        }
    }

    #[test]
    fn nack_schema_invalid_code_is_exact_string() {
        let resp = handle_request(&req("map.scope", ""));
        if let PipeResponse::Nack { code, .. } = resp {
            assert_eq!(code, "NACK_SCHEMA_INVALID");
        } else {
            panic!("expected Nack");
        }
    }

    // ── type properties ───────────────────────────────────────────────────────

    #[test]
    fn pipe_request_partial_eq_works() {
        let a = req("map.scope", "x");
        let b = req("map.scope", "x");
        let c = req("map.health", "x");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn pipe_response_partial_eq_works() {
        let a = PipeResponse::Ack {
            verb: "v".to_string(),
            detail: "d".to_string(),
        };
        let b = PipeResponse::Ack {
            verb: "v".to_string(),
            detail: "d".to_string(),
        };
        let c = PipeResponse::Nack {
            code: "c".to_string(),
            reason: "r".to_string(),
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn pipe_request_payload_round_trips_arbitrary_value() {
        let payload = json!({"key": [1, 2, 3], "nested": {"a": true}});
        let original = PipeRequest {
            verb: "map.scope".to_string(),
            scope: "x".to_string(),
            payload: payload.clone(),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: PipeRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.payload, payload);
    }

    // ── ProcessTransport — live adapter (feature = "live") ───────────────────
    // Tests verify construction and pure config surface without spawning any
    // process or making network connections.

    /// `ProcessTransport::new()` must default to the `cc-pipe` binary.
    #[cfg(feature = "live")]
    #[test]
    fn process_transport_default_binary_is_cc_pipe() {
        let t = super::live::ProcessTransport::new();
        let debug = format!("{t:?}");
        assert!(
            debug.contains("cc-pipe"),
            "default binary must be 'cc-pipe' in Debug output: {debug}"
        );
    }

    /// `ProcessTransport::with_binary` must store the provided binary name.
    #[cfg(feature = "live")]
    #[test]
    fn process_transport_with_binary_reflects_in_debug() {
        let t = super::live::ProcessTransport::with_binary("/opt/bin/custom-pipe");
        let debug = format!("{t:?}");
        assert!(
            debug.contains("/opt/bin/custom-pipe"),
            "custom binary path must appear in Debug output: {debug}"
        );
    }

    /// `ProcessTransport::default()` and `ProcessTransport::new()` must produce equal config.
    #[cfg(feature = "live")]
    #[test]
    fn process_transport_default_same_as_new() {
        let via_new = super::live::ProcessTransport::new();
        let via_default = super::live::ProcessTransport::default();
        assert_eq!(
            format!("{via_new:?}"),
            format!("{via_default:?}"),
            "new() and default() must produce the same binary configuration"
        );
    }

    /// `ProcessTransport` must be `Send + Sync`.
    #[cfg(feature = "live")]
    #[test]
    fn process_transport_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<super::live::ProcessTransport>();
    }

    /// `ProcessTransport` must be `Clone`.
    #[cfg(feature = "live")]
    #[test]
    fn process_transport_is_clone() {
        let t = super::live::ProcessTransport::new();
        let cloned = t.clone();
        assert_eq!(
            format!("{t:?}"),
            format!("{cloned:?}"),
            "clone must produce equal configuration"
        );
    }
}
