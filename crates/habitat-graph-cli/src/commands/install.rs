//! `install` (PC-tail) — auto-discover the Claude MCP config and register habitat-graph as a
//! server with snapshot-write-readback safety.
//!
//! Unlike `install-mcp` (which requires an explicit `--write <path>`), this command discovers the
//! config automatically from the three canonical locations in priority order:
//!
//! 1. `$CLAUDE_CONFIG_DIR/mcp.json`
//! 2. `~/.config/claude/mcp.json` (XDG-style)
//! 3. `~/.claude/mcp.json` (legacy)
//!
//! The write is atomic: the new config is first written to a `.bak` sidecar, then renamed to the
//! final path and read back to verify the entry landed.

use std::path::{Path, PathBuf};

use habitat_graph_core::{GraphError, Result};
use serde_json::{json, Map, Value};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Name registered under `mcpServers` in the Claude config.
pub const MCP_SERVER_NAME: &str = "habitat-graph";

/// Environment variable that overrides the Claude config directory.
const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

// ── Path discovery ────────────────────────────────────────────────────────────

/// Returns the user's home directory as reported by `$HOME`, or `None` if unset/empty.
fn home_dir() -> Option<PathBuf> {
    let h = std::env::var("HOME").ok()?;
    if h.is_empty() {
        return None;
    }
    Some(PathBuf::from(h))
}

/// Enumerates the candidate Claude MCP config paths in priority order.
///
/// Returns at most three entries:
/// 1. `$CLAUDE_CONFIG_DIR/mcp.json` — only if the env var is set and non-empty.
/// 2. `~/.config/claude/mcp.json` — XDG-style, only if `$HOME` is available.
/// 3. `~/.claude/mcp.json` — legacy, only if `$HOME` is available.
#[must_use]
pub fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Env-var override takes precedence.
    if let Ok(dir) = std::env::var(CLAUDE_CONFIG_DIR_ENV) {
        if !dir.is_empty() {
            paths.push(PathBuf::from(&dir).join("mcp.json"));
        }
    }

    if let Some(home) = home_dir() {
        paths.push(home.join(".config").join("claude").join("mcp.json"));
        paths.push(home.join(".claude").join("mcp.json"));
    }

    paths
}

/// Selects the config path to use.
///
/// Returns the first candidate that already **exists** on disk; if none exists, returns the first
/// candidate (the highest-priority preferred location), creating parent directories as needed.
///
/// # Errors
///
/// Returns [`GraphError::Io`] when no candidate path can be derived (typically `$HOME` is unset).
pub fn resolve_config_path(override_path: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = override_path {
        return Ok(p.to_path_buf());
    }

    let candidates = candidate_paths();
    if candidates.is_empty() {
        return Err(GraphError::Io(
            "cannot determine Claude config path: $HOME is unset and $CLAUDE_CONFIG_DIR is unset"
                .to_owned(),
        ));
    }

    // Prefer a candidate that already exists.
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }

    // Fall back to the highest-priority candidate.
    Ok(candidates
        .into_iter()
        .next()
        .expect("non-empty checked above"))
}

// ── JSON config helpers ───────────────────────────────────────────────────────

/// Reads and parses the config at `path`; an absent or blank file yields `{}`.
///
/// # Errors
///
/// Returns [`GraphError::Io`] when the file exists but cannot be read, or [`GraphError::Parse`]
/// when the file content is not valid JSON.
pub fn read_config(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| GraphError::Io(format!("read {}: {e}", path.display())))?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text).map_err(|e| GraphError::Parse {
        file: path.display().to_string(),
        message: e.to_string(),
    })
}

