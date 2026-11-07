// SPDX-License-Identifier: MIT
//! Dispatch: start an admitted routine run. The claim loop hands a claimed
//! routine pointer here; the run's org, team and owner come from the pointer,
//! never from whichever team is active in the UI.
//!
//! Each run launches the routine's agent through [`crate::agents::launch`],
//! prepared for the account the run is dispatched as: that account's binding
//! for the agent, else its active account for the agent's runtime, and the
//! agent's own args, MCP servers and permissions for the run's mode. The
//! agent is resolved at its current revision on every run, so an edit to it
//! (or archiving it) applies to the next run without touching the routine,
//! and the run records which revision it launched.
//!
//! The run is moved to `running` before its agent launches, so an agent that
//! finishes at once still finds a run to end. A run that cannot start (its
//! revision, target or agent is gone, or its task does not render) is ended
//! `failed` with the reason, and nothing launches. Dispatch never waits for the
//! agent: completion arrives later through [`super::finish`].

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::params;

use crate::agents::launch::AgentLaunch;
use crate::commands::dashboard_launch::RunProvenance;
use crate::coordinator::StoreResult;
use crate::domain::Principal;

use super::admit::RoutinePointer;
use super::defs;
use super::finish::{self, Ending};
use super::model::{RoutineDef, RoutineKey, RoutineMode, RoutineTrigger};
use super::runs::{phase, reason};
use super::state::RoutineStateStore;
use super::target;
use super::template;

/// `RunProvenance.trigger_source` of every routine run.
pub const TRIGGER_SOURCE: &str = "routine";

/// The launch handle (and story id) of one run's session. Per run, not per
/// routine: a finished run's session can stay registered for a while (a
/// headless run persisting its transcript, a tab left open), and the next run
/// must not be refused as busy behind it. Overlap is the run rows' business.
pub fn handle(routine_key: &str, run_id: &str) -> String {
    format!("routine:{routine_key}@{run_id}")
}

/// One agent launch for a run.
#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub codesense_home: PathBuf,
    pub org: String,
    pub team: String,
    pub handle: String,
    pub cwd: PathBuf,
    pub prompt: String,
    /// The routine's agent, prepared for this run's account and mode.
    pub agent: AgentLaunch,
    /// A terminal tab instead of a headless process.
    pub visible: bool,
    /// The run's `max_runtime`: a headless process is killed at it, as the
    /// watchdog would.
    pub max_runtime: Duration,
    pub provenance: RunProvenance,
}

pub type LaunchFuture<'a> = Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;

/// Starts a run's agent and returns its session id.
pub(crate) trait Launcher: Send + Sync {
    fn launch(&self, request: LaunchRequest) -> LaunchFuture<'_>;
    fn command(&self, _request: super::command::CommandRequest) -> LaunchFuture<'_> {
        Box::pin(async { Err("command execution is unavailable".into()) })
    }
    /// Kill whatever was launched for `run_id`.
    fn kill_run(&self, run_id: &str);
    /// What a `wake` fire resolves and delivers through (SW-AGR-2). `None`
    /// where wakes cannot be delivered: a wake run then ends
    /// `undelivered(unavailable)`, and nothing launches.
    fn wake_env(&self) -> Option<std::sync::Arc<dyn super::wake::WakeEnv>> {
        None
    }
    /// Stop `session` on `machine`, which `run_id` started there (SW-AGR-2b).
    fn kill_remote(&self, run_id: &str, machine: &str, session: &str) {
        let _ = (machine, session);
        self.kill_run(run_id);
    }
}

/// What a claimed routine run resolved to.
enum Prepared {
    Command(Box<super::command::CommandRequest>),
    /// Start an instance of its agent.
    Fresh(Box<LaunchRequest>),
    /// Wake the instance that runs (SW-AGR-2).
    Wake(Box<super::wake::WakePlan>),
    /// Start an instance on another machine (SW-AGR-2b).
    Remote(Box<super::wake::RemoteLaunch>),
}

enum NotStarted {
    /// The run is no longer `queued` (stopped, or already dispatched before a
    /// restart): nothing to do.
    Gone,
    BudgetReached(String),
    Failed {
        reason: &'static str,
        detail: String,
    },
    /// A wake that will not be sent (its routine was archived).
    Undelivered {
        reason: &'static str,
        detail: String,
    },
}

/// The claim loop's routine branch: dispatch `json` when it is a routine
/// pointer, `None` when it is some other pointer shape. A routine pointer that
/// cannot be read is retired (`Some(Ok)`): re-reading the same bytes never helps.
pub(crate) async fn execute_pointer_json(
    codesense_home: PathBuf,
    run_id: &str,
    json: &str,
    launcher: &dyn Launcher,
) -> Option<Result<(), String>> {
    Some(match super::admit::parse_pointer(json)? {
        Ok(pointer) => execute(codesense_home, pointer, launcher).await,
        Err(error) => {
            log::warn!(target: "orchestration", "claim loop: routine pointer for {run_id} failed to parse ({error}) — retiring (unrecoverable)");
            // The run row would otherwise stay admitted, and block the routine, forever.
            end(
                &codesense_home,
                run_id,
                Ending::Failed {
                    reason: reason::LAUNCH_FAILED,
                    detail: format!("unreadable dispatch request: {error}"),
                },
            )
            .await;
            Ok(())
        }
    })
}

