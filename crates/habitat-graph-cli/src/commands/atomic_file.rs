//! Durable same-directory replacement for public artifacts and owner-only private state.
//!
//! Writes use a newly created temporary file, flush it, atomically rename it over the destination,
//! and sync the containing directory. Failed writes remove their temporary file when possible.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use habitat_graph_core::{GraphError, Result};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) const PRIVATE_TEMP_SEPARATOR: &str = ".hgtp.";

pub(super) const PUBLIC_TEMP_PREFIX: &str = ".habitat-graph.tmp.";

fn is_temporary_nonce(nonce: &str) -> bool {
    let mut parts = nonce.split('.');
    let values = [parts.next(), parts.next(), parts.next()];
    values.into_iter().all(|part| {
        part.is_some_and(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase())
        })
    }) && parts.next().is_none()
}

pub(super) fn is_private_temporary_suffix(suffix: &str) -> bool {
    suffix
        .strip_prefix(PRIVATE_TEMP_SEPARATOR)
        .is_some_and(is_temporary_nonce)
}

pub(super) fn is_public_temporary_name(name: &str) -> bool {
    name.strip_prefix(PUBLIC_TEMP_PREFIX)
        .is_some_and(is_temporary_nonce)
}

fn radix36(mut value: u64) -> String {
    if value == 0 {
        return "0".to_owned();
    }
    let mut encoded = Vec::new();
    while value != 0 {
        let digit = u8::try_from(value % 36).expect("base-36 digit fits in u8");
        encoded.push(if digit < 10 {
            b'0' + digit
        } else {
            b'a' + digit - 10
        });
        value /= 36;
    }
    encoded.reverse();
    String::from_utf8(encoded).expect("base-36 encoding is ASCII")
}

/// Atomically replaces `path` with `bytes`, optionally creating the temporary file owner-only.
///
/// # Errors
///
/// Returns [`GraphError::Guard`] when `owner_only` is requested on a platform without Unix file
/// permissions — the request fails closed instead of silently writing with default ACLs.
/// Returns [`GraphError::Io`] when the temporary file cannot be created, written, synchronized,
/// renamed, or made durable through a parent-directory sync.
pub(super) fn write(path: &Path, bytes: &[u8], owner_only: bool, context: &str) -> Result<()> {
    #[cfg(not(unix))]
    if owner_only {
        return Err(GraphError::Guard(format!(
            "{context}: owner-only writes require Unix file permissions"
        )));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temporary = temporary_path(path, owner_only, context)?;

    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(if owner_only { 0o600 } else { 0o666 });

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

pub(super) fn temporary_path(path: &Path, owner_only: bool, context: &str) -> Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let filename = path
        .file_name()
        .ok_or_else(|| GraphError::Io(format!("{context} path has no filename")))?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_nanos())
                .unwrap_or_else(|_| duration.as_secs() ^ u64::from(duration.subsec_nanos()))
        });
    let nonce = format!(
        "{}.{}.{}",
        radix36(u64::from(std::process::id())),
        radix36(timestamp),
        radix36(sequence)
    );
    debug_assert!(is_private_temporary_suffix(&format!(
        "{PRIVATE_TEMP_SEPARATOR}{nonce}"
    )));
    let temporary_name = if owner_only {
        let mut name = OsString::from(filename);
        name.push(PRIVATE_TEMP_SEPARATOR);
        name.push(nonce);
        name
    } else {
        let name = format!("{PUBLIC_TEMP_PREFIX}{nonce}");
        debug_assert!(is_public_temporary_name(&name));
        OsString::from(name)
    };
    Ok(parent.join(temporary_name))
}

/// Removes `path` if present and synchronizes its containing directory.
///
/// # Errors
///
/// Returns [`GraphError::Io`] when removal or the parent-directory sync fails.
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
pub(super) fn sync_directory(directory: &Path, context: &str) -> Result<()> {
    std::fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|error| GraphError::Io(format!("{context} directory sync: {error}")))
}

#[cfg(not(unix))]
pub(super) fn sync_directory(_directory: &Path, _context: &str) -> Result<()> {
    Ok(())
}
