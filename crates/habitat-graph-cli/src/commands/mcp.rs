//! The `mcp` command — serve the graph over the Model Context Protocol (JSON-RPC) on stdio.
//!
//! A long-running line loop: read one JSON-RPC request per line from stdin, answer it with
//! [`habitat_graph_serve::handle_jsonrpc`], write the response line to stdout. Notifications (no
//! `id`) produce no output. This is the transport that lets a Claude Code / orchestrator client
//! mount habitat-graph as an MCP server (`graph_query`, `graph_path`, `graph_health`).

use std::io::{BufRead, Write};
use std::path::Path;

use habitat_graph_core::{GraphError, Result};

/// Loads a node-link `graph.json` from `graph_path`.
fn load(graph_path: &Path) -> Result<habitat_graph_core::Graph> {
    let json = std::fs::read_to_string(graph_path)
        .map_err(|e| GraphError::Io(format!("{}: {e}", graph_path.display())))?;
    habitat_graph_serve::from_node_link(&json)
}

/// Loads `graph_path` and serves MCP over stdio until end-of-input.
///
/// Exit codes: `4` if the graph cannot be loaded, `1` on a stdin/stdout I/O failure, `0` on clean
/// end-of-input.
#[must_use]
pub fn run(graph_path: &Path) -> u8 {
    let graph = match load(graph_path) {
        Ok(graph) => graph,
        Err(err) => {
            eprintln!("error: {err}");
            return 4;
        }
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                eprintln!("error: reading stdin: {err}");
                return 1;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = habitat_graph_serve::handle_jsonrpc(&graph, &line);
        if response.is_empty() {
            continue; // notification — nothing to send
        }
        if writeln!(stdout, "{response}").is_err() {
            return 1;
        }
        if stdout.flush().is_err() {
            return 1;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::run;

    #[test]
    fn missing_graph_returns_four() {
        assert_eq!(run(std::path::Path::new("/no/such/graph.json")), 4);
    }
}
