# Leveraging Obsidian Graph View & Plugins

> Back to: [[MOC]] · [[The 7 Most Powerful Use Cases of habitat-graph]] · [[Commands & graphify Comparison (S1008796)]]

habitat-graph's `extract --vault <dir>` emits a graph-view-rich Obsidian vault: every node note carries
**YAML frontmatter** (`id/community/crate/lang/file/line/degree`) + **tags** (`#crate/…`, `#lang/…`,
`#community/…`) + **typed Dataview edges** (`relation:: [[target]]`, also read by Breadcrumbs). This turns
the codebase into a navigable, queryable, analysable graph — far richer than a static viewer.

```bash
habitat-graph extract crates --vault ~/habitat-codegraph
# Obsidian → "Open folder as vault" → ~/habitat-codegraph → Graph View
```

## The plugin stack — what each unlocks

| Plugin | Capability | Powered by |
|---|---|---|
| **Native Graph View** | force-directed graph; colour **groups** by `tag:#crate/…` / `#community/…`; per-note local graph; filters | tags |
| **Juggl** | cytoscape graph, **edges coloured by type** (calls vs imports vs inherits), styling, layouts | wikilinks + frontmatter |
| **Graph Analysis** | **centrality** (betweenness/HITS → real hubs), **link prediction**, co-citation | the link graph |
| **Dataview** | query as a DB: `TABLE degree FROM #community/380 SORT degree DESC`; "all callers of X" | `relation::` inline fields |
| **Breadcrumbs** | typed up/down **navigation + trails + dependency matrix/tree** | `relation::` typed edges |
| **Extended Graph / Graph Link Types** | edge **labels** + node images in the native graph | relation names |
| **Excalidraw** | hand/auto diagrams from notes | notes |

## Anything else useful — the bigger plays
1. **Graph the whole factory, not just code.** Emit the live **service stack + bidi-wiring arcs** (habitat L8 `obsidian_protocol` + `arc_graph`) as a vault → Obsidian's graph view *is* the live wiring/pipeline/bridges map.
2. **Severed-ear overlay.** `arc_graph` severed-ear diff → notes tagged `#severed-ear` → a Dataview query lists every broken producer→consumer bridge.
3. **Cross-service mega-vault.** `extract` all 19 services → one vault, colour by service → the entire factory in one graph.
4. **MCP-driven sub-vaults.** Ask the `mcp` organ → generate a focused vault ("everything reachable from `SchedulerLoop`").
5. **Ship a `.obsidian` preset** — auto-emit graph-view colour groups (by crate) + a Juggl style file so it opens pre-styled.

See also: [[graphify Feature-Parity Plan]] (`../ai_docs/08_GRAPHIFY_PARITY_PLAN_S1008796.md`) — the comprehensive plan to reach full graphify parity.
