> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · [[EVIDENCE]] · supersedes: [[08_GRAPHIFY_PARITY_PLAN_S1008796]] · assimilates: [[09_PARITY_PLAN_GAP_ANALYSIS_S1008796]] · integration: [[02_HABITAT_INTEGRATION]] · topology: [[HABITAT_LIVE_SERVICE_STACK_SCHEMATIC_S1008796]]

# habitat-graph — Plan v2: Agent-First (Factory-Organ) Roadmap — S1008796

> **THIS DOCUMENT SUPERSEDES `ai_docs/08_GRAPHIFY_PARITY_PLAN_S1008796.md`.**
> Doc 08 (the parity plan) is retained as the historical *Frame-A* pass and as the source of the
> **OSS-Parity backlog** (Phase 4 below). Its 5-phase roadmap (PA→PE) and its single Definition of
> Done (§E, *"every graphify row ✅"*) are **no longer the live plan.** This v2 assimilates the
> two-frame gap analysis (doc 09) and re-points the roadmap at the organ's actual consumer.

## 0. Why v2 exists — the frame the plan must name

`habitat-graph` is **already BUILT, gate-green, and pushed** — 13-crate workspace, 1225 all-targets
tests / 0 failed, D0→D7 sealed (`EVIDENCE.md:6`, `:80-85`), port `:8202` claimed
(`EVIDENCE.md:83`), MCP organ **LIVE PROVEN over stdio** (`EVIDENCE.md:76-78`). The critique in
doc 09 is **not** "the build is wrong" — it is "the *next-5-phases plan* points at the wrong
consumer" (doc 09:181).

The exemplar `safishamsi/graphify` is an **anthropocentric human-developer tool** (`graph.html` for
eyes, browsable wiki, svg, `explain` prose, "suggested questions"). Doc 08 adopted *sameness with
that tool* as habitat-graph's own success metric (doc 09:9-18). But the real consumer — per the live
topology (`HABITAT_LIVE_SERVICE_STACK_SCHEMATIC_S1008796.md:62`, `:98-100`) — is the **agent
substrate**: Architect `:8144`, the Claude Code fleet, PV2 Kuramoto `:8132`, ORAC RALPH `:8133`,
POVM `:8125` / injection.db `:8140`, TIERWRIGHT `:8201`. The organ graphs the *source* of those
services and answers `graph_query/path/health` over MCP. **A machine, not a human, is the reader.**

**The pivot the plan must name** (doc 09:170-172): the §C-deferred **"OSS / public-visibility flip"**
(a one-way door, Luke @ 0.A, `EVIDENCE.md:85`) is not a release detail — it is the **fork that
decides which Definition of Done applies.** As a *private factory organ*, the right DoD is
agent-substrate fitness. As an *OSS public tool*, the graphify-parity DoD becomes meaningful. Doc 08
silently assumed the OSS DoD while the organ is private. **v2 corrects that by splitting the DoD in
two and making the parity DoD conditional.**

---

## 1. Dual Definition of Done (the load-bearing change)

Replaces doc 08 §E. Two DoDs; **which one is live is gated on the public-flip decision (Luke @ 0.A).**
Reconciles tension **T2** (doc 09:114).

### 1A. Factory-Organ DoD — **THE LIVE DEFAULT** (private organ)

The organ is *done as a factory organ* when it serves the agent substrate cheaply, coherently, and
with a trust signal. Acceptance:

| # | Criterion | Verifiable by |
|---|---|---|
| FO-1 | MCP exposes **resources + templates** (`habitat-graph://node/{label}`, `://community/{id}`, `://report`) alongside the 3 tools — agents pull subgraph context without a tool round-trip | `resources/list` + `resources/read` over stdio at `:8202`; round-trip test |
| FO-2 | **Token-budgeted serve**: `graph_query(scope, max_tokens=K)` returns a greedily-packed most-relevant subgraph that fits `K` | budget test: output token-count ≤ K; relevance-ordered |
| FO-3 | **Warm query** — no O(n) substring scan; label lookup is indexed | bench: query latency flat as graph grows; `find_by_label` no longer linear (`serve/src/query.rs:14-23`) |
| FO-4 | **Stable node identity** survives `--update`/rename; POVM keys / PV2 sphere ids / agent caches stay coherent | rename-stability gate: rename a fn, NodeId for unchanged nodes is invariant |
| FO-5 | **Completeness/confidence envelope** served on every query; `graph_query` filters by `Confidence` | response carries `{extracted, inferred, ambiguous, coverage}`; `confidence=EXTRACTED` filter test |
| FO-6 | **Daemon atomic reload + fail-soft** — `--update`/`--watch` swap the served graph atomically; concurrent queries get served-stale + a staleness header, never a torn read | `arc-swap` reload test; concurrent-rebuild soak; staleness header present |
| FO-7 | **arc-graph severed-ear served as continuous telemetry** — push-on-rebuild into `arc-coherence-gauge` + orchestrator + injection.db | a rebuild that severs an arc emits a `SeveredEarReport` delta to the gauge data source |
| FO-8 | Factory languages extracted at warranted parity: **TS/JS, then Go**, each with a pinned-oracle golden + per-grammar completeness envelope | per-grammar parity gate vs committed golden (C-1) |
| FO-9 | Workspace gate-green (check→clippy→pedantic→test), `forbid(unsafe)`, no `unwrap`/`expect` in lib, both remotes current | `/gate`; `git ls-remote` == `git rev-parse HEAD` |

