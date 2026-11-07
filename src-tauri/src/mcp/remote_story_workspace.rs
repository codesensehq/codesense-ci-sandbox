// SPDX-License-Identifier: MIT
//! A story workspace the viewer creates on a machine. The person picks only
//! the machine: both remote paths are derived here from the account home the
//! machine's own terminal server reports, so no path is ever typed or shown.
use super::remote_workspace::{self, PrepareProgress, PrepareRemoteWorkspaceParams};
use crate::commands::{
    machines,
    pty_layout::{host_home, parse_team_key},
    route_to_context::{self, ActiveContext, ContextResolution},
};
use crate::domain::SourceRepoSlug;
use crate::terminal::session_server::AccountHomeError;
use codesense_pty_core::remote_workspace::PreparedWorkspace;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::Emitter;

/// How far a remote story workspace's preparation has got, keyed by its request.
pub const REMOTE_WORKSPACE_PROGRESS_EVENT: &str = "remote-workspace-progress";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrepareRemoteStoryWorkspaceRequest {
    /// Minted once per (machine, team, workspace) and kept on the workspace
    /// record, so a repeat reconciles the same preparation.
    pub request_id: String,
    pub machine_id: String,
    pub team_key: String,
    pub workspace_id: String,
    pub story_id: String,
    /// `owner/repo` (or `host/owner/repo`) as the team configures it.
    pub source_repo: String,
    pub base_ref: String,
    pub branch: String,
}

/// The machine's answer, with the preparation identity it was made under: the
/// request a prior preparation of this workspace was made with wins over the
/// one asked for, so the workspace record can keep (and replay) it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedStoryWorkspace {
    pub result: PreparedWorkspace,
    /// `pending` once the worktree exists — the workspace is not
    /// ready for an agent until its first launch prepared the agent's context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_context: Option<&'static str>,
    pub request_id: String,
    pub base_ref: String,
    pub branch: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressPayload {
    request_id: String,
    #[serde(flatten)]
    progress: PrepareProgress,
}

/// One path component of a remote location, made from a name the user did
/// not type as a path: anything outside `[A-Za-z0-9._-]` becomes `-`.
fn component(raw: &str, what: &str) -> Result<String, String> {
    let mapped: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mapped = mapped.trim_start_matches('.').to_string();
    if mapped.is_empty() || mapped.len() > 128 {
        return Err(format!("the {what} cannot name a folder on the machine"));
    }
    Ok(mapped)
}

/// The repository and worktree a story workspace lives in on a machine whose
/// account home is `home`: `<home>/codesense/repos/<owner>/<repo>` and
/// `<home>/codesense/worktrees/<org>-<team>-<digest>/<story-id>`. The digest
/// of the whole team key keeps two teams apart whose readable names collide
/// (two orgs' `default`, or `My Team` and `My-Team`).
pub(crate) fn story_workspace_paths(
    home: &str,
    source: &SourceRepoSlug,
    team_key: &str,
    story_id: &str,
) -> Result<(String, String), String> {
    let (org, team) = parse_team_key(team_key);
    let team = team.ok_or("a story workspace needs a team")?;
    let digest = format!("{:x}", Sha256::digest(team_key.as_bytes()));
    let home = home.trim_end_matches('/');
    if !home.starts_with('/') || home.contains('\0') {
        return Err("this machine's home folder is not an absolute path".into());
    }
    let root = format!("{home}/codesense");
    let repo = format!(
        "{root}/repos/{}/{}",
        component(&source.owner.to_ascii_lowercase(), "repository owner")?,
        component(&source.repo.to_ascii_lowercase(), "repository")?,
    );
    let worktree = format!(
        "{root}/worktrees/{}-{}-{}/{}",
        component(org, "organization")?,
        component(team, "team")?,
        &digest[..6],
        component(story_id, "story id")?,
    );
    Ok((repo, worktree))
}

