//! Memory writer — POVM pathways + `injection.db` causal chains (no-risk-write regime).
//!
//! The **no-risk-write discipline** mandates that every
//! [`MemorySink::write_causal_chain`] call is immediately followed by
//! [`MemorySink::read_back_chain`]. If the result is `None` or the returned row does not
//! byte-match what was submitted, the caller receives [`GraphError::Daemon`] and must halt
//! rather than silently propagating corrupt state.
//!
//! [`persist_graph_summary`] implements this protocol end-to-end: it writes a
//! [`CausalChainRow`] that summarises a completed graph extraction, reads it back, compares,
//! and only returns `Ok` when the round-trip verifies.
//!
//! # Boundary
//!
//! All storage is behind the [`MemorySink`] trait. Tests inject [`InMemorySink`] (no disk,
//! no network, no processes). The concrete [`SqliteSink`] — backed by `rusqlite` with bound
//! `?` parameters exclusively — is compiled only with `feature = "live"`.

use habitat_graph_core::{sanitize_label, GraphError, Result};
use std::collections::HashMap;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Public data structures
// ---------------------------------------------------------------------------

/// A row in the `causal_chain` table of `injection.db`.
///
/// The `label` is the primary key (sanitised, ≤256 chars, no control characters).
/// `reinforcement_count` tracks how many times a chain has been re-confirmed.
/// `resolved_session` is `Some` once the chain is closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CausalChainRow {
    /// Primary key — the chain label.
    pub label: String,
    /// Session that last wrote or confirmed this chain.
    pub session: String,
    /// Cumulative count of times this causal chain has been reinforced.
    pub reinforcement_count: u32,
    /// Session in which this chain was resolved, or `None` if still open.
    pub resolved_session: Option<String>,
}

/// A directional weighted edge in the POVM signal mesh.
#[derive(Debug, Clone, PartialEq)]
pub struct PovmPathway {
    /// The POVM namespace this pathway lives in (e.g. `"session_1008620"`).
    pub namespace: String,
    /// Source key (the "from" end).
    pub from_key: String,
    /// Destination key (the "to" end).
    pub to_key: String,
    /// Pathway weight; negative values are valid for inhibitory connections.
    pub weight: f64,
}

// ---------------------------------------------------------------------------
// MemorySink trait
// ---------------------------------------------------------------------------

/// Storage sink for [`CausalChainRow`]s and [`PovmPathway`]s.
///
/// Implementations must be `Send + Sync` so they can be shared across threads.
/// The in-crate test double is [`InMemorySink`]; production wires the `live`-feature
/// [`SqliteSink`].
///
/// # Errors
///
/// All methods return [`GraphError::Daemon`] on storage failure.
pub trait MemorySink: Send + Sync {
    /// Writes (or upserts by label) a [`CausalChainRow`] into the causal-chain store.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Daemon`] if the write fails.
    fn write_causal_chain(&self, row: &CausalChainRow) -> Result<()>;

    /// Writes a [`PovmPathway`] into the pathway store.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Daemon`] if the write fails.
    fn write_pathway(&self, p: &PovmPathway) -> Result<()>;

    /// Reads back a [`CausalChainRow`] by `label`, returning `None` if not found.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Daemon`] if the read operation itself fails.
    fn read_back_chain(&self, label: &str) -> Result<Option<CausalChainRow>>;
}

// ---------------------------------------------------------------------------
// InMemorySink — test double, zero I/O
// ---------------------------------------------------------------------------

/// Thread-safe in-memory implementation of [`MemorySink`] — the canonical test double.
///
/// Chains are stored in a [`Mutex`]-guarded [`HashMap`] keyed by label (last write wins).
/// Pathways are stored in a [`Mutex`]-guarded [`Vec`] in insertion order.
/// No filesystem, network, or process access is performed.
#[derive(Debug, Default)]
pub struct InMemorySink {
    chains: Mutex<HashMap<String, CausalChainRow>>,
    pathways: Mutex<Vec<PovmPathway>>,
}

impl InMemorySink {
    /// Creates a new, empty `InMemorySink`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns all [`PovmPathway`]s currently held, in insertion order.
    ///
    /// Useful in tests to assert that pathway writes went through without inspecting
    /// internal state directly.
    #[must_use]
    pub fn pathways_stored(&self) -> Vec<PovmPathway> {
        if let Ok(guard) = self.pathways.lock() {
            guard.clone()
        } else {
            Vec::new()
        }
    }

    /// Returns all [`CausalChainRow`]s currently held (order unspecified).
    ///
    /// Useful in tests to assert that chain writes went through.
    #[must_use]
    pub fn chains_stored(&self) -> Vec<CausalChainRow> {
        if let Ok(guard) = self.chains.lock() {
            guard.values().cloned().collect()
        } else {
            Vec::new()
        }
    }
}

impl MemorySink for InMemorySink {
    fn write_causal_chain(&self, row: &CausalChainRow) -> Result<()> {
        if let Ok(mut guard) = self.chains.lock() {
            guard.insert(row.label.clone(), row.clone());
            Ok(())
        } else {
            Err(GraphError::Daemon(
                "InMemorySink: causal_chain mutex poisoned on write".into(),
            ))
        }
    }

