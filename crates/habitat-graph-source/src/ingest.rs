//! Ingest: read input bytes. P1 implements local file reads; remote HTTP is deferred (P6).

use std::path::Path;

use habitat_graph_core::{GraphError, Result};

/// Reads the bytes of a local file, capping at `max_bytes` (0 = unlimited).
///
/// When `max_bytes > 0` the file's size is checked via [`std::fs::metadata`] **before** any read
/// is attempted. Files whose length exceeds the cap are rejected without touching their bytes,
/// preventing accidental ingestion of arbitrarily large inputs.
///
/// # Errors
///
/// * [`GraphError::Io`] — the path does not exist, cannot be stat'd, or cannot be read (the
///   OS-level error message is included in the string).
/// * [`GraphError::Guard`] — `max_bytes > 0` **and** the file's byte length exceeds `max_bytes`.
///   The file is **not** read in this case.
pub fn read_local(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    if max_bytes > 0 {
        let meta = std::fs::metadata(path)
            .map_err(|e| GraphError::Io(format!("{}: {e}", path.display())))?;
        let file_len = meta.len();
        if file_len > max_bytes {
            return Err(GraphError::Guard(format!(
                "{} is {file_len} bytes, which exceeds the cap of {max_bytes} bytes",
                path.display()
            )));
        }
    }
    std::fs::read(path).map_err(|e| GraphError::Io(format!("{}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use tempfile::NamedTempFile;

    use super::*;

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Write `content` to a fresh named temp file and return the handle.
    fn write_temp(content: &[u8]) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("tempfile");
        f.write_all(content).expect("write");
        f.flush().expect("flush");
        f
    }

    // ── basic read ───────────────────────────────────────────────────────────

    #[test]
    fn existing_file_returns_exact_bytes() {
        let payload = b"hello, habitat-graph!";
        let f = write_temp(payload);
        let got = read_local(f.path(), 0).expect("should read");
        assert_eq!(got, payload);
    }

    #[test]
    fn empty_file_returns_empty_vec() {
        let f = write_temp(b"");
        let got = read_local(f.path(), 0).expect("empty read");
        assert!(got.is_empty(), "expected empty, got {got:?}");
    }

    #[test]
    fn binary_bytes_round_trip_exactly() {
        let payload: Vec<u8> = (0u8..=255).collect();
        let f = write_temp(&payload);
        let got = read_local(f.path(), 0).expect("binary read");
        assert_eq!(got, payload, "all 256 byte values must survive round-trip");
    }

    // ── missing file ─────────────────────────────────────────────────────────

    #[test]
    fn missing_file_yields_io_error() {
        let tmp = std::env::temp_dir().join("hg-ingest-nonexistent-file-xyz.bin");
        let err = read_local(&tmp, 0).expect_err("expected Io error");
        assert!(
            matches!(err, GraphError::Io(_)),
            "expected GraphError::Io, got {err:?}"
        );
    }

    #[test]
    fn missing_file_io_error_contains_path() {
        let tmp = std::env::temp_dir().join("hg-ingest-no-such-file-abc.bin");
        let err = read_local(&tmp, 0).expect_err("expected Io error");
        let msg = err.to_string();
        assert!(
            msg.contains("hg-ingest-no-such-file-abc"),
            "error should mention path: {msg}"
        );
    }

    #[test]
    fn missing_file_with_cap_yields_io_error() {
        // stat fails → Io, not Guard, because the file doesn't exist
        let tmp = std::env::temp_dir().join("hg-ingest-no-file-cap.bin");
        let err = read_local(&tmp, 1024).expect_err("expected Io error");
        assert!(
            matches!(err, GraphError::Io(_)),
            "expected GraphError::Io from failed stat, got {err:?}"
        );
    }

    // ── max_bytes = 0 (unlimited) ─────────────────────────────────────────────

    #[test]
    fn cap_zero_reads_any_size() {
        // 64 KiB of data; cap=0 must succeed
        let payload: Vec<u8> = (0u8..=255).cycle().take(65_536).collect();
        let f = write_temp(&payload);
        let got = read_local(f.path(), 0).expect("cap=0 should allow any size");
        assert_eq!(got.len(), 65_536);
        assert_eq!(got, payload);
    }

    #[test]
    fn cap_zero_with_empty_file_returns_empty() {
        let f = write_temp(b"");
        let got = read_local(f.path(), 0).expect("cap=0 empty");
        assert!(got.is_empty());
    }

    // ── size-cap enforcement ──────────────────────────────────────────────────

    #[test]
    fn file_larger_than_cap_yields_guard_error() {
        let f = write_temp(b"abcdefgh"); // 8 bytes
        let err = read_local(f.path(), 4).expect_err("expected Guard error");
        assert!(
            matches!(err, GraphError::Guard(_)),
            "expected GraphError::Guard, got {err:?}"
        );
    }

    #[test]
    fn guard_error_message_is_informative() {
        let f = write_temp(b"0123456789"); // 10 bytes
        let err = read_local(f.path(), 5).expect_err("guard expected");
        let msg = err.to_string();
        // Message must mention the cap and the actual size so the caller can act.
        assert!(msg.contains('5'), "cap should appear in message: {msg}");
        assert!(msg.contains("10"), "actual size should appear: {msg}");
    }

    #[test]
    fn file_exactly_at_cap_is_allowed() {
        let payload = b"exactly8"; // 8 bytes
        let f = write_temp(payload);
        let got = read_local(f.path(), 8).expect("len==cap should be allowed");
        assert_eq!(got, payload);
    }

    #[test]
    fn file_one_byte_over_cap_is_rejected() {
        let payload = b"nnn"; // 3 bytes
        let f = write_temp(payload);
        let err = read_local(f.path(), 2).expect_err("3 bytes > cap 2 → Guard");
        assert!(matches!(err, GraphError::Guard(_)));
    }

    #[test]
    fn empty_file_with_cap_one_is_allowed() {
        // 0 bytes ≤ cap 1 → Ok
        let f = write_temp(b"");
        let got = read_local(f.path(), 1).expect("empty file under cap");
        assert!(got.is_empty());
    }

    #[test]
    fn single_byte_file_at_cap_is_allowed() {
        let f = write_temp(b"X");
        let got = read_local(f.path(), 1).expect("1 byte == cap 1");
        assert_eq!(got, b"X");
    }

    #[test]
    fn two_byte_file_over_one_byte_cap_is_rejected() {
        // cap=0 must read any file; cap=1 must reject a 2-byte file
        let f = write_temp(b"X");
        let bytes = read_local(f.path(), 0).expect("cap=0 must succeed for 1-byte file");
        assert_eq!(bytes, b"X");

        let f2 = write_temp(b"XY"); // 2 bytes
        let guard = read_local(f2.path(), 1).expect_err("2 bytes > cap 1");
        assert!(matches!(guard, GraphError::Guard(_)));
    }

    // ── error kind tags ───────────────────────────────────────────────────────

    #[test]
    fn io_error_has_io_kind_tag() {
        let tmp = std::env::temp_dir().join("hg-ingest-kind-io-test.bin");
        let err = read_local(&tmp, 0).expect_err("Io");
        assert_eq!(err.kind(), "io");
    }

    #[test]
    fn guard_error_has_guard_kind_tag() {
        let f = write_temp(b"12345"); // 5 bytes
        let err = read_local(f.path(), 3).expect_err("Guard");
        assert_eq!(err.kind(), "guard");
    }

    // ── existing-but-unreadable path (judge finding: uncovered Io branch) ──────

    #[test]
    fn reading_a_directory_yields_io_error() {
        // An existing path that is not a readable file (here, a directory) must surface as a
        // GraphError::Io rather than panic — covering the std::fs::read failure branch on an
        // existing path (the same branch a permission-denied file would take).
        let dir = tempfile::tempdir().expect("tempdir");
        let err = read_local(dir.path(), 0).expect_err("reading a directory must fail");
        assert!(matches!(err, GraphError::Io(_)), "expected Io, got {err:?}");
        assert_eq!(err.kind(), "io");
    }
}
