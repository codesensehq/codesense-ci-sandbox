// SPDX-License-Identifier: MIT
//! Per-launch runtime enforcement. Registry snapshots and pending notifications
//! retain session identity across graceful exit, teardown and handle reuse.

use std::sync::Arc;

use codesense_pty_core::agent_cli::{adapter, AgentCli};

use super::super::agent_inbox::{self, AppInboxHost, InboxHost, NoteExtras};
use super::super::{mcp_workspace, pty, pty_backend};
use super::{DashboardLaunchState, LaunchTeam};

pub(crate) const EXIT_GRACE_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeadlineEntry {
    pub handle: String,
    pub session_id: String,
    pub launched_by: Option<String>,
    pub team: Option<LaunchTeam>,
    pub max_runtime_ms: Option<u64>,
    pub deadline_ms: u64,
    pub exit_sent_at_ms: Option<u64>,
    /// Set at restoration, never inferred from a late scheduler tick.
    pub restored_overdue: bool,
    /// Confirmed enforcement waiting for inbox delivery; true means killed.
    pub enforced: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SweepAction {
    pub handle: String,
    pub kind: SweepActionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SweepActionKind {
    SendExit,
    Enforce { killed: bool },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SweepReport {
    pub actions: Vec<SweepAction>,
}

/// All effects run outside the registry lock. A failed kill or notification
/// stays pending for the next pass; unknown liveness never means exited.
pub(crate) trait DeadlinePort {
    fn is_current(&self, entry: &DeadlineEntry) -> bool;
    fn send_exit(&self, entry: &DeadlineEntry, now_ms: u64) -> u64;
    fn exit_submitted(&self, entry: &DeadlineEntry, at_ms: u64);
    fn kill(&self, entry: &DeadlineEntry) -> bool;
    fn note(&self, entry: &DeadlineEntry, text: &str) -> bool;
    fn mark_exit_sent(&self, entry: &DeadlineEntry, at_ms: u64) -> bool;
    fn is_running(&self, entry: &DeadlineEntry) -> Option<bool>;
    fn mark_enforced(&self, entry: &DeadlineEntry, killed: bool);
    fn complete(&self, entry: &DeadlineEntry);
}

pub(crate) fn sweep(
    now_ms: u64,
    entries: &[DeadlineEntry],
    port: &dyn DeadlinePort,
) -> SweepReport {
    let mut actions = Vec::new();
    for entry in entries {
        if now_ms < entry.deadline_ms || !port.is_current(entry) {
            continue;
        }
        let killed = if let Some(killed) = entry.enforced {
            killed
        } else {
            if !entry.restored_overdue {
                match entry.exit_sent_at_ms {
                    None => {
                        match port.is_running(entry) {
                            Some(false) => {
                                port.complete(entry);
                                continue;
                            }
                            None => continue,
                            Some(true) => {}
                        }
                        if port.mark_exit_sent(entry, now_ms) {
                            let submitted_at = port.send_exit(entry, now_ms);
                            port.exit_submitted(entry, submitted_at);
                            actions.push(SweepAction {
                                handle: entry.handle.clone(),
                                kind: SweepActionKind::SendExit,
                            });
                        }
                        continue;
                    }
                    Some(at) if now_ms < at.saturating_add(EXIT_GRACE_MS) => continue,
                    Some(_) => {}
                }
            }
            let Some(running) = port.is_running(entry) else {
                continue;
            };
            if running && !port.kill(entry) {
                continue;
            }
            port.mark_enforced(entry, running);
            actions.push(SweepAction {
                handle: entry.handle.clone(),
                kind: SweepActionKind::Enforce { killed: running },
            });
            running
        };
        let text = format_note(entry, killed);
        if port.note(entry, &text) {
            port.complete(entry);
        }
    }
    SweepReport { actions }
}

fn format_note(entry: &DeadlineEntry, killed: bool) -> String {
    let budget = entry
        .max_runtime_ms
        .map(human_duration)
        .unwrap_or_else(|| "its budget".into());
    let outcome = if killed { "was stopped" } else { "exited" };
    format!(
        "{} exceeded maxRuntime {budget} and {outcome}",
        entry.handle
    )
}

fn human_duration(ms: u64) -> String {
    let minutes = ms / 60_000;
    if minutes >= 60 && minutes.is_multiple_of(60) {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

/// The line that asks the session's CLI to exit: its own, or, when which CLI runs cannot
/// be read, the one every CLI shares (`None` when they differ: it is stopped once its
/// grace ends).
fn exit_line(cli: Option<AgentCli>) -> Option<&'static str> {
    let line = |cli| adapter(cli).control().exit_line;
    match cli {
        Some(cli) => Some(line(cli)),
        None => {
            let shared = line(AgentCli::ALL[0]);
            AgentCli::ALL
                .into_iter()
                .all(|cli| line(cli) == shared)
                .then_some(shared)
        }
    }
}

pub(crate) struct AppDeadlinePort {
    pub app: tauri::AppHandle,
}

impl DeadlinePort for AppDeadlinePort {
    fn is_current(&self, entry: &DeadlineEntry) -> bool {
        use tauri::Manager;
        self.app
            .state::<DashboardLaunchState>()
            .deadline_is_current(entry)
    }

    fn send_exit(&self, entry: &DeadlineEntry, _now_ms: u64) -> u64 {
        // The session's CLI is read only when the CLIs' exit lines differ.
        let line = exit_line(None).or_else(|| {
            exit_line(Some(crate::mcp::activity::session_cli(
                &self.app,
                &entry.session_id,
            )?))
        });
        let Some(line) = line else {
            log::warn!(target: "agents", "deadline exit to {}: its CLI cannot be read", entry.session_id);
            return super::now_epoch_ms();
        };
        tauri::async_runtime::block_on(async {
            for (sid, outcome) in
                mcp_workspace::send_to_sessions(&self.app, vec![entry.session_id.clone()], |_| {
                    line.into()
                })
                .await
            {
                if let Err(error) = outcome {
                    log::warn!(target: "agents", "deadline {line} to {sid} failed: {error}");
                }
            }
        });
        super::now_epoch_ms()
    }

    fn exit_submitted(&self, entry: &DeadlineEntry, at_ms: u64) {
        use tauri::Manager;
        self.app
            .state::<DashboardLaunchState>()
            .deadline_exit_submitted(entry, at_ms);
    }

    fn kill(&self, entry: &DeadlineEntry) -> bool {
        use tauri::Manager;
        let state = self.app.state::<DashboardLaunchState>();
        state.record_kill_reason(&entry.session_id, "maxRuntime");
        if let Err(error) = pty::try_kill_session(&self.app, &entry.session_id) {
            state.take_kill_reason(&entry.session_id);
            log::warn!(target: "agents", "deadline kill {} failed: {error}", entry.session_id);
            return false;
        }
        true
    }

    fn note(&self, entry: &DeadlineEntry, text: &str) -> bool {
        deliver_note(
            &self.app,
            &entry.handle,
            entry.launched_by.as_deref(),
            entry.team.as_ref(),
            text,
            NoteExtras::default(),
        )
    }

    fn mark_exit_sent(&self, entry: &DeadlineEntry, at_ms: u64) -> bool {
        use tauri::Manager;
        let changed = self
            .app
            .state::<DashboardLaunchState>()
            .mark_exit_sent(entry, at_ms);
        if changed {
            super::emit_session_state_changed(&self.app);
        }
        changed
    }

    fn is_running(&self, entry: &DeadlineEntry) -> Option<bool> {
        pty_backend::alive_session_ids(&self.app, &[&entry.session_id])
            .known()
            .map(|ids| ids.contains(&entry.session_id))
    }

    fn mark_enforced(&self, entry: &DeadlineEntry, killed: bool) {
        use tauri::Manager;
        let state = self.app.state::<DashboardLaunchState>();
        state.mark_deadline_enforced(entry, killed);
        // Natural exit retains a replay tab, but its launch no longer needs a slot.
        if !killed {
            state.release_for_session(&entry.session_id);
        }
    }

    fn complete(&self, entry: &DeadlineEntry) {
        use tauri::Manager;
        self.app
            .state::<DashboardLaunchState>()
            .complete_deadline(entry);
        super::emit_session_state_changed(&self.app);
    }
}

/// Where a launch's note goes: its launcher while that is live, else the human.
#[derive(Debug, PartialEq, Eq)]
enum NoteTarget<'a> {
    Launcher(&'a str),
    Human,
}

/// `None` while the launcher's liveness cannot be read, so the note waits.
fn note_target<'a>(
    launched_by: Option<&'a str>,
    launcher_live: impl FnOnce(&str) -> Option<bool>,
) -> Option<NoteTarget<'a>> {
    match launched_by {
        None => Some(NoteTarget::Human),
        Some(launcher) => Some(if launcher_live(launcher)? {
            NoteTarget::Launcher(launcher)
        } else {
            NoteTarget::Human
        }),
    }
}

/// Post `text` to the launch's live launcher, else to the human of its team. True once stored.
fn deliver_note(
    app: &tauri::AppHandle,
    handle: &str,
    launched_by: Option<&str>,
    team: Option<&LaunchTeam>,
    text: &str,
    extras: NoteExtras<'_>,
) -> bool {
    tauri::async_runtime::block_on(async {
        let host: Arc<dyn InboxHost> = AppInboxHost::port(app.clone());
        let team = team.map(|t| (t.org.clone(), t.team.clone()));
        let Some(target) = note_target(launched_by, |launcher| launcher_is_live(app, launcher))
        else {
            return false;
        };
        let outcome = match target {
            NoteTarget::Launcher(launcher) => {
                agent_inbox::note_to_session_with(&host, launcher, text, text, extras).await
            }
            NoteTarget::Human => {
                agent_inbox::note_to_human_with(&host, team.as_ref(), text, text, extras).await
            }
        };
        if let Err(error) = &outcome {
            log::warn!(target: "agents", "launch note for {handle} failed: {error}");
        }
        outcome.is_ok()
    })
}

fn launcher_is_live(app: &tauri::AppHandle, launcher: &str) -> Option<bool> {
    super::list_sessions_inner(app)
        .ok()
        .map(|rows| rows.iter().any(|row| row.handle == launcher && row.alive))
}

pub(crate) fn run_deadline_sweep(app: &tauri::AppHandle) {
    use tauri::Manager;
    let Some(state) = app.try_state::<DashboardLaunchState>() else {
        return;
    };
    let entries = state.deadline_snapshot();
    if !entries.is_empty() {
        sweep(
            super::now_epoch_ms(),
            &entries,
            &AppDeadlinePort { app: app.clone() },
        );
    }
}

/// A bound launch that no host has yet observed ready: a candidate for the startup note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartupEntry {
    pub handle: String,
    pub session_id: String,
    pub launched_by: Option<String>,
    pub team: Option<LaunchTeam>,
}

/// Effects of the startup sweep, run outside the registry lock like [`DeadlinePort`].
pub(crate) trait StartupPort {
    /// Whether the sessions projection shows this entry's session stuck on a startup
    /// screen now (live, awaiting native readiness, past its bound).
    fn stuck(&self, entry: &StartupEntry) -> bool;
    fn note(&self, entry: &StartupEntry, text: &str) -> bool;
    fn complete(&self, entry: &StartupEntry);
}

/// One inbox note per stuck launch, completed only after the note is stored; a failed
/// note retries on the next pass. A session that became ready or exited gets none.
pub(crate) fn sweep_startup(entries: &[StartupEntry], port: &dyn StartupPort) {
    for entry in entries {
        if !port.stuck(entry) {
            continue;
        }
        let text = format!("{} is waiting on a startup screen", entry.handle);
        if port.note(entry, &text) {
            port.complete(entry);
        }
    }
}

/// The startup note's idempotency key: one per bound session.
fn startup_request_id(entry: &StartupEntry) -> String {
    format!("{}:startup", entry.session_id)
}

/// The note's parts: a click that opens the affected session (a handle can be reused
/// by a replacement), its needs-you kind, and the once-per-session request id.
fn startup_extras<'a>(entry: &StartupEntry, request_id: &'a str) -> NoteExtras<'a> {
    use crate::weft::inbox::{MessageKind, MessageOnClick};
    NoteExtras {
        on_click: Some(MessageOnClick::GotoAgent {
            agent_id: entry.session_id.clone(),
        }),
        kind: Some(MessageKind::NeedsYou),
        request_id: Some(request_id),
    }
}

