// SPDX-License-Identifier: MIT
use std::path::Path;

use log::{info, warn};
use tauri::Manager;

use crate::install::frontmatter_cache::{FrontmatterCache, SharedFrontmatterCache};

/// Serializes concurrent `init_watched_cache` calls so two overlapping
/// workspace switches cannot interleave their clear / build / merge
/// sequences. Mirrors the retired `SWITCH_LOCK` pattern.
///
/// NB-7: changed from `std::sync::Mutex` to `tokio::sync::Mutex` because
/// `init_watched_cache` now awaits the async `add_watch` (NB-7 Task 3.9).
/// `std::sync::MutexGuard` is `!Send` so cannot cross await boundaries;
/// the tokio mutex's guard is `Send`.
static INIT_WATCHED_CACHE_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// R-3: Atomic init for a workspace's frontmatter cache.
///
/// SW-WL-3 (AC-12): the previous version also eagerly attached one file
/// watcher per source repo of the active team here — the session-bound model
/// that did not scale (90+ watches for a 20-repo team regardless of what the
/// user opened). Watcher attach is now **surface-bound**: `WorkspaceOpenSet`
/// drives `lazy_attach_workspace` / `lazy_detach_workspace` per open
/// workspace. This command now only (re)builds the frontmatter cache, which
/// every team needs regardless of which surfaces are open.
///
/// Sequence:
///   1. Clear in-memory cache state (workspace transition reset).
///   2. Build a fresh cache from disk via `load_or_build`, persist a
///      warm-start copy, and merge into in-memory state (preserving any
///      entries a surface-bound watcher updated during the build).
#[tauri::command]
pub async fn init_watched_cache(
    app: tauri::AppHandle,
    org_root: String,
    cache_path: String,
) -> Result<u32, String> {
    // Serialize concurrent invocations so overlapping workspace switches
    // cannot leak entries across each other's clear / build / merge cycles.
    let _guard = INIT_WATCHED_CACHE_LOCK.lock().await;

    info!(target: "install", "init_watched_cache: org_root={}", org_root);

    let state = app.state::<SharedFrontmatterCache>();

    // 1. Workspace transition reset. Brief RCU write (SLW-RI-3).
    // RF-perf-9: `clear_entries` drops the aux story_id index too.
    FrontmatterCache::rcu(state.inner(), |c| c.clear_entries());

    // 2. Build + persist + merge.
    //
    // SLW-FX-16 (P1): the heavy work here — `load_or_build`'s recursive
    // `read_dir` crawl of the org frontmatter tree plus `persist_to_disk`'s
    // JSON serialize + file write — is CPU/disk-bound. Run it on the Tokio
    // blocking pool via `await_blocking` (mirroring `scan_pipeline_tree`, the
    // switch path's other heavy command) instead of occupying a Tauri
    // async-dispatch worker for the whole crawl, which stalled concurrent IPC
    // and surfaced as the "hang for a sec" on workspace open / switch / create.
    //
    // The borrowed `State` handle is not `'static`, so it can't cross into the
    // `spawn_blocking` closure — clone the `Arc`
    // (`SharedFrontmatterCache = Arc<RwLock<Arc<..>>>`, cheap ref-count bump)
    // and own the paths. `INIT_WATCHED_CACHE_LOCK` is still held across the
    // await (tokio's guard is `Send`), preserving switch serialization; the
    // clear above runs strictly before the awaited closure, preserving the
    // clear → build → persist → merge cache-coherence ordering.
    let state_handle: SharedFrontmatterCache = state.inner().clone();
    let org_root_path = std::path::PathBuf::from(&org_root);
    let cache_path_buf = std::path::PathBuf::from(&cache_path);
    let count = crate::commands::await_blocking("init_watched_cache", move || {
        build_persist_merge(&state_handle, &org_root_path, &cache_path_buf)
    })
    .await?;
    info!(target: "install", "init_watched_cache: indexed {} entries", count);
    Ok(count)
}

