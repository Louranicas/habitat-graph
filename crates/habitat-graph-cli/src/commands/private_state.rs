use std::io::Read as _;
use std::path::{Path, PathBuf};

use habitat_graph_core::{Graph, GraphError, Result};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

const PRIVATE_STATE_METADATA: &str = "_habitat_graph_private_state";
const PRIVATE_STATE_SCHEMA: &str = "habitat-graph.private-state.v1";

pub(super) struct StoredGraph {
    pub(super) graph: Graph,
    pub(super) public_generation: Option<String>,
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

pub(super) fn serialize(graph: &Graph, public_generation: &str) -> Result<Vec<u8>> {
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
        }),
    );
    serde_json::to_vec_pretty(&value)
        .map_err(|error| GraphError::Schema(format!("private state serialize: {error}")))
}

pub(super) fn parse(text: &str) -> Result<StoredGraph> {
    let graph = Graph::from_json(text)?;
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| GraphError::Schema(format!("private state parse: {error}")))?;
    let public_generation = match value.get(PRIVATE_STATE_METADATA) {
        None => None,
        Some(metadata) => {
            if metadata["schema"] != PRIVATE_STATE_SCHEMA {
                return Err(GraphError::Schema(format!(
                    "unsupported private state metadata schema: {:?}",
                    metadata["schema"]
                )));
            }
            let generation = metadata["public_generation"].as_str().ok_or_else(|| {
                GraphError::Schema("private state `public_generation` must be a string".to_owned())
            })?;
            if generation.len() != 64
                || !generation
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(GraphError::Schema(
                    "private state `public_generation` is invalid".to_owned(),
                ));
            }
            Some(generation.to_owned())
        }
    };
    Ok(StoredGraph {
        graph,
        public_generation,
    })
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
        let bytes = super::serialize(&graph, &generation).unwrap();
        let stored = super::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();

        assert_eq!(stored.graph, graph);
        assert_eq!(
            stored.public_generation.as_deref(),
            Some(generation.as_str())
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