/// Whether `worktree` is a story workspace's worktree in `team_key`,
/// as [`story_workspace_paths`] places it on any machine home: directly under
/// `…/codesense/worktrees/<org>-<team>-<digest>/`. A preparation receipt keeps
/// no workspace kind, so this is how a routine tells a story's work apart.
pub(crate) fn is_story_worktree(team_key: &str, worktree: &str) -> bool {
    let (org, team) = parse_team_key(team_key);
    let (Some(team), Ok(org)) = (team, component(org, "organization")) else {
        return false;
    };
    let Ok(team) = component(team, "team") else {
        return false;
    };
    let digest = format!("{:x}", Sha256::digest(team_key.as_bytes()));
    let group = format!("/codesense/worktrees/{org}-{team}-{}", &digest[..6]);
    let Some((parent, leaf)) = worktree.rsplit_once('/') else {
        return false;
    };
    worktree.starts_with('/') && !leaf.is_empty() && parent.ends_with(&group)
}

fn team_source(ctx: &ActiveContext, requested: &str) -> Result<SourceRepoSlug, String> {
    let root = crate::context_repo::path_to_org_root(
        &ctx.codesense_home,
        &ctx.principal(),
        &ctx.org_slug,
    )?;
    let org = crate::domain::load_org(ctx.org_slug.clone(), &root, None)
        .org
        .ok_or("team configuration unavailable")?;
    let team = org
        .teams
        .iter()
        .find(|t| Some(t.name.as_str()) == ctx.team_name.as_deref())
        .ok_or("team configuration unavailable")?;
    SourceRepoSlug::resolve_against(requested, &team.source_repos)
        .filter(|slug| team.source_repos.contains(slug))
        .ok_or_else(|| "source repository is not configured for the selected team".into())
}

/// The preparation identity to use for this workspace's worktree: a prior
/// intent for the same machine, team, workspace and worktree is replayed as it
/// was made (its request id, base and branch), so a record that was lost, or a
/// story whose branch or base changed since, reconciles the worktree that
/// exists instead of being refused as a different intent.
pub(super) fn replayed_identity(
    prior: &[super::remote_workspace::IntentReceipt],
    requested: &PrepareRemoteWorkspaceParams,
) -> Option<(String, String, String)> {
    prior
        .iter()
        .find(|receipt| {
            let p = &receipt.params;
            p.machine_id == requested.machine_id
                && p.team_key == requested.team_key
                && p.workspace_id == requested.workspace_id
                && p.worktree_path == requested.worktree_path
                && p.repo_path == requested.repo_path
                && p.source_repo == requested.source_repo
        })
        .map(|receipt| {
            (
                receipt.params.request_id.clone(),
                receipt.params.base_ref.clone(),
                receipt.params.branch.clone(),
            )
        })
}

fn preparation_attempt(
    mut params: PrepareRemoteWorkspaceParams,
    prior: &[super::remote_workspace::IntentReceipt],
    emit: impl Fn(ProgressPayload) + Send + Sync + 'static,
) -> (
    PrepareRemoteWorkspaceParams,
    super::remote_workspace::ProgressSink,
) {
    let request_id = params.request_id.clone();
    if let Some((id, base_ref, branch)) = replayed_identity(prior, &params) {
        params.request_id = id;
        params.base_ref = base_ref;
        params.branch = branch;
    } else {
        // A retained card can still name a forgotten preparation.
        params.request_id = uuid::Uuid::new_v4().to_string();
    }
    let progress = std::sync::Arc::new(move |progress| {
        emit(ProgressPayload {
            request_id: request_id.clone(),
            progress,
        })
    });
    (params, progress)
}

