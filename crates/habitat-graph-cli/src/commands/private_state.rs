//! Owner-only persistence for raw graphs used by `update` and `add`.
//!
//! Public artifacts contain the deterministic redacted projection, so incremental operations keep
//! the complete graph separately. In a Git repository the state is keyed by canonical output path
//! and branch/detached-HEAD context below the resolved Git metadata directory; non-Git outputs use
//! the legacy hidden sidecar. Directories are hardened to `0o700` and files to `0o600` on Unix.
//! Platforms that cannot enforce those permissions fail closed and remove unsupported legacy state.
//!
//! Bounded snapshots allow a matching earlier public generation to recover after a branch switch.
//! Output locks serialize writers, while add/update journals bind interrupted transactions to their
//! originating context, lineage, public generations, and private checksums before retrying them.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::io::{BufRead as _, Read as _};
use std::path::{Path, PathBuf};

use habitat_graph_core::{Graph, GraphError, Result};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

const PRIVATE_STATE_METADATA: &str = "_habitat_graph_private_state";
const LEGACY_PRIVATE_STATE_SCHEMA: &str = "habitat-graph.private-state.v1";
const PRIVATE_STATE_SCHEMA: &str = "habitat-graph.private-state.v2";
const SNAPSHOT_SEPARATOR: &str = ".snapshot-";
const CONTEXT_SEPARATOR: &str = ".context-";
const CONTEXT_REVISION_SEPARATOR: &str = ".context-revision-";
const EXPIRED_CONTEXT_SEPARATOR: &str = ".expired-context-";
const EXPIRED_REVISION_SEPARATOR: &str = ".expired-revision-";
const MIGRATION_CONFLICT_SEPARATOR: &str = ".migration-conflict-";
const MIGRATION_CONFLICT_DIRECTORY: &str = ".migration-conflicts";
const ADD_JOURNAL_SUFFIX: &str = ".add-journal";
const UPDATE_JOURNAL_SUFFIX: &str = ".update-journal";
const OUTPUT_LOCK_SUFFIX: &str = ".output-lock";
const MAX_SNAPSHOTS: usize = 16;
const MAX_CONTEXTS_PER_OUTPUT: usize = 16;
const MAX_GIT_CONTROL_LINE_BYTES: u64 = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct StoredGraph {
    pub(super) graph: Graph,
    pub(super) public_generation: Option<String>,
    pub(super) public_semantic_generation: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ContextIdentity {
    pub(super) key: String,
    pub(super) lineage: String,
    revision: String,
    unborn_predecessor: Option<(String, String)>,
}

pub(super) fn path_for_output(output: &Path, legacy: &Path) -> Result<PathBuf> {
    #[cfg(not(unix))]
    remove_family(legacy, "unsupported legacy private state")?;

    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
        GraphError::Io(format!(
            "resolve output directory {}: {error}",
            parent.display()
        ))
    })?;
    let Some(git_dir) = find_git_dir(&canonical_parent)? else {
        return Ok(legacy.to_path_buf());
    };

    let state_root = git_dir.join("habitat-graph");
    let state_dir = state_root.join("state");
    ensure_private_directory(&state_root)?;
    ensure_private_directory(&state_dir)?;

    let canonical_output = match std::fs::canonicalize(output) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => canonical_parent.join(
            output
                .file_name()
                .ok_or_else(|| GraphError::Io("output path has no filename".to_owned()))?,
        ),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "resolve output path {}: {error}",
                output.display()
            )))
        }
    };
    let key = output_key(&canonical_output);
    let unscoped_state_path = state_dir.join(format!("{key}.json"));
    let context = git_context_identity(&git_dir)?;
    let state_path = state_dir.join(format!("{key}{CONTEXT_SEPARATOR}{}.json", context.key));
    ensure_context_not_expired(&state_path)?;
    ensure_revision_not_expired(&state_path, &context.revision)?;
    write_context_revision(&state_path, &context.revision)?;
    let mut first_error = None;
    for result in [
        migrate_family(&unscoped_state_path, &state_path, "unscoped private state"),
        migrate_family(legacy, &state_path, "legacy private state"),
        ensure_no_family_conflicts(&state_path),
    ] {
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(state_path)
}

pub(super) fn context_identity_for_output(output: &Path) -> Result<Option<ContextIdentity>> {
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
        GraphError::Io(format!(
            "resolve output directory {}: {error}",
            parent.display()
        ))
    })?;
    let Some(git_dir) = find_git_dir(&canonical_parent)? else {
        return Ok(None);
    };
    git_context_identity(&git_dir).map(Some)
}

#[derive(Debug)]
pub(super) struct OutputTransactionLock {
    file: std::fs::File,
}

impl Drop for OutputTransactionLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub(super) fn acquire_output_lock(state_path: &Path) -> Result<OutputTransactionLock> {
    let lock_path = output_lock_path(state_path)?;
    if let Some(parent) = lock_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| GraphError::Io(format!("create output lock directory: {error}")))?;
    }
    for _ in 0..4 {
        let prior_metadata = match std::fs::symlink_metadata(&lock_path) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(GraphError::Guard(format!(
                        "output transaction lock is not a regular file: {}",
                        lock_path.display()
                    )));
                }
                Some(metadata)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(GraphError::Io(format!(
                    "inspect output transaction lock: {error}"
                )))
            }
        };
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true);
        if prior_metadata.is_some() {
            options.create(false);
        } else {
            options.create_new(true);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = match options.open(&lock_path) {
            Ok(file) => file,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::NotFound
                ) =>
            {
                continue;
            }
            Err(error) => {
                return Err(GraphError::Io(format!(
                    "open output transaction lock: {error}"
                )))
            }
        };
        let opened_metadata = file
            .metadata()
            .map_err(|error| GraphError::Io(format!("inspect output transaction lock: {error}")))?;
        if !opened_metadata.file_type().is_file()
            || prior_metadata
                .as_ref()
                .is_some_and(|metadata| !same_file(metadata, &opened_metadata))
        {
            return Err(GraphError::Guard(
                "output transaction lock changed while being opened".to_owned(),
            ));
        }
        #[cfg(unix)]
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| GraphError::Io(format!("harden output transaction lock: {error}")))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(GraphError::Guard(
                    "another habitat-graph writer is updating this output".to_owned(),
                ))
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(GraphError::Io(format!("lock output transaction: {error}")))
            }
        }
        let current_metadata = std::fs::symlink_metadata(&lock_path).map_err(|error| {
            GraphError::Guard(format!(
                "output transaction lock changed while being opened: {error}"
            ))
        })?;
        if !current_metadata.file_type().is_file()
            || !same_file(&opened_metadata, &current_metadata)
        {
            return Err(GraphError::Guard(
                "output transaction lock changed while being opened".to_owned(),
            ));
        }
        return Ok(OutputTransactionLock { file });
    }
    Err(GraphError::Guard(
        "output transaction lock changed while being opened".to_owned(),
    ))
}

fn output_lock_path(state_path: &Path) -> Result<PathBuf> {
    let filename = state_path
        .file_name()
        .and_then(|filename| filename.to_str())
        .ok_or_else(|| GraphError::Io("private state path has no UTF-8 filename".to_owned()))?;
    let base = if state_context_key(state_path).is_some() {
        filename
            .split_once(CONTEXT_SEPARATOR)
            .map_or(filename, |(key, _)| key)
    } else {
        filename
    };
    Ok(state_path.with_file_name(format!("{base}{OUTPUT_LOCK_SUFFIX}")))
}

pub(super) fn generation(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

pub(super) fn semantic_generation(graph: &Graph) -> Result<String> {
    let mut state = graph.clone().sorted();
    state.edges.sort_by(|left, right| {
        (
            left.source,
            left.target,
            left.relation.as_str(),
            left.confidence,
        )
            .cmp(&(
                right.source,
                right.target,
                right.relation.as_str(),
                right.confidence,
            ))
    });
    state.manifest.inputs.clear();
    state.manifest.tool_version.clear();
    state.manifest.generated_at = None;
    for community in &mut state.communities {
        community.label.clear();
    }
    Ok(generation(state.to_json()?.as_bytes()))
}

pub(super) fn serialize(
    graph: &Graph,
    public_generation: &str,
    public_semantic_generation: &str,
) -> Result<Vec<u8>> {
    validate_generation(public_generation, "public_generation")?;
    validate_generation(public_semantic_generation, "public_semantic_generation")?;
    let mut value = serde_json::to_value(graph)
        .map_err(|error| GraphError::Schema(format!("private state serialize: {error}")))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| GraphError::Schema("private graph state must be an object".to_owned()))?;
    object.insert(
        PRIVATE_STATE_METADATA.to_owned(),
        serde_json::json!({
            "schema": PRIVATE_STATE_SCHEMA,
            "public_generation": public_generation,
            "public_semantic_generation": public_semantic_generation,
        }),
    );
    serde_json::to_vec_pretty(&value)
        .map_err(|error| GraphError::Schema(format!("private state serialize: {error}")))
}

pub(super) fn parse(text: &str) -> Result<StoredGraph> {
    let graph = Graph::from_json(text)?;
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| GraphError::Schema(format!("private state parse: {error}")))?;
    let (public_generation, public_semantic_generation) = match value.get(PRIVATE_STATE_METADATA) {
        None => (None, None),
        Some(metadata) => {
            let schema = metadata["schema"].as_str();
            if !matches!(
                schema,
                Some(PRIVATE_STATE_SCHEMA | LEGACY_PRIVATE_STATE_SCHEMA)
            ) {
                return Err(GraphError::Schema(format!(
                    "unsupported private state metadata schema: {:?}",
                    metadata["schema"]
                )));
            }
            let generation = metadata["public_generation"].as_str().ok_or_else(|| {
                GraphError::Schema("private state `public_generation` must be a string".to_owned())
            })?;
            validate_generation(generation, "public_generation")?;
            let semantic = if schema == Some(PRIVATE_STATE_SCHEMA) {
                let semantic =
                    metadata["public_semantic_generation"]
                        .as_str()
                        .ok_or_else(|| {
                            GraphError::Schema(
                                "private state `public_semantic_generation` must be a string"
                                    .to_owned(),
                            )
                        })?;
                validate_generation(semantic, "public_semantic_generation")?;
                Some(semantic.to_owned())
            } else {
                None
            };
            (Some(generation.to_owned()), semantic)
        }
    };
    Ok(StoredGraph {
        graph,
        public_generation,
        public_semantic_generation,
    })
}

fn validate_generation(value: &str, field: &str) -> Result<()> {
    if is_generation(value) {
        Ok(())
    } else {
        Err(GraphError::Schema(format!(
            "private state `{field}` is invalid"
        )))
    }
}

