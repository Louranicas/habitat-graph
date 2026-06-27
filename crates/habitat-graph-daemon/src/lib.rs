//! `habitat-graph-daemon` — an HTTP service over a loaded knowledge graph (Module Structure Plan
//! L7 / deployment maturity D5, the warm-service MVP).
//!
//! [`handlers`] holds the pure (sync, fully testable) endpoint logic; [`server`] wires it into an
//! `axum` router and runs it. Three endpoints: `GET /health` (version + counts, the `cc-health`
//! source), `GET /query?q=` (label search), `GET /path?from=&to=` (shortest path). The graph is
//! immutable and shared (`Arc`). The salsa warm-DB / subscriptions / persistence are a later evolution.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod handlers;
pub mod server;

pub use server::{build_router, run_server};
