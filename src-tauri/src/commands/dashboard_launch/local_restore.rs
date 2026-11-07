// SPDX-License-Identifier: MIT
//! Restoring local launches across an app restart that keeps the session server.
//!
//! The launch registry lives in this app process, while a local agent's terminal lives in
//! the session server and outlives it. At bind the app writes what the registry knows about
//! the launch onto the session's own tab (its tags); when a later app process first reads
//! the layout of a session server it found already running, it rebuilds each tagged live
//! tab's entry from those tags, the way a remote tab is rebuilt from its `remoteHandle`.
//! A tab is restored only while the agent its launch named still runs in it, for the
//! account signed in now.

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use codesense_pty_core::layout::{LayoutSnapshot, TagPatch, Tags};
use tauri::Manager;

use super::{
    emit_session_state_changed, launch_team_from_key, DashboardLaunchState, LaunchEntry,
    SessionOwner, KIND_DASHBOARD_LAUNCHED, KIND_HEADLESS, KIND_REGISTERED,
};
use crate::commands::agents_launch::RestoredRevision;
use crate::commands::async_helpers::await_blocking;
use crate::commands::pty_host::LocalEffectClaim;

/// The registry handle the session's launch was bound to.
pub(crate) const TAG_LAUNCH_HANDLE: &str = "launchHandle";
/// The agent revision (`<key>@<revision>`) the registry recorded for the launch.
pub(crate) const TAG_LAUNCH_AGENT_REVISION: &str = "launchAgentRevision";
/// The team key (`org:team`) the launch was made in.
pub(crate) const TAG_LAUNCH_TEAM: &str = "launchTeam";
/// The account signed in when the session was launched.
pub(crate) const TAG_LAUNCH_PRINCIPAL: &str = "launchPrincipal";
/// The launching surface (a dashboard doc path, `codesense`, MCP).
pub(crate) const TAG_LAUNCHER: &str = "launcher";
/// Epoch ms the session was bound, so the newer of two tabs naming one handle wins.
pub(crate) const TAG_LAUNCH_BOUND_AT: &str = "launchBoundAt";
/// Epoch ms the launch started, the row's "elapsed" anchor.
pub(crate) const TAG_LAUNCH_STARTED_AT: &str = "launchStartedAt";
/// The registry kind (`dashboard-launched`, `registered`).
pub(crate) const TAG_LAUNCH_KIND: &str = "launchKind";
/// Who launched the session (a handle, or a raw session id).
pub(crate) const TAG_LAUNCHED_BY: &str = "launchedBy";
/// The display name the launcher gave the session.
pub(crate) const TAG_LAUNCH_NAME: &str = "launchName";
/// The role the launcher gave the session.
pub(crate) const TAG_LAUNCH_ROLE: &str = "launchRole";
/// The avatar the launcher gave the session.
pub(crate) const TAG_LAUNCH_AVATAR: &str = "launchAvatar";
/// The story the session works on.
pub(crate) const TAG_LAUNCH_STORY: &str = "launchStory";
/// The directory recorded at launch.
pub(crate) const TAG_LAUNCH_CWD: &str = "launchCwd";
/// The routine that launched this session, independent of its current cwd.
pub(crate) const TAG_LAUNCH_ROUTINE: &str = "launchRoutineKey";
/// epoch-ms deadline the launch was armed with. Written by the
/// app at bind so a later process can re-arm the deadline sweep on the launch
/// its session server still runs. Missing on an older tab or an unbounded
/// launch — the on-disk `PersistedLaunch.deadline_ms` is the second-best
/// source, same contract as [`TAG_LAUNCHED_BY`].
pub(crate) const TAG_LAUNCH_DEADLINE: &str = "launchDeadlineAt";
/// The tag a remote tab carries; such a tab is restored by the remote path, never here.
const TAG_REMOTE_HANDLE: &str = "remoteHandle";

/// Every tag only the app writes.
const LAUNCH_TAGS: [&str; 16] = [
    TAG_LAUNCH_HANDLE,
    TAG_LAUNCH_AGENT_REVISION,
    TAG_LAUNCH_TEAM,
    TAG_LAUNCH_PRINCIPAL,
    TAG_LAUNCHER,
    TAG_LAUNCH_BOUND_AT,
    TAG_LAUNCH_STARTED_AT,
    TAG_LAUNCH_KIND,
    TAG_LAUNCHED_BY,
    TAG_LAUNCH_NAME,
    TAG_LAUNCH_ROLE,
    TAG_LAUNCH_AVATAR,
    TAG_LAUNCH_STORY,
    TAG_LAUNCH_CWD,
    TAG_LAUNCH_ROUTINE,
    TAG_LAUNCH_DEADLINE,
];

