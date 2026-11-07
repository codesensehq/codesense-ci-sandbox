// SPDX-License-Identifier: MIT
//! Attach prepared local checkouts matching the tracked source repositories.
//! Missing checkouts require preparation with an external agent or Git CLI.
//! Existing attachments keep their paths; reconciliation never clones or moves Git data.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tauri::Emitter;

use crate::commands::attachments::{
    forget_attachment_record, group_attachments_by_remote, upsert_attachment_and_reconcile,
};
use crate::commands::paths::codesense_home_dir;
use crate::content_engine::sync::WaveCoordinator;
use crate::context_repo::attachments_io::read_attachments;
use crate::context_repo::path_to_user_attachments_file;
use crate::domain::validation::validate_non_empty_no_control;
use crate::domain::{load_org, Attachment, CredentialRef, Principal, SourceRepoSlug};
use crate::mcp::state::ActiveTeamState;
use crate::operations::git_remote::derive_slug_from_repo;
use codesense_fs_core::gate::path_within_any_root;

use super::async_helpers::await_blocking;

/// The `reconcile-workspace-progress` event name (AC-13). One payload per
/// tracked repo as it resolves, driving the Screen-12 N-of-M bar.
const PROGRESS_EVENT: &str = "reconcile-workspace-progress";

/// Bounded-walk depth cap (AC-1). Owner-nested clones (`{root}/{owner}/{repo}`)
/// sit at depth 2; the cap keeps a pathological tree from becoming a walk bomb
/// even before the repo-is-a-leaf rule kicks in.
const MAX_SCAN_DEPTH: usize = 4;

/// SW-MC-5 AC-7: the per-repo outcome of a reconcile pass. Pinned as the
/// **contract surface SW-MC-4 asserts** — MC-4 renders `Moved`/`SkippedInUse`/
/// `SkippedOccupied`/`Failed` rows against this shape rather than re-deriving
/// outcomes from side effects. `#[serde(tag = "kind")]` mirrors
/// `CloneAttachOutcome` so the FE branches on `kind`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ReconcileOutcome {
    /// Found on disk under the root and ensured attached (`managed = false`), or
    /// already in its target state (idempotent no-op).
    Attached { slug: String, repo_path: String },
    /// Tracked and already attached OUTSIDE the scanned root — surfaced, never
    /// clobbered (AC-12; the informational "already attached at {path}" row).
    AlreadyAttachedElsewhere { slug: String, path: String },
    /// Found on disk but NOT in this team's tracked set — surfaced, never
    /// auto-attached (AC-3's intersection guarantee, made visible).
    SkippedNotTracked { origin: String, path: String },
    /// A per-repo failure (clone error, move error, missing managed dir). Does
    /// not roll back other repos; a later reconcile self-heals (AC-7).
    Failed { slug: String, reason: String },
}

/// The action the reconcile state machine decides for one tracked repo — pure,
/// separated from the side effects so the routing is unit-testable without a
/// Tauri runtime, auth, or network (AC-2 state machine).
#[derive(Debug, Clone, PartialEq, Eq)]
enum RepoAction {
    /// Found under root and already recorded at that path — idempotent no-op.
    NoOp { repo_path: PathBuf },
    /// Found under root, not yet recorded — attach as `managed = false`.
    AttachFound { repo_path: PathBuf },
    /// A MANAGED record points at a stale path but the clone is found here under
    /// the root — re-point the record to where it actually is (records-are-truth).
    /// This is the self-heal for an interrupted re-home (dir moved, record not
    /// yet updated) and for a managed clone the user relocated by hand. Carries
    /// the prior record's binding so re-pointing preserves it.
    RepointManaged {
        repo_path: PathBuf,
        stale_path: PathBuf,
        prior: PriorBinding,
    },
    /// Recorded (attached, hands-off) elsewhere — surface, never touch.
    AlreadyElsewhere { path: PathBuf },
    /// Managed clone recorded under a DIFFERENT root — move to the current one.
    /// Carries the prior record's binding so the move preserves it.
    Rehome {
        old_path: PathBuf,
        prior: PriorBinding,
    },
    /// Managed clone recorded under THIS root but its dir is gone — reclone is
    /// SW-MC-7's job, not a reconcile move.
    ManagedGone { path: PathBuf },
    /// Not found, not recorded — clone the missing repo.
    CloneMissing,
}

