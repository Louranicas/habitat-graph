//! Service-health bridge — `cc-health`-style probing of factory services.
//!
//! This module implements the [`ServiceProbe`] boundary trait plus the in-memory test double
//! [`StaticProbe`] so the whole crate can be built and gated without touching any live service.
//! The concrete [`HttpProbe`] (plain HTTP GET + 200 check) lives behind
//! `#[cfg(feature = "live-bridges")]` and requires the `ureq` crate.
//!
//! ## Path-map special case — Maintenance Engine
//!
//! Not every ULTRAPLATE service exposes `/health` at root. [`default_factory_endpoints`] encodes
//! the same path-map that `cc-health` uses. In particular the **Maintenance Engine** (`me`)
//! listens at `http://localhost:8180/api/health` — **not** `/health` — matching the `cc-health`
//! path-map override. Using the bare `/health` path returns 404 and is the most common source of
//! false-DOWN readings in hand-rolled curl health checks.

use habitat_graph_core::display_safe;
use std::collections::HashMap;

/// A named service endpoint that can be health-probed.
///
/// The `id` is a short, stable tag (e.g. `"wfe"`, `"me"`) used to correlate a
/// [`HealthReport`] with the service it describes. The `url` is the full URL to GET.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEndpoint {
    /// Short, stable identifier for this service (e.g. `"wfe"`, `"pv2"`, `"me"`).
    pub id: String,
    /// Full URL to GET for a health check (e.g. `"http://localhost:8142/health"`).
    pub url: String,
}

/// The result of probing a single [`ServiceEndpoint`].
///
/// [`HealthReport::id`] always matches the [`ServiceEndpoint::id`] that was probed, not any
/// internal label the probe implementation might carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    /// The [`ServiceEndpoint::id`] that was probed.
    pub id: String,
    /// `true` when the service is reachable and returned HTTP 200 (or a preset healthy
    /// response in [`StaticProbe`]).
    pub healthy: bool,
    /// The HTTP status code returned, if a response was received at all.
    ///
    /// `None` means no response was received — e.g. connection refused, timeout, or
    /// (in [`StaticProbe`]) an unknown URL with no configured preset.
    pub status_code: Option<u16>,
    /// Human-readable detail: `"ok"` on success, a body excerpt or error description
    /// otherwise. Always passed through [`display_safe`] before storage so Trojan-Source
    /// and bidi-override codepoints cannot escape to a terminal.
    pub detail: String,
}

/// Boundary trait for probing a [`ServiceEndpoint`] and returning a [`HealthReport`].
///
/// Both the in-memory double ([`StaticProbe`]) and the live adapter ([`HttpProbe`], behind the
/// `live` feature) implement this trait, so [`probe_all`] and [`count_healthy`] are fully
/// generic and testable without I/O.
///
/// # Contract
///
/// Implementations **must not** panic. Any transport failure (connection refused, timeout, DNS
/// error, body read failure) must surface as an unhealthy [`HealthReport`] with a descriptive
/// `detail`, never as a panicking thread.
pub trait ServiceProbe: Send + Sync {
    /// Probe `endpoint` and return a [`HealthReport`] describing its current health.
    fn probe(&self, endpoint: &ServiceEndpoint) -> HealthReport;
}

/// In-memory test double that returns pre-configured [`HealthReport`]s keyed by URL.
///
/// Constructed via [`StaticProbe::new`] with an iterator of `(url, HealthReport)` pairs.
///
/// When [`ServiceProbe::probe`] is called:
/// - **Known URL** — the preset for that URL is returned, with [`HealthReport::id`]
///   overwritten by [`ServiceEndpoint::id`] so the report is tied to the endpoint probed.
/// - **Unknown URL** — an unhealthy report is returned with `status_code: None` and a
///   `detail` that names the missing URL (passed through [`display_safe`]).
///
/// If the same URL appears more than once in the constructor iterator the last entry wins.
#[derive(Debug, Default)]
pub struct StaticProbe {
    presets: HashMap<String, HealthReport>,
}

