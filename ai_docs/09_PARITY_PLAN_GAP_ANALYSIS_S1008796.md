> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · [[EVIDENCE]] · plan under analysis: [[08_GRAPHIFY_PARITY_PLAN_S1008796]] · exemplar: [[01_GRAPHIFY_EXEMPLAR_MAP]] · integration: [[02_HABITAT_INTEGRATION]] · topology: [[HABITAT_LIVE_SERVICE_STACK_SCHEMATIC_S1008796]]

# graphify Parity Plan — Two-Frame Gap Analysis (S1008796)

**Discipline:** *"Write it once, then ask what frame is that? and write it again from the frame you didn't take. Both passes are the plan."* (CLAUDE.local.md §3)

**Input:** `ai_docs/08_GRAPHIFY_PARITY_PLAN_S1008796.md` (101 lines, gap matrix + 5-phase roadmap PA→PE + DoD).

**Frame call: DOMINANT = A (Conventional / anthropocentric / feature-parity-with-a-human-dev-tool). NOT balanced.**
The plan carries Frame-B vocabulary in its periphery (TIERWRIGHT routing, the L8 integration listed under "already ahead", line 37–39), but **every phase PA→PE and the entire Definition of Done §E are Frame A.** The Frame-B substrate wiring (D6 `arc_graph`/POVM/PV2) is treated as DONE background; **zero PA→PE work advances it.** This is frame collapse, not balanced input: the exemplar's frame (a tool humans operate) has been adopted as habitat-graph's own success metric.

Evidence for the frame call (quoted):
- L5: *"close every gap between habitat-graph and `safishamsi/graphify`"* — success defined as sameness-to-human-tool.
- L43: *"Language breadth (biggest visible gap) ★ highest **user value**"* — "visible" and "user value" are human-eyes terms.
- L91: *"PA+PB reach **functional parity** (any-language graphs + all export formats)"*.
- L95–97 (§E DoD): *"Every row in §A is ✅; `habitat-graph --help` covers graphify's **command surface**; the comparison table … shows **no ❌**"*.

The real consumer (per the task framing and `HABITAT_LIVE_SERVICE_STACK_SCHEMATIC`): the **agent substrate** — Architect `:8144`, the Claude Code fleet, PV2 Kuramoto `:8132`, ORAC RALPH `:8133`, POVM `:8125`/injection.db, TIERWRIGHT `:8201`. The exemplar `graphify` is an **anthropocentric** human-developer tool (graph.html for eyes, browsable wiki, svg, `explain` prose, "suggested questions"). Optimising habitat-graph to be the same tool is the unexamined frame.

---

## PASS 1 — Conventional gap analysis (plan-if-finished)

*Assume PA→PE fully executed and gate-green. What remains, even on the plan's own terms?*

### P1-G1 — No parity corpus for 10 of the 11 new grammars; the golden-generation cost is uncounted **[HIGH]**
PA's gate is *"per-grammar node/edge parity vs a small golden"* (L47) and sizing is *"~11 extractors × (~120 LOC + ~50 tests)"* (L48). But the **only** existing golden is the httpx Python corpus (`EVIDENCE` D2, L44). To parity-test each new grammar you must (a) source a representative corpus per language, (b) **run graphify itself (Python, the oracle) on each** to emit the reference node-link JSON, (c) commit + version-pin them. None of this is in the plan or the sizing. The real PA effort is `11 × (extractor + 50 tests + corpus sourcing + oracle run + golden commit)` — the golden pipeline is the larger half and is invisible. **Risk shipped:** PA "completes" against ad-hoc or absent goldens, the parity gate becomes decorative, and per-grammar correctness is unwarranted.

### P1-G2 — The Definition of Done (§E) contradicts the documented `calls`/`uses` heuristic divergence **[HIGH]**
The httpx parity result is **NODES 97%, structural edges 96%, but `calls`/`uses` 0/156 — "deliberately not emitted"** (`EVIDENCE` D2, L44); the gate asserts only node≥80% + structural≥70%. §E (L95–97) defines done as *"every row ✅ … shows no ❌."* These cannot both hold: graphify emits 156 `calls`/`uses` edges per the httpx corpus that habitat-graph deliberately does not. **"Full parity" is unreachable** under the current extractor contract, and §E overstates the achievable state (≥80% node parity ≠ "no ❌"). Worse: PC `--mode deep` (C1, L21) *adds heuristic `uses`/`references` edges* — exactly the excluded class — yet PC defines **no deep-mode parity gate** (L58 gates update-idempotency/watch/hook/MCP-config only). **Risk:** the DoD is internally inconsistent; a reader (or Luke) reads "no ❌" as byte-parity, which was never the goal and is not delivered.

