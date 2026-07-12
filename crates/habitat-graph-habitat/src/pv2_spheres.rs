//! PV2 spheres — Leiden community → Kuramoto sphere mapping, naming-trap guarded.
//!
//! # Naming-Trap Guard
//!
//! Community labels are mutable: two communities in the same Leiden run can share a label, and
//! labels can be renamed between runs. The PV2 sphere id MUST therefore be derived from the
//! **stable numeric [`CommunityId`]**, NOT from `Community::label`. This module enforces that
//! invariant: [`sphere_id_for_community`] consults `Community::id` only and ignores `label`.
//!
//! # PV2 Integration
//!
//! [`SphereRegistrar`] is the boundary trait that decouples the community→sphere mapping from
//! its transport. [`RecordingRegistrar`] is the thread-safe in-memory double for tests (no
//! network, no I/O). The concrete live HTTP adapter ([`HttpRegistrar`]) lives behind
//! `#[cfg(feature = "live-bridges")]` and POSTs JSON to the PV2 Kuramoto sphere endpoint at
//! `http://localhost:8132`.
//!
//! [`CommunityId`]: habitat_graph_core::CommunityId

use habitat_graph_core::sanitize_label;
use habitat_graph_core::{Community, GraphError, Result};
use std::sync::Mutex;

// ─── SphereId ────────────────────────────────────────────────────────────────

/// A stable identifier for a PV2 Kuramoto sphere derived from its Leiden community.
///
/// The inner [`String`] always has the form `"habitat-graph.community.<u32>"`, where
/// `<u32>` is the decimal form of the community's [`CommunityId`]. Access the raw string
/// via `.0`.
///
/// # Naming-Trap
///
/// This value MUST be derived from [`Community::id`] (numeric, stable), NOT from
/// [`Community::label`] (mutable, collision-prone). See [`sphere_id_for_community`].
///
/// [`CommunityId`]: habitat_graph_core::CommunityId
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SphereId(pub String);

/// Derives the stable PV2 sphere identifier for a Leiden community.
///
/// Returns a [`SphereId`] whose inner string is `"habitat-graph.community.<u32>"`, where
/// `<u32>` is the decimal value of `c.id`. The `label` field of the community is **never
/// consulted** — this is the naming-trap guard.
///
/// Two communities that share the same label but have different ids produce different sphere
/// ids. A community whose label is renamed or contains special/bidi characters still produces
/// the same sphere id as before.
#[must_use]
pub fn sphere_id_for_community(c: &Community) -> SphereId {
    SphereId(format!("habitat-graph.community.{}", c.id.get()))
}

// ─── SphereRegistration ──────────────────────────────────────────────────────

/// Registration payload for a Leiden community → PV2 Kuramoto sphere mapping.
///
/// Built by [`community_to_registration`]. The `label` field has been run through
/// [`sanitize_label`] to strip control characters. Apply
/// [`habitat_graph_core::display_safe`] on `label` before any terminal or UI render
/// boundary (Trojan-Source / bidi defence).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SphereRegistration {
    /// The stable sphere id, as returned by [`sphere_id_for_community`].
    pub sphere_id: String,
    /// Number of member nodes in the community (`Community::members.len()`).
    pub member_count: usize,
    /// Human-readable community label, sanitized (control characters stripped via
    /// [`sanitize_label`]). Bidi override codepoints pass through `sanitize_label` — apply
    /// [`habitat_graph_core::display_safe`] at the render boundary.
    pub label: String,
}

/// Converts a [`Community`] into a [`SphereRegistration`] payload.
///
/// - `sphere_id`: stable id from [`sphere_id_for_community`] (naming-trap guarded — derived
///   from the numeric id, not the label).
/// - `member_count`: `community.members.len()`.
/// - `label`: `community.label` run through [`sanitize_label`] (control chars stripped).
#[must_use]
pub fn community_to_registration(c: &Community) -> SphereRegistration {
    SphereRegistration {
        sphere_id: sphere_id_for_community(c).0,
        member_count: c.members.len(),
        label: sanitize_label(&c.label),
    }
}

// ─── SphereRegistrar ─────────────────────────────────────────────────────────

