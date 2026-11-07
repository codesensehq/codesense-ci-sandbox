// SPDX-License-Identifier: MIT
//! Stopping the shared-account terminal server a legacy machine record left
//! running, when that record is removed or switched to a private account.
//! The server keeps running on its host after the record is gone, so this is
//! asked for before the record (and the sign-in it carries) goes.
use std::time::Duration;

use codesense_session_server::check::{self, Trust};
use codesense_session_server::install;
use codesense_session_server::ssh::{HostKey, Mux};
use serde::Serialize;

use super::{
    env_of, keychain, private_setup, reach, stored_sign_in, transport_snapshot, CredentialStore,
    Env, MachineDraftInput, MachineMode,
};

/// What the legacy terminal server on a machine's account is doing.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum LegacyServerRetirement {
    /// No legacy terminal server runs there.
    NotRunning,
    /// One runs and still holds `sessions` terminals; it was left running,
    /// for the person to confirm.
    Running { sessions: u32 },
    /// It was stopped, ending the `sessions` terminals it still held.
    Stopped { sessions: u32 },
}

/// One round trip: find this account's legacy terminal servers (an installed
/// `~/.codesense/host/<version>` binary, never the private root's), count the
/// terminals each holds (its children on a pty), and stop each one when
/// `stop` is 1 or it holds none. Prints `server <pid> <sessions> <outcome>`.
pub(super) fn retire_script(stop: bool) -> install::Step {
    codesense_session_server::stop::stop_script(stop, LEGACY_ROOT, None)
}

/// The legacy install root, under the account's home.
pub(super) const LEGACY_ROOT: &str = ".codesense";
/// A private-account install root, under the account's home.
pub(super) const PRIVATE_ROOT: &str = ".codesense/remote-private/v1";

/// Read the script's report. Any server left alive is an error; one still
/// holding terminals (not asked to stop) wins over one that was stopped.
pub(super) fn parse_retirement(output: &str) -> Result<LegacyServerRetirement, String> {
    let mut running = 0u32;
    let mut stopped = None::<u32>;
    for line in output.lines() {
        // The shared stop script names a server it was refused (SW-AGR-19).
        if let Some(pid) = line
            .strip_prefix("denied ")
            .or_else(|| line.strip_prefix("foreign "))
        {
            return Err(format!(
                "the legacy terminal server ({}) could not be stopped",
                pid.trim()
            ));
        }
        let mut parts = line.split_whitespace();
        let (Some("server"), Some(_pid), Some(count), Some(outcome), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            continue;
        };
        let count: u32 = count
            .parse()
            .map_err(|_| "the legacy terminal server's report was malformed".to_string())?;
        match outcome {
            "running" => running = running.saturating_add(count),
            "stopped" => stopped = Some(stopped.unwrap_or(0).saturating_add(count)),
            "alive" => return Err("the legacy terminal server did not stop".into()),
            _ => return Err("the legacy terminal server's report was malformed".into()),
        }
    }
    Ok(if running > 0 {
        LegacyServerRetirement::Running { sessions: running }
    } else if let Some(sessions) = stopped {
        LegacyServerRetirement::Stopped { sessions }
    } else {
        LegacyServerRetirement::NotRunning
    })
}

/// The sign-in and pinned host key a saved machine — still legacy, or just
/// switched to a private account — reaches its host with.
pub(super) fn saved_reach_input(
    env: &Env,
    machine_id: &str,
) -> Result<(MachineDraftInput, String), String> {
    if private_setup::claims(env, machine_id)? {
        return private_setup::reach_input(env, machine_id);
    }
    let snapshot = transport_snapshot(env, machine_id)?;
    let declaration = &snapshot.declaration;
    let pinned = declaration
        .pinned_host_key
        .clone()
        .ok_or_else(|| format!("Machine {machine_id:?} has no verified host key"))?;
    let input = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: declaration.label.clone(),
        host: declaration.host.clone(),
        port: declaration.port,
        username: snapshot.binding.username.clone(),
        jump_host: declaration.jump_host.clone(),
        sign_in: stored_sign_in(&snapshot.binding)?,
        trusted_host_key_fp: None,
    };
    Ok((input, pinned))
}

/// The sign-in a saved machine reaches its host with, for the update
/// fallback's install.
pub(super) fn saved_install_input(
    env: &Env,
    machine_id: &str,
) -> Result<MachineDraftInput, String> {
    saved_reach_input(env, machine_id).map(|(input, _)| input)
}

pub(super) fn retire_saved(
    env: &Env,
    store: &dyn CredentialStore,
    machine_id: &str,
    stop_with_sessions: bool,
) -> Result<LegacyServerRetirement, String> {
    let (input, pinned) = saved_reach_input(env, machine_id)?;
    let pinned = HostKey::parse(&pinned)?;
    let reach = reach(env, store, machine_id, &input)?;
    check::verified_host_key(&reach.connector, Trust::Pinned(&pinned), &env.scratch())?;
    let output = reach
        .ssh(Mux::Fresh)
        .output_within(&retire_script(stop_with_sessions), Duration::from_secs(20))?;
    if !output.status.success() {
        return Err(format!(
            "checking for the legacy terminal server exited {:?}",
            output.status.code()
        ));
    }
    parse_retirement(&String::from_utf8_lossy(&output.stdout))
}

/// Stop the legacy terminal server `machine_id`'s account still runs. One
/// that holds no terminals is stopped at once; one that does is stopped only
/// with `stop_with_sessions`, else reported as `running` for the caller to
/// confirm. An unreachable host is an error: nothing was stopped.
#[tauri::command]
pub async fn retire_legacy_terminal_server(
    app: tauri::AppHandle,
    machine_id: String,
    stop_with_sessions: bool,
) -> Result<LegacyServerRetirement, String> {
    let env = env_of(&app).await?;
    crate::commands::await_blocking("retire_legacy_terminal_server", move || {
        retire_saved(&env, keychain().as_ref(), &machine_id, stop_with_sessions)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_reads_running_stopped_or_none() {
        assert_eq!(parse_retirement(""), Ok(LegacyServerRetirement::NotRunning));
        assert_eq!(
            parse_retirement("server 6307 3 running\n"),
            Ok(LegacyServerRetirement::Running { sessions: 3 })
        );
        assert_eq!(
            parse_retirement("Welcome!\nserver 6307 0 stopped\nserver 7001 2 stopped\n"),
            Ok(LegacyServerRetirement::Stopped { sessions: 2 })
        );
        assert_eq!(
            parse_retirement("server 1 0 stopped\nserver 2 1 running\n"),
            Ok(LegacyServerRetirement::Running { sessions: 1 })
        );
        assert!(parse_retirement("server 6307 0 alive\n").is_err());
        assert!(parse_retirement("server 6307 x running\n").is_err());
        assert!(parse_retirement("denied 6307\n").is_err());
    }
}