impl StaticProbe {
    /// Constructs a `StaticProbe` from an iterator of `(url, HealthReport)` pairs.
    ///
    /// The URL string in each pair is used as the lookup key when [`ServiceProbe::probe`] is
    /// called. If the same URL appears more than once the **last** entry wins (iterator order).
    #[must_use]
    pub fn new<I, S>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (S, HealthReport)>,
        S: Into<String>,
    {
        let presets = pairs
            .into_iter()
            .map(|(url, report)| (url.into(), report))
            .collect();
        Self { presets }
    }
}

impl ServiceProbe for StaticProbe {
    /// Returns the preset report for `endpoint.url`, or an unhealthy report for unknown URLs.
    ///
    /// [`HealthReport::id`] is always taken from `endpoint.id`, not from any `id` stored in
    /// the preset, so the returned report is unambiguously tied to the probed endpoint.
    fn probe(&self, endpoint: &ServiceEndpoint) -> HealthReport {
        match self.presets.get(&endpoint.url) {
            Some(preset) => HealthReport {
                id: endpoint.id.clone(),
                healthy: preset.healthy,
                status_code: preset.status_code,
                detail: preset.detail.clone(),
            },
            None => HealthReport {
                id: endpoint.id.clone(),
                healthy: false,
                status_code: None,
                detail: display_safe(&format!(
                    "no preset configured for URL: {}",
                    endpoint.url
                )),
            },
        }
    }
}

/// Probes every endpoint in `endpoints` using `probe` and returns results in the same order.
///
/// The returned vector has the same length and element order as `endpoints`. Element `i` in
/// the result corresponds to `probe.probe(&endpoints[i])`. Use [`count_healthy`] to summarise.
#[must_use]
pub fn probe_all<P: ServiceProbe>(probe: &P, endpoints: &[ServiceEndpoint]) -> Vec<HealthReport> {
    endpoints.iter().map(|ep| probe.probe(ep)).collect()
}

/// Counts the number of [`HealthReport`]s where `healthy == true`.
///
/// Returns `0` for an empty slice.
#[must_use]
pub fn count_healthy(reports: &[HealthReport]) -> usize {
    reports.iter().filter(|r| r.healthy).count()
}

/// Returns the canonical ULTRAPLATE factory service endpoints, matching the `cc-health` path-map.
///
/// | ID | URL |
/// |---|---|
/// | `wfe` | `http://localhost:8142/health` |
/// | `lcm` | `http://localhost:8200/health` |
/// | `pv2` | `http://localhost:8132/health` |
/// | `povm` | `http://localhost:8125/health` |
/// | `tierwright` | `http://localhost:8201/health` |
/// | `me` | `http://localhost:8180/api/health` ← **path-map special case** |
///
/// The Maintenance Engine (`me`) uses `/api/health` rather than `/health`. This matches
/// the documented `cc-health` path-map override and must not be changed to a bare `/health`
/// path, which would return 404 and show the service as DOWN even when it is healthy.
#[must_use]
pub fn default_factory_endpoints() -> Vec<ServiceEndpoint> {
    vec![
        ServiceEndpoint {
            id: "wfe".to_owned(),
            url: "http://localhost:8142/health".to_owned(),
        },
        ServiceEndpoint {
            id: "lcm".to_owned(),
            url: "http://localhost:8200/health".to_owned(),
        },
        ServiceEndpoint {
            id: "pv2".to_owned(),
            url: "http://localhost:8132/health".to_owned(),
        },
        ServiceEndpoint {
            id: "povm".to_owned(),
            url: "http://localhost:8125/health".to_owned(),
        },
        ServiceEndpoint {
            id: "tierwright".to_owned(),
            url: "http://localhost:8201/health".to_owned(),
        },
        ServiceEndpoint {
            id: "me".to_owned(),
            url: "http://localhost:8180/api/health".to_owned(),
        },
    ]
}

