> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · [[EVIDENCE]] · **plan:** [[14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901]] · **V3 corpus:** [[16_ARCHITECTURE_SCHEMATICS_V3_S1008901]] · [[17_CROSS_MODEL_AGENTIC_CONTRACT_S1008901]] · [[18_DIAGNOSTICS_OBSERVABILITY_V3_S1008901]] · [[19_PLAN_SCHEMATIC_MAP_S1008901]]
> **grounds on:** [[01_GRAPHIFY_EXEMPLAR_MAP]] · [[06_PARITY_INTEL]] · [[08_GRAPHIFY_PARITY_PLAN_S1008796]] · [[Commands & graphify Comparison (S1008796)]]

# habitat-graph — Feature Assimilation Matrix (full graphify parity, S1008901)

The single authoritative map: **every graphify feature** (set fetched live 2026-06-28) → its
habitat-graph home, current status, the V3 phase that lands it, its gate, and its golden/oracle.
Legend: ✅ at parity · 🟡 partial/scaffolded · ❌ missing · ➕ net-new (no graphify equivalent).
Status verified against `EVIDENCE.md` + source ([VBE] where the gate has run).

---

## A. Core pipeline — ✅ DONE (at parity)

| graphify | hg home | status | proof |
|---|---|---|---|
| `extract` pipeline | `cli::extract` → detect→extract→build→analyze→export | ✅ | `EVIDENCE.md:57-58` (self-hosting: 139 nodes) |
| `query` (label search) | `serve::find_by_label` + `cli::query` + MCP `graph_query` | ✅ | `EVIDENCE.md:59` |
| `path` (shortest) | `serve::shortest_path` + `cli::path` + MCP `graph_path` | ✅ | `EVIDENCE.md:59` |
| `graph.json` (node-link) | `export::to_node_link` / `serve::from_node_link` | ✅ (97/96% content parity) | `EVIDENCE.md:44,54` |
| `GRAPH_REPORT.md` | `export::render_report` | ✅ | `EVIDENCE.md:53` |
| `obsidian/` export | `export::render_vault` (`[[wikilinks]]`+MOC) | ✅ | `EVIDENCE.md:53` |
| `graph.html` viewer | `export::render_html` (self-contained) | ✅ | git `0be2e09` |
| Leiden clustering | `analyze::detect_communities` (leiden-rs, **seeded → deterministic**) | ✅ | `EVIDENCE.md:49` |
| cache (blake3) | `cache::{CacheKey,memoize,partition}` (⚠ **orphaned/unwired** — PC C-4) | 🟡 built, unwired | `EVIDENCE.md:37`; doc 09:38 |
| Confidence tags | `core::Confidence` (EXTRACTED/INFERRED/AMBIGUOUS, byte-compat enum) | ✅ | `EVIDENCE.md:35` |
| `--mcp` server | `serve::mcp::handle_jsonrpc` (pure JSON-RPC 2.0, no rmcp) | ✅ live-proven | `EVIDENCE.md:76-78` |
| HTTP serve | `daemon` (axum `/health`/`/query`/`/path`) | ✅ live-proven | `EVIDENCE.md:60` |

## B. Grammars — Phase PA (D-A: full set, un-trimmed)

| # | grammar | ext | hg home | status | gate | golden |
|---|---|---|---|---|---|---|
| G0 | rust | `.rs` | `extract::ast::rust` | ✅ | done | self + httpx-class |
| G0 | python | `.py` | `extract::ast::python` | ✅ | done | **httpx (the oracle)** |
| G1 | typescript | `.ts .tsx` | `extract::ast::ts` | ❌ | node≥80%/struct≥70% | **source + pin oracle** |
| G2 | javascript | `.js .jsx .mjs` | `extract::ast::js` | ❌ | ″ | source + pin |
| G3 | go | `.go` | `extract::ast::go` | ❌ | ″ | source + pin |
| G4 | java | `.java` | `extract::ast::java` | ❌ | ″ | source + pin |
| G5 | c | `.c .h` | `extract::ast::c` | ❌ | ″ | source + pin |
| G6 | cpp | `.cc .cpp .hpp` | `extract::ast::cpp` | ❌ | ″ | source + pin |
| G7 | ruby | `.rb` | `extract::ast::ruby` | ❌ | ″ | source + pin |
| G8 | c# | `.cs` | `extract::ast::csharp` | ❌ | ″ | source + pin |
| G9 | kotlin | `.kt .kts` | `extract::ast::kotlin` | ❌ | ″ | source + pin |
| G10 | scala | `.scala` | `extract::ast::scala` | ❌ | ″ | source + pin |
| G11 | php | `.php` | `extract::ast::php` | ❌ | ″ | source + pin |
| G12 | docs | `.md .txt .rst` | `extract::ast::text` (heading/section nodes) | ❌ | node parity + feeds retrieval | small md golden |