struct AppStartupPort {
    app: tauri::AppHandle,
}

impl StartupPort for AppStartupPort {
    fn stuck(&self, entry: &StartupEntry) -> bool {
        use tauri::Manager;
        self.app
            .try_state::<crate::live_hub::LiveHub>()
            .is_some_and(|hub| hub.startup_stuck(&entry.session_id))
    }

    fn note(&self, entry: &StartupEntry, text: &str) -> bool {
        let request_id = startup_request_id(entry);
        deliver_note(
            &self.app,
            &entry.handle,
            entry.launched_by.as_deref(),
            entry.team.as_ref(),
            text,
            startup_extras(entry, &request_id),
        )
    }

    fn complete(&self, entry: &StartupEntry) {
        use tauri::Manager;
        self.app
            .state::<DashboardLaunchState>()
            .complete_startup(entry);
    }
}

pub(crate) fn run_startup_sweep(app: &tauri::AppHandle) {
    use tauri::Manager;
    let Some(state) = app.try_state::<DashboardLaunchState>() else {
        return;
    };
    let entries = state.startup_candidates();
    if !entries.is_empty() {
        sweep_startup(&entries, &AppStartupPort { app: app.clone() });
    }
}

#[cfg(test)]
mod tests {
    use super::super::{LaunchMeta, LaunchRecord};
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn a_session_is_asked_to_exit_in_its_own_clis_words() {
        for cli in AgentCli::ALL {
            assert_eq!(exit_line(Some(cli)), Some(adapter(cli).control().exit_line));
        }
        // Different CLI exit commands require the session runtime to be known.
        assert_eq!(exit_line(None), None);
    }