    fn write_pathway(&self, p: &PovmPathway) -> Result<()> {
        if let Ok(mut guard) = self.pathways.lock() {
            guard.push(p.clone());
            Ok(())
        } else {
            Err(GraphError::Daemon(
                "InMemorySink: pathways mutex poisoned on write".into(),
            ))
        }
    }

    fn read_back_chain(&self, label: &str) -> Result<Option<CausalChainRow>> {
        if let Ok(guard) = self.chains.lock() {
            Ok(guard.get(label).cloned())
        } else {
            Err(GraphError::Daemon(
                "InMemorySink: causal_chain mutex poisoned on read".into(),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// persist_graph_summary — no-risk-write entry point
// ---------------------------------------------------------------------------

/// Persists a summary of a completed graph extraction as a [`CausalChainRow`], then
/// immediately reads it back to verify the write (no-risk-write discipline).
///
/// The row is labelled `"habitat-graph-extract"` (passed through [`sanitize_label`]).
/// The `session` field encodes the caller-supplied session name together with the extraction
/// counts in the format `"{session} nodes={n} edges={e} communities={c}"` so the row is
/// self-documenting. `reinforcement_count` is `0`; `resolved_session` is `None`.
///
/// # Errors
///
/// - [`GraphError::Daemon`] if the sink's [`MemorySink::write_causal_chain`] fails.
/// - [`GraphError::Daemon`] if the subsequent [`MemorySink::read_back_chain`] fails.
/// - [`GraphError::Daemon`] if the read-back returns `None` (write did not persist).
/// - [`GraphError::Daemon`] if the read-back row does not exactly match what was written
///   (tamper / corruption guard — the "verify, don't trust" invariant).
pub fn persist_graph_summary<S: MemorySink>(
    sink: &S,
    session: &str,
    counts: (usize, usize, usize),
) -> Result<CausalChainRow> {
    let (nodes, edges, communities) = counts;
    let label = sanitize_label("habitat-graph-extract");
    let session_val = format!("{session} nodes={nodes} edges={edges} communities={communities}");
    let row = CausalChainRow {
        label: label.clone(),
        session: session_val,
        reinforcement_count: 0,
        resolved_session: None,
    };
    sink.write_causal_chain(&row)?;
    let read_back = sink.read_back_chain(&label)?.ok_or_else(|| {
        GraphError::Daemon(
            "persist_graph_summary: read-back returned None — write did not persist".into(),
        )
    })?;
    if read_back != row {
        return Err(GraphError::Daemon(format!(
            "persist_graph_summary: read-back mismatch — written {row:?}, got {read_back:?}"
        )));
    }
    Ok(row)
}

// ---------------------------------------------------------------------------
// SqliteSink — live adapter (feature = "live")
// ---------------------------------------------------------------------------

/// Concrete [`MemorySink`] backed by a `rusqlite` connection to `injection.db`.
///
/// Only available with `feature = "live"`. Uses **bound `?` parameters exclusively** —
/// no SQL string interpolation is performed anywhere in this type.
///
/// DDL applied on construction:
///
/// ```sql
/// CREATE TABLE IF NOT EXISTS causal_chain (
///     label               TEXT    PRIMARY KEY,
///     session             TEXT    NOT NULL,
///     reinforcement_count INTEGER NOT NULL DEFAULT 0,
///     resolved_session    TEXT
/// );
/// CREATE TABLE IF NOT EXISTS povm_pathway (
///     namespace  TEXT NOT NULL,
///     from_key   TEXT NOT NULL,
///     to_key     TEXT NOT NULL,
///     weight     REAL NOT NULL,
///     PRIMARY KEY (namespace, from_key, to_key)
/// );
/// ```
#[cfg(feature = "live-memory")]
pub struct SqliteSink {
    conn: Mutex<rusqlite::Connection>,
}

#[cfg(feature = "live-memory")]
impl SqliteSink {
    /// Opens (or creates) `injection.db` at `db_path` and applies the DDL migrations.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Io`] if the database cannot be opened or the DDL fails.
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = rusqlite::Connection::open(db_path)
            .map_err(|e| GraphError::Io(format!("SqliteSink: cannot open {db_path}: {e}")))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS causal_chain (
                 label               TEXT    PRIMARY KEY,
                 session             TEXT    NOT NULL,
                 reinforcement_count INTEGER NOT NULL DEFAULT 0,
                 resolved_session    TEXT
             );
             CREATE TABLE IF NOT EXISTS povm_pathway (
                 namespace  TEXT NOT NULL,
                 from_key   TEXT NOT NULL,
                 to_key     TEXT NOT NULL,
                 weight     REAL NOT NULL,
                 PRIMARY KEY (namespace, from_key, to_key)
             );",
        )
        .map_err(|e| GraphError::Io(format!("SqliteSink: DDL failed: {e}")))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

#[cfg(feature = "live-memory")]
impl MemorySink for SqliteSink {
    fn write_causal_chain(&self, row: &CausalChainRow) -> Result<()> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| GraphError::Daemon("SqliteSink: connection mutex poisoned".into()))?;
        guard
            .execute(
                "INSERT OR REPLACE INTO causal_chain \
                 (label, session, reinforcement_count, resolved_session) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    row.label,
                    row.session,
                    row.reinforcement_count,
                    row.resolved_session,
                ],
            )
            .map_err(|e| {
                GraphError::Daemon(format!("SqliteSink: write_causal_chain failed: {e}"))
            })?;
        Ok(())
    }

    fn write_pathway(&self, p: &PovmPathway) -> Result<()> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| GraphError::Daemon("SqliteSink: connection mutex poisoned".into()))?;
        guard
            .execute(
                "INSERT OR REPLACE INTO povm_pathway \
                 (namespace, from_key, to_key, weight) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![p.namespace, p.from_key, p.to_key, p.weight],
            )
            .map_err(|e| GraphError::Daemon(format!("SqliteSink: write_pathway failed: {e}")))?;
        Ok(())
    }

