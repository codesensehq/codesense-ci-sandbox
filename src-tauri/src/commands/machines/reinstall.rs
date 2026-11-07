// SPDX-License-Identifier: MIT
//! SW-AGR-19: the confirmed fallback for updating a remote terminal server
//! that cannot take part in the proof-bound safe replacement — one older than
//! `ShutdownForReplacement`, a legacy install, or a frozen server without a
//! proof. The person has already confirmed that its sessions end.
//!
//! Stop the server by the pid it greeted with (its executable must be one of
//! this account's installed builds), install this app's build, reconnect.
//! Nothing is installed beside a server that would not stop.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use codesense_session_server::check::{self, Trust};
use codesense_session_server::ssh::{HostKey, Mux};
use codesense_session_server::stop::stop_script;
use serde::Serialize;
use tauri::Manager;

use super::legacy_retire::{saved_install_input, saved_reach_input, LEGACY_ROOT, PRIVATE_ROOT};
use super::{
    env_of, install_server, keychain, private_setup, reach, CredentialStore, Env,
    InstallTerminalServerResult,
};
use crate::commands::host_connection_manager::{ConnectOrigin, HostConnectionManager, UpdatePlan};

/// How the person's "Update terminal server" runs on this machine.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TerminalServerUpdateMode {
    /// The proof-bound safe replacement (prepare, then restart).
    Replace,
    /// The confirmed fallback: stop by pid, install, reconnect.
    Reinstall,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ReinstallResult {
    /// The new server is connected; `sessions` terminals ended with the old,
    /// and `restored` says which of its tabs reopened (PR-4).
    Reinstalled {
        sessions: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        restored: Option<super::restore::RestoreSummary>,
    },
    /// More sessions run than the person confirmed: nothing ran; confirm
    /// again with this count.
    SessionsChanged { sessions: u32 },
    /// It stopped partway. `old_stopped`: the old server (and its sessions)
    /// is gone, and the machine offers the update again, which installs and
    /// connects; otherwise the old server still runs and is reconnected.
    Failed {
        #[serde(rename = "oldStopped")]
        old_stopped: bool,
        repair: String,
    },
}

/// What the stop script reported.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StopReport {
    /// These servers stopped, ending this many terminals in all.
    Stopped { sessions: u32 },
    /// No server ran (the named one had already exited).
    Nothing,
    /// A server still holds terminals (only when not stopping them).
    Running { sessions: u32 },
}

/// Read the stop script's report. A named pid that is not this root's, a
/// refused signal, or a server that would not stop is an error: nothing may
/// be installed beside it.
fn parse_stop_report(output: &str) -> Result<StopReport, String> {
    let (mut stopped, mut running, mut any) = (0u32, 0u32, false);
    for line in output.split('\n') {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["server", _pid, count, outcome] => {
                let count: u32 = count
                    .parse()
                    .map_err(|_| "the terminal server's stop report was malformed".to_string())?;
                match *outcome {
                    "stopped" => {
                        stopped = stopped.saturating_add(count);
                        any = true;
                    }
                    "running" => running = running.saturating_add(count),
                    "alive" => {
                        return Err(
                            "the stop signal was sent, but the old terminal server still runs"
                                .into(),
                        )
                    }
                    _ => return Err("the terminal server's stop report was malformed".into()),
                }
            }
            ["gone", _pid] => {}
            ["foreign", pid] => {
                return Err(format!(
                "process {pid} is not one of this machine's terminal servers; nothing was stopped"
            ))
            }
            ["denied", pid] => {
                return Err(format!(
                    "not permitted to stop process {pid}; nothing was stopped"
                ))
            }
            _ => {}
        }
    }
    Ok(if running > 0 {
        StopReport::Running { sessions: running }
    } else if any {
        StopReport::Stopped { sessions: stopped }
    } else {
        StopReport::Nothing
    })
}

