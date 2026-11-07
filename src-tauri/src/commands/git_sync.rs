// SPDX-License-Identifier: MIT
//! SW-GS-2 — Git Sync Workbench engine: the typed verb surface over the
//! existing in-memory `git merge-tree` plumbing in [`super::git`].
//!
//! This module is the cohesive home for the six git-sync verbs (so `git.rs`
//! does not absorb another ~1k lines). Each verb is a thin wrapper: it resolves
//! the active context-repo clone (which IS `org_root` — the context repo's git
//! root, no subdir), reuses the deterministic, tested merge primitives in
//! `git.rs`, and returns typed JSON the front-end renders (never a scraped TUI).
//!
//! The dangerous git mechanics live in `git.rs` and stay there; this module
//! only orchestrates and routes (the SW-GS any-file doc-aware + binary
//! pick-a-side policy that REPLACES the CP `.meta/*.json`-only policy).
//!
//! Transport split (architecture §2.0, F4): [`get_publish_set`] is exposed BOTH
//! as a Tauri command (the session-less picker) AND as an MCP verb (the agent),
//! over the one shared [`publish_set_impl`]. The other five verbs are MCP-only.
//!
//! All result types here serialize to SW-GS-1's locked TS view-model
//! (`src/features/git-sync/lib/types.ts`) — that file is the single source of
//! truth; these structs mirror it field-for-field.

use crate::attribution::history::{default_branch, default_branch_name, default_branch_ref};
use crate::shared::git_util::{open_repo, tree_or_err};
use std::path::Path;

pub(crate) mod context_worktree;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// AC-3 — `get_publish_set` typed view-model (mirrors SW-GS-1 `types.ts`).
// ---------------------------------------------------------------------------

/// Per-file classification badge (architecture G8/G9/G12). Surfaced
/// read-only in the picker/V2. Serializes kebab-case to match the TS
/// `PublishFileBadge` union (`"eol-only" | "mode" | "rename"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PublishFileBadge {
    /// G8 — whitespace/EOL-only change (content identical after newline normalize).
    EolOnly,
    /// G9 — mode or symlink change.
    Mode,
    /// G12 — rename.
    Rename,
}

/// Workspace kind used to label/glyph a publish bucket. `general` is the
/// synthetic unattributed bucket. Matches TS `PublishBucketKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PublishBucketKind {
    Story,
    Epic,
    General,
}

/// A workspace reference labeling a publish bucket (TS `WorkspaceRef`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRef {
    /// Stable, rename-safe id (workspace id / slug).
    pub id: String,
    /// Agent-agnostic human label.
    pub label: String,
    pub kind: PublishBucketKind,
}

/// Per-file Add/Modify/Delete status vs the publish baseline (SW-GS-2).
/// Serializes lowercase to match the TS `PublishFile.status` union. Distinct
/// from `badges` (which classify the *kind* of a non-add/delete change).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PublishFileStatus {
    Added,
    Modified,
    Deleted,
}

/// Map a libgit2 delta status to the coarse A/M/D marker. New content
/// (added / untracked / copied) reads as `Added`; a removed path as `Deleted`;
/// everything else (modified / renamed / typechange) as `Modified` — renames
/// still carry the separate `rename` badge.
fn delta_status(status: git2::Delta) -> PublishFileStatus {
    match status {
        git2::Delta::Added | git2::Delta::Untracked | git2::Delta::Copied => {
            PublishFileStatus::Added
        }
        git2::Delta::Deleted => PublishFileStatus::Deleted,
        _ => PublishFileStatus::Modified,
    }
}

/// SW-GS-3: extensions that render in the before/after TOGGLE (image / html /
/// excalidraw) rather than as a markdown content redline. Mirrors the FE
/// `VIEWER_EXTENSIONS` toggle types in `viewer-file-type.ts` (markdown is the
/// default for everything else). Rename detection is un-paired for these (see
/// `publish_set_impl`) because git's similarity heuristic mis-pairs their
/// boilerplate-heavy / binary content and they gain nothing from a rename
/// (they're never redlined by old path).
pub(crate) fn is_toggle_type(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    matches!(
        ext.as_deref(),
        Some("html" | "htm" | "excalidraw" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg")
    )
}

/// A single pending file change (TS `PublishFile`). `badges` is omitted when
/// empty so a plain content change serializes to `{ "path": …, "status": … }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishFile {
    /// Repo-relative path (post-rename for a rename).
    pub path: String,
    /// SW-GS-2: Add/Modify/Delete vs the publish baseline.
    pub status: PublishFileStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub badges: Vec<PublishFileBadge>,
    /// SW-GS-14: pre-rename repo-relative path; `Some` only for a `Renamed`
    /// delta. Drives the FE rename chip + the redline's baseline lookup (the
    /// content lives at the old path in the baseline). Also the internal
    /// provenance key (AC-10: rename new→old mapping). Omitted when absent so a
    /// non-rename serializes unchanged.
    #[serde(
        rename = "renamedFrom",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub rename_old: Option<String>,
    /// SLW-DV-6: added/removed line counts vs the baseline, from git's own
    /// line-stats. `None` when not computable — a binary delta or a synthesized
    /// (un-paired) delete/add. Omitted from serialization when absent so the
    /// existing publish / PR-diff payloads are byte-unchanged. Populated by the
    /// shared `records_from_diff` spine, so BOTH the working-tree change-set and
    /// the per-commit (History-mode) change-set carry them; the PR tree-to-tree
    /// path leaves them absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<u32>,
}

/// One change-bearing workspace bucket (TS `PublishBucket`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishBucket {
    pub workspace_ref: WorkspaceRef,
    /// Pre-computed `== files.len()` (carried so the picker renders a count
    /// without walking files).
    pub count: usize,
    pub files: Vec<PublishFile>,
}

/// The General / unattributed bucket (TS `GeneralBucket`). Always present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralBucket {
    pub count: usize,
    pub files: Vec<PublishFile>,
}

/// Grouped, counted publish set (TS `PublishSet`) — the picker + V2 contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishSet {
    pub buckets: Vec<PublishBucket>,
    pub general: GeneralBucket,
    /// Why the diff is against HEAD: no rung of the publish baseline (origin's default
    /// branch, local `main`, the branch's upstream) resolved. Absent when one did.
    #[serde(
        default,
        rename = "baselineReason",
        skip_serializing_if = "Option::is_none"
    )]
    pub baseline_reason: Option<String>,
}

/// SW-GS-3 — before/after content for one changed file, the input to the
/// rendered-markdown redline (TS `FileRedline`). `before` is the publish
/// baseline blob, `after` is the working-tree content — the SAME baseline the
/// changed-files list diffs against, so the list and the redline never
/// disagree. `binary` true ⇒ `before`/`after` are empty (the FE shows the
/// "open both / pick-a-side" affordance instead of a broken redline).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRedline {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_sha: Option<String>,
    pub status: PublishFileStatus,
    pub binary: bool,
    pub before: String,
    pub after: String,
}

/// SLW-WD-8 — the kind of ref decorating a commit in the source commit graph.
/// `Head` is the synthetic decoration for the worktree's own symbolic `HEAD`
/// (which `references()` does NOT enumerate — RD-6), so WD-9 marks "where I am"
/// off the ref labels rather than a bespoke `is_head` bool (AC-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    Local,
    Remote,
    Tag,
    Head,
}

/// SLW-WD-8 — one branch/remote/tag/HEAD decoration pointing at a commit. A plain
/// projection of git refs (AC-5): `name` is the shorthand (`main`, `origin/master`,
/// `v1.0`, or `HEAD`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefLabel {
    pub name: String,
    pub kind: RefKind,
}

/// SLW-WD-8 — one commit in the source worktree's all-refs commit graph: metadata
/// plus `parents` (the parent shas that make edges/lanes drawable — ALL parents
/// for a merge, AC-8) and `refs` (the decorations pointing at this commit, incl.
/// the synthetic `HEAD`, AC-5). Deliberately carries NO `paths`: the graph walk
/// never diffs (AC-3). `committedAt` is Unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceCommitGraphEntry {
    pub sha: String,
    pub short_sha: String,
    pub parents: Vec<String>,
    pub refs: Vec<RefLabel>,
    pub subject: String,
    pub author: String,
    pub committed_at: i64,
}

// ---------------------------------------------------------------------------
// AC-10 / SW-CP-22 — provenance grouping seam.
// ---------------------------------------------------------------------------

/// Resolve the workspace that last wrote `ctx_path` (rename-safe: callers pass
/// the post-rename path AND the pre-rename `old_path` so a renamed file still
/// resolves its writer via the old key).
///
/// **SW-CP-22 SEAM — NOT BUILT YET (cross-epic, BLOCKING for the *grouped*
/// picker, team decision 2026-06-23).** CP-4 must stamp
/// `lastWriter[ctxPath] = {workspaceId, kind}` at per-worktree reverse-sync;
/// that write-site does not exist in source today. Until it lands this returns
/// `None` for every path, so every file degrades to the General bucket — the
/// flat changeset is fully functional, only the grouping is gated. When the
/// stamp exists, this is the single function to swap (read the stamp, map
/// `{workspaceId, kind}` → [`WorkspaceRef`]); the grouping machinery below is
/// already complete.
fn lookup_last_writer(
    _repo_path: &Path,
    _ctx_path: &str,
    _old_path: Option<&str>,
) -> Option<WorkspaceRef> {
    None
}

// ---------------------------------------------------------------------------
// AC-3 — the one shared, read-only backend fn (two transports ride this).
// ---------------------------------------------------------------------------

/// Compute the structured, grouped publish set — **read-only** from the single
/// clone at `repo_path` (which is `org_root`). Pure git2; no shelling, no
/// working-tree mutation. Runs the tracked diff vs the publish baseline
/// (origin's default branch, falling back to local `main`, then `HEAD`) plus untracked
/// enumeration, classifies each file (rename / mode / EOL-only),
/// and groups by provenance into `{ buckets, general }`.
///
/// `workspace_ref` is accepted for transport parity (the picker passes the
/// active workspace) but does not change grouping: grouping is by *provenance*
/// (AC-10), which degrades every file to General until SW-CP-22 lands.
pub fn publish_set_impl(
    repo_path: &Path,
    _workspace_ref: Option<String>,
    path_prefix: Option<&str>,
) -> Result<PublishSet, String> {
    publish_set_impl_for(repo_path, _workspace_ref, path_prefix, &Default::default())
}

pub(crate) fn publish_set_impl_for(
    repo_path: &Path,
    _workspace_ref: Option<String>,
    path_prefix: Option<&str>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<PublishSet, String> {
    let repo = open_repo(repo_path)?;
    let (baseline, reason) = resolve_baseline(&repo)?;
    let records = changeset_records_for(&repo, baseline.as_ref(), path_prefix, scope)?;
    let mut set = group_by_provenance(repo_path, records);
    set.baseline_reason = Option::map(reason, str::to_owned);
    Ok(set)
}

/// SLW-WD-1 — the shared diff+classify core of the changed-file list: diff the
/// given `baseline` tree against the working tree (with index, incl. untracked)
/// and classify each delta via [`records_from_diff`]. Extracted from
/// [`publish_set_impl`] so the context-repo publish set (baseline = publish
/// default-branch **tip**) and the source worktree change-set (baseline =
/// **merge-base** fork point, [`source_worktree_changeset_impl`]) share ONE diff
/// pipeline and can't drift. The caller owns baseline selection, meta-coupling,
/// and provenance grouping — this returns the flat classified records only.
fn changeset_records(
    repo: &git2::Repository,
    baseline: Option<&git2::Tree>,
    path_prefix: Option<&str>,
) -> Result<Vec<PublishFile>, String> {
    changeset_records_for(repo, baseline, path_prefix, &Default::default())
}

fn changeset_records_for(
    repo: &git2::Repository,
    baseline: Option<&git2::Tree>,
    path_prefix: Option<&str>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Vec<PublishFile>, String> {
    let mut opts = git2::DiffOptions::new();
    // NOTE: `show_binary` is intentionally NOT set. The changed-file LIST never
    // renders binary content inline — the redline viewer fetches per-file binary
    // bytes lazily via `get_file_redline`. Loading every binary blob into the
    // diff here was the `MALLOC_LARGE` contributor under an fsevents write burst.
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_typechange(true)
        // SLW-DV-6: without this, libgit2 lists an untracked file as an Added
        // delta but carries no hunks, so its per-file line count reads 0. Turn on
        // untracked content so a new file's added lines are actually counted.
        .show_untracked_content(true);
    // Team scoping: restrict the diff to the active team's subtree server-side
    // via a git pathspec, so a burst of org-wide edits doesn't diff the whole
    // working tree. An empty/absent prefix ⇒ unscoped (org-wide) — today's behavior.
    if let Some(prefix) = path_prefix.filter(|p| !p.is_empty()) {
        opts.pathspec(prefix);
    }
    let mut diff = repo
        .diff_tree_to_workdir_with_index(baseline, Some(&mut opts))
        .map_err(|e| format!("diff: {e}"))?;
    if !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
        let root = repo.workdir().ok_or("repository has no workdir")?;
        // Names only first; denied files never participate in rename pairing, patch
        // stats, content badges or provenance/frontmatter grouping.
        let admitted: std::collections::BTreeSet<_> = diff
            .deltas()
            .flat_map(|delta| {
                [
                    delta.old_file().path().map(Path::to_path_buf),
                    delta.new_file().path().map(Path::to_path_buf),
                ]
            })
            .flatten()
            .filter(|path| scope.allows(&root.join(path)))
            .collect();
        if admitted.is_empty() {
            return Ok(Vec::new());
        }
        let mut scoped = git2::DiffOptions::new();
        scoped
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .include_typechange(true)
            .show_untracked_content(true)
            .disable_pathspec_match(true);
        for path in admitted {
            scoped.pathspec(path);
        }
        diff = repo
            .diff_tree_to_workdir_with_index(baseline, Some(&mut scoped))
            .map_err(|e| format!("diff: {e}"))?;
    }
    records_from_diff(repo, &mut diff)
}

/// SLW-DV-6 — added/removed line counts for one delta (by index into `diff`),
/// straight from git's own line-stats. `None` for a binary delta (no patch) or
/// any error, so callers distinguish "0 line changes" from "not applicable".
fn delta_line_stats(diff: &git2::Diff, idx: usize) -> (Option<u32>, Option<u32>) {
    match git2::Patch::from_diff(diff, idx) {
        Ok(Some(patch)) => {
            // A binary delta yields a hunkless patch whose `line_stats` are
            // (0,0,0) — indistinguishable from "no line change". The patch's own
            // delta carries the binary flag (set once the patch is generated), so
            // use it to report `None` instead of a misleading `Some(0)`.
            let delta = patch.delta();
            if delta.new_file().is_binary() || delta.old_file().is_binary() {
                return (None, None);
            }
            match patch.line_stats() {
                // `line_stats` → (context, additions, deletions). Saturate rather
                // than silently wrap the (unreachably large) count on `as u32`.
                Ok((_ctx, adds, dels)) => (
                    Some(u32::try_from(adds).unwrap_or(u32::MAX)),
                    Some(u32::try_from(dels).unwrap_or(u32::MAX)),
                ),
                Err(_) => (None, None),
            }
        }
        // `Ok(None)` = no patch available; `Err` = give up quietly.
        _ => (None, None),
    }
}

/// The cheap "is-dirty count" path for the closed-pane Sync badge: count the
/// changed paths vs the publish baseline, name-only. Mirrors
/// [`commit_changed_paths`]'s diff config (typechange + untracked) but takes NO
/// rename detection (`find_similar`), NO binary content (`show_binary`), and NO
/// per-delta blob inflation (`is_eol_only`) — so it never zlib-inflates a blob.
/// This is the always-on path that ran the full org-wide changeset before; it
/// stays bounded to a delta count.
///
/// `path_prefix` restricts the diff to a subtree via a git pathspec (the active
/// team's content prefix); `None`/empty ⇒ unscoped (org-wide).
///
/// Caveat (intentional cost/accuracy trade): without `find_similar` a rename
/// counts as 2 (delete + add). The badge is a "you have changes" hint; the
/// exact rename-paired count appears when the pane opens (full [`publish_set_impl`]).
pub fn changed_path_count_impl(
    repo_path: &Path,
    path_prefix: Option<&str>,
) -> Result<usize, String> {
    let repo = open_repo(repo_path)?;
    let baseline = resolve_baseline_tree(&repo)?;
    let mut opts = git2::DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_typechange(true);
    if let Some(prefix) = path_prefix.filter(|p| !p.is_empty()) {
        opts.pathspec(prefix);
    }
    let diff = repo
        .diff_tree_to_workdir_with_index(baseline.as_ref(), Some(&mut opts))
        .map_err(|e| format!("diff: {e}"))?;
    Ok(diff
        .deltas()
        .filter(|d| {
            d.new_file()
                .path()
                .or_else(|| d.old_file().path())
                .is_some()
        })
        .count())
}

/// SW-GS-19 (A1) — the shared delta-classification spine. Runs rename detection
/// (`-M`, un-paired for toggle types), classifies each delta into a
/// [`PublishFile`] (A/M/D + rename/mode/EOL badges + pre-rename path), and
/// returns the flat record list. Extracted from [`publish_set_impl`] so BOTH
/// the working-tree diff and the PR tree-to-tree diff ([`pr_publish_set_impl`])
/// classify identically — one implementation, two diff sources. The caller owns
/// baseline/tree selection, meta-coupling, and provenance grouping.
fn records_from_diff(
    repo: &git2::Repository,
    diff: &mut git2::Diff,
) -> Result<Vec<PublishFile>, String> {
    // Rename detection (AC-3 `-M`). Copies off — only renames feed the lookup.
    // `for_untracked`: the new (renamed-to) path is typically still untracked in
    // the working tree at curation time, so it must be eligible as a rename
    // target or every uncommitted rename reads as delete+add. (For a tree-to-tree
    // PR diff there are no untracked entries, so the flag is simply inert.)
    let mut find = git2::DiffFindOptions::new();
    find.renames(true).copies(false).for_untracked(true);
    diff.find_similar(Some(&mut find))
        .map_err(|e| format!("find_similar: {e}"))?;

    let mut records: Vec<PublishFile> = Vec::new();
    for (idx, delta) in diff.deltas().enumerate() {
        let new_path = delta.new_file().path().map(path_string);
        let old_path = delta.old_file().path().map(path_string);
        let path = match new_path.clone().or_else(|| old_path.clone()) {
            Some(p) => p,
            None => continue,
        };
        let status = delta.status();

        // SW-GS-3: git rename detection mis-pairs boilerplate-heavy / binary
        // non-markdown files — e.g. a freshly-added `.excalidraw` and an
        // unrelated deleted one clear the 50% similarity threshold on their
        // shared JSON scaffolding, so a genuine ADD reads as a "Modified"
        // rename. These render via the before/after toggle (never a content
        // redline by old path), so a rename buys nothing. Un-pair them back into
        // a clean Delete(old) + Add(new). Real renames are kept for redline-able
        // (markdown/text) paths, where a moved doc should diff by its old path
        // rather than read as delete+add.
        if status == git2::Delta::Renamed {
            if let (Some(np), Some(op)) = (new_path.as_ref(), old_path.as_ref()) {
                if is_toggle_type(np) {
                    records.push(PublishFile {
                        path: np.clone(),
                        status: PublishFileStatus::Added,
                        badges: Vec::new(),
                        rename_old: None,
                        // Synthesized halves of an un-paired toggle-type rename:
                        // the git delta's stats describe the rename as a whole, not
                        // these two clean add/delete records, so don't attribute
                        // them.
                        added: None,
                        removed: None,
                    });
                    records.push(PublishFile {
                        path: op.clone(),
                        status: PublishFileStatus::Deleted,
                        badges: Vec::new(),
                        rename_old: None,
                        added: None,
                        removed: None,
                    });
                    continue;
                }
            }
        }

        let mut badges = Vec::new();
        if status == git2::Delta::Renamed {
            badges.push(PublishFileBadge::Rename);
        }
        if is_mode_change(&delta) {
            badges.push(PublishFileBadge::Mode);
        }
        if is_eol_only(repo, &delta) {
            badges.push(PublishFileBadge::EolOnly);
        }

        let rename_old = if status == git2::Delta::Renamed {
            old_path
        } else {
            None
        };
        // SLW-DV-6: per-file +/− line counts from git's own line-stats (binary ⇒
        // `None`), carried on the record so the diff-overlay rail renders +N/−M
        // without re-diffing on the client. Computed LAST: `Patch::from_diff`
        // populates the delta's workdir blob oid as a side effect, which would
        // make `is_eol_only` above mis-read an unstaged change, so it must run
        // after the badge probes.
        let (added, removed) = delta_line_stats(diff, idx);
        records.push(PublishFile {
            path,
            status: delta_status(status),
            badges,
            rename_old,
            added,
            removed,
        });
    }

    Ok(records)
}

/// Why a git-sync read of the context repo has no origin default branch to compare with.
/// (`refresh_history` is for attached source repos; the context repo's git is the agent's.)
pub(crate) const NO_ORIGIN_DEFAULT: &str =
    "origin default branch not known locally; fetch origin or run `git remote set-head origin --auto`";

/// Resolve the diff baseline tree: the publish target. See [`resolve_baseline`].
fn resolve_baseline_tree(repo: &git2::Repository) -> Result<Option<git2::Tree<'_>>, String> {
    Result::map(resolve_baseline(repo), |(tree, _)| tree)
}