    fn read_back_chain(&self, label: &str) -> Result<Option<CausalChainRow>> {
        use rusqlite::OptionalExtension as _;
        let guard = self.conn.lock().map_err(|_| {
            GraphError::Daemon("SqliteSink: connection mutex poisoned on read".into())
        })?;
        let mut stmt = guard
            .prepare(
                "SELECT label, session, reinforcement_count, resolved_session \
                 FROM causal_chain WHERE label = ?1",
            )
            .map_err(|e| GraphError::Daemon(format!("SqliteSink: prepare failed: {e}")))?;
        let result = stmt
            .query_row(rusqlite::params![label], |row| {
                Ok(CausalChainRow {
                    label: row.get(0)?,
                    session: row.get(1)?,
                    reinforcement_count: row.get::<_, u32>(2)?,
                    resolved_session: row.get(3)?,
                })
            })
            .optional()
            .map_err(|e| GraphError::Daemon(format!("SqliteSink: read_back_chain failed: {e}")))?;
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{persist_graph_summary, CausalChainRow, InMemorySink, MemorySink, PovmPathway};
    use habitat_graph_core::{sanitize_label, GraphError, Result};
    use std::sync::{Arc, Mutex};
    use std::thread;

    // --- test doubles ---

    /// Returns a mismatch row regardless of what was written, to simulate storage corruption.
    struct TamperSink {
        /// Captures the last row written (so tests can inspect write calls).
        written: Mutex<Option<CausalChainRow>>,
    }

    impl TamperSink {
        fn new() -> Self {
            Self {
                written: Mutex::new(None),
            }
        }

        fn last_written(&self) -> Option<CausalChainRow> {
            if let Ok(g) = self.written.lock() {
                g.clone()
            } else {
                None
            }
        }
    }

    impl MemorySink for TamperSink {
        fn write_causal_chain(&self, row: &CausalChainRow) -> Result<()> {
            if let Ok(mut g) = self.written.lock() {
                *g = Some(row.clone());
            }
            Ok(())
        }
        fn write_pathway(&self, _p: &PovmPathway) -> Result<()> {
            Ok(())
        }
        fn read_back_chain(&self, _label: &str) -> Result<Option<CausalChainRow>> {
            Ok(Some(CausalChainRow {
                label: "tampered-label".into(),
                session: "tampered-session".into(),
                reinforcement_count: 999,
                resolved_session: Some("tampered".into()),
            }))
        }
    }

    /// Simulates a sink that accepts writes but always returns `None` on read-back
    /// (e.g. a write-then-evict race or silent I/O failure).
    struct NullReadBackSink;

    impl MemorySink for NullReadBackSink {
        fn write_causal_chain(&self, _row: &CausalChainRow) -> Result<()> {
            Ok(())
        }
        fn write_pathway(&self, _p: &PovmPathway) -> Result<()> {
            Ok(())
        }
        fn read_back_chain(&self, _label: &str) -> Result<Option<CausalChainRow>> {
            Ok(None)
        }
    }

    /// Simulates a sink that always fails on `write_causal_chain`.
    struct FailWriteSink;

    impl MemorySink for FailWriteSink {
        fn write_causal_chain(&self, _row: &CausalChainRow) -> Result<()> {
            Err(GraphError::Daemon("intentional write failure".into()))
        }
        fn write_pathway(&self, _p: &PovmPathway) -> Result<()> {
            Err(GraphError::Daemon("intentional write failure".into()))
        }
        fn read_back_chain(&self, _label: &str) -> Result<Option<CausalChainRow>> {
            Err(GraphError::Daemon("intentional read failure".into()))
        }
    }

    /// Simulates a sink that accepts writes but fails on `read_back_chain`.
    struct FailReadBackSink;

    impl MemorySink for FailReadBackSink {
        fn write_causal_chain(&self, _row: &CausalChainRow) -> Result<()> {
            Ok(())
        }
        fn write_pathway(&self, _p: &PovmPathway) -> Result<()> {
            Ok(())
        }
        fn read_back_chain(&self, _label: &str) -> Result<Option<CausalChainRow>> {
            Err(GraphError::Daemon("intentional read-back failure".into()))
        }
    }

    // -----------------------------------------------------------------------
    // Helper
    // -----------------------------------------------------------------------

    fn sample_row(label: &str) -> CausalChainRow {
        CausalChainRow {
            label: label.into(),
            session: "s1008620".into(),
            reinforcement_count: 3,
            resolved_session: None,
        }
    }

    fn sample_pathway() -> PovmPathway {
        PovmPathway {
            namespace: "ns_test".into(),
            from_key: "a".into(),
            to_key: "b".into(),
            weight: 0.75,
        }
    }

    // -----------------------------------------------------------------------
    // InMemorySink — basic read/write
    // -----------------------------------------------------------------------

    #[test]
    fn write_and_read_back_roundtrip_preserves_all_fields() {
        let sink = InMemorySink::new();
        let row = CausalChainRow {
            label: "my-chain".into(),
            session: "sess-42".into(),
            reinforcement_count: 7,
            resolved_session: Some("sess-43".into()),
        };
        sink.write_causal_chain(&row).expect("write");
        let back = sink
            .read_back_chain("my-chain")
            .expect("read")
            .expect("present");
        assert_eq!(back, row);
    }

    #[test]
    fn read_back_none_before_any_write() {
        let sink = InMemorySink::new();
        let result = sink.read_back_chain("never-written").expect("read");
        assert!(result.is_none());
    }

    #[test]
    fn read_back_none_for_different_label() {
        let sink = InMemorySink::new();
        sink.write_causal_chain(&sample_row("chain-alpha"))
            .expect("write");
        let result = sink.read_back_chain("chain-beta").expect("read");
        assert!(result.is_none(), "wrong label must return None");
    }

    #[test]
    fn overwrite_same_label_last_write_wins() {
        let sink = InMemorySink::new();
        let first = CausalChainRow {
            label: "key".into(),
            session: "first".into(),
            reinforcement_count: 1,
            resolved_session: None,
        };
        let second = CausalChainRow {
            label: "key".into(),
            session: "second".into(),
            reinforcement_count: 2,
            resolved_session: Some("closed".into()),
        };
        sink.write_causal_chain(&first).expect("write first");
        sink.write_causal_chain(&second).expect("write second");
        let back = sink.read_back_chain("key").expect("read").expect("present");
        assert_eq!(back, second);
    }

    #[test]
    fn multiple_chains_are_independently_stored() {
        let sink = InMemorySink::new();
        sink.write_causal_chain(&sample_row("chain-1"))
            .expect("write 1");
        sink.write_causal_chain(&sample_row("chain-2"))
            .expect("write 2");
        sink.write_causal_chain(&sample_row("chain-3"))
            .expect("write 3");
        for label in ["chain-1", "chain-2", "chain-3"] {
            let back = sink.read_back_chain(label).expect("read").expect("present");
            assert_eq!(back.label, label);
        }
    }

    #[test]
    fn resolved_session_some_round_trips() {
        let sink = InMemorySink::new();
        let row = CausalChainRow {
            label: "closed-chain".into(),
            session: "sess-100".into(),
            reinforcement_count: 5,
            resolved_session: Some("sess-101".into()),
        };
        sink.write_causal_chain(&row).expect("write");
        let back = sink
            .read_back_chain("closed-chain")
            .expect("read")
            .expect("present");
        assert_eq!(back.resolved_session, Some("sess-101".into()));
    }

    #[test]
    fn resolved_session_none_round_trips() {
        let sink = InMemorySink::new();
        let row = sample_row("open-chain");
        sink.write_causal_chain(&row).expect("write");
        let back = sink
            .read_back_chain("open-chain")
            .expect("read")
            .expect("present");
        assert!(back.resolved_session.is_none());
    }

    #[test]
    fn reinforcement_count_max_round_trips() {
        let sink = InMemorySink::new();
        let row = CausalChainRow {
            label: "maxed".into(),
            session: "s".into(),
            reinforcement_count: u32::MAX,
            resolved_session: None,
        };
        sink.write_causal_chain(&row).expect("write");
        let back = sink
            .read_back_chain("maxed")
            .expect("read")
            .expect("present");
        assert_eq!(back.reinforcement_count, u32::MAX);
    }

    #[test]
    fn chains_stored_returns_all_written_rows() {
        let sink = InMemorySink::new();
        sink.write_causal_chain(&sample_row("a")).expect("a");
        sink.write_causal_chain(&sample_row("b")).expect("b");
        sink.write_causal_chain(&sample_row("c")).expect("c");
        let mut stored: Vec<_> = sink.chains_stored().into_iter().map(|r| r.label).collect();
        stored.sort_unstable();
        assert_eq!(stored, vec!["a", "b", "c"]);
    }

    #[test]
    fn new_sink_chains_stored_is_empty() {
        let sink = InMemorySink::new();
        assert!(sink.chains_stored().is_empty());
    }

    // -----------------------------------------------------------------------
    // InMemorySink — pathways
    // -----------------------------------------------------------------------

    #[test]
    fn pathway_weight_stored_correctly() {
        let sink = InMemorySink::new();
        let p = PovmPathway {
            namespace: "ns".into(),
            from_key: "x".into(),
            to_key: "y".into(),
            weight: 1.234_567_891_011,
        };
        sink.write_pathway(&p).expect("write");
        let stored = sink.pathways_stored();
        assert_eq!(stored.len(), 1);
        assert!((stored[0].weight - 1.234_567_891_011).abs() < f64::EPSILON);
    }

    #[test]
    fn pathway_namespace_from_to_not_swapped() {
        let sink = InMemorySink::new();
        let p = PovmPathway {
            namespace: "my-ns".into(),
            from_key: "source".into(),
            to_key: "target".into(),
            weight: 0.5,
        };
        sink.write_pathway(&p).expect("write");
        let stored = sink.pathways_stored();
        assert_eq!(stored[0].namespace, "my-ns");
        assert_eq!(stored[0].from_key, "source");
        assert_eq!(stored[0].to_key, "target");
    }

    #[test]
    fn pathway_negative_weight_stored() {
        let sink = InMemorySink::new();
        let p = PovmPathway {
            namespace: "ns".into(),
            from_key: "a".into(),
            to_key: "b".into(),
            weight: -0.8,
        };
        sink.write_pathway(&p).expect("write");
        assert!(sink.pathways_stored()[0].weight < 0.0);
    }

    #[test]
    fn pathway_zero_weight_stored() {
        let sink = InMemorySink::new();
        let p = PovmPathway {
            namespace: "ns".into(),
            from_key: "a".into(),
            to_key: "b".into(),
            weight: 0.0,
        };
        sink.write_pathway(&p).expect("write");
        let w = sink.pathways_stored()[0].weight;
        assert_eq!(w.total_cmp(&0.0), std::cmp::Ordering::Equal);
    }

    #[test]
    fn pathway_large_weight_stored() {
        let sink = InMemorySink::new();
        let p = PovmPathway {
            namespace: "ns".into(),
            from_key: "a".into(),
            to_key: "b".into(),
            weight: 1.0e15,
        };
        sink.write_pathway(&p).expect("write");
        assert!((sink.pathways_stored()[0].weight - 1.0e15).abs() < 1.0);
    }

    #[test]
    fn pathway_self_loop_from_equals_to() {
        let sink = InMemorySink::new();
        let p = PovmPathway {
            namespace: "ns".into(),
            from_key: "node".into(),
            to_key: "node".into(),
            weight: 1.0,
        };
        sink.write_pathway(&p).expect("write");
        let stored = sink.pathways_stored();
        assert_eq!(stored[0].from_key, stored[0].to_key);
    }

    #[test]
    fn pathway_empty_namespace_stored() {
        let sink = InMemorySink::new();
        let p = PovmPathway {
            namespace: String::new(),
            from_key: "x".into(),
            to_key: "y".into(),
            weight: 0.1,
        };
        sink.write_pathway(&p).expect("write");
        assert_eq!(sink.pathways_stored()[0].namespace, "");
    }

    #[test]
    fn multiple_pathways_all_stored_in_order() {
        let sink = InMemorySink::new();
        for i in 0_u8..5 {
            sink.write_pathway(&PovmPathway {
                namespace: "ns".into(),
                from_key: format!("f{i}"),
                to_key: format!("t{i}"),
                weight: f64::from(i),
            })
            .expect("write");
        }
        let stored = sink.pathways_stored();
        assert_eq!(stored.len(), 5);
        for (i, p) in stored.iter().enumerate() {
            assert_eq!(p.from_key, format!("f{i}"));
        }
    }

    #[test]
    fn new_sink_pathways_stored_is_empty() {
        let sink = InMemorySink::new();
        assert!(sink.pathways_stored().is_empty());
    }

    #[test]
    fn pathways_stored_returns_all_written() {
        let sink = InMemorySink::new();
        sink.write_pathway(&sample_pathway()).expect("write");
        sink.write_pathway(&sample_pathway()).expect("write again");
        assert_eq!(sink.pathways_stored().len(), 2);
    }

    // -----------------------------------------------------------------------
    // persist_graph_summary — happy path
    // -----------------------------------------------------------------------

    #[test]
    fn persist_summary_happy_path_returns_correct_row() {
        let sink = InMemorySink::new();
        let row =
            persist_graph_summary(&sink, "sess-X", (10, 20, 3)).expect("persist should succeed");
        assert_eq!(row.label, sanitize_label("habitat-graph-extract"));
        assert_eq!(row.reinforcement_count, 0);
        assert!(row.resolved_session.is_none());
    }

    #[test]
    fn persist_summary_label_is_habitat_graph_extract() {
        let sink = InMemorySink::new();
        let row = persist_graph_summary(&sink, "s", (1, 2, 3)).expect("ok");
        assert_eq!(row.label, "habitat-graph-extract");
    }

    #[test]
    fn persist_summary_label_matches_sanitize_label_of_constant() {
        // Ensures code path calls sanitize_label: the label stored must equal
        // sanitize_label("habitat-graph-extract") even if the constant changes.
        let sink = InMemorySink::new();
        let row = persist_graph_summary(&sink, "s", (0, 0, 0)).expect("ok");
        assert_eq!(row.label, sanitize_label("habitat-graph-extract"));
    }

    #[test]
    fn persist_summary_session_encodes_all_three_counts() {
        let sink = InMemorySink::new();
        let row = persist_graph_summary(&sink, "session-id", (7, 14, 3)).expect("ok");
        assert!(row.session.contains("nodes=7"), "session={}", row.session);
        assert!(row.session.contains("edges=14"), "session={}", row.session);
        assert!(
            row.session.contains("communities=3"),
            "session={}",
            row.session
        );
    }

    #[test]
    fn persist_summary_session_contains_caller_session_name() {
        let sink = InMemorySink::new();
        let row = persist_graph_summary(&sink, "my-special-session", (1, 1, 1)).expect("ok");
        assert!(
            row.session.contains("my-special-session"),
            "caller session not found: {}",
            row.session
        );
    }

    #[test]
    fn persist_summary_reinforcement_count_is_zero() {
        let sink = InMemorySink::new();
        let row = persist_graph_summary(&sink, "s", (5, 5, 5)).expect("ok");
        assert_eq!(row.reinforcement_count, 0);
    }

    #[test]
    fn persist_summary_resolved_session_is_none() {
        let sink = InMemorySink::new();
        let row = persist_graph_summary(&sink, "s", (0, 0, 0)).expect("ok");
        assert!(row.resolved_session.is_none());
    }

    #[test]
    fn persist_summary_zero_counts() {
        let sink = InMemorySink::new();
        let row = persist_graph_summary(&sink, "empty-session", (0, 0, 0)).expect("ok");
        assert!(row.session.contains("nodes=0"));
        assert!(row.session.contains("edges=0"));
        assert!(row.session.contains("communities=0"));
    }

    #[test]
    fn persist_summary_large_counts() {
        let sink = InMemorySink::new();
        let row = persist_graph_summary(&sink, "large", (100_000, 999_999, 512)).expect("ok");
        assert!(row.session.contains("nodes=100000"), "{}", row.session);
        assert!(row.session.contains("edges=999999"), "{}", row.session);
        assert!(row.session.contains("communities=512"), "{}", row.session);
    }

    #[test]
    fn persist_summary_row_verifiable_in_sink_after_call() {
        let sink = InMemorySink::new();
        let returned = persist_graph_summary(&sink, "s", (3, 6, 1)).expect("ok");
        let stored = sink
            .read_back_chain("habitat-graph-extract")
            .expect("read")
            .expect("present");
        assert_eq!(stored, returned);
    }

    #[test]
    fn persist_summary_unicode_session_name() {
        let sink = InMemorySink::new();
        let row = persist_graph_summary(&sink, "séssion-2026-αβγ", (1, 2, 3)).expect("ok");
        assert!(row.session.contains("séssion-2026-αβγ"));
    }

    #[test]
    fn persist_summary_whitespace_session_name() {
        let sink = InMemorySink::new();
        let row = persist_graph_summary(&sink, "  spaced  ", (0, 1, 0)).expect("ok");
        assert!(row.session.contains("  spaced  "));
    }

    #[test]
    fn persist_summary_second_call_overwrites_first() {
        let sink = InMemorySink::new();
        persist_graph_summary(&sink, "first", (1, 2, 3)).expect("first ok");
        let second = persist_graph_summary(&sink, "second", (4, 5, 6)).expect("second ok");
        let stored = sink
            .read_back_chain("habitat-graph-extract")
            .expect("read")
            .expect("present");
        assert_eq!(stored, second);
        assert!(stored.session.contains("second"));
    }

    // -----------------------------------------------------------------------
    // persist_graph_summary — error paths (no-risk-write discipline)
    // -----------------------------------------------------------------------

    #[test]
    fn tamper_sink_causes_daemon_error() {
        let sink = TamperSink::new();
        let err = persist_graph_summary(&sink, "sess", (1, 2, 3)).expect_err("must fail");
        assert_eq!(err.kind(), "daemon");
    }

    #[test]
    fn tamper_sink_write_was_called_before_error() {
        // Verify persist_graph_summary did call write_causal_chain before detecting the mismatch.
        let sink = TamperSink::new();
        let _ = persist_graph_summary(&sink, "sess", (1, 2, 3));
        assert!(
            sink.last_written().is_some(),
            "write_causal_chain must have been called"
        );
    }

    #[test]
    fn tamper_sink_error_message_mentions_mismatch() {
        let sink = TamperSink::new();
        let err = persist_graph_summary(&sink, "sess", (0, 0, 0)).expect_err("must fail");
        let msg = err.to_string();
        // The Daemon error message must contain useful context for debugging.
        assert!(
            msg.contains("mismatch") || msg.contains("tampered"),
            "{msg}"
        );
    }

    #[test]
    fn null_read_back_causes_daemon_error() {
        let err = persist_graph_summary(&NullReadBackSink, "sess", (1, 1, 1))
            .expect_err("must fail when read-back returns None");
        assert_eq!(err.kind(), "daemon");
    }

    #[test]
    fn null_read_back_error_message_mentions_none_or_persist() {
        let err = persist_graph_summary(&NullReadBackSink, "sess", (1, 1, 1)).expect_err("fail");
        let msg = err.to_string();
        // Should indicate the read-back returned nothing.
        assert!(
            msg.contains("None") || msg.contains("persist") || msg.contains("not persist"),
            "{msg}"
        );
    }

    #[test]
    fn failing_write_propagates_daemon_error() {
        let err = persist_graph_summary(&FailWriteSink, "sess", (0, 0, 0))
            .expect_err("write fail must propagate");
        assert_eq!(err.kind(), "daemon");
    }

    #[test]
    fn failing_read_back_propagates_daemon_error() {
        let err = persist_graph_summary(&FailReadBackSink, "sess", (0, 0, 0))
            .expect_err("read-back fail must propagate");
        assert_eq!(err.kind(), "daemon");
    }

    // -----------------------------------------------------------------------
    // Concurrency
    // -----------------------------------------------------------------------

    #[test]
    fn concurrent_chain_writes_via_arc_are_sound() {
        let sink = Arc::new(InMemorySink::new());
        let mut handles = Vec::new();
        for i in 0_u32..20 {
            let s = Arc::clone(&sink);
            handles.push(thread::spawn(move || {
                let row = CausalChainRow {
                    label: format!("concurrent-chain-{i}"),
                    session: format!("sess-{i}"),
                    reinforcement_count: i,
                    resolved_session: None,
                };
                s.write_causal_chain(&row).expect("write");
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }
        assert_eq!(sink.chains_stored().len(), 20);
    }

    #[test]
    fn concurrent_pathway_writes_via_arc_are_sound() {
        let sink = Arc::new(InMemorySink::new());
        let mut handles = Vec::new();
        for i in 0_u32..15 {
            let s = Arc::clone(&sink);
            handles.push(thread::spawn(move || {
                let p = PovmPathway {
                    namespace: "concurrent-ns".into(),
                    from_key: format!("from-{i}"),
                    to_key: format!("to-{i}"),
                    weight: f64::from(i),
                };
                s.write_pathway(&p).expect("write");
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }
        assert_eq!(sink.pathways_stored().len(), 15);
    }

    #[test]
    fn concurrent_mixed_reads_and_writes_are_sound() {
        let sink = Arc::new(InMemorySink::new());
        // Pre-populate one chain.
        sink.write_causal_chain(&sample_row("stable"))
            .expect("pre-write");
        let mut handles = Vec::new();
        for i in 0_u32..10 {
            let s = Arc::clone(&sink);
            handles.push(thread::spawn(move || {
                // Concurrent reads.
                let _ = s.read_back_chain("stable");
                // Concurrent writes to distinct labels.
                let row = CausalChainRow {
                    label: format!("racing-{i}"),
                    session: "concurrent".into(),
                    reinforcement_count: 0,
                    resolved_session: None,
                };
                s.write_causal_chain(&row).expect("write");
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }
        // stable + 10 racing-* = 11 chains total.
        assert_eq!(sink.chains_stored().len(), 11);
    }

    // -----------------------------------------------------------------------
    // Trait object / Send+Sync surface
    // -----------------------------------------------------------------------

    #[test]
    fn memory_sink_is_dyn_compatible() {
        // Verifies the trait is object-safe: dyn MemorySink compiles.
        let sink: Box<dyn MemorySink> = Box::new(InMemorySink::new());
        let row = sample_row("dyn-test");
        sink.write_causal_chain(&row).expect("write via dyn");
        let back = sink
            .read_back_chain("dyn-test")
            .expect("read via dyn")
            .expect("present");
        assert_eq!(back, row);
    }

    #[test]
    fn inmemory_sink_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InMemorySink>();
    }

    #[test]
    fn arc_inmemory_sink_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Arc<InMemorySink>>();
    }

    // -----------------------------------------------------------------------
    // CausalChainRow / PovmPathway value semantics
    // -----------------------------------------------------------------------

    #[test]
    fn causal_chain_row_eq_reflexive() {
        let row = sample_row("r");
        assert_eq!(row, row.clone());
    }

    #[test]
    fn causal_chain_row_neq_when_label_differs() {
        let a = sample_row("a");
        let mut b = a.clone();
        b.label = "b".into();
        assert_ne!(a, b);
    }

    #[test]
    fn causal_chain_row_neq_when_session_differs() {
        let a = sample_row("x");
        let mut b = a.clone();
        b.session = "different".into();
        assert_ne!(a, b);
    }

    #[test]
    fn povm_pathway_eq_reflexive() {
        let p = sample_pathway();
        assert_eq!(p, p.clone());
    }

    #[test]
    fn povm_pathway_neq_when_weight_differs() {
        let a = sample_pathway();
        let mut b = a.clone();
        b.weight = 999.0;
        assert_ne!(a, b);
    }

    #[test]
    fn read_back_chain_is_case_sensitive() {
        let sink = InMemorySink::new();
        sink.write_causal_chain(&sample_row("lowercase"))
            .expect("write");
        let upper = sink.read_back_chain("LOWERCASE").expect("read");
        assert!(upper.is_none(), "label lookup must be case-sensitive");
    }

    // -----------------------------------------------------------------------
    // SqliteSink — live adapter tests (feature = "live")
    // -----------------------------------------------------------------------
    // These tests open a real in-memory SQLite database via rusqlite (bundled)
    // — no external service is required, no filesystem is touched.
    // -----------------------------------------------------------------------

    #[cfg(feature = "live-memory")]
    fn in_memory_sqlite_sink() -> super::SqliteSink {
        super::SqliteSink::new(":memory:")
            .expect("bundled rusqlite must open an in-memory DB without I/O")
    }

    /// Opening an in-memory `SqliteSink` must succeed.
    #[cfg(feature = "live-memory")]
    #[test]
    fn sqlite_sink_opens_in_memory_db() {
        let _ = in_memory_sqlite_sink();
    }

    /// Write then read back a causal chain row — full roundtrip must preserve every field.
    #[cfg(feature = "live-memory")]
    #[test]
    fn sqlite_sink_write_and_read_back_roundtrip_preserves_all_fields() {
        let sink = in_memory_sqlite_sink();
        let row = CausalChainRow {
            label: "sqlite-chain".into(),
            session: "sess-live-001".into(),
            reinforcement_count: 3,
            resolved_session: None,
        };
        sink.write_causal_chain(&row).expect("write");
        let back = sink
            .read_back_chain("sqlite-chain")
            .expect("read")
            .expect("must be present after write");
        assert_eq!(back, row);
    }

    /// Reading a label that was never written must return `None` (not an error).
    #[cfg(feature = "live-memory")]
    #[test]
    fn sqlite_sink_read_back_none_for_missing_label() {
        let sink = in_memory_sqlite_sink();
        let result = sink
            .read_back_chain("never-written")
            .expect("read succeeds");
        assert!(result.is_none(), "missing label must return None");
    }

    /// Writing the same label twice must honour last-write-wins (INSERT OR REPLACE).
    #[cfg(feature = "live-memory")]
    #[test]
    fn sqlite_sink_overwrite_same_label_last_write_wins() {
        let sink = in_memory_sqlite_sink();
        let first = CausalChainRow {
            label: "upsert-key".into(),
            session: "first".into(),
            reinforcement_count: 1,
            resolved_session: None,
        };
        let second = CausalChainRow {
            label: "upsert-key".into(),
            session: "second".into(),
            reinforcement_count: 2,
            resolved_session: Some("closed".into()),
        };
        sink.write_causal_chain(&first).expect("write first");
        sink.write_causal_chain(&second).expect("write second");
        let back = sink
            .read_back_chain("upsert-key")
            .expect("read")
            .expect("present");
        assert_eq!(back, second, "second write must overwrite first");
    }

    /// `write_pathway` must succeed for a basic `PovmPathway`.
    #[cfg(feature = "live-memory")]
    #[test]
    fn sqlite_sink_write_pathway_succeeds() {
        let sink = in_memory_sqlite_sink();
        let p = PovmPathway {
            namespace: "sqlite-ns".into(),
            from_key: "alpha".into(),
            to_key: "beta".into(),
            weight: 0.75,
        };
        sink.write_pathway(&p).expect("write pathway must not fail");
    }

    /// `persist_graph_summary` over a real `SqliteSink` must complete the no-risk-write roundtrip.
    #[cfg(feature = "live-memory")]
    #[test]
    fn sqlite_sink_persist_graph_summary_roundtrip() {
        let sink = in_memory_sqlite_sink();
        let row =
            persist_graph_summary(&sink, "live-session", (5, 10, 2)).expect("persist must succeed");
        assert_eq!(row.label, sanitize_label("habitat-graph-extract"));
        assert!(
            row.session.contains("live-session"),
            "session must contain caller session name: {}",
            row.session
        );
        assert!(
            row.session.contains("nodes=5"),
            "session must encode node count: {}",
            row.session
        );
        // Verify the row is actually in the DB (the no-risk-write read-back path).
        let stored = sink
            .read_back_chain("habitat-graph-extract")
            .expect("read")
            .expect("row must be present after persist");
        assert_eq!(stored, row);
    }

    /// The no-risk-write verification path: every field read back from `SQLite` must match written.
    #[cfg(feature = "live-memory")]
    #[test]
    fn sqlite_sink_no_risk_write_verification_passes() {
        let sink = in_memory_sqlite_sink();
        let row = CausalChainRow {
            label: "verify-integrity".into(),
            session: "integrity-session".into(),
            reinforcement_count: 7,
            resolved_session: Some("completed".into()),
        };
        sink.write_causal_chain(&row).expect("write");
        let back = sink
            .read_back_chain("verify-integrity")
            .expect("read")
            .expect("present");
        // Strict field-by-field comparison — the no-risk-write discipline.
        assert_eq!(back.label, row.label);
        assert_eq!(back.session, row.session);
        assert_eq!(back.reinforcement_count, row.reinforcement_count);
        assert_eq!(back.resolved_session, row.resolved_session);
    }

    /// `SqliteSink` must be `Send + Sync` so it can be used across threads.
    #[cfg(feature = "live-memory")]
    #[test]
    fn sqlite_sink_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<super::SqliteSink>();
    }
}