> Note FO-8: *full* polyglot breadth is **not** a factory-organ requirement — the factory runs no
> Scala/PHP/Ruby/C#/Kotlin/C/C++ services (doc 09:135). Those grammars are OSS-audience breadth.

### 1B. OSS-Parity DoD — **CONDITIONAL** (gated on the public-flip one-way door, `EVIDENCE.md:85`)

Doc 08 §E **verbatim**, and it is the *correct* DoD **only here**, because only an OSS-public
habitat-graph has human developers as its consumer (doc 09:168). Applies *if and only if* Luke flips
visibility public:

> Every row in doc 08 §A is ✅; `habitat-graph --help` covers graphify's command surface; the
> comparison table shows no ❌; parity harness green across the multi-language golden corpus.

**Carry-over correction (P1-G2, doc 09:30):** even 1B must drop the "no ❌" overstatement —
graphify emits 156 `calls`/`uses` edges per the httpx corpus that habitat-graph *deliberately does
not* (`EVIDENCE.md:44`: NODES 97%, structural 96%, `calls`/`uses` 0/156). "Full parity" under the
current extractor contract is `node≥80% + structural≥70%`, **not byte-parity.** 1B's DoD must state
the achievable envelope, not imply sameness.

---

## 2. The 7 agent-substrate features — promoted to the top (the real backlog)

These have **zero rows** in doc 08 §A — structurally, because graphify (a human tool) has no
equivalent, so a parity-driven plan *cannot surface them* (doc 09:88, 152-160). They are the highest
leverage work and now lead the roadmap.

| AGT | Feature | Where it lands | Crate / module | Effort | Replaces / reframes |
|---|---|---|---|---|---|
| **AGT-1** | MCP graph-as-**resource** + resource-templates (F1) | Phase 0 | `serve::mcp` (`mcp.rs:44-231`) | **SMALL** | new (data + JSON-RPC handler already exist) |
| **AGT-2** | **Token-budgeted serve** `graph_query(scope, max_tokens=K)` (F2) | Phase 0 | `serve::mcp` + `serve::query` | **MED** | **transforms** doc 08 A4 benchmark → actuator |
| **AGT-3** | **Warm retrieval index** — kill the O(n) substring scan; optional embedding (F3/F4) | Phase 1 | `serve` or `cache` (`query.rs:14-23`) | **MED-LARGE** | the deferred "salsa warm-DB" as the agent-latency fix |
| **AGT-4** | **arc-graph as continuous telemetry** — push-on-rebuild `SeveredEarReport` (F6) | Phase 2 | `habitat::arc_graph` (`arc_graph.rs:70-162`) | **MED** | promotes D6's one-shot extractor; the organ's killer app for S1008620 |
| **AGT-5** | **Stable node-identity contract** — content-addressed IDs surviving rename (F9) | Phase 1 | `core::ids` + `build::merge` (`merge.rs`, `assemble.rs:54`) | **MED** | generalizes D6's sphere-id naming-trap fix (`pv2_spheres.rs`) |
| **AGT-6** | **Completeness envelope + confidence filter** on the query surface (F10/F15) | Phase 1 | `core::schema` + `serve::mcp` (`confidence.rs:9-54`) | **MED** | turns silent 80–96% incompleteness into an explicit trust signal |
| **AGT-7** | **Daemon atomic reload + fail-soft backpressure** (F11/C-3) | Phase 0 | `daemon` (`daemon/src/lib.rs:1-14`, `server.rs`) | **MED** | `arc-swap` + staleness header (witness-filter-then-cap) |

---

## 3. Phase 0 (NEW) — make the already-built organ a first-class AGENT surface

*Highest leverage / smallest effort. Make the existing, already-built organ a first-class agent
consumer surface **before adding one new human feature** (doc 09:164, tension T4 doc 09:116).* No
new grammars, no new exporters — pure surface work on what is BUILT and LIVE PROVEN.

