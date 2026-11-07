// SPDX-License-Identifier: MIT
//! What remains of the story/content projection module after SW-XS-8: the
//! legacy projection engine (whole-root synthesis, `projections.yml`
//! resolution, the forward materialize, the reverse claim and the per-file
//! destination effects) is gone. Content reaches a workspace only through
//! agent preparation and the explicit operations (`agents::prepare`).
//!
//! What stays here:
//! - the structural source op behind `delete_content_file` /
//!   `move_content_file` (team content only — nothing fans out);
//! - the merge-mark preview (`preview_merge_marked_file`);
//! - the team chain's content roots, the reserved-dir rule, the sync-timing
//!   gate, and the selected-context request shapes.

pub mod selected_context;

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::content_engine::{self, ResolverContext, SuppressionHandle};
use crate::install::frontmatter_cache::{FrontmatterCache, SharedFrontmatterCache};
use crate::models::error::{install_err, AppError};

/// SW-CP-11 T4: direction the manifest materialize runs in. Forward is the
/// default (and the only behavior pre-CP-11): a locally-modified dest is left
/// untouched (honor edits). `Reconcile` is the reconcile-on-open mode — a
/// locally-modified dest made while the surface was closed is **reverse-promoted**
/// to the source layer when the source is unchanged, surfaced as a conflict when
/// both diverged, and forward-materialized otherwise — converging the surface in
/// one pass (AC-3, D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    Forward,
    Reconcile,
}

/// SW-CP-15: top-level content dirs that must never project / claim — they
/// would let a mis-named content dir bidirectionally sync source code or
/// corrupt a repo. Complements `IGNORE_GLOBS` (which match `**/…` *paths*, not
/// a bare top-level dir *name*).
///
/// The VCS-metadata half is the shared `codesense_fs_core::filters::is_vcs_metadata_dir`
/// (also consulted by the file-viewer scan) so the two surfaces can never
/// disagree about which names are VCS dirs; the build/dependency half stays
/// projection-only, because the worktree file viewer deliberately SHOWS
/// `node_modules`/`target` de-emphasized rather than hiding them.
pub(crate) fn is_reserved_top_level_dir(name: &str) -> bool {
    codesense_fs_core::filters::is_vcs_metadata_dir(name)
        || matches!(name, "node_modules" | "target")
}

/// SW-CP-14: a structural source-layer change to propagate to one code repo's
/// projected dests.
///
/// The source side has ALREADY been mutated on disk by the gesture command
/// (`delete_content_file` / `move_content_file`, under watcher suppression);
/// this executor brings each attached repo's projected dests + lockfile into
/// agreement. **Targeted** (D4): it matches ONLY the entries sourced from the
/// changed path(s) by `source_path` — never a staleness-driven
/// force-unmaterialize, which would over-delete stale-but-present sources.
#[derive(Debug, Clone)]
pub enum StructuralChange {
    /// The source at `source_path` (a file) — or every source beneath it (a
    /// directory) — was deleted. Remove the matching dests.
    Delete { source_path: PathBuf, is_dir: bool },
    /// The source moved from `from` to `to` (file or directory). Remove the old
    /// dests and re-materialize the relocated source(s) to their new dests.
    Rename {
        from: PathBuf,
        to: PathBuf,
        is_dir: bool,
    },
}

/// SW-CP-14: enumerate the regular files beneath `dir` (recursively). Used to
/// (a) suppress every child delete/rename event and (b) reconcile each cache
/// entry by exact path — the frontmatter cache is keyed by absolute file path,
/// so a directory path is never itself a cache key.
fn content_files_under(dir: &Path) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// SW-CP-14: reconcile one source path into the frontmatter cache after a
/// structural op — `refresh_under_lock_no_transition` evicts the entry when the
/// file is gone (delete / rename source) and inserts/refreshes it when present
/// (rename dest). Transition-free for the same reason as the suppressed-write
/// self-heal (the source layer owns status transitions). No-op when the path
/// isn't a `_team/{team}/content/...` cache key.
fn reconcile_cache_path(cache: &SharedFrontmatterCache, path: &Path) {
    let Some(path_str) = path.to_str() else {
        return;
    };
    let Some((team, _, _)) = crate::install::capture::parse_context_repo_parts(path_str) else {
        return;
    };
    FrontmatterCache::refresh_under_lock_no_transition(cache, path, &team);
}