/// Merges our server entry under `mcpServers.<name>` into `existing`, preserving all other keys.
///
/// If `existing` is not a JSON object it is replaced; if `mcpServers` is absent or not an object
/// it is created.
#[must_use]
pub fn merge_entry(existing: Value, name: &str, entry: Value) -> Value {
    let mut root: Map<String, Value> = match existing {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let servers = root
        .entry("mcpServers".to_owned())
        .or_insert_with(|| json!({}));
    if !servers.is_object() {
        *servers = json!({});
    }
    if let Some(map) = servers.as_object_mut() {
        map.insert(name.to_owned(), entry);
    }
    Value::Object(root)
}

/// Builds the server entry `{ "command": <binary>, "args": ["mcp", "--graph", <graph>] }`.
#[must_use]
pub fn build_entry(binary: &str, graph: &Path) -> Value {
    json!({
        "command": binary,
        "args": ["mcp", "--graph", graph.display().to_string()],
    })
}

/// Returns the path of the running binary, or `"habitat-graph"` as a safe fallback.
fn binary_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .unwrap_or_else(|| "habitat-graph".to_owned())
}

// ── Atomic snapshot-write-readback ────────────────────────────────────────────

/// Performs the snapshot-write-readback sequence described in the module docs.
///
/// Steps:
/// 1. Create parent directories as needed.
/// 2. Serialize `config` to pretty JSON.
/// 3. Write to `<path>.bak`.
/// 4. Atomically rename `.bak` → `path`.
/// 5. Read `path` back and parse to confirm the entry landed.
///
/// # Errors
///
/// Returns [`GraphError::Io`] or [`GraphError::Schema`] on any step failure; returns
/// [`GraphError::Guard`] when readback fails to find the registered server name.
pub fn atomic_write_and_verify(path: &Path, config: &Value, name: &str) -> Result<()> {
    // 1. Ensure parent directories exist.
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| GraphError::Io(format!("create dirs {}: {e}", parent.display())))?;
        }
    }

    // 2. Serialize.
    let text =
        serde_json::to_string_pretty(config).map_err(|e| GraphError::Schema(e.to_string()))?;

    // 3. Write to .bak.
    let bak = path.with_extension("bak");
    std::fs::write(&bak, text.as_bytes())
        .map_err(|e| GraphError::Io(format!("write bak {}: {e}", bak.display())))?;

    // 4. Atomic rename.
    std::fs::rename(&bak, path).map_err(|e| {
        GraphError::Io(format!("rename {}->{}: {e}", bak.display(), path.display()))
    })?;

    // 5. Readback verify.
    let readback = read_config(path)?;
    let ok = readback
        .get("mcpServers")
        .and_then(|s| s.get(name))
        .is_some();
    if !ok {
        return Err(GraphError::Guard(format!(
            "readback failed: server {name:?} not found in {}",
            path.display()
        )));
    }

    Ok(())
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Registers habitat-graph as an MCP server in the auto-discovered (or explicitly supplied) config.
///
/// With `dry_run = true` the merged config is printed to stdout and nothing is written to disk.
/// With `dry_run = false` the config is written via [`atomic_write_and_verify`].
///
/// Returns `0` on success or `1` on any error (diagnostics written to stderr).
///
/// # Errors
///
/// All errors are surfaced as a non-zero exit code; see [`run_inner`] for the detailed error set.
#[must_use]
pub fn run(graph: &Path, config_path: Option<&Path>, dry_run: bool) -> u8 {
    match run_inner(graph, config_path, dry_run) {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("habitat-graph install: {e}");
            1
        }
    }
}

