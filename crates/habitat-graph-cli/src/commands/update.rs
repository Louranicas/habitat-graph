//! The `update` command — incremental rebuild with sidecar-based extraction cache (FO-9 lifecycle).
//!
//! ## Honest cost model (C-4)
//!
//! File-level extraction is cached via owner-only private state, so only files whose `blake3`
//! content hash has changed since the last build are re-parsed by the tree-sitter extractors.
//! **Community detection is not incremental**: Leiden is always re-run on the full combined graph
//! after each incremental merge. The state cache saves AST-parsing work; it does not save analysis
//! work.
//!
//! ## Sidecar format
//!
//! The sidecar stores the complete [`Graph`] in graph-compatible JSON together with byte and
//! semantic generations of its committed public projection. Prior generations are retained as
//! owner-only snapshots so branch switches can recover matching raw lineage. On the next
//! incremental run three things are extracted from it without extra serialization overhead:
//!
//! - `graph.schema` — the [`SCHEMA_VERSION`] at build time (for the P1-G12 mismatch guard).
//! - `graph.manifest.inputs` — the `path → content_hash` map used to diff against the current
//!   file-system state.
//! - `graph.nodes` / `graph.edges` — the prior graph, pruned to drop stale-file nodes before
//!   merging with newly-extracted content.
//!
//! ## Schema-version guard (P1-G12)
//!
//! If the sidecar's `schema` field differs from the current [`SCHEMA_VERSION`], a warning is
//! emitted to stderr and a full rebuild is performed.  Silently merging across taxonomy versions
//! would corrupt the graph by mixing incompatible node/edge semantics.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use habitat_graph_core::{Graph, GraphError, Manifest, NodeId, Result, SCHEMA_VERSION};

/// Sidecar filename storing the full internal [`Graph`] JSON (relative to the output directory).
const SIDECAR: &str = ".habitat-graph-state.json";

/// Runs an incremental update over `dir`, writing refreshed artifacts into `out`.
///
/// On the first invocation (no sidecar present) or after a [`SCHEMA_VERSION`] mismatch, a full
/// rebuild is performed.  On subsequent invocations only files whose `blake3` content hash has
/// changed are re-extracted; unchanged files' nodes/edges are preserved from the prior graph.
/// Community detection is always re-run globally (honest C-4 cost: Leiden is not incremental).
///
/// ## Stdout
///
/// Emits exactly one summary line on success:
/// - `"update: {changed} changed, {n} nodes (analyze re-run globally)"` after any build.
/// - `"update: 0 changed, {n} nodes (artifacts refreshed; analyze not re-run)"` when source
///   inputs are unchanged. Public artifacts and private sidecar permissions are still refreshed so
///   an exporter-policy upgrade cannot leave legacy output behind.
///
/// Returns a process exit code: `0` on success, `4` on any error (diagnostics to stderr).
/// Targets without Unix owner-only file creation reject incremental updates and remove any
/// existing raw sidecar before source processing.
///
/// # Errors
///
/// Returns exit code `4` on any IO, parse, or serialization failure.
#[must_use]
pub fn run(dir: &Path, out: &Path) -> u8 {
    match run_inner(dir, out) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            4
        }
    }
}

/// Inner incremental-rebuild pipeline, propagating errors as [`GraphError`].
///
/// # Errors
///
/// Returns [`GraphError::Io`] on filesystem failures, [`GraphError::Parse`] on extraction
/// failures, [`GraphError::Schema`] on serialization failures, or [`GraphError::Guard`] when
/// private sidecar permissions cannot be enforced.
fn run_inner(dir: &Path, out: &Path) -> Result<()> {
    let (sidecar_path, legacy_sidecar_path, _output_lock) = prepare_update_transaction(out)?;
    let public_output = load_public_output(&out.join("graph.json"))?;

    // ── Detect all source files (sorted for R4 determinism) ─────────────────────
    let files = habitat_graph_source::detect(dir, &["rs"])?;

    // ── Load the prior sidecar (returns None on first run / mismatch) ───────────
    let Some(prior_graph) =
        load_available_prior(&sidecar_path, &legacy_sidecar_path, public_output.as_ref())?
    else {
        // No sidecar or schema mismatch → full rebuild.
        return do_full_build(out, &files, &sidecar_path, &legacy_sidecar_path);
    };
    let prior_private_checksum = private_graph_generation(&prior_graph)?;

    // ── Hash every current file (needed for the diff) ───────────────────────────
    let current_inputs = read_inputs(&files)?;

    let current_manifest =
        habitat_graph_source::build_manifest(&current_inputs, env!("CARGO_PKG_VERSION"));

    // ── Clone prior manifest into owned strings to release prior_graph borrow ───
    // `prior_map` must not borrow from `prior_graph.manifest` (we move prior_graph later).
    let prior_entries: Vec<(String, String)> = prior_graph
        .manifest
        .inputs
        .iter()
        .map(|r| (r.path.clone(), r.content_hash.clone()))
        .collect();
    // `prior_graph.manifest.inputs` borrow ends here (iter consumed, collect done).

    let current_map: HashMap<&str, &str> = current_manifest
        .inputs
        .iter()
        .map(|r| (r.path.as_str(), r.content_hash.as_str()))
        .collect();
    let prior_map: HashMap<&str, &str> = prior_entries
        .iter()
        .map(|(p, h)| (p.as_str(), h.as_str()))
        .collect();

    let current_paths: HashSet<&str> = current_map.keys().copied().collect();
    let prior_paths: HashSet<&str> = prior_map.keys().copied().collect();

    // Build diff lists as owned `String`s so they do not borrow from `prior_graph`.
    let mut added: Vec<String> = current_paths
        .difference(&prior_paths)
        .copied()
        .map(str::to_owned)
        .collect();
    let mut removed: Vec<String> = prior_paths
        .difference(&current_paths)
        .copied()
        .map(str::to_owned)
        .collect();
    let mut changed: Vec<String> = current_paths
        .intersection(&prior_paths)
        .copied()
        .filter(|&p| current_map.get(p) != prior_map.get(p))
        .map(str::to_owned)
        .collect();

    // Deterministic ordering of the three diff lists (R4 compliance).
    added.sort_unstable();
    removed.sort_unstable();
    changed.sort_unstable();

    let need_reextract = added.len() + changed.len();

    // ── Unchanged-input artifact refresh ─────────────────────────────────────────
    if need_reextract == 0 && removed.is_empty() {
        let n = prior_graph.nodes.len();
        // Export policy evolves independently of source hashes. Always rerender public artifacts
        // and atomically reharden the private sidecar so an upgrade cannot report success while
        // leaving legacy unredacted output or permissive cache permissions in place.
        write_artifacts(
            out,
            &prior_graph,
            current_manifest,
            &sidecar_path,
            &legacy_sidecar_path,
            Some(&prior_private_checksum),
        )?;
        println!("update: 0 changed, {n} nodes (artifacts refreshed; analyze not re-run)");
        return Ok(());
    }

    // ── Re-extract ONLY changed + added files (C-4 extraction saving) ───────────
    // Both sets borrow from the owned Vec<String>s, not from prior_graph.
    let to_extract_set: HashSet<&str> = added
        .iter()
        .chain(changed.iter())
        .map(String::as_str)
        .collect();
    let stale_set: HashSet<&str> = changed
        .iter()
        .chain(removed.iter())
        .map(String::as_str)
        .collect();

    let to_extract: Vec<PathBuf> = files
        .iter()
        .filter(|p| {
            let cow = p.to_string_lossy();
            to_extract_set.contains(cow.as_ref())
        })
        .cloned()
        .collect();

    let new_extractions = habitat_graph_extract::extract_files(&to_extract)?;
    let new_partial = habitat_graph_build::assemble(new_extractions);

    // ── Prune prior graph (drop stale-file nodes + dangling edges) ───────────────
    // `prior_graph` is moved here; nothing borrows it at this point.
    let pruned_prior = prune_graph(prior_graph, &stale_set);

    // ── Merge + global community detection (honest: Leiden is NOT incremental) ───
    // F12: cluster on the TRUSTED subgraph only (INFERRED/AMBIGUOUS edges excluded).
    let mut combined = habitat_graph_build::merge(new_partial, pruned_prior);
    combined.communities = habitat_graph_analyze::detect_communities(
        &habitat_graph_analyze::trusted_subgraph(&combined),
    );
    let combined = combined.sorted();

    // ── Write artifacts + refresh sidecar ────────────────────────────────────────
    let n = combined.nodes.len();
    write_artifacts(
        out,
        &combined,
        current_manifest,
        &sidecar_path,
        &legacy_sidecar_path,
        Some(&prior_private_checksum),
    )?;
    println!("update: {need_reextract} changed, {n} nodes (analyze re-run globally)");
    Ok(())
}

