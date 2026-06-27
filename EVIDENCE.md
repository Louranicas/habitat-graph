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

### D1 Schema (P0) — DONE (core + guard + cache; gate-green, 117 tests)
- `core` crate gate-green | [VBE] | `check`=0 `clippy -D warnings`=0 `pedantic`=0 `test`=0; **44 tests / 0 failed**; `forbid(unsafe)`, no `unwrap`/`expect` in lib.
- graph.json roundtrip | [VBE] | `schema::tests::json_roundtrip_preserves_graph` + deterministic `sorted()` (R4) + `two_graphs_same_content_serialize_identically`.
- Confidence R2 byte-compat | [VBE] | `confidence::tests::serde_matches_graphify_strings` asserts exact `"EXTRACTED"|"INFERRED"|"AMBIGUOUS"`.
- guard rules + Trojan-Source/bidi escapes | [VBE] | `guard::sanitize::display_safe` escapes bidi overrides/isolates/marks + zero-width + BOM + control → `\u{XXXX}`; `path::confine_to` lexical anti-traversal; `url::validate_url` rejects scheme/creds/control/**bidi** (self-test caught a bidi-in-URL bypass, fixed); `secrets::screen_for_secrets`. **82 tests / 0 failed**.
- `habitat-graph-cache` parity-transparency | [VBE] | `memo::memoize` + `memo::tests::parity_transparency_hit_equals_recompute` prove a hit is **byte-identical to a recompute**; blake3 CAS (`key::CacheKey`, domain-separated) + capacity-bounded `MemStore` + LRU eviction (POVM policy injectable) + cached/uncached `partition`. **117 tests / 0 failed** (82 core + 35 cache).
- *Note:* per-module density (8–13) grows toward the 50/module **release** floor in later waves; 50/module is a release-eligibility gate (DDF G4), not per-commit.

### D2 Extract (P1) — IN PROGRESS (source/L2 acquisition done; extract/L3 tree-sitter pending)
- `habitat-graph-source` (detect/ingest/manifest) | [VBE] | built via a **dynamic factory Workflow** (forge-rust-coder-v4 fibers one-per-module + forge-tester & agent-claim-verifier judges OUTSIDE the loop; conditional gate-repair loop, 0 rounds). Main loop re-gated authoritatively (defeating a stale-binary the verifier itself flagged). **207 workspace tests / 0 failed** (core 82, cache 35, source 90 = detect 50, ingest 18, manifest 22).
- judge meaningfulness findings ADDRESSED | [VBE] | detect 2 soft tests now assert real exclusion (hidden-by-default, gitignore glob); manifest external **BLAKE3 KAT** `af1349b9…` (spec value, not our code path) breaks the self-referential oracle — passes, proving the algorithm; ingest directory→Io branch + manifest duplicate-path covered. Production code clean (judges: 0 unwrap/expect/unsafe/silent-swallow).
- `habitat-graph-extract` (tree-sitter-rust) | [VBE] | dynamic Workflow (forge fibers + judges, 0 repair rounds); `registry` (extension dispatch + rayon `extract_files`) + `ast::rust` (walk → fn/struct/enum/trait/mod nodes + calls/defines edges); `core` gained the shared `Extraction`/`RawNode`/`RawEdge` vocabulary (avoids an extract↔backend cycle). Re-gated authoritatively; judge coverage gaps closed (method-call callee **pinned** `obj.foo()`→`"obj.foo"`, async fn, end_byte). `forbid(unsafe)` holds (tree-sitter safe API). **269 workspace tests** (core 90, cache 35, source 90, extract 54).
- parity (node/edge CONTENT vs goldens) | _pending_ | _needs the Python extractor (#15 — the committed goldens are Python) + the fixtures adapter → first parity gate. See `ai_docs/06_PARITY_INTEL`._
- *Note (standards):* detect meets the ≥50/module floor; ingest/manifest (18/22) are simple leaf functions at their meaningful-coverage level — padding to 50 would be filler, violating the harder anti-test-fitting rule. The count floor is enforced on substantive modules at release-eligibility (DDF G4).

### D3 Graph (P2) — IN PROGRESS (build/L4 done; analyze/L5 Leiden pending)
- `habitat-graph-build` (assemble/dedup/merge) | [VBE] | dynamic Workflow (forge fibers + judges, 0 repair rounds); `assemble` interns RawNode labels→NodeId (IndexMap first-seen; dangling-edge drop) → `core::Graph`; `dedup` (by id / by src,tgt,relation); `merge` by-label re-intern + edge remap. Re-gated authoritatively + closed judge contract gaps (merge tool_version a-wins, generated_at fallback, inputs concat). **347 workspace tests** (build 78). forbid(unsafe), no unwrap/expect in lib.
- Leiden community parity | _pending_ | _next: `habitat-graph-analyze` (Leiden) + centrality + patterns_

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
