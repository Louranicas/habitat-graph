//! `UDS` warm-daemon (`FO-6`) — serve `MCP` JSON-RPC over a Unix socket with atomically
//! swappable graph state.
//!
//! A long-lived daemon binds a Unix socket at `path` and serves the `MCP` JSON-RPC surface
//! ([`handle_jsonrpc`]) in a line-oriented framing: one JSON-RPC request per line produces one
//! response line (notifications — requests without an `id` — elicit no response). The served
//! state — the [`Graph`] and its prebuilt [`LabelIndex`] — is held in [`arc_swap::ArcSwap`],
//! enabling atomic, downtime-free reload: in-flight requests complete against the prior snapshot
//! while new requests see the updated graph.
//!
//! # Security hardening
//!
//! A `BufReader::lines()` / `next_line()` call accumulates the entire line before returning; a
//! malicious local client that sends a gigabyte with no newline would exhaust process memory.
//! [`serve_uds`] uses [`read_bounded_line`] — a manual [`AsyncBufRead`] fill/consume loop — to
//! cap the per-line buffer at [`MAX_LINE_BYTES`]. Connections that exceed the cap receive a
//! JSON-RPC parse-error response (`code -32700`) and are closed.

use std::io;
use std::pin::Pin;
use std::sync::Arc;

use arc_swap::ArcSwap;
use habitat_graph_core::{Graph, NodeId};
use habitat_graph_serve::{handle_jsonrpc, LabelIndex};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum content bytes buffered for a single incoming `MCP` JSON-RPC request line.
///
/// Once accumulated bytes exceed this value without a `\n`, the connection receives a
/// JSON-RPC parse-error response (`code -32700`) and is closed. The value (1 MiB) is generous
/// for any legitimate `MCP` request while bounding worst-case per-connection allocation to ~1 MiB.
pub const MAX_LINE_BYTES: usize = 1_048_576; // 1 MiB

/// Maximum number of concurrently-served connections — backpressure so a flood of connections
/// cannot exhaust file descriptors / tasks / memory. Excess connections are dropped immediately.
pub const MAX_CONNECTIONS: usize = 256;

/// The JSON-RPC parse-error response sent when an incoming line exceeds [`MAX_LINE_BYTES`].
///
/// A static literal avoids a runtime allocation in the error path.
const LINE_TOO_LONG_RESPONSE: &str = r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"line exceeded maximum length"}}"#;

// ── WarmState ─────────────────────────────────────────────────────────────────

/// An immutable, co-published `(graph, index)` pair. Held behind one [`ArcSwap`] so a reload swaps
/// **both** in a single atomic store — a reader can never observe a new graph with a stale index.
struct Snapshot {
    graph: Arc<Graph>,
    index: Arc<LabelIndex>,
}

/// Atomically-swappable warm state: the served [`Graph`] and its prebuilt [`LabelIndex`].
///
/// Held behind one [`ArcSwap`] over a [`Snapshot`] so [`WarmState::reload`] replaces both
/// atomically — in-flight requests keep serving the prior snapshot; new requests see the new one;
/// no lock; no downtime.
pub struct WarmState {
    snapshot: ArcSwap<Snapshot>,
}

impl WarmState {
    /// Builds warm state from `graph`, constructing the [`LabelIndex`] once.
    ///
    /// The index is held warm so subsequent [`Self::find`] calls answer queries in sub-linear
    /// time (for needles ≥ 3 chars) without a per-call rebuild — the `FO-3` payoff.
    #[must_use]
    pub fn new(graph: Graph) -> Arc<Self> {
        let index = LabelIndex::build(&graph);
        Arc::new(Self {
            snapshot: ArcSwap::from_pointee(Snapshot {
                graph: Arc::new(graph),
                index: Arc::new(index),
            }),
        })
    }

    /// Atomically swaps in a new `graph` and a freshly-built [`LabelIndex`] — zero downtime.
    ///
    /// In-flight calls to [`Self::handle`] or [`Self::find`] that already loaded the prior
    /// snapshot complete normally; calls that start after `reload` returns see the new graph.
    pub fn reload(&self, graph: Graph) {
        let index = LabelIndex::build(&graph);
        // Single atomic store of the co-published pair — graph and index swap together, so a reader
        // can never see a new graph paired with the old index (fixes the two-store tear).
        self.snapshot.store(Arc::new(Snapshot {
            graph: Arc::new(graph),
            index: Arc::new(index),
        }));
    }

    /// Handles one `MCP` JSON-RPC request line against the current warm graph snapshot.
    ///
    /// Returns an empty string for notifications (requests without an `id`); returns a valid
    /// JSON-RPC response for all other inputs, including a parse-error for malformed JSON.
    #[must_use]
    pub fn handle(&self, request: &str) -> String {
        let snap = self.snapshot.load();
        handle_jsonrpc(&snap.graph, request)
    }

