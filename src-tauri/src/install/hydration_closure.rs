// SPDX-License-Identifier: MIT
//! SW-HV-6 — hydration **scope policies**: the closure engine that turns
//! SW-SY-5's fixed one-epic hydrate into a user-configurable dial.
//!
//! A [`HydrationPolicy`] is a dial along three axes — **reference** (walk
//! `supporting_docs` N hops), **ancestry** (parent epic dirs N levels), and
//! **sibling breadth** (the whole surrounding epic) — bounded by `max_members`.
//! [`LIGHT`] (all axes off) is exactly SY-5's single-epic hydrate, so the common
//! case stays byte-identical and the heavier closures are opt-in sync cost.
//!
//! The engine is split in two so it is both correct and testable:
//! - [`resolve_closure_over`] is a **pure** BFS over an abstract entry-resolver
//!   (`FnMut(&Path) -> Option<ResolvedRef>`): cycle-guarded reference walk,
//!   content-root-bounded ancestry, sibling breadth, `max_members` cap →
//!   `truncated`, per-axis provenance, epic-dir de-dup. No disk, no cache — unit
//!   tested with a fixture resolver.
//! - [`resolve_hydration_closure`] wraps it with the **freshness gate** (AC-4):
//!   the reference resolver reads a frontier entry's `supporting_docs` only after
//!   verifying that entry is fresh (`entry_is_fresh` → `refresh_under_lock`),
//!   under one RCU snapshot (never a mutex held across disk I/O). A cold/missing
//!   ref target is refresh-read or recorded `deferred`, never guessed.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::frontmatter_cache::{
    entry_is_fresh, CacheEntry, FrontmatterCache, SharedFrontmatterCache,
};
use crate::context_repo::TEAM_CONTENT_DIR;

/// Default `max_members` cap for [`LIGHT`] and any preset that leaves it unset —
/// an epic-dir ceiling that bounds the worst-case closure (a widely-referenced
/// doc or deep chain) so the pull can never runaway. Surfaced (`truncated`) when
/// hit — never a silent cap (AC-5).
pub const DEFAULT_MAX_MEMBERS: u16 = 64;

/// The scope dial. Each axis `0`/`false` = off. `LIGHT` (all off) is
/// SY-5's single-epic hydrate exactly. Serialized `camelCase` for the FE mirror
/// (`src/shared/lib/hydration/policy.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HydrationPolicy {
    /// Reference axis: BFS over `supporting_docs` this many hops. `0` = off.
    pub reference_depth: u8,
    /// Ancestry axis: include parent epic dirs this many levels up, bounded at
    /// the team content root. `0` = off.
    pub ancestry_levels: u8,
    /// Sibling axis: include the whole surrounding epic (the parent dir's
    /// subtree). `false` = off.
    pub sibling_breadth: bool,
    /// Epic-dir ceiling. On overflow the closure stops and flags `truncated`.
    pub max_members: u16,
}

/// The light default — the story's own epic only, zero closure work. Identical
/// to what SY-5's `hydrate_story` materializes. Keeps the common-case sync cost
/// exactly today's (AC-1, AC-9).
pub const LIGHT: HydrationPolicy = HydrationPolicy {
    reference_depth: 0,
    ancestry_levels: 0,
    sibling_breadth: false,
    max_members: DEFAULT_MAX_MEMBERS,
};

impl HydrationPolicy {
    /// Is this the light default (all axes off)? A light policy does **zero**
    /// closure work — the closure walk early-returns to
    /// the plain epic hydrate on it (AC-1). `max_members` does not affect
    /// light-ness (an off dial is off regardless of its ceiling).
    pub fn is_light(&self) -> bool {
        self.reference_depth == 0 && self.ancestry_levels == 0 && !self.sibling_breadth
    }
}

impl Default for HydrationPolicy {
    fn default() -> Self {
        LIGHT
    }
}

/// Which axis first pulled an epic into the closure — the provenance the preview
/// renders as a chip (AC-8). De-dup keeps the first (highest-priority) axis:
/// seed > reference > ancestry > sibling (discovery order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClosureAxis {
    Seed,
    Reference,
    Ancestry,
    Sibling,
}

/// One epic dir in the resolved closure, tagged with the axis that pulled it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureMember {
    /// Absolute epic-dir path (a story's parent dir).
    pub epic_dir: PathBuf,
    /// The axis that first reached this epic.
    pub axis: ClosureAxis,
}

