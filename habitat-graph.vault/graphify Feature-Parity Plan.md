# graphify Feature-Parity Plan

> Back to: [[MOC]] · [[Commands & graphify Comparison (S1008796)]]. **Canonical (full detail):**
> `../ai_docs/08_GRAPHIFY_PARITY_PLAN_S1008796.md`. Graphify feature set fetched live 2026-06-28.

Closing every gap vs `safishamsi/graphify` while keeping the habitat gold standard.

## Gap summary (full matrix in the canonical doc)
- ✅ **At parity:** `query` · `path` · `--mcp` · `graph.html` · `obsidian/` · `GRAPH_REPORT.md` · `graph.json` · blake3 cache · Confidence tags · Leiden.
- ❌ **Languages:** +11 grammars (`ts js go java c cpp rb cs kt scala php`) + docs (`.md/.txt/.rst`) nodes.
- ❌ **Exporters:** `--svg` · `--graphml` · `--neo4j`(cypher) · `--wiki`.
- ❌ **Lifecycle:** `--update`(incremental) · `--watch` · `hook install` · `install` · `add <URL>` · `--mode deep`.
- ❌ **Analytics:** god nodes (surface) · surprising connections · suggested questions · token benchmark.
- 🟡 **Semantic/multimodal:** `backend` crate exists but **unwired**; `.pdf` + image/vision (route via TIERWRIGHT); `explain`.

## Phased roadmap
- **PA — Language breadth** (★ highest value): 1 `Extractor` + grammar per language; dynamic Workflow, 1 fiber/lang + parity judge.
- **PB — Exporters:** svg · graphml · cypher · wiki (pure `Graph -> String`, golden-tested).
- **PC — Lifecycle:** wire existing `cache`+`merge` for `--update`; `notify` watch; `git2` hook + merge driver; `install` (MCP config); `add` (ingest); `--mode deep`.
- **PD — Analytics:** god nodes · `analyze::patterns` · `analyze::questions` · `export::benchmark` (`tiktoken-rs`).
- **PE — Semantic/multimodal** (gated, local-first): wire `extract::semantic`→Backend; `.pdf`; vision via **TIERWRIGHT**; `explain`.

**Order:** PA → PB → PC → PD → PE. PA+PB = functional parity; PC = lifecycle; PD+PE = analytic/semantic.

## Decisions for Luke @ 0.A
- Vision/pdf policy: route all multimodal via **TIERWRIGHT** (recommended) vs allow direct backends?
- Restore the v1-dropped breadth (Bedrock/Gemini/Azure · office · video · translations · multi-platform install) for "ALL features", or keep dropped? *Recommend keep dropped — OSS-audience breadth, not factory need.*
- New deps to vet (`cargo-deny`): tree-sitter ×11 · quick-xml · tiktoken-rs · notify · git2 · pdf-extract · reqwest.

**Already ahead of graphify:** rayon · determinism (R4) · parity-regression gate · parity-transparent cache · forbid-unsafe · security guard · L8 factory integration · 1242 tests.
