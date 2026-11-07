// SPDX-License-Identifier: MIT
//! SW-AGR-19 PR-4: reopen a machine's tabs after its terminal server is
//! replaced. The layout lives in the server's memory only, so once the person
//! has confirmed, and before the old server stops, the app records a restore
//! record: each live tab's workspace, name, cwd, tags and the agent it ran.
//! The new server reopens them from that record, in memory.
//!
//! The record is also kept on disk (`<codesense home>/terminal-updates/`) for
//! one case: an update whose old server stopped but whose new one did not come
//! up (or an app quit in between). The layout is empty then, so the next
//! update's capture keeps the earlier record and reopens those tabs. Every
//! other ending removes it: a restore, or a failure that left the old server
//! and its tabs running.
//!
//! A tab reopens in its recorded cwd, else its workspace's folder, else the
//! account's home (and the summary says so). An agent's tab reopens as a
//! shell there, and the summary names the agent as ended — unless its saved
//! Agent opted in to resuming (PR-6, [`super::resume`]): then it is relaunched
//! as that Agent with a resume brief, and reopens as a shell only if that is
//! refused. While the server is stopped
//! the machine is not connected, so routines see it offline: nothing races
//! the restore.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use codesense_pty_core::domain::PtySessionId;
use codesense_pty_core::layout::{EntryState, LayoutSnapshot};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How long one tab's reopen may take before it is reported as still opening.
const REOPEN_BUDGET: Duration = Duration::from_secs(20);
/// How long all of a machine's reopens may take together; tabs not started
/// by then are reported as not reopened.
const RESTORE_DEADLINE: Duration = Duration::from_secs(60);
/// How many tabs reopen at once.
const REOPEN_CONCURRENCY: usize = 4;
/// How long This Mac keeps an unfinished update's record for the next one.
const LOCAL_RECORD_AGE_MS: u64 = 30 * 60 * 1000;
/// How long an unfinished update's tabs are offered for reopening.
const PENDING_RECORD_AGE_MS: u64 = 24 * 60 * 60 * 1000;
/// A server slower than this to say a session's folder is not asked again.
const SLOW_ANSWER: Duration = Duration::from_millis(1500);

/// The agent a tab ran, from its launch tags: a saved Agent (its key), or a
/// CLI launched as itself (`claude`, `codex`); `None` for a shell.
pub(crate) fn agent_of(tags: &BTreeMap<String, String>) -> Option<String> {
    if let Some(key) = tags.get("remoteAgentKey") {
        return Some(key.clone());
    }
    if let Some(revision) = tags.get("launchAgentRevision") {
        return Some(revision.split('@').next().unwrap_or(revision).to_string());
    }
    tags.get("remoteLaunch")
        .map(|launch| launch.trim_matches('"').to_string())
        .filter(|launch| !launch.is_empty() && launch != "shell")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreManifest {
    pub machine_id: String,
    pub captured_at_ms: u64,
    pub tabs: Vec<RestoreTab>,
    /// Set once the update stopped the old server (its tabs are gone): only
    /// such a record is ever offered or reused. A record of an attempt that
    /// never stopped anything describes tabs that still run.
    #[serde(default)]
    pub old_stopped: bool,
    /// This record's own id: a reopen forgets only the version it claimed,
    /// never a newer one a concurrent update wrote.
    #[serde(default)]
    pub capture_id: String,
}

/// Where a tab's folder came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CwdSource {
    /// The shell's own, as the old server last saw it.
    Session,
    /// The session's was unknown: its workspace's folder.
    Workspace,
    /// Neither was known: the account's home.
    Home,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreTab {
    /// The layout's team key (`{org}:{team}`) and workspace id.
    pub team_key: String,
    pub workspace_id: String,
    /// What the workspace is called, for the summary.
    pub workspace_name: String,
    pub name: String,
    pub cwd: Option<String>,
    pub cwd_source: CwdSource,
    /// The agent the tab ran, if any.
    pub agent: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    /// A tab the person hid (This Mac reopens it hidden; a remote tab reopens
    /// in the strip).
    #[serde(default)]
    pub background: bool,
    /// The tab's tags as they were (an Agent's launch tags; PR-6 reads them).
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    /// The workspace as the layout keys it (`{teamKey}|{form}`) and describes
    /// it, to reopen a This Mac tab in it (PR-5).
    #[serde(default)]
    pub workspace_key: String,
    #[serde(default)]
    pub descriptor: Option<codesense_pty_core::layout::WorkspaceDescriptor>,
    /// The session the tab ran (PR-6 reads its brief's records by it).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session: String,
    /// PR-6: the resume brief its Agent is relaunched with; `None` when it
    /// does not resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
}

/// What came back, and what did not, for the person.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreSummary {
    pub reopened: Vec<RestoredTab>,
    pub not_reopened: Vec<RestoredTab>,
    /// Whether the tabs could be recorded before the old server stopped.
    pub recorded: bool,
    /// Whether everything came back as it was (else the toast warns).
    pub complete: bool,
    /// Tabs of an earlier, unfinished update that are no longer offered.
    #[serde(skip_serializing_if = "is_zero")]
    pub earlier_dropped: usize,
    /// The one line the person reads (see [`RestoreSummary::line`]).
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoredTab {
    pub name: String,
    pub workspace: String,
    /// The agent that ran there and ended with the old server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// What is not as it was (another folder, a refusal, still opening).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// PR-6: its Agent was relaunched with a resume brief.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub resumed: bool,
}

impl RestoreSummary {
    /// Nothing could be recorded, so nothing reopened.
    pub fn unrecorded() -> Self {
        Self::default()
    }

    /// Whether everything came back as it was.
    pub fn all_back(&self) -> bool {
        self.recorded
            && self.earlier_dropped == 0
            && self.not_reopened.is_empty()
            && self
                .reopened
                .iter()
                .all(|tab| (tab.agent.is_none() || tab.resumed) && tab.note.is_none())
    }

    /// One line for the Updates lane and the toast.
    pub fn line(&self, label: &str) -> String {
        if !self.recorded {
            return format!(
                "{label}: its tabs could not be recorded before the terminal server stopped, so none were reopened."
            );
        }
        if self.reopened.is_empty() && self.not_reopened.is_empty() {
            return if self.earlier_dropped > 0 {
                format!(
                    "{label}: it had no open tabs; {} from an earlier, unfinished update were not reopened.",
                    plural(self.earlier_dropped, "tab")
                )
            } else {
                format!("{label}: it had no open tabs.")
            };
        }
        let mut line = format!("{label}: {} reopened", plural(self.reopened.len(), "tab"));
        let resumed: Vec<&str> = self
            .reopened
            .iter()
            .filter(|tab| tab.resumed)
            .filter_map(|tab| tab.agent.as_deref())
            .collect();
        if !resumed.is_empty() {
            line.push_str(&format!(
                "; agents resumed with a brief: {}",
                resumed.join(", ")
            ));
        }
        let agents: Vec<&str> = self
            .reopened
            .iter()
            .chain(&self.not_reopened)
            .filter(|tab| !tab.resumed)
            .filter_map(|tab| tab.agent.as_deref())
            .collect();
        if !agents.is_empty() {
            line.push_str(&format!(
                "; agents ended (relaunch them): {}",
                agents.join(", ")
            ));
        }
        let elsewhere: Vec<String> = self
            .reopened
            .iter()
            .filter_map(|tab| Some(format!("{} ({})", tab.name, tab.note.as_deref()?)))
            .collect();
        if !elsewhere.is_empty() {
            line.push_str(&format!(
                "; not exactly as it was: {}",
                elsewhere.join(", ")
            ));
        }
        let lost: Vec<String> = self
            .not_reopened
            .iter()
            .map(|tab| {
                format!(
                    "{} ({})",
                    tab.name,
                    tab.note.as_deref().unwrap_or("not reopened")
                )
            })
            .collect();
        if !lost.is_empty() {
            line.push_str(&format!("; not reopened: {}", lost.join(", ")));
        }
        if self.earlier_dropped > 0 {
            line.push_str(&format!(
                "; {} from an earlier, unfinished update were not reopened",
                plural(self.earlier_dropped, "tab")
            ));
        }
        line.push('.');
        line
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// The record for `machine_id` from its layout: every live tab, in order,
/// with the cwd `cwd_of` answers for its session (else its workspace's
/// folder on that machine).
pub fn manifest_from(
    machine_id: &str,
    snapshot: &LayoutSnapshot,
    captured_at_ms: u64,
    mut cwd_of: impl FnMut(&PtySessionId) -> Option<String>,
) -> RestoreManifest {
    let mut tabs = Vec::new();
    for (key, workspace) in &snapshot.workspaces {
        let Some((team_key, form)) = key.as_str().split_once('|') else {
            continue;
        };
        let workspace_name = if workspace.descriptor.display_name.is_empty() {
            form.to_string()
        } else {
            workspace.descriptor.display_name.clone()
        };
        let workspace_folder = workspace
            .descriptor
            .worktree_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        for tab in &workspace.tabs {
            if tab.state != EntryState::Live {
                continue;
            }
            let (cwd, cwd_source) = match cwd_of(&tab.session) {
                Some(cwd) => (Some(cwd), CwdSource::Session),
                None => match &workspace_folder {
                    Some(folder) => (Some(folder.clone()), CwdSource::Workspace),
                    None => (None, CwdSource::Home),
                },
            };
            tabs.push(RestoreTab {
                team_key: team_key.to_string(),
                workspace_id: tab
                    .tags
                    .get("remoteWorkspaceId")
                    .cloned()
                    .unwrap_or_else(|| form.to_string()),
                workspace_name: workspace_name.clone(),
                name: tab.name.clone(),
                cwd,
                cwd_source,
                agent: agent_of(&tab.tags),
                role: tab.role.clone(),
                background: tab.placement == codesense_pty_core::layout::Placement::Background,
                tags: tab.tags.clone(),
                workspace_key: key.as_str().to_string(),
                descriptor: Some(workspace.descriptor.clone()),
                session: tab.session.as_str().to_string(),
                resume: None,
            });
        }
    }
    RestoreManifest {
        machine_id: machine_id.to_string(),
        captured_at_ms,
        tabs,
        old_stopped: false,
        capture_id: uuid::Uuid::new_v4().simple().to_string(),
    }
}

/// Which record an update restores from, and whether it is the earlier one:
/// the one just read, unless it is empty (or unreadable) and an earlier
/// update of this machine stopped its old server without bringing the new
/// one up (`earlier_reusable`) — then that earlier record. An earlier record
/// that may not be reused is never chosen.
pub fn choose(
    fresh: Option<RestoreManifest>,
    earlier: Option<RestoreManifest>,
    earlier_reusable: bool,
) -> Option<(RestoreManifest, bool)> {
    match fresh {
        Some(fresh) if !fresh.tabs.is_empty() => Some((fresh, false)),
        fresh => earlier
            .filter(|earlier| earlier_reusable && earlier.old_stopped && !earlier.tabs.is_empty())
            .map(|earlier| (earlier, true))
            .or_else(|| fresh.map(|fresh| (fresh, false))),
    }
}

/// Whether an earlier record of `machine_id` is still the one to reopen: the
/// machine's last update stopped its old server without bringing up the new
/// one (a remote machine says so in its state; This Mac by the record's age).
pub(crate) fn earlier_reusable(
    app: &tauri::AppHandle,
    machine_id: &str,
    earlier: Option<&RestoreManifest>,
) -> bool {
    use crate::commands::host_connection_manager::{
        HostConnectionManager, HostConnectionState, ManagerProjection,
    };
    use tauri::Manager;
    if machine_id == super::LOCAL_MACHINE_ID {
        return earlier.is_some_and(|earlier| {
            now_ms().saturating_sub(earlier.captured_at_ms) <= LOCAL_RECORD_AGE_MS
        });
    }
    app.try_state::<HostConnectionManager>()
        .and_then(|manager| {
            manager
                .projection_snapshot([machine_id.to_string()])
                .remove(machine_id)
        })
        .is_some_and(|projection| {
            matches!(
                projection,
                ManagerProjection::Present(HostConnectionState::ReplacementFailed {
                    old_stopped: true,
                    ..
                })
            )
        })
}

/// The machines whose tabs are being reopened now: their record is theirs,
/// and nothing offers or reopens it a second time meanwhile.
static RESTORING: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
    std::sync::Mutex::new(None);

struct Restoring(String);
impl Restoring {
    fn active(machine_id: &str) -> bool {
        RESTORING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|set| set.contains(machine_id))
    }
    /// Claim `machine_id` unless a restore of it already runs.
    fn claim(machine_id: &str) -> Option<Self> {
        let mut set = RESTORING.lock().unwrap_or_else(|e| e.into_inner());
        set.get_or_insert_with(Default::default)
            .insert(machine_id.to_string())
            .then(|| Self(machine_id.to_string()))
    }
}
impl Drop for Restoring {
    fn drop(&mut self) {
        if let Some(set) = RESTORING.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            set.remove(&self.0);
        }
    }
}

/// What an update recorded before its old server stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub manifest: RestoreManifest,
    /// It is an earlier, unfinished update's record (not this one's).
    pub earlier: bool,
    /// Tabs of an earlier, unfinished update this record replaced (they are
    /// no longer offered; the summary says so).
    pub dropped_earlier: usize,
    /// This attempt wrote its record to disk (else the file is someone
    /// else's: an earlier record it kept, or none). Only a saved record is
    /// rewritten or forgotten by this attempt's failure.
    pub saved: bool,
}

