//! Generate a graphify-compatible `graph.json` golden for the JavaScript parity gate.
//!
//! Usage (from workspace root):
//!   CARGO_TARGET_DIR=./target cargo run --example gen_golden_js -p habitat-graph-cli
//!
//! Reads `.js` files from `tests/fixtures/goldens/js/raw/` (relative to workspace root),
//! extracts them using [`JsExtractor`], assembles the graph, then writes a graphify-compatible
//! golden to `tests/fixtures/goldens/js/graph.json` where node `"id"` fields are string labels
//! (not integer NodeIds) so `from_golden` can parse them.

use std::collections::BTreeMap;
use std::path::PathBuf;

fn main() {
    // Resolve workspace root = three levels up from this file's crate root.
    let crate_dir = std::env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|_| "crates/habitat-graph-cli".to_owned());
    let workspace_root = PathBuf::from(&crate_dir)
        .join("..") // crates/
        .join(".."); // workspace root

    let raw_dir = workspace_root.join("tests/fixtures/goldens/js/raw");
    let out_path = workspace_root.join("tests/fixtures/goldens/js/graph.json");

    // Detect .js files.
    let files = habitat_graph_source::detect(&raw_dir, &["js"]).unwrap_or_else(|e| {
        eprintln!("detect error: {e}");
        std::process::exit(1);
    });
    eprintln!(
        "detected {} JS file(s) in {}",
        files.len(),
        raw_dir.display()
    );

    // Extract.
    let extractions = habitat_graph_extract::extract_files(&files).unwrap_or_else(|e| {
        eprintln!("extract error: {e}");
        std::process::exit(1);
    });

    // Assemble.
    let mut graph = habitat_graph_build::assemble(extractions);
    graph.communities =
        habitat_graph_analyze::detect_communities(&habitat_graph_analyze::trusted_subgraph(&graph));
    let graph = graph.sorted();

    // Build label-to-community mapping.
    let mut community_map: BTreeMap<String, u32> = BTreeMap::new();
    for c in &graph.communities {
        for &nid in &c.members {
            if let Some(node) = graph.nodes.iter().find(|n| n.id == nid) {
                community_map.insert(node.label.clone(), c.id.get());
            }
        }
    }

    // Build NodeId → label lookup.
    let id_to_label: BTreeMap<habitat_graph_core::NodeId, &str> = graph
        .nodes
        .iter()
        .map(|n| (n.id, n.label.as_str()))
        .collect();

    // Emit graphify-compatible JSON with string IDs (not integer NodeIds).
    let nodes: Vec<serde_json::Value> = graph
        .nodes
        .iter()
        .map(|n| {
            let community = community_map
                .get(&n.label)
                .copied()
                .map_or(serde_json::Value::Null, serde_json::Value::from);
            serde_json::json!({
                "id": n.label,
                "label": n.label,
                "source_file": n.source_file,
                "source_location": format!("L{}", n.source_location.start_line),
                "community": community,
            })
        })
        .collect();

    let links: Vec<serde_json::Value> = graph
        .edges
        .iter()
        .filter_map(|e| {
            let src = *id_to_label.get(&e.source)?;
            let tgt = *id_to_label.get(&e.target)?;
            Some(serde_json::json!({
                "source": src,
                "target": tgt,
                "relation": e.relation,
                "confidence": format!("{:?}", e.confidence).to_uppercase(),
            }))
        })
        .collect();

    let envelope = serde_json::json!({
        "directed": true,
        "multigraph": false,
        "graph": {},
        "nodes": nodes,
        "links": links,
    });

    let json = serde_json::to_string_pretty(&envelope).expect("json serialization");
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    std::fs::write(&out_path, json.as_bytes()).unwrap_or_else(|e| {
        eprintln!("write error: {e}");
        std::process::exit(1);
    });

    eprintln!(
        "wrote {} nodes, {} links -> {}",
        nodes.len(),
        links.len(),
        out_path.display()
    );
}
