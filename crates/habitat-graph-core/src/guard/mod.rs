//! The security boundary (Module Structure Plan L1; interface contract §9 `Guard`).
//!
//! Every external input — URLs, paths, labels, anything bound for a renderer or persisted artifact —
//! funnels through this module before use. Core does **no I/O**, so these are *lexical* / pure guards;
//! the FS-canonicalizing confinement and live network caps live in `habitat-graph-source`/`-ingest`,
//! which build on these.
//!
//! The cardinal rule (deep-diff-forge lesson): [`display_safe`] must be applied at **every** render
//! boundary, including secondary UIs — a missed boundary is how Trojan-Source escapes leak.

pub mod path;
pub mod sanitize;
pub mod secrets;
pub mod url;

pub use path::confine_to;
pub use sanitize::{display_safe, sanitize_label, MAX_LABEL_LEN};
pub use secrets::{is_canonical_redaction_marker, screen_for_secrets, SECRET_TAG_ORDER};
pub use url::validate_url;
