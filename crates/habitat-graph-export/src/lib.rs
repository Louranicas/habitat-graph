//! `habitat-graph-export` — render artifacts from the graph (Module Structure Plan L6).
//!
//! [`to_node_link`] emits `NetworkX` node-link JSON (graphify-compatible envelope), [`render_report`]
//! the human-facing `GRAPH_REPORT.md`, [`render_vault`] an Obsidian vault, and [`render_html`] a
//! self-contained interactive `graph.html` viewer (the graphify `graph.html` analogue). (svg/graphml/
//! cypher/wiki/benchmark exporters are deferred to a later refinement.)
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod html;
pub mod json;
pub mod obsidian;
pub mod report;

pub use html::render_html;
pub use json::to_node_link;
pub use obsidian::render_vault;
pub use report::render_report;