/// SLW-RI-3: reconcile many context paths in ONE RCU cycle per team, batching the
/// per-child clones a directory structural op would otherwise pay one-at-a-time
/// (symmetric to the wave-vanished-set batching in `evict_vanished_sources`).
/// Paths outside a `_team/{team}/content/...` key are skipped; a single directory
/// op's children all share one team, but grouping keeps it robust to mixed input.
fn reconcile_cache_paths(cache: &SharedFrontmatterCache, paths: &[PathBuf]) {
    let mut by_team: std::collections::HashMap<String, Vec<PathBuf>> =
        std::collections::HashMap::new();
    for p in paths {
        if let Some(s) = p.to_str() {
            if let Some((team, _, _)) = crate::install::capture::parse_context_repo_parts(s) {
                by_team.entry(team).or_default().push(p.clone());
            }
        }
    }
    for (team, group) in by_team {
        FrontmatterCache::refresh_many_no_transition(cache, &team, &group);
    }
}

/// SW-CP-14: apply the source-side half of a structural op — the
/// watcher-suppressed fs delete/rename of the context-layer path(s) — and
/// reconcile the frontmatter cache. A directory's content files are enumerated
/// up front so every child event is suppressed and each cache entry is
/// evicted/re-keyed by exact path.
///
/// **Lock note (review F2):** this runs in the gesture command, NOT under any
/// coordinator wave lock — so a delete still works when no team is active
/// (the dest fan-out that ran under the keyed lock is gone: SW-XS-8 — nothing
/// fans team content out into worktrees). The brief cross-side window it opens
/// (source gone, peer dests not yet removed) is benign: the source op is
/// watcher-suppressed (no wave is enqueued from it) and the source is
/// cache-evicted, so any concurrent forward wave hits the SW-FX-11
/// vanished-source skip rather than resurrecting a dest; the authoritative
/// dest cleanup is the subsequent fan-out, which holds the per-target keyed
/// locks for every attached repo (SLW-RI-4).
pub fn apply_structural_source_op(
    change: &StructuralChange,
    suppression: Option<&SuppressionHandle>,
    cache: Option<&SharedFrontmatterCache>,
) -> Result<(), AppError> {
    match change {
        StructuralChange::Delete {
            source_path,
            is_dir,
        } => {
            if *is_dir {
                let affected = content_files_under(source_path);
                let remove = || {
                    crate::content_engine::atomic_io::remove_tree(source_path)
                        .map_err(|error| install_err("remove_dir_all", error))
                };
                match suppression {
                    // Suppress every child delete event (the context + reverse
                    // watchers share this dispatcher) plus the dir itself.
                    Some(h) => {
                        let mut paths = affected.clone();
                        paths.push(source_path.clone());
                        let _guard = content_engine::Suppression::new(h, paths);
                        remove()?;
                    }
                    None => remove()?,
                }
                if let Some(cache) = cache {
                    // SLW-RI-3: batch the whole directory's children into one RCU
                    // cycle per team instead of N clone→publish cycles.
                    reconcile_cache_paths(cache, &affected);
                }
            } else {
                match suppression {
                    Some(h) => content_engine::remove_suppressed(source_path, h),
                    None => content_engine::remove(source_path),
                }
                .map_err(|e| install_err("remove", e))?;
                if let Some(cache) = cache {
                    reconcile_cache_path(cache, source_path);
                }
            }
        }
        StructuralChange::Rename { from, to, is_dir } => {
            let affected = if *is_dir {
                content_files_under(from)
            } else {
                vec![from.clone()]
            };
            match suppression {
                Some(h) => content_engine::rename_suppressed(from, to, h),
                None => content_engine::rename(from, to),
            }
            .map_err(|e| install_err("rename", e))?;
            if let Some(cache) = cache {
                for old in &affected {
                    reconcile_cache_path(cache, old);
                    if let Ok(rel) = old.strip_prefix(from) {
                        // A file rename strips to an empty `rel`; `to.join("")`
                        // appends a trailing separator (an unstattable path), so
                        // the cache re-insert would no-op — use `to` directly
                        // (same guard as the propagate re-materialize site).
                        let new = if rel.as_os_str().is_empty() {
                            to.clone()
                        } else {
                            to.join(rel)
                        };
                        reconcile_cache_path(cache, &new);
                    }
                }
            }
        }
    }
    Ok(())
}

