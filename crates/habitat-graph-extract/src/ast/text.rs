//! Documentation / plain-text extractor (internal, no tree-sitter) — graphify doc-node taxonomy.
//!
//! Emits graphify's file-node taxonomy for `.md`, `.markdown`, `.txt`, and `.rst` files using
//! hand-rolled line parsing (no grammar, no FFI). The taxonomy is provisional (golden corpus
//! C-G1 deferred) and will be reconciled when the doc golden lands.
//!
//! For **Markdown** (`.md` / `.markdown`) files:
//!
//! - File node `B` (file stem, lowercased), span = whole document — always emitted.
//! - Each ATX heading (`# Title` … `###### Title`) outside a fenced code block → heading node
//!   `B_<slug>` + `contains(B → B_<slug>)` edge. `slug` is the heading text lowercased, with
//!   runs of non-alphanumeric characters collapsed to a single `_`, and then trimmed of any
//!   leading or trailing `_`.
//! - Each inline Markdown link `[text](target)` where `target` is a relative path ending
//!   `.md` or `.markdown` → `references(B → <target-stem>)` edge. External (`http://`,
//!   `https://`) and anchor-only (`#…`) targets are silently skipped.
//!
//! For **`.txt`** and **`.rst`** files: file node only (heading/link parsing deferred to PA-2).
//!
//! `calls` / `uses` edges are **never** emitted. The `references` relation is provisional and
//! documented as doc-specific (the classifier ignores it for now).

use std::path::Path;

use habitat_graph_core::{Confidence, Extraction, RawEdge, RawNode, Result, Span};

use crate::ast::util::stem_lower;
use crate::registry::Extractor;

/// Extracts doc nodes/edges from Markdown and plain-text files without a tree-sitter grammar.
///
/// For `.md` / `.markdown` files: emits a file node `B`, one heading node `B_<slug>` per ATX
/// heading that appears outside a fenced code block, and `contains` / `references` edges.
/// For `.txt` / `.rst` files: emits only the file node.
///
/// The taxonomy is provisional (`C-G1` deferred); the graphify doc golden will reconcile it
/// when available.
#[derive(Debug, Default, Clone, Copy)]
pub struct TextExtractor;

// ── Private helpers ─────────────────────────────────────────────────────────────────────────────

/// Returns `true` when `ext` identifies a Markdown variant handled by this module.
fn is_markdown_ext(ext: &str) -> bool {
    ext == "md" || ext == "markdown"
}

/// Slugifies heading text: lowercase, collapse non-alphanumeric runs to `_`, trim leading/trailing `_`.
///
/// Returns an empty string if `text` contains no alphanumeric characters, signalling the caller
/// to skip node emission.
fn make_slug(text: &str) -> String {
    let lower = text.to_lowercase();
    let mut slug = String::with_capacity(lower.len());
    // Initialise to `true` so leading non-alphanumeric characters produce no leading `_`.
    let mut last_was_sep = true;
    for ch in lower.chars() {
        if ch.is_alphanumeric() {
            slug.push(ch);
            last_was_sep = false;
        } else if !last_was_sep {
            slug.push('_');
            last_was_sep = true;
        }
    }
    slug.trim_end_matches('_').to_owned()
}

/// Strips an optional trailing ATX closing-hash sequence from heading content.
///
/// A closing sequence is valid only when preceded by whitespace. For example, `"Hello ##"` →
/// `"Hello"`, but `"Hello##"` (no preceding space) is returned unchanged. An all-hash string
/// like `"###"` is returned unchanged (treated as content, not a closing sequence).
fn strip_trailing_hashes(text: &str) -> &str {
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return trimmed;
    }
    // Compute the byte offset one-past the last non-'#' character (= start of the trailing
    // '#' run). Using `char_indices().rev()` avoids the byte-boundary hazard that `rfind`
    // alone would cause for multibyte characters.
    let trailing_start = trimmed
        .char_indices()
        .rev()
        .find(|(_, c)| *c != '#')
        .map(|(byte_pos, ch)| byte_pos + ch.len_utf8());

    let Some(hashes_start) = trailing_start else {
        // All characters are '#' — not a closing sequence; return as-is.
        return trimmed;
    };

    if hashes_start < trimmed.len() {
        // Characters from `hashes_start` onward are all '#'.
        let before_hashes = &trimmed[..hashes_start];
        if before_hashes.ends_with([' ', '\t']) {
            // Valid closing sequence preceded by whitespace — strip it.
            return before_hashes.trim_end();
        }
    }
    trimmed
}

