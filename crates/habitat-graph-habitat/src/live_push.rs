//! Delta-push — best-effort live bridge from [`arc_telemetry::ArcDelta`] to PV2/POVM.
//!
//! This module is compiled only with `feature = "live-bridges"`.  All outbound HTTP is
//! **best-effort**: individual service failures are logged to stderr and swallowed so
//! the rebuild loop that calls [`DeltaPusher::push_delta`] never crashes on unreachable
//! factory services.
//!
//! ## Protocol
//!
//! | Endpoint | Method | Path | Purpose |
//! |---|---|---|---|
//! | PV2  (`:8132`) | POST | `/arc-delta`      | Push regression/recovery arcs to the Kuramoto gauge |
//! | POVM (`:8125`) | POST | `/memory/store`   | Record arc delta in POVM namespace `habitat_graph` |
//!
//! ### PV2 body
//! ```json
//! {
//!   "newly_severed": [{"producer":"A","consumer":"B","relation":"calls"}, …],
//!   "newly_healed":  [{"producer":"X","consumer":"Y","relation":"imports_from"}, …],
//!   "coherence_delta": -0.25
//! }
//! ```
//!
//! ### POVM body
//! ```json
//! {
//!   "namespace": "habitat_graph",
//!   "key": "arc_delta",
//!   "value": "<JSON-string of the PV2 payload>"
//! }
//! ```
//!
//! ## No-crash contract
//!
//! `push_delta` **always** returns `Ok(())` as long as the delta can be serialised.
//! PV2 and POVM failures are logged individually to stderr (prefixed
//! `[habitat-graph]`) so they are diagnosable without stopping the file-watch loop.
//! The only hard error is a serialisation failure on the `ArcDelta` itself — a
//! programming defect that would indicate a bug in `ArcDelta`'s data, not a live
//! service issue.

use crate::arc_graph::Arc as GraphArc;
use crate::arc_telemetry::ArcDelta;
use habitat_graph_backend::HttpTransport;
use habitat_graph_core::{GraphError, Result};

// ─── URL constants ────────────────────────────────────────────────────────────

/// Default PV2 Kuramoto endpoint base URL (Pane-Vortex V2, port `8132`).
pub const DEFAULT_PV2_URL: &str = "http://localhost:8132";

/// Default POVM memory-store endpoint base URL (POVM Engine, port `8125`).
pub const DEFAULT_POVM_URL: &str = "http://localhost:8125";

// ─── Production factory ──────────────────────────────────────────────────────

/// Creates a [`DeltaPusher`] wired to the production HTTP transport
/// ([`UreqTransport`](habitat_graph_backend::UreqTransport)).
///
/// Use this in production code (e.g., the file-watch loop).  In tests, construct
/// [`DeltaPusher::new`] with a mock transport so no real network calls are made.
///
/// # Examples
///
/// ```rust,ignore
/// use habitat_graph_habitat::live_push::default_pusher;
///
/// let pusher = default_pusher();
/// pusher.push_delta(&delta)?;
/// ```
#[must_use]
pub fn default_pusher() -> DeltaPusher {
    use habitat_graph_backend::UreqTransport;
    DeltaPusher::new(Box::new(UreqTransport::new()))
}

// ─── DeltaPusher ──────────────────────────────────────────────────────────────

/// Pushes [`ArcDelta`] events to PV2 (`:8132`) and POVM (`:8125`) after each rebuild.
///
/// ## Best-effort semantics
///
/// Individual service failures are logged to `stderr` and swallowed; the calling
/// rebuild loop must not crash when PV2 or POVM is temporarily unreachable.
///
/// ## Testing
///
/// Inject any [`HttpTransport`] implementation — the crate provides
/// [`StaticTransport`](habitat_graph_backend::StaticTransport) as a zero-dependency
/// deterministic double.  Create custom recoding transports in test modules as shown
/// in the `tests` section below.
///
/// ## Production
///
/// Wire with `habitat_graph_backend::UreqTransport` (available when
/// `habitat-graph-backend` is compiled with `feature = "net"`):
///
/// ```rust,ignore
/// use habitat_graph_backend::UreqTransport;
/// use habitat_graph_habitat::live_push::DeltaPusher;
///
/// let pusher = DeltaPusher::new(Box::new(UreqTransport::new()));
/// ```
pub struct DeltaPusher {
    pv2_url: String,
    povm_url: String,
    transport: Box<dyn HttpTransport>,
}

