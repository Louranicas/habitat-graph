# habitat-graph

> **STATUS: PLANNING ONLY (S1008796).** No code yet — this folder currently holds the design
> corpus for refactoring [`safishamsi/graphify`](https://github.com/safishamsi/graphify) (Python)
> into a Rust crate + factory organ. No `cargo init`, no `devenv` registration, no arming key.

A queryable **knowledge-graph organ** for the ULTRAPLATE factory: turn the 500K-LOC habitat (and
any folder of code/docs) into a graph you can query, path-find, cluster, and feed to the
orchestrator kernel plugin — built in Rust, gated by the habitat's quality regime, wired to its
memory + cognitive substrates.

## Why

The exemplar (graphify) is excellent but Python — a toolchain island in a Rust habitat. The port is
unusually tractable because the hard dependencies are *already Rust*: tree-sitter (native), Leiden
(`network_partitions`), MCP (`rmcp`), NetworkX → `petgraph`. The only genuinely new work is the LLM
backend layer and the HTML viewer — both of which the habitat wants re-pointed anyway (TIERWRIGHT,
local-first).

## Planning corpus (read in order)

| Doc | What |
|---|---|
| **[`ai_docs/14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901.md`](ai_docs/14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901.md)** | **★ LIVE PLAN (v3)** — full graphify parity + agentic/multi-model (Claude 4.8+/GPT-5.5+); supersedes 08+10 ([[14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901]]) |
| [`ai_docs/15_FEATURE_ASSIMILATION_MATRIX_S1008901.md`](ai_docs/15_FEATURE_ASSIMILATION_MATRIX_S1008901.md) | **Feature matrix** — every graphify feature → home/status/phase/gate ([[15_FEATURE_ASSIMILATION_MATRIX_S1008901]]) |
| [`ai_docs/16_ARCHITECTURE_SCHEMATICS_V3_S1008901.md`](ai_docs/16_ARCHITECTURE_SCHEMATICS_V3_S1008901.md) | **Schematics v3** — target full-parity + agentic architecture ([[16_ARCHITECTURE_SCHEMATICS_V3_S1008901]]) |
| [`ai_docs/17_CROSS_MODEL_AGENTIC_CONTRACT_S1008901.md`](ai_docs/17_CROSS_MODEL_AGENTIC_CONTRACT_S1008901.md) | **Cross-model contract** — Claude 4.8+/GPT-5.5+ agentic drive ([[17_CROSS_MODEL_AGENTIC_CONTRACT_S1008901]]) |
| [`ai_docs/18_DIAGNOSTICS_OBSERVABILITY_V3_S1008901.md`](ai_docs/18_DIAGNOSTICS_OBSERVABILITY_V3_S1008901.md) | **Diagnostics v3** — per-grammar/ABI/parity-regression/semantic observability ([[18_DIAGNOSTICS_OBSERVABILITY_V3_S1008901]]) |
| [`ai_docs/19_PLAN_SCHEMATIC_MAP_S1008901.md`](ai_docs/19_PLAN_SCHEMATIC_MAP_S1008901.md) | **Plan schematic map (LLM-optimized)** — ratified plan as Mermaid + machine edge-lists + a node-link plan-graph the organ can ingest ([[19_PLAN_SCHEMATIC_MAP_S1008901]]) |
| [`runbooks/V3_LIVE_ORGAN_RUNBOOK_S1008901.md`](runbooks/V3_LIVE_ORGAN_RUNBOOK_S1008901.md) | **V3 live-organ ops runbook** — operate-frame companion (NA pass, doc 14 §7B): freshness/loop-closure policy, grammar ops, live-actuation rituals, the organ's own incidents ([[V3_LIVE_ORGAN_RUNBOOK_S1008901]]) |
| [`ai_docs/abi-matrix-s1008901.md`](ai_docs/abi-matrix-s1008901.md) | **G-ABI matrix (PASS)** — tree-sitter core 0.25.x + all 13 grammars at latest via `tree-sitter-language`; 0 defers; unblocks PA-1 ([[abi-matrix-s1008901]]) |
| [`EXECUTIVE_SUMMARY.md`](EXECUTIVE_SUMMARY.md) | **Start here** — one-page hub; thesis, the ask, at-a-glance, links to everything ([[EXECUTIVE_SUMMARY]]) |
| [`docs/DEPLOYMENT_FRAMEWORK.md`](docs/DEPLOYMENT_FRAMEWORK.md) | **Capstone** — gold-standard (deep-diff-forge) deployment framework: gate stack G0–G10, deployment modes, maturity D0–D8, receipts, rollback, bidirectional doc map |
| [`docs/MODULE_STRUCTURE_PLAN.md`](docs/MODULE_STRUCTURE_PLAN.md) | **Detailed module planning** — ~13 narrow crates (+ `cache` + `daemon`), charters (`src/` trees), dependency graph, forbidden deps, code-flow, testing gold standard |
| [`ai_docs/04_CACHING_INCREMENTAL_CLUSTER.md`](ai_docs/04_CACHING_INCREMENTAL_CLUSTER.md) | **ADR-04** — caching/incremental/cluster decision; SOTA (salsa · differential-dataflow · DBSP); the `cache` crate; POVM-weighted novelty |
| [`ai_docs/05_INTERFACE_CONTRACTS.md`](ai_docs/05_INTERFACE_CONTRACTS.md) | **Interface contracts** (pre-arm spike) — schema · salsa query graph · daemon UDS protocol · MCP/orchestrator schemas · parity-transparency invariant |
| [`EVIDENCE.md`](EVIDENCE.md) | **Evidence ledger** (template) — `claim \| warrant \| evidence` per phase D0–D8 |
| [`ai_docs/00_DEPLOYMENT_PLAN.md`](ai_docs/00_DEPLOYMENT_PLAN.md) | **Spine** — vision, 9-layer architecture, specs, security, diagnostics, migration/parity strategy, done-criteria, Luke-gated decisions |
| [`ai_docs/01_GRAPHIFY_EXEMPLAR_MAP.md`](ai_docs/01_GRAPHIFY_EXEMPLAR_MAP.md) | **Exemplar map** — all 19 graphify modules → Rust homes; every dependency → crate; what we defer |
| [`ai_docs/02_HABITAT_INTEGRATION.md`](ai_docs/02_HABITAT_INTEGRATION.md) | **Integration contract** — devenv/health, memory, orchestrator pipe, arc-graph, PV2, TIERWRIGHT, gate, repo discipline |
| [`ai_docs/03_AUTOMATION_RUNBOOKS.md`](ai_docs/03_AUTOMATION_RUNBOOKS.md) | **Automation** — justfile recipe taxonomy + runbook set; root-vs-standalone justfile decision |
| [`plan.toml`](plan.toml) | **Machine authority** — the LOOMWRIGHT warp (layers + modules + gates) |
| [`ULTRAMAP.md`](ULTRAMAP.md) | **Dependency map** — bottom-up build order + parity phases |
| [`justfile`](justfile) | **Front door** (sketch) — quality · parity · graph · deploy · habitat recipe groups |
| [`runbooks/`](runbooks/) | **Operational memory** — `DEPLOY` · `PARITY` · `MIGRATION` · `INCIDENT` runbooks |

## Architecture at a glance

```
L0 core → L1 guard → L2 source → L3 extract → L4 build → L5 analyze → L6 output → L7 iface → L8 habitat
          (9 layers · ~35 modules · forbid(unsafe) · ≥50 tests/module · ~1750-test floor)
```

Pipeline (from the exemplar): `detect → extract → build → cluster → analyze → report → export`,
plus serve(MCP) · watch · hooks · ingest · cache · guard · benchmark.

## Crate split

- `habitat-graph-core` — L0–L6 pure libs (OSS-publishable, no habitat coupling)
- `habitat-graph` — L7 binary (CLI + MCP server)
- `habitat-graph-habitat` — L8, `--features habitat` (factory wiring; additive, never load-bearing)

## Not in v1

Multi-cloud LLM backends (Bedrock/DeepSeek/Kimi/Azure), Office docs, video transcription, doc
translations, non-Claude platform installers. All reversible via the crate split. See exemplar map §D.

## Next decision (Luke @ 0.A)

Go / no-go on the refactor. This corpus is planning only; live build/scaffold is gated on
`factory.authorize.habitat-graph` (never auto-written) and a `port-claim` pass.

---
*Exemplar: github.com/safishamsi/graphify · Design corpus authored S1008796 · Claude @ cortex.*
