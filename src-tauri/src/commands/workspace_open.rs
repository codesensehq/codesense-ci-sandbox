// SPDX-License-Identifier: MIT
//! `open_workspace` with a `target`: any agent (or a person) launches another agent in a
//! story worktree or in a named team workspace ("Sprint 1") and gets a typed result.
//!
//! CodeSense records and shows; it does not decide who may launch. Besides `opened`, the
//! only outcomes are an input the resolver cannot use (`invalid`), a name that is already
//! running (`alreadyLive`) and the machine's shipped spawn cap (`busy`). The launcher is
//! whatever the caller reports in `launched_by`, kept for display.
//!
//! Every open launches an agent: the one it names, else the preferred built-in
//! (`builtin/claude`).
//!
//! A team-workspace session runs in a registered context worktree's team content folder.
//! Several agents share that worktree as tabs; the canonical clone stays untouched.
//!
//! [`open_agent`] is the whole decision, written over the [`OpenHost`] port so it runs end
//! to end in tests without a PTY; [`AppOpenHost`] is the production port.

mod context;
pub(crate) use context::validate as validate_context_worktree;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::Manager;
use tokio::sync::oneshot::error::TryRecvError;

use super::dashboard_launch::{launch_label, LaunchMeta, LaunchRecord, LaunchTeam, RunProvenance};
use super::interactive_gate::{GateRefusal, WorkspaceMode};
pub(crate) use super::mcp_workspace::{LaunchError, LaunchOutcome};
use crate::agents::adapter;
use crate::agents::launch::AgentLaunch;
use crate::agents::launch_check::{LaunchWarning, LaunchWarningCode};

/// What an open with a `target` asks for, as the caller sent it.
#[derive(Debug, Clone, Default)]
pub(crate) struct OpenRequest {
    pub target: String,
    pub story_id: Option<String>,
    pub name: Option<String>,
    pub team: Option<String>,
    pub org: Option<String>,
    pub worktree: Option<String>,
    pub create_worktree: bool,
    /// The agent to launch: a rendered key, or a bare id in the target team, then the
    /// caller's personal space, then the starters. None is the preferred built-in.
    pub agent: Option<String>,
    pub initial_prompt: Option<String>,
    pub launched_by: Option<String>,
    /// The role the launcher assigns the session; display only.
    pub role: Option<String>,
    pub queue_role: Option<Option<String>>,
    /// The avatar the launcher assigns the session; display only.
    pub avatar: Option<String>,
    /// The task a reviewer launch reviews (story targets only).
    pub reviews: Option<String>,
    pub mode: Option<String>,
    pub headed: Option<bool>,
    /// The machine a story launch runs on: a team machine's id, or `local` for This Mac.
    /// A story's workspace stays where it was created, so this picks where a new one is
    /// created and must agree with an existing one. Absent: where the workspace lives
    /// (This Mac for a new one). Story targets only.
    ///
    /// SW-WS-1: the "stays where it was created" invariant is now mutable via
    /// one authorized verb — `move_workspace` is the ONE path that updates
    /// `location.machineId`. Every refusal in this file still compares a
    /// requested machine against the current `location.machineId`; the move
    /// pipeline writes the new value and the comparison then succeeds on
    /// the target naturally.
    pub machine_id: Option<String>,
    /// how long the launched session may run, in milliseconds, as
    /// resolved from the caller's `maxRuntime` string by [`parse_max_runtime`].
    /// `None` means no limit — the sweep never touches the entry.
    pub max_runtime_ms: Option<u64>,
}

/// The org and team the open runs in when the request names no team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveTeam {
    pub org: String,
    pub team: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum OpenStatus {
    Opened,
    AlreadyLive,
    Busy,
    Invalid,
}

/// `{status, handle, name, message}`; `message` is one plain sentence an agent can relay.
/// An agent launch adds the launch `warnings` (never a refusal) when there are any. An open
/// that showed an already-running session adds what showing it did: `shown` or `notShown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct OpenResult {
    pub status: OpenStatus,
    pub handle: Option<String>,
    pub name: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<LaunchWarning>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub reveal: Option<RevealOutcome>,
    /// SW-XS-7: what Step 2 did for this launch (kept, skipped, pinned drift).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepared: Option<crate::agents::prepare::progress::PrepareSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<ContextWorktreeResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ContextWorktreeResult {
    pub id: String,
    pub name: String,
    pub branch: String,
    pub created: bool,
}

impl OpenResult {
    pub(crate) fn invalid(reason: &str) -> Self {
        OpenResult {
            status: OpenStatus::Invalid,
            handle: None,
            name: None,
            message: format!("Not opened: {}.", reason.trim().trim_end_matches('.')),
            warnings: Vec::new(),
            reveal: None,
            prepared: None,
            worktree: None,
        }
    }
}

/// What showing a running session did: `{"shown": "tab" | "restored" | "adopted" |
/// "runOutput"}`, or `{"notShown": reason}`. Never a silent success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RevealOutcome {
    Shown(ShownAs),
    NotShown(String),
}

/// How a shown session came on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ShownAs {
    /// Its tab, focused where it was.
    Tab,
    /// Its tab, put back from Hidden.
    Restored,
    /// A live session no workspace showed, adopted into a tab.
    Adopted,
    /// A headless run, as its run output.
    RunOutput,
}

/// Where a launch runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchTarget {
    /// The story's managed worktree, in the story's own workspace. The handle is the story
    /// id, or `<storyId>#<name>` for a named session beside it. `team` is the story's
    /// own org and team, resolved once so the launch uses the org the open checked.
    Story {
        story_id: String,
        handle: String,
        team: ActiveTeam,
    },
    /// A managed dir under its own handle.
    Dir {
        handle: String,
        cwd: PathBuf,
        scope: ActiveTeam,
    },
}

/// One launch through the shipped spine.
#[derive(Debug, Clone)]
pub(crate) struct Launch {
    pub target: LaunchTarget,
    pub initial_prompt: Option<String>,
    pub record: LaunchRecord,
    pub view: super::mcp_workspace::SessionView,
    /// The agent the open launches.
    pub agent: AgentLaunch,
    /// Carries the agent's `agent@revision`.
    pub provenance: RunProvenance,
}

/// Where a story's workspace lives now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspacePlace {
    ThisMac,
    Machine(String),
}

/// One launch into a story's workspace on a machine, through the remote terminal path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteLaunch {
    pub machine_id: String,
    pub team: ActiveTeam,
    pub story_id: String,
    /// The tab's name.
    pub name: String,
    /// The saved Agent's key.
    pub agent_key: String,
    /// The launch's own first prompt; absent, the Agent starts with its default one.
    pub initial_prompt: Option<String>,
    pub launched_by: Option<String>,
    pub role: Option<String>,
    pub queue_role: Option<Option<String>>,
    pub avatar: Option<String>,
    /// Bring the tab forward.
    pub focus: bool,
}

/// A launch on a machine that settled: the remote tab's handle, and whether it is new.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteLaunched {
    pub handle: String,
    pub fresh: bool,
    /// The machine's name, for the message.
    pub machine: String,
}

/// The registry, filesystem and launch-spine side of an open.
#[async_trait::async_trait]
pub(crate) trait OpenHost: Send + Sync {
    fn role_join_error(&self, _handle: &str) -> Option<String> {
        None
    }
    /// Where `story_id`'s workspace lives (open or minimized), or `None` when it has none.
    async fn story_workspace_place(
        &self,
        _team: &ActiveTeam,
        _story_id: &str,
    ) -> Option<WorkspacePlace> {
        None
    }
    /// Launch an agent into a story's workspace on a machine, creating the workspace
    /// there first when it has none, and setting the agent up there before it starts.
    /// `Err` is the machine's reason.
    async fn launch_remote(&self, _launch: RemoteLaunch) -> Result<RemoteLaunched, String> {
        Err("unavailable: launching on a machine is unavailable here".into())
    }
    /// The org and team `story_id` belongs to, resolved the way its story launch resolves
    /// its worktree; `org` is the caller's hint for its org. `Err` for an unknown story.
    async fn story_team(&self, story_id: &str, org: Option<&str>) -> Result<ActiveTeam, String>;
    /// The team's managed content root; `Err` for a team with no content.
    async fn team_content_root(&self, org: &str, team: &str) -> Result<PathBuf, String>;
    /// Whether `handle` already has a session in the launch registry.
    fn is_live(&self, handle: &str) -> bool;
    /// Hold `handle` from before staging until launch settles, waiting for an
    /// earlier open to finish before this caller checks the actual session.
    async fn claim_open(&self, handle: &str) -> OpenClaim;
    async fn prepare_context_worktree(
        &self,
        scope: &ActiveTeam,
        content: PathBuf,
        name: &str,
        requested: Option<&str>,
        create: bool,
        agent: &AgentLaunch,
    ) -> Result<context::ContextDestination, String>;
    async fn rollback_context_worktree(&self, destination: &context::ContextDestination);
    async fn live_context(
        &self,
        _handle: &str,
        _req: &OpenRequest,
    ) -> Result<Option<ContextWorktreeResult>, String> {
        Ok(None)
    }
    async fn context_created_by(
        &self,
        _destination: &mut context::ContextDestination,
        _launcher: Option<&str>,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn context_used(&self, _destination: &context::ContextDestination, _name: &str) {}
    /// Prepare the agent `reference` names for a launch against `team` with `prompt`, in
    /// `mode` when the caller chose one, else the agent's own declared mode
    /// ([`crate::agents::launch::prepare_named`]). `Err` is an agent that cannot be found.
    async fn prepare_agent(
        &self,
        reference: &str,
        team: &ActiveTeam,
        prompt: Option<&str>,
        mode: Option<adapter::LaunchMode>,
    ) -> Result<AgentLaunch, String>;
    /// Launch through the spine, returning whether the session is fresh and any startup warnings.
    async fn launch(&self, launch: Launch) -> Result<LaunchOutcome, LaunchError>;
    /// Surface a just-opened session in the viewer, stealing focus only when `focus`.
    fn reveal(&self, handle: &str, focus: bool);
    /// What Step 2 did for the launch of `handle`, once (SW-XS-7).
    fn take_prepared(
        &self,
        handle: &str,
    ) -> Option<crate::agents::prepare::progress::PrepareSummary> {
        crate::agents::prepare::progress::take_summary(handle)
    }
    /// Show an already-running session in the viewer (focused) and say what that did: its
    /// tab, restored, adopted into a tab, its run output, or why it could not be shown.
    async fn show(&self, handle: &str) -> RevealOutcome;
}

/// Serialize staging and launch for a name. Waiters recheck the settled registry
/// before reporting an existing session; owning a lock never means a launch succeeded.
type OpenGate = tokio::sync::Mutex<()>;
type OpenGateMap = BTreeMap<String, std::sync::Weak<OpenGate>>;

#[derive(Debug, Default, Clone)]
pub(crate) struct OpenClaims(std::sync::Arc<std::sync::Mutex<OpenGateMap>>);

impl OpenClaims {
    pub(crate) async fn claim(&self, handle: &str) -> OpenClaim {
        let gate = {
            let mut held = self.0.lock().unwrap_or_else(|e| e.into_inner());
            held.retain(|_, gate| gate.strong_count() > 0);
            match held.get(handle).and_then(std::sync::Weak::upgrade) {
                Some(gate) => gate,
                None => {
                    let gate = std::sync::Arc::new(tokio::sync::Mutex::new(()));
                    held.insert(handle.to_string(), std::sync::Arc::downgrade(&gate));
                    gate
                }
            }
        };
        OpenClaim {
            _guard: gate.lock_owned().await,
        }
    }
}

#[derive(Debug)]
pub(crate) struct OpenClaim {
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

/// The slug of a free name: lowercase ASCII letters and digits, every other run of
/// characters collapsed to one `-`. `None` when no letter or digit is left.
pub(crate) fn worker_slug(name: &str) -> Option<String> {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-').to_string();
    (!slug.is_empty()).then_some(slug)
}

/// The longest session label a named story handle carries, in characters (the dashboard
/// bridge addresses labels up to this length).
pub(crate) const STORY_SESSION_LABEL_MAX: usize = 64;

/// The handle of a session in a story's workspace: the story id itself, or
/// `<storyId>#<slug>` for a named one (the slug cut to [`STORY_SESSION_LABEL_MAX`]), so
/// several sessions share the story's worktree and workspace. `None` when `name` has no
/// letters or digits.
pub(crate) fn story_session_handle(story_id: &str, name: Option<&str>) -> Option<String> {
    let Some(name) = name else {
        return Some(story_id.to_string());
    };
    let slug = worker_slug(name)?;
    let label = slug[..slug.len().min(STORY_SESSION_LABEL_MAX)].trim_end_matches('-');
    Some(format!("{story_id}#{label}"))
}

/// A team-workspace session's handle. The slashes mean it can never equal a story id.
pub(crate) fn team_workspace_handle(org: &str, team: &str, slug: &str) -> String {
    format!("{}/{org}/{team}/{slug}", crate::context_repo::WORKER_DIR)
}

/// The focus rule for an open: an explicit `mode` or `headed` wins; with neither, a
/// targeted open stays unfocused so an agent launching agents never steals the viewport,
/// while the legacy story open keeps its focused default.
pub(crate) fn resolve_open_mode(
    mode: Option<&str>,
    headed: Option<bool>,
    targeted: bool,
) -> Result<WorkspaceMode, GateRefusal> {
    if targeted && mode.is_none() && headed.is_none() {
        return Ok(WorkspaceMode::VisibleUnfocused);
    }
    WorkspaceMode::resolve(mode, headed)
}

/// Whether a caller-supplied name is usable as one directory segment: exactly one
/// normal path component, so it can never climb out of (or reroot) the dir it is
/// joined onto.
pub(crate) fn is_single_path_segment(value: &str) -> bool {
    let mut components = Path::new(value).components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(segment)), None) if segment == std::ffi::OsStr::new(value)
    )
}

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// resolve the caller's `maxRuntime` into milliseconds and apply the
/// cross-field constraints that must hold before anything spawns.
///
/// - Absent / blank → `Ok(None)` (no limit, today's behaviour).
/// - Parsed via the routine `parse_max_runtime` (1m–24h, same strings).
/// - Combined with a non-local `machineId` → refused with a legible reason, so
///   `open_workspace` can return an `invalid` result without consuming a slot.
pub(crate) fn resolve_max_runtime(
    raw: Option<&str>,
    machine_id: &Option<String>,
) -> Result<Option<u64>, String> {
    let Some(trimmed) = non_blank(raw) else {
        return Ok(None);
    };
    if let Some(machine) = machine_id {
        if machine != super::machines::LOCAL_MACHINE_ID {
            return Err(format!(
                "maxRuntime {trimmed:?} is enforced on This Mac only; drop machineId or set it to \"local\""
            ));
        }
    }
    let duration =
        crate::routines::parse_max_runtime(trimmed).map_err(|error| error.to_string())?;
    let ms = u64::try_from(duration.as_millis())
        .map_err(|_| format!("maxRuntime {trimmed:?} is too large to represent as milliseconds"))?;
    Ok(Some(ms))
}

