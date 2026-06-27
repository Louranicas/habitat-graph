# The 7 Most Powerful Use Cases of habitat-graph

> Back to: [[MOC]]. Ordered by leverage; each with the command that unlocks it.

## 1 · Live MCP organ for the agent fleet  *(highest leverage)*
```bash
habitat-graph mcp --graph og/graph.json
```
Makes the graph a **standing capability every Claude / the orchestrator can call** — `graph_query`,
`graph_path`, `graph_health` over JSON-RPC on stdio. Not "graph it once": *graphing is now a tool the
fleet has.* This is the original orchestrator-plugin goal, delivered.

## 2 · Instant codebase comprehension (cold-start onboarding)
```bash
habitat-graph extract the-orchestrator/src --out og
habitat-graph query SchedulerLoop --graph og/graph.json
```
Turn any service (up to the 137K-LOC orchestrator) into a queryable graph in seconds — definitions,
symbols, structure on demand. The fastest way for a human *or* agent to understand unfamiliar code.

## 3 · Refactor impact / blast-radius analysis
```bash
habitat-graph path Span NodeId --graph og/graph.json
```
"If I change X, what's downstream?" — trace reachability **before** you touch anything.

## 4 · Architecture drift & dependency-cycle detection
Leiden community clustering + `arc_graph`'s severed-ear diff surface module clusters and broken
producer→consumer arcs (serves the S1008620 bidi-wiring arc-coherence). Catches what `cargo check` can't.

## 5 · Parity-gated migration oracle
The golden corpus + parity harness diff a new graph against a frozen oracle (97/96% vs graphify),
failing **only on REGRESSION**. Proves *equivalence* when porting/rewriting — not just "it compiles."

## 6 · Semantic enrichment — concepts, not just code
The `Backend` trait (Ollama / OpenAI / **TIERWRIGHT** `:8201`) extracts concept nodes from
docs/comments/prose and merges them into the AST graph. Local-first by default; routes via the
factory's model router. Graph the *meaning*.

## 7 · Factory cognition feed (habitat L8)
`pv2_spheres` maps graph communities → PV2 Kuramoto spheres; `memory` writes graph summaries to POVM +
`injection.db` (no-risk-write); `obsidian_protocol` emits `Back-to` notes + `MASTER_INDEX`. The codebase
graph becomes a **first-class input to the habitat's cognitive substrates**.

---
**The single most powerful move:** #2 + #1 — `extract` any service, then `mcp`-serve it, turning
"understand this codebase" into a live organ the fleet queries continuously.
