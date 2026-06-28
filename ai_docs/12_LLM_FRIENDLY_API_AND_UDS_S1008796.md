> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · [[EVIDENCE]] · plan: [[10_PLAN_V2_AGENT_FIRST_S1008796]] · schematics: [[11_ARCHITECTURE_SCHEMATICS_S1008796]] · **v3 cross-model contract:** [[17_CROSS_MODEL_AGENTIC_CONTRACT_S1008901]] · **live plan:** [[14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901]]

# habitat-graph — LLM-Friendly API + UNIX-Socket Design (S1008796)

How to make habitat-graph effortless, effective, and impactful for any frontier model (GPT-5.5,
Claude 4.8) to drive — the complete API map, the MCP-as-resource design, token-budgeted serving,
machine-readable contracts, and the UNIX-socket warm-daemon. **Design only — implementation waits for
"start coding".** Source of truth for the BUILT surface: `cli.rs`, `serve::mcp` (`mcp.rs`),
`daemon::{server,handlers}`, `core::schema`.

---

## A. Complete API mapping (the surface as built)

### A1. CLI (`habitat-graph <command>` — clap; stdout=output, stderr=diagnostics)

| Command | Args / flags | Does | stdout | Exit codes |
|---|---|---|---|---|
| `extract <dir>` | `--out <dir>`=graphify-out · `--vault <dir>` | detect→extract→build→analyze→export | `graph: N nodes, E edges, C communities -> <out>` + writes `graph.json`/`graph.html`/`GRAPH_REPORT.md` (+vault) | 0 ok · 4 error |
| `query <substr>` | `--graph <json>` | case-insensitive label search (sorted, capped 50, true total reported) | `N match(es)…` + `id [label] file:line` | 0 · 4 |
| `path <from> <to>` | `--graph <json>` | shortest undirected path (BFS, deterministic) | `path (H hop(s)): a -> b -> c` or `no path` | 0 · 4 |
| `serve` | `--graph <json>` · `--addr`=127.0.0.1:7878 | HTTP service until interrupted | bind line | 0 · 4 load · 2 bad addr · 1 runtime |
| `mcp` | `--graph <json>` | MCP JSON-RPC 2.0 over stdio | one JSON response per request line | 0 · 4 |
| `self-test` | — | in-memory corpus, no I/O | `self-test ok: 2 nodes` | 0 |
| `doctor` | — | engine + wiring diagnostics | `engine: source+extract+build+analyze+export (backend: ast-only)` | 0 |

### A2. HTTP (`serve` / `daemon`, axum over `Arc<Graph>`)

| Method · path | Query | Response (JSON) |
|---|---|---|
| `GET /health` | — | `{"nodes":139,"edges":48,"communities":97,"version":"0.0.0"}` |
| `GET /query` | `?q=<substr>` | `[{"id":N,"label":"…","source_file":"…","line":N}, …]` |
| `GET /path` | `?from=<label>&to=<label>` | `{"found":true,"hops":2,"path":["a","b","c"]}` |

### A3. MCP (JSON-RPC 2.0 over stdio — `serve::mcp::handle_jsonrpc`)

Protocol `2024-11-05`, serverInfo `habitat-graph`. Methods: `initialize` · `tools/list` · `tools/call`
· notifications (no-id → no response). Tools today:

| Tool | inputSchema | result `content[0].text` |
|---|---|---|
| `graph_query` | `{query: string}` (required) | `"N node(s) match \"q\":\n  - label [file:line]\n…"` (capped 50, true total) |
| `graph_path` | `{from, to: string}` (required) | `"path (H hop(s)): a -> b -> c"` / `"no path…"` |
| `graph_health` | `{}` | `"nodes=N edges=E communities=C schema=…"` |

JSON-RPC errors: `-32700` parse · `-32601` method-not-found · `-32602` invalid-params. Every result
carries `jsonrpc:"2.0"` + echoed `id`; `result` XOR `error`.

### A4. graph.json node-link schema (graphify-compatible)

