use std::collections::BTreeSet;
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
const MIGRATION_CONFLICT_SEPARATOR: &str = ".migration-conflict-";
const ADD_JOURNAL_SUFFIX: &str = ".add-journal";
const MAX_SNAPSHOTS: usize = 16;
const MAX_CONTEXTS_PER_OUTPUT: usize = 16;
const MAX_GIT_CONTROL_LINE_BYTES: u64 = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct StoredGraph {
    pub(super) graph: Graph,
    pub(super) public_generation: Option<String>,
    pub(super) public_semantic_generation: Option<String>,
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

    let canonical_output = canonical_parent.join(
        output
            .file_name()
            .ok_or_else(|| GraphError::Io("output path has no filename".to_owned()))?,
    );
    let key = output_key(&canonical_output);
    let unscoped_state_path = state_dir.join(format!("{key}.json"));
    let context = git_context_key(&git_dir)?;
    let state_path = state_dir.join(format!("{key}{CONTEXT_SEPARATOR}{context}.json"));
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

fn is_generation(value: &str) -> bool {
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

pub(super) fn load_matching(
    path: &Path,
    public_generation: Option<&str>,
    public_semantic_generation: Option<&str>,
    context: &str,
) -> Result<Option<StoredGraph>> {
    let current = read(path, context)?;

    if let Some(generation) = public_generation {
        let mut exact = Vec::new();
        if current
            .as_ref()
            .is_some_and(|stored| stored.public_generation.as_deref() == Some(generation))
        {
            exact.extend(current.iter().cloned());
        }
        let snapshot = snapshot_path(path, generation)?;
        if let Some(stored) = read(&snapshot, context)? {
            if stored.public_generation.as_deref() == Some(generation) {
                exact.push(stored);
            }
        }
        if let Some(stored) = select_unique_state(exact.iter(), path)? {
            return Ok(Some(stored));
        }

        let mut inherited = Vec::new();
        for candidate in context_state_candidates(path)? {
            if candidate == path {
                continue;
            }
            if let Some(stored) = read(&candidate, context)? {
                if stored.public_generation.as_deref() == Some(generation) {
                    inherited.push(stored);
                }
            }
            let snapshot = snapshot_path(&candidate, generation)?;
            if let Some(stored) = read(&snapshot, context)? {
                if stored.public_generation.as_deref() == Some(generation) {
                    inherited.push(stored);
                }
            }
        }
        if let Some(stored) = select_unique_state(inherited.iter(), path)? {
            return Ok(Some(stored));
        }
    }

    let Some(semantic_generation) = public_semantic_generation else {
        return Ok(None);
    };
    let mut states = current.into_iter().collect::<Vec<_>>();
    let mut candidates = snapshot_candidates(path)?;
    candidates.sort_unstable();
    for candidate in candidates {
        if let Some(stored) = read(&candidate, context)? {
            states.push(stored);
        }
    }
    select_unique_state(
        states.iter().filter(|stored| {
            stored.public_semantic_generation.as_deref() == Some(semantic_generation)
        }),
        path,
    )
}

fn select_unique_state<'a>(
    candidates: impl Iterator<Item = &'a StoredGraph>,
    path: &Path,
) -> Result<Option<StoredGraph>> {
    let mut selected: Option<&StoredGraph> = None;
    for candidate in candidates {
        if selected.is_some_and(|existing| existing.graph != candidate.graph) {
            return Err(GraphError::Guard(format!(
                "multiple private states match the public graph: {}",
                path.display()
            )));
        }
        selected = Some(candidate);
    }
    Ok(selected.cloned())
}

pub(super) fn write_state(path: &Path, bytes: &[u8]) -> Result<()> {
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

fn context_filename_prefix(path: &Path) -> Option<String> {
    let filename = path.file_name()?.to_str()?;
    let (key, context) = filename.split_once(CONTEXT_SEPARATOR)?;
    let context = context.strip_suffix(".json")?;
    if key.is_empty() || !is_generation(context) {
        return None;
    }
    Some(format!("{key}{CONTEXT_SEPARATOR}"))
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
            || members.iter().any(|(_, suffix)| {
                suffix.contains(ADD_JOURNAL_SUFFIX) || suffix.contains(MIGRATION_CONFLICT_SEPARATOR)
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
        remove_family(&candidate, "expired private state context")?;
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
            let conflict = migration_conflict_path(current, &bytes)?;
            match std::fs::symlink_metadata(&conflict) {
                Ok(conflict_metadata) => {
                    if !conflict_metadata.file_type().is_file()
                        || read_regular(&conflict, &conflict_metadata, context)? != bytes
                    {
                        return Err(GraphError::Guard(format!(
                            "private state migration conflict is ambiguous: {}",
                            conflict.display()
                        )));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    write(&conflict, &bytes)?;
                }
                Err(error) => {
                    return Err(GraphError::Io(format!(
                        "inspect private state migration conflict: {error}"
                    )))
                }
            }
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
        let filename = current
            .file_name()
            .ok_or_else(|| GraphError::Io("private state path has no filename".to_owned()))?;
        let mut destination_name = OsString::from(filename);
        destination_name.push(&suffix);
        let destination = current.with_file_name(destination_name);
        if let Err(error) = migrate(&source, &destination, context) {
            if first_error.is_none() {
                first_error = Some(error);
            }
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
            || suffix.starts_with(MIGRATION_CONFLICT_SEPARATOR)
        {
            members.push((entry.path(), suffix.to_owned()));
        }
    }
    members.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(members)
}

fn migration_conflict_path(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let filename = path
        .file_name()
        .ok_or_else(|| GraphError::Io("private state path has no filename".to_owned()))?;
    let mut conflict_name = OsString::from(filename);
    conflict_name.push(MIGRATION_CONFLICT_SEPARATOR);
    conflict_name.push(generation(bytes));
    Ok(path.with_file_name(conflict_name))
}

fn ensure_no_migration_conflicts(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let filename = path
        .file_name()
        .ok_or_else(|| GraphError::Io("private state path has no filename".to_owned()))?;
    let prefix = format!(
        "{}{MIGRATION_CONFLICT_SEPARATOR}",
        filename.to_string_lossy()
    );
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "list private state migration conflicts: {error}"
            )))
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            GraphError::Io(format!("list private state migration conflicts: {error}"))
        })?;
        let filename = entry.file_name();
        if filename
            .to_string_lossy()
            .strip_prefix(&prefix)
            .is_some_and(is_generation)
        {
            return Err(GraphError::Guard(format!(
                "unresolved private state migration conflict: {}",
                entry.path().display()
            )));
        }
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
    Ok(())
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
        return Ok(false);
    }

    if std::fs::symlink_metadata(path.join("objects"))
        .is_ok_and(|metadata| metadata.file_type().is_dir())
    {
        return Ok(true);
    }
    Ok(std::fs::symlink_metadata(path.join("commondir"))
        .is_ok_and(|metadata| metadata.file_type().is_file()))
}