### 3.1 AGT-1 — MCP graph-as-resource + templates
- **Now:** `serve::mcp` exposes **tools only** — `graph_query / graph_path / graph_health`
  (`mcp.rs:44-66`; LIVE PROVEN `EVIDENCE.md:76-78`). MCP also has **resources** (`resources/list`,
  `resources/read`) and **resource templates** — the native way an agent pulls a subgraph into
  context *without* a tool round-trip (and without a planning step to choose a tool).
- **Build:** add `resources/list` + `resources/read` to the dispatcher; templates
  `habitat-graph://node/{label}`, `habitat-graph://community/{id}`, `habitat-graph://report`. All
  output continues through `display_safe` (the existing render-boundary mandate, `guard/sanitize.rs:8-50`).
- **Cost:** SMALL — the `&Graph` data and the pure JSON-RPC handler already exist; this is additive
  dispatch + serialization. *Gate:* `resources/list` enumerates templates; `resources/read` round-trips a node/community/report.

### 3.2 AGT-2 — Token-budgeted serve (transform of doc 08 A4)
- **Reframe (F2, doc 09:91):** doc 08's A4 *measures* full-vs-subgraph tokens for a human to admire,
  but never becomes an **actuator**. The agent's defining constraint is a finite window; the
  highest-value organ behavior is `graph_query(scope, max_tokens=K) → greedily-packed
  most-relevant subgraph that fits K`.
- **Build:** add `max_tokens` to the `graph_query` tool input schema and to a new `query_budgeted`
  serve fn; greedy relevance-ordered pack (BFS-from-scope-seeds, highest-degree/closest first) with
  a token estimator (cheap heuristic by default; `tiktoken-rs` optional behind a feature). Keep the
  existing filter-then-cap (`MAX_QUERY_RESULTS=50`, true-total reported, `mcp.rs`).
- **Cost:** MED. *Gate:* output token-count ≤ K across fixtures; deterministic packing (R4); the
  budget never silently drops the seed node.

### 3.3 AGT-7 — Daemon atomic reload + fail-soft backpressure (C-3)
- **Defect today (P1-G6, F11, doc 09:41-42, 100):** the daemon loads `graph.json` into an
  **immutable `Arc<Graph>` at startup** (`daemon/src/lib.rs:1-14`, `EVIDENCE.md:60`). PC's
  `--update`/`--watch` rewrite `graph.json` on disk; **nothing** reloads the daemon — no SIGHUP, no
  arc-swap, no staleness contract. Agents silently query a stale graph; concurrent rebuild behavior
  is undefined.
- **Build:** replace `Arc<Graph>` with **`arc-swap::ArcSwap<Graph>`**; a reload trigger (file-mtime
  watch or explicit signal) loads → validates → atomic-swaps. Serve **stale-with-staleness-header**
  during a rebuild (`X-Graph-Generation` + `X-Graph-Stale: true|false` on HTTP; an equivalent field
  on the MCP `graph_health` envelope) — the witness-filter-then-cap discipline (MEMORY.md
  `feedback_witness_filter_then_cap`).
- **Cost:** MED. *Gate:* `arc-swap` reload test; concurrent-rebuild soak (queries never torn, never
  panic); staleness header asserted; new dep `arc-swap` vetted via `cargo-deny`.

### 3.4 `install` / MCP-register — **pulled forward** from doc 08 PC (C5), with write-safety
- **Mis-sequence fixed (P1-G9, doc 09:55-56):** the single feature that makes the organ *reachable
  by the fleet* — `install` writing the Claude Code MCP config so the organ auto-mounts — sat in
  **phase 3 of 5** behind 11 grammars + 4 exporters. The MCP organ is *already LIVE PROVEN*. Pull it
  to Phase 0.
- **Add write-safety (P1-G8, doc 09:52):** doc 08 C5 mutates `~/.claude.json`-class state with no
  backup, idempotency, or merge story. Adopt the habitat's own **snapshot → write → read-back**
  discipline (CLAUDE.local.md §5): backup the existing config, merge (never overwrite), read-back and
  error on mismatch — exactly the no-risk-write the `habitat::memory` writer already proves
  (`memory.rs`, `EVIDENCE.md:72`).
- **Cost:** SMALL-MED. *Gate:* re-run idempotency; pre-existing-entry merge preserved; backup written.

**Phase 0 exit:** the built organ is a first-class agent surface — resources, a token budget, atomic
reload, and a safe one-command mount — *before any new feature is added.*

---

## 4. Phase 1 — warm, identity-stable, trust-bearing serving

Make the served graph fast, coherent across rebuilds, and honest about what it doesn't know.

