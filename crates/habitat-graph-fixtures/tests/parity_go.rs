//! Go golden-corpus parity gate (C-G1).
//!
//! Extracts `fixtures/worked/go/src/sample.go` with `habitat-graph` and verifies the result
//! against `fixtures/worked/go/graph.json` — a hand-authored reference golden in graphify's
//! node-link id format (string ids matching the extractor's qualified-id taxonomy).
//!
//! # Edge taxonomy after assembly
//!
//! | Relation       | In golden | Notes |
//! |----------------|-----------|-------|
//! | `contains`     | 9         | file→type (4) + file→fn (5) |
//! | `method`       | 7         | Dog×3, Cat×2, Server×2 |
//! | `imports_from` | 0         | Extractor emits these but `assemble` drops them |
//! |                |           | because external stdlib nodes (`fmt`, `io`, `net/http`) |
//! |                |           | are not part of the scanned corpus — expected per |
//! |                |           | the assemble.rs dangling-edge policy. |
//! | `inherits`     | 0         | **INVARIANT**: Go has no inheritance |
//! | `calls`/`uses` | 0         | Deliberately not emitted (go.rs §14) |
//!
//! Total structural edges: 16 (`contains` + `method`).
//! The `imports_from` relation is validated at the raw [`Extraction`] level (before assembly)
//! in the focused tests below.
//!
//! # Key invariants
//! 1. Node coverage: 17/17 (100 %) — pinned baseline regression gate.
//! 2. Structural (`contains` + `method`) coverage: 16/16 (100 %) — pinned baseline.
//! 3. **Zero `inherits` edges** — Go has no inheritance; embedding is composition, not subtyping.
//! 4. `imports_from` edges emitted by extractor, dropped by assembler for external modules.

use std::path::PathBuf;

use habitat_graph_fixtures::{classify, from_core, from_golden};

// ── Pinned baselines (R3a / LS-3) ─────────────────────────────────────────────────────────────
//
// Derived from the 17-node / 16-structural-edge (9 contains + 7 method) Go corpus.
// imports_from edges are dropped by assemble for external stdlib targets.
//
// Ratchet UP on any legitimate improvement; any drop is OUR regression.
const GO_NODE_BASELINE: usize = 17;
const GO_STRUCT_BASELINE: usize = 16; // contains-9 + method-7

fn go_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = crates/habitat-graph-fixtures/
    // ../../fixtures/worked/go  →  fixtures/worked/go  (repo root)
    PathBuf::from(format!(
        "{}/../../fixtures/worked/go",
        env!("CARGO_MANIFEST_DIR")
    ))
}

// ── Primary parity gate ───────────────────────────────────────────────────────────────────────