pub(super) fn is_generation(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn matches_public(
    stored: &StoredGraph,
    public_generation: Option<&str>,
    public_semantic_generation: Option<&str>,
) -> bool {
    stored
        .public_generation
        .as_deref()
        .zip(public_generation)
        .is_some_and(|(stored, public)| stored == public)
        || stored
            .public_semantic_generation
            .as_deref()
            .zip(public_semantic_generation)
            .is_some_and(|(stored, public)| stored == public)
}

pub(super) fn read(path: &Path, context: &str) -> Result<Option<StoredGraph>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "inspect {context} {}: {error}",
                path.display()
            )))
        }
    };
    if !metadata.file_type().is_file() {
        return Err(GraphError::Guard(format!(
            "{context} is not a regular file: {}",
            path.display()
        )));
    }
    ensure(path)?;
    let text = std::fs::read_to_string(path)
        .map_err(|error| GraphError::Io(format!("read {context}: {error}")))?;
    parse(&text).map(Some)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct PrivateChecksumStatus {
    pub(super) any: bool,
    pub(super) matched: bool,
}

pub(super) fn private_checksum_status(
    path: &Path,
    expected: &[&str],
) -> Result<PrivateChecksumStatus> {
    let mut status = PrivateChecksumStatus::default();
    let mut candidates = BTreeSet::new();
    for context_path in state_context_candidates(path)? {
        candidates.insert(context_path.clone());
        candidates.extend(snapshot_candidates(&context_path)?);
    }
    for candidate in candidates {
        let Some(stored) = read(&candidate, "private transaction lineage state")? else {
            continue;
        };
        status.any = true;
        let checksum = generation(stored.graph.to_json()?.as_bytes());
        if expected.contains(&checksum.as_str()) {
            status.matched = true;
        }
    }
    Ok(status)
}

pub(super) fn private_checksum_status_at_path(
    path: &Path,
    expected: &[&str],
) -> Result<PrivateChecksumStatus> {
    let Some(stored) = read(path, "private transaction target state")? else {
        return Ok(PrivateChecksumStatus::default());
    };
    let checksum = generation(stored.graph.to_json()?.as_bytes());
    Ok(PrivateChecksumStatus {
        any: true,
        matched: expected.contains(&checksum.as_str()),
    })
}

#[derive(Default)]
struct ContextAncestry {
    verified: bool,
    known: HashSet<PathBuf>,
    ancestors: HashSet<PathBuf>,
}

impl ContextAncestry {
    fn status(&self, path: &Path) -> Option<bool> {
        if !self.verified || !self.known.contains(path) {
            None
        } else {
            Some(self.ancestors.contains(path))
        }
    }
}

fn git_dir_for_state(path: &Path) -> Option<PathBuf> {
    let state_dir = path.parent()?;
    if state_dir.file_name()? != "state" {
        return None;
    }
    let private_root = state_dir.parent()?;
    if private_root.file_name()? != "habitat-graph" {
        return None;
    }
    private_root.parent().map(Path::to_path_buf)
}

fn context_ancestry(path: &Path, candidates: &[PathBuf]) -> Result<ContextAncestry> {
    let Some(git_dir) = git_dir_for_state(path) else {
        return Ok(ContextAncestry::default());
    };
    let Some(expected_head) = context_revision(path)? else {
        return Ok(ContextAncestry::default());
    };
    let current_identity = git_context_identity(&git_dir)?;
    if current_identity.revision != expected_head
        || state_context_key(path).as_deref() != Some(current_identity.key.as_str())
    {
        return Ok(ContextAncestry::default());
    }
    let mut revisions: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut direct_ancestors = HashSet::new();
    for candidate in candidates {
        if candidate == path {
            continue;
        }
        if let Some(revision) = context_revision(candidate)? {
            if current_identity.unborn_predecessor.as_ref().is_some_and(
                |(context, predecessor_revision)| {
                    state_context_key(candidate).as_deref() == Some(context.as_str())
                        && revision == *predecessor_revision
                },
            ) {
                direct_ancestors.insert(candidate.clone());
            }
            revisions
                .entry(revision)
                .or_default()
                .push(candidate.clone());
        }
    }
    if revisions.is_empty() {
        return Ok(ContextAncestry::default());
    }

    let Ok(mut child) = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(&git_dir)
        .args(["rev-list", "HEAD"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return Ok(ContextAncestry::default());
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| GraphError::Io("read Git ancestry output".to_owned()))?;
    let mut ancestors = direct_ancestors;
    let mut observed_head = None;
    let mut line = String::new();
    let mut reader = std::io::BufReader::new(stdout);
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| GraphError::Io(format!("read Git ancestry: {error}")))?;
        if read == 0 {
            break;
        }
        let commit = line.trim();
        if !is_object_id(commit) || read > 130 {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(ContextAncestry::default());
        }
        let revision = generation(format!("commit:{}", commit.to_ascii_lowercase()).as_bytes());
        observed_head.get_or_insert_with(|| revision.clone());
        if let Some(paths) = revisions.get(&revision) {
            ancestors.extend(paths.iter().cloned());
        }
    }
    let status = child
        .wait()
        .map_err(|error| GraphError::Io(format!("wait for Git ancestry: {error}")))?;
    if !status.success() || observed_head.as_deref() != Some(expected_head.as_str()) {
        return Ok(ContextAncestry::default());
    }
    Ok(ContextAncestry {
        verified: true,
        known: revisions.into_values().flatten().collect(),
        ancestors,
    })
}

pub(super) fn context_is_verified_ancestor(ancestor: &Path, current: &Path) -> Result<bool> {
    if ancestor == current {
        return Ok(true);
    }
    let ancestry = context_ancestry(current, &[ancestor.to_path_buf(), current.to_path_buf()])?;
    match ancestry.status(ancestor) {
        Some(result) => Ok(result),
        None => Err(GraphError::Guard(format!(
            "cannot verify private state Git ancestry: {}",
            ancestor.display()
        ))),
    }
}

pub(super) fn load_matching(
    path: &Path,
    public_generation: Option<&str>,
    public_semantic_generation: Option<&str>,
    context: &str,
) -> Result<Option<StoredGraph>> {
    ensure_context_not_expired(path)?;
    let current = read(path, context)?;
    let candidates = state_context_candidates(path)?;

    if let Some(generation) = public_generation {
        let mut exact = None;
        if current
            .as_ref()
            .is_some_and(|stored| stored.public_generation.as_deref() == Some(generation))
        {
            if let Some(stored) = current.clone() {
                select_unique_state(&mut exact, stored, path)?;
            }
        }
        let snapshot = snapshot_path(path, generation)?;
        if let Some(stored) = read(&snapshot, context)? {
            if stored.public_generation.as_deref() == Some(generation) {
                select_unique_state(&mut exact, stored, path)?;
            }
        }
        if let Some(stored) = exact {
            return Ok(Some(stored));
        }

        let ancestry = context_ancestry(path, &candidates)?;
        let mut inherited = None;
        for candidate in &candidates {
            if candidate == path || context_is_expired(candidate)? {
                continue;
            }
            if let Some(stored) = read(candidate, context)? {
                if stored.public_generation.as_deref() == Some(generation) {
                    select_inherited_state(&mut inherited, stored, candidate, &ancestry, path)?;
                }
            }
            let snapshot = snapshot_path(candidate, generation)?;
            if let Some(stored) = read(&snapshot, context)? {
                if stored.public_generation.as_deref() == Some(generation) {
                    select_inherited_state(&mut inherited, stored, candidate, &ancestry, path)?;
                }
            }
        }
        if let Some(stored) = inherited {
            return Ok(Some(stored));
        }
    }

    let Some(semantic_generation) = public_semantic_generation else {
        return Ok(None);
    };
    let ancestry = context_ancestry(path, &candidates)?;
    let mut selected = None;
    for candidate in candidates {
        if candidate != path && context_is_expired(&candidate)? {
            continue;
        }
        if candidate == path {
            if let Some(stored) = current.clone().filter(|stored| {
                stored.public_semantic_generation.as_deref() == Some(semantic_generation)
            }) {
                select_unique_state(&mut selected, stored, path)?;
            }
        } else if let Some(stored) = read(&candidate, context)? {
            if stored.public_semantic_generation.as_deref() == Some(semantic_generation) {
                select_inherited_state(&mut selected, stored, &candidate, &ancestry, path)?;
            }
        }
        let mut snapshots = snapshot_candidates(&candidate)?;
        snapshots.sort_unstable();
        for snapshot in snapshots {
            if let Some(stored) = read(&snapshot, context)? {
                if stored.public_semantic_generation.as_deref() == Some(semantic_generation) {
                    if candidate == path {
                        select_unique_state(&mut selected, stored, path)?;
                    } else {
                        select_inherited_state(&mut selected, stored, &candidate, &ancestry, path)?;
                    }
                }
            }
        }
    }
    Ok(selected)
}

fn select_inherited_state(
    selected: &mut Option<StoredGraph>,
    candidate: StoredGraph,
    candidate_path: &Path,
    ancestry: &ContextAncestry,
    current_path: &Path,
) -> Result<()> {
    match ancestry.status(candidate_path) {
        Some(true) => select_unique_state(selected, candidate, current_path),
        Some(false) => Ok(()),
        None => Err(GraphError::Guard(format!(
            "cannot verify ancestry for matching private state: {}",
            candidate_path.display()
        ))),
    }
}

fn select_unique_state(
    selected: &mut Option<StoredGraph>,
    candidate: StoredGraph,
    path: &Path,
) -> Result<()> {
    if selected
        .as_ref()
        .is_some_and(|existing| existing.graph != candidate.graph)
    {
        return Err(GraphError::Guard(format!(
            "multiple private states match the public graph: {}",
            path.display()
        )));
    }
    selected.get_or_insert(candidate);
    Ok(())
}

pub(super) fn write_state(path: &Path, bytes: &[u8]) -> Result<()> {
    ensure_context_not_expired(path)?;
    let text = std::str::from_utf8(bytes)
        .map_err(|error| GraphError::Schema(format!("private state serialize: {error}")))?;
    let replacement = parse(text)?;
    let existing = match read(path, "private state") {
        Ok(existing) => existing,
        Err(error) if error.kind() == "schema" => None,
        Err(error) => return Err(error),
    };
    if let Some(existing) = existing {
        if existing != replacement && existing.public_generation != replacement.public_generation {
            if let Some(existing_generation) = existing.public_generation.as_deref() {
                let existing_bytes = std::fs::read(path)
                    .map_err(|error| GraphError::Io(format!("read private state: {error}")))?;
                let snapshot = snapshot_path(path, existing_generation)?;
                match read(&snapshot, "private state snapshot")? {
                    None => write(&snapshot, &existing_bytes)?,
                    Some(snapshot_state) if snapshot_state == existing => {}
                    Some(_) => {
                        return Err(GraphError::Guard(format!(
                            "private state snapshot generation is ambiguous: {}",
                            snapshot.display()
                        )))
                    }
                }
            }
        }
    }
    write(path, bytes)?;
    prune_snapshots(path)?;
    prune_contexts(path)
}

fn snapshot_path(path: &Path, public_generation: &str) -> Result<PathBuf> {
    validate_generation(public_generation, "public_generation")?;
    let filename = path
        .file_name()
        .ok_or_else(|| GraphError::Io("private state path has no filename".to_owned()))?;
    let mut snapshot_name = OsString::from(filename);
    snapshot_name.push(SNAPSHOT_SEPARATOR);
    snapshot_name.push(public_generation);
    Ok(path.with_file_name(snapshot_name))
}

