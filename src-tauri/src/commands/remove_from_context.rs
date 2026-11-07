// SPDX-License-Identifier: MIT
//! SLW-PT-14: REMOVE a content item from a single Context (delete it in place).
//!
//! REMOVE is the destructive sibling of SLW-PT-12's MOVE: where MOVE writes the
//! target then removes the source, REMOVE only removes — there is no target. It
//! reuses the exact same [`ContextEndpoint`] resolution
//! ([`super::move_between_contexts::resolve_endpoint_root`]) so a personal or team
//! item is addressed by the same stable logical id and never leaks an internal
//! layer/overlay word.
//!
//! The gate ladder (shared → normal confirm; personal sole-copy → the §J
//! never-backed-up gate) is decided and driven on the frontend
//! (`remove-gate.ts`); this command executes the resolved delete and reports a
//! soft-reject outcome the FE renders as a toast rather than throwing. A REMOVE
//! of an absent item is `RejectedSourceMissing` (never an error — the item is
//! already gone, which is the user's goal), and an unresolvable context is
//! `RejectedUnbacked`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::async_helpers::await_blocking;
use super::move_between_contexts::{resolve_endpoint_root, ContextEndpoint};
use super::paths::codesense_home_dir;
use crate::content_engine::is_safe_logical_name;

/// Tauri-deserialized parameters for [`remove_content_from_context`].
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveFromContextParams {
    /// The Context the item lives in (and is removed from).
    pub context: ContextEndpoint,
    /// The logical name (content-root-relative path) of the item to remove.
    pub logical_name: String,
    /// The signed-in GitHub user id — the per-user namespace key and the
    /// Personal provider's owner.
    pub user_id: u64,
}

/// Result of a REMOVE. Mirrors the move outcome shape: a soft rejection is an
/// EXPECTED user outcome the FE renders as a toast, so only real I/O failures
/// are `Err`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RemoveFromContextOutcome {
    /// The item was deleted from its context.
    Removed,
    /// The item was not present at its content root — already gone.
    RejectedSourceMissing,
    /// The context could not be resolved to a backing path (e.g. Personal not
    /// set up).
    RejectedUnbacked,
}

/// Remove a content item from one Context. Off the UI thread (a disk unlink).
///
/// The gate ladder (shared confirm vs the §J personal sole-copy gate) is decided
/// on the FE; this command executes the resolved delete and reports the
/// soft-reject outcomes the FE renders.
#[tauri::command]
pub async fn remove_content_from_context(
    app: tauri::AppHandle,
    params: RemoveFromContextParams,
) -> Result<RemoveFromContextOutcome, String> {
    let active_org = super::move_between_contexts::active_org_slug(&app).await;
    let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
    let outcome = {
        let (home, params, active_org) = (home.clone(), params.clone(), active_org.clone());
        await_blocking("remove_content_from_context", move || {
            remove_from_context_impl(&home, active_org.as_deref(), &params)
        })
        .await?
    };
    Ok(outcome)
}