> **Prerequisite G-ABI** ([[14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901]] §6): the `tree-sitter` core ABI matrix MUST close before any of G1–G11 is committed — see [[16_ARCHITECTURE_SCHEMATICS_V3_S1008901]] §3.
> **Per-grammar seam (§3.0):** each grammar ships its **completeness envelope** (`edge_classes_emitted/omitted`, `node_coverage_pct`) so no agent decides on a silently-partial grammar — see [[18_DIAGNOSTICS_OBSERVABILITY_V3_S1008901]] §2.

### B.1 The per-grammar golden pipeline (C-1 — the larger half, doc 09:27)
Each grammar's real cost is **not** the extractor; it is the oracle pipeline:

| step | artifact | note |
|---|---|---|
| 1 source a representative corpus | `worked/<lang>/raw/` | small, license-clean, idiomatic |
| 2 run the **pinned** graphify oracle | `worked/<lang>/graph.json` | record graphify version (P1-G12) |
| 3 commit a version-pinned golden | `fixtures/goldens/<lang>/` | the oracle of record |
| 4 extractor + ≥50 tests | `extract::ast::<lang>` | meaningful, not fitted |
| 5 parity gate vs the golden | `fixtures/tests/parity_<lang>.rs` | node≥80% + structural≥70% |

## C. Exporters — Phase PB

| # | graphify | hg home | status | gate | DoD note |
|---|---|---|---|---|---|
| X1 | `--svg` | `export::svg` (laid-out → SVG) | ❌ | valid XML vs golden | human-artifact (F13 split; §1B obligation post-flip) |
| X2 | `--graphml` | `export::graphml` (Gephi/yEd) | ❌ | valid XML | human/tooling |
| X3 | `--neo4j` → cypher | `export::cypher` (`CREATE`/`MERGE`) | ❌ | parses | tooling |
| X4 | `--wiki` (+`index.md`) | `export::wiki` (folds w/ obsidian) | ❌ | links resolve | dual-value (agent-crawlable) |

## D. Lifecycle & integration — Phase PC

| # | graphify | hg home | status | gate | correction bound in |
|---|---|---|---|---|---|
| C2 | `--update` (re-extract→merge) | wire `cache::partition`+`build::merge` into `cli::extract --update` | 🟡 cache unwired | idempotent + parity-stable + **analyze cost measured** | C-4 (stop calling file-cache "incremental", doc 09:38) |
| C3 | `--watch` | `serve::watch` (`notify`, debounce) | ❌ | debounce; **watch×hook single-writer lock** | P1-G10 (doc 09:58) |
| C4 | `hook install` | `serve::hooks` (`git2` + R4 merge driver) | ❌ | round-trip; conflict-free merge | — |
| C5 | `install` (MCP register) | `cli::install` writes Claude Code MCP config | ❌ | idempotent + **backup/merge/read-back** | P1-G8 (write-safety, doc 09:52) |
| C6 | `add <URL>` | `source::ingest` + `cli::add` | 🟡 ingest scaffolded | size/timeout caps + **SSRF/private-IP block** | C-2 (doc 09:46) |
| C1 | `--mode deep` (inferred edges) | `extract` heuristic `uses`/`references` @ INFERRED/AMBIGUOUS | ❌ | **gated OUT of analyze→sphere→arc** | T1/F12 (doc 09:104) |
| — | schema-versioning | `core::schema` `schema_version` field | ❌ | `--update` detects taxonomy mismatch | P1-G12 (doc 09:64) |

## E. Analytics — Phase PD