/// Dispatch one claimed routine run. `Ok` is a terminal dispatch decision for
/// the claim loop; `Err` is a transient store failure, left for `recover`.
pub(crate) async fn execute(
    codesense_home: PathBuf,
    pointer: RoutinePointer,
    launcher: &dyn Launcher,
) -> Result<(), String> {
    let run_id = pointer.run_id.clone();
    match prepare_blocking(&codesense_home, &pointer, None).await? {
        Some(Prepared::Command(request)) => {
            let routine_key = request.routine_key.clone();
            match launcher.command(*request).await {
                Ok(session) => {
                    let home = codesense_home.clone();
                    let id = run_id.clone();
                    let phase =
                        crate::commands::await_blocking("routine_command_session", move || {
                            let store =
                                RoutineStateStore::open(&home).map_err(|e| e.to_string())?;
                            record_session(&store, &id, &session, None).map_err(|e| e.to_string())
                        })
                        .await?;
                    if matches!(phase.as_str(), "cancelled" | "timed_out") {
                        launcher.kill_run(&run_id);
                    }
                    super::events::changed(&routine_key, &run_id);
                }
                Err(detail) => {
                    end(
                        &codesense_home,
                        &run_id,
                        Ending::Failed {
                            reason: reason::LAUNCH_FAILED,
                            detail,
                        },
                    )
                    .await
                }
            }
        }
        Some(Prepared::Fresh(request)) => {
            launch(&codesense_home, &run_id, *request, launcher, None).await;
        }
        Some(Prepared::Wake(plan)) => wake(&codesense_home, &pointer, &plan, launcher).await,
        Some(Prepared::Remote(remote)) => launch_remote(&codesense_home, *remote, launcher).await,
        None => {}
    }
    Ok(())
}

/// A fresh run on another machine (SW-AGR-2b): the same remote launch a wake
/// makes when its agent is absent (the routine's workspace prepared there,
/// SW-XS-7; the run's own request id; one Create, and an Attach on the same
/// id when its answer is lost), without a wake's single flight. A launch
/// never confirmed either way ends the run `unknown` and is never repeated.
async fn launch_remote(
    codesense_home: &Path,
    remote: super::wake::RemoteLaunch,
    launcher: &dyn Launcher,
) {
    use super::wake::TabReply;
    let run_id = remote.run_id.clone();
    let Some(env) = launcher.wake_env() else {
        end(
            codesense_home,
            &run_id,
            Ending::Failed {
                reason: reason::LAUNCH_FAILED,
                detail: "launches on other machines are not available in this app".into(),
            },
        )
        .await;
        return;
    };
    super::events::changed(&remote.routine_key, &run_id);
    match super::wake::open_remote(env.as_ref(), &remote, super::wake::TabOp::Create).await {
        TabReply::Opened { session, .. } | TabReply::Ended { session, .. } => {
            // Its machine and request were recorded before the Create.
            let home = codesense_home.to_path_buf();
            let (id, bound) = (run_id.clone(), session.clone());
            let recorded =
                crate::commands::await_blocking("routine_dispatch_remote_session", move || {
                    let store = RoutineStateStore::open(&home).map_err(|e| e.to_string())?;
                    record_session(&store, &id, &bound, None).map_err(|e| e.to_string())
                })
                .await;
            match recorded {
                // Stopped or timed out while it launched: the agent it started
                // on the machine is stopped too, never left running unbounded.
                Ok(phase) if phase == phase::CANCELLED || phase == phase::TIMED_OUT => {
                    log::info!(target: "routines", "routine run {run_id} ended ({phase}) while it launched on {}; stopping {session} there", remote.machine);
                    launcher.kill_remote(&run_id, &remote.machine, &session);
                }
                Ok(_) => super::events::changed(&remote.routine_key, &run_id),
                Err(e) => {
                    log::warn!(target: "routines", "routine run {run_id}: session not recorded: {e}")
                }
            }
        }
        TabReply::Unknown(detail) | TabReply::Pending(detail) => {
            super::wake::unconfirmed(
                codesense_home,
                &remote.routine_key,
                &run_id,
                &remote.agent,
                &remote.machine,
                remote.request_id,
                &detail,
                env.as_ref(),
            )
            .await;
        }
        // A fresh launch is never if-absent; a machine that says the agent
        // runs there started nothing for this run.
        TabReply::Running(detail) => {
            log::warn!(target: "routines", "routine run {run_id} not started on {}: {detail}", remote.machine);
            end(
                codesense_home,
                &run_id,
                Ending::Failed {
                    reason: reason::LAUNCH_FAILED,
                    detail,
                },
            )
            .await;
        }
        TabReply::NotStarted {
            reason: why,
            detail,
        } => {
            let reason = match why {
                codesense_pty_core::session_wake::UndeliveredReason::NeedsPreparation => {
                    reason::TARGET_UNAVAILABLE
                }
                _ => reason::LAUNCH_FAILED,
            };
            log::warn!(target: "routines", "routine run {run_id} not started on {}: {detail}", remote.machine);
            end(codesense_home, &run_id, Ending::Failed { reason, detail }).await;
        }
    }
}

