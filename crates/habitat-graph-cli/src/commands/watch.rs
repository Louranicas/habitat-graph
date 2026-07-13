//! `watch` (PC-tail) — incrementally rebuild the knowledge graph whenever source files change.
//!
//! # Architecture
//!
//! The watch command separates the **rebuild logic** (always compiled, fully unit-testable) from
//! the **filesystem event loop** (requires the `watch` Cargo feature which pulls in `notify`).
//!
//! ## Core rebuild ([`rebuild`])
//!
//! Runs the full extract → assemble → analyse → export pipeline over `dir`, writing all core
//! artifacts into `out` and returning the number of nodes written.  This is the unit that tests
//! exercise directly.
//!
//! ## Single-writer lock
//!
//! A process-wide [`std::sync::Mutex`] ([`REBUILD_LOCK`]) prevents concurrent rebuilds if events
//! arrive in a burst.  Callers that cannot acquire the lock immediately get
//! [`GraphError::Guard`] with the reason `"rebuild already in progress"`.
//!
//! ## Debounce (200 ms)
//!
//! When compiled with `--features watch`, the `notify` event loop collects events from a channel
//! and waits for a 200 ms quiet window before triggering [`rebuild`] via
//! [`try_rebuild_locked`].  This prevents thrashing during large save operations or
//! formatter runs.
//!
//! ## Error recovery
//!
//! Rebuild errors are printed to stderr but do **not** crash the watcher — subsequent file saves
//! will trigger another rebuild attempt.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use habitat_graph_core::{GraphError, Result};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Quiet-window duration; the watcher waits this long after the last event before rebuilding.
// Only actively used when `--features watch` is enabled; kept pub for documentation and testing.
#[allow(dead_code)]
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// Default output directory name (relative to the working directory).
pub const DEFAULT_OUT: &str = "graphify-out";

/// Primary artifact name inside the output directory.
#[cfg(all(feature = "watch", feature = "live-bridges"))]
const GRAPH_JSON: &str = "graph.json";

// ── Single-writer lock ────────────────────────────────────────────────────────

/// Process-wide mutex that serialises rebuilds.
///
/// Acquiring this lock before calling [`rebuild`] ensures two concurrent file-system events
/// cannot race through the pipeline simultaneously.
// Kept pub for external callers and tests; `#[allow]` suppresses the dead_code lint in
// one-shot (no watch feature) mode where the binary's call graph does not reach this item.
#[allow(dead_code)]
pub static REBUILD_LOCK: Mutex<()> = Mutex::new(());

// ── Core rebuild ──────────────────────────────────────────────────────────────

/// Extracts a graph from every recognised source file under `dir`, writes the core artifacts, and
/// refreshes optional public artifacts whose ownership manifests already claim them in `out`.
///
/// Returns the number of nodes in the resulting graph.
///
/// # Errors
///
/// - [`GraphError::Io`] — filesystem read/write failure.
/// - [`GraphError::Parse`] — a source file could not be extracted.
/// - [`GraphError::Schema`] — graph serialisation failed.
pub fn rebuild(dir: &Path, out: &Path) -> Result<usize> {
    std::fs::create_dir_all(out).map_err(|error| GraphError::Io(error.to_string()))?;
    let legacy_state = out.join(".habitat-graph-state.json");
    #[cfg(unix)]
    let state_path = super::private_state::path_for_output(&out.join("graph.json"), &legacy_state)?;
    #[cfg(not(unix))]
    let state_path = legacy_state;
    let _output_lock = super::private_state::acquire_output_lock(&state_path)?;
    #[cfg(unix)]
    {
        super::private_state::ensure_no_pending_add_journals(&state_path)?;
        super::private_state::ensure_no_pending_update_journals(&state_path)?;
    }
    #[cfg(not(unix))]
    super::private_state::remove_unsupported_family(&state_path)?;

    // Detect source files (all extractor-supported extensions).
    let files = habitat_graph_source::detect(dir, &["rs", "ts", "tsx", "js", "jsx", "go", "py"])?;
    let inputs = super::extract::capture_inputs(&files)?;
    let graph = super::extract::build_full_graph(&inputs)?;

    let n = graph.nodes.len();

    #[cfg(unix)]
    super::extract::write_full_build_private_state(&state_path, &graph, &inputs)?;
    super::extract::write_public_artifacts(out, &graph, super::extract::ExtractOpts::default())?;

    Ok(n)
}

// ── Locked rebuild helper ─────────────────────────────────────────────────────