/// Whether `key` is launch metadata only the app itself writes.
pub(crate) fn is_launch_tag(key: &str) -> bool {
    LAUNCH_TAGS.contains(&key)
}

/// The kind a tag names, as the registry spells it; anything else is a registration.
fn kind_of(tag: Option<&str>) -> &'static str {
    match tag {
        Some(KIND_DASHBOARD_LAUNCHED) => KIND_DASHBOARD_LAUNCHED,
        _ => KIND_REGISTERED,
    }
}

impl LaunchEntry {
    /// A plain registration (a workspace's main session the nav named) that a restored
    /// launch of the same session takes over.
    fn yields_to_restored_launch(&self) -> bool {
        !self.names_an_agent()
    }
}

impl DashboardLaunchState {
    /// The tags that let a later app process restore `session_id`'s launch: everything
    /// the registry recorded for it. Each recorded field is named, so a rebind clears what
    /// the session's earlier launch left; the launcher, first-writer wins, is only ever
    /// added. `None` for a session with no local launch entry.
    pub(crate) fn launch_tags_for_session(&self, session_id: &str) -> Option<TagPatch> {
        let inner = self.lock();
        let handle = inner.by_session.get(session_id)?;
        let entry = inner.by_handle.get(handle)?;
        if entry.kind == KIND_HEADLESS || entry.remote_shell.is_some() {
            return None;
        }
        let team_key = entry
            .owner
            .as_ref()
            .map(|owner| crate::commands::pty_layout::team_key(&owner.org, owner.team.as_deref()))
            .or_else(|| {
                entry
                    .meta
                    .team
                    .as_ref()
                    .map(|team| crate::commands::pty_layout::team_key(&team.org, Some(&team.team)))
            });
        let principal = entry
            .owner
            .as_ref()
            .and_then(|owner| owner.principal.clone());
        let mut patch = TagPatch::from([
            (TAG_LAUNCH_HANDLE.to_string(), Some(handle.clone())),
            (
                TAG_LAUNCH_AGENT_REVISION.to_string(),
                entry.agent_revision.clone(),
            ),
            (TAG_LAUNCH_TEAM.to_string(), team_key),
            (TAG_LAUNCH_PRINCIPAL.to_string(), principal),
            (
                TAG_LAUNCH_BOUND_AT.to_string(),
                entry.bound_at_ms.map(|at| at.to_string()),
            ),
            (
                TAG_LAUNCH_STARTED_AT.to_string(),
                entry.started_at_ms.map(|at| at.to_string()),
            ),
            (TAG_LAUNCH_KIND.to_string(), Some(entry.kind.to_string())),
            (TAG_LAUNCHED_BY.to_string(), entry.meta.launched_by.clone()),
            (TAG_LAUNCH_NAME.to_string(), entry.meta.display_name.clone()),
            (TAG_LAUNCH_ROLE.to_string(), entry.meta.role.clone()),
            (TAG_LAUNCH_AVATAR.to_string(), entry.meta.avatar.clone()),
            (TAG_LAUNCH_STORY.to_string(), entry.meta.story.clone()),
            (TAG_LAUNCH_ROUTINE.to_string(), entry.routine_key.clone()),
            ("chatWorkspaceId".into(), entry.chat_workspace_id.clone()),
            (
                TAG_LAUNCH_CWD.to_string(),
                entry
                    .worktree_dir
                    .as_ref()
                    .map(|dir| dir.to_string_lossy().into_owned()),
            ),
            (
                TAG_LAUNCH_DEADLINE.to_string(),
                entry.deadline_ms.map(|at| at.to_string()),
            ),
        ]);
        if let Some(launcher) = &entry.launcher {
            patch.insert(TAG_LAUNCHER.to_string(), Some(launcher.clone()));
        }
        Some(patch)
    }

    /// Whether `launch` may be restored now: its handle is free, and its session is free or
    /// held only by a plain registration the restored launch takes over.
    fn restorable(&self, launch: &RestoredLaunch) -> bool {
        let inner = self.lock();
        Self::restorable_in(&inner, launch)
    }