/// A live HTTP probe: GETs each endpoint URL and maps HTTP 200 → healthy.
///
/// Requires the `live` feature (which activates `ureq`). Never panics: transport errors and
/// non-200 responses both produce unhealthy [`HealthReport`]s with descriptive `detail` text,
/// always sanitised through [`display_safe`] before storage.
#[cfg(feature = "live-bridges")]
#[derive(Debug, Clone)]
pub struct HttpProbe {
    timeout_secs: u64,
}

#[cfg(feature = "live-bridges")]
impl Default for HttpProbe {
    fn default() -> Self {
        Self { timeout_secs: 5 }
    }
}

#[cfg(feature = "live-bridges")]
impl HttpProbe {
    /// Creates an `HttpProbe` with the default 5-second per-request timeout.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an `HttpProbe` with a custom per-request timeout in seconds.
    #[must_use]
    pub fn with_timeout_secs(secs: u64) -> Self {
        Self { timeout_secs: secs }
    }
}

#[cfg(feature = "live-bridges")]
impl ServiceProbe for HttpProbe {
    /// GETs `endpoint.url`; HTTP 200 → healthy, any other status or transport error → unhealthy.
    ///
    /// - **HTTP 200** → `healthy: true`, `status_code: Some(200)`, `detail: "ok"`.
    /// - **Non-200 HTTP response** → `healthy: false`, `status_code: Some(code)`,
    ///   `detail` contains the status code and a body excerpt (display-safe escaped).
    /// - **Transport error** (connection refused, timeout, DNS) → `healthy: false`,
    ///   `status_code: None`, `detail` contains the error message (display-safe escaped).
    fn probe(&self, endpoint: &ServiceEndpoint) -> HealthReport {
        use std::time::Duration;

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(self.timeout_secs))
            .build();

        match agent.get(&endpoint.url).call() {
            Ok(resp) => {
                let status = resp.status();
                let healthy = status == 200;
                let detail = if healthy {
                    "ok".to_owned()
                } else {
                    display_safe(&format!("unexpected status {status}"))
                };
                HealthReport {
                    id: endpoint.id.clone(),
                    healthy,
                    status_code: Some(status),
                    detail,
                }
            }
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                let trimmed = body.trim();
                let body_excerpt = if trimmed.is_empty() {
                    "(empty body)"
                } else {
                    trimmed
                };
                let detail = display_safe(&format!("HTTP {code}: {body_excerpt}"));
                HealthReport {
                    id: endpoint.id.clone(),
                    healthy: false,
                    status_code: Some(code),
                    detail,
                }
            }
            Err(err) => HealthReport {
                id: endpoint.id.clone(),
                healthy: false,
                status_code: None,
                detail: display_safe(&format!("transport error: {err}")),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HealthReport, ServiceEndpoint, ServiceProbe, StaticProbe, count_healthy,
        default_factory_endpoints, probe_all,
    };

    // ── helpers ─────────────────────────────────────────────────────────────────

    fn healthy_report(id: &str) -> HealthReport {
        HealthReport {
            id: id.to_owned(),
            healthy: true,
            status_code: Some(200),
            detail: "ok".to_owned(),
        }
    }

    fn unhealthy_report(id: &str) -> HealthReport {
        HealthReport {
            id: id.to_owned(),
            healthy: false,
            status_code: Some(503),
            detail: "service unavailable".to_owned(),
        }
    }

    fn ep(id: &str, url: &str) -> ServiceEndpoint {
        ServiceEndpoint {
            id: id.to_owned(),
            url: url.to_owned(),
        }
    }

    fn static_probe_with_one(url: &str, report: HealthReport) -> StaticProbe {
        StaticProbe::new([(url, report)])
    }

    // ── ServiceEndpoint ──────────────────────────────────────────────────────────

    #[test]
    fn service_endpoint_id_and_url_fields() {
        let e = ep("wfe", "http://localhost:8142/health");
        assert_eq!(e.id, "wfe");
        assert_eq!(e.url, "http://localhost:8142/health");
    }

