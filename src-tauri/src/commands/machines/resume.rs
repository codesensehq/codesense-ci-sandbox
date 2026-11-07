// SPDX-License-Identifier: MIT
//! SW-AGR-19 PR-6: opt-in resume after a terminal server update. A tab whose
//! saved Agent has `resumeAfterRestart` on gets a resume brief in its restore record, built while the old server still
//! answers; after the update it is relaunched as that same Agent, in its
//! workspace and tab, with the brief as its first prompt. If the relaunch is
//! refused, the tab reopens as a shell as every other tab does, and the
//! summary says why (a refusal that comes after the reopen budget opens the
//! shell then and says so in the Updates lane). Never a CLI session id: the
//! brief is CodeSense's own record, the same for every runtime.
//!
//! A routine's durable launch relaunches durably, in its prepared workspace,
//! as a new session: the routine run it was doing ended with the old server
//! (its run settles as any run whose session ended), and the brief names it.

use std::collections::BTreeMap;
use std::path::Path;

use codesense_pty_core::git_prompt::GitPromptStatus;

use super::restore::{reopened_from, Reopened, RestoreManifest, RestoreTab};
use super::resume_brief::{resume_brief, wants_resume, BriefMessage, ResumeInputs, INBOX_LINES};
use super::RuntimeMemberTarget;

/// The terminal's tail read for the brief, in lines.
const TRANSCRIPT_READ_LINES: u32 = 200;
/// How long building every brief may take together, after the person
/// confirmed: past it, the tabs not yet briefed reopen as shells.
const BRIEFS_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
/// A server slower than this to answer one tab's reads is not asked again.
const SLOW_READ: std::time::Duration = std::time::Duration::from_millis(1500);

/// The checkout's state as one line.
pub fn git_line(git: &GitPromptStatus) -> String {
    let mut parts = Vec::new();
    match (&git.branch, &git.sha) {
        (Some(branch), _) if !git.detached => parts.push(format!("branch {branch}")),
        (_, Some(sha)) => parts.push(format!("detached at {}", &sha[..sha.len().min(12)])),
        _ => parts.push("no commits yet".to_string()),
    }
    if let Some(upstream) = &git.upstream {
        let mut tracking = format!("tracking {upstream}");
        if git.ahead > 0 {
            tracking.push_str(&format!(", {} ahead", git.ahead));
        }
        if git.behind > 0 {
            tracking.push_str(&format!(", {} behind", git.behind));
        }
        parts.push(tracking);
    }
    for (count, what) in [
        (git.staged, "staged"),
        (git.modified, "modified"),
        (git.untracked, "untracked"),
        (git.conflicted, "conflicted"),
    ] {
        if count > 0 {
            parts.push(format!("{count} {what}"));
        }
    }
    if git.staged + git.modified + git.untracked + git.conflicted == 0 {
        parts.push("clean".to_string());
    }
    parts.join(", ")
}

/// A `signal_progress` as one line.
pub fn progress_line(progress: &crate::dashboard_stream::hub::SessionProgress) -> String {
    let mut line = progress.phase.clone();
    if let (Some(step), Some(total)) = (progress.step, progress.total) {
        line.push_str(&format!(" {step}/{total}"));
    }
    if let Some(note) = progress
        .note
        .as_deref()
        .filter(|note| !note.trim().is_empty())
    {
        line.push_str(&format!(": {note}"));
    }
    line
}

/// A `signal_done` as one line.
pub fn done_line(status: &str, summary: Option<&str>) -> String {
    match summary.filter(|summary| !summary.trim().is_empty()) {
        Some(summary) => format!("{status}: {summary}"),
        None => status.to_string(),
    }
}

/// The saved Agent a tab runs, by key: its remote launch's, or its local
/// launch's revision without the `@revision`. A CLI launched as itself has
/// none, and is not resumed.
pub fn agent_key_of(tags: &BTreeMap<String, String>) -> Option<String> {
    if let Some(key) = tags.get("remoteAgentKey") {
        return Some(key.clone());
    }
    tags.get("launchAgentRevision")
        .map(|revision| revision.split('@').next().unwrap_or(revision).to_string())
}

/// The story `tab`'s launch worked on: its launch tag, else its workspace's story.
fn story_of(tab: &RestoreTab) -> Option<String> {
    let blank = |value: &String| str::is_empty(str::trim(value));
    tab.tags
        .get(crate::commands::dashboard_launch::TAG_LAUNCH_STORY)
        .filter(|story| !blank(story))
        .or_else(|| {
            tab.descriptor
                .as_ref()
                .and_then(|descriptor| descriptor.story_id.as_ref())
                .filter(|story| !blank(story))
        })
        .map(|story| str::trim(story).to_owned())
}