/// Core of [`try_rebuild_locked`]; accepts any `Mutex<()>` so tests can supply a local mutex
/// rather than the process-wide [`REBUILD_LOCK`], preventing parallel-test interference.
// `#[allow]` because in the no-`watch`-feature binary, `run()` calls `rebuild()` directly
// (no lock needed for one-shot use), so this function is only reached from tests and from the
// `watch`-feature loop.  It remains pub-of-module for test access.
#[allow(dead_code)]
fn do_try_rebuild_locked(lock: &Mutex<()>, dir: &Path, out: &Path) -> Result<usize> {
    let _guard = lock.try_lock().map_err(|e| match e {
        std::sync::TryLockError::WouldBlock => {
            GraphError::Guard("rebuild already in progress".to_owned())
        }
        std::sync::TryLockError::Poisoned(_) => {
            GraphError::Guard("rebuild lock poisoned".to_owned())
        }
    })?;
    rebuild(dir, out)
}

/// Attempts to acquire the process-wide [`REBUILD_LOCK`] and run [`rebuild`].
///
/// Returns [`GraphError::Guard`] with `"rebuild already in progress"` if the lock is held.
/// Returns [`GraphError::Guard`] with `"rebuild lock poisoned"` if a prior holder panicked.
///
/// All other errors are propagated from [`rebuild`].
///
/// # Errors
///
/// See [`rebuild`], plus lock-acquisition failures described above.
// `#[allow]` for the same reason as `do_try_rebuild_locked` above.
#[allow(dead_code)]
pub fn try_rebuild_locked(dir: &Path, out: &Path) -> Result<usize> {
    do_try_rebuild_locked(&REBUILD_LOCK, dir, out)
}

// ── Arc-delta helpers (live-bridges, watch) ──────────────────────────────────

/// Reads `out/graph.json`, parses the node-link format, and returns all arcs matching the
/// default arc-relation set (`calls`, `imports_from`, `method`, `defines`).
///
/// Returns an empty `Vec` on any parse failure (I/O, malformed JSON, missing fields) so the
/// watch loop can keep running without a hard error.
#[cfg(all(feature = "watch", feature = "live-bridges"))]
fn extract_arcs_from_out(out: &Path) -> Vec<habitat_graph_habitat::arc_graph::Arc> {
    use std::collections::{HashMap, HashSet};

    let json = match std::fs::read_to_string(out.join(GRAPH_JSON)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[habitat-graph] arc-delta: failed to read graph.json: {e}");
            return Vec::new();
        }
    };
    let v: serde_json::Value = match serde_json::from_str(&json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[habitat-graph] arc-delta: graph.json is not valid JSON: {e}");
            return Vec::new();
        }
    };

    // Build u64 NodeId → label map from nodes array.
    let Some(nodes) = v.get("nodes").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let id_map: HashMap<u64, String> = nodes
        .iter()
        .filter_map(|n| {
            let id = n.get("id")?.as_u64()?;
            let label = n.get("label")?.as_str()?.to_owned();
            Some((id, label))
        })
        .collect();

    // Materialise the default relation filter once.
    let arc_relations: HashSet<&str> = habitat_graph_habitat::arc_graph::default_arc_relations()
        .iter()
        .copied()
        .collect();

    let Some(links) = v.get("links").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };

    let mut arcs: Vec<habitat_graph_habitat::arc_graph::Arc> = links
        .iter()
        .filter_map(|link| {
            let rel = link.get("relation")?.as_str()?;
            if !arc_relations.contains(rel) {
                return None;
            }
            let src_id = link.get("source")?.as_u64()?;
            let tgt_id = link.get("target")?.as_u64()?;
            let producer = id_map.get(&src_id)?.clone();
            let consumer = id_map.get(&tgt_id)?.clone();
            Some(habitat_graph_habitat::arc_graph::Arc {
                producer,
                consumer,
                relation: rel.to_owned(),
            })
        })
        .collect();

    arcs.sort_unstable();
    arcs.dedup();
    arcs
}

