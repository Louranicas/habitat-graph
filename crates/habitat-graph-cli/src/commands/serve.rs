//! The `serve` command — run the HTTP service (`/health`, `/query`, `/path`) over a loaded graph.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use habitat_graph_core::{GraphError, Result};

/// Loads a node-link `graph.json` from `graph_path`.
fn load(graph_path: &Path) -> Result<habitat_graph_core::Graph> {
    let json = std::fs::read_to_string(graph_path)
        .map_err(|e| GraphError::Io(format!("{}: {e}", graph_path.display())))?;
    habitat_graph_serve::from_node_link(&json)
}

/// Loads `graph_path` and serves the HTTP API on `addr` until the process is interrupted.
///
/// Returns an exit code: `4` if the graph cannot be loaded or the server fails, `2` if `addr` is not
/// a valid socket address, `1` on runtime-creation failure. On success it blocks serving and only
/// returns `0` when the server loop ends.
#[must_use]
pub fn run(graph_path: &Path, addr: &str) -> u8 {
    let graph = match load(graph_path) {
        Ok(graph) => graph,
        Err(err) => {
            eprintln!("error: {err}");
            return 4;
        }
    };
    let socket: SocketAddr = match addr.parse() {
        Ok(socket) => socket,
        Err(err) => {
            eprintln!("error: invalid address {addr:?}: {err}");
            return 2;
        }
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("error: could not start runtime: {err}");
            return 1;
        }
    };
    println!(
        "serving graph ({} nodes) on http://{socket}  [/health /query?q= /path?from=&to=]",
        graph.counts().0
    );
    match runtime.block_on(habitat_graph_daemon::run_server(Arc::new(graph), socket)) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("error: {err}");
            4
        }
    }
}

#[cfg(test)]
mod tests {
    use super::run;

    #[test]
    fn missing_graph_returns_four() {
        // Load failure happens before any bind, so this returns promptly.
        assert_eq!(
            run(std::path::Path::new("/no/such/graph.json"), "127.0.0.1:0"),
            4
        );
    }

    #[test]
    fn invalid_address_returns_two() {
        // A real (tempfile) graph would be needed to reach the addr parse after a successful load;
        // instead we rely on load failing first for a missing path. To exercise the addr branch we
        // need a loadable graph: write one, then pass a bad address.
        use std::io::Write as _;
        let mut f = tempfile::NamedTempFile::new().expect("tempfile");
        let json =
            habitat_graph_export::to_node_link(&habitat_graph_core::Graph::new()).expect("json");
        f.write_all(json.as_bytes()).expect("write");
        f.flush().expect("flush");
        assert_eq!(run(f.path(), "not-an-address"), 2);
    }
}
