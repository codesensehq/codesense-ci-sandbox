// SPDX-License-Identifier: MIT
//! Create a user-owned local context repository, then register it in one profile.
use std::path::{Component, Path, PathBuf};

use tauri::{Emitter, Manager};

use crate::context_repo::{
    codesense_docs, context_attachments, org_slug, scaffold_org_context_repo, ScaffoldOrgInputs,
};
use crate::domain::Principal;

/// Both desktop IPC and local MCP use this principal-fenced entrypoint.
#[tauri::command]
pub async fn create_local_organization(
    app: tauri::AppHandle,
    display_name: String,
    destination_path: String,
    expected_principal: String,
    from_example: Option<bool>,
) -> Result<super::org::ConnectOrgResult, String> {
    let home = super::paths::codesense_home_dir().ok_or("cannot resolve home directory")?;
    create_for_profile(
        &app,
        home,
        display_name,
        destination_path,
        expected_principal,
        from_example.unwrap_or(false),
    )
    .await
}

/// Explicit-home runtime adapter shared by IPC/MCP and native integration tests.
pub(crate) async fn create_for_profile<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: PathBuf,
    display_name: String,
    destination_path: String,
    expected_principal: String,
    from_example: bool,
) -> Result<super::org::ConnectOrgResult, String> {
    let principal = super::local_profile::active(app)?.principal;
    if principal.as_str() != expected_principal {
        return Err("Local profile changed; reload this window".into());
    }
    let handle = app.clone();
    let result = super::await_blocking("create local organization", move || {
        let profiles = handle.state::<super::local_profile::LocalProfileState>();
        // Profile selection and metadata recovery use this same lock. It spans
        // publication and registration, so a queued request cannot cross profiles.
        profiles.with_expected(Some(&expected_principal), || {
            // "Start from the CodeSense example" copies the placed docs repo's tree.
            let example = from_example.then(|| codesense_docs::docs_root(&home, &principal));
            create_at_with(
                &home,
                &principal,
                &display_name,
                Path::new(&destination_path),
                example.as_deref(),
            )
        })
    })
    .await?;
    if let Err(error) = app.emit("context-attachments-changed", ()) {
        log::warn!("Context repo created; refresh notification failed: {error}");
    }
    Ok(result)
}

fn new_destination(destination: &Path) -> Result<PathBuf, String> {
    if !destination.is_absolute()
        || destination
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err("Choose an absolute new folder beneath an existing parent".into());
    }
    let name = destination
        .file_name()
        .ok_or("Choose a new child folder, not a filesystem root")?;
    let parent = destination
        .parent()
        .ok_or("Choose an existing parent folder")?;
    // The default location (`~/CodeSense/<name>`) may not exist yet.
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    let parent = parent
        .canonicalize()
        .map_err(|e| format!("Choose an existing parent folder: {e}"))?;
    if !parent.is_dir() || destination.to_str().is_none() {
        return Err("Choose an existing parent and a valid UTF-8 folder path".into());
    }
    let destination = parent.join(name);
    require_absent(&destination)?;
    Ok(destination)
}

fn require_absent(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(format!(
            "{} already exists. Choose a new folder, or use Connect a context repo… to connect it.",
            path.display()
        )),
        Err(e) => Err(format!("Cannot inspect {}: {e}", path.display())),
    }
}

/// Atomic no-replacement rename: even an empty directory created by another
/// process between admission and publication must survive unchanged.
fn publish_new(staged: &Path, destination: &Path) -> Result<(), String> {
    #[cfg(unix)]
    let result = rustix::fs::renameat_with(
        rustix::fs::CWD,
        staged,
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from);
    // Windows MoveFile semantics refuse an existing destination directory.
    #[cfg(windows)]
    let result = std::fs::rename(staged, destination);
    result.map_err(|e| {
        format!(
            "Cannot create {}: {e}. Choose a new folder, or use Connect a context repo… to connect it.",
            destination.display()
        )
    })
}

#[cfg(test)]
pub(crate) fn create_at(
    home: &Path,
    principal: &Principal,
    display_name: &str,
    destination: &Path,
) -> Result<super::org::ConnectOrgResult, String> {
    create_at_with(home, principal, display_name, destination, None)
}

/// [`create_at`], optionally seeded with the CodeSense example (the placed docs repo
/// at `example`) before the new repo is validated and published.
pub(crate) fn create_at_with(
    home: &Path,
    principal: &Principal,
    display_name: &str,
    destination: &Path,
    example: Option<&Path>,
) -> Result<super::org::ConnectOrgResult, String> {
    create_with_attach(
        home,
        principal,
        display_name,
        destination,
        example,
        context_attachments::attach_admitted,
    )
}

