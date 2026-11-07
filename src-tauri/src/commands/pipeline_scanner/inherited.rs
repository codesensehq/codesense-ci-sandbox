// SPDX-License-Identifier: MIT
//! SW-XS-20: files a team inherits from its parent teams, in the nav scan.
//!
//! The nav scans the active team's own `content/` and overlays only ancestor
//! files admitted by [`crate::context_repo::inheritance`], nearest team first.
//! Each inherited file keeps its owning team in [`DocRef::source_team`].
//!
//! - A file the team has itself always wins (it is the team's own, no marker).
//!   That includes a merge-marked config file: the nav shows the team's own
//!   copy, while an agent gets the merge of the chain's copies.
//! - A parent's file agent context would never carry is left out the same way:
//!   the parent's projection excludes and the files projection never carries.
//! - Inherited files enter as plain docs, never stories, so the team's
//!   pipeline graph stays its own.
//! - Each inherited FILE keeps its real path in the parent team, so editing,
//!   renaming or deleting it acts on the parent's file: a child team's view
//!   edits its parent's content in place (nothing syncs it automatically; it is
//!   local content).
//! - Folders are not inherited: a folder is one merged view across the chain.
//!   A folder only a parent has appears at its place in the team's own tree
//!   (with no owner), so a new file or a drop into it lands in the active
//!   team's copy of that folder, created on write.
//!
//! The chain is resolved once per scan.

use std::collections::HashSet;
use std::path::Path;

use super::{DerivedPipelineTree, DocRef};
use crate::agents::manifest_resolve::not_carried;
use crate::content_engine::{is_projection_excluded, TeamExcludes};
use crate::context_repo::inheritance::{content_owner, org_slug_of, team_view, ContentRoot};

/// Overlay admitted ancestor files, reading metadata only after admission.
/// Folder coordinates stay in the child's tree; files keep their real paths.
pub(super) fn overlay_inherited_teams(
    tree: &mut DerivedPipelineTree,
    team_content_path: &Path,
    pipeline_root: &str,
    mut read_doc: impl FnMut(&Path) -> Result<DocRef, String>,
) -> Result<(), String> {
    let Some((root, team)) = content_owner(team_content_path) else {
        return Ok(());
    };
    if root.join(&team).join("content") != team_content_path {
        return Ok(());
    }
    let Some(slug) = org_slug_of(&root) else {
        return Ok(());
    };
    let view = team_view(&root, &slug, &team);
    let display_root = team_content_path.join(pipeline_root);
    let scope = Path::new(pipeline_root).join(&tree.dir_path);
    for layer in &view.inherited {
        let content = layer.content();
        let carried = Carried {
            root: &content,
            excludes: &content.excludes(),
        };
        let mut parent_tree = empty_tree(&display_root, &tree.dir_path);
        let mut files = layer.walk("content");
        files.sort_by(|a, b| super::natural_cmp(&a.0, &b.0));
        for (rel, path) in files {
            let Some(logical) = rel.strip_prefix("content/") else {
                continue;
            };
            let Ok(scoped) = Path::new(logical).strip_prefix(&scope) else {
                continue;
            };
            let Some(scoped) = scoped.to_str() else {
                continue;
            };
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if scoped.split('/').any(|part| part.starts_with('.'))
                || (!super::has_pipeline_extension(&name) && !super::is_supporting_doc(&name))
            {
                continue;
            }
            let mut folder = &mut parent_tree;
            let mut parts = scoped.split('/').peekable();
            let mut dir = tree.dir_path.clone();
            while let Some(name) = parts.next() {
                if parts.peek().is_none() {
                    folder.docs.push(read_doc(&path)?);
                } else {
                    dir = if dir.is_empty() {
                        name.into()
                    } else {
                        format!("{dir}/{name}")
                    };
                    let index = folder
                        .sub_epics
                        .iter()
                        .position(|f| f.dir_name == name)
                        .unwrap_or_else(|| {
                            folder.sub_epics.push(empty_tree(&display_root, &dir));
                            folder.sub_epics.len() - 1
                        });
                    folder = &mut folder.sub_epics[index];
                }
            }
        }
        merge_inherited(tree, parent_tree, layer.team(), &carried);
    }
    Ok(())
}
fn empty_tree(root: &Path, dir: &str) -> DerivedPipelineTree {
    DerivedPipelineTree {
        dir_path: dir.into(),
        dir_name: Path::new(dir)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        absolute_dir_path: root.join(dir).to_string_lossy().into_owned(),
        stories: Vec::new(),
        docs: Vec::new(),
        sub_epics: Vec::new(),
        order: None,
        order_error: None,
        main_doc: None,
    }
}
struct Carried<'a> {
    root: &'a ContentRoot,
    excludes: &'a TeamExcludes,
}
impl Carried<'_> {
    fn carries(&self, file_path: &str) -> bool {
        let Some(rel) = self.root.relative_for_read(Path::new(file_path)) else {
            return false;
        };
        let Some(logical) = rel.to_str() else {
            return false;
        };
        !not_carried(logical) && !is_projection_excluded(logical, self.excludes)
    }
}

