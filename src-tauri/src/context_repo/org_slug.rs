// SPDX-License-Identifier: MIT
//! Canonical org-slug validation gate.
//!
//! The org slug identifies an org's storage under `.../users/{uid}/orgs/`
//! and `.../users/{uid}/installs/`. SW-CR-1 relaxed the slug to one or two
//! safe `/`-separated segments — a bare `<name>` for a user-chosen attach,
//! or the GitHub-shaped `<owner>/<repo>` produced by
//! `normalize_source_repo_url` for a source-repo-derived identity.
//!
//! [`validate_org_slug`] is the canonical path-safety gate. Reuses
//! [`validate_path_segment`] for each side so the safe-segment rules
//! live in exactly one place.
//!
//! [`validate_path_segment`]: crate::content_engine::layers::validate_path_segment

use crate::content_engine::layers::validate_path_segment;

use super::scoped_key::{BUILTIN_ORG, PERSONAL_OWNER};

/// Validate `slug` as a safe org slug for use under `{codesense_home}/
/// users/{id}/orgs/` (and the sibling `installs/`). The slug is one or two
/// `/`-separated segments: a bare `<name>` is honored for a local or
/// user-chosen attach, and `<owner>/<repo>` is honored for a GitHub-shaped
/// slug. Each segment is validated via [`validate_path_segment`] so the
/// safe-segment rules live in one place.
///
/// Rejects: empty input, more than two `/`-separated segments, and any
/// segment that fails [`validate_path_segment`] (so `..`, NUL, control
/// chars, whitespace, and empty segments are all caught per-side —
/// `""`, `"a/b/c"`, `"/repo"`, `"owner/"`, `"owner/../etc"` all reject).
///
/// Also reserves three bare single-segment slugs — `_personal`, `builtin`,
/// `local` — so a mint or attach cannot claim an owner-prefix the rest of
/// the layout reads as a nested-shape owner directory. SW-CR-5: the match is
/// case-insensitive (ASCII) so a user-authored `org.yml` carrying slug
/// `"Builtin"` or `"Local"` cannot alias onto the reserved namespace on
/// macOS/Windows case-folding filesystems. Two-segment slugs led by these
/// owners (e.g. the v0.0.11 migration's `local/<name>`) stay valid — only
/// the bare single-segment form is reserved.
pub fn validate_org_slug(slug: &str) -> Option<&'static str> {
    let mut count = 0usize;
    for segment in slug.split('/') {
        if let Some(reason) = validate_path_segment(segment) {
            return Some(reason);
        }
        count += 1;
    }
    if count > 2 {
        return Some("too many '/' segments");
    }
    if count == 1
        && (slug.eq_ignore_ascii_case(PERSONAL_OWNER)
            || slug.eq_ignore_ascii_case(BUILTIN_ORG)
            || slug.eq_ignore_ascii_case("local"))
    {
        return Some("reserved owner prefix cannot be a single-segment slug");
    }
    None
}

/// Boundary shim: validate an `org_slug` command argument, mapping the
/// canonical [`validate_org_slug`] rejection reason into the
/// `"invalid org_slug: {reason}"` error string the frontend dispatches on.
///
/// Mirrors the `validate_github_name` → `validate_github_name_pub` shim
/// pattern it replaces at the registry-edit Tauri boundaries
/// (`commit_registry_edit_local`, `commit_registry_edit_as_pr`): the two
/// boundaries validated the wrong thing (a single-segment GitHub *name*
/// check that rejects the legitimate two-segment `owner/repo` slug). Both
/// now call this one shim so the FE error contract lives in exactly one
/// place. Pure, allocation-light, `O(len)` string work — no I/O.
pub fn require_valid_org_slug(org_slug: &str) -> Result<(), String> {
    match validate_org_slug(org_slug) {
        Some(reason) => Err(format!("invalid org_slug: {reason}")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SW-CR-1: org slug is one or two `/`-separated segments.
    #[test]
    fn validate_accepts_safe_slugs() {
        for ok in [
            "acme/team-context",
            // repo half may contain interior `--` (a valid GitHub repo name
            // like `my--repo`); only the owner half is constrained not to by
            // GitHub's login rules.
            "alderco/my--repo",
            ".github/workflows-repo",
            // SW-CR-1: single-segment slugs are honored for user-chosen
            // attaches (`org.yml` picks the shape and the attach honors it).
            "alderco",
            "9north",
            "notes",
        ] {
            assert!(
                validate_org_slug(ok).is_none(),
                "expected {ok:?} to be accepted by validate_org_slug"
            );
        }
    }

    #[test]
    fn validate_rejects_unsafe_or_malformed() {
        for bad in [
            "",               // empty
            "a/b/c",          // too many segments
            "alderco/../etc", // too many segments (and traversal)
            "alderco/..",     // traversal in repo half
            "../escape",      // traversal in owner half
            "/codesense",     // empty owner
            "alderco/",       // empty repo
            "alderco/ repo",  // leading whitespace in repo half
            "ev\0il/repo",    // NUL in owner half
            // SW-CR-1 N2: single-segment unsafe cases.
            ".",         // traversal (bare)
            "..",        // traversal (bare)
            " ",         // whitespace-only
            " alderco",  // leading whitespace in single segment
            "al\0derco", // NUL in single segment
            // SW-CR-1 M2: single-segment reserved owner prefixes — bare
            // forms collide with the three owner-prefix namespaces.
            // SW-CR-5: the reservation is case-insensitive (ASCII) so a
            // mixed-case form on a case-folding filesystem can't alias.
            "_personal",
            "builtin",
            "local",
            "Builtin",
            "LOCAL",
            "_Personal",
        ] {
            assert!(
                validate_org_slug(bad).is_some(),
                "expected {bad:?} to be rejected by validate_org_slug"
            );
        }
    }

    // SW-CR-1 M2: two-segment slugs led by a reserved owner stay valid —
    // the migration still rewrites v0.0.11 single-segment orgs to
    // `local/<name>`, and the agent/routines service's own `check_org` is
    // the one that rejects a `builtin/<x>` team org (not this gate).
    #[test]
    fn validate_accepts_two_segment_reserved_owner_prefixes() {
        for ok in ["local/my-workspace", "builtin/foo", "_personal/42"] {
            assert!(
                validate_org_slug(ok).is_none(),
                "expected {ok:?} to be accepted by validate_org_slug"
            );
        }
    }

    // SW-TM-1: the registry-edit boundaries call `require_valid_org_slug`, not
    // the single-segment GitHub-name checker that broke team creation. This
    // pins the *wiring + FE error contract* (accepts safe slugs; rejections
    // keep the `"invalid org_slug:"` prefix the frontend dispatches on) — the
    // exhaustive accept/reject vectors live in the `validate_*` tests above.
    #[test]
    fn require_valid_org_slug_accepts_owner_repo_and_prefixes_rejections() {
        // Accepts the real `owner/repo` slug shape that the pre-SS-FU-12 gate
        // wrongly rejected.
        assert!(require_valid_org_slug("acme/team-context").is_ok());
        // Still refuses unsafe/malformed input, with the prefix intact.
        let err = require_valid_org_slug("a/b/c/d").unwrap_err();
        assert!(err.starts_with("invalid org_slug:"), "got {err:?}");
    }

    // SLW-PT-1: the reserved personal slug is recognized by the carve-out
    // guard and cannot be a real `owner/repo` clone (leading underscore is
    // GitHub-impossible).
}
