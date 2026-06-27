//! D3 community parity gate: Leiden community detection on the httpx corpus produces a
//! non-degenerate structure and co-clusters nodes that graphify's golden places together.
//!
//! Relaxed thresholds are intentional: our Leiden uses structural-only edges
//! (`contains`/`method`/`inherits`/`imports_from`) while graphify's Python Leiden also
//! has `calls`/`uses` — sparser edge input means finer-grained clusters are expected.
//! The gate proves the isolation invariant end-to-end and that clustering is not degenerate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use habitat_graph_core::{Community, Graph, NodeId};
use habitat_graph_fixtures::communities_from_golden;

fn httpx_dir() -> PathBuf {
    PathBuf::from(format!(
        "{}/../../tests/fixtures/goldens/httpx",
        env!("CARGO_MANIFEST_DIR")
    ))
}

/// Builds `node_label → our_community_id` from Leiden output.
fn our_community_map(communities: &[Community], graph: &Graph) -> BTreeMap<String, u32> {
    let id_to_label: BTreeMap<NodeId, &str> = graph
        .nodes
        .iter()
        .map(|n| (n.id, n.label.as_str()))
        .collect();
    let mut map: BTreeMap<String, u32> = BTreeMap::new();
    for community in communities {
        let cid = community.id.get();
        for &nid in &community.members {
            if let Some(&label) = id_to_label.get(&nid) {
                map.insert(label.to_owned(), cid);
            }
        }
    }
    map
}

#[test]
fn httpx_community_structure_reasonable() {
    let dir = httpx_dir();

    // Full pipeline: detect → extract → build → analyze.
    let files =
        habitat_graph_source::detect(&dir.join("raw"), &["py"]).expect("detect py files");
    let extractions = habitat_graph_extract::extract_files(&files).expect("extract");
    let graph = habitat_graph_build::assemble(extractions);
    let communities = habitat_graph_analyze::detect_communities(&graph);

    let node_count = graph.nodes.len();

    eprintln!("\n=== httpx community structure (D3) ===");
    eprintln!("nodes: {node_count}");
    eprintln!("our communities: {}", communities.len());

    // D3.1: Community count is bounded (at least 1, at most one-per-node).
    assert!(
        !communities.is_empty(),
        "detect_communities must return at least one community on the httpx corpus"
    );
    assert!(
        communities.len() <= node_count,
        "community count {} must not exceed node count {}",
        communities.len(),
        node_count
    );

    // D3.2: Isolation invariant holds end-to-end — every node appears in exactly one community.
    let total_members: usize = communities.iter().map(|c| c.members.len()).sum();
    assert_eq!(
        total_members,
        node_count,
        "Σmembers ({total_members}) must equal node_count ({node_count}) — isolation invariant"
    );

    // D3.3: Not all singletons (meaningful clustering requires edge density).
    let non_singleton = communities.iter().filter(|c| c.members.len() >= 2).count();
    assert!(
        non_singleton >= 1,
        "httpx corpus has enough edges for at least one multi-member community; all singletons is degenerate"
    );

    // D3.4: Load golden community structure and check co-clustering precision.
    let golden_json =
        std::fs::read_to_string(dir.join("graph.json")).expect("read httpx golden");
    let golden_communities =
        communities_from_golden(&golden_json).expect("parse golden communities");

    eprintln!("golden communities: {}", golden_communities.len());

    // Find the largest golden community with >= 4 members as a co-clustering probe.
    let large_golden_community: Option<&BTreeSet<String>> = golden_communities
        .values()
        .filter(|s| s.len() >= 4)
        .max_by_key(|s| s.len());

    if let Some(co_cluster_nodes) = large_golden_community {
        eprintln!("largest probe community: {} golden nodes", co_cluster_nodes.len());

        let our_map = our_community_map(&communities, &graph);

        // For each member of the largest golden community that we also extracted,
        // collect its community-id from our Leiden run.
        let our_cids: Vec<u32> = co_cluster_nodes
            .iter()
            .filter_map(|label| our_map.get(label.as_str()))
            .copied()
            .collect();

        if our_cids.len() >= 2 {
            // Find the majority (most common) community-id in our run.
            let mut freq: BTreeMap<u32, usize> = BTreeMap::new();
            for &cid in &our_cids {
                *freq.entry(cid).or_default() += 1;
            }
            let majority_count = freq.values().copied().max().unwrap_or(0);
            let majority_pct = majority_count * 100 / our_cids.len().max(1);

            eprintln!(
                "co-clustering: {majority_count}/{} ({majority_pct}%) in majority community",
                our_cids.len()
            );

            // Threshold: >= 30% co-cluster. Relaxed because our edge set is sparser
            // (no calls/uses), producing finer-grained clusters than graphify's 6-community view.
            assert!(
                majority_count * 100 >= our_cids.len() * 30,
                "co-clustering precision {majority_pct}% should be >= 30% for the largest golden community"
            );
        }
    }

    eprintln!("=== D3 community parity PASS ===\n");
}
