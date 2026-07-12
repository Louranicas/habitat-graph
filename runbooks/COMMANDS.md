> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · [[DEPLOY_RUNBOOK]] · [[EVIDENCE]]

# habitat-graph — Complete Command Reference (S1008796)

Every command for build · gate · run · publish · verify, copy-pasteable. Run from the repo root
`/home/louranicas/claude-code-workspace/habitat-graph` unless noted. Standalone repo — these never
touch the superproject.

## 0. Orientation
```bash
cd /home/louranicas/claude-code-workspace/habitat-graph
git status -sb && git log --oneline -5
git rev-parse HEAD
git remote -v                      # origin (github) · gitlab · no-mistakes
```

## 1. Quality gate (MANDATORY before every commit — 4 stages, zero tolerance)
```bash
CARGO_TARGET_DIR=./target cargo check --workspace 2>&1 | tail -20
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
cargo clippy --workspace --all-targets -- -D warnings -W clippy::pedantic 2>&1 | tail -20
CARGO_TARGET_DIR=./target cargo test --release 2>&1 | tail -30
# workspace test total:
CARGO_TARGET_DIR=./target cargo test --release 2>&1 | grep -E "^test result:" | awk '{s+=$4} END{print "TOTAL="s}'
```

### 1a. Feature-gated checks (habitat live adapters + backend net transport)
```bash
cargo clippy -p habitat-graph-habitat --all-targets --features live -- -D warnings -W clippy::pedantic
CARGO_TARGET_DIR=./target cargo test -p habitat-graph-habitat --features live
cargo clippy -p habitat-graph-backend --all-targets --features net  -- -D warnings -W clippy::pedantic
CARGO_TARGET_DIR=./target cargo test -p habitat-graph-backend --features net
```

### 1b. Per-crate test census
```bash
for c in core cache source extract backend build analyze export daemon serve cli habitat fixtures; do
  n=$(CARGO_TARGET_DIR=./target cargo test -p habitat-graph-$c --release 2>/dev/null \
        | grep -E "^test result:" | awk '{s+=$4} END{print s}')
  printf "%-10s %s\n" "$c" "${n:-0}"
done
```

## 2. Build & run the tool
```bash
CARGO_TARGET_DIR=./target cargo build --release -p habitat-graph-cli   # -> ./target/release/habitat-graph
BIN=./target/release/habitat-graph

$BIN --version
$BIN self-test                                  # tiny in-memory corpus
$BIN doctor                                     # engine + wiring diagnostics
$BIN extract crates/habitat-graph-core/src --out graphify-out   # self-host: 139 nodes / 48 edges / 97 communities
$BIN extract crates/habitat-graph-core/src --out graphify-out --svg --graphml --neo4j --wiki
$BIN update crates/habitat-graph-core/src --out graphify-out    # cached extraction + public-artifact refresh
$BIN query Confidence --graph graphify-out/graph.json
$BIN path Span NodeId   --graph graphify-out/graph.json
$BIN serve --graph graphify-out/graph.json --addr 127.0.0.1:7878   # HTTP /health /query?q= /path?from=&to=
$BIN mcp   --graph graphify-out/graph.json      # MCP (JSON-RPC 2.0) over stdio
```

All public artifacts use the same deterministic secret redaction before format-specific escaping;
node IDs, endpoints, counts, and communities are preserved. Raw `update`/`add` state is kept only in
owner-only private files. Generated wiki/vault pages and optional exports are refreshed only when a
hidden ownership manifest proves habitat-graph owns them; explicit exporter flags adopt existing
optional files, while malformed manifests or unowned collisions fail closed.

Source walks honor repository ignore rules. A scan outside Git honors only ignore files below its
scan root, so ambient parent/global configuration cannot change a staged corpus.

`add <URL>` requires a build with `--features live`, accepts only SSRF-checked public HTTP(S)
destinations, caps the body at 10 MiB and the exchange at 30 seconds, and refuses every redirect
rather than validating only the first hop. It also refuses a malformed or non-file existing graph;
it never converts parse failure into an empty graph and overwrites prior content.

### 2a. HTTP service smoke
```bash
curl -s 127.0.0.1:7878/health
curl -s '127.0.0.1:7878/query?q=Confidence'
curl -s '127.0.0.1:7878/path?from=Span&to=NodeId'
```

### 2b. MCP organ over stdio (the orchestrator-plugin interface)
```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize"}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"graph_health","arguments":{}}}' \
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"graph_query","arguments":{"query":"Confidence"}}}' \
  '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"graph_path","arguments":{"from":"Span","to":"NodeId"}}}' \
  | $BIN mcp --graph graphify-out/graph.json
```

## 3. Deployment — what was run (S1008796)