    #[test]
    fn service_endpoint_clone_is_independent() {
        let original = ep("lcm", "http://localhost:8200/health");
        let mut cloned = original.clone();
        cloned.id = "mutated".to_owned();
        assert_eq!(original.id, "lcm");
    }

    #[test]
    fn service_endpoint_equality() {
        let a = ep("pv2", "http://localhost:8132/health");
        let b = ep("pv2", "http://localhost:8132/health");
        assert_eq!(a, b);
    }

    #[test]
    fn service_endpoint_inequality_on_id() {
        let a = ep("pv2", "http://localhost:8132/health");
        let b = ep("povm", "http://localhost:8132/health");
        assert_ne!(a, b);
    }

    #[test]
    fn service_endpoint_inequality_on_url() {
        let a = ep("svc", "http://localhost:8132/health");
        let b = ep("svc", "http://localhost:9999/health");
        assert_ne!(a, b);
    }

    // ── HealthReport ─────────────────────────────────────────────────────────────

    #[test]
    fn health_report_healthy_fields() {
        let r = healthy_report("wfe");
        assert_eq!(r.id, "wfe");
        assert!(r.healthy);
        assert_eq!(r.status_code, Some(200));
        assert_eq!(r.detail, "ok");
    }

    #[test]
    fn health_report_unhealthy_fields() {
        let r = unhealthy_report("me");
        assert_eq!(r.id, "me");
        assert!(!r.healthy);
        assert_eq!(r.status_code, Some(503));
        assert!(r.detail.contains("unavailable"));
    }

    #[test]
    fn health_report_no_status_code() {
        let r = HealthReport {
            id: "x".to_owned(),
            healthy: false,
            status_code: None,
            detail: "timeout".to_owned(),
        };
        assert!(r.status_code.is_none());
    }

    #[test]
    fn health_report_clone_is_independent() {
        let original = healthy_report("pv2");
        let mut cloned = original.clone();
        cloned.id = "mutated".to_owned();
        assert_eq!(original.id, "pv2");
    }

    #[test]
    fn health_report_equality() {
        let a = healthy_report("wfe");
        let b = healthy_report("wfe");
        assert_eq!(a, b);
    }

    #[test]
    fn health_report_inequality_on_healthy_flag() {
        let a = healthy_report("svc");
        let b = unhealthy_report("svc");
        assert_ne!(a, b);
    }

    // ── StaticProbe ───────────────────────────────────────────────────────────────

    #[test]
    fn static_probe_default_is_empty() {
        let probe = StaticProbe::default();
        let ep = ep("any", "http://localhost:1/health");
        let r = probe.probe(&ep);
        assert!(!r.healthy, "default probe has no presets; must be unhealthy");
    }

    #[test]
    fn static_probe_known_url_returns_healthy() {
        let url = "http://localhost:8142/health";
        let probe = static_probe_with_one(url, healthy_report("preset"));
        let r = probe.probe(&ep("wfe", url));
        assert!(r.healthy);
    }

    #[test]
    fn static_probe_known_url_returns_unhealthy() {
        let url = "http://localhost:8142/health";
        let probe = static_probe_with_one(url, unhealthy_report("preset"));
        let r = probe.probe(&ep("wfe", url));
        assert!(!r.healthy);
    }

    #[test]
    fn static_probe_id_taken_from_endpoint_not_preset() {
        let url = "http://localhost:8142/health";
        // Preset has id "preset_id"; endpoint has id "endpoint_id".
        let preset = healthy_report("preset_id");
        let probe = static_probe_with_one(url, preset);
        let r = probe.probe(&ep("endpoint_id", url));
        assert_eq!(r.id, "endpoint_id", "id must come from the endpoint, not the preset");
    }