/// The resolved closure: the union of epic dirs (ordered, deduped, provenance-
/// tagged), whether the cap was hit, how many stale reference entries were
/// refreshed under the gate, and any cold/missing reference targets that could
/// not be made fresh (the preview's closure-error state — AC-8).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClosureResult {
    pub members: Vec<ClosureMember>,
    pub truncated: bool,
    pub stale_refreshed: usize,
    /// Reference edges that resolved to a real on-disk story file we still could
    /// not index (never guessed). Non-story / non-existent edges are silently
    /// skipped — only a genuine cold story is a deferral.
    pub deferred: Vec<PathBuf>,
}

impl ClosureResult {
    /// The union of epic dirs, in discovery order.
    pub fn epic_dirs(&self) -> Vec<PathBuf> {
        self.members.iter().map(|m| m.epic_dir.clone()).collect()
    }
}

/// What the entry-resolver returns for a story path: its epic dir (parent) and
/// its outbound reference edges (resolved to absolute story paths). Returned
/// only for a story that is present and fresh — the pure BFS never sees a stale
/// or guessed entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRef {
    pub epic_dir: PathBuf,
    pub edges: Vec<PathBuf>,
}

/// Accumulator that enforces the `max_members` cap and epic-dir de-dup while
/// preserving discovery order + first-axis provenance.
struct Acc {
    members: Vec<ClosureMember>,
    seen: HashSet<PathBuf>,
    truncated: bool,
    max: usize,
}

impl Acc {
    fn new(max: usize) -> Self {
        Acc {
            members: Vec::new(),
            seen: HashSet::new(),
            truncated: false,
            max,
        }
    }

    /// Add `dir` under `axis` if new and under the cap. First axis wins on
    /// de-dup. Flags `truncated` (never silently drops) when the cap is hit.
    fn add(&mut self, dir: PathBuf, axis: ClosureAxis) {
        if self.seen.contains(&dir) {
            return;
        }
        if self.members.len() >= self.max {
            self.truncated = true;
            return;
        }
        self.seen.insert(dir.clone());
        self.members.push(ClosureMember {
            epic_dir: dir,
            axis,
        });
    }
}

/// SW-HV-6 (AC-3, AC-5) — the **pure** closure walk. Generic over `resolve`, an
/// entry-resolver mapping a story path → its epic dir + reference edges (`None`
/// for anything not present-and-fresh). No disk, no cache: the freshness gate
/// lives in the `resolve` the prod caller supplies, so this is exhaustively
/// unit-testable with a fixture resolver.
///
/// - **reference** — BFS over edges to `reference_depth` hops, visited-set cycle
///   guard, each reached epic tagged `Reference`.
/// - **ancestry** — parent dirs up `ancestry_levels`, bounded strictly inside
///   `content_root` (the team `content` dir is never itself an epic).
/// - **sibling** — the surrounding epic (`seed_epic_dir.parent()`), same bound.
/// - **cap** — stop at `max_members`, set `truncated`.
pub fn resolve_closure_over<F>(
    seed_epic_dir: PathBuf,
    seed_story_path: PathBuf,
    content_root: Option<&Path>,
    policy: &HydrationPolicy,
    mut resolve: F,
) -> ClosureResult
where
    F: FnMut(&Path) -> Option<ResolvedRef>,
{
    let mut acc = Acc::new(policy.max_members.max(1) as usize);
    let mut deferred: Vec<PathBuf> = Vec::new();

    // base — the seed story's own epic (always first, so it wins de-dup).
    acc.add(seed_epic_dir.clone(), ClosureAxis::Seed);

    // reference axis — cycle-guarded BFS over supporting_docs.
    if policy.reference_depth > 0 {
        let mut visited: HashSet<PathBuf> = HashSet::new();
        visited.insert(seed_story_path.clone());
        let mut frontier = vec![seed_story_path.clone()];
        for _hop in 0..policy.reference_depth {
            let mut next: Vec<PathBuf> = Vec::new();
            for story in &frontier {
                let Some(resolved) = resolve(story) else {
                    continue;
                };
                for edge in resolved.edges {
                    if !visited.insert(edge.clone()) {
                        continue; // cycle / already walked
                    }
                    match resolve(&edge) {
                        Some(target) => {
                            acc.add(target.epic_dir, ClosureAxis::Reference);
                            next.push(edge);
                        }
                        None => deferred.push(edge),
                    }
                }
            }
            if acc.truncated || next.is_empty() {
                break;
            }
            frontier = next;
        }
    }

    // ancestry axis — parent dirs, bounded strictly inside the content root.
    if policy.ancestry_levels > 0 {
        let mut cur = seed_epic_dir.clone();
        for _ in 0..policy.ancestry_levels {
            let Some(parent) = cur.parent().map(Path::to_path_buf) else {
                break;
            };
            if !is_inside_content_root(&parent, content_root) {
                break;
            }
            acc.add(parent.clone(), ClosureAxis::Ancestry);
            if acc.truncated {
                break;
            }
            cur = parent;
        }
    }

    // sibling axis — the whole surrounding epic (the parent dir's subtree).
    if policy.sibling_breadth {
        if let Some(parent) = seed_epic_dir.parent().map(Path::to_path_buf) {
            if is_inside_content_root(&parent, content_root) {
                acc.add(parent, ClosureAxis::Sibling);
            }
        }
    }

    ClosureResult {
        members: acc.members,
        truncated: acc.truncated,
        stale_refreshed: 0,
        deferred,
    }
}

