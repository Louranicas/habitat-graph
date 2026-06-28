//! The [`Extractor`] trait + language dispatch (interface contract §2).

use std::path::{Path, PathBuf};

use habitat_graph_core::{Extraction, Result};
use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};

/// A language-specific AST extractor: file source bytes → `{nodes, edges}`.
pub trait Extractor: Send + Sync {
    /// The language slug (e.g. `"rust"`).
    fn language(&self) -> &'static str;

    /// File extensions this extractor handles, lowercase and without leading dots (e.g. `["rs"]`).
    fn extensions(&self) -> &'static [&'static str];

    /// Extracts nodes and edges from one file's source bytes.
    ///
    /// # Errors
    /// Returns [`GraphError::Parse`](habitat_graph_core::GraphError::Parse) if the source cannot be
    /// parsed into a usable tree.
    fn extract(&self, path: &Path, source: &[u8]) -> Result<Extraction>;
}

/// Returns the set of registered extractors.
///
/// `rust` and `python` are always present (the proven parity baseline). The PA-1 grammars
/// (`ts`/`js`/`go`/`text`) are added only when their feature is enabled (R9a), so the registry
/// reflects exactly the languages the build was compiled for.
#[must_use]
pub fn registered_extractors() -> Vec<Box<dyn Extractor>> {
    // `mut` is unused only in the degenerate `--no-default-features` build with no PA-1 grammar.
    #[allow(unused_mut)]
    let mut extractors: Vec<Box<dyn Extractor>> = vec![
        Box::new(crate::ast::rust::RustExtractor),
        Box::new(crate::ast::python::PythonExtractor),
    ];
    #[cfg(feature = "ts")]
    extractors.push(Box::new(crate::ast::ts::TsExtractor));
    #[cfg(feature = "js")]
    extractors.push(Box::new(crate::ast::js::JsExtractor));
    #[cfg(feature = "go")]
    extractors.push(Box::new(crate::ast::go::GoExtractor));
    #[cfg(feature = "text")]
    extractors.push(Box::new(crate::ast::text::TextExtractor));
    extractors
}

