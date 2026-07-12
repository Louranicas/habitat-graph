> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · [[EVIDENCE]] · **build plan:** [[14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901]] (§7B) · **schematics:** [[16_ARCHITECTURE_SCHEMATICS_V3_S1008901]] · [[19_PLAN_SCHEMATIC_MAP_S1008901]] · **diagnostics:** [[18_DIAGNOSTICS_OBSERVABILITY_V3_S1008901]] · **cross-model:** [[17_CROSS_MODEL_AGENTIC_CONTRACT_S1008901]]
> **Sibling runbooks (D0–D7 built state):** `DEPLOY` · `PARITY` · `MIGRATION` · `INCIDENT` · `COMMANDS`. **This runbook covers the v3 LIVE-ORGAN surfaces those predate.**

# habitat-graph — V3 Live-Organ Operations Runbook (S1008901)

> [!frame] Why this exists (the operate frame)
> The build plan (docs 14–19) answers *"how do we construct the organ?"* This runbook answers
> *"what is the organ's life once it runs autonomously in the factory, over time, under load?"* — the
> **operate/inhabitant frame** the NA gap pass surfaced ([[14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901]]
> §7B). It is the artifact NA-5 (operator existence) demanded, and the home for the NA-1 (freshness)
> and NA-3 (loop-closure) operational policies.

> [!status] DESIGN-TIME runbook. The organ is **not yet deployed on `:8202`** (gated on Luke @ 0.A).
> Procedures below marked **(LIVE-PENDING)** await the running service; the rest are operable now
> (build/gate/verify). Authored ahead of deployment exactly as the sibling runbooks were.

---

## 0. The live organ at a glance

```
TRANSPORTS (T:)            STATE                      LIFECYCLE
  CLI one-shot     ──┐                                 build → gate → deploy(:8202) → soak
  HTTP serve :addr  ─┤── handle_jsonrpc ── ArcSwap<Graph> ── auto-rebuild(freshness) → reload(atomic)
  MCP stdio        ─┤        (one codec)        │ generation + stale                  → arc-delta(A2)
  UDS hg.sock 0o600 ┘                           └ warm index · budget · confidence
```
Port `8202` (claimed). UDS: `$XDG_RUNTIME_DIR/habitat-graph/hg.sock` (dir `0o700`, sock `0o600`, morphd shape). Live features split: `live-memory` / `live-bridges` / `live-semantic` (R8b).

---

## 1. Deploy & redeploy (LIVE-PENDING — gated on Luke)

> Arming + `[[services]]` registration are Luke @ 0.A one-way-ish doors. Claude **never** spawns the service bare (sandbox reaps children) — `devenv start/restart` only.

```bash
# 0. preflight — gate-green at HEAD (the build contract), both remotes current
cd habitat-graph && just gate          # check → clippy -D → pedantic → test, ${PIPESTATUS[0]} per stage
git ls-remote origin -h refs/heads/main | awk '{print $1}'   # == git rev-parse HEAD

# 1. binary deploy (TRAP: bare `cp` → trash alias; ALWAYS /usr/bin/cp -f to the command= path)
cargo build --release --features "live-memory,live-bridges"      # least-privilege; add live-semantic only if PE live
/usr/bin/cp -f target/release/habitat-graph "$(<devenv command path>)"

# 2. register + start (Luke): add [[services]] on :8202 to ~/.config/devenv/devenv.toml, then
~/.local/bin/devenv -c ~/.config/devenv/devenv.toml restart habitat-graph

# 3. verify (ground truth, never the start message)
cc-health                                                        # path-map aware; NOT a hand-rolled curl loop
curl -s -o /dev/null -w '%{http_code}\n' -m 1 http://localhost:8202/health   # 200
```
**UDS daemon (A1):** the daemon process owns both the HTTP listener and the `UnixListener`; on start it creates `$XDG_RUNTIME_DIR/habitat-graph/` (`0o700`) then binds `hg.sock` (`0o600`). On restart it must `unlink` a stale socket first (see I-1).
**Soak (after every deploy):** `/sweep`-style 30s probes for ≥10 min — `/health` stable, `generation` advancing only on real rebuilds, no breaker churn, parity-regression field clean (doc 18 §3).

---

## 2. Freshness & staleness POLICY (NA-1 — the operate-frame gap)

The build ships freshness *signals*; **operations own the policy.** A graph is perishable — it lags the source from the instant it's built.

