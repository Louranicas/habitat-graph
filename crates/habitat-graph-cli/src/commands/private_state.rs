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

use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::io::{BufRead as _, Read as _};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::io::{Seek as _, Write as _};

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
const EXPIRATION_INDEX_SUFFIX: &str = ".expirations";
const LEGACY_EXPIRATION_INDEX_SCHEMA: &str = "habitat-graph.expirations.v1";
const PREFIX_EXPIRATION_INDEX_SCHEMA: &str = "habitat-graph.expirations.v2";
const EXPIRATION_INDEX_SCHEMA: &str = "habitat-graph.expirations.v3";
const EXPIRATION_LOG_HEADER: &str = "habitat-graph.expirations.v3\n";
const EXPIRATION_RECORD_BYTES: usize = 67;
const EXPIRATION_FRAME_HEADER_BYTES: usize = 75;
const MIGRATION_CONFLICT_SEPARATOR: &str = ".migration-conflict-";
const MIGRATION_CONFLICT_DIRECTORY: &str = ".migration-conflicts";
const ADD_JOURNAL_SUFFIX: &str = ".add-journal";
const UPDATE_JOURNAL_SUFFIX: &str = ".update-journal";
const FULL_BUILD_JOURNAL_SUFFIX: &str = ".full-build-journal";
const FULL_BUILD_JOURNAL_SCHEMA: &str = "habitat-graph.full-build-journal.v1";
const OUTPUT_LOCK_SUFFIX: &str = ".output-lock";
const OUTPUT_IDENTITY_LOCK: &str = ".habitat-graph.output-identity-lock";
const MAX_SNAPSHOTS: usize = 16;
const MAX_CONTEXTS_PER_OUTPUT: usize = 16;
const MAX_EXPIRATION_LOG_OVERHEAD: usize = 256;
const MAX_LEGACY_REVISION_SCAN: usize = 4096;
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
    commit: Option<String>,
    unborn_predecessor: Option<(String, String)>,
}

#[cfg(unix)]
#[derive(Debug)]
pub(super) struct FullBuildState {
    path: PathBuf,
    output: PathBuf,
    context: Option<ContextIdentity>,
}

