// SPDX-License-Identifier: MIT
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::async_helpers::await_blocking;
use super::paths::{codesense_home_dir, frontmatter_cache_path};
use crate::context_repo::{
    apply_add_identity_to_person, apply_add_person_by_name, apply_registry_edit,
    bootstrap_workspace_content, default_skeleton_dirs, path_to_org_root, rename_team,
    seed_repo_content, with_branch_lock, write_edit_to_workdir_locked, AppliedEdit,
    BootstrapResult, RegistryEdit, SeedOutcome,
};
use crate::domain::{Person, Principal};
use crate::install::frontmatter_cache::{FrontmatterCache, SharedFrontmatterCache};
use tauri::Manager;

#[derive(Debug, Clone, Serialize)]
pub struct LocalEditOutcome {
    pub commit_message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person: Option<Person>,
}

fn apply_stable_person_edit(
    edit: &RegistryEdit,
    org_root: &Path,
) -> Result<Option<(AppliedEdit, Option<Person>)>, String> {
    let (applied, person) = match edit {
        RegistryEdit::AddPersonByName { display_name } => {
            apply_add_person_by_name(display_name, org_root)
        }
        RegistryEdit::AddIdentityToPerson {
            person_id,
            provider,
            handle,
            provider_user_id,
        } => apply_add_identity_to_person(
            person_id,
            provider,
            handle,
            provider_user_id.as_deref(),
            org_root,
        ),
        _ => return Ok(None),
    }
    .map_err(|error| error.to_string())?;
    Ok(Some((applied, Some(person))))
}

/// Apply a registry edit to the context-repo **working tree only** — no commit,
/// no PR, no push. The edit sits as a pending working-tree change until the user
/// publishes it via `/cs-publish`. (`RenameTeam` is the one exception — see the
/// branch below.)
#[tauri::command]
pub async fn commit_registry_edit_local(
    app: tauri::AppHandle,
    github_user_id: u64,
    org_slug: String,
    edit: RegistryEdit,
) -> Result<LocalEditOutcome, String> {
    let principal = super::local_profile::active(&app)?.principal;
    if principal.numeric_id()? != github_user_id {
        return Err("Local profile changed; reload this window".into());
    }
    let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
    commit_registry_edit_for_profile(&app, &home, &principal, &org_slug, edit).await
}

/// Shared local adapter. The activation lock also serializes profile selection.
pub(crate) async fn commit_registry_edit_for_profile<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &Path,
    principal: &Principal,
    org_slug: &str,
    mut edit: RegistryEdit,
) -> Result<LocalEditOutcome, String> {
    let watcher = app.try_state::<super::watch::WatcherState>();
    let _activation = match watcher.as_ref() {
        Some(watcher) => Some(watcher.lock_team_activation().await),
        None => None,
    };
    let profile = super::local_profile::active(app)?;
    if &profile.principal != principal {
        return Err("Local profile changed; retry under the selected profile".into());
    }
    if let RegistryEdit::CreateTeam {
        creator_id,
        creator_username,
        creator_display_name,
        creator_avatar_url,
        ..
    } = &mut edit
    {
        *creator_id = principal.numeric_id()?;
        *creator_username = profile.display_name.clone();
        *creator_display_name = Some(profile.display_name);
        *creator_avatar_url = None;
    }
    commit_registry_edit(app, home, principal, org_slug, edit).await
}

/// The app event every successful registry edit emits, so a view of the org
/// re-reads it no matter who made the edit (the UI or an agent over MCP).
pub const ORG_REGISTRY_CHANGED_EVENT: &str = "org-registry-changed";

/// The `org-registry-changed` payload: which org, which edit, and the team it
/// touched — plus the old name for a rename, which a listener needs to move
/// its team-keyed state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgRegistryChanged {
    pub principal: String,
    pub org_slug: String,
    pub edit: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_team: Option<String>,
}

impl OrgRegistryChanged {
    pub fn new(principal: &Principal, org_slug: &str, edit: &RegistryEdit) -> Self {
        Self {
            principal: principal.to_string(),
            org_slug: org_slug.to_string(),
            edit: edit.kind(),
            team: edit.team().map(str::to_string),
            previous_team: edit.previous_team().map(str::to_string),
        }
    }
}

