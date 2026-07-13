//! `axum` router + server wiring over the (immutable, shared) graph.
//!
//! [`build_router`] constructs the three-endpoint `axum` router; [`run_server`] binds a TCP
//! listener and drives the service until the process exits. All business logic lives in the pure
//! functions in [`crate::handlers`] — the handlers here are thin extractors only.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use habitat_graph_core::Graph;
use serde_json::Value;

/// Builds the `axum` router serving `GET /health`, `GET /query?q=`, and `GET /path?from=&to=`
/// over the shared `graph`. Each handler delegates to the pure functions in [`crate::handlers`].
///
/// The returned [`Router`] has the graph state baked in and is ready to be passed to
/// [`axum::serve()`] or used as a `tower::Service` in tests.
#[must_use]
#[allow(clippy::double_must_use, clippy::needless_pass_by_value)]
pub fn build_router(graph: Arc<Graph>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/query", get(query))
        .route("/path", get(path))
        .with_state(graph)
}

/// Runs the HTTP service on `addr` until the process ends.
///
/// Binds a [`tokio::net::TcpListener`], then calls [`axum::serve()`] with the router produced by
/// [`build_router`]. Both the bind step and the serve loop can produce errors, which are converted
/// to `String` for convenient propagation.
///
/// # Errors
/// Returns a `String` description if:
/// - [`tokio::net::TcpListener::bind`] fails (e.g. port already in use, permission denied), or
/// - [`axum::serve()`] returns an I/O error during the serving loop.
pub async fn run_server(graph: Arc<Graph>, addr: SocketAddr) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| e.to_string())?;
    axum::serve(listener, build_router(graph))
        .await
        .map_err(|e| e.to_string())
}

// ── Private axum handlers ────────────────────────────────────────────────────
//
// These are thin extractors: they pull `State` and `Query` params out of the request
// and forward to the sync, pure functions in `crate::handlers`. They must be `async fn`
// to satisfy axum's `Handler` trait bounds even though they contain no `.await` points.

async fn health(State(g): State<Arc<Graph>>) -> Json<Value> {
    Json(crate::handlers::health_json(&g))
}

async fn query(
    State(g): State<Arc<Graph>>,
    Query(p): Query<HashMap<String, String>>,
) -> Json<Value> {
    let q = p.get("q").map_or("", String::as_str);
    Json(crate::handlers::query_json(&g, q))
}