/// The request that relaunches `tab`'s saved Agent on `machine_id`, in its
/// prepared workspace and folder, under its name, with `brief` as its first
/// prompt and without taking focus.
pub fn resume_request(
    machine_id: &str,
    tab: &RestoreTab,
    brief: &str,
) -> Result<crate::commands::pty_layout::RemoteShellTabRequest, String> {
    let agent = tab
        .tags
        .get("remoteAgentKey")
        .ok_or("it was not a saved Agent's launch")?;
    let workspace = tab
        .tags
        .get("remotePreparedWorkspace")
        .ok_or("its prepared workspace is unknown")?;
    // The receipt gate checks the launch's folder against the prepared
    // workspace's exactly: the workspace's own folder, never where the shell
    // had wandered.
    let cwd = tab
        .descriptor
        .as_ref()
        .and_then(|descriptor| descriptor.worktree_path.as_ref())
        .map(|path| path.to_string_lossy().into_owned())
        .ok_or("its workspace folder is unknown")?;
    // A durable launch (a routine's, from its app-only tag) homes from its
    // receipt again, as `remote_shell::recover` re-homes it.
    let durable = tab
        .tags
        .get(crate::commands::pty_layout::TAG_REMOTE_DURABLE)
        .map(String::as_str)
        == Some("1");
    Ok(crate::commands::pty_layout::RemoteShellTabRequest::Create {
        request_id: uuid::Uuid::new_v4(),
        machine_id: machine_id.to_string(),
        team_key: tab.team_key.clone(),
        workspace_id: tab.workspace_id.clone(),
        cwd: Some(cwd),
        name: Some(tab.name.clone()),
        launch: None,
        // A saved Agent declares its own permissions.
        permission_mode: Default::default(),
        agent_key: Some(agent.clone()),
        prepared_workspace_handle: Some(workspace.clone()),
        headed: false,
        initial_prompt: Some(brief.to_string()),
        identity: Box::new(crate::commands::pty_layout::RemoteLaunchIdentity {
            launched_by: Some("CodeSense restore".to_string()),
            role: tab.role.clone(),
            // A restore keeps its story (SV-CDE-83 I1).
            story: story_of(tab),
            durable_workspace: durable,
            ..Default::default()
        }),
    })
}

/// What relaunches `tab`'s saved Agent on This Mac.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRelaunch {
    pub agent: String,
    pub handle: String,
    pub cwd: String,
    pub org: String,
    pub team: String,
    /// The story the launch worked on, kept by the relaunch.
    pub story: Option<String>,
    /// The session the relaunch continues: its task links `relates_to:continues` to that
    /// session's and keeps its parent.
    pub continues: Option<String>,
}

/// `tab`'s local launch, from its launch tags.
pub fn local_relaunch_of(tab: &RestoreTab) -> Result<LocalRelaunch, String> {
    let tag = |name: &str| {
        tab.tags
            .get(name)
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .ok_or_else(|| format!("its launch did not record {name}"))
    };
    let agent = agent_key_of(&tab.tags).ok_or("it was not a saved Agent's launch")?;
    let team = tag("launchTeam")?;
    let (org, team) = team
        .split_once(':')
        .map(|(org, team)| (org.to_string(), team.to_string()))
        .ok_or_else(|| format!("its team {team:?} is not org:team"))?;
    Ok(LocalRelaunch {
        agent,
        handle: tag("launchHandle")?,
        cwd: tag("launchCwd")?,
        org,
        team,
        story: story_of(tab),
        continues: Some(tab.session.clone()).filter(|session| !str::is_empty(str::trim(session))),
    })
}

/// Build the brief for every tab of `manifest` that resumes, while the old
/// server (`target`, `None` for a frozen one) still answers — within
/// [`BRIEFS_DEADLINE`] altogether, so a hung server never holds up the
/// update the person confirmed.
pub(crate) async fn attach_briefs(
    app: &tauri::AppHandle,
    machine_id: &str,
    target: Option<&RuntimeMemberTarget>,
    manifest: &mut RestoreManifest,
) {
    let mut briefed = manifest.clone();
    if tokio::time::timeout(
        BRIEFS_DEADLINE,
        attach_briefs_now(app, machine_id, target, &mut briefed),
    )
    .await
    .is_ok()
    {
        *manifest = briefed;
    } else {
        log::warn!(target: "machines", "resume briefs for {machine_id} took too long; those tabs reopen as shells");
    }
}

async fn attach_briefs_now(
    app: &tauri::AppHandle,
    machine_id: &str,
    target: Option<&RuntimeMemberTarget>,
    manifest: &mut RestoreManifest,
) {
    use tauri::Manager;
    if manifest
        .tabs
        .iter()
        .all(|tab| agent_key_of(&tab.tags).is_none())
    {
        return;
    }
    let context = crate::commands::route_to_context::derive_active_context(app)
        .await
        .ok();
    let machine = super::label_of(app, machine_id).await;
    let hub = app
        .try_state::<crate::dashboard_stream::DashboardStreamHub>()
        .map(|hub| hub.inner().clone());
    // Each saved Agent's setting, read once.
    let mut settings: std::collections::BTreeMap<String, Option<bool>> = Default::default();
    for key in manifest
        .tabs
        .iter()
        .filter_map(|tab| agent_key_of(&tab.tags))
    {
        if settings.contains_key(&key) {
            continue;
        }
        let setting = match &context {
            Some(context) => {
                let (home, principal, read) = (
                    context.codesense_home.clone(),
                    context.principal(),
                    key.clone(),
                );
                crate::commands::await_blocking("resume setting", move || {
                    Ok(crate::agents::AgentKey::parse(&read).and_then(|key| {
                        crate::agents::service::get(&home, &principal, &key)
                            .ok()
                            .map(|detail| detail.agent.resume_after_restart)
                    }))
                })
                .await
                .ok()
                .flatten()
            }
            None => None,
        };
        settings.insert(key, setting);
    }
    let briefed = tabs_to_brief(&manifest.tabs, |key| settings.get(key).copied().flatten());
    let mut answering = true;
    for index in briefed {
        let tab = &mut manifest.tabs[index];
        // A server that stopped answering is not asked again for later tabs.
        let asked = std::time::Instant::now();
        let reads = if answering { target } else { None };
        let inputs = gather(app, &machine, reads, context.as_ref(), hub.as_ref(), tab).await;
        answering = answering && asked.elapsed() < SLOW_READ;
        tab.resume = Some(resume_brief(&inputs));
    }
}

