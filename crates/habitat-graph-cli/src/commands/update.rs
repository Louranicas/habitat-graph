//! The `update` command — incremental rebuild with sidecar-based extraction cache (FO-9 lifecycle).
//!
//! ## Honest cost model (C-4)
//!
//! File-level extraction is cached via a sidecar file
//! (`<out>/.habitat-graph-state.json`), so only files whose `blake3` content hash has changed
//! since the last build are re-parsed by the tree-sitter extractors.  **Community detection is
//! not incremental**: Leiden is always re-run on the full combined graph after each incremental
//! merge.  The sidecar saves AST-parsing work; it does not save analysis work.
//!
//! ## Sidecar format
//!
//! The sidecar stores the complete [`Graph`] in its internal JSON format
//! (via [`Graph::to_json`] / [`Graph::from_json`]).  On the next incremental run three things
//! are extracted from it without extra serialization overhead:
//!
//! - `graph.schema` — the [`SCHEMA_VERSION`] at build time (for the P1-G12 mismatch guard).
//! - `graph.manifest.inputs` — the `path → content_hash` map used to diff against the current
//!   file-system state.
//! - `graph.nodes` / `graph.edges` — the prior graph, pruned to drop stale-file nodes before
//!   merging with newly-extracted content.
//!
//! ## Schema-version guard (P1-G12)
//!
//! If the sidecar's `schema` field differs from the current [`SCHEMA_VERSION`], a warning is
//! emitted to stderr and a full rebuild is performed.  Silently merging across taxonomy versions
//! would corrupt the graph by mixing incompatible node/edge semantics.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use habitat_graph_core::{Graph, GraphError, Manifest, NodeId, Result, SCHEMA_VERSION};

/// Sidecar filename storing the full internal [`Graph`] JSON (relative to the output directory).
const SIDECAR: &str = ".habitat-graph-state.json";
/// Primary artifact filename (node-link format, graphify-compatible).
const GRAPH_JSON: &str = "graph.json";
/// Monotonic suffix for collision-free private sidecar temporary files.
static SIDECAR_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Runs an incremental update over `dir`, writing refreshed artifacts into `out`.
///
/// On the first invocation (no sidecar present) or after a [`SCHEMA_VERSION`] mismatch, a full
/// rebuild is performed.  On subsequent invocations only files whose `blake3` content hash has
/// changed are re-extracted; unchanged files' nodes/edges are preserved from the prior graph.
/// Community detection is always re-run globally (honest C-4 cost: Leiden is not incremental).
///
/// ## Stdout
///
/// Emits exactly one summary line on success:
/// - `"update: {changed} changed, {n} nodes (analyze re-run globally)"` after any build.
/// - `"update: 0 changed, {n} nodes (artifacts refreshed; analyze not re-run)"` when source
///   inputs are unchanged. Public artifacts and private sidecar permissions are still refreshed so
///   an exporter-policy upgrade cannot leave legacy output behind.
///
/// Returns a process exit code: `0` on success, `4` on any error (diagnostics to stderr).
///
/// # Errors
///
/// Returns exit code `4` on any IO, parse, or serialization failure.
#[must_use]
pub fn run(dir: &Path, out: &Path) -> u8 {
    match run_inner(dir, out) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            4
        }
    }
}

