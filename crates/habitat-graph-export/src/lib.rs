//! `habitat-graph-export` — render artifacts from the graph (Module Structure Plan L6).
//!
//! [`to_node_link`] emits `NetworkX` node-link JSON (graphify-compatible envelope), [`render_report`]
//! the human-facing `GRAPH_REPORT.md`, [`render_vault`] an Obsidian vault, and [`render_html`] a
//! self-contained interactive `graph.html` viewer (the graphify `graph.html` analogue). PB adds the
//! full exporter set: [`render_svg`], [`render_graphml`], [`render_cypher`], and [`render_wiki`].
//!
//! All structured exporters route attacker-influenced strings (labels, paths) through [`escape`] —
//! the single tested escaping surface (STRIDE-T injection guard).
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod benchmark;
pub mod cypher;
pub mod escape;
pub mod graphml;
pub mod html;
pub mod json;
pub mod obsidian;
pub mod report;
pub mod svg;
pub mod wiki;

pub use benchmark::{token_benchmark, TokenBenchmark};
pub use cypher::render_cypher;
pub use graphml::render_graphml;
pub use html::render_html;
pub use json::to_node_link;
pub use obsidian::render_vault;
pub use report::render_report;
pub use svg::render_svg;
pub use wiki::render_wiki;