/// Resolve the run on the blocking pool. `None` when it is not started (gone,
/// or ended here with why); `wake_prompt` launches its agent as a wake's
/// launch-if-absent.
async fn prepare_blocking(
    codesense_home: &Path,
    pointer: &RoutinePointer,
    wake_prompt: Option<String>,
) -> Result<Option<Prepared>, String> {
    let run_id = pointer.run_id.clone();
    let home = codesense_home.to_path_buf();
    let for_prepare = pointer.clone();
    let prepared = crate::commands::await_blocking("routine_dispatch_prepare", move || {
        let store = RoutineStateStore::open(&home).map_err(|e| e.to_string())?;
        prepare(
            &store,
            &home,
            &for_prepare,
            finish::now_ms(),
            wake_prompt.as_deref(),
        )
        .map_err(|e| e.to_string())
    })
    .await?;
    match prepared {
        Ok(prepared) => Ok(Some(prepared)),
        Err(NotStarted::Gone) => Ok(None),
        Err(NotStarted::BudgetReached(detail)) => {
            end(
                codesense_home,
                &run_id,
                Ending::Wake {
                    phase: phase::SKIPPED,
                    reason: Some(reason::BUDGET_REACHED),
                    detail: Some(detail),
                },
            )
            .await;
            Ok(None)
        }
        Err(NotStarted::Failed { reason, detail }) => {
            log::warn!(target: "routines", "routine run {run_id} could not start ({reason}): {detail}");
            end(codesense_home, &run_id, Ending::Failed { reason, detail }).await;
            Ok(None)
        }
        Err(NotStarted::Undelivered { reason, detail }) => {
            let ending = Ending::Wake {
                phase: phase::UNDELIVERED,
                reason: Some(reason),
                detail: Some(detail),
            };
            end(codesense_home, &run_id, ending).await;
            Ok(None)
        }
    }
}

/// A wake fire: deliver into the live instance, or launch one through the
/// fresh path with the wake's prompt when none runs (SW-AGR-2).
async fn wake(
    codesense_home: &Path,
    pointer: &RoutinePointer,
    plan: &super::wake::WakePlan,
    launcher: &dyn Launcher,
) {
    use super::wake::Fired;
    let Some(env) = launcher.wake_env() else {
        end(
            codesense_home,
            &pointer.run_id,
            Ending::Wake {
                phase: phase::UNDELIVERED,
                reason: Some("unavailable"),
                detail: Some("wake delivery is not running in this app".into()),
            },
        )
        .await;
        return;
    };
    if super::wake::fire(codesense_home, plan, env.clone()).await != Fired::LaunchHere {
        return;
    }
    let launched = match prepare_blocking(codesense_home, pointer, Some(plan.prompt())).await {
        Ok(Some(Prepared::Fresh(request))) => {
            // The agent a wake starts is the agent's own instance, not the
            // run's: the run never kills it (stop, timeout), and later wakes
            // are typed into it.
            launch(
                codesense_home,
                &pointer.run_id,
                *request,
                launcher,
                Some(super::wake::LOCAL),
            )
            .await
        }
        Ok(_) => None,
        Err(e) => {
            log::warn!(target: "routines", "routine run {}: {e}", pointer.run_id);
            None
        }
    };
    if let Some((session, _)) = &launched {
        // Its agent started with this run's task as its first prompt: the
        // wake is delivered (§9.6); a result schema keeps the run open.
        delivered_by_launch(codesense_home, &pointer.run_id, super::wake::LOCAL, session).await;
    }
    super::wake::launched(env.as_ref(), plan, launched);
}

/// A wake's launch-if-absent started its agent with the task: `delivered`.
async fn delivered_by_launch(codesense_home: &Path, run_id: &str, machine: &str, session: &str) {
    let home = codesense_home.to_path_buf();
    let (id, target) = (
        run_id.to_string(),
        crate::delivery::authority::SessionRef::new(machine, session),
    );
    let _ = crate::commands::await_blocking("routine_wake_delivered_by_launch", move || {
        let store = RoutineStateStore::open(&home).map_err(|e| e.to_string())?;
        let now = finish::now_ms();
        finish::apply_wake_status(
            &store,
            &id,
            &target,
            &crate::delivery::run_status::WakeRunStatus::Delivered {
                delivered_at_ms: u64::try_from(now).unwrap_or_default(),
            },
            now,
        )
        .map_err(|e| e.to_string())
    })
    .await;
}

