> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · [[EVIDENCE]] · **plan:** [[14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901]] · **V3 corpus:** [[15_FEATURE_ASSIMILATION_MATRIX_S1008901]] · [[17_CROSS_MODEL_AGENTIC_CONTRACT_S1008901]] · [[18_DIAGNOSTICS_OBSERVABILITY_V3_S1008901]] · [[19_PLAN_SCHEMATIC_MAP_S1008901]]
> **extends:** [[11_ARCHITECTURE_SCHEMATICS_S1008796]] (built-state DAG/C4/dataflow) — this doc adds the **target (full-parity + agentic) architecture**.

# habitat-graph — Architecture Schematics v3 (target state, S1008901)

Visual + tabular maps of the **full-parity + agentic target**. Open in Obsidian/GitHub to render
Mermaid. Design only — `forbid(unsafe)` and gate discipline hold for every box drawn here.
Built-state diagrams (current DAG, current dataflow, current sequence) live in [[11_ARCHITECTURE_SCHEMATICS_S1008796]]; this doc draws **what the plan adds**.

---

## 1. Target crate/module DAG (full parity + agentic, L0→L8)

New vs built marked `+`. Acyclic + inward-only preserved (Layer D, the BMC discipline).

```mermaid
flowchart TB
  subgraph L0L1["L0/L1 · vocabulary + guard"]
    core["habitat-graph-core<br/>schema(+schema_version,+ids content-addr) · confidence · span · error(+kind)<br/>guard: display_safe · confine_to · validate_url(+SSRF) · screen_for_secrets"]
  end
  subgraph L15["L1.5 · incremental substrate"]
    cache["habitat-graph-cache<br/>blake3 CAS · memo · partition (＋WIRED in PC) · evict"]
  end
  subgraph L2["L2 · acquisition"]
    source["habitat-graph-source<br/>detect(+11 exts,+docs) · ingest(+SSRF caps) · manifest"]
  end
  subgraph L3["L3 · extraction + semantic"]
    extract["habitat-graph-extract<br/>registry · ast/rust · ast/python<br/>＋ast/{ts,js,go,java,c,cpp,ruby,csharp,kotlin,scala,php,text}<br/>＋mode_deep (gated OUT of analyze) · ＋semantic"]
    backend["habitat-graph-backend<br/>Backend · noop · ollama · openai · ＋tierwright · transport"]
  end
  subgraph L4L6["L4–L6 · graph + analysis + render"]
    build["habitat-graph-build<br/>assemble · dedup · merge(+stable-id remap)"]
    analyze["habitat-graph-analyze<br/>cluster(Leiden) · centrality · ＋patterns · ＋questions"]
    export["habitat-graph-export<br/>json · report · obsidian · html<br/>＋svg · ＋graphml · ＋cypher · ＋wiki · ＋benchmark"]
  end
  subgraph L7["L7 · transport + lifecycle"]
    daemon["habitat-graph-daemon<br/>axum HTTP · ＋ArcSwap reload · ＋UDS listener · ＋staleness"]
    serve["habitat-graph-serve<br/>load · query(+warm index,+budget,+confidence) · mcp(+resources,+typed-errors) · ＋watch · ＋hooks"]
    cli["habitat-graph-cli<br/>extract/query/path/serve/mcp/self-test/doctor<br/>＋install · ＋add · ＋explain · ＋--svg/--graphml/--neo4j/--wiki · ＋doctor --json/--schemas"]
  end
  subgraph L8["L8 · factory wiring (--features live*, split 7.6)"]
    habitat["habitat-graph-habitat<br/>bridge · memory · obsidian_protocol · pv2_spheres · orchestrator_pipe · tierwright<br/>arc_graph(＋continuous telemetry,＋delta-push)"]
  end
  fixtures["habitat-graph-fixtures<br/>＋13 per-grammar goldens · parity harness · ＋ABI matrix"]

  source-->core
  extract-->core
  extract-->backend
  backend-->core
  build-->core
  build-->extract
  analyze-->core
  analyze-->build
  export-->core
  export-->build
  export-->analyze
  cache-->core
  source-->cache
  daemon-->core
  daemon-->serve
  serve-->core
  serve-->cache
  cli-->source
  cli-->extract
  cli-->build
  cli-->analyze
  cli-->export
  cli-->serve
  cli-->daemon
  cli-->backend
  habitat-->core
  habitat-->analyze
  habitat-->serve
  habitat-->backend
  fixtures-->core
```
*Invariant: `core` depends on nothing; everything depends inward; `habitat` (L8) is additive. New surfaces (`+`) extend existing crates — only `text`/grammar modules and the UDS listener are net-new files. `cache` becomes load-bearing (PC C-4) instead of orphaned.*

