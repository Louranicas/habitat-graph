//! Meta commands: `self-test` and `doctor`.

use std::path::Path;

use habitat_graph_build::assemble;
use habitat_graph_extract::registered_extractors;

/// Exercises the engine on a tiny in-memory Rust corpus (no filesystem I/O).
///
/// Finds the registered `"rust"` extractor, runs it against a two-function snippet in memory,
/// assembles the resulting [`Extraction`](habitat_graph_core::Extraction) into a
/// [`Graph`](habitat_graph_core::Graph), and verifies that at least 2 nodes are present.
///
/// Returns `0` on success, `1` on any failure (no Rust extractor found, parse error,
/// or the assembled graph contains fewer than 2 nodes). All diagnostics go to stderr.
///
/// # Errors
///
/// This function does not return a `Result`; all failures are encoded as exit code `1`
/// with a human-readable message on stderr.
#[must_use]
pub fn self_test() -> u8 {
    let extractors = registered_extractors();
    let Some(ext) = extractors.iter().find(|e| e.language() == "rust") else {
        eprintln!("self-test: no rust extractor");
        return 1;
    };

    match ext.extract(
        Path::new("selftest.rs"),
        b"fn main() { helper(); } fn helper() {}",
    ) {
        Ok(ex) => {
            let g = assemble(vec![ex]);
            let (n, _, _) = g.counts();
            if n >= 2 {
                println!("self-test ok: {n} nodes");
                0
            } else {
                eprintln!("self-test failed: {n} nodes");
                1
            }
        }
        Err(e) => {
            eprintln!("self-test failed: {e}");
            1
        }
    }
}

