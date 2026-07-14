> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · design: [[ai_docs/03_AUTOMATION_RUNBOOKS]] · framework: [[DEPLOYMENT_FRAMEWORK]] (gate stack G0–G10)

# DEPLOY_RUNBOOK — habitat-graph

**STATUS: BUILD COMPLETE, PUBLICATION GATED (S1008796).** The crate exists and is gate-green
(13 crates, all-targets tests green / 0 failed — current count in `EVIDENCE.md` — pedantic-clean). What remains is the **Publication
(G9)** sequence below + the live devenv deploy — every step of which is **Luke @ 0.A authority**
(outward / irreversible). Mirrors `factory-map/DEPLOY_RUNBOOK.md` + SHIPWRIGHT P0–P7. **No step here
auto-arms `factory.authorize.*` and Claude never runs the push or the seal** — it only prepares them.

## Preconditions
- [x] Standalone repo, branch `main`, working tree clean; gate-green (count in `EVIDENCE.md`).
- [x] `factory.authorize.habitat-graph` = `armed` (read-only check confirmed; Luke set it).
- [ ] Port assigned via `port-claim` (recommend **8202** — next free after TIERWRIGHT `:8201`; 8145–8150 also free); `[[services]]` entry in `~/.config/devenv/devenv.toml`.
- [ ] Remotes created + added (Publication §G9). Currently **no remote configured**.

## Publication (G9) — standalone push + no-mistakes seal — GATED on Luke @ 0.A
Repo is one command from publishable. Standalone-only: **never** push to the superproject.
Claude has prepared these; Luke runs them (each is outward / irreversible):

```bash
# (a) Port-claim — recommended 8202 (free; sequential after TIERWRIGHT :8201)
atuin kv set --key port.claim.habitat-graph 8202

# (b) Create EMPTY github.com/Louranicas/habitat-graph + gitlab repos, then wire + push:
cd /home/louranicas/claude-code-workspace/habitat-graph
git remote add origin git@github.com:Louranicas/habitat-graph.git
git remote add gitlab git@gitlab.com:Louranicas/habitat-graph.git
git push -u origin main
git push -u gitlab main

# (c) No-mistakes publication seal (REAL mode — WFE×LCM gate):
just nm-converge        # or /no-mistakes
#   crates.io publish (token-gated, IRREVERSIBLE) only AFTER the seal passes.
```
Verify after push: `git remote -v` shows both; GitHub/GitLab show 13 commits on `main`; the
no-mistakes receipt is green. Then proceed to the live-deploy Steps below.

## Steps
0. **Preflight** — `just dump` (introspection OK), `just golden-verify` (goldens intact).
1. **Gate** — `just gate`. Must be green (check→clippy→pedantic→test, `${PIPESTATUS[0]}`). Red → fix, do not proceed.
2. **Parity** — `just parity`. 0 REGRESSION across `worked/` corpus. Any REGRESSION → STOP.
3. **Pre-deploy hardening** — `pre-deploy-hardening` skill (security + perf + silent-failure + zen) on the staged diff. APPROVE×4 required.
4. **Arm check** — `atuin kv get factory.authorize.habitat-graph` MUST equal `armed`. Read, never write.
5. **Build** — `just build-release` (includes `--features habitat`).
6. **Deploy** — `just deploy`. Encodes `/usr/bin/cp -f` (bare `cp`=trash alias) to the devenv `command=` path, then `devenv restart`.
7. **Health** — `just health` (`cc-health`, path-map aware). Expect `/health` 200 + sane node/edge counts.
8. **Soak** — `just soak dur=10m`. Watch health stability, bridge breaker state, no fitness regression.

## Verification
- [ ] `cc-health` shows habitat-graph UP.
- [ ] `mcp__habitat-graph__*` callable.
- [ ] Binary SHA at the `command=` path matches the freshly built release (sha, not mtime — `shortcut-over-ground-truth`).

## Rollback
- `just rollback` — restore `.bak` binary + `devenv restart`. Then RCA via `INCIDENT_RUNBOOK.md`.

## Escalation
- Arming unset / ambiguous scope / irreversible action → hand to Luke @ 0.A.
- Soak regression → rollback first, investigate second.