/// Inner incremental-rebuild pipeline, propagating errors as [`GraphError`].
///
/// # Errors
///
/// Returns [`GraphError::Io`] on filesystem failures, [`GraphError::Parse`] on extraction
/// failures, or [`GraphError::Schema`] on serialization failures.
fn run_inner(dir: &Path, out: &Path) -> Result<()> {
    // ── Detect all source files (sorted for R4 determinism) ─────────────────────
    let files = habitat_graph_source::detect(dir, &["rs"])?;
    let sidecar_path = out.join(SIDECAR);

    // ── Load the prior sidecar (returns None on first run / mismatch) ───────────
    let Some(prior_graph) = try_load_prior(&sidecar_path)? else {
        // No sidecar or schema mismatch → full rebuild.
        return do_full_build(out, &files);
    };

    // ── Hash every current file (needed for the diff) ───────────────────────────
    let current_inputs: Vec<(PathBuf, Vec<u8>)> = files
        .iter()
        .map(|p| {
            let bytes = habitat_graph_source::read_local(p, 0)?;
            Ok((p.clone(), bytes))
        })
        .collect::<Result<_>>()?;

    let current_manifest =
        habitat_graph_source::build_manifest(&current_inputs, env!("CARGO_PKG_VERSION"));

    // ── Clone prior manifest into owned strings to release prior_graph borrow ───
    // `prior_map` must not borrow from `prior_graph.manifest` (we move prior_graph later).
    let prior_entries: Vec<(String, String)> = prior_graph
        .manifest
        .inputs
        .iter()
        .map(|r| (r.path.clone(), r.content_hash.clone()))
        .collect();
    // `prior_graph.manifest.inputs` borrow ends here (iter consumed, collect done).

    let current_map: HashMap<&str, &str> = current_manifest
        .inputs
        .iter()
        .map(|r| (r.path.as_str(), r.content_hash.as_str()))
        .collect();
    let prior_map: HashMap<&str, &str> = prior_entries
        .iter()
        .map(|(p, h)| (p.as_str(), h.as_str()))
        .collect();

    let current_paths: HashSet<&str> = current_map.keys().copied().collect();
    let prior_paths: HashSet<&str> = prior_map.keys().copied().collect();

    // Build diff lists as owned `String`s so they do not borrow from `prior_graph`.
    let mut added: Vec<String> = current_paths
        .difference(&prior_paths)
        .copied()
        .map(str::to_owned)
        .collect();
    let mut removed: Vec<String> = prior_paths
        .difference(&current_paths)
        .copied()
        .map(str::to_owned)
        .collect();
    let mut changed: Vec<String> = current_paths
        .intersection(&prior_paths)
        .copied()
        .filter(|&p| current_map.get(p) != prior_map.get(p))
        .map(str::to_owned)
        .collect();

    // Deterministic ordering of the three diff lists (R4 compliance).
    added.sort_unstable();
    removed.sort_unstable();
    changed.sort_unstable();

    let need_reextract = added.len() + changed.len();

    // ── Unchanged-input artifact refresh ─────────────────────────────────────────
    if need_reextract == 0 && removed.is_empty() {
        let n = prior_graph.nodes.len();
        // Export policy evolves independently of source hashes. Always rerender public artifacts
        // and atomically reharden the private sidecar so an upgrade cannot report success while
        // leaving legacy unredacted output or permissive cache permissions in place.
        write_artifacts(out, &prior_graph, current_manifest)?;
        println!("update: 0 changed, {n} nodes (artifacts refreshed; analyze not re-run)");
        return Ok(());
    }

    // ── Re-extract ONLY changed + added files (C-4 extraction saving) ───────────
    // Both sets borrow from the owned Vec<String>s, not from prior_graph.
    let to_extract_set: HashSet<&str> = added
        .iter()
        .chain(changed.iter())
        .map(String::as_str)
        .collect();
    let stale_set: HashSet<&str> = changed
        .iter()
        .chain(removed.iter())
        .map(String::as_str)
        .collect();

    let to_extract: Vec<PathBuf> = files
        .iter()
        .filter(|p| {
            let cow = p.to_string_lossy();
            to_extract_set.contains(cow.as_ref())
        })
        .cloned()
        .collect();

    let new_extractions = habitat_graph_extract::extract_files(&to_extract)?;
    let new_partial = habitat_graph_build::assemble(new_extractions);

    // ── Prune prior graph (drop stale-file nodes + dangling edges) ───────────────
    // `prior_graph` is moved here; nothing borrows it at this point.
    let pruned_prior = prune_graph(prior_graph, &stale_set);

    // ── Merge + global community detection (honest: Leiden is NOT incremental) ───
    // F12: cluster on the TRUSTED subgraph only (INFERRED/AMBIGUOUS edges excluded).
    let mut combined = habitat_graph_build::merge(new_partial, pruned_prior);
    combined.communities = habitat_graph_analyze::detect_communities(
        &habitat_graph_analyze::trusted_subgraph(&combined),
    );
    let combined = combined.sorted();

    // ── Write artifacts + refresh sidecar ────────────────────────────────────────
    let n = combined.nodes.len();
    write_artifacts(out, &combined, current_manifest)?;
    println!("update: {need_reextract} changed, {n} nodes (analyze re-run globally)");
    Ok(())
}

/// Attempts to load the prior graph from the sidecar file.
///
/// Returns `Ok(None)` when:
/// - The sidecar does not exist (first run — a full build is needed).
/// - The sidecar is corrupted / not valid JSON (a warning is printed to stderr).
/// - The sidecar's `schema` differs from [`SCHEMA_VERSION`] (taxonomy mismatch; warning printed).
///
/// # Errors
///
/// Returns [`GraphError::Io`] only if the sidecar file exists but cannot be read from disk.
fn try_load_prior(sidecar_path: &Path) -> Result<Option<Graph>> {
    if !sidecar_path.exists() {
        return Ok(None);
    }

    let bytes =
        std::fs::read(sidecar_path).map_err(|e| GraphError::Io(format!("sidecar read: {e}")))?;
    let text = String::from_utf8_lossy(&bytes);

    let graph = match Graph::from_json(&text) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("warning: sidecar parse failed ({e}): forcing full rebuild");
            return Ok(None);
        }
    };

    if graph.schema != SCHEMA_VERSION {
        eprintln!(
            "warning: schema_version mismatch \
             (stored={:?}, current={SCHEMA_VERSION:?}): forcing full rebuild",
            graph.schema
        );
        return Ok(None);
    }

    Ok(Some(graph))
}

