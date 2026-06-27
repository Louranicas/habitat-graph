> Back to: [[CLAUDE.md]] · [[CLAUDE.local.md]] · [[habitat-graph/README]] · spine: [[00_DEPLOYMENT_PLAN]] · framework: [[DEPLOYMENT_FRAMEWORK]] · [[MODULE_STRUCTURE_PLAN]]

# habitat-graph — Caching, Incremental Computation & Cluster Architecture (ADR-04, S1008796)

**Question (Luke):** is it worthwhile integrating a cache into each crate? Or some kind of cluster
setup? **Status: PROPOSED** (planning; ADR for the ~11-crate decomposition).

---

## TL;DR (the answer up front)

- **No — do not put a cache in each crate.** That optimizes the wrong axis: it creates N independent
  invalidation truths (cache-coherence hell), violates the *core-is-vocabulary* rule, and bloats
  memory with overlapping copies. It is the central-tendency move, and it is a known anti-pattern.
- **Yes — integrate ONE incremental-computation substrate** that every stage keys into: a single
  content-addressed store (blake3) + a demand-driven memoized query engine. Each *stage* is
  cacheable; the cache *lives in one crate*. This is the rust-analyzer / Bazel / Turborepo model.
- **The "cluster" you actually want is a daemon with a shared warm cache** (the rust-analyzer LSP
  architecture), not multi-machine compute. CLI · MCP · watch · the orchestrator plugin become thin
  clients of one long-running process holding the graph + query DB. `rayon` covers intra-machine
  parallelism; true multi-machine clustering is overkill for a single 500K-LOC corpus — defer it.
- **Frontier lane (opt-in, top ~1%):** maintain the graph algorithms *incrementally* under edit
  deltas via differential-dataflow / DBSP — the arc-graph and Leiden communities update in
  microseconds per commit instead of a full rebuild. High novelty, real risk; feature-gated, D8.

This is not gold-plating: the framework already commits to an **SLO of <2s incremental rebuild** and
a **git post-commit hook**. Neither is physically achievable on 500K LOC without incrementality.

---

## 1. Why "a cache per crate" is the wrong shape (principles)

| Principle | Per-crate caches violate it | Correct |
|---|---|---|
| **Single source of invalidation** | N caches → N truths; a change in `extract` must invalidate `build`/`analyze` caches it can't see | one keyed store; invalidation is a function of input hashes |
| **Core is vocabulary, not behaviour** (Design Rule 1) | a cache is mutable state + I/O; putting it in `core` (or each crate) makes every crate stateful | the cache is its OWN crate, depended on, depending only on `core` |
| **Acyclic inward flow** (Rule 4) | cross-crate cache lookups invite back-edges | the store sits below the feature crates; flow stays inward |
| **Determinism / merge-driver** (R4) | ad-hoc caches drift from content | content-addressed keys make cache hits *provably* equal to recompute |
| **DRY / one reason to change** (Rule 3) | cache logic duplicated ×11 | one cache crate, one reason to change |

The unifying point: **caching is not a per-module concern, it is a cross-cutting substrate.** You
don't give each function its own database; you give the pipeline one incremental engine and express
each stage as a query against it.

## 2. The right shape: incremental computation over a content-addressed store

Two layers, one crate (`habitat-graph-cache`, see §6):

1. **Content-addressed store (CAS).** Key = `blake3(stage_id ‖ input_hash ‖ config_hash)` → value =
   serialized stage output. Deterministic builds (R4) make a hit *equal* to recompute. This is
   Bazel/Buck2's action cache and Nix's derivation model, scaled to one box.
2. **Demand-driven memoized queries.** The pipeline stops being a push chain
   (`detect→extract→build→…`) and becomes a *pull* graph: `query("path A B")` computes only the
   subgraph it needs, reusing memoized stage results whose inputs are unchanged. This is salsa
   (rust-analyzer), inspired by Adapton's self-adjusting computation.