/// The agent an open launches, prepared against `team`: the one `req` names, else the
/// preferred built-in (its unattended twin for a background open).
async fn resolve_agent_launch(
    host: &dyn OpenHost,
    team: ActiveTeam,
    req: &OpenRequest,
) -> Result<AgentLaunch, String> {
    let mode = agent_mode_override(req);
    // No agent named: a background open nobody watches runs the unattended built-in, so it
    // never sits on a permission prompt; a visible one runs the ordinary built-in.
    let reference = non_blank(req.agent.as_deref())
        .map(str::to_string)
        .unwrap_or_else(|| {
            crate::agents::builtin::default_key(mode == Some(adapter::LaunchMode::Headless))
                .to_string()
        });
    let prompt = non_blank(req.initial_prompt.as_deref());
    host.prepare_agent(&reference, &team, prompt, mode).await
}

/// The mode an open runs its agent in: the caller's explicit `mode` (or legacy `headed`)
/// when it sent one, so an unattended background open gets the agent's headless
/// permissions; with neither, the agent's own declared mode.
fn agent_mode_override(req: &OpenRequest) -> Option<adapter::LaunchMode> {
    if req.mode.is_none() && req.headed.is_none() {
        return None;
    }
    let mode = resolve_open_mode(req.mode.as_deref(), req.headed, true).ok()?;
    Some(if mode.is_visible() {
        adapter::LaunchMode::Interactive
    } else {
        adapter::LaunchMode::Headless
    })
}

/// The reason a mode refusal carries, as an input error rather than a refusal.
fn mode_error(refusal: GateRefusal) -> String {
    match refusal {
        GateRefusal::Denied(reason) => reason,
        other => other.to_string(),
    }
}

/// The `alreadyLive` result. A headed open brings the running session forward, as the
/// legacy open does on an idempotent reopen, and reports what showing it did.
async fn already_live(
    host: &dyn OpenHost,
    mode: WorkspaceMode,
    handle: String,
    name: String,
    req: &OpenRequest,
) -> OpenResult {
    let reveal = if mode.is_focused() {
        Some(host.show(&handle).await)
    } else {
        None
    };
    let mut result = already_live_result(handle, name, req);
    if let Some(RevealOutcome::NotShown(reason)) = &reveal {
        result.message = format!(
            "{} It could not be shown: {}.",
            result.message,
            reason.trim().trim_end_matches('.')
        );
    }
    result.reveal = reveal;
    result
}

fn already_live_result(handle: String, name: String, req: &OpenRequest) -> OpenResult {
    let identity_asked = launch_label(req.role.as_deref()).is_some()
        || launch_label(req.avatar.as_deref()).is_some();
    let message = if identity_asked {
        format!(
            "{name} is already running; its role and avatar were not changed (they are set when a session is launched)."
        )
    } else {
        format!("{name} is already running.")
    };
    // the open didn't launch anything, so a `maxRuntime` the
    // caller sent arms nothing. Surface it as a warning without touching the
    // existing deadline (if any) so a reader can tell it was ignored.
    let mut warnings = Vec::new();
    if req.max_runtime_ms.is_some() {
        warnings.push(LaunchWarning {
            code: LaunchWarningCode::MaxRuntimeIgnored,
            message: "maxRuntime ignored: session already live".to_string(),
        });
    }
    OpenResult {
        status: OpenStatus::AlreadyLive,
        message,
        handle: Some(handle),
        name: Some(name),
        warnings,
        reveal: None,
        prepared: None,
        worktree: None,
    }
}

/// The refusal for a story open whose `team` names a team other than the story's own:
/// `team` is a check for a story, never a lookup key.
fn story_team_mismatch(story_id: &str, story: &ActiveTeam, team: Option<&str>) -> Option<String> {
    non_blank(team)
        .filter(|team| *team != story.team)
        .map(|team| format!("story {story_id} is in team {}, not {team}", story.team))
}

/// The org an open looks its story or team up in: a team agent's own org (an `org`
/// naming any other is refused as not found, so it learns nothing about other orgs),
/// else the `org` the caller named, else none (the story is searched for by id).
pub(crate) fn caller_org(
    scope: Option<&crate::context_repo::caller_scope::CallerScope>,
    requested: Option<&str>,
) -> Result<Option<String>, String> {
    use crate::context_repo::caller_scope::{CallerScope, MISSING};
    let requested = non_blank(requested);
    match scope {
        None | Some(CallerScope::Human) => match requested {
            // The org names a dir under the user's home: a slug, never a path.
            Some(org) if crate::context_repo::org_slug::validate_org_slug(org).is_some() => {
                Err(format!("invalid: unknown org {org:?}"))
            }
            requested => Ok(requested.map(str::to_string)),
        },
        Some(CallerScope::Team(view)) => match requested {
            Some(org) if org != view.org() => Err(MISSING.into()),
            _ => Ok(Some(view.org().to_string())),
        },
        Some(CallerScope::Unknown) => Err(MISSING.into()),
    }
}

/// Why a caller's story open was refused, as that caller may learn it. A person gets the
/// reason. A team agent gets one sentence, naming the id and its own team, for every
/// reason that names the story (not found, not its to open, ambiguous, a candidate file)
/// or is the scoped not-found: the lookup sees every team in the agent's org, so those
/// reasons could describe a story it cannot see. A reason that does not name the story
/// (its context, an index not ready yet) reaches it unchanged. An unknown caller learns
/// nothing.
pub(crate) fn story_open_refusal(
    scope: &crate::context_repo::caller_scope::CallerScope,
    story_id: &str,
    error: String,
) -> String {
    use crate::context_repo::caller_scope::{CallerScope, MISSING};
    log::debug!("open_workspace: story {story_id:?} refused: {error}");
    match scope {
        CallerScope::Human => error,
        CallerScope::Team(_) if error != MISSING && !error.contains(story_id) => error,
        CallerScope::Team(view) => {
            let team = view.reader_team();
            format!(
                "No story {story_id} that the {team} team can open. Check the story id, and \
                 that the story's team is {team} or a team {team} can launch into."
            )
        }
        CallerScope::Unknown => MISSING.into(),
    }
}

/// The MCP handler's check before an agent opens `story_id`: the story is looked up in the
/// agent's own org, whichever org the app has selected, and only among the stories its team
/// can see. A refusal says what to check without saying whether a story it cannot see
/// exists. A blank id is left to the open, which says it needs one. Blocking: a cold or
/// rescanned org is a full scan.
pub(crate) fn agent_story_visible<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    active: &super::route_to_context::ActiveContext,
    scope: &crate::context_repo::caller_scope::CallerScope,
    story_id: &str,
    org: Option<&str>,
) -> Result<Option<super::dashboard_launch::StoryPathResolution>, String> {
    if non_blank(Some(story_id)).is_none() {
        return Ok(None);
    }
    let refuse = |error: String| story_open_refusal(scope, story_id, error);
    let org = caller_org(Some(scope), org)
        .map_err(&refuse)?
        .ok_or_else(|| refuse(crate::context_repo::caller_scope::MISSING.into()))?;
    // A story just written to a held org is found by one rescan here, so the open's later
    // lookups find it without rescanning again.
    super::dashboard_launch::resolve_story_path_in_org_as(app, active, &org, story_id, scope)
        .map(Some)
        .map_err(refuse)
}

/// Where a resolved open will launch, before anything is written.
struct Planned {
    target: LaunchTarget,
    handle: String,
    name: String,
    story: Option<String>,
    /// The captured team and canonical content root used to prepare its destination.
    stage: Option<(ActiveTeam, PathBuf)>,
    agent: AgentLaunch,
    /// The team the session runs in, when known.
    team: Option<LaunchTeam>,
}

