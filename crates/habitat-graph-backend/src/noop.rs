//! The default, network-free backend: semantic extraction is a no-op (local-first, R3).

use crate::api::Backend;
use habitat_graph_core::{Extraction, Result};

/// The local-first default backend.
///
/// [`extract_semantic`](Backend::extract_semantic) always yields an empty [`Extraction`]: code
/// extraction never reaches for a model unless a real backend is explicitly configured. This is the
/// value the extract pipeline holds when no semantic enrichment is requested.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopBackend;

impl Backend for NoopBackend {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn is_local(&self) -> bool {
        true
    }

    fn extract_semantic(&self, _text: &str, _source_file: &str) -> Result<Extraction> {
        Ok(Extraction::new())
    }
}

#[cfg(test)]
mod tests {
    use super::NoopBackend;
    use crate::api::Backend;

    #[test]
    fn name_is_noop() {
        assert_eq!(NoopBackend.name(), "noop");
    }

    #[test]
    fn is_local() {
        assert!(NoopBackend.is_local());
    }

    #[test]
    fn extraction_is_empty_for_any_text() {
        let b = NoopBackend;
        for text in ["", "hello", "fn main() {}", "a very long string ".repeat(100).as_str()] {
            let e = b.extract_semantic(text, "f.rs").expect("ok");
            assert!(e.is_empty(), "noop must never emit nodes/edges");
        }
    }

    #[test]
    fn extraction_ignores_source_file() {
        let b = NoopBackend;
        assert!(b.extract_semantic("x", "").expect("ok").is_empty());
        assert!(b.extract_semantic("x", "deep/path.md").expect("ok").is_empty());
    }

    #[test]
    fn implements_default_via_bound() {
        fn defaulted<T: Default>() -> T {
            T::default()
        }
        let b: NoopBackend = defaulted();
        assert_eq!(b.name(), "noop");
    }

    #[test]
    fn is_copy_and_clone() {
        let a = NoopBackend;
        let b = a;
        let c = a;
        assert_eq!(b.name(), c.name());
    }

    #[test]
    fn never_errors() {
        let b = NoopBackend;
        assert!(b.extract_semantic("\u{202e}malicious", "f").is_ok());
    }
}