### 3a. Pre-push secret screen
```bash
git ls-files | grep -ivE '\.(rs|toml|md|json|lock|py)$|fixtures/|goldens/|raw/'   # expect only .gitignore + justfile
/usr/bin/rg -n -i -e 'BEGIN [A-Z ]*PRIVATE KEY' -e 'AKIA[0-9A-Z]{16}' -e 'ghp_[A-Za-z0-9]{30,}' \
  -e '(api[_-]?key|secret[_-]?key|password|access[_-]?token)\s*[:=]\s*["'"'"'][A-Za-z0-9/+_-]{16,}' \
  $(git ls-files | grep -vE 'guard/secrets\.rs|secrets|fixtures/|goldens/|raw/')   # no output = clean
```

### 3b. GitHub (private) — create + push
```bash
gh repo create Louranicas/habitat-graph --private --source=. --remote=origin --push \
  --description "Rust refactor of graphify into a 13-crate knowledge-graph workspace + MCP organ for the ULTRAPLATE factory"
```

### 3c. GitLab (private, SSH push-to-create)
```bash
ssh -T -o StrictHostKeyChecking=accept-new git@gitlab.com     # expect: Welcome to GitLab, @lukeomahoney!
git remote add gitlab git@gitlab.com:lukeomahoney/habitat-graph.git
git push -u gitlab main
```

### 3d. Port claim (coordination key — NOT factory.authorize.*)
```bash
atuin kv set --key port.claim.habitat-graph 8202     # 8202 free: next after TIERWRIGHT :8201
atuin kv get port.claim.habitat-graph                # read-back -> 8202
```

### 3e. no-mistakes gate (local pre-push validator; protects future pushes)
```bash
no-mistakes init                  # local bare-repo gate + post-receive hook (reversible: no-mistakes eject)
git push no-mistakes main         # run the validation pipeline (seal)
no-mistakes axi status            # run detail: steps, findings, outcome
no-mistakes status                # repo + daemon state
no-mistakes runs                  # run history
# respond to a review awaiting approval:
no-mistakes axi respond --action approve
no-mistakes axi respond --action fix  --findings <id,...>
no-mistakes axi logs   --step review --full
```

### 3f. Commit + push BOTH remotes (the routine cycle)
```bash
# gate first (§1) — must be green
git add <explicit paths>          # NEVER `git add .`
git commit -m "type(scope): subject (S1008796)
...
Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin main
git push gitlab main
```

## 4. Final verification battery (ground truth, not proxy)
```bash
H=$(git rev-parse HEAD)
[[ -z "$(git status --porcelain)" ]] && echo "tree CLEAN"
echo "github: $(git ls-remote origin refs/heads/main | awk '{print $1}')   (== $H ?)"
echo "gitlab: $(git ls-remote gitlab refs/heads/main | awk '{print $1}')   (== $H ?)"
gh repo view Louranicas/habitat-graph --json visibility -q .visibility       # PRIVATE
atuin kv get port.claim.habitat-graph                                         # 8202
curl -s -o /dev/null -w '%{http_code}\n' -m 1 http://localhost:8202/health    # 000 = free (reservation)
# + re-run the 4-stage gate (§1) — current count is recorded in EVIDENCE.md
```

## 5. Remaining one-way doors — Luke @ 0.A only (NOT part of the current deployment)
```bash
# OSS / public visibility flip (decides the OSS-upstream stance — irreversible/indexed):
gh repo edit Louranicas/habitat-graph --visibility public
# (GitLab public:)  via gitlab.com project settings, or:  glab repo edit --visibility public

# crates.io publish (token-gated, irreversible) — bottom-up by dependency layer, after `cargo login`:
for c in core cache source extract backend build analyze export daemon serve habitat cli; do
  ( cd crates/habitat-graph-$c && cargo publish ) || break        # core first; wait for each to index
done

# Run the daemon as a factory devenv service on the claimed port :8202:
#   1) add a [[services]] entry to ~/.config/devenv/devenv.toml:
#        command = "/path/to/habitat-graph serve --graph <graph.json> --addr 127.0.0.1:8202"
#   2) /usr/bin/cp -f ./target/release/habitat-graph <command= path>   # bare cp = trash alias
#   3) ~/.local/bin/devenv -c ~/.config/devenv/devenv.toml restart habitat-graph
#   4) cc-health                                                       # path-map aware health
```

## 6. Memory / anchors (slug `s1008796-habitat-graph-rust`)
```bash
sqlite3 ~/.local/share/habitat/injection.db \
  "SELECT id,chain_type,label FROM causal_chain WHERE label='s1008796-habitat-graph-rust';"   # id 254
hmem recall "habitat-graph"
# POVM ns habitat_graph (d0715a3f) · auto-mem session-1008796-habitat-graph-rust.md
# Obsidian: ~/projects/claude_code/habitat-graph — Rust Knowledge-Graph Engine (S1008796).md
```

---
*Command reference authored S1008796 · Claude @ cortex. Gold standard = deep-diff-forge.*
