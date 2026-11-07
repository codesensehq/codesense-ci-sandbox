// SPDX-License-Identifier: MIT
//! In-memory frontmatter index for all story files across all teams.
//!
//! Scans `{org_root}/*/repos/*/*/` (legacy per-team-per-repo layout),
//! parses each pipeline file's frontmatter, and caches the result keyed by
//! absolute path. Supports incremental updates, disk persistence with mtime
//! diffing, and query by team / source_repo / both.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::UNIX_EPOCH;

use log::{debug, info, trace, warn};
use serde::{Deserialize, Serialize};

use crate::commands::pipeline_format;
use crate::commands::pipeline_format::{FrontmatterFields, PipelineFileFormat};
use crate::commands::pipeline_scanner;
use crate::context_repo::TEAM_CONTENT_DIR;

/// Current schema version. Bump when CacheEntry shape changes.
///
/// R-14: bumped to 2 — `CacheEntry` now composes `FrontmatterFields`
/// directly instead of enumerating individual fields.
/// PTS-2: bumped to 3 — `CacheEntry` retires the `dir_source_repo`
/// fallback field (legacy walk gone, every entry has frontmatter
/// `source_repo`). v2 caches on disk fail version check at
/// `load_or_build` and trigger a full rebuild (cheap — warm scan
/// of `_team/*/content/`).
/// SW-FX-9: bumped to 4 — `CacheEntry.mtime` and the `dir_mtimes`
/// values now carry epoch **nanoseconds** instead of seconds (see
/// `mtime_of`). A v3 cache on disk holds second-scale mtimes that would
/// never compare equal to the new ns stat, so it must not be trusted; the
/// version check forces a cheap cold rebuild.
/// SW-DX-2 (AC-13): bumped to 5 — `CacheEntry.open_tags` is now populated at
/// build time. A v4 cache on disk has empty `open_tags` for every entry (its
/// `#[serde(default)]`), which would silently strip open tags from every
/// scanned `StoryRef` on the cache-hit path until each file's mtime happened to
/// change. The version bump forces a cheap cold rebuild so open tags are present
/// immediately.
/// SW-SB-1: bumped to 6 — `story_meta_to_fields` now carries `sprint`/`points`/
/// `assigned_to` into `CacheEntry.fields` (SB-9 previously hardcoded `points:
/// None`, and the two strings are new). A v5 cache on disk has `None` for all
/// three (their `#[serde(default)]`), which the cache-hit path would serve onto
/// every `StoryRef` — silently blanking the board's metadata until each file's
/// mtime happened to change. Same failure mode as the v4→5 open-tags bump above;
/// the version bump forces a cheap cold rebuild so the fields are present at once.
///
/// SW-SB-22: bumped to 7 — `FrontmatterFields` (and thus `CacheEntry.fields`, R-14)
/// gains `rank`, the board-ordering string index. A v6 cache on disk has `None`
/// for it (`#[serde(default)]`), which the cache-hit path would serve onto every
/// `StoryRef` — leaving the board unsorted until each file's mtime changed. Same
/// remedy as the 5→6 bump: one cheap cold rebuild so `rank` is present at once.
///
/// SW-AU-77F: bumped to 8 — `FrontmatterFields` gains `tags`. A v7 cache has `None`
/// for it (`#[serde(default)]`), which the cache-hit path would serve onto every
/// `StoryRef`, so `tags contains X` filters would silently never match an
/// already-cached story until its mtime changed. One cold rebuild populates it.
///
/// SW-AU-77H: bumped to 9 — `FrontmatterFields` (and thus `CacheEntry.fields`, R-14)
/// gains `subject_id`, the standalone-doc automation subject marker. A v8 cache on
/// disk has `None` for it (`#[serde(default)]`); one cheap cold rebuild makes it
/// present at once rather than only after each file's mtime changes.
/// Version 10 recovers plain colon values, preserves their badge diagnostic,
/// and removes the unused completed-date timestamp from cached entries.
const CACHE_VERSION: u32 = 10;

// ---------------------------------------------------------------------------
// CacheEntry
// ---------------------------------------------------------------------------

/// A single indexed story file. **Composes `FrontmatterFields` directly** so
/// new YAML fields added to the pipeline format module flow through to the
/// cache automatically. The cache is a snapshot of the parser's output;
/// spec evolution does not require schema-mirroring updates here.
///
/// Invariant (R-14): `CacheEntry.fields` IS the parser's `FrontmatterFields`
/// (same type, not a parallel struct). When `FrontmatterFields` gains a new
/// YAML field, the cache inherits it for free. Callers access frontmatter
/// values via the accessor methods (`status()`, `team()`, etc.) rather than
/// direct field access — `team()` and `source_repo()` encode the
/// "frontmatter override OR directory-inferred fallback" precedence.
///
/// Body-extracted fields (`title` from H1, `story_text` from `## Story`)
/// are intentionally NOT cached here — they're body structure, not
/// frontmatter, and are fetched on-demand via `get_story_details`
/// (R-14 Part B). Coupling the cache to body conventions would defeat
/// the spec-evolution principle above.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub path: PathBuf,
    /// Directory-inferred team (always from `_team/{team}/` path segment).
    /// Used as fallback when frontmatter `team` is absent.
    pub dir_team: String,
    /// Composed parser output — the single source of YAML-derived fields.
    pub fields: FrontmatterFields,
    /// Parse diagnostic captured with the fields; reused by warm pipeline scans.
    #[serde(default)]
    pub metadata_parse_error: Option<String>,
    /// SW-DX-2 (AC-13): the doc's open/arbitrary frontmatter tags, captured once
    /// at cache-build via `frontmatter_editor::open_tag_values` (same read that
    /// parsed the fields). The scanner's cache-hit path lifts this straight onto
    /// `StoryRef.open_tags` with zero re-read, so the query layer filters open
    /// tags off the warm cache. `#[serde(default)]` (empty) on pre-v5 caches —
    /// which are anyway invalidated by the `CACHE_VERSION` bump.
    #[serde(default)]
    pub open_tags: std::collections::BTreeMap<String, Vec<String>>,
    pub mtime: u64,
}

impl CacheEntry {
    /// Effective team: frontmatter override if present, else directory-inferred.
    pub fn team(&self) -> &str {
        self.fields.team.as_deref().unwrap_or(&self.dir_team)
    }

    /// Effective source_repo slug.
    ///
    /// PTS-2 retires the directory-inferred fallback: the only walker
    /// (`for_each_indexable_dir`) drops the legacy arm, and
    /// `build_cache_entry`'s orphan-skip guarantees every cached entry has
    /// frontmatter `source_repo`. The accessor still returns `&str` (not
    /// `Option`) so callers don't churn; on the invariant-broken-by-bug
    /// branch it returns `""` rather than panicking.
    pub fn source_repo(&self) -> &str {
        self.fields.source_repo.as_deref().unwrap_or("")
    }

    pub fn status(&self) -> Option<&str> {
        self.fields.status.as_deref()
    }

    #[allow(dead_code)]
    pub fn document_type(&self) -> Option<&str> {
        self.fields.document_type.as_deref()
    }

    #[allow(dead_code)]
    pub fn story_id(&self) -> Option<&str> {
        self.fields.story_id.as_deref()
    }

    /// SW-GS-35: the story's dev branch (the git branch its code lives on),
    /// written at worktree establishment. `None` for older/never-opened docs.
    /// Consumed by the reviewer flow (SW-DX-12) to check out a pulled story.
    #[allow(dead_code)]
    pub fn dev_branch(&self) -> Option<&str> {
        self.fields.dev_branch.as_deref()
    }

    /// SW-WB-1: the story's **intended** git branch — the user-editable input
    /// that drives worktree creation and the worktree path (via the FE
    /// `resolveWorktreeParams`). Distinct from [`dev_branch`](Self::dev_branch),
    /// the *observed* branch written after a worktree exists. `None` (or empty)
    /// ⇒ the resolver falls back to `{branch_prefix}{story_id}` — so a cold,
    /// never-opened story still resolves a branch (SW-OR-1a).
    #[allow(dead_code)]
    pub fn branch_name(&self) -> Option<&str> {
        self.fields.branch_name.as_deref()
    }

    /// SW-OR-1b: a story's single canonical branch, resolved by precedence
    /// `branch` → `branch_name` → `dev_branch` (first present + non-blank). Going
    /// forward a story records just `branch`; the legacy `branch_name` (intended)
    /// and `dev_branch` (observed) are read only as a fallback for docs authored
    /// before the merge — never written by new code. Drives both worktree
    /// creation and the reconcile-on-open stamp, so intent and observation
    /// collapse to one value. `None` when all three are absent/blank ⇒ the
    /// resolver falls back to `{branch_prefix}{story_id}`.
    pub fn story_branch(&self) -> Option<&str> {
        [
            self.fields.branch.as_deref(),
            self.fields.branch_name.as_deref(),
            self.fields.dev_branch.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
    }

    #[allow(dead_code)]
    pub fn depends_on(&self) -> Option<&[String]> {
        self.fields.depends_on.as_deref()
    }

    #[allow(dead_code)]
    pub fn supporting_docs(&self) -> Option<&[String]> {
        self.fields.supporting_docs.as_deref()
    }
}

/// Why [`FrontmatterCache::single_by_story_id`] couldn't resolve a lone story.
/// Kept distinct (rather than a bare `Option`) so callers can render the two
/// cases differently — "no such story" vs a genuine id collision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoryIdLookup {
    /// No managed doc declares this `story_id`.
    NotFound,
    /// 2+ managed docs share this `story_id` (carries the count for the message).
    Ambiguous(usize),
}

// ---------------------------------------------------------------------------
// FrontmatterCache
// ---------------------------------------------------------------------------

/// In-memory index of story files across all teams in an org context repo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontmatterCache {
    pub version: u32,
    /// The indexed story files, keyed by absolute path.
    ///
    /// RF-perf-9: **mutate through [`upsert_entry`](Self::upsert_entry) /
    /// [`drop_entry`](Self::drop_entry) / [`clear_entries`](Self::clear_entries)**,
    /// not `entries.insert`/`remove`/`clear` directly, so the derived
    /// [`story_id_index`](Self::story_id_index) stays in lockstep. The only
    /// exception is **bulk construction** (`scan_dir_recursive` / `mtime_diff` /
    /// `discover_new_files_gated`), which may insert directly **but MUST** be
    /// followed by [`rebuild_story_id_index`](Self::rebuild_story_id_index) before
    /// the cache is published/queried (as `build_from_scan` / `load_or_build` do).
    pub entries: HashMap<PathBuf, CacheEntry>,
    /// R-7: Per-directory mtimes captured during scan. Warm-start's new-file
    /// discovery skips `read_dir` on directories whose mtime is unchanged
    /// since persist. `#[serde(default)]` keeps v1 caches deserializable —
    /// an empty map on first read after upgrade naturally falls back to a
    /// full walk (every directory is "first-seen", which forces a re-scan +
    /// populates the map for subsequent warm starts).
    #[serde(default)]
    pub dir_mtimes: HashMap<PathBuf, u64>,
    /// RF-perf-9: auxiliary `story_id → paths` index so `by_story_id` is `O(1)`
    /// instead of an `O(N)` scan of `entries` (the measured #1 hot path — the MCP
    /// `validate_story` `DUPLICATE_STORY_ID` point lookup + `hydrate`/`dehydrate`). A `Vec`
    /// value because story-id uniqueness is not guaranteed (the callers already
    /// treat `2+` as ambiguous). **Derived state, not authoritative** — it is a
    /// pure function of `entries`, so it is **not** persisted (`#[serde(skip)]`)
    /// and is rebuilt from `entries` on load (post-deserialize) and at
    /// `build_from_scan`. To keep it from ever drifting, `entries` is mutated
    /// **only** through [`upsert_entry`](Self::upsert_entry) /
    /// [`drop_entry`](Self::drop_entry) / [`clear_entries`](Self::clear_entries)
    /// on the incremental (shared, RCU) paths; the construction paths rebuild it
    /// wholesale via [`rebuild_story_id_index`](Self::rebuild_story_id_index).
    /// This does **not** touch the KEEP-listed RCU copy-on-write.
    #[serde(skip)]
    story_id_index: HashMap<String, Vec<PathBuf>>,
    /// Set by the entry mutators when a story id or team changed; the publish that
    /// carries the change takes it and counts the change once the snapshot is swapped in.
    #[serde(skip)]
    keys_changed: bool,
}

/// SLW-RI-3: the cache is shared as an **immutable snapshot** behind
/// `RwLock<Arc<_>>` — a lock around a snapshot *pointer*, not around the data.
///
/// - **Readers** clone the inner `Arc` (O(1)) under a brief read lock and drop
///   the guard before any heavy work, so `materialize` runs against the snapshot
///   with **no lock held** — a reader can never wait on a concurrent projection.
/// - **Writers** RCU (`Self::rcu`): clone the current snapshot, mutate, publish.
///   The write lock is held only across the cheap map clone + mutation, **never**
///   across `materialize`.
///
/// This is what let SLW-RI-3 delete the global `SWITCH_LOCK`: the read guard is
/// no longer held across the projection, so disjoint projections stop serializing
/// on it, and the only ordering left (per-target waves, reverse echo) is owned by
/// SLW-RI-4's keyed locks.
pub type SharedFrontmatterCache = Arc<RwLock<Arc<FrontmatterCache>>>;

/// The app's shared cache, for readers that run without Tauri state (usage story totals).
static GLOBAL: OnceLock<SharedFrontmatterCache> = OnceLock::new();

/// How many times any cache's indexed story ids or teams have changed (an entry added,
/// removed, or re-keyed); a refresh that leaves them as they were does not count.
static PUBLISHES: AtomicU64 = AtomicU64::new(0);

impl FrontmatterCache {
    pub fn new() -> Self {
        Self {
            version: CACHE_VERSION,
            entries: HashMap::new(),
            dir_mtimes: HashMap::new(),
            story_id_index: HashMap::new(),
            keys_changed: false,
        }
    }

    // ----- RF-perf-9: entry mutation choke-point + story_id index ----------

    /// RF-perf-9: insert/replace `entry` at `path`, keeping [`story_id_index`]
    /// consistent — the single incremental write path (with [`drop_entry`] /
    /// [`clear_entries`]) so the aux index can never drift from `entries`. If a
    /// prior entry at `path` carried a different `story_id` (a rename / id edit),
    /// its stale index mapping is removed before the new one is added.
    pub(crate) fn upsert_entry(&mut self, path: PathBuf, entry: CacheEntry) {
        let rekeyed = self
            .entries
            .get(&path)
            .is_none_or(|old| old.story_id() != entry.story_id() || old.dir_team != entry.dir_team);
        self.keys_changed |= rekeyed;
        if let Some(old) = self.entries.get(&path) {
            if let Some(old_id) = old.story_id() {
                // Only churn the index when the id actually changed.
                if old.story_id() != entry.story_id() {
                    Self::index_remove(&mut self.story_id_index, old_id, &path);
                }
            }
        }
        if let Some(id) = entry.story_id() {
            // Re-add unless the (unchanged-id) mapping is already present.
            Self::index_add(&mut self.story_id_index, id, &path);
        }
        self.entries.insert(path, entry);
    }

    /// RF-perf-9: remove the entry at `path`, dropping its `story_id_index`
    /// mapping. Returns the removed entry (mirrors `HashMap::remove`).
    pub(crate) fn drop_entry(&mut self, path: &Path) -> Option<CacheEntry> {
        let removed = self.entries.remove(path);
        self.keys_changed |= removed.is_some();
        if let Some(ref entry) = removed {
            if let Some(id) = entry.story_id() {
                Self::index_remove(&mut self.story_id_index, id, path);
            }
        }
        removed
    }

    /// RF-perf-9: clear both `entries` and the aux index in one shot (used by the
    /// workspace-transition reset, which RCU-clears the cache).
    pub(crate) fn clear_entries(&mut self) {
        self.keys_changed = true;
        self.entries.clear();
        self.story_id_index.clear();
    }

    /// RF-perf-9: recompute [`story_id_index`] from `entries`. Used by the
    /// construction paths (`build_from_scan`, post-deserialize warm start) where
    /// `entries` is populated in bulk without going through `upsert_entry`. `O(N)`,
    /// but only at cold/warm start — the incremental paths never call it.
    pub(crate) fn rebuild_story_id_index(&mut self) {
        let mut index: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for (path, entry) in &self.entries {
            if let Some(id) = entry.story_id() {
                index.entry(id.to_string()).or_default().push(path.clone());
            }
        }
        self.story_id_index = index;
    }

    /// Add `path` to `id`'s bucket unless already present (idempotent).
    fn index_add(index: &mut HashMap<String, Vec<PathBuf>>, id: &str, path: &Path) {
        let bucket = index.entry(id.to_string()).or_default();
        if !bucket.iter().any(|p| p == path) {
            bucket.push(path.to_path_buf());
        }
    }

    /// Remove `path` from `id`'s bucket; drop the bucket entirely when it empties
    /// (so a `by_story_id` lookup never has to skip empty buckets).
    fn index_remove(index: &mut HashMap<String, Vec<PathBuf>>, id: &str, path: &Path) {
        if let Some(bucket) = index.get_mut(id) {
            bucket.retain(|p| p != path);
            if bucket.is_empty() {
                index.remove(id);
            }
        }
    }

    // ----- Build from scan -------------------------------------------------

    /// Cold-start: walk all story files under the per-team flat layout
    /// (`_team/*/content/`) and index their frontmatter. Legacy
    /// `_team/*/repos/*/*/_bmad-output/` paths exist on disk post-migration
    /// (PTS-4 deletes them later) but are NOT indexed.
    pub fn build_from_scan(org_root: &Path) -> Self {
        let mut cache = Self::new();
        for_each_indexable_dir(org_root, |team, dir| {
            scan_dir_recursive(dir, team, &mut cache);
        });
        // RF-perf-9: the scan populates `entries` in bulk via free functions; derive
        // the aux index once here (cold path) rather than per-insert.
        cache.rebuild_story_id_index();
        info!(target: "frontmatter_cache", "build_from_scan: indexed {} entries", cache.entries.len());
        cache
    }

    /// Cold-build a cache indexing exactly one team's `{team}/content` dir.
    ///
    /// `build_from_scan` walks a whole org (`{org}/{team}/content` for every
    /// team); this indexes a single already-resolved content root at the same
    /// granularity a caller scans it — the dashboard hub scans per connected
    /// content root, not per org, so it warms each root with its own cache
    /// rather than sharing the active-org cache (which would miss cross-org
    /// roots). Reuses the same `scan_dir_recursive` + `rebuild_story_id_index`
    /// internals, so entries key identically to what a scan of `content_root`
    /// looks up. `team` is the directory-inferred team the entries carry as
    /// their `dir_team` fallback.
    pub fn build_from_content_root(content_root: &Path, team: &str) -> Self {
        let mut cache = Self::new();
        scan_dir_recursive(content_root, team, &mut cache);
        cache.rebuild_story_id_index();
        cache
    }