/// Where this machine's servers are installed, under its account's home.
fn install_root(env: &Env, machine_id: &str) -> Result<&'static str, String> {
    Ok(if private_setup::claims(env, machine_id)? {
        PRIVATE_ROOT
    } else {
        LEGACY_ROOT
    })
}

/// Stop the server `pid`, verified over the pinned host key. With no pid
/// known, nothing is stopped that holds terminals: it is only reported.
fn stop_saved(
    env: &Env,
    store: &dyn CredentialStore,
    machine_id: &str,
    pid: Option<u32>,
) -> Result<StopReport, String> {
    let root = install_root(env, machine_id)?;
    let (input, pinned) = saved_reach_input(env, machine_id)?;
    let pinned = HostKey::parse(&pinned)?;
    let reach = reach(env, store, machine_id, &input)?;
    check::verified_host_key(&reach.connector, Trust::Pinned(&pinned), &env.scratch())?;
    let output = reach.ssh(Mux::Fresh).output_within(
        &stop_script(pid.is_some(), root, pid),
        Duration::from_secs(20),
    )?;
    if !output.status.success() {
        return Err(format!(
            "the old terminal server could not be checked there (exit {:?})",
            output.status.code()
        ));
    }
    let report = parse_stop_report(&String::from_utf8_lossy(&output.stdout));
    log::warn!(
        target: "machines",
        "reinstall {machine_id}: stop pid {pid:?} under ~/{root}: {report:?}"
    );
    report
}

/// Whether the install may follow this stop report, and the terminals that
/// ended.
fn after_stop(report: Result<StopReport, String>) -> Result<u32, ReinstallResult> {
    match report {
        Ok(StopReport::Stopped { sessions }) => Ok(sessions),
        Ok(StopReport::Nothing) => Ok(0),
        Ok(StopReport::Running { sessions }) => Err(ReinstallResult::Failed {
            old_stopped: false,
            repair: format!(
                "a terminal server with {sessions} terminals runs there that CodeSense could not identify; stop it on the machine, then update"
            ),
        }),
        Err(repair) => Err(ReinstallResult::Failed {
            old_stopped: false,
            repair,
        }),
    }
}

/// Stop, then install: the blocking half of [`reinstall_machine_terminal_server`].
pub(super) fn stop_and_install(
    env: &Env,
    store: &dyn CredentialStore,
    machine_id: &str,
    pid: Option<u32>,
) -> Result<u32, ReinstallResult> {
    let sessions = after_stop(stop_saved(env, store, machine_id, pid))?;
    let installed = saved_install_input(env, machine_id)
        .and_then(|input| install_server(env, store, Some(machine_id), &input));
    after_install(installed).map(|()| sessions)
}

fn after_install(
    installed: Result<InstallTerminalServerResult, String>,
) -> Result<(), ReinstallResult> {
    match installed {
        Ok(InstallTerminalServerResult::Installed { .. }) => Ok(()),
        Ok(InstallTerminalServerResult::Failed { message, .. }) | Err(message) => {
            Err(ReinstallResult::Failed {
                old_stopped: true,
                repair: format!(
                    "the old terminal server stopped, but installing failed: {message}"
                ),
            })
        }
    }
}

/// How the server `machine_id` is connected to now stands against this app.
async fn connected_relation(
    app: &tauri::AppHandle,
    machine_id: &str,
) -> Option<super::terminal_server_info::ServerRelation> {
    let client = crate::commands::pty_backend::resolve_spawn_backend(app, machine_id, "")
        .ok()
        .and_then(|backend| backend.client())?;
    let env = env_of(app).await.ok()?;
    crate::commands::await_blocking("reinstall relation", move || {
        Ok(super::terminal_server_info::remote_info(
            &super::shipped_terminal_builds(&env),
            client.announced_version(),
            client.proto_minor(),
            client.build_stamp(),
        )
        .relation)
    })
    .await
    .ok()
}

