> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · design: [[ai_docs/03_AUTOMATION_RUNBOOKS]] · framework: [[DEPLOYMENT_FRAMEWORK]] (gate stack G0–G10)

# DEPLOY_RUNBOOK — habitat-graph

**STATUS: PLANNING SKETCH (S1008796).** Procedure for shipping a build to the live habitat once the
crate exists. Mirrors `factory-map/DEPLOY_RUNBOOK.md` + SHIPWRIGHT P0–P7. **No step here auto-arms
`factory.authorize.*`** — arming is Luke @ 0.A only.

## Preconditions
- [ ] On the standalone repo (own remotes); working tree clean (`git status`).
- [ ] `factory.authorize.habitat-graph` = `armed` (read-only check; Luke sets it). If unset → STOP, hand off.
- [ ] Port assigned via `port-claim`; `[[services]]` entry present in `~/.config/devenv/devenv.toml`.

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
