# Commands & graphify Comparison (S1008796)

> Back to: [[MOC]] · [[The 7 Most Powerful Use Cases of habitat-graph]]. Full command reference:
> `../runbooks/COMMANDS.md`. Exemplar map: `../ai_docs/01_GRAPHIFY_EXEMPLAR_MAP.md`.

## Pipe & chain — YES (verified live)
- **Chain** (`&&` / `;`): subcommands are independent; they hand off via the `graph.json` file.
  `habitat-graph extract <dir> --out og && habitat-graph query Foo --graph og/graph.json`
- **Pipe in** (`|`): `mcp` is a stdin→stdout JSON-RPC line processor — stream many requests, get many
  responses. `printf '%s\n' '<jsonrpc>' ... | habitat-graph mcp --graph og/graph.json`
- **Pipe out**: the `graph.json` is the machine-readable artifact — `... && jq '.nodes|length' og/graph.json`.
- Contract: machine commands need no TTY; `stdout` = output, `stderr` = diagnostics, documented exit codes.

## All current commands

| Command | Args | Does | stdout | Pipe / chain |
|---|---|---|---|---|
| `extract <dir>` | `--out <dir>` (def `graphify-out`) | detect→extract→build→analyze→export → `graph.json` + `GRAPH_REPORT.md` | progress line | chain → query/serve/mcp via file |
| `query <substr>` | `--graph <json>` | case-insensitive label search, sorted | human text (matches + `file:line`) | chain after extract |
| `path <from> <to>` | `--graph <json>` | shortest **undirected** path (BFS, deterministic) | human text | chain |
| `serve` | `--graph <json> --addr 127.0.0.1:7878` | HTTP `/health` `/query?q=` `/path?from=&to=` | binds + blocks | `curl` clients |
| `mcp` | `--graph <json>` | MCP JSON-RPC 2.0 over stdio (`graph_query`/`graph_path`/`graph_health`) | JSON-RPC per line | **pipe** stdin→stdout |
| `self-test` | — | tiny in-memory corpus, no I/O | `self-test ok: 2 nodes` | — |
| `doctor` | — | env + engine wiring | diagnostics | — |
| `help [cmd]` | — | clap help | usage | — |

## Side-by-side vs `safishamsi/graphify`

| Dimension | graphify (Python) | habitat-graph (Rust) |
|---|---|---|
| Language / safety | Python 3 | Rust · `forbid(unsafe)` · no `unwrap`/`expect` in lib |
| CLI subcommands | extract · query · path · install · hook · prs · export | extract · query · path · **serve** · **mcp** · self-test · doctor |
| AST extraction | tree-sitter · ~36 grammars | tree-sitter · **Rust + Python** (parity-proven), extensible `Extractor` registry |
| Graph engine | NetworkX | petgraph |
| Clustering | graspologic Leiden | leiden-rs (**seeded → deterministic**) |
| Parallelism | GIL — none | **rayon** per-file parallel |
| `graph.json` | NetworkX node-link | node-link · **R2 byte-compat · 97/96 % parity** |
| Confidence signal | EXTRACTED/INFERRED/AMBIGUOUS | `core::Confidence` (byte-compatible enum) |
| Export formats | json·html·svg·obsidian·graphml·cypher·wiki (7) | json·report·obsidian (svg/graphml/cypher/wiki **deferred**) |
| MCP server | `mcp` SDK (Python) | **pure JSON-RPC 2.0, no rmcp dep**, 3 tools |
| HTTP serve | serve.py (health + SSE) | axum `/health` `/query` `/path` |
| Cache | mtime + hash | **blake3 CAS · parity-transparent** (hit == recompute) |
| Determinism | partial | sorted + seeded (**R4**, merge-driver-ready) |
| LLM backends | Anthropic·OpenAI·Gemini·Ollama·Bedrock (5) | `Backend` trait: **Noop(default·local-first)**·Ollama·OpenAI·**TIERWRIGHT** |
| Security | security.py | `core::guard` — **Trojan-Source/bidi escapes**, path-confine, secret-screen |
| watch · hooks · benchmark | yes | deferred (v1) |
| neo4j / cypher export | yes | deferred (cypher = stretch) |
| PDF / HTML ingest | pypdf · html2text | deferred (feature `pdf`) |
| Multi-platform install | cursor·gemini·copilot·claude | trimmed to **claude** target |
| **Factory integration** | none | **habitat L8** — POVM · injection.db · PV2 spheres · cc-pipe · TIERWRIGHT · arc-graph |
| **Parity / regression gate** | none | golden corpus + harness (**regression-only fail**) |
| Tests | (unknown) | **1225 · gate-green · pedantic-clean** |

**Where habitat-graph is AHEAD:** memory safety, rayon parallelism, determinism (R4), blake3 parity-cache,
MCP-without-heavy-dep, the core security guard, the parity gate, and the entire **factory L8 integration**.
**Where it deliberately trails (v1, reversible):** breadth of grammars + export formats + watch/hooks/benchmark
+ neo4j + PDF + multi-platform install — all dropped/deferred per `../ai_docs/01_GRAPHIFY_EXEMPLAR_MAP.md §D`
because the factory needs depth + integration over OSS-audience breadth.