    /// Warm label lookup using the held [`LabelIndex`] — no per-call rebuild (`FO-3` win).
    ///
    /// Returns matching [`NodeId`]s in ascending order, identical to a brute-force scan.
    /// Empty `needle` matches every node; needles shorter than 3 chars use a linear fallback.
    #[must_use]
    pub fn find(&self, needle: &str) -> Vec<NodeId> {
        self.snapshot.load().index.find(needle)
    }
}

// ── serve_uds ─────────────────────────────────────────────────────────────────

/// Serves `MCP` JSON-RPC over a Unix socket bound at `path` until the listener errors.
///
/// Removes any stale socket file at `path` before binding, so successive calls on the same path
/// succeed without manual cleanup. Each accepted connection is handled on its own
/// [`tokio::task`] against the shared `state`; per-connection errors are silently discarded so
/// one bad connection never kills the daemon.
///
/// Resilience: a per-`accept` error (e.g. `EMFILE` on fd exhaustion, an `ECONNABORTED` race) is
/// retried after a short back-off — it never terminates the accept loop. Concurrent connections are
/// bounded by [`MAX_CONNECTIONS`]; the socket node is restricted to the owner (`0o600`).
///
/// # Errors
///
/// Returns the [`io::Error`] only from the initial [`UnixListener::bind`]; once bound, the daemon
/// serves indefinitely (per-`accept` errors are retried, not propagated).
pub async fn serve_uds(path: &std::path::Path, state: Arc<WarmState>) -> io::Result<()> {
    let _ = std::fs::remove_file(path); // remove stale socket; absent-file error is expected
    let listener = UnixListener::bind(path)?;
    // Restrict the socket node to the owner so other local users cannot connect (info-disclosure +
    // DoS surface). Unix-only; best-effort (a failure here does not abort serving).
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    // Bound concurrent connections — backpressure against a connection flood.
    let limiter = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        // A failed `accept` (EMFILE/ENFILE on fd exhaustion, ECONNABORTED race, …) must NOT kill
        // the daemon. Back off briefly so a persistent error does not busy-spin, then keep serving.
        let (stream, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_e) => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                continue;
            }
        };
        // At capacity → drop the connection rather than spawning unboundedly.
        let Ok(permit) = Arc::clone(&limiter).try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let st = Arc::clone(&state);
        tokio::spawn(async move {
            let _permit = permit; // released when the connection task ends
            let _ = handle_conn(stream, st).await;
        });
    }
}

// ── handle_conn ───────────────────────────────────────────────────────────────