fn create_with_attach(
    home: &Path,
    principal: &Principal,
    display_name: &str,
    destination: &Path,
    example: Option<&Path>,
    attach: impl FnOnce(
        &Path,
        &Principal,
        &Path,
    ) -> Result<context_attachments::ContextAttachment, String>,
) -> Result<super::org::ConnectOrgResult, String> {
    let display_name = display_name.trim();
    let slug = super::paths::sanitize_name(display_name)?;
    org_slug::require_valid_org_slug(&slug)?;
    let _admission = context_attachments::admission(home, principal, &slug).blocking_lock_owned();
    // Name conflicts first: a refused create must not leave a new parent folder behind.
    if let Some(existing) = context_attachments::registered(home, principal, &slug)? {
        return Err(format!(
            "A context repo named {slug} already exists at {}. Choose another name.",
            existing.root.display()
        ));
    }
    if crate::context_repo::path_to_managed_org_root(home, principal, &slug).exists() {
        return Err(format!(
            "A context repo named {slug} already exists in managed storage. Choose another name."
        ));
    }
    let destination = new_destination(destination)?;
    // TempDir owns only this unguessable staging sibling. Once its child is
    // published it owns no repository and can never delete the user's result.
    let staging = tempfile::Builder::new()
        .prefix(".codesense-create-")
        .tempdir_in(destination.parent().ok_or("Missing parent folder")?)
        .map_err(|e| format!("Cannot create staging folder: {e}"))?;
    let staged = staging.path().join("context");
    scaffold_org_context_repo(
        &staged,
        &ScaffoldOrgInputs {
            slug: slug.clone(),
            display_name: display_name.into(),
            created_at: chrono::Utc::now(),
        },
    )
    .map_err(|e| e.to_string())?;
    if let Some(example) = example {
        codesense_docs::copy_example(example, &staged)?;
    }
    let loaded = crate::domain::load_org(slug.clone(), &staged, None);
    if loaded.org.is_none() || !loaded.file_errors.is_empty() {
        return Err(format!(
            "New context registry failed validation: {:?}",
            loaded.file_errors
        ));
    }
    require_absent(&destination)?;
    publish_new(&staged, &destination)?;
    let attachment = attach(home, principal, &destination).map_err(|e| format!(
        "Context repo created at {}, but it is not connected: {e}. Use Connect a context repo… with this folder to retry.", destination.display()
    ))?;
    let loaded_org = super::registry::load_attached_org(&slug, &attachment.root);
    Ok(super::org::ConnectOrgResult {
        org_slug: slug,
        org_root: attachment.root.to_string_lossy().into_owned(),
        clone_outcome: "created".into(),
        loaded_org,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_canonical_empty_org_unborn_git_and_persistent_registration() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("state");
        let root = tmp.path().join("different-basename");
        let principal = Principal::local_guest();
        let result = create_at(&home, &principal, "  Example Org  ", &root).unwrap();
        assert_eq!(result.org_slug, "example-org");
        assert_eq!(result.clone_outcome, "created");
        assert!(result.loaded_org.file_errors.is_empty());
        let org = result.loaded_org.org.unwrap();
        assert_eq!(org.display_name, "Example Org");
        assert_eq!(org.remote_url, None);
        assert_eq!(org.remote_provider, None);
        assert!(org.teams.is_empty());
        assert!(org.people.is_empty());
        for file in ["org.yml", "people.yml", "teams.yml"] {
            assert!(root.join(file).is_file(), "{file}");
        }
        assert!(
            !root.join("manifest.yml").exists(),
            "manifest.yml must not be scaffolded"
        );
        let repo = git2::Repository::open(&root).unwrap();
        assert_eq!(
            repo.head().err().unwrap().code(),
            git2::ErrorCode::UnbornBranch
        );
        assert_eq!(
            repo.find_reference("HEAD").unwrap().symbolic_target(),
            Ok(Some("refs/heads/main"))
        );
        assert!(repo.index().unwrap().is_empty());
        assert!(repo.remotes().unwrap().is_empty());
        assert_eq!(
            crate::context_repo::path_to_org_root(&home, &principal, "example-org").unwrap(),
            root.canonicalize().unwrap()
        );
        assert_eq!(
            super::super::org::discover_local_orgs_impl(&home, &principal)
                .unwrap()
                .len(),
            1
        );
        context_attachments::detach(&home, &principal, "example-org").unwrap();
        assert!(root.join("org.yml").is_file());
    }

    #[test]
    fn creates_from_the_codesense_example_under_a_new_default_location() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("state");
        let principal = Principal::local_guest();
        let docs = codesense_docs::ensure_placed(
            &home,
            &principal,
            include_bytes!("../../resources/codesense-docs.tar.gz"),
        )
        .unwrap();
        // `~/CodeSense/<name>`: the parent folder doesn't exist yet.
        let root = tmp.path().join("CodeSense").join("acme");
        let result = create_at_with(
            &home,
            &principal,
            "Acme",
            &root,
            Some(&docs.attachment.root),
        )
        .unwrap();
        assert_eq!(result.org_slug, "acme");
        let org = result.loaded_org.org.unwrap();
        let team = &codesense_docs::bundle_info().team;
        assert_eq!(org.display_name, "Acme");
        assert_eq!(
            org.teams.iter().map(|t| &t.name).collect::<Vec<_>>(),
            vec![team]
        );
        assert!(root.join(team).join("content/README.md").is_file());
        // An editable copy: writes are not refused, and it is not the docs repo.
        codesense_docs::refuse_write_at(&root.join(team).join("content/README.md")).unwrap();
        assert!(git2::Repository::open(&root)
            .unwrap()
            .remotes()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn registry_edits_to_the_codesense_docs_are_refused_before_any_write() {
        use crate::context_repo::RegistryEdit;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("state");
        let principal = Principal::local_guest();
        let (place_home, place_principal) = (home.clone(), principal.clone());
        let docs = tokio::task::spawn_blocking(move || {
            codesense_docs::ensure_placed(
                &place_home,
                &place_principal,
                include_bytes!("../../resources/codesense-docs.tar.gz"),
            )
        })
        .await
        .unwrap()
        .unwrap();
        let teams_before = std::fs::read(docs.attachment.root.join("teams.yml")).unwrap();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let edit: RegistryEdit = serde_json::from_value(
            serde_json::json!({"type":"CreateTeam", "team_name":"mine", "creator_username":"Local profile"}),
        )
        .unwrap();
        let error = super::super::registry_local::commit_registry_edit(
            app.handle(),
            &home,
            &principal,
            &docs.attachment.slug,
            edit,
        )
        .await
        .unwrap_err();
        assert!(error.contains(codesense_docs::READ_ONLY_MESSAGE), "{error}");
        assert_eq!(
            std::fs::read(docs.attachment.root.join("teams.yml")).unwrap(),
            teams_before
        );
    }

    #[test]
    fn rejects_invalid_occupied_and_conflicting_destinations_without_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("state");
        let principal = Principal::local_guest();
        for name in ["", "   ", "!!!", ".."] {
            assert!(create_at(&home, &principal, name, &tmp.path().join("new")).is_err());
        }
        assert!(!tmp.path().join("new").exists());
        assert!(create_at(&home, &principal, "Valid", Path::new("relative")).is_err());
        let occupied = tmp.path().join("occupied");
        std::fs::create_dir(&occupied).unwrap();
        assert!(create_at(&home, &principal, "Valid", &occupied)
            .unwrap_err()
            .contains("already exists"));
        std::fs::write(occupied.join("mine"), "keep").unwrap();
        git2::Repository::init(&occupied).unwrap();
        assert!(create_at(&home, &principal, "Valid", &occupied).is_err());
        assert_eq!(
            std::fs::read_to_string(occupied.join("mine")).unwrap(),
            "keep"
        );
        create_at(&home, &principal, "Valid", &tmp.path().join("one")).unwrap();
        assert!(create_at(&home, &principal, "Valid", &tmp.path().join("two")).is_err());
        assert!(!tmp.path().join("two").exists());
    }

    #[test]
    fn publication_never_replaces_a_concurrent_empty_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let staged = tmp.path().join("staged");
        let destination = tmp.path().join("occupied");
        std::fs::create_dir(&staged).unwrap();
        std::fs::write(staged.join("owned"), "ours").unwrap();
        std::fs::create_dir(&destination).unwrap();
        assert!(publish_new(&staged, &destination).is_err());
        assert!(destination.is_dir());
        assert!(!destination.join("owned").exists());
        assert!(staged.join("owned").exists());
    }

    #[test]
    fn failed_registration_preserves_valid_repository_with_recovery_guidance() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("state");
        let root = tmp.path().join("created");
        let principal = Principal::local_guest();
        let error = create_with_attach(&home, &principal, "Created", &root, None, |_, _, _| {
            Err("disk full".into())
        })
        .unwrap_err();
        assert!(error.contains("not connected"));
        assert!(error.contains("Connect a context repo…"));
        assert!(error.contains(root.to_str().unwrap()));
        assert!(crate::domain::load_org("created", &root, None)
            .org
            .is_some());
        assert!(context_attachments::list(&home, &principal)
            .unwrap()
            .is_empty());
        context_attachments::attach(&home, &principal, &root).unwrap();
        assert!(!std::fs::read_dir(tmp.path()).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".codesense-create-")));
    }
    // SW-CR-5 (CR-1 R2 follow-up): user-typed display names that sanitize to a
    // reserved single-segment slug (`Builtin` → `builtin`, `Local` → `local`)
    // are refused by `require_valid_org_slug` so the slug can't alias onto the
    // reserved namespace. `_Personal` is NOT in this battery because
    // `sanitize_name` replaces `_` with `-` (so a user-typed `_Personal` lands
    // at the valid `-personal`); the `_personal` reservation is only reachable
    // from code paths that bypass `sanitize_name`. The error reads a bit
    // cryptic (`invalid org_slug: reserved owner prefix…`) because the chain
    // is `sanitize_name` → `require_valid_org_slug`; a future UX pass may map
    // these two display-name cases to friendlier copy. No files are created
    // on refuse.
    #[test]
    fn create_refuses_a_display_name_that_sanitizes_to_a_reserved_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("state");
        let principal = Principal::local_guest();
        for (name, destination_folder) in [("Builtin", "builtin-dest"), ("Local", "local-dest")] {
            let destination = tmp.path().join(destination_folder);
            let error = create_at(&home, &principal, name, &destination).unwrap_err();
            assert!(error.contains("invalid org_slug:"), "{name}: {error}");
            assert!(
                !destination.exists(),
                "{name}: {destination:?} must not exist"
            );
        }
    }

    #[test]
    fn stale_profile_refuses_before_creating_any_files() {
        let tmp = tempfile::tempdir().unwrap();
        let profiles = super::super::local_profile::LocalProfileState::load(tmp.path()).unwrap();
        let destination = tmp.path().join("not-created");
        let error = profiles
            .with_expected(Some("42"), || {
                create_at(tmp.path(), &Principal::from(42u64), "Stale", &destination)
            })
            .unwrap_err();
        assert!(error.contains("profile changed"));
        assert!(!destination.exists());
    }

    #[tokio::test]
    async fn local_setup_composes_create_team_source_discovery_and_activation() {
        use crate::context_repo::RegistryEdit;
        use crate::mcp::{routine_tools::ToolEnvelope, setup_tools};
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("state");
        let root = tmp.path().join("context");
        let principal = Principal::local_guest();
        let (create_home, create_root, create_principal) =
            (home.clone(), root.clone(), principal.clone());
        tokio::task::spawn_blocking(move || {
            create_at(&create_home, &create_principal, "Example", &create_root)
        })
        .await
        .unwrap()
        .unwrap();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let edit: RegistryEdit = serde_json::from_value(
            serde_json::json!({"type":"CreateTeam", "team_name":"engineering", "creator_username":"Local profile"}),
        )
        .unwrap();
        super::super::registry_local::commit_registry_edit(
            app.handle(),
            &home,
            &principal,
            "example",
            edit,
        )
        .await
        .unwrap();
        super::super::registry_local::commit_registry_edit(
            app.handle(),
            &home,
            &principal,
            "example",
            RegistryEdit::AddSourceRepoToTeam {
                team_name: "engineering".into(),
                owner: "example".into(),
                repo: "app".into(),
                provider: None,
                host: None,
            },
        )
        .await
        .unwrap();
        let source = tmp.path().join("source");
        let repo = git2::Repository::init(&source).unwrap();
        repo.remote("origin", "https://github.com/example/app.git")
            .unwrap();
        let attachment = super::super::attachments::existing_checkout_attachment(&source).unwrap();
        super::super::attachments::upsert_attachment_and_reconcile(
            &home,
            principal.numeric_id().unwrap(),
            attachment,
            &crate::content_engine::sync::WaveCoordinator::default(),
            &crate::mcp::state::ActiveTeamState::default(),
        )
        .await
        .unwrap();
        let orgs = setup_tools::describe_orgs(&home, &principal).unwrap();
        assert_eq!(orgs.len(), 1);
        assert!(orgs[0].load_errors.is_empty());
        assert_eq!(orgs[0].teams[0].name, "engineering");
        assert_eq!(
            orgs[0].teams[0].source_repos[0].attachment,
            setup_tools::AttachmentStatus::Attached
        );
        let switched = setup_tools::set_active_team(
            home,
            principal.numeric_id().unwrap(),
            setup_tools::SetActiveTeamParams {
                org: "example".into(),
                team: "engineering".into(),
            },
            |activation| async move {
                assert_eq!(activation.source_repos.len(), 1);
                assert_eq!(activation.team, "engineering");
                Ok(())
            },
        )
        .await
        .unwrap();
        assert!(matches!(switched, ToolEnvelope::Ok(_)));
    }
}
