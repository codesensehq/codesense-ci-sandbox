// SPDX-License-Identifier: MIT
//! Path resolver + remote-head reader for org context repos.
//!
//! Provides two orthogonal primitives used by the C-layer read model:
//!
//! 1. Pure path-computation functions (`path_to_registry_file`,
//!    `path_to_team_shared`, `path_to_source_repo`, …) that encode where
//!    working-tree content lives on disk, given an org root and a logical
//!    content location.
//!
//! 2. `read_from_origin_head`, which opens the repo at `org_root` and
//!    reads file bytes from `refs/remotes/origin/HEAD` — the remote
//!    default branch resolved offline, no GitHub API call.
//!
//! The registry read model is working-tree-authoritative (SW-ID-9): the
//! org loader reads `path_to_registry_file` + `std::fs::read` for every
//! org. `read_from_origin_head` is no longer part of that path; its sole
//! remaining consumer is worktree provenance classification
//! (`commands/worktree_provenance.rs`), which compares working-tree bytes
//! against the pristine `origin/HEAD` blob.
//!
//! See B.5 (path-resolver-and-origin-head-reader) for the full contract.

#![allow(dead_code)]

use std::fmt;
use std::path::{Path, PathBuf};

use crate::domain::{Principal, RegistryFile, SourceRepoSlug};

use super::branch_common::{is_not_found, local_branch_exists};

// ---------- Layout constants ----------

/// Subdirectory under `org_root` where non-main branches are checked out
/// via `git worktree add`. The main branch's working tree lives at
/// `org_root` itself (no `worktrees/main/` subdirectory). See B.5
/// Design Notes for the layout rationale.
pub const WORKTREES_DIR: &str = "worktrees";

pub const ORG_BINDINGS_FILE: &str = "org_bindings.yml";
pub const PENDING_ORG_CREATES_FILE: &str = "pending_org_creates.yml";

// SLW-IN-7: `TEAM_SHARED_DIR` (`"_team"`) was retired. Teams are now stored
// flat at the context-repo root (`{org_root}/{team}/…`) instead of nested
// under a shared `_team/` container — one directory per team. The four path
// parsers that used to split on `_team` now anchor on the first *layer
// marker* child (`install`/`content`/`repos`/`libraries`); see
// `LAYER_MARKER_DIRS` and `derive_org_root`.

/// Per-team content directory name (the `TeamProjection` layer source).
/// Single source of truth for the layout `{org_root}/{team}/content/`.
pub const TEAM_CONTENT_DIR: &str = "content";

/// Per-team install directory name: the retired install tiers, folded into
/// `content/` at boot (SW-XS-14) and deleted from `LayerId` (SW-XS-8).
/// Single source of truth for the `install/` layer-marker.
pub const INSTALL_DIR: &str = "install";

/// Per-team app-managed config directory name — home of machine-owned team
/// ledgers (SW-SB-24's `sprints.yaml`) that must travel to teammates but must
/// NOT surface in the content browser. It is a plain sibling of `content/` under
/// `{org_root}/{team}/`, git-tracked by the whole-repo sync like everything else,
/// but DELIBERATELY absent from [`LAYER_MARKER_DIRS`] so it is never a projection
/// layer source and the scanner (which walks only `content/`) never surfaces it.
pub const CONFIG_DIR: &str = "config";

/// SLW-PT-7: per-team scratch-notes directory name. `config/`-class: a plain
/// sibling of `content/` under `{org_root}/{team}/`, git-tracked by the
/// whole-repo sync so notes back up + travel, but DELIBERATELY absent from
/// [`LAYER_MARKER_DIRS`] so it is never a projection source and the content
/// scanner (which walks only `content/`) never surfaces it. Single source of
/// truth for the `"notes"` segment, shared by [`path_to_team_notes_dir`] and
/// `commands::scratch`'s notes-dir helper.
pub const NOTES_DIR: &str = "notes";

/// Per-space custom right-pane directory name (`{org_root}/{team}/panes/`, and the
/// same segment inside a private space). One `<id>.html` per pane. Like
/// [`CONFIG_DIR`] it is a plain sibling of `content/`: the whole-repo sync carries a
/// team's panes to teammates, a private space keeps them local, and it is
/// DELIBERATELY absent from [`LAYER_MARKER_DIRS`] so a pane is never content: never in
/// the file tree, never scanned, never projected into a worktree.
pub const PANES_DIR: &str = "panes";

/// Per-team repo-scoped tier directory name (`{team}/repos/{owner}/{repo}/`).
/// Single source of truth for the `repos/` layer-marker.
pub const REPOS_DIR: &str = "repos";

/// The layer-marker directory names that a team directory directly contains.
/// Post-SLW-IN-7 the `_team/` delimiter is gone, so the path parsers derive
/// the team as the segment immediately *before* the first of these markers
/// after the org slug. Team names equal to any of these are reserved (rejected
/// at `validate_team_name_full`) so the flat delimiter stays un-collidable.
/// `LIBRARIES_DIR` is declared lower in this file (with the canvas-library
/// helpers); referenced by value here to keep this the single ordered marker
/// set.
///
/// SLW-UD-8: `INSTALL_DIR` was dropped from this set. The team-level `install/`
/// tier is retired (its bytes folded into `content/`), so no `{org_root}/{team}/install/…`
/// path exists to anchor on, and `install` is no longer a reserved team name.
/// The per-repo install tier (`{team}/repos/{owner}/{repo}/install/`) still
/// anchors on `repos` (the first marker precedes the nested `install`), so
/// removing `install` here does not affect it. `INSTALL_DIR` itself is retained
/// (still used by `path_to_team_install_dir`).
pub const LAYER_MARKER_DIRS: [&str; 3] = [TEAM_CONTENT_DIR, REPOS_DIR, LIBRARIES_DIR];

/// SS.8: Reserved directory at the org root for codesense-internal config
/// (currently `worktree/{owner}/{repo}.yaml`). The leading underscore
/// guarantees it can't collide with a legitimate `{owner}/{repo}/`
/// source-repo directory. Distinct from the
/// hidden `{org_root}/.codesense/` namespace, which holds ephemeral
/// markers like `pts-migration.json` — `_codesense/` is for first-class
/// config, `.codesense/` is for hidden markers.
pub const CODESENSE_INTERNAL_DIR: &str = "_codesense";

/// SW-EW-5 / SW-CP-15 Task 10: Per-user root segment holding epic workspaces —
/// `{codesense_home}/users/{uid}/_epic/{org_slug}/{epic-id}/`. Each is a
/// synthetic content-engine target (a plain folder, no `.git`) holding per-repo
/// integration worktrees plus the team's inherited install + projected context.
///
/// Rooted under the per-user home as a sibling of `orgs/`, `installs/`,
/// `sources/`, and `personal/` — and therefore **outside every context-repo
/// clone** by construction. The original SW-EW-5 layout hung this off `org_root`
/// (`{org_root}/_epic/…`), but `org_root` *is* the context-repo git root
/// (`…/orgs/{owner}/{repo}/`), so that put epic workspaces — including the full
/// source-repo clones SW-EW-6 assembles inside them — **inside version control**,
/// where a `git add -A` swept a clone gitlink into the synced context repo. This
/// follows the established machine-local-state pattern (install lockfiles →
/// `users/{uid}/installs/`, managed clones → `users/{uid}/sources/`): regenerable
/// per-machine state lives under `users/{uid}/`, never inside a clone, so it can
/// never be committed or synced. The `{org_slug}` segment disambiguates epics
/// across orgs that share an owner. The leading underscore mirrors the `_epic`
/// slug-owner sentinel in `domain::workspace_type`.
pub const EPIC_DIR: &str = "_epic";

/// The machine-local root for team-workspace agent dirs: `users/{uid}/_worker/`. A
/// team-workspace session runs in its own dir here, never in the shared team content
/// root (its launch writes `.mcp.json` and agent settings into its cwd). Rooted like
/// [`EPIC_DIR`], outside every context-repo clone, so nothing staged here is synced.
pub const WORKER_DIR: &str = "_worker";

/// Per-user namespace directory under `codesense_home`. Every piece of
/// per-user local state (context repos, worktrees, attachments) lives
/// under `{codesense_home}/{USERS_DIR}/{github_user_id}/`.
pub const USERS_DIR: &str = "users";

/// Orgs directory under each user's scoped home. Used by
/// `path_to_org_root` and the migration primitive's flat-layout detection.
pub const ORGS_DIR: &str = "orgs";

/// Machine-local install-state directory under each user's scoped home.
/// Sibling of `orgs/`; holds the v3 resolver lockfiles OUTSIDE every
/// context-repo clone. Single source of truth for the segment name; used
/// by `path_to_install_root`.
pub const INSTALLS_DIR: &str = "installs";

/// SW-MC-1: machine-local managed-source-clone directory under each user's
/// scoped home. Sibling of `installs/` and `orgs/`; holds codesense-managed
/// source clones when no per-org workspace-root is configured (the hidden,
/// self-managed default). Single source of truth for the segment name; used
/// by `path_to_managed_source_clone`. (SW-MC-2 layers the configurable root
/// on top; this is the default branch.)
pub const SOURCES_DIR: &str = "sources";

/// Per-user personal-content directory under each user's scoped home.
/// Sibling of `orgs/` and `installs/`, so it lives OUTSIDE every
/// context-repo clone — personal content can never be committed into the
/// synced context repo by construction (SW-LT-1 AC-5). Single source of
/// truth for the `personal/` segment name (the retired Personal resolver
/// tiers were removed in SLW-IN-4; the canvas `path_to_personal_libraries_dir`
/// is the remaining consumer of the layout).
pub const PERSONAL_DIR: &str = "personal";

/// Per-user attachment mapping file. Maps source repo paths to
/// `(org_slug, team_name)` tuples. Lives under the user-scoped home.
pub const ATTACHMENTS_FILE: &str = "attachments.yml";

/// Top-level pipeline-output directory materialized into each tracked repo
/// (planning/implementation/test artifacts live beneath it). Single source of
/// truth for the segment name; the compound `default_skeleton_dirs` entries are
/// pinned to start with this value by `layout_const_values_are_stable`.
pub const BMAD_OUTPUT_DIR: &str = "_bmad-output";

/// Agent-config directory (skills, hooks, `settings.json`) installed into
/// tracked repos and detected on setup. Single source of truth for the
/// segment name.
pub const CLAUDE_CONFIG_DIR: &str = ".claude";

/// Default skeleton subdirectories created inside each tracked repo's
/// content directory. Single source of truth — used by
/// `seed_repo_content` and legacy `create_context_repo`.
pub fn default_skeleton_dirs() -> &'static [&'static str] {
    &[
        "_bmad-output/planning-artifacts",
        "_bmad-output/implementation-artifacts",
        "_bmad-output/test-artifacts",
    ]
}

// ---------- Path resolver ----------

/// Absolute path to a registry YAML file on the main branch's working
/// tree. Registry files always live at the repo root (alongside `.git/`),
/// NOT inside `worktrees/`.
///
/// ```text
///     org_root = /tmp/alpha
///     file     = RegistryFile::OrgYml
///     returns  = /tmp/alpha/org.yml
/// ```
///
/// Pure function — no I/O, no existence checks. Callers that need to
/// verify the file actually exists must use `std::fs::metadata` or
/// `Path::exists` separately. B.5 does not sanitize or canonicalize —
/// not a security boundary.
///
/// See B.5 (path-resolver-and-origin-head-reader).
pub fn path_to_registry_file(org_root: &Path, file: RegistryFile) -> PathBuf {
    org_root.join(file.filename())
}

