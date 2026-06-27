> Back to: [[CLAUDE.md]] · [[CLAUDE.local.md]] · [[ULTRAPLATE Master Index]] · [[EXECUTIVE_SUMMARY]]
> Corpus index: [[habitat-graph/README]] · this is the SPINE · siblings: [[01_GRAPHIFY_EXEMPLAR_MAP]] · [[02_HABITAT_INTEGRATION]] · [[03_AUTOMATION_RUNBOOKS]] · [[plan.toml]] · [[ULTRAMAP]] · capstone: [[DEPLOYMENT_FRAMEWORK]] · [[MODULE_STRUCTURE_PLAN]] · [[EVIDENCE]]

# habitat-graph — Codebase Deployment Plan (S1008796)

**Status:** PLANNING ONLY. No code, no `cargo init`, no `devenv` registration, no arming key.
**Mission:** Refactor `github.com/safishamsi/graphify` (Python) into a Rust crate+service,
`habitat-graph`, that is a first-class organ of the ULTRAPLATE factory — a queryable knowledge
graph of the 500K-LOC habitat, wired to the orchestrator kernel plugin, memory substrates, and
the cognitive field.

---

## 1. Why Rust, why now

| Driver | Detail |
|---|---|
| **Habitat is Rust** | 20 services / 500K LOC are Rust. A Python organ is a toolchain island (separate venv, `uv`, no `cargo` gate, no clippy/pedantic). Rust folds it into the one quality regime. |
| **The port is tractable** | The 3 hardest deps are already Rust: `tree-sitter` (native), Leiden (`network_partitions` in graspologic-native), MCP (`rmcp` official SDK). NetworkX → `petgraph`. See `01_GRAPHIFY_EXEMPLAR_MAP.md`. |
| **Serves work in flight** | An auto-extracted producer→consumer arc graph feeds the live `wip/bidi-wiring` (S1008620) arc-coherence effort directly. |
| **Performance** | AST extraction over 500K LOC is CPU-bound and embarrassingly parallel — Rust + `rayon` is the right substrate; Python GIL is not. |
| **Integrity** | AST-only mode is fully local (no API, no source leak), satisfying the workspace-boundary + no-leak invariants by construction. |

**Non-goal:** feature parity with every graphify niche (Office docs, video transcription, AWS
Bedrock) in v1. Those are feature-gated stretch modules. v1 = the AST→graph→cluster→query spine,
hardened and habitat-wired.

---

## 2. Architecture — 9 layers, ~35 modules (habitat-shaped)

