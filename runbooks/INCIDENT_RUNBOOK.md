> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · design: [[ai_docs/03_AUTOMATION_RUNBOOKS]] · framework: [[DEPLOYMENT_FRAMEWORK]] (rollback + blocking rules)

# INCIDENT_RUNBOOK — habitat-graph

> [!WARNING] STATUS 2026-07-25 (S1009385) — MITIGATIONS BELOW CITE RECIPES THAT DO NOT EXIST
> Verified by execution, not description. Cited but **absent** from `../justfile`: `deploy`
> (failure-mode row 2) · `arc-graph` (row 4) · `rollback` (row 6 and the whole Mitigate section).
> Each is commented out under a `# ⛔ QUARANTINED S1009385` marker carrying its own reason —
> **that block is the single source; this note does not copy it.** Cited and **live** (confirmed by
> `just --dry-run`): `health` · `extract` · `build-release` · `mcp-register` · `gate`.
> **This matters most for Rollback.** "Prefer rollback over hot-fix under pressure" is the correct
> instinct, but `just rollback` is not a command that exists — there is no `tools/rollback.sh`
> (re-confirmed 2026-07-25: no `tools/` directory at all). Under real incident pressure this runbook
> would send an operator to a recipe that errors out. **There is currently no scripted rollback path.**
> Two further verified traps in the table below:
> - Row 4 tells you to "re-run `just arc-graph`" when the arc map is empty. `arc-graph` is
>   quarantined, so the arc map has **no producer** — while `just arc-coherence` IS live and reads
>   `graphify-out/arcs.json`, a file that does not exist. The consumer runs; the producer is absent.
> - Rows 2/6 presuppose a deployed service. habitat-graph has **no `[[services]]` entry** in
>   `~/.config/devenv/devenv.toml` (re-confirmed absent 2026-07-25), so there is no live service to
>   have an incident with yet. This runbook is forward-looking, not currently actionable.
> Derive the live set at read time rather than trusting this note:
> ```bash
> just --justfile habitat-graph/justfile --working-directory habitat-graph --summary
> /usr/bin/grep -n 'QUARANTINED' habitat-graph/justfile   # the per-recipe reasons
> ```
> **Nothing below is deleted** — it is retained as the intended procedure.

**STATUS: PLANNING SKETCH (S1008796).** Blameless incident response for habitat-graph in production.
Convention: architect-diagnostics blameless-runbook + postmortem pillar. Read-only forensics first;
no bare service spawns (sandbox reaps children — use `devenv restart` only).

## Triage (first 5 minutes — read, don't touch)
1. `just health` (`cc-health`) — is `/health` 200? node/edge counts sane or zero/garbage?
2. `journalctl`/devenv logs for the service — last lines, panic strings.
3. `git log -1` on the deployed binary's source; SHA at `command=` path vs HEAD (sha, not mtime).
4. `atuin kv get` recent habitat-graph state keys.

## Common failure modes
| Symptom | Likely cause | Mitigation |
|---|---|---|
| `/health` 200 but node count 0 | empty/corrupt `graph.json`; extract failed silently | rebuild: `just extract`; check L1 guard rejected all input |
| service down after deploy | wrong binary path / stale PID / bare-`cp` no-op | verify SHA at `command=` path; redeploy `just deploy`; `devenv restart` |
| MCP tools absent | `--features habitat` not built / not registered | rebuild `just build-release`; `just mcp-register` |
| arc-graph empty | bridge-idiom recognizer missed / source moved | re-run `just arc-graph`; check extractor rules vs current tree |
| PV2 spheres not appearing | sphere-id naming mismatch (the WFE/LCM trap) | verify naming convention; do NOT mutate field without Luke |
| breaker OPEN / soak regression | downstream bridge partner down | rollback `just rollback`; probe partner health |

## Mitigate
- Prefer **rollback** over hot-fix under pressure: `just rollback` (restore `.bak` + restart).
- Never `push --force` to recover; never edit a sealed service in place during an incident.

## RCA + postmortem (blameless)
After service restored, author `runbooks/postmortems/<date>-<slug>.md`:
- **Timeline** (detection → mitigation → resolution, timestamps).
- **Root cause** (the authoritative check that proved it — sha/PIPESTATUS/the tool's own db, not a proxy).
- **Contributing factors** · **What went well** · **Action items** (each owned, each a `just`/test guard so it can't recur).
- Persist: ai_docs + Obsidian + injection.db `causal_chain` row.

## Escalation
- Data-loss risk, irreversible action, or `factory.authorize.*` decision → Luke @ 0.A.
- Repeated same-cause incident → promote the guard into the gate (`just gate` / a parity test).
