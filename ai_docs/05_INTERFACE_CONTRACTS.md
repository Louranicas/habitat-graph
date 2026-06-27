> Back to: [[CLAUDE.md]] · [[CLAUDE.local.md]] · [[habitat-graph/README]] · spine: [[00_DEPLOYMENT_PLAN]] · framework: [[DEPLOYMENT_FRAMEWORK]] · [[MODULE_STRUCTURE_PLAN]] · decision: [[04_CACHING_INCREMENTAL_CLUSTER]]

# habitat-graph — Interface Contracts (Spike, S1008796)

**STATUS: PLANNING / pre-arm spike.** Authored while `factory.authorize.habitat-graph` is **unset** —
this is *design*, within the autonomous envelope; no crate, no scaffold, no `cargo`. Its purpose: make
the armed build **zero-touch** by fixing every type/wire/error contract the ~13 crates implement, so
the build resolves no ambiguity at runtime. Each contract is a *target the parity + contract gates
(G5/G6) assert against.*

---

## 1. Core schema — `graph.json` (R2: byte-compatible with graphify)

```rust
// habitat-graph-core::schema  — the wire truth; serde-stable, sorted, deterministic (R4)
pub struct Graph {
    pub schema: &'static str,        // "habitat-graph.graph.v0"
    pub nodes: Vec<Node>,            // sorted by NodeId
    pub edges: Vec<Edge>,            // sorted by (source, target, relation)
    pub communities: Vec<Community>, // Leiden output; sorted by CommunityId
    pub manifest: Manifest,
}
pub struct Node { pub id: NodeId, pub label: String, pub source_file: PathBuf, pub source_location: Span }
pub struct Edge { pub source: NodeId, pub target: NodeId, pub relation: String, pub confidence: Confidence }
pub enum  Confidence { Extracted, Inferred, Ambiguous }   // serde: "EXTRACTED"|"INFERRED"|"AMBIGUOUS"
pub struct Community { pub id: CommunityId, pub label: String, pub members: Vec<NodeId> }
pub struct Manifest { pub inputs: Vec<InputRecord>, pub tool_version: String, pub generated_at: Option<String> }
pub struct Span { pub start_byte: u32, pub end_byte: u32, pub start_line: u32, pub end_line: u32 }
```
**Invariants:** `NodeId` is interned + stable across runs given identical input; all `Vec`s sorted
(diff-minimal for the merge driver); `generated_at` is the ONLY non-deterministic field and is omitted
under `--deterministic` (parity mode). JSON is a complete document; never partial.

## 2. Salsa query graph — the incremental contract (`habitat-graph-cache`)

The pipeline expressed as salsa inputs + tracked functions (current salsa API shape: `#[salsa::input]`,
`#[salsa::tracked]`, `#[salsa::interned]`). Granularity is the contract: change one file → only its
extraction + the transitively-dependent communities recompute.

```rust
#[salsa::input]   struct SourceFile  { path: PathBuf, content_hash: Hash, text: Arc<str> }
#[salsa::input]   struct Corpus      { roots: Vec<PathBuf>, config_hash: Hash }
#[salsa::interned] struct NodeKey    { label: String, file: PathBuf }

#[salsa::tracked] fn extract(db, f: SourceFile) -> Extraction;          // per-file  (L3)
#[salsa::tracked] fn assemble(db, c: Corpus)    -> Graph;               // fan-in    (L4)
#[salsa::tracked] fn communities(db, c: Corpus) -> Vec<Community>;      // Leiden    (L5)
#[salsa::tracked] fn centrality(db, c: Corpus)  -> CentralityMap;       // (L5)
#[salsa::tracked] fn subgraph(db, c: Corpus, scope: Scope) -> Subgraph; // demand-driven (query/map.scope)
#[salsa::tracked] fn shortest_path(db, c, a: NodeKey, b: NodeKey) -> Option<Vec<NodeKey>>;
```
**Contract:** tracked-fn outputs are pure functions of their salsa inputs. `extract` keys on
`SourceFile.content_hash`; editing whitespace-only changes that don't alter the AST must NOT
invalidate downstream (the hash is over normalized content). Coarse CAS (P0/P1) is a drop-in for the
salsa layer with the same key discipline.

## 3. Cache key + the parity-transparency invariant (`cache::cas`, `cache::key`)

```text
key(stage, inputs, config) = blake3( stage_id ‖ Σ input_hashes ‖ config_hash )
```
**INVARIANT (G5-asserted):** `∀ stage, input.  read_cached(key) ≡ recompute(stage, input)` — **byte for
byte**. A cache hit that differs from a cold recompute is a REGRESSION, not a "cache bug." The harness
proves it: every parity fixture runs twice (cold DB, then warm DB) and the two `graph.json` outputs must
be identical. If caching can ever change output, caching is wrong — never the goldens.

## 4. CLI contract (`habitat-graph-cli`)