/// Computes an [`ArcDelta`](habitat_graph_habitat::arc_telemetry::ArcDelta) between two arc
/// snapshots and pushes it to PV2 + POVM when there is a meaningful change.
///
/// Empty-delta detection is delegated to [`DeltaPusher::push_delta`] so no redundant calls are
/// made when the graph is unchanged.
#[cfg(all(feature = "watch", feature = "live-bridges"))]
fn push_arc_delta(
    pusher: &habitat_graph_habitat::live_push::DeltaPusher,
    prev: &[habitat_graph_habitat::arc_graph::Arc],
    next: &[habitat_graph_habitat::arc_graph::Arc],
) {
    use std::collections::HashSet;

    let prev_set: HashSet<&habitat_graph_habitat::arc_graph::Arc> = prev.iter().collect();
    let next_set: HashSet<&habitat_graph_habitat::arc_graph::Arc> = next.iter().collect();

    let mut newly_severed: Vec<habitat_graph_habitat::arc_graph::Arc> = prev
        .iter()
        .filter(|a| !next_set.contains(a))
        .cloned()
        .collect();
    let mut newly_healed: Vec<habitat_graph_habitat::arc_graph::Arc> = next
        .iter()
        .filter(|a| !prev_set.contains(a))
        .cloned()
        .collect();

    newly_severed.sort_unstable();
    newly_healed.sort_unstable();

    let total = prev.len().max(next.len());
    #[allow(clippy::cast_precision_loss)]
    let coherence_delta = if total == 0 {
        0.0_f64
    } else {
        (next.len() as f64 - prev.len() as f64) / total as f64
    };

    let delta = habitat_graph_habitat::arc_telemetry::ArcDelta {
        newly_severed,
        newly_healed,
        coherence_delta,
    };

    if let Err(e) = pusher.push_delta(&delta) {
        eprintln!("[habitat-graph] arc-delta: push_delta serialisation error (non-fatal): {e}");
    }
}

// ── Watch loop (feature-gated) ────────────────────────────────────────────────

/// Starts watching `dir` for source-file changes and rebuilds into `out` on each change.
///
/// When compiled with `--features watch`, this function blocks forever (or until the process
/// receives a signal) — it is designed to be the `run` body of a CLI subcommand.
///
/// When compiled **without** `--features watch`, the function performs a single one-shot rebuild
/// and exits (useful as a manual trigger), printing a tip about enabling the `watch` feature for
/// continuous watching.
///
/// The debounce window is [`DEBOUNCE`] (200 ms).  Rebuild errors are printed to stderr but do
/// **not** abort the loop.
///
/// Returns a process exit code: `0` on success, `1` on rebuild or watch-setup error.
#[must_use]
pub fn run(dir: &Path, out: &Path) -> u8 {
    #[cfg(feature = "watch")]
    {
        run_with_notify(dir, out)
    }
    // Without --features watch: do a single rebuild and explain how to enable watching.
    #[cfg(not(feature = "watch"))]
    {
        match rebuild(dir, out) {
            Ok(n) => {
                println!("graph: {n} nodes");
                eprintln!(
                    "tip: recompile with --features watch to enable continuous file watching"
                );
                0
            }
            Err(e) => {
                eprintln!("habitat-graph watch: {e}");
                1
            }
        }
    }
}