Cache granularity = **per file → per stage**, not per crate. Change one `.rs` file and only its
extraction + the communities/centrality that transitively touch it recompute. That is the whole game.

## 3. The state of the art (the distribution — central tendency → frontier)

| Tier | Approach | Exemplar | Fit for habitat-graph |
|---|---|---|---|
| **Central tendency** | per-stage `HashMap` caches, manual invalidation | most CLIs | **REJECT** — the question's premise |
| **Top quartile (proven)** | blake3 CAS + content-hash task graph | Bazel, Buck2, Turborepo, Nx | **adopt for P1–P3** — simple, deterministic, parity-safe |
| **Top ~7% (proven, invasive)** | salsa demand-driven memoized queries + **daemon with warm DB** | **rust-analyzer**, rustc query system | **adopt at D5/D6** — the interactive/daemon stage |
| **Frontier (top ~1%, novel, risk)** | incremental maintenance of graph algorithms under deltas | **differential-dataflow / timely** (McSherry), **DBSP/Feldera** (2025, verifiable), **FlowLog** (Nov 2025, incremental Datalog) | **feature-gated D8 lane** — arc-graph + Leiden as live incremental views |

**Why the phasing, not all-at-once:** salsa is *powerful but invasive* — stages become tracked
functions, threading a `&dyn Db` through every crate's API (not its logic). The pragmatic top-tail
path: start with **coarse content-addressed memoization** (non-invasive, parity-safe) in P1–P3,
**design stage APIs to be salsa-compatible** (pure, hashable inputs), and **adopt salsa when the
daemon lands** (D5/D6) and fine-grained interactivity actually pays. The frontier (differential
dataflow) is a *separate, opt-in* lane — never on the critical path to v1.

## 4. The "cluster" question — disambiguated

"Cluster" means three different things; only two are worth doing:

- **(a) Graph clustering** — already solved: Leiden community detection (`analyze::cluster`,
  `network_partitions`). Nothing to add.
- **(b) Compute clustering, intra-machine** — `rayon` data-parallel extraction over files (already
  in the plan). This is the *right* parallelism for one repo. The Python GIL never had it; Rust does.
- **(b′) Compute clustering, multi-machine** — **defer.** A 500K-LOC corpus fits in RAM on one box;
  distributed compute adds coordination cost with no payoff until corpora are 100×. The CAS is
  *remote-cache-ready* (Bazel-style) if that day comes — design the key scheme now, build it never.
- **(c) Dimensional execution lanes** (the deep-diff-forge `cluster` crate pattern) — run independent
  analysis *dimensions* (centrality, communities, patterns, arc-graph) as parallel lanes with
  explicit join policies. **Adopt a lite version**: the daemon schedules stage queries concurrently;
  lanes that miss cache run on the rayon pool. This is the honest, useful reading of "cluster setup"
  for this tool.

## 5. Habitat-native novelty (genuinely outside central tendency)

Three syntheses that exist *because* this is a factory organ, not a generic tool — the top-7%
novelty the brief asks for:

1. **POVM-weighted cache retention.** Replace LRU eviction with **Hebbian co-activation weight**:
   subgraphs the fleet actually queries together (POVM pathways, §02 `memory`) stay warm; cold
   regions evict first. The cache learns the working set from real query behaviour instead of
   guessing by recency. This is a habitat-native eviction policy with no off-the-shelf equivalent.
2. **Daemon = morphd-shaped.** The warm-DB daemon mirrors the habitat's existing `morphd` pattern
   (UDS at `$XDG_RUNTIME_DIR/habitat-graph/`, 0o600). One process holds the salsa DB; the CLI, MCP
   server, `watch`, and the **orchestrator plugin** are all thin clients of one warm graph — the
   plugin's `map.scope` query hits a hot cache, not a cold rebuild.
