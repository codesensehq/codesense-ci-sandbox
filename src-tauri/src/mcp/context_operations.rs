// SPDX-License-Identifier: MIT
//! The explicit context operations' public shapes: every target
//! form, and the selected forms kept for compatibility, which remap onto the
//! one engine in [`explicit`].
use crate::commands::route_to_context::{ActiveContext, ContextResolution};
use crate::install::story_linker::selected_context::{HydrateItem, PreviewItem, PublishItem};
use rmcp::model::ErrorData;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;

pub(crate) mod explicit;
mod local_admission;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum ContextSelection {
    Team,
    Worktree { id: String },
    Launcher,
}

pub(crate) fn bind_context(
    ctx: &mut crate::content_engine::ResolverContext,
    context: Option<ContextSelection>,
    launcher: Option<&str>,
) -> Result<(), String> {
    let id = match context {
        None | Some(ContextSelection::Team) => { ctx.context_root = None; return Ok(()); }
        Some(ContextSelection::Worktree { id }) => id,
        Some(ContextSelection::Launcher) => launcher.ok_or("no launcherContext was recorded for this session; choose a context worktree explicitly")?.to_string(),
    };
    let row = crate::install::worktree_index::resolve_context_record(
        &ctx.codesense_home,
        &ctx.principal(),
        &ctx.org_slug,
        None,
        &id,
    )?;
    if row.id != id {
        return Err("context.worktree requires a registered id, not a name".into());
    }
    ctx.context_root = Some(crate::agents::manifest_resolve::ContextRoot {
        id: row.id,
        root: row.root,
    });
    Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum WorkspaceTarget {
    #[serde(rename_all = "camelCase")]
    Local {
        code_repo_path: PathBuf,
        session_id: String,
    },
    #[serde(rename_all = "camelCase")]
    Remote {
        machine_id: String,
        workspace_handle: String,
    },
}
impl WorkspaceTarget {
    pub(crate) fn context_session(&self, connection: Option<String>) -> Option<String> {
        match self {
            Self::Local { session_id, .. } => Some(session_id.clone()),
            Self::Remote { .. } => connection,
        }
    }
}

/// A workspace an explicit operation names when the person asks
/// from the app (the unsaved-team-edits list) — the app is the requester, so
/// no agent session is borrowed. Deliberately NOT `JsonSchema`: no MCP tool
/// can take it (agents go through their own session, `WorkspaceTarget`).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum AppTarget {
    /// One of the active team's workspaces on this Mac.
    #[serde(rename_all = "camelCase")]
    Local { code_repo_path: PathBuf },
    /// One of the active team's machine workspaces (open in the viewer or
    /// not: admitted by its durable preparation receipt).
    #[serde(rename_all = "camelCase")]
    Remote {
        machine_id: String,
        workspace_handle: String,
    },
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) enum SelectedKind {
    #[serde(rename = "selected")]
    Selected,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct HydrateSelected {
    #[serde(default)]
    pub context: Option<crate::mcp::context_operations::ContextSelection>,
    pub kind: SelectedKind,
    pub request_id: String,
    pub target: WorkspaceTarget,
    pub items: Vec<HydrateItem>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PublishSelected {
    #[serde(default)]
    pub context: Option<crate::mcp::context_operations::ContextSelection>,
    pub kind: SelectedKind,
    pub request_id: String,
    pub target: WorkspaceTarget,
    pub items: Vec<PublishItem>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PreviewSelected {
    #[serde(default)]
    pub context: Option<crate::mcp::context_operations::ContextSelection>,
    pub kind: SelectedKind,
    pub target: WorkspaceTarget,
    pub items: Vec<PreviewItem>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StatusParams {
    pub target: WorkspaceTarget,
    pub request_id: String,
}
pub(crate) fn invalid(error: impl std::fmt::Display) -> ErrorData {
    ErrorData::invalid_params(error.to_string(), None)
}

impl HydrateSelected {
    /// The selected form on the engine: its `doc:` refs are the paths.
    pub(crate) fn into_explicit(self) -> Result<explicit::HydrateParams, ErrorData> {
        let _ = self.kind;
        Ok(explicit::HydrateParams {
            context: self.context,
            target: self.target,
            paths: self
                .items
                .iter()
                .map(|item| explicit::doc_path(&item.reference))
                .collect::<Result<_, _>>()
                .map_err(invalid)?,
            sets: Vec::new(),
            prune: false,
            overwrite_modified: false,
            operation_id: Some(self.request_id),
        })
    }
}

impl PublishSelected {
    /// The selected form on the engine: its paths, with their expected
    /// source and workspace hashes as conflict checks.
    pub(crate) fn into_explicit(
        self,
    ) -> Result<
        (
            explicit::PublishParams,
            std::collections::BTreeMap<String, crate::agents::prepare::publish::Expect>,
        ),
        ErrorData,
    > {
        let _ = self.kind;
        let mut expect = std::collections::BTreeMap::new();
        let mut paths = Vec::new();
        for item in self.items {
            let _ = item.op;
            expect.insert(
                item.path.clone(),
                crate::agents::prepare::publish::Expect {
                    source: Some(
                        explicit::parse_hash(&item.expected_source.hash).map_err(invalid)?,
                    ),
                    workspace: Some(
                        explicit::parse_hash(&item.expected_workspace.hash).map_err(invalid)?,
                    ),
                    // The item's `ref` names the team file a new one goes to.
                    target: Some(explicit::doc_path(&item.reference).map_err(invalid)?),
                    stat: None,
                },
            );
            paths.push(item.path);
        }
        Ok((
            explicit::PublishParams {
                context: self.context,
                target: self.target,
                paths,
                sets: Vec::new(),
                to: None,
                include_deletes: false,
                operation_id: Some(self.request_id),
            },
            expect,
        ))
    }
}

impl PreviewSelected {
    pub(crate) fn into_explicit(self) -> explicit::PreviewParams {
        let _ = self.kind;
        explicit::PreviewParams {
            context: self.context,
            target: self.target,
            paths: self.items.into_iter().map(|item| item.path).collect(),
            sets: Vec::new(),
            to: None,
            include_deletes: false,
        }
    }
}
pub(crate) fn local_repository(
    ctx: &crate::content_engine::ResolverContext,
    root: &std::path::Path,
) -> Result<crate::domain::SourceRepoSlug, ErrorData> {
    let stories = crate::install::worktree_index::story_registrations(&ctx.codesense_home, root)
        .map_err(invalid)?;
    if !stories.is_empty() {
        let mut owned = stories.into_iter().filter(|(principal, record)| {
            principal == &ctx.principal()
                && record.org_slug == ctx.org_slug
                && crate::install::worktree_index::resolve_team(record) == ctx.team_name
        });
        let (_, record) = owned
            .next()
            .ok_or_else(|| invalid("workspace ownership differs from captured session"))?;
        if owned.next().is_some() {
            return Err(invalid("workspace has ambiguous story ownership"));
        }
        return Ok(record.source_repo);
    }
    // Team workspaces and managed clones may have no story record. Their existing
    // durable install identity is authoritative; an ambiguous alias is refused.
    // Only read (a count resolves through here): the store is never
    // created, migrated or quarantined by this lookup; no store, no identity.
    let store = crate::content_engine::sync_state_store::SyncStateStore::open_existing(
        crate::content_engine::sync_state_store::default_store_path(&ctx.codesense_home),
    )
    .ok_or_else(|| invalid("workspace has no durable repository identity"))?;
    if let Some(product) = store
        .product_registration_for(root, ctx.user_id, &ctx.org_slug, ctx.team_name.as_deref())
        .map_err(invalid)?
    {
        return Ok(product.slug);
    }
    let mut matches = store
        .enumerate_for_team(&ctx.org_slug, ctx.team_name.as_deref())
        .map_err(invalid)?
        .into_iter()
        .filter(|(_, path)| std::fs::canonicalize(path).is_ok_and(|path| path == root));
    let (slug, _) = matches
        .next()
        .ok_or_else(|| invalid("workspace has no durable repository identity"))?;
    if matches.next().is_some() {
        return Err(invalid("workspace has ambiguous repository ownership"));
    }
    Ok(slug)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    #[test]
    fn selected_product_identity_survives_repository_and_canonical_aliases() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let ctx = crate::content_engine::ResolverContext {
            context_root: None,
            codesense_home: home.path().into(),
            user_id: 42,
            org_slug: "acme/context".into(),
            team_name: Some("team".into()),
        };
        let slug = crate::domain::derive_epic_workspace_slug("product").unwrap();
        let canonical = root.path().canonicalize().unwrap();
        for path in [root.path(), canonical.as_path()] {
            crate::content_engine::register_attached_code_repo(&ctx, &slug, path).unwrap();
        }
        let alias = crate::domain::SourceRepoSlug::new("acme", "alias").unwrap();
        crate::content_engine::register_attached_code_repo(&ctx, &alias, root.path()).unwrap();
        assert_eq!(super::local_repository(&ctx, &canonical).unwrap(), slug);
        let mut second_team = ctx.clone();
        second_team.team_name = Some("second".into());
        crate::content_engine::register_attached_code_repo(&second_team, &slug, root.path())
            .unwrap();
        assert_eq!(super::local_repository(&ctx, &canonical).unwrap(), slug);
        assert_eq!(
            super::local_repository(&second_team, &canonical).unwrap(),
            slug
        );
        let mut other = ctx;
        other.user_id = 99;
        assert!(super::local_repository(&other, &canonical).is_err());
    }

    #[test]
    fn strict_selected_variants_reject_unknown_and_unsupported_without_legacy_fallback() {
        use super::super::handler::{
            decode_via_flat, HydrateWorktreeParams, ReverseSyncWorktreeToolParams,
        };
        // A decode error and a form-dispatch error both count as rejected.
        let hydrate_request = |v| decode_via_flat(v, HydrateWorktreeParams::into_request);
        let publish_request = |v| decode_via_flat(v, ReverseSyncWorktreeToolParams::into_request);
        let target = json!({"kind":"local", "codeRepoPath":"/workspace", "sessionId":"session"});
        let hydrate = json!({"kind":"selected", "requestId":uuid::Uuid::new_v4().to_string(), "target":target, "items":[{"ref":"doc:guide.md"}]});
        assert!(hydrate_request(hydrate.clone()).is_ok());
        for invalid in [
            {
                let mut v = hydrate.clone();
                v["all"] = json!(true);
                v
            },
            {
                let mut v = hydrate.clone();
                v["target"]["extra"] = json!(true);
                v
            },
            {
                let mut v = hydrate.clone();
                v["items"][0]["path"] = json!("other.md");
                v
            },
            {
                let mut v = hydrate.clone();
                v["kind"] = json!("unsupported");
                v
            },
            {
                let mut v = hydrate.clone();
                v["paths"] = json!(["guide.md"]);
                v
            },
            {
                let mut v = hydrate.clone();
                v.as_object_mut().unwrap().remove("requestId");
                v
            },
        ] {
            assert!(hydrate_request(invalid.clone()).is_err(), "{invalid}");
        }
        for op in ["create", "delete", "rename"] {
            let publish = json!({"kind":"selected", "requestId":uuid::Uuid::new_v4().to_string(), "target":target, "items":[{"op":op,"path":"guide.md","ref":"doc:guide.md","expectedSource":{"kind":"hash","hash":"x"},"expectedWorkspace":{"kind":"hash","hash":"x"}}]});
            assert!(publish_request(publish).is_err(), "{op}");
        }
        let update = json!({"kind":"selected", "requestId":uuid::Uuid::new_v4().to_string(), "target":target, "items":[{"op":"update","path":"guide.md","ref":"doc:guide.md","expectedSource":{"kind":"hash","hash":"x"},"expectedWorkspace":{"kind":"hash","hash":"x"}}]});
        assert!(publish_request(update).is_ok());
    }
}
