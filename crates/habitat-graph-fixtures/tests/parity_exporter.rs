//! D4 exporter parity gate: the full pipeline (detect→extract→build→analyze→export→round-trip)
//! remains content-equivalent to graphify's committed golden on the httpx corpus.
//!
//! This extends D2 (extraction parity) by verifying that `to_node_link` serialisation and
//! `from_node_link` deserialisation preserve the node/edge coverage established by the extractor.
//! Any nodes or structural edges silently dropped during the export or round-trip are caught here.

use std::path::PathBuf;

use habitat_graph_fixtures::{classify, from_core, from_golden};

/// Pinned-oracle baseline (R3a): the verified-good httpx coverage; export+round-trip must not drop
/// below it. Ratchet UP when extraction legitimately improves (LS-3).
const HTTPX_NODE_BASELINE: usize = 140; // 140/144 golden (97%)
const HTTPX_STRUCT_BASELINE: usize = 167; // 167/174 golden (96%)

fn httpx_dir() -> PathBuf {
    PathBuf::from(format!(
        "{}/../../tests/fixtures/goldens/httpx",
        env!("CARGO_MANIFEST_DIR")
    ))
}

#[test]
fn httpx_exporter_round_trip_parity() {
    let dir = httpx_dir();

    // D4 step 1: extract pipeline (mirrors D2's parity_httpx test).
    let files =
        habitat_graph_source::detect(&dir.join("raw"), &["py"]).expect("detect py files");
    let extractions = habitat_graph_extract::extract_files(&files).expect("extract");
    let graph = habitat_graph_build::assemble(extractions);

    // D4 step 2: community detection.
    let communities = habitat_graph_analyze::detect_communities(&graph);
    let mut graph = graph;
    graph.communities = communities;

    // D4 step 3: export → node-link JSON, then round-trip back into a Graph.
    let graph = graph.sorted();
    let json = habitat_graph_export::to_node_link(&graph).expect("to_node_link");
    let round_tripped =
        habitat_graph_serve::from_node_link(&json).expect("from_node_link round-trip");
    let round_tripped = round_tripped.sorted();

    // D4 step 4: normalise the round-tripped graph and compare against the golden.
    let ours = from_core(&round_tripped);
    let golden_json =
        std::fs::read_to_string(dir.join("graph.json")).expect("read httpx golden");
    let golden = from_golden(&golden_json).expect("parse golden");

    let report = classify(&ours, &golden);

    let node_total = golden.nodes.len().max(1);
    let node_pct = (report.nodes_matched * 100 + node_total / 2) / node_total; // rounded

    eprintln!("\n=== httpx exporter round-trip parity (D4) ===");
    eprintln!(
        "NODES: {}/{} ({node_pct}%), {} extra",
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
    let struct_pct = (structural_matched * 100 + struct_total / 2) / struct_total; // rounded
    eprintln!("STRUCTURAL coverage: {structural_matched}/{structural_total} ({struct_pct}%)\n");

    // Regression gate (LS-3): export+round-trip must not drop below the PINNED-ORACLE baseline
    // (R3a). Same baselines as the D2 extraction gate — a round-trip that loses an edge fails here.
    assert!(
        report.nodes_matched >= HTTPX_NODE_BASELINE,
        "node coverage REGRESSED after round-trip: {}/{} ({node_pct}%) < baseline {HTTPX_NODE_BASELINE} (R3a)",
        report.nodes_matched,
        golden.nodes.len()
    );
    assert!(
        structural_matched >= HTTPX_STRUCT_BASELINE,
        "structural coverage REGRESSED after round-trip: {structural_matched}/{structural_total} ({struct_pct}%) < baseline {HTTPX_STRUCT_BASELINE} (R3a)"
    );
}