/// Absolute path to an epic workspace dir:
/// `{codesense_home}/users/{uid}/_epic/{org_slug}/{epic_id}/` (SW-EW-5,
/// relocated out of `org_root` by SW-CP-15 Task 10).
///
/// ```text
///     codesense_home = /home/dev/.codesense
///     github_user_id = 42
///     org_slug       = "acme/widgets-context"
///     epic_id        = "SW-EW"
///     returns        = /home/dev/.codesense/users/42/_epic/acme/widgets-context/SW-EW
/// ```
///
/// Pure function — no I/O, no existence checks. The epic id is joined
/// verbatim; callers pass a slug-validated id (the same id that
/// `domain::derive_epic_workspace_slug` accepts). Rooted under the per-user
/// home (a sibling of `orgs/`/`installs/`/`sources/`) so it is **outside
/// every context-repo clone** — local-disk-only, never committed or synced.
/// See `EPIC_DIR` for why this no longer hangs off `org_root`.
pub fn path_to_epic_workspace(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    epic_id: &str,
) -> PathBuf {
    path_to_user_home(codesense_home, principal)
        .join(EPIC_DIR)
        .join(org_slug)
        .join(epic_id)
}

/// A team-workspace agent's dir: `{home}/users/{uid}/_worker/{org_slug}/{team}/{slug}`.
/// Mirrors [`path_to_epic_workspace`]. Pure path math; the caller passes a slug that is
/// already reduced to `[a-z0-9-]`.
pub fn path_to_worker_workspace(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    team: &str,
    slug: &str,
) -> PathBuf {
    path_to_user_home(codesense_home, principal)
        .join(WORKER_DIR)
        .join(org_slug)
        .join(team)
        .join(slug)
}

/// Absolute path to a single repo's integration worktree inside an epic
/// workspace: `{workspace_dir}/{owner}/{repo}/` (SW-EW-6).
///
/// ```text
///     workspace_dir = /tmp/alpha/_epic/SW-EW
///     owner / repo  = "alderco" / "codesense"
///     returns       = /tmp/alpha/_epic/SW-EW/alderco/codesense
/// ```
///
/// Single source of truth for the per-repo worktree layout — both
/// assembly (SW-EW-6 AC-1) and teardown (AC-6) compute the path here so
/// they never drift. Pure function — no I/O.
pub fn path_to_integration_worktree(workspace_dir: &Path, slug: &SourceRepoSlug) -> PathBuf {
    try_path_to_integration_worktree(workspace_dir, slug)
        .unwrap_or_else(|_| workspace_dir.join(&slug.owner).join(&slug.repo))
}
pub fn try_path_to_integration_worktree(
    workspace_dir: &Path,
    slug: &SourceRepoSlug,
) -> Result<PathBuf, crate::domain::DomainError> {
    Ok(slug.physical_path_key()?.append_to(workspace_dir))
}

/// The `(epic_id, owner, repo)` identity of an epic *integration worktree*,
/// parsed back out of its on-disk path. `org_slug` is consumed for anchoring but
/// not carried — mirroring the epic row, which omits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpicWorktreeCoords {
    pub epic_id: String,
    pub owner: String,
    pub repo: String,
}

/// Classify a discovered worktree `path`: `Some` iff it is an epic integration
/// worktree — i.e. `…/{EPIC_DIR}/{org_slug…}/{epic_id}/{owner}/{repo}` — with
/// `epic_id`/`owner`/`repo` parsed out. The inverse of [`path_to_epic_workspace`]
/// and [`path_to_integration_worktree`], anchored on [`EPIC_DIR`] so the path
/// shape has a single source of truth.
///
/// Supported layouts are parsed by component count, never by guessing from component
/// contents:
///
/// - current: `{org-owner}/{org-repo}/{epic-id}/{host}/{owner}/{repo}`
/// - pre-host production: `{org-owner}/{org-repo}/{epic-id}/{owner}/{repo}`
/// - legacy: `{org-slug}/{epic-id}/{owner}/{repo}`
///
/// The five-component tail is structurally ambiguous: it may be a one-segment
/// org plus host, or a two-segment org without host. This path-only compatibility
/// entry point preserves the pre-host interpretation. Discovery, which also has
/// the integration branch, calls [`classify_epic_worktree_path_with_branch`] to
/// resolve that shape without guessing from host or epic-id spelling.
///
/// Pure — no filesystem syscalls. Callers feed paths from `git worktree list`, which
/// already guarantees each is a real linked worktree *root*, so no gitlink/`is_file`
/// check and no rejection of below-leaf subpaths belong here (those are a disk-scan's
/// concern, not this path-shape classifier's).
pub fn classify_epic_worktree_path(path: &Path) -> Option<EpicWorktreeCoords> {
    classify_epic_worktree_path_with_branch(path, None)
}

pub fn classify_epic_worktree_path_with_branch(
    path: &Path,
    branch: Option<&str>,
) -> Option<EpicWorktreeCoords> {
    let comps: Vec<&std::ffi::OsStr> = path.components().map(|c| c.as_os_str()).collect();
    let epic_idx = comps
        .iter()
        .position(|c| *c == std::ffi::OsStr::new(EPIC_DIR))?;
    let tail = comps.get(epic_idx + 1..)?;
    if let Some(epic_id) = branch.and_then(|value| value.strip_prefix("integration/")) {
        if tail.len() < 4 {
            return None;
        }
        let matches = tail[..tail.len() - 2]
            .iter()
            .enumerate()
            .filter(|(_, component)| component.to_string_lossy() == epic_id)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let [epic_index] = matches.as_slice() else {
            return None;
        };
        if *epic_index == 0 || tail.len() - epic_index - 1 < 2 {
            return None;
        }
        return Some(EpicWorktreeCoords {
            epic_id: epic_id.to_owned(),
            owner: tail.get(tail.len() - 2)?.to_string_lossy().into_owned(),
            repo: tail.last()?.to_string_lossy().into_owned(),
        });
    }
    let epic_offset = match tail.len() {
        6 => 4, // current two-segment org slug + host-aware repository key
        5 => {
            3 // two-segment pre-host production layout without branch evidence
        }
        4 => 3, // one-segment legacy org slug
        _ => return None,
    };
    let repo = tail.last()?;
    let owner = tail.get(tail.len() - 2)?;
    let epic_id = tail.get(tail.len() - epic_offset)?;
    Some(EpicWorktreeCoords {
        epic_id: epic_id.to_string_lossy().into_owned(),
        owner: owner.to_string_lossy().into_owned(),
        repo: repo.to_string_lossy().into_owned(),
    })
}

/// Directory (under each user's scoped home) that holds branch-review
/// workspaces — git worktrees checked out on an arbitrary branch/PR for a
/// dashboard-driven review/run (SW-DX-18). Sibling of `_epic/`; the leading
/// underscore mirrors that owner sentinel. Rooting a branch worktree here makes
/// it a codesense-**managed** target BY PATH
/// (`is_codesense_managed_target` short-circuits on `users/`) with NO
/// worktree-index record — the story-centric `WorktreeStoryRecord` index stays
/// story-only.
pub const BRANCH_DIR: &str = "_branch";

/// Absolute path to a branch-review workspace worktree:
/// `{codesense_home}/users/{uid}/_branch/{org_slug}/{owner}/{repo}/{branch_slug}/`
/// (SW-DX-18).
///
/// ```text
///     codesense_home = /home/dev/.codesense
///     github_user_id = 42
///     org_slug       = "acme/widgets-context"
///     owner / repo   = "alderco" / "codesense"
///     branch_slug    = "feat-open-branch"   (caller replaces `/` → `-`)
///     returns        = /home/dev/.codesense/users/42/_branch/acme/widgets-context/alderco/codesense/feat-open-branch
/// ```
///
/// Pure function — no I/O. `branch_slug` is joined verbatim, so callers pass a
/// slugified branch (`/` → `-`) — a nested ref (`feat/x`) must not spawn
/// intermediate dirs that collide with `git worktree add`. Rooted under the
/// per-user home so the worktree is managed by path (outside every context-repo
/// clone; local-disk-only, never committed or synced).
pub fn path_to_branch_workspace(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    slug: &SourceRepoSlug,
    branch_slug: &str,
) -> PathBuf {
    try_path_to_branch_workspace(codesense_home, principal, org_slug, slug, branch_slug)
        .unwrap_or_else(|_| {
            path_to_user_home(codesense_home, principal)
                .join(BRANCH_DIR)
                .join(org_slug)
                .join(&slug.owner)
                .join(&slug.repo)
                .join(branch_slug)
        })
}
pub fn try_path_to_branch_workspace(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    slug: &SourceRepoSlug,
    branch_slug: &str,
) -> Result<PathBuf, crate::domain::DomainError> {
    Ok(path_to_user_home(codesense_home, principal)
        .join(BRANCH_DIR)
        .join(org_slug)
        .join(slug.physical_path_key()?.append_to(Path::new("")))
        .join(branch_slug))
}

// SLW-IN-7: `path_to_team_shared` and `path_to_source_repo` (the
// branch-worktree `{org}/worktrees/{branch}/_team/…` builders) were deleted
// — both were dead (`#![allow(dead_code)]`, zero production callers) and
// joined `TEAM_SHARED_DIR` under a *different* meaning of `_team` than the
// flat default-team layout, blocking the constant's removal.

// ---------- Install path helpers ----------
//
// These resolve through the flat `{team}/` directory at the org root —
// `{team}` is the team name, a direct child of `org_root` (SLW-IN-7 dropped
// the `_team/` wrapper). They do NOT follow the git-worktree model
// (`{org}/worktrees/{branch}/`) which is not active for the default team
// layout.

/// Team workspace-wide install dir (priority 3).
///
/// Layout: `{org_root}/{team}/install/`
///
/// Pure path math — no filesystem touch, no existence check.
///
/// ```text
///     org_root = /home/dev/.codesense/users/42/orgs/acme/api
///     team     = "default"
///     returns  = /home/dev/.codesense/users/42/orgs/acme/api/default/install
/// ```
pub fn path_to_team_install_dir(org_root: &Path, team: &str) -> PathBuf {
    org_root.join(team).join(INSTALL_DIR)
}

/// Per-team flat content directory (the `TeamProjection` layer source).
///
/// Layout: `{org_root}/{team}/content/`
///
/// Pure path math — no filesystem touch, no existence check.
///
/// ```text
///     org_root = /home/dev/.codesense/users/42/orgs/acme/api
///     team     = "default"
///     returns  = /home/dev/.codesense/users/42/orgs/acme/api/default/content
/// ```
pub fn path_to_team_content_dir(org_root: &Path, team: &str) -> PathBuf {
    org_root.join(team).join(TEAM_CONTENT_DIR)
}

/// Per-team app-managed config directory (SW-SB-24 sprint ledger home).
///
/// Layout: `{org_root}/{team}/config/` — a plain sibling of `content/`/`install/`.
///
/// Pure path math — no filesystem touch, no existence check. Synced to teammates
/// by the whole-repo git tracking (no bespoke sync wave) yet invisible to the
/// content scanner/browser, which walks only `content/` (`config` is not a
/// [`LAYER_MARKER_DIRS`] member, so it is never a projection source).
///
/// ```text
///     org_root = /home/dev/.codesense/users/42/orgs/acme/api
///     team     = "default"
///     returns  = /home/dev/.codesense/users/42/orgs/acme/api/default/config
/// ```
pub fn path_to_team_config_dir(org_root: &Path, team: &str) -> PathBuf {
    org_root.join(team).join(CONFIG_DIR)
}

/// SLW-PT-7: per-team scratch-notes directory (`{org_root}/{team}/notes/`) — the
/// `config/`-class sibling of `content/` that holds the plural notes store
/// (`index.json` + `{id}.md` bodies). Repo-tracked (backs up + travels) but never
/// a projection source: [`NOTES_DIR`] is absent from [`LAYER_MARKER_DIRS`], so
/// materialize and the content browser ignore it exactly as they ignore `config/`.
///
/// Pure path math — no filesystem touch, no existence check.
pub fn path_to_team_notes_dir(org_root: &Path, team: &str) -> PathBuf {
    org_root.join(team).join(NOTES_DIR)
}