    struct RegistryPort {
        state: DashboardLaunchState,
        actions: RefCell<Vec<String>>,
        kill_ok: Cell<bool>,
        note_ok: Cell<bool>,
        running: Cell<Option<bool>>,
        send_delay_ms: Cell<u64>,
    }

    impl RegistryPort {
        fn new() -> Self {
            let state = DashboardLaunchState::new(2);
            state
                .begin_with_provenance(
                    "worker",
                    None,
                    LaunchRecord {
                        meta: LaunchMeta::default(),
                        started_with: None,
                        max_runtime_ms: Some(60_000),
                    },
                )
                .unwrap();
            state.bind("worker", "session", None).unwrap();
            Self {
                state,
                actions: RefCell::default(),
                kill_ok: Cell::new(true),
                note_ok: Cell::new(true),
                running: Cell::new(Some(true)),
                send_delay_ms: Cell::new(0),
            }
        }
        fn entry(&self) -> DeadlineEntry {
            self.state.deadline_snapshot().remove(0)
        }
        fn tick(&self, now: u64) {
            sweep(now, &self.state.deadline_snapshot(), self);
        }
    }

    impl DeadlinePort for RegistryPort {
        fn is_current(&self, e: &DeadlineEntry) -> bool {
            self.state.deadline_is_current(e)
        }
        fn send_exit(&self, _: &DeadlineEntry, now_ms: u64) -> u64 {
            self.actions.borrow_mut().push("exit".into());
            now_ms + self.send_delay_ms.get()
        }
        fn exit_submitted(&self, entry: &DeadlineEntry, at_ms: u64) {
            self.state.deadline_exit_submitted(entry, at_ms);
        }
        fn kill(&self, e: &DeadlineEntry) -> bool {
            if !self.kill_ok.get() {
                return false;
            }
            self.actions.borrow_mut().push("kill".into());
            self.state.release_for_session(&e.session_id);
            self.running.set(Some(false));
            true
        }
        fn note(&self, _: &DeadlineEntry, text: &str) -> bool {
            if !self.note_ok.get() {
                return false;
            }
            self.actions.borrow_mut().push(text.into());
            true
        }
        fn mark_exit_sent(&self, e: &DeadlineEntry, at: u64) -> bool {
            self.state.mark_exit_sent(e, at)
        }
        fn is_running(&self, _: &DeadlineEntry) -> Option<bool> {
            self.running.get()
        }
        fn mark_enforced(&self, e: &DeadlineEntry, killed: bool) {
            self.state.mark_deadline_enforced(e, killed);
        }
        fn complete(&self, e: &DeadlineEntry) {
            self.state.complete_deadline(e);
        }
    }