/// Tell every view that `org_slug`'s registry changed. Best-effort: the edit
/// already landed on disk, so a failed emit is logged, never surfaced.
pub(crate) fn emit_org_registry_changed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    principal: &Principal,
    org_slug: &str,
    edit: &RegistryEdit,
) {
    use tauri::Emitter;
    if let Err(e) = crate::live_hub::pane_sources::registry_changed(app, principal, org_slug) {
        log::warn!("pane source chain reconcile failed for {org_slug}: {e}");
    }
    if let Err(e) = app.emit(
        ORG_REGISTRY_CHANGED_EVENT,
        OrgRegistryChanged::new(principal, org_slug, edit),
    ) {
        log::warn!("org-registry-changed emit failed for {org_slug}: {e}");
    }
}

/// The one registry write path, shared by the Tauri command and the MCP tools:
/// apply `edit` to the org's working tree, then emit `org-registry-changed` so
/// the UI reloads the org. Nothing is emitted when the edit is refused.
pub(crate) async fn commit_registry_edit<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &Path,
    principal: &Principal,
    org_slug: &str,
    edit: RegistryEdit,
) -> Result<LocalEditOutcome, String> {
    let outcome = apply_local_edit(app, home, principal, org_slug, &edit).await?;
    emit_org_registry_changed(app, principal, org_slug, &edit);
    Ok(outcome)
}

async fn apply_local_edit<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &Path,
    principal: &Principal,
    org_slug: &str,
    edit: &RegistryEdit,
) -> Result<LocalEditOutcome, String> {
    crate::context_repo::org_slug::require_valid_org_slug(org_slug)?;
    // The bundled CodeSense docs are read-only: no registry edit lands there.
    if crate::context_repo::codesense_docs::is_docs_slug(org_slug) {
        return Err(crate::context_repo::codesense_docs::READ_ONLY_MESSAGE.to_string());
    }
    let org_root = path_to_org_root(home, principal, org_slug)?;
    if !org_root.exists() {
        return Err(format!(
            "org root missing for {}: {}",
            org_slug,
            org_root.display(),
        ));
    }

    // Renames use a filesystem transaction, preserving Git history and index.
    if let RegistryEdit::RenameTeam { old_name, new_name } = edit {
        // Git commit + directory moves: off the async executor. `rename_team`
        // takes the branch lock itself.
        let outcome = {
            let (home, principal, org_slug, org_root) = (
                home.to_path_buf(),
                principal.clone(),
                org_slug.to_string(),
                org_root.clone(),
            );
            let (old_name, new_name) = (old_name.clone(), new_name.clone());
            await_blocking("registry.rename_team", move || {
                rename_team(
                    &home, &principal, &org_slug, &org_root, &old_name, &new_name,
                )
                .map_err(|e| e.to_string())
            })
            .await?
        };

        // AC-10: the frontmatter cache indexes by absolute path + dir-inferred
        // team, and a dir rename preserves mtimes — so force a fresh rebuild
        // (the moved stories re-index under the new team; the old entries drop)
        // and drop the stale warm-start copy so a relaunch cold-builds rather
        // than reloading the pre-rename paths. Both are best-effort: a rename
        // already committed must not fail on a cache hiccup. The rebuild is a
        // full-org disk rescan, so run it on the blocking pool — keep the
        // command-dispatch thread free even for a large org.
        if let Some(cache) = app.try_state::<SharedFrontmatterCache>() {
            let cache = cache.inner().clone();
            let scan_root = org_root.clone();
            let persisted = frontmatter_cache_path(home);
            let _ = tokio::task::spawn_blocking(move || {
                FrontmatterCache::rebuild_from_disk(&cache, &scan_root);
                if persisted.exists() {
                    if let Err(e) = std::fs::remove_file(&persisted) {
                        log::warn!("rename_team: drop stale frontmatter cache failed: {e}");
                    }
                }
            })
            .await;
        }

        return Ok(LocalEditOutcome {
            commit_message: outcome.commit_message,
            person: None,
        });
    }

    // Historical provider lookup edits remain refused; use stable local people.
    if let RegistryEdit::AddPersonByUsername { .. } = edit {
        return Err("Service profile lookup was retired. Add a person by name, then add identity details if needed.".into());
    }

    let edit = edit.clone();
    let (applied, person) = apply_and_write_locked(org_root, move |root| {
        if let Some(stable) = apply_stable_person_edit(&edit, root)? {
            return Ok(stable);
        }
        apply_file_edit(&edit, root).map(|applied| (applied, None))
    })
    .await?;

    // No commit: the edit is a pending working-tree change. `commit_message`
    // carries the human summary of the edit for the caller's toast — it is no
    // longer tied to a git commit.
    Ok(LocalEditOutcome {
        commit_message: applied.commit_title,
        person,
    })
}

