// SPDX-License-Identifier: MIT
//! The Scratch space tab: what the user keeps in their scratch space for one
//! org, plus the all-orgs folder, and creating an "Only me" file there.
//!
//! Notes, routines and agents are listed by their own commands (the tab reuses
//! them); this module lists what has no other reader: the org's files (the
//! ones that apply over its teams' files) and the all-orgs items. Reads never
//! create a space; creating a file creates it.
//!
//! All disk work runs on the blocking pool via [`await_blocking`].

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::context_repo::scratch_space::{self, InertKind};
use crate::domain::Principal;

use super::async_helpers::await_blocking;
use super::route_to_context::resolve_ambient;

/// One file in the org's scratch space (`content/`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScratchFile {
    /// Absolute path, for opening it.
    pub path: String,
    /// Path relative to the space's `content/` (`/`-joined) — where it applies
    /// in the org's teams.
    pub logical: String,
    /// The inert kind it would apply as in every org ("Use in all orgs"), or
    /// `None` when it cannot go in all-orgs (by path or by content).
    pub inert_kind: Option<InertKind>,
    pub replaces_team: bool,
}

/// One file in the all-orgs folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScratchAllOrgsFile {
    pub path: String,
    pub logical: String,
    /// The inert kind it applies as; `None` = no org reads it.
    pub kind: Option<InertKind>,
}

/// The Scratch space tab feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScratchSpaceListing {
    /// The org listed (`None` when no org is open).
    pub org_slug: Option<String>,
    /// Whether that org's scratch space exists yet.
    pub exists: bool,
    pub files: Vec<ScratchFile>,
    pub all_orgs: Vec<ScratchAllOrgsFile>,
    /// Every org synced on this Mac — where "Copy to another org" can go.
    pub synced_orgs: Vec<String>,
    pub management: Option<crate::mcp::setup_tools::ScratchSetup>,
}

/// List `org_slug`'s scratch space (when given) and the all-orgs folder.
#[tauri::command]
pub async fn list_scratch_space(
    app: tauri::AppHandle,
    org_slug: Option<String>,
    team: Option<String>,
) -> Result<ScratchSpaceListing, String> {
    let (home, user_id) = resolve_ambient(&app).await?;
    await_blocking("list_scratch_space", move || {
        let mut listing =
            list_scratch_space_at(&home, &Principal::from(user_id), org_slug.as_deref());
        listing.synced_orgs = crate::mcp::setup_tools::synced_org_slugs(&home, user_id)?;
        if let (Some(org), Some(team)) = (org_slug.as_deref(), team.as_deref()) {
            mark_replacements(&mut listing, &home, &Principal::from(user_id), org, team)?;
        }
        listing.management = Some(crate::mcp::setup_tools::describe_scratch(
            &home,
            &Principal::from(user_id),
            &listing.synced_orgs,
        ));
        Ok(listing)
    })
    .await
}

fn mark_replacements(
    listing: &mut ScratchSpaceListing,
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
) -> Result<(), String> {
    let roots = super::scratch_use::team_content_roots_at(home, principal, org, team)?;
    for file in &mut listing.files {
        file.replaces_team = super::scratch_use::team_has_file(&roots, &file.logical);
    }
    Ok(())
}

pub(crate) fn list_scratch_space_at(
    home: &Path,
    principal: &Principal,
    org_slug: Option<&str>,
) -> ScratchSpaceListing {
    let space = org_slug.and_then(|org| scratch_space::org_space(home, principal, org));
    let files = match &space {
        Some(space) => sorted(
            scratch_space::regular_files(&space.content_dir())
                .map(|(abs, logical)| ScratchFile {
                    inert_kind: inert_kind_of(&abs, &logical),
                    path: display(&abs),
                    logical,
                    replaces_team: false,
                })
                .collect(),
            |f: &ScratchFile| f.logical.clone(),
        ),
        None => Vec::new(),
    };
    let all_orgs_root = scratch_space::scratch_all_orgs_root(home, principal);
    let all_orgs = sorted(
        scratch_space::regular_files(&all_orgs_root)
            .map(|(abs, logical)| ScratchAllOrgsFile {
                kind: inert_kind_of(&abs, &logical),
                path: display(&abs),
                logical,
            })
            .collect(),
        |f: &ScratchAllOrgsFile| f.logical.clone(),
    );
    ScratchSpaceListing {
        org_slug: org_slug.map(str::to_string),
        exists: space.is_some(),
        files,
        all_orgs,
        synced_orgs: Vec::new(),
        management: None,
    }
}