/// The account + `remote_url` carried from a managed record being re-pointed or
/// re-homed. Preserving it means a managed clone keeps the account whose token
/// cloned it (which future managed pulls authenticate with) rather than being
/// silently rebound to whoever is active at reconcile time.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PriorBinding {
    account: Option<CredentialRef>,
    remote_url: Option<String>,
}

impl PriorBinding {
    fn of(att: &Attachment) -> Self {
        Self {
            account: att.credential_ref.clone(),
            remote_url: att.remote_url.clone(),
        }
    }
}

/// AC-2 routing, pure. `found` is this repo's path if the scan found it under
/// the root; `existing` is its `attachments.yml` record (by origin) if any.
fn classify_repo_state(
    found: Option<&Path>,
    existing: Option<&Attachment>,
    root: &Path,
) -> RepoAction {
    match (found, existing) {
        (Some(found_path), Some(att)) => {
            if paths_equal(Path::new(&att.repo_path), found_path) {
                RepoAction::NoOp {
                    repo_path: found_path.to_path_buf(),
                }
            } else if att.managed {
                // A managed record points elsewhere but the clone is right here
                // under the root — the record is stale (interrupted re-home, or a
                // hand-relocated clone). Re-point it to where the dir actually is
                // so the managed marker isn't lost. Self-heals AC-7 idempotence.
                RepoAction::RepointManaged {
                    repo_path: found_path.to_path_buf(),
                    stale_path: PathBuf::from(&att.repo_path),
                    prior: PriorBinding::of(att),
                }
            } else {
                // Attached (hands-off) elsewhere AND a copy here — never touch the
                // user's own record; surface it (AC-12), don't double-attach.
                RepoAction::AlreadyElsewhere {
                    path: PathBuf::from(&att.repo_path),
                }
            }
        }
        (Some(found_path), None) => RepoAction::AttachFound {
            repo_path: found_path.to_path_buf(),
        },
        (None, Some(att)) => {
            if att.managed {
                if path_within_any_root(
                    Path::new(&att.repo_path),
                    std::slice::from_ref(&root.to_path_buf()),
                ) {
                    // Recorded under this root but the scan didn't find it → dir gone.
                    RepoAction::ManagedGone {
                        path: PathBuf::from(&att.repo_path),
                    }
                } else {
                    RepoAction::Rehome {
                        old_path: PathBuf::from(&att.repo_path),
                        prior: PriorBinding::of(att),
                    }
                }
            } else {
                // Attached in the user's own dir — hands-off (AC-2).
                RepoAction::AlreadyElsewhere {
                    path: PathBuf::from(&att.repo_path),
                }
            }
        }
        (None, None) => RepoAction::CloneMissing,
    }
}

/// AC-1: bounded walk of a workspace root, yielding each git repo directory with
/// its origin slug. A directory that is itself a git repo is a **leaf** — we
/// derive its slug and do NOT recurse into it (one `Repository::open` per repo,
/// which kills the depth-bomb and the monorepo-descent case). `node_modules`,
/// dot-directories, and non-repo/foreign-origin dirs are non-matches, never
/// errors (origin-based matching, architecture-workspace-model §5).
fn scan_workspace_root(root: &Path) -> Vec<(PathBuf, SourceRepoSlug)> {
    let mut found = Vec::new();
    // (dir, depth) worklist — iterative to bound memory and avoid recursion.
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        // Repo-is-a-leaf: `.git` present (dir OR gitlink file) → derive + stop.
        if dir.join(".git").exists() {
            if let Some(slug) = derive_slug_from_repo(&dir) {
                found.push((dir, slug));
            }
            continue;
        }
        if depth >= MAX_SCAN_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // `file_type()` reports the entry itself without following — so a
            // symlinked directory is NOT descended. That keeps the walk bounded
            // to THIS root (a `~/code/x` symlink to `/elsewhere` can't smuggle
            // foreign repos in) and rules out symlink cycles.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() || !file_type.is_dir() {
                continue;
            }
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // Skip dot-dirs (incl. `.git` internals) and dependency trees.
            if name == "node_modules" || name.starts_with('.') {
                continue;
            }
            stack.push((path, depth + 1));
        }
    }
    found
}