#[cfg(unix)]
impl FullBuildState {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    fn ensure_current(&self) -> Result<()> {
        if context_identity_for_output(&self.output)? != self.context {
            return Err(GraphError::Guard(
                "Git context changed during full build".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(super) fn path_for_output(output: &Path, legacy: &Path) -> Result<PathBuf> {
    path_for_output_with_expiration(output, legacy, false).map(|resolved| resolved.0)
}

#[cfg(test)]
pub(super) fn path_for_full_build(output: &Path, legacy: &Path) -> Result<PathBuf> {
    path_for_output_with_expiration(output, legacy, true).map(|resolved| resolved.0)
}

#[cfg(unix)]
pub(super) fn prepare_full_build_state(output: &Path, legacy: &Path) -> Result<FullBuildState> {
    let output = resolve_output_file(output)?;
    let (path, context) = path_for_output_with_expiration(&output, legacy, true)?;
    Ok(FullBuildState {
        path,
        output,
        context,
    })
}

fn path_for_output_with_expiration(
    output: &Path,
    legacy: &Path,
    allow_expired: bool,
) -> Result<(PathBuf, Option<ContextIdentity>)> {
    #[cfg(not(unix))]
    remove_unsupported_state(output, legacy)?;

    let canonical_output = resolve_output_file(output)?;
    let canonical_parent = canonical_output
        .parent()
        .ok_or_else(|| GraphError::Io("output path has no parent".to_owned()))?;
    let Some(git_dir) = find_git_dir(canonical_parent)? else {
        return Ok((legacy.to_path_buf(), None));
    };

    let state_root = git_dir.join("habitat-graph");
    let state_dir = state_root.join("state");
    ensure_private_directory(&state_root)?;
    ensure_private_directory(&state_dir)?;

    let key = output_key(&canonical_output);
    let unscoped_state_path = state_dir.join(format!("{key}.json"));
    let context = git_context_identity(&git_dir)?;
    let state_path = state_dir.join(format!("{key}{CONTEXT_SEPARATOR}{}.json", context.key));
    if !allow_expired {
        let expirations = load_expiration_index(&state_path)?;
        ensure_context_not_expired_in(&state_path, &expirations)?;
        ensure_revision_not_expired_in(&state_path, &context.revision, &expirations)?;
    }
    write_context_revision(&state_path, &context.revision, context.commit.as_deref())?;
    let mut first_error = None;
    for result in [
        migrate_full_build_journal(
            &unscoped_state_path,
            &state_path,
            "unscoped full-build journal",
        ),
        migrate_family(&unscoped_state_path, &state_path, "unscoped private state"),
        migrate_full_build_journal(legacy, &state_path, "legacy full-build journal"),
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
    Ok((state_path, Some(context)))
}

pub(super) fn resolve_output_directory(path: &Path) -> Result<PathBuf> {
    let resolved = std::fs::canonicalize(path).map_err(|error| {
        GraphError::Io(format!(
            "resolve output directory {}: {error}",
            path.display()
        ))
    })?;
    let metadata = std::fs::symlink_metadata(&resolved)
        .map_err(|error| GraphError::Io(format!("inspect output directory: {error}")))?;
    if !metadata.file_type().is_dir() {
        return Err(GraphError::Guard(format!(
            "output path is not a directory: {}",
            resolved.display()
        )));
    }
    Ok(resolved)
}

pub(super) fn resolve_output_file(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = resolve_output_directory(parent)?;
    let filename = path
        .file_name()
        .ok_or_else(|| GraphError::Io("output path has no filename".to_owned()))?;
    let resolved = parent.join(filename);
    match std::fs::symlink_metadata(&resolved) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(GraphError::Guard(format!(
            "output path must not be a symbolic link: {}",
            resolved.display()
        ))),
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                return Err(GraphError::Guard(format!(
                    "output path is not a regular file: {}",
                    resolved.display()
                )));
            }
            let entries = std::fs::read_dir(&parent)
                .map_err(|error| GraphError::Io(format!("list output directory: {error}")))?;
            let mut matches = Vec::new();
            for entry in entries {
                let entry = entry
                    .map_err(|error| GraphError::Io(format!("list output directory: {error}")))?;
                let candidate_metadata =
                    std::fs::symlink_metadata(entry.path()).map_err(|error| {
                        GraphError::Io(format!("inspect output directory entry: {error}"))
                    })?;
                if candidate_metadata.file_type().is_file()
                    && same_file(&metadata, &candidate_metadata)
                {
                    if entry.file_name() == filename {
                        return Ok(entry.path());
                    }
                    matches.push(entry.path());
                }
            }
            if matches.len() != 1 {
                return Err(GraphError::Guard(format!(
                    "output path has an ambiguous filesystem identity: {}",
                    resolved.display()
                )));
            }
            Ok(matches.remove(0))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(resolved),
        Err(error) => Err(GraphError::Io(format!(
            "inspect output path {}: {error}",
            resolved.display()
        ))),
    }
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

pub(super) fn ensure_state_context_current(
    output: &Path,
    state_path: &Path,
    operation: &str,
) -> Result<()> {
    let current = context_identity_for_output(output)?;
    if current.as_ref().map(|identity| identity.key.as_str())
        != state_context_key(state_path).as_deref()
    {
        return Err(GraphError::Guard(format!(
            "Git context changed during {operation}"
        )));
    }
    Ok(())
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
    acquire_lock_path(&lock_path, None)
}

pub(super) fn acquire_output_identity_lock(output: &Path) -> Result<OutputTransactionLock> {
    if output
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case(OUTPUT_IDENTITY_LOCK))
    {
        return Err(GraphError::Guard(format!(
            "output filename is reserved for transaction locking: {OUTPUT_IDENTITY_LOCK}"
        )));
    }
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = resolve_output_directory(parent)?;
    let filename = output
        .file_name()
        .ok_or_else(|| GraphError::Io("output path has no filename".to_owned()))?;
    acquire_lock_path(
        &parent.join(OUTPUT_IDENTITY_LOCK),
        Some(&parent.join(filename)),
    )
}

fn acquire_lock_path(lock_path: &Path, output: Option<&Path>) -> Result<OutputTransactionLock> {
    if let Some(parent) = lock_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| GraphError::Io(format!("create output lock directory: {error}")))?;
    }
    for _ in 0..4 {
        let prior_metadata = match std::fs::symlink_metadata(lock_path) {
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
        let file = match options.open(lock_path) {
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
        let current_metadata = std::fs::symlink_metadata(lock_path).map_err(|error| {
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
        ensure_lock_does_not_alias_output(&opened_metadata, output)?;
        #[cfg(unix)]
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| GraphError::Io(format!("harden output transaction lock: {error}")))?;
        return Ok(OutputTransactionLock { file });
    }
    Err(GraphError::Guard(
        "output transaction lock changed while being opened".to_owned(),
    ))
}

fn ensure_lock_does_not_alias_output(
    lock_metadata: &std::fs::Metadata,
    output: Option<&Path>,
) -> Result<()> {
    let Some(output) = output else {
        return Ok(());
    };
    match std::fs::symlink_metadata(output) {
        Ok(metadata) if metadata.file_type().is_file() && same_file(lock_metadata, &metadata) => {
            Err(GraphError::Guard(format!(
                "output path aliases its transaction lock: {}",
                output.display()
            )))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GraphError::Io(format!(
            "inspect output path for transaction locking: {error}"
        ))),
    }
}

fn output_lock_path(state_path: &Path) -> Result<PathBuf> {
    let filename = state_path
        .file_name()
        .ok_or_else(|| GraphError::Io("private state path has no filename".to_owned()))?;
    let mut base = if state_context_key(state_path).is_some() {
        let filename = filename.to_str().ok_or_else(|| {
            GraphError::Io("context-scoped private state filename is not UTF-8".to_owned())
        })?;
        OsString::from(
            filename
                .split_once(CONTEXT_SEPARATOR)
                .map_or(filename, |(key, _)| key),
        )
    } else {
        OsString::from(filename)
    };
    base.push(OUTPUT_LOCK_SUFFIX);
    Ok(state_path.with_file_name(base))
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
    read_text(path, context)?
        .map(|text| parse(&text))
        .transpose()
}

pub(super) fn read_text(path: &Path, context: &str) -> Result<Option<String>> {
    let Some(mut file) = open_private_file(path, context)? else {
        return Ok(None);
    };
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|error| GraphError::Io(format!("read {context}: {error}")))?;
    Ok(Some(text))
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
    statuses: HashMap<PathBuf, bool>,
}

impl ContextAncestry {
    fn status(&self, path: &Path) -> Option<bool> {
        self.statuses.get(path).copied()
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
    if current_identity.revision != expected_head.key
        || state_context_key(path).as_deref() != Some(current_identity.key.as_str())
    {
        return Ok(ContextAncestry::default());
    }
    let mut revisions = Vec::new();
    for candidate in candidates {
        if candidate != path {
            if let Some(revision) = context_revision(candidate)? {
                revisions.push((candidate.clone(), revision));
            }
        }
    }
    if let Some(current_commit) = current_identity.commit.as_deref() {
        migrate_legacy_revision_markers(&git_dir, current_commit, &mut revisions)?;
    }
    let mut ancestry = ContextAncestry::default();
    for (candidate, revision) in revisions {
        if current_identity.unborn_predecessor.as_ref().is_some_and(
            |(context, predecessor_revision)| {
                state_context_key(&candidate).as_deref() == Some(context.as_str())
                    && revision.key == *predecessor_revision
            },
        ) {
            ancestry.statuses.insert(candidate, true);
            continue;
        }
        let (Some(candidate_commit), Some(current_commit)) = (
            revision.commit.as_deref(),
            current_identity.commit.as_deref(),
        ) else {
            continue;
        };
        let Ok(status) = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(&git_dir)
            .args([
                "merge-base",
                "--is-ancestor",
                candidate_commit,
                current_commit,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        else {
            continue;
        };
        match status.code() {
            Some(0) => {
                ancestry.statuses.insert(candidate, true);
            }
            Some(1) => {
                ancestry.statuses.insert(candidate, false);
            }
            _ => {}
        }
    }
    if git_context_identity(&git_dir)? != current_identity {
        return Ok(ContextAncestry::default());
    }
    Ok(ancestry)
}

fn migrate_legacy_revision_markers(
    git_dir: &Path,
    current_commit: &str,
    revisions: &mut [(PathBuf, ContextRevision)],
) -> Result<()> {
    let mut unresolved: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, (_, revision)) in revisions.iter().enumerate() {
        if revision.commit.is_none() {
            unresolved
                .entry(revision.key.clone())
                .or_default()
                .push(index);
        }
    }
    if unresolved.is_empty() {
        return Ok(());
    }
    let Ok(output) = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .arg("rev-list")
        .arg(format!("--max-count={MAX_LEGACY_REVISION_SCAN}"))
        .arg(current_commit)
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Ok(());
    };
    if !output.status.success() {
        return Ok(());
    }
    let commits = std::str::from_utf8(&output.stdout).map_err(|error| {
        GraphError::Guard(format!("Git revision history is not UTF-8: {error}"))
    })?;
    for commit in commits.lines() {
        if !is_object_id(commit) {
            return Err(GraphError::Guard(
                "Git revision history contains an invalid object ID".to_owned(),
            ));
        }
        let commit = commit.to_ascii_lowercase();
        let revision_key = generation(format!("commit:{commit}").as_bytes());
        let Some(indices) = unresolved.remove(&revision_key) else {
            continue;
        };
        for index in indices {
            let (path, revision) = &mut revisions[index];
            write_context_revision(path, &revision.key, Some(&commit))?;
            revision.commit = Some(commit.clone());
        }
        if unresolved.is_empty() {
            break;
        }
    }
    Ok(())
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
    let expirations = load_expiration_index(path)?;
    ensure_context_not_expired_in(path, &expirations)?;
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
            if candidate == path || context_is_expired_in(candidate, &expirations)? {
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
        if candidate != path && context_is_expired_in(&candidate, &expirations)? {
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
    write_state_contents(path, bytes)
}

#[cfg(all(test, unix))]
pub(super) fn commit_full_build_state(state: &FullBuildState, bytes: &[u8]) -> Result<()> {
    commit_full_build_state_and_publish(state, bytes, || Ok(()))
}

#[cfg(unix)]
pub(super) fn commit_full_build_state_and_publish(
    state: &FullBuildState,
    bytes: &[u8],
    publish: impl FnOnce() -> Result<()>,
) -> Result<()> {
    commit_full_build_state_with(
        state,
        bytes,
        || write_full_build_state(state.path(), bytes),
        publish,
    )
}

#[cfg(unix)]
fn commit_full_build_state_with(
    state: &FullBuildState,
    bytes: &[u8],
    write_state: impl FnOnce() -> Result<()>,
    publish: impl FnOnce() -> Result<()>,
) -> Result<()> {
    state.ensure_current()?;
    let journal_path = full_build_journal_path(state.path())?;
    let context_journal_path = transaction_path(state.path(), FULL_BUILD_JOURNAL_SUFFIX)?;
    let journal = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": FULL_BUILD_JOURNAL_SCHEMA,
        "origin_context": state.context.as_ref().map(|identity| identity.key.as_str()),
        "private_checksum": generation(bytes),
    }))
    .map_err(|error| GraphError::Schema(format!("full-build journal serialize: {error}")))?;
    write(&context_journal_path, &journal)?;
    state.ensure_current()?;
    write(&journal_path, &journal)?;
    state.ensure_current()?;
    write_state()?;
    state.ensure_current()?;
    publish()?;
    state.ensure_current()?;
    if context_journal_path != journal_path {
        finalize_context_journal(
            &state.output,
            state.path(),
            &context_journal_path,
            "full-build context transaction",
        )?;
    }
    finalize_context_journal(
        &state.output,
        state.path(),
        &journal_path,
        "full-build transaction",
    )
}

fn full_build_journal_path(path: &Path) -> Result<PathBuf> {
    if let Some(key) = output_state_key(path) {
        return Ok(path.with_file_name(format!("{key}{FULL_BUILD_JOURNAL_SUFFIX}")));
    }
    transaction_path(path, FULL_BUILD_JOURNAL_SUFFIX)
}

pub(super) fn ensure_no_pending_full_build_journal(path: &Path) -> Result<()> {
    let context_journal_path = transaction_path(path, FULL_BUILD_JOURNAL_SUFFIX)?;
    validate_pending_full_build_journal(path, &context_journal_path)?;
    let journal_path = full_build_journal_path(path)?;
    if journal_path != context_journal_path {
        validate_pending_full_build_journal(path, &journal_path)?;
    }
    Ok(())
}

fn validate_pending_full_build_journal(path: &Path, journal_path: &Path) -> Result<()> {
    let Some(text) = read_text(journal_path, "full-build journal")? else {
        return Ok(());
    };
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| GraphError::Schema(format!("full-build journal parse: {error}")))?;
    let origin = match value.get("origin_context") {
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(origin)) if is_generation(origin) => Some(origin.as_str()),
        _ => {
            return Err(GraphError::Schema(
                "full-build journal origin is invalid".to_owned(),
            ))
        }
    };
    if value["schema"] != FULL_BUILD_JOURNAL_SCHEMA
        || !value["private_checksum"]
            .as_str()
            .is_some_and(is_generation)
    {
        return Err(GraphError::Schema(
            "full-build journal is invalid".to_owned(),
        ));
    }
    let current_context = state_context_key(path);
    if origin != current_context.as_deref() {
        return Err(GraphError::Guard(format!(
            "pending full-build transaction belongs to another Git context: {}",
            journal_path.display()
        )));
    }
    Err(GraphError::Guard(format!(
        "pending full-build transaction requires another full build: {}",
        journal_path.display()
    )))
}

pub(super) fn finalize_context_journal(
    output: &Path,
    state_path: &Path,
    journal_path: &Path,
    operation: &str,
) -> Result<()> {
    finalize_context_journal_with(output, state_path, journal_path, operation, || {
        remove(journal_path, operation)
    })
}

fn finalize_context_journal_with(
    output: &Path,
    state_path: &Path,
    journal_path: &Path,
    operation: &str,
    remove_journal: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let journal = read_text(journal_path, operation)?
        .ok_or_else(|| GraphError::Io(format!("{operation} journal disappeared")))?;
    ensure_state_context_current(output, state_path, operation)?;
    remove_journal()?;
    if let Err(error) = ensure_state_context_current(output, state_path, operation) {
        write(journal_path, journal.as_bytes())?;
        return Err(error);
    }
    Ok(())
}

fn write_full_build_state(path: &Path, bytes: &[u8]) -> Result<()> {
    let revision = context_revision(path)?;
    if expired_context_path(path).is_some() && revision.is_none() {
        return Err(GraphError::Guard(format!(
            "private state context lacks revision provenance: {}",
            path.display()
        )));
    }
    write_state_contents(path, bytes)?;
    if let Some(revision) = revision {
        remove_expiration_markers(path, &revision.key)?;
    }
    Ok(())
}

fn write_state_contents(path: &Path, bytes: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| GraphError::Schema(format!("private state serialize: {error}")))?;
    let replacement = parse(text)?;
    let existing = match read_text(path, "private state") {
        Ok(Some(text)) => match parse(&text) {
            Ok(existing) => Some((existing, text)),
            Err(error) if error.kind() == "schema" => None,
            Err(error) => return Err(error),
        },
        Ok(None) => None,
        Err(error) if error.kind() == "schema" => None,
        Err(error) => return Err(error),
    };
    if let Some((existing, existing_text)) = existing {
        if existing != replacement && existing.public_generation != replacement.public_generation {
            if let Some(existing_generation) = existing.public_generation.as_deref() {
                let snapshot = snapshot_path(path, existing_generation)?;
                match read(&snapshot, "private state snapshot")? {
                    None => write(&snapshot, existing_text.as_bytes())?,
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
    if let Some(replacement_generation) = replacement.public_generation.as_deref() {
        let snapshot = snapshot_path(path, replacement_generation)?;
        if let Some(snapshot_state) = read(&snapshot, "private state snapshot")? {
            if snapshot_state.public_generation.as_deref() != Some(replacement_generation) {
                return Err(GraphError::Guard(format!(
                    "private state snapshot generation is invalid: {}",
                    snapshot.display()
                )));
            }
            remove(&snapshot, "private state snapshot")?;
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
    let mut prefix = OsString::from(filename);
    prefix.push(SNAPSHOT_SEPARATOR);
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
        if native_filename_suffix(&filename, &prefix).is_some_and(|suffix| is_generation(&suffix)) {
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
            if member_suffix == suffix {
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

fn output_state_key(path: &Path) -> Option<&str> {
    let filename = path.file_name()?.to_str()?;
    let (key, context) = filename.split_once(CONTEXT_SEPARATOR)?;
    (is_generation(key) && context.strip_suffix(".json").is_some_and(is_generation)).then_some(key)
}

fn expiration_index_path(path: &Path) -> Option<PathBuf> {
    Some(path.with_file_name(format!(
        "{}{EXPIRATION_INDEX_SUFFIX}",
        output_state_key(path)?
    )))
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

fn context_revision_prefix(path: &Path) -> Option<String> {
    let filename = path.file_name()?.to_str()?;
    let (key, context) = filename.split_once(CONTEXT_SEPARATOR)?;
    let context = context.strip_suffix(".json")?;
    if !is_generation(key) || !is_generation(context) {
        return None;
    }
    Some(format!("{key}{CONTEXT_REVISION_SEPARATOR}{context}-"))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContextRevision {
    key: String,
    commit: Option<String>,
}

fn revision_marker_commit(path: &Path, revision: &str) -> Result<Option<String>> {
    let text = read_text(path, "private state revision marker")?
        .ok_or_else(|| GraphError::Io("private state revision marker disappeared".to_owned()))?;
    if text.is_empty() {
        return Ok(None);
    }
    if !is_object_id(&text) {
        return Err(GraphError::Guard(format!(
            "private state revision marker is invalid: {}",
            path.display()
        )));
    }
    let commit = text.to_ascii_lowercase();
    if generation(format!("commit:{commit}").as_bytes()) != revision {
        return Err(GraphError::Guard(format!(
            "private state revision marker does not match its filename: {}",
            path.display()
        )));
    }
    Ok(Some(commit))
}

fn context_revision(path: &Path) -> Result<Option<ContextRevision>> {
    let Some(prefix) = context_revision_prefix(path) else {
        return Ok(None);
    };
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let entries = std::fs::read_dir(parent)
        .map_err(|error| GraphError::Io(format!("list private state revisions: {error}")))?;
    let mut stored_revision = None;
    for entry in entries {
        let entry = entry
            .map_err(|error| GraphError::Io(format!("list private state revisions: {error}")))?;
        let filename = entry.file_name();
        let Some(revision_key) = filename
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
        else {
            continue;
        };
        if !is_generation(revision_key) {
            continue;
        }
        let candidate = ContextRevision {
            key: revision_key.to_owned(),
            commit: revision_marker_commit(&entry.path(), revision_key)?,
        };
        if stored_revision
            .as_ref()
            .is_some_and(|stored| stored != &candidate)
        {
            return Err(GraphError::Guard(format!(
                "private state context has conflicting revision provenance: {}",
                path.display()
            )));
        }
        stored_revision = Some(candidate);
    }
    Ok(stored_revision)
}

fn context_revision_path(path: &Path, revision: &str) -> Option<PathBuf> {
    if !is_generation(revision) {
        return None;
    }
    Some(path.with_file_name(format!("{}{revision}", context_revision_prefix(path)?)))
}

fn write_context_revision(path: &Path, revision: &str, commit: Option<&str>) -> Result<()> {
    let commit = commit.map(str::to_ascii_lowercase);
    if commit.as_deref().is_some_and(|commit| {
        !is_object_id(commit) || generation(format!("commit:{commit}").as_bytes()) != revision
    }) {
        return Err(GraphError::Guard(
            "private state context commit is invalid".to_owned(),
        ));
    }
    let existing = context_revision(path)?;
    if existing.as_ref().is_some_and(|stored| {
        stored.key != revision
            || stored
                .commit
                .as_ref()
                .is_some_and(|stored| Some(stored) != commit.as_ref())
    }) {
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
    if existing.is_some()
        && (commit.is_none() || existing.and_then(|stored| stored.commit).is_some())
    {
        return Ok(());
    }
    write(&marker, commit.as_deref().unwrap_or_default().as_bytes())
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

#[cfg(test)]
fn ensure_revision_not_expired(path: &Path, revision: &str) -> Result<()> {
    let expirations = load_expiration_index(path)?;
    ensure_revision_not_expired_in(path, revision, &expirations)
}

fn ensure_revision_not_expired_in(
    path: &Path,
    revision: &str,
    expirations: &ExpirationIndex,
) -> Result<()> {
    if expirations.revisions.contains(revision) {
        return Err(GraphError::Guard(format!(
            "private state for this Git revision has expired: {}",
            path.display()
        )));
    }
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

fn remove_expiration_markers(path: &Path, revision: &str) -> Result<()> {
    let context = state_context_key(path).ok_or_else(|| {
        GraphError::Guard(format!(
            "cannot admit unscoped private state context: {}",
            path.display()
        ))
    })?;
    update_expiration_index(path, |index| {
        index.contexts.remove(&context);
        index.revisions.remove(revision);
    })
}

fn remove_context_revision(path: &Path) -> Result<()> {
    let Some(revision) = context_revision(path)? else {
        return Ok(());
    };
    let marker = context_revision_path(path, &revision.key).ok_or_else(|| {
        GraphError::Guard(format!(
            "cannot remove unscoped private state revision: {}",
            path.display()
        ))
    })?;
    remove(&marker, "expired private state revision")
}

fn ensure_context_not_expired(path: &Path) -> Result<()> {
    let expirations = load_expiration_index(path)?;
    ensure_context_not_expired_in(path, &expirations)
}

fn ensure_context_not_expired_in(path: &Path, expirations: &ExpirationIndex) -> Result<()> {
    if !context_is_expired_in(path, expirations)? {
        return Ok(());
    }
    Err(GraphError::Guard(format!(
        "private state for this Git history context has expired: {}",
        path.display()
    )))
}

#[cfg(test)]
fn context_is_expired(path: &Path) -> Result<bool> {
    let expirations = load_expiration_index(path)?;
    context_is_expired_in(path, &expirations)
}

fn context_is_expired_in(path: &Path, expirations: &ExpirationIndex) -> Result<bool> {
    let Some(context) = state_context_key(path) else {
        return Ok(false);
    };
    if expirations.contexts.contains(&context) {
        return Ok(true);
    }
    let Some(expired) = expired_context_path(path) else {
        return Ok(false);
    };
    marker_exists(&expired, "expired private state context marker")
}

#[derive(Clone, Default, Eq, PartialEq)]
struct ExpirationSet {
    exact: BTreeSet<String>,
    prefixes: BTreeSet<String>,
    admitted: BTreeSet<String>,
}

impl ExpirationSet {
    fn from_exact(exact: BTreeSet<String>) -> Self {
        Self {
            exact,
            ..Self::default()
        }
    }

    fn from_prefixes(prefixes: BTreeSet<String>, mut admitted: BTreeSet<String>) -> Self {
        admitted.retain(|value| prefixes.iter().any(|prefix| value.starts_with(prefix)));
        Self {
            exact: BTreeSet::new(),
            prefixes,
            admitted,
        }
    }

    fn contains(&self, value: &str) -> bool {
        self.exact.contains(value)
            || (self.prefixes.iter().any(|prefix| value.starts_with(prefix))
                && !self.admitted.contains(value))
    }

    fn insert(&mut self, value: String) {
        self.admitted.remove(&value);
        self.exact.insert(value);
    }

    fn remove(&mut self, value: &str) {
        self.exact.remove(value);
        self.prefixes.remove(value);
        if self.prefixes.iter().any(|prefix| value.starts_with(prefix)) {
            self.admitted.insert(value.to_owned());
        } else {
            self.admitted.remove(value);
        }
    }

    fn extend(&mut self, values: impl IntoIterator<Item = String>) {
        for value in values {
            self.insert(value);
        }
    }

    fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.prefixes.is_empty() && self.admitted.is_empty()
    }

    fn prefix_values(&self) -> BTreeSet<String> {
        self.prefixes.union(&self.exact).cloned().collect()
    }
}

#[derive(Clone, Default, Eq, PartialEq)]
struct ExpirationIndex {
    contexts: ExpirationSet,
    revisions: ExpirationSet,
}

enum ExpirationStorage {
    Missing,
    Legacy,
    PrefixLegacy,
    Log {
        records: usize,
        complete_bytes: usize,
        partial_tail: bool,
    },
}

struct LoadedExpirationIndex {
    index: ExpirationIndex,
    storage: ExpirationStorage,
}

fn expiration_values(value: &serde_json::Value, field: &str) -> Result<BTreeSet<String>> {
    let values = value[field].as_array().ok_or_else(|| {
        GraphError::Schema(format!(
            "private state expiration index `{field}` is invalid"
        ))
    })?;
    let mut generations = BTreeSet::new();
    for value in values {
        let generation = value
            .as_str()
            .filter(|value| is_generation(value))
            .ok_or_else(|| {
                GraphError::Schema(format!(
                    "private state expiration index `{field}` is invalid"
                ))
            })?;
        generations.insert(generation.to_owned());
    }
    Ok(generations)
}

fn expiration_prefixes(value: &serde_json::Value, field: &str) -> Result<BTreeSet<String>> {
    let values = value[field].as_array().ok_or_else(|| {
        GraphError::Schema(format!(
            "private state expiration index `{field}` is invalid"
        ))
    })?;
    let mut prefixes = BTreeSet::new();
    for value in values {
        let prefix = value.as_str().filter(|prefix| {
            prefix.len() <= 64
                && prefix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        let Some(prefix) = prefix else {
            return Err(GraphError::Schema(format!(
                "private state expiration index `{field}` is invalid"
            )));
        };
        prefixes.insert(prefix.to_owned());
    }
    Ok(prefixes)
}

fn apply_expiration_record(index: &mut ExpirationIndex, record: &[u8]) -> Result<()> {
    if record.len() != EXPIRATION_RECORD_BYTES || record[EXPIRATION_RECORD_BYTES - 1] != b'\n' {
        return Err(GraphError::Schema(
            "private state expiration log record is invalid".to_owned(),
        ));
    }
    let value = std::str::from_utf8(&record[2..66])
        .ok()
        .filter(|value| is_generation(value))
        .ok_or_else(|| {
            GraphError::Schema("private state expiration log record is invalid".to_owned())
        })?;
    let values = match record[1] {
        b'c' => &mut index.contexts,
        b'r' => &mut index.revisions,
        _ => {
            return Err(GraphError::Schema(
                "private state expiration log record is invalid".to_owned(),
            ))
        }
    };
    match record[0] {
        b'+' => {
            values.insert(value.to_owned());
        }
        b'-' => {
            values.remove(value);
        }
        _ => {
            return Err(GraphError::Schema(
                "private state expiration log record is invalid".to_owned(),
            ))
        }
    }
    Ok(())
}

fn parse_expiration_log(text: &str) -> Result<(ExpirationIndex, usize, usize, bool)> {
    let body = text.strip_prefix(EXPIRATION_LOG_HEADER).ok_or_else(|| {
        GraphError::Schema("private state expiration log header is invalid".to_owned())
    })?;
    let mut index = ExpirationIndex::default();
    let mut records = 0;
    let mut offset = 0;
    while offset < body.len() {
        let remaining = &body.as_bytes()[offset..];
        if remaining.len() < EXPIRATION_FRAME_HEADER_BYTES {
            return Ok((index, records, offset, true));
        }
        let header = &remaining[..EXPIRATION_FRAME_HEADER_BYTES];
        if header[0] != b'@' || header[9] != b':' || header[74] != b'\n' {
            return Err(GraphError::Schema(
                "private state expiration log frame is invalid".to_owned(),
            ));
        }
        let count = std::str::from_utf8(&header[1..9])
            .ok()
            .and_then(|value| u32::from_str_radix(value, 16).ok())
            .filter(|count| *count != 0)
            .ok_or_else(|| {
                GraphError::Schema("private state expiration log frame is invalid".to_owned())
            })?;
        let checksum = std::str::from_utf8(&header[10..74])
            .ok()
            .filter(|value| is_generation(value))
            .ok_or_else(|| {
                GraphError::Schema("private state expiration log frame is invalid".to_owned())
            })?;
        let count = usize::try_from(count).map_err(|_| {
            GraphError::Schema("private state expiration log frame is invalid".to_owned())
        })?;
        let payload_length = count.checked_mul(EXPIRATION_RECORD_BYTES).ok_or_else(|| {
            GraphError::Schema("private state expiration log frame is invalid".to_owned())
        })?;
        let frame_length = EXPIRATION_FRAME_HEADER_BYTES
            .checked_add(payload_length)
            .ok_or_else(|| {
                GraphError::Schema("private state expiration log frame is invalid".to_owned())
            })?;
        if remaining.len() < frame_length {
            return Ok((index, records, offset, true));
        }
        let payload = &remaining[EXPIRATION_FRAME_HEADER_BYTES..frame_length];
        if generation(payload) != checksum {
            return Err(GraphError::Schema(
                "private state expiration log frame checksum is invalid".to_owned(),
            ));
        }
        for record in payload.chunks_exact(EXPIRATION_RECORD_BYTES) {
            apply_expiration_record(&mut index, record)?;
        }
        records = records.saturating_add(count);
        offset = offset.saturating_add(frame_length);
    }
    Ok((index, records, offset, false))
}

fn load_legacy_expiration_index(text: &str) -> Result<(ExpirationIndex, ExpirationStorage)> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| GraphError::Schema(format!("private state expiration index: {error}")))?;
    match value["schema"].as_str() {
        Some(LEGACY_EXPIRATION_INDEX_SCHEMA) => Ok((
            ExpirationIndex {
                contexts: ExpirationSet::from_exact(expiration_values(&value, "contexts")?),
                revisions: ExpirationSet::from_exact(expiration_values(&value, "revisions")?),
            },
            ExpirationStorage::Legacy,
        )),
        Some(PREFIX_EXPIRATION_INDEX_SCHEMA) => Ok((
            ExpirationIndex {
                contexts: ExpirationSet::from_prefixes(
                    expiration_prefixes(&value, "context_prefixes")?,
                    expiration_values(&value, "admitted_contexts")?,
                ),
                revisions: ExpirationSet::from_prefixes(
                    expiration_prefixes(&value, "revision_prefixes")?,
                    expiration_values(&value, "admitted_revisions")?,
                ),
            },
            ExpirationStorage::PrefixLegacy,
        )),
        _ => Err(GraphError::Schema(
            "unsupported private state expiration index schema".to_owned(),
        )),
    }
}

fn load_expiration_storage(path: &Path) -> Result<LoadedExpirationIndex> {
    let Some(index_path) = expiration_index_path(path) else {
        return Ok(LoadedExpirationIndex {
            index: ExpirationIndex::default(),
            storage: ExpirationStorage::Missing,
        });
    };
    let Some(text) = read_text(&index_path, "private state expiration index")? else {
        return Ok(LoadedExpirationIndex {
            index: ExpirationIndex::default(),
            storage: ExpirationStorage::Missing,
        });
    };
    if text.starts_with(EXPIRATION_INDEX_SCHEMA) {
        let (index, records, complete_bytes, partial_tail) = parse_expiration_log(&text)?;
        Ok(LoadedExpirationIndex {
            index,
            storage: ExpirationStorage::Log {
                records,
                complete_bytes,
                partial_tail,
            },
        })
    } else {
        let (index, storage) = load_legacy_expiration_index(&text)?;
        Ok(LoadedExpirationIndex { index, storage })
    }
}

fn load_expiration_index(path: &Path) -> Result<ExpirationIndex> {
    load_expiration_storage(path).map(|loaded| loaded.index)
}

fn legacy_expiration_markers(path: &Path) -> Result<(ExpirationIndex, Vec<PathBuf>)> {
    let Some(key) = output_state_key(path) else {
        return Ok((ExpirationIndex::default(), Vec::new()));
    };
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((ExpirationIndex::default(), Vec::new()))
        }
        Err(error) => {
            return Err(GraphError::Io(format!(
                "list private state expiration markers: {error}"
            )))
        }
    };
    let context_prefix = format!("{key}{EXPIRED_CONTEXT_SEPARATOR}");
    let revision_prefix = format!("{key}{EXPIRED_REVISION_SEPARATOR}");
    let mut index = ExpirationIndex::default();
    let mut markers = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            GraphError::Io(format!("list private state expiration markers: {error}"))
        })?;
        let filename = entry.file_name();
        let Some(filename) = filename.to_str() else {
            continue;
        };
        let value = if let Some(context) = filename.strip_prefix(&context_prefix) {
            if !is_generation(context) {
                continue;
            }
            index.contexts.insert(context.to_owned());
            context
        } else if let Some(revision) = filename.strip_prefix(&revision_prefix) {
            if !is_generation(revision) {
                continue;
            }
            index.revisions.insert(revision.to_owned());
            revision
        } else {
            continue;
        };
        debug_assert!(is_generation(value));
        marker_exists(&entry.path(), "private state expiration marker")?;
        markers.push(entry.path());
    }
    Ok((index, markers))
}

fn push_expiration_record(records: &mut Vec<u8>, operation: u8, kind: u8, value: &str) {
    debug_assert!(is_generation(value));
    records.push(operation);
    records.push(kind);
    records.extend_from_slice(value.as_bytes());
    records.push(b'\n');
}

fn expiration_changes(before: &ExpirationIndex, after: &ExpirationIndex) -> Vec<u8> {
    let mut records = Vec::new();
    for value in before.contexts.exact.difference(&after.contexts.exact) {
        push_expiration_record(&mut records, b'-', b'c', value);
    }
    for value in before.revisions.exact.difference(&after.revisions.exact) {
        push_expiration_record(&mut records, b'-', b'r', value);
    }
    for value in after.contexts.exact.difference(&before.contexts.exact) {
        push_expiration_record(&mut records, b'+', b'c', value);
    }
    for value in after.revisions.exact.difference(&before.revisions.exact) {
        push_expiration_record(&mut records, b'+', b'r', value);
    }
    records
}

fn expiration_frame(records: &[u8]) -> Result<Vec<u8>> {
    if records.is_empty() || !records.len().is_multiple_of(EXPIRATION_RECORD_BYTES) {
        return Err(GraphError::Schema(
            "private state expiration log frame is invalid".to_owned(),
        ));
    }
    let count = u32::try_from(records.len() / EXPIRATION_RECORD_BYTES)
        .map_err(|_| GraphError::Io("private state expiration log is too large".to_owned()))?;
    let header = format!("@{count:08x}:{}\n", generation(records));
    debug_assert_eq!(header.len(), EXPIRATION_FRAME_HEADER_BYTES);
    let mut frame = Vec::with_capacity(header.len().saturating_add(records.len()));
    frame.extend_from_slice(header.as_bytes());
    frame.extend_from_slice(records);
    Ok(frame)
}

fn expiration_log_bytes(index: &ExpirationIndex) -> Result<Vec<u8>> {
    if !index.contexts.prefixes.is_empty()
        || !index.contexts.admitted.is_empty()
        || !index.revisions.prefixes.is_empty()
        || !index.revisions.admitted.is_empty()
    {
        return Err(GraphError::Schema(
            "private state expiration prefixes require legacy storage".to_owned(),
        ));
    }
    let mut records = Vec::with_capacity(
        EXPIRATION_RECORD_BYTES * (index.contexts.exact.len() + index.revisions.exact.len()),
    );
    for value in &index.contexts.exact {
        push_expiration_record(&mut records, b'+', b'c', value);
    }
    for value in &index.revisions.exact {
        push_expiration_record(&mut records, b'+', b'r', value);
    }
    let frame = expiration_frame(&records)?;
    let mut bytes = Vec::with_capacity(EXPIRATION_LOG_HEADER.len().saturating_add(frame.len()));
    bytes.extend_from_slice(EXPIRATION_LOG_HEADER.as_bytes());
    bytes.extend_from_slice(&frame);
    Ok(bytes)
}

fn write_expiration_index(path: &Path, index: &ExpirationIndex) -> Result<()> {
    let index_path = expiration_index_path(path).ok_or_else(|| {
        GraphError::Guard(format!(
            "cannot update unscoped private state expiration index: {}",
            path.display()
        ))
    })?;
    if index.contexts.is_empty() && index.revisions.is_empty() {
        return remove(&index_path, "private state expiration index");
    }
    write(&index_path, &expiration_log_bytes(index)?)
}

fn write_prefix_expiration_index(path: &Path, index: &ExpirationIndex) -> Result<()> {
    let index_path = expiration_index_path(path).ok_or_else(|| {
        GraphError::Guard(format!(
            "cannot update unscoped private state expiration index: {}",
            path.display()
        ))
    })?;
    if index.contexts.is_empty() && index.revisions.is_empty() {
        return remove(&index_path, "private state expiration index");
    }
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": PREFIX_EXPIRATION_INDEX_SCHEMA,
        "context_prefixes": index.contexts.prefix_values(),
        "revision_prefixes": index.revisions.prefix_values(),
        "admitted_contexts": index.contexts.admitted,
        "admitted_revisions": index.revisions.admitted,
    }))
    .map_err(|error| GraphError::Schema(format!("private state expiration index: {error}")))?;
    write(&index_path, &bytes)
}

#[cfg(unix)]
fn append_expiration_records(path: &Path, records: &[u8], complete_bytes: usize) -> Result<()> {
    if records.is_empty() {
        return Ok(());
    }
    let frame = expiration_frame(records)?;
    let expected = std::fs::symlink_metadata(path).map_err(|error| {
        GraphError::Io(format!("inspect private state expiration log: {error}"))
    })?;
    if !expected.file_type().is_file() {
        return Err(GraphError::Guard(format!(
            "private state expiration log is not a regular file: {}",
            path.display()
        )));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).append(true);
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .map_err(|error| GraphError::Io(format!("open private state expiration log: {error}")))?;
    let opened = file.metadata().map_err(|error| {
        GraphError::Io(format!(
            "inspect opened private state expiration log: {error}"
        ))
    })?;
    if !opened.file_type().is_file() || !same_file(&expected, &opened) {
        return Err(GraphError::Guard(format!(
            "private state expiration log changed while being opened: {}",
            path.display()
        )));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| GraphError::Io(format!("harden private state expiration log: {error}")))?;
    let valid_length = EXPIRATION_LOG_HEADER.len().saturating_add(complete_bytes);
    if opened.len()
        != u64::try_from(valid_length).map_err(|_| {
            GraphError::Io("private state expiration log length overflow".to_owned())
        })?
    {
        return Err(GraphError::Guard(format!(
            "private state expiration log changed before append: {}",
            path.display()
        )));
    }
    file.seek(std::io::SeekFrom::End(0))
        .map_err(|error| GraphError::Io(format!("seek private state expiration log: {error}")))?;
    file.write_all(&frame)
        .map_err(|error| GraphError::Io(format!("append private state expiration log: {error}")))?;
    file.sync_all()
        .map_err(|error| GraphError::Io(format!("sync private state expiration log: {error}")))?;
    let current = std::fs::symlink_metadata(path).map_err(|error| {
        GraphError::Guard(format!(
            "private state expiration log changed while being appended: {error}"
        ))
    })?;
    if !current.file_type().is_file() || !same_file(&opened, &current) {
        return Err(GraphError::Guard(format!(
            "private state expiration log changed while being appended: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn append_expiration_records(_path: &Path, _records: &[u8], _complete_bytes: usize) -> Result<()> {
    Err(GraphError::Guard(
        "private graph state requires owner-only file permissions".to_owned(),
    ))
}

fn update_expiration_index(path: &Path, update: impl FnOnce(&mut ExpirationIndex)) -> Result<()> {
    let loaded = load_expiration_storage(path)?;
    let original = loaded.index.clone();
    let mut index = loaded.index;
    let (legacy, markers) = legacy_expiration_markers(path)?;
    index.contexts.extend(legacy.contexts.exact);
    index.revisions.extend(legacy.revisions.exact);
    update(&mut index);
    let index_path = expiration_index_path(path).ok_or_else(|| {
        GraphError::Guard(format!(
            "cannot update unscoped private state expiration index: {}",
            path.display()
        ))
    })?;
    match loaded.storage {
        ExpirationStorage::Missing | ExpirationStorage::Legacy => {
            write_expiration_index(path, &index)?;
        }
        ExpirationStorage::PrefixLegacy => {
            write_prefix_expiration_index(path, &index)?;
        }
        ExpirationStorage::Log {
            records,
            complete_bytes,
            partial_tail,
        } => {
            let changes = expiration_changes(&original, &index);
            let live_records = index
                .contexts
                .exact
                .len()
                .saturating_add(index.revisions.exact.len());
            if index.contexts.is_empty() && index.revisions.is_empty()
                || partial_tail
                || records
                    > live_records
                        .saturating_mul(2)
                        .saturating_add(MAX_EXPIRATION_LOG_OVERHEAD)
            {
                write_expiration_index(path, &index)?;
            } else {
                append_expiration_records(&index_path, &changes, complete_bytes)?;
            }
        }
    }
    for marker in markers {
        remove(&marker, "legacy private state expiration marker")?;
    }
    Ok(())
}

fn expire_context(path: &Path) -> Result<()> {
    let context = state_context_key(path).ok_or_else(|| {
        GraphError::Guard(format!(
            "cannot expire unscoped private state context: {}",
            path.display()
        ))
    })?;
    let revision = context_revision(path)?.ok_or_else(|| {
        GraphError::Guard(format!(
            "private state context lacks revision provenance: {}",
            path.display()
        ))
    })?;
    update_expiration_index(path, |index| {
        index.contexts.insert(context);
        index.revisions.insert(revision.key);
    })
}

fn context_member_key<'a>(filename: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = filename.strip_prefix(prefix)?;
    let context = rest.get(..64)?;
    if !is_generation(context) {
        return None;
    }
    let suffix = rest.get(64..)?.strip_prefix(".json")?;
    is_family_member_suffix(suffix).then_some(context)
}

fn prune_contexts(path: &Path) -> Result<()> {
    if expiration_index_path(path).is_some() {
        update_expiration_index(path, |_| {})?;
    }
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
                suffix == ADD_JOURNAL_SUFFIX
                    || suffix == UPDATE_JOURNAL_SUFFIX
                    || suffix == FULL_BUILD_JOURNAL_SUFFIX
                    || is_migration_conflict_suffix(suffix)
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
        expire_context(&candidate)?;
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
        let result = if is_migration_conflict_suffix(&suffix) {
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

fn migrate_full_build_journal(legacy: &Path, current: &Path, context: &str) -> Result<()> {
    migrate(
        &full_build_journal_path(legacy)?,
        &full_build_journal_path(current)?,
        context,
    )
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

#[cfg(any(test, not(unix)))]
fn remove_private_temporaries(path: &Path, context: &str) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let target = path
        .file_name()
        .ok_or_else(|| GraphError::Io("private state path has no filename".to_owned()))?;
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "list {context} temporary files: {error}"
            )))
        }
    };
    for entry in entries {
        let entry = entry
            .map_err(|error| GraphError::Io(format!("list {context} temporary files: {error}")))?;
        let filename = entry.file_name();
        let Some(suffix) = native_filename_suffix(&filename, target) else {
            continue;
        };
        let owned = super::atomic_file::is_private_temporary_suffix(&suffix)
            || suffix
                .rfind(super::atomic_file::PRIVATE_TEMP_SEPARATOR)
                .is_some_and(|index| {
                    is_family_member_suffix(&suffix[..index])
                        && super::atomic_file::is_private_temporary_suffix(&suffix[index..])
                });
        if !owned {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path()).map_err(|error| {
            GraphError::Io(format!("inspect {context} temporary file: {error}"))
        })?;
        if !metadata.file_type().is_file() {
            return Err(GraphError::Guard(format!(
                "{context} temporary path is not a regular file: {}",
                entry.path().display()
            )));
        }
        remove(&entry.path(), context)?;
    }
    Ok(())
}

#[cfg(any(test, not(unix)))]
fn remove_unsupported_state_families(output: &Path, legacy: &Path) -> Result<()> {
    remove_family(legacy, "unsupported legacy private state")?;
    let full_build_journal = full_build_journal_path(legacy)?;
    remove(&full_build_journal, "unsupported full-build journal")?;
    remove_private_temporaries(legacy, "unsupported legacy private state")?;
    remove_private_temporaries(&full_build_journal, "unsupported full-build journal")?;
    let output = resolve_output_file(output)?;
    let parent = output
        .parent()
        .ok_or_else(|| GraphError::Io("output path has no parent".to_owned()))?;
    let Some(git_dir) = find_git_dir(parent)? else {
        return Ok(());
    };
    for state_dir in repository_private_state_directories(&git_dir)? {
        remove_repository_private_state(&state_dir)?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn remove_unsupported_state(output: &Path, legacy: &Path) -> Result<()> {
    remove_unsupported_state_families(output, legacy)
}

#[cfg(any(test, not(unix)))]
fn repository_state_filename(filename: &str) -> bool {
    if let Some(index) = filename.rfind(super::atomic_file::PRIVATE_TEMP_SEPARATOR) {
        let (target, suffix) = filename.split_at(index);
        return super::atomic_file::is_private_temporary_suffix(suffix)
            && repository_state_filename(target);
    }
    let Some(key) = filename.get(..64).filter(|key| is_generation(key)) else {
        return false;
    };
    let Some(rest) = filename.get(key.len()..) else {
        return false;
    };
    if let Some(suffix) = rest.strip_prefix(".json") {
        return is_family_member_suffix(suffix);
    }
    if let Some(context) = rest.strip_prefix(CONTEXT_SEPARATOR) {
        let Some(context_key) = context.get(..64).filter(|value| is_generation(value)) else {
            return false;
        };
        return context
            .get(context_key.len()..)
            .and_then(|value| value.strip_prefix(".json"))
            .is_some_and(is_family_member_suffix);
    }
    if let Some(revision) = rest.strip_prefix(CONTEXT_REVISION_SEPARATOR) {
        let Some(context) = revision.get(..64).filter(|value| is_generation(value)) else {
            return false;
        };
        return revision
            .get(context.len()..)
            .and_then(|value| value.strip_prefix('-'))
            .is_some_and(is_generation);
    }
    if let Some(context) = rest.strip_prefix(EXPIRED_CONTEXT_SEPARATOR) {
        return is_generation(context);
    }
    if let Some(revision) = rest.strip_prefix(EXPIRED_REVISION_SEPARATOR) {
        return is_generation(revision);
    }
    rest == EXPIRATION_INDEX_SUFFIX || rest == FULL_BUILD_JOURNAL_SUFFIX
}

#[cfg(any(test, not(unix)))]
fn repository_conflict_filename(filename: &str) -> bool {
    if let Some(index) = filename.rfind(super::atomic_file::PRIVATE_TEMP_SEPARATOR) {
        let (target, suffix) = filename.split_at(index);
        return super::atomic_file::is_private_temporary_suffix(suffix)
            && repository_conflict_filename(target);
    }
    let mut parts = filename.split('-');
    parts.next().is_some_and(is_generation)
        && parts.next().is_some_and(is_generation)
        && parts.next().is_some_and(is_generation)
        && parts.next().is_none()
}

#[cfg(any(test, not(unix)))]
fn repository_private_state_directories(git_dir: &Path) -> Result<Vec<PathBuf>> {
    let common_dir = git_common_dir(git_dir)?;
    let mut git_dirs = BTreeSet::new();
    git_dirs.insert(git_dir.to_path_buf());
    git_dirs.insert(common_dir.clone());
    let worktrees = common_dir.join("worktrees");
    match std::fs::symlink_metadata(&worktrees) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(GraphError::Io(format!(
                "inspect Git worktree metadata: {error}"
            )))
        }
        Ok(metadata) if metadata.file_type().is_dir() => {
            let entries = std::fs::read_dir(&worktrees)
                .map_err(|error| GraphError::Io(format!("list Git worktrees: {error}")))?;
            for entry in entries {
                let entry = entry
                    .map_err(|error| GraphError::Io(format!("list Git worktrees: {error}")))?;
                let metadata = std::fs::symlink_metadata(entry.path()).map_err(|error| {
                    GraphError::Io(format!("inspect Git worktree metadata: {error}"))
                })?;
                if !metadata.file_type().is_dir() {
                    return Err(GraphError::Guard(format!(
                        "Git worktree metadata is not a directory: {}",
                        entry.path().display()
                    )));
                }
                if valid_git_dir(&entry.path())? {
                    git_dirs.insert(entry.path());
                }
            }
        }
        Ok(_) => {
            return Err(GraphError::Guard(format!(
                "Git worktrees path is not a directory: {}",
                worktrees.display()
            )))
        }
    }
    Ok(git_dirs
        .into_iter()
        .map(|git_dir| git_dir.join("habitat-graph").join("state"))
        .collect())
}

#[cfg(any(test, not(unix)))]
fn remove_repository_private_state(state_dir: &Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(state_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "inspect unsupported Git private state directory: {error}"
            )))
        }
    };
    if !metadata.file_type().is_dir() {
        return Err(GraphError::Guard(format!(
            "unsupported Git private state path is not a directory: {}",
            state_dir.display()
        )));
    }
    let entries = std::fs::read_dir(state_dir)
        .map_err(|error| GraphError::Io(format!("list unsupported Git private state: {error}")))?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            GraphError::Io(format!("list unsupported Git private state: {error}"))
        })?;
        let filename = entry.file_name();
        if !filename.to_str().is_some_and(repository_state_filename) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path()).map_err(|error| {
            GraphError::Io(format!("inspect unsupported Git private state: {error}"))
        })?;
        if !metadata.file_type().is_file() {
            return Err(GraphError::Guard(format!(
                "unsupported Git private state is not a regular file: {}",
                entry.path().display()
            )));
        }
        remove(&entry.path(), "unsupported Git private state")?;
    }

    let conflicts = state_dir.join(MIGRATION_CONFLICT_DIRECTORY);
    let metadata = match std::fs::symlink_metadata(&conflicts) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "inspect unsupported Git private state conflicts: {error}"
            )))
        }
    };
    if !metadata.file_type().is_dir() {
        return Err(GraphError::Guard(format!(
            "unsupported Git private state conflict path is not a directory: {}",
            conflicts.display()
        )));
    }
    let entries = std::fs::read_dir(&conflicts).map_err(|error| {
        GraphError::Io(format!(
            "list unsupported Git private state conflicts: {error}"
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            GraphError::Io(format!(
                "list unsupported Git private state conflicts: {error}"
            ))
        })?;
        let filename = entry.file_name();
        if !filename.to_str().is_some_and(repository_conflict_filename) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path()).map_err(|error| {
            GraphError::Io(format!(
                "inspect unsupported Git private state conflict: {error}"
            ))
        })?;
        if !metadata.file_type().is_file() {
            return Err(GraphError::Guard(format!(
                "unsupported Git private state conflict is not a regular file: {}",
                entry.path().display()
            )));
        }
        remove(&entry.path(), "unsupported Git private state conflict")?;
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
        .ok_or_else(|| GraphError::Io("private state path has no filename".to_owned()))?;
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
        let Some(suffix) = native_filename_suffix(&filename, base) else {
            continue;
        };
        if is_family_member_suffix(&suffix) {
            members.push((entry.path(), suffix));
        }
    }
    members.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(members)
}