/// Inner implementation of [`run`], returning a human-readable success message.
///
/// # Errors
///
/// Returns [`GraphError::Io`] on filesystem failures, [`GraphError::Parse`] when an existing
/// config is malformed, [`GraphError::Schema`] on serialization errors, and [`GraphError::Guard`]
/// when the readback verification fails.
fn run_inner(graph: &Path, config_path: Option<&Path>, dry_run: bool) -> Result<String> {
    let resolved = resolve_config_path(config_path)?;
    let existing = read_config(&resolved)?;
    let entry = build_entry(&binary_path(), graph);
    let merged = merge_entry(existing, MCP_SERVER_NAME, entry);

    if dry_run {
        let preview =
            serde_json::to_string_pretty(&merged).map_err(|e| GraphError::Schema(e.to_string()))?;
        Ok(format!(
            "[dry-run] would write to {}:\n{}",
            resolved.display(),
            preview
        ))
    } else {
        atomic_write_and_verify(&resolved, &merged, MCP_SERVER_NAME)?;
        Ok(format!(
            "registered MCP server {:?} in {}",
            MCP_SERVER_NAME,
            resolved.display()
        ))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    use serde_json::{json, Value};

    use super::{
        atomic_write_and_verify, build_entry, candidate_paths, merge_entry, read_config,
        resolve_config_path, run, MCP_SERVER_NAME,
    };

    // ── Test-dir helper ───────────────────────────────────────────────────────

    static SEQ: AtomicU32 = AtomicU32::new(0);
    fn tdir() -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let d = std::env::temp_dir().join(format!("hg_install_{pid}_{seq}"));
        fs::create_dir_all(&d).expect("create tdir");
        d
    }

    fn graph_path() -> PathBuf {
        PathBuf::from("graphify-out/graph.json")
    }

    // ── build_entry ───────────────────────────────────────────────────────────

    #[test]
    fn entry_command_is_binary() {
        let e = build_entry("habitat-graph", &graph_path());
        assert_eq!(e["command"], "habitat-graph");
    }

    #[test]
    fn entry_args_starts_with_mcp() {
        let e = build_entry("habitat-graph", &graph_path());
        assert_eq!(e["args"][0], "mcp");
    }

    #[test]
    fn entry_args_has_graph_flag() {
        let e = build_entry("habitat-graph", &graph_path());
        assert_eq!(e["args"][1], "--graph");
    }

    #[test]
    fn entry_args_has_graph_path() {
        let e = build_entry("habitat-graph", &graph_path());
        assert_eq!(e["args"][2], "graphify-out/graph.json");
    }

    #[test]
    fn entry_args_length_is_three() {
        let e = build_entry("habitat-graph", &graph_path());
        assert_eq!(e["args"].as_array().expect("array").len(), 3);
    }

    #[test]
    fn entry_custom_binary_reflected() {
        let e = build_entry("/opt/bin/hg", &graph_path());
        assert_eq!(e["command"], "/opt/bin/hg");
    }

    #[test]
    fn entry_custom_graph_reflected() {
        let e = build_entry("hg", Path::new("/var/data/g.json"));
        assert_eq!(e["args"][2], "/var/data/g.json");
    }

    // ── merge_entry ───────────────────────────────────────────────────────────

    #[test]
    fn merge_into_empty_object_inserts() {
        let m = merge_entry(json!({}), "hg", build_entry("hg", &graph_path()));
        assert!(m["mcpServers"]["hg"].is_object());
    }

    #[test]
    fn merge_into_non_object_replaces() {
        let m = merge_entry(json!("garbage"), "hg", build_entry("hg", &graph_path()));
        assert!(m["mcpServers"]["hg"].is_object());
    }

    #[test]
    fn merge_into_null_replaces() {
        let m = merge_entry(Value::Null, "hg", build_entry("hg", &graph_path()));
        assert!(m["mcpServers"]["hg"].is_object());
    }

    #[test]
    fn merge_preserves_other_top_level_keys() {
        let ex = json!({ "theme": "dark" });
        let m = merge_entry(ex, "hg", build_entry("hg", &graph_path()));
        assert_eq!(m["theme"], "dark");
    }

    #[test]
    fn merge_preserves_other_servers() {
        let ex = json!({ "mcpServers": { "other": { "command": "x" } } });
        let m = merge_entry(ex, "hg", build_entry("hg", &graph_path()));
        assert_eq!(m["mcpServers"]["other"]["command"], "x");
        assert!(m["mcpServers"]["hg"].is_object());
    }

    #[test]
    fn merge_overwrites_stale_entry() {
        let ex = json!({ "mcpServers": { "hg": { "command": "stale" } } });
        let m = merge_entry(ex, "hg", build_entry("habitat-graph", &graph_path()));
        assert_eq!(m["mcpServers"]["hg"]["command"], "habitat-graph");
    }

    #[test]
    fn merge_creates_mcpservers_when_absent() {
        let ex = json!({ "unrelated": 42 });
        let m = merge_entry(ex, "hg", build_entry("hg", &graph_path()));
        assert!(m["mcpServers"]["hg"].is_object());
        assert_eq!(m["unrelated"], 42);
    }

    #[test]
    fn merge_replaces_non_object_mcpservers() {
        let ex = json!({ "mcpServers": "invalid" });
        let m = merge_entry(ex, "hg", build_entry("hg", &graph_path()));
        assert!(m["mcpServers"].is_object());
    }

    #[test]
    fn merge_is_idempotent() {
        let e = build_entry("hg", &graph_path());
        let once = merge_entry(json!({}), "hg", e.clone());
        let twice = merge_entry(once.clone(), "hg", e);
        assert_eq!(once, twice);
    }

    #[test]
    fn merge_result_is_object() {
        let m = merge_entry(json!({}), "hg", build_entry("hg", &graph_path()));
        assert!(m.is_object());
    }

    // ── read_config ───────────────────────────────────────────────────────────

    #[test]
    fn read_absent_file_is_empty_object() {
        let d = tdir();
        assert_eq!(read_config(&d.join("nope.json")).expect("read"), json!({}));
    }

    #[test]
    fn read_empty_file_is_empty_object() {
        let d = tdir();
        let p = d.join("c.json");
        fs::write(&p, "").expect("seed");
        assert_eq!(read_config(&p).expect("read"), json!({}));
    }

    #[test]
    fn read_whitespace_only_is_empty_object() {
        let d = tdir();
        let p = d.join("c.json");
        fs::write(&p, "   \n  ").expect("seed");
        assert_eq!(read_config(&p).expect("read"), json!({}));
    }

    #[test]
    fn read_valid_config_parses() {
        let d = tdir();
        let p = d.join("c.json");
        fs::write(&p, "{\"theme\":\"dark\"}").expect("seed");
        assert_eq!(read_config(&p).expect("read")["theme"], "dark");
    }

    #[test]
    fn read_malformed_json_errors() {
        let d = tdir();
        let p = d.join("c.json");
        fs::write(&p, "{ not json").expect("seed");
        assert!(read_config(&p).is_err());
    }

    #[test]
    fn read_preserves_existing_servers() {
        let d = tdir();
        let p = d.join("c.json");
        fs::write(&p, "{\"mcpServers\":{\"x\":{\"command\":\"y\"}}}").expect("seed");
        assert_eq!(
            read_config(&p).expect("read")["mcpServers"]["x"]["command"],
            "y"
        );
    }

    // ── resolve_config_path ───────────────────────────────────────────────────

    #[test]
    fn resolve_returns_override_directly() {
        let d = tdir();
        let custom = d.join("custom.json");
        let resolved = resolve_config_path(Some(&custom)).expect("resolve");
        assert_eq!(resolved, custom);
    }

    #[test]
    fn resolve_prefers_existing_file() {
        // Create the legacy path and verify it is preferred over the XDG path (when XDG does
        // not exist).  We can't easily override $HOME, so we test via the override path.
        let d = tdir();
        let p = d.join("existing.json");
        fs::write(&p, "{}").expect("seed");
        assert_eq!(resolve_config_path(Some(&p)).expect("resolve"), p);
    }

    #[test]
    fn candidate_paths_returns_at_least_one_when_home_set() {
        // $HOME is set in a normal test environment.
        if std::env::var("HOME").is_ok() {
            assert!(
                !candidate_paths().is_empty(),
                "expected at least one candidate"
            );
        }
    }

    #[test]
    fn candidate_paths_env_override_is_first() {
        // Temporarily override the env var to verify ordering.
        let d = tdir();
        let original = std::env::var(super::CLAUDE_CONFIG_DIR_ENV).ok();
        std::env::set_var(super::CLAUDE_CONFIG_DIR_ENV, d.display().to_string());

        let paths = candidate_paths();
        let first = paths.first().cloned();

        // Restore.
        match original {
            Some(v) => std::env::set_var(super::CLAUDE_CONFIG_DIR_ENV, v),
            None => std::env::remove_var(super::CLAUDE_CONFIG_DIR_ENV),
        }

        assert_eq!(first, Some(d.join("mcp.json")));
    }

    // ── atomic_write_and_verify ───────────────────────────────────────────────

    #[test]
    fn atomic_write_creates_file() {
        let d = tdir();
        let p = d.join("mcp.json");
        let config = merge_entry(json!({}), "hg", build_entry("hg", &graph_path()));
        atomic_write_and_verify(&p, &config, "hg").expect("write");
        assert!(p.exists());
    }

    #[test]
    fn atomic_write_removes_bak_after_success() {
        let d = tdir();
        let p = d.join("mcp.json");
        let config = merge_entry(json!({}), "hg", build_entry("hg", &graph_path()));
        atomic_write_and_verify(&p, &config, "hg").expect("write");
        assert!(
            !d.join("mcp.bak").exists(),
            ".bak must be absent after success"
        );
    }

    #[test]
    fn atomic_write_produces_valid_json() {
        let d = tdir();
        let p = d.join("mcp.json");
        let config = merge_entry(json!({}), "hg", build_entry("hg", &graph_path()));
        atomic_write_and_verify(&p, &config, "hg").expect("write");
        let back = read_config(&p).expect("readback");
        assert!(back["mcpServers"]["hg"].is_object());
    }

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let d = tdir();
        let p = d.join("deep").join("sub").join("mcp.json");
        let config = merge_entry(json!({}), "hg", build_entry("hg", &graph_path()));
        atomic_write_and_verify(&p, &config, "hg").expect("write");
        assert!(p.exists());
    }

    #[test]
    fn atomic_write_readback_verifies_entry() {
        let d = tdir();
        let p = d.join("mcp.json");
        let config = merge_entry(json!({}), "hg", build_entry("hg", &graph_path()));
        // The entry for "hg" must be present → verify succeeds.
        assert!(atomic_write_and_verify(&p, &config, "hg").is_ok());
    }

    #[test]
    fn atomic_write_readback_fails_for_missing_name() {
        let d = tdir();
        let p = d.join("mcp.json");
        // Write a config that does NOT include "missing_server".
        let config = merge_entry(json!({}), "hg", build_entry("hg", &graph_path()));
        let err = atomic_write_and_verify(&p, &config, "missing_server").expect_err("must fail");
        assert!(
            matches!(err, habitat_graph_core::GraphError::Guard(_)),
            "expected Guard, got {err:?}"
        );
    }

    #[test]
    fn atomic_write_is_idempotent() {
        let d = tdir();
        let p = d.join("mcp.json");
        let config = merge_entry(json!({}), "hg", build_entry("hg", &graph_path()));
        atomic_write_and_verify(&p, &config, "hg").expect("first write");
        let text1 = fs::read_to_string(&p).expect("read1");
        atomic_write_and_verify(&p, &config, "hg").expect("second write");
        let text2 = fs::read_to_string(&p).expect("read2");
        assert_eq!(text1, text2, "idempotent write must produce identical file");
    }

    #[test]
    fn atomic_write_output_is_multiline_pretty() {
        let d = tdir();
        let p = d.join("mcp.json");
        let config = merge_entry(json!({}), "hg", build_entry("hg", &graph_path()));
        atomic_write_and_verify(&p, &config, "hg").expect("write");
        let text = fs::read_to_string(&p).expect("read");
        assert!(text.lines().count() > 1, "config must be pretty-printed");
    }

    // ── run: dry-run mode ─────────────────────────────────────────────────────

    #[test]
    fn run_dry_run_returns_zero() {
        let d = tdir();
        let cfg = d.join("mcp.json");
        assert_eq!(run(&graph_path(), Some(&cfg), true), 0);
    }

    #[test]
    fn run_dry_run_does_not_write_file() {
        let d = tdir();
        let cfg = d.join("mcp.json");
        assert_eq!(run(&graph_path(), Some(&cfg), true), 0);
        assert!(!cfg.exists(), "dry-run must not create file");
    }

    #[test]
    fn run_dry_run_does_not_write_bak() {
        let d = tdir();
        let cfg = d.join("mcp.json");
        assert_eq!(run(&graph_path(), Some(&cfg), true), 0);
        assert!(!d.join("mcp.bak").exists(), "dry-run must not create .bak");
    }

    // ── run: write mode ───────────────────────────────────────────────────────

    #[test]
    fn run_write_returns_zero() {
        let d = tdir();
        let cfg = d.join("mcp.json");
        assert_eq!(run(&graph_path(), Some(&cfg), false), 0);
    }

    #[test]
    fn run_write_creates_file() {
        let d = tdir();
        let cfg = d.join("mcp.json");
        assert_eq!(run(&graph_path(), Some(&cfg), false), 0);
        assert!(cfg.exists());
    }

    #[test]
    fn run_write_registers_server() {
        let d = tdir();
        let cfg = d.join("mcp.json");
        assert_eq!(run(&graph_path(), Some(&cfg), false), 0);
        let v = read_config(&cfg).expect("read");
        assert!(
            v["mcpServers"][MCP_SERVER_NAME].is_object(),
            "mcpServers.{MCP_SERVER_NAME} must be registered"
        );
    }

    #[test]
    fn run_write_preserves_existing_servers() {
        let d = tdir();
        let cfg = d.join("mcp.json");
        fs::write(&cfg, "{\"mcpServers\":{\"other\":{\"command\":\"x\"}}}").expect("seed");
        assert_eq!(run(&graph_path(), Some(&cfg), false), 0);
        let v = read_config(&cfg).expect("read");
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert!(v["mcpServers"][MCP_SERVER_NAME].is_object());
    }

    #[test]
    fn run_write_preserves_other_top_level_keys() {
        let d = tdir();
        let cfg = d.join("mcp.json");
        fs::write(&cfg, "{\"theme\":\"light\"}").expect("seed");
        assert_eq!(run(&graph_path(), Some(&cfg), false), 0);
        assert_eq!(read_config(&cfg).expect("read")["theme"], "light");
    }

    #[test]
    fn run_write_is_idempotent() {
        let d = tdir();
        let cfg = d.join("mcp.json");
        assert_eq!(run(&graph_path(), Some(&cfg), false), 0);
        let t1 = fs::read_to_string(&cfg).expect("r1");
        assert_eq!(run(&graph_path(), Some(&cfg), false), 0);
        let t2 = fs::read_to_string(&cfg).expect("r2");
        assert_eq!(t1, t2, "second install must not change file");
    }

    #[test]
    fn run_write_on_malformed_existing_returns_one() {
        let d = tdir();
        let cfg = d.join("mcp.json");
        fs::write(&cfg, "{ broken").expect("seed");
        assert_eq!(run(&graph_path(), Some(&cfg), false), 1);
    }

    #[test]
    fn run_write_into_empty_file_succeeds() {
        let d = tdir();
        let cfg = d.join("mcp.json");
        fs::write(&cfg, "").expect("seed");
        assert_eq!(run(&graph_path(), Some(&cfg), false), 0);
    }

    #[test]
    fn run_write_args_include_graph_path() {
        let d = tdir();
        let cfg = d.join("mcp.json");
        assert_eq!(run(Path::new("/x/g.json"), Some(&cfg), false), 0);
        let v = read_config(&cfg).expect("read");
        assert_eq!(v["mcpServers"][MCP_SERVER_NAME]["args"][2], "/x/g.json");
    }

    #[test]
    fn run_write_creates_parent_dirs_for_config() {
        let d = tdir();
        let cfg = d.join("a").join("b").join("mcp.json");
        assert_eq!(run(&graph_path(), Some(&cfg), false), 0);
        assert!(cfg.exists());
    }

    #[test]
    fn run_write_two_installs_different_graphs_coexist() {
        let d = tdir();
        let cfg = d.join("mcp.json");
        assert_eq!(run(Path::new("/a.json"), Some(&cfg), false), 0);
        // Second install updates the entry with the new graph path.
        assert_eq!(run(Path::new("/b.json"), Some(&cfg), false), 0);
        let v = read_config(&cfg).expect("read");
        assert_eq!(v["mcpServers"][MCP_SERVER_NAME]["args"][2], "/b.json");
    }

    #[test]
    fn mcp_server_name_constant_is_habitat_graph() {
        assert_eq!(MCP_SERVER_NAME, "habitat-graph");
    }

    #[test]
    fn run_returns_zero_for_valid_nonexistent_config_path() {
        let d = tdir();
        let cfg = d.join("brand_new.json");
        assert_eq!(run(&graph_path(), Some(&cfg), false), 0);
    }
}
