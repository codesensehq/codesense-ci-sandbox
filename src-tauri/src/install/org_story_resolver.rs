// SPDX-License-Identifier: MIT
//! SW-AU-51: cross-org story resolution.
//!
//! The active org's story index lives in the single [`SharedFrontmatterCache`]
//! (warm, persisted, FS-watched). Fire paths — connector, watch, loop, reactive,
//! read-model — must resolve a story in ANY *synced* org, not just the active one.
//! Retyping the shared cache or compound-keying its index would ripple through its
//! ~24 active-org readers; instead this resolver is **additive**:
//!
//! - **Active org →** delegate to the resident [`SharedFrontmatterCache`] snapshot.
//!   No second build, no duplicate memory, no staleness (the shared cache is the
//!   one FS-watched copy).
//! - **Non-active synced org →** lazily [`FrontmatterCache::build_from_scan`] the
//!   org's on-disk tree **once**, hold the `Arc` here in-memory (never persisted —
//!   the on-disk warm file is global and belongs to the active org), and serve it
//!   on every later query as an O(1) `Arc` clone. A held index is **point-in-time**:
//!   it is not FS-watched like the active org's cache, so a story ADDED to a
//!   non-active org after its first build is not seen until the process restarts.
//!   This is a bounded v1 limitation, NOT a firing-decision hazard: the fire paths
//!   read a story's *config* (owning packs, transitions, status, sprint selectors)
//!   from disk on every wake, never from this cache — the cache supplies only the
//!   story's stable identity (path / team / source_repo). An EMPTY build (root
//!   present but unpopulated or unreadable) is deliberately NOT held, so a later
//!   sync rebuilds it.
//! - **Never-cloned / unsynced org →** no root on disk, so nothing is scanned or
//!   held; resolution returns an empty index (a clean no-op fire).
//!
//! Story-id uniqueness is per-org by construction: two orgs are two separate
//! caches keyed by `org_slug`, so a story-id reused across orgs can never collide.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use crate::domain::Principal;
use crate::install::frontmatter_cache::{FrontmatterCache, SharedFrontmatterCache};