/// Add to `own` every file and folder of `inherited` (the same folder in a
/// parent team) that `own` does not already have, marked with `team`.
fn merge_inherited(
    own: &mut DerivedPipelineTree,
    inherited: DerivedPipelineTree,
    team: &str,
    carried: &Carried,
) {
    let mut taken: HashSet<String> = own
        .docs
        .iter()
        .map(|d| d.file_name.clone())
        .chain(own.stories.iter().map(|s| s.file_name.clone()))
        .chain(
            own.main_doc
                .as_ref()
                .and_then(|m| file_name_of(&m.file_path)),
        )
        .collect();
    for doc in inherited_docs(&inherited, team, carried) {
        if taken.insert(doc.file_name.clone()) {
            own.docs.push(doc);
        } else if let Some(own_doc) = own
            .docs
            .iter_mut()
            .find(|d| d.file_name == doc.file_name && d.source_team.is_none())
        {
            // The team's own doc overrides this parent's file; nearest wins.
            own_doc
                .overrides_team
                .get_or_insert_with(|| team.to_string());
        }
    }
    for sub in inherited.sub_epics {
        if let Some(own_sub) = own
            .sub_epics
            .iter_mut()
            .find(|s| s.dir_name == sub.dir_name)
        {
            merge_inherited(own_sub, sub, team, carried);
        } else {
            let at = Path::new(&own.absolute_dir_path).join(&sub.dir_name);
            if let Some(folder) = adopt_folder(sub, &at, team, carried) {
                own.sub_epics.push(folder);
            }
        }
    }
}

/// A folder only a parent team has, placed at `at` — its place in the team's
/// own tree, so a new file or a drop into it lands in the team's copy — with
/// every file in it inherited from `team`. `None` when nothing in it is
/// carried.
fn adopt_folder(
    folder: DerivedPipelineTree,
    at: &Path,
    team: &str,
    carried: &Carried,
) -> Option<DerivedPipelineTree> {
    let docs = inherited_docs(&folder, team, carried);
    let sub_epics: Vec<_> = folder
        .sub_epics
        .into_iter()
        .filter_map(|sub| {
            let sub_at = at.join(&sub.dir_name);
            adopt_folder(sub, &sub_at, team, carried)
        })
        .collect();
    if docs.is_empty() && sub_epics.is_empty() {
        return None;
    }
    Some(DerivedPipelineTree {
        dir_path: folder.dir_path,
        dir_name: folder.dir_name,
        absolute_dir_path: at.display().to_string(),
        stories: Vec::new(),
        docs,
        sub_epics,
        order: None,
        order_error: None,
        main_doc: None,
    })
}

/// Every carried file directly in `folder` as an inherited doc: its docs, its
/// stories (shown as files, kept out of the team's pipeline) and its main doc.
/// Each opens where it lives in the parent team.
fn inherited_docs(folder: &DerivedPipelineTree, team: &str, carried: &Carried) -> Vec<DocRef> {
    let docs = folder.docs.iter().map(|d| DocRef {
        source_team: Some(team.to_string()),
        overrides_team: None,
        ..d.clone()
    });
    let stories = folder.stories.iter().map(|s| DocRef {
        file_name: s.file_name.clone(),
        file_path: s.file_path.clone(),
        metadata_valid: s.metadata_valid,
        metadata_invalid_reason: s.metadata_invalid_reason.clone(),
        subject_id: None,
        source_team: Some(team.to_string()),
        overrides_team: None,
    });
    let main = folder.main_doc.iter().filter_map(|m| {
        Some(DocRef {
            file_name: file_name_of(&m.file_path)?,
            file_path: m.file_path.clone(),
            metadata_valid: true,
            metadata_invalid_reason: None,
            subject_id: None,
            source_team: Some(team.to_string()),
            overrides_team: None,
        })
    });
    docs.chain(stories)
        .chain(main)
        .filter(|d| carried.carries(&d.file_path))
        .collect()
}

