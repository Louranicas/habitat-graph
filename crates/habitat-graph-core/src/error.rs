//! The unified error taxonomy (interface contract §9).
//!
//! Library code never `unwrap`s or `expect`s; every fallible boundary returns
//! [`Result`]`<T>` = `Result<T, GraphError>`. The CLI maps variants to documented exit codes.

use thiserror::Error;

/// Crate result alias: `Result<T, GraphError>`.
pub type Result<T> = std::result::Result<T, GraphError>;

/// The single error type surfaced across habitat-graph boundaries.
///
/// `#[non_exhaustive]` so new variants can be added without a breaking change.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GraphError {
    /// An I/O operation failed (path included in the message).
    #[error("io error: {0}")]
    Io(String),

    /// A source file could not be parsed.
    #[error("parse error in {file}: {message}")]
    Parse {
        /// The file that failed to parse.
        file: String,
        /// Human-readable detail.
        message: String,
    },

    /// External input was rejected by a security/validation guard.
    #[error("guard rejected input: {0}")]
    Guard(String),

    /// A value did not satisfy the graph schema (e.g. serialization failure).
    #[error("schema violation: {0}")]
    Schema(String),

    /// A backend (LLM / semantic extraction) call failed.
    #[error("backend error: {0}")]
    Backend(String),

    /// The incremental cache failed (a hit must equal a recompute; a divergence is a bug).
    #[error("cache error: {0}")]
    Cache(String),

    /// The daemon encountered an error.
    #[error("daemon error: {0}")]
    Daemon(String),
}

impl GraphError {
    /// Returns a short, stable kind tag — useful for receipts, metrics, and exit-code mapping.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Io(_) => "io",
            Self::Parse { .. } => "parse",
            Self::Guard(_) => "guard",
            Self::Schema(_) => "schema",
            Self::Backend(_) => "backend",
            Self::Cache(_) => "cache",
            Self::Daemon(_) => "daemon",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_tags_are_stable() {
        assert_eq!(GraphError::Io("x".into()).kind(), "io");
        assert_eq!(
            GraphError::Parse {
                file: "f".into(),
                message: "m".into()
            }
            .kind(),
            "parse"
        );
        assert_eq!(GraphError::Guard("g".into()).kind(), "guard");
        assert_eq!(GraphError::Schema("s".into()).kind(), "schema");
        assert_eq!(GraphError::Backend("b".into()).kind(), "backend");
        assert_eq!(GraphError::Cache("c".into()).kind(), "cache");
        assert_eq!(GraphError::Daemon("d".into()).kind(), "daemon");
    }

    #[test]
    fn display_includes_context() {
        let e = GraphError::Parse {
            file: "a.rs".into(),
            message: "bad header".into(),
        };
        let s = e.to_string();
        assert!(s.contains("a.rs"), "{s}");
        assert!(s.contains("bad header"), "{s}");
    }

    #[test]
    fn io_display() {
        assert_eq!(
            GraphError::Io("disk full".into()).to_string(),
            "io error: disk full"
        );
    }

    #[test]
    fn guard_display() {
        assert_eq!(
            GraphError::Guard("path escapes output dir".into()).to_string(),
            "guard rejected input: path escapes output dir"
        );
    }

    #[test]
    fn result_alias_threads_through() {
        fn fallible(ok: bool) -> Result<u32> {
            if ok {
                Ok(1)
            } else {
                Err(GraphError::Schema("no".into()))
            }
        }
        assert_eq!(fallible(true).unwrap(), 1);
        assert!(fallible(false).is_err());
    }

    #[test]
    fn error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<GraphError>();
    }

    #[test]
    fn distinct_kinds_for_distinct_variants() {
        let kinds = [
            GraphError::Io(String::new()).kind(),
            GraphError::Guard(String::new()).kind(),
            GraphError::Schema(String::new()).kind(),
            GraphError::Backend(String::new()).kind(),
            GraphError::Cache(String::new()).kind(),
            GraphError::Daemon(String::new()).kind(),
        ];
        let mut uniq = kinds.to_vec();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), kinds.len(), "kind tags must be unique");
    }
}