```json
{ "directed": true, "multigraph": false, "graph": {},
  "nodes": [{"id": 0, "label": "detect", "source_file": "…/detect.rs", "source_location": "L24", "community": 380}],
  "links": [{"source": 6, "target": 0, "relation": "calls", "confidence": "INFERRED", "weight": 0.8}] }
```
`confidence ∈ {EXTRACTED, INFERRED, AMBIGUOUS}`. Determinism (R4): nodes by id, links by
`(source,target,relation)`.

---

## B. Make it LLM-native — MCP-first, resources, self-describe

A frontier model consumes a tool best when (1) it can *read data as a resource* without a planning
hop, (2) the tool schema *tells it exactly what it gets back*, (3) outputs are *typed + deterministic*
(cacheable), and (4) it knows *what it is NOT seeing*. The built organ has #3 partly; #1/#2/#4 are the
plan-v2 P0/P1 work.

### B1. MCP graph-as-RESOURCE (AGT-1) — pull subgraph context with no tool call
Add `resources/list` + `resources/read` + resource-templates to `handle_jsonrpc`:
- `habitat-graph://graph` → the whole node-link graph (or a budgeted slice)
- `habitat-graph://node/{label}` → the node + its 1-hop neighborhood (typed edges)
- `habitat-graph://community/{id}` → a community subgraph
- `habitat-graph://report` → `GRAPH_REPORT.md`
- `habitat-graph://schema` → the JSON Schema (§F) — the model self-teaches the format

An agent does `resources/read habitat-graph://node/SchedulerLoop` and gets structured context inline —
no "which tool?" planning step. Resources are the MCP idiom for *data*; tools for *actions*.