fn native_filename_suffix(filename: &OsStr, base: &OsStr) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let filename = filename.as_bytes();
        let base = base.as_bytes();
        if filename.starts_with(base) {
            std::str::from_utf8(&filename[base.len()..])
                .ok()
                .map(str::to_owned)
        } else {
            None
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;

        let filename: Vec<u16> = filename.encode_wide().collect();
        let base: Vec<u16> = base.encode_wide().collect();
        if filename.starts_with(&base) {
            String::from_utf16(&filename[base.len()..]).ok()
        } else {
            None
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        filename
            .to_str()?
            .strip_prefix(base.to_str()?)
            .map(str::to_owned)
    }
}

fn is_family_member_suffix(suffix: &str) -> bool {
    is_primary_family_member_suffix(suffix) || is_migration_conflict_suffix(suffix)
}

fn is_primary_family_member_suffix(suffix: &str) -> bool {
    suffix.is_empty()
        || suffix == ADD_JOURNAL_SUFFIX
        || suffix == UPDATE_JOURNAL_SUFFIX
        || suffix == FULL_BUILD_JOURNAL_SUFFIX
        || suffix
            .strip_prefix(SNAPSHOT_SEPARATOR)
            .is_some_and(is_generation)
}

fn is_migration_conflict_suffix(suffix: &str) -> bool {
    suffix
        .rsplit_once(MIGRATION_CONFLICT_SEPARATOR)
        .is_some_and(|(member, generation)| {
            is_primary_family_member_suffix(member) && is_generation(generation)
        })
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
    if let Some(base) = without_legacy_conflict.strip_suffix(FULL_BUILD_JOURNAL_SUFFIX) {
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

fn migration_conflict_filename_matches(filename: &OsStr, family: &str) -> bool {
    if !is_generation(family) {
        return false;
    }
    let Some(suffix) = native_filename_suffix(filename, OsStr::new(family)) else {
        return false;
    };
    let Some(suffix) = suffix.strip_prefix('-') else {
        return false;
    };
    let mut generations = suffix.split('-');
    generations.next().is_some_and(is_generation)
        && generations.next().is_some_and(is_generation)
        && generations.next().is_none()
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

    let mut families = BTreeSet::new();
    families.insert(migration_conflict_family_key(path));
    families.insert(output_key(&family_base_path(path)));
    for candidate in context_state_candidates(path)? {
        families.insert(output_key(&family_base_path(&candidate)));
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
        if !families
            .iter()
            .any(|family| migration_conflict_filename_matches(&filename, family))
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
        if is_migration_conflict_suffix(&suffix) {
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

#[cfg(windows)]
fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    left.volume_serial_number().is_some()
        && left.volume_serial_number() == right.volume_serial_number()
        && left.file_index().is_some()
        && left.file_index() == right.file_index()
}

#[cfg(not(any(unix, windows)))]
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
    ensure_supported_reference_backend(git_dir)?;
    let head = git_dir.join("HEAD");
    let identity = read_git_control_line(&head, "Git HEAD")?
        .ok_or_else(|| GraphError::Guard("Git HEAD is missing".to_owned()))?;
    let (context, lineage, revision, commit, unborn_predecessor) = if is_object_id(&identity) {
        let commit = identity.to_ascii_lowercase();
        (
            format!("detached:{commit}"),
            format!("detached:{commit}"),
            format!("commit:{commit}"),
            Some(commit),
            None,
        )
    } else {
        let reference = identity
            .strip_prefix("ref:")
            .map(str::trim)
            .filter(|reference| valid_git_reference(reference))
            .ok_or_else(|| GraphError::Guard("invalid Git HEAD".to_owned()))?;
        let (context, revision, commit, unborn_predecessor) =
            match resolve_git_reference(git_dir, reference)? {
                Some(commit) => {
                    let predecessor_context =
                        generation(format!("ref:{reference}\nunborn").as_bytes());
                    let predecessor_revision = generation(format!("unborn:{reference}").as_bytes());
                    (
                        format!("ref:{reference}\ncommit:{commit}"),
                        format!("commit:{commit}"),
                        Some(commit),
                        Some((predecessor_context, predecessor_revision)),
                    )
                }
                None => (
                    format!("ref:{reference}\nunborn"),
                    format!("unborn:{reference}"),
                    None,
                    None,
                ),
            };
        (
            context,
            format!("ref:{reference}"),
            revision,
            commit,
            unborn_predecessor,
        )
    };
    Ok(ContextIdentity {
        key: generation(context.as_bytes()),
        lineage: generation(lineage.as_bytes()),
        revision: generation(revision.as_bytes()),
        commit,
        unborn_predecessor,
    })
}

fn ensure_supported_reference_backend(git_dir: &Path) -> Result<()> {
    let common_dir = git_common_dir(git_dir)?;
    for root in [git_dir, common_dir.as_path()] {
        let stack = root.join("reftable").join("tables.list");
        match std::fs::symlink_metadata(&stack) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(GraphError::Io(format!(
                    "inspect Git reference backend: {error}"
                )))
            }
            Ok(_) => {
                return Err(GraphError::Guard(
                    "Git reftable references are not supported for private state".to_owned(),
                ))
            }
        }
    }
    Ok(())
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
                && !component.starts_with('.')
                && !component.ends_with('.')
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
    let expected = std::fs::symlink_metadata(path)
        .map_err(|error| GraphError::Io(format!("inspect private state directory: {error}")))?;
    if !expected.file_type().is_dir() {
        return Err(GraphError::Guard(format!(
            "private state directory is not a directory: {}",
            path.display()
        )));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
    }
    let directory = options
        .open(path)
        .map_err(|error| GraphError::Io(format!("open private state directory: {error}")))?;
    let opened = directory
        .metadata()
        .map_err(|error| GraphError::Io(format!("inspect private state directory: {error}")))?;
    if !opened.file_type().is_dir() || !same_file(&expected, &opened) {
        return Err(GraphError::Guard(format!(
            "private state directory changed while being opened: {}",
            path.display()
        )));
    }
    directory
        .set_permissions(std::fs::Permissions::from_mode(0o700))
        .map_err(|error| GraphError::Io(format!("harden private state directory: {error}")))?;
    let current = std::fs::symlink_metadata(path).map_err(|error| {
        GraphError::Guard(format!(
            "private state directory changed while being opened: {error}"
        ))
    })?;
    if !current.file_type().is_dir() || !same_file(&opened, &current) {
        return Err(GraphError::Guard(format!(
            "private state directory changed while being opened: {}",
            path.display()
        )));
    }
    Ok(())
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
fn open_private_file(path: &Path, context: &str) -> Result<Option<std::fs::File>> {
    let expected = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "inspect {context} {}: {error}",
                path.display()
            )))
        }
    };
    if !expected.file_type().is_file() {
        return Err(GraphError::Guard(format!(
            "{context} is not a regular file: {}",
            path.display()
        )));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|error| GraphError::Io(format!("open {context}: {error}")))?;
    let opened = file
        .metadata()
        .map_err(|error| GraphError::Io(format!("inspect opened {context}: {error}")))?;
    if !opened.file_type().is_file() || !same_file(&expected, &opened) {
        return Err(GraphError::Guard(format!(
            "{context} changed while being opened: {}",
            path.display()
        )));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| GraphError::Io(format!("harden {context}: {error}")))?;
    let current = std::fs::symlink_metadata(path).map_err(|error| {
        GraphError::Guard(format!("{context} changed while being opened: {error}"))
    })?;
    if !current.file_type().is_file() || !same_file(&opened, &current) {
        return Err(GraphError::Guard(format!(
            "{context} changed while being opened: {}",
            path.display()
        )));
    }
    Ok(Some(file))
}