fn snapshot_candidates(path: &Path) -> Result<Vec<PathBuf>> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let filename = path
        .file_name()
        .ok_or_else(|| GraphError::Io("private state path has no filename".to_owned()))?;
    let prefix = format!("{}{SNAPSHOT_SEPARATOR}", filename.to_string_lossy());
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "list private state snapshots: {error}"
            )))
        }
    };
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|error| GraphError::Io(format!("list private state snapshots: {error}")))?;
        let filename = entry.file_name();
        let filename = filename.to_string_lossy();
        if filename.strip_prefix(&prefix).is_some_and(is_generation) {
            candidates.push(entry.path());
        }
    }
    Ok(candidates)
}

fn prune_snapshots(path: &Path) -> Result<()> {
    let candidates = snapshot_candidates(path)?;
    if candidates.len() <= MAX_SNAPSHOTS {
        return Ok(());
    }
    let mut dated = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let metadata = std::fs::symlink_metadata(&candidate)
            .map_err(|error| GraphError::Io(format!("inspect private state snapshot: {error}")))?;
        if !metadata.file_type().is_file() {
            return Err(GraphError::Guard(format!(
                "private state snapshot is not a regular file: {}",
                candidate.display()
            )));
        }
        let modified = metadata
            .modified()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        dated.push((modified, candidate));
    }
    dated.sort_unstable();
    let remove_count = dated.len().saturating_sub(MAX_SNAPSHOTS);
    for (_, candidate) in dated.into_iter().take(remove_count) {
        remove(&candidate, "expired private state snapshot")?;
    }
    Ok(())
}

fn context_state_candidates(path: &Path) -> Result<Vec<PathBuf>> {
    let Some(prefix) = context_filename_prefix(path) else {
        return Ok(Vec::new());
    };
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "list private state contexts: {error}"
            )))
        }
    };
    let mut candidates = BTreeSet::new();
    for entry in entries {
        let entry = entry
            .map_err(|error| GraphError::Io(format!("list private state contexts: {error}")))?;
        let filename = entry.file_name();
        let Some(filename) = filename.to_str() else {
            continue;
        };
        let Some(context) = context_member_key(filename, &prefix) else {
            continue;
        };
        candidates.insert(parent.join(format!("{prefix}{context}.json")));
    }
    Ok(candidates.into_iter().collect())
}

pub(super) fn state_context_candidates(path: &Path) -> Result<Vec<PathBuf>> {
    let mut candidates: BTreeSet<PathBuf> = context_state_candidates(path)?.into_iter().collect();
    candidates.insert(path.to_path_buf());
    Ok(candidates.into_iter().collect())
}

pub(super) fn ensure_no_pending_add_journals(path: &Path) -> Result<()> {
    if let Some(member) = pending_transaction_paths(path, ADD_JOURNAL_SUFFIX)?
        .into_iter()
        .next()
    {
        return Err(GraphError::Guard(format!(
            "pending add transaction must be recovered before update: {}",
            member.display()
        )));
    }
    Ok(())
}

pub(super) fn ensure_no_pending_update_journals(path: &Path) -> Result<()> {
    if let Some(member) = pending_transaction_paths(path, UPDATE_JOURNAL_SUFFIX)?
        .into_iter()
        .next()
    {
        return Err(GraphError::Guard(format!(
            "pending update transaction must be recovered before writing: {}",
            member.display()
        )));
    }
    Ok(())
}

pub(super) fn update_journal_path(path: &Path) -> Result<PathBuf> {
    transaction_path(path, UPDATE_JOURNAL_SUFFIX)
}

pub(super) fn pending_update_journals(path: &Path) -> Result<Vec<PathBuf>> {
    pending_transaction_paths(path, UPDATE_JOURNAL_SUFFIX)
}

fn transaction_path(path: &Path, suffix: &str) -> Result<PathBuf> {
    let filename = path
        .file_name()
        .ok_or_else(|| GraphError::Io("private state path has no filename".to_owned()))?;
    let mut name = OsString::from(filename);
    name.push(suffix);
    Ok(path.with_file_name(name))
}

fn pending_transaction_paths(path: &Path, suffix: &str) -> Result<Vec<PathBuf>> {
    let mut paths = BTreeSet::new();
    for candidate in state_context_candidates(path)? {
        for (member, member_suffix) in family_members(&candidate)? {
            if member_suffix.starts_with(suffix) {
                paths.insert(member);
            }
        }
    }
    Ok(paths.into_iter().collect())
}

fn context_filename_prefix(path: &Path) -> Option<String> {
    let filename = path.file_name()?.to_str()?;
    let (key, context) = filename.split_once(CONTEXT_SEPARATOR)?;
    let context = context.strip_suffix(".json")?;
    if key.is_empty() || !is_generation(context) {
        return None;
    }
    Some(format!("{key}{CONTEXT_SEPARATOR}"))
}

pub(super) fn state_context_key(path: &Path) -> Option<String> {
    let filename = path.file_name()?.to_str()?;
    let (key, context) = filename.split_once(CONTEXT_SEPARATOR)?;
    let context = context.strip_suffix(".json")?;
    (is_generation(key) && is_generation(context)).then(|| context.to_owned())
}

fn expired_context_path(path: &Path) -> Option<PathBuf> {
    let filename = path.file_name()?.to_str()?;
    let (key, context) = filename.split_once(CONTEXT_SEPARATOR)?;
    let context = context.strip_suffix(".json")?;
    if !is_generation(key) || !is_generation(context) {
        return None;
    }
    Some(path.with_file_name(format!("{key}{EXPIRED_CONTEXT_SEPARATOR}{context}")))
}

fn marker_exists(path: &Path, context: &str) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(GraphError::Io(format!("inspect {context}: {error}"))),
    };
    if !metadata.file_type().is_file() {
        return Err(GraphError::Guard(format!(
            "{context} is not a regular file: {}",
            path.display()
        )));
    }
    ensure(path)?;
    Ok(true)
}

fn write_marker(path: &Path, context: &str) -> Result<()> {
    if marker_exists(path, context)? {
        Ok(())
    } else {
        write(path, b"")
    }
}

fn context_revision_prefix(path: &Path) -> Option<String> {
    let filename = path.file_name()?.to_str()?;
    let (key, context) = filename.split_once(CONTEXT_SEPARATOR)?;
    let context = context.strip_suffix(".json")?;
    if !is_generation(key) || !is_generation(context) {
        return None;
    }
    Some(format!("{key}{CONTEXT_REVISION_SEPARATOR}{context}-"))
}

fn context_revision(path: &Path) -> Result<Option<String>> {
    let Some(prefix) = context_revision_prefix(path) else {
        return Ok(None);
    };
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let entries = std::fs::read_dir(parent)
        .map_err(|error| GraphError::Io(format!("list private state revisions: {error}")))?;
    let mut revisions = BTreeSet::new();
    for entry in entries {
        let entry = entry
            .map_err(|error| GraphError::Io(format!("list private state revisions: {error}")))?;
        let filename = entry.file_name();
        let Some(revision) = filename
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
        else {
            continue;
        };
        if !is_generation(revision) {
            continue;
        }
        marker_exists(&entry.path(), "private state revision marker")?;
        revisions.insert(revision.to_owned());
    }
    if revisions.len() > 1 {
        return Err(GraphError::Guard(format!(
            "private state context has conflicting revision provenance: {}",
            path.display()
        )));
    }
    Ok(revisions.pop_first())
}

fn context_revision_path(path: &Path, revision: &str) -> Option<PathBuf> {
    if !is_generation(revision) {
        return None;
    }
    Some(path.with_file_name(format!("{}{revision}", context_revision_prefix(path)?)))
}

fn write_context_revision(path: &Path, revision: &str) -> Result<()> {
    if context_revision(path)?
        .as_deref()
        .is_some_and(|stored| stored != revision)
    {
        return Err(GraphError::Guard(format!(
            "private state context revision changed: {}",
            path.display()
        )));
    }
    let marker = context_revision_path(path, revision).ok_or_else(|| {
        GraphError::Guard(format!(
            "cannot record unscoped private state revision: {}",
            path.display()
        ))
    })?;
    write_marker(&marker, "private state revision marker")
}

fn expired_revision_path(path: &Path, revision: &str) -> Option<PathBuf> {
    if !is_generation(revision) {
        return None;
    }
    let filename = path.file_name()?.to_str()?;
    let (key, context) = filename.split_once(CONTEXT_SEPARATOR)?;
    if !is_generation(key) || !context.strip_suffix(".json").is_some_and(is_generation) {
        return None;
    }
    Some(path.with_file_name(format!("{key}{EXPIRED_REVISION_SEPARATOR}{revision}")))
}

fn ensure_revision_not_expired(path: &Path, revision: &str) -> Result<()> {
    let Some(expired) = expired_revision_path(path, revision) else {
        return Ok(());
    };
    if marker_exists(&expired, "expired private state revision marker")? {
        return Err(GraphError::Guard(format!(
            "private state for this Git revision has expired: {}",
            path.display()
        )));
    }
    Ok(())
}

fn mark_context_revision_expired(path: &Path) -> Result<()> {
    let revision = context_revision(path)?.ok_or_else(|| {
        GraphError::Guard(format!(
            "private state context lacks revision provenance: {}",
            path.display()
        ))
    })?;
    let expired = expired_revision_path(path, &revision).ok_or_else(|| {
        GraphError::Guard(format!(
            "cannot expire unscoped private state revision: {}",
            path.display()
        ))
    })?;
    write_marker(&expired, "expired private state revision marker")
}

fn remove_context_revision(path: &Path) -> Result<()> {
    let Some(revision) = context_revision(path)? else {
        return Ok(());
    };
    let marker = context_revision_path(path, &revision).ok_or_else(|| {
        GraphError::Guard(format!(
            "cannot remove unscoped private state revision: {}",
            path.display()
        ))
    })?;
    remove(&marker, "expired private state revision")
}

fn ensure_context_not_expired(path: &Path) -> Result<()> {
    if !context_is_expired(path)? {
        return Ok(());
    }
    Err(GraphError::Guard(format!(
        "private state for this Git history context has expired: {}",
        path.display()
    )))
}

fn context_is_expired(path: &Path) -> Result<bool> {
    let Some(expired) = expired_context_path(path) else {
        return Ok(false);
    };
    marker_exists(&expired, "expired private state context marker")
}

fn mark_context_expired(path: &Path) -> Result<()> {
    let Some(expired) = expired_context_path(path) else {
        return Err(GraphError::Guard(format!(
            "cannot expire unscoped private state context: {}",
            path.display()
        )));
    };
    write_marker(&expired, "expired private state context marker")
}

