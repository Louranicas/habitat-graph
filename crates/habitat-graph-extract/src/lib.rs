//! `habitat-graph-extract` — file → `{nodes, edges}` via tree-sitter (Module Structure Plan L3).
//!
//! The [`Extractor`] trait is the per-language contract; [`registry::extract_files`] dispatches a
//! file set across the registered extractors in parallel (rayon) and returns one
//! [`Extraction`](habitat_graph_core::Extraction) per file. tree-sitter exposes a safe Rust API, so
//! this crate remains `forbid(unsafe)` — the C grammar's FFI is isolated inside the upstream crates.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ast;
pub mod registry;

pub use registry::{extract_files, registered_extractors, Extractor};