    #[test]
    fn static_probe_status_code_from_preset() {
        let url = "http://localhost:8200/health";
        let mut preset = healthy_report("lcm");
        preset.status_code = Some(204);
        let probe = static_probe_with_one(url, preset);
        let r = probe.probe(&ep("lcm", url));
        assert_eq!(r.status_code, Some(204));
    }

    #[test]
    fn static_probe_detail_from_preset() {
        let url = "http://localhost:8200/health";
        let mut preset = healthy_report("lcm");
        preset.detail = "running fine".to_owned();
        let probe = static_probe_with_one(url, preset);
        let r = probe.probe(&ep("lcm", url));
        assert_eq!(r.detail, "running fine");
    }

    #[test]
    fn static_probe_unknown_url_is_unhealthy() {
        let probe = StaticProbe::default();
        let r = probe.probe(&ep("mystery", "http://localhost:9999/health"));
        assert!(!r.healthy);
    }

    #[test]
    fn static_probe_unknown_url_has_no_status_code() {
        let probe = StaticProbe::default();
        let r = probe.probe(&ep("mystery", "http://localhost:9999/health"));
        assert!(r.status_code.is_none());
    }

    #[test]
    fn static_probe_unknown_url_detail_contains_url() {
        let probe = StaticProbe::default();
        let target_url = "http://localhost:9999/health";
        let r = probe.probe(&ep("mystery", target_url));
        assert!(
            r.detail.contains(target_url),
            "detail should mention the unknown URL; got: {:?}",
            r.detail
        );
    }

    #[test]
    fn static_probe_multiple_presets_route_correctly() {
        let url_a = "http://localhost:8142/health";
        let url_b = "http://localhost:8200/health";
        let probe = StaticProbe::new([
            (url_a, healthy_report("a")),
            (url_b, unhealthy_report("b")),
        ]);
        let ra = probe.probe(&ep("wfe", url_a));
        let rb = probe.probe(&ep("lcm", url_b));
        assert!(ra.healthy, "url_a preset is healthy");
        assert!(!rb.healthy, "url_b preset is unhealthy");
    }

    #[test]
    fn static_probe_duplicate_url_last_entry_wins() {
        let url = "http://localhost:8142/health";
        let probe = StaticProbe::new([
            (url, healthy_report("first")),
            (url, unhealthy_report("second")),
        ]);
        // The second entry for the same URL should win.
        let r = probe.probe(&ep("wfe", url));
        assert!(!r.healthy, "last preset for duplicated URL must win");
    }

    #[test]
    fn static_probe_unknown_url_id_still_from_endpoint() {
        let probe = StaticProbe::default();
        let r = probe.probe(&ep("povm", "http://localhost:9999/nope"));
        assert_eq!(r.id, "povm");
    }