/// The inert kind the file at `abs` applies as in all-orgs: an allowlisted path
/// AND inert content (the same gates the all-orgs write and read apply).
pub(crate) fn inert_kind_of(abs: &Path, logical: &str) -> Option<InertKind> {
    scratch_space::inert_kind(logical).filter(|_| scratch_space::is_inert_file(abs, logical))
}

/// Create an empty "Only me" file named `name` in the org's scratch space, at
/// the place `dir` (a folder of `team`'s files) has in the team. A taken name
/// gets a numbered copy (`name-2.md`); nothing is overwritten. The org must be
/// synced on this Mac. Creates the space on first use.
#[tauri::command]
pub async fn create_scratch_file(
    app: tauri::AppHandle,
    org_slug: String,
    team: String,
    dir: String,
    name: String,
) -> Result<String, String> {
    // The team names a directory: the same path-safe segment routines require.
    if !crate::routines::model::is_valid_id(&team) {
        return Err(format!("team {team:?} is not a valid name"));
    }
    let (home, user_id) = resolve_ambient(&app).await?;
    let t = team;
    let path = {
        await_blocking("create_scratch_file", move || {
            let principal = Principal::from(user_id);
            let synced = crate::mcp::setup_tools::synced_org_slugs(&home, user_id)?;
            let org_root = crate::context_repo::path_to_org_root(&home, &principal, &org_slug)?;
            let team_content = crate::context_repo::path_to_team_content_dir(&org_root, &t);
            let private_content =
                scratch_space::org_space_at(&home, &principal, &org_slug)?.content_dir();
            let content_root =
                only_me_content_root(&team_content, &private_content, Path::new(&dir));
            create_scratch_file_at(
                &home,
                &principal,
                &OnlyMeTarget {
                    org_slug: &org_slug,
                    synced_orgs: &synced,
                    team_content: content_root,
                },
                Path::new(&dir),
                &name,
            )
        })
        .await?
    };
    Ok(display(&path))
}

fn only_me_content_root<'a>(team: &'a Path, private: &'a Path, dir: &Path) -> &'a Path {
    if dir.starts_with(private) {
        private
    } else {
        team
    }
}

/// Where an "Only me" file is being filed: the org (which must be synced on this
/// Mac) and the team content folder the chosen place is relative to.
pub(crate) struct OnlyMeTarget<'a> {
    pub org_slug: &'a str,
    pub synced_orgs: &'a [String],
    pub team_content: &'a Path,
}

pub(crate) fn create_scratch_file_at(
    home: &Path,
    principal: &Principal,
    target: &OnlyMeTarget<'_>,
    dir: &Path,
    name: &str,
) -> Result<PathBuf, String> {
    let OnlyMeTarget {
        org_slug,
        synced_orgs,
        team_content,
    } = *target;
    let name = name.trim();
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return Err(format!("'{name}' is not a file name"));
    }
    if !synced_orgs.iter().any(|o| o == org_slug) {
        return Err(format!("{org_slug} is not synced on this Mac"));
    }
    let rel = dir.strip_prefix(team_content).map_err(|_| {
        "Only me files go in the team's own files (this folder is somewhere else)".to_string()
    })?;
    let rel_logical = rel
        .components()
        .map(|c| match c {
            std::path::Component::Normal(s) => s.to_str().map(str::to_string),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| "this folder cannot hold an Only me file".to_string())?;
    // Judge the write before anything is created, so a refused create leaves no space.
    let address = scratch_space::org_space_at(home, principal, org_slug)?;
    let target_dir = rel_logical
        .iter()
        .fold(address.content_dir(), |acc, seg| acc.join(seg));
    if let Some(refusal) = scratch_space::write_refusal(&target_dir.join(name), false) {
        return Err(refusal);
    }
    scratch_space::ensure_org_space(home, principal, org_slug)?;
    std::fs::create_dir_all(&target_dir).map_err(|e| format!("couldn't create the folder: {e}"))?;
    // `create_new` claims the name atomically; a name taken meanwhile moves on.
    for candidate in candidate_names(name) {
        let path = target_dir.join(&candidate);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("couldn't create the file: {e}")),
        }
    }
    Err("couldn't find a free name for the file".to_string())
}

