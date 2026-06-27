//! `habitat-graph-export` — render artifacts from the graph (Module Structure Plan L6).
//!
//! [`to_node_link`] emits `NetworkX` node-link JSON (graphify-compatible envelope), [`render_report`]
//! the human-facing `GRAPH_REPORT.md`, and [`render_vault`] an Obsidian vault. (svg/graphml/cypher/
//! wiki/benchmark exporters are deferred to a later refinement.)
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod json;
pub mod obsidian;
pub mod report;

pub use json::to_node_link;
pub use obsidian::render_vault;
pub use report::render_report;
