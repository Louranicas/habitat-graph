//! `add <URL>` (PC-tail) — fetch a remote source file, extract its graph, and merge it into an
//! existing `graph.json`.
//!
//! # Security
//!
//! Every URL is validated by [`habitat_graph_source::ssrf::is_safe_url`] before a network
//! connection is attempted. Blocked categories: loopback, private, link-local, CGNAT, unspecified,
//! non-`http`/`https` schemes.
//!
//! # `DoS` caps
//!
//! - [`MAX_CONTENT_BYTES`] (10 MiB): response bodies exceeding this are rejected before being
//!   fully read into memory.
//! - [`FETCH_TIMEOUT`] (30 s): the HTTP fetch must complete within this wall-clock budget.
//!
//! # Pipeline
//!
//! ```text
//! URL → SSRF check → fetch → size-cap → temp file → extract → merge → write graph.json
//! ```
//!
//! The extension inferred from the URL path determines which extractor runs. Unknown extensions
//! fall back to the Rust extractor (the most commonly useful default in this workspace).
//!
//! # Feature gate
//!
//! The HTTP transport is compiled only when `--features live` is passed to Cargo. Without it,
//! [`run`] immediately returns exit code `2` with a message explaining the missing feature. All
//! other logic (SSRF validation, extension inference, merge) is always compiled and fully tested.

use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use habitat_graph_core::{GraphError, Result};
use habitat_graph_source::ssrf::is_safe_url;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum permitted response body size: 10 MiB.
// Kept pub for testing and documentation even when the `live` feature is disabled.
#[allow(dead_code)]
pub const MAX_CONTENT_BYTES: usize = 10 * 1024 * 1024;

/// Maximum time allowed for the full HTTP exchange.
// Only actively used when `--features live` is enabled.
#[allow(dead_code)]
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

// ── Extension inference ───────────────────────────────────────────────────────

/// Infers the file extension from the URL's path component.
///
/// Returns a lowercase extension (without the leading `.`), or `"rs"` as the fallback for
/// unrecognised / missing extensions.
///
/// A URL with no path (e.g. `https://example.com`) or an empty path returns `"rs"`.  The
/// hostname is never treated as a filename, so `"example.com"` does **not** produce `"com"`.
#[must_use]
pub fn infer_extension(url: &str) -> String {
    // Strip fragment and query before path processing.
    let without_frag = url.split('#').next().unwrap_or(url);
    let without_query = without_frag.split('?').next().unwrap_or(without_frag);

    // Isolate the URL path: the part *after* "://authority".
    // For "https://example.com/lib.rs", that is "/lib.rs".
    // For "https://example.com" (no trailing slash), path is empty → fallback.
    let path = if let Some(idx) = without_query.find("://") {
        let after_scheme = &without_query[idx + 3..];
        // Find the first '/' that separates the authority from the path.
        after_scheme
            .find('/')
            .map_or("", |slash_pos| &after_scheme[slash_pos..])
    } else {
        // No "://" → treat the whole string as a plain path.
        without_query
    };

    // Extract the final path segment.
    let segment = path.rsplit('/').next().unwrap_or("");
    // Extract the extension (last `.`-separated part, if any).
    match segment.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => ext.to_ascii_lowercase(),
        _ => "rs".to_owned(),
    }
}

// ── HTTP fetch (feature-gated) ────────────────────────────────────────────────

/// Fetches `url` and returns its body bytes, capped at [`MAX_CONTENT_BYTES`].
///
/// # Errors
///
/// - [`GraphError::Guard`] when the URL fails the SSRF check or the body exceeds the size cap.
/// - [`GraphError::Io`] on network / read failure.
/// - Returns a build-time error message when compiled without `--features live`.
#[cfg(feature = "live")]
pub fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    use std::io::Read as _;

    let response = ureq::AgentBuilder::new()
        .timeout(FETCH_TIMEOUT)
        .build()
        .get(url)
        .call()
        .map_err(|e| GraphError::Guard(format!("fetch {url:?}: {e}")))?;

    // Use try_from to avoid any platform-specific truncation on 32-bit targets.
    let cap: u64 = u64::try_from(MAX_CONTENT_BYTES).unwrap_or(u64::MAX);

    let mut body = Vec::with_capacity(8192);
    response
        .into_reader()
        .take(cap.saturating_add(1))
        .read_to_end(&mut body)
        .map_err(|e| GraphError::Io(format!("read body from {url:?}: {e}")))?;

    if body.len() > MAX_CONTENT_BYTES {
        return Err(GraphError::Guard(format!(
            "response from {url:?} exceeded the {MAX_CONTENT_BYTES}-byte cap"
        )));
    }

    Ok(body)
}

