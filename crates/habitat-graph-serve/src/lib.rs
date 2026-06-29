//! `habitat-graph-serve` — the query engine (Module Structure Plan L7, query side).
//!
//! [`from_node_link`] loads a node-link `graph.json` (as written by the export crate / CLI) back
//! into a [`Graph`](habitat_graph_core::Graph); [`find_by_label`] and [`shortest_path`] answer the
//! `query` and `path` operations. The [`mcp`] module layers a Model Context Protocol (JSON-RPC)
//! surface on top so the graph is a live organ a Claude Code / orchestrator client can call; the
//! HTTP transport is layered on with the daemon.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod budget;
pub mod explain;
pub mod generation;
pub mod index;
pub mod load;
pub mod mcp;
pub mod query;
pub mod resources;

pub use budget::{estimate_tokens, pack};
pub use explain::explain;
pub use generation::generation_id;
pub use index::LabelIndex;
pub use load::from_node_link;
pub use mcp::handle_jsonrpc;
pub use query::{find_by_label, shortest_path};
pub use resources::{resources_list, resources_read};
