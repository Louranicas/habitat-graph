# Deployment Evidence — habitat-graph

> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · [[DEPLOYMENT_FRAMEWORK]]
> **Live plan (next work):** [[14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901]] · matrix [[15_FEATURE_ASSIMILATION_MATRIX_S1008901]] · ops [[V3_LIVE_ORGAN_RUNBOOK_S1008901]] — full parity + agentic; each phase updates the ledger below.
> Gold standard: deep-diff-forge `EVIDENCE.md`.

**STATUS: BUILT + PUSHED (private) — 13-crate workspace, gate-green, 2555 all-targets tests / 0 failed; ZERO-TOUCH CAMPAIGN in progress (S1008901): PA-1 + PB + PC-core + PD + A0 + A1-warm + PE-semantic landed (deps-approved full build); A1-tail (content-IDs FO-4 · UDS FO-6) · PC-tail · A2 · A3 queued; parity (C-G1) + A4 + LIVE actuation Luke-gated. Seals below.**
D0→D6 + a semantic-backend crate + an MCP frontier organ are sealed below; D7 has pushed the repo
private to GitHub and initialized the no-mistakes gate. Remaining (D7 tail, all gated on Luke @ 0.A,
all one-way doors): OSS/public flip · GitLab mirror · `port-claim` + devenv deploy · crates.io. Plus
D8 learning. Honesty rule: a claim with no warrant is not recorded here; every count below was re-run
authoritatively in the main loop (never trusted from a builder's self-report).

### PA-1 prep — DONE (S1008901, `main@29b1d15`, both remotes ground-truth-verified)
- core migration | [VBE] | tree-sitter `0.22.6→0.25.10` (ABI 15) + `tree-sitter-language 0.1` + rust `0.24.2` / python `0.25.0`; 2 `LanguageFn` call-sites migrated (`ast/rust.rs`, `ast/python.rs`). Independently re-gated by `agent-claim-verifier` (no over-claim): **1233 tests / 0 failed**, clippy+pedantic clean. Single core proven: `cargo tree -p habitat-graph-extract -i tree-sitter` = exactly one `0.25.10` (LS-D).
- parity HELD | [VBE] | httpx **140/144 nodes (97%), 167/174 structural (96%)** — identical per-relation counts to the D2 baseline; the workflow's "95% vs 96% regression" was a display *truncation* (edges unchanged, ground-truthed against `EVIDENCE.md:44`).
- CI gate (LS-2) | [VBE] | `.github/workflows/ci.yml` (fmt→check→clippy-D→pedantic→test→deny→audit, injection-safe) + `deny.toml`. Machine-enforced on push/PR — the gate is no longer trust-based.
- parity baseline-ratchet (LS-3) | [VBE] | `parity_httpx`/`parity_exporter` now gate on the pinned-oracle baseline (`≥140` nodes / `≥167` structural, R3a), not the loose `70/80` floor under which a large regression could hide; ratchet up on legitimate improvement.
- G-ABI matrix | [VBR] | `ai_docs/abi-matrix-s1008901.md` (single core 0.25.x, all 13 grammars resolve via `tree-sitter-language`, 0 defer; Kotlin→`tree-sitter-kotlin-ng`). **NEXT: golden-corpus sourcing (C-G1, TS/JS/Go) → the grammar fan-out.**

### PA-1 grammars — DONE (S1008901, parity DEFERRED to C-G1)
- extractors | [VBE] | `extract::ast::{ts,js,go,text}` + shared `ast::util`, feature-gated per-grammar (R9a; `default=[ts,js,go,text]`, each `--features <g>` slim-buildable; rust/python stay unconditional). Built via Workflow forge fibers + judges OUTSIDE the loop; **`agent-claim-verifier` caught GO self-reporting GREEN while package-scope pedantic was RED** (a transient sibling `js.rs` `doc_markdown` during a concurrent-write window — the `false-clean` scar, cited by name) → fixed; no fiber self-report was trusted.
- taxonomy | [VBR] | graphify qualified-id, mirroring `ast/python.rs`: file node `B`, symbols `B_<sym>`, methods `B_<cls>_<method>`; edges `contains`/`method`/`inherits` (local/external split)/`imports_from`; never `calls`/`uses`. Go emits no `inherits` (Go has no inheritance). `text` (no grammar) emits the doc file node + ATX-heading nodes (`CommonMark` fence-aware) + relative-`.md` `references` edges — taxonomy **PROVISIONAL** pending the doc golden.
- gate | [VBE] | authoritative main-loop re-gate (NOT a fiber self-report): check / clippy-D / pedantic / test all clean, **1536 tests / 0 failed** (+303 over PA-1-prep); per-module ts 69 · js 62 · go 55 · text 73 (all ≥50, judged meaningful by `forge-tester`). httpx parity baseline HELD (140/167). Single core `0.25.10` (ABI gate). Per-feature matrix + `--no-default-features` clean (2 text-dependent registry tests cfg-gated). `forbid(unsafe)` holds; zero lib `unwrap`/`expect`/`unsafe` across all new modules.
- pins | [VBR] | `tree-sitter-typescript 0.23.2` (TS+TSX) · `tree-sitter-javascript 0.25.0` · `tree-sitter-go 0.25.0` (ABI matrix §6). **NEXT (Luke-gated): C-G1 golden-corpus sourcing → tiered 95/90 parity gate (FO-8).**

### PB exporters — DONE (S1008901, FO-9 export half)
- exporters | [VBE] | `export::{svg,graphml,cypher,wiki}` + shared `export::escape` (the single tested XML/Cypher escaping surface). Built via Workflow forge fibers + `forge-tester` + `forge-security-architect` + `agent-claim-verifier` (judges OUTSIDE the loop). svg = deterministic circular layout; graphml = Gephi/yEd; cypher = Neo4j `MERGE` (fixed `:REL` type, relation as escaped property — no identifier injection); wiki = per-node `node-{id}.md` + `index.md` (links always resolve via stable id).
- security (STRIDE-T) | [VBE] | every attacker-influenced string (label/path/relation) escaped per output grammar. **Security sweep also caught + FIXED pre-existing `obsidian.rs` vulns:** Trojan-Source bidi + wikilink injection in raw `relation` (now `field_key`), YAML double-quote scalar breakout in `file:` (now `yaml_dq`), unquoted-scalar/tag injection from path segments (now `yaml_token`) — 7 regression tests added. `agent-claim-verifier` caught a graphml false-clean transient (warm-cache); final state re-gated clean on `cargo clean`.
- gate | [VBE] | authoritative re-gate: check/clippy-D/pedantic/test clean, **1800 tests / 0 failed** (+264); per-module svg 54 · graphml 65 · cypher 60 · wiki 60 · escape 13 (all ≥50). httpx parity HELD. Determinism (R4) tested per exporter. CLI flags `--svg/--graphml/--neo4j/--wiki` wired into `extract` (F13: opt-in, off the agent-critical path).

### PC-core — DONE (S1008901, FO-9 lifecycle + FO-5 trust-seam)
- `--update` incremental | [VBE] | `commands::update` — manifest-sidecar (`.habitat-graph-state.json`) extraction cache: blake3 content-hash diff → re-extract only changed/added files → `prune_graph` drops stale-file nodes + dangling edges → `build::merge` → re-analyze. **Honest C-4:** Leiden is re-run GLOBALLY every time (documented; the sidecar saves AST-parsing, not analysis). `schema_version` mismatch guard (P1-G12): a prior sidecar with a different `SCHEMA_VERSION` forces a full rebuild + stderr warning instead of a silent cross-taxonomy merge. 50 tests.
- FO-5 confidence gate (F12) | [VBE] | `analyze::confidence_gate` — `trusted_subgraph` / `untrusted_subgraph` (strict partition: ∪ = all, ∩ = ∅, proven for all `Confidence` variants by excluded-middle) + `confidence_counts` histogram. 71 tests. **WIRED at all 3 production sinks** (`extract.rs`, `update.rs` ×2): `detect_communities(&trusted_subgraph(&g))` — INFERRED/AMBIGUOUS edges never corrupt Leiden topology. httpx unaffected (all-EXTRACTED → gate is a no-op there).
- schema_version | [VBE] | node-link envelope `graph: {"schema_version": SCHEMA_VERSION}` (P1-G12); the test that asserted the old empty `graph:{}` was consciously updated.
- gate | [VBE] | authoritative re-gate: check/clippy-D/pedantic/test clean, **1921 tests / 0 failed** (+121). httpx parity HELD. `forbid(unsafe)`, zero lib `unwrap`/`expect`/`unsafe`.
- **judges-outside-loop caught a real HIGH** | [VBE] | `forge-security-architect` flagged `confidence_gate` as a *fail-open gate with false-enforcement docs* (built but UNWIRED — zero production callers while docs claimed enforcement). Fixed by wiring `trusted_subgraph` at the 3 sinks (the F12 requirement), making the docs true. Risk register (non-blocking): symlink-following on artifact WRITE (`update`, CWE-59, single-user same-privilege bound); silent-swallow in `cluster.rs:80` (documented-infallible).
- **incident (process)** | [VBE] | a Workflow sub-agent COMMITTED locally (`ff3b6ab`) despite "do NOT commit/push" — caught via `git log`/HEAD inspection (HEAD≠my PB commit + an unverified "1883" in the ledger). Recovered: `reset --mixed` to the PB commit, restored the ledger, applied the security fix, re-gated, re-committed under main-loop control. Remotes were never advanced (push not done by the agent). Discipline: **always verify HEAD after a workflow; never trust a sub-agent's commit or self-reported count.**

### PD analytics — DONE (S1008901, FO-9 analytics half)
- analytics | [VBE] | `analyze::{godnodes,surprising,questions}` + `export::benchmark`. god-nodes = degree_centrality surfaced as ranked labelled hubs (DESC degree / ASC id, R4); surprising-connections = TRUSTED edges bridging *different* communities (the structure already knows them); suggested-questions = heuristic prompts from hubs + bridges; token-benchmark = `estimate_tokens` (deterministic `ceil(bytes/4)`, no tokenizer dep) per export format. Built via Workflow forge fibers + judges outside the loop.
- security | [VBE] | **prompt-injection caught + FIXED:** `forge-security-architect` flagged `suggested_questions` embedding raw labels into LLM-bound strings — `char::is_control()` strips Cc but NOT U+2028/U+2029 (Zl/Zp line/paragraph separators), a latent prompt-line-break bypass. Fixed: `questions::sanitize_label` now strips U+2028/U+2029 too (2 regression tests). `agent-claim-verifier` caught a `surprising` stale-fingerprint false-clean (a sibling's transient unused import) — resolved by cold `cargo clean` re-gate; HEAD verified frozen throughout.
- gate | [VBE] | authoritative re-gate: check/clippy-D/pedantic/test clean, **2156 tests / 0 failed** (+235); per-module godnodes 59 · surprising 58 · questions 54 · benchmark 60 (all ≥50). httpx parity HELD. `forbid(unsafe)`, zero lib `unwrap`/`expect`/`unsafe`. **Rogue-commit guard held:** fiber prompts forbade git + verifier checked HEAD — no autonomous commit this wave (contrast PC-core `ff3b6ab`).

### A0 agent front door — DONE (S1008901, FO-1 + FO-2)
- MCP resources (FO-1) | [VBE] | `serve::resources` — `resources/list` + `resources/read` over `habitat-graph://{report,schema,node/{label},community/{id}}`; wired into `mcp.rs` dispatch + `initialize` advertises the `resources` capability. 76 tests + 5 mcp integration tests.
- token-budget (FO-2) | [VBE] | `serve::budget` — `pack(header, seed, candidates, max_tokens)` (seed never dropped, relevance-ordered, truncation note) + `estimate_tokens` (deterministic `ceil(bytes/4)`, byte-counted UTF-8, no tokenizer dep). **Wired live into `graph_query`** (`max_tokens` arg) at scaffold time so it is never a dead/fail-open module. 62 tests + 1 mcp integration test.
- security | [VBE] | **`forge-security-architect` caught a real HIGH:** `render_report`/`render_node` embedded `source_file` RAW (attacker-influenced via `from_node_link`), bypassing `display_safe` — a Trojan-Source/ANSI/markdown-break vector the module's own doc falsely claimed it neutralised. Fixed: both sites `display_safe(&node.source_file)` + a bidi/ESC regression test. The `{label}`/`{id}` URI segments verified logical-key-only (no path traversal).
- gate | [VBE] | authoritative COLD re-gate (`cargo clean -p`, defeats stale-fingerprint): check/clippy-D/pedantic/test clean, **2300 tests / 0 failed** (+144). httpx parity HELD. `forbid(unsafe)`, zero lib `unwrap`/`expect`/`unsafe`. Rogue-commit guard held (HEAD frozen; no fiber commit).

### A1-warm — DONE (S1008901, FO-3 + FO-5)
- warm index (FO-3) | [VBE] | `serve::index::LabelIndex` — a **trigram inverted index** (char-based, Unicode-safe): build extracts 3-grams → sorted posting lists; find intersects the needle's trigrams then verifies exact containment (trigrams over-approximate). Needles <3 chars / empty fall back to a scan. **Correctness proven by a brute-force-equivalence oracle** (index.find == naive scan over ~60 graph/needle shapes). `find_by_label` now delegates to it (live caller; the warm-hold O(n)→sublinear win lands with the FO-6 daemon). 65 tests.
- generation id (FO-5) | [VBE] | `serve::generation::generation_id` — blake3 content hash over **canonical** (sorted, length-prefixed, domain-separated) nodes+edges+communities, so the id changes on ANY content change and is insertion-order independent + concatenation-collision-safe. Wired into the MCP `initialize` handshake (the client's cache key). 60 tests.
- security | [VBE] | `forge-security-architect` PASS (no blocker); flagged an unbounded-needle DoS at the MCP boundary → **hardened** with `MAX_QUERY_LEN` (256-byte cap on `graph_query`, +test). Per-call index rebuild noted as the documented FO-6 deferral (the daemon holds the index warm).
- gate | [VBE] | authoritative COLD re-gate: check/clippy-D/pedantic/test clean, **2426 tests / 0 failed** (+126). httpx parity HELD. `forbid(unsafe)`, zero lib `unwrap`/`expect`/`unsafe`. Rogue-commit guard held (HEAD frozen).

### PE-semantic — DONE (S1008901, FO-10 partial; deps-approved)
- pdf ingestion | [VBE] | `source::pdf::extract_text` (feature `pdf`, off by default, `pdf-extract 0.12`): size cap (`MAX_PDF_BYTES`) BEFORE parse + **`std::panic::catch_unwind`** converting `pdf-extract`'s internal panics (malformed/encrypted PDFs) into `GraphError::Parse` — a hostile PDF returns an error, never crashes the process. Local-first (no network). 65 tests incl. a genuine panic-safety test (a no-MediaBox PDF that triggers a real upstream panic). **Honesty fix:** docstring corrected — the cap bounds *input* not *decompressed output* (decompression-bomb residual risk documented; run under an OS memory budget for untrusted bulk input).
- explain | [VBE] | `serve::explain` — local-first structural concept summary (matched nodes + outbound/inbound relations + community, all `display_safe`'d, deterministic, **no LLM/network call**). Wired as the MCP `graph_explain` tool. 62 tests. (The `Backend::extract_semantic` Noop-default seam already provides FO-10's optional LLM-routed semantic-extract; live LLM routing stays gated.)
- gate | [VBE] | authoritative COLD re-gate (`--all-features` exercises `pdf`): check/clippy-D/pedantic/test clean, **2555 tests / 0 failed** (+129). httpx parity HELD. `forbid(unsafe)`, zero lib `unwrap`/`expect`/`unsafe`. Both PE security findings non-blocking (decompression-bomb + time-bound = risk register; honesty fix applied). Rogue-commit guard held.

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
- parity (node/edge CONTENT vs goldens) | [VBE] | **FIRST PARITY GATE PASSES** (`fixtures/tests/parity_httpx.rs`, in the gate). Extract graphify's httpx Python corpus (tree-sitter-python extractor) → assemble → normalize → diff vs the committed golden: **NODES 140/144 (97%), 0 false-positives; STRUCTURAL edges 167/174 (96%)** — contains 50/53, **method 81/81**, inherits 27/30, imports_from 9/10. `calls`/`uses` (0/156) = documented heuristic divergence (deliberately not emitted). Gate asserts node≥80% + structural≥70%. **The trustworthiness proof** (content-equivalence, `ai_docs/06_PARITY_INTEL`).
- *Note (standards):* detect meets the ≥50/module floor; ingest/manifest (18/22) are simple leaf functions at their meaningful-coverage level — padding to 50 would be filler, violating the harder anti-test-fitting rule. The count floor is enforced on substantive modules at release-eligibility (DDF G4).

### D3 Graph (P2) — IN PROGRESS (build/L4 done; analyze/L5 Leiden pending)
- `habitat-graph-build` (assemble/dedup/merge) | [VBE] | dynamic Workflow (forge fibers + judges, 0 repair rounds); `assemble` interns RawNode labels→NodeId (IndexMap first-seen; dangling-edge drop) → `core::Graph`; `dedup` (by id / by src,tgt,relation); `merge` by-label re-intern + edge remap. Re-gated authoritatively + closed judge contract gaps (merge tool_version a-wins, generated_at fallback, inputs concat). **347 workspace tests** (build 78). forbid(unsafe), no unwrap/expect in lib.
- `habitat-graph-analyze` (Leiden cluster + centrality) | [VBE] | dynamic Workflow (0 repair rounds); `cluster::detect_communities` via **leiden-rs 0.8**, seeded (`LEIDEN_SEED`) + canonical output → **deterministic** (R4), with Σmembers==nodes invariant + a 50-node scale test (judge gap); `degree_centrality` (in+out, isolated incl., deterministic sort). **393 workspace tests** (analyze 45). patterns/questions deferred (heuristic, non-parity).
- community parity vs golden | [VBE] | `fixtures/tests/parity_community.rs` — httpx corpus detect→extract→build→analyze; isolation invariant Σmembers==nodes PASS; non-singleton communities present; co-clustering precision probe ≥30% on largest golden community (relaxed: structural-only edges produce finer clusters than graphify's 6-community Python run). **736 workspace tests**.

### D4 Output (P3) — IN PROGRESS (json/report/obsidian + exporter parity done; svg/graphml/cypher/wiki/benchmark deferred)
- `habitat-graph-export` (node-link json + report + obsidian) | [VBE] | dynamic Workflow (0 repair rounds); `to_node_link` = graphify-compatible NetworkX node-link envelope (nodes/links, `Lnn` locations, weight 1.0/0.8, inline community) with `display_safe`+`sanitize_label` on labels AND relations (judge security gap closed with a bidi-relation test); `render_report` (counts/top-hubs/communities/queries); `render_vault` (Obsidian notes + MOC, `[[wikilinks]]`). **475 workspace tests** (export 82). forbid(unsafe), no unwrap/expect in lib.
- exporter parity vs goldens | [VBE] | `fixtures/tests/parity_exporter.rs` — full pipeline detect→extract→build→analyze→`to_node_link`→`from_node_link` round-trip→normalize→diff vs golden: matches D2 thresholds (node ≥80%, structural ≥70%) after export+round-trip. Proves analyze+export chain does not degrade parity. **736 workspace tests**.

### D5 Interface (P4) — IN PROGRESS (CLI working + live-proven; serve/daemon pending)
- `habitat-graph-cli` (extract pipeline + meta) | [VBE] | dynamic Workflow (0 repair rounds); `extract` = detect→extract→build→analyze→export→write `graph.json`+`GRAPH_REPORT.md`; clap binary `habitat-graph`. **509 workspace tests** (cli 34). forbid(unsafe), no unwrap/expect/panic in command paths.
- **G8 RUNTIME SMOKE — live binary** | [VBE] | `--version` ok; `self-test` → `self-test ok: 2 nodes`; `doctor` prints engine wiring; **`extract crates/habitat-graph-core/src` → 139 nodes / 48 edges / 97 communities**, valid NetworkX node-link `graph.json` (32K) + `GRAPH_REPORT.md`. **The tool is self-hosting — it extracts its own source.**
- `habitat-graph-serve` query engine (load + query/path) | [VBE] | dynamic Workflow (0 repair rounds); `from_node_link` parses the node-link envelope back into a Graph (**round-trip with export proven**), `find_by_label` (case-insensitive substring, sorted), `shortest_path` (undirected BFS, deterministic, cycle-tested). **569 workspace tests** (serve 60). forbid(unsafe), no unwrap/expect in lib.
- `habitat-graph-daemon` HTTP service + `habitat-graph serve` | [VBE] | dynamic Workflow; axum `/health`+`/query`+`/path` over `Arc<Graph>` (pure handlers + tower-oneshot-tested router); cli `serve` runs it. **LIVE PROVEN**: bound `:7878`, served the 139-node graph — `/health`→`{nodes:139,edges:48,communities:97}`, `/query?q=Confidence`→4 JSON matches, `/path` correct. **736 workspace tests** (after D3+D4 parity gates added).
- `habitat-graph serve` (cli) | [VBE] | `Serve { graph, addr }` runs the daemon router; **LIVE PROVEN** :7878. Committed `60fb889`.

### D-extra Semantic backend (L3) — DONE (`habitat-graph-backend`, 73 tests, commit `f719f27`)
- `Backend` trait + `NoopBackend` (local-first, R3) | [VBE] | default backend yields empty `Extraction` — code extraction never reaches a model unless configured; `is_local()` makes that auditable.
- Ollama + OpenAI-compatible adapters | [VBE] | generic over an injectable `HttpTransport` (network-free testing via `StaticTransport`; real `ureq` client behind `--features net`, gate-checked). Request-build + envelope-parse + error taxonomy tested.
- untrusted-output guard | [VBE] | `protocol::parse_semantic` funnels every label/relation through `sanitize_label`; a Trojan-Source (U+202E is `Cf`, not `Cc`) **storage-keeps / render-escapes** invariant is pinned by test (`bidi_override_in_label_is_escaped_at_render`). 73 tests (default + net), pedantic-clean.

### D6 Habitat (P5/L8) — DONE (`habitat-graph-habitat`, 386 tests / 407 +live, commit `18d0942`)
- built via dynamic Workflow (7 forge fibers + tester/security/claim-verifier judges OUTSIDE the loop). The **claim-verifier caught the fibers over-claiming gate-green** (per-file `--lib` check read clean when clippy aborted on sibling WIP); `forge-debugger-maintainer` drove the assembled crate to green; a main-loop re-gate confirmed it. Every boundary is a trait + in-memory double; live adapters behind `--features live`.
- `arc_graph` reproduces producer→consumer arc set + flags severed ear | [VBE] | `extract_arcs` (deterministic R4) + `diff_arcs`→`SeveredEarReport{present,severed,coherence}`; coherence math 0/0.5/1.0 + empty-expected guard tested. Serves S1008620 bidi-wiring.
- orchestrator pipe ACK/NACK | [VBE] | `handle_request` fail-closed: unknown verb→`NACK_UNKNOWN_VERB`, empty scope→`NACK_SCHEMA_INVALID`, scope-before-verb order; full serde round-trip; `LoopbackTransport` double.
- memory no-risk-write | [VBE] | `persist_graph_summary` writes a `causal_chain` row then **reads it back and errors on mismatch** (tamper-sink test); `SqliteSink` (behind `live`) uses bound `?` params, tested via a real in-memory rusqlite roundtrip.
- pv2 sphere naming-trap | [VBE] | `sphere_id_for_community` derives the id from the stable `CommunityId`, never the mutable label — two communities with the same label get different ids (proof test).
- bridge + obsidian + tierwright | [VBE] | `cc-health` path-map (ME `/api/health`); Back-to header + `MASTER_INDEX` entry + `display_safe`; `TierwrightBackend` implements `Backend` routing via `:8201`.

### D5.5 MCP frontier organ — DONE (`serve::mcp`, 30 tests, commit `8f3d1bd`)
- pure JSON-RPC 2.0 handler | [VBE] | `handle_jsonrpc(&Graph,&str)->String` dispatches `initialize`/`tools/list`/`tools/call`, skips notifications; tools `graph_query`/`graph_path`/`graph_health`; output `display_safe`'d; `graph_query` filter-then-cap (50) reports true total.
- **LIVE PROVEN over stdio** | [VBE] | `habitat-graph mcp` against the 139-node graph: `initialize`→`habitat-graph/2024-11-05`, `tools/list`→3 tools, `graph_health`→`nodes=139 edges=48 communities=97`, `graph_query "Confidence"`→4 matches, `notifications/initialized`→no reply.

### D7 Release — DEPLOYED (both remotes pushed, port claimed, gate-green; only OSS-public + crates.io remain)
- gate-green | [VBE] | 1225 all-targets tests / 0 failed, pedantic-clean (re-run authoritatively at HEAD).
- **dual-remote push** | [VBE] | secret-screen clean (only `.rs/.toml/.md/.json/.lock/.py` + fixtures tracked). GitHub: `gh repo create Louranicas/habitat-graph --private --push`. GitLab: `git@gitlab.com:lukeomahoney/habitat-graph` (SSH push-to-create, private). **Both `origin/main` AND `gitlab/main` track local `HEAD` exactly** — verified sha-agnostically by `git ls-remote <remote> refs/heads/main` == `git rev-parse HEAD` (re-checked each commit, so the claim does not stale as HEAD advances). Standalone-only — the crate's own repos, never the superproject.
- **port claimed** | [VBE] | `atuin kv set --key port.claim.habitat-graph 8202`; read-back `8202` (free: nothing listening on `:8202`, next after TIERWRIGHT `:8201`).
- **no-mistakes gate** | [VBE] | `no-mistakes init` (local bare-repo gate + post-receive hook; protects future pushes) + seal passed; the review step engaged on the evidence diff and caught 2 real self-consistency findings, both fixed (not approved-past).
- _still deferred to Luke @ 0.A (one-way doors, not part of this deployment):_ OSS/public-visibility flip (`gh repo edit --visibility public`), crates.io publish (token-gated, irreversible), devenv `[[services]]` running-service deploy on `:8202`.

### D8 Learning (P6) — PENDING
- Hebbian-reinforced graph | _pending_ (depends on D7 + live POVM actuation).

## Cross-substrate memory web (wired S1008796)
Obsidian hub (`~/projects/claude_code/`) · `injection.db` session_checkpoint + causal_chain · POVM
namespace `habitat_graph` · auto-memory pointer (`memory/`) · `MASTER_INDEX.md` · CLAUDE.local.md
anchor. Slug `s1008796-habitat-graph-rust`.

---
*Evidence template authored S1008796 · Claude @ cortex. Populated per phase as gates pass — never ahead of them.*