impl DeltaPusher {
    /// Creates a `DeltaPusher` targeting the default PV2 (`:8132`) and POVM (`:8125`) endpoints.
    ///
    /// # Errors
    ///
    /// This constructor is infallible.  The `push_delta` method can fail on serialisation
    /// (programming error) but individual HTTP failures are logged, not propagated.
    #[must_use]
    pub fn new(transport: Box<dyn HttpTransport>) -> Self {
        Self {
            pv2_url: DEFAULT_PV2_URL.to_owned(),
            povm_url: DEFAULT_POVM_URL.to_owned(),
            transport,
        }
    }

    /// Overrides the PV2 base URL.  Trailing slashes are accepted and stripped.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use habitat_graph_habitat::live_push::DeltaPusher;
    /// use habitat_graph_backend::StaticTransport;
    ///
    /// let pusher = DeltaPusher::new(Box::new(StaticTransport::ok("{}")))
    ///     .with_pv2_url("http://pv2-staging:8132");
    /// ```
    #[must_use]
    pub fn with_pv2_url(mut self, url: impl Into<String>) -> Self {
        self.pv2_url = url.into();
        self
    }

    /// Overrides the POVM base URL.  Trailing slashes are accepted and stripped.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use habitat_graph_habitat::live_push::DeltaPusher;
    /// use habitat_graph_backend::StaticTransport;
    ///
    /// let pusher = DeltaPusher::new(Box::new(StaticTransport::ok("{}")))
    ///     .with_povm_url("http://povm-staging:8125");
    /// ```
    #[must_use]
    pub fn with_povm_url(mut self, url: impl Into<String>) -> Self {
        self.povm_url = url.into();
        self
    }