/// Registers a PV2 Kuramoto sphere derived from a Leiden community.
///
/// Implementations in this module:
/// - [`RecordingRegistrar`]: thread-safe in-memory double (all builds).
/// - [`HttpRegistrar`]: live HTTP adapter (`feature = "live"` only).
pub trait SphereRegistrar: Send + Sync {
    /// Registers a single sphere from the supplied payload.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Daemon`] if the underlying transport fails (network error,
    /// non-success HTTP status, or poisoned mutex in test doubles).
    fn register(&self, reg: &SphereRegistration) -> Result<()>;
}

// ─── RecordingRegistrar ──────────────────────────────────────────────────────

/// A thread-safe in-memory [`SphereRegistrar`] that records every registration call.
///
/// Inject this where a real registrar would go, then assert on the captured data via
/// [`RecordingRegistrar::registrations`] and [`RecordingRegistrar::len`].
///
/// Internally wraps a [`Mutex`]-guarded [`Vec`], so `RecordingRegistrar` is `Send + Sync`
/// and safe to share across threads.
#[derive(Debug, Default)]
pub struct RecordingRegistrar {
    inner: Mutex<Vec<SphereRegistration>>,
}

impl RecordingRegistrar {
    /// Creates a new, empty [`RecordingRegistrar`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of all [`SphereRegistration`]s recorded so far, in call order.
    ///
    /// Returns an empty [`Vec`] if the internal [`Mutex`] has been poisoned (defensive
    /// fallback; cannot occur in single-threaded usage).
    #[must_use]
    pub fn registrations(&self) -> Vec<SphereRegistration> {
        match self.inner.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => Vec::new(),
        }
    }

    /// Returns the number of registrations recorded so far.
    ///
    /// Returns `0` if the internal [`Mutex`] has been poisoned.
    #[must_use]
    pub fn len(&self) -> usize {
        match self.inner.lock() {
            Ok(guard) => guard.len(),
            Err(_) => 0,
        }
    }

    /// Returns `true` if no registrations have been recorded yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl SphereRegistrar for RecordingRegistrar {
    fn register(&self, reg: &SphereRegistration) -> Result<()> {
        self.inner
            .lock()
            .map_err(|e| GraphError::Daemon(format!("RecordingRegistrar mutex poisoned: {e}")))?
            .push(reg.clone());
        Ok(())
    }
}

// ─── register_all ────────────────────────────────────────────────────────────

/// Registers every community in `communities` as a PV2 sphere and returns the total count.
///
/// For each community, calls [`community_to_registration`] then
/// [`SphereRegistrar::register`]. Stops and propagates the **first** error encountered;
/// communities after the failing one are not registered.
///
/// On complete success the return value equals `communities.len()`.
///
/// # Errors
///
/// Propagates the first [`GraphError`] returned by [`SphereRegistrar::register`].
pub fn register_all<R: SphereRegistrar>(r: &R, communities: &[Community]) -> Result<usize> {
    for c in communities {
        r.register(&community_to_registration(c))?;
    }
    Ok(communities.len())
}

// ─── Live adapter (feature = "live") ─────────────────────────────────────────

/// Default base URL for the PV2 Kuramoto sphere HTTP API.
#[cfg(feature = "live-bridges")]
const PV2_BASE_URL: &str = "http://localhost:8132";

/// HTTP registrar that POSTs sphere registration payloads to the PV2 Kuramoto endpoint.
///
/// Only available with the `live` crate feature (requires `ureq`). Sends a JSON body of
/// the form:
///
/// ```json
/// { "sphere_id": "...", "member_count": 42, "label": "..." }
/// ```
///
/// to `{base_url}/api/spheres/register`.
///
/// Use [`HttpRegistrar::new`] for the default PV2 instance (`http://localhost:8132`) or
/// [`HttpRegistrar::with_base_url`] for staging and integration-test environments.
#[cfg(feature = "live-bridges")]
pub struct HttpRegistrar {
    base_url: String,
}

#[cfg(feature = "live-bridges")]
impl HttpRegistrar {
    /// Creates an [`HttpRegistrar`] targeting the default PV2 endpoint (`http://localhost:8132`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            base_url: PV2_BASE_URL.to_owned(),
        }
    }

    /// Creates an [`HttpRegistrar`] targeting a custom base URL.
    ///
    /// Useful for staging environments or integration tests that run a local PV2 instance
    /// on a non-standard port.
    #[must_use]
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    fn endpoint_url(&self) -> String {
        format!("{}/api/spheres/register", self.base_url)
    }
}

