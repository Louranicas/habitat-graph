> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · [[EVIDENCE]] · plan: [[10_PLAN_V2_AGENT_FIRST_S1008796]] · API: [[12_LLM_FRIENDLY_API_AND_UDS_S1008796]] · **v3 target arch:** [[16_ARCHITECTURE_SCHEMATICS_V3_S1008901]] · **live plan:** [[14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901]]

# habitat-graph — Architecture Schematics & Mappings (S1008796)

Visual + tabular maps of the built system and the proposed agent-first surfaces. Open in Obsidian /
GitHub to render the Mermaid. Design only — no code lands until "start coding".

## 1. Crate dependency DAG (L0→L8, inward + acyclic)

```mermaid
flowchart TB
  subgraph L0L1["L0/L1 · vocabulary + guard"]
    core["habitat-graph-core<br/>schema · ids · span · confidence · error · guard"]
  end
  subgraph L15["L1.5 · incremental substrate"]
    cache["habitat-graph-cache<br/>blake3 CAS · memo · evict (⚠ unwired)"]
  end
  subgraph L2["L2 · acquisition"]
    source["habitat-graph-source<br/>detect · ingest · manifest"]
  end
  subgraph L3["L3 · extraction + semantic"]
    extract["habitat-graph-extract<br/>registry · ast/rust · ast/python"]
    backend["habitat-graph-backend<br/>Backend trait · noop · ollama · openai · transport"]
  end
  subgraph L4L6["L4–L6 · graph + analysis + render"]
    build["habitat-graph-build<br/>assemble · dedup · merge"]
    analyze["habitat-graph-analyze<br/>cluster(Leiden) · centrality"]
    export["habitat-graph-export<br/>json · report · obsidian · html"]
  end
  subgraph L7["L7 · transport"]
    daemon["habitat-graph-daemon<br/>axum Router over Arc&lt;Graph&gt;"]
    serve["habitat-graph-serve<br/>load · query · mcp(JSON-RPC)"]
    cli["habitat-graph-cli<br/>clap: extract/query/path/serve/mcp/self-test/doctor"]
  end
  subgraph L8["L8 · factory wiring (additive, --features live)"]
    habitat["habitat-graph-habitat<br/>bridge · memory · obsidian_protocol · pv2_spheres · orchestrator_pipe · tierwright · arc_graph"]
  end
  fixtures["habitat-graph-fixtures<br/>goldens + parity harness (dev)"]

  backend --> core
  source --> core
  extract --> core --> backend
  build --> core
  build --> extract
  analyze --> core
  analyze --> build
  export --> core
  export --> build
  export --> analyze
  daemon --> core
  daemon --> serve
  serve --> core
  cli --> core
  cli --> source
  cli --> extract
  cli --> build
  cli --> analyze
  cli --> export
  cli --> serve
  cli --> daemon
  habitat --> core
  habitat --> analyze
  habitat --> serve
  habitat --> backend
  fixtures --> core
```
*Rule: `core` depends on nothing; feature crates depend inward; `habitat` (L8) is additive — every
other crate compiles + runs without it. ⚠ `cache` is currently orphaned (no crate depends on it) — the
plan-v2 P1 wires it for real incremental `--update`.*

## 2. C4 — Context (the organ in the factory)

```mermaid
flowchart LR
  subgraph FACTORY["ULTRAPLATE agent factory (the consumer)"]
    ARCH["Architect :8144<br/>orchestrator"]
    FLEET["Claude / GPT fleet<br/>(MCP clients)"]
    PV2["PV2 Kuramoto :8132"]
    ORAC["ORAC RALPH :8133"]
    POVM["POVM :8125 / injection.db :8140"]
    TW["TIERWRIGHT :8201"]
  end
  HG(["habitat-graph<br/>code-knowledge-graph ORGAN :8202"])
  SRC[("source trees<br/>the 20 services' code")]

  SRC -->|extract| HG
  FLEET <-->|MCP query/path/health + resources| HG
  ARCH <-->|cc-pipe map.scope ACK/NACK| HG
  HG -->|severed-ear telemetry + graph-delta| PV2
  HG -->|causal_chain + pathways| POVM
  HG -->|RALPH consumes graph signals| ORAC
  HG -->|semantic extract routed| TW
```

## 3. C4 — Container (transports + artifacts)

```mermaid
flowchart TB
  agent["agent / LLM client"]
  human["human (OSS, flip-gated)"]
  subgraph organ["habitat-graph organ"]
    clibin["CLI binary `habitat-graph`"]
    mcpsrv["MCP server (stdio JSON-RPC)"]
    httpsrv["HTTP server (axum :addr)"]
    uds["⟂ proposed: UDS warm-daemon<br/>$XDG_RUNTIME_DIR/habitat-graph/hg.sock"]
    eng["engine: detect→extract→build→analyze→export"]
    g[("Arc&lt;Graph&gt; (warm)")]
  end
  art[("artifacts: graph.json · graph.html · GRAPH_REPORT.md · vault/")]

  agent -->|MCP tools/resources| mcpsrv
  agent -.proposed.-> uds
  human -->|browse| art
  clibin --> eng --> g
  eng --> art
  mcpsrv --> g
  httpsrv --> g
  uds -.proposed.-> g
```

## 4. End-to-end extract dataflow

```mermaid
flowchart LR
  dir([dir]) --> detect["source::detect<br/>walk + ext dispatch + .gitignore"]
  detect --> extract["extract::registry (rayon ∥)<br/>ast::rust / ast::python → RawNode/RawEdge"]
  extract --> assemble["build::assemble<br/>IndexMap intern → NodeId · dedup · merge"]
  assemble --> sorted["Graph::sorted (R4 canonical)"]
  sorted --> leiden["analyze::detect_communities<br/>leiden-rs seeded (LEIDEN_SEED)"]
  leiden --> g[("Graph")]
  g --> json["export::to_node_link → graph.json"]
  g --> report["export::render_report → GRAPH_REPORT.md"]
  g --> html["export::render_html → graph.html (self-contained)"]
  g --> vault["export::render_vault → Obsidian vault (--vault)"]
```

