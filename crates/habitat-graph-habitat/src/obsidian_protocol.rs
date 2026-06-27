//! Obsidian protocol — `Back to:` navigation headers, `MASTER_INDEX` list entries,
//! graph-note rendering, and the `hmem rebuild` hook boundary.
//!
//! # Security
//!
//! Every piece of caller-supplied text that reaches an output surface is funnelled through
//! [`habitat_graph_core::display_safe`] before inclusion. This neutralises Trojan-Source
//! bidi-override codepoints (CVE-2021-42574) before they can be written to any Obsidian note,
//! shell command, or downstream renderer.
//!
//! # Design
//!
//! All functions in this module are **pure** (no I/O, no allocation beyond `String`). The only
//! I/O lives behind the [`RebuildHook`] trait, which callers receive as a dependency:
//!
//! * [`NoopRebuild`] — always succeeds silently; the default in tests and dev environments.
//! * [`HmemRebuild`] — runs `hmem rebuild` as a subprocess; available only under the `live`
//!   feature so the default build and test suite remain process-free.

use habitat_graph_core::{display_safe, Result};

// ─── fixed wikilink anchors ──────────────────────────────────────────────────

/// The canonical `[[CLAUDE.md]]` wikilink included in every back-to header.
const WIKILINK_CLAUDE_MD: &str = "[[CLAUDE.md]]";

/// The canonical `[[CLAUDE.local.md]]` wikilink included in every back-to header.
const WIKILINK_CLAUDE_LOCAL_MD: &str = "[[CLAUDE.local.md]]";

/// The separator used between navigation links in the back-to header.
const LINK_SEP: &str = " \u{00B7} ";

// ─── render_back_to_header ───────────────────────────────────────────────────

/// Returns the standard Obsidian *Back to:* navigation header placed at the top of every
/// habitat note.
///
/// The base form with no extras is:
///
/// ```text
/// > Back to: [[CLAUDE.md]] · [[CLAUDE.local.md]]
/// ```
///
/// Each entry in `extra_links` is appended as `· [[link]]`, with the link text passed through
/// [`display_safe`] to neutralise any bidi-override codepoints before they reach the file.
///
/// # Examples
///
/// ```
/// # use habitat_graph_habitat::obsidian_protocol::render_back_to_header;
/// let h = render_back_to_header(&["Session 42"]);
/// assert!(h.contains("[[Session 42]]"));
/// assert!(h.contains("[[CLAUDE.md]]"));
/// ```
#[must_use]
pub fn render_back_to_header(extra_links: &[&str]) -> String {
    let mut out = format!(
        "> Back to: {WIKILINK_CLAUDE_MD}{LINK_SEP}{WIKILINK_CLAUDE_LOCAL_MD}"
    );
    for link in extra_links {
        let safe = display_safe(link);
        out.push_str(LINK_SEP);
        out.push_str("[[");
        out.push_str(&safe);
        out.push_str("]]");
    }
    out
}

// ─── render_master_index_entry ───────────────────────────────────────────────

/// Returns a single `MASTER_INDEX` list entry in the canonical format:
///
/// ```text
/// - [title](file) — hook
/// ```
///
/// All three text parameters are passed through [`display_safe`] before inclusion so that
/// bidi-override codepoints in untrusted titles, paths, or description hooks cannot escape into
/// the rendered Markdown.
///
/// # Examples
///
/// ```
/// # use habitat_graph_habitat::obsidian_protocol::render_master_index_entry;
/// let e = render_master_index_entry("Arc-graph", "arc_graph.md", "severed-ear diff");
/// assert_eq!(e, "- [Arc-graph](arc_graph.md) \u{2014} severed-ear diff");
/// ```
#[must_use]
pub fn render_master_index_entry(title: &str, file: &str, hook: &str) -> String {
    let safe_title = display_safe(title);
    let safe_file = display_safe(file);
    let safe_hook = display_safe(hook);
    // U+2014 = EM DASH (the canonical habitat separator)
    format!("- [{safe_title}]({safe_file}) \u{2014} {safe_hook}")
}

// ─── render_graph_note ───────────────────────────────────────────────────────