/// Extracted from `init_watched_cache` for unit testability without an
/// `AppHandle`. Builds a fresh cache from disk, persists it, and merges
/// into `state`. Does NOT clear `state` or start watchers — both are the
/// Tauri command's responsibility because they require the app handle.
fn build_persist_merge(
    state: &SharedFrontmatterCache,
    org_root: &Path,
    cache_path: &Path,
) -> Result<u32, String> {
    let new_cache = FrontmatterCache::load_or_build(cache_path, org_root);

    if let Err(e) = new_cache.persist_to_disk(cache_path) {
        warn!(target: "install", "init_watched_cache: persist failed: {}", e);
    }

    // SLW-RI-3: merge under a single RCU write (build + persist ran off-lock).
    let count = FrontmatterCache::rcu(state, |c| {
        c.merge_from(new_cache);
        c.entries.len() as u32
    });
    Ok(count)
}

// SW-XS-3: the team-switch command (`switch_team_install`) is gone. Its only
// job was the `TeamSwitch` wave; switching teams writes no workspace content.

/// SS-4c: parse `(user_id, org_slug)` from `org_root` anchored on
/// `codesense_home`. Returns `None` unless the path structure matches:
///
/// ```text
/// {codesense_home}/users/{user_id}/orgs/{org_slug}
/// ```
///
/// Iter-2 (Blind M2 / Auditor H2): anchoring closes the substring-walk
/// foot-gun where any path containing literal `users/{n}` segments
/// would parse successfully regardless of layout. By demanding the
/// `codesense_home` prefix plus a strict `users/N/orgs/SLUG` immediately
/// after, we lock down the only valid shape.
/// SS-FU-7: visibility bumped from private to `pub(crate)` so
/// `commands::worktree::run_worktree_setup` can derive a
/// `ResolverContext` for the post-create projection block without
/// inventing a thin wrapper.
/// SW-XS-5: its last production caller (the nav's per-file ownership read) is
/// gone; kept for the test-only projection helpers that still use it.
#[cfg(test)]
pub(crate) fn parse_user_id_and_org_slug_anchored(
    codesense_home: &str,
    org_root: &str,
) -> Option<(u64, String)> {
    use std::path::Path;
    let home_path = Path::new(codesense_home);
    let org_root_path = Path::new(org_root);
    if let Some(location) =
        crate::context_repo::context_attachments::location_for_path(home_path, org_root_path)
            .ok()?
    {
        return Some((
            location.principal.as_str().parse().ok()?,
            location.attachment.slug,
        ));
    }
    let rel = org_root_path.strip_prefix(home_path).ok()?;
    let mut comps = rel.components();
    let users_seg = comps.next()?.as_os_str().to_str()?;
    if users_seg != crate::context_repo::USERS_DIR {
        return None;
    }
    let user_id_seg = comps.next()?.as_os_str().to_str()?;
    let user_id = user_id_seg.parse::<u64>().ok()?;
    let orgs_seg = comps.next()?.as_os_str().to_str()?;
    if orgs_seg != crate::context_repo::ORGS_DIR {
        return None;
    }
    let owner_seg = comps.next()?.as_os_str().to_str()?;
    // SS-FU-12: org_slug now spans two path segments (`{owner}/{repo}`).
    // Combine the repo segment when present; a lone segment is a
    // pre-migration legacy org root (`owner--repo`), returned as-is for
    // back-compat. Any further trailing components are ignored, matching
    // the prior tolerance for sub-paths under the org root.
    match comps.next().and_then(|c| c.as_os_str().to_str()) {
        Some(repo_seg) => Some((user_id, format!("{}/{}", owner_seg, repo_seg))),
        None => Some((user_id, owner_seg.to_string())),
    }
}

// SS-11: the SS-4c no-op command wrappers and their no-op assertion tests
// were removed here (uninstall_source_repo / validate_install / heal_install
// / reinstall_source_repo / read_install_lockfile). install_source_repo's
// wrapper + the dead `_inner`/`_legacy` companions move with the machinery
// in SS-11b.

#[cfg(test)]
mod ss4c_parse_tests {
    use super::parse_user_id_and_org_slug_anchored;

    #[test]
    fn parses_canonical_two_segment_layout() {
        // SS-FU-12: canonical org root is `orgs/{owner}/{repo}`.
        let home = "/Users/dev/.codesense";
        let org = "/Users/dev/.codesense/users/42/orgs/acme/widgets-context";
        assert_eq!(
            parse_user_id_and_org_slug_anchored(home, org),
            Some((42, "acme/widgets-context".to_string()))
        );
    }