/// Run one registry read-modify-write off the async executor, holding the
/// branch lock from the read in `apply` through the working-tree write, so
/// concurrent edits to the same registry file can't drop each other.
async fn apply_and_write_locked<T, F>(
    org_root: PathBuf,
    apply: F,
) -> Result<(AppliedEdit, T), String>
where
    T: Send + 'static,
    F: FnOnce(&Path) -> Result<(AppliedEdit, T), String> + Send + 'static,
{
    await_blocking("registry.apply_edit", move || {
        with_branch_lock(|| {
            let (applied, extra) = apply(&org_root)?;
            write_edit_to_workdir_locked(&org_root, &applied).map_err(|e| e.to_string())?;
            Ok((applied, extra))
        })
    })
    .await
}

/// The pure file edits: apply `edit` to the registry files, plus the content
/// dirs a new source repo or team gets. Called under the branch lock.
fn apply_file_edit(edit: &RegistryEdit, org_root: &Path) -> Result<AppliedEdit, String> {
    let mut applied = apply_registry_edit(edit, org_root).map_err(|e| e.to_string())?;

    // Seed the same existing local layout used by reads; new repos use canonical paths.
    if let RegistryEdit::AddSourceRepoToTeam {
        team_name,
        owner,
        repo,
        provider,
        host,
    } = edit
    {
        let slug = crate::domain::SourceRepoSlug::with_provider_and_host(
            owner.clone(),
            repo.clone(),
            provider.as_deref().unwrap_or("github"),
            host.as_deref(),
        )
        .map_err(|e| e.to_string())?;
        let repo_content_root = crate::context_repo::layout_resolver::resolve_repository_root(
            org_root, team_name, &slug,
        )?;
        let repo_content_prefix = repo_content_root
            .strip_prefix(org_root)
            .map_err(|e| e.to_string())?
            .to_path_buf();
        std::fs::create_dir_all(&repo_content_root).map_err(|e| e.to_string())?;
        if let SeedOutcome::Seeded { gitkeep_paths } =
            seed_repo_content(&repo_content_root, default_skeleton_dirs())
                .map_err(|e| e.to_string())?
        {
            for rel in gitkeep_paths {
                // Files already exist on disk (written by seed_repo_content).
                // Appending to applied.files so the working-tree write includes
                // them in the pending change (an idempotent re-write of the same
                // empty bytes).
                applied
                    .files
                    .push((repo_content_prefix.join(rel), Vec::new()));
            }
        }
    }

    // SW-OB-1: a brand-new team gets an empty `_team/{name}/repos/` dir so
    // it's immediately browsable, mirroring the default-team scaffold.
    if let RegistryEdit::CreateTeam { team_name, .. } = edit {
        stage_new_team_repos_dir(&mut applied, org_root, team_name)?;
    }
    Ok(applied)
}

/// Repair command: seed content skeleton for all tracked repos missing it.
/// Commits locally — does not push.
#[tauri::command]
pub async fn bootstrap_workspace_content_cmd(org_root: String) -> Result<BootstrapResult, String> {
    let _home = codesense_home_dir().ok_or("cannot resolve home directory")?;
    bootstrap_workspace_content(Path::new(&org_root)).map_err(|e| e.to_string())
}

