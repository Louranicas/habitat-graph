//! Arc-telemetry — the *continuous* delta layer over the [`arc_graph`](crate::arc_graph)
//! snapshot (`FO-7`).
//!
//! [`arc_graph`](crate::arc_graph) produces a point-in-time [`SeveredEarReport`].  This module
//! diffs two reports (before/after a rebuild) into an [`ArcDelta`] that names the arcs that
//! **newly severed** (a regression to push to the gauge / `injection.db`) and **newly healed**
//! (a recovery), plus the raw coherence change.  The live delta-push to `PV2`/`POVM` is the
//! `live`-gated actuation (separate); this computation is pure + deterministic (`R4`).

use std::collections::HashSet;

use crate::arc_graph::{Arc, SeveredEarReport};

// ─── ArcDelta ────────────────────────────────────────────────────────────────

/// The change in severed-ear state between two [`SeveredEarReport`]s.
///
/// Both `newly_severed` and `newly_healed` are sorted for byte-stable, deterministic output (`R4`).
/// A rebuild that severs an arc surfaces it in `newly_severed` — the signal the orchestrator pushes
/// to the arc-coherence gauge and `injection.db`.
#[derive(Clone, Debug, PartialEq)]
pub struct ArcDelta {
    /// Arcs present in `before` but severed in `after` — regressions.
    pub newly_severed: Vec<Arc>,
    /// Arcs severed in `before` but present in `after` — recoveries.
    pub newly_healed: Vec<Arc>,
    /// `after.coherence − before.coherence` (negative = the rebuild worsened coherence).
    pub coherence_delta: f64,
}

impl ArcDelta {
    /// Returns `true` when any arc was newly severed in this delta (a regression signal).
    ///
    /// Note that `is_regression` and [`is_recovery`](ArcDelta::is_recovery) can both be `true`
    /// when some arcs sever while others heal simultaneously.
    #[must_use]
    pub fn is_regression(&self) -> bool {
        !self.newly_severed.is_empty()
    }

    /// Returns `true` when any arc was newly healed in this delta (a recovery signal).
    ///
    /// Note that `is_recovery` and [`is_regression`](ArcDelta::is_regression) can both be `true`
    /// when healing and severing happen in the same rebuild.
    #[must_use]
    pub fn is_recovery(&self) -> bool {
        !self.newly_healed.is_empty()
    }
}

// ─── diff_reports ────────────────────────────────────────────────────────────