/// Create (or reconcile) a story's workspace on a machine. Progress is
/// emitted as [`REMOTE_WORKSPACE_PROGRESS_EVENT`]; the result carries the
/// managed handle and the worktree the workspace's terminals open in.
#[tauri::command]
pub async fn prepare_remote_story_workspace(
    app: tauri::AppHandle,
    request: PrepareRemoteStoryWorkspaceRequest,
) -> Result<PreparedStoryWorkspace, String> {
    let (org, team) = parse_team_key(&request.team_key);
    let team = team.ok_or("a story workspace needs a team")?;
    let ctx = match route_to_context::derive_context_for(&app, org, team).await? {
        ContextResolution::Ok(ctx) => ctx,
        _ => return Err("selected team is unavailable".into()),
    };
    uuid::Uuid::parse_str(&request.request_id).map_err(|_| "requestId must be a UUID")?;
    let source = team_source(&ctx, &request.source_repo)?;
    machines::authorize_terminal_machine(&app, &request.machine_id, &request.team_key).await?;
    let home = host_home(&app, &request.machine_id)
        .await
        .map_err(|error| match error {
            AccountHomeError::OutdatedServer => {
                "This machine's terminal server is too old to hold workspaces; update its terminal server".to_string()
            }
            AccountHomeError::Failed(error) => error,
        })?;
    let (repo_path, worktree_path) =
        story_workspace_paths(&home, &source, &request.team_key, &request.story_id)?;
    let params = PrepareRemoteWorkspaceParams {
        request_id: request.request_id.clone(),
        machine_id: request.machine_id,
        team_key: request.team_key,
        workspace_id: request.workspace_id,
        source_repo: source,
        base_ref: request.base_ref,
        branch: request.branch,
        repo_path,
        worktree_path,
    };
    let intents = crate::context_repo::path_to_user_home(&ctx.codesense_home, &ctx.principal())
        .join("remote-workspace-intents-v1");
    let owner = ctx.user_id;
    let machine = params.machine_id.clone();
    let selection = (params.team_key.clone(), params.workspace_id.clone());
    let prior = crate::commands::await_blocking("read_story_workspace_intents", move || {
        if !intents.exists() {
            return Ok(Vec::new());
        }
        super::remote_workspace_target::read_receipts(
            intents,
            owner,
            Some(machine),
            None,
            Some(selection),
        )
    })
    .await?;
    let emitter = app.clone();
    let (params, progress) = preparation_attempt(params, &prior, move |payload| {
        let _ = emitter.emit(REMOTE_WORKSPACE_PROGRESS_EVENT, payload);
    });
    let identity = (
        params.request_id.clone(),
        params.base_ref.clone(),
        params.branch.clone(),
    );
    let machine_id = params.machine_id.clone();
    let (state_app, state_ctx) = (app.clone(), ctx.clone());
    let prepared = remote_workspace::prepare_reporting(app, params, ctx, None, progress).await;
    // No content pass here. The worktree carries only the code until an
    // agent launches in it; that launch delivers what the runtime reads (Step 2).
    let delivered = match (prepared.status.as_str(), prepared.handle.as_deref()) {
        ("alreadyPrepared", Some(handle)) => {
            agent_context_delivered(state_app, state_ctx, &machine_id, handle).await
        }
        _ => false,
    };
    let agent_context = agent_context_after(&prepared.status, delivered);
    Ok(PreparedStoryWorkspace {
        result: prepared,
        agent_context,
        request_id: identity.0,
        base_ref: identity.1,
        branch: identity.2,
    })
}

/// What a prepare leaves for the workspace's agents: a created (or replayed)
/// worktree is `pending` until an agent launch delivered its runtime context,
/// then `ready`. Nothing is copied here.
fn agent_context_after(status: &str, delivered: bool) -> Option<&'static str> {
    match status {
        "prepared" | "alreadyPrepared" if delivered => Some("ready"),
        "prepared" | "alreadyPrepared" => Some("pending"),
        _ => None,
    }
}

