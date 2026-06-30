> Back to: [[CLAUDE.md]] · [[CLAUDE.local.md]] · [[habitat-graph/README]] · spine: [[00_DEPLOYMENT_PLAN]] · framework: [[DEPLOYMENT_FRAMEWORK]]

# habitat-graph — Habitat Integration Contract (S1008796)

How L8 `habitat` (feature-gated `--features habitat`) wires `habitat-graph` into the factory's
live systems. Every wire here is **additive** — the OSS core (L0–L7) compiles and runs without it.
This mirrors ORAC's bridge-client discipline: integration never becomes load-bearing on the core.

---

## 1. Service registration (devenv + health)

| Concern | Plan | Convention ref |
|---|---|---|
| **Port** | TBD — run the **`port-claim`** skill BEFORE writing any `[[services]]` entry. Do not hardcode (S1005032: two panes both grabbed 8141, a reserved port). | CLAUDE.md Habitat Ops |
| **devenv.toml** | `command = <abs path to release binary>`; batch order = late (depends on nothing, so batch 5+); `auto_start=false` until soak-proven | CLAUDE.local.md §2 binary-deploy |
| **Health** | `axum` `/health` → `{version, nodes, edges, last_build_ts, backend_mode}`; register path-map so **`cc-health`** (not hand-rolled curl) sees it | CLAUDE.md anti-patterns |
| **Binary deploy** | `/usr/bin/cp -f` to the `command=` path, then `devenv restart habitat-graph` (bare `cp`→`trash` alias no-ops) | feedback_binary_deployment |

---

## 2. Memory substrates (the L8 `memory` module)

The graph becomes a **learning** substrate, not a static snapshot:

| Substrate | Wire | Write discipline |
|---|---|---|
| **POVM** (:8125) | on each `query`/`path`, reinforce the touched node-pair as a co-activation pathway → the map learns hot routes; feed back into `GRAPH_REPORT.md` "suggested queries" | service API → bound `?` → snapshot → read-back (NO-RISK WRITE) |
| **injection.db** | emit unresolved-arc / open-question findings as `causal_chain` rows so cold-start orientation surfaces them; resolve on close | quoted heredoc, never interpolate; `VACUUM INTO` only |
| **Obsidian vault** | the L6 obsidian exporter, in habitat mode, emits the `> Back to:` protocol + `MASTER_INDEX.md` pointers, then triggers `hmem rebuild` | bidi-link protocol; memory #5 |
| **Auto-memory** | a session-end pointer line in `MEMORY.md` when the graph materially changes | progressive-disclosure cap |

**Standing decision honored:** stcortex is PURGED — no `:3000` writes, ever.

---

## 3. Orchestrator kernel plugin (the strategic wire)

The orchestrator plugin (`zellij-habitat-orchestrator-plugin` v0.1.3, WASM) decomposes missions;
that is fundamentally a subgraph query. habitat-graph becomes its **map oracle**.

- **Transport:** the existing pipe protocol — `cc-pipe nexus -- habitat-graph query "<scope>"`.
- **L8 `orchestrator_pipe`** module owns the verb + schema. Request/response are schema-validated so
  a malformed query returns the plugin's existing **`NACK_SCHEMA_INVALID`**, and an answer the
  plugin must route through the sidecar returns **`NACK_USE_SIDECAR_SUBMIT`** — i.e. we speak the
  plugin's contract, not a new one.
- **Payload (sketch):**
  ```json
  // request
  {"verb":"map.scope","mission":"<text>","k":25}
  // response
  {"nodes":[{"id","label","file","owner"}],"arcs":[{"src","dst","transport","sealed"}],
   "communities":[{"id","label","members"}],"confidence":"EXTRACTED|INFERRED|AMBIGUOUS"}
  ```
- **Framework alignment:** `ai_docs/ZELLIJ_ORCHESTRATOR_KERNEL_DEPLOYMENT_FRAMEWORK_S1008736.md`.
- This makes habitat-graph the DRAUGHTWRIGHT/LOOMWRIGHT decomposition input: "what modules, arcs,
  and owners does mission X touch?" answered from source, not guessed.

---

## 4. The arc-graph extractor (L8 `arc_graph`) — serves S1008620 directly

The live `wip/bidi-wiring` work hand-builds a producer→consumer arc map. L8 `arc_graph` auto-builds it:

- Recognize bridge idioms in the Rust corpus: `:PORT` constants, `/health`-style URL paths, serde
  request/response struct pairs, `*_adapter.rs` / bridge-client modules, `devenv.toml` services.
- Emit edges typed `arc(producer→consumer, transport{HTTP|UDS|pipe|KV}, payload, sealed?)`.
- **Severed-ear detection:** diff successive `graph.json` — a producer node hot with no inbound
  edge at the consumer = the `arc-coherence` "producer hot, consumer reads zero" signal.
- **Output contract:** feeds `.claude/scripts/arc-coherence-gauge.sh` as a data source, and the
  `bridge-contract` / `schema-drift` skills as a static cross-check.
- This is the **recommended first habitat wire** — it pays into work already in flight and validates
  the whole graphify-as-organ thesis cheaply (read-only, no live mutation).

