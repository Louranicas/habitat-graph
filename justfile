# habitat-graph — justfile (FRONT DOOR)
# STATUS: PLANNING SKETCH (S1008796). Recipes are designed, not yet runnable (no crate exists).
# Source of truth for this STANDALONE repo's recipes; the workspace-root justfile carries thin
# proxies only (`habitat-graph-*`). Design: ai_docs/03_AUTOMATION_RUNBOOKS.md.
# Introspection contract: every recipe must survive `just --dump --dump-format json`.

set shell := ["bash", "-uc"]

cargo_target := "./target"
out          := "graphify-out"
corpus       := "."
service      := "habitat-graph"

# ── meta ──────────────────────────────────────────────────────────────────────
default:
    @just --list

# habitat introspection contract
dump:
    @just --dump --dump-format json

doc:
    cargo doc --no-deps --open

runbook name:
    @batcat runbooks/{{name}}.md 2>/dev/null || cat runbooks/{{name}}.md

# ── quality (mandatory gate) ──────────────────────────────────────────────────
# [group: quality] 4-stage zero-tolerance pipeline; abort per-stage on ${PIPESTATUS[0]}
gate:
    #!/usr/bin/env bash
    set -uo pipefail
    CARGO_TARGET_DIR={{cargo_target}} cargo check 2>&1 | tail -20; [ ${PIPESTATUS[0]} -eq 0 ] || exit 1
    cargo clippy -- -D warnings 2>&1 | tail -20;                   [ ${PIPESTATUS[0]} -eq 0 ] || exit 1
    cargo clippy -- -D warnings -W clippy::pedantic 2>&1 | tail -20; [ ${PIPESTATUS[0]} -eq 0 ] || exit 1
    CARGO_TARGET_DIR={{cargo_target}} cargo test --lib --release 2>&1 | tail -30; [ ${PIPESTATUS[0]} -eq 0 ] || exit 1

check:    ; CARGO_TARGET_DIR={{cargo_target}} cargo check
clippy:   ; cargo clippy -- -D warnings
pedantic: ; cargo clippy -- -D warnings -W clippy::pedantic
test:     ; CARGO_TARGET_DIR={{cargo_target}} cargo test --lib --release
fmt:      ; cargo fmt --all
audit:    ; cargo audit
deny:     ; cargo deny check
watch:    ; bacon                      # continuous check→clippy→pedantic (bacon-mastery)
cov:      ; cargo llvm-cov --lib

# ── parity (refactor correctness — load-bearing) ──────────────────────────────
# [group: parity]  see runbooks/PARITY_RUNBOOK.md
parity:         ; cargo test --features parity -- --nocapture     # diff vs frozen goldens; REGRESSION fails
parity-report:  ; cargo run --features parity --bin parity-report
parity-refresh: ; ./tools/refresh-goldens.sh                      # maintainer: re-freeze from pinned Python graphify
golden-verify:  ; ./tools/golden-hash-check.sh                    # drift guard on committed goldens

# ── graph (product surface; wraps the CLI) ────────────────────────────────────
# [group: graph]
extract dir=corpus: ; cargo run --release -- extract {{dir}} --no-llm --out {{out}}
query q:            ; cargo run --release -- query "{{q}}"
path a b:           ; cargo run --release -- path {{a}} {{b}}
export fmt:         ; cargo run --release -- export {{fmt}} --out {{out}}
report:             ; cargo run --release -- report --out {{out}}
serve:              ; cargo run --release --features habitat -- serve
watch-fs:           ; cargo run --release -- watch {{corpus}}
hook-install:       ; cargo run --release -- hook install
benchmark:          ; cargo run --release -- benchmark {{corpus}}

# ── deploy (build + ship; encodes habitat traps) ──────────────────────────────
# [group: deploy]  see runbooks/DEPLOY_RUNBOOK.md
build:         ; CARGO_TARGET_DIR={{cargo_target}} cargo build
build-release: ; CARGO_TARGET_DIR={{cargo_target}} cargo build --release --features habitat
# deploy: /usr/bin/cp -f (bare cp = trash alias = silent no-op); to the devenv command= path
deploy: build-release
    /usr/bin/cp -f {{cargo_target}}/release/{{service}} "$(just _command-path)"
    devenv restart {{service}}
restart:      ; devenv restart {{service}}
soak dur="10m": ; cargo run --release --features habitat --bin soak -- --service {{service}} --dur {{dur}}
rollback:     ; ./tools/rollback.sh {{service}}              # restore .bak + restart (SHIPWRIGHT P7)
_command-path: ; @grep -A3 '{{service}}' ~/.config/devenv/devenv.toml | grep command | cut -d'"' -f2

# ── habitat (factory wiring; feature = habitat) ───────────────────────────────
# [group: habitat]  see ai_docs/02_HABITAT_INTEGRATION.md
health:          ; cc-health {{service}}                    # path-map aware; never hand-rolled curl
mcp-register:    ; cargo run --release --features habitat -- install --platform claude
arc-graph:       ; cargo run --release --features habitat -- arc-graph --out {{out}}/arcs.json
arc-coherence:   ; bash ../.claude/scripts/arc-coherence-gauge.sh --source {{out}}/arcs.json
pv2-register:    ; cargo run --release --features habitat -- pv2-register   # naming-trap guarded
obsidian-export: ; cargo run --release --features habitat -- export obsidian --protocol back-to && hmem rebuild
memory-write:    ; cargo run --release --features habitat -- memory-sync    # POVM + injection.db (no-risk regime)