/// A team's custom right-pane directory (`{org_root}/{team}/panes/`). Pure path math.
pub fn path_to_team_panes_dir(org_root: &Path, team: &str) -> PathBuf {
    org_root.join(team).join(PANES_DIR)
}

/// The team config dir as the sibling of an already-resolved team `content/` dir
/// (`{team}/content` → `{team}/config`). For callers that hold a content root but
/// not `(org_root, team)` separately — e.g. the HTTP sprints route, whose read
/// token grants a `content_root`. Keeps the `config` segment in one place rather
/// than hardcoding it at the call site. Falls back to joining onto `content_dir`
/// itself only if it has no parent (a defensive impossibility for a real team dir).
pub fn team_config_dir_beside_content(content_dir: &Path) -> PathBuf {
    content_dir.parent().unwrap_or(content_dir).join(CONFIG_DIR)
}

/// The panes dir as the sibling of an already-resolved `content/` dir
/// (`{team}/content` → `{team}/panes`), for callers holding a content root.
pub fn team_panes_dir_beside_content(content_dir: &Path) -> PathBuf {
    content_dir.parent().unwrap_or(content_dir).join(PANES_DIR)
}

/// The one case-folded dedup key for a logical content path. Case-FOLDED and separator-
/// and Unicode-canonical-normalized so two paths that collide on a case-INSENSITIVE dest
/// filesystem (`foo.md` vs `Foo.md`) map to the same key; without the fold a raw-`PathBuf`
/// key treats them as distinct and lets BOTH surface (nondeterministic last-write-wins).
/// Only the collision key folds — the materialized/displayed paths keep their original case.
///
/// Do NOT hand-roll a `HashSet<PathBuf>` at a call site — it double-lists on a
/// case-insensitive FS.
pub fn override_wins_key(logical: &Path) -> String {
    use unicode_normalization::UnicodeNormalization;
    logical
        .to_string_lossy()
        .replace('\\', "/")
        .to_lowercase()
        .nfc()
        .collect()
}

/// Team repo-specific install dir (priority 4).
///
/// Layout: `{org_root}/{team}/repos/{owner}/{repo}/install/`
///
/// Pure path math — no filesystem touch, no existence check.
///
/// ```text
///     org_root = /home/dev/.codesense/users/42/orgs/acme/api
///     team     = "default"
///     slug     = alderco/codesense
///     returns  = /home/dev/.codesense/users/42/orgs/acme/api/default/repos/alderco/codesense/install
/// ```
pub fn path_to_repo_install_dir(org_root: &Path, team: &str, slug: &SourceRepoSlug) -> PathBuf {
    try_path_to_repo_install_dir(org_root, team, slug).unwrap_or_else(|_| {
        org_root
            .join(team)
            .join(REPOS_DIR)
            .join(&slug.owner)
            .join(&slug.repo)
            .join(INSTALL_DIR)
    })
}
pub fn try_path_to_repo_install_dir(
    org_root: &Path,
    team: &str,
    slug: &SourceRepoSlug,
) -> Result<PathBuf, crate::domain::DomainError> {
    try_repo_under(&org_root.join(team), slug, INSTALL_DIR)
}

/// Team repo-specific **content** dir — the two-way sibling of
/// [`path_to_repo_install_dir`] (SLW-IN-11).
///
/// Layout: `{org_root}/{team}/repos/{owner}/{repo}/content/`
///
/// Pure path math — no filesystem touch, no existence check. Mirrors
/// [`path_to_team_content_dir`] one tier deeper: it is the source root the
/// per-repo whole-root projection reads and reverse-syncs back to.
///
/// ```text
///     org_root = /home/dev/.codesense/users/42/orgs/acme/api
///     team     = "default"
///     slug     = alderco/codesense
///     returns  = /home/dev/.codesense/users/42/orgs/acme/api/default/repos/alderco/codesense/content
/// ```
pub fn path_to_repo_content_dir(org_root: &Path, team: &str, slug: &SourceRepoSlug) -> PathBuf {
    try_path_to_repo_content_dir(org_root, team, slug).unwrap_or_else(|_| {
        org_root
            .join(team)
            .join(REPOS_DIR)
            .join(&slug.owner)
            .join(&slug.repo)
            .join(TEAM_CONTENT_DIR)
    })
}
pub fn try_path_to_repo_content_dir(
    org_root: &Path,
    team: &str,
    slug: &SourceRepoSlug,
) -> Result<PathBuf, crate::domain::DomainError> {
    try_repo_under(&org_root.join(team), slug, TEAM_CONTENT_DIR)
}

/// Shared `repos/{owner}/{repo}/{leaf}/` tail under a tier base. Single source
/// of truth for the repo-scoped layout so the `install/` and `content/` leaves
/// can never drift apart.
fn try_repo_under(
    base: &Path,
    slug: &SourceRepoSlug,
    leaf: &str,
) -> Result<PathBuf, crate::domain::DomainError> {
    Ok(slug
        .physical_path_key()?
        .append_to(&base.join(REPOS_DIR))
        .join(leaf))
}

/// Backend-authoritative base directory for a team repository. DTO hydration
/// uses this so the renderer never formats physical repository keys.
pub fn path_to_team_repo_dir(org_root: &Path, team: &str, slug: &SourceRepoSlug) -> PathBuf {
    try_path_to_team_repo_dir(org_root, team, slug).unwrap_or_else(|_| {
        org_root
            .join(team)
            .join(REPOS_DIR)
            .join(&slug.owner)
            .join(&slug.repo)
    })
}
pub fn try_path_to_team_repo_dir(
    org_root: &Path,
    team: &str,
    slug: &SourceRepoSlug,
) -> Result<PathBuf, crate::domain::DomainError> {
    Ok(slug
        .physical_path_key()?
        .append_to(&org_root.join(team).join(REPOS_DIR)))
}

// ---------- Canvas wireframe-library path helpers (SW-SC-5, SW-SC-8) ----------
//
// Wireframe `.excalidrawlib` packs for the shared canvas live in a
// `libraries/` subtree that is a SIBLING of each team's `content/` dir —
// never inside it. `content/` is walked by the projection engine and
// materialized into every source repo's working tree; a canvas library must
// never land there. The `libraries/` sibling is invisible to the resolver,
// the projection engine, and the content watchers, so packs are read
// directly from `~/.codesense/…` by the canvas and never materialized,
// projected, or reverse-synced. SW-SC-8 collapsed the canvas-library tier
// to team-only; the personal tier is the only other `libraries/` surface.

/// Per-tier directory name holding canvas `.excalidrawlib` packs. Single
/// source of truth for the segment; sibling of `install/` and `content/`.
pub const LIBRARIES_DIR: &str = "libraries";

/// Team workspace-wide canvas-library dir.
///
/// Layout: `{org_root}/{team}/libraries/`
///
/// Pure path math — no filesystem touch, no existence check.
pub fn path_to_team_libraries_dir(org_root: &Path, team: &str) -> PathBuf {
    org_root.join(team).join(LIBRARIES_DIR)
}

/// Personal (local-only) canvas-library dir.
///
/// Layout: `{personal_root}/libraries/`
///
/// Roots OUTSIDE the context repo (the personal tier never commits), so a
/// user's own packs and saved shapes stay local. Pure path math — no
/// filesystem touch, no existence check.
pub fn path_to_personal_libraries_dir(personal_root: &Path) -> PathBuf {
    personal_root.join(LIBRARIES_DIR)
}

/// SS.8: Per-source-repo worktree.yaml location under the org's
/// `_codesense/` namespace.
///
/// Layout: `{org_root}/_codesense/worktree/{owner}/{repo}.yaml`
///
/// Single source of truth for the new worktree.yaml home. Backend readers,
/// the watcher relevance filter, the frontend change filter, and the
/// future SS.12 writer all consume this helper — keep them in sync by
/// never hand-building this path.
///
/// Pure path math — no filesystem touch, no existence check. Callers that
/// need to verify the file exists must `Path::exists` separately.
///
/// ```text
///     org_root = /home/dev/.codesense/users/42/orgs/acme/api
///     slug     = alderco/codesense
///     returns  = /home/dev/.codesense/users/42/orgs/acme/api/_codesense/worktree/alderco/codesense.yaml
/// ```
pub fn path_to_worktree_config(org_root: &Path, slug: &SourceRepoSlug) -> PathBuf {
    try_path_to_worktree_config(org_root, slug).unwrap_or_else(|_| {
        super::layout_resolver::LegacyRepositoryPathLocator::worktree_config(org_root, slug)
    })
}
pub fn try_path_to_worktree_config(
    org_root: &Path,
    slug: &SourceRepoSlug,
) -> Result<PathBuf, crate::domain::DomainError> {
    let keyed = slug
        .physical_path_key()?
        .append_to(&org_root.join(CODESENSE_INTERNAL_DIR).join("worktree"));
    let file_name = keyed
        .file_name()
        .expect("RepositoryPathKey always appends a repository segment")
        .to_os_string();
    let mut yaml_file_name = file_name;
    yaml_file_name.push(".yaml");
    Ok(keyed.with_file_name(yaml_file_name))
}

// ---------- User-scoped path helpers ----------

/// Path to the per-user root under `codesense_home`. Everything a
/// single GitHub user owns on this machine lives under this prefix.
///
/// Layout: `{codesense_home}/users/{github_user_id}/`
///
/// Pure path math — no filesystem touch, no existence check.
///
/// ```text
///     codesense_home = /home/dev/.codesense
///     github_user_id = 12345678
///     returns        = /home/dev/.codesense/users/12345678
/// ```
pub fn path_to_user_home(codesense_home: &Path, principal: &Principal) -> PathBuf {
    codesense_home.join(USERS_DIR).join(principal)
}

/// Per-user root for machine-local install state (the v3 resolver
/// lockfiles), a sibling of `orgs/` and therefore OUTSIDE every
/// context-repo clone. Lockfiles live here — never inside
/// `orgs/{slug}/` — so machine-local absolute `code_repo_path` values
/// can't be committed to a context repo by construction.
///
/// Layout: `{codesense_home}/users/{github_user_id}/installs/`
///
/// Pure path math — no filesystem touch, no existence check.
///
/// ```text
///     codesense_home = /home/dev/.codesense
///     github_user_id = 42
///     returns        = /home/dev/.codesense/users/42/installs
/// ```
pub fn path_to_install_root(codesense_home: &Path, principal: &Principal) -> PathBuf {
    path_to_user_home(codesense_home, principal).join(INSTALLS_DIR)
}

/// SW-MC-1: absolute path to a codesense-managed source clone in the hidden
/// default area — a sibling of `installs/` and `orgs/`, so it lives under
/// `users/` and therefore passes `is_codesense_managed_target` (materialize
/// lands without any Option-C predicate work; that's SW-MC-2's job for the
/// configurable-root case). Namespaced by `{owner}/{repo}` so two distinct
/// repos collide only if they are genuinely the same `owner/repo`.
///
/// Layout: `{codesense_home}/users/{github_user_id}/sources/{owner}/{repo}`
///
/// Pure path math — no filesystem touch, no existence check.
pub fn path_to_managed_source_clone(
    codesense_home: &Path,
    principal: &Principal,
    slug: &SourceRepoSlug,
) -> PathBuf {
    try_path_to_managed_source_clone(codesense_home, principal, slug).unwrap_or_else(|_| {
        path_to_user_home(codesense_home, principal)
            .join(SOURCES_DIR)
            .join(&slug.owner)
            .join(&slug.repo)
    })
}
pub fn try_path_to_managed_source_clone(
    codesense_home: &Path,
    principal: &Principal,
    slug: &SourceRepoSlug,
) -> Result<PathBuf, crate::domain::DomainError> {
    Ok(slug
        .physical_path_key()?
        .append_to(&path_to_user_home(codesense_home, principal).join(SOURCES_DIR)))
}

