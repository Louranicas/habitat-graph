use std::ffi::OsString;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use habitat_graph_core::{Graph, GraphError, Result};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

const PRIVATE_STATE_METADATA: &str = "_habitat_graph_private_state";
const LEGACY_PRIVATE_STATE_SCHEMA: &str = "habitat-graph.private-state.v1";
const PRIVATE_STATE_SCHEMA: &str = "habitat-graph.private-state.v2";
const SNAPSHOT_SEPARATOR: &str = ".snapshot-";

#[derive(Clone, Eq, PartialEq)]
pub(super) struct StoredGraph {
    pub(super) graph: Graph,
    pub(super) public_generation: Option<String>,
    pub(super) public_semantic_generation: Option<String>,
}

pub(super) fn path_for_output(output: &Path, legacy: &Path) -> Result<PathBuf> {
    #[cfg(not(unix))]
    remove(legacy, "unsupported legacy private state")?;

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
    let state_path = state_dir.join(format!("{key}.json"));
    migrate(legacy, &state_path, "legacy private state")?;
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
    if let Some(stored) = read(path, context)? {
        if matches_public(&stored, public_generation, public_semantic_generation) {
            return Ok(Some(stored));
        }
    }

    if let Some(generation) = public_generation {
        let snapshot = snapshot_path(path, generation)?;
        if let Some(stored) = read(&snapshot, context)? {
            if matches_public(&stored, public_generation, public_semantic_generation) {
                return Ok(Some(stored));
            }
        }
    }

    let Some(semantic_generation) = public_semantic_generation else {
        return Ok(None);
    };
    let mut candidates = snapshot_candidates(path)?;
    candidates.sort_unstable();
    let mut selected: Option<StoredGraph> = None;
    for candidate in candidates {
        let Some(stored) = read(&candidate, context)? else {
            continue;
        };
        if stored.public_semantic_generation.as_deref() != Some(semantic_generation) {
            continue;
        }
        if selected
            .as_ref()
            .is_some_and(|existing| existing.graph != stored.graph)
        {
            return Err(GraphError::Guard(format!(
                "multiple private state snapshots match the public graph: {}",
                path.display()
            )));
        }
        selected = Some(stored);
    }
    Ok(selected)
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
        if existing.public_generation != replacement.public_generation {
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
    write(path, bytes)
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

pub(super) fn migrate(legacy: &Path, current: &Path, context: &str) -> Result<()> {
    if legacy == current {
        return Ok(());
    }
    ensure(current)?;
    let metadata = match std::fs::symlink_metadata(legacy) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
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
    if !current.exists() {
        let bytes = read_regular(legacy, &metadata, context)?;
        write(current, &bytes)?;
    }
    remove(legacy, context)
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
        let marker_text = std::fs::read_to_string(&marker)
            .map_err(|error| GraphError::Io(format!("read Git metadata marker: {error}")))?;
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
    use std::fs;

    use habitat_graph_core::Graph;
    use tempfile::TempDir;

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

    #[cfg(not(unix))]
    #[test]
    fn unsupported_platform_removes_legacy_state_before_path_resolution() {
        let root = TempDir::new().unwrap();
        let output_dir = root.path().join("missing");
        let legacy = root.path().join(".habitat-graph-state.json");
        fs::write(&legacy, "raw state").unwrap();

        assert!(super::path_for_output(&output_dir.join("graph.json"), &legacy).is_err());
        assert!(!legacy.exists());
    }
}