/// Whether an agent launch already delivered its runtime context to this
/// workspace (read-only; an unreadable record reads as not yet).
async fn agent_context_delivered(
    app: tauri::AppHandle,
    ctx: route_to_context::ActiveContext,
    machine_id: &str,
    handle: &str,
) -> bool {
    let Ok(resolved) = super::remote_destination::resolve(app, ctx, machine_id, handle, None).await
    else {
        return false;
    };
    let crate::content_engine::destination::DestinationIdentity::Remote(identity) =
        resolved.io.identity()
    else {
        return false;
    };
    // A workspace is prepared once a Step 2 there ended `ready`
    // , not merely once one ran.
    crate::agents::prepare::ManagedRecord::last_ready(
        &resolved.context.codesense_home,
        &crate::domain::Principal::from(resolved.context.user_id),
        &crate::content_engine::destination::DestinationIdentity::Remote(identity.clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A prepared or replayed workspace waits for its first agent's
    /// preparation (no background setup pass is spawned — see the source guard
    /// `xs1_launch_and_prepare_never_run_the_legacy_pass`).
    #[test]
    fn xs1_a_prepared_workspace_waits_for_its_first_agent() {
        assert_eq!(agent_context_after("prepared", false), Some("pending"));
        assert_eq!(
            agent_context_after("alreadyPrepared", false),
            Some("pending")
        );
        assert_eq!(agent_context_after("alreadyPrepared", true), Some("ready"));
        assert_eq!(agent_context_after("refused", true), None);
    }

    fn slug(owner: &str, repo: &str) -> SourceRepoSlug {
        SourceRepoSlug::new(owner, repo).unwrap()
    }

    /// A story's worktree is known by where it lives on the
    /// machine (its receipt records no kind), so a routine never starts in it.
    #[test]
    fn agr12_a_story_worktree_is_known_by_its_path() {
        let team = "org:My Team";
        let (_, worktree) =
            story_workspace_paths("/home/dev", &slug("o", "r"), team, "SW-1").unwrap();
        assert!(is_story_worktree(team, &worktree));
        assert!(
            !is_story_worktree("org:My-Team", &worktree),
            "another team's"
        );
        assert!(!is_story_worktree(team, "/home/dev/work/api"));
        assert!(!is_story_worktree(team, &format!("{worktree}/nested")));
        assert!(!is_story_worktree(team, "relative/codesense/worktrees/x/y"));
    }

    #[test]
    fn paths_are_derived_from_the_machine_home() {
        let (repo, worktree) = story_workspace_paths(
            "/home/dev/",
            &slug("AlderCo", "CodeSense"),
            "alderco:default",
            "SW-RM-UX-2",
        )
        .unwrap();
        assert_eq!(repo, "/home/dev/codesense/repos/alderco/codesense");
        let digest = format!("{:x}", Sha256::digest(b"alderco:default"));
        assert_eq!(
            worktree,
            format!(
                "/home/dev/codesense/worktrees/alderco-default-{}/SW-RM-UX-2",
                &digest[..6]
            )
        );
    }

    #[test]
    fn names_never_escape_their_folder() {
        let (_, worktree) =
            story_workspace_paths("/home/dev", &slug("o", "r"), "org:My Team/..", "../x").unwrap();
        assert!(worktree.starts_with("/home/dev/codesense/worktrees/org-My-Team-..-"));
        assert!(worktree.ends_with("/-x"));
        for (team, story) in [
            ("org:..", "s"),
            ("org:t", ""),
            ("org:t", "..."),
            ("org", "s"),
        ] {
            assert!(story_workspace_paths("/home/dev", &slug("o", "r"), team, story).is_err());
        }
        assert!(story_workspace_paths("relative", &slug("o", "r"), "org:t", "s").is_err());
    }

    #[test]
    fn teams_whose_names_collide_get_their_own_worktrees() {
        let path = |team: &str| {
            story_workspace_paths("/h", &slug("o", "r"), team, "SW-1")
                .unwrap()
                .1
        };
        assert_ne!(path("acme:default"), path("other:default"));
        assert_ne!(path("acme:My Team"), path("acme:My-Team"));
        assert_eq!(path("acme:default"), path("acme:default"));
    }

    fn receipt(
        request_id: &str,
        branch: &str,
        worktree: &str,
    ) -> super::super::remote_workspace::IntentReceipt {
        let params: PrepareRemoteWorkspaceParams = serde_json::from_value(serde_json::json!({
            "requestId": request_id, "machineId": "box", "teamKey": "acme:default",
            "workspaceId": "sw-1",
            "sourceRepo": {"owner":"o","repo":"r","provider":"github","host":"github.com"},
            "baseRef": "main", "branch": branch, "repoPath": "/h/codesense/repos/o/r",
            "worktreePath": worktree}))
        .unwrap();
        super::super::remote_workspace::IntentReceipt {
            request: codesense_pty_core::remote_workspace::PrepareWorkspace {
                request_id: request_id.into(),
                owner_id: 1,
                origin_key: "k".into(),
                repository_key: "github.com/o/r".into(),
                clone_url: "https://github.com/o/r.git".into(),
                repo_path: params.repo_path.clone(),
                worktree_path: worktree.into(),
                base_ref: "main".into(),
                branch: branch.into(),
                bundle: codesense_pty_core::remote_workspace::GitBundle {
                    digest: String::new(),
                    size: 0,
                    commit: String::new(),
                },
            },
            params,
            server_id: "s".into(),
            kind: None,
        }
    }

    #[test]
    fn a_prior_intent_for_the_same_worktree_is_replayed_as_made() {
        let wanted: PrepareRemoteWorkspaceParams = serde_json::from_value(serde_json::json!({
            "requestId": "00000000-0000-4000-8000-00000000000b", "machineId": "box",
            "teamKey": "acme:default", "workspaceId": "sw-1",
            "sourceRepo": {"owner":"o","repo":"r","provider":"github","host":"github.com"},
            "baseRef": "release", "branch": "story/renamed", "repoPath": "/h/codesense/repos/o/r",
            "worktreePath": "/h/codesense/worktrees/acme-default-abc123/SW-1"}))
        .unwrap();
        let prior = vec![
            receipt("other", "story/x", "/h/elsewhere/SW-1"),
            receipt(
                "first",
                "story/sw-1",
                "/h/codesense/worktrees/acme-default-abc123/SW-1",
            ),
        ];
        assert_eq!(
            replayed_identity(&prior, &wanted),
            Some(("first".into(), "main".into(), "story/sw-1".into()))
        );
        assert_eq!(replayed_identity(&prior[..1], &wanted), None);
    }

    #[test]
    fn preparation_identity_changes_without_losing_the_callers_progress() {
        let requested = receipt("caller", "work", "/work").params;
        for prior in [Vec::new(), vec![receipt("prepared", "work", "/work")]] {
            let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = events.clone();
            let (attempt, progress) =
                preparation_attempt(requested.clone(), &prior, move |payload| {
                    seen.lock()
                        .unwrap()
                        .push(serde_json::to_value(payload).unwrap());
                });
            if prior.is_empty() {
                assert!(uuid::Uuid::parse_str(&attempt.request_id).is_ok());
            } else {
                assert_eq!(attempt.request_id, "prepared");
            }
            assert_ne!(attempt.request_id, requested.request_id);
            progress(PrepareProgress::Copying { sent: 3, total: 9 });
            assert_eq!(
                events.lock().unwrap()[0],
                serde_json::json!({"requestId":"caller", "phase":"copying", "sent":3, "total":9})
            );
        }
    }

    #[test]
    fn progress_is_tagged_by_request() {
        let payload = ProgressPayload {
            request_id: "r".into(),
            progress: PrepareProgress::Copying { sent: 3, total: 9 },
        };
        assert_eq!(
            serde_json::to_value(payload).unwrap(),
            serde_json::json!({"requestId":"r","phase":"copying","sent":3,"total":9})
        );
    }

    /// The `remote-workspace-progress` contract the frontend's setup
    /// line consumes — one payload of every Step 1 phase, in the order a
    /// machine create reports them, plus a failure. Checked in at
    /// `src/tauri/fixtures/remote-workspace-progress.golden.json` and read by
    /// the TypeScript setup-state test. Regenerate with `UPDATE_GOLDEN=1`.
    #[test]
    fn xs10_remote_workspace_progress_golden_fixture() {
        use super::remote_workspace::Step1Phase;
        let sequence: Vec<serde_json::Value> = [
            PrepareProgress::Fetching {
                objects: Some(120),
                total: Some(480),
            },
            PrepareProgress::Bundling { objects: Some(480) },
            PrepareProgress::Copying {
                sent: 0,
                total: 117_440_512,
            },
            PrepareProgress::Copying {
                sent: 42_991_616,
                total: 117_440_512,
            },
            PrepareProgress::Verifying,
            PrepareProgress::CreatingWorktree,
        ]
        .into_iter()
        .map(|progress| {
            serde_json::to_value(ProgressPayload {
                request_id: "req-1".into(),
                progress,
            })
            .unwrap()
        })
        .collect();
        let failed = serde_json::to_value(ProgressPayload {
            request_id: "req-1".into(),
            progress: PrepareProgress::Failed {
                failed_phase: Step1Phase::Copying,
                message: "the connection to build-mac dropped".into(),
            },
        })
        .unwrap();
        // `update_workspace_base` moves an existing worktree instead of
        // creating one.
        let update = serde_json::to_value(ProgressPayload {
            request_id: "req-1".into(),
            progress: PrepareProgress::UpdatingWorktree,
        })
        .unwrap();
        let golden = serde_json::json!({
            "event": REMOTE_WORKSPACE_PROGRESS_EVENT,
            "sequence": sequence,
            "update": [update],
            "failed": failed,
        });
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../src/tauri/fixtures/remote-workspace-progress.golden.json");
        let rendered = serde_json::to_string_pretty(&golden).unwrap() + "\n";
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(&path, &rendered).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap_or_default(),
            rendered,
            "the Step 1 progress contract changed: rerun with UPDATE_GOLDEN=1 and update the TS type"
        );
    }
}
