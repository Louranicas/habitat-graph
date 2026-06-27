//! Extract community structure from a graphify committed node-link golden.

use std::collections::{BTreeMap, BTreeSet};

use habitat_graph_core::{GraphError, Result};

/// Parses community assignments from a graphify node-link golden.
///
/// Returns a map: `community_id → Set<node_label>` where node labels are the graphify
/// string node ids (e.g. `"exceptions_httperror"`). Nodes without a `"community"` field
/// or with a non-integer community value are silently skipped.
///
/// # Errors
///
/// Returns [`GraphError::Schema`] if the JSON is syntactically invalid or the `"nodes"` array
/// is absent.
pub fn communities_from_golden(json: &str) -> Result<BTreeMap<u32, BTreeSet<String>>> {
    let root: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| GraphError::Schema(format!("invalid JSON: {e}")))?;

    let nodes_arr = root
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            GraphError::Schema("top-level 'nodes' key is absent or not an array".to_owned())
        })?;

    let mut map: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();

    for node_val in nodes_arr {
        let Some(id) = node_val.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(community) = node_val
            .get("community")
            .and_then(serde_json::Value::as_u64)
            .and_then(|c| u32::try_from(c).ok())
        else {
            continue;
        };
        map.entry(community).or_default().insert(id.to_owned());
    }

    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_httpx_golden() -> String {
        let path = format!(
            "{}/../../tests/fixtures/goldens/httpx/graph.json",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read httpx golden at {path}: {e}"))
    }

    const MINIMAL: &str = r#"{
        "nodes": [
            {"id": "alpha", "community": 0},
            {"id": "beta",  "community": 0},
            {"id": "gamma", "community": 1}
        ],
        "links": []
    }"#;

    #[test]
    fn minimal_two_communities() {
        let map = communities_from_golden(MINIMAL).expect("parse");
        assert_eq!(map.len(), 2);
        assert!(map[&0].contains("alpha"));
        assert!(map[&0].contains("beta"));
        assert!(map[&1].contains("gamma"));
    }

    #[test]
    fn node_without_community_is_skipped() {
        let json = r#"{"nodes": [{"id": "a"}, {"id": "b", "community": 1}], "links": []}"#;
        let map = communities_from_golden(json).expect("parse");
        assert_eq!(map.len(), 1);
        assert!(map[&1].contains("b"));
    }

    #[test]
    fn empty_nodes_array_returns_empty_map() {
        let json = r#"{"nodes": [], "links": []}"#;
        let map = communities_from_golden(json).expect("parse");
        assert!(map.is_empty());
    }

    #[test]
    fn invalid_json_returns_schema_error() {
        let err = communities_from_golden("not json").unwrap_err();
        assert_eq!(err.kind(), "schema");
    }

    #[test]
    fn absent_nodes_key_returns_schema_error() {
        let err = communities_from_golden(r#"{"links": []}"#).unwrap_err();
        assert_eq!(err.kind(), "schema");
    }

    #[test]
    fn httpx_golden_has_multiple_communities() {
        let json = load_httpx_golden();
        let map = communities_from_golden(&json).expect("parse httpx golden");
        assert!(
            map.len() >= 2,
            "httpx golden must have at least 2 communities; got {}",
            map.len()
        );
    }

    #[test]
    fn httpx_golden_largest_community_has_multiple_nodes() {
        let json = load_httpx_golden();
        let map = communities_from_golden(&json).expect("parse httpx golden");
        let max_size = map.values().map(|s| s.len()).max().unwrap_or(0);
        assert!(
            max_size >= 4,
            "largest golden community must have >= 4 members; got {max_size}"
        );
    }

    #[test]
    fn deterministic_repeated_calls() {
        let m1 = communities_from_golden(MINIMAL).expect("first parse");
        let m2 = communities_from_golden(MINIMAL).expect("second parse");
        assert_eq!(m1, m2, "repeated calls must yield identical results");
    }

    #[test]
    fn duplicate_node_in_same_community_deduplicates() {
        let json = r#"{
            "nodes": [
                {"id": "x", "community": 0},
                {"id": "x", "community": 0}
            ],
            "links": []
        }"#;
        let map = communities_from_golden(json).expect("parse");
        assert_eq!(
            map[&0].len(),
            1,
            "duplicate node id in same community must deduplicate"
        );
    }
}