fn context_member_key<'a>(filename: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = filename.strip_prefix(prefix)?;
    let context = rest.get(..64)?;
    if !is_generation(context) {
        return None;
    }
    let suffix = rest.get(64..)?.strip_prefix(".json")?;
    (suffix.is_empty()
        || suffix.starts_with(SNAPSHOT_SEPARATOR)
        || suffix.starts_with(ADD_JOURNAL_SUFFIX)
        || suffix.starts_with(UPDATE_JOURNAL_SUFFIX)
        || suffix.starts_with(MIGRATION_CONFLICT_SEPARATOR))
    .then_some(context)
}

fn prune_contexts(path: &Path) -> Result<()> {
    let candidates = context_state_candidates(path)?;
    if candidates.len() <= MAX_CONTEXTS_PER_OUTPUT {
        return Ok(());
    }

    let mut removable = Vec::new();
    let mut protected = 0;
    for candidate in &candidates {
        let members = family_members(candidate)?;
        let keep = candidate == path
            || has_migration_conflicts(candidate)?
            || members.iter().any(|(_, suffix)| {
                suffix.contains(ADD_JOURNAL_SUFFIX)
                    || suffix.contains(UPDATE_JOURNAL_SUFFIX)
                    || suffix.contains(MIGRATION_CONFLICT_SEPARATOR)
            });
        let mut modified = std::time::SystemTime::UNIX_EPOCH;
        for (member, _) in &members {
            let metadata = std::fs::symlink_metadata(member).map_err(|error| {
                GraphError::Io(format!("inspect private state context: {error}"))
            })?;
            if !metadata.file_type().is_file() {
                return Err(GraphError::Guard(format!(
                    "private state context member is not a regular file: {}",
                    member.display()
                )));
            }
            modified = modified.max(
                metadata
                    .modified()
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            );
        }
        if keep {
            protected += 1;
        } else {
            removable.push((modified, candidate.clone()));
        }
    }
    removable.sort_unstable();

    let remove_count = candidates.len().saturating_sub(MAX_CONTEXTS_PER_OUTPUT);
    if removable.len() < remove_count {
        return Err(GraphError::Guard(format!(
            "private state context retention is blocked by {protected} protected contexts"
        )));
    }
    for (_, candidate) in removable.into_iter().take(remove_count) {
        mark_context_revision_expired(&candidate)?;
        mark_context_expired(&candidate)?;
        remove_family(&candidate, "expired private state context")?;
        remove_context_revision(&candidate)?;
    }
    Ok(())
}

pub(super) fn migrate(legacy: &Path, current: &Path, context: &str) -> Result<()> {
    if legacy == current {
        return Ok(());
    }
    ensure(current)?;
    let metadata = match std::fs::symlink_metadata(legacy) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ensure_no_migration_conflicts(current)
        }
        Err(error) => {
            return Err(GraphError::Io(format!(
                "inspect {context} {}: {error}",
                legacy.display()
            )))
        }
    };
    if !metadata.file_type().is_file() {
        return Err(GraphError::Guard(format!(
            "{context} is not a regular file: {}",
            legacy.display()
        )));
    }
    ensure(legacy)?;
    let bytes = read_regular(legacy, &metadata, context)?;
    let current_metadata = match std::fs::symlink_metadata(current) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(GraphError::Io(format!(
                "inspect migrated {context} {}: {error}",
                current.display()
            )))
        }
    };
    if let Some(current_metadata) = current_metadata {
        if !current_metadata.file_type().is_file() {
            return Err(GraphError::Guard(format!(
                "migrated {context} is not a regular file: {}",
                current.display()
            )));
        }
        let current_bytes = read_regular(current, &current_metadata, context)?;
        if current_bytes != bytes {
            let conflict = store_migration_conflict(current, &bytes, context)?;
            remove(legacy, context)?;
            return Err(GraphError::Guard(format!(
                "conflicting {context} preserved at {}",
                conflict.display()
            )));
        }
    } else {
        write(current, &bytes)?;
    }
    remove(legacy, context)?;
    ensure_no_migration_conflicts(current)
}

fn migrate_family(legacy: &Path, current: &Path, context: &str) -> Result<()> {
    let mut first_error = None;
    for (source, suffix) in family_members(legacy)? {
        let result = if suffix.contains(MIGRATION_CONFLICT_SEPARATOR) {
            preserve_migration_conflict(&source, current, context)
        } else {
            let filename = current
                .file_name()
                .ok_or_else(|| GraphError::Io("private state path has no filename".to_owned()))?;
            let mut destination_name = OsString::from(filename);
            destination_name.push(&suffix);
            let destination = current.with_file_name(destination_name);
            migrate(&source, &destination, context)
        };
        if let Err(error) = result {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    if let Err(error) = migrate_conflict_family(legacy, current, context) {
        if first_error.is_none() {
            first_error = Some(error);
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(())
}

fn remove_family(path: &Path, context: &str) -> Result<()> {
    for (member, _) in family_members(path)? {
        remove(&member, context)?;
    }
    for conflict in migration_conflict_candidates(path)? {
        remove(&conflict, context)?;
    }
    Ok(())
}

fn family_members(path: &Path) -> Result<Vec<(PathBuf, String)>> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base = path
        .file_name()
        .ok_or_else(|| GraphError::Io("private state path has no filename".to_owned()))?
        .to_string_lossy();
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "list private state family: {error}"
            )))
        }
    };
    let mut members = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| GraphError::Io(format!("list private state family: {error}")))?;
        let filename = entry.file_name();
        let filename = filename.to_string_lossy();
        let Some(suffix) = filename.strip_prefix(base.as_ref()) else {
            continue;
        };
        if suffix.is_empty()
            || suffix.starts_with(SNAPSHOT_SEPARATOR)
            || suffix.starts_with(ADD_JOURNAL_SUFFIX)
            || suffix.starts_with(UPDATE_JOURNAL_SUFFIX)
            || suffix.starts_with(MIGRATION_CONFLICT_SEPARATOR)
        {
            members.push((entry.path(), suffix.to_owned()));
        }
    }
    members.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(members)
}

fn family_base_path(path: &Path) -> PathBuf {
    let Some(filename) = path.file_name().and_then(|filename| filename.to_str()) else {
        return path.to_path_buf();
    };
    let without_legacy_conflict = filename
        .rsplit_once(MIGRATION_CONFLICT_SEPARATOR)
        .filter(|(_, suffix)| is_generation(suffix))
        .map_or(filename, |(base, _)| base);
    if let Some(base) = without_legacy_conflict.strip_suffix(ADD_JOURNAL_SUFFIX) {
        return path.with_file_name(base);
    }
    if let Some(base) = without_legacy_conflict.strip_suffix(UPDATE_JOURNAL_SUFFIX) {
        return path.with_file_name(base);
    }
    if let Some((base, suffix)) = without_legacy_conflict.rsplit_once(SNAPSHOT_SEPARATOR) {
        if is_generation(suffix) {
            return path.with_file_name(base);
        }
    }
    path.with_file_name(without_legacy_conflict)
}

fn migration_conflict_directory(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(MIGRATION_CONFLICT_DIRECTORY)
}

fn ensure_migration_conflict_directory(path: &Path) -> Result<()> {
    ensure_private_directory(&migration_conflict_directory(path))
}

fn migration_conflict_candidates(path: &Path) -> Result<Vec<PathBuf>> {
    let directory = migration_conflict_directory(path);
    let metadata = match std::fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "inspect private state migration conflict directory: {error}"
            )))
        }
    };
    if !metadata.file_type().is_dir() {
        return Err(GraphError::Guard(format!(
            "private state migration conflict directory is not a directory: {}",
            directory.display()
        )));
    }

    let mut prefixes = BTreeSet::new();
    prefixes.insert(format!("{}-", migration_conflict_family_key(path)));
    prefixes.insert(format!("{}-", output_key(&family_base_path(path))));
    for candidate in context_state_candidates(path)? {
        prefixes.insert(format!("{}-", output_key(&family_base_path(&candidate))));
    }
    let entries = std::fs::read_dir(&directory).map_err(|error| {
        GraphError::Io(format!("list private state migration conflicts: {error}"))
    })?;
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            GraphError::Io(format!("list private state migration conflicts: {error}"))
        })?;
        let filename = entry.file_name();
        if !prefixes
            .iter()
            .any(|prefix| filename.to_string_lossy().starts_with(prefix))
        {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path()).map_err(|error| {
            GraphError::Io(format!("inspect private state migration conflict: {error}"))
        })?;
        if !metadata.file_type().is_file() {
            return Err(GraphError::Guard(format!(
                "private state migration conflict is not a regular file: {}",
                entry.path().display()
            )));
        }
        candidates.push(entry.path());
    }
    candidates.sort_unstable();
    Ok(candidates)
}

fn has_migration_conflicts(path: &Path) -> Result<bool> {
    Ok(!migration_conflict_candidates(path)?.is_empty())
}