#[test]
fn go_structural_parity() {
    let dir = go_dir();

    // 1. Extract the Go corpus.
    let files =
        habitat_graph_source::detect(&dir.join("src"), &["go"]).expect("detect go files");
    assert!(
        !files.is_empty(),
        "expected ≥1 .go file in fixtures/worked/go/src; found none"
    );

    let extractions = habitat_graph_extract::extract_files(&files).expect("extract go");
    let graph = habitat_graph_build::assemble(extractions);
    let ours = from_core(&graph);

    // 2. Load the hand-authored Go golden (graphify node-link format, string ids).
    let golden_json =
        std::fs::read_to_string(dir.join("graph.json")).expect("read go golden graph.json");
    let golden = from_golden(&golden_json).expect("parse go golden");

    // 3. Classify.
    let report = classify(&ours, &golden);

    let node_total = golden.nodes.len().max(1);
    let node_pct = (report.nodes_matched * 100 + node_total / 2) / node_total;

    eprintln!("\n=== go parity (C-G1) ===");
    eprintln!(
        "NODES: {}/{} golden ids covered ({node_pct}%), {} extra",
        report.nodes_matched,
        golden.nodes.len(),
        report.nodes_extra.len()
    );
    if !report.nodes_missing.is_empty() {
        eprintln!("  MISSING nodes: {:?}", report.nodes_missing);
    }

    eprintln!("EDGES by relation (matched / golden / ours):");
    let mut structural_matched = 0_usize;
    let mut structural_total = 0_usize;
    for rel in ["contains", "method", "imports_from", "inherits", "calls", "uses"] {
        let g = golden.edges_with_relation(rel);
        let o = ours.edges_with_relation(rel);
        let matched = g.intersection(&o).count();
        eprintln!("  {rel:>14}: {matched} / {} / {}", g.len(), o.len());
        if matches!(rel, "contains" | "method") {
            // imports_from excluded: external stdlib targets dropped by assemble
            structural_matched += matched;
            structural_total += g.len();
        }
    }

    let struct_total = structural_total.max(1);
    let struct_pct = (structural_matched * 100 + struct_total / 2) / struct_total;
    eprintln!("STRUCTURAL coverage: {structural_matched}/{structural_total} ({struct_pct}%)\n");

    // 4. Go-invariant: zero inherits edges in both golden and our output.
    let golden_inherits = golden.edges_with_relation("inherits");
    assert!(
        golden_inherits.is_empty(),
        "GOLDEN DEFECT: go golden must never contain inherits edges (Go has no inheritance); \
         found {}: {:?}",
        golden_inherits.len(),
        golden_inherits.iter().take(3).collect::<Vec<_>>()
    );

    let ours_inherits = ours.edges_with_relation("inherits");
    assert!(
        ours_inherits.is_empty(),
        "EXTRACTOR REGRESSION: go extractor must not emit inherits edges; \
         found {}: {:?}",
        ours_inherits.len(),
        ours_inherits.iter().take(3).collect::<Vec<_>>()
    );

    // 5. Regression gates (LS-3): pinned baselines.
    assert!(
        report.nodes_matched >= GO_NODE_BASELINE,
        "node coverage REGRESSED: {}/{} ({node_pct}%) < pinned baseline {GO_NODE_BASELINE} — \
         check go.rs or ratchet if extraction legitimately improved; missing: {:?}",
        report.nodes_matched,
        golden.nodes.len(),
        report.nodes_missing
    );

    assert!(
        structural_matched >= GO_STRUCT_BASELINE,
        "structural coverage REGRESSED: {structural_matched}/{structural_total} ({struct_pct}%) \
         < pinned baseline {GO_STRUCT_BASELINE} — check go.rs or ratchet; \
         missing: {:?}",
        report
            .edges_missing
            .iter()
            .filter(|(_, _, r)| matches!(r.as_str(), "contains" | "method"))
            .take(5)
            .collect::<Vec<_>>()
    );
}

// ── Focused structural-property tests ──────────────────────────────────────────────────────────
//
// These tests exercise the extractor API directly (no golden comparison) to lock down the
// node-taxonomy and edge-taxonomy invariants that the extractor spec (go.rs) guarantees.

fn extract_sample() -> habitat_graph_fixtures::NormalizedGraph {
    let src = go_dir().join("src");
    let files = habitat_graph_source::detect(&src, &["go"]).expect("detect");
    let extractions = habitat_graph_extract::extract_files(&files).expect("extract");
    let graph = habitat_graph_build::assemble(extractions);
    from_core(&graph)
}

// ── File node ────────────────────────────────────────────────────────────────────────────────────

#[test]
fn go_file_node_exists() {
    let g = extract_sample();
    assert!(
        g.nodes.contains("sample"),
        "file node 'sample' (stem of sample.go) must exist; got {:?}",
        g.nodes
    );
}

// ── Type nodes ───────────────────────────────────────────────────────────────────────────────────

#[test]
fn go_interface_type_node_exists() {
    let g = extract_sample();
    assert!(
        g.nodes.contains("sample_animal"),
        "'sample_animal' (Animal interface) must be extracted as a type node"
    );
}