/// Performs a full rebuild from scratch (first run or schema-version mismatch).
///
/// Reads every file in `files`, runs the full pipeline
/// (extract → assemble → analyze → export), and writes all artifacts including a fresh sidecar.
///
/// Files are read twice: once internally by `extract_files` for AST parsing, and once here to
/// compute the `blake3` content hashes for the sidecar manifest.
///
/// # Errors
///
/// Returns a [`GraphError`] on extraction, analysis, export, or IO failure.
fn do_full_build(out: &Path, files: &[PathBuf]) -> Result<()> {
    // Run the full pipeline (extract_files reads files internally).
    let extractions = habitat_graph_extract::extract_files(files)?;
    let mut graph = habitat_graph_build::assemble(extractions);
    // F12: cluster on the TRUSTED subgraph only (INFERRED/AMBIGUOUS edges excluded).
    graph.communities =
        habitat_graph_analyze::detect_communities(&habitat_graph_analyze::trusted_subgraph(&graph));
    let graph = graph.sorted();

    // Compute the sidecar manifest (second pass over the same files for content hashes).
    let inputs: Vec<(PathBuf, Vec<u8>)> = files
        .iter()
        .map(|p| {
            let bytes = habitat_graph_source::read_local(p, 0)?;
            Ok((p.clone(), bytes))
        })
        .collect::<Result<_>>()?;
    let manifest = habitat_graph_source::build_manifest(&inputs, env!("CARGO_PKG_VERSION"));

    let n = graph.nodes.len();
    write_artifacts(out, &graph, manifest)?;
    println!(
        "update: {} changed, {n} nodes (analyze re-run globally)",
        files.len()
    );
    Ok(())
}

/// Writes all three core artifacts (`graph.json`, `GRAPH_REPORT.md`, `graph.html`) and the
/// sidecar (`<out>/.habitat-graph-state.json`) into `out`, creating the directory if needed.
///
/// The sidecar stores `graph` with `current_manifest` substituted in: this ensures the sidecar
/// tracks the actual content hashes of the current file-system snapshot, not the empty manifest
/// produced by [`habitat_graph_build::assemble`].
///
/// # Errors
///
/// Returns [`GraphError::Io`] on any filesystem failure, or [`GraphError::Schema`] on
/// serialization failure.
fn write_artifacts(out: &Path, graph: &Graph, current_manifest: Manifest) -> Result<()> {
    std::fs::create_dir_all(out).map_err(|e| GraphError::Io(e.to_string()))?;

    // graph.json — NetworkX node-link envelope (graphify-compatible; P1-G12 schema_version key).
    let json = habitat_graph_export::to_node_link(graph)?;
    std::fs::write(out.join(GRAPH_JSON), json.as_bytes())
        .map_err(|e| GraphError::Io(e.to_string()))?;

    // GRAPH_REPORT.md — human-facing Markdown summary.
    let report = habitat_graph_export::render_report(graph);
    std::fs::write(out.join("GRAPH_REPORT.md"), report.as_bytes())
        .map_err(|e| GraphError::Io(e.to_string()))?;

    // graph.html — self-contained interactive viewer.
    let html = habitat_graph_export::render_html(graph)?;
    std::fs::write(out.join("graph.html"), html.as_bytes())
        .map_err(|e| GraphError::Io(e.to_string()))?;

    // Sidecar — full internal Graph with the current content-hash manifest, written last so
    // that if it exists, the other artifacts were (at least attempted to be) written first.
    let mut sidecar = graph.clone();
    sidecar.manifest = current_manifest;
    let sidecar_json = sidecar.to_json()?;
    write_private_sidecar(&out.join(SIDECAR), sidecar_json.as_bytes())?;

    Ok(())
}

/// Atomically writes the internal incremental cache with owner-only permissions.
///
/// The sidecar intentionally retains pre-projection labels needed for stable content ids and
/// incremental merge behavior. It is not a public artifact, so bytes are first written and synced
/// to a same-directory `0600` temporary file, then atomically renamed over the destination. This
/// prevents both a truncate-before-chmod exposure window and a partially written live sidecar.
fn write_private_sidecar(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| GraphError::Io("sidecar path has no parent".to_owned()))?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| GraphError::Io("sidecar filename is not valid UTF-8".to_owned()))?;
    let sequence = SIDECAR_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{filename}.tmp.{}.{}",
        std::process::id(),
        sequence
    ));

    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);

    let write_result = (|| -> Result<()> {
        let mut file = options
            .open(&temporary)
            .map_err(|error| GraphError::Io(format!("sidecar temp open: {error}")))?;
        file.write_all(bytes)
            .map_err(|error| GraphError::Io(format!("sidecar temp write: {error}")))?;
        file.sync_all()
            .map_err(|error| GraphError::Io(format!("sidecar temp sync: {error}")))?;
        drop(file);
        std::fs::rename(&temporary, path)
            .map_err(|error| GraphError::Io(format!("sidecar atomic rename: {error}")))?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| GraphError::Io(format!("sidecar directory sync: {error}")))?;
        Ok(())
    })();

    if write_result.is_err() && temporary.exists() {
        if let Err(cleanup_error) = std::fs::remove_file(&temporary) {
            eprintln!(
                "warning: failed to remove sidecar temporary file {}: {cleanup_error}",
                temporary.display()
            );
        }
    }
    write_result
}

