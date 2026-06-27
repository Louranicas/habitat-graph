> Back to: [[CLAUDE.md]] · [[CLAUDE.local.md]] · [[habitat-graph/README]] · spine: [[00_DEPLOYMENT_PLAN]] · framework: [[DEPLOYMENT_FRAMEWORK]] · [[MODULE_STRUCTURE_PLAN]]

# Graphify → habitat-graph: Module & Dependency Exemplar Map (S1008796)

The exemplar repo `safishamsi/graphify` is 19 Python modules over a linear pipeline. This document
maps **every module** and **every dependency** to its Rust home, so the refactor is a guided
translation, not a blank-page rewrite. Source of truth: graphify `ARCHITECTURE.md` + `pyproject.toml`
(fetched S1008796).

---

## A. Pipeline (exemplar) → Rust layers

Graphify's chain — verbatim from its ARCHITECTURE.md:

```
detect() → extract() → build_graph() → cluster() → analyze() → report() → export()
        (+ ingest, cache, security, validate, serve, watch, benchmark, hooks, manifest, wiki)
```

Each stage is "plain dicts + NetworkX, no shared state" → in Rust, typed structs + `petgraph`,
stages as pure functions over `&Graph` / `-> Extraction`. The statelessness is a gift: it maps
cleanly onto Rust's ownership model and onto `rayon` data-parallelism over files.

---

## B. Module-by-module map (all 19)

| # | graphify module | Responsibility (exemplar) | habitat-graph home | Rust notes |
|---|---|---|---|---|
| 1 | `detect.py` | collect files by recognized extension | L2 `source::detect` | `walkdir` + extension dispatch table; `ignore` crate to honor `.gitignore` |
| 2 | `extract.py` | file → `{nodes,edges}` dicts | L3 `extract::registry` + `ast::*` | trait `Extractor`; one impl per language family; `rayon` parallel map over files |
| 3 | `build.py` | aggregate into NetworkX graph | L4 `build::assemble` | `petgraph::Graph`; stable `NodeId` interning via `IndexMap` |
| 4 | `cluster.py` | community membership attrs | L5 `analyze::cluster` | **`network_partitions`** (graspologic-native Leiden — already Rust) or `fa-leiden-cd` |
| 5 | `analyze.py` | high-degree nodes, anomalies, open-Qs | L5 `analyze::{centrality,patterns,questions}` | `petgraph` algos (degree, betweenness); rule structs for patterns |
| 6 | `report.py` | `GRAPH_REPORT.md` | L6 `output::report` | `minijinja`/`askama` templates; deterministic ordering |
| 7 | `export.py` | json/html/svg/obsidian/graphml/cypher | L6 `output::export::*` | `serde_json`; `quick-xml` (graphml/svg); string-gen (cypher); markdown-gen (obsidian/wiki) |
| 8 | `ingest.py` | fetch remote → local, size/timeout caps | L2 `source::ingest` | `reqwest` (blocking or tokio) with `Content-Length` + timeout guards |
| 9 | `cache.py` | partition cached vs uncached | L2 `source::cache` | content-hash (`blake3`) keyed cache dir; `mtime`+hash invalidation |
| 10 | `security.py` | URL/path validate, label sanitize | L1 `guard::*` | the security boundary — port the rules exactly (256-char cap, control-char strip, path-confine) |
| 11 | `validate.py` | schema correctness of extraction dicts | L1 `guard::schema_check` | `serde` + typed structs make most of this *compile-time*; runtime checks for LLM output |
| 12 | `serve.py` | MCP stdio interface | L7 `iface::mcp` | **`rmcp`** official SDK; `#[tool]` macros for query/path/subgraph |
| 13 | `watch.py` | dir monitor → flag file | L7 `iface::watch` | `notify` crate (cross-platform inotify/FSEvents) |
| 14 | `benchmark.py` | token usage: full vs subgraph | L6 `output::benchmark` | token estimate via `tiktoken-rs`; compares corpus vs subgraph extraction |
| 15 | `hooks.py` | git post-commit auto-rebuild | L7 `iface::hooks` | `git2`; install hook scripts; the merge driver for conflict-free `graph.json` |
| 16 | `ingest`/`manifest.py` | build manifest of processed inputs | L2 `source::manifest` | manifest struct (paths, hashes, timestamps) → `serde_json` sidecar |
| 17 | `wiki.py` | markdown-wiki export | L6 `output::export::wiki` | markdown generation; folds with obsidian exporter |
| 18 | `__main__.py` | CLI entry | L7 `iface::cli` | `clap` derive; subcommands mirror graphify (`extract`/`query`/`path`/`install`/`hook`/`prs`/`export`) |
| 19 | `__init__.py` | package surface | L0 `core` / lib root | `lib.rs` re-exports; the public API surface |