| Policy knob | Default | Rationale |
|---|---|---|
| **Shelf-life** (max age before `stale:true` regardless of source change) | 1 h (configurable) | bounds how blind an agent can be without knowing |
| **Auto-rebuild cadence** | on source-`mtime` change (debounced 2 s) + a periodic floor (e.g. 15 min) | `--watch` is event-driven; the floor catches missed events |
| **Served-stale contract** | ALWAYS serve, with `stale:true` + `X-Graph-Generation`/`X-Graph-Stale` header (witness-filter-then-cap) | never a torn read, never a silent stale graph |
| **Rebuild owner** | the daemon (self-rebuild) under the single-writer lock (PC, P1-G10) | one writer; watch×hook race-guarded |

**Operating rule:** liveness ≠ freshness (the N-2 lesson). `/health` 200 means *up*; `stale:false` + recent `generation` means *fresh*. An incident is "served-stale past shelf-life" (I-4), not "down."

---

## 3. Loop-closure procedure (NA-3 — the agent's own perception-action loop)

An agent that edits code then immediately re-queries the graph may read a map that **predates its own change.** Procedure to close that loop:

```text
1. agent records G_before = graph_health.generation   (before its edit)
2. agent writes code
3. trigger rebuild:  the git hook (PC) OR an explicit `graph.reload` verb (UDS/MCP)
4. agent WAITS on generation:  poll graph_health until generation != G_before (bounded timeout)
5. agent re-queries — now guaranteed to see ≥ its own change
```
**Build dependency (feeds back to the plan):** A1/A2 must expose (a) a `graph.reload` verb and (b) a `generation`-advance an agent can wait on. Until built, the operational fallback is `extract` one-shot to a fresh `graph.json` and query that file (cold but self-consistent).

---

## 4. Grammar operations (the v3 PA surface)

> **ABI baseline (✅ G-ABI resolved, [[abi-matrix-s1008901]]):** single core **tree-sitter 0.25.x (ABI 15) + tree-sitter-language 0.1**; all 13 grammars at latest; **Kotlin = `tree-sitter-kotlin-ng`** (abandoned `tree-sitter-kotlin` caps at core <0.23). A grammar's own `tree-sitter` req is `kind=dev` — ignore it; the consumer constraint is `tree-sitter-language ^0.1`. **`cargo tree -p habitat-graph-extract -i tree-sitter` MUST show exactly one core.** (Built rust/python migrated 0.22.6→0.25 `LanguageFn` as PA-1 prep.)

| Op | Procedure |
|---|---|
| **Hot-add a grammar** | enable its `--feature <lang>` (R9a), rebuild, re-run `parity_<lang>` vs the **pinned** golden; if PASS at its tier (95/90 factory-critical, 80/70 tail) ship; the `maturity` field flips to `stable` |
| **ABI drift watch** | `doctor --json .abi` — any grammar showing `DEFERRED:` or a core-ABI mismatch (doc 18 §2). A new grammar release that bumps the core ABI is **NOT** auto-adopted (D7 discipline analog) — re-run G-ABI matrix first |
| **Grammar regresses in prod** | parity-regression field (doc 18 §3) shows a golden below its bar → **downgrade `maturity` to `beta`** (agents gate on it) → triage parse_errors → fix or pin-older (R2b); never silently keep serving a sub-bar grammar as `stable` |
| **Disable a grammar** | drop its `--feature`; the registry stops dispatching its exts; `completeness.per_language` reflects the removal |

---

## 5. Live-actuation rituals (A2 — arming-gated)

> Read-only telemetry needs no arming. **Writes** (PV2 sphere + POVM/injection.db delta-push) require `factory.authorize.habitat-graph` (armed, verified S1008901) AND the right `live-*` feature.

```bash
atuin kv get factory.authorize.habitat-graph        # MUST = armed (read-never-write)
# measure-only first (compute arc-deltas, do NOT push):  build without live-bridges
# flip to live:  rebuild with --features live-bridges  → arc-telemetry pushes to gauge/PV2/POVM/cc-pipe
# de-arm (operator):  Luke unsets the key → daemon falls back to measure-only on next reload
```
**Least-privilege (R8b):** enable only the `live-*` the surface needs — `live-memory` (injection.db writes) without `live-bridges` (PV2 HTTP) without `live-semantic` (model net). The delta-push surface should run `live-memory,live-bridges` and NOT `live-semantic`.

---

## 6. Cross-model operations (Claude 4.8+ / GPT-5.5+)