#[cfg(feature = "live-bridges")]
impl Default for HttpRegistrar {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "live-bridges")]
impl SphereRegistrar for HttpRegistrar {
    fn register(&self, reg: &SphereRegistration) -> Result<()> {
        let body = serde_json::json!({
            "sphere_id": reg.sphere_id,
            "member_count": reg.member_count,
            "label": reg.label,
        })
        .to_string();
        let url = self.endpoint_url();
        ureq::post(&url)
            .set("Content-Type", "application/json")
            .send_string(&body)
            .map(|_| ())
            .map_err(|e| {
                GraphError::Daemon(format!("PV2 sphere registration to {url} failed: {e}"))
            })
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        community_to_registration, register_all, sphere_id_for_community, RecordingRegistrar,
        SphereId, SphereRegistrar, SphereRegistration,
    };
    use habitat_graph_core::{Community, CommunityId, GraphError, NodeId, Result};
    use std::collections::HashSet;
    use std::sync::Mutex;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_community(id: u32, label: &str, member_ids: &[u32]) -> Community {
        Community {
            id: CommunityId::new(id),
            label: label.to_owned(),
            members: member_ids.iter().copied().map(NodeId::new).collect(),
        }
    }

    /// A registrar that always fails — used to test error propagation.
    struct AlwaysFailingRegistrar;
    impl SphereRegistrar for AlwaysFailingRegistrar {
        fn register(&self, _: &SphereRegistration) -> Result<()> {
            Err(GraphError::Daemon("injected failure".into()))
        }
    }

    /// A registrar that fails once `fail_after` successful calls have been made.
    struct FailAfterRegistrar {
        fail_after: usize,
        count: Mutex<usize>,
    }
    impl FailAfterRegistrar {
        fn new(fail_after: usize) -> Self {
            Self {
                fail_after,
                count: Mutex::new(0),
            }
        }
        fn calls(&self) -> usize {
            *self.count.lock().expect("not poisoned")
        }
    }
    impl SphereRegistrar for FailAfterRegistrar {
        fn register(&self, _: &SphereRegistration) -> Result<()> {
            let mut c = self.count.lock().expect("not poisoned");
            if *c >= self.fail_after {
                return Err(GraphError::Daemon("fail_after limit reached".into()));
            }
            *c += 1;
            Ok(())
        }
    }

    // ── Group 1: sphere_id_for_community ─────────────────────────────────────

    #[test]
    fn sphere_id_format_is_prefix_plus_numeric_id() {
        let c = make_community(7, "any-label", &[]);
        assert_eq!(sphere_id_for_community(&c).0, "habitat-graph.community.7");
    }

    #[test]
    fn sphere_id_id_zero_formats_correctly() {
        let c = make_community(0, "label", &[]);
        assert_eq!(sphere_id_for_community(&c).0, "habitat-graph.community.0");
    }

    #[test]
    fn sphere_id_id_one_formats_correctly() {
        let c = make_community(1, "label", &[]);
        assert_eq!(sphere_id_for_community(&c).0, "habitat-graph.community.1");
    }

    #[test]
    fn sphere_id_id_max_u32_formats_correctly() {
        let c = make_community(u32::MAX, "label", &[]);
        assert_eq!(
            sphere_id_for_community(&c).0,
            format!("habitat-graph.community.{}", u32::MAX)
        );
    }

    #[test]
    fn sphere_id_is_deterministic_for_same_community() {
        let c = make_community(42, "anything", &[1, 2, 3]);
        assert_eq!(sphere_id_for_community(&c), sphere_id_for_community(&c));
    }

    /// THE NAMING-TRAP PROOF: same label, different ids → different sphere ids.
    #[test]
    fn naming_trap_same_label_different_ids_yield_different_sphere_ids() {
        let label = "shared-label";
        let c1 = make_community(10, label, &[]);
        let c2 = make_community(11, label, &[]);
        // Same label, but different ids → sphere ids MUST differ.
        assert_ne!(
            sphere_id_for_community(&c1),
            sphere_id_for_community(&c2),
            "naming-trap: identical labels must not collapse to the same sphere id"
        );
    }