- **AGT-3 — Warm retrieval index (C-4 / F3).** Replace the O(n) case-insensitive substring scan
  (`find_by_label`, `serve/src/query.rs:14-23`, `EVIDENCE.md:59`) with an inverted/trigram label
  index built at load (and incrementally on swap). Agents query in tight loops (orchestrator
  mission-decomposition → many `map.scope` calls); O(n)/query is the latency wall. *Optional* second
  stage: a vector index over labels/docstrings + a `query_semantic` tool (F4) — **decision-gated**
  (embedding backend; see §10). *Gate:* query latency flat as node count grows; index result-set ==
  linear-scan result-set (equivalence test).
- **AGT-5 — Stable node-identity contract (F9, tension T3).** Today `--update` re-interns labels →
  `NodeId` **by first-seen order** (IndexMap `Entry::Vacant`, `build/src/assemble.rs:54`;
  `merge.rs`). A function rename changes its NodeId → every POVM pathway key (node-pair), PV2 sphere
  mapping, and agent cache silently goes incoherent. D6 already solved this for *sphere* ids
  (numeric `CommunityId`, never the mutable label — `pv2_spheres.rs`, `EVIDENCE.md:73`); generalize
  it. Move to **content-addressed node IDs** (hash of stable identity = kind ‖ qualified-path ‖
  normalized-signature, not raw label position) that survive `--update`/rename. *Gate:* a
  rename-stability test — rename a fn, assert unchanged nodes keep their IDs.
- **AGT-6 — Completeness envelope + confidence filter (F10/F15).** The `Confidence` enum is "the
  trust signal the whole graph rests on" (`confidence.rs:9-54`) but is served as a *tag for a human
  to eyeball*. Make it an agent decision filter: `graph_query` accepts a `confidence` filter
  (`EXTRACTED`-only for a load-bearing wiring decision), and **every** response carries a breakdown
  `{extracted, inferred, ambiguous, coverage_pct}`. This turns PA's silent 80–96% incompleteness
  (`EVIDENCE.md:44`) into an explicit envelope. *Gate:* filter test; envelope present on every query
  response.
- **C-4 — real incremental `--update`.** Doc 08 C2 wires `cache::partition` + `build::merge` for
  incremental update (`cache/src/partition.rs:13-28`, currently **orphaned** — no crate depends on
  cache; `cli/src/commands/extract.rs:36-90` always runs the full pipeline). But the cache is
  **file-extraction-level only**; the downstream **analyze stage (Leiden) re-runs globally** on every
  update (`analyze/src/cluster.rs:45-135`, seeded `cluster.rs:26`), and Leiden is not incremental
  (P1-G5, C-4, doc 09:38-39, 125). Wire the cache into `extract --update` **and** either bound
  re-clustering to touched communities or document the analyze cost honestly. **Stop calling
  file-cache "incremental."** *Gate:* `--update` is idempotent + parity-stable; analyze cost is
  measured, not assumed sub-second.
- **Schema-versioning (P1-G12, doc 09:64).** Add an explicit `schema_version` to the graph.json
  node-link envelope (`core/src/schema.rs:1-159`); `--update` against an older-taxonomy graph.json
  must detect the mismatch instead of silently merging incompatible schemas. Record the pinned
  graphify oracle version (the 2026-06-28 snapshot is currently un-versioned).
- **C-2 (cheap half) — SSRF private-IP block.** `validate_url` (`guard/src/url.rs:15-42`) rejects
  scheme/creds/control/bidi but **does not block private-IP / loopback / metadata** (P1-G7, doc
  09:47). In an agent factory the *agent* chooses URLs (`add http://localhost:8125/…` POVM,
  `add http://169.254.169.254/…` cloud metadata). Add a private-IP/loopback/metadata block now — it
  is cheap, the guard already exists, and it gates any future `add <URL>`. *Gate:* url-guard rejects
  `127.0.0.0/8`, `169.254.169.254`, `::1`, `10/8`, `192.168/16`, `172.16/12`.

---

## 5. Phase 2 — the substrate payoff (direct pay-in to S1008620)

This is where the organ stops being a read-only oracle and starts *feeding the cognitive loops*.

- **AGT-4 — arc-graph as continuous telemetry (F6).** D6 built `arc_graph::extract_arcs` +
  `diff_arcs → SeveredEarReport{present, severed, coherence}` (`arc_graph.rs:70-162`,
  `EVIDENCE.md:70`) — deterministic (R4), partial-graph-safe. Today it is a **one-shot extractor**.
  Promote it to a **served, push-on-rebuild telemetry stream**: on each rebuild, diff arcs vs prior
  and push the `SeveredEarReport` delta into `.claude/scripts/arc-coherence-gauge.sh` (its declared
  data sink, `02_HABITAT_INTEGRATION.md:70`), the orchestrator pipe, and an injection.db
  `causal_chain` row on a newly-severed arc. This auto-detects the severed bidi-wiring arcs the live
  S1008620 work **hand-builds** today. *Gate:* a rebuild that severs an arc emits the delta to the
  gauge; arc-coherence reads it end-to-end.