## 2. Grammar extractor fan-out (PA) — the registry as a dispatch table

```mermaid
flowchart LR
  files[("files (detect: ext dispatch)")] --> reg["extract::registry<br/>extension → Extractor (rayon ∥)"]
  reg -->|.rs| g0["ast::rust ✅"]
  reg -->|.py| g0b["ast::python ✅"]
  reg -->|.ts/.tsx| g1["ast::ts ＋"]
  reg -->|.js/.jsx| g2["ast::js ＋"]
  reg -->|.go| g3["ast::go ＋"]
  reg -->|.java| g4["ast::java ＋"]
  reg -->|.c/.h| g5["ast::c ＋"]
  reg -->|.cc/.cpp| g6["ast::cpp ＋"]
  reg -->|.rb| g7["ast::ruby ＋"]
  reg -->|.cs| g8["ast::csharp ＋"]
  reg -->|.kt| g9["ast::kotlin ＋"]
  reg -->|.scala| g10["ast::scala ＋"]
  reg -->|.php| g11["ast::php ＋"]
  reg -->|.md/.txt/.rst| g12["ast::text ＋"]
  g0 & g1 & g2 & g3 & g4 & g5 --> raw["RawNode/RawEdge + per-grammar completeness envelope"]
  g6 & g7 & g8 & g9 & g10 & g11 & g12 --> raw
  raw --> build["build::assemble → Graph"]
```
*Each fiber is one file (`ast::<lang>.rs`) → collision-free parallel build (one `forge-rust-coder-v4` per lang). Each emits its completeness envelope (§3.0 seam). The `Extractor` trait + the registry already exist (`extract/src/registry.rs:40-58`) — adding a grammar is additive dispatch, not a redesign.*

## 3. The tree-sitter ABI matrix (G-ABI — PA's hard prerequisite)

> **✅ RESOLVED (S1008901) → [[abi-matrix-s1008901]].** Outcome: single core **tree-sitter 0.25.x (ABI 15) + tree-sitter-language 0.1**; the apparent per-grammar core conflict was a `kind=dev` artifact (test-only, not built downstream) — the real `kind=normal` dep `tree-sitter-language ^0.1` is shared by all modern grammars, so they unify. **0 pins-older · 0 vendor · 0 defer** (Kotlin → maintained `tree-sitter-kotlin-ng`). The schematic below is the general decision procedure; the live result is the matrix.

```mermaid
flowchart TB
  subgraph problem["the constraint"]
    abi13["grammars needing core ABI 13"]
    abi14["grammars needing core ABI 14"]
    abi15["grammars needing core ABI 15"]
    cargo["cargo: ONE tree-sitter core in the tree"]
  end
  abi13 & abi14 & abi15 --> cargo
  cargo --> decide{"pick max-satisfying core ABI"}
  decide -->|aligned| inset["commit grammar @ compatible release"]
  decide -->|conflict| resolve["resolve: pin older release · vendor · or DEFER that ONE grammar to A4 (logged, no silent drop)"]
  inset & resolve --> out["abi-matrix-s1008901.md + Cargo/deny pin set"]
```

| axis | what it records |
|---|---|
| grammar crate | `tree-sitter-<lang>` candidate + version |
| required core ABI | 13 / 14 / 15 |
| last release @ chosen core | the pin |
| resolution | aligned · pinned-older · vendored · DEFERRED(reason) |