/// Which tabs get a brief: those whose saved Agent opts in (`setting_of` its
/// key; `None`, unreadable, opts out). A CLI launched as itself never does.
pub fn tabs_to_brief(tabs: &[RestoreTab], setting_of: impl Fn(&str) -> Option<bool>) -> Vec<usize> {
    tabs.iter()
        .enumerate()
        .filter(|(_, tab)| {
            agent_key_of(&tab.tags).is_some_and(|key| wants_resume(setting_of(&key)))
        })
        .map(|(index, _)| index)
        .collect()
}

/// What the brief says for `tab`, read from CodeSense's own records.
async fn gather(
    app: &tauri::AppHandle,
    machine: &str,
    target: Option<&RuntimeMemberTarget>,
    context: Option<&crate::commands::route_to_context::ActiveContext>,
    hub: Option<&crate::dashboard_stream::DashboardStreamHub>,
    tab: &RestoreTab,
) -> ResumeInputs {
    use super::{RuntimeSessionReference, RuntimeSessionTarget};
    let session = tab.session.clone();
    let (git, transcript) = match target {
        Some(target) if !session.is_empty() => {
            let app = app.clone();
            let reference = RuntimeSessionReference::Admitted(Box::new(RuntimeSessionTarget {
                runtime: target.clone(),
                session: codesense_pty_core::domain::PtySessionId::new(session.clone()),
            }));
            crate::commands::await_blocking("resume brief", move || {
                let git = crate::commands::pty_backend::session_prompt(&app, &reference)
                    .ok()
                    .and_then(|prompt| prompt.git)
                    .map(|git| git_line(&git));
                let transcript = crate::commands::pty_backend::with_session_reference(
                    &app,
                    &reference,
                    true,
                    |domain, session| {
                        codesense_pty_core::domain::visible_text(
                            domain,
                            session,
                            TRANSCRIPT_READ_LINES,
                        )
                        .map_err(String::from)
                    },
                )
                .map(|text| text.text)
                .unwrap_or_default();
                Ok((git, transcript))
            })
            .await
            .unwrap_or_default()
        }
        _ => (None, String::new()),
    };
    let routine = match context {
        Some(context) if !session.is_empty() => {
            let home = context.codesense_home.clone();
            let session = session.clone();
            crate::commands::await_blocking("resume routine", move || {
                Ok(
                    crate::routines::service::context_for_session(&home, &session)
                        .ok()
                        .map(|run| {
                            let mut line = format!("{}, run {}", run.routine_name, run.run_id);
                            if let Some(last) =
                                run.last_run_summary.filter(|s| !s.trim().is_empty())
                            {
                                line.push_str(&format!("; its last run: {last}"));
                            }
                            line
                        }),
                )
            })
            .await
            .ok()
            .flatten()
        }
        _ => None,
    };
    ResumeInputs {
        agent: agent_key_of(&tab.tags).unwrap_or_default(),
        tab: tab.name.clone(),
        workspace: tab.workspace_name.clone(),
        machine: machine.to_string(),
        folder: tab.cwd.clone(),
        git,
        progress: hub
            .and_then(|hub| hub.latest_progress(&session))
            .map(|progress| progress_line(&progress)),
        done: hub
            .and_then(|hub| hub.latest_done(&session))
            .map(|(status, summary)| done_line(&status, summary.as_deref())),
        routine,
        inbox: inbox_of(app, tab).await,
        transcript,
    }
}

/// The tab's conversation, newest first: what was said to it and by it.
async fn inbox_of(app: &tauri::AppHandle, tab: &RestoreTab) -> Vec<BriefMessage> {
    let Some((org, team)) = tab.team_key.split_once(':') else {
        return Vec::new();
    };
    let team = crate::commands::dashboard_launch::LaunchTeam {
        org: org.to_string(),
        team: team.to_string(),
    };
    let Ok(resolution) =
        crate::commands::route_to_context::derive_context_for(app, &team.org, &team.team).await
    else {
        return Vec::new();
    };
    let Ok(path) = crate::commands::agent_inbox::team_store_path(resolution, &team) else {
        return Vec::new();
    };
    let addresses: Vec<String> = [
        tab.tags.get("launchHandle").cloned(),
        tab.tags.get("remoteHandle").cloned(),
        Some(tab.session.clone()),
    ]
    .into_iter()
    .flatten()
    .filter(|address| !address.is_empty())
    .collect();
    crate::commands::await_blocking("resume inbox", move || {
        Ok(brief_messages(
            crate::weft::WeftStore::open(path)
                .ok()
                .and_then(|store| {
                    crate::weft::inbox::inbox_list_for_session(&store, &addresses, &[]).ok()
                })
                .unwrap_or_default(),
            &addresses,
        ))
    })
    .await
    .unwrap_or_default()
}

