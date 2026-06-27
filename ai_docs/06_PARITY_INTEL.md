> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · framework: [[DEPLOYMENT_FRAMEWORK]] · runbook: [[PARITY_RUNBOOK]] · spine: [[00_DEPLOYMENT_PLAN]]

# habitat-graph — Parity Intel (from graphify's committed goldens, S1008796)

Discovered by reading graphify's actual committed `worked/*/graph.json` (no Python install needed —
the goldens ship in the repo). This **corrects two planning assumptions** and grounds the parity work.

## 1. The goldens ship committed (no Python graphify install)

| corpus | committed golden | language |
|---|---|---|
| `worked/httpx/graph.json` | ✓ + `GRAPH_REPORT.md` | Python |
| `worked/karpathy-repos/graph.json` | ✓ | Python |
| `worked/mixed-corpus/graph.json` | ✓ | Python + Markdown |
| `worked/example/` | ✗ (raw sources only) | Python |

→ **PARITY_RUNBOOK update:** skip the "install pinned Python graphify + freeze" step — *vendor the
committed goldens directly*. They are the oracle.

## 2. graphify's `graph.json` is NetworkX node-link — NOT our schema

```jsonc
{ "directed": true, "multigraph": false, "graph": {…},
  "nodes": [ { "label":"client.py", "file_type":"code", "source_file":"worked/httpx/raw/client.py",
               "source_location":"L1", "id":"client", "community":1 } ],
  "links": [ { "relation":"imports_from", "confidence":"EXTRACTED", "source_file":"…", "source_location":"L6",
               "weight":1.0, "_src":"client", "_tgt":"models", "source":"client", "target":"models" } ] }
```

Differences from our `core::schema` (00 §3 R2):
- envelope = `nodes` + **`links`** (not `edges`), with `directed`/`multigraph`/`graph` (NetworkX).
- `source_location` is a **line string `"L16"`**, not a `Span{start_byte,…}`.
- node carries `file_type`, a string `id` (`"client"`, `"client_timeout"`), and an **inline `community`** int (no separate communities array).
- link carries `weight` (1.0 EXTRACTED / 0.8 INFERRED) + duplicated `_src/_tgt` + `source/target`.

→ **Correction:** "R2 byte-compat" is too strong. **Parity = CONTENT-equivalence** (PARITY_RUNBOOK
SEMANTIC-EQUIVALENT class), compared via a **golden-adapter** that normalizes BOTH graphify's
node-link JSON and our output to: `{ node-set keyed by id|label, edge-set keyed by
(source,target,relation,confidence), community membership }`. We may additionally add a node-link
**exporter** (`export::json`) for true interop, but it is not required for the parity gate.

## 3. graphify's extraction taxonomy (the canonical target)

Nodes are **files AND top-level symbols**:
- file node: `id` = basename without extension (`client.py` → `"client"`), `file_type` = `"code"`/`"doc"`.
- symbol node: `id` = `"<file>_<symbol>"` (`"client_timeout"`), `source_location` = its line.

Relations (the `relation` vocabulary, partial): `imports_from` (file→file), `contains` (file→symbol),
plus calls/usage edges. `confidence` ∈ {`EXTRACTED` (weight 1.0), `INFERRED` (0.8)}.

Scale (httpx): ~175 nodes, ~400+ links, 6 communities (0–5).

→ **The canonical node/edge taxonomy = graphify's** (files+symbols · imports_from/contains/calls ·
line locations · EXTRACTED/INFERRED). The Python extractor must reproduce it to hit parity; the Rust
extractor (built first, self-tested) should be **aligned to this same taxonomy** for cross-language
consistency — a follow-up before the parity gate, tracked in the task list.

## 4. Revised P1 parity sequence

1. Rust extractor (in flight) — establishes the tree-sitter machinery + Extractor trait (self-tested).
2. **Python extractor** (`tree-sitter-python`) — reproduce graphify's taxonomy (the parity-critical grammar).
3. `fixtures` crate — vendor the 3 committed goldens + the golden-adapter + content-diff classifier.
4. **First parity gate (G5)** — 0 REGRESSION on node/edge CONTENT vs `httpx` golden, then the others.
5. Align the Rust extractor's node/edge taxonomy to the canonical form.

*Parity intel authored S1008796 · Claude @ cortex. Source: graphify committed `worked/*/graph.json`.*
