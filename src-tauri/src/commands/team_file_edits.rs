// SPDX-License-Identifier: MIT
//! SW-XS-27: the app's view of team-file edits not yet back in the team —
//! "unsaved changes" for team files edited inside workspaces — and its two
//! actions, each the existing explicit operation.
//!
//! - [`team_file_edits_status`] reads only: the same count agents get from
//!   `get_sync_status.unpublished` (local workspaces now; a machine workspace
//!   only when asked, one read-only call). It is not an explicit operation:
//!   the unattended-writer guard proves it reaches no write.
//! - [`publish_context_edits`] and [`discard_team_file_edits`] are the
//!   explicit publish and discard an agent's tools run, for exactly the files
//!   the person chose, requested by the app itself (`AppTarget`: no agent
//!   session borrowed; never reachable from MCP).
//! - [`worktree_unpublished_context`] is what removing a worktree would
//!   throw away (the remove confirmation), from the same count.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

/// What the app asks to count now, besides the local workspaces.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum CountTarget {
    /// One of the team's workspaces on this Mac.
    #[serde(rename_all = "camelCase")]
    Local { code_repo_path: PathBuf },
    /// One of the team's machine workspaces: one read-only call when its
    /// machine is connected (never dialled).
    #[serde(rename_all = "camelCase")]
    Remote {
        machine_id: String,
        workspace_handle: String,
    },
}

/// The active team's unsaved team-file edits, per workspace (the
/// `get_sync_status.unpublished` shape). `count` names one workspace to count
/// now (a machine workspace is otherwise reported from its last count).
#[tauri::command]
pub async fn team_file_edits_status(
    app: tauri::AppHandle,
    count: Option<CountTarget>,
) -> Result<serde_json::Value, String> {
    use crate::mcp::team_file_edits::{unpublished, Refresh};
    let ambient = crate::commands::route_to_context::derive_active_context(&app).await?;
    let refresh = match count {
        None => Refresh::None,
        Some(CountTarget::Local { code_repo_path }) => Refresh::Local(code_repo_path),
        Some(CountTarget::Remote {
            machine_id,
            workspace_handle,
        }) => Refresh::Remote {
            machine_id,
            handle: workspace_handle,
        },
    };
    unpublished(&app, ambient, refresh).await
}

#[tauri::command]
pub async fn forget_missing_workspace(
    app: tauri::AppHandle,
    target: crate::mcp::team_file_edits::ForgetMissingTarget,
) -> Result<serde_json::Value, String> {
    let ambient = crate::commands::route_to_context::derive_active_context(&app).await?;
    crate::mcp::team_file_edits::forget_missing_workspace(&app, ambient, target).await
}

/// What the person saw of one file in the list: an action refuses a file
/// that no longer matches (rewritten since, or the team moved).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SeenFile {
    /// `blake3:<hex>` of the workspace bytes as listed.
    #[serde(default)]
    pub workspace_hash: Option<String>,
    /// `blake3:<hex>` of the team file a publish expects.
    #[serde(default)]
    pub team_hash: Option<String>,
    /// `size:mtime_ns` of a new file listed without being read.
    #[serde(default)]
    pub workspace_stat: Option<String>,
}

fn parse_stat(stat: &Option<String>) -> Result<Option<(u64, i64)>, String> {
    let Some(stat) = stat.as_deref() else {
        return Ok(None);
    };
    let (size, mtime) = stat
        .split_once(':')
        .ok_or_else(|| format!("{stat:?} is not size:mtime"))?;
    Ok(Some((
        size.parse()
            .map_err(|_| format!("{stat:?} is not size:mtime"))?,
        mtime
            .parse()
            .map_err(|_| format!("{stat:?} is not size:mtime"))?,
    )))
}

/// What the person saw, as the engine's expectations.
fn expectations(
    seen: Option<BTreeMap<String, SeenFile>>,
) -> Result<BTreeMap<String, crate::agents::prepare::publish::Expect>, String> {
    let mut expect = BTreeMap::new();
    for (path, file) in seen.unwrap_or_default() {
        expect.insert(
            path,
            crate::agents::prepare::publish::Expect {
                workspace: parse(&file.workspace_hash)?,
                source: parse(&file.team_hash)?,
                target: None,
                stat: parse_stat(&file.workspace_stat)?,
            },
        );
    }
    Ok(expect)
}

fn parse(hash: &Option<String>) -> Result<Option<[u8; 32]>, String> {
    hash.as_deref()
        .map(crate::mcp::context_operations::explicit::parse_hash)
        .transpose()
}