3. **Arc-graph as a live incremental view.** The S1008620 bidi-wiring producer→consumer map becomes
   a differential-dataflow *materialized view*: as commits land, severed-ear detection updates in
   microseconds rather than a batch re-scan. The arc-coherence gauge reads a live view, not a
   nightly job. (Frontier lane — high novelty, gated.)

## 6. Impact on the ~11-crate decomposition (minimal, additive)

Add **one** crate; do **not** touch the others' internals (only their stage-function signatures,
which become hashable-input pure functions — already the goal under R4 determinism).

```text
crates/
  habitat-graph-core/      # (unchanged) vocabulary + guard
  habitat-graph-cache/     # NEW — CAS (blake3) + memoized query engine; depends ONLY on core
  habitat-graph-source/    # now keys into cache
  habitat-graph-extract/   #  "
  habitat-graph-build/     #  "
  habitat-graph-analyze/   #  "
  habitat-graph-export/    # (reads memoized graph)
  habitat-graph-daemon/    # NEW — warm-DB host process (salsa DB + UDS, morphd-shaped); the "cluster"
  habitat-graph-serve/     # transport: rmcp MCP + axum /health + subscriptions (clients of the daemon)
  habitat-graph-cli/       # thin client (one-shot uses an ephemeral DB; long-running attaches to daemon)
  habitat-graph-habitat/   # POVM-weighted retention policy + arc-graph live view live HERE (additive)
  habitat-graph-fixtures/  # parity harness (cache MUST be transparent — hit == recompute, proven by parity)
```

So the workspace goes **11 → 13 crates** (`cache` + `daemon`). Dependency rule additions:
- `cache` depends only on `core` (never on a feature crate).
- `source`/`extract`/`build`/`analyze`/`serve`/`daemon` may depend on `cache`.
- `daemon` depends on `cache` + the engine crates; `serve` depends on `daemon` (transport over the warm DB); nothing depends on `daemon` except `serve`/`cli`'s attach path.
- `cache` is **transparent to parity**: a cache hit must be byte-identical to a recompute, asserted
  by the parity harness (Gate G5) — if caching ever changes output, that is a REGRESSION.
- The frontier differential-dataflow lane is a `--features incremental-view` opt-in inside
  `analyze` + `habitat`, never required for a green v1.

## 7. Decision (ADR)

- **Rejected:** a cache per crate (coherence, layering, DRY, memory).
- **Rejected:** a cache per crate (coherence, layering, DRY, memory).
- **Accepted:** a single `habitat-graph-cache` crate = content-addressed store + demand-driven
  memoization; stage outputs keyed on `blake3(stage ‖ inputs ‖ config)`; cache transparent to parity.
- **Accepted (committed §8.1):** **salsa** as the query engine; stages are tracked-functions from P1.
- **Accepted (committed §8.2):** **`habitat-graph-daemon`** as a first-class crate at D5 — the
  warm-DB "cluster" (rust-analyzer / morphd-shaped UDS); `rayon` intra-machine; multi-machine deferred.
- **Accepted (novel, §8.4):** POVM-weighted retention via an injected `RetentionPolicy` (core stays LRU).
- **Funded, gated (§8.3):** differential-dataflow / DBSP incremental views, first target live
  arc-coherence (D8 frontier lane, `--features incremental-view`).
- **Sequencing:** coarse CAS memoization (P0–P1) → salsa tracked-functions (P1+) → daemon (D5) →
  subscriptions+persistence (D6) → frontier lane (D8).

## 8. Decisions taken (S1008796 — capacity-maximizing; Claude @ cortex holds architecture carriage)

These four were mine to take (architecture). The *meta* go/no-go on the refactor and every live-
actuation arming (`factory.authorize.habitat-graph`) remain Luke @ 0.A — unchanged. Within that
envelope I take the highest-capacity path on each, because the daemon+salsa+incremental stack is the
one decision that multiplies the entire featureset rather than adding a single feature.