    #[test]
    fn grace_enforcement_and_notification_are_once_per_launch() {
        let port = RegistryPort::new();
        let deadline = port.entry().deadline_ms;
        port.tick(deadline - 1);
        assert!(port.actions.borrow().is_empty());
        port.tick(deadline);
        port.tick(deadline);
        port.tick(deadline + 29_999);
        assert_eq!(*port.actions.borrow(), ["exit"]);
        let stale = port.entry();
        port.tick(deadline + EXIT_GRACE_MS);
        port.tick(deadline + 90_000);
        sweep(deadline + 90_000, &[stale], &port);
        assert_eq!(
            *port.actions.borrow(),
            [
                "exit",
                "kill",
                "worker exceeded maxRuntime 1m and was stopped"
            ]
        );
        assert_eq!(port.state.available(), 2);
    }

    #[test]
    fn graceful_release_preserves_one_pending_note() {
        let port = RegistryPort::new();
        let deadline = port.entry().deadline_ms;
        port.tick(deadline);
        port.state.release_for_session("session");
        port.running.set(Some(false));
        port.tick(deadline + EXIT_GRACE_MS);
        port.tick(deadline + 90_000);
        assert_eq!(
            *port.actions.borrow(),
            ["exit", "worker exceeded maxRuntime 1m and exited"]
        );
        assert!(port.state.deadline_snapshot().is_empty());
    }