/// The team's tracked `owner/repo` set (deduped, configured order preserved).
/// A missing org / unknown team resolves to an empty set (not an error) —
/// nothing to reconcile against, consistent with `list_source_repos_for_team`.
fn load_tracked_slugs(org_root: &str, org_slug: &str, team: &str) -> Vec<SourceRepoSlug> {
    let teams = load_org(org_slug.to_string(), Path::new(org_root), None)
        .org
        .map(|o| o.teams)
        .unwrap_or_default();
    let Some(found) = teams.iter().find(|t| t.name == team) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    found
        .source_repos
        .iter()
        .filter(|s| seen.insert(s.remote_key()))
        .cloned()
        .collect()
}

/// Best-effort path equality: canonicalize both sides (resolves symlinks, `.`,
/// `..`, trailing separators) and fall back to the lexical path when a side
/// doesn't exist on disk. Records-are-truth: the stored `repo_path` and a
/// freshly-scanned path can differ textually yet name the same directory.
fn paths_equal(a: &Path, b: &Path) -> bool {
    fn canon(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }
    canon(a) == canon(b)
}

/// AC-2/AC-8: reconcile a workspace root against the team's tracked set —
/// attach found, clone missing, re-home moved. Returns a per-repo
/// `Vec<ReconcileOutcome>` (AC-7) and streams `reconcile-workspace-progress`
/// events as it goes (AC-13). Idempotent and partial-failure-tolerant: a failure
/// on one repo never rolls back another; a later run self-heals.
#[tauri::command]
#[allow(clippy::too_many_arguments)] // Tauri IPC command surface — args ARE the contract.
pub async fn reconcile_workspace(
    app: tauri::AppHandle,
    github_user_id: u64,
    credential_ref: Option<CredentialRef>,
    credential_refs: Option<Vec<CredentialRef>>,
    org_root: String,
    org_slug: String,
    team: String,
    workspace_root: String,
    coord: tauri::State<'_, Arc<WaveCoordinator>>,
    active_team: tauri::State<'_, ActiveTeamState>,
) -> Result<Vec<ReconcileOutcome>, String> {
    validate_non_empty_no_control(&workspace_root, "workspace_root").map_err(|e| e.to_string())?;
    let root = PathBuf::from(&workspace_root);
    if !root.is_absolute() {
        return Err(format!(
            "workspace_root must be an absolute path: {workspace_root}"
        ));
    }
    let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
    let principal = Principal::from(github_user_id);

    // AC-13: the scan + classify (the heavy prep) runs on the blocking pool,
    // never on the async worker thread. Shared with the dry-run preview so the
    // two can never disagree on what a reconcile would do.
    let planned = await_blocking("reconcile_prepare", {
        let root = root.clone();
        let home = home.clone();
        let principal = principal.clone();
        move || plan_reconcile(&home, &principal, &org_root, &org_slug, &team, &root)
    })
    .await?;

    let credential_refs =
        provider_credentials(credential_refs.unwrap_or_default(), credential_ref)?;
    let ctx = ExecCtx {
        home: &home,
        github_user_id,
        // SW-FG-8 (AC-8): the FE passes the provider-neutral `credential_ref`
        // directly (re-keyed off `github_account_id`).
        credential_refs: &credential_refs,
        coord: coord.inner(),
        active_team: active_team.inner(),
    };
    let total = planned.actions.len();
    let mut outcomes = Vec::with_capacity(total + planned.not_tracked.len());
    for (idx, (slug, action)) in planned.actions.into_iter().enumerate() {
        let outcome = execute_action(&ctx, action, &slug).await;
        emit_progress(&app, idx + 1, total, &slug.to_string(), &outcome);
        outcomes.push(outcome);
    }

    // AC-3: found-on-disk but NOT tracked — surfaced, never auto-attached.
    for (path, slug) in planned.not_tracked {
        outcomes.push(ReconcileOutcome::SkippedNotTracked {
            origin: slug.to_string(),
            path: path.display().to_string(),
        });
    }

    Ok(outcomes)
}

/// The classified plan for a reconcile pass — every tracked repo mapped to a
/// pure [`RepoAction`], plus the found-but-not-tracked repos (AC-3). No side
/// effects; the shared prep for the executor and the dry-run preview.
struct PlannedReconcile {
    actions: Vec<(SourceRepoSlug, RepoAction)>,
    not_tracked: Vec<(PathBuf, SourceRepoSlug)>,
}