/// SW-MC-2: resolve where a managed clone for `{owner}/{repo}` lands, honoring
/// the per-org `workspace_root_dir` setting when present and falling back to the
/// hidden default otherwise. This is the single source of truth for managed-clone
/// placement — `clone_and_attach_source_repo` consumes it in place of calling
/// [`path_to_managed_source_clone`] directly.
///
/// - `Some(root)` ⇒ `{root}/{owner}/{repo}` — a visible, `cd`-able workspace root.
/// - `None`       ⇒ [`path_to_managed_source_clone`] — the hidden default under
///   `users/{uid}/sources/`.
///
/// Pure path math — no filesystem touch, no existence check. Namespacing by
/// `{owner}/{repo}` is inherent, so two repos collide only when genuinely the
/// same `owner/repo` (SW-MC-2 AC-5).
pub fn resolve_managed_clone_dir(
    codesense_home: &Path,
    principal: &Principal,
    workspace_root_dir: Option<&Path>,
    slug: &SourceRepoSlug,
) -> PathBuf {
    let root = effective_managed_root_dir(codesense_home, principal, workspace_root_dir);
    slug.physical_path_key().map_or_else(
        |_| root.join(&slug.owner).join(&slug.repo),
        |key| key.append_to(&root),
    )
}

/// SW-MC-4: the managed-clone root DIRECTORY (without the `{owner}/{repo}` leaf) —
/// the base that [`resolve_managed_clone_dir`] namespaces under. Re-home detection
/// needs this base to decide whether a recorded clone already sits under the
/// current effective root, and the reverse re-home (workspace root cleared) needs
/// the hidden-default base as its destination. Keeping it beside
/// `resolve_managed_clone_dir` guarantees the two agree on placement.
///
/// - `Some(root)` ⇒ `root` — the visible, `cd`-able workspace root.
/// - `None`       ⇒ `{codesense_home}/users/{uid}/sources` — the hidden default.
///
/// Pure path math — no filesystem touch.
pub fn effective_managed_root_dir(
    codesense_home: &Path,
    principal: &Principal,
    workspace_root_dir: Option<&Path>,
) -> PathBuf {
    match workspace_root_dir {
        Some(root) => root.to_path_buf(),
        None => path_to_user_home(codesense_home, principal).join(SOURCES_DIR),
    }
}

/// Derive `org_root` by stripping the trailing `{team}/{marker}/...` segments
/// from a flat context-repo path (SLW-IN-7).
///
/// Post-flatten a team dir sits directly at the org root and directly
/// contains a *layer marker* (`install`/`content`/`repos`/`libraries`,
/// [`LAYER_MARKER_DIRS`]). This finds the FIRST marker segment after the org
/// slug, takes the team as the segment immediately before it, and returns
/// everything before the team as `org_root`. Handles both the repo-scoped
/// tier (`{org_root}/{team}/repos/{owner}/{repo}/install/...`, first marker
/// = `repos`) and the flat content/install/libraries layers.
///
/// A team named exactly like a marker would make this ambiguous — that is
/// why `validate_team_name_full` reserves the four marker names (D-1 Class
/// B). The FIRST-marker rule also minimizes the residual risk of a marker
/// appearing in the org slug itself (documented, accepted — org slugs are
/// not team-validated).
///
/// Returns `None` when the path contains no marker segment (callers then
/// treat the input as already-org-root) or when a marker sits so early there
/// is no parent before the team (a bogus `Some("")` — guarded like the old
/// `idx > 0` check).
///
/// Pure string math — no filesystem touch.
pub fn derive_org_root(context_repo_path: &str) -> Option<&str> {
    if let Some(location) =
        super::context_attachments::location_for_known_path(Path::new(context_repo_path)).ok()?
    {
        return context_repo_path.get(..location.attachment.root.to_str()?.len());
    }
    // Byte index of the start of the first layer-marker *segment*.
    let bytes = context_repo_path.as_bytes();
    let mut seg_start = 0usize;
    let mut marker_seg_start: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'/' {
            if LAYER_MARKER_DIRS.contains(&&context_repo_path[seg_start..i]) {
                marker_seg_start = Some(seg_start);
                break;
            }
            seg_start = i + 1;
        }
    }
    if marker_seg_start.is_none() && LAYER_MARKER_DIRS.contains(&&context_repo_path[seg_start..]) {
        marker_seg_start = Some(seg_start);
    }
    // The marker is preceded by `/{team}`; org_root ends at the slash before
    // the team segment. `marker_seg_start` points just past the slash before
    // the marker, so `marker_seg_start - 1` is that slash; the slash before
    // the team is the last `/` in `path[..marker_seg_start - 1]`.
    let marker_start = marker_seg_start?;
    let team_slash = context_repo_path[..marker_start.saturating_sub(1)].rfind('/')?;
    if team_slash == 0 {
        // Path begins with `/{team}/{marker}/…` — no parent before the team.
        return None;
    }
    Some(&context_repo_path[..team_slash])
}

/// SW-DX-9: the org **slug** from an `org_root` — the inverse of the layout
/// [`path_to_org_root`] builds (`{home}/users/{id}/{ORGS_DIR}/{slug}`). The slug
/// is everything after the first `/{ORGS_DIR}/` segment (it may itself contain a
/// `/`, e.g. `acme/widgets-context`). `None` when the path has no `/{ORGS_DIR}/`
/// segment or the slug is empty. Uses the `ORGS_DIR` constant so the layout
/// knowledge lives in one place, not as a literal in a caller. Pure string math.
pub fn org_slug_from_org_root(org_root: &str) -> Option<&str> {
    let needle = format!("/{ORGS_DIR}/");
    org_root
        .split_once(&needle)
        .map(|(_, slug)| slug)
        .filter(|s| !s.is_empty())
}

/// Path to a specific org's context repo root for a given user.
///
/// Layout: `{codesense_home}/users/{github_user_id}/orgs/{org_slug}/`
///
/// This is the canonical way to compute `org_root` for any primitive
/// that takes `org_root: &Path` (`scaffold_org_context_repo`,
/// `attach_org_remote`, etc.). `org_slug`
/// is the output of `normalize_source_repo_url` for onboarded orgs,
/// or an internally-generated slug for scaffolded orgs.
///
/// Pure path math — no filesystem touch, no existence check.
///
/// ```text
///     codesense_home = /home/dev/.codesense
///     github_user_id = 42
///     org_slug       = "acme/widgets-context"
///     returns        = /home/dev/.codesense/users/42/orgs/acme/widgets-context
/// ```
pub fn path_to_org_root(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
) -> Result<PathBuf, String> {
    match super::context_attachments::registered(codesense_home, principal, org_slug)? {
        Some(attachment) => super::context_attachments::available_root(&attachment),
        None => Ok(path_to_managed_org_root(
            codesense_home,
            principal,
            org_slug,
        )),
    }
}

/// Explicit managed storage constructor. Never resolves a user-owned checkout.
pub fn path_to_managed_org_root(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
) -> PathBuf {
    path_to_user_home(codesense_home, principal)
        .join(ORGS_DIR)
        .join(org_slug)
}

/// Path to the user's `attachments.yml` file.
///
/// Layout: `{codesense_home}/users/{github_user_id}/attachments.yml`
///
/// This is the canonical path for per-user attachment data. Two GitHub
/// users on the same OS user have two separate attachment files.
///
/// Pure path math — no filesystem touch, no existence check.
///
/// ```text
///     codesense_home = /home/dev/.codesense
///     github_user_id = 99
///     returns        = /home/dev/.codesense/users/99/attachments.yml
/// ```
pub fn path_to_user_attachments_file(codesense_home: &Path, principal: &Principal) -> PathBuf {
    path_to_user_home(codesense_home, principal).join(ATTACHMENTS_FILE)
}

pub fn path_to_user_org_bindings_file(codesense_home: &Path, principal: &Principal) -> PathBuf {
    path_to_user_home(codesense_home, principal).join(ORG_BINDINGS_FILE)
}

/// Principal-local recovery journal for shared workspace creation.
pub fn path_to_user_pending_org_creates_file(
    codesense_home: &Path,
    principal: &Principal,
) -> PathBuf {
    path_to_user_home(codesense_home, principal).join(PENDING_ORG_CREATES_FILE)
}

// ---------- Error type ----------

#[derive(Debug)]
pub enum ReadError {
    /// `refs/remotes/origin/HEAD` does not resolve. Causes:
    /// - The repo has no configured `origin` remote at all (personal
    ///   org that hasn't been linked to GitHub yet).
    /// - The repo has an `origin` remote but has never been fetched
    ///   (freshly `git init`'d with `git remote add` but no `git fetch`).
    /// - The `refs/remotes/origin/HEAD` symbolic ref was manually deleted.
    /// - The symbolic ref points at a direct ref that no longer exists —
    ///   e.g. `origin/HEAD → origin/main` but `origin/main` was deleted
    ///   by `git remote prune` after an upstream default-branch rename,
    ///   or a half-completed fetch left a dangling pointer. The
    ///   `find_reference` call still succeeds (the symbolic-ref file is
    ///   on disk), but `resolve` returns NotFound walking the target.
    /// - The symbolic ref points at an unborn branch (`UnbornBranch`).
    ///
    /// Callers should treat this as "this repo isn't eligible for remote
    /// reads" and fall back to working-tree reads (C.1's dispatch logic).
    OriginHeadNotFound,

    /// `origin/HEAD` resolves successfully, but the requested path does
    /// not exist in the tree. This is a *legitimate* read result — e.g.
    /// a fresh scaffold that was pushed before a new registry file was
    /// added. Caller semantics: the file genuinely doesn't exist on the
    /// remote default branch. Not a git error.
    FileNotFound { path: String },

    /// `origin/HEAD` resolves and the path exists, but the tree entry
    /// is NOT a blob. Two real cases surface here:
    /// - The entry is a tree (directory). `tree.get_path` returns a
    ///   `TreeEntry` of kind `Tree`; `to_object` succeeds and returns
    ///   an `Object` of kind Tree; `as_blob()` returns `None`.
    /// - The entry is a submodule (gitlink) whose pointed-at commit
    ///   IS present in the local object store. `tree.get_path` returns
    ///   a `TreeEntry` of kind `Commit`; `to_object` succeeds and
    ///   returns an `Object` of kind Commit; `as_blob()` returns
    ///   `None`. If the gitlink commit is NOT in the local object
    ///   store (typical for non-recursive clones), `to_object` fails
    ///   instead and the error surfaces as `Git { step: "load tree
    ///   entry object", source }`, not `NotABlob`.
    ///
    /// (Git trees cannot contain tag objects, so tags never surface
    /// here despite what a naive reading of the git object model
    /// might suggest.)
    ///
    /// B.5's contract is "read file bytes"; a non-blob is a caller
    /// bug (asked for bytes of a directory or submodule). Surfaced
    /// as a distinct variant so tests can catch it and callers get
    /// a clear error message instead of a cryptic git2 failure.
    NotABlob { path: String },

    /// `org_root` does not contain a `.git/` directory that `git2` can
    /// open. Distinct from `OriginHeadNotFound` because the problem is
    /// "no repo at all," not "no remote ref in a valid repo." The
    /// pre-check uses `!org_root.is_dir() || !org_root.join(".git").exists()`
    /// and matches the discipline of every other `context_repo/`
    /// helper.
    NotAScaffoldedRepo { path: PathBuf },