    #[test]
    fn late_live_sweep_still_gives_a_full_grace() {
        let port = RegistryPort::new();
        let late = port.entry().deadline_ms + 90_000;
        port.tick(late);
        port.tick(late + 29_999);
        assert_eq!(*port.actions.borrow(), ["exit"]);
        port.tick(late + EXIT_GRACE_MS);
        assert_eq!(port.actions.borrow().len(), 3);
    }

    #[test]
    fn overdue_restore_does_not_get_a_grace_even_one_millisecond_late() {
        let port = RegistryPort::new();
        port.state
            .lock()
            .by_handle
            .get_mut("worker")
            .unwrap()
            .restored_overdue = true;
        let deadline = port.entry().deadline_ms;
        port.tick(deadline + 1);
        assert_eq!(
            *port.actions.borrow(),
            ["kill", "worker exceeded maxRuntime 1m and was stopped"]
        );
    }

    #[test]
    fn stale_snapshot_cannot_mark_a_replacement_session() {
        let port = RegistryPort::new();
        let stale = port.entry();
        port.state.bind("worker", "replacement", None).unwrap();
        sweep(stale.deadline_ms, &[stale], &port);
        assert!(port.actions.borrow().is_empty());
        assert!(port.entry().exit_sent_at_ms.is_none());
    }

