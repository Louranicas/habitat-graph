> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · strategy: [[ai_docs/00_DEPLOYMENT_PLAN]] §6 · map: [[ULTRAMAP]] · framework: [[DEPLOYMENT_FRAMEWORK]] (maturity D0–D8)

# MIGRATION_RUNBOOK — habitat-graph (P0 → P6 strangler walk)

**STATUS: PLANNING SKETCH (S1008796).** The operational checklist form of the migration strategy
(`00_DEPLOYMENT_PLAN.md` §6 is the *why*; this is the *do*). Bottom-up by layer; each phase is
impl → gate → parity → independent verify. **No phase collapse** — a layer is done or it is not.

## Coexistence invariant
Until P4 ships, **Python graphify remains the reference implementation.** `graph.json` stays
schema-compatible (R2) so both interoperate. Do not delete/retire Python graphify until P4 parity
is signed off.

## Per-phase loop (apply to every phase)
1. Implement the layer's modules (bottom-up per `ULTRAMAP.md`), ≥50 tests/module, `forbid(unsafe)`.
2. `just gate` — green.
3. `just parity` — 0 REGRESSION for this phase's scope (see PARITY_RUNBOOK phase scoping).
4. Independent verify (`verify-receipt`) — re-run gate + re-diff; agents over-claim.
5. Commit (test counts + gate status in message). WFE-ledger the phase if wiring the engines.

## Phases
| Phase | Layers | Done when |
|---|---|---|
| **P0** | L0 core + L1 guard | golden `graph.json` byte-roundtrips; validation rules ported |
| **P1** | L2 source + L3 extract (rust/python/js-ts first, then go/jvm/c-cpp/misc) | node/edge parity on `worked/` |
| **P2** | L4 build + L5 analyze | community-structure parity (label-permutation equivalent) |
| **P3** | L6 output | every exporter matches goldens (json/html/svg/graphml/cypher/obsidian/wiki) |
| **P4** | L7 iface | CLI + MCP (`rmcp`) smoke; `query`/`path` parity → **Python graphify can be retired** |
| **P5** | L8 habitat | integration acceptance checklist (`02_HABITAT_INTEGRATION.md` §10) |
| **P6** | stretch | semantic LLM extraction · PDF/Office · Neo4j-live · video (feature-gated) |

## Isolation
- Run parallel-phase work in **git worktrees** (`worktree-mastery`) to avoid file contention across
  fleet panes; merge per-submodule after verifiers pass.
- The `graph.json` **merge driver** (P4 `hooks`) keeps the artifact conflict-free across worktrees.

## Exit
- v1 done = P0–P5 signed off + `00_DEPLOYMENT_PLAN.md` §7 checklist complete + no-mistakes gate passed.
- Retire Python graphify only after P4; archive its pinned version + goldens for regression history.