/// Launch a prepared run's agent and record its session. The session id and
/// handle it launched, if it did. `woke_on` (a wake's launch-if-absent) marks
/// the session as the agent's own, never the run's to kill.
async fn launch(
    codesense_home: &Path,
    run_id: &str,
    request: LaunchRequest,
    launcher: &dyn Launcher,
    woke_on: Option<&'static str>,
) -> Option<(String, String)> {
    let routine_key = request.provenance.routine_key.clone().unwrap_or_default();
    let handle = request.handle.clone();
    super::events::changed(&routine_key, run_id);
    match launcher.launch(request).await {
        Ok(session_id) => {
            let home = codesense_home.to_path_buf();
            let id = run_id.to_string();
            let recorded = session_id.clone();
            let phase_now =
                crate::commands::await_blocking("routine_dispatch_session", move || {
                    let store = RoutineStateStore::open(&home).map_err(|e| e.to_string())?;
                    record_session(&store, &id, &recorded, woke_on).map_err(|e| e.to_string())
                })
                .await;
            match phase_now {
                // Stopped or timed out while it launched: stop what was launched
                // too, unless a wake started it as the agent's own instance. Any
                // other ending (an agent that finished at once) keeps its session.
                Ok(phase)
                    if woke_on.is_none()
                        && (phase == phase::CANCELLED || phase == phase::TIMED_OUT) =>
                {
                    launcher.kill_run(run_id);
                    return None;
                }
                Ok(_) => super::events::changed(&routine_key, run_id),
                Err(e) => {
                    log::warn!(target: "routines", "routine run {run_id}: session not recorded: {e}")
                }
            }
            Some((session_id, handle))
        }
        Err(detail) => {
            log::warn!(target: "routines", "routine run {run_id} failed to launch: {detail}");
            end(
                codesense_home,
                run_id,
                Ending::Failed {
                    reason: reason::LAUNCH_FAILED,
                    detail,
                },
            )
            .await;
            None
        }
    }
}

async fn end(codesense_home: &Path, run_id: &str, ending: Ending) {
    let home = codesense_home.to_path_buf();
    let run_id = run_id.to_string();
    let _ = crate::commands::await_blocking("routine_dispatch_end", move || {
        let store = RoutineStateStore::open(&home).map_err(|e| e.to_string())?;
        finish::finish_and_announce(&store, &run_id, ending, finish::now_ms());
        Ok(())
    })
    .await;
}

/// Resolve everything the run needs. A `wake` run is handed back as its plan
/// and stays `queued` until its delivery says otherwise; any other run (and a
/// wake's launch-if-absent, `wake_prompt`) moves to `running` before its agent
/// launches.
fn prepare(
    store: &RoutineStateStore,
    home: &Path,
    pointer: &RoutinePointer,
    now: i64,
    wake_prompt: Option<&str>,
) -> StoreResult<Result<Prepared, NotStarted>> {
    let prepared = prepare_run(store, home, pointer, now, wake_prompt)?;
    if let Ok(prepared) = &prepared {
        let cwd = match prepared {
            Prepared::Command(r) => Some(r.cwd.as_path()),
            Prepared::Fresh(r) => Some(r.cwd.as_path()),
            _ => None,
        };
        note_run_start(store, &pointer.run_id, cwd);
    }
    Ok(prepared)
}