/// One reinstall per machine at a time (two windows, or an agent and a person).
static IN_FLIGHT: Mutex<Option<HashSet<String>>> = Mutex::new(None);

struct InFlight(String);
impl InFlight {
    fn claim(machine_id: &str) -> Result<Self, String> {
        let mut held = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if !held
            .get_or_insert_with(HashSet::new)
            .insert(machine_id.to_string())
        {
            return Err("this machine's terminal server is already being updated".into());
        }
        Ok(Self(machine_id.to_string()))
    }
}
impl Drop for InFlight {
    fn drop(&mut self) {
        if let Some(held) = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            held.remove(&self.0);
        }
    }
}

/// How a machine's terminal server would update, and how many sessions it
/// ends (asked of the running server; `None` when it cannot say).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalServerUpdatePlan {
    pub mode: TerminalServerUpdateMode,
    pub sessions: Option<u32>,
}

/// The sessions the machine's live server runs now, if it is connected.
pub(crate) async fn live_sessions(app: &tauri::AppHandle, machine_id: &str) -> Option<u32> {
    let client = crate::commands::pty_backend::resolve_spawn_backend(app, machine_id, "")
        .ok()
        .and_then(|backend| backend.client())?;
    crate::commands::await_blocking("terminal server sessions", move || {
        Ok(client.live_session_count())
    })
    .await
    .ok()
    .flatten()
}

/// SW-AGR-19: which update this machine's terminal server gets, and the
/// sessions it ends, for the confirmation.
#[tauri::command]
pub async fn machine_terminal_server_update_mode(
    app: tauri::AppHandle,
    machine_id: String,
) -> Result<TerminalServerUpdatePlan, String> {
    let mode = match app
        .state::<HostConnectionManager>()
        .update_plan(&machine_id)?
    {
        UpdatePlan::Replace => TerminalServerUpdateMode::Replace,
        UpdatePlan::Reinstall { .. } => TerminalServerUpdateMode::Reinstall,
    };
    Ok(TerminalServerUpdatePlan {
        mode,
        sessions: live_sessions(&app, &machine_id).await,
    })
}

/// SW-AGR-19: the confirmed reinstall. Refused unless `confirm_end_sessions`
/// (every session on the old server ends), unless the safe replacement is
/// unavailable for this server, and when more sessions run than the person
/// confirmed (`expected_sessions`).
#[tauri::command]
pub async fn reinstall_machine_terminal_server(
    app: tauri::AppHandle,
    machine_id: String,
    confirm_end_sessions: bool,
    expected_sessions: Option<u32>,
) -> Result<ReinstallResult, String> {
    // SW-AGR-19 PR-8: one update at a time — never beside an Update all run.
    super::update_all::refuse_while_running()?;
    reinstall_now(
        app,
        machine_id,
        confirm_end_sessions,
        expected_sessions,
        None,
    )
    .await
}

/// The sessions `machine_id`'s connected server runs now, sorted.
pub(crate) async fn live_session_ids(
    app: &tauri::AppHandle,
    machine_id: &str,
) -> Option<Vec<String>> {
    let client = crate::commands::pty_backend::resolve_spawn_backend(app, machine_id, "")
        .ok()
        .and_then(|backend| backend.client())?;
    crate::commands::await_blocking("terminal server sessions", move || {
        Ok(client.live_session_id_list())
    })
    .await
    .ok()
    .flatten()
}