/// A conversation (newest first) as the brief lists it: withdrawn rows left
/// out; unread where it was said to the tab and not yet read, listed first.
pub fn brief_messages(
    messages: Vec<crate::weft::inbox::InboxMessage>,
    addresses: &[String],
) -> Vec<BriefMessage> {
    let listed: Vec<BriefMessage> = messages
        .into_iter()
        .filter(|message| message.withdrawn_at_ms.is_none())
        .map(|message| BriefMessage {
            unread: message.read_at_ms.is_none() && addresses.contains(&message.to),
            from: message.from,
            text: message.text,
        })
        .collect();
    // Unread first (newest first), then the most recent of the rest.
    let (unread, read): (Vec<_>, Vec<_>) = listed.into_iter().partition(|message| message.unread);
    unread.into_iter().chain(read).take(INBOX_LINES).collect()
}

/// The Agent still opts in now (a person's "Reopen N tabs" replays briefs
/// recorded earlier; the setting may have been turned off since).
async fn still_resumes(app: &tauri::AppHandle, agent: &str) -> Result<(), String> {
    let context = crate::commands::route_to_context::derive_active_context(app).await?;
    let (home, principal, agent) = (
        context.codesense_home.clone(),
        context.principal(),
        agent.to_string(),
    );
    let setting = crate::commands::await_blocking("resume setting", move || {
        Ok(crate::agents::AgentKey::parse(&agent).and_then(|key| {
            crate::agents::service::get(&home, &principal, &key)
                .ok()
                .map(|detail| detail.agent.resume_after_restart)
        }))
    })
    .await?;
    if wants_resume(setting) {
        Ok(())
    } else {
        Err("its Agent no longer resumes after an update".to_string())
    }
}

/// How a relaunch's launch result reads: started, still starting (not yet
/// confirmed), or refused (why; a shell reopens instead).
pub fn resume_outcome(status: &str, message: Option<String>) -> Result<Reopened, String> {
    match reopened_from(status, message) {
        Reopened::Yes => Ok(Reopened::Resumed),
        Reopened::No(why) => Err(why),
        _ => Ok(Reopened::ResumeStarting),
    }
}

/// Run `relaunch` on its own task and wait at most `budget` for it: a slow
/// one keeps going (it is never cancelled half-way, which would leave its
/// launch reserved) and reads as still starting.
async fn within_budget<Late>(
    budget: std::time::Duration,
    relaunch: impl std::future::Future<Output = Result<Reopened, String>> + Send + 'static,
    late_refusal: impl FnOnce(String) -> Late + Send + 'static,
) -> Result<Reopened, String>
where
    Late: std::future::Future<Output = ()> + Send + 'static,
{
    // The relaunch hands its result over; if this side gave up waiting (the
    // receiver is gone), a refusal is handled there: race-free.
    let (result_tx, result_rx) = tokio::sync::oneshot::channel::<Result<Reopened, String>>();
    tokio::spawn(async move {
        let result = relaunch.await;
        if let Err(Err(why)) = result_tx.send(result) {
            log::warn!(target: "machines", "a relaunch still starting was refused: {why}");
            late_refusal(why).await;
        }
    });
    let mut result_rx = result_rx;
    tokio::select! {
        answered = &mut result_rx => answered
            .unwrap_or_else(|_| Err("its relaunch stopped before it answered".to_string())),
        () = tokio::time::sleep(budget) => {
            // Closed first, then read: an answer that made it in is used here;
            // any later one finds the channel closed and is handled there.
            result_rx.close();
            match result_rx.try_recv() {
                Ok(result) => result,
                Err(_) => Ok(Reopened::ResumeStarting),
            }
        }
    }
}

/// Relaunch `tab`'s saved Agent on `machine_id` with its brief, within
/// `budget`.
pub(crate) async fn relaunch_remote(
    app: &tauri::AppHandle,
    machine_id: &str,
    tab: &RestoreTab,
    brief: &str,
    budget: std::time::Duration,
) -> Result<Reopened, String> {
    let request = resume_request(machine_id, tab, brief)?;
    let (late_app, late_machine, late_tab) = (app.clone(), machine_id.to_string(), tab.clone());
    let app = app.clone();
    let agent = agent_key_of(&tab.tags).unwrap_or_default();
    within_budget(
        budget,
        async move {
            still_resumes(&app, &agent).await?;
            let outcome =
                crate::commands::pty_layout::remote_shell_tab_bound(app, request, None).await;
            resume_outcome(outcome.status, outcome.message)
        },
        move |why| async move {
            super::restore::reopen_shell_late(&late_app, &late_machine, late_tab, why).await
        },
    )
    .await
}