/// SW-OB-1: create an empty `_team/{name}/repos/` dir for a brand-new team
/// and stage its `.gitkeep` into `applied.files`, so the team is immediately
/// browsable (mirrors the default-team scaffold). `write_edit_to_workdir` also
/// creates parent dirs, but scaffolding the `repos/` dir here keeps a
/// zero-`.gitkeep` team (none staged) browsable too.
fn stage_new_team_repos_dir(
    applied: &mut AppliedEdit,
    org_root: &Path,
    team_name: &str,
) -> Result<(), String> {
    let repos_prefix = PathBuf::new().join(team_name).join("repos");
    std::fs::create_dir_all(org_root.join(&repos_prefix)).map_err(|e| e.to_string())?;
    applied
        .files
        .push((repos_prefix.join(".gitkeep"), Vec::new()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_repo::write_edit_to_workdir;
    use crate::domain::load_org;

    /// Init a git repo with a `main` branch + seeded registry files so
    /// `load_org` can compose it. Mirrors the minimal on-disk org shape.
    fn init_org_repo(dir: &Path, teams_yml: &str) {
        let repo = git2::Repository::init(dir).unwrap();
        std::fs::write(
            dir.join("org.yml"),
            "slug: acme\ndisplay_name: Acme\ncreated_at: 2026-01-01T00:00:00Z\nconfig_flags: {}\n",
        )
        .unwrap();
        // Seed the creators used by the tests so the composed org has a
        // populated roster.
        std::fs::write(
            dir.join("people.yml"),
            "people:\n  - github_user_id: 42\n    github_username: creator42\n  - github_user_id: 7\n    github_username: creator7\n",
        )
        .unwrap();
        std::fs::write(dir.join("teams.yml"), teams_yml).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        for f in ["org.yml", "people.yml", "teams.yml"] {
            index.add_path(Path::new(f)).unwrap();
        }
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }

    #[test]
    fn stable_person_edits_write_and_return_composed_people() {
        let tmp = tempfile::tempdir().unwrap();
        init_org_repo(tmp.path(), "teams: []\n");
        let apply_and_write = |edit: RegistryEdit| {
            let (applied, person) = apply_stable_person_edit(&edit, tmp.path())
                .unwrap()
                .unwrap();
            write_edit_to_workdir(tmp.path(), &applied).unwrap();
            person.unwrap()
        };

        let added = apply_and_write(RegistryEdit::AddPersonByName {
            display_name: "Grace Hopper".into(),
        });
        assert_eq!(added.display_name, "Grace Hopper");

        let linked = apply_and_write(RegistryEdit::AddIdentityToPerson {
            person_id: added.id.to_string(),
            provider: "gitlab".into(),
            handle: "grace".into(),
            provider_user_id: Some("gl-1".into()),
        });
        assert_eq!(linked.id, added.id);
        assert_eq!(linked.identities[0].provider, "gitlab");
    }

    /// Full pipeline (minus the home-dir resolution the Tauri command does):
    /// apply the CreateTeam edit, stage the team dir, write it to the working
    /// tree (no commit — mirrors the production path).
    fn create_team_pipeline(dir: &Path, team_name: &str, creator_id: u64) {
        let edit = RegistryEdit::CreateTeam {
            team_name: team_name.to_string(),
            creator_id,
            creator_username: format!("user{creator_id}"),
            creator_display_name: None,
            creator_avatar_url: None,
        };
        let mut applied = apply_registry_edit(&edit, dir).unwrap();
        stage_new_team_repos_dir(&mut applied, dir, team_name).unwrap();
        write_edit_to_workdir(dir, &applied).unwrap();
    }

    /// The current `refs/heads/main` commit OID — for asserting an edit did NOT
    /// create a commit.
    fn main_oid(dir: &Path) -> git2::Oid {
        git2::Repository::open(dir)
            .unwrap()
            .find_reference("refs/heads/main")
            .unwrap()
            .target()
            .unwrap()
    }

    #[test]
    fn create_team_local_adds_team_scaffolds_dir_and_no_branch() {
        let tmp = tempfile::tempdir().unwrap();
        init_org_repo(
            tmp.path(),
            "teams:\n  - name: platform\n    source_repos: []\n",
        );

        let before = main_oid(tmp.path());
        create_team_pipeline(tmp.path(), "payments", 42);

        // Reload from the working tree: the new team is present alongside the old.
        let loaded = load_org("acme", tmp.path(), None);
        let org = loaded.org.expect("org should load");
        let names: Vec<&str> = org.teams.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"platform"));
        assert!(names.contains(&"payments"));
        let payments = org.teams.iter().find(|t| t.name == "payments").unwrap();
        assert!(payments.source_repos.is_empty());

        // The team's repos dir + .gitkeep exist on disk.
        let gitkeep = tmp.path().join("payments").join("repos").join(".gitkeep");
        assert!(gitkeep.is_file(), "payments/repos/.gitkeep must exist");

        // AC-1: the edit is a working-tree write — `main` is byte-for-byte
        // unchanged (no new commit).
        assert_eq!(main_oid(tmp.path()), before, "no new commit on main");

        // Directory-based team layout: NO `team-payments` branch is created.
        let repo = git2::Repository::open(tmp.path()).unwrap();
        assert!(
            repo.find_branch("team-payments", git2::BranchType::Local)
                .is_err(),
            "no per-team branch should be created",
        );
    }

    /// AC-2 / AC-4: after a no-commit edit, the mutated registry file AND the
    /// staged scaffold `.gitkeep` show up in the publish set as pending paths,
    /// while `main` stays put. The edit surfaces because it is a working-tree
    /// delta against the publish baseline. This org has no remote, so
    /// `resolve_baseline_tree` falls to local `main` tip; the edit is uncommitted
    /// so `main` does not contain it and it diffs. In production a team-synced
    /// org diffs against `origin/main` the same way.
    #[test]
    fn no_commit_edit_surfaces_in_publish_set() {
        use crate::commands::git_sync::publish_set_impl;

        let tmp = tempfile::tempdir().unwrap();
        init_org_repo(tmp.path(), "teams: []\n");

        let before = main_oid(tmp.path());
        create_team_pipeline(tmp.path(), "payments", 42);

        // main is unmoved.
        assert_eq!(main_oid(tmp.path()), before, "no new commit on main");

        // The publish set lists both the mutated teams.yml and the new scaffold.
        let set = publish_set_impl(tmp.path(), None, None).expect("publish set");
        let paths: Vec<&str> = set
            .buckets
            .iter()
            .flat_map(|b| b.files.iter())
            .chain(set.general.files.iter())
            .map(|f| f.path.as_str())
            .collect();
        assert!(
            paths.contains(&"teams.yml"),
            "teams.yml must be a pending publish path, got {paths:?}",
        );
        assert!(
            paths.iter().any(|p| p.ends_with("payments/repos/.gitkeep")),
            "the new team's .gitkeep scaffold must be pending, got {paths:?}",
        );
    }

    /// The resolved half of the AddPersonByUsername branch: once
    /// `resolve_and_add_person` produces the `AppliedEdit`, the local command
    /// writes it to the working tree (no commit) and it surfaces as a pending
    /// publish path — same shape as the CreateTeam local write, proving the add
    /// branch is a plain file-only mutation. (The network resolve itself is
    /// covered by the pure `apply_add_person` / `fetch_github_profile` units.)
    #[test]
    fn add_person_local_write_surfaces_people_yml_in_publish_set() {
        use crate::commands::git_sync::publish_set_impl;
        use crate::context_repo::apply_add_person_by_name;

        let tmp = tempfile::tempdir().unwrap();
        init_org_repo(tmp.path(), "teams: []\n");
        let before = main_oid(tmp.path());

        let (applied, _) = apply_add_person_by_name("New Dev", tmp.path()).unwrap();
        assert_eq!(
            applied.commit_title, "Add New Dev",
            "toast copy reads Add, not Invite"
        );
        write_edit_to_workdir(tmp.path(), &applied).unwrap();

        // No commit on main — the add is a pending working-tree change.
        assert_eq!(main_oid(tmp.path()), before, "no new commit on main");

        let people = std::fs::read_to_string(tmp.path().join("people.yml")).unwrap();
        assert!(
            // SW-FG-4: written in the new identities shape.
            people.contains("New Dev"),
            "people.yml must carry the added person, got {people}",
        );

        let set = publish_set_impl(tmp.path(), None, None).expect("publish set");
        let paths: Vec<&str> = set
            .buckets
            .iter()
            .flat_map(|b| b.files.iter())
            .chain(set.general.files.iter())
            .map(|f| f.path.as_str())
            .collect();
        assert!(
            paths.contains(&"people.yml"),
            "people.yml must be a pending publish path, got {paths:?}",
        );
    }

    #[test]
    fn create_team_local_on_zero_team_org() {
        // teams.yml present but empty (post-OB-2 shape).
        let tmp = tempfile::tempdir().unwrap();
        init_org_repo(tmp.path(), "teams: []\n");

        create_team_pipeline(tmp.path(), "first", 7);

        let loaded = load_org("acme", tmp.path(), None);
        let org = loaded.org.expect("org should load");
        assert_eq!(org.teams.len(), 1);
        assert_eq!(org.teams[0].name, "first");
    }

    // ---------- org-registry-changed ----------

    const ORG: &str = "acme/context";
    const UID: u64 = 42;

    /// A mock app holding the auth state the commit path reads, a seeded org
    /// under a temp codesense home, and a sink recording every
    /// `org-registry-changed` payload.
    fn registry_event_fixture() -> (
        tauri::App<tauri::test::MockRuntime>,
        tempfile::TempDir,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        use tauri::Listener;
        let home = tempfile::tempdir().unwrap();
        let org_root = path_to_org_root(home.path(), &Principal::from(UID), ORG).unwrap();
        std::fs::create_dir_all(&org_root).unwrap();
        init_org_repo(
            &org_root,
            "teams:\n  - name: platform\n    source_repos: []\n",
        );
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        app.listen(ORG_REGISTRY_CHANGED_EVENT, move |event| {
            sink.lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
        (app, home, seen)
    }

    async fn commit(
        app: &tauri::App<tauri::test::MockRuntime>,
        home: &Path,
        edit: RegistryEdit,
    ) -> Result<LocalEditOutcome, String> {
        commit_registry_edit(app.handle(), home, &Principal::from(UID), ORG, edit).await
    }

    #[tokio::test]
    async fn a_committed_team_create_emits_org_registry_changed() {
        let (app, home, seen) = registry_event_fixture();
        commit(
            &app,
            home.path(),
            RegistryEdit::CreateTeam {
                team_name: "payments".into(),
                creator_id: UID,
                creator_username: "creator42".into(),
                creator_display_name: None,
                creator_avatar_url: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                serde_json::json!({ "principal": UID.to_string(), "orgSlug": ORG, "edit": "CreateTeam", "team": "payments" })
            ]
        );
    }

    #[tokio::test]
    async fn a_committed_team_rename_names_both_the_old_and_new_team() {
        let (app, home, seen) = registry_event_fixture();
        commit(
            &app,
            home.path(),
            RegistryEdit::RenameTeam {
                old_name: "platform".into(),
                new_name: "core".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![serde_json::json!({
                "principal": UID.to_string(),
                "orgSlug": ORG,
                "edit": "RenameTeam",
                "team": "core",
                "previousTeam": "platform",
            })]
        );
    }

    /// Parallel edits to the same `teams.yml` each read, modify and write it
    /// under one lock, so none is lost.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn parallel_team_creates_all_land() {
        let (app, home, _seen) = registry_event_fixture();
        let names: Vec<String> = (0..16).map(|i| format!("team-{i}")).collect();
        let tasks: Vec<_> = names
            .iter()
            .map(|name| {
                let (name, handle) = (name.clone(), app.handle().clone());
                let home = home.path().to_path_buf();
                tokio::spawn(async move {
                    commit_registry_edit(
                        &handle,
                        &home,
                        &Principal::from(UID),
                        ORG,
                        RegistryEdit::CreateTeam {
                            team_name: name,
                            creator_id: UID,
                            creator_username: "creator42".into(),
                            creator_display_name: None,
                            creator_avatar_url: None,
                        },
                    )
                    .await
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        let org_root = path_to_org_root(home.path(), &Principal::from(UID), ORG).unwrap();
        let org = load_org("acme".to_string(), &org_root, None)
            .org
            .expect("org loads");
        for name in &names {
            assert!(org.teams.iter().any(|t| &t.name == name), "{name} was lost");
        }
    }

    #[tokio::test]
    async fn a_refused_edit_emits_nothing() {
        let (app, home, seen) = registry_event_fixture();
        let refused = commit(
            &app,
            home.path(),
            RegistryEdit::DeleteTeam {
                team_name: "no-such-team".into(),
            },
        )
        .await;
        assert!(refused.is_err(), "deleting an absent team is refused");
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_edit_to_the_codesense_docs_is_refused_as_read_only() {
        let (app, home, seen) = registry_event_fixture();
        let docs = crate::context_repo::codesense_docs::bundle_info();
        let refused = commit_registry_edit(
            app.handle(),
            home.path(),
            &Principal::from(UID),
            &docs.slug,
            RegistryEdit::SetTeamLandingPage {
                team_name: docs.team.clone(),
                landing_page: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            refused,
            crate::context_repo::codesense_docs::READ_ONLY_MESSAGE
        );
        assert!(seen.lock().unwrap().is_empty());
    }

    // ---------- SW-FG-10a: reconcile_required_providers ----------
}