### P1-G3 — No performance or scale budget anywhere in PA→PE **[HIGH]**
The plan cites rayon (L37) and "speed over 500K LOC" (exemplar map L74) but sets **no extraction-time budget, no memory budget, no graph.json size cap, no `--update` latency target.** The daemon holds the whole graph as `Arc<Graph>` (`EVIDENCE` D5, L60) and label lookup is `find_by_label` = **case-insensitive substring scan = O(n) per query** (`EVIDENCE` D5, L59). At a polyglot 500K-LOC corpus × 11 grammars, both the in-memory footprint and the per-query scan are unbudgeted. PD's "token benchmark" (A4) measures *query-token* efficiency, not engine perf — the actual scale risk is unmeasured. **Risk:** the organ is built, passes parity, then falls over (OOM or unacceptable latency) on the first real factory-scale repo.

### P1-G4 — Tree-sitter grammar ABI version skew is treated as routine dependency vetting **[HIGH]**
L79–80 lists the new deps ("all mature; gate each on add"). But 11 `tree-sitter-<lang>` crates pinned simultaneously is the single biggest *integration* risk in PA: grammar crates routinely require **incompatible `tree-sitter` core ABI versions (13/14/15)**, and `cargo` permits only one core version in the tree. The plan has no ABI-compatibility matrix, no pinned-core strategy. Also unnamed: `git2`→libgit2 (system C lib), `pdf-extract`/`lopdf` (recurring RUSTSEC advisories), `reqwest` (large transitive surface). **Risk:** PA stalls in dependency hell that `cargo-deny` does not catch (deny checks licenses/advisories, not ABI), invalidating the LARGE sizing.

### P1-G5 — `--update` is advertised incremental but the analyze stage is global (the deferred salsa warm-DB) **[MED-HIGH]**
C2 wires `cache::partition` + `build::merge` for incremental `--update` (L22, L58 "update-merge idempotent"). But the cache is **file-extraction-level only**; the downstream **`analyze` stage (Leiden community detection + centrality) is global** and re-runs over the whole graph on every update (Leiden is not incremental). The plan never acknowledges that analyze, not extraction, is the `--update` bottleneck, and the "salsa warm-DB" is deferred with no placeholder. **Risk:** `--update` claims sub-second incrementality but pays full Leiden cost each call; on a large graph every `--update`/`--watch`/`hook` rebuild is O(whole-graph) community detection.

### P1-G6 — Daemon/MCP cache-invalidation is undefined when `--update`/`--watch` rewrites graph.json **[HIGH]**
The MCP organ is `handle_jsonrpc(&Graph, …)` over a loaded graph (`EVIDENCE` D5.5); the daemon holds `Arc<Graph>` (D5). PC adds `--watch` (C3) and `--update` (C2) that **mutate graph.json on disk**, but nothing defines how a live daemon/MCP server picks up the change. No reload signal, no atomic `Arc` swap (e.g. `arc-swap`), no staleness contract. **Risk:** `--watch` rewrites the graph while the daemon serves an old `Arc<Graph>` → agents silently query a stale graph with no invalidation. This is a real concurrency defect introduced by PC, against the surface (MCP) the actual consumer uses.