/// Drives a single `UDS` connection: reads `MCP` request lines, writes response lines.
///
/// Lines exceeding [`MAX_LINE_BYTES`] receive [`LINE_TOO_LONG_RESPONSE`] and terminate the
/// connection. Blank/whitespace-only lines are silently skipped. Write-half errors are swallowed
/// (client disconnected); only read-side errors that are not `ErrorKind::InvalidData` propagate.
///
/// # Errors
///
/// Returns an [`io::Error`] for read-side errors other than `ErrorKind::InvalidData`.
async fn handle_conn(stream: tokio::net::UnixStream, state: Arc<WarmState>) -> io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    loop {
        match read_bounded_line(&mut reader, MAX_LINE_BYTES).await {
            Ok(None) => break, // clean EOF
            Ok(Some(raw)) if raw.iter().all(u8::is_ascii_whitespace) => {}
            Ok(Some(raw)) => {
                let line = String::from_utf8_lossy(&raw);
                let response = state.handle(&line);
                if !response.is_empty() {
                    if write_half.write_all(response.as_bytes()).await.is_err() {
                        break;
                    }
                    if write_half.write_all(b"\n").await.is_err() {
                        break;
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                // Over-long line: send parse-error then close the connection.
                let _ = write_half
                    .write_all(LINE_TOO_LONG_RESPONSE.as_bytes())
                    .await;
                let _ = write_half.write_all(b"\n").await;
                break;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// ── read_bounded_line ─────────────────────────────────────────────────────────

/// Reads one `\n`-terminated line from `reader`, capped at `max_bytes` content bytes.
///
/// Uses the [`AsyncBufRead`] internal buffer directly (via `fill_buf` + `consume`) to avoid
/// unbounded allocation: bytes are inspected in-place and copied into an owned buffer only as
/// needed. The immutable borrow of `reader` via the returned slice is released at the end of a
/// block scope before the mutable `consume` call, satisfying the borrow checker.
///
/// Returns:
/// - `Ok(Some(bytes))` — line content without the trailing `\n` (or `\r\n`).
/// - `Ok(None)` — clean `EOF` with no pending bytes.
/// - `Err(ErrorKind::InvalidData)` — accumulated content exceeded `max_bytes` before `\n`.
/// - `Err(e)` — any other `IO` error from the underlying stream.
///
/// # Errors
///
/// Returns `Err` on `IO` errors, or `Err(ErrorKind::InvalidData)` when the per-line cap is hit.
async fn read_bounded_line<R>(reader: &mut R, max_bytes: usize) -> io::Result<Option<Vec<u8>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut buf: Vec<u8> = Vec::new();
    loop {
        // Borrow `reader` immutably via `fill_buf`, inspect the available bytes, copy what we
        // need, then drop the slice (releasing the borrow) before the mutable `consume` call.
        let (found_nl, consume_len) = {
            let avail = reader.fill_buf().await?;
            if avail.is_empty() {
                // EOF — return whatever was accumulated, or None if nothing.
                return Ok(if buf.is_empty() { None } else { Some(buf) });
            }
            let chunk_len = avail.len();
            match avail.iter().position(|&b| b == b'\n') {
                Some(nl_pos) => {
                    // Copy content up to (but not including) the `\n`.
                    buf.extend_from_slice(&avail[..nl_pos]);
                    // consume_len includes the `\n` itself.
                    (true, nl_pos + 1)
                }
                None => {
                    // No newline in this chunk — enforce the cap before accumulating.
                    if buf.len().saturating_add(chunk_len) > max_bytes {
                        // Over limit: use sentinel 0 to signal this after the borrow ends.
                        (false, 0_usize)
                    } else {
                        buf.extend_from_slice(avail);
                        (false, chunk_len)
                    }
                }
            }
            // `avail` drops here, releasing the immutable borrow of `reader`.
        };

        if found_nl {
            // Consume through and including the `\n`.
            Pin::new(&mut *reader).consume(consume_len);
            // Strip optional `\r` that preceded the `\n`.
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return Ok(Some(buf));
        }

        if consume_len == 0 {
            // Sentinel: cap exceeded, no data consumed.
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("incoming line exceeded {max_bytes}-byte limit"),
            ));
        }

        // Normal no-newline chunk: already accumulated in the block; advance the reader.
        Pin::new(&mut *reader).consume(consume_len);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use habitat_graph_core::{Confidence, Edge, Graph, Node, NodeId, Span};
    use serde_json::Value;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    use super::{serve_uds, WarmState, MAX_LINE_BYTES};

    // ── test helpers ──────────────────────────────────────────────────────────

    static SOCKET_CTR: AtomicU64 = AtomicU64::new(0);

    fn unique_socket_path() -> std::path::PathBuf {
        let n = SOCKET_CTR.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "hg-uds-test-{pid}-{n}.sock",
            pid = std::process::id(),
        ))
    }

    fn graph_with(labels: &[(u32, &str)]) -> Graph {
        let mut g = Graph::new();
        for (id, l) in labels {
            g.nodes.push(Node {
                id: NodeId::new(*id),
                label: (*l).to_owned(),
                source_file: "test.rs".to_owned(),
                source_location: Span::new(0, 1, 1, 1),
            });
        }
        g
    }

    fn graph_with_edge(labels: &[(u32, &str)], src: u32, tgt: u32) -> Graph {
        let mut g = graph_with(labels);
        g.edges.push(Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: "calls".to_owned(),
            confidence: Confidence::Extracted,
        });
        g
    }

    /// Polls `path` until a real connection succeeds or 200 x 1 ms = 200 ms have elapsed.
    ///
    /// Using `std::thread::sleep` (blocking) is intentional: the test uses a `multi_thread`
    /// runtime with ≥ 2 workers, so the server task runs on a different OS thread and is
    /// never starved by this sleep. Pure `yield_now` is insufficient under load because
    /// many concurrent test futures can exhaust the yield budget without the server task
    /// getting OS-scheduled time.
    async fn wait_for_server(path: &std::path::Path) {
        for _ in 0_u32..200 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            if UnixStream::connect(path).await.is_ok() {
                return;
            }
        }
    }

    /// Spawns a `serve_uds` task and waits until the socket is connectable.
    async fn start_test_server(graph: Graph) -> (std::path::PathBuf, Arc<WarmState>) {
        let path = unique_socket_path();
        let state = WarmState::new(graph);
        let st = Arc::clone(&state);
        let p = path.clone();
        tokio::spawn(async move {
            let _ = serve_uds(&p, st).await;
        });
        wait_for_server(&path).await;
        (path, state)
    }

    /// Sends `request` (without trailing newline) and reads one response line.
    async fn request_response(path: &std::path::Path, request: &str) -> String {
        let mut conn = UnixStream::connect(path).await.expect("connect");
        conn.write_all(request.as_bytes()).await.expect("write");
        conn.write_all(b"\n").await.expect("write newline");
        let mut reader = tokio::io::BufReader::new(conn);
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read");
        line.trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_owned()
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Sync unit tests — WarmState
    // ══════════════════════════════════════════════════════════════════════════

    /// 1 — warm state construction builds an index for immediate label queries.
    #[test]
    fn new_builds_warm_find() {
        let st = WarmState::new(graph_with(&[(1, "HttpClient"), (2, "TcpServer")]));
        assert_eq!(st.find("client"), vec![NodeId::new(1)]);
    }

    /// 2 — label search is case-insensitive, using the held warm index.
    #[test]
    fn find_case_insensitive_lookup() {
        let st = WarmState::new(graph_with(&[(1, "FooBar")]));
        assert_eq!(st.find("foobar"), vec![NodeId::new(1)]);
        assert_eq!(st.find("FOOBAR"), vec![NodeId::new(1)]);
        assert_eq!(st.find("FooBar"), vec![NodeId::new(1)]);
    }

    /// 3 — empty needle matches all nodes in ascending node-id order.
    #[test]
    fn find_empty_needle_matches_all() {
        let st = WarmState::new(graph_with(&[(3, "C"), (1, "A"), (2, "B")]));
        assert_eq!(
            st.find(""),
            vec![NodeId::new(1), NodeId::new(2), NodeId::new(3)]
        );
    }

    /// 4 — label search with no match returns empty.
    #[test]
    fn find_no_match_returns_empty() {
        let st = WarmState::new(graph_with(&[(1, "alpha")]));
        assert!(st.find("zzz").is_empty());
    }

    /// 5 — multiple matching nodes are returned in ascending node-id order, not label order.
    #[test]
    fn find_multiple_nodes_ascending_ids() {
        let st = WarmState::new(graph_with(&[(5, "node_a"), (2, "node_b"), (8, "node_c")]));
        let ids: Vec<u32> = st.find("node").iter().map(|n| n.get()).collect();
        assert_eq!(ids, vec![2, 5, 8]);
    }

    /// 6 — short (< 3 char) needle uses the linear fallback path inside the warm index.
    #[test]
    fn find_short_needle_fallback_path() {
        let st = WarmState::new(graph_with(&[(1, "axyz"), (2, "bxyz")]));
        let ids: Vec<u32> = st.find("ax").iter().map(|n| n.get()).collect();
        assert_eq!(ids, vec![1]);
    }

    /// 7 — atomic reload swaps both the graph and the label index.
    #[test]
    fn reload_swaps_graph_and_index() {
        let st = WarmState::new(graph_with(&[(1, "alpha")]));
        assert_eq!(st.find("alpha"), vec![NodeId::new(1)]);
        st.reload(graph_with(&[(2, "beta")]));
        assert!(st.find("alpha").is_empty());
        assert_eq!(st.find("beta"), vec![NodeId::new(2)]);
    }

    /// 8 — old label is absent after reload.
    #[test]
    fn reload_old_label_absent_after_reload() {
        let st = WarmState::new(graph_with(&[(1, "OldThing")]));
        st.reload(graph_with(&[(1, "NewThing")]));
        assert!(st.find("OldThing").is_empty());
    }

    /// 9 — new label is present after reload.
    #[test]
    fn reload_new_label_present_after_reload() {
        let st = WarmState::new(graph_with(&[(1, "OldThing")]));
        st.reload(graph_with(&[(1, "NewThing")]));
        assert_eq!(st.find("NewThing"), vec![NodeId::new(1)]);
    }

    /// 10 — reloading twice: the second reload wins.
    #[test]
    fn reload_twice_second_wins() {
        let st = WarmState::new(graph_with(&[(1, "first")]));
        st.reload(graph_with(&[(2, "second")]));
        st.reload(graph_with(&[(3, "third")]));
        assert!(st.find("first").is_empty());
        assert!(st.find("second").is_empty());
        assert_eq!(st.find("third"), vec![NodeId::new(3)]);
    }

    /// 11 — initialize request returns a valid response body.
    #[test]
    fn handle_initialize_response() {
        let st = WarmState::new(graph_with(&[(1, "x")]));
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let v: Value = serde_json::from_str(&st.handle(req)).unwrap();
        assert!(v["result"]["protocolVersion"].is_string());
        assert_eq!(v["result"]["serverInfo"]["name"], "habitat-graph");
    }

    /// 12 — tools/list advertises exactly four tools.
    #[test]
    fn handle_tools_list_four_tools() {
        let st = WarmState::new(graph_with(&[(1, "x")]));
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let v: Value = serde_json::from_str(&st.handle(req)).unwrap();
        assert_eq!(v["result"]["tools"].as_array().unwrap().len(), 4);
    }

    /// 13 — the warm graph-health tool reflects the current graph counts.
    #[test]
    fn handle_tools_call_graph_health() {
        let st = WarmState::new(graph_with(&[(1, "A"), (2, "B")]));
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"graph_health","arguments":{}}}"#;
        let resp = st.handle(req);
        assert!(resp.contains("nodes=2"), "{resp}");
        assert!(resp.contains("edges=0"), "{resp}");
    }

    /// 14 — the graph-query tool finds matching nodes.
    #[test]
    fn handle_tools_call_graph_query_hit() {
        let st = WarmState::new(graph_with(&[(1, "FooModule")]));
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"graph_query","arguments":{"query":"foo"}}}"#;
        assert!(st.handle(req).contains("FooModule"));
    }

    /// 15 — the graph-query tool reports no match correctly.
    #[test]
    fn handle_tools_call_graph_query_miss() {
        let st = WarmState::new(graph_with(&[(1, "FooModule")]));
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"graph_query","arguments":{"query":"zzz"}}}"#;
        assert!(st.handle(req).contains("no nodes match"));
    }

    /// 16 — the graph-path tool traverses edges.
    #[test]
    fn handle_tools_call_graph_path() {
        let g = graph_with_edge(&[(1, "Alpha"), (2, "Beta")], 1, 2);
        let st = WarmState::new(g);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"graph_path","arguments":{"from":"Alpha","to":"Beta"}}}"#;
        assert!(st.handle(req).contains("Alpha -> Beta"));
    }

    /// 17 — the graph-explain tool mentions the queried concept.
    #[test]
    fn handle_tools_call_graph_explain() {
        let st = WarmState::new(graph_with(&[(1, "Alpha")]));
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"graph_explain","arguments":{"concept":"Alpha"}}}"#;
        assert!(st.handle(req).contains("Alpha"));
    }

    /// 18 — malformed JSON returns a JSON-RPC parse error response, not a panic.
    #[test]
    fn handle_malformed_json_parse_error() {
        let st = WarmState::new(graph_with(&[(1, "x")]));
        let v: Value = serde_json::from_str(&st.handle("not json")).unwrap();
        assert_eq!(v["error"]["code"], -32700);
        assert!(v["id"].is_null());
    }

    /// 19 — notifications (requests without an id field) produce an empty response string.
    #[test]
    fn handle_notification_empty_response() {
        let st = WarmState::new(graph_with(&[(1, "x")]));
        let notif = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert_eq!(st.handle(notif), "");
    }

    /// 20 — explicit `null` id is NOT a notification; a response must be sent.
    #[test]
    fn handle_null_id_is_not_notification() {
        let st = WarmState::new(graph_with(&[(1, "x")]));
        let req = r#"{"jsonrpc":"2.0","id":null,"method":"initialize"}"#;
        let resp = st.handle(req);
        assert!(!resp.is_empty());
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(v["result"]["protocolVersion"].is_string());
    }

    /// 21 — string `id` is echoed verbatim in the response.
    #[test]
    fn handle_string_id_echoed() {
        let st = WarmState::new(graph_with(&[(1, "x")]));
        let req = r#"{"jsonrpc":"2.0","id":"req-abc","method":"initialize"}"#;
        let v: Value = serde_json::from_str(&st.handle(req)).unwrap();
        assert_eq!(v["id"], "req-abc");
    }

    /// 22 — unknown method returns method-not-found error (-32601).
    #[test]
    fn handle_unknown_method_error() {
        let st = WarmState::new(graph_with(&[(1, "x")]));
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"frobnicate"}"#;
        let v: Value = serde_json::from_str(&st.handle(req)).unwrap();
        assert_eq!(v["error"]["code"], -32601);
    }

    /// 23 — resources/list returns URIs containing "habitat-graph://".
    #[test]
    fn handle_resources_list_response() {
        let st = WarmState::new(graph_with(&[(1, "x")]));
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#;
        assert!(st.handle(req).contains("habitat-graph://"));
    }

    /// 24 — resources/read schema returns a text body containing the schema version.
    #[test]
    fn handle_resources_read_schema() {
        let st = WarmState::new(graph_with(&[(1, "x")]));
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"resources/read","params":{"uri":"habitat-graph://schema"}}"#;
        assert!(st.handle(req).contains("schema_version"));
    }

    /// 25 — health query on an empty graph reports zero counts.
    #[test]
    fn handle_empty_graph() {
        let st = WarmState::new(Graph::new());
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"graph_health","arguments":{}}}"#;
        assert!(st.handle(req).contains("nodes=0 edges=0"));
    }

    /// 26 — reloading to an empty graph clears the warm index.
    #[test]
    fn reload_to_empty_graph() {
        let st = WarmState::new(graph_with(&[(1, "Alpha")]));
        st.reload(Graph::new());
        assert!(st.find("Alpha").is_empty());
        assert!(st.find("").is_empty());
    }

    /// 27 — reloading from empty to non-empty makes new nodes findable.
    #[test]
    fn reload_from_empty_to_nonempty() {
        let st = WarmState::new(Graph::new());
        st.reload(graph_with(&[(1, "Alpha")]));
        assert_eq!(st.find("Alpha"), vec![NodeId::new(1)]);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Async UDS socket round-trip tests
    // ══════════════════════════════════════════════════════════════════════════

    /// 28 — initialize round-trip returns protocolVersion.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_initialize_round_trip() {
        let (path, _st) = start_test_server(graph_with(&[(1, "hello")])).await;
        let resp =
            request_response(&path, r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(v["result"]["protocolVersion"].is_string(), "{resp}");
        assert_eq!(v["id"], 1);
        let _ = std::fs::remove_file(&path);
    }

    /// 29 — tools/list round-trip returns tools array.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_tools_list_round_trip() {
        let (path, _st) = start_test_server(graph_with(&[(1, "x")])).await;
        let resp =
            request_response(&path, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(v["result"]["tools"].as_array().is_some(), "{resp}");
        let _ = std::fs::remove_file(&path);
    }

    /// 30 — health round-trip reflects actual node count.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_graph_health_round_trip() {
        let (path, _st) = start_test_server(graph_with(&[(1, "A"), (2, "B")])).await;
        let req = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"graph_health","arguments":{}}}"#;
        let resp = request_response(&path, req).await;
        assert!(resp.contains("nodes=2"), "{resp}");
        let _ = std::fs::remove_file(&path);
    }

    /// 31 — graph query match returns the matching node label.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_graph_query_match_round_trip() {
        let (path, _st) = start_test_server(graph_with(&[(1, "SearchTarget")])).await;
        let req = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"graph_query","arguments":{"query":"search"}}}"#;
        let resp = request_response(&path, req).await;
        assert!(resp.contains("SearchTarget"), "{resp}");
        let _ = std::fs::remove_file(&path);
    }

    /// 32 — graph query with no match reports "no nodes match".
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_graph_query_no_match_round_trip() {
        let (path, _st) = start_test_server(graph_with(&[(1, "Alpha")])).await;
        let req = r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"graph_query","arguments":{"query":"zzz_absent"}}}"#;
        let resp = request_response(&path, req).await;
        assert!(resp.contains("no nodes match"), "{resp}");
        let _ = std::fs::remove_file(&path);
    }

    /// 33 — graph path round-trip returns traversal labels.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_graph_path_round_trip() {
        let g = graph_with_edge(&[(1, "Src"), (2, "Dst")], 1, 2);
        let (path, _st) = start_test_server(g).await;
        let req = r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"graph_path","arguments":{"from":"Src","to":"Dst"}}}"#;
        let resp = request_response(&path, req).await;
        assert!(resp.contains("Src -> Dst"), "{resp}");
        let _ = std::fs::remove_file(&path);
    }

    /// 34 — graph explain round-trip mentions the queried concept.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_graph_explain_round_trip() {
        let (path, _st) = start_test_server(graph_with(&[(1, "Concept")])).await;
        let req = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"graph_explain","arguments":{"concept":"Concept"}}}"#;
        let resp = request_response(&path, req).await;
        assert!(resp.contains("Concept"), "{resp}");
        let _ = std::fs::remove_file(&path);
    }

    /// 35 — malformed JSON gives parse-error response; server stays up for next request.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_malformed_json_parse_error_not_crash() {
        let (path, _st) = start_test_server(graph_with(&[(1, "x")])).await;
        let resp = request_response(&path, "definitely not json").await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["error"]["code"], -32700, "{resp}");
        // Server must still accept the next connection.
        let resp2 =
            request_response(&path, r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).await;
        let v2: Value = serde_json::from_str(&resp2).unwrap();
        assert!(v2["result"]["protocolVersion"].is_string(), "{resp2}");
        let _ = std::fs::remove_file(&path);
    }

    /// 36 — blank lines (no content) elicit no response; subsequent request still works.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_empty_line_no_response() {
        let (path, _st) = start_test_server(graph_with(&[(1, "x")])).await;
        let mut conn = UnixStream::connect(&path).await.unwrap();
        // Two blank lines (no response expected), then a real request.
        conn.write_all(b"\n\n").await.unwrap();
        conn.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n")
            .await
            .unwrap();
        // Should receive exactly ONE response line.
        let mut reader = tokio::io::BufReader::new(conn);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        assert!(v["result"]["protocolVersion"].is_string(), "{line}");
        let _ = std::fs::remove_file(&path);
    }

    /// 37 — notifications (no `id`) receive no response line; next request echoes correct id.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_notification_no_response() {
        let (path, _st) = start_test_server(graph_with(&[(1, "x")])).await;
        let mut conn = UnixStream::connect(&path).await.unwrap();
        conn.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        conn.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"initialize\"}\n")
            .await
            .unwrap();
        let mut reader = tokio::io::BufReader::new(conn);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["id"], 99, "{line}");
        let _ = std::fs::remove_file(&path);
    }

    /// 38 — multiple requests on one connection each return the correct id.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_multiple_requests_one_connection() {
        let (path, _st) = start_test_server(graph_with(&[(1, "x"), (2, "y")])).await;
        let mut conn = UnixStream::connect(&path).await.unwrap();
        let mut reader = tokio::io::BufReader::new(&mut conn);
        for i in 1_u32..=5 {
            let req = format!(r#"{{"jsonrpc":"2.0","id":{i},"method":"initialize"}}"#);
            reader.get_mut().write_all(req.as_bytes()).await.unwrap();
            reader.get_mut().write_all(b"\n").await.unwrap();
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let v: Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(v["id"], i64::from(i), "i={i}: {line}");
        }
        let _ = std::fs::remove_file(&path);
    }

    /// 39 — every response is terminated by a `\n` byte.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_response_ends_with_newline() {
        let (path, _st) = start_test_server(graph_with(&[(1, "x")])).await;
        let mut conn = UnixStream::connect(&path).await.unwrap();
        conn.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n")
            .await
            .unwrap();
        // Allow the server one scheduling turn to respond.
        tokio::task::yield_now().await;
        let mut buf = [0u8; 8192];
        let n = conn.read(&mut buf).await.unwrap();
        assert!(n > 0, "expected at least one byte");
        assert_eq!(buf[n - 1], b'\n', "response must end with '\\n'");
        let _ = std::fs::remove_file(&path);
    }

    /// 40 — resources/list round-trip advertises habitat-graph:// URIs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_resources_list_round_trip() {
        let (path, _st) = start_test_server(graph_with(&[(1, "x")])).await;
        let resp = request_response(
            &path,
            r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#,
        )
        .await;
        assert!(resp.contains("habitat-graph://"), "{resp}");
        let _ = std::fs::remove_file(&path);
    }

    /// 41 — two concurrent connections are both served correctly.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_concurrent_two_connections() {
        let (path, _st) = start_test_server(graph_with(&[(1, "ConcNode")])).await;
        let p1 = path.clone();
        let p2 = path.clone();
        let (r1, r2) = tokio::join!(
            tokio::spawn(async move {
                request_response(&p1, r#"{"jsonrpc":"2.0","id":10,"method":"initialize"}"#).await
            }),
            tokio::spawn(async move {
                request_response(&p2, r#"{"jsonrpc":"2.0","id":20,"method":"initialize"}"#).await
            }),
        );
        let v1: Value = serde_json::from_str(&r1.unwrap()).unwrap();
        let v2: Value = serde_json::from_str(&r2.unwrap()).unwrap();
        assert_eq!(v1["id"], 10);
        assert_eq!(v2["id"], 20);
        let _ = std::fs::remove_file(&path);
    }

    /// 42 — ten concurrent connections all receive correct, independent responses.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn socket_concurrent_many_connections() {
        let (path, _st) = start_test_server(graph_with(&[(1, "x")])).await;
        let handles: Vec<_> = (0_u32..10)
            .map(|i| {
                let p = path.clone();
                tokio::spawn(async move {
                    let req = format!(r#"{{"jsonrpc":"2.0","id":{i},"method":"initialize"}}"#);
                    let resp = request_response(&p, &req).await;
                    let v: Value = serde_json::from_str(&resp).unwrap();
                    assert_eq!(v["id"], i64::from(i), "id mismatch for i={i}");
                })
            })
            .collect();
        for h in handles {
            h.await.unwrap();
        }
        let _ = std::fs::remove_file(&path);
    }

    /// 43 — requests after reload see the new graph state.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_reload_while_serving() {
        let (path, state) = start_test_server(graph_with(&[(1, "before")])).await;
        let health_req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"graph_health","arguments":{}}}"#;
        let resp1 = request_response(&path, health_req).await;
        assert!(resp1.contains("nodes=1"), "before reload: {resp1}");
        state.reload(graph_with(&[(1, "a"), (2, "b"), (3, "c")]));
        let resp2 = request_response(&path, health_req).await;
        assert!(resp2.contains("nodes=3"), "after reload: {resp2}");
        let _ = std::fs::remove_file(&path);
    }

    /// 44 — `serve_uds` removes a stale socket file and binds fresh; server becomes reachable.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_stale_socket_file_replaced() {
        let path = unique_socket_path();
        // Create a stale socket file by binding then immediately dropping a listener.
        {
            let _stale = tokio::net::UnixListener::bind(&path).unwrap();
        } // listener drops here; file stays on disk
        assert!(path.exists(), "stale socket file must still exist");
        // serve_uds must unlink the stale file and bind fresh.
        let state = WarmState::new(graph_with(&[(1, "hello")]));
        let st = Arc::clone(&state);
        let p = path.clone();
        tokio::spawn(async move {
            let _ = serve_uds(&p, st).await;
        });
        // Use wait_for_server: it probes via actual connection, so we know the NEW listener
        // is active, not merely that the stale file still exists on disk.
        wait_for_server(&path).await;
        let resp =
            request_response(&path, r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(v["result"]["protocolVersion"].is_string(), "{resp}");
        let _ = std::fs::remove_file(&path);
    }

    /// 45 — a line exceeding [`MAX_LINE_BYTES`] is rejected with a JSON-RPC error; connection drops.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_max_line_rejected() {
        let (path, _st) = start_test_server(graph_with(&[(1, "x")])).await;
        let stream = UnixStream::connect(&path).await.unwrap();
        let (mut read_half, mut write_half) = stream.into_split();
        // Spawn writer: sends MAX_LINE_BYTES + 1 bytes without a newline.
        // The server drops the connection after the cap; BrokenPipe is expected on the write.
        let oversized = vec![b'z'; MAX_LINE_BYTES + 1];
        tokio::spawn(async move {
            let _ = write_half.write_all(&oversized).await;
        });
        // Read the server's response (parse-error) or EOF.
        let mut resp = String::new();
        let n = tokio::io::BufReader::new(&mut read_half)
            .read_line(&mut resp)
            .await
            .unwrap_or(0);
        if n > 0 {
            let v: Value = serde_json::from_str(resp.trim()).unwrap_or(Value::Null);
            assert!(v.get("error").is_some(), "expected JSON error; got: {resp}");
            assert_eq!(v["error"]["code"], -32700, "{resp}");
        }
        // n == 0 (EOF without response) is also acceptable per spec.
        let _ = std::fs::remove_file(&path);
    }

    /// 46 — a 64 KiB request (well under [`MAX_LINE_BYTES`]) is accepted and produces a valid response.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_large_valid_request_accepted() {
        let (path, _st) = start_test_server(graph_with(&[(1, "LargeReq")])).await;
        let pad = "P".repeat(64_000);
        let req = format!(r#"{{"jsonrpc":"2.0","id":55,"method":"initialize","_pad":"{pad}"}}"#);
        assert!(
            req.len() < MAX_LINE_BYTES,
            "test request must be under the cap"
        );
        let resp_str = request_response(&path, &req).await;
        let v: Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(v["id"], 55, "{resp_str}");
        assert!(v["result"]["protocolVersion"].is_string());
        let _ = std::fs::remove_file(&path);
    }

    /// 47 — unknown method returns method-not-found error (-32601) over the socket.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_unknown_method_error_response() {
        let (path, _st) = start_test_server(graph_with(&[(1, "x")])).await;
        let resp = request_response(
            &path,
            r#"{"jsonrpc":"2.0","id":1,"method":"no_such_method"}"#,
        )
        .await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["error"]["code"], -32601, "{resp}");
        let _ = std::fs::remove_file(&path);
    }

    /// 48 — string `id` is echoed through the socket transport.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_string_id_echoed() {
        let (path, _st) = start_test_server(graph_with(&[(1, "x")])).await;
        let resp = request_response(
            &path,
            r#"{"jsonrpc":"2.0","id":"my-request-id","method":"tools/list"}"#,
        )
        .await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["id"], "my-request-id", "{resp}");
        let _ = std::fs::remove_file(&path);
    }

    /// 49 — a request with `id: null` is not treated as a notification; a response is sent.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_null_id_request_gets_response() {
        let (path, _st) = start_test_server(graph_with(&[(1, "x")])).await;
        let resp = request_response(
            &path,
            r#"{"jsonrpc":"2.0","id":null,"method":"initialize"}"#,
        )
        .await;
        assert!(!resp.is_empty(), "null-id request must get a response");
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(v["result"]["protocolVersion"].is_string(), "{resp}");
        let _ = std::fs::remove_file(&path);
    }

    /// 50 — resources/read schema round-trip returns a text body with the schema version.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_resources_read_schema() {
        let (path, _st) = start_test_server(graph_with(&[(1, "x")])).await;
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"resources/read","params":{"uri":"habitat-graph://schema"}}"#;
        let resp = request_response(&path, req).await;
        assert!(resp.contains("schema_version"), "{resp}");
        let _ = std::fs::remove_file(&path);
    }

    /// 51 — abrupt client disconnect does not kill the server; next connection succeeds.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_client_disconnect_server_survives() {
        let (path, _st) = start_test_server(graph_with(&[(1, "x")])).await;
        {
            let mut conn = UnixStream::connect(&path).await.unwrap();
            conn.write_all(b"{\"jsonrpc\":\"2.0\"").await.unwrap();
            // Abrupt drop — partial frame.
        }
        tokio::task::yield_now().await;
        let resp =
            request_response(&path, r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(v["result"]["protocolVersion"].is_string(), "{resp}");
        let _ = std::fs::remove_file(&path);
    }

    /// 52 — 20 sequential requests across separate connections all succeed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_many_sequential_requests() {
        let (path, _st) = start_test_server(graph_with(&[(1, "x"), (2, "y"), (3, "z")])).await;
        for i in 0_u32..20 {
            let method = if i % 3 == 0 {
                "initialize"
            } else {
                "tools/list"
            };
            let req = format!(r#"{{"jsonrpc":"2.0","id":{i},"method":"{method}"}}"#);
            let resp = request_response(&path, &req).await;
            let v: Value = serde_json::from_str(&resp).unwrap();
            assert_eq!(v["id"], i64::from(i), "i={i}: {resp}");
            assert!(v.get("result").is_some(), "i={i}: {resp}");
        }
        let _ = std::fs::remove_file(&path);
    }
}
