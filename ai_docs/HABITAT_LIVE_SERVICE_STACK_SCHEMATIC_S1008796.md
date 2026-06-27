> Back to: [[CLAUDE.md]] · [[habitat-graph/README]] · [[EVIDENCE]] · [[02_HABITAT_INTEGRATION]]

# ULTRAPLATE Habitat — Live Service Stack (end-to-end) · S1008796

> Probed live `2026-06-27`: **cc-health 19/19 up** + direct `/health` confirm. PV2 Kuramoto `r=0.590`,
> ORAC RALPH `gen=50182 fit=0.764`, VMS `mem=4678`. morphd UDS present (`0600`); telegram outbound (no port).
> This is the **runtime topology** (live dataflow), not a source-symbol graph. `habitat-graph` (this repo)
> is the dashed organ at `:8202` — it graphs the *source* of any service here and serves `graph_query/path/health` over MCP.

```mermaid
flowchart TB
  classDef live fill:#0b3d0b,stroke:#2ecc40,color:#eaffea;
  classDef organ fill:#1a2b3d,stroke:#3498db,color:#eaf2ff,stroke-dasharray:4 3;

  subgraph SURFACE["① Surface — Zellij habitat"]
    CX["Claude @ cortex"]:::live
    ZJ["Zellij panes<br/>(pane-vortex)"]:::live
  end

  subgraph CTRL["② Control plane / Orchestration"]
    ARCH["Architect :8144"]:::live
    WFE["WFE :8142"]:::live
    WFE2["WFE v2 :8143"]:::live
    LCM["LCM :8200<br/>HMAC receipts"]:::live
    TW["TIERWRIGHT :8201<br/>capability-floor router"]:::live
  end

  subgraph COG["③ Cognitive dynamics"]
    PV2["Pane-Vortex V2 :8132<br/>Kuramoto r=0.590"]:::live
    ORAC["ORAC Sidecar :8133<br/>RALPH gen=50182 fit=0.764"]:::live
    ORH["ORAC Health :8134"]:::live
  end

  subgraph CONS["④ Consensus"]
    PSW["Prometheus Swarm :10002<br/>PBFT + Kuramoto"]:::live
  end

  subgraph MEM["⑤ Memory substrates"]
    HM["Habitat Memory :8140<br/>injection.db"]:::live
    POVM["POVM :8125<br/>Hebbian POVM"]:::live
    VMS["Vortex Memory :8120<br/>OVM+POVM bridge"]:::live
    RM["Reasoning Memory :8130<br/>TSV"]:::live
  end

  subgraph ENG["⑥ Engines / Services"]
    DEV["DevOps V3 :8082"]:::live
    CS["CodeSynthor V8 :8111"]:::live
    ME["Maintenance Engine :8180<br/>/api/health · PBFT"]:::live
    SX2["SYNTHEX v2 :8092<br/>Hebbian"]:::live
    TL["Tool Library :8085<br/>hb daemon"]:::live
  end

  subgraph OBS["⑦ Observability / Out"]
    NC["Nerve Center :8083<br/>health aggregator"]:::live
    TG["Telegram (outbound)"]:::live
  end

  subgraph IR["⑧ IR / Morph"]
    MD["morphd (UDS 0600)"]:::live
  end

  HG["habitat-graph<br/>MCP :8202 (claimed, deployed)"]:::organ

  %% --- end-to-end dataflow (grounded) ---
  CX --> ARCH
  CX --> ZJ
  ZJ -->|sphere register| PV2
  ARCH -->|genesis| WFE
  WFE -->|×LCM publication gate| LCM
  WFE2 --> LCM
  LCM <-->|ralph afferent poll /ralph| ORAC
  WFE -->|sphere keepalive afferent| PV2
  ORAC -->|STDP / coupling| PV2
  ORAC --> ORH
  PSW -->|PBFT quorum| LCM
  TW -->|model route| ARCH
  TW --> SX2

  %% engines -> memory writes
  DEV --> HM
  ME --> HM
  CS --> POVM
  SX2 -->|Hebbian co-activation| POVM
  PV2 --> VMS
  POVM --> VMS
  ARCH --> RM

  %% observability fan-in
  DEV --> NC
  ME --> NC
  PV2 --> NC
  ORAC --> NC
  NC --> TG

  %% IR
  MD -->|morph IR| CS

  %% the new organ (graphs source codebases + callable by the orchestrator)
  HG -.->|MCP tools graph_query/path/health| ARCH
  HG -.->|extracts source graph of any service| CX
```

## Comms backbone (cross-cutting, not edges above)
`cc-pipe <swarm|hab|nexus>` plugin-pipe (µs–ms) · **Atuin KV** (~1.6k keys, durable, works when all
services down) · **PV2 UDS bus** (high-rate) · `fleet-ctl` (~6s fallback). Router: `comms-mesh` + `ladders.json`.

## Layer legend
① surface ② control/orchestration ③ cognitive dynamics ④ consensus ⑤ memory ⑥ engines ⑦ observability
⑧ IR. Solid = grounded dataflow; dashed = the deployed `habitat-graph` organ (graphs the *source* of any
of these codebases on demand, and exposes `graph_query/path/health` as MCP tools to the orchestrator).

---
*Live probe + schematic S1008796 · Claude @ cortex. Canonical workspace mirror: `ai_docs/HABITAT_LIVE_SERVICE_STACK_SCHEMATIC_S1008796.md`.*