/// Diffs two [`SeveredEarReport`]s into an [`ArcDelta`] (continuous telemetry, `FO-7`).
///
/// - `newly_severed` = arcs in `after.severed` that are **not** in `before.severed`.
/// - `newly_healed`  = arcs in `before.severed` that are **not** in `after.severed`.
/// - `coherence_delta` = `after.coherence − before.coherence`.
///
/// Both output vectors are sorted for deterministic, byte-stable comparison (`R4`).
/// The function is pure: no `I/O`, no mutations beyond the returned [`ArcDelta`].
#[must_use]
pub fn diff_reports(before: &SeveredEarReport, after: &SeveredEarReport) -> ArcDelta {
    let before_severed: HashSet<&Arc> = before.severed.iter().collect();
    let after_severed: HashSet<&Arc> = after.severed.iter().collect();

    let mut newly_severed: Vec<Arc> = after
        .severed
        .iter()
        .filter(|a| !before_severed.contains(a))
        .cloned()
        .collect();
    let mut newly_healed: Vec<Arc> = before
        .severed
        .iter()
        .filter(|a| !after_severed.contains(a))
        .cloned()
        .collect();

    newly_severed.sort_unstable();
    newly_healed.sort_unstable();

    ArcDelta {
        newly_severed,
        newly_healed,
        coherence_delta: after.coherence - before.coherence,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::arc_graph::{Arc, SeveredEarReport};

    use super::{diff_reports, ArcDelta};

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Build an [`Arc`] with the default relation `"calls"`.
    fn arc(p: &str, c: &str) -> Arc {
        Arc {
            producer: p.to_owned(),
            consumer: c.to_owned(),
            relation: "calls".to_owned(),
        }
    }

    /// Build an [`Arc`] with an explicit relation.
    fn arc_r(p: &str, c: &str, r: &str) -> Arc {
        Arc {
            producer: p.to_owned(),
            consumer: c.to_owned(),
            relation: r.to_owned(),
        }
    }

    /// Build a [`SeveredEarReport`] from explicit present/severed slices.
    /// Coherence is computed automatically from the slice lengths.
    fn report(present: &[Arc], severed: &[Arc]) -> SeveredEarReport {
        let total = present.len() + severed.len();
        #[allow(clippy::cast_precision_loss)]
        let coherence = if total == 0 {
            1.0
        } else {
            present.len() as f64 / total as f64
        };
        SeveredEarReport {
            present: present.to_vec(),
            severed: severed.to_vec(),
            coherence,
        }
    }

    /// Assert two `f64` values agree to within a generous floating-point tolerance.
    fn assert_close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-12, "expected {b:.15}, got {a:.15}");
    }

    // ── ArcDelta struct ───────────────────────────────────────────────────────

    /// Constructing an [`ArcDelta`] directly and reading each field works correctly.
    #[test]
    fn arc_delta_fields_accessible() {
        let delta = ArcDelta {
            newly_severed: vec![arc("a", "b")],
            newly_healed: vec![arc("c", "d")],
            coherence_delta: -0.5,
        };
        assert_eq!(delta.newly_severed.len(), 1);
        assert_eq!(delta.newly_healed.len(), 1);
        assert_close(delta.coherence_delta, -0.5);
    }

    /// `Clone` produces a value equal to the original.
    #[test]
    fn arc_delta_clone_equals_original() {
        let delta = ArcDelta {
            newly_severed: vec![arc("a", "b")],
            newly_healed: vec![],
            coherence_delta: -0.25,
        };
        assert_eq!(delta.clone(), delta);
    }

    /// `Debug` output is non-empty and contains the type name.
    #[test]
    fn arc_delta_debug_nonempty() {
        let delta = ArcDelta {
            newly_severed: vec![],
            newly_healed: vec![arc("x", "y")],
            coherence_delta: 0.1,
        };
        let s = format!("{delta:?}");
        assert!(!s.is_empty());
        assert!(s.contains("ArcDelta"));
    }

    /// `PartialEq` returns true for structurally identical values.
    #[test]
    fn arc_delta_partial_eq_same() {
        let d1 = ArcDelta {
            newly_severed: vec![arc("a", "b")],
            newly_healed: vec![],
            coherence_delta: -0.5,
        };
        let d2 = d1.clone();
        assert_eq!(d1, d2);
    }

    /// `PartialEq` returns false when `newly_severed` differs.
    #[test]
    fn arc_delta_partial_eq_different_newly_severed() {
        let d1 = ArcDelta {
            newly_severed: vec![arc("a", "b")],
            newly_healed: vec![],
            coherence_delta: 0.0,
        };
        let d2 = ArcDelta {
            newly_severed: vec![arc("x", "y")],
            newly_healed: vec![],
            coherence_delta: 0.0,
        };
        assert_ne!(d1, d2);
    }

    // ── is_regression ─────────────────────────────────────────────────────────

    /// `is_regression` is `false` when `newly_severed` is empty (even with healed arcs).
    #[test]
    fn is_regression_false_when_newly_severed_empty() {
        let delta = ArcDelta {
            newly_severed: vec![],
            newly_healed: vec![arc("a", "b")],
            coherence_delta: 0.5,
        };
        assert!(!delta.is_regression());
    }

    /// `is_regression` is `true` when exactly one arc is newly severed.
    #[test]
    fn is_regression_true_when_one_newly_severed() {
        let delta = ArcDelta {
            newly_severed: vec![arc("a", "b")],
            newly_healed: vec![],
            coherence_delta: -0.5,
        };
        assert!(delta.is_regression());
    }

    /// `is_regression` remains `true` even when some arcs are simultaneously healed.
    #[test]
    fn is_regression_true_even_when_newly_healed_non_empty() {
        let delta = ArcDelta {
            newly_severed: vec![arc("a", "b")],
            newly_healed: vec![arc("c", "d")],
            coherence_delta: 0.0,
        };
        assert!(delta.is_regression());
    }

    /// `is_regression` is `false` for the empty-delta case.
    #[test]
    fn is_regression_false_for_empty_delta() {
        let delta = ArcDelta {
            newly_severed: vec![],
            newly_healed: vec![],
            coherence_delta: 0.0,
        };
        assert!(!delta.is_regression());
    }

    // ── is_recovery ───────────────────────────────────────────────────────────

    /// `is_recovery` is `false` when `newly_healed` is empty.
    #[test]
    fn is_recovery_false_when_newly_healed_empty() {
        let delta = ArcDelta {
            newly_severed: vec![arc("a", "b")],
            newly_healed: vec![],
            coherence_delta: -0.5,
        };
        assert!(!delta.is_recovery());
    }

    /// `is_recovery` is `true` when exactly one arc is newly healed.
    #[test]
    fn is_recovery_true_when_one_newly_healed() {
        let delta = ArcDelta {
            newly_severed: vec![],
            newly_healed: vec![arc("a", "b")],
            coherence_delta: 0.5,
        };
        assert!(delta.is_recovery());
    }

    /// `is_recovery` and `is_regression` can both be `true` simultaneously.
    #[test]
    fn is_recovery_and_is_regression_simultaneously() {
        let delta = ArcDelta {
            newly_severed: vec![arc("a", "b")],
            newly_healed: vec![arc("c", "d")],
            coherence_delta: 0.0,
        };
        assert!(delta.is_regression());
        assert!(delta.is_recovery());
    }

    // ── diff_reports: no change ───────────────────────────────────────────────

    /// Diffing the same report against itself yields an empty delta.
    #[test]
    fn no_change_is_empty_delta() {
        let r = report(&[arc("a", "b")], &[]);
        let d = diff_reports(&r, &r);
        assert!(d.newly_severed.is_empty() && d.newly_healed.is_empty());
        assert!(!d.is_regression());
    }

    /// An arc that was already severed before and remains severed after is NOT newly severed.
    #[test]
    fn no_change_pre_existing_severed_arc_not_newly_severed() {
        let r = report(&[], &[arc("a", "b")]);
        let d = diff_reports(&r, &r);
        assert!(d.newly_severed.is_empty());
        assert!(d.newly_healed.is_empty());
        assert_close(d.coherence_delta, 0.0);
    }

    /// `coherence_delta` is exactly zero when both reports are the same object.
    #[test]
    fn no_change_coherence_delta_is_zero() {
        let r = report(&[arc("a", "b"), arc("c", "d")], &[arc("x", "y")]);
        let d = diff_reports(&r, &r);
        assert_close(d.coherence_delta, 0.0);
    }

    // ── diff_reports: single newly-severed ────────────────────────────────────

    /// A single arc that moves from present to severed appears in `newly_severed`.
    #[test]
    fn newly_severed_detected() {
        let before = report(&[arc("a", "b")], &[]);
        let after = report(&[], &[arc("a", "b")]);
        let d = diff_reports(&before, &after);
        assert_eq!(d.newly_severed, vec![arc("a", "b")]);
        assert!(d.is_regression());
        assert!(d.coherence_delta < 0.0);
    }

    /// Full drop (1 present → 0 present) gives `coherence_delta` of exactly `−1.0`.
    #[test]
    fn single_newly_severed_coherence_delta_is_negative_one() {
        let before = report(&[arc("a", "b")], &[]);
        let after = report(&[], &[arc("a", "b")]);
        let d = diff_reports(&before, &after);
        assert_close(d.coherence_delta, -1.0);
    }

    /// `newly_healed` is empty when only severing occurred.
    #[test]
    fn single_newly_severed_healed_list_is_empty() {
        let before = report(&[arc("a", "b")], &[]);
        let after = report(&[], &[arc("a", "b")]);
        let d = diff_reports(&before, &after);
        assert!(d.newly_healed.is_empty());
    }

    // ── diff_reports: single newly-healed ─────────────────────────────────────

    /// A single arc that moves from severed to present appears in `newly_healed`.
    #[test]
    fn newly_healed_detected() {
        let before = report(&[], &[arc("a", "b")]);
        let after = report(&[arc("a", "b")], &[]);
        let d = diff_reports(&before, &after);
        assert_eq!(d.newly_healed, vec![arc("a", "b")]);
        assert!(!d.is_regression());
    }

    /// Full recovery (0 present → 1 present) gives `coherence_delta` of exactly `+1.0`.
    #[test]
    fn single_newly_healed_coherence_delta_is_positive_one() {
        let before = report(&[], &[arc("a", "b")]);
        let after = report(&[arc("a", "b")], &[]);
        let d = diff_reports(&before, &after);
        assert_close(d.coherence_delta, 1.0);
    }

    /// `newly_severed` is empty when only healing occurred.
    #[test]
    fn single_newly_healed_severed_list_is_empty() {
        let before = report(&[], &[arc("a", "b")]);
        let after = report(&[arc("a", "b")], &[]);
        let d = diff_reports(&before, &after);
        assert!(d.newly_severed.is_empty());
    }

    // ── diff_reports: mixed severing + healing ────────────────────────────────

    /// When one arc severs and another heals in the same rebuild, both lists are populated.
    #[test]
    fn mixed_one_severed_one_healed() {
        let a = arc("a", "b");
        let x = arc("x", "y");
        let before = report(std::slice::from_ref(&a), std::slice::from_ref(&x));
        let after = report(std::slice::from_ref(&x), std::slice::from_ref(&a));
        let d = diff_reports(&before, &after);
        assert_eq!(d.newly_severed, vec![a]);
        assert_eq!(d.newly_healed, vec![x]);
        assert!(d.is_regression());
        assert!(d.is_recovery());
    }

    /// More arcs severed than healed → `newly_severed` count exceeds `newly_healed`.
    #[test]
    fn mixed_more_severed_than_healed() {
        let a = arc("a", "b");
        let b = arc("b", "c");
        let x = arc("x", "y");
        let before = report(&[a.clone(), b.clone()], std::slice::from_ref(&x));
        let after = report(std::slice::from_ref(&x), &[a.clone(), b.clone()]);
        let d = diff_reports(&before, &after);
        assert_eq!(d.newly_severed.len(), 2);
        assert_eq!(d.newly_healed.len(), 1);
        assert!(d.is_regression());
    }

    /// More arcs healed than severed → `newly_healed` count exceeds `newly_severed`.
    #[test]
    fn mixed_more_healed_than_severed() {
        let a = arc("a", "b");
        let x = arc("x", "y");
        let z = arc("z", "w");
        let before = report(std::slice::from_ref(&a), &[x.clone(), z.clone()]);
        let after = report(&[x.clone(), z.clone()], std::slice::from_ref(&a));
        let d = diff_reports(&before, &after);
        assert_eq!(d.newly_severed.len(), 1);
        assert_eq!(d.newly_healed.len(), 2);
    }

    /// When the swap is symmetric and equal-sized, `coherence_delta` is zero.
    #[test]
    fn mixed_balanced_swap_coherence_delta_zero() {
        // Before: a present + x severed → coherence 0.5.
        // After:  x present + a severed → coherence 0.5.
        let a = arc("a", "b");
        let x = arc("x", "y");
        let before = report(std::slice::from_ref(&a), std::slice::from_ref(&x));
        let after = report(std::slice::from_ref(&x), std::slice::from_ref(&a));
        let d = diff_reports(&before, &after);
        assert_close(d.coherence_delta, 0.0);
    }

    // ── diff_reports: coherence_delta ─────────────────────────────────────────

    /// `coherence_delta` is zero when both reports carry identical coherence scores.
    #[test]
    fn coherence_delta_zero_on_identical_reports() {
        let r = report(&[arc("a", "b")], &[arc("x", "y")]);
        let d = diff_reports(&r, &r);
        assert_close(d.coherence_delta, 0.0);
    }

    /// `coherence_delta` is negative when the rebuild worsened coherence.
    #[test]
    fn coherence_delta_negative_on_regression() {
        let before = report(&[arc("a", "b")], &[]);
        let after = report(&[], &[arc("a", "b")]);
        let d = diff_reports(&before, &after);
        assert!(d.coherence_delta < 0.0);
    }

    /// `coherence_delta` is positive when the rebuild improved coherence.
    #[test]
    fn coherence_delta_positive_on_recovery() {
        let before = report(&[], &[arc("a", "b")]);
        let after = report(&[arc("a", "b")], &[]);
        let d = diff_reports(&before, &after);
        assert!(d.coherence_delta > 0.0);
    }

    /// `coherence_delta` equals exactly `after.coherence − before.coherence`.
    #[test]
    fn coherence_delta_equals_after_minus_before() {
        let before = SeveredEarReport {
            present: vec![],
            severed: vec![],
            coherence: 0.3,
        };
        let after = SeveredEarReport {
            present: vec![],
            severed: vec![],
            coherence: 0.8,
        };
        let d = diff_reports(&before, &after);
        assert_close(d.coherence_delta, 0.5);
    }

    /// Half-drop: 2 arcs, 1 severs → `coherence_delta` = `−0.5`.
    #[test]
    fn coherence_delta_exact_partial_drop() {
        let a = arc("a", "b");
        let x = arc("x", "y");
        let before = report(&[a.clone(), x.clone()], &[]); // coherence = 1.0
        let after = report(std::slice::from_ref(&a), std::slice::from_ref(&x)); // coherence = 0.5
        let d = diff_reports(&before, &after);
        assert_close(d.coherence_delta, -0.5);
    }

    // ── diff_reports: empty inputs ────────────────────────────────────────────

    /// Diffing two fully empty reports produces a zero delta.
    #[test]
    fn both_before_and_after_empty() {
        let empty = report(&[], &[]);
        let d = diff_reports(&empty, &empty);
        assert!(d.newly_severed.is_empty());
        assert!(d.newly_healed.is_empty());
        assert_close(d.coherence_delta, 0.0);
    }

    /// Arc not severed in `before` but severed in `after` → `newly_severed`.
    #[test]
    fn empty_before_severed_after_has_newly_severed() {
        let before = report(&[arc("a", "b")], &[]);
        let after = report(&[], &[arc("a", "b")]);
        let d = diff_reports(&before, &after);
        assert_eq!(d.newly_severed.len(), 1);
        assert!(d.newly_healed.is_empty());
    }

    /// Arc severed in `before` but not severed in `after` → `newly_healed`.
    #[test]
    fn before_has_severed_after_empty_severed() {
        let before = report(&[], &[arc("a", "b")]);
        let after = report(&[arc("a", "b")], &[]);
        let d = diff_reports(&before, &after);
        assert!(d.newly_severed.is_empty());
        assert_eq!(d.newly_healed.len(), 1);
    }

    /// All-severed before + all-present after → every arc is newly healed, none newly severed.
    #[test]
    fn before_all_severed_after_all_present() {
        let arcs = vec![arc("a", "b"), arc("c", "d"), arc("e", "f")];
        let before = report(&[], &arcs);
        let after = report(&arcs, &[]);
        let d = diff_reports(&before, &after);
        assert!(d.newly_severed.is_empty());
        assert_eq!(d.newly_healed.len(), 3);
        assert!(!d.is_regression());
    }

    /// When neither report has any severed arcs the delta is empty regardless of `present` sets.
    #[test]
    fn both_empty_severed_lists_yields_empty_delta() {
        let before = report(&[arc("a", "b"), arc("c", "d")], &[]);
        let after = report(&[arc("p", "q"), arc("r", "s")], &[]);
        let d = diff_reports(&before, &after);
        assert!(d.newly_severed.is_empty());
        assert!(d.newly_healed.is_empty());
    }

    // ── diff_reports: identical reports ───────────────────────────────────────

    /// Identical reports with no severed arcs → empty delta.
    #[test]
    fn identical_reports_with_no_severed() {
        let r = report(&[arc("a", "b"), arc("c", "d")], &[]);
        let d = diff_reports(&r, &r);
        assert!(d.newly_severed.is_empty());
        assert!(d.newly_healed.is_empty());
        assert!(!d.is_regression());
    }

    /// Identical reports that include severed arcs → empty delta (the arcs were already severed).
    #[test]
    fn identical_reports_with_some_severed() {
        let r = report(&[arc("a", "b")], &[arc("c", "d")]);
        let d = diff_reports(&r, &r);
        assert!(d.newly_severed.is_empty());
        assert!(d.newly_healed.is_empty());
    }

    /// `coherence_delta` is zero for identical reports.
    #[test]
    fn identical_reports_coherence_delta_zero() {
        let r = report(&[arc("a", "b")], &[arc("c", "d"), arc("e", "f")]);
        let d = diff_reports(&r, &r);
        assert_close(d.coherence_delta, 0.0);
    }

    // ── diff_reports: disjoint arc sets ───────────────────────────────────────

    /// When the `after` severed set is completely new, all its arcs are newly severed.
    #[test]
    fn disjoint_all_arcs_newly_severed() {
        let a = arc("a", "b");
        let x = arc("x", "y");
        let before = report(std::slice::from_ref(&a), &[]);
        let after = SeveredEarReport {
            present: vec![],
            severed: vec![x.clone()],
            coherence: 0.0,
        };
        let d = diff_reports(&before, &after);
        assert_eq!(d.newly_severed, vec![x]);
        assert!(d.newly_healed.is_empty());
    }

    /// When the `after` severed set is empty, all prior severed arcs are newly healed.
    #[test]
    fn disjoint_all_arcs_newly_healed() {
        let a = arc("a", "b");
        let before = SeveredEarReport {
            present: vec![],
            severed: vec![a.clone()],
            coherence: 0.0,
        };
        let after = report(&[arc("x", "y")], &[]);
        let d = diff_reports(&before, &after);
        assert!(d.newly_severed.is_empty());
        assert_eq!(d.newly_healed, vec![a]);
    }

    /// Completely disjoint severed sets → both lists populated, no shared arcs.
    #[test]
    fn disjoint_complete_swap() {
        let a = arc("a", "b");
        let x = arc("x", "y");
        let before = SeveredEarReport {
            present: vec![],
            severed: vec![a.clone()],
            coherence: 0.0,
        };
        let after = SeveredEarReport {
            present: vec![],
            severed: vec![x.clone()],
            coherence: 0.0,
        };
        let d = diff_reports(&before, &after);
        assert_eq!(d.newly_severed, vec![x]);
        assert_eq!(d.newly_healed, vec![a]);
    }

    // ── diff_reports: partial overlap ─────────────────────────────────────────

    /// Arcs shared between `before.severed` and `after.severed` do not appear in either list.
    #[test]
    fn partial_overlap_shared_arcs_absent_from_delta() {
        let shared = arc("shared", "common");
        let new_sev = arc("new", "sev");
        let before = SeveredEarReport {
            present: vec![],
            severed: vec![shared.clone()],
            coherence: 0.0,
        };
        let after = SeveredEarReport {
            present: vec![],
            severed: vec![shared.clone(), new_sev.clone()],
            coherence: 0.0,
        };
        let d = diff_reports(&before, &after);
        assert!(!d.newly_severed.contains(&shared));
        assert!(!d.newly_healed.contains(&shared));
        assert_eq!(d.newly_severed, vec![new_sev]);
        assert!(d.newly_healed.is_empty());
    }

    /// Only arcs not previously severed appear in `newly_severed`.
    #[test]
    fn partial_overlap_only_new_in_newly_severed() {
        let shared = arc("s", "t");
        let new_a = arc("a", "b");
        let new_b = arc("c", "d");
        let before = SeveredEarReport {
            present: vec![],
            severed: vec![shared.clone()],
            coherence: 0.0,
        };
        let after = SeveredEarReport {
            present: vec![],
            severed: vec![shared.clone(), new_a.clone(), new_b.clone()],
            coherence: 0.0,
        };
        let d = diff_reports(&before, &after);
        assert_eq!(d.newly_severed.len(), 2);
        assert!(d.newly_severed.contains(&new_a));
        assert!(d.newly_severed.contains(&new_b));
    }

    /// Only arcs that were severed but are no longer appear in `newly_healed`.
    #[test]
    fn partial_overlap_only_gone_in_newly_healed() {
        let shared = arc("s", "t");
        let gone_a = arc("a", "b");
        let gone_b = arc("c", "d");
        let before = SeveredEarReport {
            present: vec![],
            severed: vec![shared.clone(), gone_a.clone(), gone_b.clone()],
            coherence: 0.0,
        };
        let after = SeveredEarReport {
            present: vec![],
            severed: vec![shared.clone()],
            coherence: 0.0,
        };
        let d = diff_reports(&before, &after);
        assert_eq!(d.newly_healed.len(), 2);
        assert!(d.newly_healed.contains(&gone_a));
        assert!(d.newly_healed.contains(&gone_b));
    }

    // ── diff_reports: many arcs ───────────────────────────────────────────────

    /// All 10 arcs move from present to severed → all appear in `newly_severed`.
    #[test]
    fn many_arcs_all_newly_severed() {
        let arcs: Vec<Arc> = (0..10)
            .map(|i| arc(&format!("p{i}"), &format!("c{i}")))
            .collect();
        let before = report(&arcs, &[]);
        let after = report(&[], &arcs);
        let d = diff_reports(&before, &after);
        assert_eq!(d.newly_severed.len(), 10);
        assert!(d.newly_healed.is_empty());
    }

    /// All 10 arcs move from severed to present → all appear in `newly_healed`.
    #[test]
    fn many_arcs_all_newly_healed() {
        let arcs: Vec<Arc> = (0..10)
            .map(|i| arc(&format!("p{i}"), &format!("c{i}")))
            .collect();
        let before = report(&[], &arcs);
        let after = report(&arcs, &[]);
        let d = diff_reports(&before, &after);
        assert!(d.newly_severed.is_empty());
        assert_eq!(d.newly_healed.len(), 10);
    }

    /// 8-arc batch with a stable sub-group and a swapping sub-group.
    #[test]
    fn many_arcs_partial_change() {
        let all: Vec<Arc> = (0..8)
            .map(|i| arc(&format!("p{i}"), &format!("c{i}")))
            .collect();
        let (stable, changing) = all.split_at(4);
        // Stable arcs are severed in both snapshots; changing[0..2] heal, changing[2..4] sever.
        let before = SeveredEarReport {
            present: vec![],
            severed: stable
                .iter()
                .chain(changing.iter().take(2))
                .cloned()
                .collect(),
            coherence: 0.0,
        };
        let after = SeveredEarReport {
            present: vec![],
            severed: stable
                .iter()
                .chain(changing.iter().skip(2))
                .cloned()
                .collect(),
            coherence: 0.0,
        };
        let d = diff_reports(&before, &after);
        assert_eq!(d.newly_severed.len(), 2);
        assert_eq!(d.newly_healed.len(), 2);
    }

    // ── diff_reports: relation distinguishes arcs ─────────────────────────────

    /// Same producer+consumer but different relation = different arc identity.
    #[test]
    fn relation_distinguishes_otherwise_identical_arcs() {
        let calls = arc_r("A", "B", "calls");
        let defines = arc_r("A", "B", "defines");
        let before = SeveredEarReport {
            present: vec![],
            severed: vec![calls.clone()],
            coherence: 0.0,
        };
        let after = SeveredEarReport {
            present: vec![],
            severed: vec![defines.clone()],
            coherence: 0.0,
        };
        let d = diff_reports(&before, &after);
        assert!(d.newly_severed.contains(&defines));
        assert!(d.newly_healed.contains(&calls));
    }

    /// Changing the relation of a severed arc triggers both a sever and a heal.
    #[test]
    fn relation_change_registers_as_sever_and_heal() {
        let imports = arc_r("M", "N", "imports_from");
        let method = arc_r("M", "N", "method");
        let before = SeveredEarReport {
            present: vec![],
            severed: vec![imports.clone()],
            coherence: 0.0,
        };
        let after = SeveredEarReport {
            present: vec![],
            severed: vec![method.clone()],
            coherence: 0.0,
        };
        let d = diff_reports(&before, &after);
        assert_eq!(d.newly_severed, vec![method]);
        assert_eq!(d.newly_healed, vec![imports]);
    }

    /// Multiple relation variants between the same node pair are tracked independently.
    #[test]
    fn multiple_relations_tracked_independently() {
        let calls = arc_r("A", "B", "calls");
        let defines = arc_r("A", "B", "defines");
        let method = arc_r("A", "B", "method");
        // Before: calls + defines severed.  After: defines + method severed.
        let before = SeveredEarReport {
            present: vec![],
            severed: vec![calls.clone(), defines.clone()],
            coherence: 0.0,
        };
        let after = SeveredEarReport {
            present: vec![],
            severed: vec![defines.clone(), method.clone()],
            coherence: 0.0,
        };
        let d = diff_reports(&before, &after);
        // `method` is newly severed; `defines` stays severed (not in delta); `calls` is healed.
        assert!(d.newly_severed.contains(&method));
        assert!(!d.newly_severed.contains(&defines));
        assert!(d.newly_healed.contains(&calls));
    }

    // ── diff_reports: producer/consumer distinguish arcs ──────────────────────

    /// Different producer with same consumer + relation = different arc.
    #[test]
    fn producer_distinguishes_otherwise_identical_arcs() {
        let a_to_b = arc_r("A", "B", "calls");
        let x_to_b = arc_r("X", "B", "calls");
        let before = SeveredEarReport {
            present: vec![],
            severed: vec![a_to_b.clone()],
            coherence: 0.0,
        };
        let after = SeveredEarReport {
            present: vec![],
            severed: vec![x_to_b.clone()],
            coherence: 0.0,
        };
        let d = diff_reports(&before, &after);
        assert!(d.newly_severed.contains(&x_to_b));
        assert!(d.newly_healed.contains(&a_to_b));
    }

    /// Same producer, different consumer = different arc.
    #[test]
    fn consumer_distinguishes_otherwise_identical_arcs() {
        let a_to_b = arc_r("A", "B", "calls");
        let a_to_z = arc_r("A", "Z", "calls");
        let before = SeveredEarReport {
            present: vec![],
            severed: vec![a_to_b.clone()],
            coherence: 0.0,
        };
        let after = SeveredEarReport {
            present: vec![],
            severed: vec![a_to_z.clone()],
            coherence: 0.0,
        };
        let d = diff_reports(&before, &after);
        assert!(d.newly_severed.contains(&a_to_z));
        assert!(d.newly_healed.contains(&a_to_b));
    }

    /// Swapping producer and consumer produces two distinct arcs; both in same severed set → no delta.
    #[test]
    fn swapped_producer_consumer_are_distinct_arcs() {
        let a_to_b = arc_r("A", "B", "calls");
        let b_to_a = arc_r("B", "A", "calls");
        let both = vec![a_to_b.clone(), b_to_a.clone()];
        let before = SeveredEarReport {
            present: vec![],
            severed: both.clone(),
            coherence: 0.0,
        };
        let after = SeveredEarReport {
            present: vec![],
            severed: both,
            coherence: 0.0,
        };
        let d = diff_reports(&before, &after);
        assert!(d.newly_severed.is_empty());
        assert!(d.newly_healed.is_empty());
    }

    // ── diff_reports: ordering determinism ────────────────────────────────────

    /// `newly_severed` is sorted regardless of the order arcs appear in `after.severed`.
    #[test]
    fn newly_severed_output_is_sorted() {
        let arcs = vec![
            arc_r("Z", "Z", "calls"),
            arc_r("M", "M", "calls"),
            arc_r("A", "A", "calls"),
        ];
        let before = report(&arcs, &[]);
        let after = SeveredEarReport {
            present: vec![],
            severed: arcs.clone(),
            coherence: 0.0,
        };
        let d = diff_reports(&before, &after);
        let mut sorted = d.newly_severed.clone();
        sorted.sort_unstable();
        assert_eq!(d.newly_severed, sorted);
    }

    /// `newly_healed` is sorted regardless of the order arcs appear in `before.severed`.
    #[test]
    fn newly_healed_output_is_sorted() {
        let arcs = vec![
            arc_r("Z", "A", "z"),
            arc_r("A", "Z", "a"),
            arc_r("M", "M", "m"),
        ];
        let before = SeveredEarReport {
            present: vec![],
            severed: arcs.clone(),
            coherence: 0.0,
        };
        let after = report(&arcs, &[]);
        let d = diff_reports(&before, &after);
        let mut sorted = d.newly_healed.clone();
        sorted.sort_unstable();
        assert_eq!(d.newly_healed, sorted);
    }

    /// Two calls with the same logical set but different insertion order produce identical output.
    #[test]
    fn determinism_independent_of_severed_list_order() {
        let a = arc("a", "b");
        let b = arc("b", "c");
        let c = arc("c", "d");
        let before = report(&[a.clone(), b.clone(), c.clone()], &[]);
        let after_fwd = SeveredEarReport {
            present: vec![],
            severed: vec![a.clone(), b.clone(), c.clone()],
            coherence: 0.0,
        };
        let after_rev = SeveredEarReport {
            present: vec![],
            severed: vec![c.clone(), b.clone(), a.clone()],
            coherence: 0.0,
        };
        let d1 = diff_reports(&before, &after_fwd);
        let d2 = diff_reports(&before, &after_rev);
        assert_eq!(d1.newly_severed, d2.newly_severed);
    }

    // ── diff_reports: symmetry ────────────────────────────────────────────────

    /// `diff(a, b).newly_severed` == `diff(b, a).newly_healed` and vice-versa.
    #[test]
    fn symmetry_severed_and_healed_swap() {
        let a = arc("a", "b");
        let x = arc("x", "y");
        let before = SeveredEarReport {
            present: vec![],
            severed: vec![a.clone()],
            coherence: 0.0,
        };
        let after = SeveredEarReport {
            present: vec![],
            severed: vec![x.clone()],
            coherence: 0.0,
        };
        let d_fwd = diff_reports(&before, &after);
        let d_rev = diff_reports(&after, &before);
        assert_eq!(d_fwd.newly_severed, d_rev.newly_healed);
        assert_eq!(d_fwd.newly_healed, d_rev.newly_severed);
    }

    /// `diff(a, b).coherence_delta` == `−diff(b, a).coherence_delta`.
    #[test]
    fn symmetry_coherence_delta_negated() {
        let before = SeveredEarReport {
            present: vec![],
            severed: vec![],
            coherence: 0.3,
        };
        let after = SeveredEarReport {
            present: vec![],
            severed: vec![],
            coherence: 0.7,
        };
        let d_fwd = diff_reports(&before, &after);
        let d_rev = diff_reports(&after, &before);
        assert_close(d_fwd.coherence_delta, -d_rev.coherence_delta);
    }

    /// If forward diff is a regression, the reverse diff is not (and vice-versa).
    #[test]
    fn symmetry_is_regression_inverts() {
        let a = arc("a", "b");
        let before = report(std::slice::from_ref(&a), &[]);
        let after = report(&[], std::slice::from_ref(&a));
        let d_fwd = diff_reports(&before, &after);
        let d_rev = diff_reports(&after, &before);
        assert!(d_fwd.is_regression());
        assert!(!d_rev.is_regression());
    }
}