/// The publish baseline tree and, when it fell back to `HEAD`, why. Prefer origin's
/// default branch (what we publish against, [`default_branch_ref`]), then local `main`,
/// then the checked-out branch's upstream; with none of them it is `HEAD` with
/// [`NO_ORIGIN_DEFAULT`] (a private repo has no remote and is always `HEAD`, without a
/// reason). An unborn HEAD yields `None` (diff against the empty tree → everything is
/// "added"). Local refs only, no fetch.
fn resolve_baseline(
    repo: &git2::Repository,
) -> Result<(Option<git2::Tree<'_>>, Option<&'static str>), String> {
    let private = crate::context_repo::scratch_space::is_private_repo(repo);
    if !private {
        let local_main = || {
            let main = Result::ok(git2::Repository::find_reference(repo, "refs/heads/main"))?;
            let commit = Result::ok(git2::Reference::peel_to_commit(&main))?;
            Some((
                std::string::String::from("refs/heads/main"),
                git2::Commit::id(&commit),
            ))
        };
        let upstream = || {
            Option::map(upstream_oid(repo), |oid| {
                (std::string::String::from("upstream"), oid)
            })
        };
        let found = Option::or_else(
            Option::or_else(default_branch_ref(repo), local_main),
            upstream,
        );
        if let Some((refname, oid)) = found {
            let commit = Result::map_err(git2::Repository::find_commit(repo, oid), |e| {
                format!("peel {refname}: {e}")
            })?;
            let tree = tree_or_err(&commit, format_args!("tree {refname}"))?;
            return Ok((Some(tree), None));
        }
    }
    let reason = bool::then_some(!private, NO_ORIGIN_DEFAULT);
    match repo.head() {
        Ok(head) => {
            let commit = head
                .peel_to_commit()
                .map_err(|e| format!("peel HEAD: {e}"))?;
            Ok((Some(tree_or_err(&commit, "tree HEAD")?), reason))
        }
        Err(_) => Ok((None, reason)),
    }
}

/// SLW-WD-1 (AC-3, RD-7) — resolve a **source worktree's** baseline tree: the
/// **merge-base** (fork point) of HEAD and its base ref, so the change-set is
/// *this feature's* diff, not everything on the base branch since it diverged.
///
/// Base-ref ladder (first that resolves wins): the caller-supplied `base_ref`
/// (RD-7 — WD-2 passes the fork parent for a *stacked* worktree) → origin's default
/// branch ([`default_branch`]) → `main` → the checked-out branch's upstream. The
/// merge-base is computed against **local** refs only (no fetch — WD-1 is read-only /
/// non-blocking, mirroring `resolve_pr_commits`); a stale local remote-tracking ref
/// yields a stale-but-safe baseline.
///
/// Graceful fallback (AC-8), by case — never errors:
/// - **Unborn HEAD** (no HEAD target): delegates to the shared
///   [`resolve_baseline_tree`] ladder (origin default → `main` → upstream → HEAD →
///   `None`),
///   so a fresh repo reads everything as Added.
/// - **Detached HEAD**: still has a target OID, so it merge-bases from that OID
///   like any other HEAD (better than a plain tip fallback).
/// - **Born HEAD but no base ref resolves / no merge-base with any candidate**:
///   falls back to the worktree's own HEAD **tip** tree → the change-set is just
///   its uncommitted work.
fn resolve_source_baseline_tree<'r>(
    repo: &'r git2::Repository,
    base_ref: Option<&str>,
) -> Result<Option<git2::Tree<'r>>, String> {
    // Only an UNBORN HEAD has no target OID; delegate to the shared tip ladder.
    // A detached HEAD keeps its OID and merge-bases normally below.
    let head_oid = match repo.head().ok().and_then(|h| h.target()) {
        Some(oid) => oid,
        None => return resolve_baseline_tree(repo),
    };

    // Base candidates, highest priority first: the caller-supplied `base_ref` (when
    // non-empty; `revparse_single` resolves a branch name, a remote-tracking name, a
    // full ref, or a raw OID uniformly), origin's default branch ([`default_branch`]),
    // local `main`, then — SLW-WD-8 AC-1 — the checked-out branch's upstream, the rung
    // that fires on an attached repo with no origin default locally. Without these the
    // change-set collapses to HEAD's own tree, so a COMMITTED edit vanishes ("a commit
    // just happened and it looks like no code is on the branch"). Local-only, no fetch.
    let revparse = |spec: &str| {
        let object = Result::ok(git2::Repository::revparse_single(repo, spec))?;
        Some(git2::Commit::id(&Result::ok(
            git2::Object::peel_to_commit(&object),
        )?))
    };
    let named = Option::filter(base_ref, |r| !str::is_empty(r));
    let candidates = [
        Option::and_then(named, revparse),
        default_branch(repo),
        revparse("main"),
        upstream_oid(repo),
    ];
    for base in Iterator::flatten(IntoIterator::into_iter(candidates)) {
        if let Ok(mb) = git2::Repository::merge_base(repo, head_oid, base) {
            let tree = repo
                .find_commit(mb)
                .and_then(|c| c.tree())
                .map_err(|e| format!("merge-base tree: {e}"))?;
            return Ok(Some(tree));
        }
    }

    // No base ref resolved (or no merge-base with any) ⇒ the worktree's own HEAD
    // tip is the baseline, so the change-set is just its uncommitted work.
    let tree = repo
        .find_commit(head_oid)
        .and_then(|c| c.tree())
        .map_err(|e| format!("head tree: {e}"))?;
    Ok(Some(tree))
}

/// The checked-out branch's upstream tracking-ref OID (e.g. `origin/master`).
/// `None` if HEAD is detached/unborn or the branch has no upstream configured.
fn upstream_oid(repo: &git2::Repository) -> Option<git2::Oid> {
    let head = repo.head().ok()?;
    if !head.is_branch() {
        return None;
    }
    git2::Branch::wrap(head).upstream().ok()?.get().target()
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// G9 — mode/symlink change: modes differ on a file present on BOTH sides
/// (not a pure add/delete/untracked). Catches exec-bit flips and regular↔symlink
/// type changes.
fn is_mode_change(delta: &git2::DiffDelta<'_>) -> bool {
    if matches!(
        delta.status(),
        git2::Delta::Added | git2::Delta::Deleted | git2::Delta::Untracked
    ) {
        return false;
    }
    if delta.old_file().path().is_none() || delta.new_file().path().is_none() {
        return false;
    }
    delta.old_file().mode() != delta.new_file().mode()
}

/// Max blob size (bytes) we will inflate to test EOL-only. Above this we skip
/// the inflation entirely (no EolOnly badge) — the badge is a cosmetic hint and
/// not worth zlib-inflating a large blob per Modified delta on the always-warm
/// diff path. 512 KiB comfortably covers real docs/config.
const EOL_ONLY_SIZE_CAP: u64 = 512 * 1024;

/// G8 — EOL-only: a modified text file whose content is byte-identical after
/// stripping `\r`. Reads the old blob from the baseline tree and the new
/// content from the workdir (or staged blob). Best-effort: any read/binary
/// snag returns `false` (treated as a real content change). Blobs over
/// [`EOL_ONLY_SIZE_CAP`] skip inflation and read as a real change (no badge).
fn is_eol_only(repo: &git2::Repository, delta: &git2::DiffDelta<'_>) -> bool {
    if !matches!(delta.status(), git2::Delta::Modified) {
        return false;
    }
    if delta.flags().is_binary() {
        return false;
    }
    // Size cap: skip inflation for large (or unknown-size) files BEFORE reading
    // either side. git2 does not always populate the workdir-side size, and a 0
    // there would BYPASS a naive `> CAP` check and inflate a large blob anyway —
    // defeating the guard. So require BOTH sides to be KNOWN (>0) and within the
    // cap; a 0/unknown side is treated as over-cap (no badge, no inflation). A
    // genuinely 0-length side isn't EOL-only either (empty vs non-empty), so
    // returning false there is also correct.
    let old_size = delta.old_file().size();
    let new_size = delta.new_file().size();
    let within_cap = |size: u64| size > 0 && size <= EOL_ONLY_SIZE_CAP;
    if !within_cap(old_size) || !within_cap(new_size) {
        return false;
    }
    let old_id = delta.old_file().id();
    if old_id.is_zero() {
        return false;
    }
    let old_blob = match repo.find_blob(old_id) {
        Ok(b) => b,
        Err(_) => return false,
    };
    if old_blob.is_binary() {
        return false;
    }

    let new_id = delta.new_file().id();
    let new_content: Vec<u8> = if !new_id.is_zero() {
        match repo.find_blob(new_id) {
            Ok(b) => b.content().to_vec(),
            Err(_) => return false,
        }
    } else {
        // Unstaged workdir change — read from disk.
        let Some(workdir) = repo.workdir() else {
            return false;
        };
        let Some(rel) = delta.new_file().path() else {
            return false;
        };
        match std::fs::read(workdir.join(rel)) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        }
    };

    strip_cr(old_blob.content()) == strip_cr(&new_content)
}

fn strip_cr(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().copied().filter(|&b| b != b'\r').collect()
}

/// Group the classified deltas by provenance (AC-10). Files with a resolvable
/// `lastWriter` go to a per-`wsId` bucket; the rest fall to General.
fn group_by_provenance(repo_path: &Path, records: Vec<PublishFile>) -> PublishSet {
    let mut general = GeneralBucket {
        count: 0,
        files: Vec::new(),
    };
    // Preserve first-seen bucket order for stable rendering.
    let mut bucket_order: Vec<String> = Vec::new();
    let mut buckets: std::collections::HashMap<String, PublishBucket> =
        std::collections::HashMap::new();

    for record in records {
        match lookup_last_writer(repo_path, &record.path, record.rename_old.as_deref()) {
            Some(ws) => {
                let entry = buckets.entry(ws.id.clone()).or_insert_with(|| {
                    bucket_order.push(ws.id.clone());
                    PublishBucket {
                        workspace_ref: ws,
                        count: 0,
                        files: Vec::new(),
                    }
                });
                entry.files.push(record);
            }
            None => general.files.push(record),
        }
    }

    general.count = general.files.len();
    let ordered = bucket_order
        .into_iter()
        .filter_map(|id| buckets.remove(&id))
        .map(|mut b| {
            b.count = b.files.len();
            b
        })
        .collect();

    PublishSet {
        buckets: ordered,
        general,
        baseline_reason: None,
    }
}

// ---------------------------------------------------------------------------
// Tauri command transport (F4 — the session-less picker). The MCP-verb
// transport lives on `CodesenseHandler` in `mcp/handler.rs` and rides the same
// `publish_set_impl`. NON-BLOCKING: the git2 walk runs on a blocking thread so
// a large changeset never stalls the UI thread (project-context §Performance).
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_publish_set(
    app: tauri::AppHandle,
    worktree: Option<String>,
    repo_path: String,
    workspace_ref: Option<String>,
    // SW-GS-19 (A1): when `Some`, return the changed files of that PR
    // (head-tree vs merge-base) instead of the working-tree set — same shape,
    // same grouping, only the diff source differs. Reuses the OAuth token.
    pr_number: Option<u64>,
    // SW-GS-13: when `Some` (and no `pr_number`), return the changed files of
    // that LANDED commit (its tree vs its first-parent tree) — same shape, same
    // grouping. Local + read-only: no token, no fetch (unlike the PR source).
    commit_sha: Option<String>,
    // When `Some`, restrict the working-tree diff to that subtree via a git
    // pathspec (the active team's content prefix) so a burst of org-wide edits
    // only diffs one team's files. `None`/empty ⇒ unscoped (backward-compatible).
    // Applies only to the working-tree source; PR/commit sources are unaffected.
    path_prefix: Option<String>,
) -> Result<PublishSet, String> {
    if let Some(id) = worktree {
        let row = context_worktree::resolve(&app, &id).await?;
        return tauri::async_runtime::spawn_blocking(move || {
            context_worktree::publish_set(&row.root, path_prefix.as_deref())
        })
        .await
        .map_err(|e| e.to_string())?;
    }
    let path = std::path::PathBuf::from(repo_path);
    // Precedence (mirrors the FE source descriptor): PR wins over commit wins
    // over working-tree, so the three sources are mutually exclusive here too.
    match (pr_number, commit_sha) {
        (Some(n), _) => {
            let token = String::new();
            tauri::async_runtime::spawn_blocking(move || pr_publish_set_impl(&path, n, &token))
                .await
                .map_err(|e| format!("pr publish_set task failed: {e}"))?
        }
        (None, Some(sha)) => {
            tauri::async_runtime::spawn_blocking(move || commit_publish_set_impl(&path, &sha))
                .await
                .map_err(|e| format!("commit publish_set task failed: {e}"))?
        }
        (None, None) => tauri::async_runtime::spawn_blocking(move || {
            publish_set_impl(&path, workspace_ref, path_prefix.as_deref())
        })
        .await
        .map_err(|e| format!("publish_set task failed: {e}"))?,
    }
}

/// The cheap name-only "how many files changed" count for the CLOSED-pane Sync
/// badge. Off the UI thread (`spawn_blocking`), like [`get_publish_set`], but
/// runs [`changed_path_count_impl`] — no rename detection, no binary blobs, no
/// EOL inflation. `path_prefix` scopes the diff to the active team's subtree.
#[tauri::command]
pub async fn changed_path_count(
    repo_path: String,
    path_prefix: Option<String>,
) -> Result<usize, String> {
    let path = std::path::PathBuf::from(repo_path);
    tauri::async_runtime::spawn_blocking(move || {
        changed_path_count_impl(&path, path_prefix.as_deref())
    })
    .await
    .map_err(|e| format!("changed_path_count task failed: {e}"))?
}

/// Git's binary heuristic: a NUL byte in the first 8 KB ⇒ treat as binary.
/// Used for the working-tree side, which is raw bytes (not a git blob).
pub(crate) fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0)
}

/// Reject absolute paths and any `..` component so a baseline/workdir lookup
/// keyed on the result can't escape `repo_path` and surface an arbitrary file.
/// Shared by `file_redline_impl` for both the current and pre-rename paths.
fn safe_rel_path(file_path: &str) -> Result<&Path, String> {
    let rel = Path::new(file_path);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("invalid file path: {file_path}"));
    }
    Ok(rel)
}

/// SW-GS-3 — compute the before/after content for one changed file at
/// `org_root`, diffed against the publish baseline (origin default → `main` →
/// `HEAD`, via [`resolve_baseline_tree`] — the SAME baseline as
/// [`publish_set_impl`], so the changed-files list and the redline always
/// agree). `before` is read from the baseline tree, `after` from the working
/// tree on disk. Direct tree-lookup + workdir read (no diff walk needed for a
/// single known path); status is derived from which sides are present. Binary
/// on either side ⇒ empty content (the FE renders the pick-a-side affordance).
pub fn file_redline_impl(
    repo_path: &Path,
    file_path: &str,
    old_path: Option<&str>,
) -> Result<FileRedline, String> {
    let repo = open_repo(repo_path)?;
    let baseline = resolve_baseline_tree(&repo)?;
    // A retained Changes tab may outlive the nav's selected source. Resolve a
    // live rename from this repo's own diff when its new path has no baseline blob.
    let inferred_old = if old_path.is_none()
        && baseline
            .as_ref()
            .is_some_and(|tree| tree.get_path(Path::new(file_path)).is_err())
    {
        safe_rel_path(file_path)?;
        changeset_records(&repo, baseline.as_ref(), None)?
            .into_iter()
            .find(|file| file.path == file_path)
            .and_then(|file| file.rename_old)
    } else {
        None
    };
    let old_path = old_path.or(inferred_old.as_deref());
    let mut redline =
        file_redline_against_baseline(&repo, repo_path, baseline.as_ref(), file_path, old_path)?;
    if crate::context_repo::scratch_space::is_private_repo(&repo) {
        redline.baseline_sha = repo
            .head()
            .ok()
            .and_then(|h| h.target())
            .map(|id| id.to_string()[..7].to_string());
    }
    Ok(redline)
}

/// SW-GS-3 / SLW-WD-1 — the baseline-parameterized redline core: compute a
/// [`FileRedline`] for one file, reading "before" from an EXPLICIT `baseline`
/// tree and "after" from the working tree on disk. Extracted from
/// [`file_redline_impl`] so the context-repo redline (baseline = publish tip via
/// [`resolve_baseline_tree`]) and the source worktree redline (baseline =
/// merge-base via [`resolve_source_baseline_tree`], [`source_file_redline_impl`])
/// share ONE before/after builder and can't drift on the baseline. Threading the
/// baseline as a parameter — rather than swapping [`resolve_baseline_tree`]'s
/// default — keeps git-sync's publish redline on its origin default-branch tip baseline
/// (RD-2).
fn file_redline_against_baseline(
    repo: &git2::Repository,
    repo_path: &Path,
    baseline: Option<&git2::Tree>,
    file_path: &str,
    old_path: Option<&str>,
) -> Result<FileRedline, String> {
    // Both paths are repo-relative (exactly what get_publish_set returns) and
    // must be rejected if absolute or `..`-bearing so the lookups below can't
    // escape `repo_path` (SW-GS-14: guard `old_path` too, not just `file_path`).
    let rel = safe_rel_path(file_path)?;

    // SW-GS-14: a rename's baseline content lives at the PRE-rename path. Read
    // "before" from the old path (when this is a rename) and "after" from the
    // current path on disk, so a moved doc redlines its real delta instead of
    // reading as all-added. Non-rename ⇒ both sides use the current path.
    let baseline_rel = match old_path {
        Some(op) => safe_rel_path(op)?,
        None => rel,
    };

    // "before": the path as it stands in the publish baseline (absent ⇒ added).
    let (before_bytes, before_binary): (Option<Vec<u8>>, bool) = match baseline {
        Some(tree) => match tree.get_path(baseline_rel) {
            Ok(entry) => {
                let obj = entry
                    .to_object(repo)
                    .map_err(|e| format!("baseline object {}: {e}", baseline_rel.display()))?;
                match obj.as_blob() {
                    Some(blob) => (Some(blob.content().to_vec()), blob.is_binary()),
                    None => (None, false), // a tree (dir) at that path — treat as absent
                }
            }
            Err(_) => (None, false), // not in the baseline ⇒ added
        },
        None => (None, false), // unborn baseline ⇒ everything reads as added
    };

    // "after": the working-tree file on disk. A NotFound is a real deletion;
    // any other read error (permission, I/O) is surfaced rather than silently
    // misclassified as a deletion.
    let after_bytes: Option<Vec<u8>> = match std::fs::read(repo_path.join(rel)) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("read {file_path}: {e}")),
    };
    let after_binary = after_bytes.as_deref().map(looks_binary).unwrap_or(false);

    Ok(build_redline(
        before_bytes.map(|b| (b, before_binary)),
        after_bytes.map(|b| (b, after_binary)),
    ))
}

