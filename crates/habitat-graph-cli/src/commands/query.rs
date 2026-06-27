//! The `query` and `path` commands — load a node-link `graph.json` and search it.

use std::path::Path;

use habitat_graph_core::{Graph, GraphError, Result};

/// Loads a node-link `graph.json` from `graph_path`.
fn load(graph_path: &Path) -> Result<Graph> {
    let json = std::fs::read_to_string(graph_path)
        .map_err(|e| GraphError::Io(format!("{}: {e}", graph_path.display())))?;
    habitat_graph_serve::from_node_link(&json)
}

/// Loads `graph_path` and prints every node whose label contains `needle`.
///
/// Returns `0` on success (even with zero matches), `4` if the graph cannot be loaded.
#[must_use]
pub fn run_query(graph_path: &Path, needle: &str) -> u8 {
    match load(graph_path) {
        Ok(graph) => {
            let matches = habitat_graph_serve::find_by_label(&graph, needle);
            println!("{} match(es) for {needle:?}:", matches.len());
            for node in matches {
                println!("  {} [{}] {}", node.id.get(), node.label, node.source_file);
            }
            0
        }
        Err(err) => {
            eprintln!("error: {err}");
            4
        }
    }
}

/// Loads `graph_path` and prints the shortest undirected path between labels `from` and `to`.
///
/// Returns `0` on success (including "no path"), `4` if the graph cannot be loaded.
#[must_use]
pub fn run_path(graph_path: &Path, from: &str, to: &str) -> u8 {
    match load(graph_path) {
        Ok(graph) => {
            if let Some(path) = habitat_graph_serve::shortest_path(&graph, from, to) {
                let labels: Vec<&str> = path
                    .iter()
                    .filter_map(|id| {
                        graph
                            .nodes
                            .iter()
                            .find(|n| n.id == *id)
                            .map(|n| n.label.as_str())
                    })
                    .collect();
                println!(
                    "path ({} hops): {}",
                    path.len().saturating_sub(1),
                    labels.join(" -> ")
                );
            } else {
                println!("no path from {from:?} to {to:?}");
            }
            0
        }
        Err(err) => {
            eprintln!("error: {err}");
            4
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use habitat_graph_core::{Confidence, Edge, Graph, Node, NodeId, Span};
    use tempfile::TempDir;

    use super::{run_path, run_query};

    fn node(id: u32, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: "f.rs".to_owned(),
            source_location: Span::new(0, 0, 1, 1),
        }
    }

    fn edge(s: u32, t: u32) -> Edge {
        Edge {
            source: NodeId::new(s),
            target: NodeId::new(t),
            relation: "calls".to_owned(),
            confidence: Confidence::Extracted,
        }
    }

    /// Write a small node-link graph.json to a temp dir and return (dir, path).
    fn write_graph() -> (TempDir, std::path::PathBuf) {
        let mut g = Graph::new();
        g.nodes.push(node(0, "alpha"));
        g.nodes.push(node(1, "beta"));
        g.nodes.push(node(2, "gamma"));
        g.edges.push(edge(0, 1));
        g.edges.push(edge(1, 2));
        let g = g.sorted();
        let json = habitat_graph_export::to_node_link(&g).expect("export");
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("graph.json");
        fs::write(&path, json).unwrap();
        (dir, path)
    }

    #[test]
    fn query_existing_label_returns_zero() {
        let (_dir, path) = write_graph();
        assert_eq!(run_query(&path, "alpha"), 0);
    }

    #[test]
    fn query_substring_case_insensitive_returns_zero() {
        let (_dir, path) = write_graph();
        assert_eq!(run_query(&path, "BET"), 0);
    }

    #[test]
    fn query_no_match_still_returns_zero() {
        let (_dir, path) = write_graph();
        assert_eq!(run_query(&path, "zzz"), 0);
    }

    #[test]
    fn query_missing_graph_returns_four() {
        assert_eq!(
            run_query(std::path::Path::new("/no/such/graph.json"), "x"),
            4
        );
    }

    #[test]
    fn path_connected_labels_returns_zero() {
        let (_dir, path) = write_graph();
        assert_eq!(run_path(&path, "alpha", "gamma"), 0);
    }

    #[test]
    fn path_same_node_returns_zero() {
        let (_dir, path) = write_graph();
        assert_eq!(run_path(&path, "beta", "beta"), 0);
    }

    #[test]
    fn path_no_route_returns_zero() {
        // No edge connects alpha to a non-existent label; "no path" is a success outcome.
        let (_dir, path) = write_graph();
        assert_eq!(run_path(&path, "alpha", "nonexistent"), 0);
    }

    #[test]
    fn path_missing_graph_returns_four() {
        assert_eq!(
            run_path(std::path::Path::new("/no/such/graph.json"), "a", "b"),
            4
        );
    }

    #[test]
    fn query_corrupt_graph_returns_four() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("graph.json");
        fs::write(&path, "{ not node-link").unwrap();
        assert_eq!(run_query(&path, "x"), 4);
    }
}
