// SPDX-License-Identifier: MIT
//! The root gate: one lexical normalization and one allow-list matcher shared by
//! every path gate (the worktree editor's read/write sandbox, the
//! forward-materialize guard, and [`crate::RootSetFs`]).
//!
//! The decision is **lexical**: `.`/`..` collapse before the prefix test, and
//! symlinks are not resolved. [`crate::RootSetFs`] adds a resolved-path check on
//! top; the local editor gate stays lexical and follows symlinks.

use std::path::{Component, Path, PathBuf};

/// Resolve `.` and `..` components **lexically**, without requiring the path
/// to exist on disk — the single normalization every path gate shares so a
/// crafted `…/worktree/../../etc/passwd` collapses before any prefix test.
/// A leading `..` pops nothing (the path can't escape below
/// its own root), matching the context-repo gate's behavior.
pub fn normalize_path_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    normalized
}

/// Pure allow-list decision: does `path` resolve inside any of `roots`, after
/// lexical `..`/`.` normalization of **both** sides? The env-free core shared by
/// the editor sandbox and the materialize guard. Worktree
/// records store the raw `code_repo_path`, which may itself carry `.`/`..`
/// segments, so the roots are normalized too.
///
/// A DEGENERATE root that would over-match is rejected: an empty root (`""` —
/// `starts_with(PathBuf::new())` is always true), a bare filesystem root (`/`),
/// or a non-absolute root. A real `code_repo_path` is always absolute with a
/// parent, so this keeps a corrupt / hand-edited record from degrading the
/// allow-list to "allow any path". `/` → `parent()==None`;
/// `""`/relative → `is_absolute()==false`.
pub fn path_within_any_root(path: &Path, roots: &[PathBuf]) -> bool {
    let normalized = normalize_path_components(path);
    roots.iter().any(|root| {
        let root_norm = normalize_path_components(root);
        root_norm.is_absolute()
            && root_norm.parent().is_some()
            && normalized.starts_with(&root_norm)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_path_components_collapses_traversal() {
        assert_eq!(
            normalize_path_components(Path::new("/a/b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(
            normalize_path_components(Path::new("/a/./b")),
            PathBuf::from("/a/b")
        );
        // A crafted escape collapses lexically before any prefix test.
        assert_eq!(
            normalize_path_components(Path::new("/a/b/../../etc/passwd")),
            PathBuf::from("/etc/passwd")
        );
    }

    #[test]
    fn path_within_any_root_admits_inside_denies_outside_and_traversal() {
        let roots = vec![PathBuf::from("/home/dev/acme-api-wt/feat-story-123")];

        // A file inside a registered worktree root is admitted…
        assert!(path_within_any_root(
            Path::new("/home/dev/acme-api-wt/feat-story-123/src/main.rs"),
            &roots
        ));
        // …as is the root itself (the first `list_directory(ws.cwd)` on mount).
        assert!(path_within_any_root(
            Path::new("/home/dev/acme-api-wt/feat-story-123"),
            &roots
        ));

        // Arbitrary outside paths are denied.
        assert!(!path_within_any_root(Path::new("/etc/passwd"), &roots));
        assert!(!path_within_any_root(
            Path::new("/home/dev/some-other-dir/x"),
            &roots
        ));
        // A sibling that merely shares a path prefix is NOT admitted
        // (`starts_with` is component-wise, not a substring match).
        assert!(!path_within_any_root(
            Path::new("/home/dev/acme-api-wt/feat-story-123-evil/x"),
            &roots
        ));
        // `..` traversal out of the root is normalized, then denied.
        assert!(!path_within_any_root(
            Path::new("/home/dev/acme-api-wt/feat-story-123/../../../etc/passwd"),
            &roots
        ));

        // An empty allow-list (index not yet populated) denies everything —
        // the gate never degrades to "allow any path".
        assert!(!path_within_any_root(
            Path::new("/home/dev/acme-api-wt/feat-story-123/src/main.rs"),
            &[]
        ));
    }

    #[test]
    fn path_within_any_root_rejects_degenerate_roots() {
        // A corrupt / hand-edited record with an empty or bare-root
        // `code_repo_path` must NOT degrade the gate to allow-all: `""`
        // normalizes to an empty PathBuf (every path `starts_with` it) and `/`
        // prefixes every absolute path. Both must be skipped.
        // `""`/`/` (and normalized aliases) plus non-absolute roots — none may
        // admit anything. `code_repo_path_hash` blocks non-absolute at write
        // time, so `shared`/`../x` are defense-in-depth (also can't prefix an
        // absolute candidate anyway).
        for degenerate in ["", "/", "/..", "/.", "//", "shared", "../x", "./rel"] {
            let roots = vec![PathBuf::from(degenerate)];
            assert!(
                !path_within_any_root(Path::new("/etc/passwd"), &roots),
                "degenerate root {degenerate:?} must not admit /etc/passwd"
            );
            assert!(
                !path_within_any_root(Path::new("/home/dev/anything/x"), &roots),
                "degenerate root {degenerate:?} must not admit an arbitrary path"
            );
        }
        // A legit single-parent root (`/foo`) is still admitted — the guard is
        // "has a parent", not "deep enough".
        assert!(path_within_any_root(
            Path::new("/foo/bar"),
            &[PathBuf::from("/foo")]
        ));
    }
}
