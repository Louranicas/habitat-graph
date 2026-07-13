//! File detection: collect source files under a root, filtered by extension, honoring `.gitignore`.
//!
//! Entry point: [`detect`].

use std::io::{BufRead as _, Read as _};
use std::path::{Path, PathBuf};

use habitat_graph_core::{GraphError, Result};

const MAX_GIT_MARKER_LINE_BYTES: u64 = 4096;

/// Collects files under `root` whose extension is in `extensions` (case-insensitive), honoring
/// `.gitignore`.  Results are returned in a deterministic (sorted) order.
///
/// `extensions` must be provided **without** a leading dot (e.g. `"rs"` not `".rs"`).
/// Matching is case-insensitive: a file with extension `".RS"` matches the key `"rs"`, and a key
/// of `"MD"` matches a file named `"readme.md"`.
///
/// Roots within a Git worktree use the standard gitignore sources, including nested `.gitignore`
/// files, `.ignore` files, global excludes, and `.git/info/exclude`. Roots without Git metadata in
/// their ancestor chain honor only ignore files within the scan root, so staged trees do not depend
/// on ambient parent or user configuration.
///
/// # Errors
///
/// Returns [`GraphError::Io`] wrapping the underlying walk diagnostic if `root` cannot be
/// traversed (e.g. the path does not exist or permission is denied).
pub fn detect(root: &Path, extensions: &[&str]) -> Result<Vec<PathBuf>> {
    // Lower-case every caller-supplied extension key once, outside the per-entry loop.
    let lowered: Vec<String> = extensions.iter().map(|e| e.to_lowercase()).collect();
    let canonical_root = std::fs::canonicalize(root).map_err(|error| {
        GraphError::Io(format!(
            "resolve detection root {}: {error}",
            root.display()
        ))
    })?;

    let mut paths: Vec<PathBuf> = Vec::new();
    let mut walker = ignore::WalkBuilder::new(root);
    match git_ancestor(&canonical_root) {
        None => {
            walker
                .require_git(false)
                .parents(false)
                .git_global(false)
                .git_exclude(false);
        }
        Some(metadata) => {
            walker.current_dir(metadata.root);
            if let Some(exclude) = metadata.manual_exclude {
                walker.git_exclude(false);
                if exclude.is_file() {
                    if let Some(error) = walker.add_ignore(&exclude) {
                        return Err(GraphError::Io(format!(
                            "load Git exclude {}: {error}",
                            exclude.display()
                        )));
                    }
                }
            }
        }
    }

    for entry in walker.build() {
        let entry = entry.map_err(|e| GraphError::Io(e.to_string()))?;

        // Skip directories, symlinks-to-directories, and special files.
        if entry.file_type().is_some_and(|ft| ft.is_file()) {
            let path = entry.path();
            if let Some(ext) = path.extension() {
                let ext_lower = ext.to_string_lossy().to_lowercase();
                if lowered.iter().any(|key| key == &ext_lower) {
                    paths.push(path.to_path_buf());
                }
            }
        }
    }

    paths.sort();
    Ok(paths)
}

struct GitMetadata {
    root: PathBuf,
    manual_exclude: Option<PathBuf>,
}

fn git_ancestor(root: &Path) -> Option<GitMetadata> {
    root.ancestors()
        .find_map(|ancestor| git_metadata(&ancestor.join(".git")))
}

fn git_metadata(path: &Path) -> Option<GitMetadata> {
    if path.is_dir() {
        path.join("HEAD").is_file().then_some(GitMetadata {
            root: path.parent().unwrap_or_else(|| Path::new("")).to_path_buf(),
            manual_exclude: None,
        })
    } else if path.is_file() {
        let marker = gitdir_marker(path)?;
        let gitdir = if marker.path.is_absolute() {
            marker.path
        } else {
            path.parent()
                .unwrap_or_else(|| Path::new(""))
                .join(marker.path)
        };
        if !gitdir.is_dir() || !gitdir.join("HEAD").is_file() {
            return None;
        }
        let common_dir = git_common_dir(&gitdir)?;
        Some(GitMetadata {
            root: path.parent().unwrap_or_else(|| Path::new("")).to_path_buf(),
            manual_exclude: (marker.non_utf8 || common_dir.non_utf8)
                .then(|| common_dir.path.join("info/exclude")),
        })
    } else {
        None
    }
}

