//! `habitat-graph-backend` — semantic-extraction backends (L3 of the Module Structure Plan).
//!
//! Code extraction (the [`habitat-graph-extract`](https://docs.rs/habitat-graph-extract) crate) is
//! AST-driven and fully local. This crate is the **opt-in** semantic path: turning free text
//! (docstrings, comments, prose, PDF-derived text) into graph `{nodes, edges}` via a model.
//!
//! Three design rules govern it:
//! 1. **Local-first (R3).** The default [`NoopBackend`] performs no network I/O; a model is used only
//!    when one is explicitly configured. The [`Backend::is_local`] flag makes that auditable.
//! 2. **Untrusted output.** A model's response is external input: every label and relation funnels
//!    through [`habitat_graph_core::sanitize_label`] before it enters the graph
//!    (see [`protocol::parse_semantic`]).
//! 3. **Testable without a network.** Network adapters ([`OllamaBackend`], [`OpenAiCompatBackend`])
//!    are generic over an [`HttpTransport`]; tests inject [`StaticTransport`]. The concrete
//!    `UreqTransport` lives behind the `net` feature so the default build and gate stay dep-light.
//!
//! The production path that routes through the factory's TIERWRIGHT router (`:8201`) is the
//! `habitat-graph-habitat` crate's `tierwright` module (L8), which implements [`Backend`].
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod api;
pub mod noop;
pub mod ollama;
pub mod openai_compat;
pub mod protocol;
pub mod transport;

pub use api::Backend;
pub use noop::NoopBackend;
pub use ollama::OllamaBackend;
pub use openai_compat::OpenAiCompatBackend;
pub use protocol::{build_prompt, parse_semantic, DEFAULT_RELATION};
pub use transport::{HttpTransport, RecordedRequest, StaticTransport};

#[cfg(feature = "net")]
pub use transport::UreqTransport;