    fn restorable_in(inner: &super::Inner, launch: &RestoredLaunch) -> bool {
        if inner.by_handle.contains_key(&launch.handle) {
            return false;
        }
        inner
            .by_session
            .get(&launch.session)
            .and_then(|held| inner.by_handle.get(held))
            .is_none_or(LaunchEntry::yields_to_restored_launch)
    }

    /// Rebuild `launch`'s registry entry, recording `agent_revision` (already verified, and
    /// its agent seen running). A restored dashboard launch takes a background slot again,
    /// past the limit if it must: it is running, so new launches wait for it. Never touches
    /// a handle the registry already holds, or a session a launch holds; `false` then.
    pub(crate) fn restore_launch(
        &self,
        launch: &RestoredLaunch,
        claim: Option<LocalEffectClaim>,
        agent_revision: Option<String>,
    ) -> bool {
        // SW-FX-launcher-persistence (step 3): read the on-disk fallback for
        // `launched_by` BEFORE taking the inner lock, so the persistence mutex
        // and the inner mutex are never nested. Preferred source is still the
        // session-server tag (`launch.launched_by`); the file is the fallback
        // for tabs whose tags were lost or never written.
        let persisted_launched_by = self.persisted_launched_by(&launch.handle);
        let launcher_context = self.persisted_launcher_context(&launch.handle);
        // same fallback shape for the deadline.
        let persisted_deadline = self.persisted_deadline_ms(&launch.handle);
        let persisted_worktree_dir = self.persisted_worktree_dir(&launch.handle, &launch.session);
        let mut inner = self.lock();
        if !Self::restorable_in(&inner, launch) {
            return false;
        }
        if let Some(plain) = inner.by_session.remove(&launch.session) {
            inner.by_handle.remove(&plain);
        }
        let mut entry = LaunchEntry::registered(&launch.session, launch.launcher.as_deref(), claim);
        entry.kind = launch.kind;
        if launch.kind == KIND_DASHBOARD_LAUNCHED {
            entry._permit = Some(self.cap.force_acquire());
        }
        entry.started_at_ms = launch.started_at_ms.or(launch.bound_at_ms);
        entry.bound_at_ms = launch.bound_at_ms;
        entry.agent_revision = agent_revision;
        // Preserve the deadline and identify an already-overdue restore. The tab tag is
        // written once at spawn, while human input in the pane moves the saved one
        // later, so the later of the two wins.
        entry.deadline_ms = match (launch.deadline_ms, persisted_deadline) {
            (Some(tag), Some(saved)) => Some(tag.max(saved)),
            (tag, saved) => tag.or(saved),
        };
        entry.exit_sent_at_ms = None;
        entry.restored_overdue = entry
            .deadline_ms
            .is_some_and(|deadline| deadline <= super::now_epoch_ms());
        // Reconstruct the budget as `deadline - bound_at` so the enforcement
        // note still renders a human duration (`30m` etc.). When `bound_at` is
        // also missing the note falls back to the opaque "its budget" phrase.
        entry.max_runtime_ms = entry
            .deadline_ms
            .and_then(|deadline| entry.bound_at_ms.and_then(|at| deadline.checked_sub(at)));
        entry.worktree_dir = persisted_worktree_dir;
        entry.meta.launcher_context = launcher_context;
        entry.meta.display_name = launch.name.clone();
        entry.meta.launched_by = launch.launched_by.clone().or(persisted_launched_by);
        entry.meta.role = launch.role.clone();
        entry.meta.avatar = launch.avatar.clone();
        entry.meta.story = launch.story.clone();
        entry.routine_key = launch.routine_key.clone();
        entry.chat_workspace_id = launch.chat_workspace_id.clone();
        if let Some(team_key) = launch.team_key.as_deref() {
            let (org, team) = crate::commands::pty_layout::parse_team_key(team_key);
            if !org.is_empty() {
                entry.owner = Some(SessionOwner {
                    principal: launch.principal.clone(),
                    org: org.to_string(),
                    team: team.map(str::to_string),
                });
                entry.meta.team = launch_team_from_key(team_key);
            }
        }
        inner.by_handle.insert(launch.handle.clone(), entry);
        inner
            .by_session
            .insert(launch.session.clone(), launch.handle.clone());
        drop(inner);
        // SW-FX-launcher-persistence (step 1): refresh the on-disk record with
        // what the restored session is now known to carry.
        self.queue_persist(&launch.handle);
        true
    }
}