### B2. Capability manifest + self-describe (`doctor --json`)
`doctor` today prints prose. Add `doctor --json` returning a machine manifest so an agent (or the
orchestrator's tool-router) discovers capabilities without docs:
```json
{ "name":"habitat-graph", "version":"…", "schema_version":"habitat-graph.graph.v0",
  "capabilities":{"tools":["graph_query","graph_path","graph_health"],
    "resources":["graph","node","community","report","schema"],
    "transports":["cli","http","mcp-stdio","uds"],
    "languages":["rust","python"], "features":{"live":false,"net":false}},
  "graph":{"nodes":N,"edges":E,"communities":C,"generation":G,"stale":false},
  "completeness":{"edge_classes_emitted":["calls","method","contains","inherits","imports_from"],
    "edge_classes_omitted":["uses"], "node_coverage_pct":97, "structural_pct":96} }
```

### B3. Completeness envelope + confidence filter (AGT-6) — the trust contract
The single most important LLM-friendliness feature: **never let a model make a load-bearing decision on
a silently-incomplete graph.** Every query/health/resource response carries:
```json
{ "result": …,
  "envelope": {"confidence":{"extracted":N,"inferred":N,"ambiguous":N},
               "coverage_pct":97, "edge_classes_omitted":["uses"], "stale":false, "generation":G} }
```
And `graph_query` accepts `{query, confidence?: "EXTRACTED"|"INFERRED"|"AMBIGUOUS", max_tokens?: int}`
so an agent making a wiring decision can demand `confidence:"EXTRACTED"` only. This turns the existing
`Confidence` enum from a human eyeball-tag into an agent decision filter.

### B4. Typed errors, not prose
Replace stringy errors at the agent boundary with a stable typed contract (JSON-RPC `error.data`):
```json
{"code":-32004,"message":"graph not loaded","data":{"kind":"graph_unavailable","retryable":false}}
```
`kind ∈ {graph_unavailable, stale_graph, budget_exceeded, not_found, invalid_scope}` — the model
branches on `kind`, never parses prose. (Maps from `core::GraphError::kind()`, which already exists.)

### B5. Determinism = agent-side cacheability
R4 canonical ordering + the seeded Leiden (`LEIDEN_SEED`) mean *same source → byte-identical graph →
identical query answers.* Expose a `generation` (content hash) on every response so an agent caches by
`(query, generation)` and never re-asks an unchanged graph. This is a *correctness* feature for the
fleet, not just speed.

---

## C. Token-budgeted serving (AGT-2) — fit the model's window

The defining agent constraint is a finite context window. Dumping a 1600-node graph is hostile. Design:

- `graph_query(query, max_tokens=K)` and a resource `habitat-graph://node/{label}?budget=K` return a
  **greedily-packed, relevance-ordered subgraph that fits K tokens**.
- Selection: seed = matched/scope nodes → expand by budget-bounded BFS, ordering neighbors by
  `degree × proximity × confidence`; stop when the token estimate hits K; **never drop the seed**.
- Token estimate: cheap char/heuristic by default; `tiktoken-rs` behind a feature for exactness.
- Output declares what it truncated: `{nodes_included:n, nodes_total:T, budget:K, truncated:true}` —
  the model knows the view is partial (ties to B3).

This *transforms* doc-08's "token benchmark" (a stat for a human) into a serve-side **actuator** the
agent steers with one parameter — the highest-leverage LLM-friendliness change in the plan.

---

## D. UNIX-socket warm-daemon (AGT-7) — the factory transport

### D1. Why UDS for agent↔organ (vs HTTP / stdio)
The factory is **single-host, many concurrent agents, hot-loop querying.** That is exactly UDS's
sweet spot:
- **Sub-ms latency, no TCP/HTTP overhead** — the orchestrator decomposes a mission into many
  `map.scope` calls; per-call HTTP parsing/port latency is the wall.
- **Many agents, one WARM shared graph** — stdio MCP is one-client-per-process (each reloads the
  graph cold); HTTP needs a port + contends. A UDS daemon holds one `ArcSwap<Graph>` and multiplexes
  all agents.
- **No port contention / firewall surface** — the 8082–8201 range is crowded; a socket needs none.
- **Filesystem permissions = access control** — `0o600` like morphd; only the factory user.
- **Precedent in the habitat:** morphd already speaks UDS (`$XDG_RUNTIME_DIR/morph-ir-engine/morphd.sock`,
  `0o600`) — habitat-graph follows the same shape, so the fleet's UDS tooling reuses.

### D2. Socket + framing + protocol
```text
path     : $XDG_RUNTIME_DIR/habitat-graph/hg.sock   (mode 0o600, dir 0o700)
framing  : newline-delimited JSON-RPC 2.0  (identical envelope to the stdio MCP — one codec, two transports)
           (length-prefixed framing optional for large resource bodies)
methods  : the MCP surface verbatim — initialize · tools/list · tools/call · resources/list · resources/read
           + habitat verbs: graph.health · graph.reload · graph.generation
session  : each connection = one session over the shared warm DB; concurrent reads lock-free (ArcSwap load)
```
Because the frame is *the same JSON-RPC the stdio MCP already speaks* (`serve::mcp::handle_jsonrpc` is a
pure `&str -> String`), the UDS server is a thin `tokio::net::UnixListener` accept-loop that feeds each
line to the **existing** handler — minimal new surface, maximal reuse.

### D3. UDS daemon architecture (proposed)
```mermaid
sequenceDiagram
  participant A as Agent
  participant S as hg.sock (UnixListener)
  participant H as handle_jsonrpc (reused)
  participant G as ArcSwap&lt;Graph&gt;
  A->>S: connect (0o600)
  A->>S: {id,method:"resources/read",params:{uri:"habitat-graph://node/Foo?budget=2000"}}\n
  S->>H: line
  H->>G: load() (lock-free)
  G-->>H: &Graph
  H-->>S: {result:{contents:[…budgeted subgraph…]},envelope:{…}}\n
  S-->>A: response line
  Note over G: a rebuild does ArcSwap::store(new); in-flight reads see the old snapshot — never torn
```

### D4. Transport comparison

| | stdio MCP (built) | HTTP serve (built) | **UDS warm-daemon (proposed)** |
|---|---|---|---|
| concurrent agents | 1/process | many | **many, multiplexed** |
| graph warmth | warm per-process | warm | **warm, shared** |
| latency | low | ms + port | **sub-ms** |
| access control | process | bind addr | **fs perms 0o600** |
| port needed | no | yes | **no** |
| reuses `handle_jsonrpc` | yes | partial | **yes (verbatim)** |
| best for | one Claude Code | dashboards/cross-host | **the agent fleet** |

CLI/stdio/HTTP all stay — the CLI one-shot must never *require* the daemon (Design Rule 6). UDS is the
*acceleration* path for the fleet.

---

## E. How GPT-5.5 & Claude 4.8 specifically drive it

- **Claude 4.8 / Claude Code:** mounts habitat-graph as an **MCP server** (the native path). `install`
  (plan-v2 P0) writes the MCP config; the model gets `graph_query/path/health` tools **+ resources**
  auto-discovered via `tools/list` + `resources/list`. Tool descriptions are written *for a model*
  ("Search graph nodes whose label contains a substring; returns up to 50, total reported"). Structured
  outputs + typed errors (B4) let the model branch without prose-parsing.
- **GPT-5.5 (or any function-calling model):** consume the same MCP server through an MCP↔function-call
  bridge, **or** call the HTTP/UDS surface directly. The `doctor --json` capability manifest (B2) lets a
  tool-router auto-generate function specs. The JSON Schemas (§F) drop straight into a `tools` array.
- **Both:** rely on (1) the **completeness envelope** to avoid hallucinating absent edges, (2)
  **token-budgeted** resources to fit their window, (3) **determinism + `generation`** for caching, (4)
  **typed `kind` errors** for control flow. An example agent session:
  ```text
  initialize → resources/read habitat-graph://schema  (learn the format)
  tools/call graph_query {query:"SchedulerLoop", confidence:"EXTRACTED", max_tokens:1500}
    → budgeted subgraph + envelope{coverage_pct, omitted:["uses"], generation:G}
  tools/call graph_path {from:"SchedulerLoop", to:"EventBus"}  → 3-hop path
  (cache answers by (query, G); re-query only when generation changes)
  ```

---

## F. Machine-readable contracts (ship these alongside the binary)

The organ should *carry its own schemas* so any model self-teaches. Emit on `doctor --schemas` and as
the `habitat-graph://schema` resource.

**graph.json (JSON Schema, abridged):**
```json
{ "$schema":"https://json-schema.org/draft/2020-12/schema", "title":"habitat-graph.graph.v0",
  "type":"object","required":["nodes","links"],
  "properties":{
   "nodes":{"type":"array","items":{"type":"object","required":["id","label"],
     "properties":{"id":{"type":"integer"},"label":{"type":"string"},
       "source_file":{"type":"string"},"source_location":{"type":"string","pattern":"^L[0-9]+$"},
       "community":{"type":"integer"}}}},
   "links":{"type":"array","items":{"type":"object","required":["source","target","relation"],
     "properties":{"source":{"type":"integer"},"target":{"type":"integer"},
       "relation":{"type":"string"},"weight":{"type":"number"},
       "confidence":{"enum":["EXTRACTED","INFERRED","AMBIGUOUS"]}}}}}}
```

**MCP tool (e.g. graph_query) — the schema the model reads:**
```json
{ "name":"graph_query",
  "description":"Search code-graph nodes whose label contains a substring (case-insensitive). Returns the most relevant matches packed to fit max_tokens, with a completeness envelope.",
  "inputSchema":{"type":"object","required":["query"],
    "properties":{"query":{"type":"string"},
      "confidence":{"enum":["EXTRACTED","INFERRED","AMBIGUOUS"]},
      "max_tokens":{"type":"integer","minimum":128}}} }
```

---

## G. Summary of the LLM-friendliness work (all design, gated on "start coding")

| Item | Surface | Plan-v2 phase |
|---|---|---|
| MCP resources + templates (B1) | `serve::mcp` | P0 / AGT-1 |
| `doctor --json` capability manifest (B2) | `cli::meta` | P0 |
| Completeness envelope + confidence filter (B3) | `serve::mcp` + `core::schema` | P1 / AGT-6 |
| Typed `kind` errors (B4) | `core::error` → MCP `error.data` | P0-P1 |
| `generation` content-hash for caching (B5) | `core::schema` + serve | P1 |
| Token-budgeted serve (C) | `serve::query` + `serve::mcp` | P0 / AGT-2 |
| UDS warm-daemon (D) | new `daemon` UDS path, reuses `handle_jsonrpc` | P1 / AGT-7 |
| JSON Schemas + `--schemas`/resource (F) | `cli` + `serve::mcp` | P0 |

---
*LLM-friendly API + UDS design S1008796 · Claude @ cortex. Built surface from `cli.rs`/`mcp.rs`/`schema.rs`;
proposals are plan-v2 (`10_…`) P0–P1. No code until "start coding".*