    /// Stronger naming-trap proof with three communities.
    #[test]
    fn naming_trap_three_communities_same_label_all_sphere_ids_distinct() {
        let label = "collision-bait";
        let ids: Vec<SphereId> = (0u32..3)
            .map(|i| sphere_id_for_community(&make_community(i, label, &[])))
            .collect();
        let unique: HashSet<&str> = ids.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(unique.len(), 3, "all three sphere ids must be distinct");
    }

    #[test]
    fn bidi_char_in_label_does_not_change_sphere_id() {
        // U+202E = RIGHT-TO-LEFT OVERRIDE (Trojan-Source canonical payload).
        let c_plain = make_community(5, "plain", &[]);
        let c_bidi = make_community(5, "plain\u{202E}", &[]);
        // Same id → same sphere id, regardless of label content.
        assert_eq!(
            sphere_id_for_community(&c_plain),
            sphere_id_for_community(&c_bidi)
        );
    }

    #[test]
    fn rtl_override_in_label_does_not_change_sphere_id() {
        // Additional Trojan-Source defense: full bidi isolation markers.
        let c_clean = make_community(99, "module", &[]);
        let c_trojan = make_community(99, "\u{2066}module\u{2069}/* admin */", &[]);
        assert_eq!(
            sphere_id_for_community(&c_clean),
            sphere_id_for_community(&c_trojan)
        );
    }

    #[test]
    fn empty_label_does_not_change_sphere_id() {
        let c_empty = make_community(3, "", &[]);
        let c_full = make_community(3, "non-empty-label", &[]);
        // Empty vs non-empty label: sphere id depends only on id.
        assert_eq!(
            sphere_id_for_community(&c_empty),
            sphere_id_for_community(&c_full)
        );
    }

    #[test]
    fn sphere_id_prefix_is_exact() {
        let c = make_community(100, "x", &[]);
        assert!(
            sphere_id_for_community(&c)
                .0
                .starts_with("habitat-graph.community."),
            "sphere id must start with the exact prefix"
        );
    }

    #[test]
    fn sphere_id_suffix_is_decimal_community_id() {
        let c = make_community(2048, "x", &[]);
        let sid = sphere_id_for_community(&c).0;
        let suffix = sid
            .strip_prefix("habitat-graph.community.")
            .expect("prefix present");
        assert_eq!(suffix, "2048");
    }

    // ── Group 2: community_to_registration ───────────────────────────────────

    #[test]
    fn registration_sphere_id_matches_sphere_id_for_community() {
        let c = make_community(7, "cluster-A", &[1, 2]);
        let reg = community_to_registration(&c);
        assert_eq!(reg.sphere_id, sphere_id_for_community(&c).0);
    }

    #[test]
    fn registration_member_count_matches_members_len() {
        let c = make_community(1, "c", &[10, 20, 30]);
        assert_eq!(community_to_registration(&c).member_count, 3);
    }

    #[test]
    fn registration_member_count_zero_for_no_members() {
        let c = make_community(1, "empty", &[]);
        assert_eq!(community_to_registration(&c).member_count, 0);
    }

    #[test]
    fn registration_member_count_large() {
        let members: Vec<u32> = (0..1_000).collect();
        let c = make_community(2, "big", &members);
        assert_eq!(community_to_registration(&c).member_count, 1_000);
    }

    #[test]
    fn registration_label_is_sanitized_from_control_chars() {
        let c = make_community(3, "ab\x07cd\n", &[]);
        let reg = community_to_registration(&c);
        // sanitize_label strips control chars (\x07 BEL, \n newline).
        assert_eq!(reg.label, "abcd");
    }

    #[test]
    fn registration_label_tab_stripped() {
        let c = make_community(4, "col1\tcol2", &[]);
        assert_eq!(community_to_registration(&c).label, "col1col2");
    }

    #[test]
    fn registration_label_bidi_chars_pass_through_sanitize_label() {
        // U+202E (RIGHT-TO-LEFT OVERRIDE) is Cf, NOT Cc — sanitize_label does not strip it.
        // It must be handled at render time with display_safe.
        let bidi = "\u{202E}";
        let c = make_community(5, bidi, &[]);
        assert!(
            community_to_registration(&c).label.contains('\u{202E}'),
            "sanitize_label must not strip bidi overrides (handled at render boundary)"
        );
    }

    #[test]
    fn registration_label_long_gets_capped_at_256_chars() {
        let long_label = "x".repeat(1_000);
        let c = make_community(6, &long_label, &[]);
        let reg = community_to_registration(&c);
        assert_eq!(reg.label.chars().count(), 256);
    }

