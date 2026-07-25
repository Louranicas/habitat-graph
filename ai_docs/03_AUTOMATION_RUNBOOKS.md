> Back to: [[CLAUDE.md]] · [[CLAUDE.local.md]] · [[habitat-graph/README]] · spine: [[00_DEPLOYMENT_PLAN]] · framework: [[DEPLOYMENT_FRAMEWORK]]

# habitat-graph — Automation (justfile) & Runbooks (S1008796)

> [!WARNING] STATUS 2026-07-25 (S1009385) — §2 IS A DESIGNED TAXONOMY, NOT A LIVE INVENTORY
> This document is the *design* that specified the justfile. Its closing line already says the
> artifacts are PLANNING SKETCHES, but the §2 tables read as an inventory of what exists, and four
> runbooks now cite them as real automation. Verified by execution 2026-07-25: a substantial share
> of the §2.2–§2.5 recipes are **not built** and are commented out in `../justfile` under
> `# ⛔ QUARANTINED S1009385` markers, each carrying its own reason. **That block is the single
> source — this note deliberately does not reproduce it as prose.** Derive both sets at read time:
> ```bash
> just --justfile habitat-graph/justfile --working-directory habitat-graph --summary   # live
> /usr/bin/grep -n 'QUARANTINED' habitat-graph/justfile                                # quarantined + reasons
> ```
> **Nothing below is deleted.** Three findings the quarantine markers do *not* capture, recorded
> here because they change what the gap actually is:
>
> 1. **The export capability is ORPHANED, not missing.** The justfile marks `export`, `report`,
>    `benchmark`, `arc-graph`, `pv2-register`, `obsidian-export`, `memory-write` as "NOT a CLI
>    subcommand", which reads as *unimplemented*. That framing is wrong for the export family:
>    `crates/habitat-graph-export/src/` contains `json.rs` `html.rs` `svg.rs` `graphml.rs`
>    `cypher.rs` `obsidian.rs` `wiki.rs` `report.rs` `benchmark.rs` — the **library capability
>    EXISTS; the CLI wiring is ABSENT** (`crates/habitat-graph-cli/src/cli.rs` exposes Extract,
>    Update, Query, Path, Serve, Mcp, MergeDriver, InstallMergeDriver, InstallMcp, Install, Hook,
>    Watch, Add, SelfTest, Doctor — and no Export/Report/Benchmark). This is a **front-door wiring
>    gap over working code**, which is a much cheaper fix than a missing feature, and should not be
>    left recorded as absence.
> 2. **§2.5 `arc-coherence` has a live consumer with no producer.** `arc-coherence` resolves and
>    runs; it reads `graphify-out/arcs.json`; that file does not exist and `arc-graph`, the recipe
>    that would emit it, is quarantined.
> 3. **§1's proposed root proxies were never added.** `habitat-graph-gate` / `-arc` / `-health` are
>    absent from the workspace-root justfile (re-confirmed 2026-07-25), so §5's acceptance box
>    "Root-workspace justfile carries thin proxies" is correctly still unchecked. Note `-arc` would
>    proxy to the quarantined `arc-graph`, so it cannot be added as written.
>
> §5's acceptance checklist remains the right gate; it is simply not met yet. Treat §2 as the
> target-state specification and the two commands above as the only statement of current state.

The factory's front door is `just <recipe>` and its operational memory is runbooks. This document
designs both for habitat-graph, comprehensively, and is then assimilated into the spine
(`00_DEPLOYMENT_PLAN.md` §5/§7), `plan.toml`, `README.md`, and `CLAUDE.md`.

Artifacts authored alongside this design (as PLANNING SKETCHES):
`../justfile` · `../runbooks/DEPLOY_RUNBOOK.md` · `../runbooks/PARITY_RUNBOOK.md` ·
`../runbooks/INCIDENT_RUNBOOK.md` · `../runbooks/MIGRATION_RUNBOOK.md`.

---

## 1. The root-vs-standalone justfile tension (decide first)

The habitat charter lists an anti-pattern: **"Per-repo standalone justfile → Fold into root
`justfile`."** But habitat-graph is a **standalone-push repo** (own remotes only,
`feedback_morph_ir_engine_standalone_only`). These pull in opposite directions. Resolution:

**Decision (recommended): two-tier, thin-proxy.**
- habitat-graph ships **its own `justfile`** — it's a standalone product that must be usable by an
  external clone with no habitat root present. This is the *source of truth* for its recipes.
- The **workspace root `justfile`** gets a handful of **thin proxy recipes** that delegate into the
  submodule, so the habitat's single-front-door discoverability holds:
  ```just
  # in workspace root justfile (proxies only — never duplicate logic)
  habitat-graph-gate:  ; cd habitat-graph && just gate
  habitat-graph-arc:   ; cd habitat-graph && just arc-graph
  habitat-graph-health:; cd habitat-graph && just health
  ```
- This satisfies BOTH: standalone usability (the repo's own justfile) and the habitat front-door
  convention (root proxies). The anti-pattern it forbids is *duplicated logic* fragmented across
  files — proxies carry none. **Flag for Luke** (planning decision, low-risk).

All recipes must survive `just --dump --dump-format json` (the habitat introspection contract) — so
recipe names are stable, documented, and parameterized rather than positional-magic.

---

## 2. justfile recipe taxonomy (full inventory)

Grouped by `[group(...)]`. `default` lists. Variables: `cargo_target := "./target"`,
`out := "graphify-out"`, `corpus := "."`. Recipes mirror graphify's CLI surface where they wrap it.

### 2.1 `quality` — the mandatory gate
| Recipe | Does | Habitat ref |
|---|---|---|
| `gate` | the 4-stage zero-tolerance pipeline, `${PIPESTATUS[0]}` per stage | `/gate`, quality-gate skill |
| `check` / `clippy` / `pedantic` / `test` | individual stages (for fast iteration) | — |
| `fmt` | `cargo fmt --all` | — |
| `audit` | `cargo audit` | supply-chain (00 §4) |
| `deny` | `cargo deny check` | supply-chain |
| `watch` | `bacon` continuous check→clippy→pedantic chain | bacon-mastery skill |
| `cov` | optional coverage (`cargo llvm-cov`) — confirms ≥50-tests/module density | — |

### 2.2 `parity` — refactor correctness (the load-bearing group)
| Recipe | Does |
|---|---|
| `parity` | run habitat-graph over each `worked/` fixture, diff graph vs Python golden, fail on REGRESSION |
| `parity-refresh` | (maintainer) re-run pinned Python graphify over `worked/`, regenerate goldens |
| `parity-report` | human-readable EXACT / SEMANTIC-EQUIVALENT / REGRESSION breakdown |
| `golden-verify` | hash-check committed goldens unchanged (drift guard) |

### 2.3 `graph` — the product surface (wraps the CLI / exemplar)
| Recipe | Wraps graphify | Does |
|---|---|---|
| `extract dir=corpus` | `graphify extract` | AST-only build → `graph.json` |
| `query q` | `graphify query` | semantic search |
| `path a b` | `graphify path` | shortest path |
| `export fmt` | `graphify export` | html/svg/graphml/cypher/obsidian/wiki |
| `report` | `report.py` | `GRAPH_REPORT.md` |
| `serve` | `serve.py` | MCP server (stdio/SSE) |
| `watch-fs` | `watch.py` | notify-based rebuild loop |
| `hook-install` | `hooks.py` | git post-commit + merge driver |
| `benchmark` | `benchmark.py` | token: full corpus vs subgraph |

### 2.4 `deploy` — build + ship
| Recipe | Does | Trap encoded |
|---|---|---|
| `build` / `build-release` | cargo build (+ `--features habitat` for the service binary) | — |
| `deploy` | `/usr/bin/cp -f` to the `command=` path, then `devenv restart habitat-graph` | bare `cp`→trash alias; deploy to devenv path not cwd |
| `restart` | `devenv restart habitat-graph` | no bare service spawn |
| `soak dur=10m` | post-deploy soak probe (health + breaker + fitness delta) | soak skill |
| `rollback` | restore `.bak` binary + restart | SHIPWRIGHT P7 auto-rollback |

### 2.5 `habitat` — factory wiring (feature = "habitat")
| Recipe | Does | Wire |
|---|---|---|
| `health` | `cc-health`-style probe of `/health` (path-map aware, never hand-rolled curl) | 02 §1 |
| `mcp-register` | register `mcp__habitat-graph__*` | 02 §3 |
| `arc-graph` | run the L8 arc extractor → producer→consumer arc map | 02 §4 |
| `arc-coherence` | feed `.claude/scripts/arc-coherence-gauge.sh` with the arc map | 02 §4 / S1008620 |
| `pv2-register` | register Leiden communities as PV2 spheres (naming-trap guarded) | 02 §5 |
| `obsidian-export` | emit `Back-to` protocol vault + `hmem rebuild` | 02 §2 |
| `memory-write` | POVM pathway + injection.db chain (no-risk write regime) | 02 §2 |

### 2.6 `meta`
| Recipe | Does |
|---|---|
| `default` | `just --list` |
| `dump` | `just --dump --dump-format json` (habitat introspection contract) |
| `doc` | `cargo doc --no-deps --open` |
| `runbook name` | open the named runbook in `runbooks/` |

**Why recipes, not bare commands:** they encode the habitat scar-tissue once (PIPESTATUS, `/usr/bin/cp -f`,
`devenv restart`, `cc-health`, sphere-naming) so no operator re-learns a trap. The justfile IS the
crystallized operational discipline.

---

## 3. Runbook set (operational memory)

Runbooks are step-by-step, copy-pasteable, and assume nothing. Four, each with a distinct trigger.
Convention source: `factory-map/DEPLOY_RUNBOOK.md` (the "start coding-2" pattern), the SHIPWRIGHT
P0–P7 lane, and the architect-diagnostics blameless-runbook + postmortem pillar.

| Runbook | Trigger | Owner of the gate |
|---|---|---|
| `DEPLOY_RUNBOOK.md` | shipping a build to the live habitat | preflight → gate → arm-check → deploy → soak → rollback |
| `PARITY_RUNBOOK.md` | proving a phase equivalent to Python graphify | the golden-corpus diff (REGRESSION = stop) |
| `MIGRATION_RUNBOOK.md` | walking the P0→P6 strangler sequence | per-phase parity + coexistence with Python |
| `INCIDENT_RUNBOOK.md` | habitat-graph misbehaves in production | triage → mitigate → RCA → blameless postmortem |

Each runbook: **Preconditions · Steps (numbered, with the exact `just`/CLI command) · Verification ·
Rollback/Exit · Escalation.** Live-actuation steps (deploy, arm) explicitly hand off to Luke @ 0.A —
a runbook never silently arms `factory.authorize.*`.

---

## 4. How automation maps onto the wright lineage

| Stage | Tool | Recipe/runbook |
|---|---|---|
| Plan | PLANWRIGHT / this corpus | — |
| Draught | DRAUGHTWRIGHT (design corpus) | `ai_docs/*` |
| Weave | LOOMWRIGHT (scaffold from `plan.toml`) | `just` recipes generated alongside scaffold |
| Forge | forge skill / `just build` | `quality` + `deploy` groups |
| Ship | SHIPWRIGHT / `deploy-loop` | `DEPLOY_RUNBOOK.md` |
| Verify | no-mistakes / verify-receipt | `parity` group + `PARITY_RUNBOOK.md` |

The justfile is the operator's interface to every stage; the runbooks are the procedures that
sequence the recipes with judgment + human gates.

---

## 5. Acceptance (the automation done-gate)

- [ ] `just --dump --dump-format json` parses (introspection contract holds).
- [ ] `just gate` = the canonical 4-stage pipeline, exit-code faithful.
- [ ] `just parity` runs the golden diff and fails on any REGRESSION.
- [ ] Root-workspace justfile carries thin proxies (`habitat-graph-*`), zero duplicated logic.
- [ ] All 4 runbooks present, each with Preconditions/Steps/Verification/Rollback/Escalation.
- [ ] No runbook step auto-arms `factory.authorize.*`; every live actuation hands off to Luke.
- [ ] `just deploy` encodes the `/usr/bin/cp -f` + `devenv restart` + path-map traps.

*Design authored S1008796. Artifacts are PLANNING SKETCHES — no recipe is run, no runbook actuated,
in this phase.*