/// Prints binary version and engine wiring diagnostics to stdout. Always returns `0`.
///
/// Emits exactly two lines:
/// - `habitat-graph <version>` where `<version>` is the value of `CARGO_PKG_VERSION`.
/// - `engine: source+extract+build+analyze+export (backend: ast-only)`
#[must_use]
pub fn doctor() -> u8 {
    println!("habitat-graph {}", env!("CARGO_PKG_VERSION"));
    println!("engine: source+extract+build+analyze+export (backend: ast-only)");
    0
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use habitat_graph_build::assemble;
    use habitat_graph_extract::registered_extractors;

    use super::{doctor, self_test};

    // ── corpus constants ──────────────────────────────────────────────────────

    /// The canonical two-function Rust corpus used by `self_test`.
    const TWO_FN_SOURCE: &[u8] = b"fn main() { helper(); } fn helper() {}";

    /// Single-function Rust corpus.
    const ONE_FN_SOURCE: &[u8] = b"fn solo() {}";

    /// Three-function Rust corpus.
    const THREE_FN_SOURCE: &[u8] = b"fn alpha() {} fn beta() {} fn gamma() {}";

    /// Empty Rust source — no top-level items.
    const EMPTY_SOURCE: &[u8] = b"";

    // ── smoke tests ───────────────────────────────────────────────────────────

    #[test]
    fn self_test_returns_zero() {
        assert_eq!(
            self_test(),
            0,
            "self_test must return 0 with a valid engine"
        );
    }

    #[test]
    fn doctor_returns_zero() {
        assert_eq!(doctor(), 0, "doctor must always return 0");
    }

    #[test]
    fn self_test_is_idempotent() {
        // Two consecutive calls must both succeed: no global state is mutated.
        let first = self_test();
        let second = self_test();
        assert_eq!(first, 0, "first self_test call must succeed");
        assert_eq!(second, 0, "second self_test call must succeed");
        assert_eq!(first, second, "self_test must be deterministic");
    }

    #[test]
    fn doctor_is_idempotent() {
        let first = doctor();
        let second = doctor();
        assert_eq!(first, 0);
        assert_eq!(second, 0);
        assert_eq!(first, second, "doctor must be deterministic");
    }

    // ── registry checks ───────────────────────────────────────────────────────

    #[test]
    fn rust_extractor_present_in_registry() {
        let extractors = registered_extractors();
        assert!(
            extractors.iter().any(|e| e.language() == "rust"),
            "a 'rust' extractor must be registered"
        );
    }

    #[test]
    fn rust_extractor_handles_rs_extension() {
        let extractors = registered_extractors();
        let ext = extractors
            .iter()
            .find(|e| e.language() == "rust")
            .expect("rust extractor must be present");
        assert!(
            ext.extensions().contains(&"rs"),
            "rust extractor must declare the 'rs' file extension"
        );
    }

    // ── extract step ──────────────────────────────────────────────────────────

    #[test]
    fn rust_extractor_on_two_fn_corpus_succeeds() {
        let extractors = registered_extractors();
        let ext = extractors
            .iter()
            .find(|e| e.language() == "rust")
            .expect("rust extractor must be present");
        assert!(
            ext.extract(Path::new("selftest.rs"), TWO_FN_SOURCE).is_ok(),
            "extract on two-fn corpus must succeed"
        );
    }

    #[test]
    fn rust_extractor_two_fn_corpus_yields_at_least_two_raw_nodes() {
        let extractors = registered_extractors();
        let ext = extractors
            .iter()
            .find(|e| e.language() == "rust")
            .expect("rust extractor must be present");
        let extraction = ext
            .extract(Path::new("selftest.rs"), TWO_FN_SOURCE)
            .expect("extract must succeed on two-fn corpus");
        assert!(
            extraction.nodes.len() >= 2,
            "two-fn corpus must yield ≥2 raw nodes; got {}",
            extraction.nodes.len()
        );
    }

    // ── assemble step ─────────────────────────────────────────────────────────

    #[test]
    fn assemble_two_fn_corpus_yields_two_graph_nodes() {
        let extractors = registered_extractors();
        let ext = extractors
            .iter()
            .find(|e| e.language() == "rust")
            .expect("rust extractor must be present");
        let ex = ext
            .extract(Path::new("selftest.rs"), TWO_FN_SOURCE)
            .expect("extract must succeed");
        let g = assemble(vec![ex]);
        let (n, _, _) = g.counts();
        assert!(n >= 2, "assembled graph must have ≥2 nodes; got {n}");
    }

    #[test]
    fn self_test_predicate_holds_via_inline_engine_call() {
        // Mirrors the exact predicate inside self_test() to verify the n≥2 condition.
        let extractors = registered_extractors();
        let ext = extractors
            .iter()
            .find(|e| e.language() == "rust")
            .expect("rust extractor must be registered");
        let ex = ext
            .extract(Path::new("selftest.rs"), TWO_FN_SOURCE)
            .expect("extract must succeed on known corpus");
        let g = assemble(vec![ex]);
        let (n, _, _) = g.counts();
        assert!(
            n >= 2,
            "self_test predicate (n ≥ 2) must hold on the known corpus; got {n}"
        );
    }

    // ── additional corpus sizes ───────────────────────────────────────────────

    #[test]
    fn single_fn_corpus_yields_exactly_one_graph_node() {
        let extractors = registered_extractors();
        let ext = extractors
            .iter()
            .find(|e| e.language() == "rust")
            .expect("rust extractor must be present");
        let ex = ext
            .extract(Path::new("one.rs"), ONE_FN_SOURCE)
            .expect("extract must succeed");
        let g = assemble(vec![ex]);
        let (n, _, _) = g.counts();
        assert_eq!(n, 1, "single-fn corpus must yield exactly 1 node; got {n}");
    }

    #[test]
    fn empty_source_yields_zero_nodes_and_edges() {
        let extractors = registered_extractors();
        let ext = extractors
            .iter()
            .find(|e| e.language() == "rust")
            .expect("rust extractor must be present");
        let ex = ext
            .extract(Path::new("empty.rs"), EMPTY_SOURCE)
            .expect("extract on empty source must succeed");
        let g = assemble(vec![ex]);
        let (n, e, _) = g.counts();
        assert_eq!(n, 0, "empty source must yield 0 nodes; got {n}");
        assert_eq!(e, 0, "empty source must yield 0 edges; got {e}");
    }

    #[test]
    fn three_fn_corpus_yields_exactly_three_graph_nodes() {
        let extractors = registered_extractors();
        let ext = extractors
            .iter()
            .find(|e| e.language() == "rust")
            .expect("rust extractor must be present");
        let ex = ext
            .extract(Path::new("three.rs"), THREE_FN_SOURCE)
            .expect("extract must succeed");
        let g = assemble(vec![ex]);
        let (n, _, _) = g.counts();
        assert_eq!(n, 3, "three-fn corpus must yield exactly 3 nodes; got {n}");
    }

    #[test]
    fn two_fn_corpus_graph_has_one_calls_edge() {
        // main() calls helper(); after assemble the call must resolve to one 'calls' edge.
        let extractors = registered_extractors();
        let ext = extractors
            .iter()
            .find(|e| e.language() == "rust")
            .expect("rust extractor must be present");
        let ex = ext
            .extract(Path::new("selftest.rs"), TWO_FN_SOURCE)
            .expect("extract must succeed");
        let g = assemble(vec![ex]);
        let calls_edges: Vec<_> = g.edges.iter().filter(|e| e.relation == "calls").collect();
        assert_eq!(
            calls_edges.len(),
            1,
            "two-fn corpus must produce exactly one 'calls' edge; got {calls_edges:?}"
        );
    }
}