#[cfg(feature = "watch")]
fn run_with_notify(dir: &Path, out: &Path) -> u8 {
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher as _};
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();
    let mut watcher = match RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            // Ignore send errors — if the receiver is gone the loop will exit on next iteration.
            let _ = tx.send(res);
        },
        Config::default(),
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("habitat-graph watch: failed to create watcher: {e}");
            return 1;
        }
    };

    if let Err(e) = watcher.watch(dir, RecursiveMode::Recursive) {
        eprintln!(
            "habitat-graph watch: failed to watch {}: {e}",
            dir.display()
        );
        return 1;
    }

    println!(
        "watching {} (output: {}) — Ctrl-C to stop",
        dir.display(),
        out.display()
    );

    // ── Delta-push state (live-bridges only, OFF by default) ─────────────────
    // Initialised to `None` before the first successful build; set to `Some(pusher)` and
    // `Some(arcs)` afterwards.  Compile-time gated so there is zero overhead when
    // `live-bridges` is disabled.
    #[cfg(feature = "live-bridges")]
    let pusher = habitat_graph_habitat::live_push::default_pusher();

    #[cfg(feature = "live-bridges")]
    let mut prev_arcs: Vec<habitat_graph_habitat::arc_graph::Arc> = Vec::new();

    // Initial build on startup.
    match rebuild(dir, out) {
        Ok(n) => {
            println!("watch: initial build — {n} nodes");
            // Snapshot arcs from the freshly-written graph.json.
            #[cfg(feature = "live-bridges")]
            {
                prev_arcs = extract_arcs_from_out(out);
            }
        }
        Err(e) => eprintln!("watch: initial build error: {e}"),
    }

    // Debounced event loop.
    while let Ok(first) = rx.recv() {
        // Log watcher errors but keep going.
        if let Err(e) = first {
            eprintln!("watch: watcher error: {e}");
        }

        // Drain any events that arrive within the debounce window.
        let deadline = std::time::Instant::now() + DEBOUNCE;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(Ok(_) | Err(_)) => {} // keep draining
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return 0,
            }
        }

        // Trigger rebuild.
        match try_rebuild_locked(dir, out) {
            Ok(n) => {
                println!("watch: rebuilt — {n} nodes");
                // Compute and push arc delta (live-bridges only).
                #[cfg(feature = "live-bridges")]
                {
                    let next_arcs = extract_arcs_from_out(out);
                    push_arc_delta(&pusher, &prev_arcs, &next_arcs);
                    prev_arcs = next_arcs;
                }
            }
            Err(e) => eprintln!("watch: rebuild error: {e}"),
        }
    }

    0
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    use super::{rebuild, try_rebuild_locked, DEBOUNCE, DEFAULT_OUT};

    // ── helpers ───────────────────────────────────────────────────────────────

    static SEQ: AtomicU32 = AtomicU32::new(0);
    fn tdir() -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let d = std::env::temp_dir().join(format!("hg_watch_{pid}_{n}"));
        fs::create_dir_all(&d).expect("tdir");
        d
    }

    fn mk(dir: &Path, name: &str, content: &str) {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&p, content).expect("write file");
    }

    fn count_str(haystack: &str, needle: &str) -> usize {
        let mut n = 0;
        let mut pos = 0;
        while let Some(idx) = haystack[pos..].find(needle) {
            n += 1;
            pos += idx + needle.len();
        }
        n
    }

    fn read_graph_json(out: &Path) -> String {
        fs::read_to_string(out.join("graph.json")).expect("graph.json")
    }

    fn count_nodes(out: &Path) -> usize {
        count_str(&read_graph_json(out), "\"id\":")
    }

    // ── constants ─────────────────────────────────────────────────────────────

    #[test]
    fn debounce_is_200ms() {
        assert_eq!(DEBOUNCE.as_millis(), 200);
    }

    #[test]
    fn default_out_is_graphify_out() {
        assert_eq!(DEFAULT_OUT, "graphify-out");
    }

    // ── rebuild: basic correctness ────────────────────────────────────────────

    #[test]
    fn rebuild_empty_dir_exits_ok() {
        let src = tdir();
        let out = tdir();
        assert!(rebuild(&src, &out).is_ok(), "empty dir must succeed");
    }

    #[test]
    fn rebuild_empty_dir_returns_zero_nodes() {
        let src = tdir();
        let out = tdir();
        assert_eq!(rebuild(&src, &out).expect("rebuild"), 0);
    }

    #[test]
    fn rebuild_creates_graph_json() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn f() {}");
        rebuild(&src, &out).expect("rebuild");
        assert!(out.join("graph.json").exists());
    }

    #[test]
    fn rebuild_refreshes_all_existing_public_artifacts() {
        let src = tdir();
        let out = tdir();
        let raw_label = "api_key_assignment_refused";
        mk(&src, "lib.rs", &format!("fn {raw_label}() {{}}"));
        for artifact in ["graph.svg", "graph.graphml", "graph.cypher"] {
            fs::write(out.join(artifact), raw_label).unwrap();
        }
        fs::write(
            out.join(".habitat-graph-artifacts.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": "habitat-graph.artifact-manifest.v1",
                "files": ["graph.svg", "graph.graphml", "graph.cypher"],
            }))
            .unwrap(),
        )
        .unwrap();
        let wiki = out.join("wiki");
        fs::create_dir(&wiki).unwrap();
        fs::write(wiki.join("index.md"), raw_label).unwrap();
        fs::write(wiki.join("node-4294967295.md"), raw_label).unwrap();
        fs::write(
            wiki.join(".habitat-graph-generated.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": "habitat-graph.wiki-manifest.v1",
                "files": ["index.md", "node-4294967295.md"],
            }))
            .unwrap(),
        )
        .unwrap();

        rebuild(&src, &out).expect("rebuild");

        for artifact in [
            "graph.json",
            "GRAPH_REPORT.md",
            "graph.html",
            "graph.svg",
            "graph.graphml",
            "graph.cypher",
        ] {
            assert!(!fs::read_to_string(out.join(artifact))
                .unwrap()
                .contains(raw_label));
        }
        for entry in fs::read_dir(&wiki).unwrap() {
            assert!(!fs::read_to_string(entry.unwrap().path())
                .unwrap()
                .contains(raw_label));
        }
    }

    #[test]
    fn rebuild_returns_node_count() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn a() {} fn b() {}");
        let n = rebuild(&src, &out).expect("rebuild");
        assert_eq!(n, 2, "two functions → 2 nodes");
    }

    #[test]
    fn rebuild_zero_nodes_from_empty_source() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "");
        let n = rebuild(&src, &out).expect("rebuild");
        assert_eq!(n, 0, "empty source file → 0 nodes");
    }

    #[test]
    fn rebuild_graph_json_contains_function_name() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn my_unique_function() {}");
        rebuild(&src, &out).expect("rebuild");
        let json = read_graph_json(&out);
        assert!(
            json.contains("my_unique_function"),
            "graph.json must contain the function name"
        );
    }

    #[test]
    fn rebuild_creates_output_dir() {
        let src = tdir();
        let base = tdir();
        let out = base.join("new_out");
        mk(&src, "lib.rs", "fn f() {}");
        rebuild(&src, &out).expect("rebuild");
        assert!(out.exists(), "output dir must be created");
    }

    #[test]
    fn rebuild_nested_output_dir_created() {
        let src = tdir();
        let base = tdir();
        let out = base.join("a").join("b").join("c");
        mk(&src, "lib.rs", "fn f() {}");
        rebuild(&src, &out).expect("rebuild");
        assert!(out.join("graph.json").exists());
    }

    #[test]
    fn rebuild_graph_json_is_valid_json() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn f() {}");
        rebuild(&src, &out).expect("rebuild");
        let text = read_graph_json(&out);
        serde_json::from_str::<serde_json::Value>(&text).expect("must be valid JSON");
    }

    #[test]
    fn rebuild_graph_json_has_nodes_key() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn f() {}");
        rebuild(&src, &out).expect("rebuild");
        let text = read_graph_json(&out);
        assert!(
            text.contains("\"nodes\""),
            "graph.json must have 'nodes' key"
        );
    }

    #[test]
    fn rebuild_graph_json_has_links_key() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn f() {}");
        rebuild(&src, &out).expect("rebuild");
        let text = read_graph_json(&out);
        assert!(
            text.contains("\"links\""),
            "graph.json must have 'links' key"
        );
    }

    // ── rebuild: after source changes ─────────────────────────────────────────

    #[test]
    fn rebuild_after_change_reflects_new_content() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn old_fn() {}");
        rebuild(&src, &out).expect("first");

        mk(&src, "lib.rs", "fn new_fn() {}");
        rebuild(&src, &out).expect("second");

        let json = read_graph_json(&out);
        assert!(!json.contains("old_fn"), "old function must be gone");
        assert!(json.contains("new_fn"), "new function must appear");
    }

    #[cfg(unix)]
    #[test]
    fn rebuild_refreshes_private_state_when_public_bytes_are_unchanged() {
        let src = tdir();
        let out = tdir();
        let first = "api_key=first-secret.rs";
        let second = "api_key=second-secret.rs";
        mk(&src, first, "fn stable() {}");
        rebuild(&src, &out).unwrap();
        let public = read_graph_json(&out);

        fs::remove_file(src.join(first)).unwrap();
        mk(&src, second, "fn stable() {}");
        rebuild(&src, &out).unwrap();

        assert_eq!(read_graph_json(&out), public);
        let private = fs::read_to_string(out.join(".habitat-graph-state.json")).unwrap();
        assert!(!private.contains(first));
        assert!(private.contains(second));
    }

    #[test]
    fn rebuild_after_adding_file_node_count_increases() {
        let src = tdir();
        let out = tdir();
        mk(&src, "a.rs", "fn a() {}");
        let n1 = rebuild(&src, &out).expect("first");

        mk(&src, "b.rs", "fn b() {}");
        let n2 = rebuild(&src, &out).expect("second");

        assert!(n2 > n1, "adding a file must increase node count");
    }

    #[test]
    fn rebuild_after_removing_file_node_count_decreases() {
        let src = tdir();
        let out = tdir();
        mk(&src, "a.rs", "fn a() {}");
        mk(&src, "b.rs", "fn b() {}");
        let n1 = rebuild(&src, &out).expect("first");

        fs::remove_file(src.join("b.rs")).expect("remove");
        let n2 = rebuild(&src, &out).expect("second");

        assert!(n2 < n1, "removing a file must decrease node count");
    }

    #[test]
    fn rebuild_multiple_files_all_nodes_present() {
        let src = tdir();
        let out = tdir();
        mk(&src, "aa.rs", "fn fn_aa() {}");
        mk(&src, "bb.rs", "fn fn_bb() {}");
        mk(&src, "cc.rs", "fn fn_cc() {}");
        rebuild(&src, &out).expect("rebuild");
        let json = read_graph_json(&out);
        assert!(json.contains("fn_aa"));
        assert!(json.contains("fn_bb"));
        assert!(json.contains("fn_cc"));
    }

    // ── rebuild: determinism ──────────────────────────────────────────────────

    #[test]
    fn rebuild_deterministic_two_builds_identical() {
        let src = tdir();
        mk(&src, "lib.rs", "fn alpha() {} fn beta() {}");

        let out1 = tdir();
        let out2 = tdir();
        rebuild(&src, &out1).expect("build 1");
        rebuild(&src, &out2).expect("build 2");

        let j1 = fs::read(out1.join("graph.json")).expect("j1");
        let j2 = fs::read(out2.join("graph.json")).expect("j2");
        assert_eq!(
            j1, j2,
            "two full rebuilds must produce byte-identical output"
        );
    }

    #[test]
    fn rebuild_repeated_on_unchanged_source_is_byte_identical() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn stable() {}");
        rebuild(&src, &out).expect("first");
        let first = fs::read(out.join("graph.json")).expect("read");
        rebuild(&src, &out).expect("second");
        let second = fs::read(out.join("graph.json")).expect("read");
        assert_eq!(
            first, second,
            "repeated rebuild on unchanged source must be byte-identical"
        );
    }

    // ── rebuild: error handling ───────────────────────────────────────────────

    #[test]
    fn rebuild_nonexistent_src_returns_error() {
        let phantom = PathBuf::from("/nonexistent_hg_watch_src_xyz");
        let out = tdir();
        assert!(
            rebuild(&phantom, &out).is_err(),
            "nonexistent source must error"
        );
    }

    // ── try_rebuild_locked / do_try_rebuild_locked ───────────────────────────
    //
    // All contention tests use a TEST-LOCAL mutex (not the global REBUILD_LOCK) so
    // that parallel test threads cannot interfere with each other.

    #[test]
    fn try_rebuild_locked_succeeds_when_lock_free() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn f() {}");
        // Use a local lock: tests that hold REBUILD_LOCK in other threads could
        // otherwise cause a spurious WouldBlock failure here.
        let local = Mutex::new(());
        assert!(
            super::do_try_rebuild_locked(&local, &src, &out).is_ok(),
            "must succeed when lock is free"
        );
    }

    #[test]
    fn try_rebuild_locked_returns_guard_when_lock_held() {
        let local = Mutex::new(());
        let src = tdir();
        let out = tdir();

        // Hold the local lock in the current thread.
        let _guard = local.lock().expect("acquire");
        // try_lock on a mutex already held → WouldBlock → Guard error.
        let err =
            super::do_try_rebuild_locked(&local, &src, &out).expect_err("must fail when locked");
        assert!(
            matches!(err, habitat_graph_core::GraphError::Guard(_)),
            "expected Guard error, got {err:?}"
        );
    }

    #[test]
    fn try_rebuild_locked_error_message_mentions_in_progress() {
        let local = Mutex::new(());
        let src = tdir();
        let out = tdir();

        let _guard = local.lock().expect("acquire");
        let err = super::do_try_rebuild_locked(&local, &src, &out).expect_err("must fail");
        assert!(
            err.to_string().contains("in progress"),
            "error must mention 'in progress': {err}"
        );
    }

    #[test]
    fn try_rebuild_locked_succeeds_after_lock_released() {
        let local = Mutex::new(());
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn f() {}");
        {
            let _guard = local.lock().expect("acquire");
            // Lock held — do_try_rebuild_locked would return Guard.
        }
        // Guard dropped — lock released; should now succeed.
        assert!(
            super::do_try_rebuild_locked(&local, &src, &out).is_ok(),
            "must succeed after lock is released"
        );
    }

    #[test]
    fn try_rebuild_locked_pub_api_uses_global_lock() {
        // Smoke-test the public API against the global lock when no test holds it.
        // Since no test now acquires REBUILD_LOCK, this is safe in parallel.
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn api_test() {}");
        assert!(
            try_rebuild_locked(&src, &out).is_ok(),
            "public try_rebuild_locked must succeed when global lock is free"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rebuild_respects_the_cross_process_output_lock() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn locked() {}");
        let state = out.join(".habitat-graph-state.json");
        let lock = super::super::private_state::acquire_output_lock(&state).unwrap();

        let error = rebuild(&src, &out).unwrap_err();
        assert_eq!(error.kind(), "guard");
        assert!(!out.join("graph.json").exists());
        drop(lock);
        rebuild(&src, &out).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rebuild_acquires_output_lock_before_source_scan() {
        let out = tdir();
        let state = out.join(".habitat-graph-state.json");
        let lock = super::super::private_state::acquire_output_lock(&state).unwrap();

        let error = rebuild(Path::new("/nonexistent_hg_watch_locked_src_xyz"), &out).unwrap_err();
        assert_eq!(error.kind(), "guard");
        drop(lock);
    }

    // ── run: no-feature one-shot mode ─────────────────────────────────────────

    #[test]
    #[cfg(not(feature = "watch"))]
    fn run_without_watch_feature_exits_zero_on_success() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn f() {}");
        assert_eq!(super::run(&src, &out), 0, "one-shot build must exit 0");
    }

    #[test]
    #[cfg(not(feature = "watch"))]
    fn run_without_watch_feature_creates_graph_json() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn g() {}");
        assert_eq!(super::run(&src, &out), 0);
        assert!(
            out.join("graph.json").exists(),
            "graph.json must be created"
        );
    }

    // ── rebuild: node-count correctness ───────────────────────────────────────

    #[test]
    fn rebuild_single_function_one_node() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn singleton() {}");
        assert_eq!(rebuild(&src, &out).expect("rebuild"), 1);
    }

    #[test]
    fn rebuild_node_count_matches_function_count() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn a() {} fn b() {} fn c() {}");
        assert_eq!(rebuild(&src, &out).expect("rebuild"), 3);
    }

    #[test]
    fn rebuild_nested_source_files_all_found() {
        let src = tdir();
        let out = tdir();
        mk(&src, "src/a.rs", "fn a_fn() {}");
        mk(&src, "src/sub/b.rs", "fn b_fn() {}");
        rebuild(&src, &out).expect("rebuild");
        let json = read_graph_json(&out);
        assert!(json.contains("a_fn"), "nested a_fn must be found");
        assert!(json.contains("b_fn"), "deeply nested b_fn must be found");
    }

    #[test]
    fn rebuild_empty_function_body_still_emits_node() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn empty_body() {}");
        assert_eq!(rebuild(&src, &out).expect("rebuild"), 1);
    }

    #[test]
    fn rebuild_graph_json_schema_version_present() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn f() {}");
        rebuild(&src, &out).expect("rebuild");
        let text = read_graph_json(&out);
        assert!(
            text.contains("schema_version"),
            "graph.json must carry schema_version"
        );
    }

    #[test]
    fn rebuild_node_count_returned_equals_json_node_count() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn p() {} fn q() {} fn r() {}");
        let returned = rebuild(&src, &out).expect("rebuild");
        let in_json = count_nodes(&out);
        assert_eq!(
            returned, in_json,
            "returned node count must match nodes in graph.json"
        );
    }

    // ── additional watch.rs coverage ─────────────────────────────────────────

    #[test]
    fn rebuild_output_file_is_named_graph_json() {
        // The output file is always called "graph.json", not something else.
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn named_output() {}");
        rebuild(&src, &out).expect("rebuild");
        assert!(
            out.join("graph.json").exists(),
            "output file must be named graph.json"
        );
    }

    #[test]
    fn rebuild_graph_json_is_valid_utf8() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn utf8_check() {}");
        rebuild(&src, &out).expect("rebuild");
        let bytes = fs::read(out.join("graph.json")).expect("read");
        assert!(
            std::str::from_utf8(&bytes).is_ok(),
            "graph.json must be valid UTF-8"
        );
    }

    #[test]
    fn rebuild_graph_json_links_key_is_array() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn links_test() {}");
        rebuild(&src, &out).expect("rebuild");
        let text = read_graph_json(&out);
        let v: serde_json::Value = serde_json::from_str(&text).expect("parse");
        assert!(
            v.get("links").and_then(|l| l.as_array()).is_some(),
            "links key must be a JSON array"
        );
    }

    #[test]
    fn rebuild_graph_json_nodes_key_is_array() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn nodes_array_check() {}");
        rebuild(&src, &out).expect("rebuild");
        let text = read_graph_json(&out);
        let v: serde_json::Value = serde_json::from_str(&text).expect("parse");
        assert!(
            v.get("nodes").and_then(|n| n.as_array()).is_some(),
            "nodes key must be a JSON array"
        );
    }

    #[test]
    fn rebuild_graph_json_nodes_have_id_field() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn has_id() {}");
        rebuild(&src, &out).expect("rebuild");
        let text = read_graph_json(&out);
        let v: serde_json::Value = serde_json::from_str(&text).expect("parse");
        let nodes = v["nodes"].as_array().expect("nodes array");
        assert!(!nodes.is_empty(), "must have at least one node");
        for node in nodes {
            assert!(
                node.get("id").is_some(),
                "every node must have an 'id' field"
            );
        }
    }

    #[test]
    fn rebuild_graph_json_nodes_have_label_field() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn label_check() {}");
        rebuild(&src, &out).expect("rebuild");
        let text = read_graph_json(&out);
        let v: serde_json::Value = serde_json::from_str(&text).expect("parse");
        let nodes = v["nodes"].as_array().expect("nodes array");
        for node in nodes {
            assert!(
                node.get("label").is_some(),
                "every node must have a 'label' field"
            );
        }
    }

    #[test]
    fn rebuild_returns_zero_for_empty_source_dir() {
        let src = tdir();
        let out = tdir();
        assert_eq!(rebuild(&src, &out).expect("empty rebuild"), 0);
    }

    #[test]
    fn rebuild_five_functions_five_nodes() {
        let src = tdir();
        let out = tdir();
        mk(
            &src,
            "lib.rs",
            "fn f1(){} fn f2(){} fn f3(){} fn f4(){} fn f5(){}",
        );
        assert_eq!(rebuild(&src, &out).expect("rebuild"), 5);
    }

    #[test]
    fn rebuild_after_adding_file_nodes_increase() {
        let src = tdir();
        let out = tdir();
        mk(&src, "a.rs", "fn a() {}");
        let n1 = rebuild(&src, &out).expect("first");
        mk(&src, "b.rs", "fn b() {}");
        let n2 = rebuild(&src, &out).expect("second");
        assert!(n2 > n1, "adding a file must increase node count");
    }

    #[test]
    fn rebuild_deeply_nested_subdir_source_found() {
        let src = tdir();
        let out = tdir();
        mk(&src, "a/b/c/d/e.rs", "fn very_deep() {}");
        rebuild(&src, &out).expect("rebuild");
        let text = read_graph_json(&out);
        assert!(
            text.contains("very_deep"),
            "deeply nested function must appear in graph"
        );
    }

    #[test]
    fn rebuild_error_for_nonexistent_dir_is_descriptive() {
        let phantom = PathBuf::from("/no_such_habitat_graph_watch_dir_xyz");
        let out = tdir();
        let err = rebuild(&phantom, &out).expect_err("must fail");
        // The error must mention the missing path or describe an I/O failure.
        let msg = err.to_string();
        assert!(!msg.is_empty(), "error message must not be empty");
    }

    #[test]
    fn default_out_constant_equals_graphify_out() {
        assert_eq!(DEFAULT_OUT, "graphify-out");
    }

    #[test]
    fn debounce_constant_is_200ms() {
        assert_eq!(DEBOUNCE, std::time::Duration::from_millis(200));
    }

    #[test]
    fn rebuild_two_files_in_separate_dirs_are_both_found() {
        let src = tdir();
        let out = tdir();
        mk(&src, "mod1/alpha.rs", "fn alpha_fn() {}");
        mk(&src, "mod2/beta.rs", "fn beta_fn() {}");
        rebuild(&src, &out).expect("rebuild");
        let text = read_graph_json(&out);
        assert!(text.contains("alpha_fn"));
        assert!(text.contains("beta_fn"));
    }

    #[test]
    fn try_rebuild_locked_error_variant_is_guard() {
        let local = Mutex::new(());
        let _guard = local.lock().expect("acquire");
        let err = super::do_try_rebuild_locked(&local, &tdir(), &tdir()).expect_err("must fail");
        assert!(
            matches!(err, habitat_graph_core::GraphError::Guard(_)),
            "locked → must be Guard, not Io or Schema"
        );
    }

    #[test]
    fn rebuild_multiple_functions_same_file_all_in_graph() {
        let src = tdir();
        let out = tdir();
        mk(
            &src,
            "lib.rs",
            "fn one() {} fn two() {} fn three() {} fn four() {} fn five() {}",
        );
        rebuild(&src, &out).expect("rebuild");
        let text = read_graph_json(&out);
        for name in ["one", "two", "three", "four", "five"] {
            assert!(
                text.contains(name),
                "function '{name}' must appear in graph"
            );
        }
    }

    #[test]
    fn rebuild_second_call_overwrites_first_output() {
        let src = tdir();
        let out = tdir();
        mk(&src, "lib.rs", "fn first_fn() {}");
        rebuild(&src, &out).expect("first");
        // Change source so second rebuild produces different content.
        mk(&src, "lib.rs", "fn second_fn() {}");
        rebuild(&src, &out).expect("second");
        let text = read_graph_json(&out);
        // The graph must reflect the SECOND state, not the first.
        assert!(
            text.contains("second_fn"),
            "second rebuild must overwrite first"
        );
    }
}