/// A candidate ancestor is a valid epic only if it is a **proper descendant** of
/// the team content root (the `content` dir itself is the root, never an epic).
///
/// **No known content root → DENY** (`false`). A cache-resolved seed always lives
/// under a `{team}/content/` dir (`for_each_indexable_dir` only indexes those), so
/// `content_root` is `Some` in production. If it is ever `None` (a non-standard
/// layout that could not have been indexed anyway), the safe posture is to add
/// NOTHING on the structural axes rather than walk arbitrarily upward toward the
/// fs root (`users/`, sibling teams, …) — the `max_members` cap would bound the
/// blast radius, but the *contents* would be wrong. Deny by default.
fn is_inside_content_root(candidate: &Path, content_root: Option<&Path>) -> bool {
    match content_root {
        Some(cr) => candidate.starts_with(cr) && candidate != cr,
        None => false,
    }
}

/// SW-HV-6 (AC-4) — the freshness-gated prod resolver. Snapshots the cache once
/// (lock-free), then walks the closure; every reference frontier entry is read
/// only after `entry_is_fresh` passes, else `refresh_under_lock` (snapshot →
/// lock-free disk read → single RCU commit — never a mutex across I/O) refreshes
/// it first. A cold/missing edge that is a real on-disk story we still cannot
/// index is recorded `deferred` (never guessed); a non-story / non-existent edge
/// is silently skipped. Structural axes (ancestry/sibling) are fresh by
/// construction. Anything landing after the snapshot self-heals on the next wave
/// (monotonic).
pub fn resolve_hydration_closure(
    cache: &SharedFrontmatterCache,
    seed_epic_dir: &Path,
    seed_story_path: &Path,
    dir_team: &str,
    policy: &HydrationPolicy,
) -> ClosureResult {
    let content_root = content_root_of(seed_epic_dir);
    let view =
        crate::context_repo::inheritance::content_owner(seed_story_path).and_then(|(root, _)| {
            Some(crate::context_repo::inheritance::team_view(
                &root,
                &crate::context_repo::inheritance::org_slug_of(&root)?,
                dir_team,
            ))
        });
    let admitted = |path: &Path| view.as_ref().is_none_or(|v| v.read_path(path).is_ok());
    if !admitted(seed_story_path) {
        return ClosureResult::default();
    }
    let snapshot = FrontmatterCache::snapshot(cache);
    // Memoize per path so a node reached as both an edge-target and a frontier
    // node is refreshed at most once (keeps `stale_refreshed` honest).
    let mut memo: HashMap<PathBuf, Option<ResolvedRef>> = HashMap::new();
    let stale_refreshed = std::cell::Cell::new(0usize);

    let resolve = |path: &Path| -> Option<ResolvedRef> {
        if !admitted(path) {
            return None;
        }
        if let Some(hit) = memo.get(path) {
            return hit.clone();
        }
        let entry = fresh_entry(cache, &snapshot, path, dir_team, &stale_refreshed);
        let out = entry.map(|e| ResolvedRef {
            epic_dir: e
                .path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| e.path.clone()),
            edges: resolve_edges(&e)
                .into_iter()
                .filter(|p| admitted(p))
                .collect(),
        });
        memo.insert(path.to_path_buf(), out.clone());
        out
    };

    let mut result = resolve_closure_over(
        seed_epic_dir.to_path_buf(),
        seed_story_path.to_path_buf(),
        content_root.as_deref(),
        policy,
        resolve,
    );
    // Only a genuine cold *story* (real on-disk pipeline file we couldn't index)
    // is a deferral; drop non-existent / non-story edges collected by the walk.
    //
    // Filter against a FRESH post-walk snapshot, not the pre-walk one: the
    // freshness gate may have `refresh_under_lock`-committed entries during the
    // walk (RCU), so a target that got indexed mid-walk must read as present
    // here. (A refreshed-in target already resolved to `Some` and so never
    // reached `deferred` — this is belt-and-suspenders, and keeps the filter
    // obviously correct rather than reasoning about the pre-walk snapshot.)
    result.deferred.retain(|p| admitted(p));
    let post_snapshot = FrontmatterCache::snapshot(cache);
    result
        .deferred
        .retain(|p| is_cold_story_on_disk(&post_snapshot, p));
    result.stale_refreshed = stale_refreshed.get();
    result
}