/// Where a machine's pending record is kept: its id made safe for a file
/// name, plus a hash of the exact id so two ids never share a file.
pub fn manifest_path(codesense_home: &Path, machine_id: &str) -> PathBuf {
    let safe: String = machine_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let digest = Sha256::digest(machine_id.as_bytes());
    let hash: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    codesense_home
        .join("terminal-updates")
        .join(format!("{safe}-{hash}.json"))
}

pub fn save(codesense_home: &Path, manifest: &RestoreManifest) -> Result<(), String> {
    let path = manifest_path(codesense_home, &manifest.machine_id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {dir:?}: {e}"))?;
    }
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|e| e.to_string())?;
    let staged = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    std::fs::write(&staged, bytes).map_err(|e| format!("write {staged:?}: {e}"))?;
    std::fs::rename(&staged, &path).map_err(|e| format!("save {path:?}: {e}"))
}

/// The pending record for `machine_id`, if one is readable.
pub fn load(codesense_home: &Path, machine_id: &str) -> Option<RestoreManifest> {
    let path = manifest_path(codesense_home, machine_id);
    let bytes = std::fs::read(&path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(manifest) => Some(manifest),
        Err(error) => {
            log::warn!(target: "machines", "unreadable restore record {path:?}: {error}");
            None
        }
    }
}

/// Forget the pending record for `machine_id`.
pub fn forget(codesense_home: &Path, machine_id: &str) {
    let path = manifest_path(codesense_home, machine_id);
    if let Err(error) = std::fs::remove_file(&path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            log::warn!(target: "machines", "restore record {path:?}: {error}");
        }
    }
}

/// Forget `machine_id`'s record only while it is still the version
/// `capture_id` (a newer one, written meanwhile, stays).
pub fn forget_version(codesense_home: &Path, machine_id: &str, capture_id: &str) {
    if load(codesense_home, machine_id).is_some_and(|record| record.capture_id == capture_id) {
        forget(codesense_home, machine_id);
    }
}

/// How an update ended, for its restore record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateEnd {
    /// The new server is admitted: reopen, then forget the record.
    Replaced,
    /// The old server stopped but the new one is not up: keep the record for
    /// the next update of this machine.
    OldStopped,
    /// The old server (and its tabs) still runs: forget the record.
    OldRunning,
}

/// What one tab's reopen came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reopened {
    Yes,
    /// PR-6: its Agent was relaunched with its resume brief.
    Resumed,
    /// PR-6: its Agent's relaunch was asked for and is still starting (not
    /// yet confirmed; it keeps going).
    ResumeStarting,
    /// PR-6: its Agent could not be relaunched (why); it reopened as a shell.
    NotResumed(String),
    /// Asked for; the machine has not confirmed it yet.
    Pending,
    No(String),
}

