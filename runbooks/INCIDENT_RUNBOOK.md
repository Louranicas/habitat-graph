> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · design: [[ai_docs/03_AUTOMATION_RUNBOOKS]] · framework: [[DEPLOYMENT_FRAMEWORK]] (rollback + blocking rules)

# INCIDENT_RUNBOOK — habitat-graph

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