/// Load the tracked set, scan the root, and classify each tracked repo. The one
/// place the reconcile decision is computed — both `reconcile_workspace` (which
/// executes) and `preview_workspace_reconcile` (which renders) call it, so the
/// preview can never drift from the real run.
fn plan_reconcile(
    home: &Path,
    principal: &Principal,
    org_root: &str,
    org_slug: &str,
    team: &str,
    root: &Path,
) -> Result<PlannedReconcile, String> {
    let tracked = load_tracked_slugs(org_root, org_slug, team);
    let found = scan_workspace_root(root);
    let att_path = path_to_user_attachments_file(home, principal);
    let attachments = read_attachments(&att_path).map_err(|e| e.to_string())?;

    // Index the scan by origin key; remember which found repos matched a tracked
    // slug so the rest surface as not-tracked (AC-3).
    let mut found_by_key: std::collections::HashMap<String, PathBuf> =
        std::collections::HashMap::with_capacity(found.len());
    for (path, slug) in &found {
        found_by_key
            .entry(slug.remote_key())
            .or_insert_with(|| path.clone());
    }
    let by_remote = group_attachments_by_remote(&attachments);

    let mut actions = Vec::with_capacity(tracked.len());
    let mut tracked_keys: HashSet<String> = HashSet::with_capacity(tracked.len());
    for slug in tracked {
        let key = slug.remote_key();
        tracked_keys.insert(key.clone());
        let action = classify_repo_state(
            found_by_key.get(&key).map(|p| p.as_path()),
            by_remote.get(&key),
            root,
        );
        actions.push((slug, action));
    }
    let not_tracked = found
        .into_iter()
        .filter(|(_, slug)| !tracked_keys.contains(&slug.remote_key()))
        .collect();
    Ok(PlannedReconcile {
        actions,
        not_tracked,
    })
}

/// SW-MC-5 AC-10: one row of the reconcile preview (Screen 11) — what a run
/// WOULD do, without doing it. `#[serde(tag = "action")]` so the FE branches on
/// `action` to render the disposition tag (→ attach / → will clone / → move).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum ReconcilePlan {
    /// Found under the root, will be attached (`managed = false`).
    Attach { slug: String, path: String },
    /// Missing checkout must be prepared outside the app before attachment.
    PreparationRequired { slug: String, reason: String },
    /// Tracked and already attached outside the root — will be left untouched.
    AlreadyAttachedElsewhere { slug: String, path: String },
    /// Already in its target state — nothing will change.
    NoOp { slug: String, path: String },
    /// Managed record whose directory is missing — re-clone territory (SW-MC-7).
    ManagedGone { slug: String, path: String },
    /// Found on disk but not tracked — will be ignored, never auto-attached.
    NotTracked { origin: String, path: String },
}

/// SW-MC-5 AC-10: read-only preview of a reconcile — the data behind Screen 11.
/// Shares [`plan_reconcile`] with the executor, so the preview can never drift
/// from the real run. No auth, no clone, no move; pure scan + classify on the
/// blocking pool (AC-13).
#[tauri::command]
pub async fn preview_workspace_reconcile(
    github_user_id: u64,
    org_root: String,
    org_slug: String,
    team: String,
    workspace_root: String,
) -> Result<Vec<ReconcilePlan>, String> {
    validate_non_empty_no_control(&workspace_root, "workspace_root").map_err(|e| e.to_string())?;
    let root = PathBuf::from(&workspace_root);
    if !root.is_absolute() {
        return Err(format!(
            "workspace_root must be an absolute path: {workspace_root}"
        ));
    }
    let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
    let principal = Principal::from(github_user_id);

    await_blocking("reconcile_preview", move || {
        let planned = plan_reconcile(&home, &principal, &org_root, &org_slug, &team, &root)?;
        let mut plans: Vec<ReconcilePlan> = planned
            .actions
            .into_iter()
            .map(|(slug, action)| plan_for_action(&slug, action, &home, &principal, &root))
            .collect();
        for (path, slug) in planned.not_tracked {
            plans.push(ReconcilePlan::NotTracked {
                origin: slug.to_string(),
                path: path.display().to_string(),
            });
        }
        Ok(plans)
    })
    .await
}