**Confidence enum** (graphify's `EXTRACTED|INFERRED|AMBIGUOUS` on every edge) → L0
`core::Confidence` — a first-class Rust enum, threaded through the schema. This is the single most
important type to get right; it's the trust signal the whole graph rests on.

---

## C. Dependency → crate map (from `pyproject.toml`)

| graphify dep | Role | Rust crate | Maturity / note |
|---|---|---|---|
| `networkx` | graph data structure + algos | **`petgraph`** | de-facto standard; covers degree/centrality/shortest-path (graphify `path`) |
| `graspologic` | Leiden community detection | **`network_partitions`** (graspologic-native) / `fa-leiden-cd` | **already Rust** — graspologic calls it via PyO3; we call it directly |
| `tree-sitter` | AST parsing core | **`tree-sitter`** | Rust is tree-sitter's native binding home |
| `tree-sitter-<lang>` ×15 | grammars (py, js, ts, go, rust, java, c, cpp, ruby, c#, kotlin, scala, php…) | `tree-sitter-<lang>` crates | 1:1 crate per grammar; all 15 wired exist + more (covers the advertised 36) |
| `mcp` | MCP server | **`rmcp`** (official) | `#[tool]` macros, tokio, stdio+SSE — see Sources |
| `neo4j` | Neo4j/FalkorDB export | `neo4rs` (live) or none (emit Cypher strings) | v1: emit `.cypher` text; live driver = stretch |
| `pypdf` + `html2text` | PDF/HTML text extraction | `pdf-extract`/`lopdf` + `html2md` | feature `pdf`; needed only for semantic (LLM) path |
| `watchdog` | filesystem watch | **`notify`** | feature `watch` |
| *(LLM backends: Anthropic/OpenAI/Gemini/Ollama/Bedrock)* | semantic extraction | `reqwest` + `serde` (thin clients) or `async-anthropic`/`async-openai`/`ollama-rs` | **habitat: route via TIERWRIGHT** (`02_HABITAT_INTEGRATION.md`) |
| *(graph.html viewer)* | interactive viz | `minijinja` template + bundled JS (`cytoscape`/`vis-network`/`sigma`) | emit self-contained HTML; viewer JS is vendored, not generated |
| *(CLI)* | arg parsing | `clap` | derive API |
| *(serve HTTP)* | health + MCP SSE | `axum` + `tokio` | also serves `/health` for `cc-health` |
| *(JSON)* | graph.json | `serde` + `serde_json` | schema-compat with graphify (R2) |
| *(parallelism)* | speed over 500K LOC | `rayon` | NEW capability — Python GIL had none |
| *(hashing/cache)* | content hash | `blake3` | fast cache keys + deterministic ids |
| *(tracing)* | observability | `tracing` (+ `tracing-opentelemetry`) | habitat diagnostics convention |

**Net new-risk surface:** only the **LLM backend layer** (thin HTTP clients) and the **HTML
viewer** (vendor a JS lib) are genuine from-scratch work. Everything else is a typed translation of
existing logic against a mature crate. The Leiden + tree-sitter + MCP triad — normally the scary
part of any such port — is already Rust.

---

## D. What we deliberately drop or defer from the exemplar (v1)

| Exemplar feature | Decision | Why |
|---|---|---|
| AWS Bedrock / DeepSeek / Kimi / Azure backends | DEFER | habitat routes through TIERWRIGHT; multi-cloud is OSS-audience surface, not factory need |
| Office documents (Google Workspace extra) | DEFER (feature `office`) | rare in a Rust corpus; heavy deps |
| Video transcription | DROP (v1) | out of scope for a code knowledge graph; reconsider if media corpus appears |
| 15+ doc translations | DROP | docs translation is an OSS-community concern |
| `graphify install --platform [cursor|gemini|copilot|…]` | TRIM to `claude` | the factory is Claude Code; other-platform installers are dead weight here |

These are reversible: the core/habitat crate split keeps the OSS-facing surface available if we
ever publish upstream (see spine §8.6).

---

## E. Sizing (rough, for phasing — not a commitment)

| Layer | Modules | Exemplar LOC (Py, est.) | Rust LOC (est., +tests) |
|---|---|---|---|
| L0 core | 5 | ~200 | ~600 + 250 tests |
| L1 guard | 5 | `security.py`+`validate.py` ~400 | ~700 + 250 |
| L2 source | 4 | detect/ingest/cache/manifest ~500 | ~900 + 200 |
| L3 extract | ~10 | extract.py + grammars ~1500 | ~2500 + 500 |
| L4 build | 3 | build.py ~300 | ~600 + 150 |
| L5 analyze | 4 | cluster/analyze ~600 | ~1100 + 200 |
| L6 output | 8 | report/export/wiki/benchmark ~1200 | ~2200 + 400 |
| L7 iface | 5 | serve/watch/hooks/main ~900 | ~1800 + 350 |
| L8 habitat | 7 | (new) | ~2000 + 350 |
| **Total** | **~51 modules** | **~6000 Py** | **~12.4K + 3050 tests** |

≥50 tests/module × ~35 substantive modules ⇒ the ~1750-test floor the habitat gate expects (cf.
factory-map 1934, WFE 2163).

*Sources: [graspologic-native](https://github.com/graspologic-org/graspologic-native) ·
[rmcp / rust-sdk](https://github.com/modelcontextprotocol/rust-sdk) ·
[petgraph](https://crates.io/crates/petgraph) · graphify ARCHITECTURE.md + pyproject.toml.*