fn load_available_prior(
    current: &Path,
    legacy: &Path,
    public: Option<&PublicOutput>,
) -> Result<Option<Graph>> {
    if let Some(prior) = try_load_prior(current, public)? {
        return Ok(Some(prior));
    }
    if legacy != current {
        return try_load_prior(legacy, public);
    }
    Ok(None)
}

fn read_inputs(files: &[PathBuf]) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    files
        .iter()
        .map(|path| {
            let bytes = habitat_graph_source::read_local(path, 0)?;
            Ok((path.clone(), bytes))
        })
        .collect()
}

struct PublicOutput {
    generation: String,
    semantic_generation: String,
}

fn load_public_output(path: &Path) -> Result<Option<PublicOutput>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "inspect public graph {}: {error}",
                path.display()
            )))
        }
    };
    if !metadata.file_type().is_file() {
        return Err(GraphError::Guard(format!(
            "public graph is not a regular file: {}",
            path.display()
        )));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| GraphError::Io(format!("read public graph: {error}")))?;
    let graph = if text.trim().is_empty() {
        Graph::default()
    } else {
        match habitat_graph_serve::from_node_link(&text) {
            Ok(graph) => graph,
            Err(error) => {
                eprintln!("warning: public graph parse failed ({error}): forcing full rebuild");
                return Ok(None);
            }
        }
    };
    Ok(Some(PublicOutput {
        semantic_generation: super::private_state::semantic_generation(&graph)?,
        generation: super::private_state::generation(text.as_bytes()),
    }))
}

fn prepare_private_state(out: &Path) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(out).map_err(|error| GraphError::Io(error.to_string()))?;
    let legacy = out.join(SIDECAR);
    let current = super::private_state::path_for_output(&out.join("graph.json"), &legacy)?;
    super::private_state::ensure(&current)?;
    if current != legacy {
        super::private_state::ensure(&legacy)?;
    }
    Ok((current, legacy))
}

fn prepare_update_transaction(
    out: &Path,
) -> Result<(
    PathBuf,
    PathBuf,
    super::private_state::OutputTransactionLock,
)> {
    let (state, legacy) = prepare_private_state(out)?;
    let lock = super::private_state::acquire_output_lock(&state)?;
    super::private_state::ensure_no_pending_add_journals(&state)?;
    recover_update_journal(out, &state, &legacy)?;
    Ok((state, legacy, lock))
}

const UPDATE_JOURNAL_SCHEMA: &str = "habitat-graph.update-journal.v1";

struct UpdateJournal {
    before_public_generation: Option<String>,
    after_public_generation: String,
    after_public_json: String,
    origin_context: Option<String>,
    origin_lineage: Option<String>,
    before_private_checksum: Option<String>,
    after_private_checksum: String,
    public_graph: Graph,
    state_graph: Graph,
}

impl UpdateJournal {
    fn new(
        out: &Path,
        state_path: &Path,
        public_graph: &Graph,
        state_graph: Graph,
        before_private_checksum: Option<&str>,
    ) -> Result<Self> {
        if before_private_checksum
            .is_some_and(|checksum| !super::private_state::is_generation(checksum))
        {
            return Err(GraphError::Schema(
                "update journal private generation is invalid".to_owned(),
            ));
        }
        let public_json = habitat_graph_export::to_node_link(public_graph)?;
        let origin = super::private_state::context_identity_for_output(&out.join("graph.json"))?;
        let journal = Self {
            before_public_generation: current_public_generation(&out.join("graph.json"))?,
            after_public_generation: super::private_state::generation(public_json.as_bytes()),
            after_public_json: public_json,
            origin_context: origin.as_ref().map(|identity| identity.key.clone()),
            origin_lineage: origin.as_ref().map(|identity| identity.lineage.clone()),
            before_private_checksum: before_private_checksum.map(str::to_owned),
            after_private_checksum: private_graph_generation(&state_graph)?,
            public_graph: public_graph.clone(),
            state_graph,
        };
        if journal.origin_context != super::private_state::state_context_key(state_path) {
            return Err(GraphError::Guard(
                "Git context changed while preparing update transaction".to_owned(),
            ));
        }
        Ok(journal)
    }
}

fn private_graph_generation(graph: &Graph) -> Result<String> {
    Ok(super::private_state::generation(
        graph.to_json()?.as_bytes(),
    ))
}

fn current_public_generation(path: &Path) -> Result<Option<String>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(GraphError::Io(format!("inspect public graph: {error}"))),
    };
    if !metadata.file_type().is_file() {
        return Err(GraphError::Guard(format!(
            "public graph is not a regular file: {}",
            path.display()
        )));
    }
    let bytes = std::fs::read(path)
        .map_err(|error| GraphError::Io(format!("read public graph: {error}")))?;
    Ok(Some(super::private_state::generation(&bytes)))
}

fn write_update_journal(state_path: &Path, journal: &UpdateJournal) -> Result<()> {
    let public_graph = serde_json::to_value(&journal.public_graph)
        .map_err(|error| GraphError::Schema(format!("update journal serialize: {error}")))?;
    let state_graph = serde_json::to_value(&journal.state_graph)
        .map_err(|error| GraphError::Schema(format!("update journal serialize: {error}")))?;
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": UPDATE_JOURNAL_SCHEMA,
        "before_public_generation": journal.before_public_generation,
        "after_public_generation": journal.after_public_generation,
        "after_public_json": journal.after_public_json,
        "origin_context": journal.origin_context,
        "origin_lineage": journal.origin_lineage,
        "before_private_checksum": journal.before_private_checksum,
        "after_private_checksum": journal.after_private_checksum,
        "public_graph": public_graph,
        "state_graph": state_graph,
    }))
    .map_err(|error| GraphError::Schema(format!("update journal serialize: {error}")))?;
    super::private_state::write(
        &super::private_state::update_journal_path(state_path)?,
        &bytes,
    )
}

fn journal_generation(
    value: &serde_json::Value,
    field: &str,
    optional: bool,
) -> Result<Option<String>> {
    match value.get(field) {
        Some(serde_json::Value::Null) if optional => Ok(None),
        Some(serde_json::Value::String(generation))
            if super::private_state::is_generation(generation) =>
        {
            Ok(Some(generation.clone()))
        }
        _ => Err(GraphError::Schema(format!(
            "update journal `{field}` is invalid"
        ))),
    }
}