/// Resolve `req` into a launch plan, or the `invalid` result for input it cannot use.
async fn plan_open(
    host: &dyn OpenHost,
    active: &ActiveTeam,
    story_target: bool,
    req: &OpenRequest,
    scope: Option<&crate::context_repo::caller_scope::CallerScope>,
) -> Result<Planned, OpenResult> {
    if story_target {
        let Some(story_id) = non_blank(req.story_id.as_deref()) else {
            return Err(OpenResult::invalid("story_id is required to open a story"));
        };
        // A story launches in its own team's context, so its agent resolves there too.
        // An agent's refusal is mapped like the open's first lookup, which the index can
        // change under before this one.
        let story_team = host
            .story_team(story_id, non_blank(req.org.as_deref()))
            .await
            .map_err(|reason| {
                OpenResult::invalid(&match scope {
                    Some(scope) => story_open_refusal(scope, story_id, reason),
                    None => reason,
                })
            })?;
        if let Some(reason) = story_team_mismatch(story_id, &story_team, req.team.as_deref()) {
            return Err(OpenResult::invalid(&reason));
        }
        let agent = resolve_agent_launch(host, story_team.clone(), req)
            .await
            .map_err(|reason| OpenResult::invalid(&reason))?;
        let session_name = non_blank(req.name.as_deref());
        let Some(handle) = story_session_handle(story_id, session_name) else {
            return Err(OpenResult::invalid(&format!(
                "the name {:?} has no letters or digits",
                session_name.unwrap_or_default()
            )));
        };
        let name = session_name.unwrap_or(story_id);
        return Ok(Planned {
            target: LaunchTarget::Story {
                story_id: story_id.to_string(),
                handle: handle.clone(),
                team: story_team,
            },
            handle,
            name: name.to_string(),
            story: Some(story_id.to_string()),
            stage: None,
            agent,
            // The story launch spine records the story's own team.
            team: None,
        });
    }

    let org = non_blank(req.org.as_deref()).unwrap_or(&active.org);
    let team = non_blank(req.team.as_deref()).unwrap_or(&active.team);
    let Some(name) = non_blank(req.name.as_deref()) else {
        return Err(OpenResult::invalid(
            "name is required to open a team workspace",
        ));
    };
    let Some(slug) = worker_slug(name) else {
        return Err(OpenResult::invalid(&format!(
            "the name {name:?} has no letters or digits"
        )));
    };
    let scope = ActiveTeam {
        org: org.to_string(),
        team: team.to_string(),
    };
    let agent = resolve_agent_launch(host, scope, req)
        .await
        .map_err(|reason| OpenResult::invalid(&reason))?;
    let content_root = host
        .team_content_root(org, team)
        .await
        .map_err(|reason| OpenResult::invalid(&reason))?;
    let dir = PathBuf::new();
    let handle = team_workspace_handle(org, team, &slug);
    Ok(Planned {
        target: LaunchTarget::Dir {
            handle: handle.clone(),
            cwd: dir.clone(),
            scope: ActiveTeam {
                org: org.to_string(),
                team: team.to_string(),
            },
        },
        handle,
        name: name.to_string(),
        story: non_blank(req.story_id.as_deref()).map(str::to_string),
        stage: Some((
            ActiveTeam {
                org: org.to_string(),
                team: team.to_string(),
            },
            content_root,
        )),
        agent,
        team: Some(LaunchTeam {
            org: org.to_string(),
            team: team.to_string(),
        }),
    })
}

/// Open (or find already running) the session `req` names. `active` is the UI-active
/// org and team. `Err` is a launch failure other than the spawn cap (the story has no
/// worktree, the agent CLI is missing, …), surfaced to the caller as a tool error.
pub(crate) async fn open_agent(
    host: &dyn OpenHost,
    active: &ActiveTeam,
    req: &OpenRequest,
) -> Result<OpenResult, String> {
    open_agent_as(host, active, req, None).await
}

/// [`open_agent`] for a verified caller: a story refusal reaches `scope` as
/// [`story_open_refusal`] words it.
async fn open_agent_as(
    host: &dyn OpenHost,
    active: &ActiveTeam,
    req: &OpenRequest,
    scope: Option<&crate::context_repo::caller_scope::CallerScope>,
) -> Result<OpenResult, String> {
    let story_target = match req.target.trim() {
        "story" => true,
        "team-workspace" => false,
        other => {
            return Ok(OpenResult::invalid(&format!(
                "unknown target {other:?} (use story or team-workspace)"
            )))
        }
    };
    let mode = match resolve_open_mode(req.mode.as_deref(), req.headed, true) {
        Ok(mode) => mode,
        Err(refusal) => return Ok(OpenResult::invalid(&mode_error(refusal))),
    };
    // The team names a dir under the user's home: one plain segment, never a path.
    if let Some(team) = non_blank(req.team.as_deref()) {
        if !is_single_path_segment(team) {
            return Ok(OpenResult::invalid(&format!("unknown team {team:?}")));
        }
    }
    if let Some(org) = non_blank(req.org.as_deref()) {
        if crate::context_repo::org_slug::validate_org_slug(org).is_some() {
            return Ok(OpenResult::invalid(&format!("unknown org {org:?}")));
        }
    }
    let requested_machine = non_blank(req.machine_id.as_deref());
    if requested_machine.is_some() && !story_target {
        return Ok(OpenResult::invalid(
            "machine_id launches into a story's workspace on that machine; a team folder runs on This Mac",
        ));
    }
    let planned = match plan_open(host, active, story_target, req, scope).await {
        Ok(planned) => planned,
        Err(invalid) => return Ok(invalid),
    };
    if let LaunchTarget::Story { story_id, team, .. } = &planned.target {
        let team = team.clone();
        let place = host.story_workspace_place(&team, story_id).await;
        match story_machine(story_id, place, requested_machine) {
            Ok(Some(machine_id)) => {
                if req.max_runtime_ms.is_some() {
                    return Ok(OpenResult::invalid(
                        "maxRuntime is enforced on This Mac only; this story's workspace lives on a machine",
                    ));
                }
                return open_on_machine(host, mode, planned, team, machine_id, req).await;
            }
            Ok(None) => {}
            Err(reason) => return Ok(OpenResult::invalid(&reason)),
        }
    }
    let Planned {
        target,
        handle,
        name,
        story,
        stage,
        mut agent,
        team,
    } = planned;

    // One open at a time per handle: another open of this name is staging or launching,
    // so this one stages nothing and removes nothing. Held until this launch settles.
    let _claim = host.claim_open(&handle).await;
    // A running session keeps its dir as it is: nothing is restaged under a live agent.
    // A launch from outside open_workspace racing this check is still settled by the
    // registry's reserve.
    if host.is_live(&handle) {
        let worktree = if story_target {
            None
        } else {
            match host.live_context(&handle, req).await {
                Ok(worktree) => worktree,
                Err(error) => return Ok(OpenResult::invalid(&error)),
            }
        };
        let mut result = already_live(host, mode, handle, name, req).await;
        result.worktree = worktree;
        return Ok(result);
    }
    let context_claim = match &stage {
        Some((_, content)) => Some(crate::install::worktree_index::setup_lock(content).await),
        None => None,
    };
    let mut destination = match stage {
        Some((scope, content)) => match host
            .prepare_context_worktree(
                &scope,
                content,
                &name,
                req.worktree.as_deref(),
                req.create_worktree,
                &agent,
            )
            .await
        {
            Ok(destination) => Some(destination),
            Err(error) => return Ok(OpenResult::invalid(&error)),
        },
        None => None,
    };
    if let Some(destination) = destination.as_mut().filter(|d| d.created) {
        if let Err(error) = host
            .context_created_by(destination, req.launched_by.as_deref())
            .await
        {
            host.rollback_context_worktree(destination).await;
            return Ok(OpenResult::invalid(&error));
        }
    }
    // A creator can still remove its destination on spawn failure. Keep other launches
    // out until that settles; retained destinations allow independent spawns.
    let _created_context_claim =
        context_claim.filter(|_| destination.as_ref().is_some_and(|d| d.created));
    let target = match (&destination, target) {
        (Some(destination), LaunchTarget::Dir { handle, scope, .. }) => LaunchTarget::Dir {
            handle,
            scope,
            cwd: destination.record.launch_cwd.clone(),
        },
        (_, target) => target,
    };

    // With no prompt of its own, an agent starts with its default one.
    let initial_prompt = non_blank(req.initial_prompt.as_deref())
        .map(str::to_string)
        .or_else(|| agent.default_prompt.clone());
    let launched_by = non_blank(req.launched_by.as_deref()).map(str::to_string);
    let provenance = agent.provenance(None, &handle);
    let mut warnings = agent.warnings.clone();
    if let Some(d) = &destination {
        warnings.extend(d.warnings.clone());
    }
    agent.queue_role =
        crate::queue_roles::effective_role(agent.queue_role.as_deref(), req.queue_role.as_ref())?;
    let launch = Launch {
        target,
        initial_prompt: initial_prompt.clone(),
        record: LaunchRecord {
            meta: LaunchMeta {
                launcher_context: None,
                display_name: Some(name.clone()),
                launched_by,
                story,
                team,
                role: launch_label(req.role.as_deref()),
                queue_role: agent.queue_role.clone(),
                role_join_error: None,
                avatar: launch_label(req.avatar.as_deref()),
                continues: None,
                reviews: non_blank(req.reviews.as_deref()).map(str::to_string),
            },
            started_with: initial_prompt,
            // carry the budget onto the entry; `None` → no deadline
            // is stamped at bind and the sweep never touches the launch.
            max_runtime_ms: req.max_runtime_ms,
        },
        view: session_view(mode),
        agent,
        provenance,
    };
    let outcome = host.launch(launch).await;
    if let (Err(_), Some(destination)) = (&outcome, &destination) {
        host.rollback_context_worktree(destination).await;
    }
    let (status, message) = match outcome {
        Ok(launched) if launched.fresh => {
            warnings.extend(launched.warnings);
            if let Some(destination) = &destination {
                host.context_used(destination, &name).await;
            }
            if mode.is_visible() {
                host.reveal(&handle, mode.is_focused());
            }
            (OpenStatus::Opened, format!("Opened {name}."))
        }
        Ok(_) => return Ok(already_live(host, mode, handle, name, req).await),
        Err(LaunchError::Busy) => (
            OpenStatus::Busy,
            format!(
                "{name} was not opened: too many agent sessions are running. Try again when one finishes."
            ),
        ),
        Err(LaunchError::Other(error)) => return Err(error),
    };
    if let Some(error) = host.role_join_error(&handle) {
        warnings.push(LaunchWarning {
            code: LaunchWarningCode::RoleNotJoined,
            message: format!("Not joined: {error}"),
        });
    }
    let prepared = host.take_prepared(&handle);
    Ok(OpenResult {
        status,
        handle: Some(handle),
        name: Some(name),
        message,
        warnings,
        reveal: None,
        prepared,
        worktree: destination.map(|d| ContextWorktreeResult {
            id: d.record.id,
            name: d.record.name,
            branch: d.record.branch,
            created: d.created,
        }),
    })
}

/// The machine a story launch runs on, or `None` for This Mac. A workspace stays where it
/// was created: with no machine asked for, the launch goes where the workspace lives; a
/// machine asked for must be where it lives, or where a new one is created. `Err` is why
/// the asked-for machine can't be used.
pub(crate) fn story_machine(
    story_id: &str,
    place: Option<WorkspacePlace>,
    requested: Option<&str>,
) -> Result<Option<String>, String> {
    let requested = requested.map(str::trim).filter(|m| !m.is_empty());
    let lives = |at: &WorkspacePlace| {
        let at = match at {
            WorkspacePlace::ThisMac => "This Mac".to_string(),
            WorkspacePlace::Machine(id) => format!("machine {id}"),
        };
        format!(
            "{story_id}'s workspace already lives on {at}; a workspace stays where it was created"
        )
    };
    match (requested, place) {
        (None, Some(WorkspacePlace::Machine(at))) => Ok(Some(at)),
        (None, _) => Ok(None),
        (Some(super::machines::LOCAL_MACHINE_ID), Some(at @ WorkspacePlace::Machine(_))) => {
            Err(lives(&at))
        }
        (Some(super::machines::LOCAL_MACHINE_ID), _) => Ok(None),
        (Some(want), Some(WorkspacePlace::Machine(at))) if at == want => Ok(Some(at)),
        (Some(_), Some(at)) => Err(lives(&at)),
        (Some(want), None) => Ok(Some(want.to_string())),
    }
}