- **`--watch` → delta-push, not artifact-refresh (F7, doc 09:96).** doc 08 C3's `--watch` (notify +
  debounce) detects change and rebuilds `graph.json` *for a human to re-browse*. Re-target it: on a
  source change, compute the graph **delta** and **push** it to PV2 (sphere topology update,
  `pv2_spheres.rs`), POVM/injection.db ("architecture changed"), and arc-coherence. The loops *learn
  that the architecture moved* instead of the human merely getting a fresh artifact. **Live actuation
  (PV2/POVM writes) is arming-gated** — `factory.authorize.habitat-graph` (Luke @ 0.A,
  `02_HABITAT_INTEGRATION.md:154-155`).
- **god-nodes / cross-community re-aimed at arc-graph (F8, doc 09:97).** doc 08 A1 ("god nodes",
  `analyze::degree_centrality` already computed) + A2 ("surprising connections") compute exactly the
  high-degree hubs + cross-community high-weight edges that **severed-ear / bridge analysis
  consumes.** Keep the computation; **re-target the sink** from a human report section to arc-graph
  telemetry + the `bridge-contract` / `schema-drift` skills. Drop the "surprising connections" report
  framing (vanity, F-V… / doc 09:144).
- **watch × hook single-writer lock (P1-G10, doc 09:58).** A commit fires the git hook **and**
  `notify` sees the same writes → concurrent rebuilds racing on the `graph.json` write. Add a
  single-writer lock across watch + hook. *Gate:* concurrent commit+watch produces one clean rebuild,
  not a torn file.
- **Split the rebuild pipeline (F13, doc 09:105).** Separate **agent-critical rebuild** (graph.json +
  warm index + arc delta) from **human-artifact rebuild** (html/svg/wiki). Human artifacts must never
  gate the agent path — a redrawn SVG must not widen the agent's staleness window. *Gate:*
  agent-critical path latency is independent of human-artifact generation.

---

## 6. Phase 3 — extraction breadth, **trimmed to factory-actual languages**

- **PA trimmed (doc 09:135).** doc 08 PA proposed **11 grammars** (`ts js go java c cpp rb cs kt
  scala php`). The factory runs **no Scala/PHP/Ruby/C#/Kotlin/C/C++ services** — ~7 of 11 are
  OSS-audience breadth. **Keep TS/JS, then Go** (the factory-actual non-Rust languages: the Zellij
  plugin/dashboard surfaces are TS/JS; Go appears in tooling). The other ~7 → **OSS-Parity backlog
  (Phase 4).** Existing extractors: Rust + Python (`EVIDENCE.md:43-44`); the registry already
  extension-dispatches (`extract/src/registry.rs:40-58`).
- **Doc nodes (L2) kept — genuine agent value.** `.md/.txt/.rst` heading/section nodes feed
  retrieval (AGT-3) and the Obsidian/`hmem` path that agents actually use (doc 09:135, 167). Keep.
- **C-1 — the golden-corpus budget made explicit (doc 09:27, 122).** doc 08 PA's gate is "per-grammar
  parity vs a small golden" but the **only** existing golden is httpx Python (`EVIDENCE.md:44`). The
  real per-grammar cost is `extractor + ≥50 tests + **source a representative corpus + run the
  graphify oracle on it + commit a version-pinned golden**` — the golden pipeline is the larger half
  and was invisible in doc 08's sizing. **Budget it.** Pin the graphify oracle version (P1-G12).
- **ABI-skew resolved, not "vetted" (P1-G4, doc 09:35).** `tree-sitter-<lang>` crates routinely
  require **incompatible `tree-sitter` core ABI versions (13/14/15)**; cargo permits one core
  version. Produce an ABI-compatibility matrix for TS/JS/Go *before* committing the LARGE sizing.
  `cargo-deny` checks licenses/advisories, **not** ABI — so this is a real, separate gate.
- **`--mode deep` confidence-gated OUT of the agent path (T1 / F12, doc 09:104, 113).** doc 08 C1's
  heuristic `uses`/`references` edges (the 0/156 class) **inflate degree, merge Leiden communities
  that should be distinct, corrupt centrality, and distort the community→PV2-sphere topology and
  arc-graph.** If built at all, INFERRED/AMBIGUOUS edges must be **gated out of
  analyze→sphere→arc-graph** (filter on `Confidence` at the analyze input) and exposed *only* on
  explicit human export. *Gate:* deep-mode edges never reach `analyze::detect_communities`.
