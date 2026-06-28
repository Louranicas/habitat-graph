//! Shared tree-sitter helpers for the PA-1 grammar extractors (`ts`/`js`/`go`/`text`).
//!
//! These mirror the byte/line conventions already used by the `rust` and `python` extractors, hoisted
//! into one place so the per-language modules share a single, tested implementation. The `rust` and
//! `python` extractors predate this module and keep their own copies (the proven parity baseline is
//! left untouched); new grammars use these.

use std::path::Path;

use habitat_graph_core::Span;

/// Decodes the raw bytes covered by `node` into a [`String`], replacing invalid UTF-8 lossily.
///
/// Source for the supported languages is expected to be valid UTF-8; the lossy decode is a
/// defensive measure so a malformed byte sequence yields replacement characters rather than a panic.
#[must_use]
pub fn text_of(source: &[u8], node: &tree_sitter::Node<'_>) -> String {
    String::from_utf8_lossy(&source[node.start_byte()..node.end_byte()]).into_owned()
}

/// Builds a [`Span`] from a tree-sitter node using the canonical byte+line formulas.
///
/// Tree-sitter row positions are 0-based; one is added for the 1-based line contract.
/// `u32::try_from(..).unwrap_or(u32::MAX)` guards the degenerate > 4 GiB case, and
/// `saturating_add(1)` keeps the increment defined at that ceiling.
#[must_use]
pub fn make_span(node: &tree_sitter::Node<'_>) -> Span {
    Span::new(
        u32::try_from(node.start_byte()).unwrap_or(u32::MAX),
        u32::try_from(node.end_byte()).unwrap_or(u32::MAX),
        u32::try_from(node.start_position().row)
            .unwrap_or(u32::MAX)
            .saturating_add(1),
        u32::try_from(node.end_position().row)
            .unwrap_or(u32::MAX)
            .saturating_add(1),
    )
}

/// Returns the lowercased file stem of `path` — graphify's file-node label `B`.
///
/// `"Client.ts"` → `"client"`, `"net_http.go"` → `"net_http"`. A path with no stem yields `""`.
#[must_use]
pub fn stem_lower(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::stem_lower;

    #[test]
    fn stem_lower_lowercases_simple_stem() {
        assert_eq!(stem_lower(Path::new("Client.ts")), "client");
    }

    #[test]
    fn stem_lower_strips_only_final_extension() {
        assert_eq!(stem_lower(Path::new("net.http.go")), "net.http");
    }

    #[test]
    fn stem_lower_handles_full_path() {
        assert_eq!(stem_lower(Path::new("a/b/Server.go")), "server");
    }

    #[test]
    fn stem_lower_no_extension_returns_whole_name() {
        assert_eq!(stem_lower(Path::new("README")), "readme");
    }

    #[test]
    fn stem_lower_empty_path_is_empty() {
        assert_eq!(stem_lower(Path::new("")), "");
    }

    #[test]
    fn stem_lower_preserves_underscores_and_digits() {
        assert_eq!(stem_lower(Path::new("My_Mod2.js")), "my_mod2");
    }
}