    /// Any other git2 error during `Repository::open`, `find_reference`,
    /// `resolve`, `peel_to_commit`, `get_path`, `to_object`, or
    /// `content`. Wrapped with a `step` label so failures are
    /// debuggable. Step labels (all emitted by `read_from_origin_head`):
    /// `"open repo"`, `"find origin/HEAD"`, `"resolve origin/HEAD"`,
    /// `"peel origin/HEAD to commit"`, `"load commit tree"`,
    /// `"find path in tree"`, `"load tree entry object"`. The
    /// `"find origin/HEAD"`, `"resolve origin/HEAD"`, and
    /// `"find path in tree"` labels only surface for NON-NotFound
    /// failures — NotFound (and `UnbornBranch`) routes to
    /// `OriginHeadNotFound` and `FileNotFound` respectively via
    /// `branch_common::is_not_found`.
    Git {
        step: &'static str,
        source: git2::Error,
    },
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OriginHeadNotFound => write!(
                f,
                "refs/remotes/origin/HEAD does not resolve (repo has no remote, hasn't been fetched, or the ref was deleted)"
            ),
            Self::FileNotFound { path } => {
                write!(f, "file not found on origin/HEAD: {path}")
            }
            Self::NotABlob { path } => {
                write!(f, "path is not a file on origin/HEAD: {path}")
            }
            Self::NotAScaffoldedRepo { path } => {
                write!(f, "not a scaffolded context repo: {}", path.display())
            }
            Self::Git { step, .. } => write!(f, "git operation failed during {step}"),
        }
    }
}

impl std::error::Error for ReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Git { source, .. } => Some(source),
            _ => None,
        }
    }
}

// ---------- Remote-head reader ----------

/// Read file bytes from `refs/remotes/origin/HEAD` — the remote
/// default branch of an org context repo, resolved entirely offline
/// from local git state. Never calls the GitHub API, never fetches,
/// never opens the working tree. The returned bytes are whatever the
/// git blob at the requested path contains.
///
/// **Path semantics.** `path` is a POSIX-style slash-separated path
/// inside the git tree (e.g. `"org.yml"`, `"default/content/prd.md"`),
/// NOT a filesystem path. The input is passed verbatim to
/// `git2::Tree::get_path` without normalization — callers must use
/// forward slashes regardless of host platform.
///
/// **Pathological inputs** surface as errors, not panics, but the
/// variant you get depends on whether git2's pre-flight path
/// validation (`git_path_*` in libgit2, called by `Tree::get_path`
/// before the actual tree walk starts) rejects the input or lets it
/// through to the tree walk:
/// - Empty string `""` — rejected by git2's path validation as
///   invalid → `Git { step: "find path in tree", source }`.
/// - Leading slash (absolute-like) `"/foo"` — rejected by path
///   validation → `Git`.
/// - Leading `..` (`"../foo"`) or leading `.` (`"./foo"`) — rejected
///   by git2's `path_to_repo_path` before reaching libgit2 (traversal
///   prevention); produces a generic-class error, NOT NotFound, so
///   routes to `Git { step: "find path in tree" }` not
///   `FileNotFound`. See `git2::util::path_to_repo_path`.
/// - Mid-path `..` (`"foo/../bar"`) — survives the leading-component
///   check, reaches libgit2, which looks up a literal entry named
///   `..` inside `foo/` that won't exist → `FileNotFound`.
/// - Embedded NUL byte `"foo\0bar"` — rejected during git2's internal
///   `CString` conversion → `Git`.
/// - Backslash `"foo\\bar"` — platform-dependent. On Unix, treated as
///   a single literal component whose name contains a backslash,
///   which almost certainly won't exist → `FileNotFound`. **On
///   Windows, git2's `fixup_windows_path` rewrites `\` to `/` before
///   the tree walk**, so `"foo\\bar"` is silently looked up as
///   `"foo/bar"` — which may or may not exist. B.5 always expects
///   POSIX slashes regardless of host; callers relying on backslashes
///   will see different results on different platforms.
///
/// Because the `"find path in tree"` step label now covers both
/// git2's pre-flight path validation AND the libgit2 tree walk
/// itself, a `Git { step: "find path in tree" }` error does not
/// necessarily mean the tree walk began — it may mean the input was
/// rejected before reaching libgit2.
///
/// B.5 does not validate or normalize `path` before passing it to
/// git2 — this is a thin byte-reader, not a policy layer. Callers
/// should construct `path` from trusted inputs (hardcoded registry
/// filenames, validated slugs, etc.) rather than raw user input.
///
/// **Error model.** Five variants via `ReadError`:
/// - `NotAScaffoldedRepo` — `org_root` is not a directory or lacks
///   a `.git/` subdirectory.
/// - `OriginHeadNotFound` — `refs/remotes/origin/HEAD` does not
///   resolve (no remote, never fetched, ref deleted, ref points at
///   a dangling target, or the target is an unborn branch).
/// - `FileNotFound` — `origin/HEAD` resolved but `path` isn't in
///   the tree.
/// - `NotABlob` — `path` resolved but the tree entry isn't a blob
///   (it's a tree, or a submodule whose gitlink commit is present
///   in the local object store). See the `NotABlob` variant doc
///   for the full story — submodules with missing commits surface
///   as `Git { step: "load tree entry object" }` instead.
/// - `Git { step, source }` — any other git2 failure, tagged with
///   a human-readable step label. Non-NotFound failures from the
///   ref lookup, the `resolve()` walk, and the tree lookup route
///   here; NotFound (including `UnbornBranch`) is classified
///   separately via `branch_common::is_not_found`.
///
/// **Dispatch boundary.** B.5 never falls back to the working tree.
/// If `origin/HEAD` doesn't resolve, this returns `OriginHeadNotFound`
/// and C.1 (the registry-read dispatcher) decides whether to retry
/// via `path_to_registry_file` + `std::fs::read`. Dispatch policy
/// belongs in the caller.
///
/// **Plumbing note.** Operates entirely on git2's object store via
/// `find_reference` → `resolve` → `peel_to_commit` → `commit.tree` →
/// `tree.get_path` → `to_object` → `as_blob`. The explicit walk is
/// used instead of `repo.revparse_single("origin/HEAD:{path}")`
/// because the latter conflates "ref not found" with "path not in
/// tree" under the same `git2::ErrorCode::NotFound`, and B.5 needs
/// to distinguish these.
///
/// See B.5 (path-resolver-and-origin-head-reader).
pub fn read_from_origin_head(org_root: &Path, path: &str) -> Result<Vec<u8>, ReadError> {
    if !org_root.is_dir() || !org_root.join(".git").exists() {
        return Err(ReadError::NotAScaffoldedRepo {
            path: org_root.to_path_buf(),
        });
    }
    let repo = git2::Repository::open(org_root).map_err(|source| ReadError::Git {
        step: "open repo",
        source,
    })?;

    let sym = repo
        .find_reference("refs/remotes/origin/HEAD")
        .map_err(|err| {
            if is_not_found(&err) {
                ReadError::OriginHeadNotFound
            } else {
                ReadError::Git {
                    step: "find origin/HEAD",
                    source: err,
                }
            }
        })?;
    // `resolve()` follows the symbolic-ref chain. If `origin/HEAD` points
    // at a target that was deleted (common after `git remote prune` or an
    // upstream default-branch rename), git2 returns NotFound here — same
    // meaning as a missing `origin/HEAD`, so route to the same variant.
    // Applies the `is_not_found` classifier for consistency with every
    // other NotFound check in the module.
    let resolved = sym.resolve().map_err(|err| {
        if is_not_found(&err) {
            ReadError::OriginHeadNotFound
        } else {
            ReadError::Git {
                step: "resolve origin/HEAD",
                source: err,
            }
        }
    })?;
    let commit = resolved.peel_to_commit().map_err(|source| ReadError::Git {
        step: "peel origin/HEAD to commit",
        source,
    })?;

    let tree = commit.tree().map_err(|source| ReadError::Git {
        step: "load commit tree",
        source,
    })?;

    let entry = tree.get_path(Path::new(path)).map_err(|err| {
        if is_not_found(&err) {
            ReadError::FileNotFound {
                path: path.to_string(),
            }
        } else {
            ReadError::Git {
                step: "find path in tree",
                source: err,
            }
        }
    })?;

    let object = entry.to_object(&repo).map_err(|source| ReadError::Git {
        step: "load tree entry object",
        source,
    })?;

    let blob = object.as_blob().ok_or_else(|| ReadError::NotABlob {
        path: path.to_string(),
    })?;

    Ok(blob.content().to_vec())
}

// ---------- Default-branch resolution (SW-EW-6) ----------

/// Failure modes of [`resolve_default_branch`].
#[derive(Debug)]
pub enum DefaultBranchError {
    /// A git2 operation failed, tagged with a human-readable step.
    Git {
        step: &'static str,
        source: git2::Error,
    },
    /// No resolvable default branch was found — `origin/HEAD` did not
    /// resolve to a local branch, `HEAD` is not on a local branch, and
    /// none of `main` / `master` / `develop` exist locally.
    NoDefaultBranch { path: PathBuf },
}