/// Launch the planned story agent on `machine_id`: as a tab in the story's workspace
/// there, the workspace created on the machine first when it has none.
async fn open_on_machine(
    host: &dyn OpenHost,
    mode: WorkspaceMode,
    planned: Planned,
    team: ActiveTeam,
    machine_id: String,
    req: &OpenRequest,
) -> Result<OpenResult, String> {
    if !mode.is_visible() {
        return Ok(OpenResult::invalid(
            "an agent on a machine opens as a tab there; omit headless",
        ));
    }
    let Planned {
        handle,
        name,
        story,
        agent,
        ..
    } = planned;
    if !adapter::desktop(agent.runtime).supports_remote_launch {
        return Ok(OpenResult::invalid(&format!(
            "{} does not support remote launches yet; launch locally",
            agent.descriptor().name
        )));
    }
    let story_id = story.unwrap_or_default();
    // One open of this name on this machine at a time.
    let _claim = host.claim_open(&format!("{machine_id}|{handle}")).await;
    let mut warnings = agent.warnings.clone();
    let launch = RemoteLaunch {
        machine_id,
        team,
        story_id,
        name: name.clone(),
        agent_key: agent.agent.key.to_string(),
        initial_prompt: non_blank(req.initial_prompt.as_deref()).map(str::to_string),
        launched_by: non_blank(req.launched_by.as_deref()).map(str::to_string),
        role: launch_label(req.role.as_deref()),
        queue_role: Some(crate::queue_roles::effective_role(
            agent.queue_role.as_deref(),
            req.queue_role.as_ref(),
        )?),
        avatar: launch_label(req.avatar.as_deref()),
        focus: mode.is_focused(),
    };
    let launched = host.launch_remote(launch).await?;
    if !launched.fresh {
        return Ok(already_live_result(launched.handle, name, req));
    }
    if let Some(error) = host.role_join_error(&launched.handle) {
        warnings.push(LaunchWarning {
            code: LaunchWarningCode::RoleNotJoined,
            message: format!("Not joined: {error}"),
        });
    }
    Ok(OpenResult {
        status: OpenStatus::Opened,
        message: format!("Opened {name} on {}.", launched.machine),
        handle: Some(launched.handle),
        name: Some(name),
        warnings,
        reveal: None,
        prepared: None,
        worktree: None,
    })
}

/// The app side of the `open_workspace` verb, a port so the verb's routing (legacy story
/// open vs targeted launch) runs in tests without a running app.
#[async_trait::async_trait]
pub(crate) trait OpenWorkspacePort: Send + Sync {
    /// The legacy story open through the shipped spine, launching the preferred built-in.
    /// `org` is the caller's hint for the story's org.
    async fn launch_story(
        &self,
        story_id: &str,
        org: Option<&str>,
        initial_prompt: Option<&str>,
        view: super::mcp_workspace::SessionView,
    ) -> Result<super::mcp_workspace::LaunchOutcome, String>;
    /// The UI-active team and the host a targeted open launches through; `Err` when no
    /// team is active or the signed-in context cannot be read.
    async fn targeted(&self) -> Result<(ActiveTeam, Box<dyn OpenHost>), String>;
    /// Surface a session in the viewer, stealing focus only when `focus`.
    fn reveal(&self, handle: &str, focus: bool);
    /// Ask the viewer to open a story's workspace on a machine, creating it
    /// there on first open, and wait for its verdict. `Err` is the reason it did
    /// not open (or that no verdict came within the timeout).
    async fn open_on_machine(
        &self,
        _story_id: &str,
        _machine_id: &str,
        _focus: bool,
    ) -> Result<(), String> {
        Err("opening on a machine is unavailable here".into())
    }
}

/// The whole `open_workspace` verb after its static verb check. Without `target` it is
/// the story open it has always been: result `{handle, mode, headed, reconciled}`,
/// headed by default, and any failure an error. With `target` the result is the typed
/// `{status, handle, name, message}`, and input it cannot use is `invalid`. `Err` is the
/// message of the tool error.
#[cfg(test)]
pub(crate) async fn open_workspace_call(
    port: &dyn OpenWorkspacePort,
    params: crate::mcp::handler::OpenWorkspaceParams,
) -> Result<serde_json::Value, String> {
    open_workspace_call_for(port, params, None).await
}

/// Architecture: select caller placement before the team-workspace source preflight.
pub(crate) async fn team_workspace_preflight<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    scope: &crate::context_repo::caller_scope::CallerScope,
    requested: Option<&str>,
) -> Result<super::route_to_context::ActiveContext, String> {
    let context = scope.context(app).await?;
    let selected = requested
        .and_then(super::dashboard_launch::launch_team_from_key)
        .or_else(|| {
            context
                .team_name
                .as_ref()
                .map(|team| super::dashboard_launch::LaunchTeam {
                    org: context.org_slug.clone(),
                    team: requested.unwrap_or(team).into(),
                })
        })
        .ok_or(crate::context_repo::caller_scope::MISSING)?;
    super::dashboard_launch::resolve_managed_team_content_root_for(
        &context.codesense_home,
        &context.principal(),
        &selected.org,
        &selected.team,
        scope,
    )?;
    Ok(context)
}

pub(crate) async fn open_workspace_call_for(
    port: &dyn OpenWorkspacePort,
    params: crate::mcp::handler::OpenWorkspaceParams,
    caller: Option<(
        &Path,
        &crate::domain::Principal,
        &crate::context_repo::caller_scope::CallerScope,
    )>,
) -> Result<serde_json::Value, String> {
    use super::interactive_gate::InteractiveGate;
    let machine_id = non_blank(params.machine_id.as_deref()).map(str::to_string);
    // resolve the per-launch budget. Absent/blank → no limit; a bad
    // string, out-of-range (1m–24h), or combination with a non-local machine is
    // refused `invalid` BEFORE anything spawns, so no cap slot is held.
    let max_runtime_ms = match resolve_max_runtime(params.max_runtime.as_deref(), &machine_id) {
        Ok(value) => value,
        Err(reason) => return Ok(serde_json::json!(OpenResult::invalid(&reason))),
    };
    if params.target.is_none() && max_runtime_ms.is_some() {
        return Ok(serde_json::json!(OpenResult::invalid(
            "maxRuntime requires target (story or team-workspace)"
        )));
    }
    if let Some(target) = params.target {
        let (mut active, host) = match port.targeted().await {
            Ok(resolved) => resolved,
            Err(reason) => return Ok(serde_json::json!(OpenResult::invalid(&reason))),
        };
        let story = target.trim() == "story";
        if story && caller.is_some() && non_blank(params.story_id.as_deref()).is_none() {
            return Ok(serde_json::json!(OpenResult::invalid(
                "target: story needs story_id"
            )));
        }
        let story_id = params.story_id.clone().unwrap_or_default();
        let refuse = |error: String| match caller {
            Some((_, _, scope)) if story => story_open_refusal(scope, &story_id, error),
            _ => error,
        };
        let org = caller_org(caller.map(|(_, _, scope)| scope), params.org.as_deref())
            .map_err(&refuse)?;
        let mut request = OpenRequest {
            target,
            story_id: params.story_id,
            name: params.name,
            org,
            team: params.team,
            worktree: params.worktree,
            create_worktree: params.create_worktree.unwrap_or(false),
            agent: params.agent,
            initial_prompt: params.initial_prompt,
            launched_by: params.launched_by,
            role: params.role,
            queue_role: params.queue_role,
            avatar: params.avatar,
            reviews: params.reviews,
            mode: params.mode,
            headed: params.headed,
            machine_id,
            max_runtime_ms,
        };
        if non_blank(request.reviews.as_deref()).is_some() && request.target.trim() != "story" {
            return Ok(serde_json::json!(OpenResult::invalid(
                "reviews needs target: story (it names the story task a reviewer reviews)"
            )));
        }
        if let Some((home, principal, scope)) = caller {
            if let crate::context_repo::caller_scope::CallerScope::Team(view) = scope {
                active = ActiveTeam {
                    org: view.org().into(),
                    team: view.reader_team().into(),
                };
            }
            let team = match request.target.trim() {
                "story" => host
                    .story_team(&story_id, request.org.as_deref())
                    .await
                    .map_err(&refuse)?,
                "team-workspace" => ActiveTeam {
                    org: request.org.clone().unwrap_or_else(|| active.org.clone()),
                    team: request.team.clone().unwrap_or_else(|| active.team.clone()),
                },
                _ => return Ok(serde_json::json!(OpenResult::invalid("unknown target"))),
            };
            scope
                .require_launch_team(home, principal, &team.org, &team.team)
                .map_err(&refuse)?;
            if let Some(reference) = non_blank(request.agent.as_deref()) {
                let key = crate::agents::launch::find_agent_for(
                    home,
                    principal,
                    reference,
                    Some((&team.org, &team.team)),
                    scope,
                )?;
                request.agent = Some(key.to_string());
            }
        }
        let result = open_agent_as(
            host.as_ref(),
            &active,
            &request,
            caller.map(|(_, _, scope)| scope),
        )
        .await?;
        return Ok(serde_json::json!(result));
    }
    if non_blank(params.agent.as_deref()).is_some() {
        return Ok(serde_json::json!(OpenResult::invalid(
            "agent needs a target (story or team-workspace)"
        )));
    }
    if non_blank(params.reviews.as_deref()).is_some() {
        return Ok(serde_json::json!(OpenResult::invalid(
            "reviews needs target: story (it names the story task a reviewer reviews)"
        )));
    }
    // SW-WA-F5 (AC-1/AC-5): resolve the requested visibility mode (default headed;
    // headless opt-in) and admit it — the requested mode is the focus-consent decision.
    let mode = resolve_open_mode(params.mode.as_deref(), params.headed, false)
        .map_err(|refusal| refusal.to_string())?;
    InteractiveGate::admit_workspace_mode(mode).map_err(|refusal| refusal.to_string())?;
    let story_id = params
        .story_id
        .ok_or_else(|| "story_id is required".to_string())?;
    let org =
        caller_org(caller.map(|(_, _, scope)| scope), params.org.as_deref()).map_err(|error| {
            match caller {
                Some((_, _, scope)) => story_open_refusal(scope, &story_id, error),
                None => error,
            }
        })?;
    if let Some((home, principal, scope)) = caller {
        let (_, host) = port.targeted().await?;
        let team = host
            .story_team(&story_id, org.as_deref())
            .await
            .map_err(|error| story_open_refusal(scope, &story_id, error))?;
        scope
            .require_launch_team(home, principal, &team.org, &team.team)
            .map_err(|error| story_open_refusal(scope, &story_id, error))?;
        if let Some(reason) = story_team_mismatch(&story_id, &team, params.team.as_deref()) {
            return Err(format!("invalid: {reason}"));
        }
    } else if non_blank(params.team.as_deref()).is_some() {
        let (_, host) = port.targeted().await?;
        let team = host.story_team(&story_id, org.as_deref()).await?;
        if let Some(reason) = story_team_mismatch(&story_id, &team, params.team.as_deref()) {
            return Err(format!("invalid: {reason}"));
        }
    }
    if let Some(machine_id) = machine_id {
        if !mode.is_visible() {
            return Err("a workspace on a machine opens visibly; omit headless".into());
        }
        // SW-FX-131 AC1/AC2: the call returns once the viewer has confirmed the workspace is
        // open on the machine; a failure or a timeout is the tool error with its reason.
        port.open_on_machine(&story_id, &machine_id, mode.is_focused())
            .await?;
        return Ok(serde_json::json!({
            "status": "opened",
            "handle": story_id,
            "machineId": machine_id,
            "mode": mode.as_wire(),
            "headed": mode.is_focused(),
        }));
    }
    // AC-1/AC-2: spawn via the trusted-side spine; hydrates-if-missing. Typed refusals
    // (`busy`/`denied:`/`unavailable:`) surface as the tool error.
    let outcome = port
        .launch_story(
            &story_id,
            org.as_deref(),
            params.initial_prompt.as_deref(),
            session_view(mode),
        )
        .await?;
    // SW-WA-F5 (AC-1): a visible mode surfaces the workspace as a real tab in the
    // viewer, focused when headed (also on an idempotent reopen). Headless stays
    // background.
    if mode.is_visible() {
        port.reveal(&outcome.handle, mode.is_focused());
    }
    // SW-OR-1b (AC-6): `reconciled` is `{from,to}` when the story's recorded branch
    // converged to the opened branch, else `null`.
    let mut result = serde_json::json!({
        "handle": outcome.handle,
        "mode": mode.as_wire(),
        "headed": mode.is_focused(),
        "reconciled": outcome.reconciled,
    });
    if !outcome.warnings.is_empty() {
        result["warnings"] = serde_json::json!(outcome.warnings);
    }
    Ok(result)
}