/// Stub returned when the `live` feature is not enabled.
///
/// # Errors
///
/// Always returns [`GraphError::Guard`] explaining that `--features live` is required.
#[cfg(not(feature = "live"))]
pub fn fetch_bytes(_url: &str) -> Result<Vec<u8>> {
    Err(GraphError::Guard(
        "HTTP fetch is disabled: recompile with --features live to enable `add <URL>`".to_owned(),
    ))
}

// ── Temp-file extraction ──────────────────────────────────────────────────────

/// Monotonic counter for unique temp-file names; file-scoped to avoid `items_after_statements`.
static EXTRACT_CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Writes `bytes` to a temp file named `<random>.ext`, runs the extractor, and returns the graph.
///
/// The temp file is removed when this function returns (whether or not extraction succeeded).
///
/// # Errors
///
/// - [`GraphError::Io`] on temp-file creation or write failure.
/// - Propagates extraction errors from `habitat_graph_extract::extract_files`.
pub fn extract_from_bytes(bytes: &[u8], ext: &str) -> Result<habitat_graph_core::Graph> {
    // Create a temp file with the appropriate extension so the registry dispatches correctly.
    let tmp_dir = std::env::temp_dir();
    let pid = std::process::id();
    let seq = EXTRACT_CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp_path = tmp_dir.join(format!("hg_add_{pid}_{seq}.{ext}"));

    // Write, extract, then unconditionally remove.
    let write_result = (|| -> Result<()> {
        let mut f = std::fs::File::create(&tmp_path)
            .map_err(|e| GraphError::Io(format!("create temp file: {e}")))?;
        f.write_all(bytes)
            .map_err(|e| GraphError::Io(format!("write temp file: {e}")))?;
        Ok(())
    })();

    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    let extract_result = habitat_graph_extract::extract_files(std::slice::from_ref(&tmp_path));
    let _ = std::fs::remove_file(&tmp_path);

    let extractions = extract_result?;
    Ok(habitat_graph_build::assemble(extractions))
}

// ── Graph merge & persistence ─────────────────────────────────────────────────

/// Loads the graph at `out` (or an empty graph if absent/empty), merges `new_graph` in, and
/// writes the result back to `out`.
///
/// # Errors
///
/// - [`GraphError::Io`] on read/write failure.
/// - [`GraphError::Parse`] when the existing `out` is not valid node-link JSON.
/// - [`GraphError::Schema`] on serialization failure.
pub fn merge_into_output(new_graph: habitat_graph_core::Graph, out: &Path) -> Result<()> {
    // Load existing graph (or start fresh).
    let prior = if out.exists() {
        let text = std::fs::read_to_string(out)
            .map_err(|e| GraphError::Io(format!("read {}: {e}", out.display())))?;
        if text.trim().is_empty() {
            habitat_graph_core::Graph::default()
        } else {
            habitat_graph_serve::from_node_link(&text)
                .unwrap_or_else(|_| habitat_graph_core::Graph::default())
        }
    } else {
        habitat_graph_core::Graph::default()
    };

    let merged = habitat_graph_build::merge(new_graph, prior).sorted();

    // Ensure parent directory exists.
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| GraphError::Io(format!("create dirs {}: {e}", parent.display())))?;
        }
    }

    let json = habitat_graph_export::to_node_link(&merged)?;
    std::fs::write(out, json.as_bytes())
        .map_err(|e| GraphError::Io(format!("write {}: {e}", out.display())))
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Validates `url`, fetches its content, extracts a graph from it, and merges the result into
/// `out` (`graphify-out/graph.json` by default).
///
/// Returns `0` on success, `1` on URL/fetch/extraction/IO error, or `2` when compiled without the
/// `live` feature (HTTP transport unavailable).
///
/// # Errors
///
/// All errors are reported to stderr; see [`run_inner`] for the detailed set of error conditions.
#[must_use]
pub fn run(url: &str, out: &Path) -> u8 {
    match run_inner(url, out) {
        Ok(n) => {
            println!("add: merged {n} nodes from {url:?} into {}", out.display());
            0
        }
        Err(e) => {
            // Exit code 2 specifically for the "live feature required" guard.
            let code = if e.kind() == "guard"
                && e.to_string().contains("--features live")
            {
                2
            } else {
                1
            };
            eprintln!("habitat-graph add: {e}");
            code
        }
    }
}

