> Back to: [[CLAUDE.md]] · [[CLAUDE.local.md]] · [[habitat-graph/README]] · machine pair: [[plan.toml]] · framework: [[DEPLOYMENT_FRAMEWORK]] · [[MODULE_STRUCTURE_PLAN]]

# habitat-graph — ULTRAMAP (layer → module dependency map)

**STATUS: PLANNING SKETCH.** The derived machine map LOOMWRIGHT reads alongside `plan.toml`.
Build order is strictly bottom-up; each module gate-green + ≥50 tests before the layer above starts.
Exemplar: `safishamsi/graphify`. Authored S1008796.

```
L8 habitat ─ bridge · memory · obsidian_protocol · pv2_spheres · orchestrator_pipe · tierwright · arc_graph
              │  (feature = "habitat"; ADDITIVE — depends on L0..L7, never the reverse)
              ▼
L7 iface ──── cli · mcp · watch · hooks · serve
              │  (binary crate; depends on core L0..L6)
              ▼
L6 output ─── report · export_{json,html,svg,graphml,cypher,obsidian} · benchmark
              │  depends on: schema(L0), Graph(L4), communities(L5)
              ▼
L5 analyze ── cluster(Leiden) · centrality · patterns · questions
              │  depends on: Graph(L4)
              ▼
L4 build ──── assemble · dedup · merge
              │  depends on: Extraction(L3), schema(L0)
              ▼
L3 extract ── registry · ast_{rust,python,js_ts,go,jvm,c_cpp,misc} · semantic · backends
              │  depends on: source(L2), guard(L1), schema(L0)
              ▼
L2 source ─── detect · ingest · cache · manifest
              │  depends on: guard(L1), config(L0)
              ▼
L1 guard ──── validate_url · validate_path · sanitize · schema_check · secrets
              │  depends on: types(L0), error(L0)
              ▼
L0 core ───── types · error · config · schema · confidence   ← foundation, no internal deps
```

## Build phases (parity-gated — see 00_DEPLOYMENT_PLAN §6)

| Phase | Layers | Parity gate | Risk |
|---|---|---|---|
| P0 | L0 + L1 | byte-roundtrip a golden `graph.json` | low |
| P1 | L2 + L3 (rust/python/js-ts first) | node/edge set vs Python goldens (`worked/`) | medium |
| P2 | L4 + L5 | community structure (label-permutation equivalent) | medium |
| P3 | L6 | exporter outputs vs goldens | low |
| P4 | L7 | CLI/MCP smoke + parity on `query`/`path` | low |
| P5 | L8 | integration acceptance checklist (02_HABITAT §10) | medium |
| P6 | stretch | semantic/PDF/Neo4j-live/office | deferred |

## Cross-cutting substrate — `habitat-graph-cache` (L1.5, ADR-04)

ONE incremental crate, not a cache per crate. Sits between `core` and the feature layers:
`source`/`extract`/`build`/`analyze`/`serve` key into it; it depends only on `core`. Content-addressed
(blake3) + demand-driven memoization → per-file→per-stage incrementality (the <2s rebuild SLO).
**Transparent to parity** (hit == recompute, G5). Plus **`habitat-graph-daemon`** (L7): warm-DB host
(salsa DB + UDS, morphd-shaped) — the live-server capacity multiplier (sub-second queries · subscriptions ·
persistence); CLI/MCP/watch/orchestrator are its clients. `rayon` intra-machine; differential-dataflow
frontier lane feature-gated (D8). Workspace = **~13 narrow crates**. → [[04_CACHING_INCREMENTAL_CLUSTER]].

## Cross-cutting (every module)

- `forbid(unsafe)` (except upstream tree-sitter FFI, isolated in L3 ast_* wrappers).
- no `unwrap`/`expect` in lib code; `thiserror` error taxonomy from L0.
- `tracing` span per pipeline stage.
- deterministic ordering (sorted nodes/edges) — required for the git merge driver (R4).
- ≥50 meaningful tests/module; parity tests live alongside, fed by the `worked/` golden corpus.

## Test-count floor

~35 substantive modules × 50 = **~1750 test floor** (cf. factory-map 1934, WFE 2163, morph-ir 1404).
Parity harness adds a per-fixture suite on top.