/// A remote reopen's outcome status, as the person reads it.
pub fn reopened_from(status: &str, message: Option<String>) -> Reopened {
    match status {
        "opened" | "alreadyLive" => Reopened::Yes,
        // `unknown`: the result could not be confirmed; it may well be open.
        "pending" | "unknown" => Reopened::Pending,
        status => Reopened::No(message.unwrap_or_else(|| status.to_string())),
    }
}

/// The request that reopens `tab` on `machine_id`: a shell in its workspace
/// and folder, under its name, without taking focus.
pub fn reopen_request(
    machine_id: &str,
    tab: &RestoreTab,
) -> crate::commands::pty_layout::RemoteShellTabRequest {
    crate::commands::pty_layout::RemoteShellTabRequest::Create {
        request_id: uuid::Uuid::new_v4(),
        machine_id: machine_id.to_string(),
        team_key: tab.team_key.clone(),
        workspace_id: tab.workspace_id.clone(),
        cwd: tab.cwd.clone(),
        name: Some(tab.name.clone()),
        launch: None,
        permission_mode: Default::default(),
        agent_key: None,
        prepared_workspace_handle: None,
        headed: false,
        initial_prompt: None,
        identity: Default::default(),
    }
}

/// A tab whose Agent's relaunch was refused (`not_resumed`) reopened as a
/// shell (`shell`): it reads as not resumed, with why, when the shell opened.
pub fn fold_resume(shell: Reopened, not_resumed: Option<String>) -> Reopened {
    match (shell, not_resumed) {
        (Reopened::Yes | Reopened::Pending, Some(why)) => Reopened::NotResumed(why),
        (shell, _) => shell,
    }
}