impl fmt::Display for DefaultBranchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DefaultBranchError::Git { step, source } => {
                write!(f, "default-branch resolution failed at {step}: {source}")
            }
            DefaultBranchError::NoDefaultBranch { path } => {
                write!(
                    f,
                    "no resolvable default branch (main/master/develop) in {}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for DefaultBranchError {}

/// Resolve a clone's default branch **name** offline (no network), such
/// that `refs/heads/{name}` exists locally and can be peeled to a commit.
///
/// SW-EW-6 AC-2: the integration worktree is created off this branch when
/// the epic's integration branch does not yet exist. Resolution order:
///
/// 1. The local branch whose *name* matches `refs/remotes/origin/HEAD`'s
///    symbolic target (`origin/{name}`), resolved from local refs only
///    (mirrors [`read_from_origin_head`]) — used only when `refs/heads/{name}`
///    exists locally. Matches on name, not commit identity (the caller peels
///    the local branch to its commit).
/// 2. The local `HEAD` symref, when it points at a local branch.
/// 3. The conventional names `main`, `master`, `develop`, in that order.
///
/// Returns [`DefaultBranchError::NoDefaultBranch`] when none resolve —
/// never panics.
pub fn resolve_default_branch(clone_path: &Path) -> Result<String, DefaultBranchError> {
    let repo = git2::Repository::open(clone_path).map_err(|source| DefaultBranchError::Git {
        step: "open repo",
        source,
    })?;

    // 1. origin/HEAD symbolic target → branch name, if it exists locally.
    if let Ok(sym) = repo.find_reference("refs/remotes/origin/HEAD") {
        if let Ok(Some(target)) = sym.symbolic_target() {
            if let Some(name) = target.strip_prefix("refs/remotes/origin/") {
                if local_branch_exists(&repo, name).map_err(|source| DefaultBranchError::Git {
                    step: "check origin/HEAD branch exists",
                    source,
                })? {
                    return Ok(name.to_string());
                }
            }
        }
    }

    // 2. local HEAD symref, when it points at a branch.
    if let Ok(head) = repo.head() {
        if head.is_branch() {
            if let Ok(shorthand) = head.shorthand() {
                if local_branch_exists(&repo, shorthand).map_err(|source| {
                    DefaultBranchError::Git {
                        step: "check HEAD branch exists",
                        source,
                    }
                })? {
                    return Ok(shorthand.to_string());
                }
            }
        }
    }

    // 3. conventional fallbacks.
    for name in ["main", "master", "develop"] {
        if local_branch_exists(&repo, name).map_err(|source| DefaultBranchError::Git {
            step: "check conventional branch exists",
            source,
        })? {
            return Ok(name.to_string());
        }
    }

    Err(DefaultBranchError::NoDefaultBranch {
        path: clone_path.to_path_buf(),
    })
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    use crate::context_repo::{scaffold_org_context_repo, ScaffoldOrgInputs};
    use crate::domain::parse_org_yml;

    /// SW-FG-18 regression inventory: product projection/config readers must
    /// resolve the migration epoch before touching a host-aware destination.
    /// Test helpers may still exercise the pure builders directly.
    #[test]
    fn production_repo_surface_readers_have_no_marker_blind_path_front_door() {
        // SW-XS-8 deleted the projection engine's repo overlay; what remains of
        // the story linker still must not build a repo path marker-blind.
        let linker = include_str!("../install/story_linker.rs");
        let linker = linker.split("#[cfg(test)]").next().unwrap();
        assert!(!linker.contains("path_to_repo_content_dir("));

        let worktree = include_str!("../commands/worktree.rs");
        let production = worktree
            .split("#[cfg(test)]")
            .next()
            .expect("worktree production source");
        let without_typed_builder = production.replace("try_path_to_worktree_config(", "");
        assert!(!without_typed_builder.contains("path_to_worktree_config("));
        assert!(production.contains("try_path_to_worktree_config("));
    }

    // ---------- Test helpers ----------

    /// RF-3: pin the shared layout-const values so they stay a stable wire
    /// contract, and keep test-side literals a valid independent oracle. Also
    /// guards the `default_skeleton_dirs` coupling to `BMAD_OUTPUT_DIR` without
    /// forcing an allocation in the production helper.
    #[test]
    fn layout_const_values_are_stable() {
        assert_eq!(BMAD_OUTPUT_DIR, "_bmad-output");
        assert_eq!(CLAUDE_CONFIG_DIR, ".claude");
        assert_eq!(USERS_DIR, "users");
        assert_eq!(ORGS_DIR, "orgs");
        let bmad_prefix = format!("{BMAD_OUTPUT_DIR}/");
        assert!(default_skeleton_dirs()
            .iter()
            .all(|d| d.starts_with(&bmad_prefix)));
    }

    /// SLW-UD-8 AC-5: `install` was dropped from `LAYER_MARKER_DIRS`, so
    /// `derive_org_root` no longer anchors on a team-level `install/` (there is
    /// none post-fold). It still parses every live case correctly.
    #[test]
    fn ud8_derive_org_root_after_install_unreserve() {
        // A team literally NAMED `install` (now an allowed name): the first
        // marker is `repos`, so the team is the segment just before it.
        assert_eq!(
            derive_org_root("/u/orgs/o/r/install/repos/acme/api/install/x.md"),
            Some("/u/orgs/o/r")
        );
        // The per-repo install tier still anchors on `repos` — the nested
        // `install` never becomes the first marker.
        assert_eq!(
            derive_org_root("/u/orgs/o/r/default/repos/acme/api/install/.claude/h.sh"),
            Some("/u/orgs/o/r")
        );
        // A team-level content path anchors on `content`, unaffected by the drop.
        assert_eq!(
            derive_org_root("/u/orgs/o/r/default/content/_bmad/x.md"),
            Some("/u/orgs/o/r")
        );
    }

    fn fixture_inputs() -> ScaffoldOrgInputs {
        ScaffoldOrgInputs {
            slug: "alpha-corp--context".to_string(),
            display_name: "Alpha Corp".to_string(),
            created_at: chrono::DateTime::parse_from_rfc3339("2026-04-14T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        }
    }

    /// Copy `refs/heads/main` to `refs/remotes/origin/main` and create
    /// `refs/remotes/origin/HEAD` as a symbolic ref pointing at
    /// `refs/remotes/origin/main`. This simulates the on-disk state
    /// that a real `git clone` would leave behind, without actually
    /// running a clone or a push.
    ///
    /// Parallel with B.4's `wire_remote_branch_ref` test helper in
    /// `shared_branch.rs` — the two are kept separate per-module for
    /// now; consolidate into a shared test-utility module only if a
    /// third caller emerges.
    fn wire_origin_head_to_local_main(org_root: &Path) {
        // Scaffolding no longer commits; stand in for the user's commit.
        crate::context_repo::test_fixtures::commit_all(org_root);
        let repo = git2::Repository::open(org_root).unwrap();
        let local_main = repo.find_reference("refs/heads/main").unwrap();
        let commit_oid = local_main.peel_to_commit().unwrap().id();
        repo.reference("refs/remotes/origin/main", commit_oid, true, "test setup")
            .unwrap();
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
            true,
            "test setup",
        )
        .unwrap();
    }

    // ---------- User-scoped path helper tests ----------

    #[test]
    fn path_to_user_home_produces_expected_layout() {
        let home = Path::new("/home/dev/.codesense");
        assert_eq!(
            path_to_user_home(home, &Principal::from(12345678u64)),
            PathBuf::from("/home/dev/.codesense/users/12345678"),
        );
    }

    #[test]
    fn path_to_user_home_is_byte_identical_to_pre_change_u64_layout() {
        // AC-4/AC-5 anchor: a Principal minted from a GitHub id builds the exact
        // same path the old `github_user_id.to_string()` produced — so existing
        // on-disk `users/{id}` installs resolve unchanged (no filesystem move).
        let home = Path::new("/home/dev/.codesense");
        let id = 1234567u64;
        assert_eq!(
            path_to_user_home(home, &Principal::from(id)),
            home.join(USERS_DIR).join(id.to_string()),
        );
    }

    #[test]
    fn path_to_install_root_produces_expected_layout() {
        let home = Path::new("/home/dev/.codesense");
        assert_eq!(
            path_to_install_root(home, &Principal::from(42u64)),
            PathBuf::from("/home/dev/.codesense/users/42/installs"),
        );
    }

    #[test]
    fn path_to_org_root_produces_expected_layout() {
        let home = Path::new("/home/dev/.codesense");
        assert_eq!(
            path_to_org_root(home, &Principal::from(42u64), "acme-context").unwrap(),
            PathBuf::from("/home/dev/.codesense/users/42/orgs/acme-context"),
        );
    }

    #[test]
    fn org_slug_from_org_root_inverts_path_to_org_root() {
        // Round-trips path_to_org_root, incl. a multi-segment slug.
        assert_eq!(
            org_slug_from_org_root("/home/dev/.codesense/users/42/orgs/acme-context"),
            Some("acme-context"),
        );
        assert_eq!(
            org_slug_from_org_root("/home/dev/.codesense/users/42/orgs/acme/widgets-context"),
            Some("acme/widgets-context"),
        );
        // No `/orgs/` segment, or an empty slug ⇒ None.
        assert_eq!(org_slug_from_org_root("/tmp/not/managed"), None);
        assert_eq!(
            org_slug_from_org_root("/home/dev/.codesense/users/42/orgs/"),
            None
        );
    }

    #[test]
    fn path_to_user_attachments_file_produces_expected_layout() {
        let home = Path::new("/home/dev/.codesense");
        assert_eq!(
            path_to_user_attachments_file(home, &Principal::from(99u64)),
            PathBuf::from("/home/dev/.codesense/users/99/attachments.yml"),
        );
    }

    // ---------- Path resolver tests ----------

    #[test]
    fn registry_file_path_is_org_root_join_filename() {
        let root = Path::new("/tmp/alpha");
        assert_eq!(
            path_to_registry_file(root, RegistryFile::OrgYml),
            PathBuf::from("/tmp/alpha/org.yml"),
        );
    }

    #[test]
    fn epic_workspace_path_roots_under_user_home_keyed_by_org_slug() {
        let home = Path::new("/home/dev/.codesense");
        assert_eq!(
            path_to_epic_workspace(
                home,
                &Principal::from(42u64),
                "acme/widgets-context",
                "SW-EW"
            ),
            PathBuf::from("/home/dev/.codesense/users/42/_epic/acme/widgets-context/SW-EW"),
        );
    }

    #[test]
    fn branch_workspace_path_roots_under_user_home_managed_by_path() {
        // SW-DX-18 AC-6: a branch worktree lives under `users/{uid}/_branch/…`,
        // so `is_codesense_managed_target` (which short-circuits on `users/`)
        // returns true BY PATH — no worktree-index record needed. A nested ref
        // (`feat/x`) is slugified by the caller to one segment.
        let home = Path::new("/home/dev/.codesense");
        let slug = SourceRepoSlug::new("alderco", "codesense").unwrap();
        assert_eq!(
            path_to_branch_workspace(home, &Principal::from(42u64), "acme/widgets-context", &slug, "feat-x"),
            PathBuf::from(
                "/home/dev/.codesense/users/42/_branch/acme/widgets-context/github.com/alderco/codesense/feat-x"
            ),
        );
        // Under the per-user `users/` root — the managed-by-path guarantee.
        assert!(path_to_branch_workspace(
            home,
            &Principal::from(42u64),
            "acme/widgets-context",
            &slug,
            "feat-x"
        )
        .starts_with(home.join("users").join("42")));
    }

    #[test]
    fn worker_workspace_is_a_managed_dir_outside_the_context_repo_clone() {
        let home = Path::new("/home/dev/.codesense");
        let principal = Principal::from(42u64);
        let org_slug = "acme/widgets-context";
        let dir = path_to_worker_workspace(home, &principal, org_slug, "default", "sprint-1");
        assert_eq!(
            dir,
            Path::new(
                "/home/dev/.codesense/users/42/_worker/acme/widgets-context/default/sprint-1"
            )
        );
        assert!(!dir.starts_with(path_to_org_root(home, &principal, org_slug).unwrap()));
        assert!(crate::install::worktree_index::is_codesense_managed_target(
            &dir, home
        ));
    }

    #[test]
    fn epic_workspace_is_outside_the_context_repo_clone() {
        // SW-CP-15 Task 10: the epic workspace must live OUTSIDE the git-tracked
        // context repo. `org_root` IS the clone's git root
        // (`…/orgs/{owner}/{repo}/`), so the original `{org_root}/_epic/…` layout
        // put epic workspaces (and the source clones SW-EW-6 assembles inside
        // them) under version control. They must now root under the per-user
        // home, a sibling of `orgs/`, so they can never be a descendant of any
        // org_root / clone.
        let home = Path::new("/home/dev/.codesense");
        let principal = Principal::from(42u64);
        let org_slug = "acme/widgets-context";
        let org_root = path_to_org_root(home, &principal, org_slug).unwrap();
        let epic = path_to_epic_workspace(home, &principal, org_slug, "SW-EW");
        assert!(
            !epic.starts_with(&org_root),
            "epic workspace {epic:?} must not be inside the context-repo clone {org_root:?}",
        );
        // Sibling of `orgs/`, under the per-user home — like `installs/`/`sources/`.
        assert_eq!(
            epic.parent().unwrap(),
            path_to_user_home(home, &principal)
                .join("_epic")
                .join(org_slug),
        );
    }

    // ---------- Install path helper tests ----------

    #[test]
    fn team_install_dir_is_flat_under_org_root() {
        // SLW-IN-7: no `_team/` segment — the team dir sits directly at the
        // org root.
        let root = Path::new("/home/dev/.codesense/users/42/orgs/acme/api");
        assert_eq!(
            path_to_team_install_dir(root, "default"),
            PathBuf::from("/home/dev/.codesense/users/42/orgs/acme/api/default/install"),
        );
    }

    #[test]
    fn repo_install_dir_includes_repos_segment() {
        let root = Path::new("/home/dev/.codesense/users/42/orgs/acme/api");
        let slug = SourceRepoSlug::new("alderco", "codesense").unwrap();
        assert_eq!(
            path_to_repo_install_dir(root, "default", &slug),
            PathBuf::from(
                "/home/dev/.codesense/users/42/orgs/acme/api/default/repos/github.com/alderco/codesense/install"
            ),
        );
    }

    #[test]
    fn repo_install_dir_preserves_owner_repo_layout_for_non_github() {
        let root = Path::new("/home/dev/.codesense/users/42/orgs/acme/api");
        let slug = SourceRepoSlug::with_provider_and_host(
            "alderco",
            "codesense",
            "gitlab",
            Some("gitlab.com"),
        )
        .unwrap();
        assert_eq!(
            path_to_repo_install_dir(root, "default", &slug),
            PathBuf::from(
                "/home/dev/.codesense/users/42/orgs/acme/api/default/repos/gitlab.com/alderco/codesense/install"
            ),
        );
    }

    #[test]
    fn team_content_dir_produces_expected_layout() {
        let root = Path::new("/home/dev/.codesense/users/42/orgs/acme/api");
        assert_eq!(
            path_to_team_content_dir(root, "default"),
            PathBuf::from("/home/dev/.codesense/users/42/orgs/acme/api/default/content"),
        );
    }

    // ---------- Team config dir path helper tests (SW-SB-24) ----------

    #[test]
    fn team_config_dir_is_content_sibling() {
        let root = Path::new("/home/dev/.codesense/users/42/orgs/acme/api");
        assert_eq!(
            path_to_team_config_dir(root, "default"),
            PathBuf::from("/home/dev/.codesense/users/42/orgs/acme/api/default/config"),
        );
        // Sibling-of-content derivation must agree with the (org_root, team) form.
        assert_eq!(
            team_config_dir_beside_content(&path_to_team_content_dir(root, "default")),
            path_to_team_config_dir(root, "default"),
        );
    }

    #[test]
    fn notes_dir_is_content_sibling_and_never_a_layer_marker() {
        // SLW-PT-7: the personal notes dir is `config/`-class — a plain sibling of
        // `content/` that the whole-repo sync tracks but the projection/resolver
        // engine must ignore. If `notes` ever entered LAYER_MARKER_DIRS the content
        // scanner would walk it and personal scratch would leak into the left tree —
        // the exact regression AC-1 forbids.
        let root = Path::new("/home/dev/.codesense/users/42/orgs/_personal/personal");
        assert_eq!(
            path_to_team_notes_dir(root, "personal"),
            root.join("personal").join("notes"),
        );
        assert!(
            !LAYER_MARKER_DIRS.contains(&NOTES_DIR),
            "notes/ must never be a layer marker (would make personal scratch content-browsable)"
        );
    }

    #[test]
    fn override_wins_key_folds_unicode_case_and_canonical_aliases() {
        assert_eq!(
            override_wins_key(Path::new("É.md")),
            override_wins_key(Path::new("e\u{301}.md"))
        );
        assert_eq!(
            override_wins_key(Path::new("Ä/CHILD")),
            override_wins_key(Path::new("ä/child"))
        );
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("É.md");
        let alias = temp.path().join("e\u{301}.md");
        std::fs::write(&original, b"original").unwrap();
        if alias.exists() {
            assert_eq!(std::fs::read(&alias).unwrap(), b"original");
            assert_eq!(override_wins_key(&original), override_wins_key(&alias));
        }
    }

    #[test]
    fn override_wins_key_folds_case_and_separators() {
        // The shared key case-folds and normalizes separators so two case-variant paths
        // collide on a case-insensitive FS.
        assert_eq!(override_wins_key(Path::new("Foo.md")), "foo.md");
        assert_eq!(
            override_wins_key(Path::new("packs\\Sub\\A.MD")),
            "packs/sub/a.md",
            "backslashes normalize to '/' and the whole key lowercases"
        );
        assert_eq!(
            override_wins_key(Path::new("packs/sub/a.md")),
            override_wins_key(Path::new("Packs/Sub/A.md")),
            "case-variant paths map to the SAME key"
        );
    }

    #[test]
    fn panes_dir_is_content_sibling_and_never_a_layer_marker() {
        // A pane is a registered right pane, not content: if `panes` ever entered
        // LAYER_MARKER_DIRS the scanner would list `panes/x.html` in the file tree and
        // projection would copy it into worktrees.
        let root = Path::new("/home/dev/.codesense/users/42/orgs/acme/api");
        assert_eq!(
            path_to_team_panes_dir(root, "default"),
            root.join("default").join("panes"),
        );
        assert_eq!(
            team_panes_dir_beside_content(&path_to_team_content_dir(root, "default")),
            root.join("default").join("panes"),
        );
        assert!(!LAYER_MARKER_DIRS.contains(&PANES_DIR));
    }

    #[test]
    fn config_dir_is_not_a_layer_marker_so_the_scanner_never_surfaces_it() {
        // SW-SB-24 AC-1 invisibility guard: the ledger home must NOT be a projection
        // layer source. If `config` ever enters LAYER_MARKER_DIRS, the content
        // scanner/browser would start walking it and the ledger would leak into user
        // space — the exact regression this story exists to prevent.
        assert!(
            !LAYER_MARKER_DIRS.contains(&CONFIG_DIR),
            "config/ must never be a layer marker (would make the ledger content-browsable)"
        );
    }

    // ---------- SW-MC-1: managed source clone default path ----------

    #[test]
    fn managed_source_clone_is_owner_repo_under_sources() {
        let home = Path::new("/home/dev/.codesense");
        let slug = SourceRepoSlug::new("alderco", "codesense").unwrap();
        assert_eq!(
            path_to_managed_source_clone(home, &Principal::from(42u64), &slug),
            PathBuf::from("/home/dev/.codesense/users/42/sources/github.com/alderco/codesense"),
        );
    }

    #[test]
    fn managed_source_clone_shares_user_home_with_installs() {
        // Same user-home ancestor as installs/ — proves it lives under users/
        // (so it passes is_codesense_managed_target without MC-2's Option-C).
        let home = Path::new("/home/dev/.codesense");
        let slug = SourceRepoSlug::new("o", "r").unwrap();
        let sources_user_home = path_to_managed_source_clone(home, &Principal::from(7u64), &slug)
            .parent() // .../sources/o
            .and_then(|p| p.parent()) // .../sources
            .and_then(|p| p.parent()) // .../sources/github.com
            .and_then(|p| p.parent()) // .../users/7
            .unwrap()
            .to_path_buf();
        let installs_user_home = path_to_install_root(home, &Principal::from(7u64))
            .parent()
            .unwrap()
            .to_path_buf();
        assert_eq!(sources_user_home, installs_user_home);
    }

    // ---------- SW-MC-2: configurable managed-clone root resolver ----------

    #[test]
    fn resolve_managed_clone_dir_unset_falls_back_to_hidden_default() {
        let home = Path::new("/home/dev/.codesense");
        let principal = Principal::from(42u64);
        let slug = SourceRepoSlug::new("alderco", "codesense").unwrap();
        assert_eq!(
            resolve_managed_clone_dir(home, &principal, None, &slug),
            path_to_managed_source_clone(home, &principal, &slug),
        );
    }

    #[test]
    fn resolve_managed_clone_dir_configured_root_namespaces_owner_repo() {
        let home = Path::new("/home/dev/.codesense");
        let root = Path::new("/Users/dev/mycode");
        let slug = SourceRepoSlug::new("alderco", "codesense").unwrap();
        assert_eq!(
            resolve_managed_clone_dir(home, &Principal::from(42u64), Some(root), &slug),
            PathBuf::from("/Users/dev/mycode/github.com/alderco/codesense"),
        );
    }

    #[test]
    fn resolve_managed_clone_dir_configured_root_ignores_home_and_principal() {
        // The configured branch must not depend on codesense_home or the principal —
        // only the root + owner/repo shape it (records-are-truth).
        let root = Path::new("/srv/shared");
        let slug = SourceRepoSlug::new("o", "r").unwrap();
        assert_eq!(
            resolve_managed_clone_dir(Path::new("/a"), &Principal::from(1u64), Some(root), &slug),
            resolve_managed_clone_dir(Path::new("/b"), &Principal::from(2u64), Some(root), &slug),
        );
    }

    // ---------- SW-MC-4: effective managed-clone root base ----------

    #[test]
    fn effective_managed_root_dir_unset_is_hidden_sources_base() {
        let home = Path::new("/home/dev/.codesense");
        let principal = Principal::from(42u64);
        assert_eq!(
            effective_managed_root_dir(home, &principal, None),
            path_to_user_home(home, &principal).join(SOURCES_DIR),
        );
    }

    #[test]
    fn effective_managed_root_dir_set_is_the_root_itself() {
        let root = Path::new("/Users/dev/mycode");
        assert_eq!(
            effective_managed_root_dir(Path::new("/home"), &Principal::from(1u64), Some(root)),
            PathBuf::from("/Users/dev/mycode"),
        );
    }

    #[test]
    fn effective_managed_root_dir_namespaces_to_resolve_managed_clone_dir() {
        // The base + `{owner}/{repo}` MUST equal what resolve_managed_clone_dir
        // returns — the invariant re-home detection relies on.
        let home = Path::new("/home/dev/.codesense");
        let principal = Principal::from(7u64);
        let slug = SourceRepoSlug::new("o", "r").unwrap();
        for root in [None, Some(Path::new("/ws"))] {
            assert_eq!(
                slug.physical_path_key()
                    .expect("repository identity has a physical path key")
                    .append_to(&effective_managed_root_dir(home, &principal, root)),
                resolve_managed_clone_dir(home, &principal, root, &slug),
            );
        }
    }

    // ---------- Canvas library path helper tests (SW-SC-5, SW-SC-8) ----------

    #[test]
    fn team_libraries_dir_is_install_sibling() {
        let root = Path::new("/home/dev/.codesense/users/42/orgs/acme/api");
        assert_eq!(
            path_to_team_libraries_dir(root, "default"),
            PathBuf::from("/home/dev/.codesense/users/42/orgs/acme/api/default/libraries"),
        );
    }

    #[test]
    fn personal_libraries_dir_appends_libraries() {
        let personal = Path::new("/home/dev/.codesense/users/42/personal/acme/api");
        assert_eq!(
            path_to_personal_libraries_dir(personal),
            PathBuf::from("/home/dev/.codesense/users/42/personal/acme/api/libraries"),
        );
    }

    #[test]
    fn worktree_config_path_lives_under_codesense_internal_dir() {
        let root = Path::new("/home/dev/.codesense/users/42/orgs/acme/api");
        let slug = SourceRepoSlug::new("alderco", "codesense").unwrap();
        assert_eq!(
            path_to_worktree_config(root, &slug),
            PathBuf::from(
                "/home/dev/.codesense/users/42/orgs/acme/api/_codesense/worktree/github.com/alderco/codesense.yaml"
            ),
        );
    }

    // ---------- read_from_origin_head tests ----------

    #[test]
    fn read_from_origin_head_reads_scaffolded_org_yml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("alpha");
        scaffold_org_context_repo(&org_root, &fixture_inputs()).unwrap();

        wire_origin_head_to_local_main(&org_root);

        let bytes = read_from_origin_head(&org_root, "org.yml").unwrap();
        let raw = parse_org_yml(&bytes).unwrap();
        assert_eq!(raw.slug, "alpha-corp--context");
        assert_eq!(raw.display_name, "Alpha Corp");
    }

    #[test]
    fn read_from_origin_head_returns_origin_head_not_found_for_dangling_symbolic_ref() {
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("dangling-head");
        scaffold_org_context_repo(&org_root, &fixture_inputs()).unwrap();

        // Wire `origin/HEAD → origin/main → local main commit`, then
        // delete `origin/main` — leaves `origin/HEAD` as a dangling
        // symbolic ref whose target no longer exists. `find_reference`
        // will still succeed (the symbolic-ref file is on disk), but
        // `resolve()` walks the target and returns NotFound. This is
        // the exact real-world state produced by `git remote prune`
        // after an upstream default-branch rename, and the regression
        // guard for the `resolve()` NotFound classification.
        wire_origin_head_to_local_main(&org_root);
        let repo = git2::Repository::open(&org_root).unwrap();
        repo.find_reference("refs/remotes/origin/main")
            .unwrap()
            .delete()
            .unwrap();

        let err = read_from_origin_head(&org_root, "org.yml").unwrap_err();
        assert!(
            matches!(err, ReadError::OriginHeadNotFound),
            "expected OriginHeadNotFound for dangling symbolic ref, got {err:?}",
        );
    }

    #[test]
    fn read_from_origin_head_returns_origin_head_not_found_when_no_remote() {
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("no-remote");
        scaffold_org_context_repo(&org_root, &fixture_inputs()).unwrap();

        // No wire_origin_head_to_local_main call — the repo has no
        // `refs/remotes/origin/HEAD` at all.
        let err = read_from_origin_head(&org_root, "org.yml").unwrap_err();
        assert!(
            matches!(err, ReadError::OriginHeadNotFound),
            "expected OriginHeadNotFound, got {err:?}",
        );
    }

    #[test]
    fn read_from_origin_head_returns_file_not_found_for_missing_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("missing-file");
        scaffold_org_context_repo(&org_root, &fixture_inputs()).unwrap();
        wire_origin_head_to_local_main(&org_root);

        let err = read_from_origin_head(&org_root, "nonexistent.yml").unwrap_err();
        assert!(
            matches!(&err, ReadError::FileNotFound { path } if path == "nonexistent.yml"),
            "expected FileNotFound(nonexistent.yml), got {err:?}",
        );
    }

    #[test]
    fn read_from_origin_head_returns_not_a_blob_for_directory_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("directory-path");
        scaffold_org_context_repo(&org_root, &fixture_inputs()).unwrap();
        // Seed a subdir so there is a tree entry to resolve against; the
        // scaffold itself only writes top-level files post-RF-31.
        let dir = org_root.join("default").join("content");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("story.md"), b"hi").unwrap();
        wire_origin_head_to_local_main(&org_root);

        // `default` is a directory in the commit tree. Asking for `default`
        // (not `default/content/story.md`) should surface NotABlob.
        let err = read_from_origin_head(&org_root, "default").unwrap_err();
        assert!(
            matches!(&err, ReadError::NotABlob { path } if path == "default"),
            "expected NotABlob(default), got {err:?}",
        );
    }

    #[test]
    fn read_from_origin_head_classifies_pathological_path_inputs() {
        // Regression guard for the rustdoc's "Pathological inputs"
        // paragraph. Each case is a specific claim about which
        // ReadError variant fires for a given malformed `path` string.
        // If a future git2 version changes how `path_to_repo_path`
        // validates input, one of these assertions will fail and
        // force the doc to be updated. Locks in the git2-0.19.0
        // contract as a CI signal. See the round-2 code review
        // fixes for the investigation that established these
        // classifications.
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("pathological");
        scaffold_org_context_repo(&org_root, &fixture_inputs()).unwrap();
        // Seed a subdir so mid-path `..` has a parent that exists in the tree.
        let dir = org_root.join("default").join("content");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("story.md"), b"hi").unwrap();
        wire_origin_head_to_local_main(&org_root);

        // Empty string — rejected by git2's path validation before
        // the tree walk starts → Git { step: "find path in tree" }.
        let err = read_from_origin_head(&org_root, "").unwrap_err();
        assert!(
            matches!(&err, ReadError::Git { step, .. } if *step == "find path in tree"),
            "empty path should surface as Git {{ step: \"find path in tree\" }}, got {err:?}",
        );

        // Leading slash — absolute-like path, rejected by git2's path
        // validation → Git.
        let err = read_from_origin_head(&org_root, "/foo").unwrap_err();
        assert!(
            matches!(&err, ReadError::Git { step, .. } if *step == "find path in tree"),
            "leading-slash path should surface as Git, got {err:?}",
        );

        // Leading `..` — rejected by git2's `path_to_repo_path`
        // traversal check (leading ParentDir component) with a
        // generic error → Git, NOT FileNotFound. Round-2's round-1
        // patch originally claimed FileNotFound here; this test
        // pins the corrected classification.
        let err = read_from_origin_head(&org_root, "../foo").unwrap_err();
        assert!(
            matches!(&err, ReadError::Git { step, .. } if *step == "find path in tree"),
            "leading-.. path should surface as Git (not FileNotFound), got {err:?}",
        );

        // Leading `.` — same rejection path as leading `..`.
        let err = read_from_origin_head(&org_root, "./foo").unwrap_err();
        assert!(
            matches!(&err, ReadError::Git { step, .. } if *step == "find path in tree"),
            "leading-. path should surface as Git, got {err:?}",
        );

        // Mid-path `..` — survives git2's leading-component check
        // and reaches libgit2, which looks up a literal `..` entry
        // inside `default/` that doesn't exist → FileNotFound.
        let err = read_from_origin_head(&org_root, "default/../missing").unwrap_err();
        assert!(
            matches!(&err, ReadError::FileNotFound { .. }),
            "mid-path .. should surface as FileNotFound, got {err:?}",
        );

        // Embedded NUL byte — rejected during git2's CString
        // conversion → Git.
        let err = read_from_origin_head(&org_root, "foo\0bar").unwrap_err();
        assert!(
            matches!(&err, ReadError::Git { step, .. } if *step == "find path in tree"),
            "NUL-containing path should surface as Git, got {err:?}",
        );
    }

    #[test]
    fn read_from_origin_head_errors_when_not_a_repo() {
        let tmp = tempfile::TempDir::new().unwrap();

        // (a) Directory does not exist.
        let ghost = tmp.path().join("ghost");
        let err = read_from_origin_head(&ghost, "org.yml").unwrap_err();
        assert!(
            matches!(err, ReadError::NotAScaffoldedRepo { .. }),
            "expected NotAScaffoldedRepo for nonexistent dir, got {err:?}",
        );

        // (b) Directory exists but is not a git repo.
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let err = read_from_origin_head(&empty, "org.yml").unwrap_err();
        assert!(
            matches!(err, ReadError::NotAScaffoldedRepo { .. }),
            "expected NotAScaffoldedRepo for non-git dir, got {err:?}",
        );
    }

    #[test]
    fn read_error_display_messages_contain_context() {
        let err = ReadError::OriginHeadNotFound;
        let msg = format!("{err}");
        assert!(msg.contains("origin/HEAD"), "got: {msg}");

        let err = ReadError::FileNotFound {
            path: "people.yml".to_string(),
        };
        assert!(format!("{err}").contains("people.yml"));

        let err = ReadError::NotABlob {
            path: "default".to_string(),
        };
        assert!(format!("{err}").contains("default"));

        let err = ReadError::NotAScaffoldedRepo {
            path: PathBuf::from("/tmp/nope"),
        };
        assert!(format!("{err}").contains("/tmp/nope"));

        let err = ReadError::Git {
            step: "resolve origin/HEAD",
            source: git2::Error::from_str("boom"),
        };
        assert!(format!("{err}").contains("resolve origin/HEAD"));
    }
}