## 5. Sequence — an agent calls the MCP organ

```mermaid
sequenceDiagram
  participant A as Agent (Claude/GPT)
  participant M as serve::mcp (stdio)
  participant G as Arc&lt;Graph&gt;
  A->>M: {jsonrpc,id,method:"initialize"}
  M-->>A: {result:{protocolVersion,serverInfo,capabilities}}
  A->>M: {method:"notifications/initialized"}
  Note over M: notification → no reply
  A->>M: {id,method:"tools/list"}
  M-->>A: {result:{tools:[graph_query,graph_path,graph_health]}}
  A->>M: {id,method:"tools/call",params:{name:"graph_query",arguments:{query:"Confidence"}}}
  M->>G: find_by_label("Confidence")  (⚠ O(n) today)
  G-->>M: matches
  M-->>A: {result:{content:[{type:text,text:"4 node(s)…"}],isError:false}}
```

## 6. Transport map — CLI vs HTTP vs MCP-stdio vs proposed UDS

```mermaid
flowchart TB
  subgraph now["BUILT"]
    one["CLI one-shot<br/>extract/query/path"]
    http["HTTP serve :addr<br/>/health /query /path"]
    mcp["MCP stdio<br/>1 client, JSON-RPC"]
  end
  subgraph proposed["PROPOSED (plan-v2 P1)"]
    udsd["UDS warm-daemon<br/>N agents · 1 warm DB · sub-ms<br/>resources + token-budget + atomic-reload"]
  end
  one -. cold, per-call rebuild .-> http
  http -. port contention, 1 graph .-> mcp
  mcp -. 1 client, no resources .-> udsd
```

| Transport | Concurrency | Warmth | Latency | Best for |
|---|---|---|---|---|
| CLI one-shot | 1 | cold (rebuild each call) | high | scripts, hooks, CI |
| HTTP serve | many | warm (one graph) | ms + port | dashboards, cross-host |
| MCP stdio | 1 per process | warm | low | a single Claude Code client |
| **UDS warm-daemon (proposed)** | **many agents** | **warm, shared, salsa** | **sub-ms** | **the factory fleet** — see `12_…` |

## 7. Proposed agent-first architecture (plan-v2 P0–P2)

```mermaid
flowchart TB
  subgraph agents["agent fleet (many concurrent)"]
    a1["Architect"]; a2["Claude inst"]; a3["GPT inst"]
  end
  subgraph d["habitat-graph warm daemon (proposed)"]
    sock["UDS hg.sock (0o600)"]
    mux["session mux"]
    res["MCP resources + resource-templates"]
    budg["token-budgeted subgraph selector (K)"]
    idx["warm label/trigram index (kills O(n))"]
    swap["ArcSwap&lt;Graph&gt; atomic reload + staleness"]
    arcg["arc_graph diff → SeveredEarReport"]
  end
  a1 & a2 & a3 --> sock --> mux --> res & budg
  budg --> idx --> swap
  swap --> arcg
  arcg -->|push on rebuild| PV2["PV2 spheres"]
  arcg -->|delta| POVM["POVM / injection.db"]
  arcg -->|severed-ear| GAUGE["arc-coherence-gauge"]
```

## 8. Crate → responsibility → public API (mapping)

| Crate | Layer | Responsibility | Public surface (key) |
|---|---|---|---|
| core | L0/L1 | vocabulary + security guard | `Graph/Node/Edge/Community/Confidence/Span/NodeId`, `GraphError/Result`, `display_safe/sanitize_label/confine_to/validate_url/screen_for_secrets` |
| cache | L1.5 | blake3 CAS · memo · evict (⚠ unwired) | `CacheKey`, `MemStore`, `memoize`, `partition` |
| source | L2 | detect · ingest · manifest | `detect(dir,&exts)`, `ingest`, `manifest` |
| extract | L3 | tree-sitter extraction | `registered_extractors()`, `extract_files(&files)` |
| backend | L3 | semantic LLM backends | `Backend` trait, `NoopBackend/OllamaBackend/OpenAiCompatBackend`, `HttpTransport/StaticTransport`, `parse_semantic/build_prompt` |
| build | L4 | assemble · dedup · merge | `assemble(Vec<Extraction>) -> Graph`, `dedup`, `merge` |
| analyze | L5 | Leiden + centrality | `detect_communities(&Graph)`, `degree_centrality(&Graph)` |
| export | L6 | json · report · obsidian · html | `to_node_link`, `render_report`, `render_vault`, `render_html` |
| daemon | L7 | axum HTTP host | `build_router(Arc<Graph>)`, `run_server(Arc<Graph>, addr)` |
| serve | L7 | load · query · **mcp** | `from_node_link`, `find_by_label`, `shortest_path`, `handle_jsonrpc(&Graph,&str)` |
| cli | L7 | clap product binary | commands: extract/query/path/serve/mcp/self-test/doctor |
| habitat | L8 | factory wiring (`--features live`) | `bridge/memory/obsidian_protocol/pv2_spheres/orchestrator_pipe/tierwright/arc_graph` |
| fixtures | dev | parity harness | `NormalizedGraph`, `from_golden/from_core`, `classify → ParityReport` |

---
*Architecture schematics S1008796 · Claude @ cortex. Detailed API + UDS contracts → `12_…`; diagnostics → `13_…`.*