---

## 5. Cognitive field — PV2 spheres (L8 `pv2_spheres`)

- After analysis, register each **Leiden community** as a **PV2 sphere** (:8132): community id →
  sphere id, couple spheres by inter-community edge weight.
- Populates PV2's chronically under-filled Kuramoto field (cf. `zellij-pv2-sphere-cartographer`)
  with topology that mirrors the real architecture; gives the orchestrator a coarse "which
  subsystem is hot" read.
- **Trap:** honor PV2 sphere-id naming exactly — the WFE/LCM→PV2 severance was a sphere-id naming
  mismatch (CLAUDE.local.md START CODING bidi-wiring). Read-only registration; no field mutation
  without Luke's signal.

---

## 6. LLM backend → TIERWRIGHT (L8 `tierwright`)

- Default `extract` on code = **AST-only, no network** (R3). The semantic/LLM path (docs, PDFs,
  images) routes through **TIERWRIGHT** (model router, :8201) or local **Ollama** — never a raw
  external call on source.
- L3 `backends` exposes a `Backend` trait; L8 adds the `tierwright` impl + makes it the habitat
  default. Satisfies the workspace-boundary + integrity (no-leak) invariants by construction.

---

## 7. Comms & fleet

- **cc-pipe** (`swarm|hab|nexus`) is the primary fast wire (µs–ms); **atuin KV** for durable
  coordination/state; **PV2 UDS bus** for high-rate. Router per `comms-mesh` skill + `ladders.json`.
- A `habitat-graph` pane in the Zellij habitat can subscribe to rebuild events and broadcast
  "graph updated @ <hash>" to the fleet.

---

## 8. Quality gate, publication, repo discipline

| Concern | Plan |
|---|---|
| **Gate** | the 4-stage zero-tolerance gate (`check → clippy -D → pedantic → test`) with `${PIPESTATUS[0]}`; `forbid(unsafe)`; ≥50 tests/module. `/gate`. |
| **Supply chain** | `cargo-deny` + `cargo-audit` in CI; pin grammar crate versions. |
| **Git** | `graphify hook install` analogue — post-commit delta rebuild + the merge driver for conflict-free `graph.json` across parallel worktrees (`worktree-mastery`). |
| **save-session** | a `/save-session` step rebuilds the graph + `hmem rebuild` so each session seals an updated map. |
| **Publication** | `habitat-graph prs` (PR dashboard) feeds the **no-mistakes** gate; run on meaningful changes, skip trivial (Kun Chen rule). |
| **Repo** | **standalone repo, own remotes only** — never the superproject (feedback_morph_ir_engine_standalone_only). Crate split keeps the OSS core publishable separately from L8 `habitat`. |
| **Front door** | `just <recipe>` owns the operator surface (`quality`/`parity`/`graph`/`deploy`/`habitat` groups); standalone repo owns its `justfile`, workspace-root carries thin `habitat-graph-*` proxies. Runbooks (`DEPLOY`/`PARITY`/`MIGRATION`/`INCIDENT`) sequence recipes with human gates. Design: `03_AUTOMATION_RUNBOOKS.md`. |

---

## 9. Interface contracts (the typed surface, sketch)

```rust
// L0 core::schema — graph.json shape (R2: byte-compat with graphify)
struct Node { id: NodeId, label: String, source_file: PathBuf, source_location: Span }
struct Edge { source: NodeId, target: NodeId, relation: String, confidence: Confidence }
enum Confidence { Extracted, Inferred, Ambiguous }
struct Graph { nodes: Vec<Node>, edges: Vec<Edge>, communities: Vec<Community>, manifest: Manifest }

// L7 iface::mcp — rmcp tools (mcp__habitat-graph__*)
#[tool] fn query(question: String, k: usize) -> Subgraph;
#[tool] fn path(from: String, to: String) -> Vec<NodeId>;     // graphify `path` parity
#[tool] fn subgraph(scope: String) -> Subgraph;               // orchestrator map.scope

// L8 orchestrator_pipe — verb schema (§3)
// L8 arc_graph — Arc{producer, consumer, transport, payload, sealed} (§4)
```

---

## 10. Integration acceptance checklist (the L8 done-gate)

- [ ] `cc-health` shows habitat-graph UP via path-map (not hand-rolled curl).
- [ ] `mcp__habitat-graph__{query,path,subgraph}` registered + callable.
- [ ] Orchestrator pipe verb returns schema-valid ACK / correct NACK on bad input.
- [ ] `arc_graph` reproduces the current arc-coherence arc set + flags ≥1 known severed ear.
- [ ] POVM pathway written + read-back on a real query; injection.db chain row on an open question.
- [ ] Obsidian export carries `> Back to:` protocol + lands in `hmem recall`.
- [ ] PV2 sphere count rises by #communities after a registration pass; naming-trap avoided.
- [ ] Backend defaults to AST-only on code; TIERWRIGHT path proven on one doc.

*All §1–§10 are DESIGN. No live wire is built or armed in this planning phase. Live actuation is
gated on `factory.authorize.habitat-graph` (Luke @ 0.A) and a successful core (L0–L7) build first.*
