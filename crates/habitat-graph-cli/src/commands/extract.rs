//! The `extract` command — the full pipeline (detect → extract → build → analyze → export → write).

use std::path::Path;

use habitat_graph_core::{GraphError, Result};

/// Runs the extraction pipeline over `dir` and writes `graph.json` + `GRAPH_REPORT.md` +
/// `graph.html` (a self-contained interactive viewer) into `out`.
///
/// Returns a process exit code: `0` on success, `4` on any error (diagnostics to stderr).
///
/// On success, prints a one-line summary to stdout:
/// ```text
/// graph: N nodes, E edges, C communities -> <out>
/// ```
#[must_use]
pub fn run(dir: &Path, out: &Path, vault: Option<&Path>) -> u8 {
    match run_inner(dir, out, vault) {
        Ok((n, e, c)) => {
            println!(
                "graph: {n} nodes, {e} edges, {c} communities -> {}",
                out.display()
            );
            if let Some(v) = vault {
                println!("obsidian vault ({n} notes + _MOC) -> {}", v.display());
            }
            0
        }
        Err(err) => {
            eprintln!("error: {err}");
            4
        }
    }
}

/// Inner pipeline: detect → extract → build → analyze → export → write.
///
/// Returns `(node_count, edge_count, community_count)` on success.
///
/// # Errors
///
/// Returns [`GraphError::Io`] if any filesystem operation fails: directory traversal,
/// output-directory creation, or artifact write.  Propagates [`GraphError::Parse`] from the
/// tree-sitter extractor and [`GraphError::Schema`] from JSON serialization.
fn run_inner(dir: &Path, out: &Path, vault: Option<&Path>) -> Result<(usize, usize, usize)> {
    // Detect all Rust source files under `dir`, honoring .gitignore.
    let files = habitat_graph_source::detect(dir, &["rs"])?;

    // Extract raw nodes and edges from each file (parallel, tree-sitter).
    let extractions = habitat_graph_extract::extract_files(&files)?;

    // Intern labels, resolve edges, dedup, and sort into a canonical graph.
    let mut graph = habitat_graph_build::assemble(extractions);

    // Attach Leiden communities before the final sort pass.
    graph.communities = habitat_graph_analyze::detect_communities(&graph);

    // Re-sort to canonicalize the community list (idempotent on nodes/edges).
    let graph = graph.sorted();

    // Ensure the output directory (and all parents) exist.
    std::fs::create_dir_all(out).map_err(|e| GraphError::Io(e.to_string()))?;

    // Write graph.json — NetworkX node-link envelope, graphify-compatible.
    let json = habitat_graph_export::to_node_link(&graph)?;
    std::fs::write(out.join("graph.json"), json.as_bytes())
        .map_err(|e| GraphError::Io(e.to_string()))?;

    // Write GRAPH_REPORT.md — human-facing Markdown summary.
    let report = habitat_graph_export::render_report(&graph);
    std::fs::write(out.join("GRAPH_REPORT.md"), report.as_bytes())
        .map_err(|e| GraphError::Io(e.to_string()))?;

    // Write graph.html — self-contained interactive viewer (the graphify graph.html analogue).
    let html = habitat_graph_export::render_html(&graph)?;
    std::fs::write(out.join("graph.html"), html.as_bytes())
        .map_err(|e| GraphError::Io(e.to_string()))?;

    // Optionally emit an Obsidian vault — one note per node (`[[wikilinks]]` + frontmatter/tags)
    // for Obsidian's graph view + Dataview / Juggl / Breadcrumbs.
    if let Some(vault_dir) = vault {
        std::fs::create_dir_all(vault_dir).map_err(|e| GraphError::Io(e.to_string()))?;
        for (filename, content) in habitat_graph_export::render_vault(&graph) {
            std::fs::write(vault_dir.join(&filename), content.as_bytes())
                .map_err(|e| GraphError::Io(format!("{filename}: {e}")))?;
        }
    }

    Ok(graph.counts())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::run;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Create `dir/name` (and any missing parents), writing `content`.
    fn mk_file(dir: &Path, name: &str, content: &str) {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
    }

    /// Read `out/graph.json` as a `String`.
    fn read_graph_json(out: &Path) -> String {
        fs::read_to_string(out.join("graph.json")).expect("graph.json not found")
    }

    /// Count non-overlapping occurrences of `needle` in `haystack`.
    fn count_str(haystack: &str, needle: &str) -> usize {
        let mut n = 0_usize;
        let mut pos = 0_usize;
        while let Some(idx) = haystack[pos..].find(needle) {
            n += 1;
            pos += idx + needle.len();
        }
        n
    }

    // ── T1: .rs file with two functions → exit 0 ─────────────────────────────
    // Probes: the whole pipeline runs without error for a non-trivial source file.

    #[test]
    fn single_rs_file_returns_exit_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() { b(); } fn b() {}");
        assert_eq!(run(src.path(), out.path(), None), 0);
    }

    // ── T1b: graph.html is written (the interactive viewer) ──────────────────
    #[test]
    fn graph_html_file_written() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() { b(); } fn b() {}");
        let _ = run(src.path(), out.path(), None);
        let html = fs::read_to_string(out.path().join("graph.html")).expect("graph.html");
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("graph-data"));
    }

    // ── T1c: --vault emits an Obsidian vault (notes + _MOC) ──────────────────
    #[test]
    fn vault_emitted_when_requested() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let vault = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() { b(); } fn b() {}");
        assert_eq!(run(src.path(), out.path(), Some(vault.path())), 0);
        assert!(vault.path().join("_MOC.md").exists(), "vault MOC must exist");
        // At least one node note with frontmatter + a Dataview typed edge.
        let entries: Vec<_> = fs::read_dir(vault.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .collect();
        assert!(entries.len() >= 2, "expected node notes + MOC, got {}", entries.len());
        let any = fs::read_to_string(vault.path().join("a.md")).expect("a.md");
        assert!(any.starts_with("---\n"), "note must carry frontmatter: {any}");
        assert!(any.contains("tags: [hg/node"), "note must carry tags: {any}");
        assert!(any.contains(":: [["), "note must carry a Dataview typed edge: {any}");
    }

    // ── T1d: no vault written when not requested ─────────────────────────────
    #[test]
    fn no_vault_when_not_requested() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn solo() {}");
        let _ = run(src.path(), out.path(), None);
        // out dir holds graph.json/html/report only — no _MOC.md.
        assert!(!out.path().join("_MOC.md").exists());
    }

    // ── T2: graph.json is written ────────────────────────────────────────────
    // Probes: the first artifact file is always produced on success.

    #[test]
    fn graph_json_file_written() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn hello() {}");
        let _ = run(src.path(), out.path(), None);
        assert!(
            out.path().join("graph.json").exists(),
            "graph.json must be created after a successful run"
        );
    }

    // ── T3: GRAPH_REPORT.md is written ───────────────────────────────────────
    // Probes: the second artifact file is always produced on success.

    #[test]
    fn graph_report_md_written() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn hello() {}");
        let _ = run(src.path(), out.path(), None);
        assert!(
            out.path().join("GRAPH_REPORT.md").exists(),
            "GRAPH_REPORT.md must be created after a successful run"
        );
    }

    // ── T4: graph.json has non-empty nodes array for .rs source ──────────────
    // Probes: at least one node is extracted from a file containing a function.
    // (Each node entry has an "id" field; links use "source"/"target", not "id".)

    #[test]
    fn graph_json_has_nodes_for_rs_source() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() { b(); } fn b() {}");
        let rc = run(src.path(), out.path(), None);
        assert_eq!(rc, 0, "must exit 0");
        let json = read_graph_json(out.path());
        assert!(
            json.contains("\"id\":"),
            "non-trivial source must produce ≥1 node (no \"id\":\" found); json={json:.200}"
        );
    }

    // ── T5: empty source dir → exit 0 ────────────────────────────────────────
    // Probes: zero files is a valid (empty-graph) success case, not an error.

    #[test]
    fn empty_source_dir_returns_exit_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        assert_eq!(run(src.path(), out.path(), None), 0);
    }

    // ── T6: empty source dir → graph.json nodes is [] ────────────────────────
    // Probes: empty pipeline → empty-nodes envelope, no stale data.

    #[test]
    fn empty_source_dir_writes_empty_nodes_array() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let _ = run(src.path(), out.path(), None);
        let json = read_graph_json(out.path());
        assert!(
            json.contains("\"nodes\": []"),
            "empty source must produce \"nodes\": []; json={json:.200}"
        );
        assert!(
            !json.contains("\"id\":"),
            "empty graph must have no node id fields; json={json:.200}"
        );
    }

    // ── T7: non-existent source dir → exit 4 ─────────────────────────────────
    // Probes: detect() failure maps to exit code 4 (error path).

    #[test]
    fn nonexistent_source_dir_returns_exit_four() {
        let out = TempDir::new().unwrap();
        let phantom = Path::new("/nonexistent_habitat_graph_cli_test_xyzzy_42");
        assert_eq!(run(phantom, out.path(), None), 4);
    }

    // ── T8: nested output directory is created ────────────────────────────────
    // Probes: create_dir_all makes deeply nested out paths that don't yet exist.

    #[test]
    fn nested_out_dir_is_created() {
        let src = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        let out = base.path().join("a").join("b").join("c");
        mk_file(src.path(), "lib.rs", "fn foo() {}");
        let rc = run(src.path(), &out, None);
        assert_eq!(rc, 0, "must exit 0 after creating nested out dir");
        assert!(
            out.join("graph.json").exists(),
            "graph.json must exist inside the nested dir"
        );
    }

    // ── T9: determinism — two runs produce byte-identical graph.json ──────────
    // Probes: sorted() + fixed Leiden seed → identical bytes on repeated calls.

    #[test]
    fn two_runs_produce_byte_identical_graph_json() {
        let src = TempDir::new().unwrap();
        let out1 = TempDir::new().unwrap();
        let out2 = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() { b(); } fn b() {}");
        let _ = run(src.path(), out1.path(), None);
        let _ = run(src.path(), out2.path(), None);
        let j1 = fs::read(out1.path().join("graph.json")).unwrap();
        let j2 = fs::read(out2.path().join("graph.json")).unwrap();
        assert_eq!(j1, j2, "graph.json must be byte-identical across runs");
    }

    // ── T10: graph.json has the expected NetworkX envelope keys ───────────────
    // Probes: to_node_link wraps the data in the graphify-compatible envelope.

    #[test]
    fn graph_json_has_networkx_envelope_keys() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn hello() {}");
        let _ = run(src.path(), out.path(), None);
        let json = read_graph_json(out.path());
        for key in ["\"directed\"", "\"multigraph\"", "\"nodes\"", "\"links\""] {
            assert!(
                json.contains(key),
                "graph.json must contain key {key}; json={json:.200}"
            );
        }
    }

    // ── T11: GRAPH_REPORT.md starts with the expected title ──────────────────
    // Probes: render_report output is written verbatim (correct file, not swapped).

    #[test]
    fn graph_report_starts_with_title() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn x() {}");
        let _ = run(src.path(), out.path(), None);
        let report = fs::read_to_string(out.path().join("GRAPH_REPORT.md")).unwrap();
        assert!(
            report.starts_with("# Graph Report"),
            "GRAPH_REPORT.md must start with '# Graph Report'"
        );
    }

    // ── T12: multi-item source file produces ≥2 nodes ────────────────────────
    // Probes: extractor captures all top-level items (functions + structs).

    #[test]
    fn multi_item_source_produces_multiple_nodes() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(
            src.path(),
            "lib.rs",
            "fn alpha() {} fn beta() {} struct Gamma {}",
        );
        let rc = run(src.path(), out.path(), None);
        assert_eq!(rc, 0);
        let json = read_graph_json(out.path());
        // Each node entry contains exactly one "id": field.
        let node_count = count_str(&json, "\"id\":");
        assert!(
            node_count >= 2,
            "3-item source must produce ≥2 nodes; got {node_count}"
        );
    }

    // ── T13: non-.rs files in the source dir are silently ignored ────────────
    // Probes: detect(&["rs"]) filters by extension; Markdown/TOML are skipped.

    #[test]
    fn non_rs_files_are_ignored() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "README.md", "# docs");
        mk_file(src.path(), "build.toml", "[x]");
        mk_file(src.path(), "lib.rs", "fn only_me() {}");
        let rc = run(src.path(), out.path(), None);
        assert_eq!(rc, 0);
        let json = read_graph_json(out.path());
        let node_count = count_str(&json, "\"id\":");
        assert_eq!(
            node_count, 1,
            "only the .rs file contributes nodes; got {node_count}"
        );
    }

    // ── T14: re-run overwrites existing output cleanly ────────────────────────
    // Probes: write over existing files doesn't fail or produce corrupt output.

    #[test]
    fn rerun_overwrites_existing_output_cleanly() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn alpha() {}");
        let _ = run(src.path(), out.path(), None);
        let rc = run(src.path(), out.path(), None);
        assert_eq!(rc, 0, "second run must also exit 0");
        let json = read_graph_json(out.path());
        assert!(
            json.contains("\"nodes\""),
            "second run must produce valid graph.json"
        );
    }

    // ── T15: graph.json directed:true ─────────────────────────────────────────
    // Probes: the envelope correctly marks the graph as directed.

    #[test]
    fn graph_json_directed_is_true() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let _ = run(src.path(), out.path(), None);
        let json = read_graph_json(out.path());
        assert!(
            json.contains("\"directed\": true"),
            "graph.json must contain \"directed\": true"
        );
    }

    // ── T16: two-function source produces exactly 2 nodes and exit 0 ─────────
    // Probes: node count is accurate for a small, well-understood source.

    #[test]
    fn two_function_source_produces_exactly_two_nodes() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() { b(); } fn b() {}");
        let rc = run(src.path(), out.path(), None);
        assert_eq!(rc, 0, "must exit 0");
        let json = read_graph_json(out.path());
        let node_count = count_str(&json, "\"id\":");
        assert_eq!(node_count, 2, "fn a + fn b must produce exactly 2 nodes");
    }

    // ── T17: multiple .rs files accumulate all nodes ──────────────────────────
    // Probes: the pipeline handles multiple input files (not just one).

    #[test]
    fn multiple_rs_files_accumulate_nodes() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn fn_a() {}");
        mk_file(src.path(), "b.rs", "fn fn_b() {}");
        let rc = run(src.path(), out.path(), None);
        assert_eq!(rc, 0);
        let json = read_graph_json(out.path());
        let node_count = count_str(&json, "\"id\":");
        assert_eq!(
            node_count, 2,
            "two single-fn files must produce exactly 2 nodes; got {node_count}"
        );
    }

    // ── T18: graph.json links array is present in the envelope ────────────────
    // Probes: the links key is always emitted, even for empty or edge-free graphs.

    #[test]
    fn graph_json_links_array_is_present() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn lone() {}");
        let _ = run(src.path(), out.path(), None);
        let json = read_graph_json(out.path());
        assert!(
            json.contains("\"links\""),
            "graph.json must always contain a \"links\" array"
        );
    }

    // ── T19: empty source dir still writes GRAPH_REPORT.md ───────────────────
    // Probes: both artifacts are produced even for an empty graph.

    #[test]
    fn empty_source_dir_writes_report() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let _ = run(src.path(), out.path(), None);
        assert!(
            out.path().join("GRAPH_REPORT.md").exists(),
            "GRAPH_REPORT.md must exist even for an empty graph"
        );
    }

    // ── T20: deeply-nested source is detected ────────────────────────────────
    // Probes: detect() recurses into subdirectories.

    #[test]
    fn nested_source_file_is_detected() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "sub/module/deep.rs", "fn deep_fn() {}");
        let rc = run(src.path(), out.path(), None);
        assert_eq!(rc, 0);
        let json = read_graph_json(out.path());
        let node_count = count_str(&json, "\"id\":");
        assert_eq!(node_count, 1, "deeply nested .rs must be found");
    }
}
