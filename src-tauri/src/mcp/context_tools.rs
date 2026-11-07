// SPDX-License-Identifier: MIT
//! Context tool bodies shared by the desktop router and its transport fixture.
//! The caller supplies an already authorized, bound context; UI selection is absent.
use super::handler::{
    decode_layer_file_content, engine_err_to_mcp, SenseSyncAdoptParams,
    SenseSyncReadLayerFileParams,
};
use crate::commands::{content_engine, route_to_context::ActiveContext};
use crate::content_engine::dispatcher::SuppressionHandle;
use rmcp::ErrorData;

pub(crate) async fn adopt_for(
    ctx: ActiveContext,
    params: SenseSyncAdoptParams,
    scope: &crate::context_repo::caller_scope::CallerScope,
    suppression: impl FnOnce() -> Result<SuppressionHandle, content_engine::Error>,
) -> Result<serde_json::Value, ErrorData> {
    let readable = matches!(scope, crate::context_repo::caller_scope::CallerScope::Human)
        || content_engine::derive_resolver_context(
            ctx.codesense_home.clone(),
            ctx.user_id,
            ctx.org_slug.clone(),
            ctx.team_name.clone(),
        )
        .ok()
        .and_then(|resolver| {
            crate::content_engine::content_layer_root(params.target_layer, &resolver)
        })
        .is_some_and(|root| scope.allows(&root.join(&params.logical_name)));
    let result = adopt(ctx, params, suppression).await;
    crate::context_repo::caller_scope::readback(readable, Ok(result))
        .map_err(|e| ErrorData::invalid_params(e, None))?
}

async fn adopt(
    ctx: ActiveContext,
    params: SenseSyncAdoptParams,
    suppression: impl FnOnce() -> Result<SuppressionHandle, content_engine::Error>,
) -> Result<serde_json::Value, ErrorData> {
    let expected_layer_hash = decode_expected_hash(params.expected_layer_hash.as_deref())?;
    let outcome = content_engine::write_to_layer_with_suppression(
        content_engine::WriteToLayerParams {
            layer_id: params.target_layer,
            logical_name: params.logical_name,
            content: params.content.into_bytes(),
            code_repo_slug: Some(params.code_repo_slug),
            codesense_home: ctx.codesense_home.to_string_lossy().into_owned(),
            user_id: ctx.user_id,
            org_slug: ctx.org_slug,
            team_name: ctx.team_name,
            expected_layer_hash,
            force: false,
        },
        suppression,
    )
    .await
    .map_err(engine_err_to_mcp)?;
    Ok(serde_json::json!({"outcome":outcome}))
}
#[cfg(test)]
pub(crate) async fn read(
    ctx: ActiveContext,
    params: SenseSyncReadLayerFileParams,
) -> Result<serde_json::Value, ErrorData> {
    read_for(ctx, params, Default::default()).await
}

pub(crate) async fn read_for(
    ctx: ActiveContext,
    params: SenseSyncReadLayerFileParams,
    scope: crate::context_repo::caller_scope::CallerScope,
) -> Result<serde_json::Value, ErrorData> {
    let bytes = content_engine::read_layer_file_for(
        content_engine::ReadLayerFileParams {
            layer_id: params.layer_id,
            logical_name: params.logical_name,
            code_repo_slug: params.code_repo_slug,
            codesense_home: ctx.codesense_home.to_string_lossy().into_owned(),
            user_id: ctx.user_id,
            org_slug: ctx.org_slug,
            team_name: ctx.team_name,
        },
        &scope,
    )
    .await
    .map_err(|error| match error {
        content_engine::Error::FileNotFound { .. }
            if !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) =>
        {
            ErrorData::invalid_params(crate::context_repo::caller_scope::MISSING, None)
        }
        error => engine_err_to_mcp(error),
    })?;
    Ok(decode_layer_file_content(bytes))
}