    /// Pushes `delta` to PV2 and POVM when there are newly-severed or newly-healed arcs.
    ///
    /// Returns without any network calls when `delta.newly_severed` and
    /// `delta.newly_healed` are both empty (no signal worth broadcasting).
    ///
    /// PV2 and POVM failures are each logged to `stderr` independently; both services
    /// are always attempted even if the first fails.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError`] only if the `ArcDelta` cannot be serialised to JSON —
    /// a programming defect, not a live service failure.  All HTTP-level failures
    /// are swallowed and logged.
    pub fn push_delta(&self, delta: &ArcDelta) -> Result<()> {
        // Empty delta — nothing worth broadcasting.
        if delta.newly_severed.is_empty() && delta.newly_healed.is_empty() {
            return Ok(());
        }

        // Build JSON arrays for the arc lists.
        let severed_json: Vec<serde_json::Value> =
            delta.newly_severed.iter().map(arc_to_json).collect();
        let healed_json: Vec<serde_json::Value> =
            delta.newly_healed.iter().map(arc_to_json).collect();

        // ── PV2 push ─────────────────────────────────────────────────────────
        let pv2_body = serde_json::to_string(&serde_json::json!({
            "newly_severed": &severed_json,
            "newly_healed": &healed_json,
            "coherence_delta": delta.coherence_delta,
        }))
        .map_err(|e| GraphError::Io(format!("DeltaPusher: PV2 body serialisation failed: {e}")))?;

        let pv2_endpoint = format!("{}/arc-delta", self.pv2_url.trim_end_matches('/'));
        if let Err(e) = self.transport.post_json(&pv2_endpoint, &pv2_body, &[]) {
            eprintln!(
                "[habitat-graph] DeltaPusher: PV2 push to {pv2_endpoint} failed (non-fatal): {e}"
            );
        }

        // ── POVM push ────────────────────────────────────────────────────────
        // POVM stores the full delta as a JSON string inside `value`.
        let delta_value_str = serde_json::to_string(&serde_json::json!({
            "newly_severed": &severed_json,
            "newly_healed": &healed_json,
            "coherence_delta": delta.coherence_delta,
        }))
        .map_err(|e| {
            GraphError::Io(format!("DeltaPusher: POVM value serialisation failed: {e}"))
        })?;

        let povm_body = serde_json::to_string(&serde_json::json!({
            "namespace": "habitat_graph",
            "key": "arc_delta",
            "value": delta_value_str,
        }))
        .map_err(|e| GraphError::Io(format!("DeltaPusher: POVM body serialisation failed: {e}")))?;

        let povm_endpoint = format!("{}/memory/store", self.povm_url.trim_end_matches('/'));
        if let Err(e) = self.transport.post_json(&povm_endpoint, &povm_body, &[]) {
            eprintln!(
                "[habitat-graph] DeltaPusher: POVM push to {povm_endpoint} failed (non-fatal): {e}"
            );
        }

        Ok(())
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Serialises a [`GraphArc`] to a `serde_json::Value` with `producer`, `consumer`, `relation`.
fn arc_to_json(a: &GraphArc) -> serde_json::Value {
    serde_json::json!({
        "producer": a.producer,
        "consumer": a.consumer,
        "relation": a.relation,
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{DeltaPusher, DEFAULT_POVM_URL, DEFAULT_PV2_URL};
    use crate::arc_graph::Arc as GraphArc;
    use crate::arc_telemetry::ArcDelta;
    use habitat_graph_backend::HttpTransport;
    use habitat_graph_core::{GraphError, Result};
    use std::collections::VecDeque;
    use std::sync::{Arc as StdArc, Mutex};

    // ── Test doubles ─────────────────────────────────────────────────────────

    /// Always returns the same response; records every `(url, body)` call.
    struct ConstTransport {
        response: std::result::Result<String, String>,
        calls: Mutex<Vec<(String, String)>>,
    }

    impl ConstTransport {
        fn ok() -> StdArc<Self> {
            StdArc::new(Self {
                response: Ok("{}".into()),
                calls: Mutex::new(vec![]),
            })
        }

        fn failing(msg: &str) -> StdArc<Self> {
            StdArc::new(Self {
                response: Err(msg.into()),
                calls: Mutex::new(vec![]),
            })
        }

        fn call_count(&self) -> usize {
            self.calls.lock().map(|g| g.len()).unwrap_or(0)
        }

        fn calls_snapshot(&self) -> Vec<(String, String)> {
            self.calls.lock().map(|g| g.clone()).unwrap_or_default()
        }
    }

    /// Newtype handle so we can implement the foreign `HttpTransport` trait for a local type
    /// while keeping the `StdArc<ConstTransport>` alive for inspection in test bodies.
    struct ConstHandle(StdArc<ConstTransport>);

    impl HttpTransport for ConstHandle {
        fn post_json(&self, url: &str, body: &str, _headers: &[(&str, &str)]) -> Result<String> {
            if let Ok(mut guard) = self.0.calls.lock() {
                guard.push((url.to_owned(), body.to_owned()));
            }
            match &self.0.response {
                Ok(s) => Ok(s.clone()),
                Err(msg) => Err(GraphError::Backend(msg.clone())),
            }
        }
    }

    /// Returns responses in sequence; defaults to `Ok("{}")` once the queue drains.
    struct SeqTransport {
        responses: Mutex<VecDeque<std::result::Result<String, String>>>,
        calls: Mutex<Vec<(String, String)>>,
    }

    impl SeqTransport {
        fn with_responses(responses: Vec<std::result::Result<String, String>>) -> StdArc<Self> {
            StdArc::new(Self {
                responses: Mutex::new(responses.into()),
                calls: Mutex::new(vec![]),
            })
        }

        fn call_count(&self) -> usize {
            self.calls.lock().map(|g| g.len()).unwrap_or(0)
        }

        fn calls_snapshot(&self) -> Vec<(String, String)> {
            self.calls.lock().map(|g| g.clone()).unwrap_or_default()
        }
    }

    /// Newtype handle for `SeqTransport` — same orphan-rule fix as [`ConstHandle`].
    struct SeqHandle(StdArc<SeqTransport>);

    impl HttpTransport for SeqHandle {
        fn post_json(&self, url: &str, body: &str, _headers: &[(&str, &str)]) -> Result<String> {
            if let Ok(mut guard) = self.0.calls.lock() {
                guard.push((url.to_owned(), body.to_owned()));
            }
            let resp = self.0.responses.lock().ok().and_then(|mut g| g.pop_front());
            match resp {
                Some(Ok(s)) => Ok(s),
                Some(Err(msg)) => Err(GraphError::Backend(msg)),
                None => Ok("{}".into()),
            }
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn graph_arc(p: &str, c: &str, r: &str) -> GraphArc {
        GraphArc {
            producer: p.to_owned(),
            consumer: c.to_owned(),
            relation: r.to_owned(),
        }
    }

    fn calls_arc(p: &str, c: &str) -> GraphArc {
        graph_arc(p, c, "calls")
    }

    fn empty_delta() -> ArcDelta {
        ArcDelta {
            newly_severed: vec![],
            newly_healed: vec![],
            coherence_delta: 0.0,
        }
    }

    fn severed_delta(arcs: Vec<GraphArc>) -> ArcDelta {
        let count = arcs.len();
        #[allow(clippy::cast_precision_loss)]
        ArcDelta {
            newly_severed: arcs,
            newly_healed: vec![],
            coherence_delta: -(count as f64),
        }
    }

    fn healed_delta(arcs: Vec<GraphArc>) -> ArcDelta {
        let count = arcs.len();
        #[allow(clippy::cast_precision_loss)]
        ArcDelta {
            newly_severed: vec![],
            newly_healed: arcs,
            coherence_delta: count as f64,
        }
    }

    fn mixed_delta() -> ArcDelta {
        ArcDelta {
            newly_severed: vec![calls_arc("A", "B")],
            newly_healed: vec![calls_arc("X", "Y")],
            coherence_delta: 0.0,
        }
    }

    /// Wraps a shared `ConstTransport` in a `ConstHandle` and boxes it for the pusher,
    /// while the caller keeps the `StdArc` for call inspection.
    fn pusher_const(t: StdArc<ConstTransport>) -> DeltaPusher {
        DeltaPusher::new(Box::new(ConstHandle(t)))
    }

    /// Same pattern for `SeqTransport`.
    fn pusher_seq(t: StdArc<SeqTransport>) -> DeltaPusher {
        DeltaPusher::new(Box::new(SeqHandle(t)))
    }

    // ── Group 1: URL constants ────────────────────────────────────────────────

    #[test]
    fn default_pv2_url_constant_is_localhost_8132() {
        assert_eq!(DEFAULT_PV2_URL, "http://localhost:8132");
    }

    #[test]
    fn default_povm_url_constant_is_localhost_8125() {
        assert_eq!(DEFAULT_POVM_URL, "http://localhost:8125");
    }

    // ── Group 2: DeltaPusher construction ────────────────────────────────────

    #[test]
    fn new_applies_default_pv2_url() {
        let t = ConstTransport::ok();
        let p = DeltaPusher::new(Box::new(ConstHandle(StdArc::clone(&t))));
        assert_eq!(p.pv2_url, DEFAULT_PV2_URL);
    }

    #[test]
    fn new_applies_default_povm_url() {
        let t = ConstTransport::ok();
        let p = DeltaPusher::new(Box::new(ConstHandle(StdArc::clone(&t))));
        assert_eq!(p.povm_url, DEFAULT_POVM_URL);
    }

    #[test]
    fn with_pv2_url_overrides_default() {
        let t = ConstTransport::ok();
        let p = DeltaPusher::new(Box::new(ConstHandle(StdArc::clone(&t))))
            .with_pv2_url("http://pv2-alt:9132");
        assert_eq!(p.pv2_url, "http://pv2-alt:9132");
    }

    #[test]
    fn with_povm_url_overrides_default() {
        let t = ConstTransport::ok();
        let p = DeltaPusher::new(Box::new(ConstHandle(StdArc::clone(&t))))
            .with_povm_url("http://povm-alt:9125");
        assert_eq!(p.povm_url, "http://povm-alt:9125");
    }

    #[test]
    fn builder_chain_applies_both_url_overrides() {
        let t = ConstTransport::ok();
        let p = DeltaPusher::new(Box::new(ConstHandle(StdArc::clone(&t))))
            .with_pv2_url("http://pv2:1")
            .with_povm_url("http://povm:2");
        assert_eq!(p.pv2_url, "http://pv2:1");
        assert_eq!(p.povm_url, "http://povm:2");
    }

    // ── Group 3: Empty delta → no push ───────────────────────────────────────

    #[test]
    fn empty_delta_makes_zero_transport_calls() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&empty_delta()).expect("must not fail");
        assert_eq!(
            t.call_count(),
            0,
            "empty delta must not trigger any HTTP calls"
        );
    }

    #[test]
    fn empty_delta_returns_ok() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        assert!(p.push_delta(&empty_delta()).is_ok());
    }

    #[test]
    fn empty_delta_with_nonzero_coherence_delta_still_no_calls() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        let delta = ArcDelta {
            newly_severed: vec![],
            newly_healed: vec![],
            coherence_delta: -0.9999,
        };
        p.push_delta(&delta).expect("ok");
        assert_eq!(
            t.call_count(),
            0,
            "coherence change alone must not trigger push"
        );
    }

    #[test]
    fn empty_delta_called_multiple_times_still_no_calls() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        for _ in 0..5 {
            p.push_delta(&empty_delta()).expect("ok");
        }
        assert_eq!(t.call_count(), 0);
    }

    #[test]
    fn zero_coherence_delta_empty_arcs_is_no_op() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&ArcDelta {
            newly_severed: vec![],
            newly_healed: vec![],
            coherence_delta: 0.0,
        })
        .expect("ok");
        assert_eq!(t.call_count(), 0);
    }

    // ── Group 4: Non-empty delta makes exactly 2 calls ───────────────────────

    #[test]
    fn delta_with_severed_arcs_makes_two_calls() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        assert_eq!(
            t.call_count(),
            2,
            "non-empty delta must POST to both PV2 and POVM"
        );
    }

    #[test]
    fn delta_with_healed_arcs_makes_two_calls() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&healed_delta(vec![calls_arc("X", "Y")]))
            .expect("ok");
        assert_eq!(t.call_count(), 2);
    }

    #[test]
    fn mixed_delta_makes_two_calls() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&mixed_delta()).expect("ok");
        assert_eq!(t.call_count(), 2);
    }

    #[test]
    fn two_separate_nonempty_pushes_make_four_calls_total() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        p.push_delta(&healed_delta(vec![calls_arc("X", "Y")]))
            .expect("ok");
        assert_eq!(t.call_count(), 4);
    }

    // ── Group 5: Endpoint URL routing ────────────────────────────────────────

    #[test]
    fn first_call_goes_to_pv2_arc_delta_endpoint() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let calls = t.calls_snapshot();
        assert_eq!(
            calls[0].0,
            format!("{DEFAULT_PV2_URL}/arc-delta"),
            "first call must go to PV2 /arc-delta"
        );
    }

    #[test]
    fn second_call_goes_to_povm_memory_store_endpoint() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let calls = t.calls_snapshot();
        assert_eq!(
            calls[1].0,
            format!("{DEFAULT_POVM_URL}/memory/store"),
            "second call must go to POVM /memory/store"
        );
    }

    #[test]
    fn custom_pv2_url_used_in_first_call() {
        let t = ConstTransport::ok();
        let p = DeltaPusher::new(Box::new(ConstHandle(StdArc::clone(&t))))
            .with_pv2_url("http://my-pv2:1234");
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let calls = t.calls_snapshot();
        assert_eq!(calls[0].0, "http://my-pv2:1234/arc-delta");
    }

    #[test]
    fn custom_povm_url_used_in_second_call() {
        let t = ConstTransport::ok();
        let p = DeltaPusher::new(Box::new(ConstHandle(StdArc::clone(&t))))
            .with_povm_url("http://my-povm:5678");
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let calls = t.calls_snapshot();
        assert_eq!(calls[1].0, "http://my-povm:5678/memory/store");
    }

    #[test]
    fn trailing_slash_stripped_from_pv2_url() {
        let t = ConstTransport::ok();
        let p = DeltaPusher::new(Box::new(ConstHandle(StdArc::clone(&t))))
            .with_pv2_url("http://pv2:8132/");
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let calls = t.calls_snapshot();
        assert_eq!(calls[0].0, "http://pv2:8132/arc-delta");
    }

    #[test]
    fn trailing_slash_stripped_from_povm_url() {
        let t = ConstTransport::ok();
        let p = DeltaPusher::new(Box::new(ConstHandle(StdArc::clone(&t))))
            .with_povm_url("http://povm:8125/");
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let calls = t.calls_snapshot();
        assert_eq!(calls[1].0, "http://povm:8125/memory/store");
    }

    // ── Group 6: PV2 request body shape ──────────────────────────────────────

    fn pv2_body_value(t: &StdArc<ConstTransport>) -> serde_json::Value {
        let body = &t.calls_snapshot()[0].1;
        serde_json::from_str(body).expect("PV2 body must be valid JSON")
    }

    #[test]
    fn pv2_body_has_newly_severed_key() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let v = pv2_body_value(&t);
        assert!(
            v.get("newly_severed").is_some(),
            "PV2 body must have 'newly_severed'"
        );
    }

    #[test]
    fn pv2_body_has_newly_healed_key() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let v = pv2_body_value(&t);
        assert!(
            v.get("newly_healed").is_some(),
            "PV2 body must have 'newly_healed'"
        );
    }

    #[test]
    fn pv2_body_has_coherence_delta_key() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let v = pv2_body_value(&t);
        assert!(
            v.get("coherence_delta").is_some(),
            "PV2 body must have 'coherence_delta'"
        );
    }

    #[test]
    fn pv2_body_newly_severed_is_array() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let v = pv2_body_value(&t);
        assert!(
            v["newly_severed"].is_array(),
            "PV2 body newly_severed must be a JSON array"
        );
    }

    #[test]
    fn pv2_body_newly_healed_is_array() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&healed_delta(vec![calls_arc("X", "Y")]))
            .expect("ok");
        let v = pv2_body_value(&t);
        assert!(v["newly_healed"].is_array());
    }

    #[test]
    fn pv2_body_arc_has_producer_field() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("MyProducer", "MyConsumer")]))
            .expect("ok");
        let v = pv2_body_value(&t);
        let arc = &v["newly_severed"][0];
        assert_eq!(arc["producer"], "MyProducer");
    }

    #[test]
    fn pv2_body_arc_has_consumer_field() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("P", "MyConsumer")]))
            .expect("ok");
        let v = pv2_body_value(&t);
        assert_eq!(v["newly_severed"][0]["consumer"], "MyConsumer");
    }

    #[test]
    fn pv2_body_arc_has_relation_field() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        let delta = severed_delta(vec![graph_arc("A", "B", "imports_from")]);
        p.push_delta(&delta).expect("ok");
        let v = pv2_body_value(&t);
        assert_eq!(v["newly_severed"][0]["relation"], "imports_from");
    }

    #[test]
    fn pv2_body_coherence_delta_value_matches() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        let delta = ArcDelta {
            newly_severed: vec![calls_arc("A", "B")],
            newly_healed: vec![],
            coherence_delta: -0.75,
        };
        p.push_delta(&delta).expect("ok");
        let v = pv2_body_value(&t);
        let cd = v["coherence_delta"].as_f64().expect("f64");
        assert!((cd - (-0.75)).abs() < f64::EPSILON);
    }

    // ── Group 7: POVM request body shape ─────────────────────────────────────

    fn povm_body_value(t: &StdArc<ConstTransport>) -> serde_json::Value {
        let body = &t.calls_snapshot()[1].1;
        serde_json::from_str(body).expect("POVM body must be valid JSON")
    }

    #[test]
    fn povm_body_namespace_is_habitat_graph() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let v = povm_body_value(&t);
        assert_eq!(v["namespace"], "habitat_graph");
    }

    #[test]
    fn povm_body_key_is_arc_delta() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let v = povm_body_value(&t);
        assert_eq!(v["key"], "arc_delta");
    }

    #[test]
    fn povm_body_has_value_key() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let v = povm_body_value(&t);
        assert!(v.get("value").is_some(), "POVM body must have 'value' key");
    }

    #[test]
    fn povm_body_value_is_valid_json_string() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let v = povm_body_value(&t);
        // value should be a JSON string that is itself parseable JSON.
        let inner_str = v["value"].as_str().expect("value must be a string");
        let inner: serde_json::Value =
            serde_json::from_str(inner_str).expect("value string must be valid JSON");
        assert!(inner.get("newly_severed").is_some());
        assert!(inner.get("newly_healed").is_some());
        assert!(inner.get("coherence_delta").is_some());
    }

    // ── Group 8: PV2 failure resilience ──────────────────────────────────────

    #[test]
    fn pv2_failure_returns_ok() {
        let t = ConstTransport::failing("connection refused");
        let p = pusher_const(StdArc::clone(&t));
        let result = p.push_delta(&severed_delta(vec![calls_arc("A", "B")]));
        assert!(result.is_ok(), "PV2 failure must not propagate as Err");
    }

    #[test]
    fn pv2_failure_still_calls_povm() {
        let t = SeqTransport::with_responses(vec![
            Err("pv2 down".into()), // first call (PV2) fails
            Ok("{}".into()),        // second call (POVM) succeeds
        ]);
        let p = pusher_seq(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        assert_eq!(
            t.call_count(),
            2,
            "POVM must still be called after PV2 failure"
        );
        // Second call must go to POVM
        let calls = t.calls_snapshot();
        assert!(calls[1].0.contains("/memory/store"));
    }

    #[test]
    fn povm_failure_returns_ok() {
        let t = SeqTransport::with_responses(vec![
            Ok("{}".into()),            // PV2 ok
            Err("povm offline".into()), // POVM fails
        ]);
        let p = pusher_seq(StdArc::clone(&t));
        let result = p.push_delta(&severed_delta(vec![calls_arc("A", "B")]));
        assert!(result.is_ok(), "POVM failure must not propagate as Err");
    }

    #[test]
    fn both_services_fail_returns_ok() {
        let t = SeqTransport::with_responses(vec![Err("pv2 down".into()), Err("povm down".into())]);
        let p = pusher_seq(StdArc::clone(&t));
        let result = p.push_delta(&severed_delta(vec![calls_arc("A", "B")]));
        assert!(result.is_ok(), "both failures must still return Ok");
    }

    #[test]
    fn pv2_failure_total_call_count_is_still_two() {
        let t = ConstTransport::failing("refused");
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        assert_eq!(
            t.call_count(),
            2,
            "both endpoints attempted even when PV2 fails"
        );
    }

    #[test]
    fn povm_failure_after_pv2_success_call_count_is_two() {
        let t = SeqTransport::with_responses(vec![Ok("{}".into()), Err("povm down".into())]);
        let p = pusher_seq(StdArc::clone(&t));
        p.push_delta(&severed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        assert_eq!(t.call_count(), 2);
    }

    // ── Group 9: Arc data integrity in payload ────────────────────────────────

    #[test]
    fn all_severed_arcs_appear_in_pv2_body() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        let arcs = vec![
            calls_arc("A", "B"),
            calls_arc("C", "D"),
            calls_arc("E", "F"),
        ];
        p.push_delta(&severed_delta(arcs)).expect("ok");
        let v = pv2_body_value(&t);
        let arr = v["newly_severed"].as_array().expect("array");
        assert_eq!(arr.len(), 3, "all 3 severed arcs must appear in PV2 body");
    }

    #[test]
    fn all_healed_arcs_appear_in_pv2_body() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        let arcs = vec![calls_arc("X", "Y"), calls_arc("U", "V")];
        p.push_delta(&healed_delta(arcs)).expect("ok");
        let v = pv2_body_value(&t);
        let arr = v["newly_healed"].as_array().expect("array");
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn mixed_delta_both_lists_in_pv2_body() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&mixed_delta()).expect("ok");
        let v = pv2_body_value(&t);
        assert_eq!(v["newly_severed"].as_array().expect("arr").len(), 1);
        assert_eq!(v["newly_healed"].as_array().expect("arr").len(), 1);
    }

    #[test]
    fn unicode_producer_consumer_preserved_in_pv2_body() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        let delta = severed_delta(vec![graph_arc("αβγ", "café", "calls")]);
        p.push_delta(&delta).expect("ok");
        let v = pv2_body_value(&t);
        assert_eq!(v["newly_severed"][0]["producer"], "αβγ");
        assert_eq!(v["newly_severed"][0]["consumer"], "café");
    }

    #[test]
    fn relation_other_than_calls_is_preserved() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        let delta = severed_delta(vec![graph_arc("A", "B", "method")]);
        p.push_delta(&delta).expect("ok");
        let v = pv2_body_value(&t);
        assert_eq!(v["newly_severed"][0]["relation"], "method");
    }

    #[test]
    fn ten_severed_arcs_all_in_pv2_body() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        let arcs: Vec<GraphArc> = (0..10)
            .map(|i| calls_arc(&format!("P{i}"), &format!("C{i}")))
            .collect();
        p.push_delta(&severed_delta(arcs)).expect("ok");
        let v = pv2_body_value(&t);
        assert_eq!(v["newly_severed"].as_array().expect("arr").len(), 10);
    }

    #[test]
    fn positive_coherence_delta_is_preserved_in_pv2_body() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        let delta = ArcDelta {
            newly_severed: vec![],
            newly_healed: vec![calls_arc("A", "B")],
            coherence_delta: 0.333,
        };
        p.push_delta(&delta).expect("ok");
        let v = pv2_body_value(&t);
        let cd = v["coherence_delta"].as_f64().expect("f64");
        assert!((cd - 0.333).abs() < 1e-9);
    }

    #[test]
    fn zero_coherence_delta_with_arcs_is_preserved() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        let delta = ArcDelta {
            newly_severed: vec![calls_arc("A", "B")],
            newly_healed: vec![calls_arc("X", "Y")],
            coherence_delta: 0.0,
        };
        p.push_delta(&delta).expect("ok");
        let v = pv2_body_value(&t);
        assert!((v["coherence_delta"].as_f64().expect("f64")).abs() < f64::EPSILON);
    }

    // ── Group 10: POVM inner value integrity ──────────────────────────────────

    #[test]
    fn povm_value_string_contains_severed_arc_producer() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        let delta = severed_delta(vec![calls_arc("SpecialProducer", "B")]);
        p.push_delta(&delta).expect("ok");
        let v = povm_body_value(&t);
        let inner_str = v["value"].as_str().expect("string");
        assert!(
            inner_str.contains("SpecialProducer"),
            "producer must appear in POVM value string"
        );
    }

    #[test]
    fn povm_value_string_contains_healed_arc_consumer() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        let delta = healed_delta(vec![calls_arc("A", "HealedConsumer")]);
        p.push_delta(&delta).expect("ok");
        let v = povm_body_value(&t);
        let inner_str = v["value"].as_str().expect("string");
        assert!(inner_str.contains("HealedConsumer"));
    }

    #[test]
    fn povm_value_parses_as_object_with_three_keys() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&mixed_delta()).expect("ok");
        let v = povm_body_value(&t);
        let inner: serde_json::Value =
            serde_json::from_str(v["value"].as_str().expect("str")).expect("json");
        assert!(inner.is_object());
        // Must have newly_severed, newly_healed, coherence_delta
        assert!(inner.get("newly_severed").is_some());
        assert!(inner.get("newly_healed").is_some());
        assert!(inner.get("coherence_delta").is_some());
    }

    // ── Group 11: Ordering guarantee — PV2 before POVM ───────────────────────

    #[test]
    fn pv2_is_always_called_before_povm() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&mixed_delta()).expect("ok");
        let calls = t.calls_snapshot();
        assert!(
            calls[0].0.contains("/arc-delta"),
            "first call must be to PV2 /arc-delta, got: {}",
            calls[0].0
        );
        assert!(
            calls[1].0.contains("/memory/store"),
            "second call must be to POVM /memory/store, got: {}",
            calls[1].0
        );
    }

    #[test]
    fn pv2_before_povm_even_with_only_healed_arcs() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&healed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let calls = t.calls_snapshot();
        assert!(calls[0].0.contains("/arc-delta"));
        assert!(calls[1].0.contains("/memory/store"));
    }

    // ── Group 12: Return value is always Ok on HTTP failure ──────────────────

    #[test]
    fn failing_transport_push_returns_ok() {
        let t = ConstTransport::failing("network down");
        let p = pusher_const(StdArc::clone(&t));
        let result = p.push_delta(&mixed_delta());
        assert!(
            result.is_ok(),
            "transport failure must not surface as Err: {result:?}"
        );
    }

    #[test]
    fn first_fail_second_ok_both_called_returns_ok() {
        let t = SeqTransport::with_responses(vec![Err("pv2 fail".into()), Ok("{}".into())]);
        let p = pusher_seq(StdArc::clone(&t));
        assert!(p.push_delta(&mixed_delta()).is_ok());
    }

    #[test]
    fn first_ok_second_fail_returns_ok() {
        let t = SeqTransport::with_responses(vec![Ok("{}".into()), Err("povm fail".into())]);
        let p = pusher_seq(StdArc::clone(&t));
        assert!(p.push_delta(&mixed_delta()).is_ok());
    }

    // ── Group 13: Send+Sync ───────────────────────────────────────────────────

    #[test]
    fn const_transport_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StdArc<ConstTransport>>();
    }

    #[test]
    fn seq_transport_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StdArc<SeqTransport>>();
    }

    // ── Group 14: Healed arcs only (regression guard for the none-severed path) ─

    #[test]
    fn only_healed_arcs_makes_two_calls() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&healed_delta(vec![
            calls_arc("A", "B"),
            calls_arc("C", "D"),
        ]))
        .expect("ok");
        assert_eq!(t.call_count(), 2);
    }

    #[test]
    fn pv2_body_newly_severed_empty_when_only_healed() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&healed_delta(vec![calls_arc("A", "B")]))
            .expect("ok");
        let v = pv2_body_value(&t);
        assert!(
            v["newly_severed"].as_array().expect("arr").is_empty(),
            "newly_severed must be empty when only healed arcs are present"
        );
    }

    #[test]
    fn pv2_body_newly_healed_populated_when_only_healed() {
        let t = ConstTransport::ok();
        let p = pusher_const(StdArc::clone(&t));
        p.push_delta(&healed_delta(vec![
            calls_arc("A", "B"),
            calls_arc("C", "D"),
        ]))
        .expect("ok");
        let v = pv2_body_value(&t);
        assert_eq!(v["newly_healed"].as_array().expect("arr").len(), 2);
    }
}