#[cfg(not(unix))]
fn open_private_file(path: &Path, _context: &str) -> Result<Option<std::fs::File>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => ensure(path).map(|()| None),
    }
}

#[cfg(unix)]
pub(super) fn ensure(path: &Path) -> Result<()> {
    open_private_file(path, "private state").map(|_| ())
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
    fn reftable_reference_backend_fails_closed() {
        let root = TempDir::new().unwrap();
        let git_dir = create_git(root.path(), "ref: refs/heads/.invalid\n");
        fs::create_dir_all(git_dir.join("reftable")).unwrap();
        fs::write(git_dir.join("reftable/tables.list"), "table.ref\n").unwrap();
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let legacy = output_dir.join(".habitat-graph-state.json");

        let error = super::path_for_output(&output_dir.join("graph.json"), &legacy).unwrap_err();

        assert_eq!(error.kind(), "guard");
        assert!(error.to_string().contains("reftable"));
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
    fn existing_output_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        create_git(root.path(), "ref: refs/heads/main\n");
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let alias = output_dir.join("Graph.json");
        fs::write(&output, "{}").unwrap();
        symlink("graph.json", &alias).unwrap();

        let error = super::path_for_output(
            &alias,
            &output_dir.join(".Graph.json.habitat-graph-state.json"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), "guard");
        assert!(error.to_string().contains("symbolic link"));
    }

    #[test]
    fn existing_output_uses_canonical_final_component() {
        let root = TempDir::new().unwrap();
        let output = root.path().join("Graph.json");
        fs::write(&output, "{}").unwrap();
        let alias = root.path().join("graph.json");
        if fs::symlink_metadata(&alias).is_err() {
            return;
        }

        assert_eq!(
            super::resolve_output_file(&alias).unwrap(),
            super::resolve_output_file(&output).unwrap()
        );
    }

    #[test]
    fn absent_output_aliases_share_the_identity_lock() {
        let root = TempDir::new().unwrap();
        let lower = root.path().join("graph.json");
        let upper = root.path().join("GRAPH.JSON");

        let first = super::acquire_output_identity_lock(&lower).unwrap();
        let error = super::acquire_output_identity_lock(&upper).unwrap_err();

        assert_eq!(error.kind(), "guard");
        drop(first);
        super::acquire_output_identity_lock(&upper).unwrap();
    }

    #[test]
    fn output_identity_lock_rejects_reserved_output_names() {
        let root = TempDir::new().unwrap();

        for filename in [
            super::OUTPUT_IDENTITY_LOCK,
            ".HABITAT-GRAPH.OUTPUT-IDENTITY-LOCK",
        ] {
            let output = root.path().join(filename);
            let error = super::acquire_output_identity_lock(&output).unwrap_err();

            assert_eq!(error.kind(), "guard");
            assert!(error.to_string().contains("reserved"));
            assert!(!output.exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn output_identity_lock_rejects_hard_linked_outputs_without_chmod() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = TempDir::new().unwrap();
        let lock = root.path().join(super::OUTPUT_IDENTITY_LOCK);
        let output = root.path().join("graph.json");
        fs::write(&lock, "public graph").unwrap();
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
        fs::hard_link(&lock, &output).unwrap();

        let error = super::acquire_output_identity_lock(&output).unwrap_err();

        assert_eq!(error.kind(), "guard");
        assert!(error.to_string().contains("aliases"));
        assert_eq!(
            fs::metadata(&lock).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolved_output_file_pins_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        let alias = root.path().join("alias");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        symlink(&first, &alias).unwrap();

        let output = super::resolve_output_file(&alias.join("graph.json")).unwrap();
        fs::remove_file(&alias).unwrap();
        symlink(&second, &alias).unwrap();
        fs::write(&output, "pinned").unwrap();

        assert_eq!(
            fs::read_to_string(first.join("graph.json")).unwrap(),
            "pinned"
        );
        assert!(!second.join("graph.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn full_build_state_rejects_git_revision_change() {
        let root = TempDir::new().unwrap();
        let git_dir = create_git(root.path(), "ref: refs/heads/main\n");
        write_ref(&git_dir, "refs/heads/main", &"1".repeat(40));
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let legacy = output_dir.join(".habitat-graph-state.json");
        let state = super::prepare_full_build_state(&output, &legacy).unwrap();

        write_ref(&git_dir, "refs/heads/main", &"2".repeat(40));

        let error = super::commit_full_build_state(&state, b"{}").unwrap_err();
        assert_eq!(error.kind(), "guard");
        assert!(error.to_string().contains("Git context changed"));
    }

    #[cfg(unix)]
    #[test]
    fn full_build_state_rechecks_context_after_private_commit() {
        let root = TempDir::new().unwrap();
        let git_dir = create_git(root.path(), "ref: refs/heads/main\n");
        write_ref(&git_dir, "refs/heads/main", &"1".repeat(40));
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let legacy = output_dir.join(".habitat-graph-state.json");
        let state = super::prepare_full_build_state(&output, &legacy).unwrap();
        let public = super::generation(b"public");
        let semantic = super::semantic_generation(&Graph::new()).unwrap();
        let bytes = super::serialize(&Graph::new(), &public, &semantic).unwrap();
        let error = super::commit_full_build_state_with(
            &state,
            &bytes,
            || {
                super::write_full_build_state(state.path(), &bytes)?;
                write_ref(&git_dir, "refs/heads/main", &"2".repeat(40));
                Ok(())
            },
            || panic!("publication must not run after context rotation"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), "guard");
        assert!(error.to_string().contains("Git context changed"));
        assert!(super::full_build_journal_path(state.path())
            .unwrap()
            .exists());
    }

    #[cfg(unix)]
    #[test]
    fn full_build_context_rotation_during_publication_retains_journal() {
        let root = TempDir::new().unwrap();
        let git_dir = create_git(root.path(), "ref: refs/heads/main\n");
        write_ref(&git_dir, "refs/heads/main", &"1".repeat(40));
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let legacy = output_dir.join(".habitat-graph-state.json");
        let state = super::prepare_full_build_state(&output, &legacy).unwrap();
        let public = super::generation(b"public");
        let semantic = super::semantic_generation(&Graph::new()).unwrap();
        let bytes = super::serialize(&Graph::new(), &public, &semantic).unwrap();

        let error = super::commit_full_build_state_with(
            &state,
            &bytes,
            || super::write_full_build_state(state.path(), &bytes),
            || {
                fs::write(&output, "published").unwrap();
                write_ref(&git_dir, "refs/heads/main", &"2".repeat(40));
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error.kind(), "guard");
        assert!(super::full_build_journal_path(state.path())
            .unwrap()
            .exists());
        assert!(super::ensure_no_pending_full_build_journal(state.path()).is_err());

        let original_context_journal =
            super::transaction_path(state.path(), super::FULL_BUILD_JOURNAL_SUFFIX).unwrap();
        let next_state = super::prepare_full_build_state(&output, &legacy).unwrap();
        super::commit_full_build_state(&next_state, &bytes).unwrap();

        assert!(original_context_journal.exists());
        assert!(super::ensure_no_pending_full_build_journal(state.path()).is_err());
        assert!(super::ensure_no_pending_full_build_journal(next_state.path()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn context_rotation_after_journal_removal_restores_the_journal() {
        let root = TempDir::new().unwrap();
        let git_dir = create_git(root.path(), "ref: refs/heads/main\n");
        write_ref(&git_dir, "refs/heads/main", &"1".repeat(40));
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let legacy = output_dir.join(".habitat-graph-state.json");
        let state = super::path_for_output(&output, &legacy).unwrap();
        let journal = super::transaction_path(&state, super::ADD_JOURNAL_SUFFIX).unwrap();
        super::write(&journal, b"pending transaction").unwrap();

        let error = super::finalize_context_journal_with(
            &output,
            &state,
            &journal,
            "test transaction",
            || {
                super::remove(&journal, "test transaction")?;
                write_ref(&git_dir, "refs/heads/main", &"2".repeat(40));
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error.kind(), "guard");
        assert_eq!(fs::read_to_string(journal).unwrap(), "pending transaction");
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
    fn output_transaction_lock_accepts_non_utf8_state_name() {
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

        let root = TempDir::new().unwrap();
        let state = root
            .path()
            .join(OsString::from_vec(b"state-\xff.json".to_vec()));
        let lock_path = super::output_lock_path(&state).unwrap();
        let _lock = super::acquire_output_lock(&state).unwrap();

        let mut expected = state.file_name().unwrap().as_bytes().to_vec();
        expected.extend_from_slice(super::OUTPUT_LOCK_SUFFIX.as_bytes());
        assert_eq!(lock_path.file_name().unwrap().as_bytes(), expected);
        assert!(lock_path.is_file());
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

    #[cfg(unix)]
    #[test]
    fn private_file_read_hardens_the_opened_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = TempDir::new().unwrap();
        let state = root.path().join("state.json");
        fs::write(&state, "private contents").unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o644)).unwrap();

        let text = super::read_text(&state, "test private state")
            .unwrap()
            .unwrap();

        assert_eq!(text, "private contents");
        assert_eq!(
            fs::metadata(state).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_file_read_rejects_symlinks_without_changing_the_target() {
        use std::os::unix::fs::{symlink, PermissionsExt as _};

        let root = TempDir::new().unwrap();
        let state = root.path().join("state.json");
        let target = root.path().join("target.json");
        fs::write(&target, "private contents").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        symlink(&target, &state).unwrap();

        let error = super::read_text(&state, "test private state").unwrap_err();

        assert_eq!(error.kind(), "guard");
        assert_eq!(fs::read_to_string(&target).unwrap(), "private contents");
        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o777,
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
    fn recurring_public_generation_replaces_its_stale_snapshot() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("state.json");
        let first_generation = super::generation(b"first public graph");
        let second_generation = super::generation(b"second public graph");
        let semantic = super::semantic_generation(&Graph::new()).unwrap();

        for (lineage, generation) in [
            ("first-private-lineage", &first_generation),
            ("second-private-lineage", &second_generation),
            ("recurring-private-lineage", &first_generation),
        ] {
            let mut graph = Graph::new();
            graph.manifest.tool_version = lineage.to_owned();
            super::write_state(
                &path,
                &super::serialize(&graph, generation, &semantic).unwrap(),
            )
            .unwrap();
        }

        let current = super::load_matching(
            &path,
            Some(&first_generation),
            Some(&semantic),
            "test private state",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            current.graph.manifest.tool_version,
            "recurring-private-lineage"
        );

        let mut final_graph = Graph::new();
        final_graph.manifest.tool_version = "final-private-lineage".to_owned();
        super::write_state(
            &path,
            &super::serialize(&final_graph, &second_generation, &semantic).unwrap(),
        )
        .unwrap();
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
            "recurring-private-lineage"
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
    fn context_revision_records_commit_for_bounded_ancestry_checks() {
        let root = TempDir::new().unwrap();
        init_real_git(root.path());
        commit_real_git(root.path(), "first");
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let legacy = output_dir.join(".habitat-graph-state.json");

        let state = super::path_for_output(&output, &legacy).unwrap();
        let revision = super::context_revision(&state).unwrap().unwrap();

        assert_eq!(
            revision.commit.unwrap(),
            git(root.path(), &["rev-parse", "HEAD"])
        );
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
    fn legacy_revision_marker_is_upgraded_during_ancestry_check() {
        let root = TempDir::new().unwrap();
        init_real_git(root.path());
        commit_real_git(root.path(), "ancestor");
        let ancestor_commit = git(root.path(), &["rev-parse", "HEAD"]);
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let legacy = output_dir.join(".habitat-graph-state.json");
        let public_generation = super::generation(b"shared public bytes");
        let semantic = super::semantic_generation(&Graph::new()).unwrap();
        let mut private = Graph::new();
        private.manifest.tool_version = "legacy-marker-lineage".to_owned();
        let ancestor_path = super::path_for_output(&output, &legacy).unwrap();
        super::write_state(
            &ancestor_path,
            &super::serialize(&private, &public_generation, &semantic).unwrap(),
        )
        .unwrap();
        let revision = super::context_revision(&ancestor_path).unwrap().unwrap();
        let marker = super::context_revision_path(&ancestor_path, &revision.key).unwrap();
        fs::write(&marker, "").unwrap();

        commit_real_git(root.path(), "descendant");
        let current = super::path_for_output(&output, &legacy).unwrap();
        let inherited = super::load_matching(
            &current,
            Some(&public_generation),
            Some(&semantic),
            "test private state",
        )
        .unwrap()
        .unwrap();

        assert_eq!(inherited.graph, private);
        assert_eq!(fs::read_to_string(marker).unwrap(), ancestor_commit);
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
        let legacy_full_build = super::full_build_journal_path(&legacy).unwrap();
        fs::write(&legacy, "legacy state").unwrap();
        fs::write(&legacy_snapshot, "legacy snapshot").unwrap();
        fs::write(&legacy_journal, "legacy journal").unwrap();
        fs::write(&legacy_full_build, "legacy full-build journal").unwrap();

        let state = super::path_for_output(&output_dir.join("graph.json"), &legacy).unwrap();
        let snapshot = sibling(&state, &format!("{SNAPSHOT_SEPARATOR}{generation}"));
        let journal = sibling(&state, ADD_JOURNAL_SUFFIX);
        let full_build = super::full_build_journal_path(&state).unwrap();

        assert_eq!(fs::read_to_string(state).unwrap(), "legacy state");
        assert_eq!(fs::read_to_string(snapshot).unwrap(), "legacy snapshot");
        assert_eq!(fs::read_to_string(journal).unwrap(), "legacy journal");
        assert_eq!(
            fs::read_to_string(full_build).unwrap(),
            "legacy full-build journal"
        );
        assert!(!legacy.exists());
        assert!(!legacy_snapshot.exists());
        assert!(!legacy_journal.exists());
        assert!(!legacy_full_build.exists());
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
    fn migration_conflict_discovery_ignores_prefixed_backups() {
        let root = TempDir::new().unwrap();
        let state = root.path().join("current-state.json");
        let conflict = super::migration_conflict_path(&state, b"private lineage");
        fs::create_dir_all(conflict.parent().unwrap()).unwrap();
        fs::write(&conflict, "private lineage").unwrap();
        let mut backup_name = conflict.file_name().unwrap().to_os_string();
        backup_name.push("-backup");
        let backup = conflict.with_file_name(backup_name);
        fs::write(&backup, "user backup").unwrap();

        assert_eq!(
            super::migration_conflict_candidates(&state).unwrap(),
            vec![conflict.clone()]
        );
        super::remove_family(&state, "test private state").unwrap();
        assert!(!conflict.exists());
        assert_eq!(fs::read_to_string(backup).unwrap(), "user backup");
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
    fn family_cleanup_preserves_prefix_lookalikes() {
        let root = TempDir::new().unwrap();
        let state = root.path().join("state.json");
        let generation = super::generation(b"owned generation");
        let conflict_generation = super::generation(b"conflict generation");
        let snapshot_suffix = format!("{SNAPSHOT_SEPARATOR}{generation}");
        let owned = [
            state.clone(),
            sibling(&state, &snapshot_suffix),
            sibling(&state, ADD_JOURNAL_SUFFIX),
            sibling(&state, super::UPDATE_JOURNAL_SUFFIX),
            sibling(&state, super::FULL_BUILD_JOURNAL_SUFFIX),
            sibling(
                &state,
                &format!("{}{generation}", super::MIGRATION_CONFLICT_SEPARATOR),
            ),
            sibling(
                &state,
                &format!(
                    "{snapshot_suffix}{}{conflict_generation}",
                    super::MIGRATION_CONFLICT_SEPARATOR
                ),
            ),
            sibling(
                &state,
                &format!(
                    "{ADD_JOURNAL_SUFFIX}{}{conflict_generation}",
                    super::MIGRATION_CONFLICT_SEPARATOR
                ),
            ),
        ];
        let lookalikes = [
            sibling(&state, &format!("{SNAPSHOT_SEPARATOR}{generation}.backup")),
            sibling(&state, &format!("{ADD_JOURNAL_SUFFIX}.backup")),
            sibling(&state, &format!("{}.backup", super::UPDATE_JOURNAL_SUFFIX)),
            sibling(
                &state,
                &format!("{}.backup", super::FULL_BUILD_JOURNAL_SUFFIX),
            ),
            sibling(
                &state,
                &format!("{}{generation}.backup", super::MIGRATION_CONFLICT_SEPARATOR),
            ),
        ];
        for path in owned.iter().chain(&lookalikes) {
            fs::write(path, "state").unwrap();
        }

        super::remove_family(&state, "test private state").unwrap();

        assert!(owned.iter().all(|path| !path.exists()));
        assert!(lookalikes.iter().all(|path| path.exists()));
    }

    #[cfg(unix)]
    #[test]
    fn family_discovery_preserves_lossy_filename_lookalikes() {
        use std::os::unix::ffi::OsStringExt as _;

        let root = TempDir::new().unwrap();
        let state = root
            .path()
            .join(OsString::from_vec(b"state-\xff.json".to_vec()));
        let generation = super::generation(b"owned snapshot");
        let snapshot_suffix = format!("{SNAPSHOT_SEPARATOR}{generation}");
        let snapshot = sibling(&state, &snapshot_suffix);
        let journal = sibling(&state, ADD_JOURNAL_SUFFIX);
        let lossy = root
            .path()
            .join(state.file_name().unwrap().to_string_lossy().as_ref());
        let lossy_snapshot = sibling(&lossy, &snapshot_suffix);
        let lossy_journal = sibling(&lossy, ADD_JOURNAL_SUFFIX);
        for path in [
            &state,
            &snapshot,
            &journal,
            &lossy,
            &lossy_snapshot,
            &lossy_journal,
        ] {
            fs::write(path, "state").unwrap();
        }

        assert_eq!(
            super::snapshot_candidates(&state).unwrap(),
            vec![snapshot.clone()]
        );
        super::remove_family(&state, "test private state").unwrap();

        assert!(!state.exists());
        assert!(!snapshot.exists());
        assert!(!journal.exists());
        assert!(lossy.exists());
        assert!(lossy_snapshot.exists());
        assert!(lossy_journal.exists());
    }

    #[test]
    fn unsupported_cleanup_removes_every_repository_state_family() {
        let root = TempDir::new().unwrap();
        let git_dir = create_git(root.path(), "ref: refs/heads/main\n");
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let output = super::resolve_output_file(&output).unwrap();
        let legacy = output_dir.join(".habitat-graph-state.json");
        let state_dir = git_dir.join("habitat-graph/state");
        fs::create_dir_all(&state_dir).unwrap();
        let key = super::output_key(&output);
        let generation = super::generation(b"snapshot");
        let context = super::generation(b"context");
        let unscoped = state_dir.join(format!("{key}.json"));
        let scoped = state_dir.join(format!("{key}{}{context}.json", super::CONTEXT_SEPARATOR));
        let scoped_journal = sibling(&scoped, super::UPDATE_JOURNAL_SUFFIX);
        let scoped_full_build = sibling(&scoped, super::FULL_BUILD_JOURNAL_SUFFIX);
        let unrelated = state_dir.join(format!("{}.json", super::generation(b"other output")));
        let lookalike = sibling(&unrelated, &format!("{ADD_JOURNAL_SUFFIX}.backup"));
        let invalid_temporary = sibling(
            &unrelated,
            &format!("{}1.2", super::super::atomic_file::PRIVATE_TEMP_SEPARATOR),
        );
        let conflict_dir = state_dir.join(super::MIGRATION_CONFLICT_DIRECTORY);
        fs::create_dir_all(&conflict_dir).unwrap();
        let conflict = conflict_dir.join(format!(
            "{}-{}-{}",
            super::generation(b"family"),
            super::generation(b"member"),
            super::generation(b"contents")
        ));
        let private_temporary_suffix =
            format!("{}1.2.3", super::super::atomic_file::PRIVATE_TEMP_SEPARATOR);
        let owned = [
            legacy.clone(),
            sibling(&legacy, ADD_JOURNAL_SUFFIX),
            sibling(&legacy, &private_temporary_suffix),
            unscoped.clone(),
            sibling(&unscoped, &format!("{SNAPSHOT_SEPARATOR}{generation}")),
            scoped.clone(),
            scoped_journal.clone(),
            sibling(&scoped_journal, &private_temporary_suffix),
            scoped_full_build.clone(),
            sibling(&scoped_full_build, &private_temporary_suffix),
            state_dir.join(format!("{key}{}", super::FULL_BUILD_JOURNAL_SUFFIX)),
            unrelated,
            conflict.clone(),
            sibling(&conflict, &private_temporary_suffix),
        ];
        for path in owned.iter().chain([&lookalike, &invalid_temporary]) {
            fs::write(path, "raw state").unwrap();
        }

        super::remove_unsupported_state_families(&output, &legacy).unwrap();

        assert!(owned.iter().all(|path| !path.exists()));
        assert!(lookalike.exists());
        assert!(invalid_temporary.exists());
    }

    #[test]
    fn unsupported_cleanup_removes_linked_worktree_state() {
        let root = TempDir::new().unwrap();
        let git_dir = create_git(root.path(), "ref: refs/heads/main\n");
        let linked_git_dir = git_dir.join("worktrees/linked");
        fs::create_dir_all(&linked_git_dir).unwrap();
        fs::write(linked_git_dir.join("HEAD"), "ref: refs/heads/linked\n").unwrap();
        fs::write(linked_git_dir.join("commondir"), "../..\n").unwrap();
        let output_dir = root.path().join("public");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("graph.json");
        let legacy = output_dir.join(".habitat-graph-state.json");
        let main_state = git_dir
            .join("habitat-graph/state")
            .join(format!("{}.json", super::generation(b"main state")));
        let linked_state = linked_git_dir
            .join("habitat-graph/state")
            .join(format!("{}.json", super::generation(b"linked state")));
        fs::create_dir_all(main_state.parent().unwrap()).unwrap();
        fs::create_dir_all(linked_state.parent().unwrap()).unwrap();
        fs::write(&main_state, "main raw state").unwrap();
        fs::write(&linked_state, "linked raw state").unwrap();

        super::remove_unsupported_state_families(&output, &legacy).unwrap();

        assert!(!main_state.exists());
        assert!(!linked_state.exists());
    }

    #[test]
    fn context_discovery_ignores_family_prefix_lookalikes() {
        let root = TempDir::new().unwrap();
        let key = super::generation(b"output");
        let current_context = super::generation(b"current context");
        let unrelated_context = super::generation(b"unrelated context");
        let current = root.path().join(format!(
            "{key}{}{current_context}.json",
            super::CONTEXT_SEPARATOR
        ));
        let lookalike = root.path().join(format!(
            "{key}{}{unrelated_context}.json{ADD_JOURNAL_SUFFIX}.backup",
            super::CONTEXT_SEPARATOR
        ));
        fs::write(&current, "state").unwrap();
        fs::write(lookalike, "backup").unwrap();

        assert_eq!(
            super::context_state_candidates(&current).unwrap(),
            vec![current]
        );
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
            super::write_context_revision(&path, &context, None).unwrap();
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
        let expiration_files = fs::read_dir(root.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.ends_with(super::EXPIRATION_INDEX_SUFFIX)
                    || name.contains(super::EXPIRED_CONTEXT_SEPARATOR)
                    || name.contains(super::EXPIRED_REVISION_SEPARATOR)
            })
            .count();
        assert_eq!(expiration_files, 1);
    }

    #[test]
    fn legacy_expiration_markers_are_compacted_on_state_write() {
        let root = TempDir::new().unwrap();
        let output = super::generation(b"output path");
        let current = super::generation(b"current context");
        let expired_context = super::generation(b"expired context");
        let expired_revision = super::generation(b"expired revision");
        let path = root.path().join(format!(
            "{output}{}{current}.json",
            super::CONTEXT_SEPARATOR
        ));
        let context_marker = root.path().join(format!(
            "{output}{}{expired_context}",
            super::EXPIRED_CONTEXT_SEPARATOR
        ));
        let revision_marker = root.path().join(format!(
            "{output}{}{expired_revision}",
            super::EXPIRED_REVISION_SEPARATOR
        ));
        fs::write(&context_marker, "").unwrap();
        fs::write(&revision_marker, "").unwrap();
        let public = super::generation(b"public");
        let semantic = super::semantic_generation(&Graph::new()).unwrap();

        super::write_state(
            &path,
            &super::serialize(&Graph::new(), &public, &semantic).unwrap(),
        )
        .unwrap();

        let index = super::load_expiration_index(&path).unwrap();
        assert!(index.contexts.contains(&expired_context));
        assert!(index.revisions.contains(&expired_revision));
        assert!(!context_marker.exists());
        assert!(!revision_marker.exists());
    }

    #[test]
    fn compacted_legacy_index_retries_stale_marker_cleanup() {
        let root = TempDir::new().unwrap();
        let output = super::generation(b"output path");
        let current = super::generation(b"current context");
        let expired_context = super::generation(b"expired context");
        let path = root.path().join(format!(
            "{output}{}{current}.json",
            super::CONTEXT_SEPARATOR
        ));
        let marker = root.path().join(format!(
            "{output}{}{expired_context}",
            super::EXPIRED_CONTEXT_SEPARATOR
        ));
        let index_path = super::expiration_index_path(&path).unwrap();
        fs::write(
            &index_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": super::LEGACY_EXPIRATION_INDEX_SCHEMA,
                "compacted": true,
                "contexts": [],
                "revisions": [],
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(&marker, "").unwrap();

        super::update_expiration_index(&path, |_| {}).unwrap();

        assert!(!marker.exists());
        assert!(super::load_expiration_index(&path)
            .unwrap()
            .contexts
            .contains(&expired_context));
    }

    #[test]
    fn compacted_prefix_index_preserves_denials_and_admissions() {
        let root = TempDir::new().unwrap();
        let output = super::generation(b"output path");
        let current = super::generation(b"current context");
        let path = root.path().join(format!(
            "{output}{}{current}.json",
            super::CONTEXT_SEPARATOR
        ));
        let denied_context = super::generation(b"denied context");
        let other_context = super::generation(b"other denied context");
        let admitted_context = super::generation(b"admitted context");
        let denied_revision = super::generation(b"denied revision");
        let other_revision = super::generation(b"other denied revision");
        let admitted_revision = super::generation(b"admitted revision");
        let index_path = super::expiration_index_path(&path).unwrap();
        fs::write(
            &index_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": super::PREFIX_EXPIRATION_INDEX_SCHEMA,
                "context_prefixes": [""],
                "revision_prefixes": [""],
                "admitted_contexts": [&admitted_context],
                "admitted_revisions": [&admitted_revision],
            }))
            .unwrap(),
        )
        .unwrap();

        let expired = super::load_expiration_index(&path).unwrap();
        assert!(expired.contexts.contains(&denied_context));
        assert!(expired.contexts.contains(&other_context));
        assert!(!expired.contexts.contains(&admitted_context));
        assert!(expired.revisions.contains(&denied_revision));
        assert!(expired.revisions.contains(&other_revision));
        assert!(!expired.revisions.contains(&admitted_revision));

        super::update_expiration_index(&path, |index| {
            index.contexts.remove(&denied_context);
            index.revisions.remove(&denied_revision);
        })
        .unwrap();

        let expired = super::load_expiration_index(&path).unwrap();
        assert!(!expired.contexts.contains(&denied_context));
        assert!(expired.contexts.contains(&other_context));
        assert!(!expired.revisions.contains(&denied_revision));
        assert!(expired.revisions.contains(&other_revision));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&fs::read_to_string(index_path).unwrap())
                .unwrap()["schema"],
            super::PREFIX_EXPIRATION_INDEX_SCHEMA
        );
    }

    #[test]
    fn expiration_log_preserves_exact_denials_without_prefix_false_positives() {
        let root = TempDir::new().unwrap();
        let output = super::generation(b"output path");
        let current = super::generation(b"current context");
        let path = root.path().join(format!(
            "{output}{}{current}.json",
            super::CONTEXT_SEPARATOR
        ));
        let values: Vec<_> = (0..512)
            .map(|index| super::generation(format!("expired-{index}").as_bytes()))
            .collect();
        super::update_expiration_index(&path, |index| {
            index.contexts.extend(values.iter().cloned());
        })
        .unwrap();
        let prefix = &values[0][..2];
        let mut candidate_index = 0_u64;
        let unexpired = loop {
            let candidate = super::generation(format!("unexpired-{candidate_index}").as_bytes());
            if candidate.starts_with(prefix) && !values.contains(&candidate) {
                break candidate;
            }
            candidate_index += 1;
        };

        let expired = super::load_expiration_index(&path).unwrap();
        assert!(values.iter().all(|value| expired.contexts.contains(value)));
        assert!(!expired.contexts.contains(&unexpired));
        assert!(
            fs::read_to_string(super::expiration_index_path(&path).unwrap())
                .unwrap()
                .starts_with(super::EXPIRATION_LOG_HEADER)
        );

        let admitted = values.last().unwrap();
        super::update_expiration_index(&path, |index| {
            index.contexts.remove(admitted);
        })
        .unwrap();
        let expired = super::load_expiration_index(&path).unwrap();
        assert!(!expired.contexts.contains(admitted));
        assert!(values[..values.len() - 1]
            .iter()
            .all(|value| expired.contexts.contains(value)));
    }

    #[test]
    fn expiration_log_ignores_an_incomplete_change_frame() {
        use std::io::Write as _;

        let root = TempDir::new().unwrap();
        let output = super::generation(b"output path");
        let current = super::generation(b"current context");
        let path = root.path().join(format!(
            "{output}{}{current}.json",
            super::CONTEXT_SEPARATOR
        ));
        let first = super::generation(b"first expiration");
        let second = super::generation(b"second expiration");
        let third = super::generation(b"third expiration");
        super::update_expiration_index(&path, |index| {
            index.contexts.insert(first.clone());
        })
        .unwrap();
        let index_path = super::expiration_index_path(&path).unwrap();
        let mut records = Vec::new();
        super::push_expiration_record(&mut records, b'+', b'c', &second);
        super::push_expiration_record(&mut records, b'+', b'c', &third);
        let frame = super::expiration_frame(&records).unwrap();
        let partial_length = super::EXPIRATION_FRAME_HEADER_BYTES + super::EXPIRATION_RECORD_BYTES;
        fs::OpenOptions::new()
            .append(true)
            .open(&index_path)
            .unwrap()
            .write_all(&frame[..partial_length])
            .unwrap();

        let expired = super::load_expiration_index(&path).unwrap();
        assert!(expired.contexts.contains(&first));
        assert!(!expired.contexts.contains(&second));
        assert!(!expired.contexts.contains(&third));

        super::update_expiration_index(&path, |index| {
            index.contexts.insert(second.clone());
            index.contexts.insert(third.clone());
        })
        .unwrap();
        let expired = super::load_expiration_index(&path).unwrap();
        assert!(expired.contexts.contains(&first));
        assert!(expired.contexts.contains(&second));
        assert!(expired.contexts.contains(&third));
    }

    #[test]
    fn pruned_git_context_blocks_incremental_but_allows_full_rebuild() {
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
        assert!(super::context_is_expired(&expired_path).unwrap());
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

        let renamed_revision = super::git_context_identity(&git_dir).unwrap().revision;
        let renamed_state = super::path_for_full_build(&output, &legacy).unwrap();
        assert_ne!(renamed_state, expired_path);
        assert!(super::ensure_revision_not_expired(&renamed_state, &renamed_revision).is_err());
        assert!(super::path_for_output(&output, &legacy).is_err());
        let renamed_public = super::generation(b"renamed rebuilt public");
        super::write_full_build_state(
            &renamed_state,
            &super::serialize(&Graph::new(), &renamed_public, &semantic).unwrap(),
        )
        .unwrap();
        assert_eq!(
            super::path_for_output(&output, &legacy).unwrap(),
            renamed_state
        );

        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let rebuilt_state = super::path_for_full_build(&output, &legacy).unwrap();
        assert_eq!(rebuilt_state, expired_path);
        assert!(super::context_is_expired(&rebuilt_state).unwrap());
        assert!(super::path_for_output(&output, &legacy).is_err());
        let rebuilt_public = super::generation(b"rebuilt public");
        super::write_full_build_state(
            &rebuilt_state,
            &super::serialize(&Graph::new(), &rebuilt_public, &semantic).unwrap(),
        )
        .unwrap();
        assert_eq!(
            super::path_for_output(&output, &legacy).unwrap(),
            rebuilt_state
        );
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
