//! `habitat-graph-habitat` — L8 factory wiring (Module Structure Plan §`habitat-graph-habitat`).
//!
//! This is the **only** crate that knows the ULTRAPLATE factory exists. Per Design Rule 8 (habitat
//! coupling is additive), nothing in the OSS core depends on it; the CLI reaches it solely behind
//! `--features habitat`. Each module owns a small boundary trait + an in-memory double, so the whole
//! crate is built and tested **without a single live service**. The concrete adapters that touch
//! real I/O (rusqlite → `injection.db`, HTTP → PV2 / TIERWRIGHT / `cc-health`, process → `cc-pipe` /
//! `hmem`) live behind the `live` feature and only actuate at runtime.
//!
//! | Module | Wires habitat-graph to | Boundary |
//! |---|---|---|
//! | [`bridge`] | service health + `cc-health` path-map | `ServiceProbe` |
//! | [`memory`] | POVM pathways + `injection.db` causal chains | `MemorySink` |
//! | [`obsidian_protocol`] | Obsidian `Back to:` + `MASTER_INDEX` + `hmem` | `RebuildHook` |
//! | [`pv2_spheres`] | PV2 Kuramoto spheres (Leiden community → sphere) | `SphereRegistrar` |
//! | [`orchestrator_pipe`] | orchestrator `cc-pipe` verb (ACK/NACK) | `PipeTransport` |
//! | [`tierwright`] | TIERWRIGHT model router (`:8201`) | `HttpTransport` |
//! | [`arc_graph`] | S1008620 bidi-wiring arc-coherence (severed-ear diff) | pure |
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod arc_graph;
pub mod arc_telemetry;
pub mod bridge;
pub mod memory;
pub mod obsidian_protocol;
pub mod orchestrator_pipe;
pub mod pv2_spheres;
pub mod tierwright;