- **Per-grammar completeness envelope (F15).** Every new grammar ships with its served completeness
  envelope (ties to AGT-6) so agents never make load-bearing decisions on a silently-incomplete graph.

*Method:* the proven dynamic Workflow — one `forge-rust-coder-v4` fiber per language (collision-free,
distinct files) + `forge-tester` parity judge **outside the loop** (the regime that built D2–D6,
`EVIDENCE.md:41-74`).

---

## 7. Phase 4 (OPTIONAL) — the OSS-Parity backlog, gated on the public-flip

**Built only if Luke flips visibility public (`EVIDENCE.md:85`).** Here — and only here — doc 08 §A
rows are obligations and the **OSS-Parity DoD (§1B) applies**, because the consumer is a human
developer (doc 09:168).

| Item (doc 08 ref) | Why deferred (frame) | Ship-gate if built |
|---|---|---|
| **PB exporters** — `--svg` (X1) · `--graphml`/`--neo4j` (X2/X3) · `--wiki` (X4) | Human-eyes / Gephi-yEd / Neo4j — **no factory consumer**; the 20-service stack runs no graph DB (F-V1/V2/V3, doc 09:77-79) | each vs golden; svg/graphml valid XML; cypher parses; wiki `index.md` links resolve |
| **Remaining ~7 grammars** (`java c cpp rb cs kt scala php`) | No factory services in these languages (doc 09:135) | per-grammar pinned-oracle golden (C-1) + ABI matrix |
| **`explain "<concept>"`** (S4) | **Negative value** for the factory — the agent IS the LLM; a TIERWRIGHT round-trip to pre-chew a subgraph into prose, for a consumer that reads the structured subgraph faster, wastes tokens (F-V5, doc 09:81) | human-readable summary; subgraph-context assembled |
| **`suggested questions`** (A3) | Canned questions are *for a human reader*; an agent generates its own queries from its task (F-V4, doc 09:80) | deterministic (note P1-G11: LLM-augment ≠ deterministic — resolve before gating) |
| **PDF ingest** (S2) | Low factory value (factory corpus is local source, not papers) **+ DoS surface** | **C-2 ship-gate**: `pdf-extract`/`lopdf` size cap + timeout + memory bound (decompression-bomb / malformed-object OOM, P1-G7 doc 09:46) |
| **Vision / multimodal** (S3) | Lowest value, highest effort, decision-blocked, TIERWRIGHT-hostage (doc 09:149) | route via TIERWRIGHT `:8201` only (`tierwright.rs:1-124`); image-prompt-injection defense; Luke policy decision (§10) |
| **`add <URL>`** (C6) | Remote ingest; low factory value | **C-2 ship-gate**: SSRF block (now in Phase 1) + size/timeout caps |

> The **obsidian exporter** (already DONE, `export/src/obsidian.rs`, `EVIDENCE.md:53`) is the **only**
> doc-08 export with genuine dual value — it feeds `hmem`/FTS5 recall agents use. It stays in the
> live build, not the backlog.

---

## 8. The convergent fixes (both frames demand them — strongest signal, doc 09:120-126)

These appear in **both** Frame A (security/correctness hygiene) **and** Frame B (substrate fitness),
so they are not optional and not frame-dependent. Their landing phase is fixed here:

| Conv | Fix | Lands | Grounding |
|---|---|---|---|
| **C-1** | 11→trimmed-grammar **golden-corpus budget** + pinned oracle + per-graph completeness envelope | Phase 3 (§6) | P1-G1 / F15 |
| **C-2** | pdf/url/vision **resource + SSRF + injection caps** as a ship-gate | SSRF block → Phase 1; pdf/vision sandbox → Phase 4 ship-gate | P1-G7 |
| **C-3** | daemon **atomic reload + cache-invalidation** (`arc-swap` + staleness header) | Phase 0 (AGT-7, §3.3) | P1-G6 / F11 |
| **C-4** | `--update` is **not actually incremental** (global Leiden) — fix or stop claiming it | Phase 1 (§4) | P1-G5 / F3 |

---

## 9. The transforms (re-aim existing doc-08 items at the real consumer)

Three doc-08 items are not dropped — they are **re-pointed** so their output reaches a factory
consumer instead of human eyes:

1. **Token-benchmark → serve-side budgeter.** doc 08 A4 *measured* tokens as a vanity stat; v2 turns
   it into the `graph_query(max_tokens=K)` **actuator** (AGT-2, §3.2). The single highest-leverage
   reframe in the plan (doc 09:146).