    #[test]
    fn parses_legacy_single_segment_layout() {
        // A pre-migration `owner--repo` org root is returned as-is.
        let home = "/Users/dev/.codesense";
        let org = "/Users/dev/.codesense/users/42/orgs/acme--widgets-context";
        assert_eq!(
            parse_user_id_and_org_slug_anchored(home, org),
            Some((42, "acme--widgets-context".to_string()))
        );
    }

    #[test]
    fn parses_with_trailing_path_components() {
        // Trailing components beyond `{owner}/{repo}` are ignored.
        let home = "/Users/dev/.codesense";
        let org = "/Users/dev/.codesense/users/42/orgs/acme/widgets-context/extra/segments";
        assert_eq!(
            parse_user_id_and_org_slug_anchored(home, org),
            Some((42, "acme/widgets-context".to_string()))
        );
    }

    #[test]
    fn rejects_unanchored_path() {
        // No `codesense_home` prefix → rejected even though the path
        // contains literal `users/.../orgs/...`.
        let home = "/Users/dev/.codesense";
        let org = "/tmp/users/99/orgs/evil";
        assert!(parse_user_id_and_org_slug_anchored(home, org).is_none());
    }

    #[test]
    fn rejects_wrong_segment_order() {
        let home = "/Users/dev/.codesense";
        let org = "/Users/dev/.codesense/orgs/x/users/9";
        assert!(parse_user_id_and_org_slug_anchored(home, org).is_none());
    }

    #[test]
    fn rejects_non_numeric_user_id() {
        let home = "/Users/dev/.codesense";
        let org = "/Users/dev/.codesense/users/not_a_number/orgs/acme/api";
        assert!(parse_user_id_and_org_slug_anchored(home, org).is_none());
    }
}