fn git_common_dir(gitdir: &Path) -> Option<NativePath> {
    let marker = gitdir.join("commondir");
    if !marker.exists() {
        return Some(NativePath {
            path: gitdir.to_path_buf(),
            non_utf8: false,
        });
    }
    let common = native_path_line(&marker)?;
    Some(NativePath {
        path: if common.path.is_absolute() {
            common.path
        } else {
            gitdir.join(common.path)
        },
        non_utf8: common.non_utf8,
    })
}

fn gitdir_marker(path: &Path) -> Option<NativePath> {
    let line = bounded_first_line(path)?;
    let gitdir = trim_ascii_whitespace(line.strip_prefix(b"gitdir:")?);
    if gitdir.is_empty() {
        return None;
    }
    native_path_from_bytes(gitdir)
}

fn native_path_line(path: &Path) -> Option<NativePath> {
    let line = bounded_first_line(path)?;
    if line.is_empty() {
        None
    } else {
        native_path_from_bytes(&line)
    }
}

fn bounded_first_line(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = Vec::new();
    let mut reader = std::io::BufReader::new(file).take(MAX_GIT_MARKER_LINE_BYTES + 1);
    reader.read_until(b'\n', &mut line).ok()?;
    if line.is_empty() || line.len() as u64 > MAX_GIT_MARKER_LINE_BYTES {
        return None;
    }
    Some(trim_ascii_whitespace(&line).to_vec())
}

fn trim_ascii_whitespace(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(unix)]
fn native_path_from_bytes(bytes: &[u8]) -> Option<NativePath> {
    use std::os::unix::ffi::OsStringExt as _;

    if bytes.contains(&0) {
        None
    } else {
        Some(NativePath {
            path: std::ffi::OsString::from_vec(bytes.to_vec()).into(),
            non_utf8: std::str::from_utf8(bytes).is_err(),
        })
    }
}

#[cfg(not(unix))]
fn native_path_from_bytes(bytes: &[u8]) -> Option<NativePath> {
    std::str::from_utf8(bytes).ok().map(|path| NativePath {
        path: PathBuf::from(path),
        non_utf8: false,
    })
}

struct NativePath {
    path: PathBuf,
    non_utf8: bool,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    use super::detect;
    use habitat_graph_core::GraphError;

    // ── helpers ────────────────────────────────────────────────────────────────

