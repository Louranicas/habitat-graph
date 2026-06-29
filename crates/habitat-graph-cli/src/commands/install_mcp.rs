//! `install-mcp` (PC-tail) — register habitat-graph as a Model Context Protocol server in a Claude
//! Code config so an agent can mount the graph organ.
//!
//! By default [`run`] prints the `mcpServers` JSON block to paste into a config. With `--write` it
//! merges the entry into an existing config file (e.g. `.mcp.json` / `~/.claude.json`) under
//! `mcpServers.<name>`, preserving every other server and top-level key. The merge is pure
//! ([`merge_into_config`]); only the optional file write touches disk.

use std::path::Path;

use habitat_graph_core::{GraphError, Result};
use serde_json::{json, Map, Value};

/// Default MCP server name registered in the config.
pub const DEFAULT_SERVER_NAME: &str = "habitat-graph";

/// Returns the path of the running binary, or `"habitat-graph"` if it cannot be resolved.
fn binary_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .unwrap_or_else(|| "habitat-graph".to_owned())
}

/// Builds the MCP server entry: `{ "command": <binary>, "args": ["mcp", "--graph", <graph>] }`.
fn server_entry(binary: &str, graph: &Path) -> Value {
    json!({
        "command": binary,
        "args": ["mcp", "--graph", graph.display().to_string()],
    })
}

