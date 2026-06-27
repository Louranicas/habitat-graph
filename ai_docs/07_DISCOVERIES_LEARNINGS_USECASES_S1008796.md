> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · [[EVIDENCE]] · commands: [[COMMANDS]] (runbooks)

# habitat-graph — Discoveries, Learnings & Power Use-Cases (S1008796)

Everything learned building + deploying habitat-graph this session. The narrow build/gate/run/publish
commands live in `runbooks/COMMANDS.md`; THIS doc adds the **habitat-ops commands discovered**, the
**scar-tissue learnings**, and the **7 most powerful use-cases**.

---

## A. Commands discovered (habitat-ops, beyond COMMANDS.md)

### A1. Health & situational awareness
```bash
cc-health                                  # path-map aware fleet health (19/19) — NOT hand-rolled curl loops
curl -s -o /dev/null -w '%{http_code}' -m 1 http://localhost:PORT/health   # one service (ME = :8180/api/health)
sqlite3 ~/.local/share/habitat/injection.db ".schema causal_chain"          # always .schema first
```

### A2. Git publication (standalone repos → BOTH remotes)
```bash
gh auth status
gh repo create Louranicas/<repo> --private --source=. --remote=origin --push --description "..."
gh repo view  Louranicas/<repo> --json visibility,defaultBranchRef,url
gh repo edit  Louranicas/<repo> --visibility public        # OSS flip (one-way door)
# GitLab (glab NOT installed → SSH push-to-create, makes a PRIVATE project):
ssh -T -o StrictHostKeyChecking=accept-new git@gitlab.com  # -> "Welcome to GitLab, @lukeomahoney!"
git remote add gitlab git@gitlab.com:lukeomahoney/<repo>.git
git push -u gitlab main
# Ground-truth verification (sha-AGNOSTIC — does not stale as HEAD advances):
[ "$(git ls-remote origin refs/heads/main | awk '{print $1}')" = "$(git rev-parse HEAD)" ] && echo MATCH
```

### A3. no-mistakes gate (local pre-push validator)
```bash
no-mistakes init                  # local bare-repo gate + post-receive hook (reversible: no-mistakes eject)
git push no-mistakes main         # run the validation pipeline (the seal); push step is config-skipped here
no-mistakes axi status            # structured run detail: steps[], findings[], outcome (passed/…)
no-mistakes status | runs | doctor
no-mistakes axi respond --action {approve|fix --findings <ids>|skip}
no-mistakes axi logs --step review --full
```

### A4. Coordination & memory substrates
```bash
atuin kv set --key port.claim.<svc> 8202 ; atuin kv get port.claim.<svc>    # coordination (NOT factory.authorize.*)
atuin kv list | grep port
# injection.db no-risk-write: snapshot -> idempotent insert (quoted heredoc, never shell-interpolate) -> read-back
# POVM ingest (MCP): mcp__povm-mcp__povm_ingest {namespace, source, content}
```

### A5. Rendering / viewing a Mermaid graph
```bash
# best (no install): paste block at https://mermaid.live  |  or open in Obsidian (native render)
# static SVG needs a headless chromium (NOT present in sandbox):
#   npx puppeteer browsers install chrome   # heavy
#   npx -y @mermaid-js/mermaid-cli -p pptr-no-sandbox.json -i in.mmd -o out.svg
# batcat/bat = syntax highlight only, NOT a renderer
```

---

## B. Scar-tissue learnings (this session)

1. **Parallel fibers in ONE crate cannot self-gate honestly.** A `forge-rust-coder-v4` fiber's per-file
   `cargo clippy -p CRATE --lib 2>&1 | grep myfile.rs` reads CLEAN when clippy aborts on a *sibling's*
   incomplete code — so all fibers reported green while the assembled crate had 13 pedantic errors. Fix:
   barrier → **authoritative main-loop re-gate of the whole crate `--all-targets`** + an `agent-claim-verifier`
   judge outside the loop (which caught it). The fiber seal is a *claim*, not verification.