/// Prepare the agent an open names, in `mode` when the caller chose one, else in the mode
/// the agent declares. The open always spawns a terminal session (shown or in the
/// background); the mode decides the permissions the agent runs with.
pub(crate) fn open_workspace_prepare(
    home: &Path,
    principal: &crate::domain::Principal,
    reference: &str,
    target_team: Option<(String, String)>,
    prompt: Option<&str>,
    mode: Option<adapter::LaunchMode>,
) -> Result<AgentLaunch, String> {
    crate::agents::launch::prepare_named(
        home,
        principal,
        reference,
        crate::agents::launch::PrepareOptions {
            account: None,
            target_team,
            mode_override: mode,
            prompt,
        },
    )
}

/// What the app's launch sheet asks for when a person launches an agent: the same targeted
/// open an agent makes over MCP, always shown and focused, since the person is watching.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiLaunchArgs {
    /// `story` or `team-workspace`.
    pub target: String,
    pub story_id: Option<String>,
    pub name: Option<String>,
    pub agent: Option<String>,
    pub initial_prompt: Option<String>,
    /// The manager the person put it under: that session's handle. Display only.
    pub launched_by: Option<String>,
    pub role: Option<String>,
    #[serde(default, deserialize_with = "crate::queue_roles::nullable_role")]
    pub queue_role: Option<Option<String>>,
    pub avatar: Option<String>,
    /// The machine a story launch runs on (`local`: This Mac); see [`OpenRequest`].
    pub machine_id: Option<String>,
    pub team: Option<String>,
    pub org: Option<String>,
    pub worktree: Option<String>,
    pub create_worktree: Option<bool>,
}

impl From<UiLaunchArgs> for OpenRequest {
    fn from(args: UiLaunchArgs) -> Self {
        OpenRequest {
            target: args.target,
            story_id: args.story_id,
            name: args.name,
            team: args.team,
            org: args.org,
            worktree: args.worktree,
            create_worktree: args.create_worktree.unwrap_or(false),
            agent: args.agent,
            initial_prompt: args.initial_prompt,
            launched_by: args.launched_by,
            role: args.role,
            queue_role: args.queue_role,
            avatar: args.avatar,
            reviews: None,
            mode: Some("headed".to_string()),
            headed: None,
            machine_id: args.machine_id,
            // The UI launch sheet does not expose a budget today; the
            // caller is MCP (`open_workspace`) only.
            max_runtime_ms: None,
        }
    }
}

/// Launch what the sheet asks for in the UI-active team through `port`.
pub(crate) async fn launch_from_ui(
    port: &dyn OpenWorkspacePort,
    args: UiLaunchArgs,
) -> Result<OpenResult, String> {
    let (active, host) = match port.targeted().await {
        Ok(resolved) => resolved,
        Err(reason) => return Ok(OpenResult::invalid(&reason)),
    };
    open_agent(host.as_ref(), &active, &args.into()).await
}

/// A person launches an agent from the app: a named tab in a story's workspace, or its own
/// team workspace, under the manager they picked, with the role and face they chose.
#[tauri::command]
pub async fn agent_launch_open(
    app: tauri::AppHandle,
    args: UiLaunchArgs,
) -> Result<OpenResult, String> {
    launch_from_ui(&AppOpenWorkspacePort { app }, args).await
}

/// The app-backed [`OpenWorkspacePort`].
pub(crate) struct AppOpenWorkspacePort {
    pub app: tauri::AppHandle,
}

#[async_trait::async_trait]
impl OpenWorkspacePort for AppOpenWorkspacePort {
    async fn launch_story(
        &self,
        story_id: &str,
        org: Option<&str>,
        initial_prompt: Option<&str>,
        view: super::mcp_workspace::SessionView,
    ) -> Result<super::mcp_workspace::LaunchOutcome, String> {
        // The legacy open names no agent: the preferred built-in runs, in the story's own
        // team; a headless open runs its unattended twin, which no prompt ever blocks.
        let owner = super::mcp_workspace::resolve_story_owner(&self.app, story_id, org).await?;
        // The launch resolves in the org just found, so the agent's team is its worktree's.
        let story_org = owner.org.clone();
        let ctx = super::route_to_context::derive_active_context(&self.app).await?;
        let (home, principal) = (ctx.codesense_home.clone(), ctx.principal());
        let mode = matches!(view, super::mcp_workspace::SessionView::Background)
            .then_some(adapter::LaunchMode::Headless);
        let prompt = initial_prompt.map(str::to_string);
        let agent = super::await_blocking("open_workspace.prepare_story_agent", move || {
            open_workspace_prepare(
                &home,
                &principal,
                &crate::agents::builtin::default_key(mode.is_some()).to_string(),
                Some((owner.org, owner.team)),
                prompt.as_deref(),
                mode,
            )
        })
        .await?;
        super::mcp_workspace::launch_story_session(
            &self.app,
            story_id,
            Some(&story_org),
            initial_prompt,
            agent,
            None,
            view,
        )
        .await
    }

    async fn targeted(&self) -> Result<(ActiveTeam, Box<dyn OpenHost>), String> {
        let ctx = super::route_to_context::derive_active_context(&self.app).await?;
        let team = ctx
            .team_name
            .clone()
            .ok_or_else(|| "no team is selected".to_string())?;
        let active = ActiveTeam {
            org: ctx.org_slug.clone(),
            team,
        };
        let host = AppOpenHost {
            app: self.app.clone(),
            principal: ctx.principal(),
            codesense_home: ctx.codesense_home,
        };
        Ok((active, Box::new(host)))
    }

    fn reveal(&self, handle: &str, focus: bool) {
        emit_open_workspace_reveal(&self.app, handle, focus);
    }

    async fn open_on_machine(
        &self,
        story_id: &str,
        machine_id: &str,
        focus: bool,
    ) -> Result<(), String> {
        use tauri::{Emitter, Manager};
        let ctx = super::route_to_context::derive_active_context(&self.app).await?;
        let team = ctx
            .team_name
            .as_deref()
            .ok_or_else(|| "no team is selected".to_string())?;
        let team_key = super::pty_layout::team_key(&ctx.org_slug, Some(team));
        super::machines::authorize_terminal_machine(&self.app, machine_id, &team_key).await?;
        let viewer = self.app.state::<crate::mcp::state::McpViewerState>().get();
        if let Some(refusal) = machine_open_conflict(&viewer, &team_key, story_id, machine_id) {
            return Err(refusal);
        }
        let Some(registry) = self.app.try_state::<std::sync::Arc<MachineOpenRegistry>>() else {
            return Err("unavailable: the viewer's open bridge is not running".into());
        };
        let registry = registry.inner().clone();
        let (request_id, reply) = registry.register();
        let _pending = PendingMachineOpen {
            registry: registry.clone(),
            request_id: request_id.clone(),
        };
        self.app
            .emit(
                crate::mcp::handler::OPEN_WORKSPACE_REVEAL_EVENT,
                machine_reveal_payload(story_id, machine_id, &team_key, focus, Some(&request_id)),
            )
            .map_err(|error| error.to_string())?;
        await_machine_open(reply, MACHINE_OPEN_TIMEOUT, story_id, machine_id).await
    }
}

/// Why a story's workspace can't be opened on `machine_id`: it already lives
/// somewhere else (This Mac, or another machine). A workspace stays where it
/// was created; `None` when it doesn't exist yet or already lives there.
pub(crate) fn machine_open_conflict(
    viewer: &serde_json::Value,
    team_key: &str,
    story_id: &str,
    machine_id: &str,
) -> Option<String> {
    let ws = story_workspace(viewer, team_key, story_id)?;
    match ws.machine.as_deref() {
        Some(at) if at == machine_id => None,
        Some(at) => Some(format!(
            "{story_id}'s workspace already lives on machine {at}; a workspace stays where it was created"
        )),
        None => Some(format!(
            "{story_id}'s workspace already lives on This Mac; a workspace stays where it was created"
        )),
    }
}

/// A story's workspace as the viewer holds it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct StoryWorkspace {
    /// The workspace's id in its team (the story file's stem, not always the story id).
    pub id: String,
    /// Showing (not minimized).
    pub open: bool,
    /// The machine it lives on; `None` for This Mac.
    pub machine: Option<String>,
    /// The managed handle and worktree on the machine, once prepared there.
    pub prepared: Option<(String, String)>,
    /// Why creating it failed, when it did.
    pub error: Option<String>,
}

/// The story's workspace in the pushed viewer state, open or minimized.
pub(crate) fn story_workspace(
    viewer: &serde_json::Value,
    team_key: &str,
    story_id: &str,
) -> Option<StoryWorkspace> {
    let is_story = |ws: &&serde_json::Value| {
        ws.get("kind").and_then(|v| v.as_str()) == Some("story")
            && (ws.get("storyId").and_then(|v| v.as_str()) == Some(story_id)
                || ws.get("id").and_then(|v| v.as_str()) == Some(story_id))
    };
    let open = viewer
        .get("workspaces")
        .and_then(|teams| teams.get(team_key))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.iter().find(is_story));
    let minimized = || {
        viewer
            .get("softClosedWorkspaces")
            .and_then(|teams| teams.get(team_key))
            .and_then(|shadows| shadows.as_object())
            .and_then(|shadows| {
                shadows
                    .values()
                    .filter_map(|shadow| shadow.get("workspace"))
                    .find(is_story)
            })
    };
    let ws = open.or_else(minimized)?;
    let text = |value: Option<&serde_json::Value>| {
        value
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let location = ws
        .get("location")
        .filter(|l| l.get("kind").and_then(|v| v.as_str()) == Some("machine"));
    let machine = location.and_then(|l| text(l.get("machineId")));
    let prepared = location.and_then(|l| Some((text(l.get("handle"))?, text(l.get("cwd"))?)));
    let error = (ws.get("status").and_then(|v| v.as_str()) == Some("error"))
        .then(|| text(ws.get("statusError")).unwrap_or_else(|| "its setup failed".to_string()));
    Some(StoryWorkspace {
        id: text(ws.get("id")).unwrap_or_else(|| story_id.to_string()),
        open: open.is_some(),
        machine,
        prepared,
        error,
    })
}

/// What waiting for a story's workspace on a machine sees in one look at the viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MachineWorkspaceWait {
    /// Showing and prepared on the machine: launch in this workspace, at this handle
    /// and worktree.
    Ready {
        workspace_id: String,
        handle: String,
        cwd: String,
    },
    /// Not there or not showing yet: ask the viewer to open (create) it.
    Ask,
    /// Being created, or asked for already: look again.
    Pending,
    /// It can't be used.
    Failed(String),
}