/// Reads and extracts every file in `files`, dispatching by extension, in parallel.
///
/// Files with no matching extractor are skipped (not an error). Returns one
/// [`Extraction`](habitat_graph_core::Extraction) per extracted file.
///
/// # Errors
/// Returns the first [`GraphError`](habitat_graph_core::GraphError) encountered (read or parse).
pub fn extract_files(files: &[PathBuf]) -> Result<Vec<Extraction>> {
    let extractors = registered_extractors();
    files
        .par_iter()
        .filter_map(|path| {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_lowercase)?;
            let extractor = extractors
                .iter()
                .find(|e| e.extensions().contains(&ext.as_str()))?;
            Some(
                habitat_graph_source::read_local(path, 0)
                    .and_then(|bytes| extractor.extract(path, &bytes)),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use habitat_graph_core::{Extraction, GraphError};
    use tempfile::TempDir;

    use super::{extract_files, registered_extractors};

    // ── helpers ──────────────────────────────────────────────────────────────────

    /// Write `content` to `dir/<name>` and return its `PathBuf`.
    fn write_file(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let p = dir.path().join(name);
        fs::write(&p, content).expect("write_file");
        p
    }

    /// Collect all node labels from a set of extractions, sorted for order-independent comparison.
    fn sorted_labels(extractions: &[Extraction]) -> Vec<String> {
        let mut labels: Vec<String> = extractions
            .iter()
            .flat_map(|e| e.nodes.iter().map(|n| n.label.clone()))
            .collect();
        labels.sort_unstable();
        labels
    }

    // ── registered_extractors ─────────────────────────────────────────────────

    #[test]
    fn registered_extractors_include_rust_and_python() {
        let langs: Vec<&str> = registered_extractors()
            .iter()
            .map(|e| e.language())
            .collect();
        assert!(
            langs.contains(&"rust"),
            "rust extractor must be registered; got {langs:?}"
        );
        assert!(
            langs.contains(&"python"),
            "python extractor must be registered; got {langs:?}"
        );
    }

    #[test]
    fn registered_extractor_handles_rs_extension() {
        let extractors = registered_extractors();
        let ext = &extractors[0];
        assert!(
            ext.extensions().contains(&"rs"),
            "extractor must handle 'rs'; got {:?}",
            ext.extensions()
        );
    }

    #[test]
    fn registered_extractor_language_is_rust() {
        assert_eq!(registered_extractors()[0].language(), "rust");
    }

    #[test]
    fn registered_extractor_does_not_handle_unknown_extensions() {
        // Extensions no registered extractor claims. NB: `md`/`txt` are now handled by the `text`
        // extractor (PA-1), so the unknown set uses formats with no extractor: config/lockfiles.
        let extractors = registered_extractors();
        for ext in &["toml", "json", "yaml", "lock"] {
            for e in &extractors {
                assert!(
                    !e.extensions().contains(ext),
                    "no extractor should handle '{}'; found one: {}",
                    ext,
                    e.language()
                );
            }
        }
    }

    // ── extract_files: empty and trivial cases ────────────────────────────────

    #[test]
    fn extract_empty_file_list_returns_empty_vec() {
        let result = extract_files(&[]).expect("empty input must succeed");
        assert!(result.is_empty(), "empty input must yield empty result");
    }

    #[test]
    fn extract_single_rs_file_yields_one_extraction() {
        let dir = TempDir::new().unwrap();
        let p = write_file(&dir, "single.rs", "fn solo() {}");
        let result = extract_files(&[p]).expect("single .rs must succeed");
        assert_eq!(result.len(), 1, "one file must produce one extraction");
    }

    // ── extract_files: N-file scaling ─────────────────────────────────────────

    #[test]
    fn extract_n_rs_files_yields_n_extractions() {
        let dir = TempDir::new().unwrap();
        let files: Vec<PathBuf> = (0..5_u8)
            .map(|i| write_file(&dir, &format!("f{i}.rs"), &format!("fn f{i}() {{}}")))
            .collect();
        let result = extract_files(&files).expect("5 .rs files must all succeed");
        assert_eq!(result.len(), 5, "5 files must yield 5 extractions");
    }

    #[test]
    fn extract_many_parallel_files_all_succeed_and_have_correct_node_counts() {
        const N: usize = 10;
        let dir = TempDir::new().unwrap();
        let files: Vec<PathBuf> = (0..N)
            .map(|i| write_file(&dir, &format!("m{i}.rs"), &format!("fn item_{i}() {{}}")))
            .collect();
        let result = extract_files(&files).expect("10 files must all succeed");
        assert_eq!(result.len(), N, "10 files must yield 10 extractions");
        for ex in &result {
            assert_eq!(
                ex.nodes.len(),
                1,
                "each file has exactly one fn node; got {:?}",
                ex.nodes
            );
        }
    }

    // ── extract_files: skipping logic ─────────────────────────────────────────

    #[cfg(feature = "text")]
    #[test]
    fn extract_md_file_is_extracted_by_text_extractor() {
        // PA-1 (S1008901): the `text` extractor now handles Markdown, so a `.md` file is extracted
        // (yielding at least its file node) rather than silently skipped as before any doc grammar.
        let dir = TempDir::new().unwrap();
        let p = write_file(&dir, "README.md", "# Habitat Graph");
        let result = extract_files(&[p]).expect("md must extract");
        assert_eq!(
            result.len(),
            1,
            ".md must now be extracted by the text extractor"
        );
    }

    #[test]
    fn extract_no_extension_file_is_skipped() {
        let dir = TempDir::new().unwrap();
        let p = write_file(&dir, "Makefile", "all:\n\techo done");
        let result = extract_files(&[p]).expect("no-ext skip must not error");
        assert!(
            result.is_empty(),
            "file without extension must be silently skipped"
        );
    }

    #[cfg(feature = "text")]
    #[test]
    fn extract_mixed_rs_md_and_noext_extracts_code_and_docs_only() {
        // PA-1 (S1008901): `.rs` (rust) and `.md` (text) are both extracted; a file with no matching
        // extension (Justfile) is still silently skipped.
        let dir = TempDir::new().unwrap();
        let rs1 = write_file(&dir, "a.rs", "fn alpha() {}");
        let rs2 = write_file(&dir, "b.rs", "fn beta() {}");
        let md = write_file(&dir, "README.md", "# Notes");
        let noext = write_file(&dir, "Justfile", "gate:\n\tcargo check");
        let result = extract_files(&[rs1, rs2, md, noext]).expect("mixed batch must succeed");
        assert_eq!(
            result.len(),
            3,
            "2 .rs + 1 .md extracted; the no-extension Justfile is skipped"
        );
    }

    // ── extract_files: error propagation ──────────────────────────────────────

    #[test]
    fn extract_nonexistent_rs_path_propagates_io_error() {
        let path = PathBuf::from("/nonexistent/habitat-graph/ghost.rs");
        let err = extract_files(&[path]).expect_err("nonexistent path must error");
        assert!(
            matches!(err, GraphError::Io(_)),
            "expected GraphError::Io; got {err:?}"
        );
    }

    #[test]
    fn extract_nonexistent_path_error_has_io_kind_tag() {
        let path = PathBuf::from("/no-such-dir/nope.rs");
        let err = extract_files(&[path]).expect_err("must error");
        assert_eq!(err.kind(), "io", "error kind must be 'io'");
    }

    #[test]
    fn extract_mixed_good_and_nonexistent_propagates_error() {
        // Even one unreachable .rs file must surface as an error.
        let dir = TempDir::new().unwrap();
        let good = write_file(&dir, "good.rs", "fn ok() {}");
        let missing = PathBuf::from("/tmp/no_such_file_habitat_graph_xyz_99.rs");
        let err = extract_files(&[good, missing]).expect_err("mixed good+bad must error");
        assert!(
            matches!(err, GraphError::Io(_)),
            "expected Io from missing path; got {err:?}"
        );
    }

    // ── extract_files: order independence ─────────────────────────────────────

    #[test]
    fn extract_result_set_is_order_independent() {
        let dir = TempDir::new().unwrap();
        let a = write_file(&dir, "aa.rs", "fn alpha() {}");
        let b = write_file(&dir, "bb.rs", "fn beta() {}");
        let c = write_file(&dir, "cc.rs", "fn gamma() {}");

        let result_fwd =
            extract_files(&[a.clone(), b.clone(), c.clone()]).expect("fwd must succeed");
        let result_rev = extract_files(&[c, b, a]).expect("rev must succeed");

        assert_eq!(result_fwd.len(), 3, "fwd: expected 3 extractions");
        assert_eq!(result_rev.len(), 3, "rev: expected 3 extractions");

        // Compare sets of node labels independent of extraction order.
        let labels_fwd = sorted_labels(&result_fwd);
        let labels_rev = sorted_labels(&result_rev);
        assert_eq!(
            labels_fwd, labels_rev,
            "both orderings must yield the same node label set"
        );
    }

    // ── extract_files: extension case insensitivity ────────────────────────────

    #[test]
    fn extract_extension_matching_is_case_insensitive() {
        let dir = TempDir::new().unwrap();
        // On Linux the filesystem is case-sensitive, so .RS is a distinct filename;
        // the *extension matching* must still treat it as "rs" via lowercasing.
        let p = dir.path().join("UPPER.RS");
        fs::write(&p, "fn uppercase_ext() {}").expect("write");
        let result = extract_files(&[p]).expect(".RS extension must be extracted");
        assert_eq!(result.len(), 1, ".RS must be treated the same as .rs");
    }

    // ── extract_files: content verification ───────────────────────────────────

    #[test]
    fn extract_rs_file_nodes_match_item_names_in_source() {
        let dir = TempDir::new().unwrap();
        let src = "fn alpha() {} struct Beta {} enum Gamma { X }";
        let p = write_file(&dir, "items.rs", src);
        let mut result = extract_files(&[p]).expect("must succeed");
        assert_eq!(result.len(), 1);
        let extraction = result.remove(0);
        let labels: Vec<&str> = extraction.nodes.iter().map(|n| n.label.as_str()).collect();
        assert!(labels.contains(&"alpha"), "missing 'alpha'; got {labels:?}");
        assert!(labels.contains(&"Beta"), "missing 'Beta'; got {labels:?}");
        assert!(labels.contains(&"Gamma"), "missing 'Gamma'; got {labels:?}");
    }

    #[test]
    fn extract_empty_rs_file_yields_empty_extraction() {
        let dir = TempDir::new().unwrap();
        let p = write_file(&dir, "empty.rs", "");
        let mut result = extract_files(&[p]).expect("empty .rs must succeed");
        assert_eq!(
            result.len(),
            1,
            "empty .rs still counts as one extracted file"
        );
        assert!(
            result.remove(0).is_empty(),
            "empty source must yield empty extraction"
        );
    }

    #[test]
    fn extract_source_file_field_in_extraction_contains_filename() {
        let dir = TempDir::new().unwrap();
        let p = write_file(&dir, "traced.rs", "fn traced() {}");
        let mut result = extract_files(&[p]).expect("must succeed");
        let extraction = result.remove(0);
        let node = &extraction.nodes[0];
        assert!(
            node.source_file.contains("traced.rs"),
            "source_file {:?} must contain the filename 'traced.rs'",
            node.source_file
        );
    }

    #[test]
    fn extract_rs_with_impl_emits_defines_edges() {
        let dir = TempDir::new().unwrap();
        let src = "struct Foo {} impl Foo { fn bar(&self) {} }";
        let p = write_file(&dir, "impl.rs", src);
        let mut result = extract_files(&[p]).expect("must succeed");
        let extraction = result.remove(0);
        let defines: Vec<_> = extraction
            .edges
            .iter()
            .filter(|e| e.relation == "defines")
            .collect();
        assert_eq!(
            defines.len(),
            1,
            "impl block with one method must emit one 'defines' edge; got {defines:?}"
        );
        assert_eq!(defines[0].source, "Foo");
        assert_eq!(defines[0].target, "bar");
    }
}
