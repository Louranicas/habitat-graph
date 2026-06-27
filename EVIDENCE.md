# Deployment Evidence — habitat-graph

> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · [[DEPLOYMENT_FRAMEWORK]]
> Gold standard: deep-diff-forge `EVIDENCE.md`.

**STATUS: BUILT — 13-crate workspace, gate-green, 1225 all-targets tests / 0 failed (S1008796).**
D0→D6 + a semantic-backend crate + an MCP frontier organ are sealed below. Remaining: D7 release
(port-claim + standalone remotes + no-mistakes seal — gated on Luke @ 0.A) and D8 learning.
Honesty rule: a claim with no warrant is not recorded here; every count below was re-run
authoritatively in the main loop (never trusted from a builder's self-report).

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

### D7 Release — PARTIAL (private GitHub push DONE; OSS/GitLab/crates.io deferred to Luke)
- gate-green | [VBE] | 1225 all-targets tests / 0 failed, pedantic-clean (re-run authoritatively at HEAD).
- **standalone push** | [VBE] | Luke chose "Private, then push" (S1008796). Secret-screen clean (only `.rs/.toml/.md/.json/.lock/.py` + fixtures tracked); `gh repo create Louranicas/habitat-graph --private --push`; **remote `main` sha `d3eba1d` == local HEAD** (ground-truth `git ls-remote`), visibility PRIVATE, 22 commits. Standalone-only — origin is the crate's own repo, never the superproject.
- **no-mistakes gate** | [VBE] | `no-mistakes init` (local bare-repo gate + post-receive hook; protects future pushes) + seal run `01KW4BMZ…` → `outcome: passed, findings: none`. Caveat: deep steps skipped (commit already upstream) — the substantive validation is the 1225-test gate above.
- _deferred to Luke @ 0.A (one-way doors):_ public/OSS-upstream flip (`gh repo edit --visibility public`), GitLab mirror (needs `glab`/token), `port-claim` set (rec. 8202) + devenv `[[services]]` deploy, crates.io publish (token-gated, irreversible).

### D8 Learning (P6) — PENDING
- Hebbian-reinforced graph | _pending_ (depends on D7 + live POVM actuation).

## Cross-substrate memory web (wired S1008796)
Obsidian hub (`~/projects/claude_code/`) · `injection.db` session_checkpoint + causal_chain · POVM
namespace `habitat_graph` · auto-memory pointer (`memory/`) · `MASTER_INDEX.md` · CLAUDE.local.md
anchor. Slug `s1008796-habitat-graph-rust`.

---
*Evidence template authored S1008796 · Claude @ cortex. Populated per phase as gates pass — never ahead of them.*
