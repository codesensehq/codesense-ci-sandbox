// SPDX-License-Identifier: MIT
//! File capture — detects real files in story dirs and moves them to the
//! context repo. Replaces the original with a symlink so frameworks that
//! write stories to the source repo are transparently normalised into the
//! central context-repo storage.

use crate::context_repo::{REPOS_DIR, TEAM_CONTENT_DIR};

/// Extract `(team, owner, repo)` from a flat context-repo path (SLW-IN-7).
///
/// Matches two layouts under the flat `{org_root}/{team}/…` tree:
/// - Repo-scoped: `.../{team}/repos/{owner}/{repo}/...` → `(team, owner, repo)`
/// - Flat content: `.../{team}/content/...` → `(team, "", "")`
///
/// Anchors on the FIRST `content`/`repos` layer-marker after the org slug and
/// takes the team as the segment immediately before it. A team named `content`
/// or `repos` would break this anchor, which is why `validate_team_name_full`
/// reserves the layer-marker names (D-1 Class B).
///
/// The flat-content return uses empty strings for owner/repo because that data
/// no longer lives in the path — callers that need a `source_repo` slug (e.g.
/// the watcher's cache refresh path) derive it from the cache entry's
/// frontmatter (`CacheEntry::source_repo()`) when the parsed tuple's owner is
/// empty. This keeps the function a pure path parser: no frontmatter reads,
/// no cache access.
///
/// Returns `None` if the path doesn't match either layout.
pub(crate) fn parse_context_repo_parts(
    context_repo_path: &str,
) -> Option<(String, String, String)> {
    let location = crate::context_repo::context_attachments::location_for_known_path(
        std::path::Path::new(context_repo_path),
    )
    .ok()?;
    let context_repo_path = match location.as_ref() {
        Some(location) => context_repo_path.strip_prefix(location.attachment.root.to_str()?)?,
        None => context_repo_path,
    };
    let parts: Vec<&str> = context_repo_path
        .split('/')
        .filter(|p| !p.is_empty())
        .collect();

    // Anchor on the first `content`/`repos` layer-marker; the team is the
    // segment immediately before it.
    let marker_idx = parts
        .iter()
        .position(|p| *p == TEAM_CONTENT_DIR || *p == REPOS_DIR)?;
    if marker_idx == 0 {
        return None; // no team segment precedes the marker
    }
    let team = parts[marker_idx - 1].to_string();

    match parts[marker_idx] {
        TEAM_CONTENT_DIR => Some((team, String::new(), String::new())),
        REPOS_DIR => {
            let owner = parts.get(marker_idx + 1)?.to_string();
            let repo = parts.get(marker_idx + 2)?.to_string();
            Some((team, owner, repo))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_context_repo_parts tests ----

    #[test]
    fn parse_valid_context_repo_path() {
        let path = "/home/user/.codesense/users/42/orgs/acme/api/default/repos/alderco/codesense";
        let result = parse_context_repo_parts(path);
        assert_eq!(
            result,
            Some(("default".into(), "alderco".into(), "codesense".into()))
        );
    }

    #[test]
    fn parse_with_different_team_name() {
        let path = "/ctx/backend/repos/org/api";
        let result = parse_context_repo_parts(path);
        assert_eq!(result, Some(("backend".into(), "org".into(), "api".into())));
    }

    #[test]
    fn parse_returns_none_for_missing_repos_dir() {
        let path = "/ctx/default/other/org/repo";
        assert!(parse_context_repo_parts(path).is_none());
    }

    #[test]
    fn parse_returns_none_when_marker_has_no_team_before_it() {
        // SLW-IN-7: a path that begins with a layer-marker (no team segment
        // before it) has no derivable team → None.
        let path = "/repos/org/repo";
        assert!(parse_context_repo_parts(path).is_none());
    }

    #[test]
    fn parse_team_named_like_marker_is_unreachable_reservation_holds() {
        // SLW-IN-7 D-1 Class B: `validate_team_name_full` reserves the marker
        // names so a team can never be literally `content`/`repos`. This proves
        // the anchor's assumption: given a legal flat path the FIRST marker is
        // always the layer, and the segment before it is the team. (A path where
        // the team were `content` would mis-anchor — which is exactly why the
        // name is reserved at creation.)
        let path = "/ctx/backend/content/_bmad-output/story.md";
        assert_eq!(
            parse_context_repo_parts(path),
            Some(("backend".into(), String::new(), String::new()))
        );
    }

    #[test]
    fn parse_returns_none_for_epic_workspace_path() {
        // SW-EW-7 AC-4: an `_epic/` workspace path carries no `_team` segment, so
        // the parser returns None and every watcher caller safely skips it (no
        // crash, no misroute). The watcher derives team from the paired stored
        // `context_repo_path`, never from an epic path — so no parser change is
        // needed; the agent-write round-trip (reverse watch) is EW-8's job.
        // SW-CP-15 Task 10: epic workspaces now root under the per-user home
        // (`users/{uid}/_epic/{org_slug}/{id}/`), outside any context-repo clone —
        // still `_team`-free, so the parser still returns None.
        let path = "/home/dev/.codesense/users/42/_epic/acme/codesense-context/SW-EW/owner/repo/_bmad-output/story.md";
        assert!(parse_context_repo_parts(path).is_none());
    }

    #[test]
    fn parse_with_trailing_slash() {
        let path = "/ctx/default/repos/org/repo/";
        let result = parse_context_repo_parts(path);
        assert_eq!(
            result,
            Some(("default".into(), "org".into(), "repo".into()))
        );
    }

    #[test]
    fn parse_with_double_slashes() {
        let path = "/ctx//default/repos/org/repo";
        // Double slash produces empty segment which is filtered out
        let result = parse_context_repo_parts(path);
        assert_eq!(
            result,
            Some(("default".into(), "org".into(), "repo".into()))
        );
    }

    #[test]
    fn parse_returns_none_for_truncated_path() {
        let path = "/ctx/default/repos/org";
        assert!(parse_context_repo_parts(path).is_none());
    }

    #[test]
    fn parse_flat_layout_returns_team_and_empty_owner_repo() {
        let path = "/ctx/default/content/impl/in-progress/F-1.md";
        let result = parse_context_repo_parts(path);
        assert_eq!(
            result,
            Some(("default".into(), String::new(), String::new()))
        );
    }

    #[test]
    fn parse_flat_layout_at_content_root() {
        let path = "/ctx/default/content";
        let result = parse_context_repo_parts(path);
        assert_eq!(
            result,
            Some(("default".into(), String::new(), String::new()))
        );
    }
}