    #[test]
    fn registration_all_three_fields_are_populated() {
        let c = make_community(8, "populated", &[1, 2]);
        let reg = community_to_registration(&c);
        assert!(!reg.sphere_id.is_empty(), "sphere_id must be non-empty");
        assert_eq!(reg.member_count, 2);
        assert_eq!(reg.label, "populated");
    }

    #[test]
    fn registration_label_plain_ascii_preserved() {
        let c = make_community(9, "plain ASCII label", &[]);
        assert_eq!(community_to_registration(&c).label, "plain ASCII label");
    }

    // ── Group 3: RecordingRegistrar ───────────────────────────────────────────

    #[test]
    fn recording_registrar_new_starts_empty() {
        let r = RecordingRegistrar::new();
        assert!(r.registrations().is_empty());
    }

    #[test]
    fn recording_registrar_is_empty_true_before_any_call() {
        let r = RecordingRegistrar::new();
        assert!(r.is_empty());
    }

    #[test]
    fn recording_registrar_register_returns_ok() {
        let r = RecordingRegistrar::new();
        let c = make_community(1, "c", &[]);
        assert!(r.register(&community_to_registration(&c)).is_ok());
    }

    #[test]
    fn recording_registrar_records_single_registration() {
        let r = RecordingRegistrar::new();
        let c = make_community(1, "alpha", &[10, 20]);
        r.register(&community_to_registration(&c)).expect("ok");
        let recs = r.registrations();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].sphere_id, sphere_id_for_community(&c).0);
        assert_eq!(recs[0].member_count, 2);
    }

    #[test]
    fn recording_registrar_preserves_insertion_order() {
        let r = RecordingRegistrar::new();
        let c1 = make_community(1, "first", &[1]);
        let c2 = make_community(2, "second", &[2, 3]);
        let c3 = make_community(3, "third", &[]);
        r.register(&community_to_registration(&c1)).expect("ok");
        r.register(&community_to_registration(&c2)).expect("ok");
        r.register(&community_to_registration(&c3)).expect("ok");
        let recs = r.registrations();
        assert_eq!(recs[0].sphere_id, sphere_id_for_community(&c1).0);
        assert_eq!(recs[1].sphere_id, sphere_id_for_community(&c2).0);
        assert_eq!(recs[2].sphere_id, sphere_id_for_community(&c3).0);
    }

    #[test]
    fn recording_registrar_len_tracks_call_count() {
        let r = RecordingRegistrar::new();
        assert_eq!(r.len(), 0);
        let c = make_community(1, "x", &[]);
        let reg = community_to_registration(&c);
        r.register(&reg).expect("ok");
        assert_eq!(r.len(), 1);
        r.register(&reg).expect("ok");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn recording_registrar_is_not_empty_after_register() {
        let r = RecordingRegistrar::new();
        r.register(&community_to_registration(&make_community(1, "x", &[])))
            .expect("ok");
        assert!(!r.is_empty());
    }

    #[test]
    fn recording_registrar_registrations_returns_owned_snapshot() {
        let r = RecordingRegistrar::new();
        let c = make_community(5, "snap", &[1]);
        r.register(&community_to_registration(&c)).expect("ok");
        // snapshot is owned — does not borrow r
        let snapshot = r.registrations();
        drop(r);
        assert_eq!(snapshot.len(), 1);
    }

    #[test]
    fn recording_registrar_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RecordingRegistrar>();
    }

    // ── Group 4: register_all ─────────────────────────────────────────────────

    #[test]
    fn register_all_empty_slice_returns_zero() {
        let r = RecordingRegistrar::new();
        let count = register_all(&r, &[]).expect("ok");
        assert_eq!(count, 0);
    }

    #[test]
    fn register_all_empty_slice_leaves_registrar_empty() {
        let r = RecordingRegistrar::new();
        register_all(&r, &[]).expect("ok");
        assert!(r.is_empty());
    }

    #[test]
    fn register_all_single_community_returns_one() {
        let r = RecordingRegistrar::new();
        let communities = [make_community(1, "solo", &[10])];
        assert_eq!(register_all(&r, &communities).expect("ok"), 1);
    }

    #[test]
    fn register_all_two_communities_returns_two() {
        let r = RecordingRegistrar::new();
        let communities = [make_community(1, "a", &[]), make_community(2, "b", &[1, 2])];
        assert_eq!(register_all(&r, &communities).expect("ok"), 2);
    }

    #[test]
    fn register_all_count_equals_communities_len() {
        let r = RecordingRegistrar::new();
        let communities: Vec<Community> = (0u32..10).map(|i| make_community(i, "c", &[])).collect();
        let count = register_all(&r, &communities).expect("ok");
        assert_eq!(count, communities.len());
    }

    #[test]
    fn register_all_all_registrations_are_recorded() {
        let r = RecordingRegistrar::new();
        let communities: Vec<Community> = (0u32..5).map(|i| make_community(i, "x", &[i])).collect();
        register_all(&r, &communities).expect("ok");
        assert_eq!(r.len(), 5);
    }

    #[test]
    fn register_all_propagates_registrar_error() {
        let r = AlwaysFailingRegistrar;
        let communities = [make_community(1, "x", &[])];
        let err = register_all(&r, &communities).expect_err("must fail");
        assert_eq!(err.kind(), "daemon");
    }

    #[test]
    fn register_all_stops_at_first_error_not_after() {
        // fail_after=2: first 2 calls succeed, 3rd fails.
        let r = FailAfterRegistrar::new(2);
        let communities: Vec<Community> = (0u32..5).map(|i| make_community(i, "c", &[])).collect();
        let result = register_all(&r, &communities);
        assert!(result.is_err(), "must propagate error");
        // Exactly 2 calls succeeded before the 3rd failed.
        assert_eq!(r.calls(), 2);
    }

    #[test]
    fn register_all_error_on_first_community_yields_zero_successful_calls() {
        let r = FailAfterRegistrar::new(0); // fails immediately
        let communities = [make_community(1, "x", &[])];
        let result = register_all(&r, &communities);
        assert!(result.is_err());
        assert_eq!(r.calls(), 0, "no successful calls before first failure");
    }

    #[test]
    fn register_all_registrations_ordered_by_community_slice_order() {
        let r = RecordingRegistrar::new();
        let communities = [
            make_community(10, "tenth", &[]),
            make_community(1, "first", &[]),
            make_community(5, "fifth", &[]),
        ];
        register_all(&r, &communities).expect("ok");
        let recs = r.registrations();
        assert_eq!(
            recs[0].sphere_id,
            sphere_id_for_community(&communities[0]).0
        );
        assert_eq!(
            recs[1].sphere_id,
            sphere_id_for_community(&communities[1]).0
        );
        assert_eq!(
            recs[2].sphere_id,
            sphere_id_for_community(&communities[2]).0
        );
    }

    #[test]
    fn register_all_large_batch() {
        let r = RecordingRegistrar::new();
        let communities: Vec<Community> = (0u32..500)
            .map(|i| make_community(i, "bulk", &[]))
            .collect();
        let count = register_all(&r, &communities).expect("ok");
        assert_eq!(count, 500);
        assert_eq!(r.len(), 500);
    }

    // ── Group 5: SphereId ────────────────────────────────────────────────────

    #[test]
    fn sphere_id_inner_accessible_via_dot_zero() {
        let sid = SphereId("habitat-graph.community.42".to_owned());
        assert_eq!(sid.0, "habitat-graph.community.42");
    }

    #[test]
    fn sphere_id_clone_equals_original() {
        let sid = sphere_id_for_community(&make_community(3, "c", &[]));
        assert_eq!(sid.clone(), sid);
    }

    #[test]
    fn sphere_id_equality_on_matching_strings() {
        let a = SphereId("habitat-graph.community.1".to_owned());
        let b = SphereId("habitat-graph.community.1".to_owned());
        assert_eq!(a, b);
    }

    #[test]
    fn sphere_id_inequality_for_different_community_ids() {
        let c1 = make_community(1, "same-label", &[]);
        let c2 = make_community(2, "same-label", &[]);
        assert_ne!(sphere_id_for_community(&c1), sphere_id_for_community(&c2));
    }

    #[test]
    fn sphere_id_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SphereId>();
    }

    #[test]
    fn sphere_id_debug_output_contains_inner_string() {
        let sid = sphere_id_for_community(&make_community(9, "c", &[]));
        let debug = format!("{sid:?}");
        assert!(debug.contains("habitat-graph.community.9"));
    }

    #[test]
    fn sphere_id_two_calls_for_same_community_are_equal() {
        let c = make_community(77, "repeated", &[1, 2, 3]);
        let a = sphere_id_for_community(&c);
        let b = sphere_id_for_community(&c);
        assert_eq!(a, b);
    }

    #[test]
    fn sphere_id_usable_as_hash_set_key() {
        let mut set: HashSet<SphereId> = HashSet::new();
        let c = make_community(55, "hashed", &[]);
        let sid = sphere_id_for_community(&c);
        set.insert(sid.clone());
        set.insert(sid.clone()); // duplicate
        assert_eq!(set.len(), 1, "duplicate SphereId must collapse in HashSet");
    }

    #[test]
    fn sphere_id_for_zero_and_one_are_different() {
        let s0 = sphere_id_for_community(&make_community(0, "x", &[]));
        let s1 = sphere_id_for_community(&make_community(1, "x", &[]));
        assert_ne!(s0, s1);
    }

    // ── Trait object safety ───────────────────────────────────────────────────

    #[test]
    fn sphere_registrar_is_object_safe() {
        // This test verifies the trait can be used as `dyn SphereRegistrar` at runtime.
        fn takes_dyn(r: &dyn SphereRegistrar, reg: &SphereRegistration) -> Result<()> {
            r.register(reg)
        }
        let r = RecordingRegistrar::new();
        let c = make_community(1, "dyn-test", &[]);
        takes_dyn(&r, &community_to_registration(&c)).expect("dyn dispatch works");
        assert_eq!(r.len(), 1);
    }

    // ── SphereRegistration field correctness ─────────────────────────────────

    #[test]
    fn registration_sphere_id_encodes_numeric_id_not_label() {
        let c = make_community(999, "irrelevant-label", &[]);
        let reg = community_to_registration(&c);
        assert!(reg.sphere_id.ends_with(".999"));
        assert!(!reg.sphere_id.contains("irrelevant-label"));
    }

    #[test]
    fn registration_for_max_id_contains_max_u32_decimal() {
        let c = make_community(u32::MAX, "x", &[]);
        let reg = community_to_registration(&c);
        assert!(reg.sphere_id.ends_with(&u32::MAX.to_string()));
    }

    // ── HttpRegistrar — live adapter (feature = "live") ───────────────────────
    // Tests verify construction and pure config surface (endpoint URL building)
    // without making any network connections.

    /// `HttpRegistrar::new()` must target the canonical PV2 spheres endpoint.
    #[cfg(feature = "live-bridges")]
    #[test]
    fn http_registrar_new_has_default_endpoint() {
        let r = super::HttpRegistrar::new();
        assert_eq!(
            r.endpoint_url(),
            "http://localhost:8132/api/spheres/register"
        );
    }

    /// `HttpRegistrar::with_base_url` must override the base URL used in `endpoint_url()`.
    #[cfg(feature = "live-bridges")]
    #[test]
    fn http_registrar_with_base_url_builds_custom_endpoint() {
        let r = super::HttpRegistrar::with_base_url("http://staging:9132");
        assert_eq!(r.endpoint_url(), "http://staging:9132/api/spheres/register");
    }

    /// `default()` and `new()` must produce the same endpoint.
    #[cfg(feature = "live-bridges")]
    #[test]
    fn http_registrar_default_eq_new_endpoint() {
        let via_new = super::HttpRegistrar::new();
        let via_default = super::HttpRegistrar::default();
        assert_eq!(
            via_new.endpoint_url(),
            via_default.endpoint_url(),
            "new() and default() must produce the same endpoint"
        );
    }

    /// `HttpRegistrar` must be `Send + Sync` for concurrent sphere registration.
    #[cfg(feature = "live-bridges")]
    #[test]
    fn http_registrar_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<super::HttpRegistrar>();
    }

    /// The endpoint URL always ends with `/api/spheres/register`.
    #[cfg(feature = "live-bridges")]
    #[test]
    fn http_registrar_endpoint_url_always_ends_with_register_path() {
        let r = super::HttpRegistrar::with_base_url("http://any-host:1234");
        let url = r.endpoint_url();
        assert!(
            url.ends_with("/api/spheres/register"),
            "endpoint must always end with register path; got: {url}"
        );
    }
}