    /// SW-CR-9: bounded disk scan for a file whose frontmatter carries `story_id`
    /// under the org's content tree. Walks the SAME indexable dirs as
    /// `build_from_scan` (`{team}/content/`), parses each pipeline candidate's
    /// frontmatter in-situ, and returns the first match as `(path, dir_team)` —
    /// the inputs `refresh_under_lock` expects.
    ///
    /// Used by the `resolve_story_path` fallback: when the cache misses a
    /// `story_id` (an agent dropped a story MD via a raw filesystem writer so
    /// no watcher event closed the loop), this names the file on disk so a
    /// single targeted refresh can heal the index without a full org rebuild.
    /// Scope is deliberately narrow: the first-hit scan ends as soon as a
    /// candidate is found, so the cost on a cache miss is O(stories-until-hit)
    /// not O(entire tree), and the hot (cache-hit) path never pays it.
    pub fn scan_org_for_story_id(org_root: &Path, story_id: &str) -> Option<(PathBuf, String)> {
        Self::scan_org_for_story(org_root, |_, id| id == story_id)
            .map(|(path, team, _)| (path, team))
    }

    /// [`scan_org_for_story_id`](Self::scan_org_for_story_id) for a story chosen by
    /// `(team, story_id)`; also returns the id found.
    pub fn scan_org_for_story(
        org_root: &Path,
        pick: impl Fn(&str, &str) -> bool,
    ) -> Option<(PathBuf, String, String)> {
        let mut found: Option<(PathBuf, String, String)> = None;
        for_each_indexable_dir(org_root, |team, dir| {
            if found.is_some() {
                return;
            }
            if let Some((path, id)) = scan_dir_for_story(dir, &|id| pick(team, id)) {
                found = Some((path, team.to_string(), id));
            }
        });
        found
    }

    /// Every `(path, dir_team, story_id)` of the pipeline files under the org's indexable
    /// dirs that declare a story id, whether or not they carry a `source_repo`. One walk.
    pub fn scan_org_story_ids(org_root: &Path) -> Vec<(PathBuf, String, String)> {
        let mut all = Vec::new();
        for_each_indexable_dir(org_root, |team, dir| {
            scan_dir_story_ids(dir, &mut |path, id| {
                all.push((path.to_path_buf(), team.to_string(), id.to_string()));
                false
            });
        });
        all
    }

    /// SW-HB-8: force a full rebuild of the shared cache from disk, replacing
    /// every entry under one RCU publish. Used after a team rename, which MOVES
    /// `_team/{old}` → `_team/{new}` while preserving mtimes — so the warm-start
    /// `mtime_diff` path can't self-heal it and the moved stories would
    /// otherwise stay indexed under the old path + `dir_team` (projections for
    /// the renamed team returning empty until a cold restart). Reuses
    /// `build_from_scan` (the same scan the cold path uses); `by_team(new)` then
    /// resolves and `by_team(old)` is empty, since `dir_team` is re-derived from
    /// the new path.
    pub fn rebuild_from_disk(cache: &SharedFrontmatterCache, org_root: &Path) {
        // Scan OUTSIDE the write lock — a full-org disk walk must not block
        // `snapshot()` readers or the watcher. Since this is a FULL replacement
        // (not an incremental mutation) we swap the published `Arc` directly
        // rather than via `rcu`, which would needlessly clone the old cache we're
        // about to discard AND run the scan inside its guard. Only the pointer
        // swap is under the lock; poison-tolerant like `rcu`.
        let fresh = Self::build_from_scan(org_root);
        let mut guard = cache
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Arc::new(fresh);
        AtomicU64::fetch_add(&PUBLISHES, 1, Ordering::AcqRel);
    }

    // ----- Query -----------------------------------------------------------

    #[allow(dead_code)] // Public API — consumers land in future install stories
    pub fn by_team(&self, team: &str) -> Vec<&CacheEntry> {
        self.entries.values().filter(|e| e.team() == team).collect()
    }

    #[allow(dead_code)]
    pub fn by_source_repo(&self, slug: &str) -> Vec<&CacheEntry> {
        self.entries
            .values()
            .filter(|e| e.source_repo() == slug)
            .collect()
    }

    /// SW-TS-1: every indexed story file declaring `story_id == story_id`.
    /// The `story_id → story file` source for the worktree-index
    /// branch-name fallback (`install::worktree_index`). Normally 0 or 1
    /// (story ids are unique per org); a 2+ result is treated as ambiguous
    /// by the fallback and resolves to neutral.
    pub fn by_story_id(&self, story_id: &str) -> Vec<&CacheEntry> {
        // RF-perf-9: O(1) aux-index lookup (was an O(N) `entries` scan — the measured
        // #1 hot path). Always-on frequency tally + gated per-call timing (reuses the
        // one RF-perf-0 gate); the logged `elapsed_us≈0` is the live proof of the win.
        let t = crate::install::frontmatter_perf::start_timer();
        let out: Vec<&CacheEntry> = self
            .story_id_index
            .get(story_id)
            .into_iter()
            .flatten()
            .filter_map(|path| self.entries.get(path))
            .collect();
        crate::install::frontmatter_perf::note_story_id_lookup(self.entries.len());
        if let Some(t0) = t {
            debug!(
                target: crate::install::frontmatter_perf::SYNC_TIMING_TARGET,
                "frontmatter_story_id_lookup entries={} matched={} elapsed_us={}",
                self.entries.len(),
                out.len(),
                t0.elapsed().as_micros(),
            );
        }
        out
    }

    /// Resolve `story_id` to EXACTLY ONE managed [`CacheEntry`], or the reason it
    /// couldn't ([`StoryIdLookup::NotFound`] / [`StoryIdLookup::Ambiguous`]). The
    /// single arity gate shared by every "which story is this?" caller so they
    /// agree on what "the story" means — the FE open (`resolve_story_path`), the
    /// trusted MCP cold-create (`ensure_story_worktree`, SW-OR-1a), and the
    /// worktree-index branch-name fallback (`.ok()` collapses both errors to a
    /// neutral skip). Story ids are unique within an org, so a 2+ result is a
    /// genuine cross-doc collision (surfaced elsewhere as `DUPLICATE_STORY_ID`);
    /// each caller maps the outcome to its own error vocabulary.
    pub fn single_by_story_id(&self, story_id: &str) -> Result<&CacheEntry, StoryIdLookup> {
        match self.by_story_id(story_id).as_slice() {
            [entry] => Ok(entry),
            [] => Err(StoryIdLookup::NotFound),
            more => Err(StoryIdLookup::Ambiguous(more.len())),
        }
    }

    /// SW-AU-45: the owning team of the lone story with this `story_id`, or `None` when it is not a
    /// single resolvable story (unknown / ambiguous id) or its team is blank. The same
    /// `single_by_story_id → team()` resolution the watch scheduler uses per story — reused by the
    /// reactive fire to target an agent's owning team from its handle. A non-story handle simply
    /// misses the id index → `None` (a clean drop, never a fall-through to the active team).
    pub fn team_for_story_id(&self, story_id: &str) -> Option<String> {
        let team = self.single_by_story_id(story_id).ok()?.team();
        (!team.is_empty()).then(|| team.to_string())
    }

    // RF-perf-9: the former `story_id_paths()` org-wide `(id, path)` enumeration is
    // retired. Its only consumer — the MCP `validate_story` `DUPLICATE_STORY_ID`
    // check — now does an `O(1)` `by_story_id` point lookup through the
    // `StoryIdIndex` seam (`mcp::handler`), so materializing every pair on each call
    // (the measured #1 hot path) is gone. Use `by_story_id` for the point lookup.