/// One look at the viewer while waiting for `story_id`'s workspace on `machine_id`.
/// `asked`: the viewer was already asked to open it; `seen`: an earlier look saw it.
pub(crate) fn machine_workspace_wait(
    viewer: &serde_json::Value,
    team_key: &str,
    story_id: &str,
    machine_id: &str,
    asked: bool,
    seen: bool,
) -> MachineWorkspaceWait {
    let Some(ws) = story_workspace(viewer, team_key, story_id) else {
        if seen {
            return MachineWorkspaceWait::Failed(format!(
                "{story_id}'s workspace was closed before it was ready on the machine"
            ));
        }
        return if asked {
            MachineWorkspaceWait::Pending
        } else {
            MachineWorkspaceWait::Ask
        };
    };
    if let Some(conflict) = machine_open_conflict(viewer, team_key, story_id, machine_id) {
        return MachineWorkspaceWait::Failed(conflict);
    }
    if let Some(error) = ws.error {
        return MachineWorkspaceWait::Failed(format!(
            "{story_id}'s workspace couldn't be created on the machine: {error}"
        ));
    }
    match (ws.open, ws.prepared) {
        (true, Some((handle, cwd))) => MachineWorkspaceWait::Ready {
            workspace_id: ws.id,
            handle,
            cwd,
        },
        (false, _) if !asked => MachineWorkspaceWait::Ask,
        _ => MachineWorkspaceWait::Pending,
    }
}

/// Whether a remote tab was launched under `name` in this team's `workspace_id` on
/// `machine_id` (its name as launched, not as it may have been renamed since).
pub(crate) fn launched_as(
    record: &super::pty_layout::RemoteShellRecord,
    machine_id: &str,
    team_key: &str,
    workspace_id: &str,
    name: &str,
) -> bool {
    record.machine_id == machine_id
        && record.team_key == team_key
        && record.workspace_id == workspace_id
        && record.name.as_deref() == Some(name)
}

/// How long a launch waits for a story's workspace to be created on a machine (its
/// worktree is copied there first).
const MACHINE_WORKSPACE_WAIT: std::time::Duration = std::time::Duration::from_secs(600);

/// How long a launch waits, once it asked the viewer to open the story's workspace, for
/// the workspace to show up at all. The viewer adds it before copying anything, so one
/// still absent past this was never opened (its story could not be resolved there).
const MACHINE_WORKSPACE_ASK_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// How long `open_workspace` waits for the viewer to confirm a story's workspace is open on a
/// machine (created there on first open) before the call fails. SW-FX-131 AC2.
const MACHINE_OPEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// The viewer's verdict on one machine open, sent back to the waiting `open_workspace` call.
pub type MachineOpenReply = Result<(), String>;

/// SW-FX-131 AC1/AC3: correlated reply bridge for `open_workspace` with a `machineId`. The
/// call emits the reveal carrying a request id and waits here; the viewer confirms (or
/// reports why not) through `resolve_open_workspace_machine`. Same insert/take oneshot shape
/// as [`crate::mcp::state::CanvasRenderRegistry`].
#[derive(Default)]
pub struct MachineOpenRegistry {
    inner: std::sync::Mutex<
        std::collections::HashMap<String, tokio::sync::oneshot::Sender<MachineOpenReply>>,
    >,
    seq: std::sync::atomic::AtomicU64,
}

impl MachineOpenRegistry {
    /// Allocate a process-unique request id and its receiver.
    pub fn register(&self) -> (String, tokio::sync::oneshot::Receiver<MachineOpenReply>) {
        let id = format!(
            "machine-open-{}",
            self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), tx);
        (id, rx)
    }

    /// Remove and return the sender for `request_id`. Consume-once: a late or duplicate reply
    /// finds nothing and is dropped.
    pub fn take(&self, request_id: &str) -> Option<tokio::sync::oneshot::Sender<MachineOpenReply>> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(request_id)
    }
}

/// Removes a pending machine open from the registry when its waiter goes away (the call was
/// cancelled, or it returned), so an abandoned request never leaves a sender behind.
struct PendingMachineOpen {
    registry: std::sync::Arc<MachineOpenRegistry>,
    request_id: String,
}

impl Drop for PendingMachineOpen {
    fn drop(&mut self) {
        let _ = self.registry.take(&self.request_id);
    }
}

/// The viewer reports how a machine open it was asked for ended: `error` absent when the
/// story's workspace is open on the machine, else why it isn't. The waiting call may already
/// be gone (it timed out), in which case the reply is dropped.
#[tauri::command]
pub fn resolve_open_workspace_machine(
    registry: tauri::State<'_, std::sync::Arc<MachineOpenRegistry>>,
    request_id: String,
    error: Option<String>,
) -> Result<(), String> {
    if let Some(tx) = registry.take(&request_id) {
        let reply = match error {
            Some(reason) if !reason.trim().is_empty() => Err(reason),
            _ => Ok(()),
        };
        let _ = tx.send(reply);
    }
    Ok(())
}

/// Wait for the viewer's reply to a machine open, bounded by `timeout`. The result is the
/// tool error text: `unavailable:` for a reason the viewer gave, `unconfirmed:` when no reply
/// came in time (for example the viewer is not running).
async fn await_machine_open(
    reply: tokio::sync::oneshot::Receiver<MachineOpenReply>,
    timeout: std::time::Duration,
    story_id: &str,
    machine_id: &str,
) -> Result<(), String> {
    match tokio::time::timeout(timeout, reply).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(reason))) => Err(format!("unavailable: {reason}")),
        Ok(Err(_)) => Err(format!(
            "unavailable: the viewer dropped {story_id}'s open on machine {machine_id}"
        )),
        Err(_) => Err(format!(
            "unconfirmed: {story_id}'s workspace on machine {machine_id} wasn't confirmed open within {} minutes; check CodeSense is running, then open it again",
            timeout.as_secs() / 60
        )),
    }
}

/// The viewer opens (creating on first open) a story's workspace on a machine. `request_id`
/// is set for a verb call that waits for the viewer's verdict; a launch that polls the viewer
/// state carries none.
fn machine_reveal_payload(
    story_id: &str,
    machine_id: &str,
    team_key: &str,
    focus: bool,
    request_id: Option<&str>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "handle": story_id, "focus": focus, "teamKey": team_key,
        "target": "story", "machineId": machine_id,
    });
    if let Some(request_id) = request_id {
        payload["requestId"] = serde_json::Value::String(request_id.to_string());
    }
    payload
}

fn session_view(mode: WorkspaceMode) -> super::mcp_workspace::SessionView {
    if mode.is_visible() {
        super::mcp_workspace::SessionView::Tab {
            focus: mode.is_focused(),
        }
    } else {
        super::mcp_workspace::SessionView::Background
    }
}

/// Build a reveal from the host's actual home, never the UI-active workspace.
fn reveal_payload(
    handle: &str,
    focus: bool,
    key: &codesense_pty_core::domain::WorkspaceKey,
    layout: &codesense_pty_core::layout::WorkspaceLayout,
) -> Option<serde_json::Value> {
    let descriptor = &layout.descriptor;
    let team_key =
        super::pty_layout::team_key(descriptor.org.as_deref()?, descriptor.team.as_deref());
    let workspace_id = key.as_str().strip_prefix(&format!("{team_key}|"))?;
    Some(serde_json::json!({
        "handle": handle, "focus": focus, "teamKey": team_key,
        "target": if descriptor.kind == "story" { "story" } else { "team-workspace" },
        "workspaceId": workspace_id,
        "cwd": descriptor.worktree_path,
        "workspaceKind": descriptor.kind,
        "workspaceName": descriptor.display_name,
        "branch": descriptor.branch,
    }))
}
/// Validate the original registry binding as well as its exact runtime claim.
/// No registry or admission lock is held across host I/O or event callbacks.
pub(crate) fn validate_workspace_reveal_owner<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    handle: &str,
    claim: &super::pty_host::LocalEffectClaim,
) -> Result<(), String> {
    use tauri::Manager;
    super::pty_backend::commit_local_effect_claim(app, claim, || {
        let launch = app
            .try_state::<super::dashboard_launch::DashboardLaunchState>()
            .ok_or("launch registry unavailable")?;
        if launch.resolve_owned_pty(handle).as_ref() != Some(claim) {
            return Err("workspace reveal owner changed".into());
        }
        Ok(())
    })
}

pub(crate) fn capture_workspace_reveal_owner<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    handle: &str,
) -> Result<super::pty_host::LocalEffectClaim, String> {
    use tauri::Manager;
    let claim = app
        .try_state::<super::dashboard_launch::DashboardLaunchState>()
        .and_then(|launch| launch.resolve_owned_pty(handle))
        .ok_or("workspace handle has no owned PTY")?;
    validate_workspace_reveal_owner(app, handle, &claim)?;
    Ok(claim)
}

/// Complete the production reveal after its asynchronous snapshot boundary.
/// Tests use the same capture/complete path with actual hosts and registry.
pub(crate) fn complete_workspace_reveal<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    handle: &str,
    focus: bool,
    claim: &super::pty_host::LocalEffectClaim,
    snapshots: Vec<super::machines::RuntimeLayoutReply>,
) -> Result<Option<serde_json::Value>, String> {
    use tauri::Manager;
    validate_workspace_reveal_owner(app, handle, claim)?;
    let state = app.state::<crate::terminal::session_server::SessionServerState>();
    let _permit = state.admission.effects.owned_permit(claim, || {
        let launch = app.state::<super::dashboard_launch::DashboardLaunchState>();
        if launch.resolve_owned_pty(handle).as_ref() != Some(claim) {
            return Err("workspace reveal owner changed".into());
        }
        Ok(())
    })?;
    for reply in snapshots {
        let super::machines::RuntimeLayoutReply::Live {
            target,
            snapshot,
            private_authority_epoch: None,
        } = reply
        else {
            continue;
        };
        if target != claim.target {
            continue;
        }
        for (key, layout) in snapshot.workspaces {
            let Some(tab) = layout
                .tabs
                .iter()
                .find(|tab| tab.session.as_str() == claim.session)
            else {
                continue;
            };
            let Some(mut payload) = reveal_payload(handle, focus, &key, &layout) else {
                return Ok(None);
            };
            let reference = super::machines::RuntimeSessionReference::Admitted(Box::new(
                super::machines::RuntimeSessionTarget {
                    runtime: claim.target.clone(),
                    session: tab.session.clone(),
                },
            ));
            payload["runtimeTarget"] =
                serde_json::to_value(&reference).map_err(|error| error.to_string())?;
            if focus {
                if tab.placement != codesense_pty_core::layout::Placement::Tab {
                    payload["shown"] = serde_json::json!("restored");
                    validate_workspace_reveal_owner(app, handle, claim)?;
                    super::pty_backend::with_session_reference(
                        app,
                        &reference,
                        false,
                        |host, session| {
                            host.place_tab(
                                session,
                                &key,
                                codesense_pty_core::layout::Placement::Tab,
                                None,
                                None,
                            )
                            .map_err(|error| error.to_string())
                        },
                    )?;
                }
                validate_workspace_reveal_owner(app, handle, claim)?;
                super::pty_backend::with_session_reference(
                    app,
                    &reference,
                    false,
                    |host, session| {
                        host.set_active(&super::pty_layout::app_viewer(), &key, Some(session))
                            .map_err(|error| error.to_string())
                    },
                )?;
            }
            validate_workspace_reveal_owner(app, handle, claim)?;
            return Ok(Some(payload));
        }
    }
    Ok(None)
}

/// Surface a just-opened session in the viewer without waiting on it (a launch's reveal).
pub(crate) fn emit_open_workspace_reveal(app: &tauri::AppHandle, handle: &str, focus: bool) {
    let app = app.clone();
    let handle = handle.to_string();
    tauri::async_runtime::spawn(async move {
        if let RevealOutcome::NotShown(reason) = reveal_session(&app, &handle, focus).await {
            log::warn!("open_workspace: {handle} was not shown: {reason}");
        }
    });
}