/// A live local tab's launch, as its tags recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestoredLaunch {
    pub session: String,
    pub handle: String,
    pub kind: &'static str,
    pub agent_revision: Option<String>,
    pub team_key: Option<String>,
    pub principal: Option<String>,
    pub launcher: Option<String>,
    pub bound_at_ms: Option<u64>,
    pub started_at_ms: Option<u64>,
    pub launched_by: Option<String>,
    pub name: Option<String>,
    pub role: Option<String>,
    pub avatar: Option<String>,
    pub story: Option<String>,
    pub chat_workspace_id: Option<String>,
    pub routine_key: Option<String>,
    /// the deadline-sweep budget the live tab recorded, from
    /// [`TAG_LAUNCH_DEADLINE`]. `None` for an older tab, an unbounded launch,
    /// or a tag that fell off; the restore path falls back to
    /// `PersistedLaunch.deadline_ms` in that case.
    pub deadline_ms: Option<u64>,
}

fn tag(tags: &Tags, key: &str) -> Option<String> {
    tags.get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn epoch_tag(tags: &Tags, key: &str) -> Option<u64> {
    tag(tags, key).and_then(|at| at.parse().ok())
}

/// The launches a local layout's live tabs recorded, one per handle. An exited tab, a
/// remote tab and a tab with no launch handle are left out; of two live tabs naming one
/// handle, the more recently bound wins and the other stays unregistered.
pub(crate) fn restorable_launches(snapshot: &LayoutSnapshot) -> Vec<RestoredLaunch> {
    let mut by_handle: BTreeMap<String, RestoredLaunch> = BTreeMap::new();
    for tab in snapshot.workspaces.values().flat_map(|layout| &layout.tabs) {
        if tab.state.is_exited() || tab.tags.contains_key(TAG_REMOTE_HANDLE) {
            continue;
        }
        let tags = &tab.tags;
        let Some(handle) = tag(tags, TAG_LAUNCH_HANDLE) else {
            continue;
        };
        let launch = RestoredLaunch {
            session: tab.session.as_str().to_string(),
            handle: handle.clone(),
            kind: kind_of(tags.get(TAG_LAUNCH_KIND).map(String::as_str)),
            agent_revision: tag(tags, TAG_LAUNCH_AGENT_REVISION),
            team_key: tag(tags, TAG_LAUNCH_TEAM),
            principal: tag(tags, TAG_LAUNCH_PRINCIPAL),
            launcher: tag(tags, TAG_LAUNCHER),
            bound_at_ms: epoch_tag(tags, TAG_LAUNCH_BOUND_AT),
            started_at_ms: epoch_tag(tags, TAG_LAUNCH_STARTED_AT),
            launched_by: tag(tags, TAG_LAUNCHED_BY),
            name: tag(tags, TAG_LAUNCH_NAME),
            role: tag(tags, TAG_LAUNCH_ROLE),
            avatar: tag(tags, TAG_LAUNCH_AVATAR),
            story: tag(tags, TAG_LAUNCH_STORY),
            routine_key: tag(tags, TAG_LAUNCH_ROUTINE),
            chat_workspace_id: tag(tags, "chatWorkspaceId"),
            deadline_ms: epoch_tag(tags, TAG_LAUNCH_DEADLINE),
        };
        match by_handle.entry(handle) {
            Entry::Vacant(slot) => {
                slot.insert(launch);
            }
            Entry::Occupied(mut held) => {
                let newer = (launch.bound_at_ms.unwrap_or(0), &launch.session)
                    > (held.get().bound_at_ms.unwrap_or(0), &held.get().session);
                if newer {
                    held.insert(launch);
                }
            }
        }
    }
    by_handle.into_values().collect()
}

/// Write `session_id`'s launch tags onto its local tab. Best-effort: a failure is logged
/// and never fails the launch that bound it. Writes are serialized and each reads the
/// registry when it runs, so the tab ends with the entry's latest state.
pub(crate) async fn tag_bound_launch(app: &tauri::AppHandle, session_id: &str) {
    let write_app = app.clone();
    let session = session_id.to_string();
    let written = await_blocking("dashboard_launch.tag", move || {
        let state = write_app.state::<DashboardLaunchState>();
        let _serial = state
            .tag_writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(patch) = state.launch_tags_for_session(&session) else {
            return Ok(());
        };
        crate::commands::pty_backend::with_session_reference(
            &write_app,
            &crate::commands::machines::RuntimeSessionReference::Legacy(session),
            false,
            |domain, session| {
                domain
                    .set_tags(session, patch)
                    .map_err(|error| error.to_string())
            },
        )
    })
    .await;
    if let Err(error) = written {
        log::warn!(target: "agents", "launch tags not written on session {session_id}: {error}");
    }
}

/// Once per app process: restoring the launches of the session server this app found
/// running. A restore that gives up is released, so a later layout read starts it again.
#[derive(Debug, Default)]
pub(crate) struct RestoreOnce {
    started: AtomicBool,
    /// The restore finished (or there was nothing to restore): the registry
    /// holds every live local launch. A wake resolves only after it (SW-AGR-2).
    done: AtomicBool,
    /// The last restore stopped short (it ran out of passes): until a later
    /// layout read restores again, whether a launch runs cannot be told.
    gave_up: AtomicBool,
    /// The last restore pass was released (it stopped short, paused or gave up) and
    /// none runs now; cleared when a later layout read starts one again. Only the
    /// inbox's caller gate reads it ([`Self::caller_gate_settled`]); a wake still
    /// reads a paused restore as pending.
    released: AtomicBool,
}

impl RestoreOnce {
    /// Claim the restore; `false` while one runs or once it settled.
    fn begin(&self) -> bool {
        let claimed = !self.started.swap(true, Ordering::SeqCst);
        if claimed {
            self.gave_up.store(false, Ordering::SeqCst);
            self.released.store(false, Ordering::SeqCst);
        }
        claimed
    }

    /// Nothing more to restore in this process.
    fn settle(&self) {
        self.started.store(true, Ordering::SeqCst);
        self.done.store(true, Ordering::SeqCst);
    }

    /// Where the restore stands (SW-AGR-2).
    pub(crate) fn state(&self) -> crate::routines::wake::Restore {
        use crate::routines::wake::Restore;
        if self.done.load(Ordering::SeqCst) {
            Restore::Settled
        } else if self.gave_up.load(Ordering::SeqCst) {
            Restore::GaveUp
        } else {
            Restore::Pending
        }
    }

    /// Whether the inbox may take a connection-implied tab the session list does not
    /// know for the human: once the restore settled or gave up, and while a released
    /// restore waits for a later layout read (nothing is restoring, so waiting would
    /// refuse that tab indefinitely). `false` before the first restore starts, so a
    /// fresh boot never records a not-yet-restored agent as the human, and while a
    /// restore pass runs.
    pub(crate) fn caller_gate_settled(&self) -> bool {
        self.state() != crate::routines::wake::Restore::Pending
            || self.released.load(Ordering::SeqCst)
    }

    /// The restore ran out of passes.
    fn give_up(&self) {
        self.gave_up.store(true, Ordering::SeqCst);
        self.release();
    }

    /// The restore stopped short: a later layout read may start it again.
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.started.store(false, Ordering::SeqCst);
    }
}