/// Map a decided [`RepoAction`] to its preview [`ReconcilePlan`] row, resolving
/// the clone/move target the same way the executor would (records-are-truth).
fn plan_for_action(
    slug: &SourceRepoSlug,
    action: RepoAction,
    _home: &Path,
    _principal: &Principal,
    _root: &Path,
) -> ReconcilePlan {
    let slug_str = slug.to_string();
    match action {
        RepoAction::NoOp { repo_path } => ReconcilePlan::NoOp {
            slug: slug_str,
            path: repo_path.display().to_string(),
        },
        RepoAction::AttachFound { repo_path } => ReconcilePlan::Attach {
            slug: slug_str,
            path: repo_path.display().to_string(),
        },
        // Re-point surfaces as an attach in the preview — the dir is already here
        // under the root; only the (managed) record is being corrected to match.
        RepoAction::RepointManaged { repo_path, .. } => ReconcilePlan::Attach {
            slug: slug_str,
            path: repo_path.display().to_string(),
        },
        RepoAction::AlreadyElsewhere { path } => ReconcilePlan::AlreadyAttachedElsewhere {
            slug: slug_str,
            path: path.display().to_string(),
        },
        RepoAction::ManagedGone { path } => ReconcilePlan::ManagedGone {
            slug: slug_str,
            path: path.display().to_string(),
        },
        RepoAction::CloneMissing => ReconcilePlan::PreparationRequired {
            slug: slug_str,
            reason: "Prepare this repository with your agent or Git CLI, then attach the local checkout.".into(),
        },
        RepoAction::Rehome { old_path, .. } => ReconcilePlan::AlreadyAttachedElsewhere {
            slug: slug_str,
            path: old_path.display().to_string(),
        },
    }
}

/// The shared side-effect context threaded through the executor's helpers — the
/// resolved home/principal/root plus the account binding and the wave handles.
/// Bundled so `attach_found` / `clone_missing` / `rehome_managed` take a handful
/// of args instead of ten (and drop the `too_many_arguments` allows). `auth` is
/// passed separately — only the clone path needs it, and threading a
/// `tauri::State` reference through a struct buys nothing.
struct ExecCtx<'a> {
    home: &'a Path,
    github_user_id: u64,
    /// SW-FG-8 (AC-8): the connection newly-cloned repos bind to, passed
    /// provider-neutral as `credential_ref` by the FE. `None` ⇒ bind to the
    /// active user (`github_user_id`).
    credential_refs: &'a HashMap<String, CredentialRef>,
    coord: &'a WaveCoordinator,
    active_team: &'a ActiveTeamState,
}

/// Execute one decided [`RepoAction`], producing its [`ReconcileOutcome`]. The
/// side-effecting half of the state machine; the decision half is the pure
/// [`classify_repo_state`].
async fn execute_action(
    ctx: &ExecCtx<'_>,
    action: RepoAction,
    slug: &SourceRepoSlug,
) -> ReconcileOutcome {
    let slug_str = slug.to_string();
    match action {
        RepoAction::NoOp { repo_path } => ReconcileOutcome::Attached {
            slug: slug_str,
            repo_path: repo_path.display().to_string(),
        },
        RepoAction::AlreadyElsewhere { path } => ReconcileOutcome::AlreadyAttachedElsewhere {
            slug: slug_str,
            path: path.display().to_string(),
        },
        RepoAction::ManagedGone { path } => ReconcileOutcome::Failed {
            slug: slug_str,
            reason: format!(
                "managed clone directory is missing at {} — re-clone it first (SW-MC-7)",
                path.display()
            ),
        },
        RepoAction::AttachFound { repo_path } => {
            match write_recognized_attachment(ctx, &repo_path, slug, false, None, None).await {
                Ok(()) => ReconcileOutcome::Attached {
                    slug: slug_str,
                    repo_path: repo_path.display().to_string(),
                },
                Err(reason) => ReconcileOutcome::Failed {
                    slug: slug_str,
                    reason,
                },
            }
        }
        RepoAction::RepointManaged {
            repo_path,
            stale_path,
            prior,
        } => {
            match write_recognized_attachment(
                ctx,
                &repo_path,
                slug,
                true,
                Some(&stale_path),
                Some(&prior),
            )
            .await
            {
                Ok(()) => ReconcileOutcome::Attached {
                    slug: slug_str,
                    repo_path: repo_path.display().to_string(),
                },
                Err(reason) => ReconcileOutcome::Failed {
                    slug: slug_str,
                    reason,
                },
            }
        }
        RepoAction::CloneMissing => ReconcileOutcome::Failed {
            slug: slug_str,
            reason: "Repository unavailable locally. Prepare a checkout with your agent or Git CLI and attach its folder.".into(),
        },
        RepoAction::Rehome { old_path, .. } => ReconcileOutcome::AlreadyAttachedElsewhere {
            slug: slug_str,
            path: old_path.display().to_string(),
        }
    }
}