/// The reveal of a running session no host workspace shows: the viewer's one Show adopts it
/// into a tab in its team. With no team recorded for it there is nowhere to put it, so it is
/// not shown, and the reason says so.
pub(crate) fn tabless_reveal(
    handle: &str,
    session: &str,
    team_key: Option<String>,
    focus: bool,
) -> (Option<serde_json::Value>, RevealOutcome) {
    match team_key {
        Some(team_key) => (
            Some(serde_json::json!({
                "handle": handle, "focus": focus, "teamKey": team_key,
                "target": "session", "sessionId": session,
            })),
            RevealOutcome::Shown(ShownAs::Adopted),
        ),
        None => (
            None,
            RevealOutcome::NotShown(
                "it runs with no tab, and CodeSense does not know which team it belongs to".into(),
            ),
        ),
    }
}

/// The reveal of a headless run: the viewer opens its run output.
pub(crate) fn headless_reveal(
    handle: &str,
    session: &str,
    run_id: Option<String>,
    team_key: Option<String>,
) -> (Option<serde_json::Value>, RevealOutcome) {
    match (run_id, team_key) {
        (Some(run_id), Some(team_key)) => (
            Some(serde_json::json!({
                "handle": handle, "focus": true, "teamKey": team_key,
                "target": "runOutput", "sessionId": session, "runId": run_id,
            })),
            RevealOutcome::Shown(ShownAs::RunOutput),
        ),
        _ => (
            None,
            RevealOutcome::NotShown("it is a headless run with no run output to open yet".into()),
        ),
    }
}

/// Emit `payload` only while `claim` still owns `handle`'s session.
fn emit_owned_reveal(
    app: &tauri::AppHandle,
    handle: &str,
    claim: &super::pty_host::LocalEffectClaim,
    payload: serde_json::Value,
) -> Result<(), String> {
    use tauri::{Emitter, Manager};
    validate_workspace_reveal_owner(app, handle, claim)?;
    let state = app.state::<crate::terminal::session_server::SessionServerState>();
    let _permit = state.admission.effects.owned_permit(claim, || {
        let launch = app.state::<super::dashboard_launch::DashboardLaunchState>();
        if launch.resolve_owned_pty(handle).as_ref() != Some(claim) {
            return Err("workspace reveal owner changed".into());
        }
        Ok(())
    })?;
    validate_workspace_reveal_owner(app, handle, claim)?;
    app.emit(crate::mcp::handler::OPEN_WORKSPACE_REVEAL_EVENT, payload)
        .map_err(|error| error.to_string())
}

/// Show `handle`'s running session in the viewer and say what that did: its tab focused (or
/// put back from Hidden), a live session no workspace shows adopted into a tab, a headless
/// run's output, or why it could not be shown. Never a silent no-op.
pub(crate) async fn reveal_session(
    app: &tauri::AppHandle,
    handle: &str,
    focus: bool,
) -> RevealOutcome {
    use tauri::{Emitter, Manager};
    let not_shown = |reason: String| RevealOutcome::NotShown(reason);
    let Some(launch) = app.try_state::<super::dashboard_launch::DashboardLaunchState>() else {
        return not_shown("the launch registry is unavailable".into());
    };
    let Some(session) = launch.resolve(handle) else {
        return not_shown("it has no running session".into());
    };
    let team_key = launch
        .owner_for_session(&session)
        .map(|owner| super::pty_layout::team_key(&owner.org, owner.team.as_deref()));
    if super::dashboard_launch::is_headless_session_id(&session) {
        let run_id = launch
            .run_identity_for_session(&session)
            .map(|(run_id, _)| run_id);
        let (payload, outcome) = headless_reveal(handle, &session, run_id, team_key);
        if let Some(payload) = payload {
            if let Err(error) = app.emit(crate::mcp::handler::OPEN_WORKSPACE_REVEAL_EVENT, payload)
            {
                return not_shown(error.to_string());
            }
        }
        return outcome;
    }
    let claim = match capture_workspace_reveal_owner(app, handle) {
        Ok(claim) => claim,
        Err(error) => return not_shown(format!("its session can't be shown from here ({error})")),
    };
    let snapshots = match super::pty_layout::webview_member_layout_snapshots(app.clone()).await {
        Some(Ok(snapshots)) => snapshots,
        Some(Err(error)) => {
            return not_shown(format!("the terminal hosts did not answer ({error})"))
        }
        None => return not_shown("the viewer is not ready yet".into()),
    };
    let worker_app = app.clone();
    let worker_handle = handle.to_string();
    let worker_claim = claim.clone();
    let result = super::async_helpers::await_blocking("open_workspace_reveal_owned", move || {
        complete_workspace_reveal(&worker_app, &worker_handle, focus, &worker_claim, snapshots)
    })
    .await;
    let (payload, outcome) = match result {
        Ok(Some(payload)) => {
            let shown = if payload["shown"] == "restored" {
                ShownAs::Restored
            } else {
                ShownAs::Tab
            };
            (Some(payload), RevealOutcome::Shown(shown))
        }
        // No host workspace holds it: a live session with no tab.
        Ok(None) => tabless_reveal(handle, &claim.session, team_key, focus),
        Err(error) => return not_shown(error),
    };
    if let Some(payload) = payload {
        if let Err(error) = emit_owned_reveal(app, handle, &claim, payload) {
            return not_shown(error);
        }
    }
    outcome
}

/// Only confirmed host death permits replacing a retained binding. Unknown
/// host availability preserves the session and never causes a duplicate launch.
fn retained_handle_is_live(
    state: &super::dashboard_launch::DashboardLaunchState,
    handle: &str,
    known_gone: impl FnOnce(&str) -> bool,
) -> bool {
    if let Some(session) = state.resolve(handle) {
        if known_gone(&session) {
            state.release_for_session(&session);
        }
    }
    state.has_handle(handle)
}

/// The app-backed [`OpenHost`], bound to the signed-in user.
pub(crate) struct AppOpenHost {
    pub app: tauri::AppHandle,
    pub codesense_home: PathBuf,
    pub principal: crate::domain::Principal,
}

