// SPDX-License-Identifier: MIT
//! Which managed worktrees may have a live CLI session.
//!
//! SW-XS-3 (K6): moved out of `commands::watch` so the management panel no
//! longer depends on the sync watcher module, which SW-XS-3/SW-XS-4 shrink. Pure
//! path math over the live-session home dirs the terminal hosts report
//! (`pty_backend::live_session_home_dirs`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::commands::pty_backend::Liveness;
use crate::content_engine::sync::WaveCoordinator;

/// SW-FX-20: of `candidates` whose root contains `cwd`, the most specific
/// (longest path) — the worktree a session at `cwd` is actually working in. With
/// worktrees nested under a shared parent a deep `cwd` can be under several roots;
/// the longest match wins. `None` when `cwd` is under none.
pub(crate) fn most_specific_containing(cwd: &Path, candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .filter(|p| cwd.starts_with(p))
        .max_by_key(|p| p.as_os_str().len())
        .cloned()
}

/// SLW-WL-5 (AC-2, DRY): given the live-session home dirs and a set of already-
/// normalized candidate worktree roots, the subset of roots that are LIVE — a root
/// is live iff some session's home dir resolves to it as its most-specific containing
/// root. The single source of truth for "which worktrees have a live CLI session",
/// shared by the reverse filter and the management-panel DTO
/// ([`list_managed_worktrees`](crate::commands::worktree::list_managed_worktrees))
/// so the two can never drift. Home dirs are normalized here (raw FE spawn args);
/// callers pass roots already normalized (index-aligned reuse).
pub(crate) fn live_worktree_roots(
    live_home_dirs: &[PathBuf],
    roots: &[PathBuf],
) -> HashSet<PathBuf> {
    live_home_dirs
        .iter()
        .filter_map(|home| {
            most_specific_containing(&WaveCoordinator::normalize_target_path(home), roots)
        })
        .collect()
}

/// The subset of `roots` that may have a live CLI session, for a caller that
/// would remove or tear down the rest: [`live_worktree_roots`] while the
/// sessions are known, and EVERY root while they are unknown (a terminal host
/// that cannot be asked may run an agent in any of them) — so nothing is
/// torn down on a guess.
pub(crate) fn possibly_live_worktree_roots(
    live_home_dirs: &Liveness<Vec<PathBuf>>,
    roots: &[PathBuf],
) -> HashSet<PathBuf> {
    match live_home_dirs {
        Liveness::Known(dirs) => live_worktree_roots(dirs, roots),
        Liveness::Unknown => roots.iter().cloned().collect(),
    }
}

/// How many live sessions were started inside `root` (their home dir is `root` or
/// below it), or `None` while a terminal host can't be asked.
pub(crate) fn live_sessions_inside(
    live_home_dirs: &Liveness<Vec<PathBuf>>,
    root: &Path,
) -> Option<usize> {
    let Liveness::Known(dirs) = live_home_dirs else {
        return None;
    };
    let root = WaveCoordinator::normalize_target_path(root);
    Some(
        dirs.iter()
            .filter(|home| WaveCoordinator::normalize_target_path(home).starts_with(&root))
            .count(),
    )
}

/// The number of live sessions started inside a worktree folder, so a removal can
/// refuse while something it doesn't own still runs there (and wait for the ones it
/// ended to exit). `None` when a terminal host can't be asked.
#[tauri::command]
pub async fn worktree_live_session_count(
    app: tauri::AppHandle,
    worktree_path: String,
) -> Result<Option<usize>, String> {
    let live = crate::commands::pty_backend::live_session_home_dirs_off_thread(&app).await;
    Ok(live_sessions_inside(&live, Path::new(&worktree_path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_sessions_inside_counts_sessions_at_or_below_the_folder() {
        let live = Liveness::Known(vec![
            PathBuf::from("/wt/story"),
            PathBuf::from("/wt/story/sub"),
            PathBuf::from("/wt/story-2"),
            PathBuf::from("/elsewhere"),
        ]);
        assert_eq!(live_sessions_inside(&live, Path::new("/wt/story")), Some(2));
        assert_eq!(live_sessions_inside(&live, Path::new("/wt/none")), Some(0));
        assert_eq!(
            live_sessions_inside(&Liveness::Unknown, Path::new("/wt/story")),
            None
        );
    }

    #[test]
    fn most_specific_containing_picks_longest_match() {
        let repos = vec![PathBuf::from("/src/clone"), PathBuf::from("/src/clone/wt")];
        assert_eq!(
            most_specific_containing(Path::new("/src/clone/wt/a/b"), &repos),
            Some(PathBuf::from("/src/clone/wt"))
        );
        assert_eq!(
            most_specific_containing(Path::new("/src/clone/docs"), &repos),
            Some(PathBuf::from("/src/clone"))
        );
        assert_eq!(
            most_specific_containing(Path::new("/elsewhere/repo"), &repos),
            None
        );
    }

    #[test]
    fn unknown_liveness_keeps_every_worktree_possibly_live() {
        let roots = [PathBuf::from("/repo/a"), PathBuf::from("/repo/b")];
        assert_eq!(
            possibly_live_worktree_roots(&Liveness::Unknown, &roots),
            HashSet::from(roots.clone())
        );
        assert!(possibly_live_worktree_roots(&Liveness::Known(Vec::new()), &roots).is_empty());
    }

    #[test]
    fn a_session_home_marks_its_most_specific_root_live() {
        let roots = [PathBuf::from("/repo/a"), PathBuf::from("/repo/b")];
        let live = live_worktree_roots(&[PathBuf::from("/repo/b/src")], &roots);
        assert_eq!(live, HashSet::from([PathBuf::from("/repo/b")]));
    }
}