/// Merges `entry` into `existing` under `mcpServers.<name>`, preserving every other key.
///
/// If `existing` is not a JSON object it is replaced by a fresh object; if `mcpServers` is absent
/// or not an object it is (re)created. An existing entry with the same `name` is overwritten.
#[must_use]
fn merge_into_config(existing: Value, name: &str, entry: Value) -> Value {
    let mut root: Map<String, Value> = match existing {
        Value::Object(map) => map,
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

/// Reads a config file into a [`Value`]; an absent or empty file yields an empty object.
fn read_config(path: &Path) -> Result<Value> {
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

/// Serializes `value` to pretty JSON.
fn to_pretty(value: &Value) -> Result<String> {
    serde_json::to_string_pretty(value).map_err(|e| GraphError::Schema(e.to_string()))
}

/// Registers habitat-graph as an MCP server.
///
/// With `write = None`, prints the standalone `mcpServers` block. With `write = Some(path)`, merges
/// the entry into the config at `path` (creating it if absent) and writes it back. Returns `0` on
/// success or `1` on an I/O / parse / serialization error.
#[must_use]
pub fn run(graph: &Path, name: &str, write: Option<&Path>) -> u8 {
    match run_inner(graph, name, write) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("habitat-graph install-mcp: {e}");
            1
        }
    }
}

fn run_inner(graph: &Path, name: &str, write: Option<&Path>) -> Result<()> {
    let entry = server_entry(&binary_path(), graph);
    match write {
        None => {
            let block = json!({ "mcpServers": { name: entry } });
            println!("{}", to_pretty(&block)?);
            println!("# add the above to your Claude Code MCP config (e.g. .mcp.json)");
            Ok(())
        }
        Some(path) => {
            let merged = merge_into_config(read_config(path)?, name, entry);
            let text = to_pretty(&merged)?;
            std::fs::write(path, text)
                .map_err(|e| GraphError::Io(format!("write {}: {e}", path.display())))?;
            println!("registered MCP server {name:?} in {}", path.display());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use serde_json::{json, Value};

    use super::{
        binary_path, merge_into_config, read_config, run, server_entry, to_pretty,
        DEFAULT_SERVER_NAME,
    };

    static COUNTER: AtomicU32 = AtomicU32::new(0);
    fn tdir() -> PathBuf {
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("hg_mcp_{pid}_{seq}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn graph_path() -> PathBuf {
        PathBuf::from("graphify-out/graph.json")
    }

    fn entry() -> Value {
        server_entry("habitat-graph", &graph_path())
    }

    // ── server_entry ──────────────────────────────────────────────────────────

    #[test]
    fn entry_has_command() {
        assert_eq!(entry()["command"], "habitat-graph");
    }

    #[test]
    fn entry_command_uses_given_binary() {
        let e = server_entry("/opt/bin/habitat-graph", &graph_path());
        assert_eq!(e["command"], "/opt/bin/habitat-graph");
    }

    #[test]
    fn entry_args_start_with_mcp() {
        assert_eq!(entry()["args"][0], "mcp");
    }

    #[test]
    fn entry_args_pass_graph_flag() {
        assert_eq!(entry()["args"][1], "--graph");
    }

    #[test]
    fn entry_args_contain_graph_path() {
        assert_eq!(entry()["args"][2], "graphify-out/graph.json");
    }

    #[test]
    fn entry_has_three_args() {
        assert_eq!(entry()["args"].as_array().expect("array").len(), 3);
    }

    #[test]
    fn entry_graph_path_reflects_input() {
        let e = server_entry("habitat-graph", std::path::Path::new("/data/g.json"));
        assert_eq!(e["args"][2], "/data/g.json");
    }

    // ── merge_into_config: from empty / non-object ────────────────────────────

    #[test]
    fn merge_into_empty_object_adds_server() {
        let merged = merge_into_config(json!({}), "habitat-graph", entry());
        assert!(merged["mcpServers"]["habitat-graph"].is_object());
    }

    #[test]
    fn merge_into_empty_sets_command() {
        let merged = merge_into_config(json!({}), "habitat-graph", entry());
        assert_eq!(merged["mcpServers"]["habitat-graph"]["command"], "habitat-graph");
    }

    #[test]
    fn merge_into_non_object_starts_fresh() {
        let merged = merge_into_config(json!("garbage"), "hg", entry());
        assert!(merged["mcpServers"]["hg"].is_object());
    }

    #[test]
    fn merge_into_null_starts_fresh() {
        let merged = merge_into_config(Value::Null, "hg", entry());
        assert!(merged["mcpServers"]["hg"].is_object());
    }

    #[test]
    fn merge_into_array_starts_fresh() {
        let merged = merge_into_config(json!([1, 2, 3]), "hg", entry());
        assert!(merged["mcpServers"]["hg"].is_object());
    }

    // ── merge_into_config: preserve existing ──────────────────────────────────

    #[test]
    fn merge_preserves_other_top_level_keys() {
        let existing = json!({ "theme": "dark", "mcpServers": {} });
        let merged = merge_into_config(existing, "hg", entry());
        assert_eq!(merged["theme"], "dark");
    }

    #[test]
    fn merge_preserves_other_servers() {
        let existing = json!({ "mcpServers": { "other": { "command": "x" } } });
        let merged = merge_into_config(existing, "hg", entry());
        assert_eq!(merged["mcpServers"]["other"]["command"], "x");
        assert!(merged["mcpServers"]["hg"].is_object());
    }

    #[test]
    fn merge_overwrites_same_name() {
        let existing = json!({ "mcpServers": { "hg": { "command": "stale" } } });
        let merged = merge_into_config(existing, "hg", entry());
        assert_eq!(merged["mcpServers"]["hg"]["command"], "habitat-graph");
    }

    #[test]
    fn merge_creates_mcpservers_when_absent() {
        let existing = json!({ "unrelated": true });
        let merged = merge_into_config(existing, "hg", entry());
        assert!(merged["mcpServers"]["hg"].is_object());
        assert_eq!(merged["unrelated"], true);
    }

    #[test]
    fn merge_replaces_non_object_mcpservers() {
        let existing = json!({ "mcpServers": "oops" });
        let merged = merge_into_config(existing, "hg", entry());
        assert!(merged["mcpServers"].is_object());
        assert!(merged["mcpServers"]["hg"].is_object());
    }

    #[test]
    fn merge_keeps_two_distinct_servers() {
        let first = merge_into_config(json!({}), "a", entry());
        let both = merge_into_config(first, "b", entry());
        assert!(both["mcpServers"]["a"].is_object());
        assert!(both["mcpServers"]["b"].is_object());
    }

    #[test]
    fn merge_is_idempotent_for_same_input() {
        let once = merge_into_config(json!({}), "hg", entry());
        let twice = merge_into_config(once.clone(), "hg", entry());
        assert_eq!(once, twice);
    }

    #[test]
    fn merge_custom_name_used_as_key() {
        let merged = merge_into_config(json!({}), "my-graph", entry());
        assert!(merged["mcpServers"]["my-graph"].is_object());
    }

    #[test]
    fn merge_result_is_object() {
        assert!(merge_into_config(json!({}), "hg", entry()).is_object());
    }

    #[test]
    fn merge_preserves_nested_other_server_args() {
        let existing = json!({ "mcpServers": { "other": { "args": ["a", "b"] } } });
        let merged = merge_into_config(existing, "hg", entry());
        assert_eq!(merged["mcpServers"]["other"]["args"][1], "b");
    }

    // ── read_config ───────────────────────────────────────────────────────────

    #[test]
    fn read_absent_file_is_empty_object() {
        let dir = tdir();
        let cfg = read_config(&dir.join("nope.json")).expect("read");
        assert_eq!(cfg, json!({}));
    }

    #[test]
    fn read_empty_file_is_empty_object() {
        let dir = tdir();
        let path = dir.join("c.json");
        fs::write(&path, "").expect("seed");
        assert_eq!(read_config(&path).expect("read"), json!({}));
    }

    #[test]
    fn read_whitespace_only_is_empty_object() {
        let dir = tdir();
        let path = dir.join("c.json");
        fs::write(&path, "   \n  ").expect("seed");
        assert_eq!(read_config(&path).expect("read"), json!({}));
    }

    #[test]
    fn read_valid_config_parses() {
        let dir = tdir();
        let path = dir.join("c.json");
        fs::write(&path, "{\"theme\":\"dark\"}").expect("seed");
        assert_eq!(read_config(&path).expect("read")["theme"], "dark");
    }

    #[test]
    fn read_malformed_json_errors() {
        let dir = tdir();
        let path = dir.join("c.json");
        fs::write(&path, "{ not json").expect("seed");
        assert!(read_config(&path).is_err());
    }

    #[test]
    fn read_existing_servers_preserved() {
        let dir = tdir();
        let path = dir.join("c.json");
        fs::write(&path, "{\"mcpServers\":{\"x\":{\"command\":\"y\"}}}").expect("seed");
        let cfg = read_config(&path).expect("read");
        assert_eq!(cfg["mcpServers"]["x"]["command"], "y");
    }

    // ── to_pretty ─────────────────────────────────────────────────────────────

    #[test]
    fn pretty_round_trips() {
        let value = json!({ "mcpServers": { "hg": { "command": "habitat-graph" } } });
        let text = to_pretty(&value).expect("pretty");
        let back: Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(value, back);
    }

    #[test]
    fn pretty_is_indented() {
        let text = to_pretty(&json!({ "a": 1 })).expect("pretty");
        assert!(text.contains('\n'), "pretty output spans lines");
    }

    // ── run: print mode ───────────────────────────────────────────────────────

    #[test]
    fn run_print_mode_returns_zero() {
        assert_eq!(run(&graph_path(), DEFAULT_SERVER_NAME, None), 0);
    }

    #[test]
    fn run_print_mode_custom_name_returns_zero() {
        assert_eq!(run(&graph_path(), "custom", None), 0);
    }

    // ── run: write mode ───────────────────────────────────────────────────────

    #[test]
    fn run_write_creates_file_returns_zero() {
        let dir = tdir();
        let path = dir.join("mcp.json");
        assert_eq!(run(&graph_path(), "hg", Some(&path)), 0);
        assert!(path.exists());
    }

    #[test]
    fn run_write_produces_valid_json() {
        let dir = tdir();
        let path = dir.join("mcp.json");
        assert_eq!(run(&graph_path(), "hg", Some(&path)), 0);
        let cfg = read_config(&path).expect("read");
        assert!(cfg["mcpServers"]["hg"].is_object());
    }

    #[test]
    fn run_write_registers_command() {
        let dir = tdir();
        let path = dir.join("mcp.json");
        assert_eq!(run(&graph_path(), "hg", Some(&path)), 0);
        let cfg = read_config(&path).expect("read");
        assert_eq!(cfg["mcpServers"]["hg"]["command"], binary_path());
    }

    #[test]
    fn run_write_preserves_existing_servers() {
        let dir = tdir();
        let path = dir.join("mcp.json");
        fs::write(&path, "{\"mcpServers\":{\"keep\":{\"command\":\"k\"}}}").expect("seed");
        assert_eq!(run(&graph_path(), "hg", Some(&path)), 0);
        let cfg = read_config(&path).expect("read");
        assert_eq!(cfg["mcpServers"]["keep"]["command"], "k");
        assert!(cfg["mcpServers"]["hg"].is_object());
    }

    #[test]
    fn run_write_preserves_other_top_level() {
        let dir = tdir();
        let path = dir.join("mcp.json");
        fs::write(&path, "{\"theme\":\"light\"}").expect("seed");
        assert_eq!(run(&graph_path(), "hg", Some(&path)), 0);
        assert_eq!(read_config(&path).expect("read")["theme"], "light");
    }

    #[test]
    fn run_write_is_idempotent() {
        let dir = tdir();
        let path = dir.join("mcp.json");
        assert_eq!(run(&graph_path(), "hg", Some(&path)), 0);
        let first = fs::read_to_string(&path).expect("read1");
        assert_eq!(run(&graph_path(), "hg", Some(&path)), 0);
        let second = fs::read_to_string(&path).expect("read2");
        assert_eq!(first, second, "second install is a no-op rewrite");
    }

    #[test]
    fn run_write_overwrites_stale_entry() {
        let dir = tdir();
        let path = dir.join("mcp.json");
        fs::write(&path, "{\"mcpServers\":{\"hg\":{\"command\":\"stale\"}}}").expect("seed");
        assert_eq!(run(&graph_path(), "hg", Some(&path)), 0);
        assert_eq!(read_config(&path).expect("read")["mcpServers"]["hg"]["command"], binary_path());
    }

    #[test]
    fn run_write_on_malformed_existing_returns_one() {
        let dir = tdir();
        let path = dir.join("mcp.json");
        fs::write(&path, "{ broken").expect("seed");
        assert_eq!(run(&graph_path(), "hg", Some(&path)), 1);
    }

    #[test]
    fn run_write_into_empty_file_succeeds() {
        let dir = tdir();
        let path = dir.join("mcp.json");
        fs::write(&path, "").expect("seed");
        assert_eq!(run(&graph_path(), "hg", Some(&path)), 0);
        assert!(read_config(&path).expect("read")["mcpServers"]["hg"].is_object());
    }

    #[test]
    fn run_write_args_include_graph() {
        let dir = tdir();
        let path = dir.join("mcp.json");
        assert_eq!(run(std::path::Path::new("/x/g.json"), "hg", Some(&path)), 0);
        let cfg = read_config(&path).expect("read");
        assert_eq!(cfg["mcpServers"]["hg"]["args"][2], "/x/g.json");
    }

    #[test]
    fn run_write_two_graphs_two_names_coexist() {
        let dir = tdir();
        let path = dir.join("mcp.json");
        assert_eq!(run(std::path::Path::new("/a.json"), "ga", Some(&path)), 0);
        assert_eq!(run(std::path::Path::new("/b.json"), "gb", Some(&path)), 0);
        let cfg = read_config(&path).expect("read");
        assert_eq!(cfg["mcpServers"]["ga"]["args"][2], "/a.json");
        assert_eq!(cfg["mcpServers"]["gb"]["args"][2], "/b.json");
    }

    // ── binary_path + constant ────────────────────────────────────────────────

    #[test]
    fn binary_path_is_non_empty() {
        assert!(!binary_path().is_empty());
    }

    #[test]
    fn default_server_name_is_habitat_graph() {
        assert_eq!(DEFAULT_SERVER_NAME, "habitat-graph");
    }

    #[test]
    fn default_name_used_when_writing() {
        let dir = tdir();
        let path = dir.join("mcp.json");
        assert_eq!(run(&graph_path(), DEFAULT_SERVER_NAME, Some(&path)), 0);
        assert!(read_config(&path).expect("read")["mcpServers"]["habitat-graph"].is_object());
    }

    #[test]
    fn entry_args_is_an_array() {
        assert!(entry()["args"].is_array());
    }

    #[test]
    fn merge_stores_entry_value_intact() {
        let merged = merge_into_config(json!({}), "hg", entry());
        assert_eq!(merged["mcpServers"]["hg"], entry());
    }

    #[test]
    fn read_deeply_nested_config_preserved() {
        let dir = tdir();
        let path = dir.join("c.json");
        fs::write(&path, "{\"a\":{\"b\":{\"c\":[1,2,3]}}}").expect("seed");
        let merged = merge_into_config(read_config(&path).expect("read"), "hg", entry());
        assert_eq!(merged["a"]["b"]["c"][2], 3);
    }

    #[test]
    fn run_write_output_is_pretty_multiline() {
        let dir = tdir();
        let path = dir.join("mcp.json");
        assert_eq!(run(&graph_path(), "hg", Some(&path)), 0);
        let text = fs::read_to_string(&path).expect("read");
        assert!(text.lines().count() > 1, "written config is pretty-printed");
    }

    #[test]
    fn merge_empty_name_still_inserts() {
        let merged = merge_into_config(json!({}), "", entry());
        assert!(merged["mcpServers"][""].is_object());
    }
}