| Command | stdout | exit |
|---|---|---|
| `extract <dir> [--no-llm] [--json] [--deterministic]` | `graph.json` (or summary) | 0 ok · 2 bad-args · 4 extract-error |
| `query "<q>" [--json] [--k N]` | `Subgraph` | 0 · 4 no-graph |
| `path <A> <B> [--json]` | node list or `null` | 0 · 4 |
| `export <fmt> [--out DIR]` | path written | 0 · 2 bad-fmt |
| `serve [--daemon] [--features habitat]` | — (MCP/HTTP) | 0 · 6 needs-tty(n/a) |
| `--self-test` · `doctor` | diagnostic JSON | 0 · 1 |
**Rules:** machine commands need no TTY; stdout = output, stderr = diagnostics; JSONL = one event/line;
exit codes are the documented contract (G6). `--deterministic` omits `generated_at` (parity).

## 5. MCP tools (`habitat-graph-serve`, `rmcp`)

```jsonc
// tools/list →
"query"     {question: string, k?: int}            -> Subgraph
"path"      {from: string, to: string}             -> {path: NodeKey[] | null}
"subgraph"  {scope: string}                        -> Subgraph
"subscribe" {query: string}                        -> {sub_id: string}      // → server pushes Delta events
```
`Subgraph = {nodes: Node[], edges: Edge[], communities: Community[], confidence: Confidence}`.
Errors use JSON-RPC codes: `-32602` invalid params, `-32601` method not found, `4` graph-error.

## 6. Daemon UDS protocol (`habitat-graph-daemon`) — JSON-RPC over Unix socket

Socket: `$XDG_RUNTIME_DIR/habitat-graph/habitat-graph.sock`, mode **0o600** (owner-private; the morphd
discipline). Framing: line-delimited JSON-RPC 2.0.

```text
methods:
  graph.build {corpus}              -> {nodes:int, edges:int, communities:int, build_ms:int}
  graph.query {question, k}         -> Subgraph
  graph.path  {from, to}            -> {path|null}
  session.open {}                   -> {session_id}     · session.close {session_id} -> {}
  subscribe   {query}               -> {sub_id}         · unsubscribe {sub_id} -> {}
  daemon.health {}                  -> {version, pid, sessions:int, cache:{entries,bytes}, backend_mode}
  daemon.stop  {}                   -> {} (then exits, removes socket)
notifications (server→client):
  delta {sub_id, added:Edge[], removed:Edge[], rev:int}   // pushed on salsa red-green invalidation
```
**Contract:** the daemon owns the salsa DB; one-shot CLI uses an ephemeral DB (Design Rule 6 — daemon
never required for correctness). Persistence: DB snapshot to `$XDG_STATE_HOME/habitat-graph/`, warm on
boot; a cold boot rebuilds and is still correct. `daemon.health` is the `cc-health` source.

## 7. Orchestrator pipe verb (`habitat-graph-habitat::orchestrator_pipe`)

Speaks the plugin's existing contract — we do not invent a new one.
```jsonc
// request  (cc-pipe nexus -- habitat-graph map.scope …)
{"verb":"map.scope","mission":"<text>","k":25}
// response (ACK)
{"nodes":[{"id","label","file","owner"}],"arcs":[Arc],"communities":[{"id","label","members"}],
 "confidence":"EXTRACTED|INFERRED|AMBIGUOUS"}
// malformed request            → NACK_SCHEMA_INVALID
// answer needs sidecar routing  → NACK_USE_SIDECAR_SUBMIT
```

## 8. Arc-graph contract (`habitat-graph-habitat::arc_graph`) — serves S1008620

```rust
pub struct Arc { pub producer: NodeId, pub consumer: NodeId,
                 pub transport: Transport, pub payload: String, pub sealed: bool }
pub enum Transport { Http, Uds, Pipe, Kv }
// severed-ear delta: producer node hot (out-degree>0) with NO inbound edge at the named consumer.
fn severed_ears(prev: &[Arc], next: &[Arc]) -> Vec<Arc>;   // feeds .claude/scripts/arc-coherence-gauge.sh
```
Frontier lane (`--features incremental-view`): `Vec<Arc>` becomes a differential-dataflow materialized
view; `severed_ears` updates per-commit in µs instead of a batch rescan.

## 9. Error taxonomy (`core::error`, thiserror)

```rust
pub enum GraphError {
  Io(..), Parse{file, msg}, Guard(GuardError), Schema{msg}, Backend{msg}, Cache{msg}, Daemon{msg},
}
```
No `unwrap`/`expect` in lib; every fallible boundary returns `Result<_, GraphError>`; the CLI maps
variants → the §4 exit codes. (Guard escapes Trojan-Source/bidi at every render boundary — DDF lesson.)

---

## Build-readiness check (what this contract unblocks)

When armed, the build implements *these signatures* — there is no design left to resolve at runtime:
- [ ] §1 schema is the first thing built (P0); the parity harness round-trips it before anything else.
- [ ] §3 invariant is a test, written before the cache (G5 gates it).
- [ ] §4–§7 are the contract probes (G6) — written as failing tests first, then made to pass.
- [ ] §6 daemon protocol is the D5 acceptance surface; §8 arc-graph is the D6/S1008620 payoff.

*Interface-contract spike authored S1008796 · Claude @ cortex · gold standard = Louranicas/deep-diff-forge.
Pre-arm design; the build that implements it is gated on `factory.authorize.habitat-graph` (Luke @ 0.A).*
