# habitat-graph

**One warm, deterministic code graph that every LLM agent queries — instead of re-reading your source.**

[![CI](https://github.com/Louranicas/habitat-graph/actions/workflows/ci.yml/badge.svg)](https://github.com/Louranicas/habitat-graph/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust 1.95+](https://img.shields.io/badge/rust-1.95%2B-orange.svg)](https://www.rust-lang.org)
[![tests](https://img.shields.io/badge/tests-3894%20passing-brightgreen.svg)](EVIDENCE.md)
[![unsafe: forbidden](https://img.shields.io/badge/unsafe-forbidden-success.svg)](#guarantees)

Your codebase is too big for any model's context window, so agents burn tokens re-reading the same files over and over — and each one builds a slightly different mental model of the same code. **habitat-graph** extracts a whole tree of source (and docs) into a single deterministic knowledge graph — symbols, references, call edges, communities — and serves it *once* over [MCP](https://modelcontextprotocol.io) and a warm Unix-socket daemon, so every agent, model, and tool reads the **same** answer, cached by content hash and trimmed to fit each model's token budget. It is a from-scratch Rust port of the Python tool [`safishamsi/graphify`](https://github.com/safishamsi/graphify) that reaches feature parity and goes further: content-addressed node IDs, a 3-way **git merge driver for the graph itself**, 14 language extractors, and a cross-model bridge so Claude and GPT drive the same organ — all `forbid(unsafe)`, deterministic, and gate-green at **3894 tests**.

> **Status: V3 complete · pre-release `v0.0.0`.** 13 crates · **3894 all-targets tests / 0 failed** · `forbid(unsafe)`, zero `unwrap`/`expect` in library code · deterministic (byte-identical reruns) · parity-gated against graphify (97% node / 96% structural on the `httpx` oracle). CI-gated: `fmt → check → clippy -D warnings → pedantic → test → cargo-deny → cargo-audit`. The full per-phase evidence ledger is [`EVIDENCE.md`](EVIDENCE.md). The live `:8202` service, crates.io publish, and the cross-model live-proof are roadmap items — see [Status & roadmap](#status--roadmap).

---

## Table of contents

- [Why habitat-graph](#why-habitat-graph)
- [What it is](#what-it-is)
- [What it does](#what-it-does)
- [Feature set](#feature-set)
- [Comparison with graphify](#comparison-with-graphify)
- [Install & build](#install--build)
- [Quick start](#quick-start)
- [Command reference](#command-reference)
- [Using habitat-graph from an LLM agent](#using-habitat-graph-from-an-llm-agent)
- [Architecture](#architecture)
- [Capacity & guarantees](#capacity--guarantees)
- [Documentation](#documentation)
- [Status & roadmap](#status--roadmap)
- [License](#license)

---

## Why habitat-graph

Most code tools answer *"show me this file."* Agents need a different question answered: *"how does this whole system fit together, and what's the smallest slice relevant to my task?"* — without re-reading 500K lines every turn.

The closest existing tool is [graphify](https://github.com/safishamsi/graphify): excellent, but Python — a toolchain island, non-deterministic clustering, no compiler-enforced safety, and no warm-serving story. habitat-graph keeps graphify's whole feature surface and re-founds it as a Rust **organ**:

- **One graph, many thin clients.** Extract once; the HTTP server, the MCP server, the file watcher, and your agents are all readers of a *single* hot graph — not N re-parses.
- **Deterministic by construction.** Sorted node/edge ordering, a seeded Leiden community pass, and content-addressed node IDs mean the same input produces a byte-identical graph — which is what makes the graph diffable, cacheable, and **mergeable**.
- **Local-first.** Code extraction is AST-only and never touches the network. Nothing about your source leaves the machine to build the graph.
- **Built for agents.** A token-budgeted MCP surface, a content-hash cache key in the handshake, and a cross-model translation seam mean an LLM mounts the graph as a first-class tool and pays only for the slice it asked for.

## What it is

habitat-graph is a **13-crate Rust workspace** plus a CLI (`habitat-graph`) that turns any directory of code or docs into a queryable knowledge graph and serves it to humans, scripts, and LLM agents. It began life as the shared **code-perception layer** for an agentic build system — one warm graph every agent reads instead of re-reading source — and is published here as a standalone tool. The factory-integration layer (`--features habitat`) is entirely optional and additive; the core engine has no such coupling.

## What it does

The pipeline mirrors graphify and extends it:

```
detect → extract (tree-sitter AST) → build (assemble · dedup · merge)
       → analyze (Leiden communities · degree centrality) → export
                              ⊕  serve (MCP · HTTP) · watch · git-hooks · ingest · merge-driver
```

- **Deterministic extraction** — AST-only, local-first; seeded clustering + content-addressed IDs ⇒ reproducible graphs.
- **One warm graph** — the daemon holds `Graph + LabelIndex` behind a single `arc_swap::ArcSwap<Snapshot>`, so a rebuild swaps atomically with no torn read.
- **Cached by generation** — a blake3 content hash over the canonical nodes + edges + communities is the cache key, advertised in the MCP `initialize` handshake; an unchanged graph is a free re-fetch.
- **Token-budgeted serve** — responses are packed to fit a model's window (`max_tokens`), the top match is never dropped, ordering is relevance-then-deterministic.
- **graphify parity + extensions** — 97%/96% content parity on the httpx oracle, plus determinism, four extra exporters, analytics, semantic summaries, and an agentic surface graphify has no equivalent for.

## Feature set

**Languages — 14 extractors.** `rust` and `python` (the parity baseline), `typescript` (+TSX), `javascript`, `go`, `text` (Markdown/docs: heading nodes + relative-`.md` reference edges), and 8 more via tree-sitter: `java`, `c`, `cpp`, `ruby`, `csharp`, `kotlin`, `scala`, `php`. All resolve on a single tree-sitter 0.25.x core (ABI 15); each grammar is independently feature-gateable (`--no-default-features --features go`).

**Exporters.** node-link `graph.json` · `GRAPH_REPORT.md` · self-contained `graph.html` · Obsidian vault (`[[wikilinks]]` + MOC) · **SVG** (deterministic layout) · **GraphML** (Gephi/yEd) · **Cypher** (Neo4j `MERGE`, injection-safe) · **wiki** (`node-{id}.md` + index). Every attacker-influenced string is escaped per output grammar.

**Analytics.** god-nodes (degree-centrality hubs) · surprising-connections (trusted edges that bridge communities) · suggested-questions (sanitized against prompt-injection) · token-benchmark (deterministic `ceil(bytes/4)`, no tokenizer dependency).

**Semantic (local-first).** PDF ingestion (feature `pdf`, size-capped + panic-safe) and `graph_explain` — a structural concept summary computed **with no LLM or network call**.

**Agentic / net-new (no graphify equivalent).** Content-addressed node IDs (`blake3(label)[..4]`, stable under rename/insert) · a deterministic **3-way `graph.json` git merge driver** (two branches' graphs auto-merge conflict-free) · a warm UDS daemon with atomic reload · arc-delta severed-edge telemetry · a confidence/trust gate (EXTRACTED / INFERRED / AMBIGUOUS — inferred edges never corrupt the community topology) · a Claude↔GPT cross-model bridge.

**Posture.** `forbid(unsafe)` workspace-wide; zero `unwrap`/`expect`/`unsafe` in library code; determinism tested per exporter and per merge; ≥50 meaningful tests per module.

## Comparison with graphify

Legend: ✅ parity · 🟡 partial · ➕ net-new (no graphify equivalent).

| Capability | [graphify] | habitat-graph |
| --- | :---: | :---: |
| `extract` / `query` / `path` pipeline | ✅ | ✅ |
| node-link `graph.json` (httpx oracle) | ✅ | ✅ 97% node / 96% structural |
| `GRAPH_REPORT.md` · Obsidian vault · `graph.html` | ✅ | ✅ |
| Leiden clustering | ✅ (non-deterministic) | ✅ **seeded → deterministic** |
| Confidence tags on edges | ✅ | ✅ + trust-gate wiring |
| Language grammars | 11 + docs | ✅ 13 grammars + `text` |
| Exporters: SVG · GraphML · Cypher · wiki | ✅ | ✅ (injection-safe) |
| Analytics: god-nodes · surprising · questions · token-benchmark | ✅ | ✅ (deterministic) |
| Semantic / PDF / `explain` | ✅ | ✅ local-first (no-LLM `explain`) · 🟡 PDF feature-gated |
| `update` · `watch` · git-hook · `install` · `add` lifecycle | ✅ | ✅ |
| Per-file parallelism | ❌ (Python GIL) | ➕ rayon |
| Determinism (diffable / mergeable graph) | ❌ | ➕ sorted + seeded + content-addressed |
| Parity-regression gate | ❌ | ➕ ratcheted baseline |
| Warm graph / UDS daemon (atomic reload) | ❌ | ➕ `arc_swap`, generation cache key |
| MCP / agentic surface | ✅ (rmcp) | ➕ pure JSON-RPC + resources + token budget |
| Content-addressed node IDs | ❌ | ➕ `blake3(label)` |
| 3-way `graph.json` git merge driver | ❌ | ➕ |
| Cross-model bridge (Claude 4.8+ / GPT-5.5+) | ❌ | ➕ MCP↔function-call seam |
| `forbid(unsafe)` / zero-unwrap | ❌ | ➕ workspace-wide |

Parity is measured against graphify's committed `httpx` golden: **140/144 nodes (97%)**, **167/174 structural edges (96%)**. The `calls`/`uses` edges are a deliberate heuristic divergence, not a miss. "Full parity" for the OSS-language tail is defined as the achievable envelope (node ≥80% / structural ≥70%); factory-critical grammars gate at the stricter 95/90 tier. The full matrix lives in [`ai_docs/15_FEATURE_ASSIMILATION_MATRIX_S1008901.md`](ai_docs/15_FEATURE_ASSIMILATION_MATRIX_S1008901.md).

[graphify]: https://github.com/safishamsi/graphify

## Install & build

Requires **Rust 1.95+**. The build pins a repo-local target directory.

```bash
git clone https://github.com/Louranicas/habitat-graph.git
cd habitat-graph

# Build the release CLI (repo-local target/)
CARGO_TARGET_DIR=./target cargo build --release -p habitat-graph-cli

# The binary:
./target/release/habitat-graph self-test     # -> "self-test ok: 2 nodes"
./target/release/habitat-graph doctor         # version + engine wiring
```

With [`just`](https://github.com/casey/just):

```bash
just gate        # full quality gate: check → clippy -D → pedantic → test
just parity      # diff against the graphify golden corpus (regression-only)
```

Optional features: `--features watch` (file watcher), `--features live` (`add <url>` ingest), `--features pdf` (PDF ingestion), `--features habitat` (the optional factory-integration layer).

## Quick start

```bash
# 1. Extract a graph from any folder of code (writes graph.json + GRAPH_REPORT.md + graph.html)
habitat-graph extract ./src --out graphify-out

# 2. Ask questions about it
habitat-graph query Confidence --graph graphify-out/graph.json
habitat-graph path Span NodeId  --graph graphify-out/graph.json

# 3. Add every visualisation/interop export
habitat-graph extract ./src --out graphify-out --svg --graphml --neo4j --wiki --vault ./vault

# 4. Serve it over HTTP for scripts/CI
habitat-graph serve --graph graphify-out/graph.json --addr 127.0.0.1:7878
#   GET /health  ·  GET /query?q=...  ·  GET /path?from=...&to=...

# 5. Mount it into Claude Code as an MCP server (see the LLM section below)
habitat-graph install --graph graphify-out/graph.json
```

## Command reference

Binary: `habitat-graph` (crate `habitat-graph-cli`). `extract` always emits `graph.json` + `GRAPH_REPORT.md` + `graph.html`; the `--svg` / `--graphml` / `--neo4j` / `--wiki` / `--vault` flags are additive opt-ins.

| Subcommand | Args / flags | What it does |
| --- | --- | --- |
| `extract <dir>` | `--out <dir>` · `--vault <dir>` · `--svg` · `--graphml` · `--neo4j` · `--wiki` | Build the graph from a source tree and write the requested exports. |
| `update <dir>` | `--out <dir>` | Incremental rebuild reusing cached extractions. |
| `query <substr>` | `--graph <path>` | Substring search over node labels. |
| `path <from> <to>` | `--graph <path>` | Shortest undirected path between two labels. |
| `serve` | `--graph <path>` · `--addr <host:port>` (default `127.0.0.1:7878`) | HTTP server: `/health` `/query` `/path`. |
| `mcp` | `--graph <path>` | MCP server — JSON-RPC 2.0 over **stdio**. |
| `install` | `--graph <path>` · `--dry-run` · `--config-path <p>` | Auto-discover the Claude MCP config and register habitat-graph (snapshot → write → read-back). |
| `install-mcp` | `--graph <path>` · `--name <n>` · `--write <cfg>` | Print or merge an MCP config entry. |
| `merge-driver <base> <ours> <theirs>` | (git-invoked) | 3-way merge of two `graph.json` files. |
| `install-merge-driver` | `--repo <dir>` | Register the merge driver in `.gitattributes`. |
| `hook install` | `--dir <p>` | Install a post-commit graph-rebuild hook. |
| `watch <dir>` | `--out <p>` · *(needs `--features watch`)* | Debounced rebuild on file change. |
| `add <url>` | `--out <p>` · *(needs `--features live`)* | Ingest a remote source file (SSRF-guarded: http(s) only, no private IPs). |
| `self-test` | — | Build a tiny in-memory graph and print a health line. |
| `doctor` | — | Print version + engine wiring. |

Full copy-pasteable command catalog: [`runbooks/COMMANDS.md`](runbooks/COMMANDS.md).

## Using habitat-graph from an LLM agent

This is what habitat-graph is *for*. There are three ways an agent consumes the graph.

**1 — Mount it as a Claude Code MCP server.** One command auto-discovers your Claude MCP config, snapshots it, writes the entry, and reads it back:

```bash
habitat-graph install --graph graphify-out/graph.json          # auto-discover + register
habitat-graph install --graph graphify-out/graph.json --dry-run # preview the change first
```

It registers `{ "command": "<habitat-graph>", "args": ["mcp", "--graph", "<path>"] }` under `mcpServers`, preserving every other server and key. The transport is JSON-RPC 2.0 over stdio.

**2 — The MCP surface.** The server exposes four tools and a resource tree:

| MCP tool | What it answers |
| --- | --- |
| `graph_query` | Substring label search — accepts a **`max_tokens`** budget; the top match is never dropped. |
| `graph_path` | Shortest path between two symbols. |
| `graph_health` | Node / edge / community counts. |
| `graph_explain` | A local-first structural summary of a concept — **no LLM call**. |

Resources are addressable at `habitat-graph://{report, schema, node/{label}, community/{id}}`, and the `initialize` handshake advertises the `generation` content-hash so a client can skip a re-fetch when the graph hasn't changed.

**3 — Token-budgeted, cross-model.** Every response is packed to a model's window, so an agent pays only for the slice it asked for. Claude 4.8+ speaks the MCP organ natively; GPT-5.5+ mounts the *same* graph through a pure MCP↔OpenAI-function translation seam (`mcp_tools_to_openai_functions` / `openai_function_call_to_mcp`), or directly over HTTP. A warm UDS daemon (`serve_uds`, designed socket `$XDG_RUNTIME_DIR/habitat-graph/hg.sock`, `0o600`) holds the graph hot so repeated queries answer from the in-memory index with no rebuild.

> **Honest boundary.** The HTTP `serve`, the stdio `mcp` server, and `install` are wired and live-proven (against habitat-graph's own 139-node self-host graph). The warm UDS daemon and the cross-model bridge are **built and unit-tested as libraries**; their CLI wiring and the live GPT-5.5 proof are on the [roadmap](#status--roadmap). The README never claims a path that isn't in the code.

## Architecture

Nine layers, built bottom-up, split across 13 crates:

```
L0 core → L1 guard → L2 source → L3 extract → L4 build → L5 analyze → L6 export → L7 serve/iface → L8 habitat
```

| Crate | Layer | Role |
| --- | --- | --- |
| `habitat-graph-core` | L0 | graph types, error taxonomy, determinism primitives |
| `habitat-graph-backend` · `-cache` | — | trait seams + content-hash cache |
| `habitat-graph-source` | L2 | file detection / corpus walk |
| `habitat-graph-extract` | L3 | tree-sitter AST extraction (the 14 grammars) |
| `habitat-graph-build` | L4 | assemble · dedup · merge · merge-driver |
| `habitat-graph-analyze` | L5 | Leiden communities, centrality, analytics |
| `habitat-graph-export` | L6 | all exporters |
| `habitat-graph-serve` · `-daemon` | L7 | HTTP + MCP + token budget + warm UDS daemon |
| `habitat-graph-habitat` | L8 | optional factory integration (`--features habitat`) |
| `habitat-graph-cli` · `-fixtures` | — | the binary + golden corpus |

The core (L0–L6) carries no factory coupling and is the OSS-publishable surface; L8 is additive and feature-gated. Design detail: [`docs/MODULE_STRUCTURE_PLAN.md`](docs/MODULE_STRUCTURE_PLAN.md) · [`docs/DEPLOYMENT_FRAMEWORK.md`](docs/DEPLOYMENT_FRAMEWORK.md) · [`ULTRAMAP.md`](ULTRAMAP.md).

## Capacity & guarantees

| Metric | Value |
| --- | --- |
| Crates | **13** |
| Tests (all-targets) | **3894 / 0 failed** |
| Language extractors | 13 grammars + `text` = **14** |
| graphify parity (httpx) | **97% node · 96% structural** |
| Per-module test floor | **≥50** meaningful tests (no test-fitting) |

<a id="guarantees"></a>**Guarantees:** `forbid(unsafe)` workspace-wide (the only `unsafe` is inside the upstream tree-sitter FFI, isolated in L3); zero `unwrap`/`expect` in library code (a `thiserror` taxonomy from L0); **determinism** — sorted nodes/edges + seeded Leiden + content-addressed IDs ⇒ byte-identical reruns, which is what makes the graph diffable and merge-driver-ready; **local-first** — extraction is AST-only and never reaches the network. The authoritative per-phase ledger is [`EVIDENCE.md`](EVIDENCE.md).

## Documentation

Bidirectional with this README — each design doc opens with a `Back to:` breadcrumb.

**Authoritative**
- [`EVIDENCE.md`](EVIDENCE.md) — the per-phase deployment ledger (`claim | warrant | evidence`); the single source of truth.
- [`CLAUDE.md`](CLAUDE.md) — crate charter, invariants, and the quality gate.
- [`ULTRAMAP.md`](ULTRAMAP.md) — bottom-up build order + parity phases.

**Design corpus** (`ai_docs/`)
- [`14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901.md`](ai_docs/14_PLAN_V3_UNIFIED_PARITY_AGENTIC_S1008901.md) — ★ the live plan (full parity + agentic/multi-model).
- [`15_FEATURE_ASSIMILATION_MATRIX_S1008901.md`](ai_docs/15_FEATURE_ASSIMILATION_MATRIX_S1008901.md) — every graphify feature → home/status/gate.
- [`16_ARCHITECTURE_SCHEMATICS_V3_S1008901.md`](ai_docs/16_ARCHITECTURE_SCHEMATICS_V3_S1008901.md) — target architecture.
- [`17_CROSS_MODEL_AGENTIC_CONTRACT_S1008901.md`](ai_docs/17_CROSS_MODEL_AGENTIC_CONTRACT_S1008901.md) — the Claude 4.8+ / GPT-5.5+ drive contract.
- [`18_DIAGNOSTICS_OBSERVABILITY_V3_S1008901.md`](ai_docs/18_DIAGNOSTICS_OBSERVABILITY_V3_S1008901.md) — diagnostics & observability.
- [`abi-matrix-s1008901.md`](ai_docs/abi-matrix-s1008901.md) — the tree-sitter ABI matrix (all grammars, one core).
- [`00_DEPLOYMENT_PLAN.md`](ai_docs/00_DEPLOYMENT_PLAN.md) · [`01_GRAPHIFY_EXEMPLAR_MAP.md`](ai_docs/01_GRAPHIFY_EXEMPLAR_MAP.md) · [`05_INTERFACE_CONTRACTS.md`](ai_docs/05_INTERFACE_CONTRACTS.md) — foundations.

**Operations** (`runbooks/`)
- [`V3_LIVE_ORGAN_RUNBOOK_S1008901.md`](runbooks/V3_LIVE_ORGAN_RUNBOOK_S1008901.md) — operating the live organ (freshness, grammar ops, cross-model, incidents).
- [`COMMANDS.md`](runbooks/COMMANDS.md) · [`DEPLOY_RUNBOOK.md`](runbooks/DEPLOY_RUNBOOK.md) · [`PARITY_RUNBOOK.md`](runbooks/PARITY_RUNBOOK.md) · [`MIGRATION_RUNBOOK.md`](runbooks/MIGRATION_RUNBOOK.md) · [`INCIDENT_RUNBOOK.md`](runbooks/INCIDENT_RUNBOOK.md).

**Framework** (`docs/`)
- [`DEPLOYMENT_FRAMEWORK.md`](docs/DEPLOYMENT_FRAMEWORK.md) — gate stack G0–G10, maturity D0–D8, receipts, rollback.
- [`MODULE_STRUCTURE_PLAN.md`](docs/MODULE_STRUCTURE_PLAN.md) — crate charters + dependency graph.

## Status & roadmap

**Done (V3 complete).** All of the extract → analyze → export pipeline, 14 grammars, all exporters and analytics, the HTTP + stdio-MCP servers, `install`, the git merge driver, and the warm-daemon + cross-model libraries — gate-green at 3894 tests, both remotes synced.

**Roadmap (the remaining one-way doors).**
- The live `:8202` `devenv` service (the always-on organ) and the warm UDS daemon's CLI wiring.
- The live cross-model proof against a running GPT-5.5+ (the translation seam is built + unit-tested today).
- crates.io publication.
- An optional learning loop (graph-novelty weighting).

These are deliberately gated, not blockers — every capability above is usable from a local build today.

## License

Dual-licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work, as defined in the Apache-2.0 license, shall be dual-licensed as above, without any additional terms or conditions.

---

<sub>Rust refactor of <a href="https://github.com/safishamsi/graphify">safishamsi/graphify</a>. Built deterministic, <code>forbid(unsafe)</code>, parity-gated. See <a href="EVIDENCE.md">EVIDENCE.md</a> for the receipts.</sub>


<!-- HABITAT_VAULT_HIGHWAY_START -->

## Habitat Vault Highway

> Registry: `habitat.vault-highways.v1` · vault id: `habitat-graph` · kind: `project-root` · status: `active`

This entry point is reciprocally registered in the workspace-wide vault highway. Cross-vault navigation uses absolute `file://` links because bare Obsidian wikilinks do not resolve reliably across separate vault roots.

- Workspace highway hub: [Habitat Vault Highways](file:///home/louranicas/claude-code-workspace/the-habitat-docs/Habitat%20Vault%20Highways.md)
- Main Obsidian registry (upstream, read-only here): [Habitat Cross-Vault Index](file:///home/louranicas/projects/claude_code/Habitat%20Cross-Vault%20Index.md)
- Hermes registry (upstream, read-only here): [Known Habitat Vaults — Cross Links](file:///home/louranicas/.hermes/hermes-agent-vault/Known%20Habitat%20Vaults%20%E2%80%94%20Cross%20Links.md)
- Reciprocal target recorded by the hub: `habitat-graph/README.md`

<!-- HABITAT_VAULT_HIGHWAY_END -->