/// Parses a single line as an ATX heading and returns the stripped heading text.
///
/// Returns `None` when `line` is not a valid ATX heading: more than six `#` characters, or no
/// space/tab between the `#` sequence and the heading content (e.g. `"#NoSpace"`).
fn parse_atx_heading(line: &str) -> Option<String> {
    let hash_count = line.chars().take_while(|&c| c == '#').count();
    if hash_count == 0 || hash_count > 6 {
        return None;
    }
    let rest = &line[hash_count..];
    if rest.is_empty() {
        // e.g. `######` with nothing following — empty heading text.
        return Some(String::new());
    }
    let first = rest.chars().next()?;
    if first != ' ' && first != '\t' {
        // `#Foo` without a separator — not a valid ATX heading.
        return None;
    }
    let content = &rest[first.len_utf8()..];
    let stripped = strip_trailing_hashes(content);
    Some(stripped.trim_end().to_owned())
}

/// Returns the fenced-code-block character (`` ` `` or `~`) if `line` is a fence marker.
///
/// A fence marker is three or more consecutive backticks or tildes at the start of a line,
/// preceded by at most three spaces (`CommonMark` rule). Returns `None` for non-fence lines.
fn detect_fence_marker(line: &str) -> Option<char> {
    let trimmed = line.trim_start_matches(' ');
    let leading = line.len() - trimmed.len();
    if leading > 3 {
        return None;
    }
    if trimmed.starts_with("```") {
        Some('`')
    } else if trimmed.starts_with("~~~") {
        Some('~')
    } else {
        None
    }
}

/// Parses a Markdown inline link starting at the beginning of `s` (which must begin with `[`).
///
/// Returns `(bytes_consumed, url_string)` on success, or `None` when `s` does not form a valid
/// `[text](url)` pattern.
fn parse_link_at(s: &str) -> Option<(usize, String)> {
    let close_bracket = s.find(']')?;
    let after_bracket = close_bracket + 1;
    if s.as_bytes().get(after_bracket) != Some(&b'(') {
        return None;
    }
    let url_start = after_bracket + 1;
    let remaining = &s[url_start..];
    let close_paren = remaining.find(')')?;
    let url = remaining[..close_paren].to_owned();
    Some((url_start + close_paren + 1, url))
}

