# Discoveries, Learnings & Power Use-Cases (S1008796)

> Back to: [[MOC]] · siblings [[The 7 Most Powerful Use Cases of habitat-graph]] ·
> [[Commands & graphify Comparison (S1008796)]]. Canonical: `../ai_docs/07_DISCOVERIES_LEARNINGS_USECASES_S1008796.md`.

## Habitat-ops commands discovered (beyond the build/run set)
- **Health:** `cc-health` (path-map aware, 19/19), `curl :PORT/health` (ME = `:8180/api/health`).
- **Publish (both remotes):** `gh repo create … --private --source=. --remote=origin --push`; GitLab via
  SSH push-to-create (`git@gitlab.com:lukeomahoney/<repo>`, glab not needed). Verify **sha-agnostically**:
  `[ "$(git ls-remote origin refs/heads/main|awk '{print $1}')" = "$(git rev-parse HEAD)" ]`.
- **no-mistakes gate:** `no-mistakes init` → `git push no-mistakes main` → `no-mistakes axi status`
  (steps/findings/outcome); respond `--action {approve|fix|skip}`; `eject` to remove.
- **Coordination:** `atuin kv set --key port.claim.<svc> 8202` (NOT `factory.authorize.*`).
- **Render Mermaid:** mermaid.live (paste) or Obsidian; static SVG needs headless chromium (absent in sandbox).

## Scar-tissue learnings
1. **Parallel fibers in ONE crate can't self-gate honestly** — a per-file `--lib` grep reads clean when
   clippy aborts on a sibling's WIP. Barrier → **authoritative main-loop re-gate** `--all-targets` +
   `agent-claim-verifier` judge (which caught 13 hidden pedantic errors). See [[The 7 Most Powerful Use Cases of habitat-graph]] build story.
2. **no-mistakes review does real work** — caught a stale status banner and a `[VBE]` claim that pinned a
   sha as "== HEAD" which the writing commit staled. Word remote/HEAD claims **sha-agnostically**.
3. **guard-bash preserve-list is real** — `trash node_modules` in the registered workspace path was BLOCKED;
   `package.json`/`-lock` were tracked. Check `git ls-files --error-unmatch` before deleting; bypass
   `# CLAUDE:ALLOW-PRESERVE` only after proving junk.
4. **Trojan-Source: U+202E is `Cf` not `Cc`** — `sanitize_label` keeps it, `display_safe` escapes at render
   (storage-keeps / render-escapes). Pinned by test.
5. **`$?` after a pipe = last command's exit** — use `${PIPESTATUS[0]}`.
6. **GitLab push-to-create works over SSH** (private by default).
7. **Anti-pad** — 50-tests/module is release-eligibility on SUBSTANTIVE modules; thin adapters/leaves get
   meaningful-at-level coverage (padding re-tests shared logic = filler).
8. **Dependency inversion makes L8 testable without live services** — boundary trait + in-memory double;
   live adapters behind `--features live`.

## Power use-cases
→ full list in [[The 7 Most Powerful Use Cases of habitat-graph]]. Commands + graphify side-by-side →
[[Commands & graphify Comparison (S1008796)]].