pub(crate) fn remove_from_context_for(
    home: &Path,
    active_org: Option<&str>,
    params: &RemoveFromContextParams,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<RemoveFromContextOutcome, String> {
    let readable = matches!(scope, crate::context_repo::caller_scope::CallerScope::Human)
        || resolve_endpoint_root(home, params.user_id, active_org, &params.context)
            .or_else(|| match &params.context {
                // Architecture: a valid private namespace is addressable before first use.
                // This lookup neither creates it nor admits unresolved team endpoints.
                ContextEndpoint::Personal { org_slug } => {
                    crate::context_repo::scratch_space::org_space_at(
                        home,
                        &crate::domain::Principal::from(params.user_id),
                        org_slug.as_deref().or(active_org)?,
                    )
                    .ok()
                    .map(|space| space.content_dir())
                }
                _ => None,
            })
            .is_some_and(|root| scope.allows(&root.join(&params.logical_name)));
    crate::context_repo::caller_scope::readback(
        readable,
        remove_from_context_impl(home, active_org, params),
    )
}

#[cfg(test)]
mod caller_scope_tests;

/// Testable core. Resolves the endpoint to its content root, then deletes the
/// single leaf. A missing item is a soft `RejectedSourceMissing` (already gone),
/// never an error.
fn remove_from_context_impl(
    codesense_home: &Path,
    active_org: Option<&str>,
    params: &RemoveFromContextParams,
) -> Result<RemoveFromContextOutcome, String> {
    if !is_safe_logical_name(&params.logical_name) {
        return Err(format!("unsafe logical name: {:?}", params.logical_name));
    }

    let Some(root) =
        resolve_endpoint_root(codesense_home, params.user_id, active_org, &params.context)
    else {
        return Ok(RemoveFromContextOutcome::RejectedUnbacked);
    };

    let path = root.join(&params.logical_name);
    // Never delete through a linked folder (or a link) into somewhere else.
    super::move_between_contexts::refuse_link_escape(&root, &path)?;
    if !path.exists() {
        return Ok(RemoveFromContextOutcome::RejectedSourceMissing);
    }

    crate::shared::fs_atomic::remove_file(&path)
        .map_err(|e| format!("couldn't remove '{}': {e}", path.display()))?;
    Ok(RemoveFromContextOutcome::Removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_engine::{content_layer_root, LayerId, ResolverContext};
    use crate::context_repo::scratch_space;
    use crate::domain::Principal;
    use std::path::PathBuf;
    use tempfile::tempdir;

    const UID: u64 = 42;
    /// The active org a bare Personal endpoint resolves to.
    const ORG: &str = "acme/repo";

    fn seed_personal(home: &Path) -> PathBuf {
        crate::context_repo::scratch_space::ensure_org_space(home, &Principal::from(UID), ORG)
            .unwrap();
        scratch_space::org_space(home, &Principal::from(UID), ORG)
            .unwrap()
            .content_dir()
    }

    // SLW-PT-B3: a team-all endpoint resolves the team's CONTENT root (the dir the
    // nav scans) — `TeamProjection` maps to `path_to_team_content_dir`. Seed +
    // assert there so REMOVE pins the nav-visible behavior, not the `install/` leaf.
    fn team_all_root(home: &Path, org: &str, team: &str) -> PathBuf {
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: home.to_path_buf(),
            user_id: UID,
            org_slug: org.to_string(),
            team_name: Some(team.to_string()),
        };
        content_layer_root(LayerId::TeamProjection, &ctx).unwrap()
    }

    fn team_all(org: &str, team: &str) -> ContextEndpoint {
        ContextEndpoint::TeamAll {
            org_slug: org.to_string(),
            team_name: team.to_string(),
        }
    }

    fn params(context: ContextEndpoint, logical: &str) -> RemoveFromContextParams {
        RemoveFromContextParams {
            context,
            logical_name: logical.to_string(),
            user_id: UID,
        }
    }

    #[test]
    fn removes_a_personal_item_deletes_it_in_place() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let content = seed_personal(home);
        std::fs::write(content.join("note.md"), "mine").unwrap();

        let out = remove_from_context_impl(
            home,
            Some(ORG),
            &params(ContextEndpoint::Personal { org_slug: None }, "note.md"),
        )
        .unwrap();
        assert_eq!(out, RemoveFromContextOutcome::Removed);
        assert!(!content.join("note.md").exists(), "the item is gone");
    }

    #[test]
    fn removes_a_shared_team_item() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let team_root = team_all_root(home, "acme/repo", "default");
        std::fs::create_dir_all(&team_root).unwrap();
        std::fs::write(team_root.join("skill.md"), "s").unwrap();

        let out = remove_from_context_impl(
            home,
            Some(ORG),
            &params(team_all("acme/repo", "default"), "skill.md"),
        )
        .unwrap();
        assert_eq!(out, RemoveFromContextOutcome::Removed);
        assert!(!team_root.join("skill.md").exists());
    }

    // SLW-PT-B3: REMOVE shares the fixed endpoint resolution, so a team-all doc
    // that lives in the team's CONTENT dir (where the nav scanned it) is now found
    // and removed — before the fix the endpoint looked in the `install/` leaf and
    // wrongly reported the doc already gone.
    #[test]
    fn removes_a_team_content_doc_from_the_content_dir_was_b3_soft_reject() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let content = team_all_root(home, "acme/repo", "default");
        std::fs::create_dir_all(&content).unwrap();
        std::fs::write(content.join("movable-doc.md"), "body").unwrap();

        let install_leaf = {
            let ctx = ResolverContext {
                context_root: None,
                codesense_home: home.to_path_buf(),
                user_id: UID,
                org_slug: "acme/repo".to_string(),
                team_name: Some("default".to_string()),
            };
            let org_root =
                crate::context_repo::path_to_org_root(home, &ctx.principal(), "acme/repo").unwrap();
            crate::context_repo::path_to_team_install_dir(&org_root, "default")
        };
        assert_ne!(content, install_leaf, "content vs install must differ");

        let out = remove_from_context_impl(
            home,
            Some(ORG),
            &params(team_all("acme/repo", "default"), "movable-doc.md"),
        )
        .unwrap();
        assert_eq!(out, RemoveFromContextOutcome::Removed);
        assert!(
            !content.join("movable-doc.md").exists(),
            "removed from content dir"
        );
    }

    #[test]
    fn removing_an_absent_item_is_source_missing_never_errors() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        seed_personal(home);
        let out = remove_from_context_impl(
            home,
            Some(ORG),
            &params(ContextEndpoint::Personal { org_slug: None }, "ghost.md"),
        )
        .unwrap();
        assert_eq!(out, RemoveFromContextOutcome::RejectedSourceMissing);
    }

    #[test]
    fn removing_from_absent_personal_is_unbacked_never_mints() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        // No personal repo seeded.
        let out = remove_from_context_impl(
            home,
            Some(ORG),
            &params(ContextEndpoint::Personal { org_slug: None }, "note.md"),
        )
        .unwrap();
        assert_eq!(out, RemoveFromContextOutcome::RejectedUnbacked);
        assert!(
            scratch_space::org_space(home, &Principal::from(UID), ORG).is_none(),
            "a remove must not mint the Personal repo"
        );
    }

    // SLW-PT-B4: REMOVE shares `ContextEndpoint`, so the FE→Rust serde seam bug hit
    // it too — a team-endpoint remove rejected "missing field org_slug" before any
    // delete ran. This feeds the EXACT camelCase params the shipped
    // `removeContentFromContext` wrapper emits and asserts they deserialize onto the
    // right endpoint. FAILS before `rename_all_fields`, PASSES after.
    #[test]
    fn deserializes_remove_params_with_team_endpoint_from_fe_camelcase_json() {
        let json = serde_json::json!({
            "context": { "kind": "teamAll", "orgSlug": "acme", "teamName": "alpha" },
            "logicalName": "note.md",
            "userId": 42,
        });
        let params: RemoveFromContextParams =
            serde_json::from_value(json).expect("FE remove params must deserialize");
        match params.context {
            ContextEndpoint::TeamAll {
                org_slug,
                team_name,
            } => {
                assert_eq!(org_slug, "acme");
                assert_eq!(team_name, "alpha");
            }
            other => panic!("expected TeamAll, got {other:?}"),
        }
        assert_eq!(params.logical_name, "note.md");
        assert_eq!(params.user_id, 42);
    }

    #[test]
    fn unsafe_logical_name_is_rejected() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        seed_personal(home);
        assert!(remove_from_context_impl(
            home,
            Some(ORG),
            &params(ContextEndpoint::Personal { org_slug: None }, "../escape.md")
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn remove_never_deletes_through_a_linked_folder() {
        let tmp = tempdir().unwrap();
        let home = tmp.path().join("home");
        let content = seed_personal(&home);
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.md"), "precious").unwrap();
        std::os::unix::fs::symlink(&outside, content.join("docs")).unwrap();
        let err = remove_from_context_impl(
            &home,
            Some(ORG),
            &RemoveFromContextParams {
                context: ContextEndpoint::Personal { org_slug: None },
                logical_name: "docs/keep.md".into(),
                user_id: UID,
            },
        )
        .unwrap_err();
        assert!(err.contains("link"), "{err}");
        assert!(outside.join("keep.md").exists());
    }
}

#[cfg(test)]
mod codesense_docs_tests {
    use super::*;
    use crate::context_repo::codesense_docs::{bundle_info, ensure_placed, READ_ONLY_MESSAGE};
    use crate::domain::Principal;

    #[test]
    fn nothing_is_removed_from_the_codesense_docs() {
        let tmp = tempfile::tempdir().unwrap();
        let placed = ensure_placed(
            tmp.path(),
            &Principal::from(42),
            include_bytes!("../../resources/codesense-docs.tar.gz"),
        )
        .unwrap();
        let params = RemoveFromContextParams {
            context: ContextEndpoint::TeamAll {
                org_slug: bundle_info().slug.clone(),
                team_name: bundle_info().team.clone(),
            },
            // SW-ODC-2 renamed the welcome page from README.md to README.html; pin the
            // read-only guard against a doc that still exists under the team content root.
            logical_name: "README.html".into(),
            user_id: 42,
        };
        let error = remove_from_context_impl(tmp.path(), Some("acme/repo"), &params).unwrap_err();
        assert!(error.contains(READ_ONLY_MESSAGE), "{error}");
        assert!(placed.readme_path.is_file());
    }
}
