//! `habitat-graph-build` — aggregate per-file extractions into the interned graph (L4).
//!
//! [`assemble()`] interns each `RawNode` label to a stable [`NodeId`](habitat_graph_core::NodeId),
//! resolves edge endpoints, drops duplicates, and returns a deterministic
//! [`Graph`](habitat_graph_core::Graph). [`dedup()`] and [`merge()`] support incremental rebuilds.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod assemble;
pub mod dedup;
pub mod merge;
pub mod merge_driver;
mod merge_identity;

pub use assemble::assemble;
pub use dedup::dedup;
pub use merge::merge;
pub use merge_driver::merge3;