> `cargo-deny` checks licenses/advisories, **NOT** ABI. G-ABI is a separate, explicit gate that blocks PA sizing ([[14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901]] §6).

## 4. Exporter matrix + split-rebuild (PB / F13)

```mermaid
flowchart LR
  g[("Graph")] --> crit["AGENT-CRITICAL path\n(graph.json + warm index + arc-delta)"]
  g --> human["HUMAN-ARTIFACT path\n(svg · graphml · wiki · html)"]
  g --> tool["TOOLING path\n(cypher → neo4j · graphml → Gephi/yEd)"]
  crit -->|never blocked by| human
  crit --> agents["agent fleet (latency-critical)"]
  human --> eyes["human / OSS reader"]
  tool --> dbs["neo4j · Gephi"]
```
*Rule (F13): a redrawn SVG must never widen the agent's staleness window. The human-artifact exporters run off the critical path; `--svg/--graphml/--wiki` are opt-in flags on `extract`.*

Every branch in this matrix crosses the same public-projection boundary before serialization:
screened strings become deterministic redaction markers while node IDs, edges, counts, and
communities remain intact. Format-specific escaping happens after that projection. Previously
adopted optional artifacts and generated wiki/vault files are refreshed only through conservative
ownership manifests; unowned files are preserved.

## 5. Lifecycle state machine (PC) — update · watch · hook (with the single-writer lock)

```mermaid
stateDiagram-v2
  [*] --> Idle
  Idle --> Building: extract / extract --update
  Idle --> Watching: serve --watch
  Watching --> Debounce: fs event (notify)
  Debounce --> Building: debounce window elapsed
  Idle --> Building: git post-commit hook
  Building --> Lock: acquire single-writer lock (watch×hook race guard, P1-G10)
  Lock --> Partition: cache::partition (changed vs cached)
  Partition --> Reextract: extract changed only
  Reextract --> Merge: build::merge (stable-id remap, AGT-5)
  Merge --> Recluster: analyze (⚠ global Leiden — cost measured, C-4)
  Recluster --> Swap: ArcSwap::store(new graph)
  Swap --> Delta: arc_graph::diff_arcs → SeveredEarReport (A2)
  Delta --> Idle: release lock; push delta (arming-gated)
```
*`--update` is honest: file-cache is extraction-level; Leiden re-runs globally → the plan measures/discloses the analyze cost rather than calling file-cache "incremental" (C-4, doc 09:38).*

The lifecycle has two persistence planes: redacted public artifacts and a complete raw incremental
graph. The raw plane is owner-only, keyed by canonical output plus Git context, and protected by an
output lock, bounded snapshots, and lineage-checked add/update journals. Public artifacts are written
atomically and a no-source-change update still refreshes them, preventing old export policy from
surviving indefinitely.

## 6. Semantic / multimodal pipeline (PE) — local-first, TIERWRIGHT-routed

```mermaid
flowchart LR
  src["source / .pdf / image"] --> route{"semantic path?"}
  route -->|default| noop["NoopBackend\n(local-first: code never hits a model)"]
  route -->|opt-in| guardin["DoS caps (C-2):\nsize · timeout · mem bound"]
  guardin --> tw["TIERWRIGHT :8201\n(model router — habitat rule)"]
  tw --> parse["backend::parse_semantic\n→ sanitize_label (Trojan-Source guard)"]
  parse --> raw["RawNode/RawEdge @ INFERRED/AMBIGUOUS"]
  raw --> conf{"Confidence filter\nbefore analyze (T1/F12)"}
  conf -->|EXTRACTED only| analyze["analyze (clean topology)"]
  conf -->|INFERRED/AMBIGUOUS| humanexport["human export only (not analyze/sphere/arc)"]
```
*Every label/relation from a model funnels through `core::guard::sanitize_label` (the storage-keeps/render-escapes Trojan-Source invariant, `EVIDENCE.md:66`). Inferred edges never reach community detection (they corrupt topology + sphere mapping).*