/// Publish exactly `paths` of the workspace `target` to the team — the
/// explicit publish (`reverse_sync_worktree`'s engine), requested by the app
/// on the person's click (no agent session borrowed; the record says
/// `requestedBy: "app"`). A file whose team copy changed since it was copied,
/// or that changed since it was listed (`seen`), is a conflict and is never
/// written. `include_deletes` publishes a removal (the list sends it only for
/// a file removed here, alone).
#[tauri::command]
pub(crate) async fn publish_context_edits(
    app: tauri::AppHandle,
    target: crate::mcp::context_operations::AppTarget,
    paths: Vec<String>,
    include_deletes: bool,
    seen: Option<BTreeMap<String, SeenFile>>,
    operation_id: Option<String>,
) -> Result<serde_json::Value, String> {
    if paths.is_empty() {
        return Err("choose the files to publish".into());
    }
    let expect = expectations(seen)?;
    let op = crate::mcp::context_operations::explicit::Op::Publish {
        paths,
        sets: Vec::new(),
        to: None,
        include_deletes,
        expect,
    };
    crate::mcp::context_operations::explicit::run_app(app, target, op, operation_id)
        .await
        .map_err(|e| e.message.to_string())
}

/// Discard exactly `paths` of the workspace `target`, requested by the app on
/// the person's click: a copied file gets the team's version back, a new file
/// is deleted. A file that changed since it was listed (`seen`) keeps its
/// edit. The same engine as `discard_context_edits`.
#[tauri::command]
pub(crate) async fn discard_team_file_edits(
    app: tauri::AppHandle,
    target: crate::mcp::context_operations::AppTarget,
    paths: Vec<String>,
    seen: Option<BTreeMap<String, SeenFile>>,
    operation_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let expect = expectations(seen)?;
    let op = crate::mcp::context_operations::explicit::Op::Discard { paths, expect };
    crate::mcp::context_operations::explicit::run_app(app, target, op, operation_id)
        .await
        .map_err(|e| e.message.to_string())
}

/// What removing the worktree at `code_repo_path` would throw away: its team
/// files edited and not published. `Err` when it can't be counted — the
/// caller says it couldn't check, never zero.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeUnpublished {
    pub count: usize,
    pub files: Vec<String>,
}

#[tauri::command]
pub async fn worktree_unpublished_context(
    app: tauri::AppHandle,
    code_repo_path: String,
) -> Result<WorktreeUnpublished, String> {
    let ambient = crate::commands::route_to_context::derive_active_context(&app).await?;
    let ctx = crate::commands::content_engine::derive_resolver_context(
        ambient.codesense_home,
        ambient.user_id,
        ambient.org_slug,
        ambient.team_name,
    )
    .map_err(|e| e.to_string())?;
    let root = PathBuf::from(code_repo_path);
    crate::commands::await_blocking("worktree_unpublished_context", move || {
        let root = root
            .canonicalize()
            .map_err(|e| format!("{} is unavailable: {e}", root.display()))?;
        worktree_unpublished(&ctx, &root)
    })
    .await
}