#[test]
fn go_struct_type_nodes_exist() {
    let g = extract_sample();
    for name in ["sample_dog", "sample_cat", "sample_server"] {
        assert!(
            g.nodes.contains(name),
            "struct type node '{name}' must exist; nodes: {:?}",
            g.nodes
        );
    }
}

// ── Function nodes ───────────────────────────────────────────────────────────────────────────────

#[test]
fn go_top_level_function_nodes_exist() {
    let g = extract_sample();
    for name in [
        "sample_newdog",
        "sample_newcat",
        "sample_newserver",
        "sample_makesound",
        "sample_main",
    ] {
        assert!(
            g.nodes.contains(name),
            "top-level function node '{name}' must exist; nodes: {:?}",
            g.nodes
        );
    }
}

// ── Method nodes ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn go_value_receiver_method_nodes_exist() {
    let g = extract_sample();
    for name in [
        "sample_dog_sound",
        "sample_dog_name",
        "sample_cat_sound",
        "sample_cat_name",
    ] {
        assert!(
            g.nodes.contains(name),
            "value-receiver method node '{name}' must exist; nodes: {:?}",
            g.nodes
        );
    }
}

#[test]
fn go_pointer_receiver_method_nodes_exist() {
    let g = extract_sample();
    for name in ["sample_dog_setage", "sample_server_start", "sample_server_stop"] {
        assert!(
            g.nodes.contains(name),
            "pointer-receiver method node '{name}' must exist; nodes: {:?}",
            g.nodes
        );
    }
}

// ── Invariant: zero inherits edges ───────────────────────────────────────────────────────────────

#[test]
fn go_extractor_emits_zero_inherits_edges() {
    // Core invariant from go.rs: Go has no subtype inheritance; extractor must never emit
    // 'inherits' edges regardless of embedded struct fields (composition != subtyping).
    let g = extract_sample();
    let inherits = g.edges_with_relation("inherits");
    assert!(
        inherits.is_empty(),
        "Go extractor must emit ZERO inherits edges; found {}: {:?}",
        inherits.len(),
        inherits.iter().take(5).collect::<Vec<_>>()
    );
}

// ── Contains edges ───────────────────────────────────────────────────────────────────────────────

