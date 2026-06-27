//! `habitat-graph-core` — the stable vocabulary of the habitat-graph knowledge-graph engine.
//!
//! This crate is **vocabulary, not behaviour** (Module Structure Plan, Design Rule 1): it owns the
//! interned [`ids`], the source [`span`], the [`confidence`] trust-signal, the graph [`schema`]
//! (the `graph.json` wire truth, byte-compatible with the graphify exemplar), and the [`error`]
//! taxonomy. It performs **no** filesystem, network, terminal, parser, or git operation.
//!
//! See `ai_docs/05_INTERFACE_CONTRACTS.md` §1 (schema) and §9 (errors) for the contracts this
//! crate implements, and `docs/MODULE_STRUCTURE_PLAN.md` for its place in the workspace.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod confidence;
pub mod error;
pub mod extraction;
pub mod guard;
pub mod ids;
pub mod schema;
pub mod span;

pub use confidence::Confidence;
pub use error::{GraphError, Result};
pub use extraction::{Extraction, RawEdge, RawNode};
pub use guard::{confine_to, display_safe, sanitize_label, screen_for_secrets, validate_url};
pub use ids::{CommunityId, EdgeId, NodeId};
pub use schema::{Community, Edge, Graph, InputRecord, Manifest, Node, SCHEMA_VERSION};
pub use span::Span;