fn git_context_key(git_dir: &Path) -> Result<String> {
    let head = git_dir.join("HEAD");
    let identity = read_git_control_line(&head, "Git HEAD")?
        .ok_or_else(|| GraphError::Guard("Git HEAD is missing".to_owned()))?;
    let context = if is_object_id(&identity) {
        format!("detached:{}", identity.to_ascii_lowercase())
    } else {
        let reference = identity
            .strip_prefix("ref:")
            .map(str::trim)
            .filter(|reference| valid_git_reference(reference))
            .ok_or_else(|| GraphError::Guard("invalid Git HEAD".to_owned()))?;
        match resolve_git_reference(git_dir, reference)? {
            Some(commit) => format!("ref:{reference}\ncommit:{commit}"),
            None => format!("ref:{reference}\nunborn"),
        }
    };
    Ok(generation(context.as_bytes()))
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
        let git_dir = create_git(root.path(), "ref: refs/heads/main\n");
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

        write_ref(
            &git_dir,
            "refs/heads/main",
            "1111111111111111111111111111111111111111",
        );
        let alpha_path = super::path_for_output(&output, &legacy).unwrap();
        super::write_state(
            &alpha_path,
            &super::serialize(&alpha, &public_generation, &semantic).unwrap(),
        )
        .unwrap();

        write_ref(
            &git_dir,
            "refs/heads/main",
            "2222222222222222222222222222222222222222",
        );
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

        write_ref(
            &git_dir,
            "refs/heads/main",
            "3333333333333333333333333333333333333333",
        );
        let beta_path = super::path_for_output(&output, &legacy).unwrap();
        super::write_state(
            &beta_path,
            &super::serialize(&beta, &public_generation, &semantic).unwrap(),
        )
        .unwrap();

        write_ref(
            &git_dir,
            "refs/heads/main",
            "4444444444444444444444444444444444444444",
        );
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
        let conflict = super::migration_conflict_path(&current, b"new private lineage").unwrap();
        assert_eq!(
            fs::read_to_string(&conflict).unwrap(),
            "new private lineage"
        );
        let retry = super::migrate(&legacy, &current, "test private state").unwrap_err();
        assert_eq!(retry.kind(), "guard");
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
