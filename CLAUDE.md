# habitat-graph — Crate Charter

> Back to: [[CLAUDE.md]] (workspace root) · [[CLAUDE.local.md]] · framework: [[DEPLOYMENT_FRAMEWORK]] · [[MODULE_STRUCTURE_PLAN]]
> **STATUS: BUILT (S1008796).** 13-crate workspace, gate-green, 4173 all-targets tests / 0 failed. D0→D6 + semantic backend + MCP organ sealed (see `EVIDENCE.md`). D7 release gated on Luke @ 0.A (port-claim + remotes + no-mistakes seal).

## What this is

Rust refactor of `github.com/safishamsi/graphify` into a queryable knowledge-graph organ for the
ULTRAPLATE factory. Planning corpus: `README.md` → `ai_docs/00_DEPLOYMENT_PLAN.md` →
`01_GRAPHIFY_EXEMPLAR_MAP.md` → `02_HABITAT_INTEGRATION.md` → `03_AUTOMATION_RUNBOOKS.md` →
`plan.toml` + `ULTRAMAP.md` + `justfile` + `runbooks/`.

## Front door

`just <recipe>` (introspect: `just --dump --dump-format json`). Groups: `quality` (`just gate`) ·
`parity` (`just parity`) · `graph` · `deploy` · `habitat`. Operational procedures live in
`runbooks/` (`DEPLOY` · `PARITY` · `MIGRATION` · `INCIDENT`). This standalone repo owns its
`justfile`; the workspace-root justfile carries thin `habitat-graph-*` proxies only.

## Invariants (non-negotiable)

- **Standalone repo, own remotes ONLY** — never push to the superproject. (`feedback_morph_ir_engine_standalone_only`)
- **`forbid(unsafe)`** workspace-wide; the only `unsafe` is inside the upstream `tree-sitter` FFI, isolated in L3 `ast_*` wrappers.
- **No `unwrap`/`expect`** in lib code; `thiserror` taxonomy from L0 `core::error`.
- **≥50 meaningful tests/module** (no test-fitting); parity tests fed by the `worked/` golden corpus.
- **Local-first:** code extraction defaults to AST-only (no network). LLM path routes through TIERWRIGHT/Ollama — never a raw external call on source.
- **graph.json schema-compat (R2)** with graphify during migration.
- **Determinism (R4):** sorted node/edge ordering — required for the git merge driver.
- **Public/private boundary:** every public graph projection uses one deterministic secret-redaction
  policy without changing node IDs or topology; raw `update`/`add` state is owner-only and scoped to
  the output plus Git context.

## Quality gate (mandatory, before every commit)

```bash
CARGO_TARGET_DIR=./target cargo check 2>&1 | tail -20 && \
cargo clippy -- -D warnings 2>&1 | tail -20 && \
cargo clippy -- -D warnings -W clippy::pedantic 2>&1 | tail -20 && \
CARGO_TARGET_DIR=./target cargo test --lib --release 2>&1 | tail -30
```
check → clippy → pedantic → test. Zero tolerance. `${PIPESTATUS[0]}` per stage. `cargo-deny` + `cargo-audit` in CI. → `/gate`.

## Build discipline

- **Bottom-up by layer** (L0→L8 per `ULTRAMAP.md`); no phase collapse — each module gets impl + gate + tests before the layer above.
- **Parity-gated** (`00_DEPLOYMENT_PLAN.md` §6): each phase diffs against the Python golden corpus; only REGRESSION fails.
- **Crate split:** `habitat-graph-core` (L0–L6) / `habitat-graph` (L7) / `habitat-graph-habitat` (L8, feature-gated).

## Habitat wiring (L8, `--features habitat`)

devenv+health (port via `port-claim`, `cc-health` path-map) · POVM + injection.db memory writer ·
Obsidian `Back-to` protocol + `hmem rebuild` · orchestrator pipe verb (`cc-pipe`, ACK/NACK) ·
arc-graph extractor (serves S1008620 bidi-wiring) · PV2 sphere registrar · TIERWRIGHT backend.
Details: `ai_docs/02_HABITAT_INTEGRATION.md`.

## Gated on Luke @ 0.A

go/no-go · `port-claim` · repo remotes · backend policy · `factory.authorize.habitat-graph` arming ·
OSS-upstream stance. None auto-decided.

---
*Exemplar: safishamsi/graphify · Charter authored S1008796 · Claude @ cortex.*
