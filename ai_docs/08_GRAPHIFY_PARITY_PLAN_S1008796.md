> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · [[EVIDENCE]] · exemplar: [[01_GRAPHIFY_EXEMPLAR_MAP]] · vault: `habitat-graph.vault/Commands & graphify Comparison (S1008796)`

# graphify Feature-Parity Plan (comprehensive) — S1008796

**Goal:** close every gap between habitat-graph and `safishamsi/graphify` (feature set fetched live
2026-06-28) while keeping the habitat gold standard (forbid-unsafe · no-unwrap · ≥50 meaningful
tests/substantive-module · parity-gated · deterministic R4 · local-first).

## A. Authoritative gap matrix

Legend: ✅ at parity · 🟡 partial / scaffolding exists · ❌ missing.

| # | graphify feature | hg state | gap | Rust plan (crate · module) | effort | phase |
|---|---|---|---|---|---|---|
| L1 | Code grammars: `ts js go java c cpp rb cs kt scala php` | rs · py | ❌ 11 langs | `extract::ast::{ts,js,go,java,c,cpp,ruby,csharp,kotlin,scala,php}` + `tree-sitter-*` crates; registry already extension-dispatches | HIGH | **PA** |
| L2 | Docs as nodes (`.md .txt .rst`) | ❌ | text nodes | `source::detect` add exts + `extract::ast::text` (heading/section nodes) | LOW | PA |
| X1 | `--svg` (vis export) | ❌ | svg | `export::svg` — render the laid-out graph to SVG (`quick-xml`/string-gen) | MED | **PB** |
| X2 | `--graphml` (Gephi/yEd) | ❌ | graphml | `export::graphml` (`quick-xml`) | MED | PB |
| X3 | `--neo4j` → `cypher.txt` | ❌ | cypher | `export::cypher` — `CREATE`/`MERGE` string-gen | LOW | PB |
| X4 | `--wiki` (agent-crawlable wiki + `index.md`) | ❌ | wiki | `export::wiki` — per-node article + index (folds with obsidian) | MED | PB |
| C1 | `--mode deep` (aggressive inferred edges) | ❌ | deep mode | `extract` config: emit heuristic `uses`/`references` edges at `INFERRED`/`AMBIGUOUS` confidence | MED | **PC** |
| C2 | `--update` (re-extract changed → merge) | 🟡 `cache` crate unwired | incremental | wire `cache::partition` + `build::merge` into `extract --update` (blake3 SHA already there) | MED | PC |
| C3 | `--watch` (auto-sync on change) | ❌ | watcher | `serve::watch` — `notify` crate, debounce, re-run pipeline | MED | PC |
| C4 | `hook install` (post-commit rebuild) | ❌ | git hook | `serve::hooks` — `git2` install + conflict-free `graph.json` merge driver (R4 already deterministic) | MED | PC |
| C5 | `install` (initial setup / MCP register) | ❌ | installer | `cli::install` — write the Claude Code MCP config so the `mcp` organ auto-mounts | LOW | PC |
| C6 | `add <URL>` (fetch paper/tweet → merge) | 🟡 `ingest` scaffolding | remote ingest | wire `source::ingest` (size/timeout caps) + `cli::add` → extract → merge | MED | PC |
| A1 | God nodes (highest-degree) | 🟡 `degree_centrality` exists | surface | `export::report` — "God nodes" top-degree section | LOW | **PD** |
| A2 | Surprising connections (cross-domain, ranked) | ❌ | patterns | `analyze::patterns` — cross-community high-weight edge ranking | MED | PD |
| A3 | Suggested questions (4–5) | ❌ | questions | `analyze::questions` — heuristic from hubs/bridges; LLM-augmented via Backend | MED | PD |
| A4 | Token benchmark (per-query efficiency) | ❌ | benchmark | `export::benchmark` — `tiktoken-rs`, full-corpus vs subgraph token estimate | MED | PD |
| S1 | Semantic path wired into extract (docs/prose) | 🟡 `backend` crate exists, **unwired** | wire | `extract::semantic` calls `Backend` (Noop default; TIERWRIGHT/Ollama opt-in) | MED | **PE** |
| S2 | PDF papers (`.pdf`) | ❌ | pdf | feature `pdf`: `pdf-extract`/`lopdf` → text → `extract::semantic` | MED | PE |
| S3 | Images / multimodal (Claude vision) | ❌ | vision | feature `vision`: route image bytes via **TIERWRIGHT** (factory model router); never a raw external call | HIGH | PE |
| S4 | `explain "<concept>"` (LLM node explain) | ❌ | explain | `cli::explain` → subgraph context → `Backend`/TIERWRIGHT summary | MED | PE |
| ✅ | `query` · `path` · `--mcp` · `graph.html` · `obsidian/` · `GRAPH_REPORT.md` · `graph.json` · cache(blake3) · Confidence tags · Leiden | — | at parity | — | — | DONE |