## 7. Cross-model serving topology (A0/A1/A3) — Claude 4.8+ · GPT-5.5+

```mermaid
flowchart TB
  subgraph models["agentic LLM clients (model-agnostic)"]
    cc["Claude Code 4.8+\n(native MCP)"]
    gpt["GPT-5.5+\n(function-calling)"]
    arch["Architect :8144 orchestrator"]
  end
  subgraph organ["habitat-graph organ :8202"]
    mcpstdio["MCP stdio (built)"]
    bridge["MCP↔function-call bridge (XM, A3)"]
    uds["UDS warm-daemon hg.sock 0o600 (A1)"]
    http["HTTP serve (built)"]
    handler["handle_jsonrpc (one codec, all transports)"]
    res["resources + templates (A0)"]
    budg["token-budget K (A0)"]
    idx["warm index (A1)"]
    swap["ArcSwap<Graph> + generation + staleness (A1)"]
  end
  cc --> mcpstdio --> handler
  gpt --> bridge --> handler
  gpt -.direct.-> http --> handler
  arch --> uds --> handler
  handler --> res & budg
  budg --> idx --> swap
```
*One handler, four transports — `serve::mcp::handle_jsonrpc` is a pure `&str → String`, so UDS + bridge + HTTP all reuse it verbatim (12 §D2). Capability negotiation + the XM test matrix → [[17_CROSS_MODEL_AGENTIC_CONTRACT_S1008901]].*

## 8. Phase dependency graph (build order)

```mermaid
flowchart LR
  GABI["G-ABI matrix"] --> PA
  seam["doctor --json + envelope seam (§3.0)"] --> PA
  PA["PA grammars"] --> PB["PB exporters"]
  PA --> PD["PD analytics"]
  PB --> PC["PC lifecycle"]
  PC --> PE["PE semantic/MM"]
  PD --> A0["A0 front door"]
  PE --> A0
  A0 --> A1["A1 warm/ids/UDS"]
  A1 --> A2["A2 arc-telemetry"]
  A2 --> A3["A3 cross-model"]
  A3 -.flip-gated.-> A4["A4 OSS public"]
```

## 9. Target crate → responsibility → new public surface (Δ vs doc 11 §8)

| Crate | New in v3 | Public Δ |
|---|---|---|
| core | schema_version · content-addr `NodeId` · `GraphError::kind` taxonomy · SSRF in `validate_url` | `schema_version()`, `NodeId::content_addressed(...)`, `kind()` |
| cache | **wired** (was orphaned) | `partition` consumed by `serve`/`cli --update` |
| source | +11 grammar exts + docs; ingest SSRF caps | `detect` ext set; `ingest` caps |
| extract | +12 `ast::*` modules; `semantic`; `mode_deep` | `registered_extractors()` grows; `extract_semantic(...)` |
| analyze | +`patterns` +`questions` | `surprising_connections`, `suggested_questions` |
| export | +svg/graphml/cypher/wiki/benchmark | `to_svg/to_graphml/to_cypher/render_wiki/token_benchmark` |
| serve | warm index · budget · confidence filter · resources · typed errors · watch · hooks | `query_budgeted`, `resources_read`, MCP `error.data` |
| daemon | ArcSwap reload · UDS listener · staleness | `run_uds(...)`, `reload(...)`, `generation()` |
| cli | install · add · explain · export flags · `doctor --json/--schemas` | new subcommands/flags |
| habitat | arc_graph continuous telemetry + delta-push | `arc_telemetry_stream`, `push_delta(...)` (live*) |
| fixtures | 13 per-grammar goldens + ABI matrix harness | `parity_<lang>` suites |

---
*Architecture schematics v3 (target) S1008901 (2026-06-28) · Claude @ cortex. Built-state → [[11_ARCHITECTURE_SCHEMATICS_S1008796]]; API/UDS contracts → [[12_LLM_FRIENDLY_API_AND_UDS_S1008796]]; diagnostics → [[18_DIAGNOSTICS_OBSERVABILITY_V3_S1008901]].*
