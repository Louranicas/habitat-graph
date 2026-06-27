> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · design: [[ai_docs/03_AUTOMATION_RUNBOOKS]] · strategy: [[ai_docs/00_DEPLOYMENT_PLAN]] §6 · framework: [[DEPLOYMENT_FRAMEWORK]] (G5 parity gate)

# PARITY_RUNBOOK — habitat-graph

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