fn store_migration_conflict(path: &Path, bytes: &[u8], context: &str) -> Result<PathBuf> {
    let conflict = migration_conflict_path(path, bytes);
    ensure_migration_conflict_directory(path)?;
    match std::fs::symlink_metadata(&conflict) {
        Ok(metadata) => {
            if !metadata.file_type().is_file()
                || read_regular(&conflict, &metadata, context)? != bytes
            {
                return Err(GraphError::Guard(format!(
                    "private state migration conflict is ambiguous: {}",
                    conflict.display()
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write(&conflict, bytes)?;
        }
        Err(error) => {
            return Err(GraphError::Io(format!(
                "inspect private state migration conflict: {error}"
            )))
        }
    }
    Ok(conflict)
}

fn preserve_migration_conflict(source: &Path, current: &Path, context: &str) -> Result<()> {
    let metadata = std::fs::symlink_metadata(source).map_err(|error| {
        GraphError::Io(format!("inspect {context} {}: {error}", source.display()))
    })?;
    if !metadata.file_type().is_file() {
        return Err(GraphError::Guard(format!(
            "{context} is not a regular file: {}",
            source.display()
        )));
    }
    let bytes = read_regular(source, &metadata, context)?;
    let conflict = store_migration_conflict(current, &bytes, context)?;
    remove(source, context)?;
    Err(GraphError::Guard(format!(
        "conflicting {context} preserved at {}",
        conflict.display()
    )))
}

fn migrate_conflict_family(legacy: &Path, current: &Path, context: &str) -> Result<()> {
    if family_base_path(legacy) == family_base_path(current) {
        return Ok(());
    }
    let mut first_error = None;
    let mut first_conflict = None;
    for source in migration_conflict_candidates(legacy)? {
        let result = (|| {
            let metadata = std::fs::symlink_metadata(&source).map_err(|error| {
                GraphError::Io(format!("inspect {context} {}: {error}", source.display()))
            })?;
            let bytes = read_regular(&source, &metadata, context)?;
            let conflict = store_migration_conflict(current, &bytes, context)?;
            remove(&source, context)?;
            Ok(conflict)
        })();
        match result {
            Ok(conflict) => {
                first_conflict.get_or_insert(conflict);
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    if let Some(conflict) = first_conflict {
        return Err(GraphError::Guard(format!(
            "conflicting {context} preserved at {}",
            conflict.display()
        )));
    }
    Ok(())
}

fn migration_conflict_path(path: &Path, bytes: &[u8]) -> PathBuf {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let family = migration_conflict_family_key(path);
    let member = output_key(path);
    parent
        .join(MIGRATION_CONFLICT_DIRECTORY)
        .join(format!("{family}-{member}-{}", generation(bytes)))
}

fn migration_conflict_family_key(path: &Path) -> String {
    let base = family_base_path(path);
    let Some(filename) = base.file_name().and_then(|filename| filename.to_str()) else {
        return output_key(&base);
    };
    if let Some((key, context)) = filename.split_once(CONTEXT_SEPARATOR) {
        if is_generation(key) && context.strip_suffix(".json").is_some_and(is_generation) {
            return key.to_owned();
        }
    }
    if let Some(key) = filename.strip_suffix(".json") {
        if is_generation(key) {
            return key.to_owned();
        }
    }
    output_key(&base)
}

fn ensure_no_migration_conflicts(path: &Path) -> Result<()> {
    if let Some(conflict) = migration_conflict_candidates(path)?.into_iter().next() {
        return Err(GraphError::Guard(format!(
            "unresolved private state migration conflict: {}",
            conflict.display()
        )));
    }
    Ok(())
}

fn ensure_no_family_conflicts(path: &Path) -> Result<()> {
    for (member, suffix) in family_members(path)? {
        if suffix.contains(MIGRATION_CONFLICT_SEPARATOR) {
            return Err(GraphError::Guard(format!(
                "unresolved private state migration conflict: {}",
                member.display()
            )));
        }
    }
    ensure_no_migration_conflicts(path)
}

fn read_regular(path: &Path, expected: &std::fs::Metadata, context: &str) -> Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| GraphError::Io(format!("read {context}: {error}")))?;
    let opened = file
        .metadata()
        .map_err(|error| GraphError::Io(format!("inspect opened {context}: {error}")))?;
    if !opened.file_type().is_file() || !same_file(expected, &opened) {
        return Err(GraphError::Guard(format!(
            "{context} changed while being migrated: {}",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| GraphError::Io(format!("read {context}: {error}")))?;
    Ok(bytes)
}

#[cfg(unix)]
fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file(_left: &std::fs::Metadata, _right: &std::fs::Metadata) -> bool {
    true
}

fn find_git_dir(start: &Path) -> Result<Option<PathBuf>> {
    for directory in start.ancestors() {
        let marker = directory.join(".git");
        let metadata = match std::fs::symlink_metadata(&marker) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(GraphError::Io(format!(
                    "inspect Git metadata {}: {error}",
                    marker.display()
                )))
            }
        };
        if metadata.file_type().is_dir() {
            let canonical = std::fs::canonicalize(&marker).map_err(|error| {
                GraphError::Io(format!(
                    "resolve Git metadata {}: {error}",
                    marker.display()
                ))
            })?;
            if valid_git_dir(&canonical)? {
                return Ok(Some(canonical));
            }
            continue;
        }
        if !metadata.file_type().is_file() {
            return Err(GraphError::Guard(format!(
                "Git metadata marker is not a regular file or directory: {}",
                marker.display()
            )));
        }
        let marker_text = read_git_control_line(&marker, "Git metadata marker")?
            .ok_or_else(|| GraphError::Guard("Git metadata marker is missing".to_owned()))?;
        let target = marker_text
            .strip_prefix("gitdir:")
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .ok_or_else(|| GraphError::Guard("invalid Git metadata marker".to_owned()))?;
        let target = Path::new(target);
        let resolved = if target.is_absolute() {
            target.to_path_buf()
        } else {
            directory.join(target)
        };
        let canonical = std::fs::canonicalize(&resolved).map_err(|error| {
            GraphError::Io(format!(
                "resolve Git metadata {}: {error}",
                resolved.display()
            ))
        })?;
        let target_metadata = std::fs::symlink_metadata(&canonical)
            .map_err(|error| GraphError::Io(format!("inspect Git directory: {error}")))?;
        if !target_metadata.file_type().is_dir() {
            return Err(GraphError::Guard(format!(
                "Git metadata target is not a directory: {}",
                canonical.display()
            )));
        }
        if !valid_git_dir(&canonical)? {
            return Err(GraphError::Guard(format!(
                "Git metadata target is not a valid Git directory: {}",
                canonical.display()
            )));
        }
        return Ok(Some(canonical));
    }
    Ok(None)
}

fn valid_git_dir(path: &Path) -> Result<bool> {
    let head_metadata = match std::fs::symlink_metadata(path.join("HEAD")) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(GraphError::Io(format!("inspect Git HEAD: {error}"))),
    };
    if !head_metadata.file_type().is_file() {
        return Err(GraphError::Guard(format!(
            "Git HEAD is not a regular file: {}",
            path.join("HEAD").display()
        )));
    }

    let objects = path.join("objects");
    match std::fs::metadata(&objects) {
        Ok(metadata) if metadata.is_dir() => return Ok(true),
        Ok(_) => {
            return Err(GraphError::Guard(format!(
                "Git objects path is not a directory: {}",
                objects.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(GraphError::Io(format!("inspect Git objects: {error}"))),
    }
    let commondir = path.join("commondir");
    match std::fs::metadata(&commondir) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(GraphError::Guard(format!(
            "Git common-directory marker is not a file: {}",
            commondir.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(GraphError::Guard(format!(
                "Git metadata has HEAD but no objects or commondir: {}",
                path.display()
            )))
        }
        Err(error) => Err(GraphError::Io(format!(
            "inspect Git common-directory marker: {error}"
        ))),
    }
}

fn git_context_identity(git_dir: &Path) -> Result<ContextIdentity> {
    let head = git_dir.join("HEAD");
    let identity = read_git_control_line(&head, "Git HEAD")?
        .ok_or_else(|| GraphError::Guard("Git HEAD is missing".to_owned()))?;
    let (context, lineage, revision, unborn_predecessor) = if is_object_id(&identity) {
        let commit = identity.to_ascii_lowercase();
        (
            format!("detached:{commit}"),
            format!("detached:{commit}"),
            format!("commit:{commit}"),
            None,
        )
    } else {
        let reference = identity
            .strip_prefix("ref:")
            .map(str::trim)
            .filter(|reference| valid_git_reference(reference))
            .ok_or_else(|| GraphError::Guard("invalid Git HEAD".to_owned()))?;
        let (context, revision, unborn_predecessor) =
            match resolve_git_reference(git_dir, reference)? {
                Some(commit) => {
                    let predecessor_context =
                        generation(format!("ref:{reference}\nunborn").as_bytes());
                    let predecessor_revision = generation(format!("unborn:{reference}").as_bytes());
                    (
                        format!("ref:{reference}\ncommit:{commit}"),
                        format!("commit:{commit}"),
                        Some((predecessor_context, predecessor_revision)),
                    )
                }
                None => (
                    format!("ref:{reference}\nunborn"),
                    format!("unborn:{reference}"),
                    None,
                ),
            };
        (
            context,
            format!("ref:{reference}"),
            revision,
            unborn_predecessor,
        )
    };
    Ok(ContextIdentity {
        key: generation(context.as_bytes()),
        lineage: generation(lineage.as_bytes()),
        revision: generation(revision.as_bytes()),
        unborn_predecessor,
    })
}

fn resolve_git_reference(git_dir: &Path, initial: &str) -> Result<Option<String>> {
    let common_dir = git_common_dir(git_dir)?;
    let mut reference = initial.to_owned();
    for _ in 0..8 {
        let mut value = read_git_control_line(&git_dir.join(&reference), "Git reference")?;
        if value.is_none() && common_dir != git_dir {
            value = read_git_control_line(&common_dir.join(&reference), "Git reference")?;
        }
        let Some(value) = value else {
            return read_packed_reference(&common_dir, &reference);
        };
        if is_object_id(&value) {
            return Ok(Some(value.to_ascii_lowercase()));
        }
        value
            .strip_prefix("ref:")
            .map(str::trim)
            .filter(|candidate| valid_git_reference(candidate))
            .ok_or_else(|| GraphError::Guard("invalid symbolic Git reference".to_owned()))?
            .clone_into(&mut reference);
    }
    Err(GraphError::Guard(
        "Git symbolic reference chain is too deep".to_owned(),
    ))
}

fn git_common_dir(git_dir: &Path) -> Result<PathBuf> {
    let Some(value) = read_git_control_line(&git_dir.join("commondir"), "Git common directory")?
    else {
        return Ok(git_dir.to_path_buf());
    };
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(GraphError::Guard("invalid Git common directory".to_owned()));
    }
    let value = Path::new(&value);
    let candidate = if value.is_absolute() {
        value.to_path_buf()
    } else {
        git_dir.join(value)
    };
    let canonical = std::fs::canonicalize(&candidate).map_err(|error| {
        GraphError::Io(format!(
            "resolve Git common directory {}: {error}",
            candidate.display()
        ))
    })?;
    let metadata = std::fs::symlink_metadata(&canonical)
        .map_err(|error| GraphError::Io(format!("inspect Git common directory: {error}")))?;
    if !metadata.file_type().is_dir() {
        return Err(GraphError::Guard(format!(
            "Git common directory is not a directory: {}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn read_packed_reference(git_dir: &Path, reference: &str) -> Result<Option<String>> {
    let path = git_dir.join("packed-refs");
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "inspect Git packed references: {error}"
            )))
        }
    };
    if !metadata.file_type().is_file() {
        return Err(GraphError::Guard(format!(
            "Git packed references are not a regular file: {}",
            path.display()
        )));
    }
    let file = std::fs::File::open(&path)
        .map_err(|error| GraphError::Io(format!("read Git packed references: {error}")))?;
    let opened = file
        .metadata()
        .map_err(|error| GraphError::Io(format!("inspect Git packed references: {error}")))?;
    if !same_file(&metadata, &opened) {
        return Err(GraphError::Guard(
            "Git packed references changed while being read".to_owned(),
        ));
    }
    let mut reader = std::io::BufReader::new(file);
    loop {
        let mut line = Vec::new();
        let mut limited = (&mut reader).take(MAX_GIT_CONTROL_LINE_BYTES + 1);
        let read = limited
            .read_until(b'\n', &mut line)
            .map_err(|error| GraphError::Io(format!("read Git packed references: {error}")))?;
        if read == 0 {
            return Ok(None);
        }
        if line.len() as u64 > MAX_GIT_CONTROL_LINE_BYTES {
            return Err(GraphError::Guard(
                "Git packed reference line is too long".to_owned(),
            ));
        }
        let line = std::str::from_utf8(&line)
            .map_err(|error| {
                GraphError::Guard(format!("Git packed references are not UTF-8: {error}"))
            })?
            .trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        let (commit, name) = line
            .split_once(' ')
            .ok_or_else(|| GraphError::Guard("invalid Git packed reference".to_owned()))?;
        if !is_object_id(commit) || !valid_git_reference(name) {
            return Err(GraphError::Guard("invalid Git packed reference".to_owned()));
        }
        if name == reference {
            return Ok(Some(commit.to_ascii_lowercase()));
        }
    }
}

fn read_git_control_line(path: &Path, context: &str) -> Result<Option<String>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(GraphError::Io(format!("inspect {context}: {error}"))),
    };
    if !metadata.file_type().is_file() {
        return Err(GraphError::Guard(format!(
            "{context} is not a regular file: {}",
            path.display()
        )));
    }
    let file = std::fs::File::open(path)
        .map_err(|error| GraphError::Io(format!("read {context}: {error}")))?;
    let opened = file
        .metadata()
        .map_err(|error| GraphError::Io(format!("inspect {context}: {error}")))?;
    if !same_file(&metadata, &opened) {
        return Err(GraphError::Guard(format!(
            "{context} changed while being read"
        )));
    }
    let mut line = Vec::new();
    let mut reader = std::io::BufReader::new(file).take(MAX_GIT_CONTROL_LINE_BYTES + 1);
    reader
        .read_until(b'\n', &mut line)
        .map_err(|error| GraphError::Io(format!("read {context}: {error}")))?;
    if line.is_empty() {
        return Ok(Some(String::new()));
    }
    if line.len() as u64 > MAX_GIT_CONTROL_LINE_BYTES {
        return Err(GraphError::Guard(format!("{context} is too long")));
    }
    let value = std::str::from_utf8(&line)
        .map_err(|error| GraphError::Guard(format!("{context} is not UTF-8: {error}")))?
        .trim();
    Ok(Some(value.to_owned()))
}

fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_git_reference(reference: &str) -> bool {
    reference.starts_with("refs/")
        && !reference.ends_with('/')
        && !reference.ends_with('.')
        && !reference.contains("..")
        && !reference.contains("@{")
        && reference.split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && !component.as_bytes().ends_with(b".lock")
        })
        && !reference.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
}

#[cfg(unix)]
fn output_key(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;

    blake3::hash(path.as_os_str().as_bytes())
        .to_hex()
        .to_string()
}

#[cfg(not(unix))]
fn output_key(path: &Path) -> String {
    blake3::hash(path.to_string_lossy().as_bytes())
        .to_hex()
        .to_string()
}

#[cfg(unix)]
fn harden_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| GraphError::Io(format!("inspect private state directory: {error}")))?;
    if !metadata.file_type().is_dir() {
        return Err(GraphError::Guard(format!(
            "private state directory is not a directory: {}",
            path.display()
        )));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| GraphError::Io(format!("harden private state directory: {error}")))
}

#[cfg(unix)]
fn ensure_private_directory(path: &Path) -> Result<()> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(GraphError::Io(format!(
                "create private state directory {}: {error}",
                path.display()
            )))
        }
    }
    harden_directory(path)
}