fn file_name_of(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::super::{
        default_summary_headings, scan_pipeline_tree_and_heal_index, MetadataValidityConfig,
    };
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// An org at `root` whose `teams.yml` is `teams_yml`, with each team's
    /// content dir created.
    fn write_org(root: &Path, teams_yml: &str, teams: &[&str]) {
        write(
            &root.join("org.yml"),
            "slug: acme/ctx\ndisplay_name: Acme\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
        );
        write(
            &root.join("people.yml"),
            "people:\n  - github_user_id: 1\n    github_username: alice\n",
        );
        write(&root.join("teams.yml"), teams_yml);
        for team in teams {
            std::fs::create_dir_all(root.join(team).join("content")).unwrap();
            write(
                &root.join(team).join("config/sharing.yml"),
                "share: ['content/**']\n",
            );
        }
    }

    fn scan(content: &Path) -> DerivedPipelineTree {
        scan_scope(content, "")
    }

    fn scan_scope(content: &Path, scope: &str) -> DerivedPipelineTree {
        scan_pipeline_tree_and_heal_index(
            None,
            content,
            "",
            scope,
            "",
            &default_summary_headings(),
            &MetadataValidityConfig::default(),
        )
        .unwrap()
    }

    fn sub<'a>(tree: &'a DerivedPipelineTree, name: &str) -> &'a DerivedPipelineTree {
        tree.sub_epics
            .iter()
            .find(|s| s.dir_name == name)
            .unwrap_or_else(|| panic!("no folder {name} in {tree:#?}"))
    }

    fn doc<'a>(tree: &'a DerivedPipelineTree, name: &str) -> &'a DocRef {
        tree.docs
            .iter()
            .find(|d| d.file_name == name)
            .unwrap_or_else(|| panic!("no doc {name} in {tree:#?}"))
    }

    const STORY: &str = "---\ntype: story\nstatus: todo\n---\n# s\n";

    #[test]
    fn scan_fills_source_team_for_files_inherited_from_parent_teams() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_org(
            root,
            "teams:\n  - name: default\n    parent_team: parent\n  - name: parent\n    parent_team: grand\n  - name: grand\n",
            &["default", "parent", "grand"],
        );
        let own = root.join("default/content");
        let parent = root.join("parent/content");
        let grand = root.join("grand/content");
        write(&own.join("epic/own.md"), "# own\n");
        write(&own.join("epic/shared.md"), "# overridden here\n");
        write(&parent.join("epic/shared.md"), "# parent version\n");
        write(&parent.join("epic/only-parent.md"), "# parent\n");
        write(&parent.join("epic/parent-story.md"), STORY);
        write(&parent.join("rules/deep/rule.md"), "# rule\n");
        write(
            &grand.join("epic/only-parent.md"),
            "# grand, shadowed by parent\n",
        );
        write(&grand.join("epic/only-grand.md"), "# grand\n");

        let tree = scan(&own);
        let epic = sub(&tree, "epic");

        // Own files, including the override of a parent file: no marker.
        assert_eq!(doc(epic, "own.md").source_team, None);
        let shared = doc(epic, "shared.md");
        assert_eq!(shared.source_team, None);
        // ...and says which parent's file it overrides.
        assert_eq!(shared.overrides_team.as_deref(), Some("parent"));
        assert_eq!(doc(epic, "own.md").overrides_team, None);
        assert_eq!(
            shared.file_path,
            own.join("epic/shared.md").display().to_string()
        );
        assert_eq!(
            epic.docs
                .iter()
                .filter(|d| d.file_name == "shared.md")
                .count(),
            1
        );

        // A file only the parent has: inherited from the parent, opened where it lives.
        let only_parent = doc(epic, "only-parent.md");
        assert_eq!(only_parent.source_team.as_deref(), Some("parent"));
        assert_eq!(
            only_parent.file_path,
            parent.join("epic/only-parent.md").display().to_string()
        );
        // The nearest team wins down the chain.
        assert_eq!(
            doc(epic, "only-grand.md").source_team.as_deref(),
            Some("grand")
        );
        // A parent's story is shown as an inherited file, never in the team's pipeline.
        assert_eq!(
            doc(epic, "parent-story.md").source_team.as_deref(),
            Some("parent")
        );
        assert!(epic.stories.is_empty());

        // A folder only the parent has shows at its place in the team's own
        // tree (folders are shared, not inherited); its files are the parent's.
        let rules = sub(&tree, "rules");
        assert_eq!(
            rules.absolute_dir_path,
            own.join("rules").display().to_string()
        );
        let deep = sub(rules, "deep");
        assert_eq!(deep.dir_path, "rules/deep");
        let rule = doc(deep, "rule.md");
        assert_eq!(rule.source_team.as_deref(), Some("parent"));
        assert_eq!(
            rule.file_path,
            parent.join("rules/deep/rule.md").display().to_string()
        );
    }

    #[test]
    fn scan_of_a_parentless_team_marks_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_org(
            root,
            "teams:\n  - name: default\n  - name: other\n",
            &["default", "other"],
        );
        let own = root.join("default/content");
        write(&own.join("epic/own.md"), "# own\n");
        write(&root.join("other/content/epic/theirs.md"), "# sibling\n");

        let tree = scan(&own);
        let epic = sub(&tree, "epic");
        assert_eq!(epic.docs.len(), 1);
        assert_eq!(doc(epic, "own.md").source_team, None);
    }

    const PARENT_TEAMS: &str =
        "teams:\n  - name: default\n    parent_team: parent\n  - name: parent\n";

    #[test]
    fn a_drilled_scan_inherits_only_that_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_org(root, PARENT_TEAMS, &["default", "parent"]);
        let own = root.join("default/content");
        let parent = root.join("parent/content");
        write(&own.join("epic/own.md"), "# own\n");
        write(&parent.join("epic/theirs.md"), "# parent\n");
        write(&parent.join("epic/deeper/deep.md"), "# deep\n");
        write(&parent.join("other/elsewhere.md"), "# not in scope\n");

        let tree = scan_scope(&own, "epic");
        assert_eq!(
            doc(&tree, "theirs.md").source_team.as_deref(),
            Some("parent")
        );
        let deeper = sub(&tree, "deeper");
        assert_eq!(deeper.dir_path, "epic/deeper");
        assert_eq!(
            deeper.absolute_dir_path,
            own.join("epic/deeper").display().to_string()
        );
        assert!(!format!("{tree:?}").contains("elsewhere.md"));

        // A scope the parent does not have adds nothing (and is no error).
        write(&own.join("mine-only/a.md"), "# a\n");
        let tree = scan_scope(&own, "mine-only");
        assert_eq!(tree.docs.len(), 1);
        assert_eq!(doc(&tree, "a.md").source_team, None);
    }

    #[test]
    fn a_missing_parent_team_adds_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // `parent` is declared but has no directory on this machine.
        write_org(root, PARENT_TEAMS, &["default"]);
        let own = root.join("default/content");
        write(&own.join("epic/own.md"), "# own\n");

        let tree = scan(&own);
        let epic = sub(&tree, "epic");
        assert_eq!(epic.docs.len(), 1);
        assert_eq!(tree.sub_epics.len(), 1);
    }

    #[test]
    fn files_agent_context_would_not_carry_are_not_inherited() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_org(root, PARENT_TEAMS, &["default", "parent"]);
        let own = root.join("default/content");
        let parent = root.join("parent/content");
        write(
            &root.join("parent/config/projection-excludes.yml"),
            "patterns:\n  - \"private/**\"\n  - \"epic/secret.md\"\n",
        );
        write(&own.join("epic/own.md"), "# own\n");
        write(&parent.join("epic/shared-rule.md"), "# carried\n");
        write(&parent.join("epic/secret.md"), "# excluded\n");
        write(&parent.join("private/notes.md"), "# excluded folder\n");

        let tree = scan(&own);
        let epic = sub(&tree, "epic");
        assert_eq!(
            doc(epic, "shared-rule.md").source_team.as_deref(),
            Some("parent")
        );
        assert!(epic.docs.iter().all(|d| d.file_name != "secret.md"));
        // A parent-only folder with nothing carried is not shown at all.
        assert!(tree.sub_epics.iter().all(|s| s.dir_name != "private"));
    }

    /// SW-XS-20 (M1): a child team's view edits its parent's FILE in place — a
    /// save to the path the scan reports lands in the parent's content and
    /// leaves no copy in the child's. A new file dropped into a folder only the
    /// parent has lands in the child team's copy of that folder, created on write.
    #[test]
    fn a_save_lands_in_the_parents_file_and_a_new_file_in_the_active_team() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_org(root, PARENT_TEAMS, &["default", "parent"]);
        let own = root.join("default/content");
        let parent = root.join("parent/content");
        write(&own.join("epic/own.md"), "# own\n");
        write(&parent.join("epic/rule.md"), "# before\n");
        write(&parent.join("rules/r.md"), "# r\n");

        let tree = scan(&own);
        let rule = doc(sub(&tree, "epic"), "rule.md").file_path.clone();
        crate::content_engine::write_tmp_fsync_rename(Path::new(&rule), b"# after\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(parent.join("epic/rule.md")).unwrap(),
            "# after\n"
        );
        assert!(!own.join("epic/rule.md").exists());

        // The create path (`create_file` / OS-drop ingest) makes the folder.
        let rules_dir = sub(&tree, "rules").absolute_dir_path.clone();
        let created = Path::new(&rules_dir).join("new.md");
        std::fs::create_dir_all(created.parent().unwrap()).unwrap();
        crate::content_engine::write_tmp_fsync_rename(&created, b"# new\n").unwrap();
        assert!(own.join("rules/new.md").is_file());
        assert!(!parent.join("rules/new.md").exists());

        // The next scan: the new file is the team's own, the parent's stays the parent's.
        let tree = scan(&own);
        let rules = sub(&tree, "rules");
        assert_eq!(doc(rules, "new.md").source_team, None);
        assert_eq!(doc(rules, "r.md").source_team.as_deref(), Some("parent"));
    }
    #[test]
    fn inherited_nav_overlay_excludes_private_parent_names_and_bytes() {
        let (_home, root) = crate::context_repo::inheritance::test_fixture();
        std::fs::remove_file(root.join("child/content/secret.md")).unwrap();
        std::fs::remove_file(root.join("child/content/shared.md")).unwrap();
        let tree = scan(&root.join("child/content"));
        assert_eq!(
            doc(&tree, "shared.md").source_team.as_deref(),
            Some("parent")
        );
        let shown = format!("{tree:?}");
        assert!(!shown.contains("secret.md"));
        assert!(!shown.contains("parent-secret") && !shown.contains("grand-secret"));
        assert!(!shown.contains("grand/SKILL.md"));
    }
    #[cfg(unix)]
    #[test]
    fn navigation_alias_owner_matrix_keeps_strict_own_walks_and_gates_inherited_reads() {
        let fixture = crate::context_repo::inheritance::alias_fixture();
        let child = fixture.root.join("child/content");
        let private = fixture.private.join("content");
        let child_tree = scan(&child);
        let private_tree = scan(&private);
        for case in &fixture.cases {
            let filename = format!("{}.md", case.id);
            let expected = case.origin == "inherited" && case.admitted;
            assert_eq!(
                child_tree.docs.iter().any(|d| d.file_name == filename),
                expected,
                "{}",
                case.id
            );
            assert!(
                !private_tree.docs.iter().any(|d| d.file_name == filename),
                "{}",
                case.id
            );
            if !case.admitted {
                std::fs::remove_file(&case.content).unwrap();
                assert_eq!(
                    serde_json::to_value(scan(&child)).unwrap(),
                    serde_json::to_value(&child_tree).unwrap(),
                    "{}",
                    case.id
                );
                assert_eq!(
                    serde_json::to_value(scan(&private)).unwrap(),
                    serde_json::to_value(&private_tree).unwrap(),
                    "{}",
                    case.id
                );
            }
        }
        let shown = format!("{child_tree:?}{private_tree:?}");
        assert!(!shown.contains("parent-secret.md-bytes"));
    }
    #[cfg(unix)]
    #[test]
    fn scoped_nav_directory_aliases_answer_as_missing_before_reading_parent_docs() {
        let fixture = crate::context_repo::inheritance::alias_fixture();
        let hidden = fixture
            .cases
            .iter()
            .find(|c| c.id == "alias-own-unshared")
            .unwrap()
            .target_content
            .parent()
            .unwrap()
            .join("private-epic");
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(hidden.join("secret.md"), "UNSHARED_NAV_DIRECTORY_SENTINEL").unwrap();
        for content in [
            fixture.root.join("child/content"),
            fixture.private.join("content"),
        ] {
            let alias = content.join("linked-directory");
            std::os::unix::fs::symlink(&hidden, &alias).unwrap();
            let denied =
                super::super::scan_pipeline_tree_impl(&content, "", "linked-directory", "")
                    .unwrap_err();
            std::fs::remove_file(alias).unwrap();
            let missing =
                super::super::scan_pipeline_tree_impl(&content, "", "linked-directory", "")
                    .unwrap_err();
            assert_eq!(denied, missing);
            assert!(!denied.contains("UNSHARED_NAV_DIRECTORY_SENTINEL"));
        }
    }
}