/// SW-GS-19 (A1) — assemble a [`FileRedline`] from resolved before/after sides,
/// each `Some((bytes, is_binary))` when the side is present. Derives A/M/D from
/// which sides exist; if EITHER side is binary the content is emptied and
/// `binary` is set (the FE shows the pick-a-side affordance). Shared by the
/// working-tree redline ([`file_redline_impl`]) and the PR tree-pair redline
/// ([`pr_file_redline_impl`]) so both classify + stringify identically.
pub(crate) fn build_redline(
    before: Option<(Vec<u8>, bool)>,
    after: Option<(Vec<u8>, bool)>,
) -> FileRedline {
    let status = match (before.is_some(), after.is_some()) {
        (false, true) => PublishFileStatus::Added,
        (true, false) => PublishFileStatus::Deleted,
        _ => PublishFileStatus::Modified, // both present (both-absent is defensive)
    };
    let before_binary = before.as_ref().map(|(_, b)| *b).unwrap_or(false);
    let after_binary = after.as_ref().map(|(_, b)| *b).unwrap_or(false);
    if before_binary || after_binary {
        return FileRedline {
            before_label: None,
            after_label: None,
            baseline_sha: None,
            status,
            binary: true,
            before: String::new(),
            after: String::new(),
        };
    }
    FileRedline {
        before_label: None,
        after_label: None,
        baseline_sha: None,
        status,
        binary: false,
        before: before
            .map(|(b, _)| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default(),
        after: after
            .map(|(b, _)| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default(),
    }
}

/// SW-GS-3 — the redline content verb. Read-only, off the UI thread
/// (`spawn_blocking`), mirroring [`get_publish_set`]. `file_path` is
/// `org_root`-relative (exactly what `get_publish_set` returns). SW-GS-14:
/// `old_path` (also `org_root`-relative) is `Some` only when the file is a
/// rename, so "before" is read from the pre-rename baseline path.
#[tauri::command]
pub async fn get_file_redline(
    app: tauri::AppHandle,
    worktree: Option<String>,
    repo_path: String,
    file_path: String,
    old_path: Option<String>,
    // SW-GS-19 (A1): when `Some`, redline the file across the PR (merge-base
    // blob vs PR-head blob) instead of baseline-vs-working-tree.
    pr_number: Option<u64>,
    // SW-GS-13: when `Some` (and no `pr_number`), redline the file across a
    // landed commit (first-parent blob vs commit blob) — local git objects only,
    // no working-tree read, no network.
    commit_sha: Option<String>,
) -> Result<FileRedline, String> {
    if let Some(id) = worktree {
        let row = context_worktree::resolve(&app, &id).await?;
        return tauri::async_runtime::spawn_blocking(move || {
            context_worktree::redline(&row, &file_path, old_path.as_deref())
        })
        .await
        .map_err(|e| e.to_string())?;
    }
    let path = std::path::PathBuf::from(repo_path);
    // Treat an empty old path as "no rename" (defense in depth — the FE only
    // sends a real pre-rename path or nothing, but don't trust the boundary).
    let old_path = old_path.filter(|p| !p.is_empty());
    if let Some(n) = pr_number {
        let token = String::new();
        // SW-GS-19: a rename's "before" lives at the pre-rename path in the
        // merge-base tree (the list's `find_similar` flags PR renames), so pass
        // `old_path` through exactly as the working-tree redline does — otherwise
        // a renamed doc reads as all-added.
        return tauri::async_runtime::spawn_blocking(move || {
            pr_file_redline_impl(&path, n, &file_path, old_path.as_deref(), &token)
        })
        .await
        .map_err(|e| format!("pr file_redline task failed: {e}"))?;
    }
    if let Some(sha) = commit_sha {
        // Same rename handling as the PR path: "before" reads from the pre-rename
        // path in the first-parent tree.
        return tauri::async_runtime::spawn_blocking(move || {
            commit_file_redline_impl(&path, &sha, &file_path, old_path.as_deref())
        })
        .await
        .map_err(|e| format!("commit file_redline task failed: {e}"))?;
    }
    tauri::async_runtime::spawn_blocking(move || {
        file_redline_impl(&path, &file_path, old_path.as_deref())
    })
    .await
    .map_err(|e| format!("file_redline task failed: {e}"))?
}

// ---------------------------------------------------------------------------
// SLW-WD-1 — Source worktree change-set + per-file redline. The SAME diff engine
// as the context-repo publish set, re-pointed at a *source* worktree's git and
// baselined at the **merge-base** (fork point) so the result is *this feature's*
// diff. Data-only, read-only, non-blocking (`spawn_blocking`); no writes, no
// reverse/route side effects (RD-1..RD-7). Feeds the WD-2 review overlay.
// ---------------------------------------------------------------------------

/// SLW-WD-1 (AC-1/2/5) — the flat change-set of a source worktree: the files
/// that differ between the merge-base baseline ([`resolve_source_baseline_tree`])
/// and the working tree (with index + untracked). Reuses [`changeset_records`]
/// (so classification is byte-identical to the publish set) and returns the flat
/// `Vec<PublishFile>` DIRECTLY — no `group_by_provenance` bucketing (RD-3/RD-5/RD-6).
pub fn source_worktree_changeset_impl(
    repo_path: &Path,
    base_ref: Option<&str>,
) -> Result<Vec<PublishFile>, String> {
    let repo = open_repo(repo_path)?;
    let baseline = resolve_source_baseline_tree(&repo, base_ref)?;
    changeset_records(&repo, baseline.as_ref(), None)
}

/// SLW-WD-1 (AC-4) — per-file before/after redline for a source worktree file,
/// computed against the SAME merge-base baseline as the change-set. Reuses the
/// shared [`file_redline_against_baseline`]/[`build_redline`], so the returned
/// shape is byte-identical to [`file_redline_impl`] and WD-2 renders it with the
/// existing components (binary ⇒ `binary: true` + empty content).
pub fn source_file_redline_impl(
    repo_path: &Path,
    file_path: &str,
    old_path: Option<&str>,
    base_ref: Option<&str>,
) -> Result<FileRedline, String> {
    let repo = open_repo(repo_path)?;
    let baseline = resolve_source_baseline_tree(&repo, base_ref)?;
    file_redline_against_baseline(&repo, repo_path, baseline.as_ref(), file_path, old_path)
}

/// SLW-WD-1 (AC-1/2/5/6) — the source worktree change-set command. Takes a
/// worktree **path** (WD-2 supplies `ws.cwd`, RD-5) and an optional `base_ref`
/// (RD-7 — the fork-parent branch for a stacked worktree; `None` ⇒ merge-base
/// with origin's default branch). Read-only, off the UI thread; no network, no writes.
#[tauri::command]
pub async fn source_worktree_changeset(
    repo_path: String,
    base_ref: Option<String>,
) -> Result<Vec<PublishFile>, String> {
    let path = std::path::PathBuf::from(repo_path);
    tauri::async_runtime::spawn_blocking(move || {
        source_worktree_changeset_impl(&path, base_ref.as_deref())
    })
    .await
    .map_err(|e| format!("source changeset task failed: {e}"))?
}

/// SLW-WD-1 (AC-4/AC-6) — the source worktree per-file redline command. Mirrors
/// [`get_file_redline`] (path-in, `FileRedline`-out) but baselined at the
/// worktree's merge-base. `old_path` is `Some` only for a rename (its "before"
/// reads from the pre-rename path); empty is normalized to `None`.
#[tauri::command]
pub async fn source_file_redline(
    repo_path: String,
    file_path: String,
    old_path: Option<String>,
    base_ref: Option<String>,
) -> Result<FileRedline, String> {
    let path = std::path::PathBuf::from(repo_path);
    let old_path = old_path.filter(|p| !p.is_empty());
    tauri::async_runtime::spawn_blocking(move || {
        source_file_redline_impl(&path, &file_path, old_path.as_deref(), base_ref.as_deref())
    })
    .await
    .map_err(|e| format!("source file_redline task failed: {e}"))?
}

// ---------------------------------------------------------------------------
// SW-GS-19 (A1) — PR-sourced diff: the changed-files list + redline for an open
// PR, resolved from git TREES (PR head vs merge-base) so the doc-aware redline
// works on real blobs without ever checking out or touching the working tree.
// Reuses the shared `records_from_diff` / `build_redline` spines — only the diff
// source differs from the working-tree path. All git2 work runs under
// `spawn_blocking` in the callers above (non-blocking).
// ---------------------------------------------------------------------------

/// Check the PR head and its base are available locally: `refs/remotes/pr/{n}` (an
/// agent or the Git CLI fetches `refs/pull/{n}/head` there; GitHub exposes every PR head
/// on the BASE repo, so same-repo `gs/*` and fork PRs (R7) both work) and origin's
/// default branch. No network: CodeSense does not fetch here; the error says what to
/// prepare.
fn fetch_pr_refs(repo: &git2::Repository, _token: &str, pr_number: u64) -> Result<(), String> {
    resolve_pr_commits(repo, pr_number).map(|_| ()).map_err(|e|
        format!("PR refs are unavailable locally. Prepare refs/remotes/pr/{pr_number} and origin's default branch with your agent or Git CLI, then retry: {e}"))
}

/// Resolve the PR diff's `(before, after)` commit OIDs from LOCAL refs (no
/// network): `after` = the fetched PR head (`refs/remotes/pr/{n}`), `before` =
/// the merge-base of the PR head and origin's default branch — so the redline shows the
/// PR's OWN delta, not unrelated `main` movement since it branched. Errors if the
/// refs aren't present (caller fetches first via [`fetch_pr_refs`]).
pub(crate) fn resolve_pr_commits(
    repo: &git2::Repository,
    pr_number: u64,
) -> Result<(git2::Oid, git2::Oid), String> {
    let head = repo
        .find_reference(&format!("refs/remotes/pr/{pr_number}"))
        .map_err(|e| format!("PR #{pr_number} head not fetched: {e}"))?
        .peel_to_commit()
        .map_err(|e| format!("peel PR #{pr_number} head: {e}"))?
        .id();
    let (base_ref, base) = Option::ok_or(default_branch_ref(repo), NO_ORIGIN_DEFAULT)?;
    let merge_base = Result::map_err(git2::Repository::merge_base(repo, base, head), |e| {
        let branch = str::trim_start_matches(&base_ref, "refs/remotes/");
        format!("merge-base({branch}, PR #{pr_number}): {e}")
    })?;
    Ok((merge_base, head))
}

/// Peel the `(before, after)` commit OIDs to their trees. Small shared helper so
/// both PR impls resolve trees identically.
fn pr_trees<'r>(
    repo: &'r git2::Repository,
    before_oid: git2::Oid,
    after_oid: git2::Oid,
) -> Result<(git2::Tree<'r>, git2::Tree<'r>), String> {
    let before = repo
        .find_commit(before_oid)
        .and_then(|c| c.tree())
        .map_err(|e| format!("before tree: {e}"))?;
    let after = repo
        .find_commit(after_oid)
        .and_then(|c| c.tree())
        .map_err(|e| format!("after tree: {e}"))?;
    Ok((before, after))
}

/// SW-GS-19/13 — classify a tree-vs-tree diff into the grouped [`PublishSet`]:
/// the shared tail of the PR ([`pr_publish_set_impl`]) and commit
/// ([`commit_publish_set_impl`]) sources — only tree RESOLUTION differs. `before`
/// is `None` for a root commit (diff against the empty tree).
fn grouped_set_from_trees(
    repo: &git2::Repository,
    repo_path: &Path,
    before: Option<&git2::Tree>,
    after: &git2::Tree,
) -> Result<PublishSet, String> {
    let mut opts = git2::DiffOptions::new();
    opts.include_typechange(true).show_binary(true);
    let mut diff = repo
        .diff_tree_to_tree(before, Some(after), Some(&mut opts))
        .map_err(|e| format!("tree diff: {e}"))?;
    let records = records_from_diff(repo, &mut diff)?;
    Ok(group_by_provenance(repo_path, records))
}

/// SW-GS-19/13 — the tree-pair redline tail: read `before_rel` from `before` (a
/// rename's pre-rename path; a `None` tree ⇒ absent) and `rel` from `after`, then
/// assemble via [`build_redline`]. Shared by the PR and commit redlines.
fn redline_from_trees(
    repo: &git2::Repository,
    before: Option<&git2::Tree>,
    after: &git2::Tree,
    rel: &Path,
    before_rel: &Path,
) -> Result<FileRedline, String> {
    let before_side = match before {
        Some(t) => tree_side(repo, t, before_rel)?,
        None => None,
    };
    let after_side = tree_side(repo, after, rel)?;
    Ok(build_redline(before_side, after_side))
}

/// SW-GS-19 (A1) — the PR-sourced publish set: diff the PR-head tree against the
/// merge-base tree and classify via the shared [`grouped_set_from_trees`] tail →
/// the SAME grouped [`PublishSet`] the working-tree list renders.
fn pr_publish_set_impl(
    repo_path: &Path,
    pr_number: u64,
    token: &str,
) -> Result<PublishSet, String> {
    let repo = open_repo(repo_path)?;
    fetch_pr_refs(&repo, token, pr_number)?;
    let (before_oid, after_oid) = resolve_pr_commits(&repo, pr_number)?;
    let (before_tree, after_tree) = pr_trees(&repo, before_oid, after_oid)?;
    grouped_set_from_trees(&repo, repo_path, Some(&before_tree), &after_tree)
}

/// SW-GS-19 (A1) — the PR-sourced redline for one file: before = the blob in the
/// merge-base tree, after = the blob in the PR-head tree (both from git objects —
/// the working tree is never read). Reuses [`build_redline`]. If the PR refs
/// aren't local yet (a redline click without a prior list fetch), fetch on demand
/// so the call is self-sufficient; the common path (list fetched first) is
/// network-free.
fn pr_file_redline_impl(
    repo_path: &Path,
    pr_number: u64,
    file_path: &str,
    old_path: Option<&str>,
    token: &str,
) -> Result<FileRedline, String> {
    let repo = open_repo(repo_path)?;
    let rel = safe_rel_path(file_path)?;
    // A rename's baseline content lives at the PRE-rename path (guard `old_path`
    // against traversal too, like the working-tree redline).
    let before_rel = match old_path {
        Some(op) => safe_rel_path(op)?,
        None => rel,
    };
    if repo
        .find_reference(&format!("refs/remotes/pr/{pr_number}"))
        .is_err()
    {
        fetch_pr_refs(&repo, token, pr_number)?;
    }
    let (before_oid, after_oid) = resolve_pr_commits(&repo, pr_number)?;
    let (before_tree, after_tree) = pr_trees(&repo, before_oid, after_oid)?;
    redline_from_trees(&repo, Some(&before_tree), &after_tree, rel, before_rel)
}

/// Read one path from `tree` as a redline side: `Some((bytes, is_binary))` when
/// it's a blob there, `None` when absent (or a directory). The tree-side analog
/// of the workdir read in [`file_redline_impl`], feeding [`build_redline`].
fn tree_side(
    repo: &git2::Repository,
    tree: &git2::Tree,
    rel: &Path,
) -> Result<Option<(Vec<u8>, bool)>, String> {
    match tree.get_path(rel) {
        Ok(entry) => {
            let obj = entry
                .to_object(repo)
                .map_err(|e| format!("tree object {}: {e}", rel.display()))?;
            Ok(obj
                .as_blob()
                .map(|blob| (blob.content().to_vec(), blob.is_binary())))
        }
        Err(_) => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// SW-GS-13 — commit-sourced diff + sync history. The read-only, LOCAL mirror of
// the PR path: a landed commit's changed files + per-file redline come from its
// tree vs its first-parent tree (root → empty tree; merge → first parent), so
// the same grouped list + `build_redline` render a landed commit exactly like a
// PR or the working tree — but with NO network (no `fetch_pr_refs`, no token).
// All git2 work runs under `spawn_blocking` in the commands (non-blocking).
// ---------------------------------------------------------------------------

/// Resolve a commit's `(before, after)` diff trees from LOCAL objects: `after` =
/// the commit's own tree, `before` = its first parent's tree — a root commit
/// yields `None` (diffed against the empty tree → everything reads as added), a
/// merge commit uses its first parent. The local analog of
/// [`resolve_pr_commits`] + [`pr_trees`], minus the fetch.
fn commit_trees<'r>(
    repo: &'r git2::Repository,
    sha: &str,
) -> Result<(Option<git2::Tree<'r>>, git2::Tree<'r>), String> {
    let oid = git2::Oid::from_str(sha).map_err(|e| format!("bad commit sha {sha}: {e}"))?;
    let commit = repo
        .find_commit(oid)
        .map_err(|e| format!("find commit {sha}: {e}"))?;
    commit_diff_trees(&commit)
}

/// The `(before, after)` diff trees for an already-resolved commit — factored out
/// of [`commit_trees`] so the history walk (which holds the `Commit` already) and
/// the sha-keyed impls share ONE parent-selection rule. The returned trees are
/// tied to the repo's lifetime, not the `commit` borrow.
fn commit_diff_trees<'r>(
    commit: &git2::Commit<'r>,
) -> Result<(Option<git2::Tree<'r>>, git2::Tree<'r>), String> {
    let after = tree_or_err(commit, "commit tree")?;
    let before = match commit.parent(0) {
        Ok(parent) => Some(tree_or_err(&parent, "parent tree")?),
        // No first parent ⇒ root commit; diff against the empty tree.
        Err(_) => None,
    };
    Ok((before, after))
}

/// The changed file paths of `commit` vs its first parent (root ⇒ every path in
/// the tree), name-only (NO blob reads) — cheap enough to run per row so the
/// History list can tag each commit Pipeline/Documentation in TS via the node
/// index, with NO per-commit IPC round-trip. Paths are `org_root`-relative (the
/// same shape [`commit_publish_set_impl`] emits), so the FE joins + matches them
/// against the node-owned path set directly.
pub(crate) fn commit_changed_paths(
    repo: &git2::Repository,
    commit: &git2::Commit,
) -> Result<Vec<String>, String> {
    let (before, after) = commit_diff_trees(commit)?;
    changed_paths_between(repo, before.as_ref(), &after)
        .map_err(|error| format!("history path diff: {error}"))
}

/// Typed local-object errors let contribution readers retain durable facts in shallow clones.
pub(crate) fn commit_changed_paths_local(
    repo: &git2::Repository,
    commit: &git2::Commit,
) -> Result<Vec<String>, git2::Error> {
    let (before, after) = local_commit_trees(commit)?;
    changed_paths_between(repo, before.as_ref(), &after)
}

fn local_commit_trees<'r>(
    commit: &git2::Commit<'r>,
) -> Result<(Option<git2::Tree<'r>>, git2::Tree<'r>), git2::Error> {
    let after = git2::Commit::tree(commit)?;
    let before = if git2::Commit::parent_count(commit) == 0 {
        None
    } else {
        Some(git2::Commit::tree(&git2::Commit::parent(commit, 0)?)?)
    };
    Ok((before, after))
}

fn local_tree_diff<'r>(
    repo: &'r git2::Repository,
    before: Option<&git2::Tree>,
    after: &git2::Tree,
) -> Result<git2::Diff<'r>, git2::Error> {
    let mut opts = git2::DiffOptions::new();
    opts.include_typechange(true);
    repo.diff_tree_to_tree(before, Some(after), Some(&mut opts))
}

/// Raw headers retain the parent IDs that libgit2 hides at shallow boundaries.
pub(crate) fn commit_recorded_parent_count(commit: &git2::Commit) -> usize {
    let parents = std::iter::Iterator::filter(
        <[u8]>::split(git2::Commit::raw_header_bytes(commit), |byte| {
            *byte == b'\n'
        }),
        |line| <[u8]>::starts_with(line, b"parent "),
    );
    std::iter::Iterator::count(parents)
}

/// New-side added lines only, coalesced into inclusive windows per file. Binary files and
/// submodule pointers add no lines; `detect_renames` also follows moved files.
/// Missing first-parent objects stay errors; only genuine roots use the empty tree.
pub(crate) fn commit_added_windows_local(
    repo: &git2::Repository,
    commit: &git2::Commit,
    detect_renames: bool,
) -> Result<std::collections::BTreeMap<String, Vec<(u32, u32)>>, git2::Error> {
    if git2::Repository::is_shallow(repo)
        && git2::Commit::parent_count(commit) == 0
        && commit_recorded_parent_count(commit) > 0
    {
        return Err(git2::Error::new(
            git2::ErrorCode::NotFound,
            git2::ErrorClass::Odb,
            crate::attribution::ranges::HISTORY_PAST_SHALLOW,
        ));
    }
    let (before, after) = local_commit_trees(commit)?;
    let mut diff = local_tree_diff(repo, before.as_ref(), &after)?;
    // A moved file is not newly added text: only its edited lines count when asked to follow it.
    if detect_renames {
        // Overrides a repository's configured `diff.renameLimit`; the default 50% similarity
        // applies and copies are not followed.
        let mut options = git2::DiffFindOptions::new();
        options.renames(true).rename_limit(1000);
        git2::Diff::find_similar(&mut diff, Some(&mut options))?;
    }
    let mut files = std::collections::BTreeMap::new();
    for index in 0..git2::Diff::deltas(&diff).len() {
        let Some(patch) = git2::Patch::from_diff(&diff, index)? else {
            continue;
        };
        let delta = git2::Patch::delta(&patch);
        if delta.flags().contains(git2::DiffFlags::BINARY)
            || git2::DiffFile::mode(&git2::DiffDelta::new_file(&delta)) == git2::FileMode::Commit
        {
            continue;
        }
        let Some(path) = git2::DiffFile::path(&git2::DiffDelta::new_file(&delta)).map(path_string)
        else {
            continue;
        };
        let mut windows: Vec<(u32, u32)> = Vec::new();
        for hunk in 0..git2::Patch::num_hunks(&patch) {
            let (_, lines) = git2::Patch::hunk(&patch, hunk)?;
            for index in 0..lines {
                let line = git2::Patch::line_in_hunk(&patch, hunk, index)?;
                if git2::DiffLine::origin(&line) != '+' {
                    continue;
                }
                let Some(number) = git2::DiffLine::new_lineno(&line) else {
                    continue;
                };
                if let Some(last) = windows.last_mut().filter(|last| last.1 + 1 == number) {
                    last.1 = number;
                } else {
                    windows.push((number, number));
                }
            }
        }
        if !std::vec::Vec::is_empty(&windows) {
            files.insert(path, windows);
        }
    }
    Ok(files)
}

fn changed_paths_between(
    repo: &git2::Repository,
    before: Option<&git2::Tree>,
    after: &git2::Tree,
) -> Result<Vec<String>, git2::Error> {
    let diff = local_tree_diff(repo, before, after)?;
    Ok(diff
        .deltas()
        .filter_map(|d| {
            d.new_file()
                .path()
                .or_else(|| d.old_file().path())
                .map(path_string)
        })
        .collect())
}

/// SW-GS-13 — the commit-sourced publish set: diff a landed commit's tree against
/// its first-parent tree and classify via the shared [`records_from_diff`] spine
/// → the SAME grouped [`PublishSet`] the working-tree + PR lists render (so
/// `groupChangesByNode` + the team-scope filter work unchanged). Read-only,
/// local (no fetch). Paths are `org_root`-relative — the same shape the PR/
/// working-tree sets emit, which is what lets SW-GS-7's team-scope filter apply.
fn commit_publish_set_impl(repo_path: &Path, sha: &str) -> Result<PublishSet, String> {
    let repo = open_repo(repo_path)?;
    let (before_tree, after_tree) = commit_trees(&repo, sha)?;
    grouped_set_from_trees(&repo, repo_path, before_tree.as_ref(), &after_tree)
}

/// SW-GS-13 — the commit-sourced redline for one file: before = the blob in the
/// first-parent tree (root/absent ⇒ added), after = the blob in the commit tree.
/// Both from git objects — no working-tree read, no network. Reuses
/// [`build_redline`] / [`tree_side`], mirroring [`pr_file_redline_impl`].
pub(crate) fn commit_file_redline_impl(
    repo_path: &Path,
    sha: &str,
    file_path: &str,
    old_path: Option<&str>,
) -> Result<FileRedline, String> {
    let repo = open_repo(repo_path)?;
    let rel = safe_rel_path(file_path)?;
    // A rename's baseline content lives at the PRE-rename path (guard it too).
    let before_rel = match old_path {
        Some(op) => safe_rel_path(op)?,
        None => rel,
    };
    let (before_tree, after_tree) = commit_trees(&repo, sha)?;
    redline_from_trees(&repo, before_tree.as_ref(), &after_tree, rel, before_rel)
}

/// A newest-first revwalk (`TOPOLOGICAL | TIME`) with NO roots pushed yet — the
/// caller pushes a single tip (history) or all refs (graph). TOPOLOGICAL keeps
/// children strictly before parents so order is well-defined even when a batch of
/// commits shares a timestamp (a pure TIME sort is unstable on equal times);
/// TIME orders across independent branches. Shared by the two commit walks.
pub(crate) fn new_sorted_revwalk(repo: &git2::Repository) -> Result<git2::Revwalk<'_>, String> {
    let mut revwalk = repo.revwalk().map_err(|e| format!("revwalk: {e}"))?;
    revwalk
        .set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)
        .map_err(|e| format!("revwalk sort: {e}"))?;
    Ok(revwalk)
}