| Client | Mount |
|---|---|
| **Claude Code 4.8+** | `habitat-graph install` writes the Claude Code MCP config (snapshot→write→read-back, R5/PC); tools + resources auto-discovered via `tools/list`+`resources/list` |
| **GPT-5.5+** | MCP↔function-call bridge (A3) **or** direct HTTP/UDS; tool-router builds function specs from `doctor --json` (capability manifest) |
| **fleet / Architect** | UDS `hg.sock` — many agents, one warm graph |

**XM harness lane (C-G2 / A3):** the only place live models are called — read-only against the organ, routed per habitat policy (TIERWRIGHT for any model-side work, R5a), run as a bounded manual/CI lane, never a serve-path dependency. XM-1…7 (doc 17 §9) are the acceptance probes.
**Capability negotiation:** a model reads the manifest and uses only present features (`budget`/`confidence_filter`/`resources`/`uds`) — older-organ × newer-model degrades gracefully (no version hard-coding).

---

## 7. Diagnostics & health (operationalized)

Read `doctor --json` (doc 18 §1) — the agent/operator manifest. Key operational fields:
`graph.{generation,stale}` · `completeness.per_language[].{node_pct,struct_pct,maturity,parse_errors}` · `abi.deferred[]` · `parity.by_golden[].verdict` · `semantic.{is_local,rejected_dos,timeouts}`.

**Operator decision tree** (doc 18 §9): `/health` 200? → `stale`? → parity REGRESSION? → arc-coherence drop > δ? → healthy. Use `cc-health` (path-map aware), never a hand-rolled `:8202/health` loop.

---

## 8. Incident playbooks (the organ's OWN incidents)