// ---------------------------------------------------------------------------
// Tests: R-5 capture_recursive + R-3 init_watched_cache orchestration
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::frontmatter_cache::CacheEntry;
    use std::fs;
    use std::path::PathBuf;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("codesense_test").join(format!(
            "install_cmd_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    fn write_story(root: &Path, team: &str, owner: &str, repo: &str, name: &str, body: &str) {
        // PTS-2: write under the per-team flat layout (the only path the
        // cache walker indexes). `owner` + `repo` are auto-injected into
        // the frontmatter when absent so the orphan-skip doesn't drop the
        // entry.
        let content_dir = root.join(team).join(crate::context_repo::TEAM_CONTENT_DIR);
        fs::create_dir_all(&content_dir).unwrap();
        let bytes = if body.contains("source_repo:") || !body.starts_with("---") {
            body.to_string()
        } else {
            body.replacen("---\n", &format!("---\nsource_repo: {owner}/{repo}\n"), 1)
        };
        fs::write(content_dir.join(name), bytes).unwrap();
    }

    fn stub_entry(path_str: &str, mtime: u64) -> CacheEntry {
        CacheEntry {
            path: PathBuf::from(path_str),
            dir_team: "default".into(),
            fields: crate::commands::pipeline_format::FrontmatterFields {
                team: Some("default".into()),
                source_repo: Some("org/repo".into()),
                status: Some("todo".into()),
                ..Default::default()
            },
            metadata_parse_error: None,
            open_tags: std::collections::BTreeMap::new(),
            mtime,
        }
    }

    // ----- R-5 -----------------------------------------------------------

    // ----- R-3 -----------------------------------------------------------

    /// Cold-start path: empty state + populated org → state populated.
    #[test]
    fn build_persist_merge_cold_start_populates_state() {
        let dir = test_dir("cold");
        write_story(
            &dir,
            "default",
            "org",
            "repo",
            "s.md",
            "---\nstatus: todo\n---\n# S",
        );
        let cache_path = dir.join("cache.json");

        let state = FrontmatterCache::new_shared();
        let count = build_persist_merge(&state, &dir, &cache_path).unwrap();

        assert_eq!(count, 1);
        assert_eq!(state.read().unwrap().entries.len(), 1);
        cleanup(&dir);
    }

    /// Workspace-transition reset: prior workspace's entries must be dropped
    /// by the clear step before the new workspace's entries are merged in.
    /// Mirrors the Tauri command's `clear → build → persist → merge` sequence.
    #[test]
    fn init_sequence_clear_then_merge_drops_prior_workspace_entries() {
        let dir = test_dir("clear_drops_prior");
        write_story(
            &dir,
            "default",
            "neworg",
            "repo",
            "new.md",
            "---\nstatus: todo\n---\n# New",
        );
        let cache_path = dir.join("cache.json");

        // State holds a stale entry from a prior workspace.
        let prior_path =
            PathBuf::from("/tmp/stale/prior-workspace/default/repos/old/repo/stale.md");
        let state = FrontmatterCache::new_shared();
        FrontmatterCache::rcu(&state, |c| {
            // RF-perf-9: through the choke-point so the aux story_id index stays in
            // lockstep (the invariant holds in tests too, not just prod).
            c.upsert_entry(
                prior_path.clone(),
                stub_entry(prior_path.to_str().unwrap(), 42),
            );
        });
        assert_eq!(state.read().unwrap().entries.len(), 1);

        // Simulate the Tauri command's step 1 (clear), then step 3-5
        // (build / persist / merge). Watcher add_watch step is irrelevant
        // for this assertion — there are no watchers in tests.
        FrontmatterCache::rcu(&state, |c| c.clear_entries());
        let count = build_persist_merge(&state, &dir, &cache_path).unwrap();

        assert_eq!(count, 1, "only the new org's entry should remain");
        let locked = state.read().unwrap();
        assert!(
            !locked.entries.contains_key(&prior_path),
            "stale prior-workspace entry must be gone"
        );
        cleanup(&dir);
    }

    /// Race-window preservation: an entry added to state AFTER the clear
    /// (simulating a watcher event during the build window) must survive
    /// the merge — `merge_from` is additive, so paths absent from the
    /// new_cache are preserved.
    #[test]
    fn init_sequence_preserves_watcher_added_entries_during_build_window() {
        let dir = test_dir("preserve_watcher");
        write_story(
            &dir,
            "default",
            "org",
            "repo",
            "scanned.md",
            "---\nstatus: todo\n---\n# S",
        );
        let cache_path = dir.join("cache.json");

        let state = FrontmatterCache::new_shared();
        // Simulated watcher-added entry for a path that the disk scan
        // will NOT see (e.g., created after the walker passed its parent).
        let watcher_path = PathBuf::from("/tmp/watcher-only/story.md");
        FrontmatterCache::rcu(&state, |c| {
            // RF-perf-9: choke-point keeps the aux story_id index consistent.
            c.upsert_entry(
                watcher_path.clone(),
                stub_entry(watcher_path.to_str().unwrap(), 500),
            );
        });

        let count = build_persist_merge(&state, &dir, &cache_path).unwrap();

        // Scanned entry from disk + watcher-added entry both present.
        assert_eq!(count, 2);
        let locked = state.read().unwrap();
        assert!(
            locked.entries.contains_key(&watcher_path),
            "watcher-added entry must survive merge"
        );
        cleanup(&dir);
    }

    /// Persistence side effect: build_persist_merge writes the warm-start
    /// copy to `cache_path` on every call. Confirms the persist step runs
    /// and is fault-tolerant (warn-only on persist error per the contract).
    #[test]
    fn build_persist_merge_writes_warm_start_cache_to_disk() {
        let dir = test_dir("persist");
        write_story(
            &dir,
            "default",
            "org",
            "repo",
            "s.md",
            "---\nstatus: todo\n---\n# S",
        );
        let cache_path = dir.join("cache.json");

        let state = FrontmatterCache::new_shared();
        build_persist_merge(&state, &dir, &cache_path).unwrap();

        assert!(
            cache_path.exists(),
            "warm-start cache file should be persisted"
        );
        let json = fs::read_to_string(&cache_path).unwrap();
        assert!(
            json.contains("\"version\""),
            "persisted cache should be valid JSON with version field"
        );
        cleanup(&dir);
    }

    // ----- SLW-FX-16 (P1: crawl on the blocking pool) --------------------

    /// AC-P1: the disk crawl now runs on the Tokio blocking pool via
    /// `await_blocking`. Driving `build_persist_merge` through the *exact*
    /// production wrapper must produce a byte-identical result to a direct
    /// call — same indexed count, same merged state, warm-start cache written.
    /// This pins that the P1 relocation changed only the execution thread, not
    /// the behavior, and that the `Arc`-cloned handle + owned paths satisfy the
    /// `Send + 'static` bound `spawn_blocking` requires.
    #[tokio::test]
    async fn init_watched_cache_build_runs_through_await_blocking_byte_identical() {
        let dir = test_dir("await_blocking_identity");
        write_story(
            &dir,
            "default",
            "org",
            "repo",
            "s.md",
            "---\nstatus: todo\n---\n# S",
        );
        let cache_path = dir.join("cache.json");

        let state = FrontmatterCache::new_shared();
        // Mirror the command: an Arc-cloned handle + owned PathBufs move into
        // the blocking closure; the original handle observes the merge after.
        let state_handle = state.clone();
        let org_root = dir.clone();
        let cp = cache_path.clone();
        let count = crate::commands::await_blocking("init_watched_cache", move || {
            build_persist_merge(&state_handle, &org_root, &cp)
        })
        .await
        .unwrap();

        assert_eq!(count, 1);
        assert_eq!(state.read().unwrap().entries.len(), 1);
        assert!(
            cache_path.exists(),
            "warm-start cache must be persisted from inside the blocking closure"
        );
        cleanup(&dir);
    }

    /// AC-P1b: because the crawl runs on the blocking pool, a fast concurrent
    /// command is NOT serialized behind it. A slow task — a real
    /// `build_persist_merge` crawl plus a 200ms synchronous stall standing in
    /// for a large first-run crawl — is spawned in the background; a trivial
    /// `await_blocking` command is then issued and timed. If the crawl occupied
    /// the shared async worker, the trivial command would queue behind it and
    /// take ~200ms; on the blocking pool it returns almost immediately.
    ///
    /// The 120ms bound is deliberately BETWEEN the two outcomes (~sub-ms +
    /// dispatch when parallel, ~200ms when serialized) so the assertion
    /// actually discriminates parallel from serial — unlike a bound set above
    /// the serial time, which a regressed (serialized) run would still pass.
    /// The ~80ms slack below the 200ms crawl and the trivial task's near-zero
    /// true cost keep it robust against dispatch latency on contended runners.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn init_watched_cache_crawl_does_not_block_concurrent_command() {
        use std::time::{Duration, Instant};

        let dir = test_dir("p1b_concurrency");
        write_story(
            &dir,
            "default",
            "org",
            "repo",
            "s.md",
            "---\nstatus: todo\n---\n# S",
        );
        let cache_path = dir.join("cache.json");
        let state = FrontmatterCache::new_shared();
        let state_handle = state.clone();
        let org_root = dir.clone();
        let cp = cache_path.clone();

        // Slow crawl running on the blocking pool in the background.
        let slow = tokio::spawn(crate::commands::await_blocking(
            "init_watched_cache",
            move || {
                let c = build_persist_merge(&state_handle, &org_root, &cp)?;
                std::thread::sleep(Duration::from_millis(200));
                Ok::<u32, String>(c)
            },
        ));

        // A trivial command issued while the crawl is in flight. If the crawl
        // held an async worker, this would queue ~200ms behind it.
        let start = Instant::now();
        let other = crate::commands::await_blocking("concurrent_cmd", || Ok::<u32, String>(7))
            .await
            .unwrap();
        let waited = start.elapsed();

        assert_eq!(other, 7);
        assert!(
            waited < Duration::from_millis(120),
            "concurrent command must not serialize behind the ~200ms crawl (waited {:?})",
            waited
        );

        // The crawl still completes correctly and merges into the shared cache.
        let crawl = slow.await.unwrap().unwrap();
        assert_eq!(crawl, 1);
        assert_eq!(state.read().unwrap().entries.len(), 1);
        cleanup(&dir);
    }
}