/// SW-AGR-19 PR-6: a relaunch refused after the reopen budget (its summary
/// read "still starting"): reopen the tab as a shell now, and say so in the
/// Updates lane.
pub(super) async fn reopen_shell_late(
    app: &tauri::AppHandle,
    machine_id: &str,
    tab: RestoreTab,
    why: String,
) {
    let opened = if machine_id == super::LOCAL_MACHINE_ID {
        let home = dirs::home_dir()
            .map(|home| home.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".into());
        let (open, spawn) = reopen_local_request(&tab, &home, |folder| Path::new(folder).is_dir());
        crate::commands::pty_layout::open_layout_tab(
            app.clone(),
            super::LOCAL_MACHINE_ID.into(),
            open,
            spawn,
        )
        .await
        .is_ok()
    } else {
        let outcome = crate::commands::pty_layout::remote_shell_tab_bound(
            app.clone(),
            reopen_request(machine_id, &tab),
            None,
        )
        .await;
        !matches!(
            reopened_from(outcome.status, outcome.message),
            Reopened::No(_)
        )
    };
    let label = super::label_of(app, machine_id).await;
    let agent = tab.agent.as_deref().unwrap_or("its agent");
    post_summary(
        app,
        if opened {
            format!(
                "{label}: {} ({agent}) could not be resumed ({why}); its tab reopened as a shell.",
                tab.name
            )
        } else {
            format!(
                "{label}: {} ({agent}) could not be resumed ({why}), and its tab did not reopen.",
                tab.name
            )
        },
    )
    .await;
}

/// Fold each tab's reopen into the summary.
pub fn summarize(results: Vec<(RestoreTab, Reopened)>) -> RestoreSummary {
    let mut summary = RestoreSummary {
        recorded: true,
        ..RestoreSummary::default()
    };
    for (tab, reopened) in results {
        let folder = match tab.cwd_source {
            CwdSource::Session => None,
            CwdSource::Workspace => {
                Some("its folder was unknown; reopened in the workspace folder")
            }
            CwdSource::Home => Some("its folder was unknown; reopened in the home folder"),
        };
        let resumed = matches!(reopened, Reopened::Resumed | Reopened::ResumeStarting);
        let (into_reopened, note) = match reopened {
            Reopened::Yes => (true, folder.map(str::to_string)),
            Reopened::Resumed => (true, None),
            Reopened::ResumeStarting => (true, Some("still starting with its brief".to_string())),
            Reopened::NotResumed(why) => (
                true,
                Some(match folder {
                    Some(folder) => format!("not resumed ({why}); {folder}"),
                    None => format!("not resumed ({why})"),
                }),
            ),
            Reopened::Pending => (
                true,
                Some(match folder {
                    Some(folder) => format!("still opening; {folder}"),
                    None => "still opening".to_string(),
                }),
            ),
            Reopened::No(why) => (false, Some(why)),
        };
        let entry = RestoredTab {
            name: tab.name,
            workspace: tab.workspace_name,
            agent: tab.agent,
            note,
            resumed,
        };
        if into_reopened {
            summary.reopened.push(entry);
        } else {
            summary.not_reopened.push(entry);
        }
    }
    summary
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

/// The layout whose tabs end with `machine_id`'s terminal server: its live
/// one, else its last frozen one. On This Mac only the session server's
/// (the app's in-process host keeps its own tabs through the update).
pub(crate) fn server_layout(
    replies: &[super::RuntimeLayoutReply],
    machine_id: &str,
) -> Option<super::RuntimeLayoutReply> {
    use super::{RuntimeLayoutReply, RuntimeMemberAddress, RuntimeMemberSlot};
    let served = |member: &RuntimeMemberAddress| {
        member.machine_id == machine_id && member.slot != RuntimeMemberSlot::InProcess
    };
    let mine = |reply: &RuntimeLayoutReply| match reply {
        RuntimeLayoutReply::Live { target, .. } => served(&target.member),
        RuntimeLayoutReply::Historical { member, .. } => served(member),
        RuntimeLayoutReply::Unavailable { .. } => false,
    };
    replies
        .iter()
        .find(|reply| mine(reply) && matches!(reply, RuntimeLayoutReply::Live { .. }))
        .or_else(|| replies.iter().find(|reply| mine(reply)))
        .cloned()
}

/// Record `machine_id`'s tabs once the person has confirmed and before its
/// server stops. Its live layout is preferred (each tab's cwd asked of the old
/// server, until it stops answering); a frozen server's last layout restores
/// with workspace folders. See [`choose`] for an earlier, unfinished update's
/// record. `None` when nothing could be recorded.
pub(crate) async fn capture(
    app: &tauri::AppHandle,
    codesense_home: &Path,
    machine_id: &str,
) -> Option<Recorded> {
    use super::{RuntimeLayoutReply, RuntimeSessionReference, RuntimeSessionTarget};
    let replies =
        match crate::commands::pty_layout::webview_member_layout_snapshots(app.clone()).await {
            Some(Ok(replies)) => replies,
            _ => Vec::new(),
        };
    let reply = server_layout(&replies, machine_id);
    let id = machine_id.to_string();
    let app = app.clone();
    let prompt_app = app.clone();
    let live_target = match &reply {
        Some(RuntimeLayoutReply::Live { target, .. }) => Some(target.clone()),
        _ => None,
    };
    let fresh = match reply {
        Some(RuntimeLayoutReply::Live {
            target, snapshot, ..
        }) => crate::commands::await_blocking("restore manifest", move || {
            // A server too slow to answer is not asked again per tab; one
            // session that is gone does not stop the others' answers.
            let mut answering = true;
            Ok(manifest_from(&id, &snapshot, now_ms(), |session| {
                if !answering {
                    return None;
                }
                let reference = RuntimeSessionReference::Admitted(Box::new(RuntimeSessionTarget {
                    runtime: target.clone(),
                    session: session.clone(),
                }));
                let asked = std::time::Instant::now();
                match crate::commands::pty_backend::session_prompt(&prompt_app, &reference) {
                    Ok(prompt) => prompt.cwd.map(|cwd| cwd.to_string_lossy().into_owned()),
                    Err(_) => {
                        answering = asked.elapsed() < SLOW_ANSWER;
                        None
                    }
                }
            }))
        })
        .await
        .ok(),
        Some(RuntimeLayoutReply::Historical { frozen, .. }) => {
            Some(manifest_from(&id, &frozen.snapshot, now_ms(), |_| None))
        }
        _ => None,
    };
    // PR-6: the briefs of Agents that resume, while the old server answers.
    let mut fresh = fresh;
    if let Some(manifest) = fresh.as_mut() {
        super::resume::attach_briefs(&app, machine_id, live_target.as_ref(), manifest).await;
    }
    let earlier = load(codesense_home, machine_id);
    let reusable = earlier_reusable(&app, machine_id, earlier.as_ref());
    // An earlier record with tabs is never overwritten by an empty capture:
    // its tabs stay offered ("Reopen N tabs") until reopened or replaced by
    // a record that has tabs of its own.
    let keep_earlier = !reusable
        && earlier
            .as_ref()
            .is_some_and(|e| e.old_stopped && !e.tabs.is_empty());
    // An earlier record this one replaces is said in the summary.
    let dropped = earlier
        .as_ref()
        .filter(|e| e.old_stopped && !e.tabs.is_empty() && !reusable)
        .map(|e| e.tabs.len())
        .unwrap_or(0);
    match choose(fresh, earlier, reusable) {
        Some((manifest, earlier)) => {
            let keeps = keep_earlier && manifest.tabs.is_empty();
            let saved = !keeps
                && !earlier
                && match save(codesense_home, &manifest) {
                    Ok(()) => true,
                    Err(error) => {
                        log::warn!(target: "machines", "restore record for {machine_id}: {error}");
                        false
                    }
                };
            Some(Recorded {
                manifest,
                earlier,
                dropped_earlier: if keeps { 0 } else { dropped },
                saved,
            })
        }
        None => {
            if !keep_earlier {
                forget(codesense_home, machine_id);
            }
            None
        }
    }
}

/// Reopen the recorded tabs on the machine's new (remote) server, a few at a
/// time, in order, within one shared deadline.
async fn reopen_remote(
    app: &tauri::AppHandle,
    machine_id: &str,
    manifest: RestoreManifest,
) -> RestoreSummary {
    use crate::commands::pty_layout::remote_shell_tab_bound;
    use futures_util::stream::{self, StreamExt};
    let deadline = tokio::time::Instant::now() + RESTORE_DEADLINE;
    // Workspaces reopen a few at a time; one workspace's tabs one after
    // another, so they come back in their order (a remote open has no index).
    let groups = by_workspace(manifest.tabs, |tab| {
        format!("{}|{}", tab.team_key, tab.workspace_id)
    });
    let results = stream::iter(groups)
        .map(|tabs| {
            let app = app.clone();
            async move {
                let mut done = Vec::with_capacity(tabs.len());
                for tab in tabs {
                    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if left.is_zero() {
                        done.push((tab, Reopened::No("not reopened in time".into())));
                        continue;
                    }
                    // PR-6: an Agent that resumes is relaunched with its
                    // brief; refused, it reopens as a shell.
                    let not_resumed = match tab.resume.clone() {
                        Some(brief) => match super::resume::relaunch_remote(
                            &app,
                            machine_id,
                            &tab,
                            &brief,
                            left.min(REOPEN_BUDGET),
                        )
                        .await
                        {
                            Ok(reopened) => {
                                done.push((tab, reopened));
                                continue;
                            }
                            Err(why) => Some(why),
                        },
                        None => None,
                    };
                    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                    let request = reopen_request(machine_id, &tab);
                    let reopened = match tokio::time::timeout(
                        left.min(REOPEN_BUDGET),
                        remote_shell_tab_bound(app.clone(), request, None),
                    )
                    .await
                    {
                        Ok(outcome) => reopened_from(outcome.status, outcome.message),
                        // It keeps opening on the machine: it may well appear.
                        Err(_) => Reopened::Pending,
                    };
                    let reopened = fold_resume(reopened, not_resumed);
                    done.push((tab, reopened));
                }
                done
            }
        })
        .buffered(REOPEN_CONCURRENCY)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect();
    summarize(results)
}

/// Tabs grouped by workspace, in their recorded order: workspaces reopen a
/// few at a time, one workspace's tabs one after another.
fn by_workspace(
    tabs: Vec<RestoreTab>,
    key: impl Fn(&RestoreTab) -> String,
) -> Vec<Vec<RestoreTab>> {
    let mut groups: Vec<(String, Vec<RestoreTab>)> = Vec::new();
    for tab in tabs {
        let group = key(&tab);
        match groups.iter_mut().find(|(known, _)| *known == group) {
            Some((_, tabs)) => tabs.push(tab),
            None => groups.push((group, vec![tab])),
        }
    }
    groups.into_iter().map(|(_, tabs)| tabs).collect()
}

/// The layout tab and warm spawn that reopen `tab` on This Mac: a shell in
/// its workspace and folder (the home folder when that folder is gone),
/// under its name and role, without taking focus. `folder_exists` is asked
/// of the recorded folder.
pub(crate) fn reopen_local_request(
    tab: &RestoreTab,
    home: &str,
    folder_exists: impl Fn(&str) -> bool,
) -> (
    codesense_pty_core::layout::OpenTab,
    crate::commands::pty_layout::TabSpawn,
) {
    use crate::commands::mcp_workspace::{AGENT_COLS, AGENT_ROWS};
    use crate::commands::pty::{AttachWarmArgs, SpawnPtySize};
    use crate::commands::pty_layout::{app_viewer, TabSpawn};
    use codesense_pty_core::layout::{OpenTab, Placement};
    // A record from before workspace keys were kept names its workspace by id.
    let form = tab
        .workspace_key
        .split_once('|')
        .map(|(_, form)| form.to_string())
        .unwrap_or_else(|| tab.workspace_id.clone());
    let open = OpenTab {
        viewer: app_viewer(),
        workspace: crate::commands::pty_layout::workspace_key(&tab.team_key, &form),
        session: PtySessionId::new(crate::commands::mcp_workspace::mint_session_id()),
        descriptor: tab.descriptor.clone(),
        name: Some(tab.name.clone()),
        role: tab.role.clone(),
        tags: Default::default(),
        placement: if tab.background {
            Placement::Background
        } else {
            Placement::Tab
        },
        index: None,
        activate: false,
    };
    let cwd = tab
        .cwd
        .clone()
        .filter(|cwd| folder_exists(cwd))
        .unwrap_or_else(|| home.to_string());
    let spawn = TabSpawn::Warm(AttachWarmArgs {
        target_cwd: cwd,
        size: SpawnPtySize {
            cols: AGENT_COLS,
            rows: AGENT_ROWS,
        },
        story_id: tab.descriptor.as_ref().and_then(|d| d.story_id.clone()),
        workspace_id: Some(form),
        epic_id: None,
        scrollback: None,
        starship_preset: None,
        shell_integration: None,
    });
    (open, spawn)
}

/// Reopen the recorded tabs on This Mac's new server, a few workspaces at a
/// time, in order, within one shared deadline.
async fn reopen_local(app: &tauri::AppHandle, manifest: RestoreManifest) -> RestoreSummary {
    use crate::commands::pty_layout::open_layout_tab;
    use futures_util::stream::{self, StreamExt};
    let deadline = tokio::time::Instant::now() + RESTORE_DEADLINE;
    let home = dirs::home_dir()
        .map(|home| home.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".into());
    let groups = by_workspace(manifest.tabs, |tab| {
        if tab.workspace_key.is_empty() {
            format!("{}|{}", tab.team_key, tab.workspace_id)
        } else {
            tab.workspace_key.clone()
        }
    });
    let results = stream::iter(groups)
        .map(|tabs| {
            let app = app.clone();
            let home = home.clone();
            async move {
                let mut done = Vec::with_capacity(tabs.len());
                for mut tab in tabs {
                    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if left.is_zero() {
                        done.push((tab, Reopened::No("not reopened in time".into())));
                        continue;
                    }
                    // PR-6: an Agent that resumes is relaunched with its
                    // brief; refused, it reopens as a shell.
                    let not_resumed = match tab.resume.clone() {
                        Some(brief) => match super::resume::relaunch_local(
                            &app,
                            &tab,
                            &brief,
                            left.min(REOPEN_BUDGET),
                        )
                        .await
                        {
                            Ok(reopened) => {
                                done.push((tab, reopened));
                                continue;
                            }
                            Err(why) => Some(why),
                        },
                        None => None,
                    };
                    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                    let (open, spawn) =
                        reopen_local_request(&tab, &home, |folder| Path::new(folder).is_dir());
                    if tab
                        .cwd
                        .as_deref()
                        .is_some_and(|cwd| !Path::new(cwd).is_dir())
                    {
                        tab.cwd_source = CwdSource::Home;
                    }
                    let reopened = match tokio::time::timeout(
                        left.min(REOPEN_BUDGET),
                        open_layout_tab(app.clone(), super::LOCAL_MACHINE_ID.into(), open, spawn),
                    )
                    .await
                    {
                        Ok(Ok(_)) => Reopened::Yes,
                        Ok(Err(why)) => Reopened::No(why),
                        // It keeps opening: it may well appear.
                        Err(_) => Reopened::Pending,
                    };
                    let reopened = fold_resume(reopened, not_resumed);
                    done.push((tab, reopened));
                }
                done
            }
        })
        .buffered(REOPEN_CONCURRENCY)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect();
    summarize(results)
}

/// Mark this attempt's own saved record as "old server stopped", unless the
/// file now holds another record with tabs (an earlier one, or a newer one a
/// concurrent update wrote) — an empty or foreign record never replaces it.
fn mark_stopped(codesense_home: &Path, machine_id: &str, recorded: &Recorded) {
    if !recorded.saved {
        return;
    }
    let on_disk = load(codesense_home, machine_id);
    let other_with_tabs = on_disk.as_ref().is_some_and(|disk| {
        disk.capture_id != recorded.manifest.capture_id && !disk.tabs.is_empty()
    });
    if other_with_tabs {
        return;
    }
    let mut manifest = recorded.manifest.clone();
    manifest.old_stopped = true;
    if let Err(error) = save(codesense_home, &manifest) {
        log::warn!(target: "machines", "restore record for {machine_id}: {error}");
    }
}

/// How an update's end settles its record on disk, before any reopen: which
/// record (if any) it reopens now. Kept apart from the reopen so the rules
/// are tested on a folder alone.
pub(crate) fn settle_record(
    codesense_home: &Path,
    machine_id: &str,
    recorded: Option<&Recorded>,
    end: UpdateEnd,
) -> Option<RestoreManifest> {
    match end {
        // Its tabs are gone with the old server: this attempt's own record
        // may now be offered and reused.
        UpdateEnd::OldStopped => {
            if let Some(recorded) = recorded {
                mark_stopped(codesense_home, machine_id, recorded);
            }
            None
        }
        // This attempt's own record describes tabs that still run; an
        // earlier, unfinished update's record stays for its next try.
        UpdateEnd::OldRunning => {
            if let Some(recorded) = recorded.filter(|recorded| recorded.saved) {
                forget_version(codesense_home, machine_id, &recorded.manifest.capture_id);
            }
            None
        }
        UpdateEnd::Replaced => recorded.map(|recorded| recorded.manifest.clone()),
    }
}

/// After an update: settle its restore record by how it ended, and when the
/// new server is up, reopen the tabs and tell the person. The summary comes
/// back for the update's result (`None` unless replaced).
pub(crate) async fn finish(
    app: &tauri::AppHandle,
    codesense_home: &Path,
    machine_id: &str,
    label: &str,
    recorded: Option<Recorded>,
    end: UpdateEnd,
) -> Option<RestoreSummary> {
    let reopening = settle_record(codesense_home, machine_id, recorded.as_ref(), end);
    if end != UpdateEnd::Replaced {
        announce(app, machine_id);
        return None;
    }
    // The update claims the machine's restore: never alongside a person's
    // "Reopen N tabs".
    let Some(_restoring) = Restoring::claim(machine_id) else {
        // Its own tabs are gone with the old server: keep them offered.
        if let Some(recorded) = recorded.as_ref() {
            mark_stopped(codesense_home, machine_id, recorded);
        }
        announce(app, machine_id);
        let waiting = reopening.as_ref().map_or(0, |manifest| manifest.tabs.len());
        let mut summary = RestoreSummary {
            recorded: recorded.is_some(),
            ..RestoreSummary::default()
        };
        summary.message = if waiting > 0 {
            format!(
                "{label}: another reopen was running, so its {} are offered to reopen in Machines.",
                plural(waiting, "tab")
            )
        } else {
            format!("{label}: another reopen was running.")
        };
        return Some(summary);
    };
    let consumed = recorded
        .as_ref()
        .is_some_and(|recorded| recorded.earlier || !recorded.manifest.tabs.is_empty());
    let dropped = recorded
        .as_ref()
        .map_or(0, |recorded| recorded.dropped_earlier);
    // Its old server is gone: until reopened, a crash keeps the tabs offered.
    if let Some(recorded) = recorded.as_ref() {
        mark_stopped(codesense_home, machine_id, recorded);
    }
    let claimed = reopening
        .as_ref()
        .map(|manifest| manifest.capture_id.clone());
    let mut summary = match reopening {
        Some(manifest) => reopen(app, machine_id, manifest).await,
        None => RestoreSummary::unrecorded(),
    };
    summary.earlier_dropped = dropped;
    // Forgotten once reopened (a crash mid-reopen keeps it), and only the
    // version this restore reopened.
    if let (true, Some(claimed)) = (consumed, claimed) {
        forget_version(codesense_home, machine_id, &claimed);
    }
    summary.message = summary.line(label);
    summary.complete = summary.all_back();
    announce(app, machine_id);
    log::info!(target: "machines", "restore {machine_id}: {}", summary.message);
    post_summary(app, summary.message.clone()).await;
    Some(summary)
}

async fn reopen(
    app: &tauri::AppHandle,
    machine_id: &str,
    manifest: RestoreManifest,
) -> RestoreSummary {
    if machine_id == super::LOCAL_MACHINE_ID {
        reopen_local(app, manifest).await
    } else {
        reopen_remote(app, machine_id, manifest).await
    }
}

/// SW-AGR-19: the tabs an unfinished update of `machine_id` recorded, to
/// offer reopening them now (`None` when there are none to offer).
pub(crate) fn pending(
    app: &tauri::AppHandle,
    codesense_home: &Path,
    machine_id: &str,
) -> Option<RestoreManifest> {
    let _ = app;
    // A restore under way owns its record.
    if Restoring::active(machine_id) {
        return None;
    }
    pending_record(codesense_home, machine_id)
}

/// The record offered to reopen: its old server stopped, it has tabs, and it
/// is recent.
pub(crate) fn pending_record(codesense_home: &Path, machine_id: &str) -> Option<RestoreManifest> {
    load(codesense_home, machine_id).filter(|earlier| {
        earlier.old_stopped
            && !earlier.tabs.is_empty()
            && now_ms().saturating_sub(earlier.captured_at_ms) <= PENDING_RECORD_AGE_MS
    })
}

/// SW-AGR-19: reopen, at the person's word, the tabs an unfinished update
/// recorded (the machine is connected again), then forget them.
pub(crate) async fn reopen_pending(
    app: &tauri::AppHandle,
    codesense_home: &Path,
    machine_id: &str,
    label: &str,
) -> Option<RestoreSummary> {
    let _restoring = Restoring::claim(machine_id)?;
    // Only a record that is offered: its old server stopped, it has tabs, and
    // it is recent (never an update's record while its old server runs).
    let manifest = pending_record(codesense_home, machine_id)?;
    let claimed = manifest.capture_id.clone();
    let mut summary = reopen(app, machine_id, manifest).await;
    forget_version(codesense_home, machine_id, &claimed);
    announce(app, machine_id);
    summary.message = summary.line(label);
    summary.complete = summary.all_back();
    post_summary(app, summary.message.clone()).await;
    Some(summary)
}

/// SW-AGR-19: how many tabs an unfinished update of `machine_id` left to
/// reopen, for the Machines row's "Reopen N tabs" (`None`: nothing to offer).
#[tauri::command]
pub async fn machine_pending_tabs(
    app: tauri::AppHandle,
    machine_id: String,
) -> Result<Option<u32>, String> {
    let Some(home) = super::codesense_home_of(&app).await else {
        return Ok(None);
    };
    Ok(pending(&app, &home, &machine_id)
        .map(|manifest| u32::try_from(manifest.tabs.len()).unwrap_or(u32::MAX)))
}

/// SW-AGR-19: reopen, at the person's word, the tabs an unfinished update of
/// `machine_id` left (it must be connected again); they are then forgotten.
#[tauri::command]
pub async fn reopen_machine_pending_tabs(
    app: tauri::AppHandle,
    machine_id: String,
) -> Result<Option<RestoreSummary>, String> {
    use tauri::Manager;
    let home = super::codesense_home_of(&app)
        .await
        .ok_or("CodeSense's home folder is not available")?;
    if pending(&app, &home, &machine_id).is_none() {
        return Ok(None);
    }
    app.state::<crate::commands::host_connection_manager::HostConnectionManager>()
        .connected_backend(&machine_id)
        .map_err(|_| "connect the machine first".to_string())?;
    let label = super::label_of(&app, &machine_id).await;
    Ok(reopen_pending(&app, &home, &machine_id, &label).await)
}

/// The event a machine's pending tabs changed on (Machines re-asks).
pub const RESTORE_CHANGED_EVENT: &str = "terminal-restore-changed";

fn announce(app: &tauri::AppHandle, machine_id: &str) {
    use tauri::Emitter;
    let _ = app.emit(RESTORE_CHANGED_EVENT, machine_id);
}

/// Tell the person what came back through the active team's human inbox.
/// The update's own result carries the summary too.
pub(crate) async fn post_summary(app: &tauri::AppHandle, line: String) {
    use tauri::Emitter;
    let context = match crate::commands::route_to_context::derive_active_context(app).await {
        Ok(context) => context,
        Err(error) => {
            log::warn!(target: "machines", "restore summary not posted: {error}");
            return;
        }
    };
    let Some(team) = context.team_name.clone() else {
        log::info!(target: "machines", "restore summary not posted: no active team");
        return;
    };
    let host = crate::commands::agent_inbox::AppInboxHost::port(app.clone());
    let team = (context.org_slug.clone(), team);
    let written = post_summary_note(&host, &team, &line).await;
    if let Err(error) = written {
        log::warn!(target: "machines", "restore summary not posted: {error}");
    }
    let _ = app.emit(
        "mcp:agent-message",
        serde_json::json!({
            "sessionId": "codesense:terminal-servers",
            "role": "update",
            "text": line,
            "renderTarget": "terminal",
        }),
    );
}

/// Persist the resolved team's outcome as ordinary unread mail, using the shared system sender.
pub(crate) async fn post_summary_note(
    host: &std::sync::Arc<dyn crate::commands::agent_inbox::InboxHost>,
    team: &(String, String),
    line: &str,
) -> Result<i64, String> {
    crate::commands::agent_inbox::note_to_human(host, Some(team), line, line).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesense_pty_core::domain::WorkspaceKey;
    use codesense_pty_core::layout::{Placement, TabEntry, WorkspaceDescriptor, WorkspaceLayout};

    fn tab(session: &str, name: &str, tags: &[(&str, &str)], state: EntryState) -> TabEntry {
        TabEntry {
            session: PtySessionId::new(session),
            name: name.into(),
            role: None,
            placement: Placement::Tab,
            state,
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn snapshot(worktree: Option<&str>) -> LayoutSnapshot {
        let mut snapshot: LayoutSnapshot = serde_json::from_value(serde_json::json!({
            "schema": 1, "serverId": "s", "serverName": "box", "epoch": "e", "rev": 1,
            "workspaces": {}, "viewer": {}
        }))
        .unwrap();
        snapshot.workspaces.insert(
            WorkspaceKey::new("acme:default|SW-1"),
            WorkspaceLayout {
                descriptor: WorkspaceDescriptor {
                    display_name: "SW-1 login".into(),
                    worktree_path: worktree.map(Into::into),
                    ..Default::default()
                },
                tabs: vec![
                    tab("a", "shell", &[], EntryState::Live),
                    tab(
                        "b",
                        "Barb",
                        &[
                            ("remoteAgentKey", "team/barb"),
                            ("remoteWorkspaceId", "SW-1-remote"),
                        ],
                        EntryState::Live,
                    ),
                    tab(
                        "c",
                        "Mia",
                        &[("launchAgentRevision", "team/mia@7")],
                        EntryState::Live,
                    ),
                    tab(
                        "d",
                        "done",
                        &[],
                        EntryState::Exited {
                            exit_code: Some(0),
                            signal: None,
                        },
                    ),
                ],
                ..Default::default()
            },
        );
        snapshot
    }

    #[test]
    fn agr19_the_record_keeps_every_live_tab_with_its_folder_and_agent() {
        let manifest = manifest_from("box", &snapshot(Some("/srv/SW-1")), 7, |session| {
            (session.as_str() == "a").then(|| "/home/me/src".to_string())
        });
        assert_eq!(
            manifest.tabs.len(),
            3,
            "an exited tab has nothing to reopen"
        );
        let shell = &manifest.tabs[0];
        assert_eq!(
            (
                shell.team_key.as_str(),
                shell.workspace_id.as_str(),
                shell.cwd.as_deref(),
                shell.cwd_source
            ),
            (
                "acme:default",
                "SW-1",
                Some("/home/me/src"),
                CwdSource::Session
            )
        );
        assert_eq!(shell.workspace_name, "SW-1 login");
        // A remote launch's workspace id is its own tag's, not the key's.
        assert_eq!(manifest.tabs[1].workspace_id, "SW-1-remote");
        assert_eq!(manifest.tabs[1].agent.as_deref(), Some("team/barb"));
        assert_eq!(
            manifest.tabs[1]
                .tags
                .get("remoteAgentKey")
                .map(String::as_str),
            Some("team/barb")
        );
        // Unknown cwd: the workspace's folder.
        assert_eq!(
            (manifest.tabs[1].cwd.as_deref(), manifest.tabs[1].cwd_source),
            (Some("/srv/SW-1"), CwdSource::Workspace)
        );
        // A local launch is its Agent, not its revision.
        assert_eq!(manifest.tabs[2].agent.as_deref(), Some("team/mia"));
        // No workspace folder either: the home.
        let bare = manifest_from("box", &snapshot(None), 7, |_| None);
        assert_eq!(
            (bare.tabs[0].cwd.as_deref(), bare.tabs[0].cwd_source),
            (None, CwdSource::Home)
        );
    }

    #[test]
    fn agr19_a_tab_reopens_where_it_was_under_its_name_without_focus() {
        let manifest = manifest_from("box", &snapshot(Some("/srv/SW-1")), 7, |session| {
            (session.as_str() == "a").then(|| "/home/me/src".to_string())
        });
        let crate::commands::pty_layout::RemoteShellTabRequest::Create {
            machine_id,
            team_key,
            workspace_id,
            cwd,
            name,
            headed,
            agent_key,
            launch,
            prepared_workspace_handle,
            ..
        } = reopen_request("box", &manifest.tabs[1])
        else {
            panic!("a create")
        };
        assert_eq!(
            (
                machine_id.as_str(),
                team_key.as_str(),
                workspace_id.as_str()
            ),
            ("box", "acme:default", "SW-1-remote")
        );
        assert_eq!(cwd.as_deref(), Some("/srv/SW-1"));
        assert_eq!(name.as_deref(), Some("Barb"));
        assert!(!headed, "restore never takes focus");
        // Shells only: no launch, Agent or prepared workspace, so the remote
        // open skips Step 2 at runtime (the writer guard's EXPLICIT entries
        // rest on this).
        assert!(launch.is_none(), "restore never starts a CLI");
        assert!(agent_key.is_none(), "restore never starts an Agent");
        assert!(
            prepared_workspace_handle.is_none(),
            "restore never prepares a workspace"
        );
    }

    #[test]
    fn agr19_this_mac_records_the_session_server_layout_not_the_in_app_one() {
        use crate::commands::machines::RuntimeLayoutReply;
        let live = |slot: &str, rev: u64| -> RuntimeLayoutReply {
            serde_json::from_value(serde_json::json!({
                "mode": "live",
                "member": {"machineId": "local", "slot": slot},
                "admissionRevision": "1",
                "namespace": {"serverId": slot, "epoch": "e", "generation": "1"},
                "snapshot": {
                    "schema": 1, "serverId": slot, "serverName": "local",
                    "epoch": "e", "rev": rev, "workspaces": {},
                    "viewer": {"active": {}, "views": {}, "hidden": []}
                }
            }))
            .unwrap()
        };
        // The in-app host sorts first; both are live.
        let replies = vec![live("inProcess", 1), live("sidecar", 2)];
        let chosen = server_layout(&replies, "local");
        assert!(
            matches!(&chosen, Some(RuntimeLayoutReply::Live { snapshot, .. }) if snapshot.rev == 2),
            "{chosen:?}"
        );
        assert!(server_layout(&replies[..1], "local").is_none());
        assert!(server_layout(&replies, "box").is_none());
    }

    #[test]
    fn agr19_only_a_stopped_recent_record_with_tabs_is_offered_to_reopen() {
        let home = tempfile::tempdir().unwrap();
        // An update's record while its old server still runs: never reopened.
        let mut record = manifest_from("box", &snapshot(None), now_ms(), |_| None);
        save(home.path(), &record).unwrap();
        assert!(pending_record(home.path(), "box").is_none());
        record.old_stopped = true;
        save(home.path(), &record).unwrap();
        assert_eq!(pending_record(home.path(), "box"), Some(record.clone()));
        record.captured_at_ms = 1;
        save(home.path(), &record).unwrap();
        assert!(pending_record(home.path(), "box").is_none(), "too old");
    }

    #[test]
    fn agr19_a_replaced_update_that_loses_the_claim_keeps_its_tabs_offered() {
        let home = tempfile::tempdir().unwrap();
        let mine = manifest_from("box", &snapshot(None), now_ms(), |_| None);
        save(home.path(), &mine).unwrap();
        let recorded = Recorded {
            manifest: mine.clone(),
            earlier: false,
            dropped_earlier: 0,
            saved: true,
        };
        // Replaced, but a person's Reopen holds the machine: the record is
        // kept, marked stopped, so "Reopen N tabs" offers it afterwards.
        settle_record(home.path(), "box", Some(&recorded), UpdateEnd::Replaced);
        mark_stopped(home.path(), "box", &recorded);
        assert!(pending_record(home.path(), "box")
            .is_some_and(|offered| offered.capture_id == mine.capture_id));
    }

    #[test]
    fn agr19_a_mac_tab_reopens_in_its_workspace_folder_and_place() {
        use crate::commands::pty_layout::TabSpawn;
        use codesense_pty_core::layout::Placement;
        let manifest = manifest_from("local", &snapshot(Some("/srv/SW-1")), 7, |session| {
            (session.as_str() == "a").then(|| "/home/me/src".to_string())
        });
        let mut shell = manifest.tabs[0].clone();
        shell.role = Some("reviewer".into());
        shell.background = true;
        let (open, spawn) = reopen_local_request(&shell, "/home/me", |_| true);
        assert_eq!(open.workspace.as_str(), "acme:default|SW-1");
        assert_eq!(open.name.as_deref(), Some("shell"));
        assert_eq!(open.role.as_deref(), Some("reviewer"));
        assert_eq!(open.placement, Placement::Background);
        assert!(!open.activate, "restore never takes focus");
        assert!(open.tags.is_empty(), "a reopened tab is a plain shell");
        let TabSpawn::Warm(warm) = spawn else {
            panic!("a warm shell")
        };
        assert_eq!(warm.target_cwd, "/home/me/src");
        assert_eq!(warm.workspace_id.as_deref(), Some("SW-1"));
        // A folder that is gone: the home folder.
        let (_, spawn) = reopen_local_request(&shell, "/home/me", |_| false);
        let TabSpawn::Warm(warm) = spawn else {
            panic!("a warm shell")
        };
        assert_eq!(warm.target_cwd, "/home/me");
        // A record from before workspace keys were kept: its workspace id.
        let mut legacy = manifest.tabs[0].clone();
        legacy.workspace_key = String::new();
        legacy.workspace_id = "SW-9".into();
        let (open, _) = reopen_local_request(&legacy, "/home/me", |_| true);
        assert_eq!(open.workspace.as_str(), "acme:default|SW-9");
    }

    #[test]
    fn agr19_tabs_reopen_grouped_by_workspace_in_order() {
        let manifest = manifest_from("box", &snapshot(None), 7, |_| None);
        let mut tabs = manifest.tabs.clone();
        let mut other = tabs[0].clone();
        other.workspace_id = "SW-2".into();
        other.name = "other".into();
        tabs.insert(1, other);
        let groups = by_workspace(tabs, |tab| tab.workspace_id.clone());
        let names: Vec<Vec<&str>> = groups
            .iter()
            .map(|group| group.iter().map(|tab| tab.name.as_str()).collect())
            .collect();
        assert_eq!(
            names,
            vec![vec!["shell", "Mia"], vec!["other"], vec!["Barb"]]
        );
    }

    #[test]
    fn agr19_an_empty_layout_keeps_the_record_of_an_update_that_stopped_its_old_server() {
        let mut full = manifest_from("box", &snapshot(None), 7, |_| None);
        // A record whose update never stopped the old server is never reused.
        assert_eq!(
            choose(None, Some(full.clone()), true),
            None,
            "its tabs still run"
        );
        full.old_stopped = true;
        let empty = RestoreManifest {
            machine_id: "box".into(),
            captured_at_ms: 9,
            tabs: Vec::new(),
            old_stopped: false,
            capture_id: "empty".into(),
        };
        // A fresh record with tabs always wins.
        assert_eq!(
            choose(Some(full.clone()), Some(empty.clone()), true),
            Some((full.clone(), false))
        );
        // Empty now: the earlier, unfinished update's record — only while it
        // may be reused.
        assert_eq!(
            choose(Some(empty.clone()), Some(full.clone()), true),
            Some((full.clone(), true))
        );
        assert_eq!(
            choose(Some(empty.clone()), Some(full.clone()), false),
            Some((empty.clone(), false))
        );
        assert_eq!(
            choose(None, Some(full.clone()), true),
            Some((full.clone(), true))
        );
        assert_eq!(choose(None, Some(full.clone()), false), None);
        assert_eq!(
            choose(Some(empty.clone()), None, true),
            Some((empty, false))
        );
        assert_eq!(choose(None, None, true), None);
    }

    #[test]
    fn agr19_an_update_end_settles_only_its_own_record() {
        let home = tempfile::tempdir().unwrap();
        let mut earlier = manifest_from("box", &snapshot(None), 1, |_| None);
        earlier.old_stopped = true;
        save(home.path(), &earlier).unwrap();
        // A reused earlier record (not saved by this attempt) survives a
        // failure that left the old server running…
        let reused = Recorded {
            manifest: earlier.clone(),
            earlier: true,
            dropped_earlier: 0,
            saved: false,
        };
        settle_record(home.path(), "box", Some(&reused), UpdateEnd::OldRunning);
        assert_eq!(load(home.path(), "box"), Some(earlier.clone()));
        // …and this attempt's own empty record never replaces it on a stop.
        let empty = Recorded {
            manifest: RestoreManifest {
                machine_id: "box".into(),
                captured_at_ms: 2,
                tabs: Vec::new(),
                old_stopped: false,
                capture_id: "mine".into(),
            },
            earlier: false,
            dropped_earlier: 0,
            saved: true,
        };
        settle_record(home.path(), "box", Some(&empty), UpdateEnd::OldStopped);
        assert_eq!(load(home.path(), "box"), Some(earlier.clone()));
        // This attempt's own record with tabs: marked stopped on a stop…
        let mine = manifest_from("box", &snapshot(None), 3, |_| None);
        save(home.path(), &mine).unwrap();
        let own = Recorded {
            manifest: mine.clone(),
            earlier: false,
            dropped_earlier: 0,
            saved: true,
        };
        settle_record(home.path(), "box", Some(&own), UpdateEnd::OldStopped);
        assert!(load(home.path(), "box")
            .is_some_and(|r| r.old_stopped && r.capture_id == mine.capture_id));
        // …forgotten (its version only) when the old server still runs…
        let newer = manifest_from("box", &snapshot(None), 4, |_| None);
        save(home.path(), &newer).unwrap();
        settle_record(home.path(), "box", Some(&own), UpdateEnd::OldRunning);
        assert_eq!(
            load(home.path(), "box"),
            Some(newer.clone()),
            "a newer record stays"
        );
        // …and handed over to reopen on a replace.
        assert_eq!(
            settle_record(home.path(), "box", Some(&own), UpdateEnd::Replaced),
            Some(mine)
        );
        assert_eq!(
            settle_record(home.path(), "box", None, UpdateEnd::Replaced),
            None
        );
    }

    #[test]
    fn agr19_a_reopen_forgets_only_the_version_it_claimed() {
        let home = tempfile::tempdir().unwrap();
        let claimed = manifest_from("box", &snapshot(None), 7, |_| None);
        let newer = manifest_from("box", &snapshot(None), 8, |_| None);
        assert_ne!(claimed.capture_id, newer.capture_id);
        save(home.path(), &newer).unwrap();
        forget_version(home.path(), "box", &claimed.capture_id);
        assert_eq!(
            load(home.path(), "box"),
            Some(newer.clone()),
            "a newer record stays"
        );
        forget_version(home.path(), "box", &newer.capture_id);
        assert_eq!(load(home.path(), "box"), None);
    }

    #[test]
    fn agr19_a_record_is_saved_loaded_and_forgotten_per_exact_machine() {
        let home = tempfile::tempdir().unwrap();
        let one = manifest_from("box/1", &snapshot(None), 7, |_| None);
        let two = manifest_from("box_1", &snapshot(None), 8, |_| None);
        save(home.path(), &one).unwrap();
        save(home.path(), &two).unwrap();
        assert_ne!(
            manifest_path(home.path(), "box/1"),
            manifest_path(home.path(), "box_1")
        );
        assert_eq!(load(home.path(), "box/1"), Some(one));
        forget(home.path(), "box/1");
        assert_eq!(load(home.path(), "box/1"), None);
        assert_eq!(load(home.path(), "box_1"), Some(two));
        std::fs::write(manifest_path(home.path(), "box_1"), b"{not json").unwrap();
        assert_eq!(
            load(home.path(), "box_1"),
            None,
            "a corrupt record reads as none"
        );
    }

    #[test]
    fn agr19_a_restore_under_way_is_never_offered_or_run_twice() {
        let running = Restoring::claim("box-r").unwrap();
        assert!(Restoring::active("box-r"));
        assert!(Restoring::claim("box-r").is_none());
        drop(running);
        assert!(!Restoring::active("box-r"));
        let claimed = Restoring::claim("box-r").unwrap();
        assert!(Restoring::active("box-r"));
        drop(claimed);
    }

    #[test]
    fn agr19_an_agent_is_named_by_its_saved_agent_or_its_cli() {
        let tags = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        assert_eq!(
            agent_of(&tags(&[("remoteAgentKey", "team/barb")])).as_deref(),
            Some("team/barb")
        );
        assert_eq!(
            agent_of(&tags(&[("launchAgentRevision", "team/mia@7")])).as_deref(),
            Some("team/mia")
        );
        // A CLI launched as itself, no saved Agent.
        assert_eq!(
            agent_of(&tags(&[("remoteLaunch", "\"claude\"")])).as_deref(),
            Some("claude")
        );
        assert_eq!(agent_of(&tags(&[("remoteLaunch", "\"shell\"")])), None);
        assert_eq!(agent_of(&tags(&[])), None);
    }

    #[test]
    fn agr19_outcomes_read_as_reopened_pending_or_not() {
        assert_eq!(reopened_from("opened", None), Reopened::Yes);
        assert_eq!(reopened_from("alreadyLive", None), Reopened::Yes);
        assert_eq!(reopened_from("pending", None), Reopened::Pending);
        assert_eq!(reopened_from("unknown", None), Reopened::Pending);
        assert_eq!(
            reopened_from("refused", Some("the workspace is closed".into())),
            Reopened::No("the workspace is closed".into())
        );
        assert_eq!(reopened_from("ended", None), Reopened::No("ended".into()));
    }

    #[test]
    fn agr19_the_summary_says_what_came_back_and_what_did_not() {
        let manifest = manifest_from("box", &snapshot(Some("/srv/SW-1")), 7, |session| {
            (session.as_str() == "a").then(|| "/home/me".to_string())
        });
        let mut tabs = manifest.tabs.into_iter();
        let summary = summarize(vec![
            (tabs.next().unwrap(), Reopened::Yes),
            (tabs.next().unwrap(), Reopened::Pending),
            (tabs.next().unwrap(), Reopened::No("agent refused".into())),
        ]);
        assert_eq!(summary.reopened.len(), 2);
        assert!(!summary.all_back());
        assert_eq!(summary.reopened[1].agent.as_deref(), Some("team/barb"));
        // A refusal that starts with "agent" is not taken for an agent.
        assert_eq!(
            summary.line("box"),
            "box: 2 tabs reopened; agents ended (relaunch them): team/barb, team/mia; not exactly as it was: Barb (still opening; its folder was unknown; reopened in the workspace folder); not reopened: Mia (agent refused)."
        );
        let clean = summarize(vec![(
            manifest_from("box", &snapshot(None), 7, |_| Some("/x".into()))
                .tabs
                .remove(0),
            Reopened::Yes,
        )]);
        assert!(clean.all_back());
        assert_eq!(clean.line("box"), "box: 1 tab reopened.");
        assert_eq!(
            RestoreSummary {
                recorded: true,
                ..RestoreSummary::default()
            }
            .line("box"),
            "box: it had no open tabs."
        );
        assert!(RestoreSummary::unrecorded()
            .line("box")
            .contains("could not be recorded"));
        let replaced = RestoreSummary {
            recorded: true,
            earlier_dropped: 2,
            ..RestoreSummary::default()
        };
        assert!(!replaced.all_back());
        assert!(replaced
            .line("box")
            .contains("2 tabs from an earlier, unfinished update were not reopened"));
    }

    #[test]
    fn agr19_the_summary_says_which_agents_resumed_and_which_did_not() {
        let manifest = manifest_from("box", &snapshot(None), 7, |_| Some("/x".into()));
        let mut tabs = manifest.tabs.into_iter().skip(1);
        let summary = summarize(vec![
            (tabs.next().unwrap(), Reopened::Resumed),
            (
                tabs.next().unwrap(),
                Reopened::NotResumed("its prepared workspace is unknown".into()),
            ),
        ]);
        assert!(summary.reopened[0].resumed);
        assert!(!summary.reopened[1].resumed);
        assert_eq!(
            summary.line("box"),
            "box: 2 tabs reopened; agents resumed with a brief: team/barb; agents ended (relaunch them): team/mia; not exactly as it was: Mia (not resumed (its prepared workspace is unknown))."
        );
        assert!(!summary.all_back());
        // Every agent resumed, in its own folder: everything came back.
        let all = summarize(vec![(
            manifest_from("box", &snapshot(None), 7, |_| Some("/x".into()))
                .tabs
                .remove(1),
            Reopened::Resumed,
        )]);
        assert!(all.all_back());
    }

    #[test]
    fn agr19_a_refused_relaunch_reads_as_a_shell_not_resumed() {
        assert_eq!(
            fold_resume(Reopened::Yes, Some("its workspace is not open".into())),
            Reopened::NotResumed("its workspace is not open".into())
        );
        assert_eq!(
            fold_resume(Reopened::Pending, Some("x".into())),
            Reopened::NotResumed("x".into())
        );
        // The shell did not open either: it reads as not reopened.
        assert_eq!(
            fold_resume(Reopened::No("gone".into()), Some("x".into())),
            Reopened::No("gone".into())
        );
        assert_eq!(fold_resume(Reopened::Yes, None), Reopened::Yes);
    }

    #[test]
    fn agr19_a_record_keeps_each_tabs_session_and_brief() {
        let mut manifest = manifest_from("box", &snapshot(None), 7, |_| None);
        assert_eq!(manifest.tabs[0].session, "a");
        assert_eq!(manifest.tabs[0].resume, None);
        manifest.tabs[1].resume = Some("brief".into());
        let home = tempfile::tempdir().unwrap();
        save(home.path(), &manifest).unwrap();
        assert_eq!(load(home.path(), "box"), Some(manifest));
    }
}