/// Relaunch `tab`'s saved Agent on This Mac with its brief, within `budget`.
pub(crate) async fn relaunch_local(
    app: &tauri::AppHandle,
    tab: &RestoreTab,
    brief: &str,
    budget: std::time::Duration,
) -> Result<Reopened, String> {
    let relaunch = local_relaunch_of(tab)?;
    let (late_app, late_tab) = (app.clone(), tab.clone());
    let (app, tab, brief) = (app.clone(), tab.clone(), brief.to_string());
    within_budget(
        budget,
        async move { relaunch_local_now(&app, &tab, &brief, relaunch).await },
        move |why| async move {
            super::restore::reopen_shell_late(&late_app, super::LOCAL_MACHINE_ID, late_tab, why)
                .await
        },
    )
    .await
}

/// The launch record a local relaunch of `tab` is made with. A restore keeps its story and
/// team and continues its session's task (SV-CDE-83 I1); `launched_by` stays
/// "CodeSense restore".
pub(crate) fn restore_record(
    tab: &RestoreTab,
    relaunch: &LocalRelaunch,
) -> crate::commands::dashboard_launch::LaunchRecord {
    use crate::commands::dashboard_launch::{LaunchMeta, LaunchRecord, LaunchTeam};
    LaunchRecord {
        meta: LaunchMeta {
            display_name: Some(tab.name.clone()),
            launched_by: Some("CodeSense restore".to_string()),
            team: Some(LaunchTeam {
                org: relaunch.org.clone(),
                team: relaunch.team.clone(),
            }),
            role: tab.role.clone(),
            story: relaunch.story.clone(),
            continues: relaunch.continues.clone(),
            ..Default::default()
        },
        started_with: None,
        // a resume restores the launch without re-applying
        // a budget. The deadline sweep rebuilds the deadline from the
        // launch tag (`launchDeadlineAt`), not from this record.
        max_runtime_ms: None,
    }
}