/// The passes a restore gets before it stops and waits for a later layout read.
const MAX_RESTORE_PASSES: u32 = 8;

/// The wait after restore pass `pass` (1-based) before retrying what it deferred.
fn retry_delay(pass: u32) -> std::time::Duration {
    std::time::Duration::from_millis((500u64 << pass.min(6)).min(30_000))
}

/// The local session server's layout among a layout read's replies.
fn local_sidecar_layout(
    replies: &[crate::commands::machines::RuntimeLayoutReply],
) -> Option<&LayoutSnapshot> {
    use crate::commands::machines::{RuntimeLayoutReply, RuntimeMemberSlot, LOCAL_MACHINE_ID};
    replies.iter().find_map(|reply| match reply {
        RuntimeLayoutReply::Live {
            target, snapshot, ..
        } if target.member.machine_id == LOCAL_MACHINE_ID
            && target.member.slot == RuntimeMemberSlot::Sidecar =>
        {
            Some(&**snapshot)
        }
        _ => None,
    })
}

/// After the webview read every host's layout: restore the launches the local session
/// server's live tabs recorded, when this app process did not start that server itself.
/// Runs in the background, retrying with backoff what it had to defer.
pub(crate) fn spawn_local_launch_restore(
    app: &tauri::AppHandle,
    replies: &[crate::commands::machines::RuntimeLayoutReply],
) {
    let launches = app.state::<DashboardLaunchState>();
    let Some(server) = app.try_state::<crate::terminal::session_server::SessionServerState>()
    else {
        // No session server on this Mac: no launch outlived the app.
        launches.local_restore.settle();
        return;
    };
    if server.launched_sidecar() {
        // A server this process started holds nothing from before it.
        launches.local_restore.settle();
        return;
    }
    let Some(snapshot) = local_sidecar_layout(replies) else {
        // No live layout of this Mac's session server: nothing it holds can be
        // restored now. Settled; a wake does not launch over it either, since
        // it treats an unreadable session server as unseen (SW-AGR-2).
        launches.local_restore.settle();
        return;
    };
    if !launches.local_restore.begin() {
        return;
    }
    let snapshot = snapshot.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let host = AppRestoreHost { app: app.clone() };
        let mut snapshot = snapshot;
        for pass in 1..=MAX_RESTORE_PASSES {
            if restore_local_launches(&host, &snapshot).await {
                app.state::<DashboardLaunchState>().local_restore.settle();
                // SV-7: the restored launches are this app's: claim them.
                let claim_app = app.clone();
                let _ = tauri::async_runtime::spawn_blocking(move || {
                    crate::commands::pty_backend::claim_held_sessions(&claim_app, true)
                })
                .await;
                return;
            }
            if pass == MAX_RESTORE_PASSES {
                break;
            }
            tokio::time::sleep(retry_delay(pass)).await;
            // Read under the webview's boot again, so the read never prunes what it shows.
            let read_app = app.clone();
            let replies = await_blocking("dashboard_launch.restore_layout", move || {
                crate::commands::pty_layout::member_layout_snapshots_blocking(&read_app)
                    .unwrap_or_else(|| Err("no webview layout read yet".into()))
            })
            .await;
            match replies.as_deref().map(local_sidecar_layout) {
                Ok(Some(next)) => snapshot = next.clone(),
                Ok(None) => {
                    log::warn!(target: "agents", "local launch restore paused: the session server's layout is not live; a later layout read resumes it");
                    app.state::<DashboardLaunchState>().local_restore.release();
                    return;
                }
                Err(error) => {
                    log::warn!(target: "agents", "local launch restore paused: {error}; a later layout read resumes it");
                    app.state::<DashboardLaunchState>().local_restore.release();
                    return;
                }
            }
        }
        log::warn!(target: "agents", "local launch restore stopped after {MAX_RESTORE_PASSES} passes; a later layout read retries it");
        app.state::<DashboardLaunchState>().local_restore.give_up();
    });
}

