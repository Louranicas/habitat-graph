//! `habitat-graph-cache` — the incremental substrate (ADR-04, interface contract §2/§3).
//!
//! ONE cross-cutting cache crate, never a cache per crate. It provides a content-addressed store
//! ([`cas`]) keyed on blake3 fingerprints ([`key`]) and a demand-driven memoization layer ([`memo`])
//! with a pluggable retention policy ([`evict`]) — LRU by default; the habitat crate injects a
//! POVM-weighted policy at runtime.
//!
//! **The parity-transparency invariant (the reason this crate can exist):** a cache hit MUST be
//! byte-identical to a recompute. If caching ever changes output, caching is wrong — never the
//! goldens. This is asserted here ([`memo::memoize`] tests) and again at the G5 parity gate.
//!
//! Depends only on `habitat-graph-core` (Design Rule 4: inward, acyclic).
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cas;
pub mod evict;
pub mod key;
pub mod memo;
pub mod partition;

pub use cas::{ContentStore, MemStore};
pub use evict::{Lru, RetentionPolicy};
pub use key::CacheKey;
pub use memo::memoize;
pub use partition::partition;