| ID | Symptom | First moves |
|---|---|---|
| **I-1** | UDS daemon crash / **stale socket** / perm leak | `/usr/bin/ps aux \| grep '[h]abitat-graph'`; if dead, `devenv restart habitat-graph`; on `EADDRINUSE` for `hg.sock`, the start path must `unlink` the stale socket (stale-inode lesson); verify `0o600`/dir `0o700` |
| **I-2** | `:8202` won't bind / port contention | `ss -tlnp \| grep 8202` (the trap: free-in-ss ≠ free-in-devenv — `atuin kv get port.claim.habitat-graph` = 8202); resolve the squatter, `devenv restart` |
| **I-3** | graph won't rebuild (parse storm / OOM) | `doctor --json .extract_metrics.per_language[].parse_errors` to find the offending grammar; disable its `--feature` (§4); cap memory; the **last good `generation` keeps serving** (ArcSwap never swaps in a failed build) |
| **I-4** | **served-stale past shelf-life** (the freshness incident, NA-1) | confirm `stale:true` + age > shelf-life; check the rebuild owner (watch alive? hook firing? single-writer lock stuck?); manual `graph.reload`; this is the *real* incident, not "down" |
| **I-5** | parity REGRESSION in prod (a grammar drops below bar) | doc 18 §3 `parity.by_golden`; downgrade `maturity`→`beta` immediately (agents stop trusting it); triage vs the **pinned** oracle (a moving oracle is impossible by R3a, so it's *our* regression); fix or pin-older |
| **I-6** | TIERWRIGHT outage degrades semantic path | `doctor --json .semantic` → `timeouts` spike, `is_local` audit; semantic is opt-in + off-by-default, so the **agent-critical EXTRACTED graph is unaffected** — degrade semantic only, alarm, do not fail the read path |
| **I-7** | severed-arc storm (arc-telemetry alarm) | a real factory-wiring regression (or a graph rebuild that dropped a grammar → false arcs); confirm against the declared arc set (C-G5); if real → injection.db trap + S1008620; if artifact → it's I-3/I-5 underneath |
| **I-8** | DoS attempt (`rejected_dos` spike on pdf/url) | the C-2 caps working as designed; confirm caps (size/timeout/mem) holding; these are off the agent-critical path (R11b) so no serve impact; log the source |
| **I-9** | a model can't drive the organ | run XM-1…7 (doc 17 §9) for the failing client; check capability negotiation (manifest discoverable?), transport mount (MCP config / bridge / UDS perms), token-budget basis (R7a tokenizer field) |
| **I-10** | `update`/`add` refuses a pending transaction or private lineage | stop concurrent writers and return to the Git context that created the operation; retry the same command so the owner-only journal can recover. Do not delete state or journals by hand. A changed public graph, multiple journals, unverified ancestry, or mismatched checksum fails closed and needs forensic review before regeneration. |

---

## 9. Rollback

```bash
# binary (always /usr/bin/cp -f; keep a .bak at deploy time)
/usr/bin/cp -f "<command-path>.bak" "<command-path>" && devenv restart habitat-graph
# schema_version mismatch (PC, C-G4): rebuild-on-mismatch is the safe default — never in-place upgrade a graph.json across taxonomies
habitat-graph extract <dir> --out <fresh>          # produce a clean current-schema graph
# git (standalone, both remotes; never --force main; --force-with-lease on a feature only)
git revert <bad>  # prefer revert over reset for a pushed commit
```

---

## 10. Capacity & contention (C-G7 / NA)

- **Warm-graph memory:** one `ArcSwap<Graph>` per organ; a rebuild briefly holds *two* graphs (old served + new building) — size headroom for 2× the largest graph.
- **N-agent UDS load:** concurrent reads are lock-free (`ArcSwap::load`); set a max-concurrent-connection cap + backpressure (reject-with-`busy` typed error, not block) — **capacity target is C-G7, set in A1.**
- **Rebuild vs serve:** the single-writer lock serializes rebuilds; reads never block on a rebuild (served-stale, §2).

---

## 11. Multi-graph / fleet (NA-2 / Decision 7.8 — LIVE-PENDING the decision)

Until **Decision 7.8** (organ cardinality) is made, operate **one organ per repo** (the self-hosting default). If/when graph-of-graphs is chosen: one daemon multiplexing N graphs, cross-repo edges become a new relation class, and the manifest grows a `graphs[]` array. Per-service fleet = N daemons on N sockets behind a registry. **Do not build any of this before 7.8 is decided** (avoids the NA-2 over-build).

---

## 12. Operate-frame requirements that feed BACK to the build (the loop closes)

The runbook is not just downstream — it imposes contracts the build must satisfy:

| Runbook need | Build must provide | Lands |
|---|---|---|
| §2 freshness policy | shelf-life config + periodic-rebuild floor + served-stale header | A1 |
| §3 loop closure | `graph.reload` verb + a waitable `generation` | A1/A2 |
| §4 grammar regress | a `maturity` *downgrade* path observable at runtime | PA/A1 |
| §8 I-1 | start-path `unlink` of a stale UDS socket | A1 |
| §8 I-3 | ArcSwap never swaps in a failed build (last-good persists) | A1 |
| §10 | a daemon capacity target + `busy` typed error (backpressure) | A1 (C-G7) |

---

## 13. Quick-reference command card

```bash
just gate                                  # the build contract (check→clippy→pedantic→test)
cc-health                                  # fleet-aware health (NOT hand-rolled curl)
curl -s -o /dev/null -w '%{http_code}\n' -m1 localhost:8202/health
habitat-graph doctor --json | jq '.graph,.completeness.per_language,.parity.by_golden,.abi'
atuin kv get factory.authorize.habitat-graph        # arming (read-never-write)
atuin kv get port.claim.habitat-graph               # 8202
git ls-remote origin -h refs/heads/main | awk '{print $1}'   # == rev-parse HEAD
devenv -c ~/.config/devenv/devenv.toml restart habitat-graph
```

---

## 14. Cross-reference (bidirectional)

| Runbook § | Canonical source |
|---|---|
| §2 freshness | [[14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901]] §7B NA-1 · [[18_DIAGNOSTICS_OBSERVABILITY_V3_S1008901]] §1 |
| §3 loop-closure | §7B NA-3 |
| §4 grammars | [[15_FEATURE_ASSIMILATION_MATRIX_S1008901]] §B · [[18_DIAGNOSTICS_OBSERVABILITY_V3_S1008901]] §1-2 |
| §5 actuation | [[16_ARCHITECTURE_SCHEMATICS_V3_S1008901]] §10 · §7A R8b |
| §6 cross-model | [[17_CROSS_MODEL_AGENTIC_CONTRACT_S1008901]] |
| §7-8 diagnostics/incidents | [[18_DIAGNOSTICS_OBSERVABILITY_V3_S1008901]] §9 |
| §11 fleet | §7B Decision 7.8 |
| §12 feedback | [[14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901]] §7B |

---
*V3 Live-Organ Operations Runbook S1008901 (2026-06-29) · Claude @ cortex. The operate-frame companion to the build-frame corpus (docs 14–19). Design-time; LIVE-PENDING procedures await the `:8202` deploy (gated on Luke @ 0.A). Sibling D0–D7 runbooks: DEPLOY/PARITY/MIGRATION/INCIDENT/COMMANDS.*