    /// Create `rel` (and any missing parent directories) inside `dir`, then return the
    /// absolute path.  The file is created with empty content so every test starts from
    /// a known state.
    fn mkfile(dir: &TempDir, rel: &str) -> PathBuf {
        let path = dir.path().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, b"").unwrap();
        path
    }

    /// Write a `.gitignore` at the root of `dir`.
    fn mkgitignore(dir: &TempDir, contents: &str) {
        fs::write(dir.path().join(".gitignore"), contents).unwrap();
    }

    /// Write a `.gitignore` at an arbitrary `subpath` relative to `dir`.
    fn mkgitignore_at(dir: &TempDir, subpath: &str, contents: &str) {
        let p = dir.path().join(subpath);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, contents).unwrap();
    }

    /// Collect just the file-names (not full paths) from a detect result.
    fn filenames(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    // ── basic correctness ──────────────────────────────────────────────────────

    #[test]
    fn empty_dir_returns_empty() {
        let dir = TempDir::new().unwrap();
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn single_matching_file_returned() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "main.rs");
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].to_string_lossy().ends_with("main.rs"));
    }

    #[test]
    fn multiple_matching_files_all_returned() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "a.rs");
        mkfile(&dir, "b.rs");
        mkfile(&dir, "c.rs");
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn non_matching_extension_excluded() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "main.rs");
        mkfile(&dir, "readme.md");
        mkfile(&dir, "data.json");
        let got = detect(dir.path(), &["rs"]).unwrap();
        let names = filenames(&got);
        assert!(names.contains(&"main.rs".to_owned()));
        assert!(!names.contains(&"readme.md".to_owned()));
        assert!(!names.contains(&"data.json".to_owned()));
    }

    #[test]
    fn multiple_extensions_all_kept() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "main.rs");
        mkfile(&dir, "readme.md");
        mkfile(&dir, "ignored.json");
        let got = detect(dir.path(), &["rs", "md"]).unwrap();
        let names = filenames(&got);
        assert!(names.contains(&"main.rs".to_owned()));
        assert!(names.contains(&"readme.md".to_owned()));
        assert!(!names.contains(&"ignored.json".to_owned()));
    }

    #[test]
    fn empty_extensions_list_returns_empty() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "main.rs");
        let got = detect(dir.path(), &[]).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn file_without_extension_excluded() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "Makefile");
        mkfile(&dir, "main.rs");
        let got = detect(dir.path(), &["rs"]).unwrap();
        let names = filenames(&got);
        assert!(!names.contains(&"Makefile".to_owned()));
        assert!(names.contains(&"main.rs".to_owned()));
    }

    // ── case-insensitivity ────────────────────────────────────────────────────

    #[test]
    fn uppercase_file_extension_matches_lowercase_key() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "main.RS");
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(got.len(), 1, "uppercase .RS must match key 'rs'");
    }

    #[test]
    fn lowercase_file_extension_matches_uppercase_key() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "main.rs");
        let got = detect(dir.path(), &["RS"]).unwrap();
        assert_eq!(got.len(), 1, "lowercase .rs must match key 'RS'");
    }

    #[test]
    fn mixed_case_file_extension_matches() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "main.Rs");
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(got.len(), 1, "mixed-case .Rs must match key 'rs'");
    }

    #[test]
    fn mixed_case_key_matches_lowercase_file() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "readme.md");
        let got = detect(dir.path(), &["MD"]).unwrap();
        assert_eq!(got.len(), 1, "key 'MD' must match file 'readme.md'");
    }

    #[test]
    fn both_key_and_extension_uppercase() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "readme.MD");
        let got = detect(dir.path(), &["MD"]).unwrap();
        assert_eq!(got.len(), 1, "key 'MD' must match file 'readme.MD'");
    }

    #[test]
    fn mixed_case_files_and_keys_all_matched() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "a.RS");
        mkfile(&dir, "b.rs");
        mkfile(&dir, "c.Rs");
        mkfile(&dir, "d.rS");
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(got.len(), 4, "all four case variants must be matched");
    }

    #[test]
    fn case_insensitivity_does_not_match_wrong_extension() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "script.PY");
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert!(got.is_empty(), ".PY must not match key 'rs'");
    }

    // ── nested subdirectories ─────────────────────────────────────────────────

    #[test]
    fn one_level_subdir_included() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "root.rs");
        mkfile(&dir, "sub/child.rs");
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn two_level_nesting_included() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "a/b/deep.rs");
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].to_string_lossy().ends_with("deep.rs"));
    }

    #[test]
    fn three_level_nesting_included() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "a/b/c/very_deep.rs");
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn mix_of_root_and_nested_files() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "root.rs");
        mkfile(&dir, "src/lib.rs");
        mkfile(&dir, "src/bin/main.rs");
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn nested_non_matching_files_excluded() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "src/lib.rs");
        mkfile(&dir, "src/lib.py");
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(got.len(), 1);
    }

    // ── gitignore ─────────────────────────────────────────────────────────────

    #[test]
    fn gitignored_file_at_root_excluded() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "keep.rs");
        mkfile(&dir, "ignored.rs");
        mkgitignore(&dir, "ignored.rs\n");
        let got = detect(dir.path(), &["rs"]).unwrap();
        let names = filenames(&got);
        assert!(names.contains(&"keep.rs".to_owned()));
        assert!(!names.contains(&"ignored.rs".to_owned()));
    }

    #[test]
    fn gitignored_directory_all_contents_excluded() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "keep.rs");
        mkfile(&dir, "target/debug/build.rs");
        mkgitignore(&dir, "target/\n");
        let got = detect(dir.path(), &["rs"]).unwrap();
        let names = filenames(&got);
        assert!(names.contains(&"keep.rs".to_owned()));
        assert!(!names.contains(&"build.rs".to_owned()));
    }

    #[test]
    fn gitignore_glob_pattern_excludes() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "lib.rs");
        mkfile(&dir, "generated_schema.rs");
        mkgitignore(&dir, "generated_*.rs\n");
        let got = detect(dir.path(), &["rs"]).unwrap();
        let names = filenames(&got);
        assert!(names.contains(&"lib.rs".to_owned()));
        assert!(!names.contains(&"generated_schema.rs".to_owned()));
    }

    #[test]
    fn nested_gitignore_respected() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "src/lib.rs");
        mkfile(&dir, "src/gen/generated.rs");
        mkgitignore_at(&dir, "src/gen/.gitignore", "generated.rs\n");
        let got = detect(dir.path(), &["rs"]).unwrap();
        let names = filenames(&got);
        assert!(names.contains(&"lib.rs".to_owned()));
        assert!(!names.contains(&"generated.rs".to_owned()));
    }

    #[test]
    fn non_git_scan_does_not_inherit_parent_gitignore() {
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("staged");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("visible.rs"), b"").unwrap();
        fs::write(parent.path().join(".gitignore"), "visible.rs\n").unwrap();

        let got = detect(&root, &["rs"]).unwrap();
        assert_eq!(filenames(&got), vec!["visible.rs"]);
    }

    #[test]
    fn lexical_repository_ancestor_does_not_change_non_git_scan() {
        let sandbox = TempDir::new().unwrap();
        let repository = sandbox.path().join("repository");
        fs::create_dir_all(repository.join(".git")).unwrap();
        fs::write(repository.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::create_dir(repository.join("pivot")).unwrap();
        let staged = sandbox.path().join("staged");
        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("visible.rs"), b"").unwrap();
        fs::write(sandbox.path().join(".ignore"), "visible.rs\n").unwrap();

        let lexical_root = repository.join("pivot/../../staged");
        let got = detect(&lexical_root, &["rs"]).unwrap();

        assert_eq!(filenames(&got), vec!["visible.rs"]);
    }

    #[test]
    fn git_scan_still_inherits_repository_gitignore() {
        let repository = TempDir::new().unwrap();
        fs::create_dir_all(repository.path().join(".git")).unwrap();
        fs::write(
            repository.path().join(".git/HEAD"),
            "ref: refs/heads/main\n",
        )
        .unwrap();
        fs::create_dir_all(repository.path().join("src")).unwrap();
        fs::write(repository.path().join("src/ignored.rs"), b"").unwrap();
        fs::write(repository.path().join(".gitignore"), "ignored.rs\n").unwrap();

        let got = detect(repository.path(), &["rs"]).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn git_subdirectory_scan_inherits_repository_gitignore() {
        let repository = TempDir::new().unwrap();
        fs::create_dir_all(repository.path().join(".git")).unwrap();
        fs::write(
            repository.path().join(".git/HEAD"),
            "ref: refs/heads/main\n",
        )
        .unwrap();
        fs::create_dir_all(repository.path().join("src")).unwrap();
        fs::write(repository.path().join("src/ignored.rs"), b"").unwrap();
        fs::write(repository.path().join(".gitignore"), "ignored.rs\n").unwrap();

        let got = detect(&repository.path().join("src"), &["rs"]).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn stale_worktree_pointer_does_not_enable_parent_ignores() {
        let repository = TempDir::new().unwrap();
        fs::write(repository.path().join(".git"), "gitdir: .missing-gitdir\n").unwrap();
        fs::create_dir_all(repository.path().join("src")).unwrap();
        fs::write(repository.path().join("src/ignored.rs"), b"").unwrap();
        fs::write(repository.path().join(".gitignore"), "ignored.rs\n").unwrap();

        let got = detect(&repository.path().join("src"), &["rs"]).unwrap();
        assert_eq!(filenames(&got), vec!["ignored.rs"]);
    }

    #[test]
    fn oversized_worktree_pointer_is_not_git_metadata() {
        let repository = TempDir::new().unwrap();
        fs::write(
            repository.path().join(".git"),
            vec![b'x'; usize::try_from(super::MAX_GIT_MARKER_LINE_BYTES).unwrap() + 1],
        )
        .unwrap();
        fs::create_dir_all(repository.path().join("src")).unwrap();
        fs::write(repository.path().join("src/ignored.rs"), b"").unwrap();
        fs::write(repository.path().join(".gitignore"), "ignored.rs\n").unwrap();

        let got = detect(&repository.path().join("src"), &["rs"]).unwrap();
        assert_eq!(filenames(&got), vec!["ignored.rs"]);
    }

    #[test]
    fn worktree_subdirectory_scan_inherits_repository_gitignore() {
        let repository = TempDir::new().unwrap();
        fs::create_dir_all(repository.path().join(".git-data")).unwrap();
        fs::write(
            repository.path().join(".git-data/HEAD"),
            "ref: refs/heads/main\n",
        )
        .unwrap();
        fs::write(repository.path().join(".git"), "gitdir: .git-data\n").unwrap();
        fs::create_dir_all(repository.path().join("src")).unwrap();
        fs::write(repository.path().join("src/ignored.rs"), b"").unwrap();
        fs::write(repository.path().join(".gitignore"), "ignored.rs\n").unwrap();

        let got = detect(&repository.path().join("src"), &["rs"]).unwrap();
        assert!(got.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_worktree_pointer_enables_repository_ignores() {
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

        let repository = TempDir::new().unwrap();
        let gitdir_name = std::ffi::OsString::from_vec(b".git-data-\xff".to_vec());
        let common_dir = repository.path().join(&gitdir_name);
        let gitdir = common_dir.join("worktrees/linked");
        fs::create_dir_all(common_dir.join("info")).unwrap();
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(gitdir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        fs::write(common_dir.join("info/exclude"), "/src/excluded.rs\n").unwrap();
        let mut marker = b"gitdir: ".to_vec();
        marker.extend_from_slice(gitdir_name.as_os_str().as_bytes());
        marker.extend_from_slice(b"/worktrees/linked");
        marker.push(b'\n');
        fs::write(repository.path().join(".git"), marker).unwrap();
        fs::create_dir(repository.path().join("src")).unwrap();
        fs::write(repository.path().join("src/ignored.rs"), b"").unwrap();
        fs::write(repository.path().join("src/excluded.rs"), b"").unwrap();
        fs::write(repository.path().join("src/kept.rs"), b"").unwrap();
        fs::write(repository.path().join(".gitignore"), "ignored.rs\n").unwrap();

        let got = detect(&repository.path().join("src"), &["rs"]).unwrap();

        assert_eq!(filenames(&got), vec!["kept.rs"]);
    }

    #[test]
    fn multiple_gitignored_files_all_excluded() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "keep.rs");
        mkfile(&dir, "drop_a.rs");
        mkfile(&dir, "drop_b.rs");
        mkgitignore(&dir, "drop_a.rs\ndrop_b.rs\n");
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(got.len(), 1);
        assert!(filenames(&got).contains(&"keep.rs".to_owned()));
    }

    #[test]
    fn no_gitignore_means_all_returned() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "a.rs");
        mkfile(&dir, "b.rs");
        // No .gitignore created
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(got.len(), 2);
    }

    // ── sorting ───────────────────────────────────────────────────────────────

    #[test]
    fn output_is_sorted_ascending() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "z.rs");
        mkfile(&dir, "a.rs");
        mkfile(&dir, "m.rs");
        let got = detect(dir.path(), &["rs"]).unwrap();
        let mut expected = got.clone();
        expected.sort();
        assert_eq!(got, expected, "detect output must be in sorted order");
    }

    #[test]
    fn output_sorted_across_subdirs() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "z/z.rs");
        mkfile(&dir, "a/a.rs");
        mkfile(&dir, "m/m.rs");
        let got = detect(dir.path(), &["rs"]).unwrap();
        let mut expected = got.clone();
        expected.sort();
        assert_eq!(got, expected, "cross-directory order must be sorted");
    }

    #[test]
    fn sort_is_deterministic_across_calls() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "b.rs");
        mkfile(&dir, "a.rs");
        mkfile(&dir, "c.rs");
        let first = detect(dir.path(), &["rs"]).unwrap();
        let second = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(first, second, "repeated calls must return identical order");
    }

    #[test]
    fn sort_is_total_all_paths_distinct() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "x/a.rs");
        mkfile(&dir, "y/a.rs");
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(got.len(), 2);
        assert_ne!(got[0], got[1], "each path must be distinct");
    }

    // ── edge cases ────────────────────────────────────────────────────────────

    #[test]
    fn double_extension_last_segment_matched() {
        // Rust's Path::extension() returns the part after the LAST dot.
        // So "archive.tar.gz" has extension "gz".
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "archive.tar.gz");
        mkfile(&dir, "archive.tar.bz2");
        let got_gz = detect(dir.path(), &["gz"]).unwrap();
        assert_eq!(got_gz.len(), 1);
        assert!(got_gz[0].to_string_lossy().ends_with("archive.tar.gz"));

        let got_bz2 = detect(dir.path(), &["bz2"]).unwrap();
        assert_eq!(got_bz2.len(), 1);
    }

    #[test]
    fn hidden_file_excluded_by_default() {
        // ignore::WalkBuilder defaults to hidden(true): dotfiles are skipped. Assert that contract
        // explicitly — a hidden ".hidden.rs" is NOT returned, while a visible sibling is.
        let dir = TempDir::new().unwrap();
        mkfile(&dir, ".hidden.rs");
        mkfile(&dir, "visible.rs");
        let names = filenames(&detect(dir.path(), &["rs"]).unwrap());
        assert!(names.contains(&"visible.rs".to_owned()));
        assert!(
            !names.contains(&".hidden.rs".to_owned()),
            "hidden dotfiles must be excluded by default"
        );
    }

    #[test]
    fn duplicate_extension_key_no_double_counting() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "main.rs");
        // Provide "rs" twice in the list — must not return main.rs twice.
        let got = detect(dir.path(), &["rs", "rs"]).unwrap();
        assert_eq!(
            got.len(),
            1,
            "duplicate extension keys must not cause duplicates in output"
        );
    }

    #[test]
    fn only_directories_no_files_returns_empty() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("src/lib")).unwrap();
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn three_extension_types_all_found() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "a.rs");
        mkfile(&dir, "b.md");
        mkfile(&dir, "c.toml");
        mkfile(&dir, "d.py");
        let got = detect(dir.path(), &["rs", "md", "toml"]).unwrap();
        assert_eq!(got.len(), 3);
        let names = filenames(&got);
        assert!(names.contains(&"a.rs".to_owned()));
        assert!(names.contains(&"b.md".to_owned()));
        assert!(names.contains(&"c.toml".to_owned()));
        assert!(!names.contains(&"d.py".to_owned()));
    }

    #[test]
    fn files_same_name_different_dirs_both_returned() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "pkg_a/lib.rs");
        mkfile(&dir, "pkg_b/lib.rs");
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn single_char_extension() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "archive.c");
        mkfile(&dir, "archive.h");
        let got = detect(dir.path(), &["c"]).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].to_string_lossy().ends_with("archive.c"));
    }

    #[test]
    fn numeric_extension_matched() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "page.1");
        mkfile(&dir, "page.2");
        let got = detect(dir.path(), &["1"]).unwrap();
        assert_eq!(got.len(), 1);
    }

    // ── error handling ────────────────────────────────────────────────────────

    #[test]
    fn nonexistent_root_returns_io_error() {
        let path = std::path::Path::new("/nonexistent_habitat_graph_test_root_12345678");
        let result = detect(path, &["rs"]);
        assert!(result.is_err(), "nonexistent root must produce an error");
    }

    #[test]
    fn error_is_grapherror_io_variant() {
        let path = std::path::Path::new("/nonexistent_habitat_graph_test_root_12345678");
        let err = detect(path, &["rs"]).unwrap_err();
        assert!(
            matches!(err, GraphError::Io(_)),
            "walk error must map to GraphError::Io, got: {err:?}"
        );
    }

    #[test]
    fn io_error_message_is_non_empty() {
        let path = std::path::Path::new("/nonexistent_habitat_graph_test_root_12345678");
        let err = detect(path, &["rs"]).unwrap_err();
        if let GraphError::Io(msg) = err {
            assert!(!msg.is_empty(), "Io error message must carry context");
        }
    }

    // ── returned paths ────────────────────────────────────────────────────────

    #[test]
    fn returned_paths_are_absolute() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "main.rs");
        let got = detect(dir.path(), &["rs"]).unwrap();
        for p in &got {
            assert!(p.is_absolute(), "path must be absolute: {p:?}");
        }
    }

    #[test]
    fn returned_paths_exist_on_disk() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "main.rs");
        let got = detect(dir.path(), &["rs"]).unwrap();
        for p in &got {
            assert!(p.exists(), "returned path must exist: {p:?}");
        }
    }

    #[test]
    fn returned_paths_are_regular_files() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "main.rs");
        let got = detect(dir.path(), &["rs"]).unwrap();
        for p in &got {
            assert!(p.is_file(), "returned path must be a regular file: {p:?}");
        }
    }

    #[test]
    fn all_returned_paths_under_root() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "src/main.rs");
        mkfile(&dir, "tests/test.rs");
        let root = dir.path();
        let got = detect(root, &["rs"]).unwrap();
        for p in &got {
            assert!(
                p.starts_with(root),
                "every path must be under root; got {p:?}"
            );
        }
    }

    // ── ok-vs-error ───────────────────────────────────────────────────────────

    #[test]
    fn returns_ok_on_valid_root() {
        let dir = TempDir::new().unwrap();
        assert!(detect(dir.path(), &["rs"]).is_ok());
    }

    #[test]
    fn returns_ok_when_no_files_match() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "data.json");
        assert!(detect(dir.path(), &["rs"]).is_ok());
    }

    #[test]
    fn count_matches_number_of_eligible_files() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "a.rs");
        mkfile(&dir, "b.rs");
        mkfile(&dir, "c.py");
        mkfile(&dir, "d.md");
        let got = detect(dir.path(), &["rs", "md"]).unwrap();
        assert_eq!(got.len(), 3, "3 eligible files: a.rs, b.rs, d.md");
    }

    // ── gitignore with multiple rules ─────────────────────────────────────────

    #[test]
    fn gitignore_multiple_patterns_each_applied() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "keep.rs");
        mkfile(&dir, "generated.rs");
        mkfile(&dir, "debug.rs");
        mkgitignore(&dir, "generated.rs\ndebug.rs\n");
        let got = detect(dir.path(), &["rs"]).unwrap();
        assert_eq!(got.len(), 1);
        assert!(filenames(&got).contains(&"keep.rs".to_owned()));
    }

    #[test]
    fn gitignored_extension_pattern_excluded() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "keep.rs");
        mkfile(&dir, "skip.generated.rs");
        mkgitignore(&dir, "*.generated.rs\n");
        let got = detect(dir.path(), &["rs"]).unwrap();
        let names = filenames(&got);
        assert!(names.contains(&"keep.rs".to_owned()));
        assert!(
            !names.contains(&"skip.generated.rs".to_owned()),
            "a file matching a .gitignore glob must be excluded"
        );
    }

    #[test]
    fn gitignore_subdir_content_partially_excluded() {
        let dir = TempDir::new().unwrap();
        mkfile(&dir, "src/keep.rs");
        mkfile(&dir, "src/skip.rs");
        mkgitignore_at(&dir, "src/.gitignore", "skip.rs\n");
        let got = detect(dir.path(), &["rs"]).unwrap();
        let names = filenames(&got);
        assert!(names.contains(&"keep.rs".to_owned()));
        assert!(!names.contains(&"skip.rs".to_owned()));
    }
}