/// Keep the shared index complete; derive caller visibility before resolving IDs.
pub(crate) fn visible_cache(
    cache: &FrontmatterCache,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> FrontmatterCache {
    let mut visible = cache.clone();
    visible.entries.retain(|path, _| scope.allows(path));
    visible.rebuild_story_id_index();
    visible
}

/// Cross-org story index: `org_slug → that org's frontmatter cache`. Additive to
/// [`SharedFrontmatterCache`]; see the module docs for the delegate/build/no-op rules.
#[derive(Default)]
pub struct OrgStoryResolver {
    /// Lazily-built, in-memory-only per-org indexes. The active org is NEVER a key
    /// here — it is always served from the shared cache — so a workspace switch
    /// needs no eviction to stay correct.
    caches: RwLock<HashMap<String, Arc<FrontmatterCache>>>,
}

impl OrgStoryResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve the frontmatter index for `org_slug`.
    ///
    /// `active_org` is the org whose warm cache is resident in `shared` (the
    /// [`crate::commands::sprint_run`] callers read it from `ActiveTeamState`);
    /// `codesense_home` + `principal` are user-global (every org lives under the
    /// same user home), so a caller passes its own ambient facts unchanged for any
    /// target org.
    ///
    /// Build-once discipline (AC-7): a non-active org is scanned on the FIRST query
    /// only; later queries return the held `Arc` — never a per-tick rescan. The scan
    /// runs OFF the write lock (mirroring [`FrontmatterCache::rebuild_from_disk`]),
    /// so concurrent fire paths never serialize behind it; a double-build race is
    /// benign (the first insert wins, the loser's scan is discarded).
    pub fn snapshot_for(
        &self,
        shared: &SharedFrontmatterCache,
        active_org: Option<&str>,
        codesense_home: &Path,
        principal: &Principal,
        org_slug: &str,
    ) -> Arc<FrontmatterCache> {
        // Active org: the resident warm cache — no build, no duplicate (AC-7).
        if active_org == Some(org_slug) {
            return FrontmatterCache::snapshot(shared);
        }

        // Already built this session: O(1) `Arc` clone (AC-7 — no per-tick rescan).
        if let Some(hit) = self
            .caches
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(org_slug)
        {
            return Arc::clone(hit);
        }

        // AC-6: a never-cloned / unsynced org has no on-disk root — scan nothing,
        // hold nothing (so a later sync builds fresh rather than serving a cached
        // empty), and resolve to an empty index (a clean no-op fire).
        let org_root =
            match crate::context_repo::path_to_org_root(codesense_home, principal, org_slug) {
                Ok(root) => root,
                Err(error) => {
                    log::warn!("{error}");
                    return Arc::new(FrontmatterCache::new());
                }
            };
        if !org_root.is_dir() {
            return Arc::new(FrontmatterCache::new());
        }

        // Cold-build OFF the lock, then publish under a brief write guard.
        let built = Arc::new(FrontmatterCache::build_from_scan(&org_root));
        // Do NOT hold an EMPTY build. An org root can pass `is_dir()` yet yield no entries — it is
        // present but not yet populated (synced-but-empty, mid-sync), or unreadable (`build_from_scan`
        // swallows I/O errors → empty). Holding that would freeze an empty index for the session, so a
        // story synced in later never resolves until restart. Returning it unheld keeps the org on the
        // rebuild path (its stories have nothing to fire while empty anyway — a re-scan of an empty tree
        // is cheap). A non-empty build IS held (the build-once fast path above).
        if built.entries.is_empty() {
            return built;
        }
        let mut guard = self.caches.write().unwrap_or_else(|p| p.into_inner());
        // A concurrent caller may have inserted while we scanned — keep theirs so
        // the held identity is stable (the wasted scan is the only cost).
        if let Some(existing) = guard.get(org_slug) {
            return Arc::clone(existing);
        }
        guard.insert(org_slug.to_string(), Arc::clone(&built));
        built
    }

    /// Whether `org_slug` has a held index (never the active org).
    pub fn is_held(&self, org_slug: &str) -> bool {
        self.caches
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(org_slug)
    }

    /// Drop `org_slug`'s held index so the next [`Self::snapshot_for`] rescans it, for a
    /// lookup that missed in a point-in-time index (a story added after the first build).
    /// `false` when nothing was held (the active org, or one not built yet): a rescan
    /// would find nothing the last query did not already see.
    pub fn evict(&self, org_slug: &str) -> bool {
        self.caches
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(org_slug)
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn test_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("codesense_test").join(format!(
            "org_resolver_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Create `{home}/users/{principal}/orgs/{org}/{team}/content/{file}` with
    /// `source_repo` + `story_id` frontmatter so `build_from_scan` indexes it.
    fn seed_story(
        home: &Path,
        principal: &Principal,
        org: &str,
        team: &str,
        file: &str,
        story_id: &str,
    ) -> PathBuf {
        let org_root = crate::context_repo::path_to_org_root(home, principal, org).unwrap();
        let content = org_root
            .join(team)
            .join(crate::context_repo::TEAM_CONTENT_DIR);
        fs::create_dir_all(&content).unwrap();
        let path = content.join(file);
        fs::write(
            &path,
            format!("---\nsource_repo: acme/{team}\nstory_id: {story_id}\nstatus: todo\n---\n# {story_id}\n"),
        )
        .unwrap();
        path
    }

    #[test]
    fn active_org_delegates_to_shared_cache() {
        // AC-7: the active org is served from the resident shared cache — the
        // resolver never builds or holds it.
        let home = test_home("active_delegates");
        let principal = Principal::from(1u64);
        let shared = FrontmatterCache::new_shared();
        let resolver = OrgStoryResolver::new();

        let got = resolver.snapshot_for(
            &shared,
            Some("acme/active"),
            &home,
            &principal,
            "acme/active",
        );
        let direct = FrontmatterCache::snapshot(&shared);
        assert!(
            Arc::ptr_eq(&got, &direct),
            "active org must return the shared cache snapshot, not a rebuild"
        );
        assert!(
            resolver.caches.read().unwrap().is_empty(),
            "active org must never be held in the resolver map"
        );
    }

    #[test]
    fn non_active_org_builds_once_and_holds() {
        // AC-7: a non-active org is cold-built on first query and served as the
        // SAME held Arc thereafter (no per-tick rescan).
        let home = test_home("build_once");
        let principal = Principal::from(1u64);
        seed_story(&home, &principal, "acme/other", "web", "s1.md", "OTH-1");
        let shared = FrontmatterCache::new_shared();
        let resolver = OrgStoryResolver::new();

        let first = resolver.snapshot_for(
            &shared,
            Some("acme/active"),
            &home,
            &principal,
            "acme/other",
        );
        assert!(
            first.single_by_story_id("OTH-1").is_ok(),
            "cold build must index the non-active org's story"
        );
        let second = resolver.snapshot_for(
            &shared,
            Some("acme/active"),
            &home,
            &principal,
            "acme/other",
        );
        assert!(
            Arc::ptr_eq(&first, &second),
            "second query must serve the held Arc — no rebuild (AC-7)"
        );
    }

    #[test]
    fn unsynced_org_scans_nothing_and_holds_nothing() {
        // AC-6: an org absent locally contributes an empty index and is not held,
        // so a later sync builds fresh.
        let home = test_home("unsynced");
        let principal = Principal::from(1u64);
        let shared = FrontmatterCache::new_shared();
        let resolver = OrgStoryResolver::new();

        let got = resolver.snapshot_for(
            &shared,
            Some("acme/active"),
            &home,
            &principal,
            "acme/ghost",
        );
        assert_eq!(got.entries.len(), 0, "never-cloned org resolves to empty");
        assert!(
            resolver.caches.read().unwrap().is_empty(),
            "never-cloned org must not be held (AC-6)"
        );
    }

    #[test]
    fn present_but_empty_org_is_not_held() {
        // An org root that exists but is empty/unpopulated must NOT be held — else a story synced in
        // later would never resolve. First query resolves empty + holds nothing; after content lands,
        // the next query rebuilds and sees it.
        let home = test_home("empty_not_held");
        let principal = Principal::from(1u64);
        // Create the org root (so `is_dir()` passes) with no team content yet.
        let org_root =
            crate::context_repo::path_to_org_root(&home, &principal, "acme/late").unwrap();
        fs::create_dir_all(&org_root).unwrap();
        let shared = FrontmatterCache::new_shared();
        let resolver = OrgStoryResolver::new();

        let first =
            resolver.snapshot_for(&shared, Some("acme/active"), &home, &principal, "acme/late");
        assert_eq!(
            first.entries.len(),
            0,
            "present-but-empty org resolves empty"
        );
        assert!(
            resolver.caches.read().unwrap().is_empty(),
            "an empty build must not be held"
        );

        // Content lands; the next query rebuilds and now resolves it.
        seed_story(&home, &principal, "acme/late", "web", "LATE-1.md", "LATE-1");
        let second =
            resolver.snapshot_for(&shared, Some("acme/active"), &home, &principal, "acme/late");
        assert!(
            second.single_by_story_id("LATE-1").is_ok(),
            "a story synced after the first (empty) query must resolve on the next"
        );
    }

    #[test]
    fn cross_org_same_story_id_does_not_collide() {
        // AC-5: the same story-id in two orgs resolves to the org it was queried
        // under — the org dimension is the map key, so no collision is possible.
        let home = test_home("no_collide");
        let principal = Principal::from(1u64);
        seed_story(&home, &principal, "acme/alpha", "web", "dup.md", "DUP-1");
        seed_story(&home, &principal, "acme/beta", "api", "dup.md", "DUP-1");
        let shared = FrontmatterCache::new_shared();
        let resolver = OrgStoryResolver::new();

        // Active is a third org so BOTH alpha and beta take the build path.
        let a = resolver.snapshot_for(
            &shared,
            Some("acme/active"),
            &home,
            &principal,
            "acme/alpha",
        );
        let b = resolver.snapshot_for(&shared, Some("acme/active"), &home, &principal, "acme/beta");

        let a_entry = a.single_by_story_id("DUP-1").expect("alpha resolves DUP-1");
        let b_entry = b.single_by_story_id("DUP-1").expect("beta resolves DUP-1");
        assert_eq!(
            a_entry.team(),
            "web",
            "alpha's DUP-1 is the web team's story"
        );
        assert_eq!(
            b_entry.team(),
            "api",
            "beta's DUP-1 is the api team's story"
        );
    }

    #[test]
    fn evicting_a_held_org_rescans_it_and_the_active_org_is_never_held() {
        // A story written to a non-active org after its first build is invisible to the
        // held index; evicting the org makes the next query see it.
        let home = test_home("evict_rescan");
        let principal = Principal::from(1u64);
        seed_story(&home, &principal, "acme/other", "web", "s1.md", "OTH-1");
        let shared = FrontmatterCache::new_shared();
        let resolver = OrgStoryResolver::new();
        let first = resolver.snapshot_for(
            &shared,
            Some("acme/active"),
            &home,
            &principal,
            "acme/other",
        );
        seed_story(&home, &principal, "acme/other", "web", "s2.md", "OTH-2");
        assert!(first.single_by_story_id("OTH-2").is_err());

        assert!(resolver.is_held("acme/other"));
        assert!(resolver.evict("acme/other"), "a held org is evicted");
        assert!(!resolver.is_held("acme/other"));
        let rebuilt = resolver.snapshot_for(
            &shared,
            Some("acme/active"),
            &home,
            &principal,
            "acme/other",
        );
        assert!(rebuilt.single_by_story_id("OTH-2").is_ok());

        assert!(
            !resolver.evict("acme/active"),
            "the active org is never held"
        );
        assert!(
            !resolver.evict("acme/ghost"),
            "an unbuilt org has nothing to rescan"
        );
    }
}
