> Back to: [[CLAUDE.md]] · [[CLAUDE.local.md]] · [[habitat-graph/README]] · [[ULTRAPLATE Master Index]]
> Hub for: [[DEPLOYMENT_FRAMEWORK]] · [[MODULE_STRUCTURE_PLAN]] · [[04_CACHING_INCREMENTAL_CLUSTER]] · [[05_INTERFACE_CONTRACTS]] · [[EVIDENCE]] · [[00_DEPLOYMENT_PLAN]] · [[01_GRAPHIFY_EXEMPLAR_MAP]] · [[02_HABITAT_INTEGRATION]] · [[03_AUTOMATION_RUNBOOKS]] · [[plan.toml]] · [[ULTRAMAP]] · [[justfile]] · [[DEPLOY_RUNBOOK]] · [[PARITY_RUNBOOK]] · [[MIGRATION_RUNBOOK]] · [[INCIDENT_RUNBOOK]]

# habitat-graph — Executive Summary (S1008796)

**STATUS: PLANNING ONLY.** No code, no `cargo init`, no `devenv` entry, no arming key. This is the
hub of a 13-file design corpus for refactoring [`safishamsi/graphify`](https://github.com/safishamsi/graphify)
(Python) into a Rust factory organ.

---

## The thesis (one paragraph)

Graphify turns any folder of code/docs into a queryable knowledge graph (tree-sitter AST · Leiden
communities · MCP server · Obsidian/Neo4j/GraphML exports). The habitat is 500K LOC of Rust that no
context can hold — exactly graphify's use case — but graphify is a **Python toolchain island** with
no `cargo` gate. Porting it to Rust folds it into the one quality regime AND lets it become a live
organ: a map-oracle the **orchestrator plugin** queries for mission decomposition, and an
auto-built **producer→consumer arc map** that serves the active `wip/bidi-wiring` work directly. The
port is unusually tractable because the three hardest dependencies are **already Rust** —
tree-sitter (native), Leiden (`network_partitions`), MCP (`rmcp`); NetworkX → `petgraph`. Only the
LLM-backend layer and HTML viewer are genuinely new, and the habitat wants those re-pointed anyway
(TIERWRIGHT / local-first).

## The ask

**Go / no-go on the Rust refactor.** Everything below is designed; nothing is built. → details in [[00_DEPLOYMENT_PLAN]] §8.

---

## At a glance

| Dimension | Value | Source |
|---|---|---|
| Exemplar | `safishamsi/graphify` — 19 Py modules, linear pipeline | [[01_GRAPHIFY_EXEMPLAR_MAP]] |
| Architecture | 9 layers, **~13 narrow crates** (+ `cache` incremental substrate + `daemon` warm-DB host), ~1750-test floor | [[ULTRAMAP]] · [[MODULE_STRUCTURE_PLAN]] |
| Capacity | **live knowledge-graph server** (salsa + daemon): sub-second incremental queries · subscriptions · persistence · shared warm graph | [[04_CACHING_INCREMENTAL_CLUSTER]] |
| Crate split | `-core` (L0–L6) / `habitat-graph` (L7) / `-habitat` (L8, feature-gated) | [[00_DEPLOYMENT_PLAN]] §2 |
| Migration | P0→P6 strangler, **parity-gated** against graphify goldens | [[MIGRATION_RUNBOOK]] · [[PARITY_RUNBOOK]] |
| Front door | `just` (quality/parity/graph/deploy/habitat groups) + 4 runbooks | [[justfile]] · [[03_AUTOMATION_RUNBOOKS]] |
| Habitat wiring | orchestrator pipe · arc-graph · POVM/injection · Obsidian/hmem · PV2 · TIERWRIGHT | [[02_HABITAT_INTEGRATION]] |

## Why it's tractable (the dependency story)

`tree-sitter` native · Leiden = `network_partitions` (graspologic's own Rust) · MCP = `rmcp`
(official SDK) · NetworkX → `petgraph` · `+rayon` parallelism Python's GIL never had. Net new-risk
surface = LLM backends + HTML viewer only. → [[01_GRAPHIFY_EXEMPLAR_MAP]] §C.

## Two strategic payoffs

1. **Arc-graph extractor** (L8) auto-builds the producer→consumer map the live S1008620 bidi-wiring
   work builds by hand — flags severed ears, feeds `arc-coherence-gauge.sh`. *Recommended first wire.* → [[02_HABITAT_INTEGRATION]] §4
2. **Orchestrator map-oracle** — the orchestrator kernel plugin pipes `map.scope` queries (ACK/NACK
   schema) to get the subgraph for mission decomposition. → [[02_HABITAT_INTEGRATION]] §3

## Phasing (parity-gated, bottom-up)

`P0` core+guard → `P1` extract → `P2` build+analyze → `P3` output → `P4` interface *(Python graphify
retireable)* → `P5` habitat → `P6` stretch. Each phase: impl → `just gate` → `just parity` (0
REGRESSION) → independent verify. → [[MIGRATION_RUNBOOK]]

## Invariants

`forbid(unsafe)` · no `unwrap` in lib · ≥50 tests/module · local-first (AST-only default) · graph.json
schema-compat · standalone-repo push · 4-stage gate. → [[CLAUDE.md]] (crate charter)

## Open decisions for Luke @ 0.A

go/no-go · `port-claim` · repo remotes · backend policy · `factory.authorize.habitat-graph` arming ·
OSS-upstream stance · root-justfile two-tier proxy. None auto-decided. → [[00_DEPLOYMENT_PLAN]] §8 · [[plan.toml]] `[gates.human]`

---
*Corpus: 13 files / ~1150 lines. Exec summary authored S1008796 · Claude @ cortex. All links resolve within `habitat-graph/`.*