Mirrors graphify's linear pipeline `detect → extract → build → cluster → analyze → report →
export`, decomposed into the habitat's layered form (cf. DevOps V3 = 8 layers/40 modules). Each
module: `forbid(unsafe)`, no `unwrap`/`expect` in lib, ≥50 meaningful tests. Full machine
authority in `../plan.toml` + `../ULTRAMAP.md`.

| Layer | Name | Responsibility | Key modules |
|---|---|---|---|
| **L0** | `core` | Shared types, error taxonomy, config, the node/edge/graph schema, `Confidence{Extracted,Inferred,Ambiguous}` | `types`, `error`, `config`, `schema`, `confidence` |
| **L1** | `guard` | Security boundary — URL/path validation, label sanitization (256-char cap, control-char strip), schema validation, secret screen | `validate_url`, `validate_path`, `sanitize`, `schema_check`, `secrets` |
| **L2** | `source` | Acquire + triage input — file detection by extension, remote ingest (size/timeout caps), cache partition (cached vs uncached), manifest | `detect`, `ingest`, `cache`, `manifest` |
| **L3** | `extract` | File → `{nodes,edges}` — tree-sitter AST extractors (per language family), the LLM/semantic extractor, the dispatch registry | `ast::{rust,python,js_ts,go,jvm,c_cpp,ruby,php,…}`, `semantic`, `registry`, `backends` |
| **L4** | `build` | Aggregate extraction into a `petgraph` graph — node/edge dedup, merge, confidence reconciliation | `assemble`, `dedup`, `merge` |
| **L5** | `analyze` | Structure mining — Leiden communities, degree/centrality, anomaly + pattern detection, open-questions surfacing | `cluster` (Leiden), `centrality`, `patterns`, `questions` |
| **L6** | `output` | Emit artifacts — `GRAPH_REPORT.md`, exporters (json/html/svg/graphml/cypher/obsidian/wiki), token benchmark | `report`, `export::{json,html,svg,graphml,cypher,obsidian,wiki}`, `benchmark` |
| **L7** | `iface` | Drive it — CLI (`clap`), MCP server (`rmcp` stdio+SSE), directory watch (`notify`), git hooks (`git2`), HTTP/health (`axum`) | `cli`, `mcp`, `watch`, `hooks`, `serve` |
| **L8** | `habitat` | Factory wiring (feature-gated `--features habitat`) — health/bridge, POVM+injection writer, Obsidian-protocol export, PV2 sphere registrar, orchestrator pipe verb, TIERWRIGHT backend, arc-graph extractor | `bridge`, `memory`, `obsidian_protocol`, `pv2_spheres`, `orchestrator_pipe`, `tierwright`, `arc_graph` |

**Crate shape:** a Cargo workspace. The *publishing grouping* is `habitat-graph-core` (pure libs),
`habitat-graph` (the binary, L7), `habitat-graph-habitat` (L8, feature-gated so the OSS core stays
clean and standalone-publishable). The *authoritative* decomposition is **~13 narrow crates**
(one reason to change each) — see `MODULE_STRUCTURE_PLAN.md`, including the cross-cutting
`habitat-graph-cache` incremental substrate and the `habitat-graph-daemon` warm-DB host
(ADR-04, `04_CACHING_INCREMENTAL_CLUSTER.md`). Habitat
coupling stays *additive*, not load-bearing on the core — the same discipline as ORAC's bridge clients.

---

## 3. Specs & contracts (summary — EARS-style)

- **R1 (AST parity):** WHEN given a file in a supported language, the extractor SHALL emit the same
  node/edge set as graphify's `extract_<lang>` for the `worked/` corpus, within a documented
  tolerance. *Verifiable via the parity harness (§6).*
- **R2 (schema):** The graph SHALL serialize to a `graph.json` byte-compatible with graphify's
  schema so existing consumers (graph.html viewer, MCP clients) interoperate during migration.
- **R3 (local-first):** WHEN extracting a `*.rs`/code file, the system SHALL default to AST-only
  (no network). LLM extraction SHALL be opt-in and route through the configured backend.
- **R4 (determinism):** Given identical input + config, extraction+build SHALL be deterministic
  (sorted node/edge ordering) so `graph.json` diffs are minimal — required for the git merge driver.
- **R5 (safety):** All external input (URLs, paths, labels) SHALL pass L1 `guard` before use;
  graph paths SHALL resolve within the output dir.
- Full interface contracts (graph.json schema, MCP tool signatures, the orchestrator pipe verb
  schema) → `02_HABITAT_INTEGRATION.md` §Contracts.

---

## 4. Security posture (STRIDE-lite)

| Threat | Surface | Mitigation |
|---|---|---|
| **Tampering** | malicious source files fed to tree-sitter | tree-sitter is a parser, not an evaluator; never `exec` extracted content; label sanitization |
| **Info disclosure** | LLM backend leaking source | AST-only default (R3); habitat backend = TIERWRIGHT/local Ollama; no raw external call on source |
| **Elevation/Path** | `../` traversal in ingest/export | L1 `validate_path` confines to output dir (graphify's existing rule, ported) |
| **DoS** | huge remote ingest | size + timeout caps in L2 `ingest` |
| **Supply chain** | crate deps | `cargo-deny` + `cargo-audit` in the gate; pin grammar crate versions |
| **Memory safety** | parser FFI (tree-sitter is C) | the *only* `unsafe` is inside the upstream `tree-sitter` crate; our code is `forbid(unsafe)`; wrap FFI calls, never expose raw pointers |

Gate: Critical/High findings BLOCK; Medium/Low → risk register. (`forge-security-architect` pass.)

---

## 5. Diagnostics & ops

- **Health:** `/health` (axum) returning version, graph node/edge count, last-build timestamp, backend mode. Path-map registered so `cc-health` sees it.
- **Observability:** structured `tracing` spans per pipeline stage (detect/extract/build/cluster/export); optional OTel export (feature `otel`).
- **SLO sketch:** cold full-corpus extract < N min (measure first, set target after baseline); incremental rebuild (single-file delta) < 2 s — the figure that makes the git post-commit hook usable.
- **Runbook:** `devenv restart habitat-graph`; rebuild = `habitat-graph extract . --no-llm`; recovery = delete `graph.json`, full rebuild (idempotent). Full operational set → `runbooks/` (`DEPLOY` · `PARITY` · `MIGRATION` · `INCIDENT`); front door → `justfile`. Design: `03_AUTOMATION_RUNBOOKS.md`.
- **Readiness gate:** the production-readiness checklist (9-cat) is the corpus acceptance gate, same as the orchestrator-kernel v0.1.2 receipts.
- **Automation:** `just` is the front door (`just gate` / `just parity` / `just deploy` / `just arc-graph`); recipes encode the habitat scar-tissue (PIPESTATUS, `/usr/bin/cp -f`, `cc-health`, sphere-naming) so no operator re-learns a trap. Introspectable via `just --dump --dump-format json`.

---

## 6. Migration & parity strategy (the crux of a refactor)

A refactor is only trustworthy if it's *proven equivalent*, not just "also compiles." Discipline:

1. **Golden corpus:** vendor graphify's `worked/` examples (example, httpx, karpathy-repos,
   mixed-corpus) as fixtures. Run Python graphify once, freeze its `graph.json` outputs as goldens.
2. **Parity harness:** a test mode that runs `habitat-graph` over each fixture and diffs the graph
   (node set, edge set, communities) against the Python golden. Diffs are categorized
   EXACT / SEMANTIC-EQUIVALENT (ordering, id scheme) / REGRESSION. Only REGRESSION fails the gate.
3. **Strangler order (lowest-risk first):**
   - **P0 — Core + Guard + schema** (L0/L1): types, error, validation, `graph.json` (de)serialize. Parity = byte-roundtrip a golden `graph.json`.
   - **P1 — AST extraction spine** (L2/L3 AST, Rust+Python+JS/TS first): the bulk of value, fully local. Parity vs goldens.
   - **P2 — Build + Analyze** (L4/L5): petgraph assembly + Leiden (`network_partitions`) + analysis. Parity on community structure (allow label-permutation equivalence).
   - **P3 — Output** (L6): report + json/html/svg/obsidian/graphml/cypher exporters.
   - **P4 — Interface** (L7): CLI, MCP (`rmcp`), watch, hooks, serve.
   - **P5 — Habitat** (L8): bridge/health, memory writer, Obsidian-protocol, PV2, orchestrator pipe, TIERWRIGHT, arc-graph extractor.
   - **P6 — Stretch:** LLM semantic extraction, PDF/Office, Neo4j live driver, video.
4. **Each phase:** own impl → 4-stage gate (check→clippy -D→pedantic→test) → parity harness green → independent verify (`verify-receipt` / `agent-claim-verifier`). No phase collapse.
5. **Coexistence:** until P4 ships, Python graphify remains the reference; `graph.json` schema-compat (R2) lets them interoperate.

---

## 7. Definition of done (v1)

- [ ] P0–P5 complete; each module ≥50 meaningful tests; full 4-stage gate green (`${PIPESTATUS[0]}`).
- [ ] Parity harness: 0 REGRESSION across the `worked/` golden corpus.
- [ ] `forbid(unsafe)` workspace-wide (except the upstream `tree-sitter` FFI); `cargo-deny`/`audit` clean.
- [ ] Health endpoint live; `cc-health` path-map aware; `/health` 200 under soak.
- [ ] Arc-graph extractor (L8 `arc_graph`) reproduces the `arc-coherence-gauge.sh` arc set + flags severed ears.
- [ ] MCP server registered (`mcp__habitat-graph__*`); orchestrator pipe verb schema-validated (ACK/NACK).
- [ ] Standalone repo with own remotes; four-surface persistence of this plan; vault + MASTER_INDEX updated.
- [ ] `just --dump --dump-format json` parses; `just gate`/`just parity` exit-code faithful; root-workspace justfile carries thin `habitat-graph-*` proxies (zero duplicated logic).
- [ ] All 4 runbooks present + followed; no runbook step auto-arms `factory.authorize.*`.
- [ ] No-mistakes publication gate passed on the v1 seal.

---

## 8. What needs Luke @ node 0.A (sparse-signal / arming decisions)

These are deliberately NOT auto-decided (see `09`-class open questions, folded here):

1. **Go / no-go on the Rust refactor at all** vs keeping Python graphify as an external tool. (This plan assumes go-on-planning only.)
2. **Port assignment** — run the `port-claim` skill before any `devenv.toml` entry (S1005032 collision trap). Do NOT hardcode a port in this plan.
3. **Standalone repo name + remotes** (`github.com/Louranicas/habitat-graph`?) and the standalone-only push discipline confirmation.
4. **Backend policy** — confirm AST-only default + TIERWRIGHT routing for the semantic path; whether any external LLM call on source is ever permitted.
5. **`factory.authorize.habitat-graph` arming** — gates any live build/scaffold via LOOMWRIGHT and any service spawn. This plan never writes that key.
6. **OSS-upstream stance** — do we contribute the Rust port back to safishamsi/graphify, or keep it habitat-internal? Affects the core/habitat crate split licensing.
7. **Root-justfile proxy** — confirm the two-tier resolution (standalone repo owns its `justfile`; workspace-root justfile carries thin `habitat-graph-*` proxies). Reconciles the "fold into root justfile" anti-pattern with standalone-push discipline. Recommended in `03_AUTOMATION_RUNBOOKS.md` §1; low-risk.

---

*Authored S1008796 · Claude @ cortex. Design corpus, planning phase. Exemplar = safishamsi/graphify.
Next sibling docs: the module-by-module exemplar map and the habitat integration contract.*