#[cfg(test)]
mod host_aware_surface_tests {
    use super::*;

    #[test]
    fn equal_slugs_on_different_hosts_are_distinct_on_every_typed_surface() {
        let github = SourceRepoSlug::new("acme", "api").unwrap();
        let gitlab = SourceRepoSlug::with_provider("acme", "api", "gitlab").unwrap();
        let root = Path::new("/state");
        let principal = Principal::from(7u64);

        assert_ne!(
            path_to_repo_content_dir(root, "team", &github),
            path_to_repo_content_dir(root, "team", &gitlab)
        );
        assert_ne!(
            path_to_repo_install_dir(root, "team", &github),
            path_to_repo_install_dir(root, "team", &gitlab)
        );
        assert_ne!(
            path_to_worktree_config(root, &github),
            path_to_worktree_config(root, &gitlab)
        );
        assert_ne!(
            path_to_managed_source_clone(root, &principal, &github),
            path_to_managed_source_clone(root, &principal, &gitlab)
        );
        assert_ne!(
            path_to_integration_worktree(root, &github),
            path_to_integration_worktree(root, &gitlab)
        );
        assert_ne!(
            path_to_branch_workspace(root, &principal, "org", &github, "branch"),
            path_to_branch_workspace(root, &principal, "org", &gitlab, "branch")
        );
    }