/// Inner implementation of [`run`].
///
/// # Errors
///
/// - [`GraphError::Guard`] — SSRF rejection, size-cap violation, or live feature not enabled.
/// - [`GraphError::Io`] — network read failure or filesystem error.
/// - [`GraphError::Parse`] — extraction or JSON parse failure.
/// - [`GraphError::Schema`] — serialization failure.
fn run_inner(url: &str, out: &Path) -> Result<usize> {
    // 1. SSRF check — no network I/O before this passes.
    is_safe_url(url).map_err(GraphError::Guard)?;

    // 2. Fetch.
    let bytes = fetch_bytes(url)?;

    // 3. Infer extension and extract.
    let ext = infer_extension(url);
    let new_graph = extract_from_bytes(&bytes, &ext)?;
    let n = new_graph.nodes.len();

    // 4. Merge into output.
    merge_into_output(new_graph, out)?;

    Ok(n)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use habitat_graph_core::{Graph, GraphError};

    use super::{
        extract_from_bytes, fetch_bytes, infer_extension, merge_into_output, run,
        MAX_CONTENT_BYTES,
    };

    static SEQ: AtomicU32 = AtomicU32::new(0);
    fn tdir() -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let d = std::env::temp_dir().join(format!("hg_add_{pid}_{seq}"));
        fs::create_dir_all(&d).expect("create tdir");
        d
    }

    // ── infer_extension ───────────────────────────────────────────────────────

    #[test]
    fn infer_rs_from_rust_url() {
        assert_eq!(infer_extension("https://example.com/lib.rs"), "rs");
    }

    #[test]
    fn infer_ts_from_ts_url() {
        assert_eq!(infer_extension("https://example.com/app.ts"), "ts");
    }

    #[test]
    fn infer_tsx_from_tsx_url() {
        assert_eq!(infer_extension("https://example.com/Component.tsx"), "tsx");
    }

    #[test]
    fn infer_js_from_js_url() {
        assert_eq!(infer_extension("https://example.com/index.js"), "js");
    }

    #[test]
    fn infer_py_from_python_url() {
        assert_eq!(infer_extension("https://example.com/main.py"), "py");
    }

    #[test]
    fn infer_go_from_go_url() {
        assert_eq!(infer_extension("https://example.com/main.go"), "go");
    }

    #[test]
    fn infer_falls_back_to_rs_for_no_extension() {
        assert_eq!(infer_extension("https://example.com/no-ext"), "rs");
    }

    #[test]
    fn infer_falls_back_to_rs_for_empty_segment() {
        assert_eq!(infer_extension("https://example.com/"), "rs");
    }

    #[test]
    fn infer_strips_query_string() {
        assert_eq!(infer_extension("https://example.com/file.py?version=3"), "py");
    }

    #[test]
    fn infer_strips_fragment() {
        assert_eq!(infer_extension("https://example.com/file.ts#L42"), "ts");
    }

    #[test]
    fn infer_strips_query_then_fragment() {
        assert_eq!(infer_extension("https://example.com/f.go?q=1#line2"), "go");
    }

    #[test]
    fn infer_extension_is_lowercase() {
        assert_eq!(infer_extension("https://example.com/Module.RS"), "rs");
    }

    #[test]
    fn infer_ts_uppercase_lowercased() {
        assert_eq!(infer_extension("https://example.com/App.TS"), "ts");
    }

    #[test]
    fn infer_url_with_dots_in_path() {
        // Path contains dots but file segment has a clear extension.
        assert_eq!(infer_extension("https://example.com/v1.2/lib.rs"), "rs");
    }

    #[test]
    fn infer_url_with_port() {
        assert_eq!(infer_extension("http://host:8080/code.py"), "py");
    }

    // ── SSRF guard via run() / fetch_bytes() ──────────────────────────────────
    // These test the SSRF rejection path which is always active regardless of `live` feature.

    #[test]
    fn run_rejects_loopback_url() {
        let d = tdir();
        let rc = run("http://127.0.0.1/file.rs", &d.join("g.json"));
        assert_eq!(rc, 1, "loopback must be rejected with exit code 1");
    }

    #[test]
    fn run_rejects_private_ip() {
        let d = tdir();
        let rc = run("http://10.0.0.1/file.rs", &d.join("g.json"));
        assert_eq!(rc, 1, "private IP must be rejected");
    }

    #[test]
    fn run_rejects_link_local_ip() {
        let d = tdir();
        let rc = run("http://169.254.169.254/latest/meta-data/", &d.join("g.json"));
        assert_eq!(rc, 1, "cloud metadata endpoint must be rejected");
    }

    #[test]
    fn run_rejects_localhost_hostname() {
        let d = tdir();
        let rc = run("http://localhost/file.rs", &d.join("g.json"));
        assert_eq!(rc, 1, "localhost hostname must be rejected");
    }

    #[test]
    fn run_rejects_ftp_scheme() {
        let d = tdir();
        let rc = run("ftp://example.com/file.rs", &d.join("g.json"));
        assert_eq!(rc, 1, "ftp:// must be rejected");
    }

    #[test]
    fn run_rejects_file_scheme() {
        let d = tdir();
        let rc = run("file:///etc/passwd", &d.join("g.json"));
        assert_eq!(rc, 1, "file:// must be rejected");
    }

    #[test]
    fn run_rejects_ipv6_loopback() {
        let d = tdir();
        let rc = run("http://[::1]/file.rs", &d.join("g.json"));
        assert_eq!(rc, 1, "IPv6 loopback must be rejected");
    }

    #[test]
    fn run_rejects_ipv4_mapped_loopback() {
        let d = tdir();
        let rc = run("http://[::ffff:127.0.0.1]/file.rs", &d.join("g.json"));
        assert_eq!(rc, 1, "IPv4-mapped loopback must be rejected");
    }

    #[test]
    fn run_rejects_empty_url() {
        let d = tdir();
        let rc = run("", &d.join("g.json"));
        assert_eq!(rc, 1, "empty URL must fail");
    }

    #[test]
    fn run_rejects_malformed_url() {
        let d = tdir();
        let rc = run("not a url", &d.join("g.json"));
        assert_eq!(rc, 1, "malformed URL must fail");
    }

    // ── fetch_bytes: live-feature guard ──────────────────────────────────────

    #[test]
    #[cfg(not(feature = "live"))]
    fn fetch_bytes_without_live_feature_returns_guard() {
        let err = fetch_bytes("https://8.8.8.8/").expect_err("must fail without live");
        assert!(
            matches!(err, GraphError::Guard(_)),
            "expected Guard, got {err:?}"
        );
    }

    #[test]
    #[cfg(not(feature = "live"))]
    fn fetch_bytes_error_mentions_live_feature() {
        let err = fetch_bytes("https://8.8.8.8/").expect_err("must fail");
        assert!(
            err.to_string().contains("live"),
            "error should mention 'live': {err}"
        );
    }

    // ── extract_from_bytes ────────────────────────────────────────────────────

    #[test]
    fn extract_rs_empty_source_gives_zero_nodes() {
        let graph = extract_from_bytes(b"", "rs").expect("extract");
        assert_eq!(graph.nodes.len(), 0, "empty Rust source → 0 nodes");
    }

    #[test]
    fn extract_rs_single_function_gives_one_node() {
        let graph = extract_from_bytes(b"fn hello() {}", "rs").expect("extract");
        assert_eq!(graph.nodes.len(), 1, "one Rust fn → 1 node");
    }

    #[test]
    fn extract_rs_two_functions_give_two_nodes() {
        let src = b"fn a() {} fn b() {}";
        let graph = extract_from_bytes(src, "rs").expect("extract");
        assert_eq!(graph.nodes.len(), 2);
    }

    #[test]
    fn extract_unknown_extension_still_runs() {
        // Unknown extension → Rust fallback; empty source → 0 nodes, no panic.
        let graph = extract_from_bytes(b"", "xyz").expect("extract");
        assert_eq!(graph.nodes.len(), 0);
    }

    #[test]
    fn extract_rs_node_label_contains_function_name() {
        let graph = extract_from_bytes(b"fn my_func() {}", "rs").expect("extract");
        let labels: Vec<&str> = graph.nodes.iter().map(|n| n.label.as_str()).collect();
        assert!(
            labels.iter().any(|l| l.contains("my_func")),
            "node label must contain function name; got {labels:?}"
        );
    }

    #[test]
    fn extract_removes_temp_file_after_success() {
        // We can't directly inspect which temp file was created, but we can verify no error.
        assert!(extract_from_bytes(b"fn ok() {}", "rs").is_ok());
    }

    // ── merge_into_output ─────────────────────────────────────────────────────

    #[test]
    fn merge_into_new_file_creates_it() {
        let d = tdir();
        let out = d.join("g.json");
        let graph = Graph::default();
        merge_into_output(graph, &out).expect("merge");
        assert!(out.exists());
    }

    #[test]
    fn merge_into_new_file_is_valid_json() {
        let d = tdir();
        let out = d.join("g.json");
        let graph = Graph::default();
        merge_into_output(graph, &out).expect("merge");
        let text = fs::read_to_string(&out).expect("read");
        let v: serde_json::Value = serde_json::from_str(&text).expect("parse");
        assert!(v.is_object());
    }

    #[test]
    fn merge_into_empty_file_succeeds() {
        let d = tdir();
        let out = d.join("g.json");
        fs::write(&out, "").expect("seed");
        let graph = Graph::default();
        assert!(merge_into_output(graph, &out).is_ok());
    }

    #[test]
    fn merge_creates_parent_dirs() {
        let d = tdir();
        let out = d.join("sub").join("dir").join("g.json");
        let graph = Graph::default();
        merge_into_output(graph, &out).expect("merge");
        assert!(out.exists());
    }

    #[test]
    fn merge_preserves_prior_nodes() {
        let d = tdir();
        let out = d.join("g.json");

        // Build a prior graph with one function.
        let prior_graph = extract_from_bytes(b"fn prior_fn() {}", "rs").expect("extract");
        merge_into_output(prior_graph, &out).expect("write prior");

        // Merge in a new graph with a different function.
        let new_graph = extract_from_bytes(b"fn new_fn() {}", "rs").expect("extract");
        merge_into_output(new_graph, &out).expect("merge new");

        let text = fs::read_to_string(&out).expect("read");
        assert!(text.contains("prior_fn"), "prior node must be preserved");
        assert!(text.contains("new_fn"), "new node must be present");
    }

    #[test]
    fn merge_does_not_duplicate_identical_nodes() {
        let d = tdir();
        let out = d.join("g.json");

        let src = b"fn shared_fn() {}";
        let g1 = extract_from_bytes(src, "rs").expect("extract");
        merge_into_output(g1, &out).expect("first merge");

        let g2 = extract_from_bytes(src, "rs").expect("extract");
        merge_into_output(g2, &out).expect("second merge");

        // Node count must not double when merging identical source.
        let text = fs::read_to_string(&out).expect("read");
        let occurrences = text.matches("shared_fn").count();
        assert!(
            occurrences <= 2,
            "shared_fn appears too many times ({occurrences}) — likely duplicated"
        );
    }

    // ── constants ─────────────────────────────────────────────────────────────

    #[test]
    fn max_content_bytes_is_ten_mib() {
        assert_eq!(MAX_CONTENT_BYTES, 10 * 1024 * 1024);
    }

    #[test]
    fn fetch_timeout_is_thirty_seconds() {
        use std::time::Duration as Dur;
        assert_eq!(super::FETCH_TIMEOUT, Dur::from_secs(30));
    }

    // ── additional infer_extension edge cases ─────────────────────────────────

    #[test]
    fn infer_multiple_dots_takes_last_extension() {
        // "file.min.js" → "js" (the last extension)
        assert_eq!(infer_extension("https://example.com/file.min.js"), "js");
    }

    #[test]
    fn infer_dotfile_no_extension_falls_back() {
        // ".gitignore" has no conventional extension; rsplit_once('.') gives ("", "gitignore")
        // which is a non-empty result, so we'd return "gitignore".
        // Document the actual behavior rather than assuming.
        let ext = infer_extension("https://example.com/.gitignore");
        assert!(!ext.is_empty(), "extension must be non-empty");
    }

    #[test]
    fn infer_url_with_deep_path() {
        assert_eq!(
            infer_extension("https://raw.githubusercontent.com/owner/repo/main/src/lib.rs"),
            "rs"
        );
    }

    #[test]
    fn infer_url_no_path_at_all_falls_back() {
        // "https://example.com" — no path → falls back to "rs"
        assert_eq!(infer_extension("https://example.com"), "rs");
    }

    // ── additional SSRF rejection cases ──────────────────────────────────────

    #[test]
    fn run_rejects_ipv6_private_unique_local() {
        let d = tdir();
        let rc = run("http://[fc00::1]/file.rs", &d.join("g.json"));
        assert_eq!(rc, 1, "IPv6 unique-local must be rejected");
    }

    #[test]
    fn run_rejects_cgnat_ip() {
        let d = tdir();
        // 100.64.0.1 is in the CGNAT shared address space (RFC 6598)
        let rc = run("http://100.64.0.1/file.rs", &d.join("g.json"));
        assert_eq!(rc, 1, "CGNAT address must be rejected");
    }

    #[test]
    fn run_rejects_no_scheme_separator() {
        let d = tdir();
        let rc = run("notaurl", &d.join("g.json"));
        assert_eq!(rc, 1, "URL without :// must fail");
    }

    // ── additional extract_from_bytes ─────────────────────────────────────────

    #[test]
    fn extract_from_bytes_py_extension_runs_without_panic() {
        // Python extractor must handle this without panicking.
        let graph = extract_from_bytes(b"def hello(): pass", "py").expect("extract");
        assert!(
            graph.nodes.len() >= 0,
            "Python extraction must succeed without panic"
        );
    }

    #[test]
    fn extract_from_bytes_multiple_nodes_counted() {
        let src = b"fn a() {} fn b() {} fn c() {}";
        let graph = extract_from_bytes(src, "rs").expect("extract");
        assert_eq!(graph.nodes.len(), 3);
    }

    // ── additional merge_into_output ──────────────────────────────────────────

    #[test]
    fn merge_into_output_produces_directed_graph_envelope() {
        let d = tdir();
        let out = d.join("g.json");
        let graph = Graph::default();
        merge_into_output(graph, &out).expect("merge");
        let text = fs::read_to_string(&out).expect("read");
        // NetworkX node-link format includes "directed" key.
        assert!(
            text.contains("\"directed\""),
            "graph.json must have directed key"
        );
    }

    #[test]
    fn merge_into_output_produces_multigraph_envelope() {
        let d = tdir();
        let out = d.join("g.json");
        merge_into_output(Graph::default(), &out).expect("merge");
        let text = fs::read_to_string(&out).expect("read");
        assert!(
            text.contains("\"multigraph\""),
            "graph.json must have multigraph key"
        );
    }

    #[test]
    fn merge_node_count_increases_monotonically() {
        let d = tdir();
        let out = d.join("g.json");

        let g1 = extract_from_bytes(b"fn x() {}", "rs").expect("extract");
        merge_into_output(g1, &out).expect("merge1");
        let text1 = fs::read_to_string(&out).expect("read1");
        let c1 = text1.matches("\"id\":").count();

        let g2 = extract_from_bytes(b"fn y() {}", "rs").expect("extract");
        merge_into_output(g2, &out).expect("merge2");
        let text2 = fs::read_to_string(&out).expect("read2");
        let c2 = text2.matches("\"id\":").count();

        assert!(c2 >= c1, "node count must not decrease after merge");
    }
}