/// What a restore needs from the app around the registry.
#[allow(async_fn_in_trait)]
pub(crate) trait RestoreHost {
    fn state(&self) -> &DashboardLaunchState;
    /// The account signed in now (`None` signed out); `Err` while it cannot be read yet.
    async fn signed_in(&self) -> Result<Option<String>, String>;
    /// Check a recorded agent revision the way a bind does.
    async fn check_revision(&self, label: &str) -> RestoredRevision;
    /// Whether `binary` runs in `session`'s process tree now; `Err` when unknown.
    async fn agent_alive(&self, session: &str, binary: &str) -> Result<bool, String>;
    /// Claim the session for its runtime and restore `launch` under that claim.
    async fn commit_restore(
        &self,
        launch: &RestoredLaunch,
        agent_revision: Option<String>,
    ) -> Result<bool, String>;
    /// The registry changed.
    fn announce(&self);
}

struct AppRestoreHost {
    app: tauri::AppHandle,
}

impl RestoreHost for AppRestoreHost {
    fn state(&self) -> &DashboardLaunchState {
        self.app.state::<DashboardLaunchState>().inner()
    }

    async fn signed_in(&self) -> Result<Option<String>, String> {
        let ctx = crate::commands::route_to_context::derive_active_context(&self.app).await?;
        Ok(Some(ctx.principal().as_str().to_string()))
    }

    async fn check_revision(&self, label: &str) -> RestoredRevision {
        let ctx = match crate::commands::route_to_context::derive_active_context(&self.app).await {
            Ok(ctx) => ctx,
            Err(error) => return RestoredRevision::Transient(error),
        };
        let (home, principal, label) = (
            ctx.codesense_home.clone(),
            ctx.principal(),
            label.to_string(),
        );
        await_blocking("dashboard_launch.restore_revision", move || {
            Ok(crate::commands::agents_launch::restored_agent_revision(
                &home, &principal, &label,
            ))
        })
        .await
        .unwrap_or_else(RestoredRevision::Transient)
    }