#[cfg(not(unix))]
fn ensure_private_directory(_path: &Path) -> Result<()> {
    Err(GraphError::Guard(
        "private graph state requires owner-only directory permissions".to_owned(),
    ))
}

#[cfg(not(unix))]
fn harden_directory(_path: &Path) -> Result<()> {
    Err(GraphError::Guard(
        "private graph state requires owner-only directory permissions".to_owned(),
    ))
}

#[cfg(unix)]
pub(super) fn ensure(path: &Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(GraphError::Io(format!("inspect private state: {error}"))),
    };
    if !metadata.file_type().is_file() {
        return Err(GraphError::Guard(format!(
            "private state is not a regular file: {}",
            path.display()
        )));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| GraphError::Io(format!("harden private state: {error}")))
}

#[cfg(not(unix))]
pub(super) fn ensure(path: &Path) -> Result<()> {
    if let Err(error) = std::fs::remove_file(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(GraphError::Io(format!(
                "remove unsupported private state: {error}"
            )));
        }
    }
    Err(GraphError::Guard(
        "private graph state requires owner-only file permissions".to_owned(),
    ))
}

#[cfg(unix)]
pub(super) fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    super::atomic_file::write(path, bytes, true, "private state")
}

#[cfg(not(unix))]
pub(super) fn write(_path: &Path, _bytes: &[u8]) -> Result<()> {
    Err(GraphError::Guard(
        "private graph state requires owner-only file permissions".to_owned(),
    ))
}