/// Write (or re-point) a recognized attachment for `repo_path` through the
/// shared insert-gated write path — same slug derivation, same reconcile, same
/// wave fan-out as attaching one repo by hand. `managed` picks whether codesense
/// owns the dir (hands-off attach vs managed clone). `forget_stale`, when set,
/// removes a prior record at that path FIRST (the re-point / self-heal case) so
/// the origin resolves to exactly one record; the subsequent insert at the new
/// path fires the lockfile carry-forward. `prior`, when set (re-point / re-home
/// of an existing managed record), carries that record's account + `remote_url`
/// forward so the binding survives — a managed clone keeps the account whose
/// token cloned it rather than rebinding to whoever is active now.
async fn write_recognized_attachment(
    ctx: &ExecCtx<'_>,
    repo_path: &Path,
    slug: &SourceRepoSlug,
    managed: bool,
    forget_stale: Option<&Path>,
    prior: Option<&PriorBinding>,
) -> Result<(), String> {
    if let Some(stale) = forget_stale {
        // Best-effort: a stale record we couldn't drop is shadowed by the
        // origin-dedup on read anyway, so never fail the re-point on it.
        let _ =
            forget_attachment_record(ctx.home, ctx.github_user_id, &stale.display().to_string());
    }
    // A managed record binds to the connection whose token cloned it: preserve a
    // prior binding, else the cloning connection, else the user. An attached
    // (hands-off) record carries the current connection as-is.
    let selected = credential_for_slug(ctx.credential_refs, slug);
    let credential_ref = if managed {
        prior
            .and_then(|p| p.account.clone())
            .or(selected)
            .or_else(|| {
                (slug.provider == "github").then(|| CredentialRef::github(ctx.github_user_id))
            })
    } else {
        selected
    };
    let remote_url = prior
        .and_then(|p| p.remote_url.clone())
        .map_or_else(|| slug.cloud_https_remote().map_err(|e| e.to_string()), Ok)?;
    let attachment = Attachment {
        repo_path: repo_path.display().to_string(),
        remote_url: Some(remote_url),
        attached_at: None,
        tracking_prompt_dismissed_at: None,
        credential_ref,
        managed,
    };
    upsert_attachment_and_reconcile(
        ctx.home,
        ctx.github_user_id,
        attachment,
        ctx.coord,
        ctx.active_team,
    )
    .await
}

fn provider_credentials(
    refs: Vec<CredentialRef>,
    legacy: Option<CredentialRef>,
) -> Result<HashMap<String, CredentialRef>, String> {
    let mut by_provider = HashMap::new();
    for credential in refs.into_iter().chain(legacy) {
        let provider = credential.provider.trim().to_ascii_lowercase();
        if provider.is_empty() || credential.account.is_empty() {
            return Err("credential reference must include provider and account".into());
        }
        if by_provider.insert(provider.clone(), credential).is_some() {
            return Err(format!(
                "multiple active credentials supplied for provider '{provider}'"
            ));
        }
    }
    Ok(by_provider)
}

fn credential_for_slug(
    refs: &HashMap<String, CredentialRef>,
    slug: &SourceRepoSlug,
) -> Option<CredentialRef> {
    refs.get(&slug.provider.to_ascii_lowercase()).cloned()
}

/// AC-13: emit one N-of-M progress tick for a resolved repo. Best-effort — a
/// dropped event never fails the reconcile.
fn emit_progress(
    app: &tauri::AppHandle,
    done: usize,
    total: usize,
    repo: &str,
    outcome: &ReconcileOutcome,
) {
    let _ = app.emit(
        PROGRESS_EVENT,
        serde_json::json!({
            "done": done,
            "total": total,
            "repo": repo,
            "phase": progress_phase(outcome),
        }),
    );
}