/// `name`, then `<stem>-2.<ext>`, `<stem>-3.<ext>`, … (bounded).
fn candidate_names(name: &str) -> impl Iterator<Item = String> + '_ {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    std::iter::once(name.to_string()).chain((2..10_000).map(move |n| format!("{stem}-{n}{ext}")))
}

fn sorted<T, K: Ord>(mut items: Vec<T>, key: impl Fn(&T) -> K) -> Vec<T> {
    items.sort_by_key(|item| key(item));
    items
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn me() -> Principal {
        Principal::from(42u64)
    }

    #[test]
    fn lists_only_the_named_orgs_space_and_the_all_orgs_folder() {
        let tmp = tempdir().unwrap();
        let (a, _) = scratch_space::ensure_org_space(tmp.path(), &me(), "acme/a").unwrap();
        let (b, _) = scratch_space::ensure_org_space(tmp.path(), &me(), "acme/b").unwrap();
        std::fs::create_dir_all(a.content_dir().join("docs")).unwrap();
        std::fs::write(a.content_dir().join("docs/plan.md"), "a").unwrap();
        std::fs::write(b.content_dir().join("secret-b.md"), "b").unwrap();
        let all_orgs = scratch_space::scratch_all_orgs_root(tmp.path(), &me());
        std::fs::create_dir_all(all_orgs.join("skills/review")).unwrap();
        std::fs::write(all_orgs.join("skills/review/SKILL.md"), "review").unwrap();
        std::fs::write(all_orgs.join("run.sh"), "echo").unwrap();

        let listing = list_scratch_space_at(tmp.path(), &me(), Some("acme/a"));
        assert!(listing.exists);
        assert_eq!(listing.org_slug.as_deref(), Some("acme/a"));
        let logicals: Vec<_> = listing.files.iter().map(|f| f.logical.as_str()).collect();
        assert_eq!(logicals, ["docs/plan.md"], "never another org's file");
        assert_eq!(listing.files[0].inert_kind, None, "a doc is not inert");
        let carried: Vec<_> = listing
            .all_orgs
            .iter()
            .map(|f| (f.logical.as_str(), f.kind))
            .collect();
        assert_eq!(
            carried,
            [
                ("run.sh", None),
                ("skills/review/SKILL.md", Some(InertKind::Skill))
            ]
        );
    }

    #[test]
    fn an_org_file_says_whether_it_can_be_used_in_all_orgs() {
        let tmp = tempdir().unwrap();
        let (a, _) = scratch_space::ensure_org_space(tmp.path(), &me(), "acme/a").unwrap();
        let content = a.content_dir();
        std::fs::create_dir_all(content.join("prompts")).unwrap();
        std::fs::write(content.join("prompts/plan.md"), "Plan it").unwrap();
        std::fs::write(content.join("prompts/sneaky.md"), "run !`rm -rf ~`").unwrap();
        let listing = list_scratch_space_at(tmp.path(), &me(), Some("acme/a"));
        let kinds: Vec<_> = listing
            .files
            .iter()
            .map(|f| (f.logical.as_str(), f.inert_kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("prompts/plan.md", Some(InertKind::Prompt)),
                ("prompts/sneaky.md", None)
            ],
            "the allowlist AND the content check"
        );
    }

    #[test]
    fn listing_never_creates_a_space_and_works_with_no_org() {
        let tmp = tempdir().unwrap();
        let listing = list_scratch_space_at(tmp.path(), &me(), Some("acme/a"));
        assert!(!listing.exists);
        assert!(listing.files.is_empty());
        assert!(scratch_space::org_space(tmp.path(), &me(), "acme/a").is_none());
        let none = list_scratch_space_at(tmp.path(), &me(), None);
        assert_eq!(none.org_slug, None);
        assert!(!none.exists);
    }

    fn synced() -> Vec<String> {
        vec!["acme/a".to_string()]
    }

    fn create(home: &Path, team_content: &Path, dir: &Path, name: &str) -> Result<PathBuf, String> {
        let synced = synced();
        create_scratch_file_at(
            home,
            &me(),
            &OnlyMeTarget {
                org_slug: "acme/a",
                synced_orgs: &synced,
                team_content,
            },
            dir,
            name,
        )
    }

    #[test]
    fn an_only_me_file_lands_in_the_orgs_space_at_the_teams_place() {
        let tmp = tempdir().unwrap();
        let team_content = tmp.path().join("org/default/content");
        let dir = team_content.join("docs/notes");
        let path = create(tmp.path(), &team_content, &dir, "idea.md").unwrap();
        let space = scratch_space::org_space(tmp.path(), &me(), "acme/a").expect("created");
        assert_eq!(path, space.content_dir().join("docs/notes/idea.md"));
        assert!(path.is_file());
        assert!(
            !dir.join("idea.md").exists(),
            "nothing is written into the team"
        );

        let again = create(tmp.path(), &team_content, &dir, "idea.md").unwrap();
        assert_eq!(again, space.content_dir().join("docs/notes/idea-2.md"));
        let third = create(tmp.path(), &team_content, &dir, "idea.md").unwrap();
        assert_eq!(third, space.content_dir().join("docs/notes/idea-3.md"));
        let at_root = create(tmp.path(), &team_content, &team_content, "x.md").unwrap();
        assert_eq!(at_root, space.content_dir().join("x.md"));
    }

    #[test]
    fn private_replacement_badges_include_parent_team_content() {
        let tmp = tempdir().unwrap();
        let org = "acme/a";
        let org_root = crate::context_repo::path_to_org_root(tmp.path(), &me(), org).unwrap();
        std::fs::create_dir_all(&org_root).unwrap();
        std::fs::write(
            org_root.join("org.yml"),
            "slug: acme/a\ndisplay_name: A\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
        )
        .unwrap();
        std::fs::write(org_root.join("people.yml"), "people: []\n").unwrap();
        std::fs::write(
            org_root.join("teams.yml"),
            "teams:\n  - name: parent\n  - name: dev\n    parent_team: parent\n",
        )
        .unwrap();
        let parent = crate::context_repo::path_to_team_content_dir(&org_root, "parent");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::write(parent.join("guide.md"), "Shared guide").unwrap();
        let config = parent.parent().unwrap().join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("sharing.yml"), "share: ['content/guide.md']\n").unwrap();
        let (space, _) = scratch_space::ensure_org_space(tmp.path(), &me(), org).unwrap();
        std::fs::write(space.content_dir().join("guide.md"), "Private guide").unwrap();
        std::fs::write(space.content_dir().join("only-me.md"), "Private only").unwrap();
        let mut listing = list_scratch_space_at(tmp.path(), &me(), Some(org));
        mark_replacements(&mut listing, tmp.path(), &me(), org, "dev").unwrap();
        assert!(
            listing
                .files
                .iter()
                .find(|f| f.logical == "guide.md")
                .unwrap()
                .replaces_team
        );
        assert!(
            !listing
                .files
                .iter()
                .find(|f| f.logical == "only-me.md")
                .unwrap()
                .replaces_team
        );
    }

    #[test]
    fn a_private_nav_subfolder_uses_the_private_content_root() {
        let tmp = tempdir().unwrap();
        let team_content = tmp.path().join("org/default/content");
        let private_content = scratch_space::org_space_at(tmp.path(), &me(), "acme/a")
            .unwrap()
            .content_dir();
        let dir = private_content.join("docs/notes");
        let root = only_me_content_root(&team_content, &private_content, &dir);
        let path = create(tmp.path(), root, &dir, "idea.md").unwrap();
        assert_eq!(path, dir.join("idea.md"));
        assert!(path.is_file());
        assert!(!team_content.exists());
        assert_eq!(
            only_me_content_root(&team_content, &private_content, &team_content.join("docs")),
            team_content
        );
    }

    #[test]
    fn an_only_me_file_refuses_a_folder_outside_the_team_or_a_bad_name() {
        let tmp = tempdir().unwrap();
        let team_content = tmp.path().join("org/default/content");
        let outside = tmp.path().join("org/other/content");
        assert!(create(tmp.path(), &team_content, &outside, "a.md").is_err());
        for bad in ["", " ", "../a.md", "a/b.md", ".."] {
            assert!(
                create(tmp.path(), &team_content, &team_content, bad).is_err(),
                "{bad:?}"
            );
        }
        assert!(
            scratch_space::org_space(tmp.path(), &me(), "acme/a").is_none(),
            "a refused create leaves no space"
        );
    }

    #[test]
    fn an_only_me_file_never_follows_a_link_out_of_the_scratch_space() {
        let tmp = tempdir().unwrap();
        let team_content = tmp.path().join("users/42/orgs/acme/a/default/content");
        std::fs::create_dir_all(team_content.join("docs")).unwrap();
        let (space, _) = scratch_space::ensure_org_space(tmp.path(), &me(), "acme/a").unwrap();
        std::fs::create_dir_all(space.content_dir()).unwrap();
        // A link in the scratch space back into the team's shared files.
        std::os::unix::fs::symlink(team_content.join("docs"), space.content_dir().join("docs"))
            .unwrap();
        let err = create(
            tmp.path(),
            &team_content,
            &team_content.join("docs"),
            "idea.md",
        )
        .unwrap_err();
        assert!(err.contains("link"), "{err}");
        assert!(
            !team_content.join("docs/idea.md").exists(),
            "nothing lands in the team's shared files"
        );
    }

    #[test]
    fn an_only_me_file_needs_an_org_synced_on_this_mac() {
        let tmp = tempdir().unwrap();
        let team_content = tmp.path().join("org/default/content");
        let err = create_scratch_file_at(
            tmp.path(),
            &me(),
            &OnlyMeTarget {
                org_slug: "other/org",
                synced_orgs: &synced(),
                team_content: &team_content,
            },
            &team_content,
            "a.md",
        )
        .unwrap_err();
        assert!(err.contains("not synced"), "{err}");
        assert!(scratch_space::org_space(tmp.path(), &me(), "other/org").is_none());
    }

    #[test]
    fn the_listing_serializes_with_the_field_names_the_tab_reads() {
        let tmp = tempdir().unwrap();
        let all_orgs = scratch_space::scratch_all_orgs_root(tmp.path(), &me());
        std::fs::create_dir_all(all_orgs.join("prompts")).unwrap();
        std::fs::write(all_orgs.join("prompts/fix.md"), "fix it").unwrap();
        let json =
            serde_json::to_value(list_scratch_space_at(tmp.path(), &me(), Some("acme/a"))).unwrap();
        assert_eq!(json["orgSlug"], "acme/a");
        assert_eq!(json["exists"], false);
        assert!(json["files"].is_array());
        assert_eq!(json["allOrgs"][0]["logical"], "prompts/fix.md");
        assert_eq!(json["allOrgs"][0]["kind"], "prompt");
        assert!(json["syncedOrgs"].is_array());
    }
}
