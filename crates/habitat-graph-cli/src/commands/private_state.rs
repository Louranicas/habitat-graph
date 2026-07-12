use std::path::Path;

use habitat_graph_core::{GraphError, Result};

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let filename = path
        .file_name()
        .ok_or_else(|| GraphError::Io("private state path has no filename".to_owned()))?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut temporary_name = OsString::from(".");
    temporary_name.push(filename);
    temporary_name.push(format!(".tmp.{}.{}", std::process::id(), sequence));
    let temporary = parent.join(temporary_name);

    let mut options = OpenOptions::new();
    options.create_new(true).write(true).mode(0o600);

    let write_result = (|| -> Result<()> {
        let mut file = options
            .open(&temporary)
            .map_err(|error| GraphError::Io(format!("private state temp open: {error}")))?;
        file.write_all(bytes)
            .map_err(|error| GraphError::Io(format!("private state temp write: {error}")))?;
        file.sync_all()
            .map_err(|error| GraphError::Io(format!("private state temp sync: {error}")))?;
        drop(file);
        std::fs::rename(&temporary, path)
            .map_err(|error| GraphError::Io(format!("private state atomic rename: {error}")))?;
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| GraphError::Io(format!("private state directory sync: {error}")))?;
        Ok(())
    })();

    if write_result.is_err() && temporary.exists() {
        if let Err(cleanup_error) = std::fs::remove_file(&temporary) {
            eprintln!(
                "warning: failed to remove private state temporary file {}: {cleanup_error}",
                temporary.display()
            );
        }
    }
    write_result
}

#[cfg(not(unix))]
pub(super) fn write(_path: &Path, _bytes: &[u8]) -> Result<()> {
    Err(GraphError::Guard(
        "private graph state requires owner-only file permissions".to_owned(),
    ))
}
