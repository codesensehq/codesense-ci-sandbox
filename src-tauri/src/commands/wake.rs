// SPDX-License-Identifier: MIT
//! SW-AGR-9 / SW-AGR-3: the wake and timer commands the app window calls.

use tauri::{AppHandle, Manager, Webview};

use crate::commands::machines::LOCAL_MACHINE_ID;
use crate::delivery::app::DeliveryState;
use crate::delivery::authority::{Caller, SessionRef};
use crate::delivery::events::WakeHistoryEntry;
use crate::delivery::wakeup::{self, ScheduledWakeup, WakeupAsk, WakeupRow};
use crate::delivery::DeliveryError;

/// A tab's wake history (its timer popover): its last deliveries, oldest
/// first. `machineId` names a remote tab's machine; omitted, this Mac.
#[tauri::command]
pub(crate) fn wake_history(
    app: AppHandle,
    session_id: String,
    machine_id: Option<String>,
) -> Vec<WakeHistoryEntry> {
    app.try_state::<DeliveryState>()
        .map(|service| {
            service.history(
                machine_id.as_deref().unwrap_or(LOCAL_MACHINE_ID),
                &session_id,
            )
        })
        .unwrap_or_default()
}

/// Proof that the person at this desktop acted: carried by
/// [`Caller::Human`], which skips a bypass session's wake consent and may set
/// a timer on any tab. Its field is private to this module, so nothing else
/// (no MCP tool, no routine host) can make one; tests use `for_test`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HumanGesture(());

impl HumanGesture {
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self(())
    }
}

/// The app's one window. Every webview this app opens today is `main`, so
/// this check does not tell a dashboard from the app: what keeps dashboards
/// out is their sandbox (an opaque-origin iframe with no Tauri IPC, see
/// `html-viewer.tsx`) and the bridge allowlist with no timer verb. Agents
/// reach the app only through MCP, never through these commands. The check
/// refuses any other window a later change might add.
const PERSON_WINDOW: &str = "main";

/// The person behind a command from the app window, or a refusal.
fn person(webview: &Webview) -> Result<Caller, String> {
    person_in(webview.label())
}

fn person_in(label: &str) -> Result<Caller, String> {
    if label == PERSON_WINDOW {
        Ok(Caller::Human(HumanGesture(())))
    } else {
        Err("Reminders are set from the CodeSense window".into())
    }
}

fn delivery(app: &AppHandle) -> Result<DeliveryState, String> {
    app.try_state::<DeliveryState>()
        .map(|state| state.inner().clone())
        .ok_or_else(|| "Wake delivery is not running".to_string())
}

/// What to tell the person about `error`, naming machines as they do.
async fn say(app: &AppHandle, error: DeliveryError) -> String {
    let labels = match error {
        DeliveryError::Offline { .. } => crate::commands::machines::machine_labels(app).await,
        _ => Default::default(),
    };
    wakeup::message_on(&error, |machine| labels.get(machine).cloned())
}

/// The tab a person acts on: `machineId` names a remote tab's machine;
/// omitted, this Mac. A headless run has no tab to remind, and on a remote
/// machine only a terminal tab this desktop opened there (never a remote
/// task) is one.
fn tab(
    app: &AppHandle,
    session_id: &str,
    machine_id: Option<String>,
) -> Result<SessionRef, String> {
    if session_id.trim().is_empty() {
        return Err("sessionId is required".into());
    }
    let machine = machine_id.unwrap_or_else(|| LOCAL_MACHINE_ID.to_string());
    let launch = app.try_state::<crate::commands::dashboard_launch::DashboardLaunchState>();
    let (headless, remote_tab) = match launch {
        Some(launch) => (
            launch.headless_pid_for_session(session_id).is_some(),
            launch
                .remote_shell_for_session(session_id)
                .map(|record| record.machine_id),
        ),
        None => (false, None),
    };
    wakeup::require_tab(session_id, &machine, headless, remote_tab.as_deref())
        .map_err(|e| wakeup::message(&e))?;
    Ok(SessionRef::new(machine, session_id))
}

/// Run `job` off the async runtime; a refusal reads as people say it
/// (machines by their labels). The MCP timer tools use it too.
pub(crate) async fn blocking<T: Send + 'static>(
    app: &AppHandle,
    label: &'static str,
    job: impl FnOnce() -> Result<T, DeliveryError> + Send + 'static,
) -> Result<T, String> {
    match crate::commands::await_blocking(label, move || Ok(job())).await? {
        Ok(value) => Ok(value),
        Err(error) => Err(say(app, error).await),
    }
}

/// SW-AGR-3 "Remind this agent…": set a timer on a tab, local or remote,
/// with the person as its author. It wakes only that tab.
#[tauri::command]
pub(crate) async fn remind_session(
    app: AppHandle,
    webview: Webview,
    session_id: String,
    machine_id: Option<String>,
    timer: WakeupAsk,
) -> Result<ScheduledWakeup, String> {
    let caller = person(&webview)?;
    let target = tab(&app, &session_id, machine_id)?;
    let service = delivery(&app)?;
    blocking(&app, "wake.remind_session", move || {
        wakeup::schedule(
            &service,
            &caller,
            &target,
            &timer,
            crate::routines::finish::now_ms() as u64,
            wakeup::HUMAN_AUTHOR,
        )
    })
    .await
}

/// SW-AGR-3: a tab's active timers (its timer popover), as the terminal
/// server that runs it holds them.
#[tauri::command]
pub(crate) async fn session_timers(
    app: AppHandle,
    webview: Webview,
    session_id: String,
    machine_id: Option<String>,
) -> Result<Vec<WakeupRow>, String> {
    let caller = person(&webview)?;
    let target = tab(&app, &session_id, machine_id)?;
    let service = delivery(&app)?;
    blocking(&app, "wake.session_timers", move || {
        wakeup::list(&service, &caller, &target)
    })
    .await
}

/// SW-AGR-3: cancel one of a tab's timers (the popover's Cancel); whether
/// it was active.
#[tauri::command]
pub(crate) async fn cancel_session_timer(
    app: AppHandle,
    webview: Webview,
    session_id: String,
    machine_id: Option<String>,
    timer_id: String,
) -> Result<bool, String> {
    let caller = person(&webview)?;
    let target = tab(&app, &session_id, machine_id)?;
    let service = delivery(&app)?;
    blocking(&app, "wake.cancel_session_timer", move || {
        wakeup::cancel(&service, &caller, &target, &timer_id)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    // Only the app window acts as the person, and only this module makes the
    // gesture that says so.
    #[test]
    fn only_the_main_window_acts_as_the_person() {
        assert_eq!(person_in("main"), Ok(Caller::Human(HumanGesture(()))));
        for other in ["dashboard", "other", ""] {
            assert!(person_in(other).is_err(), "{other}");
        }
    }
}