**Already AHEAD of graphify** (keep): rayon parallelism · determinism (R4) · parity-regression gate ·
blake3 parity-transparent cache · `forbid(unsafe)` · the core security guard (Trojan-Source/bidi) ·
the L8 factory integration (POVM/PV2/cc-pipe/TIERWRIGHT/arc-graph) · 1242 gate-green tests.

## B. Phased roadmap (value-ordered, each phase gate-green + parity-tested before the next)

### PA — Language breadth (biggest visible gap)  ★ highest user value
The registry dispatches by extension; each language = one `Extractor` impl + its `tree-sitter-<lang>`
crate + a per-grammar golden. **Fan-out shape:** a dynamic Workflow, one `forge-rust-coder-v4` fiber per
language (collision-free, distinct files) + `forge-tester` parity judge outside the loop. Add `.md/.txt/.rst`
text nodes (L2) in the same wave. *Gate:* per-grammar node/edge parity vs a small golden; `forbid(unsafe)`
holds (tree-sitter safe API). *Sizing:* ~11 extractors × (~120 LOC + ~50 tests).

### PB — Exporters (svg · graphml · cypher · wiki)
Four pure `Graph -> String` transforms in `export::{svg,graphml,cypher,wiki}` + CLI flags
(`--svg --graphml --neo4j --wiki`) wired into `extract`. *Gate:* each exporter vs a golden; svg/graphml
validate as XML; cypher parses; wiki `index.md` links resolve. Folds the wiki exporter with the obsidian one.

### PC — Lifecycle & integration (`--update` · `--watch` · `hook install` · `install` · `add` · `--mode deep`)
Wire the existing `cache` + `build::merge` for incremental `--update`; `notify`-based `--watch`; `git2`
hook + the deterministic `graph.json` merge driver; `install` writing the Claude Code MCP config; `add <URL>`
via `source::ingest`; `--mode deep` heuristic edges at lower confidence. *Gate:* update-merge is idempotent
+ parity-stable; watch debounce; hook round-trip; MCP config validates.

### PD — Analytics (god nodes · surprising connections · suggested questions · token benchmark)
Surface god nodes in the report (centrality already computed); `analyze::patterns` ranks cross-community
high-weight edges; `analyze::questions` generates 4–5 from hubs/bridges (heuristic, LLM-augmentable);
`export::benchmark` via `tiktoken-rs`. *Gate:* deterministic outputs; benchmark token math tested.

### PE — Semantic / multimodal (LLM path, local-first, TIERWRIGHT-routed)
Wire `extract::semantic` to call the existing `Backend` (Noop default — **no behavior change** unless a
backend is configured); `.pdf` via feature `pdf`; images via feature `vision` **routed through TIERWRIGHT**
(never a raw external call on source — habitat rule); `explain` builds a subgraph context and asks the
Backend. *Gate:* semantic path off by default; `is_local()` audit; mock-tested without network.

## C. Decisions needed (Luke @ 0.A) before PE / some of PC
- **Multimodal/vision policy:** route ALL image/pdf semantic calls via TIERWRIGHT `:8201` (recommended,
  habitat rule) vs allow direct Ollama/OpenAI? (S3/S2)
- **Deliberately-dropped in v1** (`01_GRAPHIFY_EXEMPLAR_MAP §D`) — restore for "ALL features" or keep dropped?
  Bedrock/Gemini/Azure backends · Office docs · video transcription · doc translations · multi-platform
  install (cursor/copilot/gemini — habitat is Claude-only). *Recommendation: keep dropped; they are
  OSS-audience breadth, not factory need — but say the word and PA/PB-style waves can add them.*
- **New deps** to vet via `cargo-deny`: `tree-sitter-<lang>` ×11 · `quick-xml` · `tiktoken-rs` · `notify` ·
  `git2` · `pdf-extract`/`lopdf` · `reqwest` (ingest). All mature; gate each on add.

## D. Sizing & sequencing
| Phase | Scope | Rough effort | Method |
|---|---|---|---|
| PA | 11 grammars + docs nodes | LARGE | dynamic Workflow (1 fiber/lang) + parity judge |
| PB | 4 exporters | MEDIUM | direct or 1 fiber/exporter |
| PC | 6 lifecycle features | MEDIUM-LARGE | direct, cache/merge already exist |
| PD | 4 analytics | MEDIUM | direct (heuristic) |
| PE | semantic+pdf+vision+explain | LARGE (gated) | direct, feature-gated, TIERWRIGHT-routed |

**Recommended order:** PA → PB → PC → PD → PE. PA+PB reach *functional* parity (any-language graphs + all
export formats); PC reaches *lifecycle* parity; PD+PE reach *analytic/semantic* parity. Each phase ships
gate-green + parity-tested + pushed to both remotes, EVIDENCE updated per phase.

## E. Definition of done (full parity)
Every row in §A is ✅; `habitat-graph --help` covers graphify's command surface; the comparison table in
`habitat-graph.vault/Commands & graphify Comparison` shows no ❌; parity harness green across the multi-language
golden corpus; workspace gate-green; both remotes current.

---
*Parity plan authored S1008796 · Claude @ cortex. Graphify feature set fetched live 2026-06-28.*