/// The confirmed reinstall itself (the Update all run's own step, which also
/// binds it to the confirmed session ids: one that started since refuses it
/// before anything stops).
pub(crate) async fn reinstall_now(
    app: tauri::AppHandle,
    machine_id: String,
    confirm_end_sessions: bool,
    expected_sessions: Option<u32>,
    confirmed_ids: Option<Vec<String>>,
) -> Result<ReinstallResult, String> {
    if !confirm_end_sessions {
        return Err("confirmEndSessions must be true: updating ends the server's sessions".into());
    }
    let _in_flight = InFlight::claim(&machine_id)?;
    let manager = app.state::<HostConnectionManager>();
    let pid = match manager.update_plan(&machine_id)? {
        UpdatePlan::Reinstall { pid } => pid,
        UpdatePlan::Replace => {
            return Err("this terminal server supports the safe replacement; use it".into())
        }
    };
    // The confirmation named how many sessions end; more since is a new
    // question, never a silent extra: answer with the count to confirm.
    if let (Some(expected), Some(live)) =
        (expected_sessions, live_sessions(&app, &machine_id).await)
    {
        if live > expected {
            return Ok(ReinstallResult::SessionsChanged { sessions: live });
        }
    }
    if let Some(confirmed) = &confirmed_ids {
        match live_session_ids(&app, &machine_id).await {
            Some(live) if live.iter().all(|id| confirmed.contains(id)) => {}
            Some(live) => {
                return Ok(ReinstallResult::SessionsChanged {
                    sessions: u32::try_from(live.len()).unwrap_or(u32::MAX),
                })
            }
            None => return Err("its sessions could not be read; nothing was stopped".into()),
        }
    }
    // Record its tabs while the old server can still answer for them.
    let env = env_of(&app).await?;
    let home = env.codesense_home.clone();
    let label = super::summary_of(&env, &machine_id)
        .map(|summary| summary.label)
        .unwrap_or_else(|_| machine_id.clone());
    let recorded = super::restore::capture(&app, &home, &machine_id).await;
    // Let go of the old server before it stops, so nothing reconnects to it.
    if let Err(error) = manager.disconnect(&machine_id).await {
        super::restore::finish(
            &app,
            &home,
            &machine_id,
            &label,
            recorded,
            super::restore::UpdateEnd::OldRunning,
        )
        .await;
        return Err(format!(
            "couldn't let go of the old terminal server: {error}"
        ));
    }
    let id = machine_id.clone();
    let stopped = crate::commands::await_blocking("reinstall_machine_terminal_server", move || {
        Ok(stop_and_install(&env, keychain().as_ref(), &id, pid))
    })
    .await
    .unwrap_or_else(|repair| {
        Err(ReinstallResult::Failed {
            old_stopped: false,
            repair,
        })
    });
    let failure = match stopped {
        Ok(sessions) => match manager
            .connect_remote(
                app.clone(),
                machine_id.clone(),
                ConnectOrigin::ExplicitForeground,
            )
            .await
        {
            Ok(()) if manager.connected_backend(&machine_id).is_ok() => {
                // The server now answering must be this app's build.
                if let Some(relation) = connected_relation(&app, &machine_id).await {
                    if relation == super::terminal_server_info::ServerRelation::Older {
                        let failed = ReinstallResult::Failed {
                            old_stopped: true,
                            repair: "the machine answered with an older terminal server than this app's; update again".into(),
                        };
                        log::warn!(target: "machines", "reinstall {machine_id}: still older after install");
                        return Ok(failed);
                    }
                }
                log::warn!(
                    target: "machines",
                    "reinstall {machine_id}: reinstalled; {sessions} terminals ended"
                );
                let restored = super::restore::finish(
                    &app,
                    &home,
                    &machine_id,
                    &label,
                    recorded,
                    super::restore::UpdateEnd::Replaced,
                )
                .await;
                return Ok(ReinstallResult::Reinstalled { sessions, restored });
            }
            Ok(()) => ReinstallResult::Failed {
                old_stopped: true,
                repair: "the new terminal server is installed but did not connect; update again"
                    .into(),
            },
            Err(repair) => ReinstallResult::Failed {
                old_stopped: true,
                repair: format!(
                    "the new terminal server is installed but did not connect: {repair}"
                ),
            },
        },
        Err(failed) => failed,
    };
    if let ReinstallResult::Failed {
        old_stopped,
        repair,
    } = &failure
    {
        log::warn!(
            target: "machines",
            "reinstall {machine_id}: failed (old stopped: {old_stopped}): {repair}"
        );
        // The record of its tabs: kept for the next update when they are gone
        // with the old server, forgotten when they still run.
        let end = if *old_stopped {
            super::restore::UpdateEnd::OldStopped
        } else {
            super::restore::UpdateEnd::OldRunning
        };
        super::restore::finish(&app, &home, &machine_id, &label, recorded, end).await;
        if *old_stopped {
            // The machine offers the update again, which installs and connects.
            manager.record_update_failure(&machine_id, true, repair.clone());
        } else {
            // Nothing stopped: hold the old server again. After the
            // disconnect an explicit connect admits whatever runs there: a
            // live server is connected again, a frozen or legacy one is frozen
            // again (and offers the update again), an unreachable one is
            // retried by the manager. Nothing to record.
            let _ = manager
                .connect_remote(
                    app.clone(),
                    machine_id.clone(),
                    ConnectOrigin::ExplicitForeground,
                )
                .await;
        }
    }
    Ok(failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing is installed beside a server that was not stopped, and the
    /// result says whether the old server is gone.
    #[test]
    fn agr19_only_a_stopped_or_absent_server_is_followed_by_the_install() {
        assert_eq!(
            parse_stop_report("server 41 3 stopped\n"),
            Ok(StopReport::Stopped { sessions: 3 })
        );
        assert_eq!(parse_stop_report("gone 41\n"), Ok(StopReport::Nothing));
        assert_eq!(parse_stop_report(""), Ok(StopReport::Nothing));
        assert_eq!(
            parse_stop_report("Welcome!\nserver 9 2 running\n"),
            Ok(StopReport::Running { sessions: 2 })
        );
        for refused in [
            "foreign 41\n",
            "denied 41\n",
            "server 41 3 alive\n",
            "server 41 x stopped\n",
        ] {
            assert!(parse_stop_report(refused).is_err(), "{refused}");
        }
        assert_eq!(after_stop(Ok(StopReport::Stopped { sessions: 3 })), Ok(3));
        assert_eq!(after_stop(Ok(StopReport::Nothing)), Ok(0));
        for kept in [
            after_stop(Ok(StopReport::Running { sessions: 2 })),
            after_stop(parse_stop_report("foreign 41\n")),
            after_stop(Err("ssh refused".into())),
        ] {
            assert!(matches!(
                kept,
                Err(ReinstallResult::Failed {
                    old_stopped: false,
                    ..
                })
            ));
        }
        assert!(matches!(
            after_install(Err("disk full".into())),
            Err(ReinstallResult::Failed {
                old_stopped: true,
                ..
            })
        ));
        assert!(matches!(
            after_install(Ok(InstallTerminalServerResult::Failed {
                ok: false,
                code: "failed",
                message: "upload cut".into(),
            })),
            Err(ReinstallResult::Failed {
                old_stopped: true,
                ..
            })
        ));
    }

    /// The fallback's script: by pid, under this machine's own root.
    #[test]
    fn agr19_the_stop_targets_one_pid_under_one_root() {
        let private = stop_script(true, PRIVATE_ROOT, Some(4242)).script;
        assert!(private.contains("only=4242\n"));
        assert!(private.contains("readlink -f \"$HOME/.codesense/remote-private/v1\""));
        let legacy = stop_script(true, LEGACY_ROOT, None).script;
        assert!(legacy.contains("only=\n"));
        assert!(legacy.contains("readlink -f \"$HOME/.codesense\" "));
    }

    #[test]
    fn agr19_one_reinstall_per_machine_at_a_time() {
        let held = InFlight::claim("box-a").unwrap();
        assert!(InFlight::claim("box-a").is_err());
        let other = InFlight::claim("box-b").unwrap();
        drop(held);
        let again = InFlight::claim("box-a").unwrap();
        drop((other, again));
    }
}
