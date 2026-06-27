//! The first parity gate: extract graphify's `httpx` Python corpus with `habitat-graph` and compare
//! against graphify's committed golden, per relation.
//!
//! Per the parity strategy (`ai_docs/06_PARITY_INTEL`, content-equivalence with documented
//! divergence): the structural relations (`contains` / `method` / `inherits` / `imports_from`) are
//! expected to be well-covered; `calls` / `uses` are name-resolution heuristics we deliberately do
//! not emit, reported as documented divergence rather than a regression.

use std::path::PathBuf;

use habitat_graph_fixtures::{classify, from_core, from_golden};

fn httpx_dir() -> PathBuf {
    PathBuf::from(format!(
        "{}/../../tests/fixtures/goldens/httpx",
        env!("CARGO_MANIFEST_DIR")
    ))
}

#[test]
fn httpx_structural_parity() {
    let dir = httpx_dir();

    // Our extraction: detect -> extract -> assemble -> normalize.
    let files = habitat_graph_source::detect(&dir.join("raw"), &["py"]).expect("detect py files");
    let extractions = habitat_graph_extract::extract_files(&files).expect("extract");
    let graph = habitat_graph_build::assemble(extractions);
    let ours = from_core(&graph);

    // Golden.
    let golden_json = std::fs::read_to_string(dir.join("graph.json")).expect("read httpx golden");
    let golden = from_golden(&golden_json).expect("parse golden");

    let report = classify(&ours, &golden);

    let node_total = golden.nodes.len().max(1);
    let node_pct = report.nodes_matched * 100 / node_total;
    eprintln!("\n=== httpx parity ===");
    eprintln!(
        "NODES: {}/{} golden ids covered ({node_pct}%), {} extra",
        report.nodes_matched,
        golden.nodes.len(),
        report.nodes_extra.len()
    );
    eprintln!("EDGES by relation (matched / golden / ours):");
    let mut structural_matched = 0_usize;
    let mut structural_total = 0_usize;
    for rel in [
        "contains",
        "method",
        "inherits",
        "imports_from",
        "calls",
        "uses",
    ] {
        let g = golden.edges_with_relation(rel);
        let o = ours.edges_with_relation(rel);
        let matched = g.intersection(&o).count();
        eprintln!("  {rel:>12}: {matched} / {} / {}", g.len(), o.len());
        if matches!(rel, "contains" | "method" | "inherits" | "imports_from") {
            structural_matched += matched;
            structural_total += g.len();
        }
    }
    let struct_total = structural_total.max(1);
    let struct_pct = structural_matched * 100 / struct_total;
    eprintln!("STRUCTURAL coverage: {structural_matched}/{structural_total} ({struct_pct}%)\n");

    // Content-equivalence thresholds (integer ratios — no float casts).
    assert!(
        report.nodes_matched * 100 >= golden.nodes.len() * 80,
        "node coverage {node_pct}% should be >= 80%"
    );
    assert!(
        structural_matched * 100 >= structural_total * 70,
        "structural-edge coverage {struct_pct}% should be >= 70%"
    );
}
