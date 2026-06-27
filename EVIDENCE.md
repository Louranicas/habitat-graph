# Deployment Evidence — habitat-graph

> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · [[DEPLOYMENT_FRAMEWORK]]
> Gold standard: deep-diff-forge `EVIDENCE.md`.

**STATUS: PLANNING — TEMPLATE, NO SEALED EVIDENCE YET.** No crate exists, so there is nothing to
warrant. This file documents the *form* evidence will take and is populated per phase (D1→D8) as
gates pass. Honesty rule: a claim with no warrant is not recorded here.

## Warrant labels (from the gold standard)
- `[VBE]` — verified by execution (real process; output + exit code asserted).
- `[VBR]` — verified by read (source/file:line read directly).
- `[IFP]` — inferred from pattern (consistent with the codebase, not directly checked).
- `[CONJ]` — conjecture (explicitly unverified).

## Evidence schema (per phase)
Each sealed phase records `claim | warrant | evidence`:
```text
- <claim> | [VBE] | <command output / file:line / test count> 
```

## Phase ledger (to be filled — currently all PENDING)

### D0 Bootstrap — DONE (S1008796, armed)
- corpus authored | [VBR] | `docs/`, `ai_docs/`, `plan.toml`, `ULTRAMAP.md`, `runbooks/` present.
- standalone repo genesis | [VBE] | `git init -b main` in `habitat-graph/` (own `.git`, never the superproject).
- workspace compiles | [VBE] | `cargo check --workspace` rc 0 (serde/serde_json/thiserror resolved).

### D1 Schema (P0) — IN PROGRESS (core landed; cache + guard pending)
- `core` crate gate-green | [VBE] | `check`=0 `clippy -D warnings`=0 `pedantic`=0 `test`=0; **44 tests / 0 failed**; `forbid(unsafe)`, no `unwrap`/`expect` in lib.
- graph.json roundtrip | [VBE] | `schema::tests::json_roundtrip_preserves_graph` + deterministic `sorted()` (R4) + `two_graphs_same_content_serialize_identically`.
- Confidence R2 byte-compat | [VBE] | `confidence::tests::serde_matches_graphify_strings` asserts exact `"EXTRACTED"|"INFERRED"|"AMBIGUOUS"`.
- guard rules + Trojan-Source/bidi escapes | _pending_ | _next: `habitat-graph-core::guard` + `habitat-graph-cache` parity-transparency_

### D2 Extract (P1) — PENDING
- AST node/edge parity (rust/python/js-ts) | _pending_ | _0 REGRESSION vs goldens_

### D3 Graph (P2) — PENDING
- Leiden community parity | _pending_ | _label-permutation equivalent_

### D4 Output (P3) — PENDING
- exporter parity | _pending_ | _json/html/svg/graphml/cypher/obsidian vs goldens_

### D5 Interface (P4) — PENDING
- CLI + MCP contract | _pending_ | _tools/list, exit codes; Python graphify retireable_

### D6 Habitat (P5) — PENDING
- arc-graph reproduces arc-coherence set + flags severed ear | _pending_
- orchestrator pipe ACK/NACK | _pending_ | POVM write+read-back | _pending_

### D7 Release — PENDING
- gate-green + no-mistakes seal | _pending_ | crates.io (token-gated, irreversible)

### D8 Learning (P6) — PENDING
- Hebbian-reinforced graph | _pending_

## Cross-substrate memory web (to be wired at first seal, like DDF)
Obsidian hub · HMS `injection.db` session_checkpoint + causal_chain · POVM namespace `habitat_graph`
· auto-memory pointer · MASTER_INDEX. (Not yet created — planning phase.)

---
*Evidence template authored S1008796 · Claude @ cortex. Populated per phase as gates pass — never ahead of them.*
