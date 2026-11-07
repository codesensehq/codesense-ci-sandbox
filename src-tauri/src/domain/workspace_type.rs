// SPDX-License-Identifier: MIT
//! Workspace-type identity for synthetic content-engine targets (SW-EW-5).
//!
//! A *workspace* is a synthetic content-engine target that is not itself a
//! git clone — the epic workspace dir is the first one. It carries a
//! [`SourceRepoSlug`] whose `owner` is a **reserved sentinel** that can never
//! be a real GitHub owner (GitHub owners cannot contain `_`), so a synthetic
//! workspace slug is always distinguishable from a git-derived slug
//! ([`derive_slug_from_repo`](crate::operations::git_remote::derive_slug_from_repo))
//! on disk, with no extra record and no parameter threading.
//!
//! [`WorkspaceType`] is the **single source of truth** for the sentinel
//! mapping. Today it has one variant; future variants (`Qa`, …) add a
//! sentinel arm and stay purely additive. Behavioral dispatch (assembly,
//! inheritance, teardown) keys off the slug discriminator
//! ([`is_epic_workspace_slug`]), never a `match` on this enum — so adding a
//! variant never forces a sweep of behavioral call sites.

use std::path::Path;

use super::error::DomainError;
use super::source_repo_slug::SourceRepoSlug;

/// Reserved slug-owner sentinel marking a synthetic *epic* workspace.
///
/// `_epic` is a legal slug segment (underscores are accepted by
/// `SourceRepoSlug` validation) yet an illegal GitHub owner, so it can be
/// constructed but can never collide with a real `owner/repo`.
pub const EPIC_OWNER_SENTINEL: &str = "_epic";

/// The type of a synthetic workspace target.
///
/// `Epic` is the first variant. The enum owns the owner-sentinel mapping; it
/// is intentionally **not** matched for behavioral dispatch (see module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceType {
    /// An epic workspace — the dir holding per-repo integration worktrees plus
    /// the team's inherited install + projected context (SW-EW-5..8).
    Epic,
}

impl WorkspaceType {
    /// The reserved slug-owner sentinel marking a synthetic workspace of this
    /// type on disk. This is the one place a `WorkspaceType` maps to its
    /// on-disk marker; the slug helpers below route through it.
    pub const fn owner_sentinel(self) -> &'static str {
        match self {
            WorkspaceType::Epic => EPIC_OWNER_SENTINEL,
        }
    }
}

/// Construct the synthetic slug for an epic workspace: `_epic/{epic_id}`.
///
/// Returns the constructor's [`DomainError`] when `epic_id` is empty,
/// whitespace-padded, contains control chars / path separators, or is `.`/`..`
/// — never panics. The Ungrouped sentinel (`""`) therefore surfaces as
/// `DomainError::EmptyField`, which callers treat as "not a registrable
/// workspace."
pub fn derive_epic_workspace_slug(
    epic_id: impl Into<String>,
) -> Result<SourceRepoSlug, DomainError> {
    SourceRepoSlug::new(WorkspaceType::Epic.owner_sentinel(), epic_id)
}

/// The durable, on-disk, background-derivable discriminator: is this slug a
/// synthetic epic workspace (vs a git-derived `owner/repo`)?
pub fn is_epic_workspace_slug(slug: &SourceRepoSlug) -> bool {
    slug.owner == WorkspaceType::Epic.owner_sentinel()
}