/// Returns `graph` with all nodes whose [`habitat_graph_core::Node::source_file`] appears in
/// `stale_files` removed, along with every edge that references a dropped node.
///
/// Communities are always cleared regardless of what files are stale: community detection is
/// re-run globally on the combined graph after the incremental merge.
fn prune_graph(mut graph: Graph, stale_files: &HashSet<&str>) -> Graph {
    // Collect the ids of retained nodes while pruning.
    let mut kept: HashSet<NodeId> = HashSet::new();
    graph.nodes.retain(|n| {
        if stale_files.contains(n.source_file.as_str()) {
            false
        } else {
            kept.insert(n.id);
            true
        }
    });
    // Drop edges that reference any removed node (dangling-edge policy, consistent with merge).
    graph
        .edges
        .retain(|e| kept.contains(&e.source) && kept.contains(&e.target));
    // Communities are always re-run globally after the merge; clear to avoid stale data.
    graph.communities.clear();
    graph
}

// ── Tests ─────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::{run, SIDECAR};
    use habitat_graph_core::Graph;

    // ── helpers ──────────────────────────────────────────────────────────────────

    /// Create `dir/name` (and any missing parent directories) with the given `content`.
    fn mk_file(dir: &Path, name: &str, content: &str) {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
    }

    /// Remove a file (wraps `fs::remove_file`).
    fn rm_file(dir: &Path, name: &str) {
        fs::remove_file(dir.join(name)).unwrap();
    }

    /// Read `out/graph.json` as a `String`.
    fn read_graph_json(out: &Path) -> String {
        fs::read_to_string(out.join("graph.json")).expect("graph.json missing")
    }

    /// Read `out/<SIDECAR>` as a `String`.
    fn read_sidecar(out: &Path) -> String {
        fs::read_to_string(out.join(SIDECAR)).expect("sidecar missing")
    }

    /// Count non-overlapping occurrences of `needle` in `haystack`.
    fn count_str(haystack: &str, needle: &str) -> usize {
        let mut n = 0;
        let mut pos = 0;
        while let Some(idx) = haystack[pos..].find(needle) {
            n += 1;
            pos += idx + needle.len();
        }
        n
    }

    /// Count the number of nodes in `out/graph.json` (each node has exactly one `"id":` key).
    fn count_nodes(out: &Path) -> usize {
        count_str(&read_graph_json(out), "\"id\":")
    }

    /// Parse the sidecar as a [`Graph`].
    fn parse_sidecar(out: &Path) -> Graph {
        Graph::from_json(&read_sidecar(out)).expect("sidecar must parse as Graph")
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T1–T5  First build
    // ─────────────────────────────────────────────────────────────────────────────

    // T1: first build of a non-empty dir exits 0.
    #[test]
    fn fresh_dir_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        assert_eq!(run(src.path(), out.path()), 0);
    }

    // T2: first build writes graph.json.
    #[test]
    fn fresh_dir_writes_graph_json() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        assert!(out.path().join("graph.json").exists());
    }

    // T3: first build writes the sidecar.
    #[test]
    fn fresh_dir_writes_sidecar_with_owner_only_permissions() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn private_cache() {}");
        let _ = run(src.path(), out.path());
        let sidecar = out.path().join(SIDECAR);
        assert!(sidecar.exists(), "sidecar must be written on first build");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(sidecar).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "sidecar must be owner-readable/writable only");
        }
    }

    #[test]
    fn fresh_dir_writes_sidecar() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        assert!(
            out.path().join(SIDECAR).exists(),
            "sidecar must be written on first build"
        );
    }

    // T4: first build writes GRAPH_REPORT.md.
    #[test]
    fn fresh_dir_writes_graph_report() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        assert!(out.path().join("GRAPH_REPORT.md").exists());
    }

    // T5: first build writes graph.html.
    #[test]
    fn fresh_dir_writes_graph_html() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        assert!(out.path().join("graph.html").exists());
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T6–T9  No-op (second run, nothing changed)
    // ─────────────────────────────────────────────────────────────────────────────

    // T6: second run with no changes exits 0.
    #[test]
    fn noop_second_run_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        assert_eq!(run(src.path(), out.path()), 0, "no-op must exit 0");
    }

    // T7: no-op does not change graph.json bytes.
    #[test]
    fn noop_graph_json_byte_identical() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() { b(); } fn b() {}");
        let _ = run(src.path(), out.path());
        let before = fs::read(out.path().join("graph.json")).unwrap();
        let _ = run(src.path(), out.path());
        let after = fs::read(out.path().join("graph.json")).unwrap();
        assert_eq!(before, after, "no-op must leave graph.json byte-identical");
    }

    // T8: an unchanged-input refresh leaves the sidecar parseable as a valid Graph.
    #[test]
    fn noop_sidecar_still_parseable() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn x() {}");
        let _ = run(src.path(), out.path());
        let _ = run(src.path(), out.path());
        let g = parse_sidecar(out.path());
        assert_eq!(g.nodes.len(), 1);
    }

    #[test]
    fn unchanged_input_refreshes_legacy_outputs_and_hardens_sidecar() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let raw_label = "api_key_assignment_refused";
        mk_file(src.path(), "lib.rs", &format!("fn {raw_label}() {{}}"));
        assert_eq!(run(src.path(), out.path()), 0);

        for artifact in ["graph.json", "GRAPH_REPORT.md", "graph.html"] {
            fs::write(out.path().join(artifact), raw_label).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(out.path().join(SIDECAR), fs::Permissions::from_mode(0o644))
                .unwrap();
        }

        assert_eq!(run(src.path(), out.path()), 0);
        for artifact in ["graph.json", "GRAPH_REPORT.md", "graph.html"] {
            let refreshed = fs::read_to_string(out.path().join(artifact)).unwrap();
            assert!(
                !refreshed.contains(raw_label),
                "legacy raw label survived refresh in {artifact}"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(out.path().join(SIDECAR))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        let temporary_count = fs::read_dir(out.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".habitat-graph-state.json.tmp")
            })
            .count();
        assert_eq!(temporary_count, 0, "sidecar temp file must not survive");
    }

    // T9: no-op on an empty source directory exits 0.
    #[test]
    fn noop_empty_dir_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let _ = run(src.path(), out.path()); // first build (empty)
        assert_eq!(
            run(src.path(), out.path()),
            0,
            "empty-dir no-op must exit 0"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T10–T16  Changed file
    // ─────────────────────────────────────────────────────────────────────────────

    // T10: changing a file and re-running exits 0.
    #[test]
    fn change_file_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn old() {}");
        let _ = run(src.path(), out.path());
        mk_file(src.path(), "lib.rs", "fn new_fn() {}");
        assert_eq!(run(src.path(), out.path()), 0);
    }

    // T11: after changing a file graph.json is updated (hash differs from prior run).
    #[test]
    fn change_file_graph_json_differs() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn alpha() {}");
        let _ = run(src.path(), out.path());
        let before = fs::read(out.path().join("graph.json")).unwrap();
        mk_file(src.path(), "lib.rs", "fn beta() {}");
        let _ = run(src.path(), out.path());
        let after = fs::read(out.path().join("graph.json")).unwrap();
        assert_ne!(before, after, "graph.json must change when source changes");
    }

    // T12: renaming the function in a changed file → old label is gone.
    #[test]
    fn change_function_name_old_label_gone() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn old_name() {}");
        let _ = run(src.path(), out.path());
        mk_file(src.path(), "lib.rs", "fn new_name() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            !json.contains("old_name"),
            "old function name must not appear in updated graph"
        );
    }

    // T13: renaming the function in a changed file → new label is present.
    #[test]
    fn change_function_name_new_label_present() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn old_name() {}");
        let _ = run(src.path(), out.path());
        mk_file(src.path(), "lib.rs", "fn new_name() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            json.contains("new_name"),
            "new function name must appear in updated graph"
        );
    }

    // T14: after changing a file the sidecar has a new content_hash for that path.
    #[test]
    fn change_file_sidecar_has_new_hash() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn v1() {}");
        let _ = run(src.path(), out.path());
        let g1 = parse_sidecar(out.path());
        let hash1 = g1.manifest.inputs[0].content_hash.clone();

        mk_file(src.path(), "lib.rs", "fn v2() {}");
        let _ = run(src.path(), out.path());
        let g2 = parse_sidecar(out.path());
        let hash2 = &g2.manifest.inputs[0].content_hash;

        assert_ne!(
            hash1, *hash2,
            "content_hash must change when file content changes"
        );
    }

    // T15: nodes from unchanged files are preserved after an incremental update.
    #[test]
    fn change_file_unchanged_nodes_preserved() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn keep_me() {}");
        mk_file(src.path(), "b.rs", "fn changed_fn() {}");
        let _ = run(src.path(), out.path());

        mk_file(src.path(), "b.rs", "fn renamed_fn() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            json.contains("keep_me"),
            "node from unchanged file must still be present"
        );
    }

    // T16: changing multiple files is fully reflected in the next update.
    #[test]
    fn change_multiple_files_all_updated() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn a_old() {}");
        mk_file(src.path(), "b.rs", "fn b_old() {}");
        let _ = run(src.path(), out.path());

        mk_file(src.path(), "a.rs", "fn a_new() {}");
        mk_file(src.path(), "b.rs", "fn b_new() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(!json.contains("a_old"), "a_old must be gone");
        assert!(!json.contains("b_old"), "b_old must be gone");
        assert!(json.contains("a_new"), "a_new must be present");
        assert!(json.contains("b_new"), "b_new must be present");
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T17–T21  Added file
    // ─────────────────────────────────────────────────────────────────────────────

    // T17: adding a new source file and re-running exits 0.
    #[test]
    fn add_file_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        mk_file(src.path(), "b.rs", "fn b() {}");
        assert_eq!(run(src.path(), out.path()), 0);
    }

    // T18: nodes from an added file appear in graph.json.
    #[test]
    fn add_file_nodes_appear() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn existing() {}");
        let _ = run(src.path(), out.path());
        mk_file(src.path(), "b.rs", "fn brand_new() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            json.contains("brand_new"),
            "added file's function must appear"
        );
    }

    // T19: the sidecar tracks the newly added file's path and hash.
    #[test]
    fn add_file_sidecar_tracks_it() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        mk_file(src.path(), "b.rs", "fn b() {}");
        let _ = run(src.path(), out.path());

        let g = parse_sidecar(out.path());
        let paths: Vec<&str> = g.manifest.inputs.iter().map(|r| r.path.as_str()).collect();
        assert!(
            paths.iter().any(|p| p.ends_with("b.rs")),
            "sidecar must track the new file; got {paths:?}"
        );
    }

    // T20: a file added inside a nested subdirectory is detected and included.
    #[test]
    fn add_file_in_nested_subdir() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let _ = run(src.path(), out.path()); // empty first build
        mk_file(src.path(), "deep/sub/new.rs", "fn nested_fn() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            json.contains("nested_fn"),
            "deeply nested file must be found after add"
        );
    }

    // T21: adding a file increases the node count in graph.json.
    #[test]
    fn add_file_increases_node_count() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn fn_a() {}");
        let _ = run(src.path(), out.path());
        let before = count_nodes(out.path());

        mk_file(src.path(), "b.rs", "fn fn_b() {}");
        let _ = run(src.path(), out.path());
        let after = count_nodes(out.path());
        assert!(
            after > before,
            "node count must increase after adding a file"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T22–T26  Removed file
    // ─────────────────────────────────────────────────────────────────────────────

    // T22: removing a file and re-running exits 0.
    #[test]
    fn remove_file_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn a() {}");
        mk_file(src.path(), "b.rs", "fn b() {}");
        let _ = run(src.path(), out.path());
        rm_file(src.path(), "b.rs");
        assert_eq!(run(src.path(), out.path()), 0);
    }

    // T23: nodes from a removed file are absent after the update.
    #[test]
    fn remove_file_nodes_gone() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn keep() {}");
        mk_file(src.path(), "b.rs", "fn gone() {}");
        let _ = run(src.path(), out.path());
        rm_file(src.path(), "b.rs");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            !json.contains("gone"),
            "node from removed file must be absent"
        );
        assert!(
            json.contains("keep"),
            "node from remaining file must still be present"
        );
    }

    // T24: edges whose source or target was in a removed file are also dropped.
    #[test]
    fn remove_file_edges_dropped() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        // A single file with an internal call edge: fn a() calls fn b().
        mk_file(src.path(), "ab.rs", "fn a() { b(); } fn b() {}");
        mk_file(src.path(), "other.rs", "fn other() {}");
        let _ = run(src.path(), out.path());

        rm_file(src.path(), "ab.rs");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        // After removal the links array must contain no references to "a" or "b".
        assert!(
            !json.contains("\"a\"") && !json.contains("\"b\""),
            "labels from removed file must not appear as link endpoints"
        );
    }

    // T25: the sidecar no longer tracks the removed file's path.
    #[test]
    fn remove_file_sidecar_drops_path() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn a() {}");
        mk_file(src.path(), "b.rs", "fn b() {}");
        let _ = run(src.path(), out.path());
        rm_file(src.path(), "b.rs");
        let _ = run(src.path(), out.path());

        let g = parse_sidecar(out.path());
        let paths: Vec<&str> = g.manifest.inputs.iter().map(|r| r.path.as_str()).collect();
        assert!(
            !paths.iter().any(|p| p.ends_with("b.rs")),
            "sidecar must not track the removed file; got {paths:?}"
        );
    }

    // T26: removing all source files produces an empty graph.
    #[test]
    fn remove_all_files_empty_graph() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        rm_file(src.path(), "a.rs");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            json.contains("\"nodes\": []"),
            "empty graph must have an empty nodes array"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T27–T29  Schema-version mismatch
    // ─────────────────────────────────────────────────────────────────────────────

    // T27: a sidecar with a wrong schema version triggers a full rebuild (exits 0).
    #[test]
    fn schema_mismatch_still_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn f() {}");
        let _ = run(src.path(), out.path());

        // Corrupt the sidecar's schema field to simulate a taxonomy version bump.
        let sidecar_str = read_sidecar(out.path());
        let corrupted =
            sidecar_str.replace("habitat-graph.graph.v0", "habitat-graph.graph.OLD_VERSION");
        fs::write(out.path().join(SIDECAR), corrupted).unwrap();

        assert_eq!(
            run(src.path(), out.path()),
            0,
            "schema mismatch must still succeed"
        );
    }

    // T28: after a schema-mismatch full rebuild the sidecar carries the correct schema.
    #[test]
    fn schema_mismatch_sidecar_refreshed() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn f() {}");
        let _ = run(src.path(), out.path());

        let sidecar_str = read_sidecar(out.path());
        let corrupted =
            sidecar_str.replace("habitat-graph.graph.v0", "habitat-graph.graph.OLD_VERSION");
        fs::write(out.path().join(SIDECAR), corrupted).unwrap();
        let _ = run(src.path(), out.path());

        let g = parse_sidecar(out.path());
        assert_eq!(
            g.schema,
            habitat_graph_core::SCHEMA_VERSION,
            "sidecar schema must be refreshed after mismatch rebuild"
        );
    }

    // T29: after a schema-mismatch rebuild the subsequent run works incrementally.
    #[test]
    fn schema_mismatch_subsequent_run_is_incremental() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn original() {}");
        let _ = run(src.path(), out.path());

        // Corrupt → forced full rebuild.
        let sidecar_str = read_sidecar(out.path());
        let corrupted = sidecar_str.replace("habitat-graph.graph.v0", "habitat-graph.graph.STALE");
        fs::write(out.path().join(SIDECAR), corrupted).unwrap();
        let _ = run(src.path(), out.path()); // full rebuild

        // Now a no-change incremental run should succeed and produce a correct graph.
        let rc = run(src.path(), out.path());
        assert_eq!(rc, 0);
        assert!(read_graph_json(out.path()).contains("original"));
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T30–T33  Missing / corrupted artifacts
    // ─────────────────────────────────────────────────────────────────────────────

    // T30: deleting the sidecar triggers a fresh full rebuild (exits 0).
    #[test]
    fn deleted_sidecar_triggers_full_rebuild() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn fn1() {}");
        let _ = run(src.path(), out.path());
        fs::remove_file(out.path().join(SIDECAR)).unwrap();
        assert_eq!(
            run(src.path(), out.path()),
            0,
            "missing sidecar must trigger full rebuild"
        );
    }

    // T31: a completely fresh output directory (no prior graph.json or sidecar) exits 0.
    #[test]
    fn fresh_out_dir_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn first() {}");
        // No prior run — purely first-time build.
        assert_eq!(run(src.path(), out.path()), 0);
    }

    // T32: a corrupted (non-JSON) sidecar falls back to a full rebuild (exits 0).
    #[test]
    fn corrupted_sidecar_json_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn ok() {}");
        let _ = run(src.path(), out.path());
        // Overwrite sidecar with garbage.
        fs::write(out.path().join(SIDECAR), b"NOT JSON AT ALL!!!").unwrap();
        assert_eq!(
            run(src.path(), out.path()),
            0,
            "corrupt sidecar must still succeed"
        );
    }

    // T33: after a corrupted-sidecar full rebuild the new sidecar is valid.
    #[test]
    fn corrupted_sidecar_rebuild_produces_valid_sidecar() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn fn_x() {}");
        let _ = run(src.path(), out.path());
        fs::write(out.path().join(SIDECAR), b"{ bad json").unwrap();
        let _ = run(src.path(), out.path());
        // Sidecar must now be parseable again.
        let g = parse_sidecar(out.path());
        assert_eq!(g.nodes.len(), 1);
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T34–T35  Determinism
    // ─────────────────────────────────────────────────────────────────────────────

    // T34: two fresh full builds from identical sources produce byte-identical graph.json.
    #[test]
    fn determinism_two_full_builds_identical() {
        let src = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn foo() { bar(); } fn bar() {}");

        let out1 = TempDir::new().unwrap();
        let out2 = TempDir::new().unwrap();
        let _ = run(src.path(), out1.path());
        let _ = run(src.path(), out2.path());

        let j1 = fs::read(out1.path().join("graph.json")).unwrap();
        let j2 = fs::read(out2.path().join("graph.json")).unwrap();
        assert_eq!(
            j1, j2,
            "two full builds must produce byte-identical graph.json"
        );
    }

    // T35: incremental build (re-extract changed file) produces the same graph.json as a
    // fresh full rebuild with the same final set of source files.
    #[test]
    fn determinism_incremental_equals_full_rebuild() {
        let src = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn a_v1() {}");
        mk_file(src.path(), "b.rs", "fn b_fn() {}");

        // Build 1: full build with v1 source.
        let out_incr = TempDir::new().unwrap();
        let _ = run(src.path(), out_incr.path());

        // Change a.rs to v2.
        mk_file(src.path(), "a.rs", "fn a_v2() {}");

        // Incremental update (knows v1→v2 change).
        let _ = run(src.path(), out_incr.path());
        let j_incr = fs::read(out_incr.path().join("graph.json")).unwrap();

        // Fresh full build with v2 source (no prior sidecar).
        let out_full = TempDir::new().unwrap();
        let _ = run(src.path(), out_full.path());
        let j_full = fs::read(out_full.path().join("graph.json")).unwrap();

        assert_eq!(
            j_incr, j_full,
            "incremental update must produce the same graph.json as a fresh full build"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T36–T38  Empty source directory
    // ─────────────────────────────────────────────────────────────────────────────

    // T36: empty source directory on first run exits 0.
    #[test]
    fn empty_source_dir_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        assert_eq!(run(src.path(), out.path()), 0);
    }

    // T37: empty source directory produces a graph.json with an empty nodes array.
    #[test]
    fn empty_source_dir_empty_nodes_array() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            json.contains("\"nodes\": []"),
            "empty source must produce \"nodes\": []"
        );
    }

    // T38: empty source directory writes a sidecar with zero inputs.
    #[test]
    fn empty_source_dir_writes_sidecar_with_no_inputs() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let _ = run(src.path(), out.path());
        let g = parse_sidecar(out.path());
        assert!(
            g.manifest.inputs.is_empty(),
            "empty-source sidecar must have zero inputs"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T39–T41  Error handling
    // ─────────────────────────────────────────────────────────────────────────────

    // T39: non-existent source directory returns exit code 4.
    #[test]
    fn nonexistent_dir_returns_four() {
        let out = TempDir::new().unwrap();
        let phantom = std::path::Path::new("/nonexistent_habitat_graph_update_test_xyz42");
        assert_eq!(run(phantom, out.path()), 4);
    }

    // T40: output directory is created if it does not exist.
    #[test]
    fn out_dir_created_if_missing() {
        let src = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        let out = base.path().join("new_out_dir");
        mk_file(src.path(), "lib.rs", "fn f() {}");
        assert_eq!(run(src.path(), &out), 0);
        assert!(out.join("graph.json").exists());
    }

    // T41: deeply nested output directory is created.
    #[test]
    fn nested_out_dir_created() {
        let src = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        let out = base.path().join("a").join("b").join("c");
        mk_file(src.path(), "lib.rs", "fn deep() {}");
        assert_eq!(run(src.path(), &out), 0);
        assert!(out.join("graph.json").exists());
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T42–T46  Output correctness
    // ─────────────────────────────────────────────────────────────────────────────

    // T42: two functions in one file produce exactly two nodes.
    #[test]
    fn two_functions_produces_two_nodes() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn p() { q(); } fn q() {}");
        let _ = run(src.path(), out.path());
        assert_eq!(count_nodes(out.path()), 2, "must produce exactly 2 nodes");
    }

    // T43: nodes from multiple source files are all present in graph.json.
    #[test]
    fn multiple_files_all_nodes_present() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "aa.rs", "fn fn_aa() {}");
        mk_file(src.path(), "bb.rs", "fn fn_bb() {}");
        mk_file(src.path(), "cc.rs", "fn fn_cc() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(json.contains("fn_aa"), "fn_aa missing");
        assert!(json.contains("fn_bb"), "fn_bb missing");
        assert!(json.contains("fn_cc"), "fn_cc missing");
    }

    // T44: graph.json contains all required NetworkX envelope keys.
    #[test]
    fn graph_json_has_networkx_envelope_keys() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn f() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        for key in ["\"directed\"", "\"multigraph\"", "\"nodes\"", "\"links\""] {
            assert!(json.contains(key), "graph.json must contain key {key}");
        }
    }

    // T45: graph.json carries schema_version in the graph metadata object (P1-G12).
    #[test]
    fn graph_json_has_schema_version_in_envelope() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn f() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            json.contains("schema_version"),
            "graph.json must carry schema_version"
        );
        assert!(
            json.contains(habitat_graph_core::SCHEMA_VERSION),
            "schema_version must match SCHEMA_VERSION"
        );
    }

    // T46: the sidecar round-trips via Graph::from_json with the correct schema.
    #[test]
    fn sidecar_is_valid_internal_graph_json() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn sidecar_fn() {}");
        let _ = run(src.path(), out.path());
        let g = parse_sidecar(out.path());
        assert_eq!(
            g.schema,
            habitat_graph_core::SCHEMA_VERSION,
            "sidecar schema must equal SCHEMA_VERSION"
        );
        assert_eq!(g.nodes.len(), 1, "sidecar must contain the extracted node");
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T47–T50  Incremental correctness
    // ─────────────────────────────────────────────────────────────────────────────

    // T47: adding a file then removing it returns the graph to its original state.
    #[test]
    fn add_then_remove_returns_to_original() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "base.rs", "fn base() {}");
        let _ = run(src.path(), out.path());
        let original_count = count_nodes(out.path());

        mk_file(src.path(), "extra.rs", "fn extra() {}");
        let _ = run(src.path(), out.path());
        assert!(
            count_nodes(out.path()) > original_count,
            "add must increase count"
        );

        rm_file(src.path(), "extra.rs");
        let _ = run(src.path(), out.path());
        assert_eq!(
            count_nodes(out.path()),
            original_count,
            "remove must restore original node count"
        );
    }

    // T48: nodes from unchanged files are not duplicated after an incremental update.
    #[test]
    fn unchanged_nodes_not_duplicated_after_incremental() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "stable.rs", "fn stable_fn() {}");
        mk_file(src.path(), "changing.rs", "fn ch_v1() {}");
        let _ = run(src.path(), out.path());

        mk_file(src.path(), "changing.rs", "fn ch_v2() {}");
        let _ = run(src.path(), out.path());

        // Exact node count: stable_fn + ch_v2 = 2 (not 3).
        assert_eq!(
            count_nodes(out.path()),
            2,
            "incremental update must not duplicate unchanged nodes"
        );
    }

    // T49: after an incremental update a subsequent no-change run is a no-op (exits 0).
    #[test]
    fn second_run_after_change_is_noop() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn init() {}");
        let _ = run(src.path(), out.path());

        mk_file(src.path(), "lib.rs", "fn updated() {}");
        let _ = run(src.path(), out.path()); // incremental

        // No further change → no-op.
        assert_eq!(run(src.path(), out.path()), 0);
        assert!(read_graph_json(out.path()).contains("updated"));
    }

    // T50: changing the same file twice produces the correct final state each time.
    #[test]
    fn change_file_twice_second_change_reflected() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn v1() {}");
        let _ = run(src.path(), out.path());

        mk_file(src.path(), "lib.rs", "fn v2() {}");
        let _ = run(src.path(), out.path());
        assert!(read_graph_json(out.path()).contains("v2"));
        assert!(!read_graph_json(out.path()).contains("v1"));

        mk_file(src.path(), "lib.rs", "fn v3() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(json.contains("v3"), "v3 must appear after second change");
        assert!(!json.contains("v2"), "v2 must be gone after second change");
        assert!(!json.contains("v1"), "v1 must still be gone");
    }
}