/// Fetch a fresh [`CacheEntry`] for `path`: trust the snapshot if the entry is
/// present and fresh; otherwise `refresh_under_lock` (RCU) and take the freshly
/// committed entry. A cold path is refresh-read too (never guessed) — `None`
/// when it still cannot be indexed (missing / non-story).
fn fresh_entry(
    cache: &SharedFrontmatterCache,
    snapshot: &FrontmatterCache,
    path: &Path,
    dir_team: &str,
    stale_refreshed: &std::cell::Cell<usize>,
) -> Option<CacheEntry> {
    let owning_team =
        crate::context_repo::inheritance::real_content_owner(path).map(|(_, team)| team);
    let dir_team = owning_team.as_deref().unwrap_or(dir_team);
    if let Some(entry) = snapshot.entries.get(path) {
        if entry_is_fresh(entry) {
            return Some(entry.clone());
        }
        // Stale — refresh before reading its edges (AC-4). Count only when the
        // refresh actually produced a fresh entry, consistent with the cold path
        // below (`stale_refreshed` = entries successfully refreshed-in, not
        // refresh *attempts* — a stale entry whose file vanished counts 0).
        let (_old, new) = FrontmatterCache::refresh_under_lock(cache, path, dir_team);
        if new.is_some() {
            stale_refreshed.set(stale_refreshed.get() + 1);
        }
        return new;
    }
    // Cold / missing — attempt a refresh-read; count it only if it produced an
    // entry (a real story we hadn't indexed).
    let (_old, new) = FrontmatterCache::refresh_under_lock(cache, path, dir_team);
    if new.is_some() {
        stale_refreshed.set(stale_refreshed.get() + 1);
    }
    new
}

/// Resolve an entry's `supporting_docs` (relative to the story file's dir) to
/// absolute, lexically-normalized story paths. Non-empty only for entries with
/// forward reference edges.
fn resolve_edges(entry: &CacheEntry) -> Vec<PathBuf> {
    let Some(base) = entry.path.parent() else {
        return Vec::new();
    };
    entry
        .supporting_docs()
        .unwrap_or(&[])
        .iter()
        .map(|d| lexical_normalize(&base.join(d)))
        .collect()
}

/// Is `path` a real on-disk pipeline-format story file that the (snapshot) cache
/// does not index? That is the only edge worth flagging `deferred` — a genuine
/// cold story. A missing file or a non-story doc (e.g. an `.html` mockup) is not
/// an error.
fn is_cold_story_on_disk(snapshot: &FrontmatterCache, path: &Path) -> bool {
    if snapshot.entries.contains_key(path) {
        return false;
    }
    let is_pipeline = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|ext| crate::commands::pipeline_format::detect_format(ext).is_some())
        .unwrap_or(false);
    is_pipeline && path.is_file()
}

/// The team content root for an epic dir: the nearest ancestor named
/// [`TEAM_CONTENT_DIR`] (`content`). Ancestry/sibling axes are bounded strictly
/// inside it. `None` when the epic dir is not under a `content` dir (a
/// non-standard layout — the axes then simply walk unbounded to the fs root,
/// which the caps still bound).
///
/// `pub(crate)` because it doubles as the **team content dir** locator for the
/// SW-HV-6 config commands: `{org_root}/{team}/content` is the git-shared team
/// content root (where team-default policy lives), so a command
/// with only the worktree's story path can find its team's default without
/// re-deriving org_root/team by hand.
pub(crate) fn content_root_of(epic_dir: &Path) -> Option<PathBuf> {
    let mut cur = Some(epic_dir);
    while let Some(p) = cur {
        if p.file_name().and_then(|n| n.to_str()) == Some(TEAM_CONTENT_DIR) {
            return Some(p.to_path_buf());
        }
        cur = p.parent();
    }
    None
}