2. **`--watch` artifact-refresh → delta-push.** doc 08 C3 refreshed human artifacts; v2 pushes the
   graph **delta** into PV2/POVM/arc-coherence so the cognitive loops learn the architecture moved
   (F7, §5).
3. **`--mode deep` discovery → confidence-gated out.** doc 08 C1 added richer INFERRED edges for human
   serendipity; v2 keeps them **out of** analyze/sphere/arc-graph (they poison community detection,
   centrality, and sphere topology) and exposes them only on explicit human export (T1/F12, §6).

---

## 10. Phased roadmap, sizing & method

| Phase | Items | Frame served | Effort | Method | Phase gate |
|---|---|---|---|---|---|
| **P0** | AGT-1 MCP resources · AGT-2 token-budget serve · AGT-7 daemon atomic reload+fail-soft · `install` pulled-forward (+write-safety) | **Factory (front door)** | **SMALL-MED** | direct (data + JSON-RPC handler already exist) | resources round-trip · `max_tokens=K` packs ≤K · `arc-swap` reload + staleness header · `install` idempotent + backup |
| **P1** | AGT-3 warm index · AGT-5 stable IDs · AGT-6 completeness envelope + confidence filter · C-4 real incremental `--update` · schema-versioning · C-2 SSRF private-IP block | **Factory** | **MED-LARGE** | direct | flat query latency · rename-stability gate · envelope on every query · `--update` idempotent + analyze cost measured · `schema_version` present · url-guard blocks private-IP |
| **P2** | AGT-4 arc-graph push telemetry · `--watch`→delta-push (F7) · god-nodes/cross-community→arc-graph (F8) · watch×hook lock · split rebuild pipeline (F13) | **Factory (pays into S1008620)** | **MED** | direct (`arc_graph` exists) | severed-ear delta on rebuild reaches the gauge · delta pushed to PV2/POVM (arming-gated) · single-writer lock · agent path latency-independent of human artifacts |
| **P3** | PA **trimmed** TS/JS→Go + doc-nodes (L2) · golden corpus + pinned oracle (C-1) · ABI matrix (P1-G4) · per-grammar completeness envelope · `--mode deep` gated OUT (T1/F12) | **BOTH (trimmed)** | **LARGE** | dynamic Workflow (1 fiber/lang) + parity judge outside the loop | per-grammar parity vs pinned-oracle golden · ABI resolved · deep-mode edges excluded from analyze |
| **P4** *(OPTIONAL — gated on OSS-public flip, `EVIDENCE.md:85`)* | PB svg/graphml/cypher/wiki · remaining 7 grammars · explain · suggested-questions · pdf (sandboxed) · vision (TIERWRIGHT, decision-gated) · `add <URL>` | **OSS-Parity** | **LARGE** | as doc 08 PA/PB/PD/PE | **doc 08 §1B parity DoD applies HERE ONLY** · pdf/vision/url pass C-2 caps |

**Sizing notes (the hidden costs doc 08 missed):**
- **P0 is the cheapest, highest-leverage phase** — it ships *zero new features*, only surfaces the
  built organ better. Do it first.
- **P3 effort is dominated by the golden pipeline, not the extractors.** Each grammar = extractor +
  ≥50 tests + *corpus sourcing + oracle run + version-pinned golden commit* (C-1). Doc 08 sized only
  the first half.
- **AGT-3's optional embedding stage and P4's vision are the only items needing a backend decision**
  (§11) — everything else is local-first and ships behind no external call.
- The build **method is proven**: D0→D6 were built by dynamic Workflow with judges outside the loop;
  the claim-verifier already caught a fiber over-claiming gate-green (`EVIDENCE.md:69`). Keep it.

---

## 11. Decisions for Luke @ 0.A (none auto-decided)

1. **THE FORK — public-flip / which DoD is live (§1).** Private factory organ → **Factory-Organ DoD
   (§1A), Phases 0–3** is the live plan. OSS-public flip → **OSS-Parity DoD (§1B), Phase 4** unlocks.
   This one decision gates the entire shape of the roadmap. *Recommendation: stay private; adopt §1A
   as the live default.*
2. **Arming `factory.authorize.habitat-graph`** for Phase 2 live actuation (PV2 sphere + POVM/
   injection.db delta-push). Read-only telemetry needs no arming; **writes do**
   (`02_HABITAT_INTEGRATION.md:154-155`). Until armed, P2 runs in measure-only (arc-graph computes,
   does not push).
