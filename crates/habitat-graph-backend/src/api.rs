//! The [`Backend`] trait — the semantic-extraction contract every model adapter implements.

use habitat_graph_core::{Extraction, Result};

/// A semantic-extraction backend: turns free text into graph `{nodes, edges}`.
///
/// Implementors are `Send + Sync` so one backend can be shared across the rayon extraction pool.
/// The default [`NoopBackend`](crate::NoopBackend) keeps extraction local-first; the network
/// adapters ([`OllamaBackend`](crate::OllamaBackend),
/// [`OpenAiCompatBackend`](crate::OpenAiCompatBackend)) are generic over an
/// [`HttpTransport`](crate::HttpTransport) so they can be tested without a network.
pub trait Backend: Send + Sync {
    /// A short, stable identifier for receipts and diagnostics (e.g. `"noop"`, `"ollama"`).
    fn name(&self) -> &'static str;

    /// Whether this backend performs **no** network I/O. The local-first audit reads this flag.
    fn is_local(&self) -> bool;

    /// Extracts semantic `{nodes, edges}` from `text`, attributing nodes to `source_file`.
    ///
    /// # Errors
    /// Returns [`GraphError::Backend`](habitat_graph_core::GraphError::Backend) on transport
    /// failure or when the response cannot be parsed into the expected shape.
    fn extract_semantic(&self, text: &str, source_file: &str) -> Result<Extraction>;
}

#[cfg(test)]
mod tests {
    use super::Backend;
    use crate::NoopBackend;
    use habitat_graph_core::{Extraction, Result};

    // A second trivial implementor proves the trait is object-safe and usable behind a `dyn` ptr.
    struct ConstBackend;
    impl Backend for ConstBackend {
        fn name(&self) -> &'static str {
            "const"
        }
        fn is_local(&self) -> bool {
            false
        }
        fn extract_semantic(&self, _text: &str, source_file: &str) -> Result<Extraction> {
            let mut e = Extraction::new();
            e.nodes.push(habitat_graph_core::RawNode {
                label: "k".into(),
                source_file: source_file.into(),
                span: habitat_graph_core::Span::new(0, 0, 0, 0),
            });
            Ok(e)
        }
    }

    #[test]
    fn trait_is_object_safe() {
        let backends: Vec<Box<dyn Backend>> = vec![Box::new(NoopBackend), Box::new(ConstBackend)];
        assert_eq!(backends.len(), 2);
    }

    #[test]
    fn dyn_dispatch_routes_to_impl() {
        let b: &dyn Backend = &ConstBackend;
        assert_eq!(b.name(), "const");
        assert!(!b.is_local());
        let e = b.extract_semantic("x", "f.md").expect("ok");
        assert_eq!(e.counts(), (1, 0));
        assert_eq!(e.nodes[0].source_file, "f.md");
    }

    #[test]
    fn is_local_distinguishes_backends() {
        assert!(NoopBackend.is_local());
        assert!(!ConstBackend.is_local());
    }
}