    #[test]
    fn failed_kill_and_unknown_liveness_never_claim_success() {
        let port = RegistryPort::new();
        let deadline = port.entry().deadline_ms;
        port.tick(deadline);
        port.kill_ok.set(false);
        port.tick(deadline + EXIT_GRACE_MS);
        port.running.set(None);
        port.kill_ok.set(true);
        port.tick(deadline + 60_000);
        assert_eq!(*port.actions.borrow(), ["exit"]);
        port.running.set(Some(true));
        port.tick(deadline + 90_000);
        assert_eq!(port.actions.borrow().len(), 3);
    }

    #[test]
    fn notification_failure_retries_without_killing_twice() {
        let port = RegistryPort::new();
        let deadline = port.entry().deadline_ms;
        port.tick(deadline);
        port.note_ok.set(false);
        port.tick(deadline + EXIT_GRACE_MS);
        assert_eq!(*port.actions.borrow(), ["exit", "kill"]);
        port.note_ok.set(true);
        port.tick(deadline + 60_000);
        port.tick(deadline + 90_000);
        assert_eq!(
            *port.actions.borrow(),
            [
                "exit",
                "kill",
                "worker exceeded maxRuntime 1m and was stopped"
            ]
        );
    }
    #[test]
    fn a_session_finished_before_grace_is_not_reported_as_timed_out() {
        let port = RegistryPort::new();
        let deadline = port.entry().deadline_ms;
        port.running.set(Some(false));
        port.tick(deadline);
        port.tick(deadline + EXIT_GRACE_MS);
        assert!(port.actions.borrow().is_empty());
        assert!(port.state.deadline_snapshot().is_empty());
    }

    #[test]
    fn grace_begins_after_exit_submission_finishes() {
        let port = RegistryPort::new();
        let deadline = port.entry().deadline_ms;
        port.send_delay_ms.set(20_000);
        port.tick(deadline);
        port.tick(deadline + EXIT_GRACE_MS);
        assert_eq!(*port.actions.borrow(), ["exit"]);
        port.tick(deadline + 20_000 + EXIT_GRACE_MS);
        assert_eq!(port.actions.borrow().len(), 3);
    }

    struct StartupRig {
        state: DashboardLaunchState,
        stuck: Cell<bool>,
        launcher_live: Cell<Option<bool>>,
        note_ok: Cell<bool>,
        notes: RefCell<Vec<(NoteTargetOwned, String)>>,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum NoteTargetOwned {
        Launcher(String),
        Human,
    }

    impl StartupRig {
        fn new(launched_by: Option<&str>) -> Self {
            let state = DashboardLaunchState::new(2);
            state
                .begin_with_provenance(
                    "worker",
                    None,
                    LaunchRecord {
                        meta: LaunchMeta {
                            launched_by: launched_by.map(str::to_string),
                            ..Default::default()
                        },
                        started_with: None,
                        max_runtime_ms: None,
                    },
                )
                .unwrap();
            state.bind("worker", "session", None).unwrap();
            Self {
                state,
                stuck: Cell::new(true),
                launcher_live: Cell::new(Some(true)),
                note_ok: Cell::new(true),
                notes: RefCell::default(),
            }
        }
        fn tick(&self) {
            sweep_startup(&self.state.startup_candidates(), self);
        }
        fn notes(&self) -> Vec<String> {
            self.notes.borrow().iter().map(|(_, t)| t.clone()).collect()
        }
    }