| # | graphify | hg home | status | gate | A2 re-sink |
|---|---|---|---|---|---|
| A1 | god-nodes (highest-degree) | `export::report` section (degree_centrality exists) | 🟡 computed | top-degree section | → arc-graph bridge analysis (F8) |
| A2 | surprising connections | `analyze::patterns` (cross-community high-weight) | ❌ | deterministic ranking | → severed-ear (F8) |
| A3 | suggested questions | `analyze::questions` (heuristic; LLM-aug) | ❌ | deterministic (note: LLM-aug ≠ deterministic, P1-G11) | human-only / OSS |
| A4 | token benchmark | `export::benchmark` (`tiktoken-rs`) | ❌ | token math tested | **→ transformed into AGT-2 serve-side budgeter (A0)** |

## F. Semantic / multimodal — Phase PE (local-first, TIERWRIGHT-routed)

| # | graphify | hg home | status | gate | policy |
|---|---|---|---|---|---|
| S1 | semantic extract (docs/prose) | `extract::semantic` → `Backend` | 🟡 backend crate built, unwired | off-by-default; `is_local()` audit; mock-tested | Noop default; opt-in TIERWRIGHT/Ollama |
| S2 | pdf papers | feature `pdf`: `pdf-extract`/`lopdf` | ❌ | **DoS caps** (size+timeout+mem; decompression-bomb) | C-2 ship-gate |
| S3 | images / vision | feature `vision`: route via **TIERWRIGHT `:8201`** | ❌ | image-prompt-injection defense; Luke policy (7.3) | never raw external call |
| S4 | `explain "<concept>"` | `cli::explain` → subgraph → Backend | ❌ | summary; subgraph assembled | human-facing; OSS-leaning |

## G. Multi-backend / install breadth — doc-01 §D (decision 7.5)

| graphify | hg disposition | restore? |
|---|---|---|
| Anthropic/OpenAI/Ollama backends | ✅ `Backend` trait (Noop/Ollama/OpenAI) + TIERWRIGHT | done |
| Bedrock/Gemini/Azure/DeepSeek/Kimi backends | DEFER | 7.5 (OSS-breadth; say the word) |
| Office docs / video / 15+ doc-translations | DROP (v1) | 7.5 |
| `install --platform [cursor\|gemini\|copilot\|…]` | TRIM → `claude` | 7.5 (factory is Claude Code) |

## H. ➕ Net-new — agentic + cross-model (no graphify equivalent) — Block 2

| # | feature | hg home | phase | design ref |
|---|---|---|---|---|
| AGT-1 | MCP graph-as-resource + templates | `serve::mcp` | A0 | [[12_LLM_FRIENDLY_API_AND_UDS_S1008796]] §B1 |
| AGT-2 | token-budgeted serve `max_tokens=K` | `serve::query`+`mcp` | A0 | 12 §C |
| AGT-3 | warm retrieval index (kill O(n)) | `serve`/`cache` | A1 | 12 §A; doc 09 C-4 |
| AGT-4 | arc-graph continuous severed-ear telemetry | `habitat::arc_graph` | A2 | [[18_DIAGNOSTICS_OBSERVABILITY_V3_S1008901]] §3 |
| AGT-5 | content-addressed stable node IDs | `core::ids`+`build::merge` | A1 | 12 §B5 |
| AGT-6 | completeness envelope + confidence filter | `core::schema`+`serve::mcp` | A0 (seam in PA) | 13 §2 |
| AGT-7 | UDS warm-daemon + atomic reload | `daemon` (UDS) | A1 | 12 §D |
| XM-1…7 | cross-model contract (Claude 4.8+ / GPT-5.5+) | `serve`+`cli::meta`+bridge | A3 | [[17_CROSS_MODEL_AGENTIC_CONTRACT_S1008901]] |

## I. Already AHEAD of graphify (keep — do not regress)

rayon per-file parallelism · determinism R4 (sorted + seeded Leiden, merge-driver-ready) ·
parity-regression gate · blake3 **parity-transparent** cache (hit == recompute) · `forbid(unsafe)` ·
core security guard (Trojan-Source/bidi escapes, path-confine, secret-screen, SSRF planned) ·
pure-JSON-RPC MCP (no heavy rmcp dep) · **L8 factory integration** (POVM · injection.db · PV2 ·
cc-pipe · TIERWRIGHT · arc-graph) · the cross-model agentic contract (doc 17).

---
*Feature assimilation matrix S1008901 (2026-06-28) · Claude @ cortex. Full roadmap + gates → [[14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901]]. Status [VBE] from `EVIDENCE.md`; ❌/🟡 are the live backlog.*
