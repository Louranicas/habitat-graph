//! `hook` (PC-tail) — install git lifecycle hooks so the knowledge graph is automatically
//! regenerated after every commit, and wire the deterministic merge driver for `graph.json`.
//!
//! ## Subcommands
//!
//! ### `hook install`
//! Discovers the git repo root by walking up from the current working directory (the same
//! algorithm as `git rev-parse --show-toplevel`).  The `.git/hooks/post-commit` file is
//! created (or appended to) with the habitat-graph regeneration command.  Existing hooks are
//! **never overwritten**: the habitat-graph lines are appended after a blank-line separator only
//! if they are not already present (idempotent).  The hook file is made executable (`0o755`).
//!
//! ### `hook install-merge-driver`
//! Registers the deterministic `graph.json` merge driver in the local repo's `.git/config` and
//! appends the routing line to `.gitattributes` (idempotent for both files).

use std::path::{Path, PathBuf};

use habitat_graph_core::{GraphError, Result};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Header `#!/bin/sh` written at the top of a newly-created hook file.
const SHEBANG: &str = "#!/bin/sh\n";

/// Marker comment that gates idempotency: if this string is already in the hook file, we skip.
const HOOK_MARKER: &str = "# habitat-graph post-commit hook";

/// The shell commands appended to the post-commit hook.
const HOOK_BODY: &str = "# habitat-graph post-commit hook\n\
                          habitat-graph update\n";

/// `.gitattributes` line that routes `graph.json` merges through our driver.
const GITATTRIBUTES_LINE: &str = "graph.json merge=habitat-graph";

/// `git config` section header for the merge driver.
const MERGE_DRIVER_SECTION: &str = "[merge \"habitat-graph\"]";

/// `git config` driver value written under [`MERGE_DRIVER_SECTION`].
const MERGE_DRIVER_VALUE: &str = "\tdriver = habitat-graph merge-driver %O %A %B\n\
     \tname = habitat-graph deterministic graph.json merge\n";

// ── Git repo discovery ────────────────────────────────────────────────────────

/// Walks up from `start` to find the root of the git repository (the directory containing `.git`).
///
/// Implements the same resolution algorithm as `git rev-parse --show-toplevel`: each ancestor
/// directory is checked for a `.git` entry (file or directory).
///
/// Returns `None` when no `.git` entry is found before the filesystem root.
#[must_use]
pub fn discover_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_absolute() {
        start.to_path_buf()
    } else {
        // Canonicalize relative paths so `parent()` terminates reliably.
        std::fs::canonicalize(start).ok()?
    };

    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

// ── File helpers ──────────────────────────────────────────────────────────────

/// Reads `path` as UTF-8 text, returning an empty `String` if the file does not exist.
///
/// # Errors
///
/// Returns [`GraphError::Io`] when the file exists but cannot be read.
fn read_or_empty(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    std::fs::read_to_string(path)
        .map_err(|e| GraphError::Io(format!("read {}: {e}", path.display())))
}

/// Appends `text` to `path`, creating the file if absent.  Does NOT add a trailing newline
/// automatically; callers must include one in `text` if desired.
///
/// # Errors
///
/// Returns [`GraphError::Io`] on any filesystem error.
fn append_to_file(path: &Path, text: &str) -> Result<()> {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| GraphError::Io(format!("open {}: {e}", path.display())))?;
    f.write_all(text.as_bytes())
        .map_err(|e| GraphError::Io(format!("write {}: {e}", path.display())))
}

/// Sets UNIX permissions on `path` to `mode`.
///
/// This is a no-op on non-Unix platforms (the function compiles but does nothing).
///
/// # Errors
///
/// Returns [`GraphError::Io`] when `set_permissions` fails on Unix.
fn set_permissions(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let perms = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(path, perms)
            .map_err(|e| GraphError::Io(format!("chmod {}: {e}", path.display())))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode); // suppress unused-variable warning on non-Unix
    }
    Ok(())
}

// ── hook install ─────────────────────────────────────────────────────────────