/// SLW-WD-8 (RD-2) — the paging tail for the source commit graph: iterate an
/// ALREADY-pushed, ALREADY-sorted `revwalk`, skip through and past the `cursor`
/// commit, then project the next `limit` commits via `project`.
///
/// Cursor contract (AC-9): an absent or MALFORMED cursor starts at the tip; a
/// well-formed but UNREACHABLE cursor (its branch was deleted/force-moved between
/// pages) is never matched, so `skipping` stays set and the walk yields an EMPTY
/// page — returned as `Ok(empty)`, never an error. That is safe only because the
/// same ref-set change bumps WD-9's nonce → WD-9 restarts paging from the tip.
pub(crate) fn paged_revwalk<T>(
    repo: &git2::Repository,
    revwalk: git2::Revwalk<'_>,
    cursor: Option<String>,
    limit: usize,
    mut project: impl FnMut(&git2::Commit) -> Result<T, String>,
) -> Result<Vec<T>, String> {
    // A zero-width page is empty by definition — return before the walk so the
    // push-then-check collect loop can't emit one commit for `limit == 0`.
    if limit == 0 {
        return Ok(Vec::new());
    }
    // Load-older: skip through and past the cursor commit, then collect the next
    // `limit`. An invalid/absent cursor OID means "no cursor" → start at the tip.
    let cursor_oid = cursor.as_deref().and_then(|c| git2::Oid::from_str(c).ok());
    let mut skipping = cursor_oid.is_some();

    // Cap only the PRE-allocation (never the result) so an absurd `limit` can't
    // request a near-`usize::MAX` allocation up front; the Vec still grows to the
    // real page size when a caller legitimately asks for a large window.
    let mut out = Vec::with_capacity(limit.min(1024));
    for oid_result in revwalk {
        let oid = oid_result.map_err(|e| format!("revwalk iter: {e}"))?;
        if skipping {
            if Some(oid) == cursor_oid {
                skipping = false;
            }
            continue;
        }
        let commit = repo
            .find_commit(oid)
            .map_err(|e| format!("find commit {oid}: {e}"))?;
        out.push(project(&commit)?);
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// SLW-WD-8 — classify a full ref name into its graph decoration kind, or `None`
/// for refs that aren't graph decorations (stash, notes, …). The single owner of
/// the `refs/heads|remotes|tags` prefix rules, shared by the decoration map and
/// the walk-root predicate.
pub(crate) fn ref_kind(name: &str) -> Option<RefKind> {
    if name.starts_with("refs/heads/") {
        Some(RefKind::Local)
    } else if name.starts_with("refs/remotes/") {
        Some(RefKind::Remote)
    } else if name.starts_with("refs/tags/") {
        Some(RefKind::Tag)
    } else {
        None
    }
}

/// SLW-WD-8 (RD-6/AC-5) — invert the repo's refs into `oid → [RefLabel]` ONCE, so
/// each emitted commit is decorated in O(1). Branch/remote/tag refs are peeled to
/// their commit (so an ANNOTATED tag decorates the commit it wraps, not the tag
/// object); symbolic refs (`origin/HEAD`) are skipped — redundant with the branch
/// they point at, which is decorated directly. The symbolic `HEAD` — which
/// `references()` does NOT enumerate — is added explicitly as a synthetic `Head`
/// label on the worktree's HEAD commit (in a LINKED worktree `repo.head()`
/// correctly returns THAT worktree's per-worktree HEAD).
pub(crate) fn build_ref_label_map(
    repo: &git2::Repository,
) -> Result<std::collections::HashMap<git2::Oid, Vec<RefLabel>>, String> {
    let mut map: std::collections::HashMap<git2::Oid, Vec<RefLabel>> =
        std::collections::HashMap::new();
    for reference in repo.references().map_err(|e| format!("references: {e}"))? {
        let reference = reference.map_err(|e| format!("reference: {e}"))?;
        // Skip symbolic refs (e.g. origin/HEAD) — redundant with their target.
        if matches!(reference.symbolic_target(), Ok(Some(_))) {
            continue;
        }
        let name = reference.name().unwrap_or_default();
        // Only branch/remote/tag refs are graph decorations (stash, notes, … out).
        let Some(kind) = ref_kind(name) else {
            continue;
        };
        // Peel to commit so an annotated tag decorates the wrapped commit.
        let Ok(commit) = reference.peel_to_commit() else {
            continue;
        };
        let label = RefLabel {
            name: reference.shorthand().unwrap_or(name).to_string(),
            kind,
        };
        map.entry(commit.id()).or_default().push(label);
    }
    // Explicit HEAD decoration (references() omits the symbolic HEAD — RD-6/AC-5).
    if let Ok(head) = repo.head() {
        if let Ok(commit) = head.peel_to_commit() {
            map.entry(commit.id()).or_default().push(RefLabel {
                name: "HEAD".to_string(),
                kind: RefKind::Head,
            });
        }
    }
    Ok(map)
}

/// Project one commit to the graph view-model: metadata + ALL parent shas (a
/// merge yields >1 so WD-9 can draw the join — AC-8) + the ref decorations
/// pointing at it (empty when none). NO diff at walk time (AC-3).
fn graph_entry(
    commit: &git2::Commit,
    refs_map: &std::collections::HashMap<git2::Oid, Vec<RefLabel>>,
) -> SourceCommitGraphEntry {
    let sha = commit.id().to_string();
    SourceCommitGraphEntry {
        short_sha: sha.chars().take(7).collect(),
        parents: commit.parent_ids().map(|p| p.to_string()).collect(),
        refs: refs_map.get(&commit.id()).cloned().unwrap_or_default(),
        subject: Option::flatten(Result::ok(commit.summary()))
            .unwrap_or("")
            .to_string(),
        author: commit.author().name().unwrap_or("").to_string(),
        committed_at: commit.time().seconds(),
        sha,
    }
}

/// SLW-WD-8 (AC-2/3/5/8) — the source worktree's all-refs commit graph: a paged,
/// newest-first window of commits reachable from every local + remote branch ref
/// (`refs/heads/*`, `refs/remotes/*`) plus HEAD, each projected to a
/// [`SourceCommitGraphEntry`] with its parents (edges) + ref decorations. NO
/// per-commit diff at walk time (AC-3) — changed files come from the lazy
/// [`source_commit_changeset_impl`]. Read-only, local objects only.
fn source_commit_graph_impl(
    repo_path: &Path,
    limit: usize,
    cursor: Option<String>,
) -> Result<Vec<SourceCommitGraphEntry>, String> {
    let repo = open_repo(repo_path)?;
    let refs_map = build_ref_label_map(&repo)?;

    let mut revwalk = new_sorted_revwalk(&repo)?;
    let mut pushed = false;
    for reference in repo.references().map_err(|e| format!("references: {e}"))? {
        let reference = reference.map_err(|e| format!("reference: {e}"))?;
        let name = reference.name().unwrap_or_default();
        // Walk roots are the branch/remote tips (tags decorate but don't seed the
        // walk — an unreachable tag shouldn't drag in orphan history).
        if matches!(ref_kind(name), Some(RefKind::Local | RefKind::Remote)) {
            // Direct branch/remote refs have a target OID; the symbolic
            // `origin/HEAD` (target() == None) is skipped — redundant with the
            // branch it points at, which is pushed directly.
            if let Some(oid) = reference.target() {
                revwalk
                    .push(oid)
                    .map_err(|e| format!("revwalk push: {e}"))?;
                pushed = true;
            }
        }
    }
    // Push HEAD too, so a DETACHED HEAD (no branch ref) still walks (AC-8); a
    // duplicate push of an already-pushed branch tip is a no-op.
    if let Ok(Some(oid)) = repo.head().map(|h| h.target()) {
        revwalk
            .push(oid)
            .map_err(|e| format!("revwalk push HEAD: {e}"))?;
        pushed = true;
    }
    // Nothing to walk (unborn HEAD / empty repo) ⇒ empty graph, not an error (AC-8).
    if !pushed {
        return Ok(Vec::new());
    }

    paged_revwalk(&repo, revwalk, cursor, limit, |commit| {
        Ok(graph_entry(commit, &refs_map))
    })
}

/// SLW-WD-8 (RD-3) — the FLAT per-commit changed-file records: diff a commit's
/// tree against its first-parent tree (root ⇒ empty tree) and classify via the
/// shared [`records_from_diff`] spine — WITHOUT the context-repo
/// meta-coupling/provenance grouping [`grouped_set_from_trees`] applies. The
/// commit-tree analog of [`changeset_records`] (working-tree), sharing the same
/// classify spine so the two can't drift.
fn commit_changeset_records(
    repo: &git2::Repository,
    sha: &str,
) -> Result<Vec<PublishFile>, String> {
    let (before, after) = commit_trees(repo, sha)?;
    let mut opts = git2::DiffOptions::new();
    opts.include_typechange(true).show_binary(true);
    let mut diff = repo
        .diff_tree_to_tree(before.as_ref(), Some(&after), Some(&mut opts))
        .map_err(|e| format!("commit diff: {e}"))?;
    records_from_diff(repo, &mut diff)
}

/// SLW-WD-8 (AC-4) — the per-commit change-set for a source worktree: the flat
/// `Vec<PublishFile>` for `sha` vs its first parent. Read-only, local.
pub fn source_commit_changeset_impl(
    repo_path: &Path,
    sha: &str,
) -> Result<Vec<PublishFile>, String> {
    let repo = open_repo(repo_path)?;
    commit_changeset_records(&repo, sha)
}

/// SLW-WD-8 (AC-2/3/6) — the source worktree commit graph (all refs, paginated,
/// newest-first). Read-only, off the UI thread (`spawn_blocking`); no network, no
/// writes. `cursor` (a sha) resumes AFTER that commit for load-older paging under
/// a FIXED ref set (AC-9).
#[tauri::command]
pub async fn source_commit_graph(
    repo_path: String,
    limit: usize,
    cursor: Option<String>,
) -> Result<Vec<SourceCommitGraphEntry>, String> {
    let path = std::path::PathBuf::from(repo_path);
    tauri::async_runtime::spawn_blocking(move || source_commit_graph_impl(&path, limit, cursor))
        .await
        .map_err(|e| format!("source_commit_graph task failed: {e}"))?
}

/// SLW-WD-8 (AC-4/6) — the FLAT per-commit change-set for a source worktree commit
/// (`sha` vs its first parent). Read-only, off the UI thread; no network, no writes.
#[tauri::command]
pub async fn source_commit_changeset(
    repo_path: String,
    sha: String,
) -> Result<Vec<PublishFile>, String> {
    let path = std::path::PathBuf::from(repo_path);
    tauri::async_runtime::spawn_blocking(move || source_commit_changeset_impl(&path, &sha))
        .await
        .map_err(|e| format!("source_commit_changeset task failed: {e}"))?
}

/// SLW-WD-8 (AC-4/6, RD-4) — the per-commit per-file redline for a source worktree.
/// Reuses [`commit_file_redline_impl`] VERBATIM (before = first-parent blob, after
/// = commit blob — both git objects), pointed at the worktree path. `old_path` is
/// set only for a rename; empty is normalized to `None`. Read-only, off the UI
/// thread; binary files return `binary: true` + empty content.
#[tauri::command]
pub async fn source_commit_file_redline(
    repo_path: String,
    sha: String,
    file_path: String,
    old_path: Option<String>,
) -> Result<FileRedline, String> {
    let path = std::path::PathBuf::from(repo_path);
    let old_path = old_path.filter(|p| !p.is_empty());
    tauri::async_runtime::spawn_blocking(move || {
        commit_file_redline_impl(&path, &sha, &file_path, old_path.as_deref())
    })
    .await
    .map_err(|e| format!("source_commit_file_redline task failed: {e}"))?
}

/// SW-GS-3: raw bytes of `file_path` as it stands in the publish baseline
/// (`None` when the path isn't present in the baseline). Reuses
/// [`resolve_baseline_tree`]. Used by the HTTP `/viewer-baseline` route to serve
/// the "before" side of an image / HTML before-after toggle (these formats are
/// URL-served, not string-served like the markdown redline).
pub(crate) fn read_baseline_blob(
    repo_path: &Path,
    file_path: &str,
) -> Result<Option<Vec<u8>>, String> {
    let rel = safe_rel_path(file_path)?;
    let repo = open_repo(repo_path)?;
    let Some(tree) = resolve_baseline_tree(&repo)? else {
        return Ok(None);
    };
    tree_blob_bytes(&repo, &tree, rel)
}

/// SW-GS-10 — raw bytes of `file_path` as it stands in an ARBITRARY tree
/// (`None` when the path is absent there). The blob-by-OID read the conflict
/// redline needs: `/viewer-baseline?tree=…` serves the immutable snapshot
/// (`oursTreeOid`) and the merged tree so the resolved-vs-ours after-redline
/// renders from git objects — no disk write, no markers. Sibling of
/// [`read_baseline_blob`] (which is pinned to the origin default-branch publish baseline);
/// reuses the same private [`tree_blob_bytes`]. The `git2::Oid::from_str` +
/// `find_tree` idiom mirrors [`plan_writeback`].
pub(crate) fn read_tree_blob(
    repo_path: &Path,
    tree_oid: &str,
    file_path: &str,
) -> Result<Option<Vec<u8>>, String> {
    let rel = safe_rel_path(file_path)?;
    let repo = open_repo(repo_path)?;
    let oid = git2::Oid::from_str(tree_oid).map_err(|e| format!("tree oid: {e}"))?;
    let tree = repo.find_tree(oid).map_err(|e| format!("find tree: {e}"))?;
    tree_blob_bytes(&repo, &tree, rel)
}

/// Bytes of `rel` in `tree`, or `None` when the path is absent there (or is not a
/// blob). The single git2 tree→blob-bytes read, shared by the baseline reader and
/// the write-back freshness guard.
fn tree_blob_bytes(
    repo: &git2::Repository,
    tree: &git2::Tree,
    rel: &Path,
) -> Result<Option<Vec<u8>>, String> {
    // SW-GS-19: the bytes-only projection of `tree_side` (drops the is-binary
    // flag the redline needs but this baseline/write-back reader doesn't).
    Ok(tree_side(repo, tree, rel)?.map(|(bytes, _)| bytes))
}

/// SW-GS-20 — does the working tree hold any uncommitted change (tracked
/// modification/deletion OR an untracked, non-ignored file/dir)? Drives the
/// pull fast-path split: a CLEAN tree behind origin takes the cheap plain
/// fast-forward; a DIRTY tree takes the inbound 3-way merge that preserves WIP.
fn working_tree_dirty(repo: &git2::Repository) -> Result<bool, String> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    let statuses = repo
        .statuses(Some(&mut opts))
        .map_err(|e| format!("status: {e}"))?;
    Ok(!statuses.is_empty())
}

/// Count how many commits origin's default branch is ahead of local HEAD (0 ⇒ not
/// behind). Used for the incoming signal.
pub fn incoming_count(repo: &git2::Repository) -> Result<u32, String> {
    let local = match repo.head().ok().and_then(|h| h.target()) {
        Some(o) => o,
        None => return Ok(0),
    };
    let Some(origin) = default_branch(repo) else {
        return Ok(0);
    };
    if local == origin {
        return Ok(0);
    }
    // origin ahead ⇒ local is an ancestor of origin.
    let (ahead, _behind) = repo
        .graph_ahead_behind(origin, local)
        .map_err(|e| format!("graph_ahead_behind: {e}"))?;
    Ok(ahead as u32)
}

// ---------------------------------------------------------------------------
// SW-GS-6 — out-of-sync detection ("N incoming" badge).
//
// The proactive counterpart to GS-8's push-time behind check: a cheap,
// read-only `fetch` + behind-count so the sync toggle can warn "pull first"
// BEFORE a publish is attempted. Deliberately leaner than `get_pull_set_impl`
// (no tree diff, no 3-way regions) — a badge only needs the commit count.
// ---------------------------------------------------------------------------

/// Behind-count for the incoming badge (SW-GS-6). A struct (not a bare `u32`)
/// so an optional ahead-count can be added later without a signature break.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingStatus {
    /// Commits origin's default branch is ahead of local HEAD (0 ⇒ up to date).
    pub incoming: u32,
    /// SW-GS-31 — is a *pure* fast-forward currently possible (behind +
    /// not-diverged + clean working tree)? The auto-sync-down driver fires only
    /// when this is `true`, so a dirty/diverged repo is skipped on the cheap
    /// read-only probe alone (no lock, no fetch loop).
    pub can_fast_forward: bool,
    /// Why `incoming` is 0 and `canFastForward` false: origin's default branch is not
    /// known locally ([`NO_ORIGIN_DEFAULT`]). Absent when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch_reason: Option<String>,
}

/// SW-GS-31 — is a *pure* fast-forward currently possible? True iff the repo is on
/// origin's default branch, that remote-tracking ref is a strict descendant of the
/// local branch (behind, NOT diverged), and the working tree is clean. Degrades to
/// `false` (never a hard error for the state) off the default branch, with no origin
/// default branch locally ([`IncomingStatus::default_branch_reason`] says so), or on an
/// unborn branch — the same passive contract as the badge count.
fn repo_can_fast_forward(repo: &git2::Repository) -> Result<bool, String> {
    let Some((origin_ref, origin)) = default_branch_ref(repo) else {
        return Ok(false);
    };
    // On the default branch? A detached or other HEAD is never a fast-forward target.
    let head = match repo.head() {
        Ok(h) => h,
        Err(_) => return Ok(false),
    };
    let on_default = git2::Reference::shorthand(&head) == Ok(default_branch_name(&origin_ref));
    if !git2::Reference::is_branch(&head) || !on_default {
        return Ok(false);
    }
    let local = match head.target() {
        Some(o) => o,
        None => return Ok(false),
    };
    // Up-to-date (local already is / contains origin) ⇒ nothing to fast-forward.
    if local == origin
        || repo
            .graph_descendant_of(local, origin)
            .map_err(|e| format!("graph check: {e}"))?
    {
        return Ok(false);
    }
    // Diverged (origin is NOT a strict descendant of local — local-only commits or
    // real overlap) ⇒ not a pure fast-forward; the manual Pull owns that case.
    if !repo
        .graph_descendant_of(origin, local)
        .map_err(|e| format!("graph check: {e}"))?
    {
        return Ok(false);
    }
    // Behind + not-diverged: a pure fast-forward is possible iff the tree is clean
    // (a dirty tree routes to the SW-GS-20 manual 3-way merge, never the auto path).
    Ok(!working_tree_dirty(repo)?)
}

/// SW-GS-6 — read-only behind-count for `repo_path`, off the refs already on disk
/// (CodeSense fetches nothing here; agents do the git), reusing the shipped
/// [`incoming_count`] signal. Passive: with no origin default branch locally it counts
/// 0 and says why in `defaultBranchReason`, rather than erroring.
pub fn incoming_status_impl(repo_path: &Path) -> Result<IncomingStatus, String> {
    let repo = open_repo(repo_path)?;
    Ok(IncomingStatus {
        incoming: incoming_count(&repo)?,
        // SW-GS-31 — cheap additive on the already-open repo: the auto-sync-down gate.
        can_fast_forward: repo_can_fast_forward(&repo)?,
        // A private repo has no origin to fetch or set-head, so it gets no reason.
        default_branch_reason: bool::then(
            !crate::context_repo::scratch_space::is_private_repo(&repo)
                && Option::is_none(&default_branch(&repo)),
            || std::string::String::from(NO_ORIGIN_DEFAULT),
        ),
    })
}

/// SW-GS-6 Tauri command — the session-less "N incoming" probe for the sync
/// toggle. Off-thread (`spawn_blocking`) so the git2 read never stalls the UI
/// thread; the front-end calls it fire-and-forget (project-context §Performance).
#[tauri::command]
pub async fn incoming_status(repo_path: String) -> Result<IncomingStatus, String> {
    let path = std::path::PathBuf::from(repo_path);
    tauri::async_runtime::spawn_blocking(move || incoming_status_impl(&path))
        .await
        .map_err(|e| format!("incoming_status task failed: {e}"))?
}