/// SW-SY-7 (AC-1) / RF-perf-0 (AC-1): the ONE `log` target that gates every gated
/// sync-timing emit — the reverse claim-walk split, the SW-SY-8 residual counters
/// (via a `pub(crate) use` re-export), AND the RF-perf-0 forward-materialize
/// `walk_ns`/`filter_ns`/`materialize_ns` phase split. RF-perf-0 renamed it from
/// `REVERSE_TIMING_TARGET` (value `…::reverse_timing`) to this neutral
/// `content_engine::sync::timing`: the target has carried the `forward_materialize`
/// line since SW-SY-7, so the `reverse_` name was a misnomer — one gate, both
/// directions. Emitted only when this target is at `Debug` or below (default prod is
/// `Info`), so `log_enabled!` short-circuits the `Instant::now` calls to nothing on
/// a normal build — present for a `Debug`-level profiling run, never hot in prod.
/// Arm it by setting the ambient level to `Debug`: `lib.rs` parses `CODESENSE_LOG`
/// as a bare `LevelFilter` (NOT a per-target `target=level` directive), so a **dev
/// build is already `Debug`** (armed by default) and a release build arms with
/// `CODESENSE_LOG=debug`. (SW-SY-8 re-verified: the per-target string does not parse.)
pub(crate) const SYNC_TIMING_TARGET: &str = "content_engine::sync::timing";

/// SW-SY-7 (AC-1) / RF-perf-0 (AC-1, DRY): is the sync-timing spine armed? THE single
/// read of the [`SYNC_TIMING_TARGET`] gate — every gated-timing site (the SY-7 forward
/// wrapper + claim-walk, the RF-perf-0 forward phase split, and the SW-SY-8 residual
/// counters via `residual_metrics::timing_enabled`) funnels through here, so "one gate,
/// both directions" is enforced by construction, not by four copies of the same macro.
/// `log_enabled!` is a cheap flag read; below `Debug` (default prod) it is `false` and
/// every `Instant::now` guarded by it is skipped — inert-when-off.
#[inline]
pub(crate) fn sync_timing_enabled() -> bool {
    log::log_enabled!(target: SYNC_TIMING_TARGET, log::Level::Debug)
}

#[cfg(test)]
mod structural_ops_tests {
    //! SW-CP-14: the structural source executor (`apply_structural_source_op`).
    //! SW-XS-4: the dest-side propagation is gone with the automatic reverse
    //! delete that was its last caller.
    use super::*;
    use std::fs;

    struct Fx {
        _tmp: tempfile::TempDir,
        content_root: PathBuf,
    }

    fn setup() -> Fx {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: tmp.path().to_path_buf(),
            user_id: 42,
            org_slug: "alderco/codesense".to_string(),
            team_name: Some("default".to_string()),
        };
        let content_root = crate::content_engine::content_layer_root(
            crate::content_engine::LayerId::TeamProjection,
            &ctx,
        )
        .unwrap();
        Fx {
            _tmp: tmp,
            content_root,
        }
    }

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    // apply_structural_source_op (no suppression / no cache — headless path):
    // delete removes the source file; rename moves it.
    #[test]
    fn apply_source_op_deletes_and_renames_unsuppressed() {
        let fx = setup();
        let f = fx.content_root.join("impl/x.md");
        write(&f, b"x");
        apply_structural_source_op(
            &StructuralChange::Delete {
                source_path: f.clone(),
                is_dir: false,
            },
            None,
            None,
        )
        .unwrap();
        assert!(!f.exists());

        let from = fx.content_root.join("impl/a.md");
        let to = fx.content_root.join("impl/b.md");
        write(&from, b"body");
        apply_structural_source_op(
            &StructuralChange::Rename {
                from: from.clone(),
                to: to.clone(),
                is_dir: false,
            },
            None,
            None,
        )
        .unwrap();
        assert!(!from.exists());
        assert_eq!(fs::read(&to).unwrap(), b"body");
    }
}

#[cfg(test)]
mod reserved_dir_tests {
    use super::*;

    #[test]
    fn is_reserved_top_level_dir_covers_vcs_and_build_dirs() {
        for reserved in [".git", "node_modules", "target", ".hg", ".svn"] {
            assert!(
                is_reserved_top_level_dir(reserved),
                "{reserved} is reserved"
            );
        }
        for allowed in [".claude", ".github", ".vscode", "src"] {
            assert!(
                !is_reserved_top_level_dir(allowed),
                "{allowed} is not reserved"
            );
        }
    }
}
