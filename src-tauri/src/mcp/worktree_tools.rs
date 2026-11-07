// SPDX-License-Identifier: MIT
//! `list_worktrees`: this Mac's managed worktrees, as the Worktrees panel lists them. A
//! dashboard's `worktrees()` returns the same listing from the same command.

use std::sync::Arc;

use tauri::{AppHandle, Manager};

use crate::commands::worktree::{list_managed_worktrees, ManagedWorktreeListing};
use crate::content_engine::sync::WaveCoordinator;
use crate::install::frontmatter_cache::SharedFrontmatterCache;

/// Every managed worktree on this Mac (`list_worktrees`): story, epic and orphan rows.
pub(crate) async fn list_worktrees(app: &AppHandle) -> Result<ManagedWorktreeListing, String> {
    let coord = app
        .try_state::<Arc<WaveCoordinator>>()
        .ok_or("the sync coordinator is not running")?;
    let cache = app
        .try_state::<SharedFrontmatterCache>()
        .ok_or("the frontmatter cache is not running")?;
    list_managed_worktrees(app.clone(), coord, cache).await
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ListWorktreesParams {
    pub kind: Option<String>,
    pub org: Option<String>,
    pub team: Option<String>,
}
pub(crate) async fn filtered(
    app: &AppHandle,
    params: ListWorktreesParams,
) -> Result<ManagedWorktreeListing, String> {
    Ok(filter_listing(list_worktrees(app).await?, &params))
}

fn filter_listing(
    mut listing: ManagedWorktreeListing,
    params: &ListWorktreesParams,
) -> ManagedWorktreeListing {
    listing.rows.retain(|row| {
        params.kind.as_ref().is_none_or(|kind| {
            serde_json::to_value(row.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .as_ref()
                == Some(kind)
        }) && params
            .org
            .as_ref()
            .is_none_or(|org| row.org_slug.as_ref() == Some(org))
            && params
                .team
                .as_ref()
                .is_none_or(|team| row.team.as_ref() == Some(team))
    });
    listing.rows.sort_by_key(|r| {
        std::cmp::Reverse(r.context.as_ref().map(|r| r.last_used_at).unwrap_or(0))
    });
    listing
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::worktree::{ContextWorktreeFields, WorktreeKind, WorktreeRow};
    fn row(id: &str, org: &str, team: &str, used: u64) -> WorktreeRow {
        WorktreeRow {
            kind: WorktreeKind::Context,
            context: Some(ContextWorktreeFields {
                id: id.into(),
                name: id.into(),
                launch_cwd: format!("/work/{id}/{team}/content"),
                base_sha: "base".into(),
                created_at: 1,
                last_used_at: used,
                agents: vec![],
            }),
            workspace_key: id.into(),
            path: format!("/work/{id}"),
            branch: Some(format!("{team}/{id}")),
            dirty: Some(false),
            repo_path: None,
            live_cli_session: false,
            story_id: None,
            source_repo: None,
            sync_status: None,
            failure_reason: None,
            story_source_path: None,
            org_slug: Some(org.into()),
            team: Some(team.into()),
            orphaned: None,
            last_reverse_sync_at: None,
            last_reverse_sync_by: None,
            attribution: None,
            epic_id: None,
            owner: None,
            repo: None,
        }
    }
    #[test]
    fn context_results_filter_team_org_kind_and_order_by_last_use() {
        let mut source = row("story", "acme", "default", 0);
        source.kind = WorktreeKind::Story;
        source.context = None;
        let listing = ManagedWorktreeListing {
            rows: vec![
                row("old", "acme", "default", 2),
                row("other-team", "acme", "other", 5),
                row("other-org", "other", "default", 6),
                row("recent", "acme", "default", 4),
                source,
            ],
            reaped: 0,
        };
        let all = filter_listing(listing.clone(), &ListWorktreesParams::default());
        assert_eq!(all.rows.len(), 5);
        let selected = filter_listing(
            listing,
            &ListWorktreesParams {
                kind: Some("context".into()),
                org: Some("acme".into()),
                team: Some("default".into()),
            },
        );
        let wire = serde_json::to_value(selected).unwrap();
        assert_eq!(
            wire["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["recent", "old"]
        );
        assert_eq!(wire["rows"][0]["branch"], "default/recent");
        assert_eq!(wire["rows"][0]["launchCwd"], "/work/recent/default/content");
        assert_eq!(wire["rows"][0]["baseSha"], "base");
        assert_eq!(wire["rows"][0]["agents"], serde_json::json!([]));
    }
}