### P1-G7 — New input surfaces (pdf / `add <URL>` / vision) ship without resource, SSRF, or injection limits **[HIGH]**
The guard crate handles Trojan-Source/bidi on *labels* and validates URL scheme/creds/control/bidi (`EVIDENCE` D1, L36) — but the **new** PE/PC surfaces are under-guarded:
- **PDF (S2, L32):** `pdf-extract`/`lopdf` are classic DoS surfaces (decompression bombs, malformed-object OOM). `forbid(unsafe)` does not stop a pure-Rust parser allocating gigabytes. No size cap / timeout / resource limit named.
- **`add <URL>` (C6, L26):** says "size/timeout caps" but **no SSRF guard**. In an agent factory the *agent* chooses the URL — `add http://localhost:8125/…` (POVM), `add http://169.254.169.254/…` (cloud metadata) reaches into the service mesh. EVIDENCE's url guard blocks scheme/creds/control/bidi, **not** private-IP/loopback/metadata.
- **Vision (S3, L33):** image bytes are untrusted input forwarded to a routing model; prompt-injection-via-image (manipulating TIERWRIGHT's model) is unconsidered.

**Risk:** an agent-reachable path from a code-knowledge tool into the live service mesh and the model router.

### P1-G8 — `install` mutates the user's Claude Code MCP config with no write-safety discipline **[MED]**
C5 (L25) writes the Claude Code MCP config so the organ auto-mounts. This mutates `~/.claude.json`-class state with **no backup, no idempotency, no merge-vs-overwrite story** — in stark contrast to the habitat's own "snapshot → write → read-back" memory-write discipline (CLAUDE.local.md §5). **Risk:** a re-run or a pre-existing entry corrupts the user's MCP config.

### P1-G9 — Mis-sequence: `install`/MCP-registration (the consumer's front door) is buried in PC, behind 11 grammars + 4 exporters **[MED-HIGH]**
The single feature that makes the organ reachable by the agent fleet — `install` registering the MCP organ (C5) — sits in **phase 3 of 5**, gated behind all of PA+PB. The MCP organ is already `LIVE PROVEN` (`EVIDENCE` D5.5). Sequencing its registration after the entire human-feature fan-out inverts value-order *even within Frame A*. **Risk:** months of grammar/exporter work ship before the factory can actually mount the organ.

### P1-G10 — `--watch` (C3) and `hook install` (C4) can double-fire with no cross-mechanism lock **[MED]**
Both trigger rebuilds; a commit fires the git hook **and** `notify` sees the same file writes → concurrent rebuilds racing on the graph.json write. L58 gates only intra-watch debounce, not watch×hook interaction. **Risk:** corrupted/partial graph.json under concurrent rebuild.

### P1-G11 — PD analytics asserts deterministic gate AND LLM-augmentation for the same feature **[MED]**
A3 "suggested questions … heuristic, **LLM-augmented via Backend**" (L29), gate = *"deterministic outputs"* (L64). LLM-augmented ≠ deterministic. The plan never resolves how the gate stays green when the Backend is live. (This is also a Frame-B seam — see PASS 2.)

### P1-G12 — No rollback / schema-versioning for graph.json as the node taxonomy grows **[MED]**
PA adds new node kinds (PHP traits, Kotlin objects, Scala givens, C macros…) — i.e. graph.json's schema evolves — yet the node-link envelope has **no schema version**, and `--update` re-interns by-label (`EVIDENCE` D3 merge). An `--update` against a graph.json built by an older hg (different taxonomy) silently merges incompatible schemas. R2 "byte-compat with graphify" pins to a graphify snapshot fetched 2026-06-28 with **no recorded version**; if upstream changes, the parity gate breaks unattributably. No rollback procedure for a regressing grammar in a deployed graph.json. **Risk:** silent schema drift across versions; un-pinned oracle.

---

## PASS 2 — Non-anthropocentric gap analysis (the frame the plan didn't take)

*Re-written from the frame the plan refused: **habitat-graph's consumer is the non-anthropocentric agent factory**, not a human developer. The success metric is substrate fitness — does the organ serve the orchestrator/fleet/loops/memory cheaply, coherently, and with trust signal — not feature-sameness with a tool humans operate.*

### 2A. Anthropocentric vanity — parity features the agent substrate does NOT consume

These cost real effort to chase graphify-parity, and **no factory consumer reads their output.**

- **F-V1 [B] `--svg` (X1, PB, MED).** An SVG is a vector image for human eyes. No factory consumer (orchestrator, arc-coherence gauge, PV2, fleet) consumes SVG; agents read graph.json / query MCP. *Pure vanity for the factory.* **Confidence 0.95.**
- **F-V2 [B] `--graphml` / `--neo4j`-cypher (X2/X3, PB).** Exports to Gephi/yEd (human GUI graph tools) and Neo4j — **none of which the 20-service factory runs** (no graph DB in the stack). Vanity unless/until a graph DB is deployed. **Confidence 0.8.**
- **F-V3 [B] `--wiki` (X4, PB, MED).** Marketed "agent-crawlable wiki + index.md" (L20) — but an agent does **not crawl a browsable wiki**; it queries MCP with a scope and gets a subgraph (cheaper, typed). The wiki is a *human* navigation artifact. (The obsidian exporter, already DONE, has genuine dual value — it feeds `hmem`/FTS5 recall agents use — but the standalone wiki is human-frame.) **Confidence 0.85.**
- **F-V4 [B] `suggested questions` (A3, PD).** graphify proposes 4–5 questions *for a human reader*. An agent generates its own queries from its task; it does not need canned questions. Decoration. (The hub/bridge analysis *under* A3 has agent value — see 2C-F8.) **Confidence 0.85.**
- **F-V5 [B] `explain "<concept>"` (S4, PE, MED).** "LLM node explain" → human-readable prose. **The agent IS an LLM**; spending a TIERWRIGHT round-trip + tokens to pre-chew a subgraph into prose, for a consumer that reads the structured subgraph faster, is not merely vanity — it is *negative value* (see 2C-F12). **Confidence 0.9.**
- **F-V6 [B] graph.html viewer (DONE).** Vendored cytoscape JS for human interactive viz (exemplar map L70). Sunk, but flagged: every `--watch`/`--update` cycle that regenerates it spends budget no agent reclaims (2C-F13). **Confidence 0.9.**

*Dominant frame missed all six because:* the plan's success metric is "match graphify's command surface" (§E), and graphify is a human tool — so its human-only outputs are imported as obligations without asking who in the factory reads them.

### 2B. What the agent substrate NEEDS that graphify-parity OMITS ENTIRELY

The §A gap matrix has **zero rows** for these — structurally, because graphify (a human tool) has no equivalent, so a parity-driven plan *cannot surface them.* These are the real backlog.

- **F1 [B] MCP graph-as-RESOURCE + resource-templates.** *Observation:* the MCP organ exposes only **tools** (`graph_query/path/health`, `EVIDENCE` D5.5). MCP also has **resources** (`resources/list`/`read`, templates like `habitat-graph://node/{label}`, `://community/{id}`, `://report`) — the native way an agent pulls a subgraph into context *without* a tool round-trip. *Missed because:* graphify's MCP surface is tools-only; parity copies it. *Risk:* agents pay a tool call (and a planning step) for context they could address directly; the organ is a worse MCP citizen than it could be at near-zero cost (data + handler exist). *Recommendation:* add resources + templates as the first new MCP surface. **Confidence 0.85.**
- **F2 [B] Token-budgeted context selection (serve-side).** *Observation:* PD's A4 *measures* full-vs-subgraph tokens (L30) for a human to admire — but never becomes an **actuator**. The agent's defining constraint is a finite window; the highest-value organ behavior is `graph_query(scope, max_tokens=K) → greedily-packed most-relevant subgraph that fits K`. *Missed because:* graphify benchmarks tokens as a vanity stat (its user is a human reading a report), so parity ports the measurement, not the control. *Risk:* the one token-aware feature is inverted into a stat instead of the single most valuable agent capability. *Recommendation:* transform A4 from benchmark into a serve-side budget on MCP `graph_query`. **Confidence 0.9.**
- **F3 [B] Warm retrieval index (kill the O(n) substring scan).** *Observation:* `find_by_label` is a linear substring scan (`EVIDENCE` D5); the "salsa warm-DB" is deferred. Agents query in tight loops (orchestrator mission-decomposition → many `map.scope` calls). *Missed because:* a human runs graphify once and browses; latency never mattered. *Risk:* at factory query rates over a large graph, O(n)/query is the latency wall; the organ is too slow to sit in an agent loop. *Recommendation:* inverted/trigram label index; this is the deferred warm-DB reframed as the agent-latency fix. **Confidence 0.85.**
- **F4 [B] Embedding / semantic retrieval for agents.** *Observation:* agent retrieval is semantic ("code related to consensus quorum"), not substring. S1's semantic path (PE) wires a Backend for **extraction** (prose→nodes), never for **retrieval** — no vector index, no `query_semantic(embedding)→nearest`. The factory's memory substrates (POVM Hebbian, Reasoning Memory, VMS) are all associative; habitat-graph offers none. *Missed because:* graphify's semantic layer is about ingesting docs, not serving semantic neighbors to a machine. *Risk:* agents get lexical matches where they need conceptual neighbors; the organ can't answer the queries agents actually ask. *Recommendation:* a vector index over labels/docstrings; `query_semantic` MCP tool. **Confidence 0.8.**
- **F5 [B] Structure → POVM-pathway SEEDING (write-direction).** *Observation:* `02_HABITAT_INTEGRATION` §2 reinforces POVM on agent *query* (reactive read-path). The deeper value: the graph's **structure** (intra-community co-occurrence, high-weight edges) should **seed** POVM pathways as a Hebbian prior. None of PA→PE carries *any* §2 wiring — the parity plan adds zero substrate-feeding. *Missed because:* graphify has no cognitive substrate to feed. *Risk:* habitat-graph stays a read-only oracle; the substrate never metabolizes its structure; the "learning graph" thesis (`02` §2) is never realized. *Recommendation:* a structure→POVM seeding pass as a first-class phase. **Confidence 0.75.**
- **F6 [B] arc-graph severed-ear as CONTINUOUS factory telemetry.** *Observation:* D6 built `arc_graph` + `SeveredEarReport{present,severed,coherence}` (`EVIDENCE` D6, L70) — **the organ's killer app**: it auto-detects the severed bidi-wiring arcs the live S1008620 work hand-builds (`arc-coherence-gauge.sh`). But PA→PE advance it **not at all**; it's frozen as a one-shot extractor. *Missed because:* graphify has no factory wiring to witness, so parity has no row for it. *Risk:* the single highest-leverage substrate feature is left dormant while 5 phases chase human features; the bidi-wiring loop keeps hand-building what the organ could serve. *Recommendation:* promote arc-graph to a served, push-on-rebuild telemetry stream feeding `arc-coherence-gauge` + the orchestrator + injection.db. **Confidence 0.9.**
- **F7 [B] Graph deltas pushed to the cognitive loops (PV2/ORAC).** *Observation:* `02` §5 registers Leiden communities as PV2 spheres **once**. The agent-substrate need is **continuous**: on a source change, compute the graph delta and **push** it to PV2 (sphere topology update) + injection.db ("architecture changed") + arc-coherence. graphify's `--watch` (C3) detects change and rebuilds graph.json *for a human to re-browse*. *Missed because:* the human framing of `--watch` is "refresh the artifact," not "update the substrate's nervous system." *Risk:* PC builds a watcher whose only consumer is the human artifacts; the loops never learn that the architecture moved. *Recommendation:* re-target `--watch` from artifact-refresh to delta-push into PV2/POVM/arc-coherence. **Confidence 0.8.**
- **F8 [B] Cross-community/centrality analysis re-aimed at arc-graph, not a human report.** *Observation:* A2 "surprising connections" + A1 "god nodes" compute exactly the cross-community high-weight edges and high-degree hubs that **severed-ear/bridge analysis consumes** — but the plan emits them as a human report section (god nodes, "surprising connections"). *Missed because:* the output is framed for a reader. *Risk:* the analysis is built then thrown at human eyes instead of feeding arc-graph/orchestrator. *Recommendation:* keep the computation, re-target the sink to arc-graph telemetry + the `bridge-contract`/`schema-drift` skills. **Confidence 0.8.**
- **F9 [B] Stable node-IDENTITY contract for agent-cache/POVM-key/sphere-id coherence.** *Observation:* determinism (R4) is justified by the **git merge driver** (L24, CLAUDE.md invariant) — the human/git frame. The stronger reason: **agents cache, POVM pathways key on node-pairs, PV2 sphere ids derive from community ids.** If `--update` re-interns labels→NodeId by-label (`EVIDENCE` D3 merge) and a function is renamed, the NodeId changes and every downstream cache key / pathway / sphere goes incoherent with no migration. D6 already solved the *sphere*-id naming trap (`EVIDENCE` L73) — proving the team sees the issue — but never generalized it to a node-identity contract. *Missed because:* sorted-output determinism (enough for git) is mistaken for identity stability (needed for caches). *Risk:* every re-extraction silently invalidates agent caches, POVM keys, and sphere mappings on any rename. *Recommendation:* content-addressed stable node IDs that survive `--update`/rename; gate with a rename-stability test. **Confidence 0.85.**
- **F10 [B] Confidence as an agent DECISION filter, served on the query surface.** *Observation:* the Confidence enum is "the trust signal the whole graph rests on" (exemplar map L51) — but it's served as a *tag for a human to eyeball*. The agent need: MCP `graph_query` should **filter by confidence** ("EXTRACTED-only for a load-bearing wiring decision") and responses should carry a confidence breakdown. *Missed because:* graphify shows confidence to a reader, doesn't gate machine decisions on it. *Risk:* (compounded by C1 below) agents make wiring/decomposition decisions on a mix of EXTRACTED and AMBIGUOUS edges with no filter. *Recommendation:* confidence filter + breakdown on the MCP surface. **Confidence 0.85.**
- **F11 [B] Daemon backpressure / fail-soft contract under concurrent rebuild.** *Observation:* the factory discipline is fail-soft (POVM is "fail-soft secondary") and witness-filter-then-cap (MEMORY.md). The plan never defines what a concurrent agent `graph_query` gets while the daemon is mid-`--update`: stale? error? block? *Missed because:* a human re-runs graphify sequentially; concurrency with a live consumer is a substrate-only concern. *Risk:* agents in a tight loop hit a rebuilding daemon and get undefined behavior. *Recommendation:* serve-stale-with-staleness-header (witness pattern); defined fail-soft contract. **Confidence 0.75.**

### 2C. Where chasing the human exemplar ACTIVELY DEGRADES agent-substrate fit (second-order)

- **F12 [B] `--mode deep` (C1) poisons the structures the substrate consumes.** *Observation:* for a human, more INFERRED edges = serendipity; for the substrate, a flood of the lowest-confidence `uses`/`references` edges **inflates degree, merges Leiden communities that should be distinct (community detection is edge-density-driven), corrupts the "god nodes" centrality (A1), and distorts the community→PV2-sphere topology (`02` §5) and arc-graph.** *Risk:* enabling deep mode to chase parity degrades three agent-frame organs at once (communities, centrality, arc-graph) with no confidence-filter to protect them. *Recommendation:* gate deep-mode edges OUT of analyze/sphere/arc-graph; serve them only on explicit human-export. **Confidence 0.85.**
- **F13 [B] Human-artifact regeneration competes with agent-query freshness for the rebuild budget.** *Observation:* if `--watch`/`--update` regenerate html+svg+wiki on every change to keep human artifacts fresh, that I/O+CPU raises rebuild latency — which directly widens the **staleness window** the agent queries against. The plan never separates "agent-critical rebuild (graph.json + index)" from "human-artifact rebuild (html/svg/wiki)." *Risk:* slow vanity artifacts gate the fast agent path; the substrate is stale *because* the human SVG is being redrawn. *Recommendation:* split the rebuild pipeline; agent-critical path first and independently, human artifacts lazy/on-demand. **Confidence 0.8.**
- **F14 [B] The parity DoD (§E) is a roadmap that structurally cannot reach the real consumer.** *Observation:* §E ties "done" to "every graphify row ✅." Engineering capacity is therefore consumed by 11 grammars + 4 exporters + explain/vision **before any agent-native feature (F1–F11) is even on the board** — none of them are graphify rows, so the DoD guarantees they never get prioritized. *Risk:* frame collapse becomes a backlog filter: the highest-leverage substrate features are permanently out-of-scope because the success metric is sameness-to-a-human-tool. *Recommendation:* replace the single parity DoD with a **conditional dual DoD** (see PASS 3); the parity DoD applies only if/when habitat-graph flips OSS-public (where human developers are the consumer). **Confidence 0.9.**
- **F15 [B] PA's 80–96% parity = silently-incomplete graphs fed to agents making load-bearing decisions.** *Observation:* the httpx precedent is 97% node / 96% structural / **0% calls-uses** parity; rolling that across 11 grammars feeds PV2 spheres, arc-graph, and orchestrator `map.scope` graphs that are 80–96% complete with **whole edge classes missing and no signal about which 4–20%.** For a human browsing, mostly-right is fine; for an agent answering "does mission X touch service Y?", a silently-incomplete graph yields **confidently-wrong** answers. *Risk:* breadth (human value) without a served completeness envelope (agent need) is a trust hazard that scales with every grammar added. *Recommendation:* serve a per-graph completeness/confidence envelope on every query (ties to F10). **Confidence 0.8.**

---

## Load-bearing tensions (Frame A vs Frame B — require explicit reconciliation)

- **T1 — deep mode.** Frame A: *add `--mode deep` for richer discovery* (C1). Frame B: *deep mode poisons communities, centrality, sphere topology, and arc-graph* (F12). **Reconcile:** build C1 only with a confidence gate that keeps INFERRED/AMBIGUOUS edges out of the analyze→sphere→arc-graph pipeline; expose them solely on human-export.
- **T2 — Definition of Done.** Frame A: *DoD = every graphify row ✅* (§E). Frame B: *DoD = agent-substrate fitness — token-budgeted serve, sub-second warm query, arc-graph telemetry live, semantic retrieval, identity coherence* (F1–F11). **Reconcile:** split into a **Factory-Organ DoD** (the live default) and an **OSS-Parity DoD** (optional, gated on the public-flip one-way door, `EVIDENCE` D7).
- **T3 — what determinism is for.** Frame A: *determinism for the git merge driver* (L24). Frame B: *determinism + stable identity for agent-cache / POVM-key / sphere-id coherence* (F9) — a strictly stronger requirement. **Reconcile:** specify content-addressed node IDs surviving rename, not merely sorted output; add a rename-stability gate.
- **T4 — front-door sequencing.** Frame A: *`install`/MCP-register mid-plan in PC* (C5). Frame B: *the MCP organ IS the consumer's front door — harden + resource it first* (F1/F2, P1-G9). **Reconcile:** pull `install` + MCP-resources + token-budget to Phase 0.

---

## Convergent findings (both frames produce the same recommendation — strongest signal)

- **C-1 [CONVERGENT, HIGH]** — the 11-grammar parity-corpus / golden-generation dependency must be made explicit and budgeted. *Frame A* (P1-G1): the parity gate is meaningless without per-language goldens + a pinned oracle. *Frame B* (F15): agents need a per-language completeness envelope. Both → source/commit 11 corpora, pin the graphify oracle version, emit a completeness envelope per graph.
- **C-2 [CONVERGENT, HIGH]** — pdf/url/vision ingest needs hard resource + SSRF + injection limits *before* PE/C6. *Frame A* (P1-G7): security/supply-chain hygiene. *Frame B*: the *agent* chooses these inputs at runtime, making it an agent-reachable path into the service mesh and model router. Both → caps/timeouts/private-IP-block/sandbox as a ship-gate.
- **C-3 [CONVERGENT, HIGH]** — the daemon needs atomic reload + cache-invalidation when `--update`/`--watch` rewrites graph.json. *Frame A* (P1-G6): stale serve = correctness bug. *Frame B* (F11): agent-cache coherence + warm serving. Both → `arc-swap` atomic reload + staleness header.
- **C-4 [CONVERGENT, MED-HIGH]** — `--update` is not actually incremental (global Leiden). *Frame A* (P1-G5): the deferred salsa warm-DB / perf gap. *Frame B* (F3): sub-second agent query needs a warm index. Both → incremental analyze or a warm retrieval index; stop pretending file-cache = incremental.

---

## PASS 3 — Recommendations (reconciliation + re-prioritization)

### Phase-by-phase verdict (which frame each phase serves)

| Phase | Item | Serves | Verdict |
|---|---|---|---|
| PA | 11 grammars + doc nodes | **BOTH** (partial) | **KEEP but TRIM** to factory languages (TS/JS, then Go); the factory has **no Scala/PHP/Ruby/C#/Kotlin services** — ~7 of 11 grammars are OSS-audience breadth, defer to the public-flip backlog. Doc nodes (L2) feed retrieval → high agent value, keep. Add golden-corpus pre-task + completeness envelope (C-1, F15). |
| PB | svg / graphml / cypher / wiki | **Frame A only (VANITY)** | **DROP from the factory roadmap** (F-V1/V2/V3). Move to the optional OSS-Parity backlog. Obsidian export (DONE) is the only dual-value piece. |
| PC | `install` (MCP register) | **BOTH (front door)** | **PULL FORWARD to Phase 0** (P1-G9, T4) + add config write-safety (P1-G8). |
| PC | `--update` + merge | **BOTH** | **KEEP**, but fix incremental-analyze (C-4) + daemon reload (C-3) + schema-versioning (P1-G12) + stable IDs (F9). |
| PC | `--watch` | **re-frame B** | **KEEP, re-target** from artifact-refresh to delta-push into PV2/POVM/arc-coherence (F7); split rebuild pipeline (F13); lock vs hook (P1-G10). |
| PC | `hook install` | **BOTH-ish** | KEEP, low priority. |
| PC | `add <URL>` | **Frame A; low factory value** | **DEFER + HARDEN** (SSRF, C-2). The factory corpus is local source, not remote papers. |
| PC | `--mode deep` | **ACTIVELY HARMFUL** | **DROP from the agent path** (T1, F12); if built, confidence-gate its edges out of analyze/sphere/arc-graph. |
| PD | god nodes (A1) | **BOTH** | KEEP (already computed); also feed arc-graph (F8). |
| PD | surprising connections (A2) | **analysis BOTH / presentation vanity** | KEEP the computation, **re-target** to arc-graph telemetry, drop the "surprising" report framing (F8). |
| PD | suggested questions (A3) | **Frame A only (VANITY)** | **DROP** (F-V4). |
| PD | token benchmark (A4) | **invert to BOTH** | **TRANSFORM** from measurement into the serve-side token budget (F2) — highest-leverage reframe in the plan. |
| PE | semantic extraction (S1) | **BOTH (partial)** | KEEP, but pivot toward **retrieval embeddings** (F4), not just extraction. |
| PE | pdf (S2) | **low factory value + high risk** | **DEFER + HARDEN** (C-2). |
| PE | vision (S3) | **lowest value, highest effort, decision-blocked, TIERWRIGHT-hostage** | **DROP/DEFER** (P1-G... DoD hostage; F-V… effort). |
| PE | explain (S4) | **Frame A only (VANITY + wasteful)** | **DROP** (F-V5, F12-class waste — the agent is the LLM). |

### The highest-leverage agent-substrate features the plan is MISSING ENTIRELY (the real backlog)

1. **AGT-1 — MCP graph-as-resource + resource-templates** (F1). Smallest effort, highest leverage; data + handler already exist.
2. **AGT-2 — Token-budgeted serve** (`graph_query(scope, max_tokens=K)`, F2). Transform A4 from stat to actuator. The single most valuable agent capability.
3. **AGT-3 — Warm retrieval index** (inverted/trigram + optional embedding, F3/F4). The deferred "salsa warm-DB" reframed as the agent-latency fix. Kills the O(n) substring scan.
4. **AGT-4 — arc-graph as continuous telemetry** (F6). Promote D6's one-shot `SeveredEarReport` to a served, push-on-rebuild stream feeding `arc-coherence-gauge` + orchestrator + injection.db. The organ's killer app for S1008620.
5. **AGT-5 — Stable node-identity contract** (F9). Content-addressed IDs surviving `--update`/rename; rename-stability gate. Keeps POVM keys / sphere ids / agent caches coherent.
6. **AGT-6 — Completeness/confidence envelope + confidence filter on the query surface** (F10/F15). Turns PA's silent 80–96% incompleteness into an explicit agent trust signal.
7. **AGT-7 — Daemon atomic reload + fail-soft backpressure contract** (F11, C-3). `arc-swap` + staleness header (witness-filter-then-cap).

### Proposed re-sequence

- **Phase 0 (NEW — highest leverage / smallest effort):** AGT-1 (MCP resources) + AGT-2 (token-budget serve) + AGT-7 (daemon reload) + `install` pulled from PC (with write-safety). *Make the existing, already-built organ a first-class agent consumer surface before adding one new human feature.*
- **Phase 1:** AGT-3 (warm index) + AGT-5 (stable IDs) + AGT-6 (completeness envelope) + fix `--update` incrementality (C-4) + schema-versioning.
- **Phase 2:** AGT-4 (arc-graph telemetry) + re-targeted `--watch` delta-push (F7) + god-nodes/cross-community analysis re-aimed at arc-graph (F8). *This is the direct pay-in to the live S1008620 bidi-wiring loop.*
- **Phase 3:** PA **trimmed** to factory languages (TS/JS, then Go) + golden corpus + completeness envelope. Doc-nodes (L2) for retrieval.
- **Phase 4 (OPTIONAL — gated on the OSS-public flip, `EVIDENCE` D7):** the OSS-Parity vanity backlog — PB exporters, the remaining 7 grammars, explain, suggested-questions, pdf/vision. **The parity DoD (§E) is the correct DoD only here**, because only an OSS-public habitat-graph has human developers as its consumer.

### The pivot the plan must name

The §C-deferred **"OSS/public flip"** (one-way door, Luke @ 0.A, `EVIDENCE` D7) is not a release detail — it is the **fork that decides which DoD applies.** As a private factory organ, the right DoD is **agent-substrate fitness** (Factory-Organ DoD: Phase 0–2 above). As an OSS public tool, the graphify parity DoD (§E) becomes meaningful. The plan currently assumes the OSS DoD by default while the organ is private — that is the frame collapse in one sentence. **Recommendation:** make §E conditional on the public-flip decision and adopt the Factory-Organ DoD as the live default.

---

## Frame-call confidence & caveats

- **Frame call (A dominant, not balanced): 0.92.** The phases and DoD are unambiguously Frame A; the Frame-B material is peripheral and unadvanced by any phase.
- **PASS 1 findings: 0.85** (cite plan/EVIDENCE lines directly; P1-G1/G2/G3/G4/G6/G7 are HIGH).
- **PASS 2 findings: 0.8** — the omissions (F1–F11) are inferred from the live topology + `02_HABITAT_INTEGRATION`'s own stated intent (POVM seeding, PV2 spheres, arc-graph) that PA→PE never advance; this is warranted, not speculative.
- **Caveat:** the organ is genuinely strong on the Frame-A axis already (1242 gate-green tests, determinism, parity-transparent cache, the security guard, D6 habitat wiring built). The critique is **not** "the build is wrong" — it is "the *next-5-phases plan* points at the wrong consumer." D6 already contains the seeds (arc-graph, POVM writer, PV2 registrar) the parity plan then ignores.

---
*Two-frame gap analysis authored S1008796 · na-gap-analyst @ cortex. Second pass of `08_GRAPHIFY_PARITY_PLAN`. Both passes are the plan (CLAUDE.local.md §3).*