/// Renders a complete Obsidian knowledge-graph note.
///
/// The note structure is:
///
/// ```text
/// > Back to: [[CLAUDE.md]] · [[CLAUDE.local.md]]
///
/// # <title>
///
/// ## Stats
///
/// nodes: N  edges: E  communities: C
///
/// ## Top hubs
///
/// - [[label]] (degree)
/// ```
///
/// Constraints:
/// * The note **starts** with the standard [`render_back_to_header`] (no extra links).
/// * `title` and every hub label are passed through [`display_safe`] before inclusion.
/// * `counts` is `(nodes, edges, communities)`.
/// * An empty `top_hubs` slice still emits the `## Top hubs` heading (with no list items).
///
/// # Examples
///
/// ```
/// # use habitat_graph_habitat::obsidian_protocol::render_graph_note;
/// let note = render_graph_note("Workspace", (100, 250, 12), &[("main", 40), ("lib", 30)]);
/// assert!(note.starts_with("> Back to:"));
/// assert!(note.contains("## Stats"));
/// assert!(note.contains("[[main]] (40)"));
/// ```
#[must_use]
pub fn render_graph_note(
    title: &str,
    counts: (usize, usize, usize),
    top_hubs: &[(&str, usize)],
) -> String {
    let (nodes, edges, communities) = counts;
    let safe_title = display_safe(title);
    let back_to = render_back_to_header(&[]);

    // Pre-compute hub lines so we know approximate capacity.
    let hub_lines: Vec<String> = top_hubs
        .iter()
        .map(|(label, degree)| {
            let safe_label = display_safe(label);
            format!("\n- [[{safe_label}]] ({degree})")
        })
        .collect();

    let hub_block: String = hub_lines.concat();

    format!(
        "{back_to}\n\n# {safe_title}\n\n\
         ## Stats\n\nnodes: {nodes}  edges: {edges}  communities: {communities}\n\n\
         ## Top hubs{hub_block}"
    )
}

// ─── RebuildHook ─────────────────────────────────────────────────────────────

/// A hook invoked after the graph is mutated to signal that external text-search indices
/// should be rebuilt.
///
/// The crate ships two implementations:
/// * [`NoopRebuild`] — silent no-op; suitable for tests and environments where `hmem` is absent.
/// * [`HmemRebuild`] — runs `hmem rebuild` as a subprocess (requires the `live` feature).
///
/// Callers receive a `&dyn RebuildHook` or a generic `R: RebuildHook` so the concrete choice
/// is a dependency that can be injected at the call-site.
pub trait RebuildHook {
    /// Triggers a rebuild of dependent indices.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Io`] if the underlying rebuild command fails or returns a non-zero
    /// exit code.
    fn rebuild(&self) -> Result<()>;
}

// ─── NoopRebuild ─────────────────────────────────────────────────────────────

/// A [`RebuildHook`] that does nothing and always succeeds.
///
/// Use this as the default hook in tests and in contexts where `hmem` is not deployed.
///
/// ```
/// # use habitat_graph_habitat::obsidian_protocol::{NoopRebuild, RebuildHook};
/// let hook = NoopRebuild::new();
/// assert!(hook.rebuild().is_ok());
/// ```
#[derive(Debug, Clone, Default)]
pub struct NoopRebuild;

impl NoopRebuild {
    /// Creates a new `NoopRebuild`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl RebuildHook for NoopRebuild {
    fn rebuild(&self) -> Result<()> {
        Ok(())
    }
}

// ─── HmemRebuild (live feature only) ─────────────────────────────────────────

/// A [`RebuildHook`] that runs `hmem rebuild` as a subprocess.
///
/// Only compiled when the `live` feature is enabled (which adds the real process-spawning
/// adapter). A non-zero exit code or process-spawn error is mapped to [`GraphError::Io`].
///
/// # Note
///
/// `hmem` must be present in `PATH` at call time. If the binary is absent,
/// [`RebuildHook::rebuild`] returns [`GraphError::Io`] with the spawn error detail.
#[cfg(feature = "live")]
#[derive(Debug, Clone)]
pub struct HmemRebuild;