/// SLW-FX-23: is `(slug, code_repo_path)` a **synthetic (non-git) workspace
/// surface that reverse-sync should treat as live**?
///
/// Reverse-liveness historically keys on git-clone liveness — a repo is live
/// only if it holds an FS-watch subscriber whose slug derives from a git
/// remote. That structurally excludes an epic workspace (deliberately non-git:
/// a plain dir on an `integration/*` view, no remote), so edits authored there
/// never round-trip. This predicate is the single extension seam that admits a
/// synthetic surface: it is reverse-live iff its slug is a workspace sentinel
/// ([`is_epic_workspace_slug`] — generalizes to future [`WorkspaceType`]
/// variants) **and** its dir still exists on disk. The `is_dir` freshness guard
/// keeps a stale lockfile / closed-epic dir out of the sweep.
///
/// Takes the two primitive fields (not `AttachedCodeRepo`) so this domain-layer
/// predicate stays free of any `content_engine` dependency; the caller passes
/// `(&a.slug, &a.code_repo_path)`. The git-clone liveness rule in
/// `filter_live_repos` is unchanged — the reverse sweep composes this predicate
/// on top of it (see `content_engine::sync::wave::admit_synthetic_reverse_surfaces`).
pub fn is_synthetic_reverse_live(slug: &SourceRepoSlug, code_repo_path: &Path) -> bool {
    is_epic_workspace_slug(slug) && code_repo_path.is_dir()
}

/// SW-EW-15: derive a collision-resistant epic id from the epic doc's path.
///
/// The legacy id was the epic dir's LEAF name alone, so two epics with the same
/// leaf under different parents collided on disk (`_epic/{id}`) AND on the FE
/// integration id. Including the parent dir disambiguates them:
/// `{parent}__{leaf}` (e.g. `seamless-workspaces__epic-workspaces`), where
/// `leaf` is the epic doc's enclosing dir and `parent` is that dir's parent.
///
/// `__` is the join token because a path separator would be rejected by
/// [`SourceRepoSlug`] validation (the id becomes the `repo` of `_epic/{id}`).
/// Each segment is sanitized to a slug/filesystem-safe token via
/// [`sanitize_epic_id_segment`]; if the doc has no usable parent dir the bare
/// leaf is returned (still valid, just not parent-disambiguated). This is the
/// SINGLE derivation site — both `ensure_epic_workspace` and
/// `assemble_epic_worktrees` route through it so the dir they target always
/// agrees, and the FE never reproduces it (it reads the returned dir verbatim).
pub fn derive_epic_id_from_doc_path(epic_file_path: &Path) -> String {
    let dir = epic_file_path.parent();
    let leaf = dir
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .map(sanitize_epic_id_segment)
        .filter(|s| !s.is_empty());
    let parent = dir
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .map(sanitize_epic_id_segment)
        .filter(|s| !s.is_empty());
    match (parent, leaf) {
        (Some(p), Some(l)) => format!("{p}__{l}"),
        (None, Some(l)) => l,
        // Degenerate (doc path has no usable dir name): fall back to the file
        // stem so we still produce SOMETHING registrable rather than empty.
        _ => epic_file_path
            .file_stem()
            .and_then(|n| n.to_str())
            .map(sanitize_epic_id_segment)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "epic".to_string()),
    }
}