/// A short phase tag for the progress event, derived from the resolved outcome.
fn progress_phase(outcome: &ReconcileOutcome) -> &'static str {
    match outcome {
        ReconcileOutcome::Attached { .. } => "attached",
        ReconcileOutcome::AlreadyAttachedElsewhere { .. } => "alreadyAttached",
        ReconcileOutcome::SkippedNotTracked { .. } => "skippedNotTracked",
        ReconcileOutcome::Failed { .. } => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_remote_uses_slug_cloud_host_and_rejects_unknown_hosts() {
        let gitlab =
            SourceRepoSlug::with_provider_and_host("acme", "api", "gitlab", Some("gitlab.com"))
                .unwrap();
        assert_eq!(
            gitlab.cloud_https_remote().unwrap(),
            "https://gitlab.com/acme/api"
        );
        let self_managed =
            SourceRepoSlug::with_provider_and_host("acme", "api", "gitea", Some("git.example.com"))
                .unwrap();
        assert!(self_managed.cloud_https_remote().is_err());
    }

    fn attachment(repo_path: &str, managed: bool) -> Attachment {
        Attachment {
            repo_path: repo_path.to_string(),
            remote_url: Some("https://github.com/o/r".to_string()),
            attached_at: None,
            tracking_prompt_dismissed_at: None,
            credential_ref: None,
            managed,
        }
    }

    // ---------- classify_repo_state (pure routing, AC-2/3/4/5) ----------

    #[test]
    fn found_and_recorded_same_path_is_noop() {
        let root = Path::new("/ws");
        let att = attachment("/ws/o/r", false);
        let action = classify_repo_state(Some(Path::new("/ws/o/r")), Some(&att), root);
        assert_eq!(
            action,
            RepoAction::NoOp {
                repo_path: PathBuf::from("/ws/o/r")
            }
        );
    }

    #[test]
    fn found_not_recorded_attaches() {
        let root = Path::new("/ws");
        let action = classify_repo_state(Some(Path::new("/ws/codesense")), None, root);
        assert_eq!(
            action,
            RepoAction::AttachFound {
                repo_path: PathBuf::from("/ws/codesense")
            }
        );
    }

    #[test]
    fn found_but_recorded_elsewhere_surfaces_not_double_attaches() {
        let root = Path::new("/ws");
        let att = attachment("/home/me/other/r", false);
        let action = classify_repo_state(Some(Path::new("/ws/o/r")), Some(&att), root);
        assert_eq!(
            action,
            RepoAction::AlreadyElsewhere {
                path: PathBuf::from("/home/me/other/r")
            }
        );
    }

    #[test]
    fn managed_record_found_under_root_repoints_not_surfaces() {
        // Interrupted re-home (or a hand-relocated managed clone): the managed
        // record points at a stale path but the dir is found here under the root.
        // Must re-point (heal) rather than surface as AlreadyElsewhere.
        let root = Path::new("/ws");
        let att = attachment("/oldroot/o/r", true);
        let action = classify_repo_state(Some(Path::new("/ws/o/r")), Some(&att), root);
        assert_eq!(
            action,
            RepoAction::RepointManaged {
                repo_path: PathBuf::from("/ws/o/r"),
                stale_path: PathBuf::from("/oldroot/o/r"),
                prior: PriorBinding::of(&att),
            }
        );
    }

    #[test]
    fn attached_elsewhere_off_disk_is_hands_off() {
        let root = Path::new("/ws");
        let att = attachment("/home/me/code/r", false);
        let action = classify_repo_state(None, Some(&att), root);
        assert_eq!(
            action,
            RepoAction::AlreadyElsewhere {
                path: PathBuf::from("/home/me/code/r")
            }
        );
    }

    #[test]
    fn managed_under_old_root_rehomes() {
        let root = Path::new("/newroot");
        let att = attachment("/oldroot/o/r", true);
        let action = classify_repo_state(None, Some(&att), root);
        assert_eq!(
            action,
            RepoAction::Rehome {
                old_path: PathBuf::from("/oldroot/o/r"),
                prior: PriorBinding::of(&att),
            }
        );
    }

    #[test]
    fn rehome_carries_prior_account_and_remote_url() {
        // The managed clone keeps the account whose token cloned it (and its
        // stored remote_url), never rebinding to the current account on re-home.
        let root = Path::new("/newroot");
        let mut att = attachment("/oldroot/o/r", true);
        att.credential_ref = Some(CredentialRef::github(999));
        att.remote_url = Some("git@github.com:o/r.git".into());
        match classify_repo_state(None, Some(&att), root) {
            RepoAction::Rehome { prior, .. } => {
                assert_eq!(prior.account, Some(CredentialRef::github(999)));
                assert_eq!(prior.remote_url.as_deref(), Some("git@github.com:o/r.git"));
            }
            other => panic!("expected Rehome, got {other:?}"),
        }
    }

    #[test]
    fn managed_under_current_root_but_missing_is_managed_gone() {
        let root = Path::new("/ws");
        let att = attachment("/ws/o/r", true);
        // Not found on disk (scan miss) but recorded under this very root.
        let action = classify_repo_state(None, Some(&att), root);
        assert_eq!(
            action,
            RepoAction::ManagedGone {
                path: PathBuf::from("/ws/o/r")
            }
        );
    }

    #[test]
    fn missing_and_unrecorded_clones() {
        let root = Path::new("/ws");
        let action = classify_repo_state(None, None, root);
        assert_eq!(action, RepoAction::CloneMissing);
    }

    // ---------- scanner (AC-1) ----------

    fn init_repo_with_origin(dir: &Path, origin: &str) {
        let repo = git2::Repository::init(dir).unwrap();
        repo.remote("origin", origin).unwrap();
    }

    #[test]
    fn scan_finds_flat_and_owner_nested_repos_by_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Flat: {root}/codesense
        let flat = root.join("codesense");
        std::fs::create_dir_all(&flat).unwrap();
        init_repo_with_origin(&flat, "https://github.com/alderco/codesense");
        // Owner-nested: {root}/acme/api
        let nested = root.join("acme").join("api");
        std::fs::create_dir_all(&nested).unwrap();
        init_repo_with_origin(&nested, "https://github.com/acme/api");

        let found = scan_workspace_root(root);
        let mut keys: Vec<String> = found.iter().map(|(_, s)| s.remote_key()).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["github.com/acme/api", "github.com/alderco/codesense"]
        );
    }

    #[test]
    fn scan_skips_node_modules_and_dotdirs_and_non_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("node_modules").join("pkg")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::create_dir_all(root.join("just-a-folder")).unwrap();
        let found = scan_workspace_root(root);
        assert!(
            found.is_empty(),
            "no git repos → nothing found, got {found:?}"
        );
    }

    #[test]
    fn scan_does_not_descend_into_a_repo_leaf() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let outer = root.join("outer");
        std::fs::create_dir_all(&outer).unwrap();
        init_repo_with_origin(&outer, "https://github.com/o/outer");
        // A nested repo INSIDE the outer repo must NOT be surfaced — a repo is a leaf.
        let inner = outer.join("vendor").join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        init_repo_with_origin(&inner, "https://github.com/o/inner");

        let found = scan_workspace_root(root);
        let keys: Vec<String> = found.iter().map(|(_, s)| s.remote_key()).collect();
        assert_eq!(
            keys,
            vec!["github.com/o/outer"],
            "must stop at the repo leaf"
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_follow_symlinked_dirs() {
        // A symlink under the root pointing at an external repo must NOT be
        // descended — keeps the walk bounded to the real root (no smuggling).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let external = tmp.path().join("external");
        std::fs::create_dir_all(&external).unwrap();
        init_repo_with_origin(&external, "https://github.com/o/external");
        std::os::unix::fs::symlink(&external, root.join("link")).unwrap();

        let found = scan_workspace_root(&root);
        assert!(
            found.is_empty(),
            "must not follow a symlinked dir into an external repo, got {found:?}"
        );
    }

    // ---------- outcome serde shape (MC-4 contract) ----------

    #[test]
    fn already_attached_elsewhere_outcome_shape() {
        let json = serde_json::to_value(ReconcileOutcome::AlreadyAttachedElsewhere {
            slug: "o/r".into(),
            path: "/home/me/r".into(),
        })
        .unwrap();
        assert_eq!(json["kind"], "alreadyAttachedElsewhere");
        assert_eq!(json["path"], "/home/me/r");
    }

    #[test]
    fn preview_requires_external_preparation_for_missing_checkout() {
        let plan = plan_for_action(
            &SourceRepoSlug::new("o", "r").unwrap(),
            RepoAction::CloneMissing,
            Path::new("/home"),
            &Principal::from(42u64),
            Path::new("/ws"),
        );
        assert!(matches!(plan, ReconcilePlan::PreparationRequired { .. }));
    }
}