    /// R-7: Returns an iterator instead of an allocated `Vec`. Callers that
    /// need indexed access or `.len()` should `.collect::<Vec<_>>()` upfront;
    /// callers that only iterate (or call `.count()` on a fresh derivation)
    /// benefit from zero allocation. `by_team` and `by_source_repo` keep
    /// their `Vec` return type — neither has production callers outside
    /// tests, so the surface change is intentionally minimal.
    pub fn by_team_and_repo<'a>(
        &'a self,
        team: &'a str,
        slug: &'a str,
    ) -> impl Iterator<Item = &'a CacheEntry> + 'a {
        self.entries
            .values()
            .filter(move |e| e.team() == team && e.source_repo() == slug)
    }

    /// SW-CP-15: is `path` indexed in the frontmatter cache? The whole-root
    /// projection walk (`ProjectionFilter::WholeRoot`) uses this to skip files
    /// the always-on staleness story floor already owns (every cache-indexed,
    /// `source_repo`-tagged file), so whole-root projection never
    /// double-projects a story or un-drops a stale epic the floor filtered.
    pub fn indexes_path(&self, path: &Path) -> bool {
        self.entries.contains_key(path)
    }

    // ----- Incremental update (T8) -----------------------------------------

    #[allow(dead_code)]
    pub fn update_entry(&mut self, path: PathBuf, entry: CacheEntry) {
        self.upsert_entry(path, entry);
    }

    #[allow(dead_code)]
    pub fn remove_entry(&mut self, path: &Path) -> Option<CacheEntry> {
        self.drop_entry(path)
    }

    /// R-2: Disk read phase of a cache refresh — runs **without** any cache
    /// lock. Splitting this from the in-memory commit lets the watcher avoid
    /// holding `RwLock::write()` across the `build_cache_entry` file read/parse.
    ///
    /// Returns the built `CacheEntry` (or `None` if the file isn't a pipeline
    /// file / is unreadable). Does **not** touch `self.entries` — that's
    /// `commit_refresh`'s job. The refresh is read-only: it never mutates the
    /// source file (SR-13 removed the status-transition write that used to run
    /// here).
    ///
    /// Because this is an associated function with no `self`, it's
    /// compile-time impossible to acquire the cache lock from here.
    ///
    /// Implementation phase of [`refresh_under_lock`]. External callers
    /// should prefer `refresh_under_lock` — `pub(crate)` here is for tests
    /// and for the composed `refresh_under_lock` itself.
    pub(crate) fn prepare_refresh(path: &Path, dir_team: &str) -> Option<CacheEntry> {
        build_cache_entry(path, dir_team)
    }

    /// R-2 + R-5: Single-parse variant of `prepare_refresh`. Takes
    /// caller-supplied `FrontmatterFields` and skips the body read + re-parse.
    /// (SW-CP-18: its former prod caller `capture::capture_file` is retired, so
    /// this path is currently exercised only by tests.) Stats the file for
    /// `mtime`. Lock-free — does not touch `self.entries`.
    ///
    /// Honours the F.30 HTML-empty gate: HTML files whose `fields.is_empty()`
    /// are treated as non-pipeline documents (no insert).
    pub(crate) fn prepare_refresh_from_fields(
        path: &Path,
        fields: FrontmatterFields,
        dir_team: &str,
    ) -> Option<CacheEntry> {
        if is_html_path(path) && fields.is_empty() {
            None
        } else {
            let mtime = mtime_of(path).unwrap_or(0);
            // SW-DX-2 (AC-13): this caller-supplied-fields hand-off path (test-
            // only per the doc-comment) has no raw content to walk, so open tags
            // are empty. The prod index path (`build_cache_entry`) populates them.
            Some(build_cache_entry_from_fields(
                path,
                fields,
                mtime,
                dir_team,
                std::collections::BTreeMap::new(),
            ))
        }
    }

    /// R-2: In-memory commit phase. Holds the cache write lock briefly to
    /// remove the old entry and insert the new one. No disk I/O.
    ///
    /// Returns `(old_entry, new_entry)` so the caller can diff
    /// team/source_repo changes. `old_entry` is whatever
    /// was actually in the map at commit time — which may differ from what
    /// `prepare_refresh`'s `old_status` saw if another writer interleaved.
    /// See *Concurrent writers* in the R-2 story for why that's benign.
    ///
    /// Implementation phase of [`refresh_under_lock`]. External callers
    /// should prefer `refresh_under_lock`.
    pub(crate) fn commit_refresh(
        &mut self,
        path: &Path,
        new: Option<CacheEntry>,
    ) -> (Option<CacheEntry>, Option<CacheEntry>) {
        // RF-perf-9: through the choke-point so `story_id_index` tracks the swap.
        let flagged = self.keys_changed;
        let old = self.drop_entry(path);
        if let Some(ref entry) = new {
            self.upsert_entry(path.to_path_buf(), entry.clone());
        }
        // A refresh swaps the entry; only a different story id or team is a key change.
        let same_keys = match (&old, &new) {
            (Some(was), Some(now)) => {
                was.story_id() == now.story_id() && was.dir_team == now.dir_team
            }
            (None, None) => true,
            _ => false,
        };
        self.keys_changed = flagged || !same_keys;
        (old, new)
    }

    /// SLW-RI-3: wrap an owned cache as a shared snapshot handle
    /// (`Arc<RwLock<Arc<_>>>`). Production registers this in Tauri managed state;
    /// tests use it (and [`new_shared`](Self::new_shared)) instead of hand-rolling
    /// the nesting.
    pub fn shared(cache: Self) -> SharedFrontmatterCache {
        Arc::new(RwLock::new(Arc::new(cache)))
    }

    /// Register the app's shared cache so state-less readers can take its snapshot.
    pub fn register_global(cache: &SharedFrontmatterCache) {
        let _ = OnceLock::set(&GLOBAL, Arc::clone(cache));
    }

    /// Index the pipeline files under `org_root` into the app's shared cache, registering an
    /// empty one first when none is (tests only: the process shares one).
    #[cfg(test)]
    pub(crate) fn index_into_global(org_root: &Path) {
        let shared = Arc::clone(OnceLock::get_or_init(&GLOBAL, Self::new_shared));
        let built = Self::build_from_scan(org_root);
        Self::rcu(&shared, |cache| {
            for (path, entry) in built.entries {
                cache.upsert_entry(path, entry);
            }
        });
    }

    /// The registered shared cache's current snapshot; `None` before registration.
    pub fn global_snapshot() -> Option<Arc<FrontmatterCache>> {
        OnceLock::get(&GLOBAL).map(Self::snapshot)
    }

    /// Counts changes to any cache's story ids or teams; a memo stamped with it is stale once it moves.
    pub fn publish_generation() -> u64 {
        AtomicU64::load(&PUBLISHES, Ordering::Acquire)
    }

    /// The story under `org_root` that `pick(dir_team, story_id)` chooses, from the index.
    /// `None` when no indexed file lies under `org_root` (the index does not cover that org);
    /// otherwise `Some` of the match, the lowest path when several match.
    pub fn named_story_under(
        &self,
        org_root: &Path,
        pick: impl Fn(&str, &str) -> bool,
    ) -> Option<Option<(PathBuf, String)>> {
        let mut covered = false;
        let mut found: Option<(&PathBuf, &str)> = None;
        for (path, entry) in &self.entries {
            if !Path::starts_with(path, org_root) {
                continue;
            }
            covered = true;
            let Some(id) = CacheEntry::story_id(entry) else {
                continue;
            };
            if pick(&entry.dir_team, id) && Option::is_none_or(found, |(best, _)| path < best) {
                found = Some((path, id));
            }
        }
        bool::then(covered, || {
            Option::map(found, |(path, id)| (path.clone(), id.to_string()))
        })
    }

    /// SLW-RI-3: construct an empty shared cache handle.
    pub fn new_shared() -> SharedFrontmatterCache {
        Self::shared(Self::new())
    }

    /// SLW-RI-3: O(1) snapshot — take the read lock just long enough to bump the
    /// inner `Arc` refcount, then drop the guard. The returned snapshot is read
    /// with NO lock held (e.g. `materialize` runs against it), so a reader can
    /// never wait on a concurrent projection. Poison-tolerant: a writer that
    /// panicked mid-mutation left the OLD snapshot intact (RCU only publishes on
    /// success), so recovering via `into_inner` is safe and — critically for
    /// AC-6 — means a single panic can NOT brick every future reader (the failure
    /// mode a plain `.read().ok()` would have: a poisoned `RwLock` returns `Err`
    /// forever). Reads are therefore infallible.
    pub fn snapshot(cache: &SharedFrontmatterCache) -> Arc<FrontmatterCache> {
        Arc::clone(
            &cache
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// SLW-RI-3: RCU write — clone the current snapshot, apply `f` to the clone,
    /// then publish it as the new snapshot. The write lock is held ONLY across the
    /// cheap map clone + `f` (a `HashMap` mutation), **never** across `materialize`
    /// or other heavy work. Serializes writers (no lost updates) and publishes
    /// atomically. Poison-tolerant: the guarded value is an `Arc` *pointer* (no
    /// torn data — a writer that panics mid-mutation never stored, so the old
    /// snapshot is intact), so we recover via `into_inner` rather than bricking
    /// every future cache write. This centralizes the poison tolerance the deleted
    /// `SWITCH_LOCK` used to carry.
    pub fn rcu<R>(cache: &SharedFrontmatterCache, f: impl FnOnce(&mut FrontmatterCache) -> R) -> R {
        Self::rcu_with_deadline(cache, None, f).expect("unbounded cache writer cannot expire")
    }

    /// Navigation's worker must not wait indefinitely behind another cache writer.
    /// Existing writers retain their lock/clone/publish semantics without a deadline.
    fn rcu_with_deadline<R>(
        cache: &SharedFrontmatterCache,
        deadline: Option<std::time::Instant>,
        f: impl FnOnce(&mut FrontmatterCache) -> R,
    ) -> Option<R> {
        let mut guard = if let Some(end) = deadline {
            loop {
                if std::time::Instant::now() >= end {
                    return None;
                }
                match cache.try_write() {
                    Ok(guard) => break guard,
                    Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
                    Err(std::sync::TryLockError::WouldBlock) => {
                        std::thread::sleep(std::time::Duration::from_millis(1))
                    }
                }
            }
        } else {
            cache
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        };
        if deadline.is_some_and(|end| std::time::Instant::now() >= end) {
            return None;
        }
        let mut next = (**guard).clone();
        let out = f(&mut next);
        let changed = std::mem::take(&mut next.keys_changed);
        *guard = Arc::new(next);
        // Counted only after the swap, so a reader that sees the new count also sees the
        // new snapshot.
        if changed {
            AtomicU64::fetch_add(&PUBLISHES, 1, Ordering::AcqRel);
        }
        Some(out)
    }

    /// R-2: Composed prepare → commit refresh under the cache's snapshot lock.
    /// Performs the disk read outside any lock via `prepare_refresh`, then
    /// commits via a single RCU cycle. Returned tuple matches `commit_refresh`.
    ///
    /// This is the canonical entry point for watcher call sites — they
    /// should not reconstruct the lock juggling pattern themselves.
    pub fn refresh_under_lock(
        cache: &SharedFrontmatterCache,
        path: &Path,
        dir_team: &str,
    ) -> (Option<CacheEntry>, Option<CacheEntry>) {
        let new = Self::prepare_refresh(path, dir_team);
        Self::rcu(cache, |c| c.commit_refresh(path, new))
    }

    /// SW-FX-9 Part 2B: refresh the cached entry for `path` by re-reading disk.
    /// Builds the entry from the file as-is via the pure `build_cache_entry`
    /// (orphan / non-pipeline → `None` → entry removed) and commits under the
    /// write lock. Same return tuple as `commit_refresh`. Used by the
    /// post-materialize wave-completion refresh so projection/worktree index
    /// consumers stop reading stale fields after a suppressed materialize.
    ///
    /// Since SR-13 removed the cache-refresh status-transition write, both this
    /// and `refresh_under_lock` are read-only. They are kept separate on purpose:
    /// this variant reads disk *inside* the RCU write section (SLW-RI-4 — the
    /// decision reflects current disk state under keyed concurrency), whereas
    /// `refresh_under_lock` reads before taking the lock. Different concurrency
    /// contracts, not duplication.
    pub fn refresh_under_lock_no_transition(
        cache: &SharedFrontmatterCache,
        path: &Path,
        dir_team: &str,
    ) -> (Option<CacheEntry>, Option<CacheEntry>) {
        // SLW-RI-3: re-read disk INSIDE the RCU write critical section so the
        // decision reflects *current* disk state, not a stale projection
        // snapshot. Under SLW-RI-4 keyed concurrency, disjoint projections run
        // in parallel and may touch the same shared source; a source re-created
        // between another wave's snapshot and this evict is refreshed (Some),
        // not clobbered — the AC-5 convergence guarantee. The lock is held
        // across only a single small `build_cache_entry` (one file read + parse),
        // never the heavy materialize the old `SWITCH_LOCK` covered.
        Self::rcu(cache, |c| {
            let new = build_cache_entry(path, dir_team);
            c.commit_refresh(path, new)
        })
    }

    /// SLW-RI-3: evict/refresh many vanished sources in ONE RCU cycle — one
    /// snapshot clone + N per-path disk re-reads + commits + one publish —
    /// rather than N separate clone→publish cycles (which would each clone the
    /// whole map). Each path is re-read from disk under the single write-lock
    /// hold, giving the same per-path AC-5 convergence as
    /// [`refresh_under_lock_no_transition`]. `paths` is typically a wave's
    /// vanished set; all share the plan's single `team`.
    pub fn refresh_many_no_transition(
        cache: &SharedFrontmatterCache,
        team: &str,
        paths: &[PathBuf],
    ) {
        if paths.is_empty() {
            return;
        }
        Self::rcu(cache, |c| {
            for path in paths {
                let new = build_cache_entry(path, team);
                c.commit_refresh(path, new);
            }
        });
    }

    /// SW-LN-B50: re-index stories a pipeline scan parsed as indexable but found
    /// no entry for (a mid-write read left them out). Each path is re-read from
    /// disk and grouped by its team (the segment before `content/`), one RCU cycle
    /// per team. Transition-free like the other self-heal refreshes. Paths outside
    /// a team content tree are skipped.
    pub fn reindex_scan_gaps(cache: &SharedFrontmatterCache, paths: &[PathBuf]) {
        // Without a deadline, the shared implementation cannot expire.
        let _completed = Self::reindex_scan_gaps_bounded(cache, paths, None);
    }

    pub fn reindex_scan_gaps_with_deadline(
        cache: &SharedFrontmatterCache,
        paths: &[PathBuf],
        deadline: std::time::Instant,
    ) -> bool {
        Self::reindex_scan_gaps_bounded(cache, paths, Some(deadline))
    }

    fn reindex_scan_gaps_bounded(
        cache: &SharedFrontmatterCache,
        paths: &[PathBuf],
        deadline: Option<std::time::Instant>,
    ) -> bool {
        let mut by_team: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for p in paths {
            if deadline.is_some_and(|end| std::time::Instant::now() >= end) {
                return false;
            }
            if let Some((team, _, _)) = p
                .to_str()
                .and_then(crate::install::capture::parse_context_repo_parts)
            {
                by_team.entry(team).or_default().push(p.clone());
            } else {
                debug!(
                    target: "frontmatter_cache",
                    "index gap outside a team content folder, skipped: {}",
                    p.display()
                );
            }
        }
        for (team, group) in by_team {
            debug!(
                target: "frontmatter_cache",
                "re-indexing {} stor(y/ies) the index had no entry for (team {team})",
                group.len()
            );
            // Preserve current-disk decisions under the existing RCU owner,
            // cloning once per team instead of once per arbitrary chunk.
            let completed = Self::rcu_with_deadline(cache, deadline, |snapshot| {
                for path in &group {
                    if deadline.is_some_and(|end| std::time::Instant::now() >= end) {
                        return false;
                    }
                    let new = build_cache_entry(path, &team);
                    snapshot.commit_refresh(path, new);
                }
                deadline.is_none_or(|end| std::time::Instant::now() < end)
            });
            if completed != Some(true) {
                return false;
            }
        }
        true
    }

    /// SW-LN-B50 (AC-3): make sure `story_id` (the story file at `path`) resolves in
    /// the index before a caller looks it up by id. When it doesn't — the index has no
    /// entry for the file, or a partial one from a mid-write read — the file is re-read
    /// from disk now. When it still can't resolve, the error says why in words a person
    /// can act on, never "not indexed yet".
    pub fn ensure_story_indexed(
        cache: &SharedFrontmatterCache,
        path: &Path,
        story_id: &str,
    ) -> Result<(), String> {
        if Self::snapshot(cache).single_by_story_id(story_id).is_ok() {
            return Ok(());
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let Some((team, _, _)) = path
            .to_str()
            .and_then(crate::install::capture::parse_context_repo_parts)
        else {
            return Err(format!(
                "{name} could not be read as a story: it is not in a team's content folder."
            ));
        };
        Self::refresh_under_lock_no_transition(cache, path, &team);
        match Self::snapshot(cache).single_by_story_id(story_id) {
            Ok(_) => return Ok(()),
            Err(StoryIdLookup::Ambiguous(n)) => {
                return Err(format!(
                    "{n} story files have the id {story_id}; give {name} a unique story_id."
                ))
            }
            Err(StoryIdLookup::NotFound) => {}
        }
        let why = match pipeline_scanner::extract_story_meta(path).map(story_meta_to_fields) {
            None => "its frontmatter could not be read".to_string(),
            Some(f) if !f.is_pipeline_story() => "its frontmatter has no story `type`".to_string(),
            Some(f) if f.source_repo.is_none() => {
                "its frontmatter has no `source_repo`".to_string()
            }
            Some(f) if f.story_id.as_deref() != Some(story_id) => {
                format!("its frontmatter's story_id is not {story_id}")
            }
            Some(_) => "its frontmatter is incomplete".to_string(),
        };
        Err(format!("{name} could not be read as a story: {why}."))
    }

    /// R-2 + R-5: Composed prepare → commit refresh under the cache's
    /// `RwLock` using caller-supplied `FrontmatterFields`. Same lock
    /// discipline as `refresh_under_lock` but skips the disk read + parse
    /// in the prepare phase. (SW-CP-18: its former prod caller
    /// `capture::capture_and_refresh_cache` is retired; currently test-only.)
    pub fn refresh_under_lock_from_fields(
        cache: &SharedFrontmatterCache,
        path: &Path,
        fields: FrontmatterFields,
        dir_team: &str,
    ) -> (Option<CacheEntry>, Option<CacheEntry>) {
        let new = Self::prepare_refresh_from_fields(path, fields, dir_team);
        Self::rcu(cache, |c| c.commit_refresh(path, new))
    }

    // ----- Persistence (T10, T11) ------------------------------------------

    /// Serialize to compact JSON and write atomically (temp file + rename).
    /// R-7: Delegated to `shared::json_io::write_json_atomic_compact` — the
    /// cache is machine-only, written on every change, and benefits from the
    /// smaller on-disk form. The shared writer also provides temp-cleanup on
    /// write-failure (which the prior inlined version lacked).
    pub fn persist_to_disk(&self, cache_path: &Path) -> Result<(), String> {
        crate::shared::json_io::write_json_atomic_compact(cache_path, self)
            .map_err(|e| format!("failed to persist cache to {}: {}", cache_path.display(), e))?;
        debug!(target: "frontmatter_cache", "persisted {} entries to {}", self.entries.len(), cache_path.display());
        Ok(())
    }

    /// Load from disk if present and version matches, then mtime-diff against
    /// actual files. Falls back to full scan on miss/mismatch.
    pub fn load_or_build(cache_path: &Path, org_root: &Path) -> Self {
        if let Ok(json) = fs::read_to_string(cache_path) {
            if let Ok(mut cached) = serde_json::from_str::<FrontmatterCache>(&json) {
                if cached.version == CACHE_VERSION {
                    info!(target: "frontmatter_cache", "warm start: loaded {} entries from disk", cached.entries.len());
                    // RF-perf-9: `story_id_index` is `#[serde(skip)]` → empty after
                    // deserialize. Rebuild it AFTER `mtime_diff` (whose free-function
                    // inserts/removes go straight to `entries`, bypassing the
                    // choke-point) so the index reflects the final post-diff state.
                    mtime_diff(&mut cached, org_root);
                    cached.rebuild_story_id_index();
                    return cached;
                }
                info!(target: "frontmatter_cache", "cache version mismatch (got {}, want {}), rebuilding",
                    cached.version, CACHE_VERSION);
            }
        }
        info!(target: "frontmatter_cache", "cold start: no valid cache at {}", cache_path.display());
        Self::build_from_scan(org_root)
    }

    // ----- Merge (R-3) -----------------------------------------------------

    /// Merge entries from `other` into `self`.
    ///
    /// Used by `init_watched_cache` to reconcile a freshly-scanned cache
    /// with watcher events that may have updated `self` during the scan
    /// window. Semantics:
    /// - Path absent from `self` → insert `other`'s entry.
    /// - Path present in both → keep `self`'s entry unless `other.mtime`
    ///   is strictly newer.
    /// - Path in `self` but not `other` → preserved (watcher-added entries
    ///   that the scan didn't see must survive).
    pub fn merge_from(&mut self, other: FrontmatterCache) {
        for (path, new_entry) in other.entries {
            let take_new = match self.entries.get(&path) {
                Some(existing) => existing.mtime < new_entry.mtime,
                None => true,
            };
            if take_new {
                self.upsert_entry(path, new_entry);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Returns mtime as **epoch nanoseconds** (SW-FX-9), or `None` if the file
/// is unreadable. Nanosecond resolution closes the same-second blind spot
/// shared by the warm-start `mtime_diff` and the scan-side freshness gate
/// (`entry_is_fresh`): on a filesystem that records sub-second mtime
/// (APFS/ext4/NTFS — every platform codesense ships on) any real write
/// advances the mtime, so a changed file compares strictly newer than its
/// cached entry and an unchanged file compares exactly equal. On a coarse
/// filesystem that only records whole-second mtime (FAT/exFAT, some network
/// mounts) the low bits are zero, so the gate degrades to the *prior*
/// second-granularity behaviour — two writes inside one second can still be
/// missed, exactly as before this change, never worse. `u64` nanoseconds
/// since the epoch overflows in year ~2554 — far beyond any practical concern.
pub(crate) fn mtime_of(path: &Path) -> Option<u64> {
    match fs::metadata(path).and_then(|m| m.modified()) {
        Ok(t) => Some(
            t.duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
        ),
        Err(e) => {
            debug!(target: "frontmatter_cache", "cannot stat {}: {}", path.display(), e);
            None
        }
    }
}

/// SW-FX-9: pure freshness comparison for the scan-side disk-truth gate.
/// A cached entry is **fresh** (safe to serve without re-reading disk) when
/// the file on disk is no newer than the entry: `disk_mtime <= entry_mtime`.
/// Equality means the file is unchanged since indexing — fresh — which is
/// exactly what preserves memoization on the hot scan path (re-parsing on
/// `==` would re-parse every unchanged file on every scan). A disk mtime
/// strictly greater than the cached mtime means the file changed since
/// indexing (a watcher-suppressed materialize, or a write the watcher hasn't
/// refreshed yet) — stale, so the caller re-parses from disk. On a sub-second
/// filesystem any real write advances `disk_mtime`, so an edit always lands in
/// the stale arm; on a whole-second filesystem this degrades to the prior
/// second-granularity behaviour (see `mtime_of`), never worse. There is
/// deliberately no coarse-FS branch: it cannot be derived from two mtime
/// values, so the comparison stays a single total order.
pub(crate) fn mtime_is_fresh(entry_mtime: u64, disk_mtime: u64) -> bool {
    disk_mtime <= entry_mtime
}

/// SW-FX-9: stat `entry.path` and decide whether the pipeline scan may trust
/// the cached entry without re-reading disk. A stat failure (file gone or
/// unreadable) returns `false` so the caller falls through to a fresh disk
/// read — which handles the missing-file case identically to a cache miss.
/// This stat-failure posture intentionally differs from `mtime_diff`'s
/// (warm-start treats an unreadable file as fresh / keep, to avoid thrashing
/// on a transient mount glitch): the scan path wants disk-truth *now*, so
/// "can't confirm fresh" means re-read, whereas warm-start optimises for not
/// re-walking. They share the pure [`mtime_is_fresh`] comparison, not the
/// failure handling. This is the thin `stat` wrapper kept next to `mtime_of`
/// so the unit (nanoseconds) has one home.
pub(crate) fn entry_is_fresh(entry: &CacheEntry) -> bool {
    match mtime_of(&entry.path) {
        Some(disk_mtime) => mtime_is_fresh(entry.mtime, disk_mtime),
        None => false,
    }
}

fn has_pipeline_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| pipeline_format::detect_format(ext).is_some())
        .unwrap_or(false)
}

fn build_cache_entry(path: &Path, dir_team: &str) -> Option<CacheEntry> {
    if !has_pipeline_extension(path) {
        return None;
    }
    let mtime = mtime_of(path)?; // Skip unreadable files
    let mut meta = pipeline_scanner::extract_story_meta(path)?;
    let parse_error = meta.metadata_parse_error.take();
    let fields = story_meta_to_fields(meta);
    // PTS-2: every cached entry must have frontmatter `source_repo` since
    // the directory-inferred fallback is retired. Orphan = skip (return
    // `None` either way — non-story files stay out of the `source_repo`-keyed
    // cache by design; per [[project_projection_engine_cache_bound]] they're
    // reached via the `All`/`Glob` subtree-walk, not the cache).
    //
    // SW-FX-12: split the *log level* on `classify_orphan`. A genuine
    // pipeline story missing `source_repo` (R-18 backfill gap, or a
    // hand-authored story that forgot the field) is a real defect → `warn!`.
    // Ordinary non-story content (docs, mockups, README/index, no-`type:`
    // files) correctly carries none → `debug!`, so a real cache-index defect
    // isn't buried under ~30 lines of expected boot noise.
    if fields.source_repo.is_none() {
        match classify_orphan(&fields) {
            OrphanKind::Story => warn!(
                target: "frontmatter_cache",
                "orphan skipped (no frontmatter source_repo): {}",
                path.display(),
            ),
            OrphanKind::NonStory => debug!(
                target: "frontmatter_cache",
                "non-story content skipped (no source_repo, not a pipeline story): {}",
                path.display(),
            ),
        }
        return None;
    }
    // SW-DX-2 (AC-13): capture the doc's open frontmatter tags at index time so
    // the scanner's cache-hit path lifts them onto `StoryRef.open_tags` with no
    // re-read. Reuses the SAME `open_tag_values` walker (via `extract_open_tags`)
    // that fills the inline scan path — one extractor, no drift.
    let open_tags = pipeline_scanner::extract_open_tags(path);
    let mut entry = build_cache_entry_from_fields(path, fields, mtime, dir_team, open_tags);
    entry.metadata_parse_error = parse_error;
    Some(entry)
}

/// Why a `source_repo`-less pipeline-extension file is skipped at index time.
/// Splits the orphan-skip log level so a genuinely misconfigured story stays
/// visible while expected non-story content goes quiet.
///
/// SW-FX-12: returned by [`classify_orphan`] so the warn-vs-debug branch is
/// unit-assertable without log capture (AC-1 / AC-2).
#[derive(Debug, PartialEq, Eq)]
enum OrphanKind {
    /// `document_type ∈ {story,bug,spike}` but no `source_repo` — a real
    /// hand-authored story or backfill gap that should surface loudly.
    Story,
    /// Not a pipeline story (doc, README/index, no `type:`) — expected
    /// non-cached content; skip silently.
    NonStory,
}

/// Classify a `source_repo`-less pipeline-extension file (see [`OrphanKind`]).
/// Pure over the parsed fields; gates only the *log level* — the skip itself
/// is unconditional. Shares the single `FrontmatterFields::is_pipeline_story`
/// predicate with the scanner's story-vs-doc routing and miss-count scope, so
/// the three decisions can never drift (AC-5).
fn classify_orphan(fields: &FrontmatterFields) -> OrphanKind {
    if fields.is_pipeline_story() {
        OrphanKind::Story
    } else {
        OrphanKind::NonStory
    }
}

/// R-5/R-14: Pure mapping from caller-supplied frontmatter fields to a cache
/// entry. With R-14's composition refactor, this is now mostly a struct
/// literal — `fields` is moved in whole-cloth, no per-field copying.
/// Shared by `build_cache_entry` (disk path) and
/// `refresh_entry_from_fields` (hand-off path).
fn build_cache_entry_from_fields(
    path: &Path,
    fields: FrontmatterFields,
    mtime: u64,
    dir_team: &str,
    open_tags: std::collections::BTreeMap<String, Vec<String>>,
) -> CacheEntry {
    CacheEntry {
        path: path.to_path_buf(),
        dir_team: dir_team.to_string(),
        fields,
        metadata_parse_error: None,
        open_tags,
        mtime,
    }
}

/// Adapter from `StoryMeta` (pipeline_scanner's title-aware extraction)
/// to `FrontmatterFields` (the lossless metadata view the cache uses).
/// Drops `title` / `story_text` since the cache does not need body-derived fields.
///
/// R-14: `pub(crate)` so `pipeline_scanner::walk_dir` can reuse it on the
/// cache-miss fallback path. Single source of truth for the StoryMeta →
/// FrontmatterFields projection.
pub(crate) fn story_meta_to_fields(meta: pipeline_scanner::StoryMeta) -> FrontmatterFields {
    FrontmatterFields {
        depends_on: meta.depends_on,
        // SW-AU-77F: carried through so the cache-miss projection reaches
        // `StoryRef.tags`; omitting it would drop markdown `tags` on a cold scan.
        tags: meta.tags,
        status: meta.status,
        document_type: meta.document_type,
        supporting_docs: meta.supporting_docs,
        story_id: meta.story_id,
        source_repo: meta.source_repo,
        // SV-CDE-85: story_changes reads the file itself; the cache does not carry it.
        work_repo: None,
        // SW-GS-35: story-scoped — unlike epic-only `branch` below, this IS
        // carried through so `CacheEntry::dev_branch()` populates.
        dev_branch: meta.dev_branch,
        // SW-WB-1: story-scoped intended branch — carried through to keep the
        // StoryMeta → FrontmatterFields projection lossless.
        branch_name: meta.branch_name,
        team: meta.team,
        // SW-EW-5: epic-only field; stories never carry it.
        repos: None,
        // SW-OR-1b: `branch` is now story-scoped too (the unified canonical
        // branch), threaded through so `CacheEntry::story_branch()` sees it. On an
        // epic doc this same key is the integration branch (SW-EW-5); a story and
        // an epic never share a StoryMeta, so the two readings never collide.
        branch: meta.branch,
        // SLW-UD-2 legacy epic-only list fields; stories never carry them.
        scope_context: None,
        scope_install: None,
        // SW-SB-1: board metadata — carried through the projection so the
        // cache-hit arm (`CacheEntry.fields`) surfaces them on `StoryRef`,
        // closing the seam SB-9 left here. `points` was lenient-parsed upstream.
        sprint: meta.sprint,
        points: meta.points,
        assigned_to: meta.assigned_to,
        // SW-SB-22: board-ordering rank — carried through so the cache-hit arm
        // surfaces it on `StoryRef` (the reason for the CACHE_VERSION 6→7 bump).
        rank: meta.rank,
        // SW-AU-77H: threaded through so the cache-miss scan arm (which builds
        // `FrontmatterFields` via this projection) carries a standalone doc's
        // `subject_id` onto `DocRef`. A `type: story` file uses `story_id` for
        // identity, so this is only ever set for a non-story doc.
        subject_id: meta.subject_id,
    }
}

/// Returns `true` when `path`'s extension resolves to the HTML pipeline
/// format. Used by the F.30 HTML-empty gate in `refresh_entry_from_fields`.
fn is_html_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(pipeline_format::detect_format)
        .map(|f| f == PipelineFileFormat::Html)
        .unwrap_or(false)
}

/// PTS-2 / SLW-IN-7: Yields every indexable content directory under `org_root`
/// paired with its directory-derived team. Only the flat per-team layout is
/// walked: `{team}/content/` → yields `(team, content_dir)`.
///
/// The legacy per-`(team, repo)` arm (`{team}/repos/{owner}/{repo}/`)
/// is retired post-PTS-2: content there exists on disk but is NOT indexed
/// (the migration copies everything to the flat layout; PTS-4 deletes
/// legacy sources later).
///
/// Every story indexed here must carry frontmatter `source_repo:` —
/// `build_cache_entry`'s orphan-skip enforces this. Stories landing in
/// this layout via PTS-2 migration carry the field by construction
/// (backfilled on destination copies during migration).
///
/// Both `build_from_scan` (cold) and `mtime_diff`'s Phase 3 new-file
/// discovery (warm) consume this so the two walkers never drift.
pub(crate) fn for_each_indexable_dir(org_root: &Path, mut f: impl FnMut(&str, &Path)) {
    let teams = match fs::read_dir(org_root) {
        Ok(entries) => entries,
        Err(e) => {
            warn!(
                target: "frontmatter_cache",
                "cannot read org root {}: {}",
                org_root.display(),
                e,
            );
            return;
        }
    };

    for team_entry in teams.filter_map(|e| e.ok()) {
        if !team_entry.path().is_dir() {
            continue;
        }
        let team_name = team_entry.file_name().to_string_lossy().to_string();

        // SLW-IN-7: teams are flat directories directly under `org_root`. Skip
        // the `_`/`.`-prefixed system entries (e.g. `_codesense`, `.git`) and
        // the `worktrees/` sibling (full repo checkouts) — none are teams, and
        // anything beneath a reserved entry is not team-indexed.
        if team_name.starts_with('_')
            || team_name.starts_with('.')
            || team_name == crate::context_repo::WORKTREES_DIR
        {
            continue;
        }

        // The flat `{team}/content/` dir is the only indexed root per team.
        // Every cached entry must have frontmatter `source_repo` (orphan-skip in
        // `build_cache_entry`); the legacy `{team}/repos/.../_bmad-output/` arm
        // is retired (PTS-2 AC 28) — content there exists on disk post-migration
        // but isn't indexed.
        let content_root = team_entry.path().join(TEAM_CONTENT_DIR);
        if content_root.is_dir() {
            f(&team_name, &content_root);
        }
    }
}

/// SW-CR-9: short-circuiting disk scan for the first pipeline candidate under
/// `dir` whose frontmatter carries `story_id`. Shares the walker and the
/// pipeline-ext gate with `scan_dir_recursive`, but parses only until a hit —
/// not every file — so the cost on a cache miss is bounded (first hit, not
/// whole tree). Non-pipeline files, symlinks, and unreadable candidates are
/// skipped. Returns `None` on no match or an unreadable dir (`walk_dir` already
/// swallows per-entry io errors; a dir-level failure here is a clean miss).
fn scan_dir_for_story(dir: &Path, pick: &dyn Fn(&str) -> bool) -> Option<(PathBuf, String)> {
    let mut hit: Option<(PathBuf, String)> = None;
    scan_dir_story_ids(dir, &mut |path, id| {
        if pick(id) {
            hit = Some((path.to_path_buf(), id.to_string()));
        }
        hit.is_some()
    });
    hit
}

/// Calls `visit(path, story_id)` for each pipeline file under `dir` that declares a story id,
/// until it returns `true`.
fn scan_dir_story_ids(dir: &Path, visit: &mut dyn FnMut(&Path, &str) -> bool) {
    use crate::shared::dir_walker::{walk_dir, WalkAction};

    let mut done = false;
    let _ = walk_dir(dir, &mut |path,
                                result|
     -> Result<
        WalkAction,
        std::convert::Infallible,
    > {
        if done {
            return Ok(WalkAction::Skip);
        }
        let meta = match result {
            Ok(m) => m,
            Err(_) => return Ok(WalkAction::Skip),
        };
        if meta.is_dir() {
            return Ok(WalkAction::Recurse);
        }
        if meta.file_type().is_symlink() {
            return Ok(WalkAction::Skip);
        }
        if !has_pipeline_extension(path) {
            return Ok(WalkAction::Skip);
        }
        if let Some(found_id) = pipeline_scanner::extract_story_meta(path)
            .and_then(|m| story_meta_to_fields(m).story_id)
        {
            done = visit(path, &found_id);
        }
        Ok(WalkAction::Skip)
    });
}

fn scan_dir_recursive(dir: &Path, team: &str, cache: &mut FrontmatterCache) {
    use crate::shared::dir_walker::{walk_dir, WalkAction};

    // R-7: Capture the top-level dir's mtime BEFORE reading children. The
    // walker captures subdir mtimes in the callback below. A stat failure
    // persists sentinel `0` so the next warm-start unconditionally re-scans
    // this dir (rather than omitting it from `dir_mtimes` entirely, which
    // would let the gate skip a subtree whose files we already inserted —
    // orphaned-subtree bug).
    cache
        .dir_mtimes
        .insert(dir.to_path_buf(), mtime_of(dir).unwrap_or(0));

    let _ = walk_dir(dir, &mut |path,
                                result|
     -> Result<
        WalkAction,
        std::convert::Infallible,
    > {
        let meta = match result {
            Ok(m) => m,
            // Silent swallow preserved: io errors don't abort the scan.
            Err(_) => return Ok(WalkAction::Skip),
        };
        if meta.is_dir() {
            // R-7: Capture this subdir's mtime before the walker recurses.
            // Sentinel `0` on stat failure — see top-level comment.
            cache
                .dir_mtimes
                .insert(path.to_path_buf(), mtime_of(path).unwrap_or(0));
            return Ok(WalkAction::Recurse);
        }
        // Skip symlinks (use symlink_metadata via walker; symlinks-to-dirs
        // intentionally never recursed into) and only insert real files.
        if !meta.file_type().is_symlink() {
            if let Some(ce) = build_cache_entry(path, team) {
                cache.entries.insert(path.to_path_buf(), ce);
            }
        }
        Ok(WalkAction::Skip)
    });
}

/// Warm-start mtime diffing: remove entries for deleted files, re-parse
/// entries where mtime changed, add entries for new files.
fn mtime_diff(cache: &mut FrontmatterCache, org_root: &Path) {
    // RF-perf-9 (AC-1): always-on frequency tally + gated whole-pass timing (the cold,
    // once-per-warm-start multi-pass). Reuses the one RF-perf-0 gate.
    crate::install::frontmatter_perf::note_mtime_diff();
    let t = crate::install::frontmatter_perf::start_timer();

    // 1. Remove entries for files that no longer exist
    let stale: Vec<PathBuf> = cache
        .entries
        .keys()
        .filter(|p| !p.exists())
        .cloned()
        .collect();
    for path in &stale {
        cache.entries.remove(path);
    }

    // 2. Re-parse entries where mtime changed. Use dir_team (directory-
    //    inferred, never stale) as fallback — not the old cached team
    //    which may have been a frontmatter override now removed.
    let to_refresh: Vec<(PathBuf, String)> = cache
        .entries
        .iter()
        .filter_map(|(path, entry)| {
            let current_mtime = mtime_of(path).unwrap_or(0);
            // Stale = disk strictly newer than the cached entry — the SAME
            // comparison the scan-side gate uses (SW-FX-9 `mtime_is_fresh`), so
            // warm-start and scan agree on "fresh vs stale by mtime" and share
            // the nanosecond resolution that closes the same-second blind spot.
            // Only the *comparison* is shared: `unwrap_or(0)` keeps the legacy
            // "stat failure → treat as fresh, don't re-thrash" warm-start
            // behavior (`0 <= entry.mtime` is fresh), which deliberately
            // differs from the scan gate's re-read-on-failure posture.
            if mtime_is_fresh(entry.mtime, current_mtime) {
                None
            } else {
                Some((path.clone(), entry.dir_team.clone()))
            }
        })
        .collect();
    for (path, dir_team) in to_refresh {
        if let Some(new_entry) = build_cache_entry(&path, &dir_team) {
            cache.entries.insert(path, new_entry);
        }
    }

    // 3. Scan for new files. R-7: use mtime-gated walker. The team/owner/repo
    //    outer structure has very few entries (≤ 1-3 of each typically) so we
    //    keep walking it unconditionally; the mtime gate kicks in at the
    //    repo_dir level and below where the bulk of directories live.
    //
    // Pre-compute a parent→children index ONCE here so the gated walker's
    // "unchanged" branch is O(children) instead of O(total_tracked_dirs) per
    // recursion level. With ~50 tracked dirs the cost was sub-ms either way,
    // but the optimization premise erodes once tracked-dir count climbs into
    // the thousands.
    let children_by_parent: HashMap<PathBuf, Vec<PathBuf>> = {
        let mut map: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        for p in cache.dir_mtimes.keys() {
            if let Some(parent) = p.parent() {
                map.entry(parent.to_path_buf()).or_default().push(p.clone());
            }
        }
        map
    };

    // PTS-2: Walk the per-team flat layout via the shared iterator so the
    // cold and warm discovery paths can never drift. Mtime-gated walker
    // preserves R-7's sub-ms warm-start cost.
    for_each_indexable_dir(org_root, |team_name, dir| {
        discover_new_files_gated(dir, team_name, cache, &children_by_parent);
    });

    // 4. Prune dir_mtimes for directories that no longer exist on disk.
    // Use `try_exists()` and ONLY prune on `Ok(false)` (definitely gone).
    // Ambiguous results (permission denied, symlink loop) preserve the entry
    // so a transient mount glitch doesn't force a full re-walk on the next
    // pass.
    let dead_dirs: Vec<PathBuf> = cache
        .dir_mtimes
        .keys()
        .filter(|p| matches!(p.try_exists(), Ok(false)))
        .cloned()
        .collect();
    for d in &dead_dirs {
        cache.dir_mtimes.remove(d);
    }

    if !stale.is_empty() {
        trace!(target: "frontmatter_cache", "mtime_diff: removed {} stale entries", stale.len());
    }
    if !dead_dirs.is_empty() {
        trace!(target: "frontmatter_cache", "mtime_diff: pruned {} dead dir_mtimes", dead_dirs.len());
    }
    info!(
        target: "frontmatter_cache",
        "mtime_diff complete: {} entries, {} tracked dirs",
        cache.entries.len(),
        cache.dir_mtimes.len(),
    );
    if let Some(t0) = t {
        debug!(
            target: crate::install::frontmatter_perf::SYNC_TIMING_TARGET,
            "frontmatter_mtime_diff entries={} tracked_dirs={} elapsed_us={}",
            cache.entries.len(),
            cache.dir_mtimes.len(),
            t0.elapsed().as_micros(),
        );
    }
}

/// R-7: New-file discovery with per-directory mtime gating.
///
/// At each directory:
///   - **Unchanged mtime** (stored == current) → skip `read_dir`; recurse
///     only into the subdirectories we already know about (looked up via
///     the pre-computed `children_by_parent` index — O(1) per dir). No new
///     direct children can exist at this level if the dir mtime hasn't
///     moved.
///   - **Changed mtime, first-seen dir, or stat failure** → `read_dir`,
///     insert any files not already in `cache.entries`, recurse into every
///     subdir. On stat failure we still attempt the scan (matches legacy
///     `scan_new_files_recursive`) and record sentinel 0 so the next pass
///     re-scans unconditionally.
///
/// When `dir_mtimes` is empty (e.g. first run after R-7 upgrade), every dir
/// is "first-seen" so we naturally fall back to a full walk that also
/// populates `dir_mtimes` for subsequent warm starts. Symlinks are skipped,
/// mirroring `scan_dir_recursive`.
fn discover_new_files_gated(
    dir: &Path,
    team: &str,
    cache: &mut FrontmatterCache,
    children_by_parent: &HashMap<PathBuf, Vec<PathBuf>>,
) {
    // Capture current mtime BEFORE read_dir: a write between our stat and
    // the read leaves us with the older value, which on the next pass
    // differs from the (now newer) on-disk mtime → re-scan triggered.
    // `None` here = stat failure; we still try `read_dir` below (legacy
    // behavior) and persist sentinel 0 so the next pass re-scans.
    let current_mtime_opt = mtime_of(dir);
    let stored = cache.dir_mtimes.get(dir).copied();

    // Gate hit: both reads succeeded AND values match → no new direct
    // children exist; recurse only into known subdirs.
    if let (Some(current), Some(prev)) = (current_mtime_opt, stored) {
        if current == prev {
            if let Some(subs) = children_by_parent.get(dir) {
                for sub in subs {
                    discover_new_files_gated(sub, team, cache, children_by_parent);
                }
            }
            return;
        }
    }

    // First-seen dir, mtime changed, or stat failure → full read_dir.
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            discover_new_files_gated(&path, team, cache, children_by_parent);
        } else if !meta.file_type().is_symlink() && !cache.entries.contains_key(&path) {
            if let Some(ce) = build_cache_entry(&path, team) {
                cache.entries.insert(path, ce);
            }
        }
    }
    // Record the mtime we captured BEFORE read_dir (or sentinel 0 on stat
    // failure) so the next pass can gate correctly.
    cache
        .dir_mtimes
        .insert(dir.to_path_buf(), current_mtime_opt.unwrap_or(0));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::SystemTime;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("codesense_test").join(format!(
            "fm_cache_{}_{}",
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

    /// SW-LN-B50 (AC-3): a story the index left out is indexed on demand; a file that
    /// can't be a story says why instead of "not indexed yet".
    #[test]
    fn ensure_story_indexed_heals_a_missed_story_or_says_why() {
        let tmp = tempfile::TempDir::new().unwrap();
        let epic = tmp.path().join("general").join("content").join("epic");
        std::fs::create_dir_all(&epic).unwrap();
        let cache = FrontmatterCache::shared(FrontmatterCache::new());

        let good = epic.join("TR-1-a.md");
        std::fs::write(
            &good,
            "---\ntype: story\nstory_id: TR-1\nstatus: ready\nsource_repo: local/fixture-api\n---\n# TR-1\n",
        )
        .unwrap();
        FrontmatterCache::ensure_story_indexed(&cache, &good, "TR-1").expect("indexes the story");
        assert!(FrontmatterCache::snapshot(&cache)
            .single_by_story_id("TR-1")
            .is_ok());

        let orphan = epic.join("TR-2-b.md");
        std::fs::write(
            &orphan,
            "---\ntype: story\nstory_id: TR-2\nstatus: ready\n---\n# TR-2\n",
        )
        .unwrap();
        let err = FrontmatterCache::ensure_story_indexed(&cache, &orphan, "TR-2").unwrap_err();
        assert!(
            err.contains("TR-2-b.md could not be read as a story"),
            "{err}"
        );
        assert!(err.contains("source_repo"), "{err}");
        assert!(!err.contains("indexed"), "{err}");

        // A partial entry from a mid-write read (source_repo landed, story_id not yet)
        // is re-read, not trusted because the path has an entry.
        let partial = epic.join("TR-3-c.md");
        std::fs::write(
            &partial,
            "---\ntype: story\nsource_repo: local/fixture-api\n---\n",
        )
        .unwrap();
        FrontmatterCache::refresh_under_lock(&cache, &partial, "general");
        assert!(FrontmatterCache::snapshot(&cache)
            .entries
            .contains_key(&partial));
        std::fs::write(
            &partial,
            "---\ntype: story\nstory_id: TR-3\nstatus: ready\nsource_repo: local/fixture-api\n---\n# TR-3\n",
        )
        .unwrap();
        FrontmatterCache::ensure_story_indexed(&cache, &partial, "TR-3")
            .expect("re-reads the partial entry");
    }

    #[test]
    fn story_meta_to_fields_carries_dev_branch_to_accessor() {
        // SW-GS-35 (AC-1): the StoryMeta → FrontmatterFields adapter threads
        // dev_branch through so `CacheEntry::dev_branch()` populates. SW-OR-1b:
        // `branch` is now threaded on the story path too (was hardcoded None) —
        // when the meta carries no `branch`, the field is still None.
        let meta = pipeline_scanner::StoryMeta {
            source_repo: Some("org/repo".to_string()),
            dev_branch: Some("feat/SW-GS-35-foo".to_string()),
            ..Default::default()
        };
        let fields = story_meta_to_fields(meta);
        assert_eq!(fields.dev_branch.as_deref(), Some("feat/SW-GS-35-foo"));
        assert!(fields.branch.is_none(), "no `branch` in meta ⇒ None");

        let entry = CacheEntry {
            path: PathBuf::from("/tmp/s.md"),
            dir_team: "default".to_string(),
            fields,
            metadata_parse_error: None,
            open_tags: Default::default(),
            mtime: 0,
        };
        assert_eq!(entry.dev_branch(), Some("feat/SW-GS-35-foo"));
    }

    fn story_branch_entry(
        branch: Option<&str>,
        branch_name: Option<&str>,
        dev_branch: Option<&str>,
    ) -> CacheEntry {
        CacheEntry {
            path: PathBuf::from("/tmp/s.md"),
            dir_team: "default".to_string(),
            fields: FrontmatterFields {
                branch: branch.map(str::to_string),
                branch_name: branch_name.map(str::to_string),
                dev_branch: dev_branch.map(str::to_string),
                ..Default::default()
            },
            metadata_parse_error: None,
            open_tags: Default::default(),
            mtime: 0,
        }
    }

    #[test]
    fn story_branch_resolves_by_precedence() {
        // SW-OR-1b (AC-1): `branch` → `branch_name` → `dev_branch`, first
        // non-blank wins; a story authored with `branch` prefers it.
        assert_eq!(
            story_branch_entry(
                Some("feat/canonical"),
                Some("feat/intended"),
                Some("feat/observed")
            )
            .story_branch(),
            Some("feat/canonical")
        );
        // A legacy-only doc still resolves via fallback.
        assert_eq!(
            story_branch_entry(None, Some("feat/intended"), Some("feat/observed")).story_branch(),
            Some("feat/intended")
        );
        assert_eq!(
            story_branch_entry(None, None, Some("feat/observed")).story_branch(),
            Some("feat/observed")
        );
        // Blank/whitespace `branch` is skipped, not preferred over a real legacy value.
        assert_eq!(
            story_branch_entry(Some("   "), Some("feat/intended"), None).story_branch(),
            Some("feat/intended")
        );
        // All absent ⇒ None (the resolver then falls back to `{prefix}{story_id}`).
        assert_eq!(story_branch_entry(None, None, None).story_branch(), None);
    }

    #[test]
    fn story_meta_to_fields_carries_board_metadata() {
        // SW-SB-1 (AC-5 cache arm): the StoryMeta → FrontmatterFields projection
        // threads sprint/points/assigned_to through so the cache-hit `StoryRef`
        // build surfaces them — closing the seam SB-9 left (was hardcoded None).
        // SW-SB-22 (AC-1 cache arm): `rank` is threaded the same way — the reason
        // for the CACHE_VERSION 6→7 bump.
        let meta = pipeline_scanner::StoryMeta {
            sprint: Some("2026-08-A".to_string()),
            points: Some(5.0),
            assigned_to: Some("octocat-dev".to_string()),
            rank: Some("h0".to_string()),
            ..Default::default()
        };
        let fields = story_meta_to_fields(meta);
        assert_eq!(fields.sprint.as_deref(), Some("2026-08-A"));
        assert_eq!(fields.points, Some(5.0));
        assert_eq!(fields.assigned_to.as_deref(), Some("octocat-dev"));
        assert_eq!(fields.rank.as_deref(), Some("h0"));
    }

    /// PTS-2: Create per-team flat-layout story files. The `owner` + `repo`
    /// args are used to inject `source_repo:` frontmatter when the story
    /// body lacks it — required post-PTS-2 since the dir-derived fallback
    /// is retired (`build_cache_entry`'s orphan skip would otherwise drop
    /// the entry).
    #[test]
    fn build_from_content_root_indexes_a_single_content_dir() {
        let dir = test_dir("content_root");
        setup_org(
            &dir,
            "default",
            "acme",
            "codesense",
            &[
                ("1-epic/a.md", "---\nstatus: todo\nstory_id: A-1\n---\n# A"),
                ("1-epic/b.md", "---\nstatus: done\nstory_id: B-1\n---\n# B"),
            ],
        );
        let content_root = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR);
        let cache = FrontmatterCache::build_from_content_root(&content_root, "default");

        // Entries keyed by absolute path — so a scan of the same root hits them.
        assert!(cache
            .entries
            .contains_key(&content_root.join("1-epic").join("a.md")));
        assert_eq!(cache.entries.len(), 2, "both stories indexed");
        // RF-perf-9 aux index derived, so story-id resolution works.
        assert!(cache.single_by_story_id("A-1").is_ok());
        // The passed team lands on the entries as their dir_team fallback.
        assert_eq!(cache.by_story_id("B-1")[0].dir_team, "default");

        cleanup(&dir);
    }

    fn setup_org(root: &Path, team: &str, owner: &str, repo: &str, stories: &[(&str, &str)]) {
        let content_dir = root.join(team).join(crate::context_repo::TEAM_CONTENT_DIR);
        fs::create_dir_all(&content_dir).unwrap();
        for (name, content) in stories {
            let file_path = content_dir.join(name);
            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            let bytes = if content.contains("source_repo:") || !content.starts_with("---") {
                content.to_string()
            } else {
                // Inject `source_repo:` into the existing frontmatter block.
                let slug = format!("{}/{}", owner, repo);
                content.replacen("---\n", &format!("---\nsource_repo: {slug}\n"), 1)
            };
            fs::write(&file_path, bytes).unwrap();
        }
    }

    #[test]
    fn build_from_scan_indexes_story_files() {
        let dir = test_dir("build_scan");
        setup_org(
            &dir,
            "default",
            "alderco",
            "codesense",
            &[
                (
                    "story-a.md",
                    "---\nstatus: todo\nstory_id: A-1\n---\n# Story A",
                ),
                ("story-b.md", "---\nstatus: done\n---\n# Story B"),
                ("readme.txt", "not a pipeline file"),
            ],
        );

        let cache = FrontmatterCache::build_from_scan(&dir);
        assert_eq!(
            cache.entries.len(),
            2,
            "should index 2 .md files, skip .txt"
        );

        let by_team = cache.by_team("default");
        assert_eq!(by_team.len(), 2);

        let by_repo = cache.by_source_repo("alderco/codesense");
        assert_eq!(by_repo.len(), 2);

        // R-7: by_team_and_repo is now an iterator; collect when we need
        // indexed access + .len() (tests stay readable with one collect call).
        let by_both: Vec<&CacheEntry> = cache
            .by_team_and_repo("default", "alderco/codesense")
            .collect();
        assert_eq!(by_both.len(), 2);

        // Verify fields
        let a = by_both
            .iter()
            .find(|e| e.story_id() == Some("A-1"))
            .unwrap();
        assert_eq!(a.status(), Some("todo"));
        assert_eq!(a.team(), "default");
        assert_eq!(a.source_repo(), "alderco/codesense");

        cleanup(&dir);
    }

    // ---- PTS-2: flat-layout walk (content/ only) ----

    /// Create a story file directly under `_team/{team}/content/{rel}` (PTS-2
    /// flat layout). Bypasses `setup_org`'s legacy-only structure.
    fn write_flat_story(root: &Path, team: &str, rel: &str, content: &str) {
        let file_path = root
            .join(team)
            .join(crate::context_repo::TEAM_CONTENT_DIR)
            .join(rel);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&file_path, content).unwrap();
    }

    #[test]
    fn rebuild_from_disk_reflects_a_team_rename() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_flat_story(
            dir,
            "payments",
            "epic-1/story.md",
            "---\nstatus: todo\nsource_repo: alderco/codesense\n---\n# Story",
        );
        let shared = FrontmatterCache::shared(FrontmatterCache::build_from_scan(dir));
        // Indexed under the old team name.
        assert_eq!(
            FrontmatterCache::snapshot(&shared)
                .by_team("payments")
                .len(),
            1
        );
        assert_eq!(
            FrontmatterCache::snapshot(&shared).by_team("billing").len(),
            0
        );

        // Rename the content dir on disk (mtimes preserved — mimics fs::rename).
        fs::rename(dir.join("payments"), dir.join("billing")).unwrap();
        // Still stale in-memory until reconciled.
        assert_eq!(
            FrontmatterCache::snapshot(&shared)
                .by_team("payments")
                .len(),
            1
        );

        FrontmatterCache::rebuild_from_disk(&shared, dir);

        // Now queryable under the new team name; nothing dangling under the old.
        let snap = FrontmatterCache::snapshot(&shared);
        assert_eq!(snap.by_team("billing").len(), 1);
        assert_eq!(snap.by_team("payments").len(), 0);
    }

    #[test]
    fn build_from_scan_walks_only_content_dir() {
        let dir = test_dir("pts2_flat_layout");

        // PTS-2: legacy layout exists on disk but is NOT indexed. Write
        // directly under `_team/{team}/repos/.../`_bmad-output/ — bypass
        // setup_org since the helper now writes to the flat layout.
        let legacy_dir = dir
            .join("default")
            .join(crate::context_repo::REPOS_DIR)
            .join("alderco")
            .join("codesense")
            .join("_bmad-output");
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(
            legacy_dir.join("legacy.md"),
            "---\nstatus: todo\nsource_repo: alderco/codesense\n---\n# Legacy",
        )
        .unwrap();

        // PTS-2 flat layout: this is the only indexed location.
        write_flat_story(
            &dir,
            "default",
            "impl/in-progress/flat.md",
            "---\nstatus: todo\nsource_repo: alderco/codesense\nstory_id: F-1\n---\n# Flat",
        );

        let cache = FrontmatterCache::build_from_scan(&dir);
        assert_eq!(
            cache.entries.len(),
            1,
            "PTS-2: only the flat-layout story should be indexed; legacy is ignored",
        );

        let flat_path = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR)
            .join("impl/in-progress/flat.md");
        let flat = cache.entries.get(&flat_path).expect("flat entry");
        assert_eq!(flat.source_repo(), "alderco/codesense");
        assert_eq!(flat.team(), "default");
        assert_eq!(flat.story_id(), Some("F-1"));

        cleanup(&dir);
    }

    // ---- SW-CR-9: targeted scan for a story id -----------------------------

    #[test]
    fn scan_org_for_story_id_finds_a_flat_layout_file() {
        let dir = test_dir("scan_for_story_id_hit");
        write_flat_story(
            &dir,
            "default",
            "epic-1/scanned.md",
            "---\ntype: story\nstatus: todo\nstory_id: SW-SCAN-1\nsource_repo: alderco/codesense\n---\n# S",
        );
        let found = FrontmatterCache::scan_org_for_story_id(&dir, "SW-SCAN-1")
            .expect("scan finds the flat-layout story");
        assert_eq!(found.1, "default");
        assert!(found.0.ends_with("scanned.md"), "got {}", found.0.display());
        cleanup(&dir);
    }

    #[test]
    fn scan_org_for_story_id_returns_none_on_no_hit() {
        let dir = test_dir("scan_for_story_id_miss");
        write_flat_story(
            &dir,
            "default",
            "epic-1/other.md",
            "---\ntype: story\nstatus: todo\nstory_id: SW-OTHER\nsource_repo: alderco/codesense\n---\n# o",
        );
        assert!(FrontmatterCache::scan_org_for_story_id(&dir, "SW-ABSENT").is_none());
        cleanup(&dir);
    }

    #[test]
    fn scan_org_for_story_id_skips_non_pipeline_files() {
        let dir = test_dir("scan_for_story_id_skips");
        let content = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR);
        fs::create_dir_all(&content).unwrap();
        // A non-pipeline file whose contents mention the id must not match —
        // the scan is scoped to .md/.html frontmatter.
        fs::write(
            content.join("notes.txt"),
            "story_id: SW-TRAP (plain-text, not a pipeline file)",
        )
        .unwrap();
        assert!(FrontmatterCache::scan_org_for_story_id(&dir, "SW-TRAP").is_none());
        cleanup(&dir);
    }

    #[test]
    fn build_from_scan_empty_content_dir_is_noop() {
        let dir = test_dir("pts1_empty_content");
        setup_org(
            &dir,
            "default",
            "alderco",
            "codesense",
            &[(
                "only.md",
                "---\nstatus: todo\nsource_repo: alderco/codesense\n---\n# Only",
            )],
        );
        // Create empty content/ dir.
        let content_dir = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR);
        fs::create_dir_all(&content_dir).unwrap();

        let cache = FrontmatterCache::build_from_scan(&dir);
        assert_eq!(
            cache.entries.len(),
            1,
            "empty content/ yields zero new entries"
        );

        cleanup(&dir);
    }

    #[test]
    fn build_from_scan_skips_flat_orphan_without_source_repo() {
        // A `source_repo`-less file under _team/{team}/content/ has no
        // path-based fallback — it must be skipped (not cached). This fixture
        // has no `type:`, so post-SW-FX-12 it is non-story content skipped at
        // `debug!` (the warn is now reserved for story-typed orphans — see
        // `build_from_scan_warn_split_*`). The *skip* is unchanged either way.
        let dir = test_dir("pts1_flat_orphan");
        write_flat_story(
            &dir,
            "default",
            "orphan.md",
            "---\nstatus: todo\n---\n# Orphan without source_repo",
        );

        let cache = FrontmatterCache::build_from_scan(&dir);
        assert_eq!(cache.entries.len(), 0, "orphan flat entry must be skipped");

        cleanup(&dir);
    }

    // ----- SW-FX-12: orphan-log split (warn for stories, debug for docs) -----

    /// AC-5 / AC-1 / AC-2: the index-time log level is decided by the single
    /// `is_pipeline_story` predicate via `classify_orphan`. Story-typed
    /// frontmatter → `Story` (warn); everything else (no `type:`, `type: doc`,
    /// no frontmatter at all) → `NonStory` (debug). Asserting the classifier
    /// directly makes the warn-vs-debug branch testable without log capture.
    #[test]
    fn classify_orphan_warns_only_for_pipeline_stories() {
        let story = |t: &str| FrontmatterFields {
            document_type: Some(t.to_string()),
            ..Default::default()
        };
        // Pipeline-story types (incl. whitespace/case variants) → Story.
        assert_eq!(classify_orphan(&story("story")), OrphanKind::Story);
        assert_eq!(classify_orphan(&story("bug")), OrphanKind::Story);
        assert_eq!(classify_orphan(&story("spike")), OrphanKind::Story);
        assert_eq!(classify_orphan(&story(" Story ")), OrphanKind::Story);

        // Non-story content → NonStory (silent at index).
        assert_eq!(classify_orphan(&story("doc")), OrphanKind::NonStory);
        assert_eq!(classify_orphan(&story("retro")), OrphanKind::NonStory);
        assert_eq!(
            classify_orphan(&FrontmatterFields::default()),
            OrphanKind::NonStory,
            "no `type:` (incl. no-frontmatter files) is non-story content",
        );
    }

    /// AC-1 / AC-2 / AC-4: a mixed tree — a non-story doc (no `type:`), a
    /// `type: doc` file, a no-frontmatter README, a story-typed file missing
    /// `source_repo`, and a genuine story with `source_repo` — caches ONLY the
    /// genuine story. Every orphan (story-typed or not) is skipped, so the
    /// cached-entry set is unchanged by the log-level split (docs were never
    /// cached).
    #[test]
    fn build_from_scan_caches_only_source_repo_stories_in_mixed_tree() {
        let dir = test_dir("swfx12_mixed_tree");
        // Non-story content (all correctly carry no source_repo) — skipped.
        write_flat_story(
            &dir,
            "default",
            "docs/no-type.md",
            "---\nstatus: todo\n---\n# Doc",
        );
        write_flat_story(
            &dir,
            "default",
            "docs/typed-doc.md",
            "---\ntype: doc\n---\n# Spec",
        );
        write_flat_story(&dir, "default", "README.md", "# Readme, no frontmatter\n");
        // Story-typed but missing source_repo — a genuine orphan (would warn),
        // still skipped (not cached).
        write_flat_story(
            &dir,
            "default",
            "stories/orphan-story.md",
            "---\ntype: story\nstatus: todo\n---\n# Orphan story",
        );
        // Genuine cacheable story.
        write_flat_story(
            &dir,
            "default",
            "stories/real.md",
            "---\ntype: story\nstatus: todo\nsource_repo: alderco/codesense\nstory_id: SW-1\n---\n# Real",
        );

        let cache = FrontmatterCache::build_from_scan(&dir);
        assert_eq!(
            cache.entries.len(),
            1,
            "only the source_repo-bearing story is cached; all orphans (story-typed or doc) are skipped",
        );
        let real_path = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR)
            .join("stories/real.md");
        assert!(
            cache.entries.contains_key(&real_path),
            "the real story must be the cached entry"
        );

        cleanup(&dir);
    }

    #[test]
    fn mtime_diff_discovers_new_flat_layout_files() {
        // Warm start parallel: a fresh story dropped under content/ must be
        // discovered by mtime_diff's Phase 3, mirroring the build_from_scan
        // behavior so warm starts don't silently miss new captures.
        let dir = test_dir("pts1_warm_flat");
        setup_org(
            &dir,
            "default",
            "alderco",
            "codesense",
            &[(
                "seed.md",
                "---\nstatus: todo\nsource_repo: alderco/codesense\n---\n# Seed",
            )],
        );

        // Cold scan first.
        let mut cache = FrontmatterCache::build_from_scan(&dir);
        assert_eq!(cache.entries.len(), 1);

        // macOS APFS / Linux ext4 quantize mtime to whole seconds; without a
        // pause, the dir-mtime gate sees "unchanged" and skips read_dir, so
        // the new file is missed. Bump the stored dir mtime back one second
        // to force the gate to take the "changed" branch deterministically
        // — equivalent to the seed having been written ≥1s before added.md
        // in real-world timing.
        let team_content_dir = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR);
        if let Some(stored) = cache.dir_mtimes.get_mut(&team_content_dir) {
            *stored = stored.saturating_sub(1);
        }

        // Drop a new flat-layout file AFTER the cold scan — simulates a
        // capture event that happened while the cache was warm.
        write_flat_story(
            &dir,
            "default",
            "added.md",
            "---\nstatus: todo\nsource_repo: alderco/codesense\nstory_id: W-1\n---\n# Added",
        );

        mtime_diff(&mut cache, &dir);

        let added_path = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR)
            .join("added.md");
        let added = cache
            .entries
            .get(&added_path)
            .expect("warm-start must discover flat-layout files");
        assert_eq!(added.source_repo(), "alderco/codesense");

        cleanup(&dir);
    }

    #[test]
    fn build_from_scan_multiple_teams() {
        let dir = test_dir("multi_team");
        setup_org(
            &dir,
            "alpha",
            "org",
            "repo",
            &[("s1.md", "---\nstatus: todo\n---\n# S1")],
        );
        setup_org(
            &dir,
            "beta",
            "org",
            "repo",
            &[("s2.md", "---\nstatus: done\n---\n# S2")],
        );

        let cache = FrontmatterCache::build_from_scan(&dir);
        assert_eq!(cache.entries.len(), 2);
        assert_eq!(cache.by_team("alpha").len(), 1);
        assert_eq!(cache.by_team("beta").len(), 1);

        cleanup(&dir);
    }

    #[test]
    fn build_from_scan_nested_dirs() {
        let dir = test_dir("nested");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("epic/sub/story.md", "---\nstatus: todo\n---\n# Nested")],
        );

        let cache = FrontmatterCache::build_from_scan(&dir);
        assert_eq!(cache.entries.len(), 1);

        cleanup(&dir);
    }

    #[test]
    fn query_empty_cache() {
        let cache = FrontmatterCache::new();
        assert!(cache.by_team("any").is_empty());
        assert!(cache.by_source_repo("any").is_empty());
        assert_eq!(cache.by_team_and_repo("any", "any").count(), 0);
    }

    #[test]
    fn update_and_remove_entry() {
        let mut cache = FrontmatterCache::new();
        let path = PathBuf::from("/tmp/test.md");
        let entry = CacheEntry {
            path: path.clone(),
            dir_team: "default".into(),
            fields: FrontmatterFields {
                status: Some("todo".into()),
                ..Default::default()
            },
            metadata_parse_error: None,
            open_tags: std::collections::BTreeMap::new(),
            mtime: 12345,
        };

        cache.update_entry(path.clone(), entry);
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.by_team("default").len(), 1);

        let removed = cache.remove_entry(&path);
        assert!(removed.is_some());
        assert_eq!(cache.entries.len(), 0);
    }

    #[test]
    fn prepare_and_commit_refresh_returns_old_and_new() {
        // R-2: Migrated from refresh_entry_returns_old_and_new. Verifies the
        // prepare_refresh / commit_refresh split preserves the (old, new)
        // contract.
        let dir = test_dir("refresh");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("s.md", "---\nstatus: todo\n---\n# S")],
        );

        let mut cache = FrontmatterCache::build_from_scan(&dir);
        assert_eq!(cache.entries.len(), 1);

        let story_path = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR)
            .join("s.md");

        // Modify the file. PTS-2: include `source_repo:` so the
        // orphan-skip in `build_cache_entry` doesn't drop the entry.
        fs::write(
            &story_path,
            "---\nstatus: done\nsource_repo: org/repo\nstory_id: S-1\n---\n# S updated",
        )
        .unwrap();

        let new = FrontmatterCache::prepare_refresh(&story_path, "default");
        let (old, new) = cache.commit_refresh(&story_path, new);
        assert!(old.is_some());
        assert!(new.is_some());
        assert_eq!(old.unwrap().status(), Some("todo"));
        assert_eq!(new.as_ref().unwrap().status(), Some("done"));
        assert_eq!(new.as_ref().unwrap().story_id(), Some("S-1"));

        cleanup(&dir);
    }

    #[test]
    fn cache_cold_done_file_is_never_stamped_on_refresh() {
        // SR-13: a `status: done` file with NO `completed_at`, discovered
        // cache-cold (old_status = None — e.g. a freshly pulled teammate's
        // story), must NOT be rewritten. The passive back-fill that stamped
        // `Utc::now()` here produced the spurious pulled-repo diffs this story
        // removes. The cache refresh is read-only.
        let dir = test_dir("sr13_cold_done");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[(
                "s.md",
                "---\nstatus: done\nsource_repo: org/repo\nstory_id: S-1\n---\n# S",
            )],
        );
        let story_path = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR)
            .join("s.md");
        let before = fs::read_to_string(&story_path).unwrap();

        // Cache-cold discovery: no prior cache entry. The refresh is read-only.
        let entry = FrontmatterCache::prepare_refresh(&story_path, "default")
            .expect("cold done story should still index");
        assert_eq!(entry.status(), Some("done"));

        let after = fs::read_to_string(&story_path).unwrap();
        assert_eq!(
            before, after,
            "cache refresh must not rewrite a cold done file"
        );
        assert!(
            !after.contains("completed_at"),
            "no completed_at may be stamped by a passive refresh"
        );

        cleanup(&dir);
    }

    #[test]
    fn commit_refresh_with_none_removes_entry() {
        // commit_refresh(path, None) should remove the entry (file deleted case)
        // and return (Some(old), None).
        let dir = test_dir("commit_none");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("s.md", "---\nstatus: todo\n---\n# S")],
        );

        let mut cache = FrontmatterCache::build_from_scan(&dir);
        let story_path = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR)
            .join("s.md");
        assert!(cache.entries.contains_key(&story_path));

        let (old, new) = cache.commit_refresh(&story_path, None);
        assert!(old.is_some());
        assert!(new.is_none());
        assert!(!cache.entries.contains_key(&story_path));

        cleanup(&dir);
    }

    #[test]
    fn recovered_fields_survive_persist_and_reload() {
        let dir = test_dir("recovered_persist");
        setup_org(&dir, "default", "org", "repo", &[("s.md", "---\ntype: story\nstatus: todo\nstory_id: REC-A\ntitle: Inbox: first pass\ndepends_on: [REC-B]\n---\n")]);
        let cache = FrontmatterCache::build_from_scan(&dir);
        assert_eq!(cache.entries.len(), 1);
        let cache_file = dir.join("cache.json");
        cache.persist_to_disk(&cache_file).unwrap();
        let loaded = FrontmatterCache::load_or_build(&cache_file, &dir);
        let entry = loaded.entries.values().next().unwrap();
        assert_eq!(entry.story_id(), Some("REC-A"));
        assert_eq!(entry.status(), Some("todo"));
        assert_eq!(entry.fields.depends_on.as_ref().unwrap(), &["REC-B"]);
        assert_eq!(loaded.version, 10);
        assert!(entry
            .metadata_parse_error
            .as_deref()
            .unwrap()
            .contains("Line 6 (title)"));
        cleanup(&dir);
    }

    #[test]
    fn persist_and_reload_round_trip() {
        let dir = test_dir("persist");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("s.md", "---\nstatus: todo\n---\n# S")],
        );

        let cache = FrontmatterCache::build_from_scan(&dir);
        let cache_file = dir.join("cache.json");
        cache.persist_to_disk(&cache_file).unwrap();

        let loaded = FrontmatterCache::load_or_build(&cache_file, &dir);
        assert_eq!(loaded.entries.len(), cache.entries.len());
        assert_eq!(loaded.version, CACHE_VERSION);

        cleanup(&dir);
    }

    #[test]
    fn load_or_build_falls_back_on_version_mismatch() {
        let dir = test_dir("version_mismatch");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("s.md", "---\nstatus: todo\n---\n# S")],
        );

        // Write a cache with wrong version
        let cache_file = dir.join("cache.json");
        let bad = r#"{"version":999,"entries":{}}"#;
        fs::write(&cache_file, bad).unwrap();

        let loaded = FrontmatterCache::load_or_build(&cache_file, &dir);
        assert_eq!(loaded.version, CACHE_VERSION);
        assert_eq!(loaded.entries.len(), 1); // Rebuilt from scan

        cleanup(&dir);
    }

    /// SW-AU-77F: a cache stamped with the pre-`tags` version (7) is invalidated so the
    /// new `tags` field is populated by a cold rebuild — not served as a stale `None`.
    /// Guards the `CACHE_VERSION` bump: reverting it would break `tags contains X` on
    /// already-cached stories, and this test (unlike the generic v999 case) would catch it.
    #[test]
    fn load_or_build_invalidates_pre_tags_version_7() {
        let dir = test_dir("pre_tags_version");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("s.md", "---\nstatus: todo\ntags: [infra]\n---\n# S")],
        );
        let cache_file = dir.join("cache.json");
        fs::write(&cache_file, r#"{"version":7,"entries":{}}"#).unwrap();

        let loaded = FrontmatterCache::load_or_build(&cache_file, &dir);
        assert_eq!(loaded.version, CACHE_VERSION);
        assert_eq!(
            loaded.entries.len(),
            1,
            "a v7 cache is a miss → cold rebuild"
        );

        cleanup(&dir);
    }

    #[test]
    fn load_or_build_cold_start_no_file() {
        let dir = test_dir("cold_start");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("s.md", "---\nstatus: todo\n---\n# S")],
        );

        let cache_file = dir.join("nonexistent.json");
        let loaded = FrontmatterCache::load_or_build(&cache_file, &dir);
        assert_eq!(loaded.entries.len(), 1);

        cleanup(&dir);
    }

    #[test]
    fn mtime_diff_removes_deleted_files() {
        let dir = test_dir("mtime_delete");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[
                ("a.md", "---\nstatus: todo\n---\n# A"),
                ("b.md", "---\nstatus: done\n---\n# B"),
            ],
        );

        let mut cache = FrontmatterCache::build_from_scan(&dir);
        assert_eq!(cache.entries.len(), 2);

        // Delete one file
        let b_path = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR)
            .join("b.md");
        fs::remove_file(&b_path).unwrap();

        mtime_diff(&mut cache, &dir);
        assert_eq!(cache.entries.len(), 1);

        cleanup(&dir);
    }

    #[test]
    fn mtime_of_returns_epoch_nanos() {
        let dir = test_dir("mtime_epoch");
        let file = dir.join("test.md");
        fs::write(&file, "content").unwrap();

        let mtime = mtime_of(&file).expect("should return Some for readable file");
        // SW-FX-9: mtime_of now returns epoch *nanoseconds*. A just-written
        // file's mtime is within a few seconds of `now`; `abs_diff` avoids any
        // subtract-overflow regardless of clock/FS granularity ordering.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        assert!(mtime > 0);
        assert!(
            now.abs_diff(mtime) < 10_000_000_000,
            "recent file should be within ~10s in ns (now={now}, mtime={mtime})"
        );

        // Unreadable file returns None
        assert!(mtime_of(Path::new("/nonexistent/file.md")).is_none());

        cleanup(&dir);
    }

    #[test]
    fn has_pipeline_extension_checks_correctly() {
        assert!(has_pipeline_extension(Path::new("story.md")));
        assert!(has_pipeline_extension(Path::new("story.html")));
        assert!(has_pipeline_extension(Path::new("story.htm")));
        assert!(!has_pipeline_extension(Path::new("readme.txt")));
        assert!(!has_pipeline_extension(Path::new("data.json")));
        assert!(!has_pipeline_extension(Path::new("noext")));
    }

    #[test]
    fn source_repo_inferred_from_frontmatter_when_present() {
        let dir = test_dir("fm_source_repo");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[(
                "s.md",
                "---\nstatus: todo\nsource_repo: custom/override\n---\n# S",
            )],
        );

        let cache = FrontmatterCache::build_from_scan(&dir);
        let entry = cache.entries.values().next().unwrap();
        assert_eq!(entry.source_repo(), "custom/override");

        cleanup(&dir);
    }

    #[test]
    fn team_inferred_from_frontmatter_when_present() {
        let dir = test_dir("fm_team");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("s.md", "---\nstatus: todo\nteam: custom-team\n---\n# S")],
        );

        let cache = FrontmatterCache::build_from_scan(&dir);
        let entry = cache.entries.values().next().unwrap();
        // team from frontmatter overrides directory-inferred team
        assert_eq!(entry.team(), "custom-team");

        cleanup(&dir);
    }

    // ---- R-5: refresh_entry_from_fields ----

    /// Asserts two cache entries are equal field-by-field, except mtime
    /// (which can drift slightly between two read-time captures).
    fn assert_cache_entries_match(a: &CacheEntry, b: &CacheEntry) {
        assert_eq!(a.path, b.path, "path");
        assert_eq!(a.team(), b.team(), "team");
        assert_eq!(a.source_repo(), b.source_repo(), "source_repo");
        assert_eq!(a.dir_team, b.dir_team, "dir_team");
        assert_eq!(a.status(), b.status(), "status");
        assert_eq!(a.document_type(), b.document_type(), "document_type");
        assert_eq!(a.story_id(), b.story_id(), "story_id");
        assert_eq!(a.depends_on(), b.depends_on(), "depends_on");
        assert_eq!(a.supporting_docs(), b.supporting_docs(), "supporting_docs");
    }

    #[test]
    fn refresh_entry_from_fields_matches_refresh_entry() {
        let dir = test_dir("r5_matches");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[(
                "match.md",
                "---\nstatus: in-progress\nteam: explicit-team\nstory_id: M-1\ndepends_on:\n  - dep-a\n  - dep-b\n---\n# Match",
            )],
        );

        let story_path = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR)
            .join("match.md");

        // Path A: prepare_refresh (reads file)
        let mut cache_a = FrontmatterCache::new();
        let new_a_prep = FrontmatterCache::prepare_refresh(&story_path, "default");
        let (_, new_a) = cache_a.commit_refresh(&story_path, new_a_prep);

        // Path B: prepare_refresh_from_fields (uses caller-supplied fields).
        // Extract fields the same way capture would.
        let content = fs::read_to_string(&story_path).unwrap();
        let fields =
            pipeline_format::extract_meta(pipeline_format::PipelineFileFormat::Markdown, &content);
        let mut cache_b = FrontmatterCache::new();
        let new_b_prep =
            FrontmatterCache::prepare_refresh_from_fields(&story_path, fields, "default");
        let (_, new_b) = cache_b.commit_refresh(&story_path, new_b_prep);

        let a = new_a.expect("prepare_refresh should insert");
        let b = new_b.expect("prepare_refresh_from_fields should insert");
        assert_cache_entries_match(&a, &b);

        cleanup(&dir);
    }

    #[test]
    fn refresh_entry_from_fields_skips_empty_html() {
        let dir = test_dir("r5_empty_html");
        let html_dir = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR);
        fs::create_dir_all(&html_dir).unwrap();
        let html_path = html_dir.join("ghost.html");
        // Plain HTML with no cs:* meta tags.
        fs::write(
            &html_path,
            "<!doctype html><html><body>Not a story</body></html>",
        )
        .unwrap();

        let empty = FrontmatterFields::default();
        assert!(empty.is_empty());

        let mut cache = FrontmatterCache::new();
        let prep = FrontmatterCache::prepare_refresh_from_fields(&html_path, empty, "default");
        let (old, new) = cache.commit_refresh(&html_path, prep);
        assert!(old.is_none(), "no prior entry");
        assert!(new.is_none(), "empty HTML should not be inserted");
        assert!(!cache.entries.contains_key(&html_path));

        cleanup(&dir);
    }

    #[test]
    fn refresh_entry_from_fields_inserts_md_with_default_fields() {
        // F.30 HTML gate must NOT apply to Markdown — empty-fields Markdown
        // is a valid (if minimal) entry.
        let dir = test_dir("r5_md_default");
        let md_dir = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR);
        fs::create_dir_all(&md_dir).unwrap();
        let md_path = md_dir.join("minimal.md");
        fs::write(&md_path, "# Minimal — no frontmatter at all").unwrap();

        let empty = FrontmatterFields::default();
        let mut cache = FrontmatterCache::new();
        let prep = FrontmatterCache::prepare_refresh_from_fields(&md_path, empty, "default");
        let (_, new) = cache.commit_refresh(&md_path, prep);
        let entry = new.expect("Markdown with default fields should still insert");
        assert_eq!(entry.team(), "default");
        // PTS-2: dir-derived `source_repo` fallback is retired; empty
        // frontmatter `source_repo` surfaces as `""` (the orphan-skip in
        // `build_cache_entry` doesn't fire on this `from_fields` path —
        // build_cache_entry_from_fields trusts the caller-supplied fields).
        assert_eq!(entry.source_repo(), "");
        assert!(entry.status().is_none());

        cleanup(&dir);
    }

    // -----------------------------------------------------------------------
    // R-3: merge_from
    // -----------------------------------------------------------------------

    /// In-memory CacheEntry factory for merge_from tests — avoids the disk
    /// I/O of `build_from_scan`. `mtime` and `status` are the fields the
    /// merge semantics actually inspect; the rest are filled with defaults.
    fn entry_at(path: &str, mtime: u64, status: &str) -> CacheEntry {
        CacheEntry {
            path: PathBuf::from(path),
            dir_team: "default".into(),
            fields: FrontmatterFields {
                status: Some(status.into()),
                ..Default::default()
            },
            metadata_parse_error: None,
            open_tags: std::collections::BTreeMap::new(),
            mtime,
        }
    }

    /// RF-perf-9 benchmark helper: a synthetic entry with a unique `story_id` and the
    /// realistic optional `Vec<String>` fields (`depends_on`/`supporting_docs`) so the
    /// `story_id` scan and (in the sibling `pipeline_scanner` bench) the `build_story_ref`
    /// clone are measured against representative shapes.
    #[cfg(test)]
    fn bench_entry(i: usize) -> (PathBuf, CacheEntry) {
        let path = PathBuf::from(format!("/tmp/rf_perf9_bench/story_{i}.md"));
        let entry = CacheEntry {
            path: path.clone(),
            dir_team: "default".into(),
            fields: FrontmatterFields {
                story_id: Some(format!("RF-bench-{i}")),
                status: Some("todo".into()),
                source_repo: Some("alderco/codesense".into()),
                depends_on: Some(vec![format!("RF-bench-{}", i.saturating_sub(1))]),
                supporting_docs: Some(vec!["README.md".into()]),
                ..Default::default()
            },
            metadata_parse_error: None,
            open_tags: std::collections::BTreeMap::new(),
            mtime: 0,
        };
        (path, entry)
    }

    /// RF-perf-9 (AC-4): the aux `story_id_index` — and the `by_story_id` lookup built
    /// on it — must agree **exactly** with a naive full-scan of `entries`, across every
    /// mutation path. Validates the private index structure directly (so a *stale*
    /// bucket, not just a missing one, is caught) plus per-id `by_story_id` parity.
    fn assert_index_parity(cache: &FrontmatterCache, ctx: &str) {
        use std::collections::BTreeSet;
        // The index, flattened to (id, path) pairs, must equal the naive scan set —
        // this catches BOTH missing mappings and stale ones the scan wouldn't produce.
        let index_pairs: BTreeSet<(String, PathBuf)> = cache
            .story_id_index
            .iter()
            .flat_map(|(id, paths)| paths.iter().map(move |p| (id.clone(), p.clone())))
            .collect();
        let naive: BTreeSet<(String, PathBuf)> = cache
            .entries
            .values()
            .filter_map(|e| e.story_id().map(|id| (id.to_string(), e.path.clone())))
            .collect();
        assert_eq!(
            index_pairs, naive,
            "story_id_index parity failed after {ctx}"
        );

        let ids: BTreeSet<String> = naive.iter().map(|(id, _)| id.clone()).collect();
        for id in ids {
            let got: BTreeSet<PathBuf> = cache
                .by_story_id(&id)
                .iter()
                .map(|e| e.path.clone())
                .collect();
            let want: BTreeSet<PathBuf> = cache
                .entries
                .values()
                .filter(|e| e.story_id() == Some(id.as_str()))
                .map(|e| e.path.clone())
                .collect();
            assert_eq!(got, want, "by_story_id({id}) parity failed after {ctx}");
        }
        assert!(
            cache.by_story_id("__no_such_id__").is_empty(),
            "a miss must return empty after {ctx}"
        );
    }

    fn id_entry(path: &str, id: Option<&str>) -> CacheEntry {
        CacheEntry {
            path: PathBuf::from(path),
            dir_team: "default".into(),
            fields: FrontmatterFields {
                story_id: id.map(|s| s.to_string()),
                ..Default::default()
            },
            metadata_parse_error: None,
            open_tags: std::collections::BTreeMap::new(),
            mtime: 0,
        }
    }

    #[test]
    fn rf_perf9_story_id_index_parity_across_mutations() {
        let mut cache = FrontmatterCache::new();
        assert_index_parity(&cache, "empty");

        // insert (upsert) — including one entry with no story_id
        cache.upsert_entry(PathBuf::from("/a.md"), id_entry("/a.md", Some("ID-1")));
        cache.upsert_entry(PathBuf::from("/b.md"), id_entry("/b.md", Some("ID-2")));
        cache.upsert_entry(PathBuf::from("/c.md"), id_entry("/c.md", None));
        assert_index_parity(&cache, "inserts");

        // ambiguous: a second path shares ID-1 (the `2+` case callers treat as neutral)
        cache.upsert_entry(PathBuf::from("/dup.md"), id_entry("/dup.md", Some("ID-1")));
        assert_eq!(cache.by_story_id("ID-1").len(), 2, "ambiguous id → 2 paths");
        assert_index_parity(&cache, "ambiguous insert");

        // id change at the SAME path (frontmatter edit) — old mapping must drop
        cache.upsert_entry(PathBuf::from("/a.md"), id_entry("/a.md", Some("ID-9")));
        assert!(
            cache
                .by_story_id("ID-1")
                .iter()
                .all(|e| e.path.as_path() != Path::new("/a.md")),
            "old id mapping for /a.md must be gone"
        );
        assert_index_parity(&cache, "id change");

        // remove (drop)
        cache.drop_entry(Path::new("/b.md"));
        assert_index_parity(&cache, "drop");

        // commit_refresh — replace then remove (the incremental hot path)
        cache.commit_refresh(
            Path::new("/dup.md"),
            Some(id_entry("/dup.md", Some("ID-7"))),
        );
        assert_index_parity(&cache, "commit_refresh replace");
        cache.commit_refresh(Path::new("/dup.md"), None);
        assert_index_parity(&cache, "commit_refresh remove");

        // merge_from — additive + newer-mtime override of the None-id /c.md
        let mut other = FrontmatterCache::new();
        other.upsert_entry(PathBuf::from("/m.md"), id_entry("/m.md", Some("ID-3")));
        let mut newer = id_entry("/c.md", Some("ID-4"));
        newer.mtime = 100;
        other.upsert_entry(PathBuf::from("/c.md"), newer);
        cache.merge_from(other);
        assert_index_parity(&cache, "merge_from");

        // clear
        cache.clear_entries();
        assert_index_parity(&cache, "clear");
        assert!(
            cache.story_id_index.is_empty(),
            "index cleared with entries"
        );
    }

    #[test]
    fn single_by_story_id_distinguishes_found_absent_and_ambiguous() {
        // The shared arity authority behind resolve_story_path (FE open),
        // ensure_story_worktree (SW-OR-1a create), and the worktree-index
        // branch-name fallback. All three depend on these three outcomes.
        let mut cache = FrontmatterCache::new();
        cache.upsert_entry(PathBuf::from("/a.md"), id_entry("/a.md", Some("ID-1")));

        // Exactly one → Ok(entry).
        assert_eq!(
            cache.single_by_story_id("ID-1").map(|e| e.path.clone()),
            Ok(PathBuf::from("/a.md"))
        );
        // None → NotFound (distinct from ambiguous).
        assert_eq!(
            cache.single_by_story_id("NOPE").err(),
            Some(StoryIdLookup::NotFound)
        );
        // 2+ → Ambiguous(count) so a caller can name the collision.
        cache.upsert_entry(PathBuf::from("/dup.md"), id_entry("/dup.md", Some("ID-1")));
        assert_eq!(
            cache.single_by_story_id("ID-1").err(),
            Some(StoryIdLookup::Ambiguous(2))
        );
    }

    // SW-AU-45: the reactive fire's owning-team resolution. A lone story → its team; an unknown or
    // ambiguous id → None (the clean-drop the reactive path relies on, never a fall to the active team).
    #[test]
    fn team_for_story_id_resolves_lone_story_else_none() {
        let mut cache = FrontmatterCache::new();
        cache.upsert_entry(PathBuf::from("/a.md"), id_entry("/a.md", Some("ID-1")));

        // Exactly one → its team (id_entry's dir_team).
        assert_eq!(cache.team_for_story_id("ID-1").as_deref(), Some("default"));
        // Unknown → None (e.g. a non-story handle like `repo::branch` that misses the id index).
        assert!(cache.team_for_story_id("repo::branch").is_none());
        // Ambiguous (2+ share the id) → None, not an arbitrary pick.
        cache.upsert_entry(PathBuf::from("/dup.md"), id_entry("/dup.md", Some("ID-1")));
        assert!(cache.team_for_story_id("ID-1").is_none());
    }

    #[test]
    fn rf_perf9_index_rebuilt_on_build_and_deserialize() {
        let dir = test_dir("perf9_rebuild");
        setup_org(
            &dir,
            "default",
            "acme",
            "repo",
            &[
                ("s1.md", "---\nstatus: todo\nstory_id: ID-A\n---\n# s1"),
                ("s2.md", "---\nstatus: todo\nstory_id: ID-B\n---\n# s2"),
            ],
        );
        // Cold build derives the index.
        let cache = FrontmatterCache::build_from_scan(&dir);
        assert_index_parity(&cache, "build_from_scan");
        assert_eq!(cache.by_story_id("ID-A").len(), 1);

        // Persist → deserialize: `#[serde(skip)]` drops the index; the load path must
        // rebuild it (validated here directly, as `load_or_build` does post-`mtime_diff`).
        let json = serde_json::to_string(&cache).unwrap();
        let mut loaded: FrontmatterCache = serde_json::from_str(&json).unwrap();
        assert!(
            loaded.by_story_id("ID-A").is_empty(),
            "index is empty immediately after deserialize (serde-skip)"
        );
        loaded.rebuild_story_id_index();
        assert_index_parity(&loaded, "rebuild post-deserialize");
        assert_eq!(loaded.by_story_id("ID-A").len(), 1);
        cleanup(&dir);
    }

    /// RF-perf-9 (AC-1/AC-2): `#[ignore]`d benchmark for the `story_id` scans + the
    /// warm-start `mtime_diff`. Reports per-call cost across N ∈ {200,1000,3000} so the
    /// ranking (AC-2) reads the `O(N)` scaling directly, not a single-N point. Machine-
    /// dependent → `#[ignore]`d, mirroring `slw_rs6_measure_walk_cost` /
    /// `sw_sy7_measure_claim_walk_breakdown`. Run:
    /// `cargo test -p codesense --lib rf_perf9_measure_frontmatter_lookups -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn rf_perf9_measure_frontmatter_lookups() {
        use std::time::Instant;
        const REPS: u32 = 2000;

        let before = crate::install::frontmatter_perf::COUNTERS.snapshot();
        println!("=== RF-perf-9 AC-2 by_story_id (per-call vs N — should be flat O(1)) ===");
        let mut sink = 0usize;
        for &n in &[200usize, 1000, 3000] {
            let mut cache = FrontmatterCache::new();
            for i in 0..n {
                let (p, e) = bench_entry(i);
                cache.upsert_entry(p, e); // RF-perf-9: keep the aux index populated
            }
            let target = format!("RF-bench-{}", n / 2);

            let t = Instant::now();
            for _ in 0..REPS {
                sink += cache.by_story_id(&target).len();
            }
            let by_id_ns = t.elapsed().as_nanos() as f64 / REPS as f64;

            // The pre-fix O(N) scan cost, measured inline for comparison (the aux index
            // removed exactly this from the hot path — before/after in one run).
            let t = Instant::now();
            for _ in 0..REPS {
                sink += cache
                    .entries
                    .values()
                    .filter(|e| e.story_id() == Some(target.as_str()))
                    .count();
            }
            let scan_ns = t.elapsed().as_nanos() as f64 / REPS as f64;

            println!(
                "N={n:5} | by_story_id (index)={by_id_ns:7.0} ns/call | old O(N) scan={scan_ns:8.0} ns/call"
            );
        }
        let after = crate::install::frontmatter_perf::COUNTERS.snapshot();
        println!(
            "always-on tally delta: by_story_id calls={} (sink={sink})",
            after.story_id_lookup_calls - before.story_id_lookup_calls,
        );

        // Warm-start mtime_diff — the cold path, on real disk (build once, re-diff with
        // nothing changed to isolate the recurring multi-pass cost).
        println!("=== RF-perf-9 warm-start mtime_diff (cold path) ===");
        let dir = test_dir("perf9_mtime");
        let stories: Vec<(String, String)> = (0..1000)
            .map(|i| {
                (
                    format!("s{i}.md"),
                    format!("---\nstatus: todo\nstory_id: RF-mt-{i}\n---\n# s{i}\n"),
                )
            })
            .collect();
        let refs: Vec<(&str, &str)> = stories
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        setup_org(&dir, "default", "alderco", "codesense", &refs);
        let mut cache = FrontmatterCache::build_from_scan(&dir);
        let t = Instant::now();
        mtime_diff(&mut cache, &dir);
        println!(
            "mtime_diff warm N={} elapsed_us={}",
            cache.entries.len(),
            t.elapsed().as_micros()
        );
        cleanup(&dir);
    }

    #[test]
    fn merge_from_inserts_entries_absent_from_self() {
        let mut cache = FrontmatterCache::new();
        let mut other = FrontmatterCache::new();
        let path = PathBuf::from("/tmp/new.md");
        other
            .entries
            .insert(path.clone(), entry_at("/tmp/new.md", 100, "todo"));

        cache.merge_from(other);

        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.entries.get(&path).unwrap().mtime, 100);
    }

    #[test]
    fn merge_from_updates_when_other_has_newer_mtime() {
        let mut cache = FrontmatterCache::new();
        let path = PathBuf::from("/tmp/a.md");
        cache
            .entries
            .insert(path.clone(), entry_at("/tmp/a.md", 100, "todo"));

        let mut other = FrontmatterCache::new();
        other
            .entries
            .insert(path.clone(), entry_at("/tmp/a.md", 200, "done"));

        cache.merge_from(other);

        let entry = cache.entries.get(&path).unwrap();
        assert_eq!(entry.mtime, 200);
        assert_eq!(entry.status(), Some("done"));
    }

    #[test]
    fn merge_from_preserves_self_when_existing_is_newer_or_equal() {
        let mut cache = FrontmatterCache::new();
        let path = PathBuf::from("/tmp/a.md");
        // self represents a live watcher update (newer than the scan result)
        cache
            .entries
            .insert(path.clone(), entry_at("/tmp/a.md", 200, "done"));

        let mut other = FrontmatterCache::new();
        // other represents the scan result (older view)
        other
            .entries
            .insert(path.clone(), entry_at("/tmp/a.md", 100, "todo"));

        cache.merge_from(other);

        let entry = cache.entries.get(&path).unwrap();
        assert_eq!(entry.mtime, 200);
        assert_eq!(entry.status(), Some("done"));
    }

    #[test]
    fn merge_from_preserves_self_when_mtime_is_equal() {
        // Equal mtime means the scan saw the same revision as the watcher;
        // self's entry wins because the watcher path is authoritative for
        // in-memory mutations (e.g., status transitions that haven't re-stat'd).
        let mut cache = FrontmatterCache::new();
        let path = PathBuf::from("/tmp/a.md");
        cache
            .entries
            .insert(path.clone(), entry_at("/tmp/a.md", 100, "done"));

        let mut other = FrontmatterCache::new();
        other
            .entries
            .insert(path.clone(), entry_at("/tmp/a.md", 100, "todo"));

        cache.merge_from(other);

        assert_eq!(cache.entries.get(&path).unwrap().status(), Some("done"));
    }

    #[test]
    fn merge_from_does_not_remove_entries_absent_from_other() {
        // A watcher-added entry that the scan didn't see must survive the merge.
        let mut cache = FrontmatterCache::new();
        let path = PathBuf::from("/tmp/live.md");
        cache
            .entries
            .insert(path.clone(), entry_at("/tmp/live.md", 100, "todo"));

        let other = FrontmatterCache::new();
        cache.merge_from(other);

        assert!(cache.entries.contains_key(&path));
    }

    // ----- R-7 T1: per-directory mtime tracking ---------------------------

    #[test]
    fn build_from_scan_populates_dir_mtimes() {
        let dir = test_dir("dir_mtimes_populated");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[
                ("a.md", "---\nstatus: todo\n---\n# A"),
                ("sub/b.md", "---\nstatus: todo\n---\n# B"),
            ],
        );

        let cache = FrontmatterCache::build_from_scan(&dir);
        assert_eq!(cache.entries.len(), 2);
        let repo_dir = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR);
        assert!(
            cache.dir_mtimes.contains_key(&repo_dir),
            "repo dir should be tracked"
        );
        assert!(
            cache.dir_mtimes.contains_key(&repo_dir.join("sub")),
            "subdir should be tracked",
        );

        cleanup(&dir);
    }

    #[test]
    fn dir_mtimes_round_trips_through_persist() {
        let dir = test_dir("dir_mtimes_persist");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("a.md", "---\nstatus: todo\n---\n# A")],
        );

        let cache = FrontmatterCache::build_from_scan(&dir);
        let cache_file = dir.join("cache.json");
        cache.persist_to_disk(&cache_file).unwrap();

        let loaded = FrontmatterCache::load_or_build(&cache_file, &dir);
        assert_eq!(loaded.entries.len(), cache.entries.len());
        assert_eq!(
            loaded.dir_mtimes.len(),
            cache.dir_mtimes.len(),
            "dir_mtimes must survive persist + reload",
        );

        cleanup(&dir);
    }

    #[test]
    fn mtime_gate_skips_read_dir_when_unchanged() {
        // Directly verifies the optimization: if dir_mtimes[d] matches disk's
        // current mtime for d, mtime_diff's Phase 3 does NOT read_dir on d —
        // even if there's an unindexed file inside.
        let dir = test_dir("mtime_gate_skips");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("a.md", "---\nstatus: todo\n---\n# A")],
        );

        let mut cache = FrontmatterCache::build_from_scan(&dir);
        assert_eq!(cache.entries.len(), 1);

        let repo_dir = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR);

        let new_file = repo_dir.join("hidden.md");
        fs::write(&new_file, "---\nstatus: todo\n---\n# hidden").unwrap();

        // Poison: pretend the cache was persisted AFTER hidden.md was created.
        let post_write_mtime = mtime_of(&repo_dir).unwrap();
        cache.dir_mtimes.insert(repo_dir.clone(), post_write_mtime);

        mtime_diff(&mut cache, &dir);

        assert!(
            !cache.entries.contains_key(&new_file),
            "mtime-gated mtime_diff must skip read_dir on unchanged dirs",
        );
        assert_eq!(cache.entries.len(), 1);

        cleanup(&dir);
    }

    #[test]
    fn mtime_gate_full_walk_when_dir_mtimes_empty() {
        // Simulates first run after R-7 upgrade.
        let dir = test_dir("mtime_gate_fallback");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("a.md", "---\nstatus: todo\n---\n# A")],
        );

        let mut cache = FrontmatterCache::build_from_scan(&dir);
        cache.dir_mtimes.clear();

        let repo_dir = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR);
        let new_file = repo_dir.join("fresh.md");
        // PTS-2: orphan-skip requires frontmatter source_repo on every entry.
        fs::write(
            &new_file,
            "---\nstatus: todo\nsource_repo: org/repo\n---\n# fresh",
        )
        .unwrap();

        mtime_diff(&mut cache, &dir);

        assert!(
            cache.entries.contains_key(&new_file),
            "empty dir_mtimes → fallback full walk → fresh file must be discovered",
        );
        assert!(
            !cache.dir_mtimes.is_empty(),
            "fallback walk must repopulate dir_mtimes for the next warm start",
        );
        assert!(cache.dir_mtimes.contains_key(&repo_dir));

        cleanup(&dir);
    }

    #[test]
    fn mtime_diff_prunes_dead_dir_mtimes() {
        let dir = test_dir("dir_mtimes_prune");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("epic/story.md", "---\nstatus: todo\n---\n# S")],
        );

        let mut cache = FrontmatterCache::build_from_scan(&dir);
        let epic_dir = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR)
            .join("epic");
        assert!(cache.dir_mtimes.contains_key(&epic_dir));

        fs::remove_dir_all(&epic_dir).unwrap();
        mtime_diff(&mut cache, &dir);

        assert!(
            !cache.dir_mtimes.contains_key(&epic_dir),
            "dead dir_mtimes entries must be pruned",
        );

        cleanup(&dir);
    }

    // ----- SW-FX-9: scan-side freshness gate -------------------------------

    /// AC-3: the pure freshness comparison. Drives all three arms with no FS
    /// timing race: a disk mtime strictly newer than the cached entry is
    /// stale (re-parse); strictly older is fresh; equal is fresh (an
    /// unchanged file — this arm is what keeps AC-6 memoization intact).
    #[test]
    fn mtime_is_fresh_arms() {
        // Disk strictly newer than the cached entry → stale.
        assert!(!mtime_is_fresh(100, 101), "disk newer must be stale");
        // Disk strictly older than the cached entry → fresh.
        assert!(mtime_is_fresh(100, 99), "disk older must be fresh");
        // Equal → fresh (unchanged file; re-parsing here would break memoization).
        assert!(mtime_is_fresh(100, 100), "equal must be fresh");
        // Boundary: one nanosecond newer is the smallest stale step.
        assert!(!mtime_is_fresh(u64::MAX - 1, u64::MAX));
        assert!(mtime_is_fresh(u64::MAX, u64::MAX));
    }

    /// AC-1: `entry_is_fresh` stats the real file. A cache entry stamped with
    /// an mtime far in the past (simulating a stale index) is reported stale
    /// against a file that exists on disk; an entry stamped with the file's
    /// actual mtime is fresh; a missing file is stale (fall through to disk).
    #[test]
    fn entry_is_fresh_against_disk() {
        let dir = test_dir("entry_is_fresh");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("s.md", "---\nsource_repo: org/repo\nstatus: todo\n---\n# S")],
        );
        let cache = FrontmatterCache::build_from_scan(&dir);
        let real = cache
            .entries
            .values()
            .find(|e| e.path.file_name().and_then(|n| n.to_str()) == Some("s.md"))
            .expect("scanned entry for s.md");

        // An entry holding the file's real mtime is fresh.
        assert!(entry_is_fresh(real), "untouched entry must be fresh");

        // An entry stamped in the distant past is stale (disk is newer).
        let stale = CacheEntry {
            mtime: 0,
            ..real.clone()
        };
        assert!(!entry_is_fresh(&stale), "past-mtime entry must be stale");

        // A missing file is stale → caller falls through to a fresh disk read.
        let gone = CacheEntry {
            path: dir.join("default/content/does-not-exist.md"),
            ..real.clone()
        };
        assert!(!entry_is_fresh(&gone), "missing file must be stale");

        cleanup(&dir);
    }

    /// AC-12 / AC-14 (Part 2B): the transition-free refresh re-reads one path
    /// and updates that entry in place (no full rebuild); index consumers
    /// (`by_team_and_repo`) then read the fresh fields — the staleness a
    /// watcher-suppressed content write would otherwise leave until cold boot.
    #[test]
    fn refresh_no_transition_updates_single_entry() {
        let dir = test_dir("refresh_no_transition");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("s.md", "---\nsource_repo: org/repo\nstatus: todo\n---\n# S")],
        );
        let cache_arc = FrontmatterCache::shared(FrontmatterCache::build_from_scan(&dir));
        let path = {
            let c = cache_arc.read().unwrap();
            c.entries
                .keys()
                .find(|p| p.ends_with("s.md"))
                .expect("indexed s.md")
                .clone()
        };
        assert_eq!(
            cache_arc
                .read()
                .unwrap()
                .entries
                .get(&path)
                .unwrap()
                .status(),
            Some("todo"),
        );

        // Simulate a suppressed content write: status done + a new supporting_docs ref.
        fs::write(
            &path,
            "---\nsource_repo: org/repo\nstatus: done\nsupporting_docs:\n  - d.excalidraw\n---\n# S",
        )
        .unwrap();

        let (old, new) =
            FrontmatterCache::refresh_under_lock_no_transition(&cache_arc, &path, "default");
        assert_eq!(old.unwrap().status(), Some("todo"));
        assert_eq!(new.unwrap().status(), Some("done"));

        // AC-14: the index consumer reads the fresh fields.
        let c = cache_arc.read().unwrap();
        let entry = c
            .by_team_and_repo("default", "org/repo")
            .next()
            .expect("entry visible to index consumer");
        assert_eq!(entry.status(), Some("done"));
        assert!(
            entry
                .fields
                .supporting_docs
                .as_ref()
                .is_some_and(|d| d.iter().any(|s| s == "d.excalidraw")),
            "supporting_docs must be visible to index consumers after refresh",
        );
        drop(c);
        cleanup(&dir);
    }

    /// AC-13 / AC-15 (Part 2B): the refresh is read-only w.r.t. disk — it runs
    /// NO status transition and never rewrites the dest. That no-write property
    /// is what keeps it from defeating the materialize's watcher-suppression
    /// (a write here would fire the watcher → risk a reverse-sync loop).
    #[test]
    fn refresh_no_transition_does_not_write_dest() {
        let dir = test_dir("refresh_no_write");
        // status `done` with no `completed_at` — the shape that would tempt a
        // status transition to stamp + rewrite the file.
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("s.md", "---\nsource_repo: org/repo\nstatus: done\n---\n# S")],
        );
        let cache_arc = FrontmatterCache::shared(FrontmatterCache::build_from_scan(&dir));
        let path = {
            let c = cache_arc.read().unwrap();
            c.entries
                .keys()
                .find(|p| p.ends_with("s.md"))
                .expect("indexed s.md")
                .clone()
        };
        let before = fs::read(&path).unwrap();
        let before_mtime = mtime_of(&path);

        FrontmatterCache::refresh_under_lock_no_transition(&cache_arc, &path, "default");

        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "refresh must not rewrite the dest"
        );
        assert_eq!(
            mtime_of(&path),
            before_mtime,
            "refresh must not touch dest mtime"
        );
        cleanup(&dir);
    }

    // SLW-RI-3: synthetic entry for the RCU concurrency/panic tests.
    fn ri3_stub_entry(p: &str) -> CacheEntry {
        CacheEntry {
            path: std::path::PathBuf::from(p),
            dir_team: "default".to_string(),
            fields: crate::commands::pipeline_format::FrontmatterFields::default(),
            metadata_parse_error: None,
            open_tags: std::collections::BTreeMap::new(),
            mtime: 1,
        }
    }

    #[test]
    fn sv_cde_100_the_key_change_flag_is_set_by_a_rekey_and_cleared_by_the_publish() {
        let stub = |id: Option<&str>| {
            let mut entry = ri3_stub_entry("/a/1.md");
            entry.fields.story_id = id.map(str::to_string);
            entry
        };
        let path = std::path::PathBuf::from("/a/1.md");
        let cache = FrontmatterCache::new_shared();
        FrontmatterCache::rcu(&cache, |c| c.upsert_entry(path.clone(), stub(Some("A-1"))));
        assert!(
            !FrontmatterCache::snapshot(&cache).keys_changed,
            "a publish carries the flag, not the snapshot"
        );
        FrontmatterCache::rcu(&cache, |c| {
            c.upsert_entry(path.clone(), stub(Some("A-1")));
            assert!(!c.keys_changed, "an unchanged refresh is not a change");
            c.upsert_entry(path.clone(), stub(Some("A-2")));
            assert!(c.keys_changed, "an id edit is");
        });
    }

    #[test]
    fn sv_cde_100_a_refresh_is_a_key_change_only_when_the_id_or_team_differs() {
        let dir = test_dir("key_change_refresh");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[(
                "s.md",
                "---\ntype: story\nstatus: todo\nstory_id: KC-1\nsource_repo: a/b\n---\n# S",
            )],
        );
        let path = dir
            .join("default")
            .join(crate::context_repo::TEAM_CONTENT_DIR)
            .join("s.md");
        let cache = FrontmatterCache::shared(FrontmatterCache::build_from_scan(&dir));
        let refresh = |content: &str| {
            std::fs::write(&path, content).unwrap();
            let new = FrontmatterCache::prepare_refresh(&path, "default");
            FrontmatterCache::rcu(&cache, |c| {
                c.commit_refresh(&path, new);
                c.keys_changed
            })
        };
        assert!(!refresh(
            "---\ntype: story\nstatus: done\nstory_id: KC-1\nsource_repo: a/b\n---\n# S edited"
        ));
        assert!(refresh(
            "---\ntype: story\nstatus: done\nstory_id: KC-2\nsource_repo: a/b\n---\n# S"
        ));
        cleanup(&dir);
    }

    #[test]
    fn ri3_concurrent_rcu_writes_do_not_lose_updates() {
        // SLW-RI-3 (AC-5, guarantee a): the RCU write lock serializes concurrent
        // writers, so two threads each inserting 200 distinct entries produce a
        // cache with all 400 — no lost updates from interleaved
        // read(clone)-modify-publish. A plain `RwLock<Cache>` with in-place
        // mutation would also serialize; the point is that the snapshot-swap RCU
        // does NOT drop a concurrent writer's committed entry.
        let cache = FrontmatterCache::new_shared();
        let c1 = cache.clone();
        let c2 = cache.clone();
        let h1 = std::thread::spawn(move || {
            for i in 0..200 {
                let p = format!("/a/{i}.md");
                FrontmatterCache::rcu(&c1, |c| {
                    c.entries
                        .insert(std::path::PathBuf::from(&p), ri3_stub_entry(&p));
                });
            }
        });
        let h2 = std::thread::spawn(move || {
            for i in 0..200 {
                let p = format!("/b/{i}.md");
                FrontmatterCache::rcu(&c2, |c| {
                    c.entries
                        .insert(std::path::PathBuf::from(&p), ri3_stub_entry(&p));
                });
            }
        });
        h1.join().unwrap();
        h2.join().unwrap();
        assert_eq!(
            FrontmatterCache::snapshot(&cache).entries.len(),
            400,
            "concurrent RCU writers must not lose updates"
        );
    }

    #[test]
    fn ri3_concurrent_evict_and_refresh_of_live_source_never_clobbers() {
        // SLW-RI-3 (AC-5, guarantee b — the load-bearing race): one wave "evicts"
        // source S from a STALE snapshot while another refreshes S concurrently.
        // Because every evict/refresh RE-READS disk inside the RCU write section
        // and S exists on disk throughout, S is always rebuilt (`Some`) — never
        // removed. So a live shared source survives concurrent evict+refresh
        // regardless of interleaving. (An unguarded stale-snapshot evict — using
        // the snapshot's value instead of re-reading disk — would drop S.)
        let dir = test_dir("ri3_concurrent_evict_refresh");
        setup_org(
            &dir,
            "default",
            "org",
            "repo",
            &[("s.md", "---\nsource_repo: org/repo\nstatus: todo\n---\n# S")],
        );
        let cache = FrontmatterCache::shared(FrontmatterCache::build_from_scan(&dir));
        let path = cache
            .read()
            .unwrap()
            .entries
            .keys()
            .find(|p| p.ends_with("s.md"))
            .expect("indexed s.md")
            .clone();

        let c_evict = cache.clone();
        let p_evict = path.clone();
        let evicter = std::thread::spawn(move || {
            for _ in 0..300 {
                // The "evict from a stale snapshot" path — re-reads disk under the
                // RCU lock; S is present, so it refreshes rather than removes.
                FrontmatterCache::refresh_many_no_transition(
                    &c_evict,
                    "default",
                    std::slice::from_ref(&p_evict),
                );
            }
        });
        let c_refresh = cache.clone();
        let p_refresh = path.clone();
        let refresher = std::thread::spawn(move || {
            for _ in 0..300 {
                FrontmatterCache::refresh_under_lock(&c_refresh, &p_refresh, "default");
            }
        });
        evicter.join().unwrap();
        refresher.join().unwrap();

        assert!(
            FrontmatterCache::snapshot(&cache)
                .entries
                .contains_key(&path),
            "a live shared source must survive concurrent evict+refresh (AC-5)"
        );
        cleanup(&dir);
    }

    #[test]
    fn ri3_rcu_panic_leaves_snapshot_intact_and_recovers() {
        // SLW-RI-3 (AC-6): a panic inside an RCU closure mutated only the private
        // clone (never published), so the authoritative snapshot is untouched; the
        // poisoned lock is recovered on the next write (no bricking of the cache).
        let cache = FrontmatterCache::new_shared();
        FrontmatterCache::rcu(&cache, |c| {
            // RF-perf-9: choke-point keeps the aux story_id index consistent.
            c.upsert_entry(
                std::path::PathBuf::from("/keep.md"),
                ri3_stub_entry("/keep.md"),
            );
        });
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            FrontmatterCache::rcu(&cache, |c| {
                c.clear_entries();
                panic!("boom mid-mutation");
            });
        }));
        assert!(r.is_err(), "the panic must propagate");
        // Authoritative snapshot untouched — the `clear()` ran on the discarded
        // clone and was never published.
        assert_eq!(
            FrontmatterCache::snapshot(&cache).entries.len(),
            1,
            "panic must not publish the mutation"
        );
        // Subsequent write still works — poison recovered via `into_inner`.
        FrontmatterCache::rcu(&cache, |c| c.clear_entries());
        assert_eq!(
            FrontmatterCache::snapshot(&cache).entries.len(),
            0,
            "cache must remain writable after a poisoned write"
        );
    }

    #[test]
    fn v1_cache_triggers_rebuild_at_load_via_version_mismatch() {
        // R-14 (replaces R-7 v1-compat test): `CacheEntry` was restructured
        // to compose `FrontmatterFields` directly, so the v1 JSON shape
        // (individual `team`/`source_repo`/`status`/etc. fields at the
        // entry top level) no longer deserializes. Confirms:
        //   (1) Direct from_str on v1 JSON fails — fields are now nested.
        //   (2) load_or_build falls through to build_from_scan, producing
        //       a v2 cache populated from the actual disk state.
        let dir = test_dir("v1_cache_rebuild");
        let cache_path = dir.join("cache.json");
        let v1_json = r#"{
            "version": 1,
            "entries": {
                "/tmp/story.md": {
                    "path": "/tmp/story.md",
                    "team": "default",
                    "source_repo": "org/repo",
                    "dir_team": "default",
                    "status": "done",
                    "document_type": null,
                    "story_id": null,
                    "depends_on": null,
                    "supporting_docs": null,
                    "completed_at": "2024-06-15T12:00:00Z",
                    "mtime": 12345
                }
            }
        }"#;
        fs::write(&cache_path, v1_json).unwrap();

        // (1) v1 JSON no longer deserializes — `fields` is now required.
        let result: Result<FrontmatterCache, _> = serde_json::from_str(v1_json);
        assert!(
            result.is_err(),
            "v1 cache shape should fail to deserialize after R-14 restructure",
        );

        // (2) load_or_build falls through (the failed deserialize bypasses
        // the version check) and rebuilds from build_from_scan.
        let loaded = FrontmatterCache::load_or_build(&cache_path, &dir);
        assert_eq!(loaded.version, CACHE_VERSION);
        assert_eq!(
            loaded.entries.len(),
            0,
            "no actual story files in temp dir → empty fresh cache",
        );

        cleanup(&dir);
    }

    // ----- R-7 T4: by_team_and_repo iterator API --------------------------

    #[test]
    fn by_team_and_repo_iterator_yields_expected_entries() {
        let dir = test_dir("by_tr_iter");
        setup_org(
            &dir,
            "alpha",
            "acme",
            "api",
            &[
                ("s1.md", "---\nstatus: todo\n---\n# 1"),
                ("s2.md", "---\nstatus: done\n---\n# 2"),
            ],
        );
        setup_org(
            &dir,
            "alpha",
            "acme",
            "web",
            &[("s3.md", "---\nstatus: todo\n---\n# 3")],
        );
        setup_org(
            &dir,
            "beta",
            "acme",
            "api",
            &[("s4.md", "---\nstatus: todo\n---\n# 4")],
        );

        let cache = FrontmatterCache::build_from_scan(&dir);

        assert_eq!(cache.by_team_and_repo("nope", "nope").count(), 0);

        let mut names: Vec<String> = cache
            .by_team_and_repo("alpha", "acme/api")
            .map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["s1.md".to_string(), "s2.md".to_string()]);

        cleanup(&dir);
    }

    // ----- R-7 T6: warm-start benchmark -----------------------------------
    //
    // Run with:
    //   cd src-tauri && cargo test --release bench_warm_start -- --ignored --nocapture
    //
    // Fixture is 15 epics × 14 stories = 210 stories across 3 directory layers.
    // Measures the median of 5 warm-start `load_or_build` invocations against
    // a pre-persisted cache.

    #[test]
    #[ignore = "perf bench — run with --release --ignored --nocapture"]
    fn bench_warm_start_load_or_build() {
        use chrono::{Duration, Utc};

        let dir = test_dir("bench_warm_start");
        // Timestamps RELATIVE to Utc::now() so the bench doesn't silently
        // change meaning as years pass. Nest under `done:` so build_cache_entry
        // actually parses the value.
        let base = Utc::now() - Duration::days(180);
        let mut owned: Vec<(String, String)> = Vec::new();
        for ep in 0..15 {
            for st in 0..14 {
                let path = format!("epic{ep}/sub{}/story-{st}.md", ep % 3);
                let ts = (base + Duration::days((st as i64) % 28)).to_rfc3339();
                let body = format!(
                    "---\nstatus: done\nstory_id: S-{ep}-{st}\ndone:\n  completed_at: \"{ts}\"\n---\n# S{ep}-{st}",
                );
                owned.push((path, body));
            }
        }
        let pairs: Vec<(&str, &str)> = owned
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        setup_org(&dir, "default", "alderco", "codesense", &pairs);

        let cache_path = dir.join("cache.json");
        let cold = FrontmatterCache::build_from_scan(&dir);
        cold.persist_to_disk(&cache_path).expect("persist");
        assert!(
            cold.entries.len() >= 200,
            "fixture should have ≥200 stories"
        );

        let mut times: Vec<std::time::Duration> = Vec::with_capacity(5);
        for _ in 0..5 {
            let start = std::time::Instant::now();
            let loaded = FrontmatterCache::load_or_build(&cache_path, &dir);
            times.push(start.elapsed());
            assert_eq!(loaded.entries.len(), cold.entries.len());
            std::hint::black_box(&loaded);
        }
        times.sort();
        let median = times[2];
        println!(
            "[BENCH r7/warm_start] entries={} median={:?} all={:?}",
            cold.entries.len(),
            median,
            times
        );

        cleanup(&dir);
    }
}

#[cfg(test)]
mod navigation_cache_deadline_tests {
    use super::*;
    #[test]
    fn expired_navigation_does_not_clone_or_publish_a_cache_snapshot() {
        let cache = FrontmatterCache::new_shared();
        let before = FrontmatterCache::snapshot(&cache);
        assert!(!FrontmatterCache::reindex_scan_gaps_with_deadline(
            &cache,
            &[PathBuf::from("/ctx/team/content/story.md")],
            std::time::Instant::now()
        ));
        assert!(Arc::ptr_eq(&before, &FrontmatterCache::snapshot(&cache)));
    }
    #[test]
    fn navigation_exits_when_another_writer_holds_the_cache() {
        let cache = FrontmatterCache::new_shared();
        let guard = cache.write().unwrap();
        let start = std::time::Instant::now();
        assert_eq!(
            FrontmatterCache::rcu_with_deadline(
                &cache,
                Some(start + std::time::Duration::from_millis(10)),
                |_| ()
            ),
            None
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        drop(guard);
    }
}