async fn path(
    State(g): State<Arc<Graph>>,
    Query(p): Query<HashMap<String, String>>,
) -> Json<Value> {
    let from = p.get("from").map_or("", String::as_str);
    let to = p.get("to").map_or("", String::as_str);
    Json(crate::handlers::path_json(&g, from, to))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::build_router;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use habitat_graph_core::{
        Confidence, Edge, Graph, Manifest, Node, NodeId, Span, SCHEMA_VERSION,
    };
    use std::sync::Arc;
    use tower::ServiceExt;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_node(id: u32, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: format!("{label}.rs"),
            source_location: Span::new(0, 1, 1, 1),
        }
    }

    fn make_edge(src: u32, tgt: u32) -> Edge {
        Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: "calls".to_owned(),
            confidence: Confidence::Extracted,
        }
    }

    /// A small connected graph: alpha(1)→beta(2); orphan(3) has no edges.
    fn test_graph() -> Arc<Graph> {
        Arc::new(Graph {
            schema: SCHEMA_VERSION.to_owned(),
            nodes: vec![
                make_node(1, "alpha"),
                make_node(2, "beta"),
                make_node(3, "orphan"),
            ],
            node_content_ids: Default::default(),
            edges: vec![make_edge(1, 2)],
            communities: Vec::new(),
            manifest: Manifest::default(),
        })
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    async fn body_str(body: Body) -> String {
        let bytes = to_bytes(body, usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    async fn json_body(body: Body) -> serde_json::Value {
        let s = body_str(body).await;
        serde_json::from_str(&s).unwrap()
    }

    // ── GET /health ───────────────────────────────────────────────────────────

    /// The `/health` route replies with HTTP 200.
    #[tokio::test]
    async fn health_returns_200() {
        let resp = build_router(test_graph())
            .oneshot(get("/health"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    /// The `/health` response body contains the `"version"` key.
    #[tokio::test]
    async fn health_body_has_version_key() {
        let resp = build_router(test_graph())
            .oneshot(get("/health"))
            .await
            .unwrap();
        let body = body_str(resp.into_body()).await;
        assert!(body.contains("version"), "body missing 'version': {body}");
    }

    /// `/health` counts accurately reflect the graph passed to the router.
    #[tokio::test]
    async fn health_counts_reflect_graph() {
        let resp = build_router(test_graph())
            .oneshot(get("/health"))
            .await
            .unwrap();
        let v = json_body(resp.into_body()).await;
        assert_eq!(v["nodes"], 3, "expected 3 nodes");
        assert_eq!(v["edges"], 1, "expected 1 edge");
        assert_eq!(v["communities"], 0, "expected 0 communities");
    }

    /// An empty graph reports zero for all counts in `/health`.
    #[tokio::test]
    async fn health_empty_graph_zero_counts() {
        let resp = build_router(Arc::new(Graph::new()))
            .oneshot(get("/health"))
            .await
            .unwrap();
        let v = json_body(resp.into_body()).await;
        assert_eq!(v["nodes"], 0_u64);
        assert_eq!(v["edges"], 0_u64);
        assert_eq!(v["communities"], 0_u64);
    }

    // ── GET /query ────────────────────────────────────────────────────────────

    /// `GET /query?q=alpha` returns HTTP 200.
    #[tokio::test]
    async fn query_alpha_returns_200() {
        let resp = build_router(test_graph())
            .oneshot(get("/query?q=alpha"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    /// The `/query` response body contains the `"matches"` key.
    #[tokio::test]
    async fn query_body_has_matches_key() {
        let resp = build_router(test_graph())
            .oneshot(get("/query?q=alpha"))
            .await
            .unwrap();
        let body = body_str(resp.into_body()).await;
        assert!(body.contains("matches"), "body missing 'matches': {body}");
    }

    /// Searching for `"alpha"` finds exactly the alpha node with the expected label.
    #[tokio::test]
    async fn query_alpha_finds_one_matching_node() {
        let resp = build_router(test_graph())
            .oneshot(get("/query?q=alpha"))
            .await
            .unwrap();
        let v = json_body(resp.into_body()).await;
        assert_eq!(v["count"], 1_u64, "expected 1 match");
        assert_eq!(v["matches"][0]["label"], "alpha");
    }

    /// Omitting the `q` parameter defaults to an empty needle, which matches all nodes.
    #[tokio::test]
    async fn query_no_q_param_matches_all_nodes() {
        let resp = build_router(test_graph())
            .oneshot(get("/query"))
            .await
            .unwrap();
        let v = json_body(resp.into_body()).await;
        assert_eq!(v["count"], 3_u64, "empty/missing q must match all 3 nodes");
    }

    /// A query with no matching nodes returns `count=0` and an empty `matches` array.
    #[tokio::test]
    async fn query_no_match_empty_matches() {
        let resp = build_router(test_graph())
            .oneshot(get("/query?q=zzz_absent"))
            .await
            .unwrap();
        let v = json_body(resp.into_body()).await;
        assert_eq!(v["count"], 0_u64);
        assert!(v["matches"].as_array().unwrap().is_empty());
    }

    /// The `count` field always equals the length of the `matches` array.
    #[tokio::test]
    async fn query_count_equals_matches_array_length() {
        let resp = build_router(test_graph())
            .oneshot(get("/query?q=a")) // "alpha" and "orphan" both contain 'a'
            .await
            .unwrap();
        let v = json_body(resp.into_body()).await;
        let count = v["count"].as_u64().unwrap();
        let len = u64::try_from(v["matches"].as_array().unwrap().len()).unwrap();
        assert_eq!(count, len, "count must equal matches.length");
    }

    // ── GET /path ─────────────────────────────────────────────────────────────

    /// `GET /path?from=alpha&to=beta` returns HTTP 200.
    #[tokio::test]
    async fn path_alpha_to_beta_returns_200() {
        let resp = build_router(test_graph())
            .oneshot(get("/path?from=alpha&to=beta"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    /// The `/path` response body contains the `"found"` key.
    #[tokio::test]
    async fn path_body_has_found_key() {
        let resp = build_router(test_graph())
            .oneshot(get("/path?from=alpha&to=beta"))
            .await
            .unwrap();
        let body = body_str(resp.into_body()).await;
        assert!(body.contains("found"), "body missing 'found': {body}");
    }

    /// Connected nodes (alpha→beta edge exists) produce `"found": true`.
    #[tokio::test]
    async fn path_connected_nodes_found_true() {
        let resp = build_router(test_graph())
            .oneshot(get("/path?from=alpha&to=beta"))
            .await
            .unwrap();
        let v = json_body(resp.into_body()).await;
        assert_eq!(v["found"], true);
    }

    /// Disconnected nodes (orphan has no edges) produce `"found": false` and an empty path.
    #[tokio::test]
    async fn path_disconnected_nodes_found_false() {
        let resp = build_router(test_graph())
            .oneshot(get("/path?from=alpha&to=orphan"))
            .await
            .unwrap();
        let v = json_body(resp.into_body()).await;
        assert_eq!(v["found"], false);
        assert!(v["path"].as_array().unwrap().is_empty());
    }

    /// `from == to` produces a trivial path of length 1 containing that label.
    #[tokio::test]
    async fn path_same_node_trivial_path() {
        let resp = build_router(test_graph())
            .oneshot(get("/path?from=alpha&to=alpha"))
            .await
            .unwrap();
        let v = json_body(resp.into_body()).await;
        assert_eq!(v["found"], true);
        let path = v["path"].as_array().unwrap();
        assert_eq!(path.len(), 1);
        assert_eq!(path[0], "alpha");
    }

    /// The `path` array contains the correct labels in traversal order.
    #[tokio::test]
    async fn path_labels_in_traversal_order() {
        let resp = build_router(test_graph())
            .oneshot(get("/path?from=alpha&to=beta"))
            .await
            .unwrap();
        let v = json_body(resp.into_body()).await;
        let labels: Vec<&str> = v["path"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(labels, ["alpha", "beta"]);
    }

    // ── Unknown routes ────────────────────────────────────────────────────────

    /// Any route not registered on the router returns HTTP 404.
    #[tokio::test]
    async fn unknown_route_returns_404() {
        let resp = build_router(test_graph())
            .oneshot(get("/nope"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    /// A POST to a GET-only route also returns HTTP 405 or 404 (not 200).
    #[tokio::test]
    async fn post_to_get_only_route_not_200() {
        let req = Request::builder()
            .method("POST")
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = build_router(test_graph()).oneshot(req).await.unwrap();
        assert_ne!(resp.status(), 200);
    }
}