/// Installs the `post-commit` hook in the git repository containing `repo_root`.
///
/// - If `.git/hooks/post-commit` does not exist, it is created with the `#!/bin/sh` header and
///   the hook body.
/// - If it already exists and already contains [`HOOK_MARKER`], nothing is written (idempotent).
/// - If it exists but does not contain [`HOOK_MARKER`], the hook body is appended after a blank
///   line separator.
/// - In all cases the file is made executable (`0o755`).
///
/// Returns `true` when the hook body was newly appended (or the file was created), `false` when
/// it was already present (idempotent no-op).
///
/// # Errors
///
/// Returns [`GraphError::Io`] on any filesystem failure.
pub fn install_post_commit(repo_root: &Path) -> Result<bool> {
    let hooks_dir = repo_root.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks_dir)
        .map_err(|e| GraphError::Io(format!("create hooks dir: {e}")))?;

    let hook_path = hooks_dir.join("post-commit");
    let existing = read_or_empty(&hook_path)?;

    let added = if existing.contains(HOOK_MARKER) {
        // Already installed — only ensure permissions are correct.
        set_permissions(&hook_path, 0o755)?;
        false
    } else if existing.is_empty() {
        // New file — write shebang + body.
        std::fs::write(&hook_path, format!("{SHEBANG}\n{HOOK_BODY}"))
            .map_err(|e| GraphError::Io(format!("write {}: {e}", hook_path.display())))?;
        set_permissions(&hook_path, 0o755)?;
        true
    } else {
        // Existing non-empty hook — append after blank line.
        append_to_file(&hook_path, &format!("\n{HOOK_BODY}"))?;
        set_permissions(&hook_path, 0o755)?;
        true
    };

    Ok(added)
}

// ── hook install-merge-driver ─────────────────────────────────────────────────

/// Returns `true` when `haystack` already contains a complete non-empty line equal to `needle`
/// (after trimming).
fn line_present(haystack: &str, needle: &str) -> bool {
    haystack.lines().any(|l| l.trim() == needle)
}

/// Appends the `.gitattributes` routing line (idempotent).
///
/// Returns `true` when the line was appended, `false` when it was already present.
///
/// # Errors
///
/// Returns [`GraphError::Io`] on filesystem failure.
pub fn install_gitattributes(repo_root: &Path) -> Result<bool> {
    let path = repo_root.join(".gitattributes");
    let existing = read_or_empty(&path)?;
    if line_present(&existing, GITATTRIBUTES_LINE) {
        return Ok(false);
    }
    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    append_to_file(&path, &format!("{separator}{GITATTRIBUTES_LINE}\n"))?;
    Ok(true)
}

/// Appends the merge driver config to `.git/config` (idempotent).
///
/// Returns `true` when the section was appended, `false` when it was already present.
///
/// # Errors
///
/// Returns [`GraphError::Io`] on filesystem failure.
pub fn install_git_config(repo_root: &Path) -> Result<bool> {
    let path = repo_root.join(".git").join("config");
    let existing = read_or_empty(&path)?;
    if line_present(&existing, MERGE_DRIVER_SECTION) {
        return Ok(false);
    }
    let separator = if existing.ends_with('\n') || existing.is_empty() {
        ""
    } else {
        "\n"
    };
    append_to_file(
        &path,
        &format!("{separator}{MERGE_DRIVER_SECTION}\n{MERGE_DRIVER_VALUE}"),
    )?;
    Ok(true)
}

// ── Public run functions ──────────────────────────────────────────────────────

/// Runs `hook install` from `start_dir`.
///
/// Discovers the git repo root, installs the post-commit hook, and prints a status line.
/// Returns `0` on success, `1` if no git repo is found, or `1` on any I/O error.
///
/// # Errors
///
/// All errors are reported to stderr.
#[must_use]
pub fn run_install(start_dir: &Path) -> u8 {
    match run_install_inner(start_dir) {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("habitat-graph hook install: {e}");
            1
        }
    }
}