    async fn agent_alive(&self, session: &str, binary: &str) -> Result<bool, String> {
        let (app, session, binary) = (self.app.clone(), session.to_string(), binary.to_string());
        await_blocking("dashboard_launch.restore_probe", move || {
            crate::commands::pty_backend::with_session_reference(
                &app,
                &crate::commands::machines::RuntimeSessionReference::Legacy(session),
                false,
                |host, session| crate::commands::pty::named_agent_alive(host, session, &binary),
            )
        })
        .await
    }

    async fn commit_restore(
        &self,
        launch: &RestoredLaunch,
        agent_revision: Option<String>,
    ) -> Result<bool, String> {
        let (claim_app, session) = (self.app.clone(), launch.session.clone());
        let claim = await_blocking("dashboard_launch.restore_claim", move || {
            crate::commands::pty_backend::capture_local_effect_claim(&claim_app, &session)
        })
        .await?;
        crate::commands::pty_backend::commit_local_effect_claim(&self.app, &claim, || {
            Ok(self
                .state()
                .restore_launch(launch, Some(claim.clone()), agent_revision))
        })
    }

    fn announce(&self) {
        emit_session_state_changed(&self.app);
    }
}

/// Restore every launch `snapshot`'s live tabs recorded and the registry lacks:
/// - a launch tagged with another account than the one signed in now is skipped;
/// - a launch that named an agent is restored only when that revision verifies for the
///   account, as a bind verifies one, and that agent's CLI still runs in the tab — a tab
///   whose agent exited stays unregistered, so Start launches afresh. SW-WA-F10:
///   for an SW-WA-F10 launched session (argv-direct, agent as PID-1 of PTY) the
///   agent's exit ends the tab; for a mixed-mode shell tab (user typed `claude`
///   by hand) the agent's exit still returns to zsh, which is likewise not a
///   restorable launched tab.
///
/// What cannot be decided yet (the account, the agent's files, the process probe, the
/// session's runtime claim) is deferred. `true` when nothing was deferred.
pub(crate) async fn restore_local_launches<H: RestoreHost>(
    host: &H,
    snapshot: &LayoutSnapshot,
) -> bool {
    let launches: Vec<RestoredLaunch> = restorable_launches(snapshot)
        .into_iter()
        .filter(|launch| host.state().restorable(launch))
        .collect();
    if launches.is_empty() {
        return true;
    }
    let signed_in = match host.signed_in().await {
        Ok(principal) => principal,
        Err(error) => {
            log::warn!(target: "agents", "restoring local launches deferred: {error}");
            return false;
        }
    };
    let mut settled = true;
    let mut changed = false;
    for launch in &launches {
        if launch.principal.is_some() && launch.principal != signed_in {
            log::info!(target: "agents", "launch {} not restored: private owner changed", launch.handle);
            continue;
        }
        let agent_revision = match &launch.agent_revision {
            None => None,
            Some(label) => match host.check_revision(label).await {
                RestoredRevision::Verified { revision, binary } => {
                    match host.agent_alive(&launch.session, &binary).await {
                        Ok(true) => Some(revision),
                        Ok(false) => {
                            log::info!(target: "agents", "launch {} not restored: {binary} no longer runs in its tab", launch.handle);
                            continue;
                        }
                        Err(error) => {
                            log::warn!(target: "agents", "launch {} deferred: {error}", launch.handle);
                            settled = false;
                            continue;
                        }
                    }
                }
                RestoredRevision::Rejected(error) => {
                    log::info!(target: "agents", "launch {} not restored: {error}", launch.handle);
                    continue;
                }
                RestoredRevision::Transient(error) => {
                    log::warn!(target: "agents", "launch {} deferred: {error}", launch.handle);
                    settled = false;
                    continue;
                }
            },
        };
        match host.commit_restore(launch, agent_revision.clone()).await {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                // The session's runtime ownership may not be known yet.
                log::warn!(target: "agents", "launch {} deferred: {error}", launch.handle);
                settled = false;
                continue;
            }
        }
        changed = true;
        log::info!(
            target: "agents",
            "restored launch {} on session {} (agent {:?})",
            launch.handle,
            launch.session,
            agent_revision,
        );
    }
    if changed {
        host.announce();
    }
    settled
}

#[cfg(test)]
mod tests;