#[cfg(feature = "live")]
impl HmemRebuild {
    /// Creates a new `HmemRebuild`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "live")]
impl Default for HmemRebuild {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "live")]
impl RebuildHook for HmemRebuild {
    fn rebuild(&self) -> Result<()> {
        use habitat_graph_core::GraphError;
        let status = std::process::Command::new("hmem")
            .arg("rebuild")
            .status()
            .map_err(|e| GraphError::Io(format!("failed to spawn `hmem rebuild`: {e}")))?;
        if status.success() {
            Ok(())
        } else {
            let code = status
                .code()
                .map_or_else(|| "signal".to_string(), |c| c.to_string());
            Err(GraphError::Io(format!(
                "`hmem rebuild` exited with non-zero status {code}"
            )))
        }
    }
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        render_back_to_header, render_graph_note, render_master_index_entry, NoopRebuild,
        RebuildHook,
    };

    // ── render_back_to_header ────────────────────────────────────────────────

    #[test]
    fn back_to_no_extras_exact_string() {
        let h = render_back_to_header(&[]);
        assert_eq!(h, "> Back to: [[CLAUDE.md]] \u{00B7} [[CLAUDE.local.md]]");
    }

    #[test]
    fn back_to_starts_with_blockquote_arrow() {
        let h = render_back_to_header(&[]);
        assert!(h.starts_with("> "), "must start with '> ': {h:?}");
    }

    #[test]
    fn back_to_always_contains_claude_md_wikilink() {
        assert!(render_back_to_header(&[]).contains("[[CLAUDE.md]]"));
        assert!(render_back_to_header(&["extra"]).contains("[[CLAUDE.md]]"));
    }

    #[test]
    fn back_to_always_contains_claude_local_md_wikilink() {
        assert!(render_back_to_header(&[]).contains("[[CLAUDE.local.md]]"));
        assert!(render_back_to_header(&["x"]).contains("[[CLAUDE.local.md]]"));
    }

    #[test]
    fn back_to_single_extra_appended_as_wikilink() {
        let h = render_back_to_header(&["Session"]);
        assert!(h.contains("[[Session]]"), "wikilink missing: {h}");
    }

    #[test]
    fn back_to_single_extra_adds_one_separator() {
        let h = render_back_to_header(&["Session"]);
        // Base has 1 separator; one extra adds another → total 2.
        assert_eq!(h.matches('\u{00B7}').count(), 2, "separator count wrong: {h}");
    }

    #[test]
    fn back_to_multiple_extras_all_appended() {
        let h = render_back_to_header(&["A", "B", "C"]);
        assert!(h.contains("[[A]]"), "A missing: {h}");
        assert!(h.contains("[[B]]"), "B missing: {h}");
        assert!(h.contains("[[C]]"), "C missing: {h}");
    }

    #[test]
    fn back_to_multiple_extras_order_preserved() {
        let h = render_back_to_header(&["First", "Second"]);
        let p_first = h.find("[[First]]").expect("First present");
        let p_second = h.find("[[Second]]").expect("Second present");
        assert!(p_first < p_second, "order not preserved: {h}");
    }

    #[test]
    fn back_to_rlo_in_extra_is_escaped() {
        // U+202E = RIGHT-TO-LEFT OVERRIDE (Trojan-Source).
        let h = render_back_to_header(&["\u{202E}evil"]);
        assert!(!h.contains('\u{202E}'), "raw RLO leaked: {h:?}");
        assert!(h.contains("\\u{202E}"), "escaped form absent: {h}");
    }

    #[test]
    fn back_to_zero_width_space_in_extra_is_escaped() {
        let h = render_back_to_header(&["la\u{200B}bel"]);
        assert!(!h.contains('\u{200B}'), "zero-width space leaked: {h:?}");
    }

    #[test]
    fn back_to_bom_in_extra_is_escaped() {
        let h = render_back_to_header(&["\u{FEFF}label"]);
        assert!(!h.contains('\u{FEFF}'), "BOM leaked: {h:?}");
    }

    #[test]
    fn back_to_no_extras_no_trailing_separator() {
        let h = render_back_to_header(&[]);
        assert!(!h.ends_with('\u{00B7}'), "trailing separator: {h}");
        assert!(!h.ends_with(' '), "trailing space: {h}");
    }

    #[test]
    fn back_to_extra_double_bracket_not_triple() {
        let h = render_back_to_header(&["Note"]);
        assert!(!h.contains("[[["), "triple bracket: {h}");
        assert!(h.contains("[[Note]]"), "double bracket missing: {h}");
    }

    #[test]
    fn back_to_four_extras_separator_count() {
        let h = render_back_to_header(&["A", "B", "C", "D"]);
        // 1 base separator + 4 extra separators = 5 middle-dot chars.
        assert_eq!(h.matches('\u{00B7}').count(), 5, "separator count: {h}");
    }

    #[test]
    fn back_to_empty_extra_string_still_appends_wikilink() {
        let h = render_back_to_header(&[""]);
        // An empty string produces [[]] — it's caller responsibility to pass meaningful links.
        assert!(h.contains("[[]]"), "empty wikilink missing: {h}");
    }

    #[test]
    fn back_to_lre_in_extra_escaped() {
        let h = render_back_to_header(&["\u{202A}note"]);
        assert!(!h.contains('\u{202A}'), "LRE leaked: {h:?}");
    }

    // ── render_master_index_entry ────────────────────────────────────────────

    #[test]
    fn master_index_exact_format_plain_text() {
        let e = render_master_index_entry("Arc-graph", "arc_graph.md", "severed-ear diff");
        assert_eq!(
            e,
            "- [Arc-graph](arc_graph.md) \u{2014} severed-ear diff"
        );
    }

    #[test]
    fn master_index_starts_with_bullet_space() {
        let e = render_master_index_entry("T", "f", "h");
        assert!(e.starts_with("- "), "got: {e}");
    }

    #[test]
    fn master_index_title_inside_square_brackets() {
        let e = render_master_index_entry("My Title", "f.md", "h");
        assert!(e.contains("[My Title]"), "got: {e}");
    }

    #[test]
    fn master_index_file_inside_parentheses() {
        let e = render_master_index_entry("T", "notes/file.md", "h");
        assert!(e.contains("(notes/file.md)"), "got: {e}");
    }

    #[test]
    fn master_index_hook_after_em_dash() {
        let e = render_master_index_entry("T", "f", "my hook text");
        let pos_dash = e.find('\u{2014}').expect("em dash present");
        assert!(
            e[pos_dash..].contains("my hook text"),
            "hook after em dash missing: {e}"
        );
    }

    #[test]
    fn master_index_uses_em_dash_not_ascii_hyphen() {
        let e = render_master_index_entry("T", "f", "h");
        // Must contain U+2014, not a bare ASCII hyphen between title and hook.
        assert!(e.contains('\u{2014}'), "em dash absent: {e}");
    }

    #[test]
    fn master_index_rlo_in_title_escaped() {
        let e = render_master_index_entry("Ti\u{202E}tle", "f", "h");
        assert!(!e.contains('\u{202E}'), "raw RLO in title: {e:?}");
        assert!(e.contains("\\u{202E}"), "escaped form absent: {e}");
    }

    #[test]
    fn master_index_lrm_in_file_escaped() {
        let e = render_master_index_entry("T", "fi\u{200E}le.md", "h");
        assert!(!e.contains('\u{200E}'), "LRM in file: {e:?}");
    }

    #[test]
    fn master_index_lre_in_hook_escaped() {
        let e = render_master_index_entry("T", "f", "ho\u{202A}ok");
        assert!(!e.contains('\u{202A}'), "LRE in hook: {e:?}");
    }

    #[test]
    fn master_index_empty_title_produces_empty_brackets() {
        let e = render_master_index_entry("", "file.md", "hook");
        assert!(e.contains("[](file.md)"), "got: {e}");
    }

    #[test]
    fn master_index_empty_hook_still_has_em_dash() {
        let e = render_master_index_entry("Title", "file.md", "");
        assert!(e.contains('\u{2014}'), "em dash missing: {e}");
    }

    #[test]
    fn master_index_spaces_in_title_preserved() {
        let e = render_master_index_entry("My Session Notes", "n.md", "h");
        assert!(e.contains("[My Session Notes]"), "got: {e}");
    }

    #[test]
    fn master_index_unicode_title_preserved() {
        let e = render_master_index_entry("caf\u{00E9}", "f.md", "h");
        assert!(e.contains("caf\u{00E9}"), "Unicode title lost: {e}");
    }

    // ── render_graph_note ────────────────────────────────────────────────────

    #[test]
    fn graph_note_starts_with_back_to_header() {
        let note = render_graph_note("Graph", (10, 20, 5), &[]);
        let back_to = render_back_to_header(&[]);
        assert!(
            note.starts_with(&back_to),
            "does not start with back-to header: {note}"
        );
    }

    #[test]
    fn graph_note_first_line_contains_both_wikilinks() {
        let note = render_graph_note("Graph", (1, 2, 3), &[]);
        let first = note.lines().next().expect("at least one line");
        assert!(first.contains("[[CLAUDE.md]]"), "first line: {first}");
        assert!(first.contains("[[CLAUDE.local.md]]"), "first line: {first}");
    }

    #[test]
    fn graph_note_contains_stats_heading() {
        let note = render_graph_note("G", (0, 0, 0), &[]);
        assert!(note.contains("## Stats"), "Stats heading absent: {note}");
    }

    #[test]
    fn graph_note_node_count_present() {
        let note = render_graph_note("G", (42, 0, 0), &[]);
        assert!(
            note.contains("nodes: 42"),
            "node count missing: {note}"
        );
    }

    #[test]
    fn graph_note_edge_count_present() {
        let note = render_graph_note("G", (0, 99, 0), &[]);
        assert!(
            note.contains("edges: 99"),
            "edge count missing: {note}"
        );
    }

    #[test]
    fn graph_note_community_count_present() {
        let note = render_graph_note("G", (0, 0, 7), &[]);
        assert!(
            note.contains("communities: 7"),
            "community count missing: {note}"
        );
    }

    #[test]
    fn graph_note_contains_top_hubs_heading() {
        let note = render_graph_note("G", (1, 2, 3), &[("hub", 5)]);
        assert!(
            note.contains("## Top hubs"),
            "Top hubs heading absent: {note}"
        );
    }

    #[test]
    fn graph_note_empty_hubs_still_has_top_hubs_heading() {
        let note = render_graph_note("G", (0, 0, 0), &[]);
        assert!(
            note.contains("## Top hubs"),
            "Top hubs heading absent when hubs empty: {note}"
        );
    }

    #[test]
    fn graph_note_hub_rendered_as_wikilink() {
        let note = render_graph_note("G", (1, 2, 3), &[("MyHub", 10)]);
        assert!(note.contains("[[MyHub]]"), "wikilink absent: {note}");
    }

    #[test]
    fn graph_note_hub_degree_in_parens() {
        let note = render_graph_note("G", (1, 2, 3), &[("Hub", 42)]);
        assert!(note.contains("(42)"), "degree in parens absent: {note}");
    }

    #[test]
    fn graph_note_hub_full_format() {
        let note = render_graph_note("G", (1, 2, 3), &[("Hub", 7)]);
        assert!(
            note.contains("[[Hub]] (7)"),
            "hub format wrong: {note}"
        );
    }

    #[test]
    fn graph_note_multiple_hubs_all_present() {
        let hubs = [("Alpha", 10_usize), ("Beta", 5), ("Gamma", 3)];
        let note = render_graph_note("G", (3, 5, 2), &hubs);
        assert!(note.contains("[[Alpha]]"), "Alpha absent: {note}");
        assert!(note.contains("[[Beta]]"), "Beta absent: {note}");
        assert!(note.contains("[[Gamma]]"), "Gamma absent: {note}");
    }

    #[test]
    fn graph_note_hub_order_preserved() {
        let hubs = [("First", 10_usize), ("Second", 5)];
        let note = render_graph_note("G", (1, 2, 3), &hubs);
        let p_first = note.find("[[First]]").expect("First");
        let p_second = note.find("[[Second]]").expect("Second");
        assert!(p_first < p_second, "hub order not preserved: {note}");
    }

    #[test]
    fn graph_note_rlo_in_title_escaped() {
        let note = render_graph_note("Ti\u{202E}tle", (1, 2, 3), &[]);
        assert!(!note.contains('\u{202E}'), "raw RLO in title: {note:?}");
        assert!(note.contains("\\u{202E}"), "escaped form absent: {note}");
    }

    #[test]
    fn graph_note_rlo_in_hub_label_escaped() {
        let note = render_graph_note("G", (1, 2, 3), &[("ba\u{202E}d", 1)]);
        assert!(!note.contains('\u{202E}'), "raw RLO in hub: {note:?}");
        assert!(note.contains("\\u{202E}"), "escaped hub absent: {note}");
    }

    #[test]
    fn graph_note_escaped_hub_still_inside_wikilink_brackets() {
        let note = render_graph_note("G", (1, 2, 3), &[("\u{202E}evil", 1)]);
        assert!(
            note.contains("[[\\u{202E}evil]]"),
            "wikilink around escaped char: {note}"
        );
    }

    #[test]
    fn graph_note_zero_counts_formatted_correctly() {
        let note = render_graph_note("Empty", (0, 0, 0), &[]);
        assert!(note.contains("nodes: 0"), "nodes 0: {note}");
        assert!(note.contains("edges: 0"), "edges 0: {note}");
        assert!(note.contains("communities: 0"), "communities 0: {note}");
    }

    #[test]
    fn graph_note_title_in_h1_heading() {
        let note = render_graph_note("My Graph", (1, 2, 3), &[]);
        assert!(note.contains("# My Graph"), "H1 heading absent: {note}");
    }

    #[test]
    fn graph_note_stats_heading_before_top_hubs() {
        let note = render_graph_note("G", (1, 2, 3), &[("H", 1)]);
        let p_stats = note.find("## Stats").expect("Stats");
        let p_hubs = note.find("## Top hubs").expect("Top hubs");
        assert!(p_stats < p_hubs, "Stats must precede Top hubs: {note}");
    }

    #[test]
    fn graph_note_back_to_before_stats() {
        let note = render_graph_note("G", (1, 2, 3), &[]);
        let p_back = note.find("Back to").expect("Back to");
        let p_stats = note.find("## Stats").expect("Stats");
        assert!(p_back < p_stats, "Back-to must precede Stats: {note}");
    }

    #[test]
    fn graph_note_never_empty() {
        let note = render_graph_note("", (0, 0, 0), &[]);
        assert!(!note.is_empty());
    }

    #[test]
    fn graph_note_hub_bullet_prefix() {
        let note = render_graph_note("G", (1, 2, 3), &[("H", 1)]);
        assert!(note.contains("\n- [[H]]"), "bullet prefix wrong: {note}");
    }

    // ── NoopRebuild ─────────────────────────────────────────────────────────

    #[test]
    fn noop_rebuild_returns_ok() {
        let hook = NoopRebuild::new();
        assert!(hook.rebuild().is_ok());
    }

    #[test]
    fn noop_rebuild_idempotent_multiple_calls() {
        let hook = NoopRebuild::new();
        for _ in 0..10 {
            assert!(hook.rebuild().is_ok());
        }
    }

    #[test]
    fn noop_rebuild_via_trait_object() {
        let hook: &dyn RebuildHook = &NoopRebuild::new();
        assert!(hook.rebuild().is_ok());
    }

    #[test]
    fn noop_rebuild_default_is_ok() {
        // Construct directly (unit struct — no Default call needed) to satisfy
        // clippy::default_constructed_unit_structs while still exercising the code path.
        let hook = NoopRebuild;
        assert!(hook.rebuild().is_ok());
    }

    #[test]
    fn noop_rebuild_clone_is_ok() {
        let a = NoopRebuild::new();
        let b = a.clone();
        assert!(b.rebuild().is_ok());
    }

    #[test]
    fn noop_rebuild_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NoopRebuild>();
    }

    #[test]
    fn noop_rebuild_boxed_trait_object() {
        let hook: Box<dyn RebuildHook> = Box::new(NoopRebuild::new());
        assert!(hook.rebuild().is_ok());
    }

    // ── HmemRebuild — live adapter (feature = "live") ────────────────────────
    // Tests verify construction and the pure config surface of the `hmem rebuild`
    // subprocess hook.  No subprocess is spawned; no binary must be on PATH.

    /// `HmemRebuild::new()` must construct without panicking.
    #[cfg(feature = "live")]
    #[test]
    fn hmem_rebuild_new_constructs_ok() {
        let _ = super::HmemRebuild::new();
    }

    /// `HmemRebuild::default()` and `HmemRebuild::new()` must produce equal instances.
    #[cfg(feature = "live")]
    #[test]
    fn hmem_rebuild_default_same_as_new() {
        let via_new = super::HmemRebuild::new();
        // Direct construction (unit struct — avoids clippy::default_constructed_unit_structs).
        let via_default = super::HmemRebuild;
        // Both are unit structs — their Debug representations must be identical.
        assert_eq!(
            format!("{via_new:?}"),
            format!("{via_default:?}"),
            "new() and direct construction must produce identical HmemRebuild instances"
        );
    }

    /// `HmemRebuild` must be `Clone`.
    #[cfg(feature = "live")]
    #[test]
    fn hmem_rebuild_is_clone() {
        let original = super::HmemRebuild::new();
        let cloned = original.clone();
        assert_eq!(
            format!("{original:?}"),
            format!("{cloned:?}"),
            "clone must produce an equal HmemRebuild"
        );
    }

    /// `HmemRebuild` must be `Send + Sync` — it may be shared across threads.
    #[cfg(feature = "live")]
    #[test]
    fn hmem_rebuild_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<super::HmemRebuild>();
    }

    /// `HmemRebuild` must satisfy the `RebuildHook` trait bound — verifiable at compile time.
    #[cfg(feature = "live")]
    #[test]
    fn hmem_rebuild_satisfies_rebuild_hook_trait_bound() {
        fn takes_rebuild_hook<R: RebuildHook>(_: &R) {}
        takes_rebuild_hook(&super::HmemRebuild::new());
    }
}