fn decode_expected_hash(value: Option<&str>) -> Result<Option<[u8; 32]>, ErrorData> {
    value
        .map(|value| {
            if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(ErrorData::invalid_params(
                    "expectedLayerHash must be a 64-digit blake3 hex hash",
                    None,
                ));
            }
            let mut hash = [0; 32];
            for (index, byte) in hash.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                    .map_err(|_| ErrorData::invalid_params("invalid expectedLayerHash", None))?;
            }
            Ok(hash)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn reverse_mcp_context_hash_preserves_conflicts_and_bound_destination() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ActiveContext {
            codesense_home: root.path().into(),
            user_id: 41,
            org_slug: "acme/context".into(),
            team_name: Some("bound".into()),
        };
        let dispatcher = crate::content_engine::dispatcher::Dispatcher::new(1, 100, 1000, 500);
        let params = |content: &str, hash: Option<String>| SenseSyncAdoptParams {
            code_repo_path: "/remote/ignored".into(),
            code_repo_slug: crate::domain::SourceRepoSlug::new("acme", "api").unwrap(),
            logical_name: "notes/chosen.md".into(),
            target_layer: crate::content_engine::layers::LayerId::TeamProjection,
            content: content.into(),
            expected_layer_hash: hash,
        };
        let suppression = || Ok(dispatcher.suppression_handle());
        assert_eq!(
            adopt(ctx.clone(), params("original", None), suppression)
                .await
                .unwrap()["outcome"]["kind"],
            "written"
        );
        let original = blake3::hash(b"original").to_hex().to_string();
        assert_eq!(
            adopt(
                ctx.clone(),
                params("updated", Some(original.clone())),
                suppression
            )
            .await
            .unwrap()["outcome"]["kind"],
            "written"
        );
        assert_eq!(
            adopt(ctx.clone(), params("stale", Some(original)), suppression)
                .await
                .unwrap()["outcome"]["kind"],
            "rejectedConflict"
        );
        for invalid in ["", "xyz", &"g".repeat(64), &"a".repeat(63)] {
            assert!(adopt(
                ctx.clone(),
                params("bad", Some(invalid.into())),
                suppression
            )
            .await
            .is_err());
        }
        let output = read(
            ctx,
            SenseSyncReadLayerFileParams {
                layer_id: crate::content_engine::layers::LayerId::TeamProjection,
                logical_name: "notes/chosen.md".into(),
                code_repo_slug: Some(crate::domain::SourceRepoSlug::new("acme", "api").unwrap()),
            },
        )
        .await
        .unwrap();
        assert_eq!(output["contentUtf8"], "updated");
    }

    /// A caller still sending the old repo-agnostic
    /// `targetLayer:"TeamWorkspace"` lands in the team's `content/` (never the
    /// retired `install/`); `"TeamRepo"` is refused and never lands team-wide.
    #[tokio::test]
    async fn retired_layer_names_from_an_old_caller() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ActiveContext {
            codesense_home: root.path().into(),
            user_id: 41,
            org_slug: "acme/context".into(),
            team_name: Some("bound".into()),
        };
        let dispatcher = crate::content_engine::dispatcher::Dispatcher::new(1, 100, 1000, 500);
        let suppression = || Ok(dispatcher.suppression_handle());
        let json = |layer: &str| {
            serde_json::json!({
                "codeRepoPath": "/w",
                "codeRepoSlug": {"owner":"acme","repo":"api"},
                "logicalName": "notes/old-caller.md",
                "targetLayer": layer,
                "content": "from an old caller",
            })
        };
        let params: SenseSyncAdoptParams = serde_json::from_value(json("TeamWorkspace")).unwrap();
        assert_eq!(
            adopt(ctx.clone(), params, suppression).await.unwrap()["outcome"]["kind"],
            "written"
        );
        let org = crate::context_repo::path_to_org_root(
            root.path(),
            &crate::domain::Principal::from(41u64),
            "acme/context",
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(
                crate::context_repo::path_to_team_content_dir(&org, "bound")
                    .join("notes/old-caller.md")
            )
            .unwrap(),
            "from an old caller"
        );
        assert!(
            !crate::context_repo::path_to_team_install_dir(&org, "bound")
                .join("notes/old-caller.md")
                .exists()
        );
        let refused = serde_json::from_value::<SenseSyncAdoptParams>(json("TeamRepo")).unwrap_err();
        assert!(
            refused.to_string().contains("reverse_sync_worktree"),
            "{refused}"
        );
    }
}

pub(crate) async fn read_request<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    session: Option<&str>,
    remote: Option<crate::commands::dashboard_launch::LaunchTeam>,
    ctx: ActiveContext,
    params: SenseSyncReadLayerFileParams,
) -> Result<serde_json::Value, ErrorData> {
    let scope = crate::context_repo::caller_scope::for_session(app, session, remote).await;
    read_for(ctx, params, scope).await
}

#[cfg(test)]
mod caller_scope_tests {
    use super::*;
    use crate::context_repo::caller_scope::tests::AppFixture;
    use tauri::Manager;
    #[test]
    fn layer_request_uses_session_scope_even_when_the_ui_selects_parent() {
        let f = AppFixture::new();
        f.session("child-session", Some("child"));
        f.app
            .state::<crate::mcp::state::ActiveTeamState>()
            .update(crate::mcp::state::ActiveTeam {
                org_slug: "acme/ctx".into(),
                team_name: "parent".into(),
            });
        let read = |name: &str, team: &str| {
            let ctx = ActiveContext {
                codesense_home: f.fixture.home.clone(),
                user_id: 1,
                org_slug: "acme/ctx".into(),
                team_name: Some(team.into()),
            };
            let params = SenseSyncReadLayerFileParams {
                layer_id: crate::content_engine::layers::LayerId::TeamProjection,
                logical_name: name.into(),
                code_repo_slug: None,
            };
            tauri::async_runtime::block_on(read_request(
                f.app.handle(),
                Some("child-session"),
                None,
                ctx,
                params,
            ))
        };
        let denied = read("secret.md", "parent").unwrap_err();
        let missing = read("missing.md", "parent").unwrap_err();
        assert_eq!(denied, missing);
        assert_eq!(
            read("shared.md", "parent").unwrap()["contentUtf8"],
            "parent-shared.md-bytes"
        );
        assert_eq!(
            read("secret.md", "sibling").unwrap()["contentUtf8"],
            "sibling-secret.md-bytes"
        );
    }
}