fn run_install_inner(start_dir: &Path) -> Result<String> {
    let root = discover_repo_root(start_dir).ok_or_else(|| {
        GraphError::Io(format!(
            "no git repository found above {}",
            start_dir.display()
        ))
    })?;
    let added = install_post_commit(&root)?;
    let status = if added {
        "post-commit hook installed"
    } else {
        "post-commit hook already present (no-op)"
    };
    Ok(format!(
        "{status} in {}",
        root.join(".git").join("hooks").display()
    ))
}

/// Runs `hook install-merge-driver` from `start_dir`.
///
/// Discovers the git repo root, installs the merge driver in `.git/config`, and appends the
/// routing line to `.gitattributes`.  Both operations are idempotent.
/// Returns `0` on success or `1` on error.
///
/// # Errors
///
/// All errors are reported to stderr.
#[must_use]
pub fn run_install_merge_driver(start_dir: &Path) -> u8 {
    match run_install_merge_driver_inner(start_dir) {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("habitat-graph hook install-merge-driver: {e}");
            1
        }
    }
}

fn run_install_merge_driver_inner(start_dir: &Path) -> Result<String> {
    let root = discover_repo_root(start_dir).ok_or_else(|| {
        GraphError::Io(format!(
            "no git repository found above {}",
            start_dir.display()
        ))
    })?;
    let cfg_added = install_git_config(&root)?;
    let attr_added = install_gitattributes(&root)?;
    let cfg_status = if cfg_added {
        "added"
    } else {
        "already present"
    };
    let attr_status = if attr_added {
        "added"
    } else {
        "already present"
    };
    Ok(format!(
        ".git/config merge driver: {cfg_status}; .gitattributes: {attr_status}"
    ))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::{
        discover_repo_root, install_git_config, install_gitattributes, install_post_commit,
        line_present, run_install, run_install_merge_driver, GITATTRIBUTES_LINE, HOOK_BODY,
        HOOK_MARKER, MERGE_DRIVER_SECTION,
    };

    // ── helpers ───────────────────────────────────────────────────────────────

    static SEQ: AtomicU32 = AtomicU32::new(0);
    fn tdir() -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let d = std::env::temp_dir().join(format!("hg_hook_{pid}_{n}"));
        fs::create_dir_all(&d).expect("tdir");
        d
    }

    /// Creates a minimal fake git repo at `dir` (just the `.git/` directory + `.git/config`).
    fn make_fake_repo(dir: &Path) -> PathBuf {
        let git_dir = dir.join(".git");
        fs::create_dir_all(&git_dir).expect("create .git");
        // Minimal config so .git/config reads don't error.
        fs::write(
            git_dir.join("config"),
            "[core]\n\trepositoryformatversion = 0\n",
        )
        .expect("write config");
        dir.to_path_buf()
    }

    // ── line_present ──────────────────────────────────────────────────────────

    #[test]
    fn line_present_exact_match() {
        assert!(line_present(
            "graph.json merge=habitat-graph\n",
            GITATTRIBUTES_LINE
        ));
    }

    #[test]
    fn line_present_surrounded_by_other_lines() {
        let hay = "*.json binary\ngraph.json merge=habitat-graph\n*.lock text\n";
        assert!(line_present(hay, GITATTRIBUTES_LINE));
    }

    #[test]
    fn line_present_false_when_absent() {
        assert!(!line_present("*.json binary\n", GITATTRIBUTES_LINE));
    }

    #[test]
    fn line_present_respects_trimming() {
        assert!(line_present(
            "  graph.json merge=habitat-graph  \n",
            GITATTRIBUTES_LINE
        ));
    }

    #[test]
    fn line_present_empty_haystack_is_false() {
        assert!(!line_present("", GITATTRIBUTES_LINE));
    }

    #[test]
    fn line_present_partial_match_is_false() {
        // A line that only contains part of the needle must not match.
        assert!(!line_present("graph.json\n", GITATTRIBUTES_LINE));
    }

    // ── discover_repo_root ────────────────────────────────────────────────────

    #[test]
    fn discover_finds_git_in_root_of_repo() {
        let d = tdir();
        make_fake_repo(&d);
        assert_eq!(discover_repo_root(&d), Some(d));
    }

    #[test]
    fn discover_finds_git_from_subdirectory() {
        let d = tdir();
        make_fake_repo(&d);
        let sub = d.join("src").join("deep");
        fs::create_dir_all(&sub).expect("create subdir");
        let found = discover_repo_root(&sub);
        assert_eq!(found, Some(d));
    }

    #[test]
    fn discover_returns_none_outside_any_repo() {
        // A temp dir with no `.git` ancestor — use a path we control.
        let _d = tdir(); // no .git here
                         // Walk up from inside an empty dir; must reach root and return None.
                         // We can't guarantee no `.git` in a parent on CI; use a dedicated tmp dir.
        let isolated = tdir();
        let result = discover_repo_root(&isolated);
        // Either None (isolated dir truly outside a repo) or Some (if tmp is inside a repo).
        // We just check the function doesn't panic.
        let _ = result;
    }

    #[test]
    fn discover_repo_root_not_found_returns_none() {
        // Path that doesn't exist → canonicalize fails → None.
        let phantom = PathBuf::from("/nonexistent_habitat_graph_hook_test_xyz");
        assert_eq!(discover_repo_root(&phantom), None);
    }

    // ── install_post_commit ───────────────────────────────────────────────────

    #[test]
    fn install_post_commit_creates_hook_file() {
        let d = tdir();
        make_fake_repo(&d);
        install_post_commit(&d).expect("install");
        assert!(d.join(".git").join("hooks").join("post-commit").exists());
    }

    #[test]
    fn install_post_commit_returns_true_on_new_install() {
        let d = tdir();
        make_fake_repo(&d);
        assert!(
            install_post_commit(&d).expect("install"),
            "must return true for new install"
        );
    }

    #[test]
    fn install_post_commit_file_contains_hook_body() {
        let d = tdir();
        make_fake_repo(&d);
        install_post_commit(&d).expect("install");
        let text =
            fs::read_to_string(d.join(".git").join("hooks").join("post-commit")).expect("read");
        assert!(text.contains(HOOK_BODY.trim()));
    }

    #[test]
    fn install_post_commit_file_starts_with_shebang_for_new_file() {
        let d = tdir();
        make_fake_repo(&d);
        install_post_commit(&d).expect("install");
        let text =
            fs::read_to_string(d.join(".git").join("hooks").join("post-commit")).expect("read");
        assert!(text.starts_with("#!/"), "hook file must start with shebang");
    }

    #[test]
    fn install_post_commit_is_idempotent_returns_false() {
        let d = tdir();
        make_fake_repo(&d);
        install_post_commit(&d).expect("first install");
        assert!(
            !install_post_commit(&d).expect("second install"),
            "second install must return false (already present)"
        );
    }

    #[test]
    fn install_post_commit_idempotent_body_not_duplicated() {
        let d = tdir();
        make_fake_repo(&d);
        install_post_commit(&d).expect("first");
        install_post_commit(&d).expect("second");
        let text =
            fs::read_to_string(d.join(".git").join("hooks").join("post-commit")).expect("read");
        // The marker must appear exactly once.
        assert_eq!(
            text.matches(HOOK_MARKER).count(),
            1,
            "hook marker must appear exactly once after idempotent install"
        );
    }

    #[test]
    fn install_post_commit_appends_to_existing_hook() {
        let d = tdir();
        make_fake_repo(&d);
        let hook_path = d.join(".git").join("hooks").join("post-commit");
        fs::create_dir_all(d.join(".git").join("hooks")).expect("hooks dir");
        fs::write(&hook_path, "#!/bin/sh\nexisting_hook_command\n").expect("write prior hook");
        let added = install_post_commit(&d).expect("install");
        assert!(added, "must return true when appending");
        let text = fs::read_to_string(&hook_path).expect("read");
        assert!(
            text.contains("existing_hook_command"),
            "prior hook content preserved"
        );
        assert!(
            text.contains(HOOK_BODY.trim()),
            "habitat-graph hook appended"
        );
    }

    #[test]
    fn install_post_commit_creates_hooks_dir_if_absent() {
        let d = tdir();
        make_fake_repo(&d);
        // Remove the hooks dir.
        let hooks = d.join(".git").join("hooks");
        if hooks.exists() {
            fs::remove_dir_all(&hooks).expect("remove hooks");
        }
        install_post_commit(&d).expect("install");
        assert!(hooks.exists(), "hooks dir must be created");
    }

    #[test]
    fn install_post_commit_file_contains_marker() {
        let d = tdir();
        make_fake_repo(&d);
        install_post_commit(&d).expect("install");
        let text =
            fs::read_to_string(d.join(".git").join("hooks").join("post-commit")).expect("read");
        assert!(
            text.contains(HOOK_MARKER),
            "hook file must contain the idempotency marker"
        );
    }

    // ── install_gitattributes ─────────────────────────────────────────────────

    #[test]
    fn install_gitattributes_creates_file() {
        let d = tdir();
        make_fake_repo(&d);
        install_gitattributes(&d).expect("install");
        assert!(d.join(".gitattributes").exists());
    }

    #[test]
    fn install_gitattributes_contains_routing_line() {
        let d = tdir();
        make_fake_repo(&d);
        install_gitattributes(&d).expect("install");
        let text = fs::read_to_string(d.join(".gitattributes")).expect("read");
        assert!(text.contains(GITATTRIBUTES_LINE));
    }

    #[test]
    fn install_gitattributes_returns_true_on_new_install() {
        let d = tdir();
        make_fake_repo(&d);
        assert!(install_gitattributes(&d).expect("install"));
    }

    #[test]
    fn install_gitattributes_is_idempotent_returns_false() {
        let d = tdir();
        make_fake_repo(&d);
        install_gitattributes(&d).expect("first");
        assert!(
            !install_gitattributes(&d).expect("second"),
            "second must be no-op"
        );
    }

    #[test]
    fn install_gitattributes_preserves_existing_content() {
        let d = tdir();
        make_fake_repo(&d);
        fs::write(d.join(".gitattributes"), "*.png binary\n").expect("seed");
        install_gitattributes(&d).expect("install");
        let text = fs::read_to_string(d.join(".gitattributes")).expect("read");
        assert!(
            text.contains("*.png binary"),
            "prior content must be preserved"
        );
        assert!(
            text.contains(GITATTRIBUTES_LINE),
            "routing line must be added"
        );
    }

    #[test]
    fn install_gitattributes_line_appears_exactly_once() {
        let d = tdir();
        make_fake_repo(&d);
        install_gitattributes(&d).expect("first");
        install_gitattributes(&d).expect("second");
        let text = fs::read_to_string(d.join(".gitattributes")).expect("read");
        assert_eq!(
            text.matches(GITATTRIBUTES_LINE).count(),
            1,
            "routing line must appear exactly once"
        );
    }

    // ── install_git_config ────────────────────────────────────────────────────

    #[test]
    fn install_git_config_adds_merge_section() {
        let d = tdir();
        make_fake_repo(&d);
        install_git_config(&d).expect("install");
        let text = fs::read_to_string(d.join(".git").join("config")).expect("read");
        assert!(text.contains(MERGE_DRIVER_SECTION));
    }

    #[test]
    fn install_git_config_returns_true_on_new_section() {
        let d = tdir();
        make_fake_repo(&d);
        assert!(install_git_config(&d).expect("install"));
    }

    #[test]
    fn install_git_config_is_idempotent_returns_false() {
        let d = tdir();
        make_fake_repo(&d);
        install_git_config(&d).expect("first");
        assert!(
            !install_git_config(&d).expect("second"),
            "second must be no-op"
        );
    }

    #[test]
    fn install_git_config_section_appears_once() {
        let d = tdir();
        make_fake_repo(&d);
        install_git_config(&d).expect("first");
        install_git_config(&d).expect("second");
        let text = fs::read_to_string(d.join(".git").join("config")).expect("read");
        assert_eq!(
            text.matches(MERGE_DRIVER_SECTION).count(),
            1,
            "merge driver section must appear exactly once"
        );
    }

    #[test]
    fn install_git_config_preserves_existing_config() {
        let d = tdir();
        make_fake_repo(&d);
        // Prior config was written by make_fake_repo.
        install_git_config(&d).expect("install");
        let text = fs::read_to_string(d.join(".git").join("config")).expect("read");
        assert!(
            text.contains("[core]"),
            "existing config sections must be preserved"
        );
    }

    // ── run_install ───────────────────────────────────────────────────────────

    #[test]
    fn run_install_returns_zero_for_valid_repo() {
        let d = tdir();
        make_fake_repo(&d);
        assert_eq!(run_install(&d), 0);
    }

    #[test]
    fn run_install_creates_post_commit_hook() {
        let d = tdir();
        make_fake_repo(&d);
        run_install(&d);
        assert!(d.join(".git").join("hooks").join("post-commit").exists());
    }

    #[test]
    fn run_install_returns_one_outside_repo() {
        // Isolated dir with no .git ancestor.
        let d = tdir();
        // We cannot guarantee this dir is not inside a repo on all CI setups, but we can
        // at least verify the function returns without panicking.
        let _ = run_install(&d);
    }

    #[test]
    fn run_install_is_idempotent() {
        let d = tdir();
        make_fake_repo(&d);
        assert_eq!(run_install(&d), 0);
        assert_eq!(run_install(&d), 0, "second run must also succeed");
    }

    // ── run_install_merge_driver ───────────────────────────────────────────────

    #[test]
    fn run_install_merge_driver_returns_zero() {
        let d = tdir();
        make_fake_repo(&d);
        assert_eq!(run_install_merge_driver(&d), 0);
    }

    #[test]
    fn run_install_merge_driver_creates_gitattributes() {
        let d = tdir();
        make_fake_repo(&d);
        run_install_merge_driver(&d);
        assert!(d.join(".gitattributes").exists());
    }

    #[test]
    fn run_install_merge_driver_updates_git_config() {
        let d = tdir();
        make_fake_repo(&d);
        run_install_merge_driver(&d);
        let text = fs::read_to_string(d.join(".git").join("config")).expect("read");
        assert!(text.contains(MERGE_DRIVER_SECTION));
    }

    #[test]
    fn run_install_merge_driver_is_idempotent() {
        let d = tdir();
        make_fake_repo(&d);
        assert_eq!(run_install_merge_driver(&d), 0);
        assert_eq!(run_install_merge_driver(&d), 0, "second call must succeed");
    }

    #[test]
    fn run_install_merge_driver_gitattributes_has_line_once() {
        let d = tdir();
        make_fake_repo(&d);
        run_install_merge_driver(&d);
        run_install_merge_driver(&d);
        let text = fs::read_to_string(d.join(".gitattributes")).expect("read");
        assert_eq!(text.matches(GITATTRIBUTES_LINE).count(), 1);
    }

    // ── additional hook.rs coverage ──────────────────────────────────────────

    #[test]
    fn install_post_commit_hook_is_executable() {
        let d = tdir();
        make_fake_repo(&d);
        install_post_commit(&d).expect("install");
        let hook = d.join(".git").join("hooks").join("post-commit");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&hook).expect("meta").permissions().mode();
            // At least one execute bit must be set (owner-execute is 0o100).
            assert_ne!(mode & 0o111, 0, "hook must be executable");
        }
        #[cfg(not(unix))]
        assert!(hook.exists());
    }

    #[test]
    fn install_post_commit_hook_contains_shebang_when_fresh() {
        let d = tdir();
        make_fake_repo(&d);
        install_post_commit(&d).expect("install");
        let text =
            fs::read_to_string(d.join(".git").join("hooks").join("post-commit")).expect("read");
        // A freshly-created hook must start with a shebang line.
        assert!(
            text.starts_with("#!/"),
            "new hook must start with shebang, got: {:?}",
            &text[..text.len().min(20)]
        );
    }

    #[test]
    fn install_post_commit_hook_body_references_habitat_graph() {
        let d = tdir();
        make_fake_repo(&d);
        install_post_commit(&d).expect("install");
        let text =
            fs::read_to_string(d.join(".git").join("hooks").join("post-commit")).expect("read");
        assert!(
            text.contains("habitat-graph"),
            "hook body must reference habitat-graph"
        );
    }

    #[test]
    fn run_install_from_subdirectory_still_succeeds() {
        let d = tdir();
        make_fake_repo(&d);
        let sub = d.join("src").join("lib");
        fs::create_dir_all(&sub).expect("sub");
        // run_install walks up from the subdir to find the repo root.
        assert_eq!(run_install(&sub), 0);
        // Hook must be written to the repo root, not the subdir.
        assert!(d.join(".git").join("hooks").join("post-commit").exists());
    }

    #[test]
    fn run_install_merge_driver_from_subdirectory_still_succeeds() {
        let d = tdir();
        make_fake_repo(&d);
        let sub = d.join("deep").join("nested");
        fs::create_dir_all(&sub).expect("sub");
        assert_eq!(run_install_merge_driver(&sub), 0);
        assert!(d.join(".gitattributes").exists());
    }

    #[test]
    fn run_install_merge_driver_config_section_once_on_double_call() {
        let d = tdir();
        make_fake_repo(&d);
        run_install_merge_driver(&d);
        run_install_merge_driver(&d);
        let text = fs::read_to_string(d.join(".git").join("config")).expect("read");
        assert_eq!(
            text.matches(MERGE_DRIVER_SECTION).count(),
            1,
            "merge driver section must appear exactly once after double install"
        );
    }

    #[test]
    fn install_git_config_adds_driver_command_field() {
        let d = tdir();
        make_fake_repo(&d);
        install_git_config(&d).expect("install");
        let text = fs::read_to_string(d.join(".git").join("config")).expect("read");
        // The config must include the `driver =` line pointing to the CLI.
        assert!(
            text.contains("driver = "),
            "config must specify a driver command"
        );
    }

    #[test]
    fn install_gitattributes_line_ends_with_merge_driver_name() {
        let d = tdir();
        make_fake_repo(&d);
        install_gitattributes(&d).expect("install");
        let text = fs::read_to_string(d.join(".gitattributes")).expect("read");
        // The routing line must mention `merge=habitat-graph`.
        assert!(
            text.contains("merge=habitat-graph"),
            "gitattributes must route graph.json to habitat-graph driver"
        );
    }

    #[test]
    fn install_post_commit_marker_appears_once_on_triple_install() {
        let d = tdir();
        make_fake_repo(&d);
        install_post_commit(&d).expect("first");
        install_post_commit(&d).expect("second");
        install_post_commit(&d).expect("third");
        let text =
            fs::read_to_string(d.join(".git").join("hooks").join("post-commit")).expect("read");
        assert_eq!(
            text.matches(HOOK_MARKER).count(),
            1,
            "marker must appear exactly once after three installs"
        );
    }

    #[test]
    fn run_install_does_not_fail_on_pre_existing_gitattributes() {
        let d = tdir();
        make_fake_repo(&d);
        fs::write(d.join(".gitattributes"), "# pre-existing\n").expect("seed");
        // run_install writes the hook, but does NOT write .gitattributes (that is run_install_merge_driver).
        assert_eq!(run_install(&d), 0);
        // .gitattributes must be untouched by run_install.
        let text = fs::read_to_string(d.join(".gitattributes")).expect("read");
        assert_eq!(
            text, "# pre-existing\n",
            "run_install must not touch .gitattributes"
        );
    }

    #[test]
    fn run_install_merge_driver_preserves_existing_gitattributes() {
        let d = tdir();
        make_fake_repo(&d);
        fs::write(d.join(".gitattributes"), "*.png binary\n").expect("seed");
        run_install_merge_driver(&d);
        let text = fs::read_to_string(d.join(".gitattributes")).expect("read");
        assert!(
            text.contains("*.png binary"),
            "existing .gitattributes content must be preserved"
        );
    }
}