impl AppOpenHost {
    fn prepare_context_at(
        home: &Path,
        principal: &crate::domain::Principal,
        scope: &ActiveTeam,
        content: &Path,
        name: &str,
        requested: Option<&str>,
        create: bool,
        agent: &AgentLaunch,
        progress: &dyn Fn(&crate::agents::prepare::PrepareProgress),
    ) -> Result<context::ContextDestination, String> {
        let destination =
            context::resolve(home, principal, scope, content, name, requested, create)?;
        if let Err(error) = context::layer(home, principal, &destination, agent, progress) {
            context::rollback(home, principal, &destination);
            return Err(error);
        }
        Ok(destination)
    }
    /// The story's workspace on `machine_id`, showing and prepared there: asks the viewer
    /// to open it (creating it on the machine on first open) and waits until it is.
    async fn machine_workspace_ready(
        &self,
        team_key: &str,
        story_id: &str,
        machine_id: &str,
        focus: bool,
    ) -> Result<(String, String, String), String> {
        use tauri::{Emitter, Manager};
        let deadline = tokio::time::Instant::now() + MACHINE_WORKSPACE_WAIT;
        let (mut asked, mut seen) = (false, false);
        let mut asked_at: Option<tokio::time::Instant> = None;
        // SW-FX-131 AC3: the viewer's verdict on the ask, when it gives one. A failure ends the
        // wait with its reason; the guard keeps the request registered until this returns.
        let registry = self
            .app
            .try_state::<std::sync::Arc<MachineOpenRegistry>>()
            .map(|registry| registry.inner().clone());
        let mut verdict: Option<(
            PendingMachineOpen,
            tokio::sync::oneshot::Receiver<MachineOpenReply>,
        )> = None;
        loop {
            let viewer = self.app.state::<crate::mcp::state::McpViewerState>().get();
            seen |= story_workspace(&viewer, team_key, story_id).is_some();
            let reported = verdict.as_mut().map(|(_, reply)| reply.try_recv());
            match reported {
                Some(Ok(Err(reason))) => return Err(format!("unavailable: {reason}")),
                Some(Ok(Ok(()))) | Some(Err(TryRecvError::Closed)) => verdict = None,
                _ => {}
            }
            match machine_workspace_wait(&viewer, team_key, story_id, machine_id, asked, seen) {
                MachineWorkspaceWait::Ready {
                    workspace_id,
                    handle,
                    cwd,
                } => return Ok((workspace_id, handle, cwd)),
                MachineWorkspaceWait::Failed(reason) => {
                    return Err(format!("unavailable: {reason}"))
                }
                MachineWorkspaceWait::Ask => {
                    // A minimized workspace comes back only when brought forward.
                    let minimized = story_workspace(&viewer, team_key, story_id).is_some();
                    let ask = registry.as_ref().map(|registry| {
                        let (request_id, reply) = registry.register();
                        (
                            PendingMachineOpen {
                                registry: registry.clone(),
                                request_id,
                            },
                            reply,
                        )
                    });
                    let request_id = ask.as_ref().map(|(pending, _)| pending.request_id.clone());
                    self.app
                        .emit(
                            crate::mcp::handler::OPEN_WORKSPACE_REVEAL_EVENT,
                            machine_reveal_payload(
                                story_id,
                                machine_id,
                                team_key,
                                focus || minimized,
                                request_id.as_deref(),
                            ),
                        )
                        .map_err(|error| error.to_string())?;
                    verdict = ask;
                    asked = true;
                    asked_at.get_or_insert_with(tokio::time::Instant::now);
                }
                MachineWorkspaceWait::Pending => {}
            }
            if !seen && asked_at.is_some_and(|at| at.elapsed() >= MACHINE_WORKSPACE_ASK_GRACE) {
                return Err(format!(
                    "unavailable: {story_id}'s workspace wasn't opened on machine {machine_id}; open the story's workspace there, then launch again"
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "unconfirmed: {story_id}'s workspace on machine {machine_id} isn't ready yet; launch again once it is"
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        }
    }

    /// The live tab in the story's workspace on the machine already running a saved
    /// Agent launched under `name`, by its handle. Read from what each tab was launched
    /// as, so a tab a person renamed since is still found.
    async fn live_remote_agent(
        &self,
        machine_id: &str,
        team_key: &str,
        workspace_id: &str,
        name: &str,
    ) -> Option<String> {
        use tauri::Manager;
        let launch = self
            .app
            .try_state::<super::dashboard_launch::DashboardLaunchState>()?;
        let tabs = crate::mcp::terminal_tabs::LiveTabs::read(&self.app).await;
        tabs.tabs.into_iter().find_map(|tab| {
            let handle = tab.handle?;
            let record = launch.remote_shell_by_handle(&handle)?;
            (tab.machine_id == machine_id
                && !tab.exited
                && record.agent.is_some()
                && launched_as(&record, machine_id, team_key, workspace_id, name))
            .then_some(handle)
        })
    }
}

#[async_trait::async_trait]
impl OpenHost for AppOpenHost {
    fn role_join_error(&self, handle: &str) -> Option<String> {
        use tauri::Manager;
        crate::commands::dashboard_launch::DashboardLaunchState::snapshot(
            &self
                .app
                .state::<super::dashboard_launch::DashboardLaunchState>(),
        )
        .into_iter()
        .find(|e| e.handle == handle)
        .and_then(|e| e.meta.role_join_error)
    }

    async fn story_workspace_place(
        &self,
        team: &ActiveTeam,
        story_id: &str,
    ) -> Option<WorkspacePlace> {
        use tauri::Manager;
        let team_key = super::pty_layout::team_key(&team.org, Some(&team.team));
        let viewer = self.app.state::<crate::mcp::state::McpViewerState>().get();
        let ws = story_workspace(&viewer, &team_key, story_id)?;
        Some(
            ws.machine
                .map_or(WorkspacePlace::ThisMac, WorkspacePlace::Machine),
        )
    }

    async fn launch_remote(&self, launch: RemoteLaunch) -> Result<RemoteLaunched, String> {
        use tauri::Manager;
        let team_key = super::pty_layout::team_key(&launch.team.org, Some(&launch.team.team));
        super::machines::authorize_terminal_machine(&self.app, &launch.machine_id, &team_key)
            .await?;
        let machine = super::machines::machine_labels(&self.app)
            .await
            .get(&launch.machine_id)
            .cloned()
            .unwrap_or_else(|| launch.machine_id.clone());
        let (workspace_id, workspace_handle, cwd) = self
            .machine_workspace_ready(
                &team_key,
                &launch.story_id,
                &launch.machine_id,
                launch.focus,
            )
            .await?;
        if let Some(handle) = self
            .live_remote_agent(&launch.machine_id, &team_key, &workspace_id, &launch.name)
            .await
        {
            return Ok(RemoteLaunched {
                handle,
                fresh: false,
                machine,
            });
        }
        let launched_by = launch.launched_by.map(|raw| {
            self.app
                .try_state::<super::dashboard_launch::DashboardLaunchState>()
                .map_or_else(|| raw.clone(), |state| state.normalize_launched_by(&raw))
        });
        let request = super::pty_layout::RemoteShellTabRequest::Create {
            request_id: uuid::Uuid::new_v4(),
            machine_id: launch.machine_id.clone(),
            team_key: team_key.clone(),
            workspace_id,
            cwd: Some(cwd),
            name: Some(launch.name.clone()),
            launch: None,
            permission_mode: Default::default(),
            agent_key: Some(launch.agent_key),
            prepared_workspace_handle: Some(workspace_handle),
            headed: launch.focus,
            initial_prompt: launch.initial_prompt,
            identity: Box::new(super::pty_layout::RemoteLaunchIdentity {
                launcher_context: self
                    .app
                    .try_state::<super::dashboard_launch::DashboardLaunchState>()
                    .and_then(|s| {
                        s.context_of_launcher(&self.codesense_home, launched_by.as_deref())
                    }),
                launched_by,
                role: launch.role,
                queue_role: launch.queue_role,
                avatar: launch.avatar,
                story: Some(launch.story_id.clone()),
                ..Default::default()
            }),
        };
        let outcome =
            super::pty_layout::remote_shell_tab_bound(self.app.clone(), request, None).await;
        if !matches!(outcome.status, "opened" | "alreadyLive") {
            let code = outcome.code.as_deref().unwrap_or(outcome.status);
            let why = outcome
                .message
                .clone()
                .unwrap_or_else(|| format!("{machine} didn't open the agent"));
            return Err(format!("{code}: {why}"));
        }
        if outcome.headed {
            if let Err(error) =
                crate::mcp::machine_tools::reveal_remote_terminal(&self.app, &team_key, &outcome)
                    .await
            {
                log::warn!("open_workspace: could not reveal the remote agent: {error}");
            }
        }
        Ok(RemoteLaunched {
            handle: outcome.handle.unwrap_or_default(),
            fresh: outcome.status == "opened",
            machine,
        })
    }

    async fn story_team(&self, story_id: &str, org: Option<&str>) -> Result<ActiveTeam, String> {
        let owner = super::mcp_workspace::resolve_story_owner(&self.app, story_id, org).await?;
        Ok(ActiveTeam {
            org: owner.org,
            team: owner.team,
        })
    }

    fn is_live(&self, handle: &str) -> bool {
        use tauri::Manager;
        self.app
            .try_state::<super::dashboard_launch::DashboardLaunchState>()
            .is_some_and(|state| {
                retained_handle_is_live(&state, handle, |session| {
                    super::pty_backend::alive_session_ids(&self.app, &[session]).known_gone(session)
                })
            })
    }

    async fn claim_open(&self, handle: &str) -> OpenClaim {
        static CLAIMS: std::sync::OnceLock<OpenClaims> = std::sync::OnceLock::new();
        CLAIMS.get_or_init(OpenClaims::default).claim(handle).await
    }

    async fn team_content_root(&self, org: &str, team: &str) -> Result<PathBuf, String> {
        let (home, principal) = (self.codesense_home.clone(), self.principal.clone());
        let (org, team) = (org.to_string(), team.to_string());
        super::await_blocking("open_workspace.team_content_root", move || {
            super::dashboard_launch::resolve_managed_team_content_root(
                &home, &principal, &org, &team,
            )
        })
        .await
    }

    async fn context_created_by(
        &self,
        destination: &mut context::ContextDestination,
        launcher: Option<&str>,
    ) -> Result<(), String> {
        use crate::install::worktree_index::{self, ContextWorktreeOwner};
        let session = launcher.and_then(|launcher| {
            self.app
                .try_state::<super::dashboard_launch::DashboardLaunchState>()
                .and_then(|s| {
                    crate::commands::dashboard_launch::DashboardLaunchState::snapshot(&s)
                        .into_iter()
                        .find(|r| r.handle == launcher || r.session_id.as_deref() == Some(launcher))
                })
        });
        destination.record.owner = Some(match session {
            Some(s) => ContextWorktreeOwner::Agent {
                name: s.meta.display_name.unwrap_or_else(|| s.handle.clone()),
                handle: s.handle,
            },
            None => ContextWorktreeOwner::Person,
        });
        let row = destination.record.clone();
        let (home, principal) = (self.codesense_home.clone(), self.principal.clone());
        super::await_blocking("context worktree creator", move || {
            worktree_index::write_context_record(&home, &principal, &row).map_err(|e| e.to_string())
        })
        .await
    }

    async fn prepare_context_worktree(
        &self,
        scope: &ActiveTeam,
        content: PathBuf,
        name: &str,
        requested: Option<&str>,
        create: bool,
        agent: &AgentLaunch,
    ) -> Result<context::ContextDestination, String> {
        let (home, principal, scope, name, requested, agent) = (
            self.codesense_home.clone(),
            self.principal.clone(),
            scope.clone(),
            name.to_string(),
            requested.map(str::to_string),
            agent.clone(),
        );
        let sink = crate::commands::worktree::TauriEventSink(self.app.clone());
        super::await_blocking("open_workspace.context_worktree", move || {
            Self::prepare_context_at(
                &home,
                &principal,
                &scope,
                &content,
                &name,
                requested.as_deref(),
                create,
                &agent,
                &crate::agents::prepare::progress::to_sink(&sink),
            )
        })
        .await
    }

    async fn live_context(
        &self,
        handle: &str,
        req: &OpenRequest,
    ) -> Result<Option<ContextWorktreeResult>, String> {
        use tauri::Manager;
        let cwd = self
            .app
            .try_state::<super::dashboard_launch::DashboardLaunchState>()
            .and_then(|state| {
                crate::commands::dashboard_launch::DashboardLaunchState::snapshot(&state)
                    .into_iter()
                    .find(|s| s.handle == handle)
                    .and_then(|s| s.worktree_dir)
            });
        let (home, principal, requested, create) = (
            self.codesense_home.clone(),
            self.principal.clone(),
            req.worktree.clone(),
            req.create_worktree,
        );
        super::await_blocking("open_workspace.live_context", move || {
            let Some(cwd) = cwd else {
                return Ok(None);
            };
            let Some(current) = crate::install::worktree_index::context_for_cwd(&home, &cwd) else {
                return Ok(None);
            };
            context::validate(&current)?;
            if create {
                return Err(format!(
                    "This name is already running in {}. Give the new agent another name",
                    current.name
                ));
            }
            if let Some(requested) = requested {
                let scope = ActiveTeam {
                    org: current.org.clone(),
                    team: current.team.clone(),
                };
                let selected = context::resolve(
                    &home,
                    &principal,
                    &scope,
                    &current.launch_cwd,
                    &current.name,
                    Some(&requested),
                    false,
                )?;
                if selected.record.id != current.id {
                    return Err(format!(
                        "This name is already running in {}. Give the new agent another name",
                        current.name
                    ));
                }
            }
            Ok(Some(ContextWorktreeResult {
                id: current.id,
                name: current.name,
                branch: current.branch,
                created: false,
            }))
        })
        .await
    }

    async fn context_used(&self, destination: &context::ContextDestination, name: &str) {
        let (home, principal, row, name) = (
            self.codesense_home.clone(),
            self.principal.clone(),
            destination.record.clone(),
            name.to_string(),
        );
        if let Err(error) = super::await_blocking("open_workspace.context_used", move || {
            context::note_used(&home, &principal, &row, &name)
        })
        .await
        {
            log::warn!("could not record context worktree use: {error}");
        }
        super::worktree::emit_worktree_changed(&self.app);
    }

    async fn rollback_context_worktree(&self, destination: &context::ContextDestination) {
        let (home, principal, destination) = (
            self.codesense_home.clone(),
            self.principal.clone(),
            destination.clone(),
        );
        let _ = super::await_blocking("open_workspace.context_rollback", move || {
            context::rollback(&home, &principal, &destination);
            Ok(())
        })
        .await;
    }

    async fn prepare_agent(
        &self,
        reference: &str,
        team: &ActiveTeam,
        prompt: Option<&str>,
        mode: Option<adapter::LaunchMode>,
    ) -> Result<AgentLaunch, String> {
        let (home, principal) = (self.codesense_home.clone(), self.principal.clone());
        let (reference, prompt) = (reference.to_string(), prompt.map(str::to_string));
        let target_team = Some((team.org.clone(), team.team.clone()));
        super::await_blocking("open_workspace.prepare_agent", move || {
            open_workspace_prepare(
                &home,
                &principal,
                &reference,
                target_team,
                prompt.as_deref(),
                mode,
            )
        })
        .await
    }

    async fn launch(&self, mut launch: Launch) -> Result<LaunchOutcome, LaunchError> {
        launch.record.meta.launcher_context = self
            .app
            .try_state::<super::dashboard_launch::DashboardLaunchState>()
            .and_then(|s| {
                s.context_of_launcher(
                    &self.codesense_home,
                    launch.record.meta.launched_by.as_deref(),
                )
            });
        use super::mcp_workspace::{
            launch_managed_dir_session_typed, launch_story_session_typed, ManagedLaunchOptions,
        };
        match launch.target {
            LaunchTarget::Story {
                story_id,
                handle,
                team,
            } => {
                launch_story_session_typed(
                    &self.app,
                    &story_id,
                    launch.initial_prompt.as_deref(),
                    super::mcp_workspace::StoryLaunchOptions {
                        handle: Some(handle),
                        org: Some(team.org),
                        provenance: Some(launch.provenance),
                        record: launch.record,
                        view: launch.view,
                        agent: launch.agent,
                    },
                )
                .await
            }
            LaunchTarget::Dir { handle, cwd, scope } => {
                let context =
                    crate::install::worktree_index::context_for_cwd(&self.codesense_home, &cwd)
                        .ok_or_else(|| {
                            LaunchError::Other("context worktree registration disappeared".into())
                        })?;
                context::validate(&context).map_err(LaunchError::Other)?;
                let mut home = super::mcp_workspace::LaunchHome::new(
                    &scope.org,
                    Some(&scope.team),
                    &context.id,
                    "context",
                    &cwd,
                    launch.view,
                );
                home.descriptor.display_name = context.name.clone();
                home.descriptor.branch = Some(context.branch.clone());
                launch_managed_dir_session_typed(
                    &self.app,
                    &handle,
                    &cwd,
                    &self.codesense_home,
                    launch.initial_prompt.as_deref(),
                    ManagedLaunchOptions {
                        provenance: Some(launch.provenance),
                        record: launch.record,
                        extra_args: Vec::new(),
                        home,
                        agent: launch.agent,
                    },
                )
                .await
            }
        }
    }

    fn reveal(&self, handle: &str, focus: bool) {
        emit_open_workspace_reveal(&self.app, handle, focus);
    }

    async fn show(&self, handle: &str) -> RevealOutcome {
        reveal_session(&self.app, handle, true).await
    }
}

#[cfg(test)]
pub(crate) mod tests;