#[test]
fn go_file_contains_type_nodes() {
    let g = extract_sample();
    for target in ["sample_animal", "sample_dog", "sample_cat", "sample_server"] {
        let edge = (
            "sample".to_owned(),
            target.to_owned(),
            "contains".to_owned(),
        );
        assert!(
            g.edges.contains(&edge),
            "expected contains edge sample→{target}; contains edges from sample: {:?}",
            g.edges
                .iter()
                .filter(|(s, _, r)| s == "sample" && r == "contains")
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn go_file_contains_function_nodes() {
    let g = extract_sample();
    for target in [
        "sample_newdog",
        "sample_newcat",
        "sample_newserver",
        "sample_makesound",
        "sample_main",
    ] {
        let edge = (
            "sample".to_owned(),
            target.to_owned(),
            "contains".to_owned(),
        );
        assert!(
            g.edges.contains(&edge),
            "expected contains edge sample→{target}"
        );
    }
}

// ── Method edges ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn go_dog_method_edges_exist() {
    let g = extract_sample();
    for target in ["sample_dog_sound", "sample_dog_name", "sample_dog_setage"] {
        let edge = (
            "sample_dog".to_owned(),
            target.to_owned(),
            "method".to_owned(),
        );
        assert!(
            g.edges.contains(&edge),
            "expected method edge sample_dog→{target}"
        );
    }
}

#[test]
fn go_cat_method_edges_exist() {
    let g = extract_sample();
    for target in ["sample_cat_sound", "sample_cat_name"] {
        let edge = (
            "sample_cat".to_owned(),
            target.to_owned(),
            "method".to_owned(),
        );
        assert!(
            g.edges.contains(&edge),
            "expected method edge sample_cat→{target}"
        );
    }
}

#[test]
fn go_server_method_edges_exist() {
    let g = extract_sample();
    for target in ["sample_server_start", "sample_server_stop"] {
        let edge = (
            "sample_server".to_owned(),
            target.to_owned(),
            "method".to_owned(),
        );
        assert!(
            g.edges.contains(&edge),
            "expected method edge sample_server→{target}"
        );
    }
}

// ── Edge-count floors ────────────────────────────────────────────────────────────────────────────

#[test]
fn go_contains_edge_count_floor() {
    let g = extract_sample();
    let count = g.edges_with_relation("contains").len();
    assert!(
        count >= 9,
        "expected ≥9 contains edges (4 types + 5 fns); got {count}"
    );
}

#[test]
fn go_method_edge_count_floor() {
    let g = extract_sample();
    let count = g.edges_with_relation("method").len();
    assert!(
        count >= 7,
        "expected ≥7 method edges (Dog×3, Cat×2, Server×2); got {count}"
    );
}

// ── imports_from: validated at the Extraction level ──────────────────────────────────────────────
//
// After `assemble`, imports_from edges for external stdlib modules (`fmt`, `io`, `net/http`) are
// DROPPED because those module names have no corresponding nodes in the scanned corpus.
// This is the assemble.rs dangling-edge policy: external symbol targets are not interned.
// The Go extractor DOES emit these raw edges; they are visible in unit tests at the
// Extraction level (go.rs §12). We validate that behaviour here by calling the extractor
// directly, bypassing the assembly step.

#[test]
fn go_extractor_raw_emits_imports_from_edges() {
    // Bypass assemble — call GoExtractor directly to inspect raw Extraction output.
    use habitat_graph_extract::ast::go::GoExtractor;
    use crate::registry::Extractor as _;

    let src_path = go_dir().join("src").join("sample.go");
    let source = std::fs::read(&src_path).expect("read sample.go");

    let ex = GoExtractor
        .extract(&src_path, &source)
        .expect("raw extract sample.go");

    let imports: Vec<_> = ex
        .edges
        .iter()
        .filter(|e| e.relation == "imports_from")
        .collect();

    assert!(
        !imports.is_empty(),
        "Go extractor must emit imports_from edges at the Extraction level; \
         sample.go has grouped import(fmt / io / net/http)"
    );

    let targets: Vec<&str> = imports.iter().map(|e| e.target.as_str()).collect();
    for expected in ["fmt", "io", "net/http"] {
        assert!(
            targets.contains(&expected),
            "imports_from target '{expected}' missing; got {targets:?}"
        );
    }
}

// ── Node-count floor ─────────────────────────────────────────────────────────────────────────────

#[test]
fn go_node_count_floor() {
    let g = extract_sample();
    assert!(
        g.nodes.len() >= 17,
        "expected ≥17 nodes (1 file + 4 types + 7 methods + 5 fns); got {}",
        g.nodes.len()
    );
}

// ── No calls / uses emitted ──────────────────────────────────────────────────────────────────────

#[test]
fn go_extractor_does_not_emit_calls_edges() {
    // go.rs §14: 'calls' and 'uses' are deliberately NOT emitted.
    let g = extract_sample();
    let calls = g.edges_with_relation("calls");
    assert!(
        calls.is_empty(),
        "go extractor must NOT emit 'calls' edges (documented omission); \
         found {}: {:?}",
        calls.len(),
        calls.iter().take(3).collect::<Vec<_>>()
    );
}

#[test]
fn go_extractor_does_not_emit_uses_edges() {
    let g = extract_sample();
    let uses = g.edges_with_relation("uses");
    assert!(
        uses.is_empty(),
        "go extractor must NOT emit 'uses' edges (documented omission); \
         found {}: {:?}",
        uses.len(),
        uses.iter().take(3).collect::<Vec<_>>()
    );
}

// Bring in the Extractor trait for the raw-extraction test above.
mod registry {
    pub use habitat_graph_extract::registry::Extractor;
}