pub(super) fn remove(path: &Path, context: &str) -> Result<()> {
    super::atomic_file::remove(path, context)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};

    use habitat_graph_core::Graph;
    use tempfile::TempDir;

    use super::{ADD_JOURNAL_SUFFIX, SNAPSHOT_SEPARATOR};

    fn sibling(path: &Path, suffix: &str) -> PathBuf {
        let mut name = OsString::from(path.file_name().unwrap());
        name.push(suffix);
        path.with_file_name(name)
    }

    fn create_git(root: &Path, head: &str) -> PathBuf {
        let git_dir = root.join(".git");
        fs::create_dir_all(git_dir.join("objects")).unwrap();
        fs::write(git_dir.join("HEAD"), head).unwrap();
        git_dir
    }

    fn write_ref(git_dir: &Path, reference: &str, commit: &str) {
        let path = git_dir.join(reference);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("{commit}\n")).unwrap();
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn init_real_git(root: &Path) {
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "Habitat Graph Tests"]);
        git(root, &["config", "user.email", "tests@example.invalid"]);
    }

    fn commit_real_git(root: &Path, content: &str) {
        fs::write(root.join("lineage.txt"), content).unwrap();
        git(root, &["add", "lineage.txt"]);
        git(root, &["commit", "-q", "-m", content]);
    }

    #[test]
    fn worktree_git_file_uses_its_private_git_directory() {
        let root = TempDir::new().unwrap();
        let git_dir = root.path().join("metadata/worktree");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(git_dir.join("commondir"), "..\n").unwrap();

        let worktree = root.path().join("checkout");
        let output_dir = worktree.join("public");
        fs::create_dir_all(&output_dir).unwrap();
        fs::write(worktree.join(".git"), "gitdir: ../metadata/worktree\n").unwrap();
        let legacy = output_dir.join(".habitat-graph-state.json");

        let state = super::path_for_output(&output_dir.join("graph.json"), &legacy).unwrap();
        assert!(state.starts_with(git_dir.join("habitat-graph/state")));
        assert_ne!(state, legacy);
    }

    #[test]
    fn oversized_git_metadata_marker_is_rejected() {
        let root = TempDir::new().unwrap();
        fs::write(
            root.path().join(".git"),
            vec![b'x'; usize::try_from(super::MAX_GIT_CONTROL_LINE_BYTES).unwrap() + 1],
        )
        .unwrap();
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let legacy = output_dir.join(".habitat-graph-state.json");

        let error = super::path_for_output(&output_dir.join("graph.json"), &legacy).unwrap_err();
        assert_eq!(error.kind(), "guard");
    }

    #[test]
    fn git_private_path_migrates_legacy_state_immediately() {
        let root = TempDir::new().unwrap();
        let git_dir = root.path().join(".git");
        fs::create_dir_all(git_dir.join("objects")).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let legacy = output_dir.join(".habitat-graph-state.json");
        fs::write(&legacy, "raw private state").unwrap();

        let state = super::path_for_output(&output_dir.join("graph.json"), &legacy).unwrap();

        assert!(!legacy.exists());
        assert_eq!(fs::read_to_string(state).unwrap(), "raw private state");
    }

    #[cfg(unix)]
    #[test]
    fn existing_output_aliases_share_private_state_identity() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        create_git(root.path(), "ref: refs/heads/main\n");
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let alias = output_dir.join("Graph.json");
        fs::write(&output, "{}").unwrap();
        symlink("graph.json", &alias).unwrap();

        let direct =
            super::path_for_output(&output, &output_dir.join(".habitat-graph-state.json")).unwrap();
        let through_alias = super::path_for_output(
            &alias,
            &output_dir.join(".Graph.json.habitat-graph-state.json"),
        )
        .unwrap();
        assert_eq!(direct, through_alias);
    }

    #[cfg(unix)]
    #[test]
    fn git_objects_directory_symlink_is_followed() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let git_dir = root.path().join(".git");
        fs::create_dir_all(git_dir.join("object-store")).unwrap();
        symlink("object-store", git_dir.join("objects")).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let legacy = output_dir.join(".habitat-graph-state.json");

        let state = super::path_for_output(&output_dir.join("graph.json"), &legacy).unwrap();
        assert!(state.starts_with(git_dir.join("habitat-graph/state")));
    }

    #[cfg(unix)]
    #[test]
    fn broken_git_objects_symlink_fails_closed() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let git_dir = root.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        symlink("missing-objects", git_dir.join("objects")).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let legacy = output_dir.join(".habitat-graph-state.json");

        let error = super::path_for_output(&output_dir.join("graph.json"), &legacy).unwrap_err();
        assert_eq!(error.kind(), "guard");
        assert!(error.to_string().contains("no objects or commondir"));
    }

    #[test]
    fn output_transaction_lock_is_cross_handle_exclusive() {
        let root = TempDir::new().unwrap();
        let state = root.path().join("state.json");
        let first = super::acquire_output_lock(&state).unwrap();
        let error = super::acquire_output_lock(&state).unwrap_err();
        assert_eq!(error.kind(), "guard");
        drop(first);
        super::acquire_output_lock(&state).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn output_transaction_lock_rejects_symlinks_without_changing_the_target() {
        use std::os::unix::fs::{symlink, PermissionsExt as _};

        let root = TempDir::new().unwrap();
        let state = root.path().join("state.json");
        let lock = root.path().join("state.json.output-lock");
        let target = root.path().join("target");
        fs::write(&target, "unchanged").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        symlink(&target, &lock).unwrap();

        let error = super::acquire_output_lock(&state).unwrap_err();
        assert_eq!(error.kind(), "guard");
        assert_eq!(fs::read_to_string(&target).unwrap(), "unchanged");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn private_state_records_its_public_generation() {
        let graph = Graph::new();
        let generation = super::generation(b"public graph");
        let semantic_generation = super::semantic_generation(&graph).unwrap();
        let bytes = super::serialize(&graph, &generation, &semantic_generation).unwrap();
        let stored = super::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();

        assert_eq!(stored.graph, graph);
        assert_eq!(
            stored.public_generation.as_deref(),
            Some(generation.as_str())
        );
        assert_eq!(
            stored.public_semantic_generation.as_deref(),
            Some(semantic_generation.as_str())
        );
    }

    #[test]
    fn legacy_private_state_metadata_remains_readable() {
        let graph = Graph::new();
        let generation = super::generation(b"legacy public graph");
        let mut value = serde_json::to_value(&graph).unwrap();
        value.as_object_mut().unwrap().insert(
            super::PRIVATE_STATE_METADATA.to_owned(),
            serde_json::json!({
                "schema": super::LEGACY_PRIVATE_STATE_SCHEMA,
                "public_generation": generation,
            }),
        );

        let stored = super::parse(&serde_json::to_string(&value).unwrap()).unwrap();
        assert_eq!(stored.graph, graph);
        assert!(stored.public_generation.is_some());
        assert!(stored.public_semantic_generation.is_none());
    }

    #[test]
    fn overwritten_state_remains_addressable_by_public_generation() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("state.json");
        let mut first = Graph::new();
        first.manifest.tool_version = "first-private-lineage".to_owned();
        let mut second = Graph::new();
        second.manifest.tool_version = "second-private-lineage".to_owned();
        let first_generation = super::generation(b"first public graph");
        let second_generation = super::generation(b"second public graph");
        let first_semantic = super::semantic_generation(&Graph::new()).unwrap();
        let mut second_public = Graph::new();
        second_public.schema = "second-public-schema".to_owned();
        let second_semantic = super::semantic_generation(&second_public).unwrap();

        let first_bytes = super::serialize(&first, &first_generation, &first_semantic).unwrap();
        super::write_state(&path, &first_bytes).unwrap();
        let second_bytes = super::serialize(&second, &second_generation, &second_semantic).unwrap();
        super::write_state(&path, &second_bytes).unwrap();

        let restored = super::load_matching(
            &path,
            Some(&first_generation),
            Some(&first_semantic),
            "test private state",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            restored.graph.manifest.tool_version,
            "first-private-lineage"
        );
        let current = super::read(&path, "test private state").unwrap().unwrap();
        assert_eq!(
            current.graph.manifest.tool_version,
            "second-private-lineage"
        );
    }

    #[test]
    fn exact_snapshot_precedes_semantically_matching_current_state() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("state.json");
        let mut first = Graph::new();
        first.manifest.tool_version = "first-private-lineage".to_owned();
        let mut second = Graph::new();
        second.manifest.tool_version = "second-private-lineage".to_owned();
        let first_generation = super::generation(b"first formatting");
        let second_generation = super::generation(b"second formatting");
        let semantic = super::semantic_generation(&Graph::new()).unwrap();

        let first_bytes = super::serialize(&first, &first_generation, &semantic).unwrap();
        super::write_state(&path, &first_bytes).unwrap();
        let second_bytes = super::serialize(&second, &second_generation, &semantic).unwrap();
        super::write_state(&path, &second_bytes).unwrap();

        let restored = super::load_matching(
            &path,
            Some(&first_generation),
            Some(&semantic),
            "test private state",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            restored.graph.manifest.tool_version,
            "first-private-lineage"
        );
    }

    #[test]
    fn semantic_fallback_rejects_current_snapshot_ambiguity() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("state.json");
        let mut first = Graph::new();
        first.manifest.tool_version = "first-private-lineage".to_owned();
        let mut second = Graph::new();
        second.manifest.tool_version = "second-private-lineage".to_owned();
        let first_generation = super::generation(b"first formatting");
        let second_generation = super::generation(b"second formatting");
        let unmatched_generation = super::generation(b"unmatched formatting");
        let semantic = super::semantic_generation(&Graph::new()).unwrap();

        super::write_state(
            &path,
            &super::serialize(&first, &first_generation, &semantic).unwrap(),
        )
        .unwrap();
        super::write_state(
            &path,
            &super::serialize(&second, &second_generation, &semantic).unwrap(),
        )
        .unwrap();

        let error = super::load_matching(
            &path,
            Some(&unmatched_generation),
            Some(&semantic),
            "test private state",
        )
        .unwrap_err();
        assert_eq!(error.kind(), "guard");
    }

    #[test]
    fn unverified_context_cannot_inherit_semantic_state() {
        let root = TempDir::new().unwrap();
        let output = super::generation(b"output path");
        let first_context = super::generation(b"first context");
        let second_context = super::generation(b"second context");
        let first_path = root.path().join(format!(
            "{output}{}{first_context}.json",
            super::CONTEXT_SEPARATOR
        ));
        let second_path = root.path().join(format!(
            "{output}{}{second_context}.json",
            super::CONTEXT_SEPARATOR
        ));
        let mut private = Graph::new();
        private.manifest.tool_version = "private lineage".to_owned();
        let semantic = super::semantic_generation(&Graph::new()).unwrap();
        let old_generation = super::generation(b"old formatting");
        let new_generation = super::generation(b"new formatting");
        super::write_state(
            &first_path,
            &super::serialize(&private, &old_generation, &semantic).unwrap(),
        )
        .unwrap();

        let error = super::load_matching(
            &second_path,
            Some(&new_generation),
            Some(&semantic),
            "test private state",
        )
        .unwrap_err();
        assert_eq!(error.kind(), "guard");
    }

    #[test]
    fn cross_context_semantic_ambiguity_fails_closed() {
        let root = TempDir::new().unwrap();
        let output = super::generation(b"output path");
        let path = |context: &str| {
            root.path().join(format!(
                "{output}{}{context}.json",
                super::CONTEXT_SEPARATOR
            ))
        };
        let mut alpha = Graph::new();
        alpha.manifest.tool_version = "alpha lineage".to_owned();
        let mut beta = Graph::new();
        beta.manifest.tool_version = "beta lineage".to_owned();
        let semantic = super::semantic_generation(&Graph::new()).unwrap();
        super::write_state(
            &path(&super::generation(b"alpha context")),
            &super::serialize(&alpha, &super::generation(b"alpha bytes"), &semantic).unwrap(),
        )
        .unwrap();
        super::write_state(
            &path(&super::generation(b"beta context")),
            &super::serialize(&beta, &super::generation(b"beta bytes"), &semantic).unwrap(),
        )
        .unwrap();

        let error = super::load_matching(
            &path(&super::generation(b"current context")),
            Some(&super::generation(b"current bytes")),
            Some(&semantic),
            "test private state",
        )
        .unwrap_err();
        assert_eq!(error.kind(), "guard");
    }

    #[test]
    fn git_branches_keep_byte_identical_public_variants_separate() {
        let root = TempDir::new().unwrap();
        let git_dir = create_git(root.path(), "ref: refs/heads/alpha\n");
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let legacy = output_dir.join(".habitat-graph-state.json");
        let generation = super::generation(b"shared public graph");
        let semantic = super::semantic_generation(&Graph::new()).unwrap();
        let mut alpha = Graph::new();
        alpha.manifest.tool_version = "alpha-private-lineage".to_owned();
        let mut beta = Graph::new();
        beta.manifest.tool_version = "beta-private-lineage".to_owned();

        let alpha_path = super::path_for_output(&output, &legacy).unwrap();
        super::write_state(
            &alpha_path,
            &super::serialize(&alpha, &generation, &semantic).unwrap(),
        )
        .unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/beta\n").unwrap();
        let beta_path = super::path_for_output(&output, &legacy).unwrap();
        assert_ne!(alpha_path, beta_path);
        super::write_state(
            &beta_path,
            &super::serialize(&beta, &generation, &semantic).unwrap(),
        )
        .unwrap();

        fs::write(git_dir.join("HEAD"), "ref: refs/heads/alpha\n").unwrap();
        let restored_path = super::path_for_output(&output, &legacy).unwrap();
        let restored = super::read(&restored_path, "test private state")
            .unwrap()
            .unwrap();
        assert_eq!(
            restored.graph.manifest.tool_version,
            "alpha-private-lineage"
        );
    }

    #[test]
    fn git_context_tracks_the_resolved_commit() {
        let root = TempDir::new().unwrap();
        let git_dir = create_git(root.path(), "ref: refs/heads/main\n");
        let first_commit = "1111111111111111111111111111111111111111";
        let second_commit = "2222222222222222222222222222222222222222";
        write_ref(&git_dir, "refs/heads/main", first_commit);
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let legacy = output_dir.join(".habitat-graph-state.json");

        let first = super::path_for_output(&output, &legacy).unwrap();
        write_ref(&git_dir, "refs/heads/main", second_commit);
        let second = super::path_for_output(&output, &legacy).unwrap();
        assert_ne!(first, second);

        write_ref(&git_dir, "refs/heads/renamed", second_commit);
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/renamed\n").unwrap();
        let renamed = super::path_for_output(&output, &legacy).unwrap();
        assert_ne!(second, renamed);
    }

    #[test]
    fn new_commit_inherits_only_unique_exact_generation_state() {
        let root = TempDir::new().unwrap();
        init_real_git(root.path());
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let legacy = output_dir.join(".habitat-graph-state.json");
        let public_generation = super::generation(b"shared public bytes");
        let semantic = super::semantic_generation(&Graph::new()).unwrap();
        let mut alpha = Graph::new();
        alpha.manifest.tool_version = "alpha-private-lineage".to_owned();
        let mut beta = Graph::new();
        beta.manifest.tool_version = "beta-private-lineage".to_owned();

        commit_real_git(root.path(), "first");
        let alpha_path = super::path_for_output(&output, &legacy).unwrap();
        super::write_state(
            &alpha_path,
            &super::serialize(&alpha, &public_generation, &semantic).unwrap(),
        )
        .unwrap();

        commit_real_git(root.path(), "second");
        let inherited_path = super::path_for_output(&output, &legacy).unwrap();
        let inherited = super::load_matching(
            &inherited_path,
            Some(&public_generation),
            Some(&semantic),
            "test private state",
        )
        .unwrap()
        .unwrap();
        assert_eq!(inherited.graph, alpha);

        commit_real_git(root.path(), "third");
        let beta_path = super::path_for_output(&output, &legacy).unwrap();
        super::write_state(
            &beta_path,
            &super::serialize(&beta, &public_generation, &semantic).unwrap(),
        )
        .unwrap();

        commit_real_git(root.path(), "fourth");
        let ambiguous_path = super::path_for_output(&output, &legacy).unwrap();
        let error = super::load_matching(
            &ambiguous_path,
            Some(&public_generation),
            Some(&semantic),
            "test private state",
        )
        .unwrap_err();
        assert_eq!(error.kind(), "guard");
    }

    #[test]
    fn first_commit_inherits_same_ref_unborn_state() {
        let root = TempDir::new().unwrap();
        init_real_git(root.path());
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let legacy = output_dir.join(".habitat-graph-state.json");
        let public_generation = super::generation(b"unborn public bytes");
        let semantic = super::semantic_generation(&Graph::new()).unwrap();
        let mut private = Graph::new();
        private.manifest.tool_version = "unborn-private-lineage".to_owned();

        let unborn_path = super::path_for_output(&output, &legacy).unwrap();
        super::write_state(
            &unborn_path,
            &super::serialize(&private, &public_generation, &semantic).unwrap(),
        )
        .unwrap();

        commit_real_git(root.path(), "first");
        let committed_path = super::path_for_output(&output, &legacy).unwrap();
        assert_ne!(unborn_path, committed_path);
        let inherited = super::load_matching(
            &committed_path,
            Some(&public_generation),
            Some(&semantic),
            "test private state",
        )
        .unwrap()
        .unwrap();
        assert_eq!(inherited.graph, private);
    }

    #[test]
    fn unrelated_history_cannot_inherit_matching_private_state() {
        let root = TempDir::new().unwrap();
        init_real_git(root.path());
        commit_real_git(root.path(), "ancestor");
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let legacy = output_dir.join(".habitat-graph-state.json");
        let generation = super::generation(b"shared public bytes");
        let semantic = super::semantic_generation(&Graph::new()).unwrap();
        let mut private = Graph::new();
        private.manifest.tool_version = "ancestor-private-lineage".to_owned();
        let ancestor_path = super::path_for_output(&output, &legacy).unwrap();
        super::write_state(
            &ancestor_path,
            &super::serialize(&private, &generation, &semantic).unwrap(),
        )
        .unwrap();

        git(root.path(), &["checkout", "-q", "--orphan", "unrelated"]);
        git(root.path(), &["rm", "-q", "-f", "lineage.txt"]);
        commit_real_git(root.path(), "unrelated");
        let unrelated_path = super::path_for_output(&output, &legacy).unwrap();
        let inherited = super::load_matching(
            &unrelated_path,
            Some(&generation),
            Some(&semantic),
            "test private state",
        )
        .unwrap();
        assert!(inherited.is_none());
    }

    #[test]
    fn git_private_path_migrates_the_complete_legacy_family() {
        let root = TempDir::new().unwrap();
        create_git(root.path(), "ref: refs/heads/main\n");
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let legacy = output_dir.join(".habitat-graph-state.json");
        let generation = super::generation(b"legacy snapshot");
        let legacy_snapshot = sibling(&legacy, &format!("{SNAPSHOT_SEPARATOR}{generation}"));
        let legacy_journal = sibling(&legacy, ADD_JOURNAL_SUFFIX);
        fs::write(&legacy, "legacy state").unwrap();
        fs::write(&legacy_snapshot, "legacy snapshot").unwrap();
        fs::write(&legacy_journal, "legacy journal").unwrap();

        let state = super::path_for_output(&output_dir.join("graph.json"), &legacy).unwrap();
        let snapshot = sibling(&state, &format!("{SNAPSHOT_SEPARATOR}{generation}"));
        let journal = sibling(&state, ADD_JOURNAL_SUFFIX);

        assert_eq!(fs::read_to_string(state).unwrap(), "legacy state");
        assert_eq!(fs::read_to_string(snapshot).unwrap(), "legacy snapshot");
        assert_eq!(fs::read_to_string(journal).unwrap(), "legacy journal");
        assert!(!legacy.exists());
        assert!(!legacy_snapshot.exists());
        assert!(!legacy_journal.exists());
    }

    #[test]
    fn migration_conflicts_are_preserved_and_remain_blocking() {
        let root = TempDir::new().unwrap();
        let legacy = root.path().join("legacy-state.json");
        let current = root.path().join("current-state.json");
        fs::write(&legacy, "new private lineage").unwrap();
        fs::write(&current, "old private lineage").unwrap();

        let error = super::migrate(&legacy, &current, "test private state").unwrap_err();
        assert_eq!(error.kind(), "guard");
        assert!(!legacy.exists());
        let conflict = super::migration_conflict_path(&current, b"new private lineage");
        assert_eq!(
            fs::read_to_string(&conflict).unwrap(),
            "new private lineage"
        );
        let retry = super::migrate(&legacy, &current, "test private state").unwrap_err();
        assert_eq!(retry.kind(), "guard");
    }

    #[test]
    fn migration_conflict_blocks_every_git_context_for_the_output() {
        let root = TempDir::new().unwrap();
        let git_dir = create_git(root.path(), "ref: refs/heads/alpha\n");
        write_ref(
            &git_dir,
            "refs/heads/alpha",
            "1111111111111111111111111111111111111111",
        );
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let legacy = output_dir.join(".habitat-graph-state.json");
        let alpha_state = super::path_for_output(&output, &legacy).unwrap();
        fs::write(&alpha_state, "alpha lineage").unwrap();
        fs::write(&legacy, "conflicting lineage").unwrap();

        assert!(super::path_for_output(&output, &legacy).is_err());
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/beta\n").unwrap();
        write_ref(
            &git_dir,
            "refs/heads/beta",
            "2222222222222222222222222222222222222222",
        );

        let error = super::path_for_output(&output, &legacy).unwrap_err();
        assert_eq!(error.kind(), "guard");
        assert!(!super::migration_conflict_candidates(&alpha_state)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn long_snapshot_migration_conflict_uses_a_bounded_filename() {
        let root = TempDir::new().unwrap();
        let base = root.path().join(format!(
            "{}{}{}.json",
            super::generation(b"output"),
            super::CONTEXT_SEPARATOR,
            super::generation(b"context")
        ));
        let current = sibling(
            &base,
            &format!("{SNAPSHOT_SEPARATOR}{}", super::generation(b"snapshot")),
        );
        let legacy = root.path().join("legacy-snapshot.json");
        fs::write(&legacy, "new private lineage").unwrap();
        fs::write(&current, "old private lineage").unwrap();

        let error = super::migrate(&legacy, &current, "test private state").unwrap_err();
        assert_eq!(error.kind(), "guard");
        let conflict = super::migration_conflict_path(&current, b"new private lineage");
        assert!(conflict.file_name().unwrap().as_encoded_bytes().len() <= 255);
        assert_eq!(fs::read_to_string(conflict).unwrap(), "new private lineage");
    }

    #[test]
    fn family_migration_secures_every_member_before_reporting_conflict() {
        let root = TempDir::new().unwrap();
        create_git(root.path(), "ref: refs/heads/main\n");
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let legacy = output_dir.join(".habitat-graph-state.json");
        let state = super::path_for_output(&output, &legacy).unwrap();
        fs::write(&state, "current state").unwrap();
        let snapshot_suffix = format!(
            "{SNAPSHOT_SEPARATOR}{}",
            super::generation(b"legacy snapshot")
        );
        let legacy_snapshot = sibling(&legacy, &snapshot_suffix);
        let legacy_journal = sibling(&legacy, ADD_JOURNAL_SUFFIX);
        fs::write(&legacy, "conflicting state").unwrap();
        fs::write(&legacy_snapshot, "legacy snapshot").unwrap();
        fs::write(&legacy_journal, "legacy journal").unwrap();

        let error = super::path_for_output(&output, &legacy).unwrap_err();
        assert_eq!(error.kind(), "guard");
        assert!(!legacy.exists());
        assert!(!legacy_snapshot.exists());
        assert!(!legacy_journal.exists());
        assert_eq!(
            fs::read_to_string(sibling(&state, &snapshot_suffix)).unwrap(),
            "legacy snapshot"
        );
        assert_eq!(
            fs::read_to_string(sibling(&state, ADD_JOURNAL_SUFFIX)).unwrap(),
            "legacy journal"
        );
        assert!(super::path_for_output(&output, &legacy).is_err());
    }

    #[test]
    fn private_state_snapshot_retention_is_bounded() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("state.json");
        let semantic = super::semantic_generation(&Graph::new()).unwrap();

        for index in 0..(super::MAX_SNAPSHOTS + 4) {
            let mut graph = Graph::new();
            graph.manifest.tool_version = format!("private-lineage-{index}");
            let generation = super::generation(format!("public-{index}").as_bytes());
            let bytes = super::serialize(&graph, &generation, &semantic).unwrap();
            super::write_state(&path, &bytes).unwrap();
        }

        assert_eq!(
            super::snapshot_candidates(&path).unwrap().len(),
            super::MAX_SNAPSHOTS
        );
    }

    #[test]
    fn private_state_context_retention_is_bounded_per_output() {
        let root = TempDir::new().unwrap();
        let output_key = super::generation(b"output path");
        let semantic = super::semantic_generation(&Graph::new()).unwrap();
        let mut current = None;

        for index in 0..(super::MAX_CONTEXTS_PER_OUTPUT + 4) {
            let context = super::generation(format!("commit-{index}").as_bytes());
            let path = root.path().join(format!(
                "{output_key}{}{}.json",
                super::CONTEXT_SEPARATOR,
                context
            ));
            super::write_context_revision(&path, &context).unwrap();
            let mut graph = Graph::new();
            graph.manifest.tool_version = format!("private-lineage-{index}");
            let public = super::generation(format!("public-{index}").as_bytes());
            super::write_state(
                &path,
                &super::serialize(&graph, &public, &semantic).unwrap(),
            )
            .unwrap();
            current = Some(path);
        }

        let current = current.unwrap();
        assert!(current.exists());
        assert_eq!(
            super::context_state_candidates(&current).unwrap().len(),
            super::MAX_CONTEXTS_PER_OUTPUT
        );
    }

    #[test]
    fn pruned_git_context_remains_explicitly_expired() {
        let root = TempDir::new().unwrap();
        let git_dir = create_git(root.path(), "ref: refs/heads/main\n");
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let legacy = output_dir.join(".habitat-graph-state.json");
        let semantic = super::semantic_generation(&Graph::new()).unwrap();
        let mut contexts = Vec::new();

        for index in 0..=super::MAX_CONTEXTS_PER_OUTPUT {
            let commit = format!("{index:040x}");
            write_ref(&git_dir, "refs/heads/main", &commit);
            let path = super::path_for_output(&output, &legacy).unwrap();
            let mut graph = Graph::new();
            graph.manifest.tool_version = format!("private-lineage-{index}");
            let public = super::generation(format!("public-{index}").as_bytes());
            super::write_state(
                &path,
                &super::serialize(&graph, &public, &semantic).unwrap(),
            )
            .unwrap();
            contexts.push((commit, path));
        }

        let (expired_commit, expired_path) = contexts
            .into_iter()
            .find(|(_, path)| !path.exists())
            .expect("one old context must be pruned");
        assert!(super::expired_context_path(&expired_path).unwrap().exists());
        write_ref(&git_dir, "refs/heads/main", &expired_commit);

        let error = super::path_for_output(&output, &legacy).unwrap_err();
        assert_eq!(error.kind(), "guard");
        assert!(error.to_string().contains("history context has expired"));

        fs::write(git_dir.join("HEAD"), "ref: refs/heads/recovered\n").unwrap();
        write_ref(&git_dir, "refs/heads/recovered", &expired_commit);
        let renamed_error = super::path_for_output(&output, &legacy).unwrap_err();
        assert_eq!(renamed_error.kind(), "guard");
        assert!(renamed_error
            .to_string()
            .contains("Git revision has expired"));
    }

    #[cfg(not(unix))]
    #[test]
    fn unsupported_platform_removes_legacy_state_before_path_resolution() {
        let root = TempDir::new().unwrap();
        let output_dir = root.path().join("missing");
        let legacy = root.path().join(".habitat-graph-state.json");
        let snapshot = sibling(
            &legacy,
            &format!(
                "{SNAPSHOT_SEPARATOR}{}",
                super::generation(b"legacy snapshot")
            ),
        );
        let journal = sibling(&legacy, ADD_JOURNAL_SUFFIX);
        fs::write(&legacy, "raw state").unwrap();
        fs::write(&snapshot, "raw snapshot").unwrap();
        fs::write(&journal, "raw journal").unwrap();

        assert!(super::path_for_output(&output_dir.join("graph.json"), &legacy).is_err());
        assert!(!legacy.exists());
        assert!(!snapshot.exists());
        assert!(!journal.exists());
    }
}