2. **The no-mistakes review does real work.** On a docs commit it caught two genuine honesty bugs in my own
   EVIDENCE.md — a stale status banner, and a `[VBE]` claim that pinned a sha as "== HEAD" which the writing
   commit itself staled. Lesson: **word remote/HEAD claims sha-AGNOSTICALLY** (`ls-remote == rev-parse`,
   re-checked each commit) so they cannot self-stale.
3. **The guard-bash preserve-list is real and correct.** `trash node_modules` in the registered workspace
   path was BLOCKED — and rightly: `package.json`/`package-lock.json` were tracked (real), `node_modules`
   was pre-existing + gitignored. Always check `git ls-files --error-unmatch` + mtime before deleting; the
   bypass is `# CLAUDE:ALLOW-PRESERVE` (one-shot, audited) — use only after proving the target is junk.
4. **Trojan-Source: U+202E is `Cf` (format), not `Cc` (control).** `sanitize_label` strips control chars and
   therefore RETAINS bidi format chars; the core's two-layer policy escapes them at the render boundary via
   `display_safe`. Storage-keeps / render-escapes — pinned by test in both `backend` and `serve::mcp`.
5. **`$?` after a pipe is the LAST command's exit, not the pipeline's.** Use `${PIPESTATUS[0]}`. (Bit me on a
   port-scan and a secret-screen — the ground truth was "did any match lines print", not the captured code.)
6. **GitLab push-to-create works over SSH** (glab not needed); makes a private project by default — consistent
   with a private GitHub repo.
7. **Anti-pad discipline.** The 50-tests/module floor is a *release-eligibility* gate on SUBSTANTIVE modules,
   not per-commit. Thin I/O adapters / leaves (ingest, manifest, backend adapters over the shared `protocol`,
   daemon handlers) get meaningful-at-level coverage; padding them re-tests shared logic = filler.
8. **Dependency inversion is what makes L8 testable without live services.** Every habitat boundary is a trait
   + in-memory double; live adapters (rusqlite/ureq/process) behind `--features live`. The crate builds + tests
   green with NO POVM/PV2/orchestrator running; real writes only fire at runtime behind the flag.

---

## C. The 7 most powerful use-cases (full text → README + EVIDENCE)

1. **Instant codebase comprehension** — `extract <service>/src` graphs any service (up to the 137K-LOC
   orchestrator) in seconds; query symbols, find definitions, trace shortest paths. Fastest cold-start
   understanding for a human or an agent.
2. **Live MCP organ for the agent fleet** — `habitat-graph mcp` makes the graph a STANDING capability every
   Claude / the orchestrator can call (`graph_query`/`graph_path`/`graph_health` over JSON-RPC). Turns
   "understand this code" into a persistent tool. *(The original orchestrator-plugin goal.)*
3. **Refactor impact / blast-radius** — `shortest_path` + adjacency answers "if I change X, what's downstream?"
   BEFORE you touch it. De-risks refactors across a large tree.
4. **Architecture drift & cycle detection** — Leiden community clustering + `arc_graph` severed-ear diff surface
   module clusters and broken producer→consumer arcs (serves the S1008620 bidi-wiring arc-coherence). Catches
   what `cargo check` cannot.
5. **Parity-gated migration oracle** — the golden corpus + parity harness diff a new graph against a frozen
   oracle (97/96% vs graphify), failing only on REGRESSION. Proves equivalence when porting/rewriting, not
   just "it compiles."
6. **Semantic enrichment (concepts, not just code)** — the `Backend` trait (Ollama / OpenAI / TIERWRIGHT)
   extracts concept nodes from docs/comments/prose and merges them into the AST graph. Local-first by default;
   routes through the factory model router.
7. **Factory cognition feed (habitat L8)** — `pv2_spheres` maps graph communities → Kuramoto spheres; `memory`
   writes graph summaries to POVM + injection.db (no-risk-write); `obsidian_protocol` emits Back-to notes +
   MASTER_INDEX. The codebase graph becomes a first-class input to the habitat's cognitive substrates.

---
*Discoveries + learnings + use-cases, S1008796 · Claude @ cortex. Gold standard = deep-diff-forge.*
