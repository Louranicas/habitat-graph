//! Lexical path confinement (anti-traversal). Pure: no filesystem access (Design Rule 1).
//!
//! The FS-canonicalizing confinement lives in `habitat-graph-source`; this is the cheap lexical
//! pre-check that rejects `..` escapes without touching disk.

use std::path::{Component, Path, PathBuf};

/// Normalizes a path lexically — resolving `.` and `..` without any filesystem access.
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Confines `candidate` (resolved relative to `root`) to within `root`, rejecting `..` escapes.
///
/// Returns the lexically-normalized confined path.
///
/// # Errors
/// Returns [`GraphError::Guard`](crate::GraphError::Guard) if the resolved path escapes `root`.
pub fn confine_to(root: &Path, candidate: &Path) -> crate::Result<PathBuf> {
    let root_n = lexical_normalize(root);
    let resolved = lexical_normalize(&root_n.join(candidate));
    if resolved.starts_with(&root_n) {
        Ok(resolved)
    } else {
        Err(crate::GraphError::Guard(format!(
            "path escapes confinement root: {}",
            candidate.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn simple_child_is_allowed() {
        let got = confine_to(Path::new("/out"), Path::new("sub/file.json")).unwrap();
        assert_eq!(got, Path::new("/out/sub/file.json"));
    }

    #[test]
    fn current_dir_components_collapse() {
        let got = confine_to(Path::new("/out"), Path::new("./a/./b")).unwrap();
        assert_eq!(got, Path::new("/out/a/b"));
    }

    #[test]
    fn parent_traversal_escaping_root_is_rejected() {
        assert!(confine_to(Path::new("/out"), Path::new("../etc/passwd")).is_err());
        assert!(confine_to(Path::new("/out"), Path::new("../../etc/passwd")).is_err());
    }

    #[test]
    fn internal_parent_that_stays_inside_is_allowed() {
        // /out/a/../b normalizes to /out/b — still inside.
        let got = confine_to(Path::new("/out"), Path::new("a/../b")).unwrap();
        assert_eq!(got, Path::new("/out/b"));
    }

    #[test]
    fn sibling_prefix_is_not_confused_for_child() {
        // "/outsider" must NOT be treated as inside "/out" (component-wise, not string-prefix).
        assert!(confine_to(Path::new("/out"), Path::new("../outsider/x")).is_err());
    }

    #[test]
    fn absolute_candidate_escaping_is_rejected() {
        assert!(confine_to(Path::new("/out"), Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn root_itself_is_confined() {
        let got = confine_to(Path::new("/out"), Path::new(".")).unwrap();
        assert_eq!(got, Path::new("/out"));
    }

    #[test]
    fn deep_nesting_allowed() {
        let got = confine_to(Path::new("/out"), Path::new("a/b/c/d/e.json")).unwrap();
        assert_eq!(got, Path::new("/out/a/b/c/d/e.json"));
    }
}