    impl StartupPort for StartupRig {
        fn stuck(&self, _: &StartupEntry) -> bool {
            self.stuck.get()
        }
        fn note(&self, entry: &StartupEntry, text: &str) -> bool {
            let Some(target) =
                note_target(entry.launched_by.as_deref(), |_| self.launcher_live.get())
            else {
                return false;
            };
            if !self.note_ok.get() {
                return false;
            }
            let target = match target {
                NoteTarget::Launcher(launcher) => NoteTargetOwned::Launcher(launcher.into()),
                NoteTarget::Human => NoteTargetOwned::Human,
            };
            self.notes.borrow_mut().push((target, text.into()));
            true
        }
        fn complete(&self, entry: &StartupEntry) {
            self.state.complete_startup(entry);
        }
    }

    #[test]
    fn the_startup_notes_click_targets_the_affected_session_even_after_the_handle_is_reused() {
        use crate::weft::inbox::{MessageKind, MessageOnClick};
        let rig = StartupRig::new(Some("lead"));
        let original = rig.state.startup_candidates().remove(0);
        // The handle is rebound to a replacement before the note is opened.
        rig.state.bind("worker", "replacement", None).unwrap();
        let extras = startup_extras(&original, "s:startup");
        assert_eq!(
            extras.on_click,
            Some(MessageOnClick::GotoAgent {
                agent_id: "session".into()
            })
        );
        assert_eq!(extras.kind, Some(MessageKind::NeedsYou));
        assert_eq!(extras.request_id, Some("s:startup"));
    }

    #[test]
    fn a_stuck_launch_notes_its_live_launcher_once() {
        let rig = StartupRig::new(Some("lead"));
        rig.tick();
        rig.tick();
        rig.tick();
        assert_eq!(
            *rig.notes.borrow(),
            [(
                NoteTargetOwned::Launcher("lead".into()),
                "worker is waiting on a startup screen".to_string()
            )]
        );
        assert!(rig.state.startup_candidates().is_empty());
    }

    #[test]
    fn a_stuck_launch_falls_back_to_the_human_when_the_launcher_is_gone_or_a_person_launched_it() {
        for (launched_by, live) in [(Some("lead"), Some(false)), (None, Some(true))] {
            let rig = StartupRig::new(launched_by);
            rig.launcher_live.set(live);
            rig.tick();
            assert_eq!(rig.notes.borrow()[0].0, NoteTargetOwned::Human);
            assert_eq!(rig.notes.borrow().len(), 1);
        }
    }

    #[test]
    fn unknown_launcher_liveness_or_a_failed_note_retries_without_duplicating() {
        let rig = StartupRig::new(Some("lead"));
        rig.launcher_live.set(None);
        rig.tick();
        assert!(rig.notes().is_empty());
        rig.launcher_live.set(Some(true));
        rig.note_ok.set(false);
        rig.tick();
        assert!(rig.notes().is_empty());
        rig.note_ok.set(true);
        rig.tick();
        rig.tick();
        assert_eq!(rig.notes().len(), 1);
    }

    #[test]
    fn a_launch_ready_before_the_bound_or_exited_gets_no_note() {
        // The projection does not show it stuck: ready, exited or inside the bound.
        let rig = StartupRig::new(Some("lead"));
        rig.stuck.set(false);
        rig.tick();
        assert!(rig.notes().is_empty());
        // Once the registry stamps it ready it is no longer a candidate at all.
        rig.stuck.set(true);
        rig.state.mark_ready("session");
        rig.tick();
        assert!(rig.notes().is_empty());
        assert!(rig.state.startup_candidates().is_empty());
    }

    #[test]
    fn a_rebound_session_is_noted_afresh_and_a_stale_entry_cannot_complete_it() {
        let rig = StartupRig::new(Some("lead"));
        let stale = rig.state.startup_candidates().remove(0);
        rig.state.bind("worker", "replacement", None).unwrap();
        rig.state.complete_startup(&stale);
        let fresh = rig.state.startup_candidates();
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].session_id, "replacement");
        assert_ne!(startup_request_id(&stale), startup_request_id(&fresh[0]));
    }
}
