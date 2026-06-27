//! `habitat-graph-analyze` — structure mining (Module Structure Plan L5).
//!
//! [`detect_communities`] runs the Leiden algorithm to cluster the graph; [`degree_centrality`]
//! ranks nodes by connectivity. (Heuristic pattern/anomaly + open-question surfacing are deferred
//! to a later refinement — they are not parity-critical.)
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod centrality;
pub mod cluster;

pub use centrality::degree_centrality;
pub use cluster::detect_communities;