fn journal_graph(value: &serde_json::Value, field: &str) -> Result<Graph> {
    let text = serde_json::to_string(value)
        .map_err(|error| GraphError::Schema(format!("update journal {field}: {error}")))?;
    let graph = Graph::from_json(&text)?;
    if graph.schema != SCHEMA_VERSION {
        return Err(GraphError::Schema(format!(
            "update journal {field} schema mismatch"
        )));
    }
    Ok(graph)
}

fn load_update_journal(path: &Path) -> Result<UpdateJournal> {
    super::private_state::ensure(path)?;
    let text = std::fs::read_to_string(path)
        .map_err(|error| GraphError::Io(format!("read update journal: {error}")))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| GraphError::Schema(format!("update journal parse: {error}")))?;
    if value["schema"] != UPDATE_JOURNAL_SCHEMA {
        return Err(GraphError::Schema(format!(
            "unsupported update journal schema: {:?}",
            value["schema"]
        )));
    }
    let origin_context = journal_generation(&value, "origin_context", true)?;
    let origin_lineage = journal_generation(&value, "origin_lineage", true)?;
    if origin_context.is_some() != origin_lineage.is_some() {
        return Err(GraphError::Schema(
            "update journal origin is invalid".to_owned(),
        ));
    }
    let journal = UpdateJournal {
        before_public_generation: journal_generation(&value, "before_public_generation", true)?,
        after_public_generation: journal_generation(&value, "after_public_generation", false)?
            .ok_or_else(|| GraphError::Schema("update journal generation missing".to_owned()))?,
        after_public_json: value["after_public_json"]
            .as_str()
            .ok_or_else(|| {
                GraphError::Schema("update journal public projection is invalid".to_owned())
            })?
            .to_owned(),
        origin_context,
        origin_lineage,
        before_private_checksum: journal_generation(&value, "before_private_checksum", true)?,
        after_private_checksum: journal_generation(&value, "after_private_checksum", false)?
            .ok_or_else(|| GraphError::Schema("update journal checksum missing".to_owned()))?,
        public_graph: journal_graph(&value["public_graph"], "public_graph")?,
        state_graph: journal_graph(&value["state_graph"], "state_graph")?,
    };
    let intended_public = habitat_graph_serve::from_node_link(&journal.after_public_json)?;
    if intended_public.schema != SCHEMA_VERSION
        || private_graph_generation(&journal.state_graph)? != journal.after_private_checksum
        || super::private_state::generation(journal.after_public_json.as_bytes())
            != journal.after_public_generation
    {
        return Err(GraphError::Schema(
            "update journal generation mismatch".to_owned(),
        ));
    }
    Ok(journal)
}

fn update_journal_state_path(path: &Path) -> Result<PathBuf> {
    let filename = path
        .file_name()
        .and_then(|filename| filename.to_str())
        .and_then(|filename| filename.strip_suffix(".update-journal"))
        .ok_or_else(|| GraphError::Schema("invalid update journal path".to_owned()))?;
    Ok(path.with_file_name(filename))
}

fn validate_update_journal_origin(
    out: &Path,
    current_state_path: &Path,
    journal_state_path: &Path,
    journal: &UpdateJournal,
) -> Result<()> {
    let current = super::private_state::context_identity_for_output(&out.join("graph.json"))?;
    let same_context = current_state_path == journal_state_path;
    let stored_context = super::private_state::state_context_key(journal_state_path);
    let migrated_unscoped = same_context
        && journal.origin_context.is_none()
        && current.is_some()
        && stored_context == current.as_ref().map(|identity| identity.key.clone());
    if !migrated_unscoped {
        if journal.origin_context != stored_context {
            return Err(GraphError::Guard(
                "pending update belongs to a different Git context".to_owned(),
            ));
        }
        if same_context {
            if journal.origin_context != current.as_ref().map(|identity| identity.key.clone()) {
                return Err(GraphError::Guard(
                    "pending update belongs to a different Git context".to_owned(),
                ));
            }
        } else {
            let Some(current) = current.as_ref() else {
                return Err(GraphError::Guard(
                    "pending update belongs to a different Git context".to_owned(),
                ));
            };
            if journal.origin_lineage.as_deref() != Some(current.lineage.as_str())
                || !super::private_state::context_is_verified_ancestor(
                    journal_state_path,
                    current_state_path,
                )?
            {
                return Err(GraphError::Guard(
                    "pending update belongs to a different Git context".to_owned(),
                ));
            }
        }
    }
    if let Some(before) = journal.before_private_checksum.as_deref() {
        let status = super::private_state::private_checksum_status(
            journal_state_path,
            &[before, &journal.after_private_checksum],
        )?;
        if status.any && !status.matched {
            return Err(GraphError::Guard(
                "pending update private lineage changed".to_owned(),
            ));
        }
    }
    Ok(())
}

fn commit_update_journal(
    out: &Path,
    state_path: &Path,
    legacy_state_path: &Path,
    journal_path: &Path,
    journal: &UpdateJournal,
) -> Result<()> {
    super::extract::write_public_artifacts(
        out,
        &journal.public_graph,
        super::extract::ExtractOpts::default(),
    )?;
    let public_json = habitat_graph_export::to_node_link(&journal.public_graph)?;
    let public_graph = habitat_graph_serve::from_node_link(&public_json)?;
    let state_json = super::private_state::serialize(
        &journal.state_graph,
        &super::private_state::generation(public_json.as_bytes()),
        &super::private_state::semantic_generation(&public_graph)?,
    )?;
    super::private_state::write_state(state_path, &state_json)?;
    if state_path != legacy_state_path {
        super::private_state::remove(legacy_state_path, "legacy private state")?;
    }
    super::private_state::remove(journal_path, "update journal")
}

fn recover_update_journal(out: &Path, state_path: &Path, legacy_state_path: &Path) -> Result<bool> {
    let journals = super::private_state::pending_update_journals(state_path)?;
    if journals.len() > 1 {
        return Err(GraphError::Guard(format!(
            "multiple pending update transactions exist for {}",
            state_path.display()
        )));
    }
    let Some(journal_path) = journals.first() else {
        return Ok(false);
    };
    let journal_state_path = update_journal_state_path(journal_path)?;
    let journal = load_update_journal(journal_path)?;
    validate_update_journal_origin(out, state_path, &journal_state_path, &journal)?;
    let current = current_public_generation(&out.join("graph.json"))?;
    if current != journal.before_public_generation
        && current.as_deref() != Some(journal.after_public_generation.as_str())
    {
        return Err(GraphError::Guard(
            "public graph changed while an update transaction is pending".to_owned(),
        ));
    }
    commit_update_journal(out, state_path, legacy_state_path, journal_path, &journal)?;
    Ok(true)
}