/// The local relaunch itself (prepare, then launch), on its own task.
async fn relaunch_local_now(
    app: &tauri::AppHandle,
    tab: &RestoreTab,
    brief: &str,
    relaunch: LocalRelaunch,
) -> Result<Reopened, String> {
    use crate::commands::mcp_workspace::{
        launch_managed_dir_session, LaunchHome, ManagedLaunchOptions, SessionView,
    };
    let context = crate::commands::route_to_context::derive_active_context(app).await?;
    let home = context.codesense_home.clone();
    let principal = context.principal();
    // Launched by someone else (another profile): never relaunched as them.
    if let Some(launched_as) = tab
        .tags
        .get(crate::commands::dashboard_launch::TAG_LAUNCH_PRINCIPAL)
        .filter(|launched_as| !launched_as.is_empty())
    {
        if launched_as != principal.as_str() {
            return Err("it was launched by another CodeSense user".to_string());
        }
    }
    still_resumes(app, &relaunch.agent).await?;
    // Its launch folder must still be a managed one on disk.
    let cwd = Path::new(&relaunch.cwd).to_path_buf();
    if !crate::commands::mcp_workspace::managed_launch_dir(&cwd, &home) || !cwd.is_dir() {
        return Err("its launch folder is gone or not managed by CodeSense".to_string());
    }
    let key = crate::agents::AgentKey::parse(&relaunch.agent)
        .ok_or_else(|| format!("{:?} is not an agent key", relaunch.agent))?;
    let (prepare_home, prepare_brief, target) = (
        home.clone(),
        brief.to_string(),
        (relaunch.org.clone(), relaunch.team.clone()),
    );
    let agent = crate::commands::await_blocking("resume prepare", move || {
        crate::agents::launch::prepare(
            &prepare_home,
            &principal,
            &key,
            crate::agents::launch::PrepareOptions {
                target_team: Some(target),
                prompt: Some(&prepare_brief),
                ..Default::default()
            },
        )
    })
    .await?;
    let form = tab
        .workspace_key
        .split_once('|')
        .map(|(_, form)| form.to_string())
        .unwrap_or_else(|| relaunch.handle.clone());
    let mut launch_home = LaunchHome::new(
        &relaunch.org,
        Some(&relaunch.team),
        &form,
        "agent",
        &cwd,
        if tab.background {
            SessionView::Background
        } else {
            SessionView::Tab { focus: false }
        },
    );
    // Back in the workspace it ran in, as the layout keyed it.
    if !tab.workspace_key.is_empty() {
        launch_home.workspace = crate::commands::pty_layout::workspace_key(&tab.team_key, &form);
    }
    if let Some(descriptor) = tab.descriptor.clone() {
        launch_home.descriptor = descriptor;
    }
    let launched = launch_managed_dir_session(
        app,
        &relaunch.handle,
        &cwd,
        &home,
        Some(brief),
        ManagedLaunchOptions {
            provenance: None,
            record: restore_record(tab, &relaunch),
            extra_args: Vec::new(),
            agent,
            home: launch_home,
        },
    )
    .await;
    match launched {
        Ok(true) => Ok(Reopened::Resumed),
        Ok(false) => Err("a session already runs for it".to_string()),
        Err(why) => Err(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::machines::restore::CwdSource;

    fn tab(tags: &[(&str, &str)]) -> RestoreTab {
        RestoreTab {
            team_key: "acme:default".into(),
            workspace_id: "SW-1".into(),
            workspace_name: "SW-1 login".into(),
            name: "Triage".into(),
            cwd: Some("/srv/SW-1".into()),
            cwd_source: CwdSource::Session,
            agent: Some("acme:default/triage".into()),
            role: Some("triage".into()),
            background: false,
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            workspace_key: "acme:default|SW-1".into(),
            descriptor: Some(codesense_pty_core::layout::WorkspaceDescriptor {
                worktree_path: Some("/srv/SW-1-worktree".into()),
                ..Default::default()
            }),
            session: "s-1".into(),
            resume: None,
        }
    }

    #[test]
    fn agr19_git_state_reads_as_one_line() {
        let mut git = GitPromptStatus {
            branch: Some("main".into()),
            upstream: Some("origin/main".into()),
            ahead: 2,
            modified: 3,
            untracked: 1,
            ..Default::default()
        };
        assert_eq!(
            git_line(&git),
            "branch main, tracking origin/main, 2 ahead, 3 modified, 1 untracked"
        );
        git = GitPromptStatus {
            detached: true,
            sha: Some("0123456789abcdef0123".into()),
            ..Default::default()
        };
        assert_eq!(git_line(&git), "detached at 0123456789ab, clean");
    }

    #[test]
    fn agr19_progress_and_results_read_as_one_line() {
        let progress = crate::dashboard_stream::hub::SessionProgress {
            phase: "dev".into(),
            step: Some(2),
            total: Some(5),
            note: Some("login test".into()),
            at_ms: 0,
        };
        assert_eq!(progress_line(&progress), "dev 2/5: login test");
        assert_eq!(done_line("success", Some("merged")), "success: merged");
        assert_eq!(done_line("failed", Some("  ")), "failed");
    }

    #[test]
    fn agr19_a_remote_agent_relaunches_in_its_prepared_workspace_with_its_brief() {
        let resumable = tab(&[
            ("remoteAgentKey", "acme:default/triage"),
            ("remotePreparedWorkspace", "prep-7"),
            ("remoteDurable", "1"),
        ]);
        let crate::commands::pty_layout::RemoteShellTabRequest::Create {
            agent_key,
            prepared_workspace_handle,
            initial_prompt,
            headed,
            launch,
            name,
            cwd,
            identity,
            ..
        } = resume_request("box", &resumable, "the brief").unwrap()
        else {
            panic!("a create")
        };
        assert_eq!(agent_key.as_deref(), Some("acme:default/triage"));
        assert_eq!(prepared_workspace_handle.as_deref(), Some("prep-7"));
        assert_eq!(initial_prompt.as_deref(), Some("the brief"));
        assert!(!headed, "never takes focus");
        assert!(launch.is_none(), "the saved Agent, never a raw CLI");
        assert_eq!(name.as_deref(), Some("Triage"));
        // The prepared workspace's own folder, not where the shell was.
        assert_eq!(cwd.as_deref(), Some("/srv/SW-1-worktree"));
        assert!(
            identity.durable_workspace,
            "a routine's durable launch stays durable"
        );
        assert_eq!(identity.role.as_deref(), Some("triage"));
        let mut plain = resumable.clone();
        plain.tags.remove("remoteDurable");
        let crate::commands::pty_layout::RemoteShellTabRequest::Create { identity, .. } =
            resume_request("box", &plain, "b").unwrap()
        else {
            panic!("a create")
        };
        assert!(!identity.durable_workspace);
        let mut nowhere = resumable.clone();
        nowhere.descriptor = None;
        assert!(
            resume_request("box", &nowhere, "b").is_err(),
            "its folder is unknown"
        );
        // Without its prepared workspace it is not relaunched (a shell reopens).
        assert!(resume_request("box", &tab(&[("remoteAgentKey", "a/b")]), "b").is_err());
        assert!(resume_request("box", &tab(&[]), "b").is_err());
    }

    /// SV-CDE-83 I1, case 1 end to end below the terminal server: a story dev launch is
    /// captured; the server restarts and the restore relaunches the tab with the record
    /// [`restore_record`] makes; the new session's record, `session_read.story`, its task's story,
    /// parent and `continues` link, and `story_changes`' tasks all keep the story.
    #[tokio::test]
    async fn cde83_a_terminal_server_restart_keeps_a_story_devs_story() {
        use crate::commands::dashboard_launch::{
            compose_launch_write, DashboardLaunchState, LaunchMeta, LaunchRecord, LaunchTeam,
        };
        use crate::context_repo::caller_scope::CallerScope;
        use crate::session_usage::record::LaunchFacts;
        use crate::session_usage::task_capture::{capture_launch_task, Capture, TaskInputs};
        use crate::session_usage::UsageStore;

        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("usage.sqlite");
        let open = || UsageStore::open(&path).unwrap();
        let launch =
            |state: &DashboardLaunchState, handle: &str, session: &str, record: LaunchRecord| {
                state.set_task_store_path_for_test(path.clone());
                assert!(state.begin_with_provenance(handle, None, record).unwrap());
                state.bind(handle, session, Some("codesense")).unwrap();
                let write = compose_launch_write(
                    state,
                    session,
                    handle,
                    LaunchFacts::default().with_task(TaskInputs {
                        brief: Some("Build SV-1.".into()),
                        ..Default::default()
                    }),
                )
                .unwrap();
                let store = open();
                store.write_session_record(&write.record).unwrap();
                capture_launch_task(
                    &store,
                    &Capture {
                        session_id: session,
                        record: &write.record,
                        inputs: &write.task,
                        routine: write.routine.as_ref(),
                        launcher_task: write.launcher.as_deref(),
                        story_file: None,
                        now_ms: 10,
                    },
                )
                .unwrap()
            };

        // The story dev, as open_workspace launched it.
        let before = DashboardLaunchState::new(4);
        let dev = launch(
            &before,
            "SV-1#dev",
            "s-1",
            LaunchRecord {
                meta: LaunchMeta {
                    story: Some("SV-1".into()),
                    team: Some(LaunchTeam {
                        org: "acme".into(),
                        team: "default".into(),
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert_eq!(dev.story_id.as_deref(), Some("SV-1"));

        // The server restarted: the tab's tags carry the launch; the restore relaunches it.
        let tab = tab(&[
            ("launchAgentRevision", "acme:default/dev@4"),
            ("launchHandle", "SV-1#dev"),
            ("launchCwd", "/Users/me/.codesense/w/SV-1"),
            ("launchTeam", "acme:default"),
            ("launchStory", "SV-1"),
        ]);
        let relaunch = local_relaunch_of(&tab).unwrap();
        let after = DashboardLaunchState::new(4);
        let resumed = launch(
            &after,
            &relaunch.handle,
            "s-2",
            restore_record(&tab, &relaunch),
        );

        let record = open().read_session_record("s-2").unwrap().unwrap();
        assert_eq!(record.story_id.as_deref(), Some("SV-1"));
        assert_eq!(record.launched_by.as_deref(), Some("CodeSense restore"));
        assert_eq!(resumed.story_id.as_deref(), Some("SV-1"));
        assert_eq!(
            resumed.parent_id, dev.parent_id,
            "the relaunch keeps its parent"
        );
        assert_eq!(resumed.parent_source.as_deref(), Some("continues"));
        assert!(open()
            .task_links("s-2")
            .unwrap()
            .iter()
            .any(|l| l.kind == "relates_to:continues" && l.target == "s-1"));

        let read = crate::session_usage::session_read::read(
            &path,
            &home.path().join("session-conversations"),
            &serde_json::from_value(serde_json::json!({"sessionId": "s-2"})).unwrap(),
            &CallerScope::Human,
        )
        .await
        .unwrap();
        assert_eq!(
            read["story"]["id"], "SV-1",
            "session_read.story survives the restart"
        );

        let changes = crate::session_usage::story_changes::read(
            home.path(),
            &serde_json::from_value(serde_json::json!({"storyId": "SV-1"})).unwrap(),
            &CallerScope::Human,
        )
        .await
        .unwrap();
        let tasks: Vec<String> = changes
            .tasks
            .iter()
            .filter_map(|t| t["taskId"].as_str().map(str::to_owned))
            .collect();
        assert!(
            tasks.contains(&"s-1".to_string()) && tasks.contains(&"s-2".to_string()),
            "{tasks:?}"
        );
    }

    /// SV-CDE-83 I1, case 1 (both paths): a restore relaunch keeps the story it worked on, from
    /// its launch tag, else its workspace's; a tab with neither gets none (case 3).
    #[test]
    fn cde83_a_restore_relaunch_keeps_its_story() {
        let mut remote = tab(&[
            ("remoteAgentKey", "acme:default/triage"),
            ("remotePreparedWorkspace", "prep-7"),
        ]);
        let story = |tab: &RestoreTab| match resume_request("box", tab, "b").unwrap() {
            crate::commands::pty_layout::RemoteShellTabRequest::Create { identity, .. } => {
                identity.story
            }
            _ => panic!("a create"),
        };
        assert_eq!(story(&remote), None, "nothing is guessed");
        remote.descriptor.as_mut().unwrap().story_id = Some("SW-1".into());
        assert_eq!(story(&remote).as_deref(), Some("SW-1"));

        let mut local = tab(&[
            ("launchAgentRevision", "acme:default/triage@4"),
            ("launchHandle", "triage-1"),
            ("launchCwd", "/Users/me/.codesense/w/triage"),
            ("launchTeam", "acme:default"),
            ("launchStory", "SW-2"),
        ]);
        let relaunch = local_relaunch_of(&local).unwrap();
        assert_eq!(
            relaunch.story.as_deref(),
            Some("SW-2"),
            "the launch tag wins"
        );
        assert_eq!(relaunch.continues.as_deref(), Some("s-1"));
        local.tags.insert("launchStory".into(), " ".into());
        assert_eq!(local_relaunch_of(&local).unwrap().story, None);
        local.descriptor.as_mut().unwrap().story_id = Some("SW-3".into());
        assert_eq!(
            local_relaunch_of(&local).unwrap().story.as_deref(),
            Some("SW-3")
        );
        local.session = String::new();
        assert_eq!(local_relaunch_of(&local).unwrap().continues, None);
    }

    #[test]
    fn agr19_the_brief_lists_unread_mail_first_and_never_withdrawn_mail() {
        let message = |id: i64, to: &str, read: bool, withdrawn: bool| {
            let mut row = crate::weft::inbox::InboxMessage {
                id,
                from: format!("p{id}"),
                to: to.to_string(),
                text: format!("m{id}"),
                created_at_ms: id,
                read_at_ms: None,
                in_reply_to: None,
                in_reply_to_team: None,
                primary_file: None,
                attachments: Vec::new(),
                archived_at_ms: None,
                withdrawn_at_ms: None,
                delivery: None,
                on_click: None,
                content_type: None,
                via: None,
                role: None,
                reply_to: None,
                summary: None,
                kind: None,
            };
            row.read_at_ms = read.then_some(1);
            row.withdrawn_at_ms = withdrawn.then_some(1);
            row
        };
        // Newest first: 14 recent read rows, then an old unread one.
        let mut rows: Vec<_> = (2..16)
            .rev()
            .map(|id| message(id, "triage-1", true, false))
            .collect();
        rows.push(message(1, "triage-1", false, false));
        rows.insert(0, message(99, "triage-1", false, true));
        let listed = brief_messages(rows, &["triage-1".to_string()]);
        assert_eq!(listed.len(), INBOX_LINES);
        assert_eq!(listed[0].text, "m1", "an old unread message is kept, first");
        assert!(listed[0].unread);
        assert!(
            listed.iter().all(|m| m.text != "m99"),
            "withdrawn mail is left out"
        );
        assert_eq!(listed[1].text, "m15");
    }

    #[test]
    fn agr19_a_relaunch_still_starting_is_not_taken_for_resumed() {
        assert_eq!(resume_outcome("opened", None), Ok(Reopened::Resumed));
        assert_eq!(
            resume_outcome("pending", None),
            Ok(Reopened::ResumeStarting)
        );
        assert_eq!(
            resume_outcome("unknown", None),
            Ok(Reopened::ResumeStarting)
        );
        assert_eq!(
            resume_outcome("refused", Some("not open".into())),
            Err("not open".into())
        );
    }

    #[tokio::test]
    async fn agr19_a_slow_relaunch_keeps_going_and_reads_as_starting() {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let outcome = within_budget(
            std::time::Duration::from_millis(20),
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                let _ = done_tx.send(());
                Ok(Reopened::Resumed)
            },
            |_| async { panic!("not refused") },
        )
        .await;
        assert_eq!(outcome, Ok(Reopened::ResumeStarting));
        // Never cancelled: it finishes on its own.
        tokio::time::timeout(std::time::Duration::from_secs(2), done_rx)
            .await
            .expect("the relaunch ran to its end")
            .unwrap();
    }

    #[tokio::test]
    async fn agr19_a_refusal_after_the_budget_reopens_the_shell_then() {
        let (shell_tx, shell_rx) = tokio::sync::oneshot::channel();
        let outcome = within_budget(
            std::time::Duration::from_millis(20),
            async {
                tokio::time::sleep(std::time::Duration::from_millis(60)).await;
                Err("its workspace is not open".to_string())
            },
            move |why| async move {
                let _ = shell_tx.send(why);
            },
        )
        .await;
        assert_eq!(outcome, Ok(Reopened::ResumeStarting));
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), shell_rx)
                .await
                .unwrap()
                .unwrap(),
            "its workspace is not open"
        );
        // Refused in time: the caller opens the shell, nothing late.
        let refused = within_budget(
            std::time::Duration::from_secs(2),
            async { Err("no".to_string()) },
            |_| async { panic!("not late") },
        )
        .await;
        assert_eq!(refused, Err("no".to_string()));
    }

    #[test]
    fn agr19_only_tabs_whose_agent_opts_in_get_a_brief() {
        let tabs = vec![
            tab(&[("remoteAgentKey", "acme:default/triage")]),
            tab(&[("remoteAgentKey", "acme:default/dev")]),
            tab(&[("remoteAgentKey", "acme:default/gone")]),
            tab(&[("remoteLaunch", "\"claude\"")]),
            tab(&[]),
        ];
        let briefed = tabs_to_brief(&tabs, |key| match key {
            "acme:default/triage" => Some(true),
            "acme:default/dev" => Some(false),
            // An Agent that could not be read: no brief.
            _ => None,
        });
        assert_eq!(briefed, vec![0]);
    }

    #[test]
    fn agr19_a_mac_agent_relaunches_from_its_launch_tags() {
        let resumable = tab(&[
            ("launchAgentRevision", "acme:default/triage@4"),
            ("launchHandle", "triage-1"),
            ("launchCwd", "/Users/me/.codesense/w/triage"),
            ("launchTeam", "acme:default"),
        ]);
        assert_eq!(
            local_relaunch_of(&resumable),
            Ok(LocalRelaunch {
                agent: "acme:default/triage".into(),
                handle: "triage-1".into(),
                cwd: "/Users/me/.codesense/w/triage".into(),
                org: "acme".into(),
                team: "default".into(),
                story: None,
                continues: Some("s-1".into()),
            })
        );
        let missing = tab(&[("launchAgentRevision", "a/b@1")]);
        assert!(local_relaunch_of(&missing)
            .unwrap_err()
            .contains("launchTeam"));
        assert_eq!(
            agent_key_of(&resumable.tags).as_deref(),
            Some("acme:default/triage")
        );
        assert_eq!(agent_key_of(&tab(&[("remoteLaunch", "claude")]).tags), None);
    }
}
