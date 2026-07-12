use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use habitat_graph_core::{GraphError, Result};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn write(path: &Path, bytes: &[u8], owner_only: bool, context: &str) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    path.file_name()
        .ok_or_else(|| GraphError::Io(format!("{context} path has no filename")))?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let temporary_name = format!(
        ".habitat-graph.tmp.{}.{timestamp}.{sequence}",
        std::process::id()
    );
    let temporary = parent.join(temporary_name);

    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(if owner_only { 0o600 } else { 0o666 });
    #[cfg(not(unix))]
    let _ = owner_only;

    let mut created = false;
    let write_result = (|| -> Result<()> {
        let mut file = options
            .open(&temporary)
            .map_err(|error| GraphError::Io(format!("{context} temp open: {error}")))?;
        created = true;
        file.write_all(bytes)
            .map_err(|error| GraphError::Io(format!("{context} temp write: {error}")))?;
        file.sync_all()
            .map_err(|error| GraphError::Io(format!("{context} temp sync: {error}")))?;
        drop(file);
        std::fs::rename(&temporary, path)
            .map_err(|error| GraphError::Io(format!("{context} atomic rename: {error}")))?;
        created = false;
        sync_directory(parent, context)
    })();

    if write_result.is_err() && created {
        if let Err(cleanup_error) = std::fs::remove_file(&temporary) {
            if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "warning: failed to remove {context} temporary file {}: {cleanup_error}",
                    temporary.display()
                );
            }
        }
    }
    write_result
}

pub(super) fn remove(path: &Path, context: &str) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            sync_directory(parent, context)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GraphError::Io(format!("{context} remove: {error}"))),
    }
}

#[cfg(unix)]
fn sync_directory(directory: &Path, context: &str) -> Result<()> {
    std::fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|error| GraphError::Io(format!("{context} directory sync: {error}")))
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path, _context: &str) -> Result<()> {
    Ok(())
}