/// Attempts to load the prior graph from the sidecar file.
///
/// Returns `Ok(None)` when:
/// - The sidecar does not exist (first run — a full build is needed).
/// - The sidecar is corrupted / not valid JSON (a warning is printed to stderr).
/// - The sidecar's `schema` differs from [`SCHEMA_VERSION`] (taxonomy mismatch; warning printed).
///
/// # Errors
///
/// Returns an error if private state or public output cannot be inspected safely.
fn try_load_prior(sidecar_path: &Path, public: Option<&PublicOutput>) -> Result<Option<Graph>> {
    let mut state = if let Some(public) = public {
        match super::private_state::load_matching(
            sidecar_path,
            Some(&public.generation),
            Some(&public.semantic_generation),
            "incremental private state",
        ) {
            Ok(state) => state,
            Err(error) if error.kind() == "schema" => {
                eprintln!("warning: sidecar parse failed ({error}): forcing full rebuild");
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    if state.is_none() {
        state = match super::private_state::read(sidecar_path, "incremental private state") {
            Ok(state) => state,
            Err(error) if error.kind() == "schema" => {
                eprintln!("warning: sidecar parse failed ({error}): forcing full rebuild");
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
    }
    let Some(state) = state else {
        return Ok(None);
    };

    let public_matches = if let Some(public) = public {
        if super::private_state::matches_public(
            &state,
            Some(&public.generation),
            Some(&public.semantic_generation),
        ) {
            true
        } else {
            let projected = habitat_graph_export::to_node_link(&state.graph)?;
            let projected = habitat_graph_serve::from_node_link(&projected)?;
            super::private_state::semantic_generation(&projected)? == public.semantic_generation
        }
    } else {
        false
    };
    if !public_matches {
        eprintln!("warning: private state does not match graph.json: forcing full rebuild");
        return Ok(None);
    }

    let graph = state.graph;

    if graph.schema != SCHEMA_VERSION {
        eprintln!(
            "warning: schema_version mismatch \
             (stored={:?}, current={SCHEMA_VERSION:?}): forcing full rebuild",
            graph.schema
        );
        return Ok(None);
    }

    Ok(Some(graph))
}

/// Performs a full rebuild from scratch (first run or schema-version mismatch).
///
/// Reads every file in `files`, runs the full pipeline
/// (extract → assemble → analyze → export), and writes all artifacts including a fresh sidecar.
///
/// Files are read twice: once internally by `extract_files` for AST parsing, and once here to
/// compute the `blake3` content hashes for the sidecar manifest.
///
/// # Errors
///
/// Returns a [`GraphError`] on extraction, analysis, export, or IO failure.
fn do_full_build(
    out: &Path,
    files: &[PathBuf],
    sidecar_path: &Path,
    legacy_sidecar_path: &Path,
) -> Result<()> {
    // Run the full pipeline (extract_files reads files internally).
    let extractions = habitat_graph_extract::extract_files(files)?;
    let mut graph = habitat_graph_build::assemble(extractions);
    // F12: cluster on the TRUSTED subgraph only (INFERRED/AMBIGUOUS edges excluded).
    graph.communities =
        habitat_graph_analyze::detect_communities(&habitat_graph_analyze::trusted_subgraph(&graph));
    let graph = graph.sorted();

    // Compute the sidecar manifest (second pass over the same files for content hashes).
    let inputs = read_inputs(files)?;
    let manifest = habitat_graph_source::build_manifest(&inputs, env!("CARGO_PKG_VERSION"));

    let n = graph.nodes.len();
    write_artifacts(
        out,
        &graph,
        manifest,
        sidecar_path,
        legacy_sidecar_path,
        None,
    )?;
    println!(
        "update: {} changed, {n} nodes (analyze re-run globally)",
        files.len()
    );
    Ok(())
}

/// Writes all three core artifacts (`graph.json`, `GRAPH_REPORT.md`, `graph.html`), refreshes any
/// existing optional exports, and writes the private state cache.
///
/// The sidecar stores `graph` with `current_manifest` substituted in: this ensures the sidecar
/// tracks the actual content hashes of the current file-system snapshot, not the empty manifest
/// produced by [`habitat_graph_build::assemble`].
///
/// # Errors
///
/// Returns [`GraphError::Io`] on any filesystem failure, or [`GraphError::Schema`] on
/// serialization failure.
fn write_artifacts(
    out: &Path,
    graph: &Graph,
    current_manifest: Manifest,
    sidecar_path: &Path,
    legacy_sidecar_path: &Path,
    before_private_checksum: Option<&str>,
) -> Result<()> {
    let mut sidecar = graph.clone();
    sidecar.manifest = current_manifest;
    let journal = UpdateJournal::new(out, sidecar_path, graph, sidecar, before_private_checksum)?;
    write_update_journal(sidecar_path, &journal)?;
    commit_update_journal(
        out,
        sidecar_path,
        legacy_sidecar_path,
        &super::private_state::update_journal_path(sidecar_path)?,
        &journal,
    )
}

/// Returns `graph` with all nodes whose [`habitat_graph_core::Node::source_file`] appears in
/// `stale_files` removed, along with every edge that references a dropped node.
///
/// Communities are always cleared regardless of what files are stale: community detection is
/// re-run globally on the combined graph after the incremental merge.
fn prune_graph(mut graph: Graph, stale_files: &HashSet<&str>) -> Graph {
    // Collect the ids of retained nodes while pruning.
    let mut kept: HashSet<NodeId> = HashSet::new();
    graph.nodes.retain(|n| {
        if stale_files.contains(n.source_file.as_str()) {
            false
        } else {
            kept.insert(n.id);
            true
        }
    });
    // Drop edges that reference any removed node (dangling-edge policy, consistent with merge).
    graph
        .edges
        .retain(|e| kept.contains(&e.source) && kept.contains(&e.target));
    // Communities are always re-run globally after the merge; clear to avoid stale data.
    graph.communities.clear();
    graph
}

// ── Tests ─────────────────────────────────────────────────────────────────────────────────────

#[cfg(all(test, unix))]
mod tests {
    use std::collections::HashSet;
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::{run, SIDECAR};
    use habitat_graph_core::Graph;

    // ── helpers ──────────────────────────────────────────────────────────────────

    /// Create `dir/name` (and any missing parent directories) with the given `content`.
    fn mk_file(dir: &Path, name: &str, content: &str) {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
    }

    /// Remove a file (wraps `fs::remove_file`).
    fn rm_file(dir: &Path, name: &str) {
        fs::remove_file(dir.join(name)).unwrap();
    }

    /// Read `out/graph.json` as a `String`.
    fn read_graph_json(out: &Path) -> String {
        fs::read_to_string(out.join("graph.json")).expect("graph.json missing")
    }

    /// Read `out/<SIDECAR>` as a `String`.
    fn read_sidecar(out: &Path) -> String {
        fs::read_to_string(out.join(SIDECAR)).expect("sidecar missing")
    }

    /// Count non-overlapping occurrences of `needle` in `haystack`.
    fn count_str(haystack: &str, needle: &str) -> usize {
        let mut n = 0;
        let mut pos = 0;
        while let Some(idx) = haystack[pos..].find(needle) {
            n += 1;
            pos += idx + needle.len();
        }
        n
    }

    /// Count the number of nodes in `out/graph.json` (each node has exactly one `"id":` key).
    fn count_nodes(out: &Path) -> usize {
        count_str(&read_graph_json(out), "\"id\":")
    }

    /// Parse the sidecar as a [`Graph`].
    fn parse_sidecar(out: &Path) -> Graph {
        Graph::from_json(&read_sidecar(out)).expect("sidecar must parse as Graph")
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T1–T5  First build
    // ─────────────────────────────────────────────────────────────────────────────

    // T1: first build of a non-empty dir exits 0.
    #[test]
    fn fresh_dir_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        assert_eq!(run(src.path(), out.path()), 0);
    }

    // T2: first build writes graph.json.
    #[test]
    fn fresh_dir_writes_graph_json() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        assert!(out.path().join("graph.json").exists());
    }

    // T3: first build writes the sidecar.
    #[test]
    fn fresh_dir_writes_sidecar_with_owner_only_permissions() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn private_cache() {}");
        let _ = run(src.path(), out.path());
        let sidecar = out.path().join(SIDECAR);
        assert!(sidecar.exists(), "sidecar must be written on first build");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(sidecar).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "sidecar must be owner-readable/writable only");
        }
    }

    #[test]
    fn git_outputs_store_incremental_state_under_git_metadata() {
        let repo = TempDir::new().unwrap();
        fs::create_dir(repo.path().join(".git")).unwrap();
        fs::create_dir(repo.path().join(".git/objects")).unwrap();
        fs::write(repo.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let src = repo.path().join("src");
        let out = repo.path().join("public");
        fs::create_dir(&src).unwrap();
        mk_file(&src, "lib.rs", "fn api_key_private_cache() {}");

        assert_eq!(run(&src, &out), 0);
        assert_eq!(run(&src, &out), 0);
        assert!(!out.join(SIDECAR).exists());

        let state_dir = repo.path().join(".git/habitat-graph/state");
        let states: Vec<_> = fs::read_dir(&state_dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
            .collect();
        assert_eq!(states.len(), 1);
        assert!(fs::read_to_string(states[0].path())
            .unwrap()
            .contains("api_key_private_cache"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(states[0].path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn fresh_dir_writes_sidecar() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        assert!(
            out.path().join(SIDECAR).exists(),
            "sidecar must be written on first build"
        );
    }

    // T4: first build writes GRAPH_REPORT.md.
    #[test]
    fn fresh_dir_writes_graph_report() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        assert!(out.path().join("GRAPH_REPORT.md").exists());
    }

    // T5: first build writes graph.html.
    #[test]
    fn fresh_dir_writes_graph_html() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        assert!(out.path().join("graph.html").exists());
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T6–T9  No-op (second run, nothing changed)
    // ─────────────────────────────────────────────────────────────────────────────

    // T6: second run with no changes exits 0.
    #[test]
    fn noop_second_run_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        assert_eq!(run(src.path(), out.path()), 0, "no-op must exit 0");
    }

    // T7: no-op does not change graph.json bytes.
    #[test]
    fn noop_graph_json_byte_identical() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() { b(); } fn b() {}");
        let _ = run(src.path(), out.path());
        let before = fs::read(out.path().join("graph.json")).unwrap();
        let _ = run(src.path(), out.path());
        let after = fs::read(out.path().join("graph.json")).unwrap();
        assert_eq!(before, after, "no-op must leave graph.json byte-identical");
    }

    // T8: an unchanged-input refresh leaves the sidecar parseable as a valid Graph.
    #[test]
    fn noop_sidecar_still_parseable() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn x() {}");
        let _ = run(src.path(), out.path());
        let _ = run(src.path(), out.path());
        let g = parse_sidecar(out.path());
        assert_eq!(g.nodes.len(), 1);
    }

    #[test]
    fn mismatched_public_generation_forces_source_rebuild() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn source_truth() {}");
        assert_eq!(run(src.path(), out.path()), 0);

        let committed_public = read_graph_json(out.path());
        let committed_graph = habitat_graph_serve::from_node_link(&committed_public).unwrap();
        let mut stale_private = super::super::private_state::parse(&read_sidecar(out.path()))
            .unwrap()
            .graph;
        stale_private.nodes[0].label = "stale_private_cache".to_owned();
        let stale_bytes = super::super::private_state::serialize(
            &stale_private,
            &super::super::private_state::generation(committed_public.as_bytes()),
            &super::super::private_state::semantic_generation(&committed_graph).unwrap(),
        )
        .unwrap();
        fs::write(out.path().join(SIDECAR), stale_bytes).unwrap();

        let mut checked_out = committed_graph;
        checked_out.nodes[0].label = "checked_out_public".to_owned();
        fs::write(
            out.path().join("graph.json"),
            habitat_graph_export::to_node_link(&checked_out).unwrap(),
        )
        .unwrap();

        assert_eq!(run(src.path(), out.path()), 0);
        let rebuilt = read_graph_json(out.path());
        assert!(rebuilt.contains("source_truth"));
        assert!(!rebuilt.contains("stale_private_cache"));
    }

    #[test]
    fn unchanged_input_refreshes_legacy_outputs_and_hardens_sidecar() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let raw_label = "api_key_assignment_refused";
        mk_file(src.path(), "lib.rs", &format!("fn {raw_label}() {{}}"));
        assert_eq!(run(src.path(), out.path()), 0);

        for artifact in ["graph.json", "GRAPH_REPORT.md", "graph.html"] {
            fs::write(out.path().join(artifact), raw_label).unwrap();
        }
        for artifact in ["graph.svg", "graph.graphml", "graph.cypher"] {
            fs::write(out.path().join(artifact), raw_label).unwrap();
        }
        fs::write(
            out.path().join(".habitat-graph-artifacts.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": "habitat-graph.artifact-manifest.v1",
                "files": ["graph.svg", "graph.graphml", "graph.cypher"],
            }))
            .unwrap(),
        )
        .unwrap();
        let existing_ids: HashSet<u32> = parse_sidecar(out.path())
            .nodes
            .iter()
            .map(|node| node.id.get())
            .collect();
        let stale_id = (0..=u32::MAX)
            .find(|candidate| !existing_ids.contains(candidate))
            .unwrap();
        let wiki = out.path().join("wiki");
        fs::create_dir(&wiki).unwrap();
        fs::write(wiki.join("index.md"), raw_label).unwrap();
        fs::write(wiki.join(format!("node-{stale_id}.md")), raw_label).unwrap();
        fs::write(
            wiki.join(".habitat-graph-generated.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": "habitat-graph.wiki-manifest.v1",
                "files": ["index.md", format!("node-{stale_id}.md")],
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(wiki.join("user.md"), "keep me").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(out.path().join(SIDECAR), fs::Permissions::from_mode(0o644))
                .unwrap();
        }

        assert_eq!(run(src.path(), out.path()), 0);
        for artifact in [
            "graph.json",
            "GRAPH_REPORT.md",
            "graph.html",
            "graph.svg",
            "graph.graphml",
            "graph.cypher",
        ] {
            let refreshed = fs::read_to_string(out.path().join(artifact)).unwrap();
            assert!(
                !refreshed.contains(raw_label),
                "legacy raw label survived refresh in {artifact}"
            );
        }
        assert!(!wiki.join(format!("node-{stale_id}.md")).exists());
        assert_eq!(fs::read_to_string(wiki.join("user.md")).unwrap(), "keep me");
        for entry in fs::read_dir(&wiki).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == "user.md" {
                continue;
            }
            let refreshed = fs::read_to_string(entry.path()).unwrap();
            assert!(!refreshed.contains(raw_label));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(out.path().join(SIDECAR))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        let temporary_count = fs::read_dir(out.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".habitat-graph-state.json.tmp")
            })
            .count();
        assert_eq!(temporary_count, 0, "sidecar temp file must not survive");
    }

    #[test]
    fn existing_sidecar_is_hardened_before_source_detection_failure() {
        use std::os::unix::fs::PermissionsExt as _;

        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn private_cache() {}");
        assert_eq!(run(src.path(), out.path()), 0);

        let sidecar = out.path().join(SIDECAR);
        fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o644)).unwrap();
        let missing = src.path().join("missing");
        assert_eq!(run(&missing, out.path()), 4);

        let mode = fs::metadata(sidecar).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // T9: no-op on an empty source directory exits 0.
    #[test]
    fn noop_empty_dir_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let _ = run(src.path(), out.path()); // first build (empty)
        assert_eq!(
            run(src.path(), out.path()),
            0,
            "empty-dir no-op must exit 0"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T10–T16  Changed file
    // ─────────────────────────────────────────────────────────────────────────────

    // T10: changing a file and re-running exits 0.
    #[test]
    fn change_file_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn old() {}");
        let _ = run(src.path(), out.path());
        mk_file(src.path(), "lib.rs", "fn new_fn() {}");
        assert_eq!(run(src.path(), out.path()), 0);
    }

    // T11: after changing a file graph.json is updated (hash differs from prior run).
    #[test]
    fn change_file_graph_json_differs() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn alpha() {}");
        let _ = run(src.path(), out.path());
        let before = fs::read(out.path().join("graph.json")).unwrap();
        mk_file(src.path(), "lib.rs", "fn beta() {}");
        let _ = run(src.path(), out.path());
        let after = fs::read(out.path().join("graph.json")).unwrap();
        assert_ne!(before, after, "graph.json must change when source changes");
    }

    // T12: renaming the function in a changed file → old label is gone.
    #[test]
    fn change_function_name_old_label_gone() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn old_name() {}");
        let _ = run(src.path(), out.path());
        mk_file(src.path(), "lib.rs", "fn new_name() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            !json.contains("old_name"),
            "old function name must not appear in updated graph"
        );
    }

    // T13: renaming the function in a changed file → new label is present.
    #[test]
    fn change_function_name_new_label_present() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn old_name() {}");
        let _ = run(src.path(), out.path());
        mk_file(src.path(), "lib.rs", "fn new_name() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            json.contains("new_name"),
            "new function name must appear in updated graph"
        );
    }

    // T14: after changing a file the sidecar has a new content_hash for that path.
    #[test]
    fn change_file_sidecar_has_new_hash() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn v1() {}");
        let _ = run(src.path(), out.path());
        let g1 = parse_sidecar(out.path());
        let hash1 = g1.manifest.inputs[0].content_hash.clone();

        mk_file(src.path(), "lib.rs", "fn v2() {}");
        let _ = run(src.path(), out.path());
        let g2 = parse_sidecar(out.path());
        let hash2 = &g2.manifest.inputs[0].content_hash;

        assert_ne!(
            hash1, *hash2,
            "content_hash must change when file content changes"
        );
    }

    // T15: nodes from unchanged files are preserved after an incremental update.
    #[test]
    fn change_file_unchanged_nodes_preserved() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn keep_me() {}");
        mk_file(src.path(), "b.rs", "fn changed_fn() {}");
        let _ = run(src.path(), out.path());

        mk_file(src.path(), "b.rs", "fn renamed_fn() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            json.contains("keep_me"),
            "node from unchanged file must still be present"
        );
    }

    // T16: changing multiple files is fully reflected in the next update.
    #[test]
    fn change_multiple_files_all_updated() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn a_old() {}");
        mk_file(src.path(), "b.rs", "fn b_old() {}");
        let _ = run(src.path(), out.path());

        mk_file(src.path(), "a.rs", "fn a_new() {}");
        mk_file(src.path(), "b.rs", "fn b_new() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(!json.contains("a_old"), "a_old must be gone");
        assert!(!json.contains("b_old"), "b_old must be gone");
        assert!(json.contains("a_new"), "a_new must be present");
        assert!(json.contains("b_new"), "b_new must be present");
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T17–T21  Added file
    // ─────────────────────────────────────────────────────────────────────────────

    // T17: adding a new source file and re-running exits 0.
    #[test]
    fn add_file_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        mk_file(src.path(), "b.rs", "fn b() {}");
        assert_eq!(run(src.path(), out.path()), 0);
    }

    // T18: nodes from an added file appear in graph.json.
    #[test]
    fn add_file_nodes_appear() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn existing() {}");
        let _ = run(src.path(), out.path());
        mk_file(src.path(), "b.rs", "fn brand_new() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            json.contains("brand_new"),
            "added file's function must appear"
        );
    }

    // T19: the sidecar tracks the newly added file's path and hash.
    #[test]
    fn add_file_sidecar_tracks_it() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        mk_file(src.path(), "b.rs", "fn b() {}");
        let _ = run(src.path(), out.path());

        let g = parse_sidecar(out.path());
        let paths: Vec<&str> = g.manifest.inputs.iter().map(|r| r.path.as_str()).collect();
        assert!(
            paths.iter().any(|p| p.ends_with("b.rs")),
            "sidecar must track the new file; got {paths:?}"
        );
    }

    // T20: a file added inside a nested subdirectory is detected and included.
    #[test]
    fn add_file_in_nested_subdir() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let _ = run(src.path(), out.path()); // empty first build
        mk_file(src.path(), "deep/sub/new.rs", "fn nested_fn() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            json.contains("nested_fn"),
            "deeply nested file must be found after add"
        );
    }

    // T21: adding a file increases the node count in graph.json.
    #[test]
    fn add_file_increases_node_count() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn fn_a() {}");
        let _ = run(src.path(), out.path());
        let before = count_nodes(out.path());

        mk_file(src.path(), "b.rs", "fn fn_b() {}");
        let _ = run(src.path(), out.path());
        let after = count_nodes(out.path());
        assert!(
            after > before,
            "node count must increase after adding a file"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T22–T26  Removed file
    // ─────────────────────────────────────────────────────────────────────────────

    // T22: removing a file and re-running exits 0.
    #[test]
    fn remove_file_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn a() {}");
        mk_file(src.path(), "b.rs", "fn b() {}");
        let _ = run(src.path(), out.path());
        rm_file(src.path(), "b.rs");
        assert_eq!(run(src.path(), out.path()), 0);
    }

    // T23: nodes from a removed file are absent after the update.
    #[test]
    fn remove_file_nodes_gone() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn keep() {}");
        mk_file(src.path(), "b.rs", "fn gone() {}");
        let _ = run(src.path(), out.path());
        rm_file(src.path(), "b.rs");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            !json.contains("gone"),
            "node from removed file must be absent"
        );
        assert!(
            json.contains("keep"),
            "node from remaining file must still be present"
        );
    }

    // T24: edges whose source or target was in a removed file are also dropped.
    #[test]
    fn remove_file_edges_dropped() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        // A single file with an internal call edge: fn a() calls fn b().
        mk_file(src.path(), "ab.rs", "fn a() { b(); } fn b() {}");
        mk_file(src.path(), "other.rs", "fn other() {}");
        let _ = run(src.path(), out.path());

        rm_file(src.path(), "ab.rs");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        // After removal the links array must contain no references to "a" or "b".
        assert!(
            !json.contains("\"a\"") && !json.contains("\"b\""),
            "labels from removed file must not appear as link endpoints"
        );
    }

    // T25: the sidecar no longer tracks the removed file's path.
    #[test]
    fn remove_file_sidecar_drops_path() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn a() {}");
        mk_file(src.path(), "b.rs", "fn b() {}");
        let _ = run(src.path(), out.path());
        rm_file(src.path(), "b.rs");
        let _ = run(src.path(), out.path());

        let g = parse_sidecar(out.path());
        let paths: Vec<&str> = g.manifest.inputs.iter().map(|r| r.path.as_str()).collect();
        assert!(
            !paths.iter().any(|p| p.ends_with("b.rs")),
            "sidecar must not track the removed file; got {paths:?}"
        );
    }

    // T26: removing all source files produces an empty graph.
    #[test]
    fn remove_all_files_empty_graph() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn a() {}");
        let _ = run(src.path(), out.path());
        rm_file(src.path(), "a.rs");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            json.contains("\"nodes\": []"),
            "empty graph must have an empty nodes array"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T27–T29  Schema-version mismatch
    // ─────────────────────────────────────────────────────────────────────────────

    // T27: a sidecar with a wrong schema version triggers a full rebuild (exits 0).
    #[test]
    fn schema_mismatch_still_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn f() {}");
        let _ = run(src.path(), out.path());

        // Corrupt the sidecar's schema field to simulate a taxonomy version bump.
        let sidecar_str = read_sidecar(out.path());
        let corrupted =
            sidecar_str.replace("habitat-graph.graph.v0", "habitat-graph.graph.OLD_VERSION");
        fs::write(out.path().join(SIDECAR), corrupted).unwrap();

        assert_eq!(
            run(src.path(), out.path()),
            0,
            "schema mismatch must still succeed"
        );
    }

    // T28: after a schema-mismatch full rebuild the sidecar carries the correct schema.
    #[test]
    fn schema_mismatch_sidecar_refreshed() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn f() {}");
        let _ = run(src.path(), out.path());

        let sidecar_str = read_sidecar(out.path());
        let corrupted =
            sidecar_str.replace("habitat-graph.graph.v0", "habitat-graph.graph.OLD_VERSION");
        fs::write(out.path().join(SIDECAR), corrupted).unwrap();
        let _ = run(src.path(), out.path());

        let g = parse_sidecar(out.path());
        assert_eq!(
            g.schema,
            habitat_graph_core::SCHEMA_VERSION,
            "sidecar schema must be refreshed after mismatch rebuild"
        );
    }

    // T29: after a schema-mismatch rebuild the subsequent run works incrementally.
    #[test]
    fn schema_mismatch_subsequent_run_is_incremental() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn original() {}");
        let _ = run(src.path(), out.path());

        // Corrupt → forced full rebuild.
        let sidecar_str = read_sidecar(out.path());
        let corrupted = sidecar_str.replace("habitat-graph.graph.v0", "habitat-graph.graph.STALE");
        fs::write(out.path().join(SIDECAR), corrupted).unwrap();
        let _ = run(src.path(), out.path()); // full rebuild

        // Now a no-change incremental run should succeed and produce a correct graph.
        let rc = run(src.path(), out.path());
        assert_eq!(rc, 0);
        assert!(read_graph_json(out.path()).contains("original"));
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T30–T33  Missing / corrupted artifacts
    // ─────────────────────────────────────────────────────────────────────────────

    // T30: deleting the sidecar triggers a fresh full rebuild (exits 0).
    #[test]
    fn deleted_sidecar_triggers_full_rebuild() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn fn1() {}");
        let _ = run(src.path(), out.path());
        fs::remove_file(out.path().join(SIDECAR)).unwrap();
        assert_eq!(
            run(src.path(), out.path()),
            0,
            "missing sidecar must trigger full rebuild"
        );
    }

    // T31: a completely fresh output directory (no prior graph.json or sidecar) exits 0.
    #[test]
    fn fresh_out_dir_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn first() {}");
        // No prior run — purely first-time build.
        assert_eq!(run(src.path(), out.path()), 0);
    }

    // T32: a corrupted (non-JSON) sidecar falls back to a full rebuild (exits 0).
    #[test]
    fn corrupted_sidecar_json_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn ok() {}");
        let _ = run(src.path(), out.path());
        // Overwrite sidecar with garbage.
        fs::write(out.path().join(SIDECAR), b"NOT JSON AT ALL!!!").unwrap();
        assert_eq!(
            run(src.path(), out.path()),
            0,
            "corrupt sidecar must still succeed"
        );
    }

    // T33: after a corrupted-sidecar full rebuild the new sidecar is valid.
    #[test]
    fn corrupted_sidecar_rebuild_produces_valid_sidecar() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn fn_x() {}");
        let _ = run(src.path(), out.path());
        fs::write(out.path().join(SIDECAR), b"{ bad json").unwrap();
        let _ = run(src.path(), out.path());
        // Sidecar must now be parseable again.
        let g = parse_sidecar(out.path());
        assert_eq!(g.nodes.len(), 1);
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T34–T35  Determinism
    // ─────────────────────────────────────────────────────────────────────────────

    // T34: two fresh full builds from identical sources produce byte-identical graph.json.
    #[test]
    fn determinism_two_full_builds_identical() {
        let src = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn foo() { bar(); } fn bar() {}");

        let out1 = TempDir::new().unwrap();
        let out2 = TempDir::new().unwrap();
        let _ = run(src.path(), out1.path());
        let _ = run(src.path(), out2.path());

        let j1 = fs::read(out1.path().join("graph.json")).unwrap();
        let j2 = fs::read(out2.path().join("graph.json")).unwrap();
        assert_eq!(
            j1, j2,
            "two full builds must produce byte-identical graph.json"
        );
    }

    // T35: incremental build (re-extract changed file) produces the same graph.json as a
    // fresh full rebuild with the same final set of source files.
    #[test]
    fn determinism_incremental_equals_full_rebuild() {
        let src = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn a_v1() {}");
        mk_file(src.path(), "b.rs", "fn b_fn() {}");

        // Build 1: full build with v1 source.
        let out_incr = TempDir::new().unwrap();
        let _ = run(src.path(), out_incr.path());

        // Change a.rs to v2.
        mk_file(src.path(), "a.rs", "fn a_v2() {}");

        // Incremental update (knows v1→v2 change).
        let _ = run(src.path(), out_incr.path());
        let j_incr = fs::read(out_incr.path().join("graph.json")).unwrap();

        // Fresh full build with v2 source (no prior sidecar).
        let out_full = TempDir::new().unwrap();
        let _ = run(src.path(), out_full.path());
        let j_full = fs::read(out_full.path().join("graph.json")).unwrap();

        assert_eq!(
            j_incr, j_full,
            "incremental update must produce the same graph.json as a fresh full build"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T36–T38  Empty source directory
    // ─────────────────────────────────────────────────────────────────────────────

    // T36: empty source directory on first run exits 0.
    #[test]
    fn empty_source_dir_exits_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        assert_eq!(run(src.path(), out.path()), 0);
    }

    // T37: empty source directory produces a graph.json with an empty nodes array.
    #[test]
    fn empty_source_dir_empty_nodes_array() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            json.contains("\"nodes\": []"),
            "empty source must produce \"nodes\": []"
        );
    }

    // T38: empty source directory writes a sidecar with zero inputs.
    #[test]
    fn empty_source_dir_writes_sidecar_with_no_inputs() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let _ = run(src.path(), out.path());
        let g = parse_sidecar(out.path());
        assert!(
            g.manifest.inputs.is_empty(),
            "empty-source sidecar must have zero inputs"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T39–T41  Error handling
    // ─────────────────────────────────────────────────────────────────────────────

    // T39: non-existent source directory returns exit code 4.
    #[test]
    fn nonexistent_dir_returns_four() {
        let out = TempDir::new().unwrap();
        let phantom = std::path::Path::new("/nonexistent_habitat_graph_update_test_xyz42");
        assert_eq!(run(phantom, out.path()), 4);
    }

    // T40: output directory is created if it does not exist.
    #[test]
    fn out_dir_created_if_missing() {
        let src = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        let out = base.path().join("new_out_dir");
        mk_file(src.path(), "lib.rs", "fn f() {}");
        assert_eq!(run(src.path(), &out), 0);
        assert!(out.join("graph.json").exists());
    }

    // T41: deeply nested output directory is created.
    #[test]
    fn nested_out_dir_created() {
        let src = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        let out = base.path().join("a").join("b").join("c");
        mk_file(src.path(), "lib.rs", "fn deep() {}");
        assert_eq!(run(src.path(), &out), 0);
        assert!(out.join("graph.json").exists());
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T42–T46  Output correctness
    // ─────────────────────────────────────────────────────────────────────────────

    // T42: two functions in one file produce exactly two nodes.
    #[test]
    fn two_functions_produces_two_nodes() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn p() { q(); } fn q() {}");
        let _ = run(src.path(), out.path());
        assert_eq!(count_nodes(out.path()), 2, "must produce exactly 2 nodes");
    }

    // T43: nodes from multiple source files are all present in graph.json.
    #[test]
    fn multiple_files_all_nodes_present() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "aa.rs", "fn fn_aa() {}");
        mk_file(src.path(), "bb.rs", "fn fn_bb() {}");
        mk_file(src.path(), "cc.rs", "fn fn_cc() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(json.contains("fn_aa"), "fn_aa missing");
        assert!(json.contains("fn_bb"), "fn_bb missing");
        assert!(json.contains("fn_cc"), "fn_cc missing");
    }

    // T44: graph.json contains all required NetworkX envelope keys.
    #[test]
    fn graph_json_has_networkx_envelope_keys() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn f() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        for key in ["\"directed\"", "\"multigraph\"", "\"nodes\"", "\"links\""] {
            assert!(json.contains(key), "graph.json must contain key {key}");
        }
    }

    // T45: graph.json carries schema_version in the graph metadata object (P1-G12).
    #[test]
    fn graph_json_has_schema_version_in_envelope() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn f() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(
            json.contains("schema_version"),
            "graph.json must carry schema_version"
        );
        assert!(
            json.contains(habitat_graph_core::SCHEMA_VERSION),
            "schema_version must match SCHEMA_VERSION"
        );
    }

    // T46: the sidecar round-trips via Graph::from_json with the correct schema.
    #[test]
    fn sidecar_is_valid_internal_graph_json() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn sidecar_fn() {}");
        let _ = run(src.path(), out.path());
        let g = parse_sidecar(out.path());
        assert_eq!(
            g.schema,
            habitat_graph_core::SCHEMA_VERSION,
            "sidecar schema must equal SCHEMA_VERSION"
        );
        assert_eq!(g.nodes.len(), 1, "sidecar must contain the extracted node");
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // T47–T50  Incremental correctness
    // ─────────────────────────────────────────────────────────────────────────────

    // T47: adding a file then removing it returns the graph to its original state.
    #[test]
    fn add_then_remove_returns_to_original() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "base.rs", "fn base() {}");
        let _ = run(src.path(), out.path());
        let original_count = count_nodes(out.path());

        mk_file(src.path(), "extra.rs", "fn extra() {}");
        let _ = run(src.path(), out.path());
        assert!(
            count_nodes(out.path()) > original_count,
            "add must increase count"
        );

        rm_file(src.path(), "extra.rs");
        let _ = run(src.path(), out.path());
        assert_eq!(
            count_nodes(out.path()),
            original_count,
            "remove must restore original node count"
        );
    }

    // T48: nodes from unchanged files are not duplicated after an incremental update.
    #[test]
    fn unchanged_nodes_not_duplicated_after_incremental() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "stable.rs", "fn stable_fn() {}");
        mk_file(src.path(), "changing.rs", "fn ch_v1() {}");
        let _ = run(src.path(), out.path());

        mk_file(src.path(), "changing.rs", "fn ch_v2() {}");
        let _ = run(src.path(), out.path());

        // Exact node count: stable_fn + ch_v2 = 2 (not 3).
        assert_eq!(
            count_nodes(out.path()),
            2,
            "incremental update must not duplicate unchanged nodes"
        );
    }

    // T49: after an incremental update a subsequent no-change run is a no-op (exits 0).
    #[test]
    fn second_run_after_change_is_noop() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn init() {}");
        let _ = run(src.path(), out.path());

        mk_file(src.path(), "lib.rs", "fn updated() {}");
        let _ = run(src.path(), out.path()); // incremental

        // No further change → no-op.
        assert_eq!(run(src.path(), out.path()), 0);
        assert!(read_graph_json(out.path()).contains("updated"));
    }

    // T50: changing the same file twice produces the correct final state each time.
    #[test]
    fn change_file_twice_second_change_reflected() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn v1() {}");
        let _ = run(src.path(), out.path());

        mk_file(src.path(), "lib.rs", "fn v2() {}");
        let _ = run(src.path(), out.path());
        assert!(read_graph_json(out.path()).contains("v2"));
        assert!(!read_graph_json(out.path()).contains("v1"));

        mk_file(src.path(), "lib.rs", "fn v3() {}");
        let _ = run(src.path(), out.path());
        let json = read_graph_json(out.path());
        assert!(json.contains("v3"), "v3 must appear after second change");
        assert!(!json.contains("v2"), "v2 must be gone after second change");
        assert!(!json.contains("v1"), "v1 must still be gone");
    }

    #[test]
    fn pending_add_transaction_blocks_update_artifact_writes() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn before_pending_add() {}");
        assert_eq!(run(src.path(), out.path()), 0);
        let public_before = read_graph_json(out.path());
        fs::write(out.path().join(format!("{SIDECAR}.add-journal")), "pending").unwrap();
        mk_file(src.path(), "lib.rs", "fn changed_during_pending_add() {}");

        assert_eq!(run(src.path(), out.path()), 4);
        assert_eq!(read_graph_json(out.path()), public_before);
        assert!(out.path().join(format!("{SIDECAR}.add-journal")).exists());
    }

    #[test]
    fn pending_update_recovers_private_lineage_after_public_commit() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn local() {}");
        assert_eq!(run(src.path(), out.path()), 0);
        let state_path = out.path().join(SIDECAR);
        let stored = super::super::private_state::read(&state_path, "test state")
            .unwrap()
            .unwrap();
        let before_checksum = super::private_graph_generation(&stored.graph).unwrap();

        let remote = TempDir::new().unwrap();
        mk_file(remote.path(), "remote.rs", "fn api_key_remote() {}");
        let remote_graph = habitat_graph_build::assemble(
            habitat_graph_extract::extract_files(&[remote.path().join("remote.rs")]).unwrap(),
        );
        let pending = habitat_graph_build::merge(remote_graph, stored.graph.clone()).sorted();
        let mut state_graph = pending.clone();
        state_graph.manifest = stored.graph.manifest;
        let journal = super::UpdateJournal::new(
            out.path(),
            &state_path,
            &pending,
            state_graph,
            Some(&before_checksum),
        )
        .unwrap();
        super::write_update_journal(&state_path, &journal).unwrap();
        fs::write(
            out.path().join("graph.json"),
            habitat_graph_export::to_node_link(&pending).unwrap(),
        )
        .unwrap();

        assert_eq!(run(src.path(), out.path()), 0);
        let private = fs::read_to_string(&state_path).unwrap();
        assert!(private.contains("api_key_remote"));
        assert!(
            !super::super::private_state::update_journal_path(&state_path)
                .unwrap()
                .exists()
        );
    }
}

#[cfg(all(test, not(unix)))]
mod non_unix_tests {
    use std::fs;

    use tempfile::TempDir;

    use super::{run, SIDECAR};

    #[test]
    fn update_rejects_and_removes_unsupported_sidecar() {
        let source = TempDir::new().unwrap();
        let output = TempDir::new().unwrap();
        let sidecar = output.path().join(SIDECAR);
        fs::write(&sidecar, "private").unwrap();

        assert_eq!(run(source.path(), output.path()), 4);
        assert!(!sidecar.exists());
    }
}