fn worktree_unpublished(
    ctx: &crate::content_engine::ResolverContext,
    root: &std::path::Path,
) -> Result<WorktreeUnpublished, String> {
    use crate::agents::prepare::edits::EditsState;
    let edits = crate::mcp::team_file_edits::count_local(ctx, root);
    if edits.state != EditsState::Checked {
        return Err(edits
            .reason
            .unwrap_or_else(|| "its team-file edits couldn't be counted".into()));
    }
    Ok(WorktreeUnpublished {
        count: edits.counts.total(),
        files: edits.files.into_iter().map(|f| f.path).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::prepare::step3::tests::{write, Fx};
    use crate::agents::resolve::tests::{ORG, TEAM};

    /// The remove confirmation counts the same edits the list shows — and
    /// never CodeSense's own local files.
    #[test]
    fn the_remove_confirmation_counts_the_same_edits() {
        let fx = Fx::new();
        fx.hydrate(&fx.want(&["CLAUDE.md"], &[".claude/skills"]));
        write(&fx.root, "CLAUDE.md", b"edited here");
        write(&fx.root, ".claude/skills/new/SKILL.md", b"# New\n");
        write(&fx.root, ".claude/settings.local.json", b"{}");
        let ctx = crate::content_engine::ResolverContext {
            context_root: None,
            codesense_home: fx.home.path().to_path_buf(),
            user_id: 1,
            org_slug: ORG.into(),
            team_name: Some(TEAM.into()),
        };
        let slug = crate::domain::SourceRepoSlug::new("acme", "api").unwrap();
        crate::content_engine::register_attached_code_repo(&ctx, &slug, &fx.root).unwrap();
        let found = worktree_unpublished(&ctx, &fx.root).unwrap();
        assert_eq!(found.count, 2);
        assert_eq!(
            found.files,
            vec![
                ".claude/skills/new/SKILL.md".to_string(),
                "CLAUDE.md".into()
            ]
        );
        // Not an attached workspace: can't be counted, never "0".
        let stranger = tempfile::tempdir().unwrap();
        assert!(worktree_unpublished(&ctx, &stranger.path().canonicalize().unwrap()).is_err());
    }

    fn app_fixture() -> (Fx, crate::content_engine::ResolverContext) {
        let fx = Fx::new();
        fx.hydrate(&fx.want(&["CLAUDE.md"], &[".claude/skills"]));
        let ctx = crate::content_engine::ResolverContext {
            context_root: None,
            codesense_home: fx.home.path().to_path_buf(),
            user_id: 1,
            org_slug: ORG.into(),
            team_name: Some(TEAM.into()),
        };
        let slug = crate::domain::SourceRepoSlug::new("acme", "api").unwrap();
        crate::content_engine::register_attached_code_repo(&ctx, &slug, &fx.root).unwrap();
        (fx, ctx)
    }

    fn blake(bytes: &[u8]) -> [u8; 32] {
        *blake3::hash(bytes).as_bytes()
    }

    /// The app's own request (no agent session): admitted only for one of the
    /// active team's workspaces on this Mac, recorded as the app's, and it
    /// publishes exactly the chosen file.
    #[test]
    fn the_app_publishes_a_teams_workspace_without_a_session() {
        use crate::mcp::context_operations::explicit::{execute_app_local, Op};
        let (fx, ctx) = app_fixture();
        write(&fx.root, "CLAUDE.md", b"edited here");
        let stranger = tempfile::tempdir().unwrap();
        let publish = |paths: Vec<String>| Op::Publish {
            paths,
            sets: Vec::new(),
            to: None,
            include_deletes: false,
            expect: Default::default(),
        };
        assert!(
            execute_app_local(
                ctx.clone(),
                stranger.path(),
                publish(vec!["a.md".into()]),
                "op-x"
            )
            .is_err(),
            "not one of the team's workspaces"
        );
        let other_team = crate::content_engine::ResolverContext {
            team_name: Some("platform".into()),
            ..ctx.clone()
        };
        assert!(execute_app_local(
            other_team,
            &fx.root,
            publish(vec!["CLAUDE.md".into()]),
            "op-y"
        )
        .is_err());
        let (value, state) =
            execute_app_local(ctx, &fx.root, publish(vec!["CLAUDE.md".into()]), "op-1").unwrap();
        assert_eq!(state, "ready", "{value}");
        assert_eq!(value["target"]["requestedBy"], "app");
        assert_eq!(
            std::fs::read(fx.team.join("CLAUDE.md")).unwrap(),
            b"edited here"
        );
    }

    /// What the person saw is pinned: a file rewritten since it was listed is
    /// neither published nor discarded; the one that still matches is.
    #[test]
    fn a_file_rewritten_since_it_was_listed_is_left_alone() {
        use crate::agents::prepare::publish::Expect;
        use crate::mcp::context_operations::explicit::{execute_app_local, Op};
        let (fx, ctx) = app_fixture();
        let team_claude = std::fs::read(fx.team.join("CLAUDE.md")).unwrap();
        write(&fx.root, "CLAUDE.md", b"what the person saw");
        write(&fx.root, ".claude/skills/new/SKILL.md", b"seen new");
        // Rewritten after the person looked.
        write(&fx.root, "CLAUDE.md", b"rewritten later");
        write(&fx.root, ".claude/skills/new/SKILL.md", b"rewritten new");
        let seen = |path: &str, bytes: &[u8]| {
            (
                path.to_string(),
                Expect {
                    workspace: Some(blake(bytes)),
                    ..Default::default()
                },
            )
        };
        let (value, _) = execute_app_local(
            ctx.clone(),
            &fx.root,
            Op::Publish {
                paths: vec!["CLAUDE.md".into()],
                sets: Vec::new(),
                to: None,
                include_deletes: false,
                expect: [seen("CLAUDE.md", b"what the person saw")].into(),
            },
            "op-p",
        )
        .unwrap();
        assert_eq!(
            std::fs::read(fx.team.join("CLAUDE.md")).unwrap(),
            team_claude,
            "{value}"
        );
        let discard = |expect: Vec<(&str, &[u8])>| Op::Discard {
            paths: vec!["CLAUDE.md".into(), ".claude/skills/new/SKILL.md".into()],
            expect: expect
                .into_iter()
                .map(|(p, b)| {
                    (
                        p.to_string(),
                        Expect {
                            workspace: Some(blake(b)),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
        };
        let (value, state) = execute_app_local(
            ctx.clone(),
            &fx.root,
            discard(vec![
                ("CLAUDE.md", b"what the person saw"),
                (".claude/skills/new/SKILL.md", b"seen new"),
            ]),
            "op-d1",
        )
        .unwrap();
        assert_eq!(state, "failed", "{value}");
        assert_eq!(value["failedCount"], 2, "{value}");
        assert_eq!(fx.read("CLAUDE.md").unwrap(), b"rewritten later");
        assert_eq!(
            fx.read(".claude/skills/new/SKILL.md").unwrap(),
            b"rewritten new"
        );
        // Pinned to what is there now: both are settled.
        let (value, state) = execute_app_local(
            ctx,
            &fx.root,
            discard(vec![
                ("CLAUDE.md", b"rewritten later"),
                (".claude/skills/new/SKILL.md", b"rewritten new"),
            ]),
            "op-d2",
        )
        .unwrap();
        assert_eq!(state, "ready", "{value}");
        assert_eq!(fx.read("CLAUDE.md").unwrap(), team_claude);
        assert!(fx.read(".claude/skills/new/SKILL.md").is_none());
    }

    /// A new file too large to publish can still be thrown away.
    #[test]
    fn a_new_file_too_large_to_publish_can_be_discarded() {
        use crate::mcp::context_operations::explicit::{execute_app_local, Op};
        let (fx, ctx) = app_fixture();
        let big = vec![b'x'; 4 * 1024 * 1024 + 1];
        write(&fx.root, ".claude/skills/big/blob.bin", &big);
        let edits = crate::mcp::team_file_edits::count_local(&ctx, &fx.root);
        let file = edits
            .files
            .iter()
            .find(|f| f.path.ends_with("blob.bin"))
            .unwrap();
        assert!(file.discardable, "{file:?}");
        let (value, state) = execute_app_local(
            ctx,
            &fx.root,
            Op::Discard {
                paths: vec![".claude/skills/big/blob.bin".into()],
                expect: Default::default(),
            },
            "op-big",
        )
        .unwrap();
        assert_eq!(state, "ready", "{value}");
        assert!(fx.read(".claude/skills/big/blob.bin").is_none());
    }

    /// Review: a new file the count listed unread is pinned by its stat; a
    /// rewrite after that keeps it (Discard can't be undone).
    #[test]
    fn an_unread_new_file_rewritten_since_is_not_deleted() {
        use crate::agents::prepare::publish::Expect;
        use crate::mcp::context_operations::explicit::{execute_app_local, Op};
        let (fx, ctx) = app_fixture();
        write(&fx.root, ".claude/skills/new/SKILL.md", b"# New\n");
        let edits = crate::mcp::team_file_edits::count_local(&ctx, &fx.root);
        let file = edits
            .files
            .iter()
            .find(|f| f.path.ends_with("new/SKILL.md"))
            .unwrap();
        assert!(file.workspace_hash.is_none(), "listed unread");
        let stat = file.workspace_stat.clone().expect("with its stat");
        let (size, mtime) = stat.split_once(':').unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(
            &fx.root,
            ".claude/skills/new/SKILL.md",
            b"# Rewritten by the agent\n",
        );
        let (value, state) = execute_app_local(
            ctx,
            &fx.root,
            Op::Discard {
                paths: vec![".claude/skills/new/SKILL.md".into()],
                expect: [(
                    ".claude/skills/new/SKILL.md".to_string(),
                    Expect {
                        stat: Some((size.parse().unwrap(), mtime.parse().unwrap())),
                        ..Default::default()
                    },
                )]
                .into(),
            },
            "op-stat",
        )
        .unwrap();
        assert_eq!(state, "failed", "{value}");
        assert_eq!(value["status"], "failed");
        assert_eq!(
            fx.read(".claude/skills/new/SKILL.md").unwrap(),
            b"# Rewritten by the agent\n"
        );
    }
}
