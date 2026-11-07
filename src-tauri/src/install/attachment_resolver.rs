// SPDX-License-Identifier: MIT
//! SW-BSI-3: resolve a managed context's logical path to the story whose
//! worktree subtree owns it. Powers the inbox-attachment hydration-state
//! check: an attachment that is not under any linked story subtree is
//! unknown, and one that is maps to a story-scoped hydration primitive.

use std::path::Path;

use crate::context_repo::path_to_org_root;
use crate::domain::Principal;
use crate::install::worktree_index::iter_story_records;

/// The story_id owning `logical_path` within `(org_slug, team)`, if any
/// linked worktree record's epic subtree is an ancestor of the path.
///
/// Returns `None` when no record is found or the path is outside every
/// known epic directory. A silent `None` on `iter_records` failure is
/// intentional: a transient index-read error on this read-only lookup
/// should degrade to "unknown owner" rather than refuse the whole status
/// response.
pub(crate) fn owning_story_for_path(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    team: &str,
    logical_path: &str,
) -> Option<String> {
    let org_root = path_to_org_root(codesense_home, principal, org_slug).ok()?;
    let content_root = org_root.join(team).join("content");
    let abs_path = crate::context_repo::inheritance::normalize(&content_root.join(logical_path));
    if !matches!(
        crate::context_repo::inheritance::team_view(&org_root, org_slug, team)
            .admits_path(&abs_path),
        crate::context_repo::inheritance::Admit::Own
            | crate::context_repo::inheritance::Admit::Inherited { .. }
    ) {
        return None;
    }

    let records = iter_story_records(codesense_home, principal).ok()?;
    for record in records {
        if record.org_slug != org_slug
            || !matches!(
                crate::context_repo::inheritance::team_view(&org_root, org_slug, team)
                    .admits_path(&record.story_source_path),
                crate::context_repo::inheritance::Admit::Own
                    | crate::context_repo::inheritance::Admit::Inherited { .. }
            )
        {
            continue;
        }
        let Some(epic_dir) = record.story_source_path.parent() else {
            continue;
        };
        if abs_path.starts_with(epic_dir) {
            return Some(record.story_id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_repo::USERS_DIR;
    use crate::domain::{Principal, SourceRepoSlug};
    use crate::install::worktree_index::{write_record, WorktreeStoryRecord};

    fn alice() -> Principal {
        Principal::parse_local("1").unwrap()
    }

    fn seed_org_root(home: &std::path::Path, principal: &Principal, org: &str, team: &str) {
        let root = home
            .join(USERS_DIR)
            .join(principal.as_str())
            .join("orgs")
            .join(org)
            .join(team)
            .join("content");
        std::fs::create_dir_all(&root).unwrap();
    }

    #[test]
    fn resolves_to_the_story_whose_epic_subtree_owns_the_path() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let principal = alice();
        let org = "acme/widgets-context";
        let team = "backend";
        seed_org_root(home, &principal, org, team);

        // A record for SW-BSI-3 whose story file sits in epic `25-builder-skills-integration`.
        let content_root = home
            .join(USERS_DIR)
            .join(principal.as_str())
            .join("orgs")
            .join(org)
            .join(team)
            .join("content");
        let epic_dir = content_root
            .join("_bmad-output")
            .join("08-dashboards-and-agents")
            .join("25-builder-skills-integration");
        std::fs::create_dir_all(&epic_dir).unwrap();
        let story_source_path = epic_dir.join("SW-BSI-3-remote-leg.md");
        std::fs::write(&story_source_path, "---\ntype: story\n---\n").unwrap();

        let worktree_dir = tmp.path().join("worktrees").join("SW-BSI-3");
        std::fs::create_dir_all(&worktree_dir).unwrap();
        let record = WorktreeStoryRecord::new(
            worktree_dir,
            org,
            SourceRepoSlug::new("acme", "widgets").unwrap(),
            "SW-BSI-3",
            &story_source_path,
        );
        write_record(home, &principal, &record).unwrap();

        // A file INSIDE the epic subtree resolves to the owning story.
        let logical = "_bmad-output/08-dashboards-and-agents/25-builder-skills-integration/trial/live-digest.html";
        assert_eq!(
            owning_story_for_path(home, &principal, org, team, logical),
            Some("SW-BSI-3".to_string())
        );

        // A file OUTSIDE every linked epic resolves to None.
        let unknown = "docs/unrelated.md";
        assert_eq!(
            owning_story_for_path(home, &principal, org, team, unknown),
            None
        );

        // A file under the epic directory itself is also owned.
        let direct_epic_child =
            "_bmad-output/08-dashboards-and-agents/25-builder-skills-integration/README.md";
        assert_eq!(
            owning_story_for_path(home, &principal, org, team, direct_epic_child),
            Some("SW-BSI-3".to_string())
        );
    }

    #[test]
    fn org_scope_isolates_records_from_other_orgs() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let principal = alice();
        seed_org_root(home, &principal, "acme/one", "backend");
        seed_org_root(home, &principal, "acme/two", "backend");

        for org in ["acme/one", "acme/two"] {
            let epic_dir = home
                .join(USERS_DIR)
                .join(principal.as_str())
                .join("orgs")
                .join(org)
                .join("backend")
                .join("content")
                .join("_bmad-output")
                .join("epic");
            std::fs::create_dir_all(&epic_dir).unwrap();
            let story_source_path = epic_dir.join(format!("STORY-{}.md", org.replace('/', "-")));
            std::fs::write(&story_source_path, "---\ntype: story\n---\n").unwrap();
            let worktree_dir = tmp.path().join("wt").join(org.replace('/', "-"));
            std::fs::create_dir_all(&worktree_dir).unwrap();
            let record = WorktreeStoryRecord::new(
                worktree_dir,
                org,
                SourceRepoSlug::new("acme", "widgets").unwrap(),
                format!("STORY-{}", org.replace('/', "-")),
                &story_source_path,
            );
            write_record(home, &principal, &record).unwrap();
        }

        // The resolver only sees the queried org's records.
        assert_eq!(
            owning_story_for_path(
                home,
                &principal,
                "acme/one",
                "backend",
                "_bmad-output/epic/supporting.md"
            ),
            Some("STORY-acme-one".to_string())
        );
        assert_eq!(
            owning_story_for_path(
                home,
                &principal,
                "acme/two",
                "backend",
                "_bmad-output/epic/supporting.md"
            ),
            Some("STORY-acme-two".to_string())
        );
    }
}

#[cfg(test)]
mod inheritance_tests {
    use super::*;
    #[test]
    fn attachment_parent_paths_cannot_disclose_private_story_names() {
        use crate::install::worktree_index::{write_record, WorktreeStoryRecord};
        let (home, root) = crate::context_repo::inheritance::test_fixture();
        let me = Principal::from(1u64);
        let source = root.join("parent/content/secret.md");
        let worktree = home.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        let record = WorktreeStoryRecord::new(
            worktree,
            "acme/ctx",
            crate::domain::SourceRepoSlug::new("acme", "api").unwrap(),
            "PRIVATE-STORY",
            &source,
        );
        write_record(home.path(), &me, &record).unwrap();
        assert_eq!(
            owning_story_for_path(home.path(), &me, "acme/ctx", "parent", "secret.md"),
            Some("PRIVATE-STORY".into())
        );
        let shared_worktree = home.path().join("shared-worktree");
        std::fs::create_dir_all(&shared_worktree).unwrap();
        let shared_record = WorktreeStoryRecord::new(
            shared_worktree,
            "acme/ctx",
            crate::domain::SourceRepoSlug::new("acme", "api").unwrap(),
            "SHARED-STORY",
            root.join("parent/content/shared.md"),
        );
        write_record(home.path(), &me, &shared_record).unwrap();
        assert_eq!(
            owning_story_for_path(
                home.path(),
                &me,
                "acme/ctx",
                "child",
                "../../parent/content/secret.md"
            ),
            None
        );
        assert_eq!(
            owning_story_for_path(
                home.path(),
                &me,
                "acme/ctx",
                "child",
                "../../parent/content/shared.md"
            ),
            Some("SHARED-STORY".into())
        );
    }
}