/// Sanitize one path component into a slug-, filesystem- AND git-ref-safe
/// token: trim, drop a whole-segment `.`/`..` to empty (callers filter empties),
/// and map every char outside `[A-Za-z0-9_-]` to `-`. The wider mapping matters
/// because the derived id ALSO becomes the `integration/{id}` branch ref
/// ([`derive_integration_branch`]); names that pass [`SourceRepoSlug`]
/// validation can still be invalid git refs. `.` is deliberately NOT preserved
/// — git rejects refs containing `..`, a component starting/ending with `.`, or
/// ending in `.lock`, so mapping every `.` to `-` makes the whole class safe
/// (e.g. `v1..2` → `v1-2`, `epic.lock` → `epic-lock`, `.hidden` → `-hidden`).
/// Common slug-like names (`epic-workspaces`, `SW-EW-15`) are unchanged.
fn sanitize_epic_id_segment(seg: &str) -> String {
    let trimmed = seg.trim();
    if trimmed == "." || trimmed == ".." {
        return String::new();
    }
    trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Prefix for an epic's derived integration branch. An epic with no explicit
/// `branch:` frontmatter materializes its worktrees on
/// `{INTEGRATION_BRANCH_PREFIX}{epic-id}` (e.g. `integration/SW-EW`) — **never**
/// the repo default. Single source of truth shared by EW-5 (read) and EW-6
/// (worktree assembly / write-back).
pub const INTEGRATION_BRANCH_PREFIX: &str = "integration/";

/// Derive an epic's integration branch name from its id:
/// `{INTEGRATION_BRANCH_PREFIX}{epic_id}`.
pub fn derive_integration_branch(epic_id: &str) -> String {
    format!("{INTEGRATION_BRANCH_PREFIX}{epic_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epic_owner_sentinel_is_underscore_epic() {
        assert_eq!(EPIC_OWNER_SENTINEL, "_epic");
        assert_eq!(WorkspaceType::Epic.owner_sentinel(), "_epic");
    }

    #[test]
    fn derive_epic_workspace_slug_constructs_synthetic_slug() {
        let slug = derive_epic_workspace_slug("SW-EW").unwrap();
        assert_eq!(slug.owner, "_epic");
        assert_eq!(slug.repo, "SW-EW");
        assert_eq!(format!("{slug}"), "_epic/SW-EW");
    }

    #[test]
    fn synthetic_slug_is_discriminated_as_epic_workspace() {
        let slug = derive_epic_workspace_slug("SW-EW").unwrap();
        assert!(is_epic_workspace_slug(&slug));
    }

    #[test]
    fn git_derived_slug_is_not_an_epic_workspace() {
        // A real GitHub owner can never be `_epic` (underscores are illegal
        // in GitHub owners), so the discriminator is collision-proof.
        let git = SourceRepoSlug::new("alderco", "codesense").unwrap();
        assert!(!is_epic_workspace_slug(&git));
    }

    // SLW-FX-23 (AC-1): a synthetic epic surface whose dir exists on disk is
    // reverse-live even with no git-clone subscriber — the whole point of the
    // fix. This is the predicate the reverse sweep composes via
    // `admit_synthetic_reverse_surfaces`.
    #[test]
    fn synthetic_epic_surface_with_existing_dir_is_reverse_live() {
        let dir = tempfile::TempDir::new().unwrap();
        let slug = derive_epic_workspace_slug("SLW-FX").unwrap();
        assert!(is_synthetic_reverse_live(&slug, dir.path()));
    }

    // SLW-FX-23 (AC-2): a stale synthetic surface (closed epic / stale lockfile)
    // whose dir no longer exists is NOT reverse-live — the `is_dir` freshness
    // guard keeps the sweep off a gone epic dir.
    #[test]
    fn stale_synthetic_epic_surface_missing_dir_is_not_reverse_live() {
        let slug = derive_epic_workspace_slug("SLW-FX").unwrap();
        let gone = Path::new("/nonexistent/epic/dir/slw-fx-23");
        assert!(!is_synthetic_reverse_live(&slug, gone));
    }

    // SLW-FX-23 (AC-3): a git-derived surface is NEVER admitted by the synthetic
    // rule, even when its dir exists — git clones are decided solely by the
    // subscriber-match branch, which this predicate leaves untouched.
    #[test]
    fn git_surface_is_never_synthetic_reverse_live() {
        let dir = tempfile::TempDir::new().unwrap();
        let git = SourceRepoSlug::new("alderco", "codesense").unwrap();
        assert!(!is_synthetic_reverse_live(&git, dir.path()));
    }

    #[test]
    fn empty_epic_id_returns_error_never_panics() {
        // The Ungrouped sentinel ("") must not register and must not panic.
        let err = derive_epic_workspace_slug("").unwrap_err();
        assert_eq!(err, DomainError::EmptyField { field: "repo" });
    }

    #[test]
    fn slug_unsafe_epic_id_returns_error() {
        // A path separator in the epic id is rejected by the constructor.
        assert!(derive_epic_workspace_slug("a/b").is_err());
        assert!(derive_epic_workspace_slug("..").is_err());
    }

    #[test]
    fn integration_branch_derives_from_prefix_and_id() {
        assert_eq!(INTEGRATION_BRANCH_PREFIX, "integration/");
        assert_eq!(derive_integration_branch("SW-EW"), "integration/SW-EW");
    }

    #[test]
    fn epic_id_from_doc_path_joins_parent_and_leaf() {
        let id = derive_epic_id_from_doc_path(Path::new(
            "/ctx/content/seamless-workspaces/epic-workspaces/README.md",
        ));
        assert_eq!(id, "seamless-workspaces__epic-workspaces");
    }

    #[test]
    fn same_leaf_different_parent_yields_distinct_ids() {
        let a = derive_epic_id_from_doc_path(Path::new("/ctx/planning/epics/foo/epic.md"));
        let b = derive_epic_id_from_doc_path(Path::new("/ctx/docs/epics/foo/epic.md"));
        // Both leaves are `foo`, but the parent dir disambiguates them.
        assert_eq!(a, "epics__foo");
        assert_eq!(b, "epics__foo");
        // ...and a genuinely different parent gives a genuinely different id.
        let c = derive_epic_id_from_doc_path(Path::new("/ctx/planning/foo/epic.md"));
        assert_eq!(c, "planning__foo");
        assert_ne!(a, c);
    }

    #[test]
    fn derived_id_always_passes_slug_construction() {
        for p in [
            "/ctx/a b/epic dir/README.md",
            "/ctx/weird.dir/leaf/epic.md",
            "/leaf-only/epic.md",
            "epic.md",
        ] {
            let id = derive_epic_id_from_doc_path(Path::new(p));
            assert!(!id.is_empty(), "empty id for {p}");
            // The whole point: the derived id is a valid `_epic/{id}` slug repo.
            assert!(
                derive_epic_workspace_slug(&id).is_ok(),
                "id {id:?} (from {p}) is not slug-safe",
            );
        }
    }

    #[test]
    fn sanitizes_dot_segments_and_separators() {
        // A `.`/`..` dir segment is dropped; the remaining segment carries the
        // id rather than producing an invalid `..`-bearing slug.
        assert_eq!(sanitize_epic_id_segment(".."), "");
        assert_eq!(sanitize_epic_id_segment("."), "");
        assert_eq!(sanitize_epic_id_segment(" leaf "), "leaf");
    }

    #[test]
    fn sanitizes_to_git_ref_safe_token() {
        // Spaces and git-ref metacharacters map to `-` so the derived id is a
        // valid `integration/{id}` branch ref, not just a valid slug.
        assert_eq!(sanitize_epic_id_segment("a b"), "a-b");
        assert_eq!(sanitize_epic_id_segment("we~ird^na:me"), "we-ird-na-me");
        let id = derive_epic_id_from_doc_path(Path::new("/ctx/my epics/epic one/epic.md"));
        assert_eq!(id, "my-epics__epic-one");
        // The branch derived from a spaced dir name is now git-ref-safe.
        let branch = derive_integration_branch(&id);
        assert!(
            !branch.contains(' '),
            "branch {branch:?} must have no spaces"
        );
    }

    #[test]
    fn sanitizes_dot_shapes_that_git_rejects_in_refs() {
        // `..`, leading/trailing dot, and `.lock` suffix all make a git ref
        // invalid even though they pass slug validation — every `.` maps to `-`.
        assert_eq!(sanitize_epic_id_segment("v1..2"), "v1--2");
        assert_eq!(sanitize_epic_id_segment(".hidden"), "-hidden");
        assert_eq!(sanitize_epic_id_segment("foo."), "foo-");
        assert_eq!(sanitize_epic_id_segment("epic.lock"), "epic-lock");
        for p in ["/ctx/my..epics/foo/epic.md", "/ctx/.hidden/leaf/epic.md"] {
            let id = derive_epic_id_from_doc_path(Path::new(p));
            let branch = derive_integration_branch(&id);
            assert!(
                !branch.contains(".."),
                "branch {branch:?} must not contain .."
            );
            assert!(derive_epic_workspace_slug(&id).is_ok());
        }
    }

    #[test]
    fn degenerate_path_falls_back_to_stem() {
        // No dir component at all → fall back to the file stem.
        let id = derive_epic_id_from_doc_path(Path::new("epic.md"));
        assert_eq!(id, "epic");
    }
}