    #[test]
    fn equal_slug_dark_driver_keeps_legacy_github_git_and_routes_gitlab_elsewhere() {
        let root = Path::new("/state");
        let principal = Principal::from(7u64);
        let github = SourceRepoSlug::new("acme", "api").unwrap();
        let gitlab = SourceRepoSlug::with_provider("acme", "api", "gitlab").unwrap();
        let recorded_legacy_github = root.join("users/7/sources/acme/api");

        assert_eq!(
            recorded_legacy_github,
            root.join("users/7/sources/acme/api")
        );
        assert_ne!(
            recorded_legacy_github,
            path_to_managed_source_clone(root, &principal, &gitlab)
        );
        assert_ne!(
            path_to_managed_source_clone(root, &principal, &github),
            path_to_managed_source_clone(root, &principal, &gitlab)
        );
    }

    #[test]
    fn dotted_repo_worktree_configs_keep_the_full_repository_name() {
        let slug = SourceRepoSlug::new("acme", "api.v2").unwrap();

        assert_eq!(
            path_to_worktree_config(Path::new("/state"), &slug),
            PathBuf::from("/state/_codesense/worktree/github.com/acme/api.v2.yaml")
        );
    }

    #[test]
    fn epic_classifier_treats_hosts_as_opaque_segments() {
        let coords = classify_epic_worktree_path(Path::new(
            "/state/_epic/acme/context/SW-1/gitlab/acme/api",
        ))
        .unwrap();

        assert_eq!(coords.epic_id, "SW-1");
        assert_eq!(coords.owner, "acme");
        assert_eq!(coords.repo, "api");

        let one_segment_org = classify_epic_worktree_path_with_branch(
            Path::new("/state/_epic/acme/foo/gitlab/acme/api"),
            Some("integration/foo"),
        )
        .unwrap();
        assert_eq!(one_segment_org.epic_id, "foo");
    }
}

#[cfg(test)]
mod default_branch_tests {
    use super::*;

    /// Init a git repo at `dir` with one commit on a branch named
    /// `initial_branch`, returning the commit id. Sets `init.defaultBranch`
    /// via an explicit `HEAD` symref so the branch name is deterministic.
    fn init_repo_with_branch(dir: &Path, initial_branch: &str) -> git2::Oid {
        let repo = git2::Repository::init(dir).unwrap();
        repo.set_head(&format!("refs/heads/{initial_branch}"))
            .unwrap();
        let mut index = repo.index().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let oid = repo
            .commit(
                Some(&format!("refs/heads/{initial_branch}")),
                &sig,
                &sig,
                "init",
                &tree,
                &[],
            )
            .unwrap();
        oid
    }

    #[test]
    fn resolves_from_local_head_symref() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_branch(dir.path(), "main");
        assert_eq!(resolve_default_branch(dir.path()).unwrap(), "main");
    }

    #[test]
    fn resolves_nonstandard_branch_via_head() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_branch(dir.path(), "trunk");
        // No origin/HEAD, HEAD → refs/heads/trunk, no conventional name.
        assert_eq!(resolve_default_branch(dir.path()).unwrap(), "trunk");
    }

    #[test]
    fn resolves_via_origin_head_symbolic_target() {
        let dir = tempfile::tempdir().unwrap();
        let oid = init_repo_with_branch(dir.path(), "master");
        let repo = git2::Repository::open(dir.path()).unwrap();
        // Simulate a clone whose upstream default is `master`: create the
        // remote-tracking ref + origin/HEAD pointing at it. Detach local
        // HEAD so step 1 (origin/HEAD) is what resolves, not step 2.
        repo.reference("refs/remotes/origin/master", oid, true, "")
            .unwrap();
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/master",
            true,
            "",
        )
        .unwrap();
        repo.set_head_detached(oid).unwrap();
        assert_eq!(resolve_default_branch(dir.path()).unwrap(), "master");
    }

    #[test]
    fn resolves_conventional_when_head_detached() {
        let dir = tempfile::tempdir().unwrap();
        let oid = init_repo_with_branch(dir.path(), "develop");
        let repo = git2::Repository::open(dir.path()).unwrap();
        repo.set_head_detached(oid).unwrap();
        // No origin/HEAD, HEAD detached → conventional chain finds `develop`.
        assert_eq!(resolve_default_branch(dir.path()).unwrap(), "develop");
    }

    #[test]
    fn errors_with_no_resolvable_branch() {
        let dir = tempfile::tempdir().unwrap();
        // Empty repo: no commits, no branches.
        git2::Repository::init(dir.path()).unwrap();
        let err = resolve_default_branch(dir.path()).unwrap_err();
        assert!(matches!(err, DefaultBranchError::NoDefaultBranch { .. }));
    }
}
