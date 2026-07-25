> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · design: [[ai_docs/03_AUTOMATION_RUNBOOKS]] · strategy: [[ai_docs/00_DEPLOYMENT_PLAN]] §6 · framework: [[DEPLOYMENT_FRAMEWORK]] (G5 parity gate)

# PARITY_RUNBOOK — habitat-graph

> [!WARNING] STATUS 2026-07-25 (S1009385) — EVERY `just` STEP IN THIS RUNBOOK IS UNBUILT
> Verified by execution, not description. All four recipes this runbook sequences —
> `parity-refresh` · `golden-verify` · `parity` · `parity-report` — **do not exist** in
> `../justfile`; they are commented out under `# ⛔ QUARANTINED S1009385` markers, each with its own
> reason recorded there. **That block is the single source; this note does not copy it.**
> The parity *oracle itself* is also absent, so restoring the recipes alone would not make this
> runbook runnable — re-confirmed 2026-07-25: no `tools/` directory (so no `refresh-goldens.sh`,
> `golden-hash-check.sh`, or `PARITY_VERSIONS.txt`), no `tests/goldens/`, no
> `tests/fixtures/worked/`, no cargo feature named `parity`, and one `[[bin]]` target only
> (`habitat-graph`) — so `--bin parity-report` has nothing to build.
> Derive the live set at read time rather than trusting this note:
> ```bash
> just --justfile habitat-graph/justfile --working-directory habitat-graph --summary
> /usr/bin/grep -n 'QUARANTINED' habitat-graph/justfile   # the per-recipe reasons
> ```
> **Nothing below is deleted.** Read it as the *design* of the parity gate. Treat any claim
> elsewhere that "parity is enforced" as unsubstantiated until the oracle above is built: the
> load-bearing gate this runbook describes is currently not executable, and no phase can honestly
> be signed off on `0 REGRESSION` while it is absent.

**STATUS: PLANNING SKETCH (S1008796).** The load-bearing procedure of the whole refactor: prove the
Rust port emits graphs *equivalent* to Python graphify before any phase is declared done. A port
that "compiles and looks right" is not done — it is done when the golden diff is clean.

## Concept
`worked/` (graphify's own example corpus: example, httpx, karpathy-repos, mixed-corpus) is the
oracle. Python graphify's output on it = the **golden**. habitat-graph must reproduce it within a
documented tolerance. Diffs classify as:
- **EXACT** — identical node/edge/community sets.
- **SEMANTIC-EQUIVALENT** — differ only by id scheme / ordering / community label permutation. PASS.
- **REGRESSION** — missing/extra nodes or edges, wrong relations, wrong confidence. **FAILS the gate.**

## Preconditions
- [ ] Pinned Python graphify in an isolated venv (`uv tool install graphify==<pinned>`), recorded in `tools/PARITY_VERSIONS.txt`.
- [ ] `worked/` fixtures vendored under `tests/fixtures/worked/`.

## Steps — freeze goldens (maintainer, infrequent)
1. `just parity-refresh` → runs pinned Python graphify over each fixture, writes `tests/goldens/<fixture>/graph.json`.
2. Inspect for sanity; commit goldens with the pinned version stamped in the message.
3. `just golden-verify` records the golden hashes (drift guard).

## Steps — run parity (every phase, every build)
1. `just golden-verify` — confirm goldens unchanged (no silent golden edits).
2. `just parity` — habitat-graph builds each fixture graph + diffs vs golden; REGRESSION → non-zero exit.
3. `just parity-report` — human breakdown: per-fixture EXACT / SEMANTIC / REGRESSION counts.

## Verification / sign-off
- [ ] 0 REGRESSION across all fixtures for the layers in scope this phase.
- [ ] Any SEMANTIC-EQUIVALENT class is documented (why it's equivalent, not a bug).
- [ ] Independent re-run via `verify-receipt` / `agent-claim-verifier` (agents over-claim — re-run the diff, don't trust the summary).

## Phase scoping (see MIGRATION_RUNBOOK)
- P0: parity = byte-roundtrip a golden `graph.json` (schema only).
- P1: parity = node/edge sets (extraction).
- P2: parity = community structure (label-permutation equivalent).
- P3+: parity = exporter byte/structure match.

## Exit / escalation
- Persistent REGRESSION that is actually a graphify *bug* (port is more correct) → record as an
  intentional divergence in `tools/PARITY_DIVERGENCES.md` with rationale; downgrade from REGRESSION
  to documented-divergence only with explicit sign-off. Never silently reclassify.
