//! Token-budgeted packing (FO-2) — fit a relevance-ordered result within `max_tokens`.
//!
//! An MCP client (e.g. Claude Code) passes `max_tokens = K`; the serve layer must pack ≤ K tokens
//! worth of content, relevance-ordered, and **never drop the seed** (the most-relevant item).
//!
//! # Token estimation
//!
//! Token cost uses the deterministic `ceil(bytes / 4)` heuristic — a dependency-free approximation
//! of BPE tokenisation that is byte-stable across calls and platforms (R4). Multi-byte UTF-8
//! characters cost proportionally more because their **byte length** is counted, not their
//! code-point count.
//!
//! # Truncation note
//!
//! When candidates are dropped the output is suffixed with a line of the form:
//!
//! ```text
//! … N more omitted (token budget K)
//! ```
//!
//! where `N` is the number of dropped candidates and `K` is `max_tokens`. The note is never added
//! when all candidates fit.

use std::fmt::Write as _;

/// Estimates the token count of `text` as `ceil(bytes / 4)` — a dependency-free BPE approximation.
///
/// Token cost is measured in **UTF-8 bytes**, not Unicode code-points, so multi-byte characters
/// count proportionally more (e.g. a CJK character is 3 bytes ≈ 1 token; a four-byte emoji counts
/// as 1 token at the 4-byte boundary). The result is deterministic and byte-stable (R4).
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Packs `header`, an optional `seed`, and as many `candidates` (in order) as fit within
/// `max_tokens` tokens, then appends a truncation note when any candidates are dropped.
///
/// # Packing policy
///
/// - `header` is **always** included and forms the start of the returned `String`.
/// - `seed` (the most-relevant result, e.g. the top graph-query match) is **always** included,
///   even if `seed` alone exceeds `max_tokens`. An agent must always receive the best result.
/// - Candidates are taken **in order** while the running [`estimate_tokens`] sum stays
///   `<= max_tokens`, stopping at the first candidate that would exceed the budget.
/// - When `max_tokens == 0`, only `header` and `seed` are included; all candidates are dropped.
/// - When any candidates are dropped the output is suffixed with a line of the form
///   `"… N more omitted (token budget K)\n"`, where `N` is the dropped count and `K` is
///   `max_tokens`.
///
/// # Determinism
///
/// Identical inputs always produce identical output (R4). No interior randomness, no sorting.
///
/// # Overflow / underflow safety
///
/// `used` is accumulated with [`usize::saturating_add`] so that absurdly large inputs never
/// cause a panic. The dropped-count subtraction `candidates.len() - included` cannot underflow
/// because `included <= candidates.len()` is maintained as a loop invariant.
#[must_use]
pub fn pack(header: &str, seed: Option<&str>, candidates: &[String], max_tokens: usize) -> String {
    let mut out = String::from(header);
    let mut used = estimate_tokens(header);

    if let Some(s) = seed {
        out.push_str(s);
        used = used.saturating_add(estimate_tokens(s));
    }

    let mut included = 0_usize;
    for cand in candidates {
        let cost = estimate_tokens(cand);
        if used.saturating_add(cost) > max_tokens {
            break;
        }
        out.push_str(cand);
        used = used.saturating_add(cost);
        included += 1;
    }

    let dropped = candidates.len() - included;
    if dropped > 0 {
        let _ = writeln!(out, "… {dropped} more omitted (token budget {max_tokens})");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{estimate_tokens, pack};

    // ── estimate_tokens ──────────────────────────────────────────────────────

    #[test]
    fn estimate_zero_bytes() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_one_byte() {
        // ceil(1/4) = 1
        assert_eq!(estimate_tokens("a"), 1);
    }

    #[test]
    fn estimate_three_bytes() {
        // ceil(3/4) = 1
        assert_eq!(estimate_tokens("abc"), 1);
    }

    #[test]
    fn estimate_four_bytes_boundary() {
        // 4 bytes → ceil(4/4) = 1 (exact boundary, no rounding up)
        assert_eq!(estimate_tokens("abcd"), 1);
    }

    #[test]
    fn estimate_five_bytes() {
        // ceil(5/4) = 2
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn estimate_eight_bytes() {
        // ceil(8/4) = 2
        assert_eq!(estimate_tokens("aaaaaaaa"), 2);
    }

    #[test]
    fn estimate_nine_bytes() {
        // ceil(9/4) = 3
        assert_eq!(estimate_tokens("aaaaaaaaa"), 3);
    }

    #[test]
    fn estimate_twelve_bytes() {
        // ceil(12/4) = 3
        assert_eq!(estimate_tokens("aaaaaaaaaaaa"), 3);
    }

    #[test]
    fn estimate_thirteen_bytes() {
        // ceil(13/4) = 4
        assert_eq!(estimate_tokens("aaaaaaaaaaaaa"), 4);
    }

    #[test]
    fn estimate_sixteen_bytes() {
        // ceil(16/4) = 4
        assert_eq!(estimate_tokens("aaaaaaaaaaaaaaaa"), 4);
    }

    #[test]
    fn estimate_multibyte_e_acute_two_bytes() {
        // é = 0xC3 0xA9 (2 UTF-8 bytes) → ceil(2/4) = 1
        assert_eq!(estimate_tokens("é"), 1);
    }

    #[test]
    fn estimate_multibyte_e_acute_plus_ascii_three_bytes() {
        // "é" (2 bytes) + "e" (1 byte) = 3 bytes → ceil(3/4) = 1
        assert_eq!(estimate_tokens("ée"), 1);
    }

    #[test]
    fn estimate_multibyte_two_e_acute_four_bytes() {
        // "éé" = 2+2 = 4 bytes → ceil(4/4) = 1
        assert_eq!(estimate_tokens("éé"), 1);
    }

    #[test]
    fn estimate_multibyte_five_bytes() {
        // "ééa" = 2+2+1 = 5 bytes → ceil(5/4) = 2
        assert_eq!(estimate_tokens("ééa"), 2);
    }

    #[test]
    fn estimate_cjk_three_bytes() {
        // 中 = 0xE4 0xB8 0xAD (3 UTF-8 bytes) → ceil(3/4) = 1
        assert_eq!(estimate_tokens("中"), 1);
    }

    #[test]
    fn estimate_emoji_four_bytes() {
        // 𝄞 (MUSICAL SYMBOL G CLEF) = 4 UTF-8 bytes → ceil(4/4) = 1
        assert_eq!(estimate_tokens("𝄞"), 1);
    }

    #[test]
    fn estimate_emoji_two_copies_eight_bytes() {
        // "𝄞𝄞" = 4+4 = 8 bytes → ceil(8/4) = 2
        assert_eq!(estimate_tokens("𝄞𝄞"), 2);
    }

    #[test]
    fn estimate_counts_bytes_not_code_points() {
        // "中中中中" = 4 Unicode code-points but 12 UTF-8 bytes → ceil(12/4) = 3 (not 1)
        assert_eq!(estimate_tokens("中中中中"), 3);
    }

    #[test]
    fn estimate_newline_is_one_byte() {
        // '\n' = 0x0A (1 byte) → ceil(1/4) = 1
        assert_eq!(estimate_tokens("\n"), 1);
    }

    #[test]
    fn estimate_two_spaces() {
        // 2 bytes → ceil(2/4) = 1
        assert_eq!(estimate_tokens("  "), 1);
    }

    // ── pack — structural invariants ─────────────────────────────────────────

    #[test]
    fn pack_output_starts_with_header() {
        let out = pack("HEADER\n", Some("seed\n"), &["cand\n".to_owned()], 100);
        assert!(
            out.starts_with("HEADER\n"),
            "output must start with header: {out:?}"
        );
    }

    #[test]
    fn pack_empty_header_works() {
        // Empty header: seed immediately at start
        let out = pack("", Some("seed\n"), &[], 100);
        assert_eq!(out, "seed\n");
    }

    #[test]
    fn pack_empty_candidates_no_note() {
        let out = pack("H\n", None, &[], 100);
        assert!(!out.contains("omitted"), "{out:?}");
    }

    #[test]
    fn pack_all_empty_inputs_produce_empty_string() {
        // header="", seed=Some(""), no candidates, max_tokens=0 → empty output, no note
        let out = pack("", Some(""), &[], 0);
        assert_eq!(out, "");
    }

    // ── pack — seed behaviour ────────────────────────────────────────────────

    #[test]
    fn pack_seed_none_produces_only_header() {
        let out = pack("H\n", None, &[], 100);
        assert_eq!(out, "H\n");
    }

    #[test]
    fn pack_seed_always_included_at_max_tokens_zero_with_candidates() {
        let cands = vec!["cand\n".to_owned()];
        let out = pack("H", Some("SEED"), &cands, 0);
        assert!(
            out.contains("SEED"),
            "seed must appear even at max_tokens=0: {out:?}"
        );
    }

    #[test]
    fn pack_seed_always_included_at_max_tokens_zero_no_candidates() {
        let out = pack("H", Some("SEED"), &[], 0);
        assert!(out.contains("SEED"), "{out:?}");
    }

    #[test]
    fn pack_seed_always_included_even_when_exceeds_budget() {
        // seed = 100-char ASCII string → 25 tokens; max_tokens=1: seed must still appear
        let big_seed = "x".repeat(100);
        let out = pack("H", Some(&big_seed), &[], 1);
        assert!(
            out.contains(&big_seed),
            "seed must be in output even over budget: {out:?}"
        );
    }

    #[test]
    fn pack_seed_immediately_follows_header() {
        let out = pack("HDR", Some("SEED"), &["cand".to_owned()], 100);
        assert!(
            out.starts_with("HDRSEED"),
            "seed must follow header directly: {out:?}"
        );
    }

    #[test]
    fn pack_empty_seed_some_is_included_at_zero_cost() {
        // Some("") → seed present, 0 tokens; output = header + "" = header
        let out = pack("H", Some(""), &[], 0);
        assert_eq!(out, "H");
    }

    #[test]
    fn pack_seed_none_candidates_processed_normally() {
        let cands: Vec<String> = vec!["a\n".to_owned(), "b\n".to_owned()];
        let out = pack("H\n", None, &cands, 10_000);
        assert!(out.contains("a\n") && out.contains("b\n"), "{out:?}");
        assert!(!out.contains("omitted"), "{out:?}");
    }

    // ── pack — max_tokens = 0 ────────────────────────────────────────────────

    #[test]
    fn pack_max_zero_no_seed_no_candidates_no_note() {
        let out = pack("H", None, &[], 0);
        assert_eq!(out, "H");
        assert!(!out.contains("omitted"), "{out:?}");
    }

    #[test]
    fn pack_max_zero_seed_no_candidates_no_note() {
        let out = pack("H", Some("SEED"), &[], 0);
        assert_eq!(out, "HSEED");
        assert!(!out.contains("omitted"), "{out:?}");
    }

    #[test]
    fn pack_max_zero_seed_with_candidates_note_shows_all_dropped() {
        let cands: Vec<String> = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let out = pack("H", Some("S"), &cands, 0);
        assert!(
            out.contains("3 more omitted"),
            "all 3 must be noted as dropped: {out:?}"
        );
    }

    #[test]
    fn pack_max_zero_no_seed_with_candidates_note_shown() {
        let cands: Vec<String> = vec!["x".to_owned()];
        let out = pack("H", None, &cands, 0);
        assert!(out.contains("1 more omitted"), "{out:?}");
    }

    // ── pack — all fit ───────────────────────────────────────────────────────

    #[test]
    fn pack_all_fit_large_budget_no_note() {
        let cands: Vec<String> = vec!["a\n".to_owned(), "b\n".to_owned(), "c\n".to_owned()];
        let out = pack("H\n", None, &cands, 10_000);
        assert!(!out.contains("omitted"), "no truncation expected: {out:?}");
        assert!(out.contains("a\n") && out.contains("b\n") && out.contains("c\n"));
    }

    #[test]
    fn pack_all_fit_single_candidate() {
        let cands = vec!["only\n".to_owned()];
        let out = pack("H\n", None, &cands, 10_000);
        assert!(out.contains("only\n"));
        assert!(!out.contains("omitted"));
    }

    // ── pack — none fit ──────────────────────────────────────────────────────

    #[test]
    fn pack_none_fit_note_count_equals_total_candidates() {
        // Budget = 0, header costs 1 token → candidates can't fit
        let cands: Vec<String> = (0..5).map(|i| format!("item{i}")).collect();
        let out = pack("H", None, &cands, 0);
        assert!(out.contains("5 more omitted"), "{out:?}");
    }

    #[test]
    fn pack_none_fit_when_header_exactly_consumes_budget() {
        // header = "x"*40 = 40 bytes = 10 tokens; max_tokens = 10
        // candidate "y"*4 = 4 bytes = 1 token → 10+1 > 10 → dropped
        let header = "x".repeat(40);
        let cands: Vec<String> = vec!["y".repeat(4)];
        let out = pack(&header, None, &cands, 10);
        assert!(out.contains("1 more omitted"), "{out:?}");
    }

    // ── pack — partial fit ───────────────────────────────────────────────────

    #[test]
    fn pack_partial_first_three_of_five_fit() {
        // header = "" (0 tokens), seed = None
        // each candidate "aaaa" = 4 bytes = 1 token; max_tokens = 3
        // → first 3 fit (used: 0→1→2→3), 4th (3+1=4>3) breaks → dropped = 2
        let cands: Vec<String> = (0..5).map(|_| "aaaa".to_owned()).collect();
        let out = pack("", None, &cands, 3);
        assert!(out.contains("2 more omitted"), "{out:?}");
    }

    #[test]
    fn pack_partial_one_candidate_dropped() {
        // header="", seed=None, 2 cands of 1 token each, budget=1 → 1 fits, 1 dropped
        let cands: Vec<String> = vec!["aaaa".to_owned(), "bbbb".to_owned()];
        let out = pack("", None, &cands, 1);
        assert!(out.contains("1 more omitted"), "{out:?}");
        assert!(
            out.contains("aaaa"),
            "first cand must be in output: {out:?}"
        );
        assert!(
            !out.contains("bbbb"),
            "second cand must be dropped: {out:?}"
        );
    }

    #[test]
    fn pack_budget_exactly_fits_all_candidates_no_note() {
        // 4 candidates × 1 token each = 4 tokens = max_tokens → all fit, no note
        let cands: Vec<String> = (0..4).map(|_| "aaaa".to_owned()).collect();
        let out = pack("", None, &cands, 4);
        assert!(
            !out.contains("omitted"),
            "all fit exactly, no note expected: {out:?}"
        );
        assert_eq!(out, "aaaa".repeat(4));
    }

    #[test]
    fn pack_budget_consumed_by_header_plus_seed_leaves_no_room() {
        // header = "aaaa" (1 token), seed = "bbbb" (1 token), used=2, max_tokens=2
        // candidate "cccc" (1 token): 2+1 > 2 → dropped
        let cands: Vec<String> = vec!["cccc".to_owned()];
        let out = pack("aaaa", Some("bbbb"), &cands, 2);
        assert!(out.contains("1 more omitted"), "{out:?}");
    }

    // ── pack — note wording ──────────────────────────────────────────────────

    #[test]
    fn pack_note_starts_with_ellipsis_space_count() {
        let cands: Vec<String> = vec!["x".to_owned(), "y".to_owned()];
        let out = pack("", None, &cands, 0);
        assert!(
            out.contains("… 2 more omitted"),
            "note format incorrect: {out:?}"
        );
    }

    #[test]
    fn pack_note_contains_token_budget_keyword() {
        let cands = vec!["x".to_owned()];
        let out = pack("", None, &cands, 0);
        assert!(out.contains("token budget"), "{out:?}");
    }

    #[test]
    fn pack_note_contains_exact_max_tokens_value() {
        // candidate too large to fit; note must show max_tokens = 42
        let cands = vec!["x".repeat(200)]; // 200 bytes = 50 tokens; won't fit in 42
        let out = pack("", None, &cands, 42);
        assert!(
            out.contains("token budget 42"),
            "budget value must be in note: {out:?}"
        );
    }

    #[test]
    fn pack_note_contains_exact_dropped_count() {
        // header = "x"*12 = 12 bytes = 3 tokens; max_tokens = 3; 10 candidates each 1 token
        // used=3 after header, 3+1>3 → all dropped
        let header = "x".repeat(12);
        let cands: Vec<String> = (0..10).map(|_| "aaaa".to_owned()).collect();
        let out = pack(&header, None, &cands, 3);
        assert!(out.contains("10 more omitted"), "{out:?}");
    }

    #[test]
    fn pack_note_ends_with_newline() {
        let cands = vec!["x".repeat(200)];
        let out = pack("", None, &cands, 0);
        assert!(out.ends_with('\n'), "note must end with newline: {out:?}");
    }

    // ── pack — ordering and determinism ─────────────────────────────────────

    #[test]
    fn pack_candidates_appear_in_input_order() {
        let cands: Vec<String> = vec![
            "first\n".to_owned(),
            "second\n".to_owned(),
            "third\n".to_owned(),
        ];
        let out = pack("", None, &cands, 10_000);
        let pos_first = out.find("first").unwrap_or(usize::MAX);
        let pos_second = out.find("second").unwrap_or(0);
        let pos_third = out.find("third").unwrap_or(0);
        assert!(
            pos_first < pos_second && pos_second < pos_third,
            "order not preserved: {out:?}"
        );
    }

    #[test]
    fn pack_first_oversized_candidate_stops_all_remaining() {
        // cand[0] is huge (many tokens), cand[1] is tiny; greedy break means both dropped
        let large = "z".repeat(400); // 400 bytes = 100 tokens
        let cands: Vec<String> = vec![large, "tiny".to_owned()];
        let out = pack("", None, &cands, 2);
        // budget=2; large needs 100 (doesn't fit) → break; "tiny" never tried
        assert!(
            out.contains("2 more omitted"),
            "both must be dropped: {out:?}"
        );
        assert!(!out.contains("tiny"), "tiny must not appear: {out:?}");
    }

    #[test]
    fn pack_determinism_identical_inputs_same_output() {
        let cands: Vec<String> = (0..10).map(|i| format!("item{i}\n")).collect();
        let a = pack("header\n", Some("seed\n"), &cands, 20);
        let b = pack("header\n", Some("seed\n"), &cands, 20);
        assert_eq!(a, b, "identical inputs must produce identical output");
    }

    #[test]
    fn pack_determinism_large_candidate_set() {
        let cands: Vec<String> = (0..100).map(|i| format!("node_{i}\n")).collect();
        let a = pack("q\n", Some("top\n"), &cands, 50);
        let b = pack("q\n", Some("top\n"), &cands, 50);
        assert_eq!(a, b);
    }

    // ── pack — content correctness ───────────────────────────────────────────

    #[test]
    fn pack_included_candidate_text_present_verbatim() {
        let cands: Vec<String> = vec!["specific_content".to_owned()];
        let out = pack("", None, &cands, 10_000);
        assert!(out.contains("specific_content"), "{out:?}");
    }

    #[test]
    fn pack_dropped_candidate_text_absent_from_output() {
        // header = "H"*40 = 40 bytes = 10 tokens = max_tokens; candidate can't fit
        let header = "H".repeat(40);
        let cands: Vec<String> = vec!["must_not_appear".to_owned()];
        let out = pack(&header, None, &cands, 10);
        // used=10, cost of "must_not_appear"=ceil(15/4)=4 → 10+4>10 → dropped
        assert!(!out.contains("must_not_appear"), "{out:?}");
    }

    #[test]
    fn pack_seed_text_appears_verbatim_in_output() {
        let seed = "verbatim_seed_text_unique";
        let out = pack("H", Some(seed), &[], 100);
        assert!(out.contains(seed), "{out:?}");
    }

    // ── pack — boundary arithmetic ───────────────────────────────────────────

    #[test]
    fn pack_budget_exactly_fits_one_candidate() {
        // header="" (0 tokens), seed=None, cand="aaaa" (1 token), max_tokens=1 → fits, no note
        let cands = vec!["aaaa".to_owned()];
        let out = pack("", None, &cands, 1);
        assert!(out.contains("aaaa"), "{out:?}");
        assert!(!out.contains("omitted"), "{out:?}");
    }

    #[test]
    fn pack_budget_one_too_small_for_two_byte_token_candidate() {
        // "aaaaa" = 5 bytes = ceil(5/4)=2 tokens; budget=1 → doesn't fit
        let cands = vec!["aaaaa".to_owned()];
        let out = pack("", None, &cands, 1);
        assert!(out.contains("1 more omitted"), "{out:?}");
    }

    #[test]
    fn pack_large_candidate_set_correct_dropped_count() {
        // 100 candidates each 1 token ("aaaa"), budget=5 → 5 fit, 95 dropped
        let cands: Vec<String> = (0..100).map(|_| "aaaa".to_owned()).collect();
        let out = pack("", None, &cands, 5);
        assert!(out.contains("95 more omitted"), "{out:?}");
    }

    #[test]
    fn pack_header_exact_budget_no_candidates_no_note() {
        // header = "aaaa"*5 = 20 bytes = 5 tokens = max_tokens; no candidates → no note
        let header = "aaaa".repeat(5);
        let out = pack(&header, None, &[], 5);
        assert!(!out.contains("omitted"), "{out:?}");
    }

    #[test]
    fn pack_seed_costs_tokens_and_reduces_candidate_space() {
        // header="" (0), seed="aaaa" (1 token), used=1, max_tokens=1
        // candidate "bbbb" (1 token): 1+1>1 → dropped
        let cands: Vec<String> = vec!["bbbb".to_owned()];
        let out = pack("", Some("aaaa"), &cands, 1);
        assert!(out.contains("1 more omitted"), "{out:?}");
        assert!(out.contains("aaaa"), "seed must be present: {out:?}");
    }

    #[test]
    fn pack_seed_multibyte_counted_by_utf8_bytes() {
        // seed "中中" = 6 bytes = ceil(6/4)=2 tokens; max_tokens=1 (seed still included always)
        // candidate "ab" = 2 bytes = 1 token; used=2 after seed → 2+1>1 → dropped
        let cands: Vec<String> = vec!["ab".to_owned()];
        let out = pack("", Some("中中"), &cands, 1);
        assert!(
            out.contains("中中"),
            "multibyte seed must be present: {out:?}"
        );
        assert!(
            out.contains("1 more omitted"),
            "candidate must be dropped: {out:?}"
        );
    }

    #[test]
    fn pack_zero_candidates_zero_max_tokens_no_note() {
        // No candidates to drop → no note regardless of max_tokens
        let out = pack("header", None, &[], 0);
        assert!(!out.contains("omitted"), "{out:?}");
    }
}