1. **Salsa — COMMITTED.** Adopt salsa (the rust-analyzer engine: demand-driven memoized queries,
   automatic dependency tracking, 2025 parallel-eval + persistence) as THE query engine, not
   hand-rolled CAS. Stage APIs are salsa tracked-functions from P1 (pure, hashable inputs); the
   coarse CAS is the P0/P1 fallback while the salsa DB comes online. *Why:* fine-grained
   incrementality + persistence is the foundation everything else stands on; deferring it forces a
   rewrite later.
2. **Daemon — FIRST-CLASS, from D5.** Promote the warm-DB daemon to its own crate
   (`habitat-graph-daemon`, §6) and build it at D5, not "later." *Why:* this is the capacity
   multiplier — it turns a batch CLI into a live server and is the precondition for subscriptions,
   persistence, and a shared warm graph across CLI · MCP · watch · orchestrator. The CLI keeps a
   one-shot ephemeral-DB path for scripts (no daemon required for correctness — Design Rule 6).
3. **Frontier lane — FUNDED (gated).** Commit the differential-dataflow incremental-view lane with a
   concrete first target: **live arc-coherence** (the S1008620 payoff). Feature `incremental-view`,
   prototyped right after P5, never on the v1 critical path. *Why:* highest novelty + it directly
   serves work in flight; gating contains the risk.
4. **POVM retention — INJECTED, core stays clean.** `cache::evict` exposes a `RetentionPolicy` trait
   (LRU default in the OSS core); the `PovmWeighted` policy is implemented in `habitat-graph-habitat`
   and injected at runtime. *Why:* keeps the OSS core substrate-independent AND keeps the novel
   Hebbian eviction — best of both.

### Capability expansion unlocked by these decisions (the featureset increase)

| New capability | Enabled by | Surface |
|---|---|---|
| Sub-second incremental queries on a warm graph | salsa + daemon | MCP/CLI `query`/`path`/`subgraph` |
| **Live subscriptions** — watch a query, get pushed deltas | daemon + salsa red-green | new MCP `subscribe` tool + `watch --query` |
| **Persistence** — graph survives restart, warm on boot | salsa durable incrementality | daemon state in `$XDG_STATE_HOME/habitat-graph/` |
| **Shared warm graph** — one hot DB, many clients | daemon (UDS, morphd-shaped) | CLI · MCP · watch · orchestrator pipe |
| **Live arc-coherence** — severed-ear deltas in µs | differential-dataflow lane | `arc_graph` materialized view → gauge |
| **Hebbian working-set retention** | POVM policy injection | habitat crate, runtime |

These are features a batch tool structurally cannot have. On the deployment maturity ladder: **D5 now
includes the daemon + subscriptions + persistence**; **D8 the frontier lane** — both recorded in
`DEPLOYMENT_FRAMEWORK.md`.

---

## Sources (current SOTA, fetched S1008796)

- [Salsa](https://salsa-rs.github.io/salsa/overview.html) · [rust-analyzer durable incrementality](https://rust-analyzer.github.io/blog/2023/07/24/durable-incrementality.html) — demand-driven memoized queries; 2025 work adds parallel eval + persistence.
- [differential-dataflow](https://github.com/TimelyDataflow/differential-dataflow) · [timely-dataflow](https://github.com/TimelyDataflow/timely-dataflow) (McSherry) — ~100,000× on incremental updates (15s → 230µs).
- [DBSP / Feldera](https://www.feldera.com/) — 2025 "database-grade, verifiable" incremental engine. · [FlowLog (arXiv 2511.00865, Nov 2025)](https://arxiv.org/abs/2511.00865) — incremental Datalog atop DD.
- Bazel/Buck2 action cache + CAS; Turborepo/Nx content-hash task graphs; Adapton (self-adjusting computation, Acar) — the proven content-addressed + incremental lineage.

*ADR-04 authored S1008796 · Claude @ cortex · gold standard = Louranicas/deep-diff-forge. Planning; no crate exists.*