#[cfg(test)]
mod tests {
    fn commit_local(path: &Path, files: &[(&str, &str)]) -> git2::Oid {
        let repo = git2::Repository::open(path).unwrap();
        for (rel, content) in files {
            fs::write(path.join(rel), content).unwrap();
        }
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        let parent = repo
            .find_commit(repo.head().unwrap().target().unwrap())
            .unwrap();
        let oid = repo
            .commit(
                Some("refs/heads/main"),
                &sig,
                &sig,
                "local",
                &tree,
                &[&parent],
            )
            .unwrap();
        let obj = repo.find_object(oid, None).unwrap();
        repo.reset(&obj, git2::ResetType::Hard, None).unwrap();
        oid
    }
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Init a repo with one commit on `main` and an `origin/main` remote-tracking
    /// ref pointing at it, so `publish_set_impl` diffs the workdir against the
    /// publish baseline. Returns the repo working dir.
    fn repo_with_main(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let repo = git2::Repository::init(&path).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "Test").unwrap();
            cfg.set_str("user.email", "test@example.com").unwrap();
        }
        for (rel, content) in files {
            let full = path.join(rel);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(full, content).unwrap();
        }
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = repo.signature().unwrap();
        let commit_oid = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        repo.set_head("refs/heads/main").unwrap();
        let head_obj = repo.find_object(commit_oid, None).unwrap();
        repo.reset(&head_obj, git2::ResetType::Hard, None).unwrap();
        // Simulate the publish baseline.
        repo.reference("refs/remotes/origin/main", commit_oid, true, "origin/main")
            .unwrap();
        (dir, path)
    }

    #[test]
    fn read_tree_blob_reads_by_oid_and_misses_gracefully() {
        // SW-GS-10 T3 — the blob-by-(tree OID + path) read the conflict redline
        // uses (via `/viewer-baseline?tree=…`) to render resolved-vs-ours from git
        // objects, no disk write.
        let (_dir, path) = repo_with_main(&[("docs/a.md", "hello\n"), ("b.md", "bee\n")]);
        let repo = git2::Repository::open(&path).unwrap();
        let tree_oid = repo
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .tree()
            .unwrap()
            .id()
            .to_string();

        // Present path → exact blob bytes from that tree.
        let bytes = read_tree_blob(&path, &tree_oid, "docs/a.md").unwrap();
        assert_eq!(bytes.as_deref(), Some(&b"hello\n"[..]));

        // Absent path in that tree → None (a missing file, not an error).
        assert_eq!(
            read_tree_blob(&path, &tree_oid, "missing.md").unwrap(),
            None
        );

        // A malformed OID is a typed error, never a panic.
        assert!(read_tree_blob(&path, "not-an-oid", "docs/a.md").is_err());
    }

    #[test]
    fn incoming_status_observes_local_refs_without_fetching() {
        let (_remote_dir, remote_path) = repo_with_main(&[("a.md", "base\n")]);
        let local_dir = tempfile::tempdir().unwrap();
        let local_path = local_dir.path().join("checkout");
        let repo = git2::Repository::clone(remote_path.to_str().unwrap(), &local_path).unwrap();
        let before = repo
            .find_reference("refs/remotes/origin/main")
            .unwrap()
            .target();
        commit_local(&remote_path, &[("a.md", "remote change\n")]);
        let index_before = fs::read(repo.path().join("index")).unwrap();
        let status = incoming_status_impl(&local_path).unwrap();
        assert_eq!(status.incoming, 0);
        assert_eq!(
            repo.find_reference("refs/remotes/origin/main")
                .unwrap()
                .target(),
            before
        );
        assert_eq!(fs::read(repo.path().join("index")).unwrap(), index_before);
        assert_eq!(
            fs::read_to_string(local_path.join("a.md")).unwrap(),
            "base\n"
        );
    }

    fn all_paths(set: &PublishSet) -> Vec<String> {
        let mut v: Vec<String> = set.general.files.iter().map(|f| f.path.clone()).collect();
        for b in &set.buckets {
            v.extend(b.files.iter().map(|f| f.path.clone()));
        }
        v.sort();
        v
    }

    fn find_file<'a>(set: &'a PublishSet, path: &str) -> Option<&'a PublishFile> {
        set.general
            .files
            .iter()
            .chain(set.buckets.iter().flat_map(|b| b.files.iter()))
            .find(|f| f.path == path)
    }

    // --- SW-GS-19 (A1): tree-sourced PR diff helpers ---------------------------

    /// Commit `changes` (path → Some(content) to add/modify, None to delete) as a
    /// child of `main`'s tip on `refs/remotes/pr/{n}` — the shape a fetched PR head
    /// takes. Top-level paths only (TreeBuilder is flat). No workdir writes.
    fn commit_pr_head(path: &Path, n: u64, changes: &[(&str, Option<&str>)]) -> git2::Oid {
        let repo = git2::Repository::open(path).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let base_tree = parent.tree().unwrap();
        let mut builder = repo.treebuilder(Some(&base_tree)).unwrap();
        for (rel, content) in changes {
            match content {
                Some(c) => {
                    let blob = repo.blob(c.as_bytes()).unwrap();
                    builder.insert(rel, blob, 0o100644).unwrap();
                }
                None => {
                    builder.remove(rel).ok();
                }
            }
        }
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        let oid = repo
            .commit(None, &sig, &sig, "pr", &tree, &[&parent])
            .unwrap();
        repo.reference(&format!("refs/remotes/pr/{n}"), oid, true, "pr head")
            .unwrap();
        oid
    }

    #[test]
    fn build_redline_derives_status_from_sides() {
        let added = build_redline(None, Some((b"new\n".to_vec(), false)));
        assert_eq!(added.status, PublishFileStatus::Added);
        assert!(!added.binary);
        assert_eq!(added.before, "");
        assert_eq!(added.after, "new\n");

        let deleted = build_redline(Some((b"old\n".to_vec(), false)), None);
        assert_eq!(deleted.status, PublishFileStatus::Deleted);
        assert_eq!(deleted.before, "old\n");
        assert_eq!(deleted.after, "");

        let modified = build_redline(
            Some((b"a\n".to_vec(), false)),
            Some((b"b\n".to_vec(), false)),
        );
        assert_eq!(modified.status, PublishFileStatus::Modified);
    }

    #[test]
    fn build_redline_binary_empties_content() {
        let r = build_redline(Some((b"x".to_vec(), false)), Some((vec![0, 1, 2], true)));
        assert!(r.binary);
        assert_eq!(r.before, "");
        assert_eq!(r.after, "");
        assert_eq!(r.status, PublishFileStatus::Modified);
    }

    #[test]
    fn resolve_pr_commits_uses_merge_base() {
        let (_dir, path) = repo_with_main(&[("a.md", "hello\n")]);
        let head = commit_pr_head(&path, 7, &[("a.md", Some("hi\n"))]);
        let repo = git2::Repository::open(&path).unwrap();
        let (before, after) = resolve_pr_commits(&repo, 7).unwrap();
        assert_eq!(after, head, "after = PR head");
        // The PR branched straight off origin/main, so the merge-base IS main's tip.
        assert_eq!(
            before,
            repo.refname_to_id("refs/remotes/origin/main").unwrap()
        );
    }

    #[test]
    fn pr_tree_diff_classifies_via_shared_spine() {
        let (_dir, path) = repo_with_main(&[("a.md", "hello\n")]);
        commit_pr_head(
            &path,
            7,
            &[("a.md", Some("hello there\n")), ("b.md", Some("new\n"))],
        );
        let repo = git2::Repository::open(&path).unwrap();
        let (before_oid, after_oid) = resolve_pr_commits(&repo, 7).unwrap();
        let (before_tree, after_tree) = pr_trees(&repo, before_oid, after_oid).unwrap();
        let mut opts = git2::DiffOptions::new();
        opts.include_typechange(true);
        let mut diff = repo
            .diff_tree_to_tree(Some(&before_tree), Some(&after_tree), Some(&mut opts))
            .unwrap();
        let mut records = records_from_diff(&repo, &mut diff).unwrap();
        records.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].path, "a.md");
        assert_eq!(records[0].status, PublishFileStatus::Modified);
        assert_eq!(records[1].path, "b.md");
        assert_eq!(records[1].status, PublishFileStatus::Added);
    }

    #[test]
    fn pr_redline_reads_both_sides_from_trees() {
        let (_dir, path) = repo_with_main(&[("a.md", "hello\n")]);
        commit_pr_head(&path, 7, &[("a.md", Some("hello there\n"))]);
        let repo = git2::Repository::open(&path).unwrap();
        let (before_oid, after_oid) = resolve_pr_commits(&repo, 7).unwrap();
        let (before_tree, after_tree) = pr_trees(&repo, before_oid, after_oid).unwrap();
        let before = tree_side(&repo, &before_tree, Path::new("a.md")).unwrap();
        let after = tree_side(&repo, &after_tree, Path::new("a.md")).unwrap();
        let r = build_redline(before, after);
        assert_eq!(r.status, PublishFileStatus::Modified);
        assert_eq!(r.before, "hello\n");
        assert_eq!(r.after, "hello there\n");
    }

    #[test]
    fn pr_redline_rename_reads_before_from_old_path() {
        // A PR that renames old.md -> new.md WITH an edit must redline against the
        // pre-rename content, not read as all-added (the get_file_redline PR path
        // threads `old_path` into `before_rel`).
        let (_dir, path) = repo_with_main(&[("old.md", "original\n")]);
        commit_pr_head(
            &path,
            7,
            &[("old.md", None), ("new.md", Some("original + edit\n"))],
        );
        let repo = git2::Repository::open(&path).unwrap();
        let (before_oid, after_oid) = resolve_pr_commits(&repo, 7).unwrap();
        let (before_tree, after_tree) = pr_trees(&repo, before_oid, after_oid).unwrap();
        // Mirror pr_file_redline_impl's rename handling: before from old path.
        let before = tree_side(&repo, &before_tree, Path::new("old.md")).unwrap();
        let after = tree_side(&repo, &after_tree, Path::new("new.md")).unwrap();
        let r = build_redline(before, after);
        assert_eq!(r.status, PublishFileStatus::Modified);
        assert_eq!(r.before, "original\n");
        assert_eq!(r.after, "original + edit\n");
    }

    #[test]
    fn pr_redline_absent_path_reads_as_added() {
        let (_dir, path) = repo_with_main(&[("a.md", "hello\n")]);
        commit_pr_head(&path, 7, &[("b.md", Some("brand new\n"))]);
        let repo = git2::Repository::open(&path).unwrap();
        let (before_oid, after_oid) = resolve_pr_commits(&repo, 7).unwrap();
        let (before_tree, after_tree) = pr_trees(&repo, before_oid, after_oid).unwrap();
        let before = tree_side(&repo, &before_tree, Path::new("b.md")).unwrap();
        let after = tree_side(&repo, &after_tree, Path::new("b.md")).unwrap();
        assert!(before.is_none(), "b.md absent in the merge-base");
        assert_eq!(
            build_redline(before, after).status,
            PublishFileStatus::Added
        );
    }

    #[test]
    fn modified_and_untracked_land_in_general() {
        let (_dir, path) = repo_with_main(&[("a.md", "hello\n")]);
        fs::write(path.join("a.md"), "hello world\n").unwrap();
        fs::write(path.join("b.md"), "new\n").unwrap();

        let set = publish_set_impl(&path, None, None).unwrap();

        // AC-10: no provenance stamp yet → everything degrades to General.
        assert!(set.buckets.is_empty(), "no grouping until SW-CP-22");
        assert_eq!(set.general.count, 2);
        assert_eq!(
            all_paths(&set),
            vec!["a.md".to_string(), "b.md".to_string()]
        );
        // Plain content changes carry no badge.
        assert!(find_file(&set, "a.md").unwrap().badges.is_empty());
        assert!(find_file(&set, "b.md").unwrap().badges.is_empty());
    }

    #[test]
    fn surfaces_add_modify_delete_status() {
        let (_dir, path) =
            repo_with_main(&[("keep.md", "k\n"), ("mod.md", "old\n"), ("del.md", "bye\n")]);
        fs::write(path.join("mod.md"), "new\n").unwrap(); // modified
        fs::remove_file(path.join("del.md")).unwrap(); // deleted
        fs::write(path.join("add.md"), "fresh\n").unwrap(); // added (untracked)

        let set = publish_set_impl(&path, None, None).unwrap();

        assert_eq!(
            find_file(&set, "mod.md").unwrap().status,
            PublishFileStatus::Modified
        );
        assert_eq!(
            find_file(&set, "del.md").unwrap().status,
            PublishFileStatus::Deleted
        );
        assert_eq!(
            find_file(&set, "add.md").unwrap().status,
            PublishFileStatus::Added
        );
        // Unchanged file is absent from the publish set.
        assert!(find_file(&set, "keep.md").is_none());
    }

    #[test]
    fn rename_is_detected_and_badged() {
        // Substantial identical content so find_similar scores it a rename.
        let body = "line one\nline two\nline three\nline four\nline five\n";
        let (_dir, path) = repo_with_main(&[("foo.md", body)]);
        fs::remove_file(path.join("foo.md")).unwrap();
        fs::write(path.join("bar.md"), body).unwrap();

        let set = publish_set_impl(&path, None, None).unwrap();

        let bar = find_file(&set, "bar.md").expect("renamed-to path present");
        assert!(bar.badges.contains(&PublishFileBadge::Rename));
        // SW-GS-14: the pre-rename path is captured for the FE chip + redline.
        assert_eq!(bar.rename_old.as_deref(), Some("foo.md"));
        // The old path is consumed by the rename (no separate Deleted row).
        assert!(find_file(&set, "foo.md").is_none());
    }

    #[test]
    fn excalidraw_rename_is_unpaired_into_add_and_delete() {
        // Two .excalidraw files whose shared JSON scaffolding clears git's
        // similarity threshold — find_similar would pair them as a rename.
        // SW-GS-3: a toggle-type (non-redlinable) rename must un-pair into a
        // clean Added(new) + Deleted(old), never a "Modified" rename.
        let body =
            "{\"type\":\"excalidraw\",\"version\":2,\"source\":\"x\",\"elements\":[],\"appState\":{}}\n";
        let (_dir, path) = repo_with_main(&[("old.excalidraw", body)]);
        fs::remove_file(path.join("old.excalidraw")).unwrap();
        fs::write(path.join("new.excalidraw"), body).unwrap();

        let set = publish_set_impl(&path, None, None).unwrap();

        let new = find_file(&set, "new.excalidraw").expect("added path present");
        assert_eq!(new.status, PublishFileStatus::Added);
        assert!(
            new.badges.is_empty(),
            "un-paired add carries no rename badge, got {:?}",
            new.badges
        );
        let old = find_file(&set, "old.excalidraw").expect("deleted path present");
        assert_eq!(old.status, PublishFileStatus::Deleted);
    }

    #[test]
    fn changeset_classifies_binary_add_modify_without_show_binary() {
        // `changeset_records` no longer sets `show_binary` (binary content is
        // fetched lazily by the redline viewer). The changed-file LIST must still
        // classify binary A/M correctly — the list carries name+status+badges, not
        // content, so dropping the binary payload is behavior-preserving here.
        let png_v1: &[u8] = &[0x89, b'P', b'N', b'G', 0, 1, 2, 3, 0, 4];
        let png_v2: &[u8] = &[0x89, b'P', b'N', b'G', 0, 9, 8, 7, 0, 6, 5];
        let (_dir, path) = repo_with_main(&[("keep.md", "k\n")]);
        // Commit a tracked binary into the baseline, then modify it, to get a
        // Modified binary delta. `added.bin` is written AFTER the commit so it
        // stays an untracked add.
        let repo = git2::Repository::open(&path).unwrap();
        fs::write(path.join("tracked.bin"), png_v1).unwrap();
        {
            let mut index = repo.index().unwrap();
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree_oid = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_oid).unwrap();
            let sig = repo.signature().unwrap();
            let parent = repo.head().unwrap().peel_to_commit().unwrap();
            let commit = repo
                .commit(Some("HEAD"), &sig, &sig, "add binaries", &tree, &[&parent])
                .unwrap();
            // Move the publish baseline forward so tracked.bin is baseline-tracked.
            repo.reference("refs/remotes/origin/main", commit, true, "advance")
                .unwrap();
        }
        // Now modify the tracked binary + leave added.bin untracked-added.
        fs::write(path.join("tracked.bin"), png_v2).unwrap();
        fs::write(path.join("added.bin"), png_v1).unwrap();

        let set = publish_set_impl(&path, None, None).unwrap();
        assert_eq!(
            find_file(&set, "tracked.bin").map(|f| f.status),
            Some(PublishFileStatus::Modified),
            "binary modify still classifies as Modified"
        );
        assert_eq!(
            find_file(&set, "added.bin").map(|f| f.status),
            Some(PublishFileStatus::Added),
            "binary add still classifies as Added"
        );
    }

    #[test]
    fn symlink_typechange_is_mode_badged() {
        let (_dir, path) = repo_with_main(&[("target.md", "x\n"), ("link", "placeholder\n")]);
        // Replace the regular file `link` with a symlink → a type/mode change.
        fs::remove_file(path.join("link")).unwrap();
        std::os::unix::fs::symlink("target.md", path.join("link")).unwrap();

        let set = publish_set_impl(&path, None, None).unwrap();

        let link = find_file(&set, "link").expect("link path present");
        assert!(
            link.badges.contains(&PublishFileBadge::Mode),
            "symlink type change should be mode-badged, got {:?}",
            link.badges
        );
    }

    #[test]
    fn eol_only_change_is_badged() {
        let (_dir, path) = repo_with_main(&[("doc.md", "alpha\nbeta\n")]);
        fs::write(path.join("doc.md"), "alpha\r\nbeta\r\n").unwrap();

        let set = publish_set_impl(&path, None, None).unwrap();

        let doc = find_file(&set, "doc.md").expect("doc present");
        assert!(
            doc.badges.contains(&PublishFileBadge::EolOnly),
            "CRLF-only change should be eol-only, got {:?}",
            doc.badges
        );
    }

    #[test]
    fn eol_only_size_cap_suppresses_badge_on_large_blob() {
        // An EOL-only change on a blob over the cap must NOT be inflated to test
        // EOL — it reads as a plain Modified with no EolOnly badge. A small blob
        // with the identical kind of change still gets the badge (the cap alone
        // decides). This bounds the always-warm diff path's blob inflation.
        let big_line = "x".repeat(1024);
        let mut lf = String::new();
        let mut crlf = String::new();
        // ~2 MiB — comfortably over EOL_ONLY_SIZE_CAP (512 KiB).
        for _ in 0..2048 {
            lf.push_str(&big_line);
            lf.push('\n');
            crlf.push_str(&big_line);
            crlf.push_str("\r\n");
        }
        assert!(lf.len() as u64 > EOL_ONLY_SIZE_CAP);
        let (_dir, path) = repo_with_main(&[("big.md", lf.as_str())]);
        fs::write(path.join("big.md"), &crlf).unwrap();

        let set = publish_set_impl(&path, None, None).unwrap();
        let big = find_file(&set, "big.md").expect("big present");
        assert_eq!(big.status, PublishFileStatus::Modified);
        assert!(
            !big.badges.contains(&PublishFileBadge::EolOnly),
            "large blob must skip EOL inflation (no badge), got {:?}",
            big.badges
        );
    }

    #[test]
    fn eol_only_size_cap_covers_large_workdir_side() {
        // The cap must also guard the WORKDIR side: a small baseline blob whose
        // UNSTAGED working-tree version grows past the cap must not be inflated.
        // (git2 may report the workdir size as 0/unknown for such a delta; the
        // cap treats an unknown size as over-cap, so either way no inflation runs
        // and no EolOnly badge appears.)
        let big_line = "y".repeat(1024);
        let mut big_crlf = String::new();
        for _ in 0..2048 {
            big_crlf.push_str(&big_line);
            big_crlf.push_str("\r\n");
        }
        assert!(big_crlf.len() as u64 > EOL_ONLY_SIZE_CAP);
        // Baseline is a small file; the workdir version is the huge CRLF blob.
        let (_dir, path) = repo_with_main(&[("grow.md", "small\n")]);
        fs::write(path.join("grow.md"), &big_crlf).unwrap();

        let set = publish_set_impl(&path, None, None).unwrap();
        let grow = find_file(&set, "grow.md").expect("grow present");
        assert_eq!(grow.status, PublishFileStatus::Modified);
        assert!(
            !grow.badges.contains(&PublishFileBadge::EolOnly),
            "large workdir side must skip EOL inflation (no badge), got {:?}",
            grow.badges
        );
    }

    #[test]
    fn changeset_records_pathspec_scopes_classified_list_to_subtree() {
        // The CLASSIFIED-list path (not just the count) must honor the pathspec:
        // a non-None `path_prefix` on `publish_set_impl` excludes every change
        // outside the team subtree, and results match the client-scoped view.
        let (_dir, path) = repo_with_main(&[
            ("team/a.md", "a\n"),
            ("team/sub/b.md", "b\n"),
            ("other/c.md", "c\n"),
        ]);
        fs::write(path.join("team/a.md"), "a changed\n").unwrap();
        fs::write(path.join("team/sub/b.md"), "b changed\n").unwrap();
        fs::write(path.join("other/c.md"), "c changed\n").unwrap();
        fs::write(path.join("team/new.md"), "new\n").unwrap(); // untracked add in subtree
        fs::write(path.join("other/new.md"), "new\n").unwrap(); // untracked add OUTSIDE

        let scoped = publish_set_impl(&path, None, Some("team")).unwrap();
        let mut paths = all_paths(&scoped);
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "team/a.md".to_string(),
                "team/new.md".to_string(),
                "team/sub/b.md".to_string(),
            ],
            "pathspec must restrict the classified list to the team subtree"
        );
        assert!(
            find_file(&scoped, "other/c.md").is_none()
                && find_file(&scoped, "other/new.md").is_none(),
            "no out-of-subtree change may appear in the scoped list"
        );
    }

    #[test]
    fn changed_path_count_counts_add_modify_delete_and_untracked() {
        // Baseline: a.md + b.md tracked. Then: modify a.md, delete b.md, add an
        // untracked c.md → 3 changed paths (A/M/D incl. untracked), name-only.
        let (_dir, path) = repo_with_main(&[("a.md", "one\n"), ("b.md", "two\n")]);
        fs::write(path.join("a.md"), "one changed\n").unwrap();
        fs::remove_file(path.join("b.md")).unwrap();
        fs::write(path.join("c.md"), "three\n").unwrap();

        let count = changed_path_count_impl(&path, None).unwrap();
        assert_eq!(count, 3, "modify + delete + untracked-add = 3");
    }

    #[test]
    fn changed_path_count_pathspec_restricts_to_subtree() {
        // Changes in two subtrees; the pathspec restricts the count to one.
        let (_dir, path) = repo_with_main(&[("team/a.md", "a\n"), ("other/b.md", "b\n")]);
        fs::write(path.join("team/a.md"), "a changed\n").unwrap();
        fs::write(path.join("other/b.md"), "b changed\n").unwrap();
        fs::write(path.join("team/c.md"), "new\n").unwrap();

        let all = changed_path_count_impl(&path, None).unwrap();
        assert_eq!(all, 3, "unscoped sees both subtrees");

        let scoped = changed_path_count_impl(&path, Some("team")).unwrap();
        assert_eq!(scoped, 2, "pathspec restricts to the team subtree");
    }

    #[test]
    fn changed_path_count_does_no_eol_or_rename_work() {
        // A CRLF-only change on a small file WOULD earn an EolOnly badge in the
        // full set, and a rename WOULD pair in the full set. The cheap count does
        // neither: it just counts deltas — a rename reads as delete + add (2),
        // an EOL-only change reads as one changed path (no inflation attempted).
        let (_dir, path) = repo_with_main(&[("doc.md", "alpha\nbeta\n"), ("foo.md", "keep\n")]);
        // EOL-only change on doc.md.
        fs::write(path.join("doc.md"), "alpha\r\nbeta\r\n").unwrap();
        // Rename foo.md → bar.md (content preserved).
        fs::remove_file(path.join("foo.md")).unwrap();
        fs::write(path.join("bar.md"), "keep\n").unwrap();

        // doc.md (1) + foo.md delete (1) + bar.md add (1) = 3 (rename NOT paired).
        let count = changed_path_count_impl(&path, None).unwrap();
        assert_eq!(
            count, 3,
            "no rename pairing → delete + add counted separately"
        );

        // Sanity: the FULL set pairs the rename (2 → 1) so its count differs.
        let set = publish_set_impl(&path, None, None).unwrap();
        assert!(find_file(&set, "bar.md").unwrap().rename_old.is_some());
    }

    #[test]
    fn serializes_to_ts_view_model_shape() {
        let set = PublishSet {
            buckets: vec![PublishBucket {
                workspace_ref: WorkspaceRef {
                    id: "ws1".into(),
                    label: "story-x".into(),
                    kind: PublishBucketKind::Story,
                },
                count: 1,
                files: vec![PublishFile {
                    path: "a.md".into(),
                    status: PublishFileStatus::Modified,
                    badges: vec![PublishFileBadge::EolOnly, PublishFileBadge::Rename],
                    rename_old: Some("old/a.md".into()),
                    added: None,
                    removed: None,
                }],
            }],
            general: GeneralBucket {
                count: 1,
                files: vec![PublishFile {
                    path: "b.md".into(),
                    status: PublishFileStatus::Added,
                    badges: vec![],
                    rename_old: None,
                    added: None,
                    removed: None,
                }],
            },
            baseline_reason: None,
        };
        let json = serde_json::to_value(&set).unwrap();
        // camelCase keys + kebab-case badges + omitted empty badges (TS contract).
        assert_eq!(json["buckets"][0]["workspaceRef"]["kind"], "story");
        assert_eq!(json["buckets"][0]["files"][0]["status"], "modified");
        assert_eq!(json["buckets"][0]["files"][0]["badges"][0], "eol-only");
        assert_eq!(json["buckets"][0]["files"][0]["badges"][1], "rename");
        // SW-GS-14: the pre-rename path serializes as `renamedFrom` for a rename…
        assert_eq!(json["buckets"][0]["files"][0]["renamedFrom"], "old/a.md");
        assert_eq!(json["general"]["files"][0]["status"], "added");
        assert_eq!(json["general"]["files"][0].get("badges"), None);
        // …and is omitted for a non-rename (skip_serializing_if).
        assert_eq!(json["general"]["files"][0].get("renamedFrom"), None);
        // SLW-DV-6: absent line counts are omitted, so publish / PR-diff payloads
        // stay byte-unchanged (the "0/0 for None" fields never appear).
        assert!(json["buckets"][0]["files"][0].get("added").is_none());
        assert!(json["buckets"][0]["files"][0].get("removed").is_none());
        assert!(json["general"]["files"][0].get("added").is_none());
        assert!(json["general"]["files"][0].get("removed").is_none());
    }

    // --- gs_integrate / gs_submit_resolution (AC-4 / AC-5) ---

    // --- SW-GS-3: file_redline_impl ---------------------------------------

    #[test]
    fn redline_modified_returns_both_sides() {
        let (_dir, path) = repo_with_main(&[("a.md", "# Title\n\noriginal\n")]);
        fs::write(path.join("a.md"), "# Title\n\nedited\n").unwrap();

        let r = file_redline_impl(&path, "a.md", None).unwrap();
        assert_eq!(r.status, PublishFileStatus::Modified);
        assert!(!r.binary);
        assert_eq!(r.before, "# Title\n\noriginal\n");
        assert_eq!(r.after, "# Title\n\nedited\n");
    }

    #[test]
    fn redline_added_has_empty_before() {
        let (_dir, path) = repo_with_main(&[("a.md", "x\n")]);
        fs::write(path.join("b.md"), "brand new\n").unwrap();

        let r = file_redline_impl(&path, "b.md", None).unwrap();
        assert_eq!(r.status, PublishFileStatus::Added);
        assert!(!r.binary);
        assert_eq!(r.before, "");
        assert_eq!(r.after, "brand new\n");
    }

    #[test]
    fn redline_deleted_has_empty_after() {
        let (_dir, path) = repo_with_main(&[("a.md", "gone soon\n")]);
        fs::remove_file(path.join("a.md")).unwrap();

        let r = file_redline_impl(&path, "a.md", None).unwrap();
        assert_eq!(r.status, PublishFileStatus::Deleted);
        assert!(!r.binary);
        assert_eq!(r.before, "gone soon\n");
        assert_eq!(r.after, "");
    }

    #[test]
    fn redline_binary_returns_empty_sides() {
        let (_dir, path) = repo_with_main(&[("a.md", "x\n")]);
        // A new file with an embedded NUL reads as binary.
        fs::write(path.join("img.bin"), [0x89, 0x50, 0x00, 0x4e]).unwrap();

        let r = file_redline_impl(&path, "img.bin", None).unwrap();
        assert_eq!(r.status, PublishFileStatus::Added);
        assert!(r.binary);
        assert_eq!(r.before, "");
        assert_eq!(r.after, "");
    }

    #[test]
    fn baseline_blob_reads_committed_bytes_and_misses_untracked() {
        let (_dir, path) = repo_with_main(&[("img.bin", "BASELINE")]);
        // Modify the tracked file in the working tree — baseline still has the old bytes.
        fs::write(path.join("img.bin"), "WORKDIR").unwrap();
        fs::write(path.join("added.bin"), "NEW").unwrap();

        let committed = read_baseline_blob(&path, "img.bin").unwrap();
        assert_eq!(committed.as_deref(), Some(&b"BASELINE"[..]));
        // An added (untracked) file has no baseline → None (no "before" side).
        assert_eq!(read_baseline_blob(&path, "added.bin").unwrap(), None);
        // Traversal rejected.
        assert!(read_baseline_blob(&path, "../secret").is_err());
    }

    #[test]
    fn redline_rejects_path_traversal() {
        let (_dir, path) = repo_with_main(&[("a.md", "x\n")]);
        for bad in ["../secret", "../../etc/passwd", "/etc/passwd", "a/../../b"] {
            assert!(
                file_redline_impl(&path, bad, None).is_err(),
                "expected {bad} to be rejected"
            );
            // SW-GS-14: the pre-rename path is guarded too.
            assert!(
                file_redline_impl(&path, "a.md", Some(bad)).is_err(),
                "expected old_path {bad} to be rejected"
            );
        }
        // A normal nested repo-relative path is still accepted.
        assert!(file_redline_impl(&path, "a.md", None).is_ok());
    }

    #[test]
    fn redline_rename_diffs_old_baseline_against_new_workdir() {
        // SW-GS-14: a moved doc must redline its REAL delta (old-path baseline
        // vs new-path working tree), not read as all-added.
        let (_dir, path) = repo_with_main(&[("old.md", "# Title\n\noriginal\n")]);
        fs::remove_file(path.join("old.md")).unwrap();
        fs::write(path.join("new.md"), "# Title\n\nedited\n").unwrap();

        // With the old path supplied, "before" = old-path baseline blob.
        let r = file_redline_impl(&path, "new.md", Some("old.md")).unwrap();
        assert_eq!(r.status, PublishFileStatus::Modified);
        assert!(!r.binary);
        assert_eq!(r.before, "# Title\n\noriginal\n");
        assert_eq!(r.after, "# Title\n\nedited\n");

        // A pure move (no edit) → identical sides → zero diff downstream.
        fs::write(path.join("new.md"), "# Title\n\noriginal\n").unwrap();
        let pure = file_redline_impl(&path, "new.md", Some("old.md")).unwrap();
        assert_eq!(pure.before, pure.after);

        // A live tab can resolve its rename without relying on the selected nav.
        let blind = file_redline_impl(&path, "new.md", None).unwrap();
        assert_eq!(blind.status, PublishFileStatus::Modified);
        assert_eq!(blind.before, "# Title\n\noriginal\n");

        // An empty old path is treated as "no rename" — degrades to all-added,
        // never an error or a bogus baseline lookup (boundary defense).
        let empty = file_redline_impl(&path, "new.md", Some("")).unwrap();
        assert_eq!(empty.status, PublishFileStatus::Added);
        assert_eq!(empty.before, "");
    }

    #[test]
    fn redline_resolves_private_rename_without_nav_metadata_and_follows_commit() {
        use crate::context_repo::scratch_space;
        let home = tempfile::tempdir().unwrap();
        let principal = crate::domain::Principal::from(42u64);
        let (space, _) =
            scratch_space::ensure_org_space(home.path(), &principal, "acme/plans").unwrap();
        let original = "# Private plan\n\nKeep this document private.\nFirst step.\nSecond step.\n";
        let edited = original.replace("Second step.", "Changed second step.");
        fs::write(space.content_dir().join("old.md"), original).unwrap();
        scratch_space::ensure_content_baseline(home.path(), &principal, "acme/plans").unwrap();
        fs::rename(
            space.content_dir().join("old.md"),
            space.content_dir().join("new.md"),
        )
        .unwrap();
        fs::write(space.content_dir().join("new.md"), &edited).unwrap();
        let root = scratch_space::scratch_root(home.path(), &principal);
        let rel = "orgs/acme/plans/content/new.md";
        let redline = file_redline_impl(&root, rel, None).unwrap();
        assert_eq!(redline.before, original);
        assert_eq!(redline.after, edited);
        let repo = git2::Repository::open(&root).unwrap();
        let mut index = repo.index().unwrap();
        index
            .remove_path(Path::new("orgs/acme/plans/content/old.md"))
            .unwrap();
        index.add_path(Path::new(rel)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let signature = git2::Signature::now("Test", "test@example.com").unwrap();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Rename private plan",
            &tree,
            &[&parent],
        )
        .unwrap();
        let committed = file_redline_impl(&root, rel, None).unwrap();
        assert_eq!(committed.before, edited);
        assert_eq!(committed.before, committed.after);
    }

    #[test]
    fn redline_unborn_baseline_reads_as_added() {
        // With no origin/main, no main, and an unborn HEAD, resolve_baseline_tree
        // returns None → the "before" side is empty and the file reads as added.
        let (_dir, path) = repo_with_main(&[("a.md", "content\n")]);
        let repo = git2::Repository::open(&path).unwrap();
        // Point HEAD at an unborn orphan branch so the baseline refs can be
        // removed and resolve_baseline_tree finds nothing.
        repo.set_head("refs/heads/orphan").unwrap();
        repo.find_reference("refs/remotes/origin/main")
            .unwrap()
            .delete()
            .unwrap();
        repo.find_reference("refs/heads/main")
            .unwrap()
            .delete()
            .unwrap();

        let r = file_redline_impl(&path, "a.md", None).unwrap();
        assert_eq!(r.status, PublishFileStatus::Added);
        assert_eq!(r.before, "");
        assert_eq!(r.after, "content\n");
    }

    // --- SW-GS-6: out-of-sync detection ------------------------------------

    /// Move `origin/main` one commit ahead — building on the CURRENT origin tip
    /// (a chain) so repeated calls move origin strictly further ahead of local,
    /// while local `main` stays put. Simulates teammates' unpulled pushes.
    fn advance_origin(path: &Path) -> git2::Oid {
        let repo = git2::Repository::open(path).unwrap();
        let tip = repo
            .find_reference("refs/remotes/origin/main")
            .unwrap()
            .target()
            .unwrap();
        let parent = repo.find_commit(tip).unwrap();
        let tree = parent.tree().unwrap();
        let sig = repo.signature().unwrap();
        // NO ref update on the commit itself (local `main` doesn't move); only
        // the remote-tracking ref advances.
        let child = repo
            .commit(None, &sig, &sig, "remote work", &tree, &[&parent])
            .unwrap();
        repo.reference("refs/remotes/origin/main", child, true, "advance")
            .unwrap();
        child
    }

    #[test]
    fn incoming_status_up_to_date() {
        let (_dir, path) = repo_with_main(&[("a.md", "hi\n")]);
        assert_eq!(incoming_status_impl(&path).unwrap().incoming, 0);
    }

    #[test]
    fn incoming_status_behind_counts_commits() {
        let (_dir, path) = repo_with_main(&[("a.md", "hi\n")]);
        advance_origin(&path);
        advance_origin(&path);
        // origin/main is now 2 commits ahead of local.
        assert_eq!(incoming_status_impl(&path).unwrap().incoming, 2);
    }

    #[test]
    fn incoming_status_no_origin_ref_is_zero() {
        let (_dir, path) = repo_with_main(&[("a.md", "hi\n")]);
        let repo = git2::Repository::open(&path).unwrap();
        repo.find_reference("refs/remotes/origin/main")
            .unwrap()
            .delete()
            .unwrap();
        // No fetch; missing origin ⇒ count 0 with the reason, not an error.
        let status = incoming_status_impl(&path).unwrap();
        assert_eq!(status.incoming, 0);
        assert_eq!(
            status.default_branch_reason.as_deref(),
            Some(NO_ORIGIN_DEFAULT)
        );
        // A private repo (no origin by design) is told no fetch or set-head reason.
        repo.config()
            .unwrap()
            .set_bool("codesense.private", true)
            .unwrap();
        assert_eq!(
            incoming_status_impl(&path).unwrap().default_branch_reason,
            None
        );
    }

    #[test]
    fn incoming_status_is_read_only() {
        let (_dir, path) = repo_with_main(&[("a.md", "hi\n")]);
        advance_origin(&path);
        let repo = git2::Repository::open(&path).unwrap();
        let head_before = repo.head().unwrap().target().unwrap();
        let file_before = fs::read_to_string(path.join("a.md")).unwrap();

        assert_eq!(incoming_status_impl(&path).unwrap().incoming, 1);

        assert_eq!(
            head_before,
            repo.head().unwrap().target().unwrap(),
            "local HEAD must not move"
        );
        assert_eq!(
            file_before,
            fs::read_to_string(path.join("a.md")).unwrap(),
            "working tree must be untouched"
        );
    }

    // --- SW-GS-31: canFastForward probe field --------------------------------

    #[test]
    fn can_fast_forward_true_when_behind_clean_not_diverged() {
        // origin/main one ahead of local, clean tree, on `main` → a pure FF is
        // possible (the auto-sync-down gate fires).
        let (_dir, path) = repo_with_main(&[("a.md", "hi\n")]);
        advance_origin(&path);
        let status = incoming_status_impl(&path).unwrap();
        assert_eq!(status.incoming, 1);
        assert!(
            status.can_fast_forward,
            "behind + clean + not-diverged ⇒ true"
        );
    }

    #[test]
    fn can_fast_forward_false_when_up_to_date() {
        // local == origin ⇒ nothing to fast-forward.
        let (_dir, path) = repo_with_main(&[("a.md", "hi\n")]);
        let status = incoming_status_impl(&path).unwrap();
        assert_eq!(status.incoming, 0);
        assert!(!status.can_fast_forward, "up-to-date ⇒ false");
    }

    #[test]
    fn can_fast_forward_false_when_tree_is_dirty() {
        // Behind, but an uncommitted edit makes the tree dirty → NOT a pure FF
        // (the manual 3-way merge owns this case; the auto path must skip it).
        let (_dir, path) = repo_with_main(&[("a.md", "hi\n")]);
        advance_origin(&path);
        fs::write(path.join("a.md"), "my wip\n").unwrap();
        let status = incoming_status_impl(&path).unwrap();
        assert_eq!(status.incoming, 1, "still behind");
        assert!(!status.can_fast_forward, "dirty tree ⇒ false");
    }

    #[test]
    fn can_fast_forward_false_when_untracked_file_makes_tree_dirty() {
        // An untracked file also makes the tree dirty (mirrors working_tree_dirty's
        // include_untracked) → auto path must skip.
        let (_dir, path) = repo_with_main(&[("a.md", "hi\n")]);
        advance_origin(&path);
        fs::write(path.join("scratch.txt"), "notes\n").unwrap();
        assert!(!incoming_status_impl(&path).unwrap().can_fast_forward);
    }

    #[test]
    fn can_fast_forward_false_when_diverged() {
        // A local-only commit diverges from origin (origin is no longer a strict
        // descendant of local) → NeedsResolve territory, never an auto FF.
        let (_dir, path) = repo_with_main(&[("a.md", "hi\n")]);
        commit_local(&path, &[("local-only.md", "mine\n")]);
        advance_origin_main(&path, &[("origin-only.md", "theirs\n")]);
        assert!(!incoming_status_impl(&path).unwrap().can_fast_forward);
    }

    #[test]
    fn can_fast_forward_false_when_no_origin_ref() {
        // No origin/main on disk → false (degrades quietly, like the badge count).
        let (_dir, path) = repo_with_main(&[("a.md", "hi\n")]);
        let repo = git2::Repository::open(&path).unwrap();
        repo.find_reference("refs/remotes/origin/main")
            .unwrap()
            .delete()
            .unwrap();
        assert!(!incoming_status_impl(&path).unwrap().can_fast_forward);
    }

    // -- SW-GS-5: pull_ff (fetch skipped in unit tests via token=None) ---------

    /// Add a commit on top of the current `origin/main` tip with the given
    /// top-level files, updating ONLY `refs/remotes/origin/main` (as a fetch
    /// would). HEAD and the working tree are left untouched. Returns the new oid.
    fn advance_origin_main(path: &Path, files: &[(&str, &str)]) -> git2::Oid {
        let repo = git2::Repository::open(path).unwrap();
        let parent_oid = repo
            .find_reference("refs/remotes/origin/main")
            .unwrap()
            .target()
            .unwrap();
        let parent = repo.find_commit(parent_oid).unwrap();
        let mut builder = repo.treebuilder(Some(&parent.tree().unwrap())).unwrap();
        for (rel, content) in files {
            let blob = repo.blob(content.as_bytes()).unwrap();
            builder.insert(rel, blob, 0o100644).unwrap();
        }
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        // Dangling commit (ref = None), then point origin/main at it.
        let oid = repo
            .commit(None, &sig, &sig, "incoming", &tree, &[&parent])
            .unwrap();
        repo.reference("refs/remotes/origin/main", oid, true, "advance origin")
            .unwrap();
        oid
    }

    #[test]
    fn no_working_tree_checkout_on_gs_path() {
        // AC-7 guard: the in-memory gs pipeline (snapshot → integrate → submit →
        // publish → absorb) must never call a working-tree-mutating checkout. Scan
        // this module's source (comments stripped), skipping the ONE sanctioned
        // clean-fast-forward function `pull_ff_impl` (SW-GS-5) — the clean-ff pull
        // button that DOES check the working tree out and is explicitly NOT on the
        // in-memory path. Needles are split so this assertion can't match itself.
        let src = include_str!("git_sync.rs");
        let needles = [
            concat!("checkout_", "head("),
            concat!("checkout_", "tree("),
            concat!("try_fast_", "forward("),
        ];
        // Track whether we're inside the allowlisted pull_ff_impl body (top-level
        // fn: opens at `fn pull_ff_impl`, closes at the next `}` in column 0).
        let mut in_pull_ff = false;
        for line in src.lines() {
            if line.starts_with("pub fn pull_ff_impl") || line.starts_with("fn pull_ff_impl") {
                in_pull_ff = true;
            } else if in_pull_ff && line == "}" {
                in_pull_ff = false;
                continue;
            }
            if in_pull_ff {
                continue;
            }
            let code = line.split("//").next().unwrap_or(line);
            for needle in needles {
                assert!(
                    !code.contains(needle),
                    "working-tree checkout on the gs path: {line}"
                );
            }
        }
    }

    // --- SW-GS-13: commit-sourced diff + sync history --------------------------

    /// Add another commit on `main` (advancing BOTH `main` and the `origin/main`
    /// publish ref to it). `Some(content)` writes/replaces a path, `None` deletes
    /// it. Nested paths are fine (index-based, unlike the flat `commit_pr_head`).
    fn commit_more(path: &Path, message: &str, changes: &[(&str, Option<&str>)]) -> git2::Oid {
        let repo = git2::Repository::open(path).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let mut index = repo.index().unwrap();
        for (rel, content) in changes {
            let full = path.join(rel);
            match content {
                Some(c) => {
                    if let Some(dir) = full.parent() {
                        fs::create_dir_all(dir).unwrap();
                    }
                    fs::write(&full, c).unwrap();
                    index.add_path(Path::new(rel)).unwrap();
                }
                None => {
                    fs::remove_file(&full).ok();
                    index.remove_path(Path::new(rel)).unwrap();
                }
            }
        }
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        let oid = repo
            .commit(
                Some("refs/heads/main"),
                &sig,
                &sig,
                message,
                &tree,
                &[&parent],
            )
            .unwrap();
        repo.reference("refs/remotes/origin/main", oid, true, "origin/main")
            .unwrap();
        oid
    }

    #[test]
    fn commit_publish_set_diffs_against_first_parent() {
        let (_dir, path) = repo_with_main(&[("docs/a.md", "hello\n"), ("keep.md", "k\n")]);
        let c = commit_more(
            &path,
            "edit",
            &[
                ("docs/a.md", Some("hello there\n")), // modified
                ("docs/new.md", Some("new\n")),       // added
                ("keep.md", None),                    // deleted
            ],
        );
        let set = commit_publish_set_impl(&path, &c.to_string()).unwrap();
        // Paths are org-root-relative (nested path preserved) — the SAME shape the
        // working-tree/PR sets emit, so SW-GS-7's team-scope filter applies (D7).
        assert_eq!(
            find_file(&set, "docs/a.md").unwrap().status,
            PublishFileStatus::Modified
        );
        assert_eq!(
            find_file(&set, "docs/new.md").unwrap().status,
            PublishFileStatus::Added
        );
        assert_eq!(
            find_file(&set, "keep.md").unwrap().status,
            PublishFileStatus::Deleted
        );
    }

    #[test]
    fn immutable_commit_source_is_unscoped_by_path_prefix() {
        // The `path_prefix` pathspec is a WORKING-TREE-only scope: it threads
        // through `publish_set_impl`/`changeset_records`, NOT through the
        // immutable commit/PR sources. `commit_publish_set_impl` has no prefix
        // param at all, so a landed commit's set spans every subtree regardless
        // of the active team — pinning the working-tree-only contract (the FE
        // still applies its client-side team filter to a commit/PR review view).
        let (_dir, path) = repo_with_main(&[("team/a.md", "a\n"), ("other/b.md", "b\n")]);
        let c = commit_more(
            &path,
            "edit both subtrees",
            &[
                ("team/a.md", Some("a changed\n")),
                ("other/b.md", Some("b changed\n")),
            ],
        );
        let set = commit_publish_set_impl(&path, &c.to_string()).unwrap();
        // Both subtrees present — the commit source cannot be pathspec-scoped.
        assert!(
            find_file(&set, "team/a.md").is_some() && find_file(&set, "other/b.md").is_some(),
            "commit source must be unscoped (no path_prefix), got {:?}",
            all_paths(&set)
        );
    }

    #[test]
    fn commit_publish_set_root_commit_diffs_empty_tree() {
        // The init commit is a root (no parent) → everything reads as Added.
        let (_dir, path) = repo_with_main(&[("a.md", "1\n"), ("b.md", "2\n")]);
        let repo = git2::Repository::open(&path).unwrap();
        let root = repo.head().unwrap().peel_to_commit().unwrap().id();
        let (before, _after) = commit_trees(&repo, &root.to_string()).unwrap();
        assert!(before.is_none(), "root commit has no first-parent tree");
        let set = commit_publish_set_impl(&path, &root.to_string()).unwrap();
        assert_eq!(
            find_file(&set, "a.md").unwrap().status,
            PublishFileStatus::Added
        );
        assert_eq!(
            find_file(&set, "b.md").unwrap().status,
            PublishFileStatus::Added
        );
    }

    #[test]
    fn commit_trees_merge_uses_first_parent() {
        let (_dir, path) = repo_with_main(&[("base.md", "base\n")]);
        let repo = git2::Repository::open(&path).unwrap();
        let base = repo.head().unwrap().peel_to_commit().unwrap();
        let base_tree = base.tree().unwrap();
        let sig = repo.signature().unwrap();

        let make_child = |extra: &str, content: &[u8]| -> git2::Commit {
            let mut b = repo.treebuilder(Some(&base_tree)).unwrap();
            let blob = repo.blob(content).unwrap();
            b.insert(extra, blob, 0o100644).unwrap();
            let tree = repo.find_tree(b.write().unwrap()).unwrap();
            let oid = repo
                .commit(None, &sig, &sig, extra, &tree, &[&base])
                .unwrap();
            repo.find_commit(oid).unwrap()
        };
        let p1 = make_child("one.md", b"one\n");
        let p2 = make_child("two.md", b"two\n");

        // Merge tree = union of both children.
        let mut mb = repo.treebuilder(Some(&base_tree)).unwrap();
        let b1 = repo.blob(b"one\n").unwrap();
        let b2 = repo.blob(b"two\n").unwrap();
        mb.insert("one.md", b1, 0o100644).unwrap();
        mb.insert("two.md", b2, 0o100644).unwrap();
        let merge_tree = repo.find_tree(mb.write().unwrap()).unwrap();
        let merge = repo
            .commit(None, &sig, &sig, "merge", &merge_tree, &[&p1, &p2])
            .unwrap();

        // Diffing vs the FIRST parent (p1, which already has one.md) shows only
        // two.md as added — never crashing on the merge.
        let set = commit_publish_set_impl(&path, &merge.to_string()).unwrap();
        assert!(
            find_file(&set, "two.md").is_some(),
            "two.md added vs first parent"
        );
        assert!(
            find_file(&set, "one.md").is_none(),
            "one.md already present in the first parent"
        );
    }

    #[test]
    fn commit_file_redline_reads_both_sides_from_commit_and_parent() {
        let (_dir, path) = repo_with_main(&[("a.md", "hello\n")]);
        let c = commit_more(&path, "edit", &[("a.md", Some("hello there\n"))]);
        let r = commit_file_redline_impl(&path, &c.to_string(), "a.md", None).unwrap();
        assert_eq!(r.status, PublishFileStatus::Modified);
        assert_eq!(r.before, "hello\n");
        assert_eq!(r.after, "hello there\n");
        assert!(!r.binary);
    }

    #[test]
    fn commit_file_redline_added_file_has_empty_before() {
        let (_dir, path) = repo_with_main(&[("a.md", "hello\n")]);
        let c = commit_more(&path, "add", &[("new.md", Some("fresh\n"))]);
        let r = commit_file_redline_impl(&path, &c.to_string(), "new.md", None).unwrap();
        assert_eq!(r.status, PublishFileStatus::Added);
        assert_eq!(r.before, "");
        assert_eq!(r.after, "fresh\n");
    }

    #[test]
    fn commit_trees_bad_sha_is_error() {
        let (_dir, path) = repo_with_main(&[("a.md", "1\n")]);
        let repo = git2::Repository::open(&path).unwrap();
        assert!(commit_trees(&repo, "not-an-oid").is_err());
    }

    // --- SLW-WD-1: source worktree change-set + per-file redline ---------------
    //
    // A source worktree is modeled as a feature branch (its own commits) off
    // `main`, with `origin/main` movable independently to prove the baseline is
    // the merge-base (fork point), not the base-branch tip. `repo_with_main`
    // seeds `main` == `origin/main` at the init commit with HEAD on `main`.

    /// Branch `name` off the current HEAD and move HEAD onto it. The working tree
    /// is unchanged (same tree), so no checkout is needed.
    fn checkout_new_branch(path: &Path, name: &str) {
        let repo = git2::Repository::open(path).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch(name, &head, false).unwrap();
        repo.set_head(&format!("refs/heads/{name}")).unwrap();
    }

    /// Commit `changes` onto the CURRENT HEAD branch (advancing it) and write the
    /// files to the working tree + index. Returns the new commit oid.
    fn commit_on_head(path: &Path, message: &str, changes: &[(&str, &str)]) -> git2::Oid {
        let repo = git2::Repository::open(path).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let refname = repo.head().unwrap().name().unwrap().to_string();
        let mut index = repo.index().unwrap();
        for (rel, content) in changes {
            let full = path.join(rel);
            if let Some(dir) = full.parent() {
                fs::create_dir_all(dir).unwrap();
            }
            fs::write(&full, content).unwrap();
            index.add_path(Path::new(rel)).unwrap();
        }
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some(&refname), &sig, &sig, message, &tree, &[&parent])
            .unwrap()
    }

    // `advance_origin_main(path, files)` (defined above in the SW-GS-5 section)
    // advances ONLY `refs/remotes/origin/main`, leaving HEAD + working tree
    // untouched — exactly the "upstream moved after we branched" shape WD-1 needs.

    fn find_record<'a>(records: &'a [PublishFile], path: &str) -> Option<&'a PublishFile> {
        records.iter().find(|r| r.path == path)
    }

    /// (Task 1 — AC-1/AC-2/AC-5) A committed edit is `Modified` and an untracked
    /// new file is `Added`, in a flat list; unchanged files are absent.
    #[test]
    fn source_changeset_includes_committed_edit_and_untracked_added() {
        let (_dir, path) = repo_with_main(&[("keep.md", "k\n"), ("edit.md", "old\n")]);
        checkout_new_branch(&path, "feature");
        commit_on_head(&path, "edit", &[("edit.md", "new\n")]);
        fs::write(path.join("untracked.md"), "fresh\n").unwrap();

        let records = source_worktree_changeset_impl(&path, None).unwrap();

        assert_eq!(
            find_record(&records, "edit.md").unwrap().status,
            PublishFileStatus::Modified,
            "committed feature edit is Modified vs merge-base"
        );
        assert_eq!(
            find_record(&records, "untracked.md").unwrap().status,
            PublishFileStatus::Added,
            "untracked new file is Added (AC-2)"
        );
        assert!(
            find_record(&records, "keep.md").is_none(),
            "unchanged file is absent from the change-set"
        );
    }

    /// SLW-DV-6 (AC-2) The change-set carries per-file +/− line counts from git's
    /// own line-stats: a modify reports both sides, a pure add reports 0 removed,
    /// and a binary file reports `None` (not `Some(0)`) so the rail can tell
    /// "no line diff" from "not applicable".
    #[test]
    fn source_changeset_reports_per_file_line_counts() {
        let (_dir, path) = repo_with_main(&[
            ("edit.md", "old\n"),
            ("del.md", "gone\nsoon\n"),
            ("eol.md", "a\nb\n"),
        ]);
        checkout_new_branch(&path, "feature");
        // one line replaced by two → +2 / −1.
        commit_on_head(&path, "edit", &[("edit.md", "one\ntwo\n")]);
        // untracked add of two lines → +2 / −0.
        fs::write(path.join("added.md"), "a\nb\n").unwrap();
        // untracked binary (embedded NUL) → no textual patch → None / None.
        fs::write(path.join("blob.bin"), [0u8, 1, 2, 3, 0, 9]).unwrap();
        // a deleted tracked file → +0 / −2.
        fs::remove_file(path.join("del.md")).unwrap();
        // a CRLF-only rewrite → EOL-only badge; still gets real counts. This is
        // the regression guard for the `is_eol_only` / `delta_line_stats`
        // ordering hazard: the badge must survive the line-stat side effect.
        fs::write(path.join("eol.md"), "a\r\nb\r\n").unwrap();

        let records = source_worktree_changeset_impl(&path, None).unwrap();

        let edit = find_record(&records, "edit.md").unwrap();
        assert_eq!(edit.added, Some(2), "edit.md added lines");
        assert_eq!(edit.removed, Some(1), "edit.md removed lines");

        let added = find_record(&records, "added.md").unwrap();
        assert_eq!(added.added, Some(2), "added.md added lines");
        assert_eq!(added.removed, Some(0), "a pure add removes nothing");

        let blob = find_record(&records, "blob.bin").unwrap();
        assert_eq!(blob.added, None, "binary file has no line count");
        assert_eq!(blob.removed, None, "binary file has no line count");

        let del = find_record(&records, "del.md").unwrap();
        assert_eq!(del.added, Some(0), "a delete adds nothing");
        assert_eq!(del.removed, Some(2), "del.md removed lines");

        let eol = find_record(&records, "eol.md").unwrap();
        assert!(
            eol.badges.contains(&PublishFileBadge::EolOnly),
            "EOL-only badge must survive line-stat computation (ordering intact)"
        );
        assert!(
            eol.added.is_some() && eol.removed.is_some(),
            "EOL-only delta still reports counts"
        );
    }

    /// (Task 2 — AC-3/RD-2) Upstream movement on `origin/main` AFTER the branch
    /// point does not leak into the change-set: the baseline is the merge-base,
    /// not the base-branch tip.
    #[test]
    fn source_changeset_baseline_is_merge_base_not_origin_tip() {
        let (_dir, path) = repo_with_main(&[("edit.md", "old\n")]);
        checkout_new_branch(&path, "feature");
        commit_on_head(&path, "edit", &[("edit.md", "new\n")]);
        advance_origin_main(&path, &[("upstream.md", "from main\n")]);

        let records = source_worktree_changeset_impl(&path, None).unwrap();

        assert!(
            find_record(&records, "upstream.md").is_none(),
            "post-branch upstream commit must NOT appear (merge-base baseline)"
        );
        assert_eq!(
            find_record(&records, "edit.md").unwrap().status,
            PublishFileStatus::Modified
        );
    }

    /// (Task 2b — AC-3/RD-7) A stacked worktree passes its fork-parent branch as
    /// `base_ref` → the parent's commits are excluded; `None` folds them in
    /// (proving `base_ref` actually re-baselines).
    #[test]
    fn source_changeset_stacked_base_ref_excludes_parent_commits() {
        let (_dir, path) = repo_with_main(&[("base.md", "b\n")]);
        checkout_new_branch(&path, "parent");
        commit_on_head(&path, "parent work", &[("parent.md", "from parent\n")]);
        checkout_new_branch(&path, "child");
        commit_on_head(&path, "child work", &[("child.md", "from child\n")]);

        let stacked = source_worktree_changeset_impl(&path, Some("parent")).unwrap();
        assert!(
            find_record(&stacked, "child.md").is_some(),
            "own work is present"
        );
        assert!(
            find_record(&stacked, "parent.md").is_none(),
            "parent's commit excluded via base_ref merge-base"
        );

        let default = source_worktree_changeset_impl(&path, None).unwrap();
        assert!(
            find_record(&default, "parent.md").is_some(),
            "without base_ref, the parent's commit folds in (origin/main merge-base)"
        );
    }

    /// (Task 3 — AC-4) Per-file redline reads before/after against the merge-base;
    /// binary files return `binary: true` + empty content.
    #[test]
    fn source_file_redline_reads_before_after_against_merge_base() {
        let (_dir, path) = repo_with_main(&[("edit.md", "old\n")]);
        checkout_new_branch(&path, "feature");
        commit_on_head(&path, "edit", &[("edit.md", "new\n")]);
        advance_origin_main(&path, &[("upstream.md", "x\n")]);

        let r = source_file_redline_impl(&path, "edit.md", None, None).unwrap();
        assert_eq!(r.status, PublishFileStatus::Modified);
        assert_eq!(
            r.before, "old\n",
            "before reads from merge-base, not origin tip"
        );
        assert_eq!(r.after, "new\n");
        assert!(!r.binary);
    }

    /// (Task 3 + RD-7) The per-file redline honors `base_ref` too, so the redline
    /// and the change-set always agree on the baseline for a STACKED worktree. A
    /// file the parent already committed reads as unchanged content against the
    /// parent baseline (before == after), NOT as all-added.
    #[test]
    fn source_file_redline_respects_stacked_base_ref() {
        let (_dir, path) = repo_with_main(&[("base.md", "b\n")]);
        checkout_new_branch(&path, "parent");
        commit_on_head(&path, "parent work", &[("shared.md", "from parent\n")]);
        checkout_new_branch(&path, "child");
        commit_on_head(&path, "child edit", &[("shared.md", "edited by child\n")]);

        // base_ref=parent ⇒ baseline is merge_base(child, parent), which equals the
        // parent tip here since child branched directly off parent (shared.md ==
        // "from parent") — so the child's edit is a genuine Modified with the parent
        // content as before.
        let r = source_file_redline_impl(&path, "shared.md", None, Some("parent")).unwrap();
        assert_eq!(r.status, PublishFileStatus::Modified);
        assert_eq!(
            r.before, "from parent\n",
            "before reads from the base_ref merge-base"
        );
        assert_eq!(r.after, "edited by child\n");
    }

    #[test]
    fn source_file_redline_binary_empties_content() {
        let (_dir, path) = repo_with_main(&[("keep.md", "k\n")]);
        checkout_new_branch(&path, "feature");
        fs::write(path.join("blob.bin"), [0u8, 1, 2, 3]).unwrap();

        let r = source_file_redline_impl(&path, "blob.bin", None, None).unwrap();
        assert_eq!(r.status, PublishFileStatus::Added);
        assert!(r.binary);
        assert_eq!(r.before, "");
        assert_eq!(r.after, "");
    }

    /// (Task 4 — AC-8) Clean worktree → empty change-set (not error); unborn HEAD
    /// falls back gracefully without panic.
    #[test]
    fn source_changeset_clean_worktree_is_empty_not_error() {
        let (_dir, path) = repo_with_main(&[("a.md", "a\n")]);
        checkout_new_branch(&path, "feature");
        let records = source_worktree_changeset_impl(&path, None).unwrap();
        assert!(
            records.is_empty(),
            "clean worktree yields an empty change-set"
        );
    }

    #[test]
    fn source_changeset_unborn_head_falls_back_without_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        git2::Repository::init(&path).unwrap();
        fs::write(path.join("new.md"), "hi\n").unwrap();

        let records = source_worktree_changeset_impl(&path, None).unwrap();
        assert_eq!(
            find_record(&records, "new.md").unwrap().status,
            PublishFileStatus::Added,
            "unborn HEAD ⇒ None baseline ⇒ everything reads as Added"
        );
    }

    /// (Task 5 — AC-6/AC-7) Both commands are read-only: no working-tree writes,
    /// no new commits/refs.
    #[test]
    fn source_commands_are_read_only() {
        fn snapshot_tree(path: &Path) -> Vec<(String, Vec<u8>)> {
            fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
                for entry in fs::read_dir(dir).unwrap() {
                    let p = entry.unwrap().path();
                    if p.file_name().is_some_and(|n| n == ".git") {
                        continue;
                    }
                    if p.is_dir() {
                        walk(base, &p, out);
                    } else {
                        let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                        out.push((rel, fs::read(&p).unwrap()));
                    }
                }
            }
            let mut out = Vec::new();
            walk(path, path, &mut out);
            out.sort();
            out
        }
        fn refs_snapshot(path: &Path) -> (Option<git2::Oid>, Option<git2::Oid>) {
            let repo = git2::Repository::open(path).unwrap();
            let head = repo.head().ok().and_then(|h| h.target());
            let origin = repo
                .find_reference("refs/remotes/origin/main")
                .ok()
                .and_then(|r| r.target());
            (head, origin)
        }

        let (_dir, path) = repo_with_main(&[("edit.md", "old\n"), ("keep.md", "k\n")]);
        checkout_new_branch(&path, "feature");
        commit_on_head(&path, "edit", &[("edit.md", "new\n")]);
        fs::write(path.join("untracked.md"), "fresh\n").unwrap();

        let tree_before = snapshot_tree(&path);
        let refs_before = refs_snapshot(&path);

        source_worktree_changeset_impl(&path, None).unwrap();
        source_file_redline_impl(&path, "edit.md", None, None).unwrap();
        source_file_redline_impl(&path, "untracked.md", None, None).unwrap();

        assert_eq!(
            tree_before,
            snapshot_tree(&path),
            "WD-1 must not write to the working tree"
        );
        assert_eq!(
            refs_before,
            refs_snapshot(&path),
            "WD-1 must not create commits or move refs"
        );
    }

    /// (Task 7 — characterization) When the worktree hasn't diverged from
    /// `origin/main`, the source change-set (merge-base baseline) and the context
    /// publish set (origin/main-tip baseline) see the SAME baseline → identical
    /// per-file records, proving WD-2 can reuse the redline model with no shim.
    #[test]
    fn source_changeset_shape_matches_context_publish_records() {
        let (_dir, path) = repo_with_main(&[("mod.md", "old\n"), ("keep.md", "k\n")]);
        fs::write(path.join("mod.md"), "new\n").unwrap();
        fs::write(path.join("add.md"), "fresh\n").unwrap();

        let mut source = source_worktree_changeset_impl(&path, None).unwrap();
        let ctx = publish_set_impl(&path, None, None).unwrap();

        // Compare the WHOLE `PublishFile` (path, status, badges, rename_old), not
        // just (path, status) — the DoD claims byte-identical *records*, so the
        // badge/rename fields must match too or WD-2 would need a shim.
        let mut ctx_records: Vec<PublishFile> = ctx
            .general
            .files
            .iter()
            .chain(ctx.buckets.iter().flat_map(|b| b.files.iter()))
            .cloned()
            .collect();
        source.sort_by(|a, b| a.path.cmp(&b.path));
        ctx_records.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(
            source, ctx_records,
            "source change-set records must match the context publish records byte-for-byte"
        );
    }

    /// (RD-2 anti-drift) The CONTEXT publish redline must stay on the
    /// `origin/main` **tip** baseline after the WD-1 extract — a regression guard
    /// so a future refactor can't silently collapse the two baselines. Here the
    /// tip and the merge-base DIFFER (upstream moved), and the context redline
    /// must follow the tip.
    #[test]
    fn context_redline_still_baselines_on_origin_tip_after_extract() {
        let (_dir, path) = repo_with_main(&[("shared.md", "v1\n")]);
        // origin/main advances `shared.md` to v2 (and HEAD/main stays at v1 via a
        // detached advance of origin/main only).
        advance_origin_main(&path, &[("shared.md", "v2\n")]);
        // Working tree edits shared.md to v3.
        fs::write(path.join("shared.md"), "v3\n").unwrap();

        // Context redline: before == origin/main TIP (v2), not the merge-base (v1).
        let ctx = file_redline_impl(&path, "shared.md", None).unwrap();
        assert_eq!(
            ctx.before, "v2\n",
            "context redline must baseline on origin/main tip"
        );
        assert_eq!(ctx.after, "v3\n");
    }

    // === SLW-WD-8: commit graph + per-commit diff + baseline fix ===============
    //
    // AC-1 targets repos whose default branch is NOT `main` (e.g. `master`) or an
    // attached repo without a local `origin/main` — the exact case where the old
    // ladder (`base_ref` → `origin/main` → `main`) found nothing and collapsed the
    // baseline to HEAD's own tree, making committed work vanish from the change-set.

    /// Init a repo whose ONLY branch is `default` (e.g. `master`), HEAD on it, with
    /// `origin/{default}` seeded to the init commit — but deliberately NO `main`
    /// and NO `origin/main`, so the old baseline ladder resolves nothing. Returns
    /// (dir, path).
    fn repo_with_default_branch(
        default: &str,
        files: &[(&str, &str)],
    ) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let repo = git2::Repository::init(&path).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "Test").unwrap();
            cfg.set_str("user.email", "test@example.com").unwrap();
        }
        // A real `origin` remote (default `+refs/heads/*:refs/remotes/origin/*`
        // refspec) so `Branch::upstream()` can map a tracking ref back to a remote
        // — the attached-repo shape rung (a) resolves against.
        repo.remote("origin", "https://example.invalid/repo.git")
            .unwrap();
        for (rel, content) in files {
            let full = path.join(rel);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(full, content).unwrap();
        }
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        let refname = format!("refs/heads/{default}");
        let commit_oid = repo
            .commit(Some(&refname), &sig, &sig, "init", &tree, &[])
            .unwrap();
        repo.set_head(&refname).unwrap();
        let head_obj = repo.find_object(commit_oid, None).unwrap();
        repo.reset(&head_obj, git2::ResetType::Hard, None).unwrap();
        repo.reference(
            &format!("refs/remotes/origin/{default}"),
            commit_oid,
            true,
            "seed",
        )
        .unwrap();
        (dir, path)
    }

    /// (Task 1a — AC-1, upstream rung) On a `develop`-default repo (no `origin/HEAD`,
    /// no `origin/main` or `origin/master`, no `main`), only the checked-out feature
    /// branch's `upstream()` (→ `origin/develop`) resolves the baseline, so a COMMITTED
    /// edit shows in the change-set. Without the upstream rung the ladder finds nothing
    /// → HEAD fallback → the committed edit vanishes.
    #[test]
    fn source_changeset_baseline_resolves_via_upstream_on_develop_default() {
        let (_dir, path) = repo_with_default_branch("develop", &[("edit.md", "old\n")]);
        checkout_new_branch(&path, "feature");
        // Upstream rung: feature tracks origin/develop. No origin/HEAD, main or master.
        {
            let repo = git2::Repository::open(&path).unwrap();
            let mut feature = repo
                .find_branch("feature", git2::BranchType::Local)
                .unwrap();
            feature.set_upstream(Some("origin/develop")).unwrap();
        }
        commit_on_head(&path, "edit", &[("edit.md", "new\n")]);

        let records = source_worktree_changeset_impl(&path, None).unwrap();
        assert_eq!(
            find_record(&records, "edit.md").unwrap().status,
            PublishFileStatus::Modified,
            "committed edit must resolve via the upstream() baseline rung, not vanish to HEAD"
        );
    }

    /// (Task 1b — AC-1, rung (b)) With NO upstream on the feature branch, the
    /// `refs/remotes/origin/HEAD` symbolic ref (→ `origin/master`) resolves the
    /// baseline instead, so the committed edit still shows.
    #[test]
    fn source_changeset_baseline_resolves_via_origin_head_symbolic() {
        let (_dir, path) = repo_with_default_branch("master", &[("edit.md", "old\n")]);
        // Rung (b): origin/HEAD → origin/master, and the feature branch has NO
        // upstream configured, so rung (a) can't fire.
        {
            let repo = git2::Repository::open(&path).unwrap();
            repo.reference_symbolic(
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/master",
                true,
                "set-head",
            )
            .unwrap();
        }
        checkout_new_branch(&path, "feature");
        commit_on_head(&path, "edit", &[("edit.md", "new\n")]);

        let records = source_worktree_changeset_impl(&path, None).unwrap();
        assert_eq!(
            find_record(&records, "edit.md").unwrap().status,
            PublishFileStatus::Modified,
            "committed edit must resolve via the origin/HEAD symbolic baseline rung"
        );
    }

    /// Snapshot every ref (name → target OID / symbolic target) plus HEAD, sorted,
    /// so a read-only assertion can prove the WD-8 commands move nothing.
    fn ref_snapshot(path: &Path) -> Vec<(String, String)> {
        let repo = git2::Repository::open(path).unwrap();
        let mut v: Vec<(String, String)> = repo
            .references()
            .unwrap()
            .filter_map(|r| r.ok())
            .filter_map(|r| {
                let name = r.name().ok()?.to_string();
                let target = r
                    .target()
                    .map(|o| o.to_string())
                    .or_else(|| r.symbolic_target().ok().flatten().map(|s| s.to_string()))
                    .unwrap_or_default();
                Some((name, target))
            })
            .collect();
        let head = repo
            .head()
            .ok()
            .and_then(|h| h.target())
            .map(|o| o.to_string())
            .unwrap_or_default();
        v.push(("HEAD".to_string(), head));
        v.sort();
        v
    }

    /// (Task 2 — AC-2/5/8) The graph walks ALL refs (both diverging branches +
    /// their shared base), each entry carrying its `parents` (edges) and ref
    /// decorations (branch tips + the synthetic `HEAD`), newest-first.
    #[test]
    fn source_commit_graph_walks_all_refs_with_parents_and_decorations() {
        let (_dir, path) = repo_with_main(&[("base.md", "b\n")]); // c0 == main == origin/main
        let repo = git2::Repository::open(&path).unwrap();
        let c0 = repo.head().unwrap().peel_to_commit().unwrap();
        let base_tree = c0.tree().unwrap();
        let sig = repo.signature().unwrap();

        // Two branches diverging from c0 (neither an ancestor of the other).
        let mk = |file: &str, content: &[u8], branch: &str| -> git2::Oid {
            let mut b = repo.treebuilder(Some(&base_tree)).unwrap();
            b.insert(file, repo.blob(content).unwrap(), 0o100644)
                .unwrap();
            let tree = repo.find_tree(b.write().unwrap()).unwrap();
            repo.commit(
                Some(&format!("refs/heads/{branch}")),
                &sig,
                &sig,
                branch,
                &tree,
                &[&c0],
            )
            .unwrap()
        };
        let a1 = mk("a.md", b"a\n", "branch-a");
        let b1 = mk("b.md", b"b\n", "branch-b");
        repo.set_head("refs/heads/branch-a").unwrap();

        let graph = source_commit_graph_impl(&path, 50, None).unwrap();
        let shas: Vec<String> = graph.iter().map(|e| e.sha.clone()).collect();
        assert!(shas.contains(&a1.to_string()), "branch-a tip present");
        assert!(shas.contains(&b1.to_string()), "branch-b tip present");
        assert!(shas.contains(&c0.id().to_string()), "shared base present");

        let find = |oid: git2::Oid| {
            graph
                .iter()
                .find(|e| e.sha == oid.to_string())
                .unwrap_or_else(|| panic!("commit {oid} missing from graph"))
        };
        // Parents make the edges: a1/b1 fork from c0; c0 is a root.
        assert_eq!(find(a1).parents, vec![c0.id().to_string()]);
        assert_eq!(find(b1).parents, vec![c0.id().to_string()]);
        assert!(
            find(c0.id()).parents.is_empty(),
            "root commit has no parents"
        );

        // Decorations are a plain projection of refs; HEAD marks the checked-out tip.
        let names = |oid: git2::Oid| -> Vec<String> {
            find(oid).refs.iter().map(|r| r.name.clone()).collect()
        };
        assert!(names(a1).contains(&"branch-a".to_string()));
        assert!(
            names(a1).contains(&"HEAD".to_string()),
            "HEAD decorates the checked-out branch tip (AC-5)"
        );
        assert!(names(b1).contains(&"branch-b".to_string()));
        assert!(
            names(c0.id()).contains(&"origin/main".to_string()),
            "remote-tracking ref decorates the base"
        );
        assert!(
            find(a1).refs.iter().any(|r| r.kind == RefKind::Head),
            "HEAD label carries kind=Head"
        );

        // Newest-first: children precede their parent.
        let pos = |oid: git2::Oid| shas.iter().position(|s| *s == oid.to_string()).unwrap();
        assert!(pos(a1) < pos(c0.id()));
        assert!(pos(b1) < pos(c0.id()));
    }

    /// (Task 2 — AC-2/9) Cursor paging: page 2 resumes AFTER the cursor with no
    /// overlap; a malformed cursor starts at the tip; a parseable-but-UNREACHABLE
    /// cursor yields `Ok(empty)` — never an error (the AC-9 degenerate mode WD-9's
    /// restart-on-nonce relies on).
    #[test]
    fn source_commit_graph_paging_and_cursor_cases() {
        let (_dir, path) = repo_with_main(&[("f.md", "0\n")]);
        let c0 = git2::Repository::open(&path)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id();
        let c1 = commit_on_head(&path, "c1", &[("f.md", "1\n")]);
        let c2 = commit_on_head(&path, "c2", &[("f.md", "2\n")]);
        let c3 = commit_on_head(&path, "c3", &[("f.md", "3\n")]);

        // Page 1: newest 2 → [c3, c2].
        let page1 = source_commit_graph_impl(&path, 2, None).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].sha, c3.to_string());
        assert_eq!(page1[1].sha, c2.to_string());

        // Page 2: cursor = c2 (last of page 1) → resumes AFTER it: [c1, c0].
        let page2 = source_commit_graph_impl(&path, 2, Some(c2.to_string())).unwrap();
        assert_eq!(page2.first().unwrap().sha, c1.to_string());
        assert!(page2.iter().any(|e| e.sha == c0.to_string()));
        assert!(
            page2
                .iter()
                .all(|e| e.sha != c3.to_string() && e.sha != c2.to_string()),
            "page 2 does not re-emit page 1's commits"
        );

        // Malformed cursor → starts at the tip (same as None).
        let malformed = source_commit_graph_impl(&path, 2, Some("not-an-oid".into())).unwrap();
        assert_eq!(malformed[0].sha, c3.to_string());

        // Parseable-but-unreachable cursor → Ok(empty), NOT an error (AC-9).
        let unreachable = "1234567890123456789012345678901234567890".to_string();
        let empty = source_commit_graph_impl(&path, 2, Some(unreachable)).unwrap();
        assert!(
            empty.is_empty(),
            "an unreachable cursor yields Ok(empty), the end-of-window signal WD-9 relies on"
        );
    }

    /// (Task 3 — AC-3/AC-8) A merge commit returns ALL parents (so WD-9 can draw
    /// the join), and a graph entry carries NO `paths` field (the walk never diffs).
    #[test]
    fn source_commit_graph_merge_parents_and_no_paths_field() {
        let (_dir, path) = repo_with_main(&[("base.md", "b\n")]);
        let repo = git2::Repository::open(&path).unwrap();
        let c0 = repo.head().unwrap().peel_to_commit().unwrap();
        let base_tree = c0.tree().unwrap();
        let sig = repo.signature().unwrap();
        let child = |file: &str, content: &[u8]| -> git2::Commit {
            let mut b = repo.treebuilder(Some(&base_tree)).unwrap();
            b.insert(file, repo.blob(content).unwrap(), 0o100644)
                .unwrap();
            let tree = repo.find_tree(b.write().unwrap()).unwrap();
            let oid = repo.commit(None, &sig, &sig, file, &tree, &[&c0]).unwrap();
            repo.find_commit(oid).unwrap()
        };
        let p1 = child("one.md", b"one\n");
        let p2 = child("two.md", b"two\n");
        let mut mb = repo.treebuilder(Some(&base_tree)).unwrap();
        mb.insert("one.md", repo.blob(b"one\n").unwrap(), 0o100644)
            .unwrap();
        mb.insert("two.md", repo.blob(b"two\n").unwrap(), 0o100644)
            .unwrap();
        let merge_tree = repo.find_tree(mb.write().unwrap()).unwrap();
        // Commit with no ref update (main's tip isn't the first parent), then
        // attach a branch ref so the graph walk reaches the merge.
        let merge = repo
            .commit(None, &sig, &sig, "merge", &merge_tree, &[&p1, &p2])
            .unwrap();
        repo.reference("refs/heads/merged", merge, true, "merge branch")
            .unwrap();

        let graph = source_commit_graph_impl(&path, 50, None).unwrap();
        let m = graph.iter().find(|e| e.sha == merge.to_string()).unwrap();
        let mut got = m.parents.clone();
        got.sort();
        let mut want = vec![p1.id().to_string(), p2.id().to_string()];
        want.sort();
        assert_eq!(got, want, "a merge returns ALL parent shas (AC-8)");

        // AC-3 structural: the serialized entry has no `paths` key.
        let json = serde_json::to_value(m).unwrap();
        assert!(
            json.get("paths").is_none(),
            "a graph entry carries NO paths — the walk never diffs (AC-3)"
        );
        assert!(json.get("parents").is_some());
    }

    /// (Task 4 — AC-4) The per-commit change-set is the FLAT `Vec<PublishFile>` for
    /// a commit vs its first parent (root ⇒ vs empty tree); the redline reads
    /// before/after from the commit + first-parent trees.
    #[test]
    fn source_commit_changeset_and_redline_flat_worktree_pointed() {
        let (_dir, path) = repo_with_main(&[("a.md", "old\n"), ("keep.md", "k\n")]);
        let c0 = git2::Repository::open(&path)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id();
        let c = commit_more(
            &path,
            "edit+add",
            &[("a.md", Some("new\n")), ("added.md", Some("x\n"))],
        );

        let records = source_commit_changeset_impl(&path, &c.to_string()).unwrap();
        assert_eq!(
            find_record(&records, "a.md").unwrap().status,
            PublishFileStatus::Modified
        );
        assert_eq!(
            find_record(&records, "added.md").unwrap().status,
            PublishFileStatus::Added
        );
        assert!(
            find_record(&records, "keep.md").is_none(),
            "unchanged file absent from the per-commit change-set"
        );

        // Redline (verbatim reuse of commit_file_redline_impl) reads the tree pair.
        let r = commit_file_redline_impl(&path, &c.to_string(), "a.md", None).unwrap();
        assert_eq!(r.before, "old\n");
        assert_eq!(r.after, "new\n");
        assert!(!r.binary);

        // Root commit ⇒ diff vs the empty tree ⇒ everything Added.
        let root = source_commit_changeset_impl(&path, &c0.to_string()).unwrap();
        assert_eq!(
            find_record(&root, "a.md").unwrap().status,
            PublishFileStatus::Added,
            "root commit diffs against the empty tree"
        );
    }

    /// (Task 4 — AC-4) A binary file in a commit returns `binary: true` + empty
    /// content, as everywhere.
    #[test]
    fn source_commit_file_redline_binary_empties_content() {
        let (_dir, path) = repo_with_main(&[("keep.md", "k\n")]);
        let repo = git2::Repository::open(&path).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let base_tree = parent.tree().unwrap();
        let sig = repo.signature().unwrap();
        let mut b = repo.treebuilder(Some(&base_tree)).unwrap();
        b.insert("blob.bin", repo.blob(&[0u8, 1, 2, 0, 3]).unwrap(), 0o100644)
            .unwrap();
        let tree = repo.find_tree(b.write().unwrap()).unwrap();
        let c = repo
            .commit(
                Some("refs/heads/main"),
                &sig,
                &sig,
                "bin",
                &tree,
                &[&parent],
            )
            .unwrap();

        let r = commit_file_redline_impl(&path, &c.to_string(), "blob.bin", None).unwrap();
        assert!(r.binary);
        assert_eq!(r.before, "");
        assert_eq!(r.after, "");
    }

    /// (Task 5 — AC-6/7) All three WD-8 reads are read-only: no ref moves, no new
    /// commits, HEAD unchanged.
    #[test]
    fn source_commit_commands_are_read_only() {
        let (_dir, path) = repo_with_main(&[("a.md", "old\n")]);
        let c = commit_more(&path, "edit", &[("a.md", Some("new\n"))]);

        let before = ref_snapshot(&path);
        source_commit_graph_impl(&path, 50, None).unwrap();
        source_commit_changeset_impl(&path, &c.to_string()).unwrap();
        commit_file_redline_impl(&path, &c.to_string(), "a.md", None).unwrap();
        assert_eq!(
            before,
            ref_snapshot(&path),
            "WD-8 commands must not move refs or create commits"
        );
    }

    /// (Task 6 — AC-8) An unborn HEAD / empty repo yields an EMPTY graph, not an
    /// error.
    #[test]
    fn source_commit_graph_empty_repo_is_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        git2::Repository::init(&path).unwrap();
        let graph = source_commit_graph_impl(&path, 50, None).unwrap();
        assert!(
            graph.is_empty(),
            "unborn HEAD ⇒ empty graph, never an error"
        );
    }

    /// SLW-DV-6: the per-commit (History-mode) change-set uses its OWN
    /// `DiffOptions` (tree-to-tree, no `show_untracked_content`); assert it still
    /// carries per-file line counts through the shared `records_from_diff` spine —
    /// a modify reports both sides, a committed binary reports `None`. Guards the
    /// commit path independently of the working-tree path's own line-count test.
    #[test]
    fn source_commit_changeset_reports_per_file_line_counts() {
        let (_dir, path) = repo_with_main(&[("a.md", "old\n")]);
        let c = commit_more(
            &path,
            "edit + binary",
            // one line replaced by two → +2 / −1; a committed blob with NUL bytes
            // reads as binary → None / None.
            &[("a.md", Some("one\ntwo\n")), ("blob.bin", Some("x\0y\0z"))],
        );

        let records = source_commit_changeset_impl(&path, &c.to_string()).unwrap();

        let edit = find_record(&records, "a.md").unwrap();
        assert_eq!(edit.added, Some(2), "a.md added lines");
        assert_eq!(edit.removed, Some(1), "a.md removed lines");

        let blob = find_record(&records, "blob.bin").unwrap();
        assert_eq!(blob.added, None, "committed binary has no line count");
        assert_eq!(blob.removed, None, "committed binary has no line count");
    }

    /// (Task 7 — characterization) The FLAT per-commit records are byte-identical
    /// to the GROUPED commit set's records (RD-3) — so WD-9 reuses `SourceDiffPane`
    /// with no shim. Only the bucketing is dropped, never the record shape.
    #[test]
    fn source_commit_changeset_records_match_grouped_commit_set() {
        let (_dir, path) = repo_with_main(&[("a.md", "old\n"), ("keep.md", "k\n")]);
        let c = commit_more(
            &path,
            "edit+add",
            &[("a.md", Some("new\n")), ("added.md", Some("x\n"))],
        );

        let mut flat = source_commit_changeset_impl(&path, &c.to_string()).unwrap();
        let grouped = commit_publish_set_impl(&path, &c.to_string()).unwrap();
        let mut grouped_records: Vec<PublishFile> = grouped
            .general
            .files
            .iter()
            .chain(grouped.buckets.iter().flat_map(|b| b.files.iter()))
            .cloned()
            .collect();
        flat.sort_by(|a, b| a.path.cmp(&b.path));
        grouped_records.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(
            flat, grouped_records,
            "flat per-commit records must match the grouped set's records byte-for-byte"
        );
    }

    /// (AC-2) A commit reachable ONLY via a `refs/remotes/*` ref (no local branch,
    /// not HEAD) still SEEDS the walk — proving remote tips are walk roots, not
    /// merely decorations. Also pins `limit == 0` ⇒ an empty page.
    #[test]
    fn source_commit_graph_walks_remote_only_refs_and_honors_limit_zero() {
        let (_dir, path) = repo_with_main(&[("base.md", "b\n")]);
        let repo = git2::Repository::open(&path).unwrap();
        let c0 = repo.head().unwrap().peel_to_commit().unwrap();
        let sig = repo.signature().unwrap();
        // A commit reachable ONLY via a remote-tracking ref.
        let mut b = repo.treebuilder(Some(&c0.tree().unwrap())).unwrap();
        b.insert("remote.md", repo.blob(b"r\n").unwrap(), 0o100644)
            .unwrap();
        let tree = repo.find_tree(b.write().unwrap()).unwrap();
        let remote_only = repo
            .commit(None, &sig, &sig, "remote work", &tree, &[&c0])
            .unwrap();
        repo.reference(
            "refs/remotes/origin/feature",
            remote_only,
            true,
            "remote branch",
        )
        .unwrap();

        let graph = source_commit_graph_impl(&path, 50, None).unwrap();
        let entry = graph
            .iter()
            .find(|e| e.sha == remote_only.to_string())
            .expect("a commit reachable only via refs/remotes/* must seed the walk");
        assert!(
            entry
                .refs
                .iter()
                .any(|r| r.kind == RefKind::Remote && r.name == "origin/feature"),
            "the remote tip decorates its commit"
        );

        // limit == 0 ⇒ an empty page, never one commit.
        assert!(
            source_commit_graph_impl(&path, 0, None).unwrap().is_empty(),
            "limit=0 yields an empty page"
        );
    }

    // --- SV-CDE-82 (CDE-80): origin's default branch, never a hardcoded origin/main ---

    /// Move `refname` one empty commit ahead of its tip; local branches stay put.
    fn advance_remote(path: &Path, refname: &str) -> git2::Oid {
        let repo = git2::Repository::open(path).unwrap();
        let parent = repo
            .find_reference(refname)
            .unwrap()
            .peel_to_commit()
            .unwrap();
        let sig = repo.signature().unwrap();
        let child = repo
            .commit(
                None,
                &sig,
                &sig,
                "remote work",
                &parent.tree().unwrap(),
                &[&parent],
            )
            .unwrap();
        repo.reference(refname, child, true, "advance").unwrap();
        child
    }

    /// The sync panel's default-branch reads (incoming count, fast-forward gate, PR
    /// merge-base, publish baseline) on a `master` default with no `origin/HEAD`, a
    /// `release` default named by `origin/HEAD`, and a `release` default with no
    /// `origin/HEAD` (nothing names it locally, so each read degrades or gives a reason).
    /// Red on the base for the first two rows: every read looked only at
    /// `refs/remotes/origin/main` (incoming 0, canFastForward false, the PR merge-base
    /// `no origin/main`, and the publish baseline fell back to HEAD, hiding the
    /// unpushed commit).
    #[test]
    fn sync_reads_follow_origin_default_branch_not_origin_main() {
        for (default, origin_head, resolved) in [
            ("master", false, true),
            ("release", true, true),
            ("release", false, false),
        ] {
            let row = format!("{default}, origin/HEAD {origin_head}");
            let (_dir, path) = repo_with_default_branch(default, &[("a.md", "a\n")]);
            let remote = format!("refs/remotes/origin/{default}");
            if origin_head {
                git2::Repository::open(&path)
                    .unwrap()
                    .reference_symbolic("refs/remotes/origin/HEAD", &remote, true, "set-head")
                    .unwrap();
            }
            advance_remote(&path, &remote);
            let status = incoming_status_impl(&path).unwrap();
            assert_eq!(status.incoming, u32::from(resolved), "{row}");
            assert_eq!(status.can_fast_forward, resolved, "{row}");
            let reason = (!resolved).then_some(NO_ORIGIN_DEFAULT);
            assert_eq!(status.default_branch_reason.as_deref(), reason, "{row}");

            let pr = commit_pr_head(&path, 9, &[("a.md", Some("pr\n"))]);
            let repo = git2::Repository::open(&path).unwrap();
            let local = repo
                .refname_to_id(&format!("refs/heads/{default}"))
                .unwrap();
            match resolve_pr_commits(&repo, 9) {
                Ok((before, after)) if resolved => {
                    assert_eq!((before, after), (local, pr), "{row}")
                }
                Err(reason) if !resolved => assert_eq!(reason, NO_ORIGIN_DEFAULT, "{row}"),
                other => panic!("{row}: {other:?}"),
            }

            // An unpushed local commit is in the publish set against origin's default;
            // with none known the set is against HEAD and says why.
            let remote_tip = repo.refname_to_id(&remote).unwrap();
            repo.reference(&format!("refs/heads/{default}"), remote_tip, true, "pull")
                .unwrap();
            repo.reset(
                &repo.find_object(remote_tip, None).unwrap(),
                git2::ResetType::Hard,
                None,
            )
            .unwrap();
            commit_on_head(&path, "unpushed", &[("a.md", "local\n")]);
            let set = publish_set_impl(&path, None, None).unwrap();
            assert_eq!(
                find_file(&set, "a.md").map(|file| file.status),
                resolved.then_some(PublishFileStatus::Modified),
                "{row}"
            );
            assert_eq!(set.baseline_reason.as_deref(), reason, "{row}");
        }
    }

    /// The publish baseline's upstream rung: a `develop` default with no `origin/HEAD`
    /// (so no origin default resolves) and no local `main`, on a branch tracking
    /// `origin/develop`, keeps an unpushed commit in the publish set with no reason.
    /// Red on the base: its ladder was `origin/main` → `main` → HEAD, so the commit
    /// vanished into the HEAD baseline.
    #[test]
    fn publish_baseline_falls_back_to_the_branch_upstream() {
        let (_dir, path) = repo_with_default_branch("develop", &[("a.md", "a\n")]);
        git2::Repository::open(&path)
            .unwrap()
            .find_branch("develop", git2::BranchType::Local)
            .unwrap()
            .set_upstream(Some("origin/develop"))
            .unwrap();
        commit_on_head(&path, "unpushed", &[("a.md", "local\n")]);
        let set = publish_set_impl(&path, None, None).unwrap();
        assert_eq!(
            find_file(&set, "a.md").map(|file| file.status),
            Some(PublishFileStatus::Modified)
        );
        assert_eq!(set.baseline_reason, None);
        let status = incoming_status_impl(&path).unwrap();
        assert_eq!(
            status.default_branch_reason.as_deref(),
            Some(NO_ORIGIN_DEFAULT),
            "the incoming badge reads origin's default branch only"
        );
    }
}