/// Returns `true` when `url` is a relative Markdown path that warrants a `references` edge.
///
/// Specifically: the URL must not start with `http://`, `https://`, or `#`; and after stripping
/// an optional title attribute, fragment, and query string, it must end with `.md` or `.markdown`.
fn is_relative_md_link(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") || trimmed.starts_with('#')
    {
        return false;
    }
    // Strip optional title attribute (space-separated after the URL).
    let without_title = trimmed.split(' ').next().unwrap_or(trimmed);
    // Strip fragment and query string.
    let without_frag = without_title.split('#').next().unwrap_or(without_title);
    let clean = without_frag.split('?').next().unwrap_or(without_frag);
    Path::new(clean)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

/// Extracts the lowercased file stem from a Markdown link target (relative path).
fn md_link_stem(url: &str) -> String {
    let trimmed = url.trim();
    let without_title = trimmed.split(' ').next().unwrap_or(trimmed);
    let without_frag = without_title.split('#').next().unwrap_or(without_title);
    let clean = without_frag.split('?').next().unwrap_or(without_frag);
    stem_lower(Path::new(clean))
}

/// Scans `line` for inline Markdown links and appends `references` edges for relative `.md` /
/// `.markdown` targets.
fn extract_md_links(line: &str, b: &str, result: &mut Extraction) {
    let mut start = 0_usize;
    while start < line.len() {
        // Advance to the next `[` at or after `start`.
        let sub = &line[start..];
        let Some(offset) = sub.find('[') else { break };
        let link_start = start + offset;

        match parse_link_at(&line[link_start..]) {
            Some((consumed, ref url)) => {
                if is_relative_md_link(url) {
                    let stem = md_link_stem(url);
                    if !stem.is_empty() {
                        result.edges.push(RawEdge {
                            source: b.to_owned(),
                            target: stem,
                            relation: "references".to_owned(),
                            confidence: Confidence::Extracted,
                        });
                    }
                }
                // Advance past the consumed link; guard against zero-progress.
                start = link_start + consumed.max(1);
            }
            None => {
                // Not a valid link; advance past the `[` and keep scanning.
                start = link_start + 1;
            }
        }
    }
}

/// Parses Markdown ATX headings and inline links from `source`, appending nodes and edges to `result`.
fn parse_markdown(source: &[u8], b: &str, source_file: &str, result: &mut Extraction) {
    let text = String::from_utf8_lossy(source);
    // `None` = not in a fence; `Some(c)` = inside a fence opened by character `c`.
    let mut fence: Option<char> = None;
    let mut byte_offset: u32 = 0;
    let mut line_no: u32 = 0;

    for raw_line in text.split('\n') {
        line_no = line_no.saturating_add(1);
        // `raw_line.len()` is the UTF-8 byte count of the line *excluding* the `\n` separator.
        let line_bytes = u32::try_from(raw_line.len()).unwrap_or(u32::MAX);
        // Strip trailing `\r` for CRLF line endings before processing.
        let line = raw_line.trim_end_matches('\r');

        if let Some(fence_char) = detect_fence_marker(line) {
            match fence {
                None => fence = Some(fence_char),
                Some(c) if c == fence_char => fence = None,
                // Different fence character inside an open fence — ignore (e.g. `~~~` inside `` ``` ``).
                Some(_) => {}
            }
            byte_offset = byte_offset.saturating_add(line_bytes).saturating_add(1);
            continue;
        }

        if fence.is_none() {
            if let Some(heading_text) = parse_atx_heading(line) {
                let slug = make_slug(&heading_text);
                if !slug.is_empty() {
                    let label = format!("{b}_{slug}");
                    let span = Span::new(
                        byte_offset,
                        byte_offset.saturating_add(line_bytes),
                        line_no,
                        line_no,
                    );
                    result.nodes.push(RawNode {
                        label: label.clone(),
                        source_file: source_file.to_owned(),
                        span,
                    });
                    result.edges.push(RawEdge {
                        source: b.to_owned(),
                        target: label,
                        relation: "contains".to_owned(),
                        confidence: Confidence::Extracted,
                    });
                }
            }
            extract_md_links(line, b, result);
        }

        byte_offset = byte_offset.saturating_add(line_bytes).saturating_add(1);
    }
}

// ── Extractor impl ─────────────────────────────────────────────────────────────────────────────

impl Extractor for TextExtractor {
    fn language(&self) -> &'static str {
        "text"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["md", "markdown", "txt", "rst"]
    }

    /// Extracts doc nodes/edges from the bytes at `path`.
    ///
    /// For `.md` / `.markdown`: emits a file node `B` plus heading nodes and `contains` /
    /// `references` edges. For `.txt` / `.rst`: emits only the file node. An empty file produces
    /// exactly one node (the file node) and no edges.
    ///
    /// # Errors
    ///
    /// Infallible in practice — this extractor performs no I/O and uses no external parser.
    /// The [`Result`] return type satisfies the [`Extractor`] trait contract.
    fn extract(&self, path: &Path, source: &[u8]) -> Result<Extraction> {
        let source_file = path.to_string_lossy().into_owned();
        let b = stem_lower(path);

        let len = u32::try_from(source.len()).unwrap_or(u32::MAX);
        // `split` on `b'\n'` always yields at least one segment (even for empty input),
        // so `line_count >= 1`.
        let line_count =
            u32::try_from(source.split(|&c: &u8| c == b'\n').count()).unwrap_or(u32::MAX);

        let mut result = Extraction::new();
        result.nodes.push(RawNode {
            label: b.clone(),
            source_file: source_file.clone(),
            span: Span::new(0, len, 1, line_count),
        });

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if is_markdown_ext(&ext) {
            parse_markdown(source, &b, &source_file, &mut result);
        }

        Ok(result)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::Path;

    use habitat_graph_core::{Confidence, Extraction};

    use super::TextExtractor;
    use crate::registry::Extractor;

    // ── Helpers ────────────────────────────────────────────────────────────────────────────────

    /// Run the extractor on `src` as if it came from `filename`; panic on extractor error.
    fn extract(src: &str, filename: &str) -> Extraction {
        TextExtractor
            .extract(Path::new(filename), src.as_bytes())
            .unwrap_or_else(|e| panic!("extractor failed on {filename}: {e}"))
    }

    /// Returns `true` if `ex` contains a node with the given label.
    fn has_node(ex: &Extraction, label: &str) -> bool {
        ex.nodes.iter().any(|n| n.label == label)
    }

    /// Returns `true` if `ex` contains an edge with matching source, target, and relation.
    fn has_edge(ex: &Extraction, src: &str, tgt: &str, rel: &str) -> bool {
        ex.edges
            .iter()
            .any(|e| e.source == src && e.target == tgt && e.relation == rel)
    }

    /// Returns the count of edges with the given relation.
    fn count_relation(ex: &Extraction, rel: &str) -> usize {
        ex.edges.iter().filter(|e| e.relation == rel).count()
    }

    /// Returns the node with the given label, panicking if absent.
    fn get_node<'e>(ex: &'e Extraction, label: &str) -> &'e habitat_graph_core::RawNode {
        ex.nodes
            .iter()
            .find(|n| n.label == label)
            .unwrap_or_else(|| {
                panic!(
                    "node '{label}' not found; got {:?}",
                    ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
                )
            })
    }

    // ── Group 1: File node basics ──────────────────────────────────────────────────────────────

    #[test]
    fn empty_md_yields_exactly_one_file_node_and_no_edges() {
        let ex = extract("", "readme.md");
        assert_eq!(ex.nodes.len(), 1, "empty md: expected exactly 1 node");
        assert_eq!(ex.nodes[0].label, "readme");
        assert_eq!(ex.edges.len(), 0, "empty md: expected no edges");
    }

    #[test]
    fn empty_txt_yields_file_node_only() {
        let ex = extract("", "notes.txt");
        assert_eq!(ex.nodes.len(), 1);
        assert_eq!(ex.nodes[0].label, "notes");
        assert_eq!(ex.edges.len(), 0);
    }

    #[test]
    fn empty_rst_yields_file_node_only() {
        let ex = extract("", "manual.rst");
        assert_eq!(ex.nodes.len(), 1);
        assert_eq!(ex.nodes[0].label, "manual");
        assert_eq!(ex.edges.len(), 0);
    }

    #[test]
    fn file_stem_lowercased_in_md_label() {
        let ex = extract("", "README.md");
        assert_eq!(ex.nodes[0].label, "readme");
    }

    #[test]
    fn file_stem_lowercased_in_txt_label() {
        let ex = extract("", "NOTES.txt");
        assert_eq!(ex.nodes[0].label, "notes");
    }

    #[test]
    fn mixed_case_stem_fully_lowercased() {
        let ex = extract("", "MyDocument.md");
        assert_eq!(ex.nodes[0].label, "mydocument");
    }

    #[test]
    fn dot_markdown_extension_processed_as_markdown() {
        // `.markdown` extension must behave identically to `.md`.
        let ex = extract("# Hello\n", "guide.markdown");
        assert!(has_node(&ex, "guide"), "file node missing");
        assert!(
            has_node(&ex, "guide_hello"),
            "heading node missing for .markdown extension"
        );
    }

    // ── Group 2: ATX heading levels ───────────────────────────────────────────────────────────

    #[test]
    fn h1_heading_emits_heading_node_and_contains_edge() {
        let ex = extract("# Title\n", "doc.md");
        assert!(has_node(&ex, "doc_title"), "h1 heading node missing");
        assert!(
            has_edge(&ex, "doc", "doc_title", "contains"),
            "contains edge for h1 missing"
        );
    }

    #[test]
    fn h2_heading_emits_node() {
        let ex = extract("## Section\n", "doc.md");
        assert!(has_node(&ex, "doc_section"), "h2 heading node missing");
    }

    #[test]
    fn h3_heading_emits_node() {
        let ex = extract("### Subsection\n", "doc.md");
        assert!(has_node(&ex, "doc_subsection"), "h3 heading node missing");
    }

    #[test]
    fn h4_heading_emits_node() {
        let ex = extract("#### Detail\n", "doc.md");
        assert!(has_node(&ex, "doc_detail"), "h4 heading node missing");
    }

    #[test]
    fn h5_heading_emits_node() {
        let ex = extract("##### Fine\n", "doc.md");
        assert!(has_node(&ex, "doc_fine"), "h5 heading node missing");
    }

    #[test]
    fn h6_heading_emits_node() {
        let ex = extract("###### Micro\n", "doc.md");
        assert!(has_node(&ex, "doc_micro"), "h6 heading node missing");
    }

    #[test]
    fn seven_hashes_not_parsed_as_heading() {
        // ATX headings are only valid for 1–6 `#` characters.
        let ex = extract("####### NotAHeading\n", "doc.md");
        assert_eq!(
            ex.nodes.len(),
            1,
            "7 hashes must not produce a heading node"
        );
        assert_eq!(ex.edges.len(), 0);
    }

    #[test]
    fn hash_without_space_not_parsed_as_heading() {
        // `#NoSpace` lacks the required space after `#` — not an ATX heading.
        let ex = extract("#NoSpace\n", "doc.md");
        assert_eq!(
            ex.nodes.len(),
            1,
            "#NoSpace must not produce a heading node"
        );
    }

    // ── Group 3: Slug computation ──────────────────────────────────────────────────────────────

    #[test]
    fn heading_slug_spaces_collapsed_to_single_underscore() {
        let ex = extract("# Hello World\n", "x.md");
        assert!(
            has_node(&ex, "x_hello_world"),
            "spaces must collapse to single '_'"
        );
    }

    #[test]
    fn heading_slug_punctuation_becomes_underscore() {
        let ex = extract("# Hello, World!\n", "x.md");
        // "Hello, World!" → lowercase → "hello, world!" → collapse non-alnum → "hello_world_"
        // → trim trailing '_' → "hello_world"
        assert!(
            has_node(&ex, "x_hello_world"),
            "punctuation must collapse to '_' and trailing '_' be trimmed"
        );
    }

    #[test]
    fn heading_text_mixed_case_lowercased_in_slug() {
        let ex = extract("## APIReference\n", "x.md");
        assert!(
            has_node(&ex, "x_apireference"),
            "heading text must be lowercased in slug"
        );
    }

    #[test]
    fn heading_slug_leading_special_chars_trimmed() {
        // If heading text starts with non-alnum, the slug must not have a leading '_'.
        let ex = extract("# !Intro\n", "x.md");
        // "!intro" → slug = "intro" (leading '!' trimmed because `last_was_sep` starts `true`)
        assert!(
            has_node(&ex, "x_intro"),
            "leading non-alnum must not produce leading '_' in slug"
        );
    }

    #[test]
    fn heading_slug_multiple_spaces_collapsed_to_single_underscore() {
        let ex = extract("#  Two  Spaces \n", "x.md");
        // "  Two  Spaces " → "two  spaces " → "two_spaces" (runs collapsed, trailing trimmed)
        assert!(
            has_node(&ex, "x_two_spaces"),
            "multiple spaces must collapse to single '_'"
        );
    }

    #[test]
    fn heading_all_nonalnum_text_produces_empty_slug_no_node() {
        // A heading whose text is entirely non-alphanumeric yields an empty slug → no node.
        let ex = extract("# --- \n", "x.md");
        assert_eq!(
            ex.nodes.len(),
            1,
            "all-non-alnum heading must not emit a heading node"
        );
    }

    #[test]
    fn heading_trailing_double_hash_stripped() {
        // `## Title ##` — the trailing `##` (preceded by space) is a valid ATX closing sequence.
        let ex = extract("## Title ##\n", "x.md");
        assert!(
            has_node(&ex, "x_title"),
            "trailing ## must be stripped; node 'x_title' missing"
        );
        // Must NOT emit "x_title_" (with underscore) — the slug is clean.
        assert!(
            !has_node(&ex, "x_title_"),
            "slug must not have trailing underscore"
        );
    }

    #[test]
    fn heading_trailing_hashes_not_preceded_by_space_retained_in_slug() {
        // `## X#` — the `#` is NOT preceded by a space → not a closing sequence.
        // Slug of "X#" → lowercase "x#" → 'x' alnum, '#' non-alnum → "x_" → trim → "x".
        // So the node is still "x_x" — the '#' gets collapsed into the slug.
        let ex = extract("## X#\n", "x.md");
        // The '##' is not a closing sequence (not preceded by space); slug = "x".
        assert!(
            has_node(&ex, "x_x"),
            "non-closing trailing hash becomes part of slug; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_headings_all_emitted() {
        let src = "# A\n## B\n### C\n";
        let ex = extract(src, "doc.md");
        assert!(has_node(&ex, "doc_a"), "heading A missing");
        assert!(has_node(&ex, "doc_b"), "heading B missing");
        assert!(has_node(&ex, "doc_c"), "heading C missing");
        // 1 file node + 3 heading nodes = 4 nodes; 3 contains edges.
        assert_eq!(ex.nodes.len(), 4, "expected 4 nodes total");
        assert_eq!(
            count_relation(&ex, "contains"),
            3,
            "expected 3 contains edges"
        );
    }

    // ── Group 4: Fenced code blocks ────────────────────────────────────────────────────────────

    #[test]
    fn heading_inside_backtick_fence_not_parsed() {
        let src = "```\n# Not a heading\n```\n";
        let ex = extract(src, "doc.md");
        assert_eq!(
            ex.nodes.len(),
            1,
            "heading inside ``` fence must not be emitted"
        );
        assert_eq!(ex.edges.len(), 0);
    }

    #[test]
    fn heading_outside_fence_parsed_heading_inside_not() {
        let src = "# Real Heading\n```\n# Inside Fence\n```\n";
        let ex = extract(src, "doc.md");
        assert!(
            has_node(&ex, "doc_real_heading"),
            "real heading must be parsed"
        );
        assert!(
            !has_node(&ex, "doc_inside_fence"),
            "heading inside fence must NOT be parsed"
        );
        assert_eq!(ex.nodes.len(), 2, "only file + 1 heading node");
    }

    #[test]
    fn heading_inside_tilde_fence_not_parsed() {
        let src = "~~~\n# Not a heading\n~~~\n";
        let ex = extract(src, "doc.md");
        assert_eq!(
            ex.nodes.len(),
            1,
            "heading inside ~~~ fence must not be emitted"
        );
    }

    #[test]
    fn two_fences_with_heading_between_them_parsed() {
        let src = "```\n# skip\n```\n# Visible\n```\n# skip2\n```\n";
        let ex = extract(src, "doc.md");
        assert!(
            has_node(&ex, "doc_visible"),
            "heading between fences must be parsed"
        );
        assert!(
            !has_node(&ex, "doc_skip"),
            "heading in first fence must be skipped"
        );
        assert!(
            !has_node(&ex, "doc_skip2"),
            "heading in second fence must be skipped"
        );
    }

    #[test]
    fn tilde_fence_not_closed_by_backtick_fence_marker() {
        // A ``` line inside a ~~~ fence must NOT close it.
        let src = "~~~\n# skip\n```\n# still inside\n~~~\n# visible\n";
        let ex = extract(src, "doc.md");
        assert!(
            has_node(&ex, "doc_visible"),
            "heading after closed ~~~ fence must be parsed"
        );
        assert!(
            !has_node(&ex, "doc_skip"),
            "heading inside ~~~ fence must be skipped"
        );
        assert!(
            !has_node(&ex, "doc_still_inside"),
            "``` does not close ~~~ fence"
        );
    }

    #[test]
    fn link_inside_fenced_block_not_emitted() {
        let src = "```\n[guide](guide.md)\n```\n";
        let ex = extract(src, "doc.md");
        assert_eq!(
            ex.edges.len(),
            0,
            "link inside fence must not emit a references edge"
        );
    }

    // ── Group 5: Inline links ──────────────────────────────────────────────────────────────────

    #[test]
    fn relative_md_link_emits_references_edge() {
        let ex = extract("[See guide](guide.md)\n", "readme.md");
        assert!(
            has_edge(&ex, "readme", "guide", "references"),
            "relative .md link must emit references edge; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn relative_md_link_stem_correctly_extracted() {
        let ex = extract("[link](api_reference.md)\n", "readme.md");
        assert!(
            has_edge(&ex, "readme", "api_reference", "references"),
            "stem must be extracted from filename without extension"
        );
    }

    #[test]
    fn http_link_skipped() {
        let ex = extract("[External](http://example.com)\n", "readme.md");
        assert_eq!(ex.edges.len(), 0, "http:// link must be skipped");
    }

    #[test]
    fn https_link_skipped() {
        let ex = extract("[Secure](https://example.com/page.md)\n", "readme.md");
        // Even though the URL ends with `.md`, it's an external link → skip.
        assert_eq!(
            ex.edges.len(),
            0,
            "https:// link must be skipped even if ends with .md"
        );
    }

    #[test]
    fn anchor_only_link_skipped() {
        let ex = extract("[Jump](#section)\n", "readme.md");
        assert_eq!(ex.edges.len(), 0, "#anchor link must be skipped");
    }

    #[test]
    fn non_md_extension_link_skipped() {
        let ex = extract("[Script](script.sh)\n", "readme.md");
        assert_eq!(ex.edges.len(), 0, "non-.md link must be skipped");
    }

    #[test]
    fn markdown_extension_link_emits_edge() {
        let ex = extract("[guide](guide.markdown)\n", "readme.md");
        assert!(
            has_edge(&ex, "readme", "guide", "references"),
            ".markdown extension link must emit references edge"
        );
    }

    #[test]
    fn multiple_links_on_one_line_all_emitted() {
        let ex = extract("[A](a.md) and [B](b.md)\n", "readme.md");
        assert!(has_edge(&ex, "readme", "a", "references"), "link A missing");
        assert!(has_edge(&ex, "readme", "b", "references"), "link B missing");
        assert_eq!(
            count_relation(&ex, "references"),
            2,
            "expected exactly 2 references edges"
        );
    }

    #[test]
    fn link_with_directory_path_stem_extracted() {
        let ex = extract("[ref](docs/reference.md)\n", "readme.md");
        assert!(
            has_edge(&ex, "readme", "reference", "references"),
            "stem from directory path must be extracted"
        );
    }

    #[test]
    fn link_with_dot_slash_prefix_stem_extracted() {
        let ex = extract("[guide](./guide.md)\n", "readme.md");
        assert!(
            has_edge(&ex, "readme", "guide", "references"),
            "./guide.md stem must be 'guide'"
        );
    }

    #[test]
    fn link_target_stem_is_lowercased() {
        let ex = extract("[X](Guide.md)\n", "readme.md");
        assert!(
            has_edge(&ex, "readme", "guide", "references"),
            "link target stem must be lowercased"
        );
    }

    #[test]
    fn empty_url_link_not_emitted() {
        let ex = extract("[empty]()\n", "readme.md");
        assert_eq!(ex.edges.len(), 0, "empty URL must not emit an edge");
    }

    // ── Group 6: Plain text / rst ─────────────────────────────────────────────────────────────

    #[test]
    fn txt_file_heading_syntax_not_parsed() {
        // `.txt` files must only produce the file node, even if the source looks like Markdown.
        let ex = extract("# Heading\n[link](other.md)\n", "notes.txt");
        assert_eq!(ex.nodes.len(), 1, "txt: only file node expected");
        assert_eq!(ex.edges.len(), 0, "txt: no edges expected");
    }

    #[test]
    fn rst_file_heading_syntax_not_parsed() {
        let ex = extract("# Heading\n[link](other.md)\n", "manual.rst");
        assert_eq!(ex.nodes.len(), 1, "rst: only file node expected");
        assert_eq!(ex.edges.len(), 0, "rst: no edges expected");
    }

    // ── Group 7: Edge confidence and taxonomy invariants ──────────────────────────────────────

    #[test]
    fn all_contains_edges_have_extracted_confidence() {
        let ex = extract("# A\n## B\n", "doc.md");
        for edge in ex.edges.iter().filter(|e| e.relation == "contains") {
            assert_eq!(
                edge.confidence,
                Confidence::Extracted,
                "contains edge must have Extracted confidence: {edge:?}"
            );
        }
    }

    #[test]
    fn all_references_edges_have_extracted_confidence() {
        let ex = extract("[A](a.md)\n", "doc.md");
        for edge in ex.edges.iter().filter(|e| e.relation == "references") {
            assert_eq!(
                edge.confidence,
                Confidence::Extracted,
                "references edge must have Extracted confidence: {edge:?}"
            );
        }
    }

    #[test]
    fn no_calls_or_uses_edges_ever_emitted() {
        let ex = extract("# A\n[b](b.md)\n", "doc.md");
        for edge in &ex.edges {
            assert_ne!(edge.relation, "calls", "calls edge must never be emitted");
            assert_ne!(edge.relation, "uses", "uses edge must never be emitted");
        }
    }

    #[test]
    fn no_inherits_or_method_edges_ever_emitted() {
        let ex = extract("# A\n[b](b.md)\n", "doc.md");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "inherits",
                "inherits edge must never be emitted"
            );
            assert_ne!(edge.relation, "method", "method edge must never be emitted");
        }
    }

    // ── Group 8: Span correctness ─────────────────────────────────────────────────────────────

    #[test]
    fn file_node_span_starts_at_byte_zero() {
        let ex = extract("# Hello\n", "doc.md");
        assert_eq!(
            ex.nodes[0].span.start_byte, 0,
            "file node span must start at byte 0"
        );
    }

    #[test]
    fn file_node_span_end_byte_equals_source_length() {
        let src = "# Hello\n## World\n";
        let ex = extract(src, "doc.md");
        let file_node = get_node(&ex, "doc");
        assert_eq!(
            file_node.span.end_byte as usize,
            src.len(),
            "file node span end_byte must equal source byte length"
        );
    }

    #[test]
    fn file_node_span_is_well_formed() {
        let ex = extract("# Hello\n", "doc.md");
        assert!(
            ex.nodes[0].span.is_well_formed(),
            "file node span must be well-formed"
        );
    }

    #[test]
    fn heading_node_span_is_well_formed() {
        let ex = extract("# Title\n", "doc.md");
        let n = get_node(&ex, "doc_title");
        assert!(
            n.span.is_well_formed(),
            "heading span must be well-formed: {n:?}"
        );
        assert!(!n.span.is_empty(), "heading span must not be empty");
    }

    #[test]
    fn heading_on_line_one_has_start_line_one() {
        let ex = extract("# Title\n", "doc.md");
        let n = get_node(&ex, "doc_title");
        assert_eq!(
            n.span.start_line, 1,
            "heading on line 1 must have start_line=1"
        );
        assert_eq!(
            n.span.end_line, 1,
            "single-line heading must have end_line=1"
        );
    }

    #[test]
    fn heading_on_line_two_has_start_line_two() {
        let ex = extract("\n## Section\n", "doc.md");
        let n = get_node(&ex, "doc_section");
        assert_eq!(
            n.span.start_line, 2,
            "heading on line 2 must have start_line=2; got {n:?}"
        );
    }

    #[test]
    fn heading_span_start_byte_matches_line_byte_offset() {
        // "# A\n" = 4 bytes, so heading on line 2 starts at byte 4.
        let ex = extract("# A\n## B\n", "doc.md");
        let n = get_node(&ex, "doc_b");
        assert_eq!(
            n.span.start_byte, 4,
            "heading on line 2 must start at byte 4; got {n:?}"
        );
    }

    #[test]
    fn file_node_span_start_line_is_one_for_any_source() {
        for src in &["", "# H\n", "plain text\n"] {
            let ex = extract(src, "doc.md");
            assert_eq!(
                ex.nodes[0].span.start_line, 1,
                "file node start_line must always be 1 for source: {src:?}"
            );
        }
    }

    // ── Group 9: Node counts and integration ──────────────────────────────────────────────────

    #[test]
    fn source_file_field_present_in_file_node() {
        let ex = extract("", "path/to/doc.md");
        assert!(
            ex.nodes[0].source_file.contains("doc.md"),
            "source_file must contain the filename; got {:?}",
            ex.nodes[0].source_file
        );
    }

    #[test]
    fn source_file_field_present_in_heading_node() {
        let ex = extract("# Title\n", "path/to/doc.md");
        let n = get_node(&ex, "doc_title");
        assert!(
            n.source_file.contains("doc.md"),
            "heading node source_file must contain filename; got {:?}",
            n.source_file
        );
    }

    #[test]
    fn language_slug_is_text() {
        assert_eq!(TextExtractor.language(), "text");
    }

    #[test]
    fn extensions_include_md_markdown_txt_rst() {
        let exts = TextExtractor.extensions();
        for e in &["md", "markdown", "txt", "rst"] {
            assert!(exts.contains(e), "missing extension '{e}'");
        }
    }

    #[test]
    fn extract_is_infallible_for_any_md_source() {
        // Malformed or unusual markdown content must not cause an error.
        for src in &[
            "",
            "# \n",
            "```\nunclosed fence",
            "[broken](link",
            "# A\n```\n# B\n```\n# C\n",
        ] {
            TextExtractor
                .extract(Path::new("x.md"), src.as_bytes())
                .unwrap_or_else(|e| panic!("extractor must not error on {src:?}: {e}"));
        }
    }

    #[test]
    fn mixed_headings_and_links_correct_counts() {
        let src = "# Intro\n\n[guide](guide.md)\n\n## Details\n\n[ref](ref.md)\n";
        let ex = extract(src, "doc.md");
        // 1 file + 2 heading nodes = 3 nodes.
        assert_eq!(ex.nodes.len(), 3, "expected 3 nodes");
        assert_eq!(
            count_relation(&ex, "contains"),
            2,
            "expected 2 contains edges"
        );
        assert_eq!(
            count_relation(&ex, "references"),
            2,
            "expected 2 references edges"
        );
    }

    #[test]
    fn two_md_links_produce_two_references_edges() {
        let ex = extract("[A](a.md)\n[B](b.md)\n", "doc.md");
        assert_eq!(
            count_relation(&ex, "references"),
            2,
            "two relative .md links must produce two references edges"
        );
    }

    #[test]
    fn heading_and_link_on_same_source_coexist() {
        let ex = extract("# Guide\n\n[More](more.md)\n", "doc.md");
        assert!(has_node(&ex, "doc_guide"), "heading node missing");
        assert!(
            has_edge(&ex, "doc", "doc_guide", "contains"),
            "contains edge missing"
        );
        assert!(
            has_edge(&ex, "doc", "more", "references"),
            "references edge missing"
        );
    }

    #[test]
    fn heading_after_fenced_block_parsed_correctly() {
        let src = "# Before\n```python\nx = 1\n```\n# After\n";
        let ex = extract(src, "doc.md");
        assert!(
            has_node(&ex, "doc_before"),
            "heading before fence must be parsed"
        );
        assert!(
            has_node(&ex, "doc_after"),
            "heading after fence must be parsed"
        );
        assert_eq!(ex.nodes.len(), 3, "expected 3 nodes (file + 2 headings)");
    }

    #[test]
    fn empty_heading_after_hash_emits_no_node_for_empty_slug() {
        // `# ` (hash + space, no content) → heading text = "" → slug = "" → skip.
        let ex = extract("# \n", "doc.md");
        assert_eq!(
            ex.nodes.len(),
            1,
            "heading with empty text must not emit a node"
        );
    }

    #[test]
    fn heading_with_only_numbers_emits_node() {
        let ex = extract("# 2024\n", "changelog.md");
        assert!(
            has_node(&ex, "changelog_2024"),
            "heading with only numeric text must emit a node"
        );
    }

    #[test]
    fn heading_unicode_alphanumeric_included_in_slug() {
        // Unicode letters are alphanumeric per `char::is_alphanumeric`.
        let ex = extract("# Café\n", "doc.md");
        // "café" → all chars are alphanumeric (c,a,f,é) → slug = "café"
        assert!(
            has_node(&ex, "doc_café"),
            "unicode alphanumeric chars must be included in slug; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_headings_produces_correct_edge_count() {
        let src = "# A\n# B\n# C\n# D\n# E\n";
        let ex = extract(src, "doc.md");
        assert_eq!(
            count_relation(&ex, "contains"),
            5,
            "5 headings must produce 5 contains edges"
        );
    }

    #[test]
    fn txt_with_prose_and_no_headings_yields_one_node() {
        let ex = extract("Hello world\nThis is a test.\n", "notes.txt");
        assert_eq!(ex.nodes.len(), 1);
        assert!(has_node(&ex, "notes"));
    }

    #[test]
    fn heading_with_all_uppercase_text_slug_lowercased() {
        let ex = extract("# UPPERCASE TITLE\n", "doc.md");
        assert!(
            has_node(&ex, "doc_uppercase_title"),
            "slug must be fully lowercased"
        );
    }

    #[test]
    fn unclosed_fence_suppresses_subsequent_headings() {
        // An unclosed ``` fence — everything after it is inside the fence.
        let src = "# Before\n```\n# After\n";
        let ex = extract(src, "doc.md");
        assert!(
            has_node(&ex, "doc_before"),
            "heading before unclosed fence must be parsed"
        );
        assert!(
            !has_node(&ex, "doc_after"),
            "heading inside unclosed fence must be suppressed"
        );
    }

    #[test]
    fn link_with_dot_md_in_query_string_not_emitted() {
        // A link where the path does not end with `.md` (the `.md` is in the query) → skip.
        let ex = extract("[x](page.html?ref=guide.md)\n", "doc.md");
        assert_eq!(
            ex.edges.len(),
            0,
            "link whose base path is not .md must not emit an edge"
        );
    }
}
