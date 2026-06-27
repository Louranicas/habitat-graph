//! `habitat-graph-serve` — the query engine (Module Structure Plan L7, query side).
//!
//! [`from_node_link`] loads a node-link `graph.json` (as written by the export crate / CLI) back
//! into a [`Graph`](habitat_graph_core::Graph); [`find_by_label`] and [`shortest_path`] answer the
//! `query` and `path` operations. The async MCP/HTTP transport is layered on top (with the daemon).
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod load;
pub mod query;

pub use load::from_node_link;
pub use query::{find_by_label, shortest_path};