fn prepare_run(
    store: &RoutineStateStore,
    home: &Path,
    pointer: &RoutinePointer,
    now: i64,
    wake_prompt: Option<&str>,
) -> StoreResult<Result<Prepared, NotStarted>> {
    let failed =
        |reason: &'static str, detail: String| Ok(Err(NotStarted::Failed { reason, detail }));
    let Some(run) = store.run(&pointer.run_id)? else {
        return Ok(Err(NotStarted::Gone));
    };
    if run.phase != phase::QUEUED || run.reason.is_some() {
        return Ok(Err(NotStarted::Gone));
    }
    let Some(key) = RoutineKey::parse(&pointer.routine_key) else {
        return failed(
            reason::REVISION_MISSING,
            format!("'{}' is not a routine key", pointer.routine_key),
        );
    };
    let principal = Principal::from(pointer.principal.as_str());
    if !key.resolves_for(&principal) {
        return failed(
            reason::ACCOUNT_MISMATCH,
            format!("'{key}' is not {principal}'s routine"),
        );
    }
    let def = match pinned_revision(home, &principal, &key, pointer.revision) {
        Ok(def) => def,
        Err(detail) => return failed(reason::REVISION_MISSING, detail),
    };
    // Held runs bypass admission when released. Current caps still govern the next turn.
    let current = match super::service::get_current(home, &principal, &key) {
        Ok(def) => def,
        Err(detail) => return failed(reason::REVISION_MISSING, detail.to_string()),
    };
    let mut governed = def.clone();
    governed.policy.budget_usd = current.def.policy.budget_usd;
    if let Err(detail) =
        crate::session_usage::spend_caps::refuse_routine(home, &principal, &key, &governed)
    {
        return Ok(Err(NotStarted::BudgetReached(detail)));
    }
    let wake = wake_prompt.is_none()
        && super::service::effective_mode(home, &principal, &key, &def) == RoutineMode::Wake;
    // A run on another machine works in that machine's workspace (resolved
    // there): the target need not be cloned on this Mac.
    let remote = super::wake::TargetMachine::parse(def.target.machine.as_deref()).is_remote();
    let cwd = match target::resolve(home, &principal, &key, &def) {
        Ok(cwd) => cwd,
        Err(_) if remote => PathBuf::new(),
        Err(detail) => return failed(reason::TARGET_UNAVAILABLE, detail),
    };
    let state = store.state(&key)?;
    let values = template_values(&def, pointer, state.as_ref());
    if def.is_command() {
        if let Err(detail) = def.validate().and_then(|_| def.validate_agents_for(&key)) {
            return failed(reason::LAUNCH_FAILED, detail.to_string());
        }
        let argv = match super::command::render(&def.command, &values) {
            Ok(argv) => argv,
            Err(detail) => return failed(reason::TASK_UNRENDERABLE, detail),
        };
        let root = match super::service::key_content_dir(home, &principal, &key) {
            Ok(root) => root,
            Err(e) => return failed(reason::TARGET_UNAVAILABLE, e.to_string()),
        };
        let program = match super::command::program_path(&root, &argv[0]) {
            Ok(path) => path,
            Err(detail) => return failed(reason::LAUNCH_FAILED, detail),
        };
        let evidence = super::command::Evidence {
            argv: super::command::redacted_argv(&argv),
            cwd: Some(cwd.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let started = store.transaction(|tx| Ok(tx.execute(
            "UPDATE routine_runs SET phase = 'running', started_at = ?2, command_details = ?3 WHERE run_id = ?1 AND phase = 'queued' AND reason IS NULL",
            params![pointer.run_id, now, serde_json::to_string(&evidence).unwrap()],
        )? == 1))?;
        if !started {
            return Ok(Err(NotStarted::Gone));
        }
        return Ok(Ok(Prepared::Command(Box::new(
            super::command::CommandRequest {
                home: home.to_path_buf(),
                routine_key: key.to_string(),
                routine_id: key.id,
                run_id: pointer.run_id.clone(),
                program,
                argv,
                cwd,
                max_runtime: Duration::from_millis(finish::run_limit_ms(&run).max(0) as u64),
            },
        ))));
    }
    if wake {
        return wake_plan(home, &principal, &key, &def, pointer, &values);
    }
    let rendered = template::render(&def.task, &values);
    if !rendered.unresolved.is_empty() {
        return failed(
            reason::TASK_UNRENDERABLE,
            format!("no value for {}", rendered.unresolved.join(", ")),
        );
    }
    // The task is the agent's whole prompt: whoever wrote it decides what the
    // agent does, including whether it calls `signal_done`. A wake's launch
    // starts with what the wake would have typed.
    let prompt = wake_prompt.map_or(rendered.text, str::to_string);
    if prompt.trim().is_empty() {
        return failed(
            reason::TASK_UNRENDERABLE,
            "the task is empty once filled in".to_string(),
        );
    }
    if remote && wake_prompt.is_none() {
        return remote_fresh(store, &key, &def, pointer, &principal, prompt, now);
    }

    // Resolved at its current revision on every run, so an edit to the agent
    // applies to the next run without touching the routine.
    let agent = match super::agents::prepare_run(home, &principal, &key, &def, &prompt) {
        Ok(agent) => agent,
        Err(detail) => return failed(reason::AGENT_UNAVAILABLE, detail),
    };
    for warning in &agent.warnings {
        log::warn!(target: "routines", "routine {key} run {}: {}", pointer.run_id, warning.message);
    }

    let cwd = if matches!(
        def.target.resolve(),
        Ok(super::model::RoutineTarget::TeamWorkspace)
    ) {
        match super::workspace::prepare(home, &principal, &key, &cwd, &agent) {
            Ok(dir) => dir,
            Err(detail) => return failed(reason::LAUNCH_FAILED, detail),
        }
    } else {
        cwd
    };

    let agent_ref = agent.revision_label();
    let started = store.transaction(|tx| {
        Ok(tx.execute(
            "UPDATE routine_runs SET phase = 'running', started_at = ?2, agent_ref = ?3
             WHERE run_id = ?1 AND phase = 'queued' AND reason IS NULL",
            params![pointer.run_id, now, agent_ref],
        )? == 1)
    })?;
    if !started {
        return Ok(Err(NotStarted::Gone));
    }
    let handle = handle(&pointer.routine_key, &pointer.run_id);
    let provenance = agent.provenance(
        Some(RunProvenance {
            run_id: pointer.run_id.clone(),
            story_id: handle.clone(),
            automation_id: None,
            trigger_source: Some(TRIGGER_SOURCE.to_string()),
            team: Some(key.team.clone()),
            workspace: Some(cwd.to_string_lossy().into_owned()),
            scheduled_at: pointer.scheduled_at.map(rfc3339),
            fired_at: Some(rfc3339(pointer.fired_at)),
            routine_key: Some(pointer.routine_key.clone()),
            routine: Some(crate::session_usage::task_capture::RoutineBrief {
                name: def.name.clone(),
                task: def.task.clone(),
            }),
            ..Default::default()
        }),
        &handle,
    );
    Ok(Ok(Prepared::Fresh(Box::new(LaunchRequest {
        codesense_home: home.to_path_buf(),
        org: key.org.clone(),
        team: key.team.clone(),
        handle,
        cwd,
        prompt,
        agent,
        // A wake always lands in a tab.
        visible: def.policy.visible || wake_prompt.is_some(),
        max_runtime: Duration::from_millis(
            u64::try_from(finish::run_limit_ms(&run)).unwrap_or_default(),
        ),
        provenance,
    }))))
}

/// A fresh run on another machine (SW-AGR-2b): moved to `running` before it
/// launches, as every fresh run is, under its run's request id.
fn remote_fresh(
    store: &RoutineStateStore,
    key: &RoutineKey,
    def: &RoutineDef,
    pointer: &RoutinePointer,
    principal: &Principal,
    prompt: String,
    now: i64,
) -> StoreResult<Result<Prepared, NotStarted>> {
    let failed =
        |reason: &'static str, detail: String| Ok(Err(NotStarted::Failed { reason, detail }));
    let agent = match def.agent.key_for(key) {
        Ok(agent) => agent,
        Err(e) => return failed(reason::AGENT_UNAVAILABLE, e.to_string()),
    };
    let target = match def.target.resolve() {
        Ok(target) => target,
        Err(e) => return failed(reason::TARGET_UNAVAILABLE, e.to_string()),
    };
    let machine = def
        .target
        .machine
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_string();
    let started = store.transaction(|tx| {
        Ok(tx.execute(
            // Its machine and launch request are on the run before the
            // Create goes out: a stop or a timeout meanwhile knows where.
            "UPDATE routine_runs SET phase = 'running', started_at = ?2, agent_ref = ?3,
                    launch_machine = ?4, launch_request = ?5
             WHERE run_id = ?1 AND phase = 'queued' AND reason IS NULL",
            params![
                pointer.run_id,
                now,
                agent.to_string(),
                machine,
                super::wake::launch_request_id(&pointer.run_id).to_string()
            ],
        )? == 1)
    })?;
    if !started {
        return Ok(Err(NotStarted::Gone));
    }
    Ok(Ok(Prepared::Remote(Box::new(super::wake::RemoteLaunch {
        run_id: pointer.run_id.clone(),
        routine_key: key.to_string(),
        routine_name: def.name.clone(),
        org: key.org.clone(),
        team: key.team.clone(),
        agent: agent.to_string(),
        machine,
        target,
        prompt,
        principal: principal.clone(),
        request_id: super::wake::launch_request_id(&pointer.run_id),
    }))))
}

/// Capture every actually started run here. The persisted start time gates capture, so
/// queued wakes and held runs create nothing. Delivery calls the same seam once it starts.
pub(super) fn note_run_start(store: &RoutineStateStore, run_id: &str, cwd: Option<&Path>) {
    let result = (|| -> Result<(), String> {
        let Some(run) = store.run(run_id).map_err(|e| e.to_string())? else { return Ok(()); };
        let Some(started) = run.started_at else { return Ok(()); };
        let Some(home) = store.path().parent().and_then(Path::parent) else { return Ok(()); };
        let pointer = super::wake::kept_pointer(store, run_id)
            .ok_or_else(|| format!("started routine {run_id} has no dispatch request"))?;
        let principal = Principal::from(pointer.principal.as_str());
        let key = RoutineKey::parse(&run.routine_key)
            .ok_or_else(|| format!("invalid routine key {}", run.routine_key))?;
        let def = pinned_revision(home, &principal, &key, run.revision)?;
        let routine = crate::session_usage::task_capture::RoutineBrief {
            name: def.name, task: def.task,
        };
        let policy_dir = crate::session_usage::task_capture::policy_dir_for(home, &principal, &key.org, &key.team);
        crate::session_usage::task_capture::record_routine_start(
            store.path(),
            &crate::session_usage::task_capture::RoutineStart {
                org: &key.org, team: &key.team, run_id, routine: &routine,
                policy_dir: policy_dir.as_deref(),
                cwd: cwd.or_else(|| run.command.cwd.as_deref().map(Path::new)),
                session_id: run.session_id.as_deref(), now_ms: started,
            },
        );
        Ok(())
    })();
    if let Err(error) = result {
        log::warn!(target: "session_usage", "routine task {run_id} not captured: {error}");
    }
}

fn wake_plan(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    def: &RoutineDef,
    pointer: &RoutinePointer,
    values: &BTreeMap<String, String>,
) -> StoreResult<Result<Prepared, NotStarted>> {
    let failed =
        |reason: &'static str, detail: String| Ok(Err(NotStarted::Failed { reason, detail }));
    let agent = match def.agent.key_for(key) {
        Ok(agent) => agent,
        Err(e) => return failed(reason::AGENT_UNAVAILABLE, e.to_string()),
    };
    if super::service::get_current(home, principal, key).is_ok_and(|d| d.def.is_archived()) {
        return Ok(Err(NotStarted::Undelivered {
            reason: "archived",
            detail: "the routine was archived before its wake was sent".to_string(),
        }));
    }
    let text = crate::delivery::authority::render_wake_text(&def.task, values);
    if text.text.trim().is_empty() {
        return failed(
            reason::TASK_UNRENDERABLE,
            "the task is empty once filled in".to_string(),
        );
    }
    if !text.withheld.is_empty() {
        log::info!(target: "routines", "routine {key} run {}: wake text leaves {} as written", pointer.run_id, text.withheld.join(", "));
    }
    // §9.5: the marker names the routine and who last edited it.
    let editor = super::service::routine_dir_of(home, principal, key)
        .ok()
        .and_then(|dir| defs::read_revision(&dir, pointer.revision).ok().flatten())
        .map(|record| record.author.name)
        .filter(|name| !name.trim().is_empty());
    let marker = match editor {
        Some(editor) => format!("{} (last edited by {editor})", def.name),
        None => def.name.clone(),
    };
    Ok(Ok(Prepared::Wake(Box::new(super::wake::WakePlan {
        pointer: pointer.clone(),
        routine_key: key.to_string(),
        marker: marker
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect(),
        agent: agent.to_string(),
        text: text.text,
        header: crate::delivery::authority::run_header(&def.name, &pointer.run_id),
        max_wait_ms: super::wake::max_wait_ms(def),
        target: super::wake::TargetMachine::parse(def.target.machine.as_deref()),
        preferred: super::wake::preferred_handles(pointer),
        principal: principal.clone(),
        routine_name: def.name.clone(),
        where_target: def
            .target
            .resolve()
            .unwrap_or(super::model::RoutineTarget::TeamWorkspace),
        limits: super::wake::wake_limits_of(
            def,
            &key.to_string(),
            &super::host::local_host_id(home).unwrap_or_default(),
            super::state::RoutineStateStore::open(home)
                .and_then(|store| store.wake_budget(&key.to_string()))
                .unwrap_or(0),
        ),
    }))))
}

/// Record the launched session on the run, whatever has happened to it since,
/// and return its phase now.
fn record_session(
    store: &RoutineStateStore,
    run_id: &str,
    session_id: &str,
    woke_on: Option<&str>,
) -> StoreResult<String> {
    let phase = store.transaction(|tx| {
        tx.execute(
            "UPDATE routine_runs SET session_id = ?2 WHERE run_id = ?1 AND session_id IS NULL",
            params![run_id, session_id],
        )?;
        if let Some(machine) = woke_on {
            tx.execute(
                "UPDATE routine_runs SET wake_machine = ?2 WHERE run_id = ?1",
                params![run_id, machine],
            )?;
        }
        Ok(tx.query_row(
            "SELECT phase FROM routine_runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )?)
    })?;
    crate::session_usage::task_outcome::link_run_session(store.path(), run_id, session_id);
    Ok(phase)
}

/// The definition as of `revision`: its immutable snapshot, or the current
/// definition when that is the same revision.
fn pinned_revision(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    revision: u32,
) -> Result<RoutineDef, String> {
    let dir = super::service::routine_dir_of(home, principal, key).map_err(|e| e.to_string())?;
    defs::read_pinned(&dir, revision)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("routine {key} has no revision {revision}"))
}

/// The values a run's task renders with: the built-ins, the declared variables
/// (defaults under the fire's own values), and the firing event's fields.
pub(crate) fn template_values(
    def: &RoutineDef,
    pointer: &RoutinePointer,
    state: Option<&super::state::RoutineState>,
) -> BTreeMap<String, String> {
    let fired = pointer.scheduled_at.unwrap_or(pointer.fired_at);
    let mut values = BTreeMap::new();
    values.insert("date".to_string(), local_date(def, fired));
    values.insert("timestamp".to_string(), rfc3339(fired));
    values.insert(
        "last_run_at".to_string(),
        state
            .and_then(|s| s.last_run_at)
            .map(rfc3339)
            .unwrap_or_default(),
    );
    values.insert(
        "last_run_summary".to_string(),
        state
            .and_then(|s| s.last_result_text.clone())
            .unwrap_or_default(),
    );
    values.extend(declared_values(def, &pointer.variables));
    if def.is_command() {
        values.extend(pointer.variables.clone());
    }
    if let Some(serde_json::Value::Object(fields)) = &pointer.trigger_payload {
        for (field, value) in fields {
            let text = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            values.insert(format!("trigger.{field}"), text);
        }
    }
    values
}

/// The routine's declared variables: their defaults, under the fire's own
/// values for the ones it declares (an undeclared value is ignored).
pub(super) fn declared_values(
    def: &RoutineDef,
    fired: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut values: BTreeMap<String, String> = def
        .variables
        .iter()
        .filter_map(|(name, v)| Some((name.clone(), v.default.clone()?)))
        .collect();
    for (name, value) in fired {
        if def.variables.contains_key(name) {
            values.insert(name.clone(), value.clone());
        }
    }
    values
}

/// The fire's calendar date in the routine's first schedule zone, else this
/// machine's.
fn local_date(def: &RoutineDef, at_ms: i64) -> String {
    let zone = def
        .triggers
        .iter()
        .find_map(|t| match t {
            RoutineTrigger::Schedule(spec) if !spec.tz.trim().is_empty() => Some(spec.tz.clone()),
            _ => None,
        })
        .unwrap_or_else(super::schedule::system_zone);
    let at = at(at_ms);
    match zone.parse::<chrono_tz::Tz>() {
        Ok(tz) => at.with_timezone(&tz).format("%Y-%m-%d").to_string(),
        Err(_) => at.format("%Y-%m-%d").to_string(),
    }
}

fn at(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_default()
}

fn rfc3339(ms: i64) -> String {
    at(ms).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The production [`Launcher`]: a headless process, or a terminal tab when the
/// routine asks to be visible, through the app's launch spines.
pub struct AppLauncher {
    app: tauri::AppHandle,
    /// The app's wake environment (`delivery::app::install` manages it):
    /// one for the app, so a launch in flight is seen by every fire.
    wake: Option<std::sync::Arc<super::wake_app::AppWakeEnv>>,
}

impl AppLauncher {
    pub fn new(app: tauri::AppHandle) -> Self {
        use tauri::Manager;
        let wake = app
            .try_state::<std::sync::Arc<super::wake_app::AppWakeEnv>>()
            .map(|env| env.inner().clone());
        Self { app, wake }
    }
}

impl Launcher for AppLauncher {
    fn command(&self, request: super::command::CommandRequest) -> LaunchFuture<'_> {
        Box::pin(async move {
            use tauri::Manager;
            let registry = self
                .app
                .state::<crate::commands::dashboard_launch::DashboardLaunchState>();
            let (session, pid, cancel) = super::command::register(&request, &registry)?;
            let app = self.app.clone();
            let done = session.clone();
            tauri::async_runtime::spawn(async move {
                super::command::capture(request, pid, cancel).await;
                app.state::<crate::commands::dashboard_launch::DashboardLaunchState>()
                    .release_for_session(&done);
            });
            Ok(session)
        })
    }
    fn launch(&self, request: LaunchRequest) -> LaunchFuture<'_> {
        Box::pin(async move {
            use crate::commands::dashboard_launch::DashboardLaunchState;
            use crate::commands::mcp_workspace::{
                launch_managed_dir_session, LaunchHome, ManagedLaunchOptions, SessionView,
            };
            use tauri::Manager;
            let LaunchRequest {
                codesense_home,
                org,
                team,
                handle,
                cwd,
                prompt,
                agent,
                visible,
                max_runtime,
                provenance,
            } = request;
            if visible {
                let fresh = launch_managed_dir_session(
                    &self.app,
                    &handle,
                    &cwd,
                    &codesense_home,
                    Some(&prompt),
                    ManagedLaunchOptions {
                        provenance: Some(provenance),
                        // The routine's team holds the run's inbox: never whichever team
                        // the viewer shows when it fires or reads.
                        record: crate::commands::dashboard_launch::LaunchRecord {
                            meta: crate::commands::dashboard_launch::LaunchMeta {
                                team: Some(crate::commands::dashboard_launch::LaunchTeam {
                                    org: org.clone(),
                                    team: team.clone(),
                                }),
                                ..Default::default()
                            },
                            started_with: None,
                            // routines never go through `open_workspace`,
                            // so the deadline sweep is out of scope here. The
                            // routine policy's own `max_runtime` watchdog bounds
                            // their lifetime.
                            max_runtime_ms: None,
                        },
                        extra_args: Vec::new(),
                        agent,
                        home: LaunchHome::new(
                            &org,
                            Some(&team),
                            &handle,
                            "agent",
                            &cwd,
                            SessionView::Tab { focus: false },
                        ),
                    },
                )
                .await?;
                if !fresh {
                    return Err(format!(
                        "busy: a session is already active for handle '{handle}'"
                    ));
                }
                self.app
                    .state::<DashboardLaunchState>()
                    .resolve(&handle)
                    .ok_or_else(|| format!("the session launched for '{handle}' is not bound"))
            } else {
                crate::commands::dashboard_launch::launch_headless_session(
                    self.app.clone(),
                    self.app.state::<DashboardLaunchState>(),
                    handle.clone(),
                    handle,
                    cwd.to_string_lossy().into_owned(),
                    prompt,
                    String::new(),
                    Some(TRIGGER_SOURCE.to_string()),
                    Some(crate::commands::dashboard_launch::LaunchedIn {
                        org,
                        team: Some(team),
                    }),
                    Some(provenance),
                    Some(max_runtime),
                    agent,
                )
                .await
                .map(|launched| launched.session_id)
            }
        })
    }

    fn kill_run(&self, run_id: &str) {
        use super::finish::Sessions;
        super::finish::AppSessions::new(self.app.clone()).kill_run(run_id, None);
    }

    fn kill_remote(&self, run_id: &str, machine: &str, session: &str) {
        use super::finish::Sessions;
        super::finish::AppSessions::new(self.app.clone()).kill_on(
            run_id,
            Some(machine),
            Some(session),
        );
    }

    fn wake_env(&self) -> Option<std::sync::Arc<dyn super::wake::WakeEnv>> {
        self.wake
            .clone()
            .map(|env| env as std::sync::Arc<dyn super::wake::WakeEnv>)
    }
}