3. **AGT-3 embedding stage (F4) & P4 vision (S3) — backend policy.** Both need a model path. Options:
   local Ollama · TIERWRIGHT `:8201` routing (the habitat rule, `tierwright.rs`) · defer. *Recommendation:
   TIERWRIGHT-routed if built; defer the embedding stage until the trigram index proves insufficient.*
4. **New deps to vet (`cargo-deny`/`cargo-audit`).** Phase 0–3: `arc-swap` (AGT-7), `tiktoken-rs`
   (optional, AGT-2), `notify` (`--watch`), `git2`→libgit2 (hook). Phase 3: `tree-sitter-{typescript,
   javascript,go}` — **resolve the ABI matrix first** (P1-G4). Phase 4 only: `pdf-extract`/`lopdf`
   (RUSTSEC history), `reqwest` (`add <URL>`).
5. **Disposition of the OSS-Parity backlog (§7).** Keep it as a *conditional* Phase 4, or drop the
   human-only items entirely (svg/wiki/explain/suggested-questions) even from the OSS path?
   *Recommendation: keep as conditional backlog; build only on the public flip.*
6. **`--feature live` granularity (reviewer risk).** Today `live = {rusqlite, ureq, backend/net}` is
   one coarse flag — you cannot enable injection.db writes without also pulling PV2 HTTP + semantic
   net. Split into `live-memory` / `live-bridges` / `live-semantic`? *Recommendation: split before P2
   live actuation, so the delta-push surface is least-privilege.*

---

## 12. Definition of done — recap

- **Live (private organ):** the **Factory-Organ DoD (§1A, FO-1…FO-9)** — agent-resourced MCP,
  token-budgeted + warm + identity-stable + trust-bearing serving, atomic reload, arc-graph telemetry
  live, factory languages (TS/JS→Go) at warranted parity, gate-green on both remotes.
- **Conditional (OSS public):** the **OSS-Parity DoD (§1B)** — doc 08 §A rows ✅ at the achievable
  `node≥80% + structural≥70%` envelope (not byte-parity, P1-G2), across the multi-language golden
  corpus — **applies only after the public-flip one-way door.**
- **Every phase:** ships gate-green (`check→clippy→pedantic→test`, `${PIPESTATUS[0]}`),
  `forbid(unsafe)`, no `unwrap`/`expect` in lib, ≥50 meaningful tests on substantive modules at
  release-eligibility, EVIDENCE.md updated per phase (never ahead of the gate), both remotes current.

---

## 13. Disposition of every doc-08 row (nothing lost)

| doc 08 row | v2 disposition |
|---|---|
| L1 11 grammars | **TRIM** → TS/JS+Go (P3); other 7 → P4 OSS backlog |
| L2 doc nodes | **KEEP** (P3) — feeds retrieval |
| X1 svg · X2 graphml · X3 cypher · X4 wiki (PB) | **DEFER** → P4 (vanity for the factory, F-V1/V2/V3) |
| C1 `--mode deep` | **GATE OUT** of analyze/sphere/arc-graph (P3, T1/F12); human-export only |
| C2 `--update`+merge | **KEEP + FIX** real incrementality (P1, C-4) + stable IDs (AGT-5) + schema-version |
| C3 `--watch` | **KEEP + RE-TARGET** to delta-push (P2, F7) |
| C4 hook install | **KEEP**, low priority (P2) + watch×hook lock |
| C5 install / MCP register | **PULL FORWARD** to P0 + write-safety |
| C6 `add <URL>` | **DEFER + HARDEN** (P4, SSRF block lands P1, C-2) |
| A1 god nodes | **KEEP + RE-AIM** to arc-graph (P2, F8) |
| A2 surprising connections | **KEEP computation, DROP report framing**, re-aim to arc-graph (P2, F8) |
| A3 suggested questions | **DROP** (P4 only, F-V4) |
| A4 token benchmark | **TRANSFORM** → AGT-2 serve-side budgeter (P0, F2) |
| S1 semantic extraction | **KEEP, PIVOT** toward retrieval embeddings (P1 AGT-3 optional / F4) |
| S2 pdf | **DEFER + HARDEN** (P4, C-2) |
| S3 vision | **DEFER** (P4, decision-gated, F-V… effort) |
| S4 explain | **DROP** (P4 only — negative value, F-V5) |
| — (new) | **AGT-1…AGT-7** added as P0–P2 leads |

---
*Plan v2 (agent-first) authored S1008796 · Claude @ cortex. Supersedes `08_GRAPHIFY_PARITY_PLAN_S1008796`;
assimilates `09_PARITY_PLAN_GAP_ANALYSIS_S1008796` (both passes are the plan, CLAUDE.local.md §3).
Live actuation gated on `factory.authorize.habitat-graph` + the public-flip one-way door (Luke @ 0.A).*