/// The team name owning `epic_dir` — the path component just above its
/// `content` root (`{org_root}/{team}/content/...`). Used as `refresh_under_lock`'s
/// `dir_team` and to label the freshness refresh, robust regardless of the app's
/// active team. Empty string when the layout has no `content` root.
pub(crate) fn team_of(epic_dir: &Path) -> String {
    content_root_of(epic_dir)
        .and_then(|cr| {
            cr.parent()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

/// Lexically resolve `.` / `..` without touching disk (so a reference edge does
/// not incur a `canonicalize` syscall per hop and stays firmlink-neutral). A
/// leading `..` with nothing to pop is preserved (defensive; real content-repo
/// paths are absolute).
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                _ => out.push(Component::ParentDir),
            },
            c => out.push(c),
        }
    }
    out.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- policy model ----

    #[test]
    fn light_is_light_regardless_of_cap() {
        assert!(LIGHT.is_light());
        let capped = HydrationPolicy {
            max_members: 5,
            ..LIGHT
        };
        assert!(capped.is_light());
    }

    #[test]
    fn any_axis_on_is_not_light() {
        assert!(!HydrationPolicy {
            reference_depth: 1,
            ..LIGHT
        }
        .is_light());
        assert!(!HydrationPolicy {
            ancestry_levels: 1,
            ..LIGHT
        }
        .is_light());
        assert!(!HydrationPolicy {
            sibling_breadth: true,
            ..LIGHT
        }
        .is_light());
    }

    #[test]
    fn serde_round_trip_and_camel_case() {
        let p = HydrationPolicy {
            reference_depth: 2,
            ancestry_levels: 1,
            sibling_breadth: true,
            max_members: 64,
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("referenceDepth"), "camelCase: {json}");
        assert!(json.contains("siblingBreadth"), "camelCase: {json}");
        let back: HydrationPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    // ---- pure closure walk (fixture resolver) ----

    // A fixture cache: story path -> (epic dir, edges). Mirrors what the prod
    // freshness-gated resolver produces, minus the disk/cache concern.
    fn fixture(
        map: &HashMap<PathBuf, (PathBuf, Vec<PathBuf>)>,
    ) -> impl FnMut(&Path) -> Option<ResolvedRef> + '_ {
        move |p: &Path| {
            map.get(p).map(|(epic, edges)| ResolvedRef {
                epic_dir: epic.clone(),
                edges: edges.clone(),
            })
        }
    }

    fn pb(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn light_yields_seed_only() {
        let map = HashMap::new();
        let res = resolve_closure_over(
            pb("/c/content/epicA"),
            pb("/c/content/epicA/story.md"),
            Some(Path::new("/c/content")),
            &LIGHT,
            fixture(&map),
        );
        assert_eq!(res.epic_dirs(), vec![pb("/c/content/epicA")]);
        assert_eq!(res.members[0].axis, ClosureAxis::Seed);
        assert!(!res.truncated);
    }

    #[test]
    fn reference_axis_transitive_with_cycle_guard() {
        // story -> b -> c -> back to story (cycle). depth 2 reaches b's + c's epic.
        let mut map = HashMap::new();
        map.insert(
            pb("/c/content/eA/story.md"),
            (pb("/c/content/eA"), vec![pb("/c/content/eB/b.md")]),
        );
        map.insert(
            pb("/c/content/eB/b.md"),
            (pb("/c/content/eB"), vec![pb("/c/content/eC/c.md")]),
        );
        map.insert(
            pb("/c/content/eC/c.md"),
            (pb("/c/content/eC"), vec![pb("/c/content/eA/story.md")]), // cycle back
        );
        let policy = HydrationPolicy {
            reference_depth: 2,
            ..LIGHT
        };
        let res = resolve_closure_over(
            pb("/c/content/eA"),
            pb("/c/content/eA/story.md"),
            Some(Path::new("/c/content")),
            &policy,
            fixture(&map),
        );
        let dirs = res.epic_dirs();
        assert!(dirs.contains(&pb("/c/content/eA")));
        assert!(dirs.contains(&pb("/c/content/eB")));
        assert!(dirs.contains(&pb("/c/content/eC")));
        assert_eq!(dirs.len(), 3, "cycle back to seed adds nothing new");
        assert!(!res.truncated);
    }

    #[test]
    fn reference_depth_bounds_hops() {
        let mut map = HashMap::new();
        map.insert(
            pb("/c/content/eA/story.md"),
            (pb("/c/content/eA"), vec![pb("/c/content/eB/b.md")]),
        );
        map.insert(
            pb("/c/content/eB/b.md"),
            (pb("/c/content/eB"), vec![pb("/c/content/eC/c.md")]),
        );
        map.insert(pb("/c/content/eC/c.md"), (pb("/c/content/eC"), vec![]));
        let policy = HydrationPolicy {
            reference_depth: 1,
            ..LIGHT
        };
        let res = resolve_closure_over(
            pb("/c/content/eA"),
            pb("/c/content/eA/story.md"),
            Some(Path::new("/c/content")),
            &policy,
            fixture(&map),
        );
        let dirs = res.epic_dirs();
        assert!(dirs.contains(&pb("/c/content/eB")));
        assert!(!dirs.contains(&pb("/c/content/eC")), "1 hop stops at eB");
    }

    #[test]
    fn ancestry_axis_bounded_at_content_root() {
        let map = HashMap::new();
        // seed epic is content/parent/child; ancestry 5 levels must stop before
        // the content root (never add content/ itself or above).
        let policy = HydrationPolicy {
            ancestry_levels: 5,
            ..LIGHT
        };
        let res = resolve_closure_over(
            pb("/c/content/parent/child"),
            pb("/c/content/parent/child/story.md"),
            Some(Path::new("/c/content")),
            &policy,
            fixture(&map),
        );
        let dirs = res.epic_dirs();
        assert!(dirs.contains(&pb("/c/content/parent")), "level 1 parent");
        assert!(
            !dirs.contains(&pb("/c/content")),
            "content root is never an epic"
        );
        assert!(!dirs.contains(&pb("/c")), "never above the content root");
        // seed + one ancestor.
        assert_eq!(dirs.len(), 2);
    }

    #[test]
    fn no_content_root_denies_structural_axes_no_runaway_upward_walk() {
        // Degenerate layout: content_root is None. Ancestry/sibling must add
        // NOTHING (deny) rather than walk arbitrarily up toward the fs root.
        let map = HashMap::new();
        let policy = HydrationPolicy {
            ancestry_levels: 5,
            sibling_breadth: true,
            ..LIGHT
        };
        let res = resolve_closure_over(
            pb("/somewhere/odd/epicA"),
            pb("/somewhere/odd/epicA/story.md"),
            None, // no content root
            &policy,
            fixture(&map),
        );
        assert_eq!(
            res.epic_dirs(),
            vec![pb("/somewhere/odd/epicA")],
            "with no content root, only the seed — no upward walk"
        );
    }

    #[test]
    fn sibling_axis_adds_surrounding_epic() {
        let map = HashMap::new();
        let policy = HydrationPolicy {
            sibling_breadth: true,
            ..LIGHT
        };
        let res = resolve_closure_over(
            pb("/c/content/group/mine"),
            pb("/c/content/group/mine/story.md"),
            Some(Path::new("/c/content")),
            &policy,
            fixture(&map),
        );
        let m = res
            .members
            .iter()
            .find(|m| m.epic_dir == pb("/c/content/group"))
            .expect("surrounding epic present");
        assert_eq!(m.axis, ClosureAxis::Sibling);
    }

    #[test]
    fn axes_compose_and_dedup_with_first_axis_winning() {
        // reference edge points into the SAME dir the ancestry axis would add —
        // reference runs first, so the epic is tagged Reference (first wins).
        let mut map = HashMap::new();
        map.insert(
            pb("/c/content/parent/child/story.md"),
            (
                pb("/c/content/parent/child"),
                vec![pb("/c/content/parent/ref.md")],
            ),
        );
        map.insert(
            pb("/c/content/parent/ref.md"),
            (pb("/c/content/parent"), vec![]),
        );
        let policy = HydrationPolicy {
            reference_depth: 1,
            ancestry_levels: 1,
            ..LIGHT
        };
        let res = resolve_closure_over(
            pb("/c/content/parent/child"),
            pb("/c/content/parent/child/story.md"),
            Some(Path::new("/c/content")),
            &policy,
            fixture(&map),
        );
        let m = res
            .members
            .iter()
            .find(|m| m.epic_dir == pb("/c/content/parent"))
            .expect("parent present once");
        assert_eq!(m.axis, ClosureAxis::Reference, "first axis wins de-dup");
        assert_eq!(
            res.epic_dirs()
                .iter()
                .filter(|d| **d == pb("/c/content/parent"))
                .count(),
            1,
            "no duplicate"
        );
    }

    #[test]
    fn max_members_caps_and_flags_truncated_never_silent() {
        // A hub story referencing many distinct epics; cap at 3 (seed + 2).
        let mut map = HashMap::new();
        let edges: Vec<PathBuf> = (0..10)
            .map(|i| pb(&format!("/c/content/e{i}/s.md")))
            .collect();
        map.insert(
            pb("/c/content/seed/story.md"),
            (pb("/c/content/seed"), edges.clone()),
        );
        for (i, e) in edges.iter().enumerate() {
            map.insert(e.clone(), (pb(&format!("/c/content/e{i}")), vec![]));
        }
        let policy = HydrationPolicy {
            reference_depth: 1,
            max_members: 3,
            ..LIGHT
        };
        let res = resolve_closure_over(
            pb("/c/content/seed"),
            pb("/c/content/seed/story.md"),
            Some(Path::new("/c/content")),
            &policy,
            fixture(&map),
        );
        assert_eq!(res.members.len(), 3, "hard cap");
        assert!(res.truncated, "cap surfaced, never silent");
    }

    #[test]
    fn deferred_edge_when_ref_target_unresolvable() {
        // seed references a doc the resolver can't resolve → deferred, never
        // guessed. (The prod wrapper further filters to real cold stories.)
        let mut map = HashMap::new();
        map.insert(
            pb("/c/content/eA/story.md"),
            (pb("/c/content/eA"), vec![pb("/c/content/gone/missing.md")]),
        );
        let policy = HydrationPolicy {
            reference_depth: 1,
            ..LIGHT
        };
        let res = resolve_closure_over(
            pb("/c/content/eA"),
            pb("/c/content/eA/story.md"),
            Some(Path::new("/c/content")),
            &policy,
            fixture(&map),
        );
        assert_eq!(
            res.epic_dirs(),
            vec![pb("/c/content/eA")],
            "no phantom epic"
        );
        assert_eq!(res.deferred, vec![pb("/c/content/gone/missing.md")]);
    }

    #[test]
    fn lexical_normalize_resolves_dotdot() {
        assert_eq!(
            lexical_normalize(Path::new("/c/content/eA/../eB/b.md")),
            pb("/c/content/eB/b.md")
        );
    }

    #[test]
    fn content_root_of_finds_content_ancestor() {
        assert_eq!(
            content_root_of(Path::new("/x/team/content/a/b")),
            Some(pb("/x/team/content"))
        );
        assert_eq!(content_root_of(Path::new("/no/root/here")), None);
    }

    #[test]
    fn team_of_reads_component_above_content() {
        assert_eq!(team_of(Path::new("/org/teamx/content/epicA")), "teamx");
        assert_eq!(team_of(Path::new("/no/content/here/x")), "no");
    }

    // ---- AC-4 freshness gate (real disk + real cache) ----

    fn write_story(path: &Path, story_id: &str, supporting: &[&str]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut fm = format!("---\ntype: story\nsource_repo: acme/api\nstory_id: {story_id}\n");
        if !supporting.is_empty() {
            fm.push_str("supporting_docs:\n");
            for s in supporting {
                fm.push_str(&format!("  - {s}\n"));
            }
        }
        fm.push_str("---\n# body\n");
        std::fs::write(path, fm).unwrap();
    }

    /// AC-4 (hard, non-negotiable): a `supporting_docs` edge that was edited on
    /// disk after the cache was built is **refreshed then walked** — the closure
    /// follows the NEW on-disk edge, never the stale cached one. This is the
    /// load-bearing "no stale-cache closure — ever" guarantee.
    #[test]
    fn reference_walk_refreshes_stale_edge_and_never_uses_stale_target() {
        use super::super::frontmatter_cache::FrontmatterCache;

        let tmp = tempfile::tempdir().unwrap();
        let org = tmp.path();
        let content = org.join("teamx").join("content");
        let seed = content.join("epicA").join("story.md");
        let old_target = content.join("epicOld").join("old.md");
        let new_target = content.join("epicNew").join("new.md");

        // Build state: seed references OLD only.
        write_story(&seed, "SEED", &["../epicOld/old.md"]);
        write_story(&old_target, "OLD", &[]);
        write_story(&new_target, "NEW", &[]);

        let cache = FrontmatterCache::new_shared();
        FrontmatterCache::rebuild_from_disk(&cache, org);

        // The cache now indexes seed's OLD edge. Rewrite the seed on disk to
        // reference NEW instead — the cache is now STALE for the seed.
        write_story(&seed, "SEED", &["../epicNew/new.md"]);
        // Force the seed entry stale deterministically (mtime granularity aside).
        FrontmatterCache::rcu(&cache, |c| {
            if let Some(e) = c.entries.get_mut(&seed) {
                e.mtime = 0;
            }
        });

        let policy = HydrationPolicy {
            reference_depth: 1,
            ..LIGHT
        };
        let res =
            resolve_hydration_closure(&cache, &content.join("epicA"), &seed, "teamx", &policy);
        let dirs = res.epic_dirs();

        assert!(
            dirs.contains(&content.join("epicNew")),
            "refreshed edge → NEW epic walked: {dirs:?}"
        );
        assert!(
            !dirs.contains(&content.join("epicOld")),
            "stale edge must NOT survive the refresh: {dirs:?}"
        );
        assert!(
            res.stale_refreshed >= 1,
            "the stale seed entry was refreshed under the gate"
        );
    }

    /// AC-4 corollary: when the cache is already fresh, the walk uses it directly
    /// and refreshes nothing (the gate is not a blanket re-read).
    #[test]
    fn reference_walk_trusts_fresh_cache_without_refresh() {
        use super::super::frontmatter_cache::FrontmatterCache;

        let tmp = tempfile::tempdir().unwrap();
        let org = tmp.path();
        let content = org.join("teamx").join("content");
        let seed = content.join("epicA").join("story.md");
        let target = content.join("epicB").join("b.md");
        write_story(&seed, "SEED", &["../epicB/b.md"]);
        write_story(&target, "B", &[]);

        let cache = FrontmatterCache::new_shared();
        FrontmatterCache::rebuild_from_disk(&cache, org);

        let policy = HydrationPolicy {
            reference_depth: 1,
            ..LIGHT
        };
        let res =
            resolve_hydration_closure(&cache, &content.join("epicA"), &seed, "teamx", &policy);
        assert!(res.epic_dirs().contains(&content.join("epicB")));
        assert_eq!(res.stale_refreshed, 0, "fresh cache → no refresh");
    }
    #[test]
    fn hydrate_story_reference_closure_cannot_leave_the_view_or_spoof_seed_team() {
        let (home, root) = crate::context_repo::inheritance::test_fixture();
        let seed = root.join("child/content/epic/story.md");
        write_story(
            &seed,
            "SEED",
            &[
                "../../../parent/content/secret.md",
                "../../../grand/content/shared.md",
            ],
        );
        write_story(&root.join("parent/content/secret.md"), "HIDDEN", &[]);
        write_story(&root.join("grand/content/shared.md"), "SHARED", &[]);
        let spoof = root.join("parent/content/spoof.md");
        std::fs::write(&spoof, "---\ntype: story\nsource_repo: acme/api\nstory_id: SPOOF\nteam: child\n---\nparent-secret-bytes\n").unwrap();
        let cache = FrontmatterCache::new_shared();
        FrontmatterCache::rebuild_from_disk(&cache, &root);
        let result = resolve_hydration_closure(
            &cache,
            seed.parent().unwrap(),
            &seed,
            "child",
            &HydrationPolicy {
                reference_depth: 1,
                ..LIGHT
            },
        );
        assert!(result.epic_dirs().contains(&root.join("grand/content")));
        let shown = format!("{result:?}");
        assert!(!shown.contains("parent/content"));
        assert!(!shown.contains("secret.md"));
        let ctx = crate::commands::content_engine::derive_resolver_context(
            home.path().to_path_buf(),
            1,
            "acme/ctx".into(),
            Some("child".into()),
        )
        .unwrap();
        let snapshot = FrontmatterCache::snapshot(&cache);
        for id in ["SPOOF", "MISSING"] {
            let e = crate::commands::route_to_context::resolve_seed_paths(
                &snapshot,
                &ctx,
                "acme/api",
                id,
                "hydrate_story",
            );
            assert!(e.is_err());
        }
    }
}