    #[test]
    fn static_probe_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StaticProbe>();
    }

    #[test]
    fn static_probe_detail_is_display_safe() {
        // A URL containing a bidi-override codepoint must not leak raw into the detail field.
        let url = "http://localhost:9999/\u{202E}evil";
        let probe = StaticProbe::default();
        let r = probe.probe(&ep("evil", url));
        assert!(
            !r.detail.contains('\u{202E}'),
            "RLO bidi override must not appear raw in detail"
        );
    }

    // ── probe_all ────────────────────────────────────────────────────────────────

    #[test]
    fn probe_all_empty_slice_returns_empty_vec() {
        let probe = StaticProbe::default();
        let results = probe_all(&probe, &[]);
        assert!(results.is_empty());
    }

    #[test]
    fn probe_all_single_healthy_endpoint() {
        let url = "http://localhost:8142/health";
        let probe = static_probe_with_one(url, healthy_report("p"));
        let endpoints = [ep("wfe", url)];
        let results = probe_all(&probe, &endpoints);
        assert_eq!(results.len(), 1);
        assert!(results[0].healthy);
    }

    #[test]
    fn probe_all_single_unhealthy_endpoint() {
        let url = "http://localhost:8142/health";
        let probe = static_probe_with_one(url, unhealthy_report("p"));
        let endpoints = [ep("wfe", url)];
        let results = probe_all(&probe, &endpoints);
        assert_eq!(results.len(), 1);
        assert!(!results[0].healthy);
    }

    #[test]
    fn probe_all_result_length_matches_input_length() {
        let probe = StaticProbe::default();
        let endpoints: Vec<ServiceEndpoint> = (0..5)
            .map(|i| ep(&format!("s{i}"), &format!("http://localhost:{}/health", 9000 + i)))
            .collect();
        let results = probe_all(&probe, &endpoints);
        assert_eq!(results.len(), endpoints.len());
    }

    #[test]
    fn probe_all_order_preserved_for_mixed_results() {
        let url_a = "http://localhost:8142/health";
        let url_b = "http://localhost:8200/health";
        let url_c = "http://localhost:8132/health";
        let probe = StaticProbe::new([
            (url_a, healthy_report("a")),
            (url_b, unhealthy_report("b")),
            (url_c, healthy_report("c")),
        ]);
        let endpoints = [ep("wfe", url_a), ep("lcm", url_b), ep("pv2", url_c)];
        let results = probe_all(&probe, &endpoints);
        assert_eq!(results.len(), 3);
        assert!(results[0].healthy, "first must be healthy");
        assert!(!results[1].healthy, "second must be unhealthy");
        assert!(results[2].healthy, "third must be healthy");
    }

    #[test]
    fn probe_all_ids_match_endpoints_in_order() {
        let url_a = "http://localhost:8142/health";
        let url_b = "http://localhost:8200/health";
        let probe = StaticProbe::new([
            (url_a, healthy_report("p")),
            (url_b, healthy_report("q")),
        ]);
        let endpoints = [ep("wfe", url_a), ep("lcm", url_b)];
        let results = probe_all(&probe, &endpoints);
        assert_eq!(results[0].id, "wfe");
        assert_eq!(results[1].id, "lcm");
    }

    #[test]
    fn probe_all_all_unknown_urls_yields_all_unhealthy() {
        let probe = StaticProbe::default();
        let endpoints: Vec<ServiceEndpoint> = (0..3)
            .map(|i| ep(&format!("s{i}"), &format!("http://localhost:{}/health", 9100 + i)))
            .collect();
        let results = probe_all(&probe, &endpoints);
        assert!(
            results.iter().all(|r| !r.healthy),
            "all unknown → all unhealthy"
        );
    }

    #[test]
    fn probe_all_all_known_urls_yields_all_healthy() {
        let urls = [
            "http://localhost:8142/health",
            "http://localhost:8200/health",
            "http://localhost:8132/health",
        ];
        let pairs: Vec<(&str, HealthReport)> = urls
            .iter()
            .map(|&u| (u, healthy_report("p")))
            .collect();
        let probe = StaticProbe::new(pairs);
        let endpoints: Vec<ServiceEndpoint> = urls
            .iter()
            .enumerate()
            .map(|(i, &u)| ep(&format!("s{i}"), u))
            .collect();
        let results = probe_all(&probe, &endpoints);
        assert!(results.iter().all(|r| r.healthy), "all known → all healthy");
    }

    // ── count_healthy ─────────────────────────────────────────────────────────────

    #[test]
    fn count_healthy_empty_slice_is_zero() {
        assert_eq!(count_healthy(&[]), 0);
    }

    #[test]
    fn count_healthy_single_healthy() {
        assert_eq!(count_healthy(&[healthy_report("x")]), 1);
    }

    #[test]
    fn count_healthy_single_unhealthy() {
        assert_eq!(count_healthy(&[unhealthy_report("x")]), 0);
    }

    #[test]
    fn count_healthy_all_healthy() {
        let reports = vec![healthy_report("a"), healthy_report("b"), healthy_report("c")];
        assert_eq!(count_healthy(&reports), 3);
    }

    #[test]
    fn count_healthy_none_healthy() {
        let reports = vec![
            unhealthy_report("a"),
            unhealthy_report("b"),
            unhealthy_report("c"),
        ];
        assert_eq!(count_healthy(&reports), 0);
    }

    #[test]
    fn count_healthy_one_of_two_healthy() {
        let reports = vec![healthy_report("a"), unhealthy_report("b")];
        assert_eq!(count_healthy(&reports), 1);
    }

    #[test]
    fn count_healthy_two_of_four_healthy() {
        let reports = vec![
            healthy_report("a"),
            unhealthy_report("b"),
            healthy_report("c"),
            unhealthy_report("d"),
        ];
        assert_eq!(count_healthy(&reports), 2);
    }

    #[test]
    fn count_healthy_matches_probe_all_pipeline() {
        let url = "http://localhost:8142/health";
        let probe = static_probe_with_one(url, healthy_report("p"));
        let endpoints = [ep("wfe", url)];
        let reports = probe_all(&probe, &endpoints);
        assert_eq!(count_healthy(&reports), 1);
    }

    // ── default_factory_endpoints ──────────────────────────────────────────────────

    #[test]
    fn default_factory_endpoints_has_six_entries() {
        assert_eq!(default_factory_endpoints().len(), 6);
    }

    #[test]
    fn default_factory_endpoints_ids_are_unique() {
        let eps = default_factory_endpoints();
        let mut ids: Vec<&str> = eps.iter().map(|e| e.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 6, "all six IDs must be distinct");
    }

    #[test]
    fn default_factory_endpoints_urls_are_unique() {
        let eps = default_factory_endpoints();
        let mut urls: Vec<&str> = eps.iter().map(|e| e.url.as_str()).collect();
        urls.sort_unstable();
        urls.dedup();
        assert_eq!(urls.len(), 6, "all six URLs must be distinct");
    }

    #[test]
    fn default_factory_endpoints_contains_wfe() {
        let eps = default_factory_endpoints();
        let wfe = eps.iter().find(|e| e.id == "wfe").expect("wfe entry must exist");
        assert_eq!(wfe.url, "http://localhost:8142/health");
    }

    #[test]
    fn default_factory_endpoints_contains_lcm() {
        let eps = default_factory_endpoints();
        let lcm = eps.iter().find(|e| e.id == "lcm").expect("lcm entry must exist");
        assert_eq!(lcm.url, "http://localhost:8200/health");
    }

    #[test]
    fn default_factory_endpoints_contains_pv2() {
        let eps = default_factory_endpoints();
        let pv2 = eps.iter().find(|e| e.id == "pv2").expect("pv2 entry must exist");
        assert_eq!(pv2.url, "http://localhost:8132/health");
    }

    #[test]
    fn default_factory_endpoints_contains_povm() {
        let eps = default_factory_endpoints();
        let povm = eps.iter().find(|e| e.id == "povm").expect("povm entry must exist");
        assert_eq!(povm.url, "http://localhost:8125/health");
    }

    #[test]
    fn default_factory_endpoints_contains_tierwright() {
        let eps = default_factory_endpoints();
        let tw = eps
            .iter()
            .find(|e| e.id == "tierwright")
            .expect("tierwright entry must exist");
        assert_eq!(tw.url, "http://localhost:8201/health");
    }

    #[test]
    fn default_factory_endpoints_contains_me() {
        let eps = default_factory_endpoints();
        let me = eps.iter().find(|e| e.id == "me").expect("me (ME) entry must exist");
        // Path-map special case: ME uses /api/health, NOT /health.
        assert_eq!(me.url, "http://localhost:8180/api/health");
    }

    #[test]
    fn default_factory_endpoints_me_uses_api_health_not_bare_health() {
        let eps = default_factory_endpoints();
        let me = eps.iter().find(|e| e.id == "me").expect("me must exist");
        assert!(
            me.url.contains("/api/health"),
            "ME must use /api/health; bare /health returns 404"
        );
        assert!(
            !me.url.ends_with("/health") || me.url.contains("/api/health"),
            "ME URL must not end with bare /health"
        );
    }

    #[test]
    fn default_factory_endpoints_me_bare_health_path_is_wrong() {
        // Regression guard: the bare /health path must NEVER appear for ME.
        let eps = default_factory_endpoints();
        let me = eps.iter().find(|e| e.id == "me").expect("me must exist");
        assert_ne!(
            me.url, "http://localhost:8180/health",
            "ME must NOT use bare /health — that returns 404"
        );
    }

    #[test]
    fn default_factory_endpoints_all_use_localhost() {
        let eps = default_factory_endpoints();
        for ep in &eps {
            assert!(
                ep.url.starts_with("http://localhost:"),
                "endpoint {} URL must start with http://localhost:; got: {}",
                ep.id,
                ep.url
            );
        }
    }

    #[test]
    fn probe_all_with_all_default_endpoints_unhealthy_when_no_presets() {
        let probe = StaticProbe::default();
        let eps = default_factory_endpoints();
        let results = probe_all(&probe, &eps);
        assert_eq!(results.len(), 6);
        assert_eq!(
            count_healthy(&results),
            0,
            "no presets → all six factory services must be unhealthy"
        );
    }

    #[test]
    fn probe_all_with_all_default_endpoints_healthy_when_all_presets_set() {
        let eps = default_factory_endpoints();
        let pairs: Vec<(String, HealthReport)> = eps
            .iter()
            .map(|e| (e.url.clone(), healthy_report(&e.id)))
            .collect();
        let probe = StaticProbe::new(pairs);
        let results = probe_all(&probe, &eps);
        assert_eq!(count_healthy(&results), 6);
    }

    // ── HttpProbe — live adapter (feature = "live") ───────────────────────────
    // These tests verify construction and pure config surface without making
    // any network connections.

    /// `HttpProbe::new()` must construct without panicking.
    #[cfg(feature = "live-bridges")]
    #[test]
    fn http_probe_new_constructs_ok() {
        let _ = super::HttpProbe::new();
    }

    /// `HttpProbe::default()` and `HttpProbe::new()` must produce the same configuration.
    #[cfg(feature = "live-bridges")]
    #[test]
    fn http_probe_default_same_debug_as_new() {
        let via_new = super::HttpProbe::new();
        let via_default = super::HttpProbe::default();
        // Debug output encodes timeout_secs; equal output proves equal config.
        assert_eq!(
            format!("{via_new:?}"),
            format!("{via_default:?}"),
            "new() and default() must produce identical configuration"
        );
    }

    /// The default timeout is 5 seconds — visible in the Debug representation.
    #[cfg(feature = "live-bridges")]
    #[test]
    fn http_probe_default_timeout_is_five_seconds() {
        let p = super::HttpProbe::new();
        let debug = format!("{p:?}");
        assert!(
            debug.contains('5'),
            "default timeout (5 s) must appear in Debug output: {debug}"
        );
    }

    /// A custom timeout must be reflected in the Debug representation.
    #[cfg(feature = "live-bridges")]
    #[test]
    fn http_probe_with_timeout_secs_reflects_in_debug() {
        let p = super::HttpProbe::with_timeout_secs(30);
        let debug = format!("{p:?}");
        assert!(
            debug.contains("30"),
            "custom timeout 30 must appear in Debug output: {debug}"
        );
    }

    /// `HttpProbe` must be `Send + Sync` — it crosses thread boundaries in multi-probe scenarios.
    #[cfg(feature = "live-bridges")]
    #[test]
    fn http_probe_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<super::HttpProbe>();
    }

    /// `HttpProbe` must be `Clone` so callers can share a template probe.
    #[cfg(feature = "live-bridges")]
    #[test]
    fn http_probe_is_clone() {
        let original = super::HttpProbe::new();
        let cloned = original.clone();
        assert_eq!(
            format!("{original:?}"),
            format!("{cloned:?}"),
            "clone must produce equal configuration"
        );
    }
}
