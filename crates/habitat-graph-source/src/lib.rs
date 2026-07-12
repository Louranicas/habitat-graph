//! `habitat-graph-source` — input acquisition + triage (Module Structure Plan L2).
//!
//! Three responsibilities: [`detect()`] collects candidate files (honoring `.gitignore`), [`ingest`]
//! reads their bytes, and [`manifest`] records each processed input with a content hash for
//! provenance. The cached/uncached partition lives in `habitat-graph-cache` (ADR-04: single cache
//! truth), not here.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod detect;
pub mod ingest;
pub mod manifest;
#[cfg(feature = "pdf")]
pub mod pdf;
pub mod ssrf;

pub use detect::detect;
pub use ingest::read_local;
pub use manifest::build_manifest;
#[cfg(feature = "pdf")]
pub use pdf::extract_text;
pub use ssrf::ip_is_blocked;
