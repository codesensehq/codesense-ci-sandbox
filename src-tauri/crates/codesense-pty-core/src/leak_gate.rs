// SPDX-License-Identifier: MIT
//! The PTY lifecycle iteration the resource leak gate measures: explicit
//! teardown, natural exit, and deadline recovery over real PTYs, each proven to
//! release its child, process group, reaper, and registry entries.

#[cfg(target_os = "macos")]
use std::sync::Arc;

/// Reaper tasks enqueued but not yet finished, so the gate can wait for every
/// background `wait` to drain before it samples the heap.
pub(crate) static PTY_REAPERS_IN_FLIGHT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Process evidence for the lifecycle the gate just exercised.
#[derive(Clone, Copy, Debug)]
pub struct LeakProcessEvidence {
    pub pid: u32,
    pub process_group: i32,
    pub started_seconds: Option<u64>,
    pub started_microseconds: Option<u64>,
    pub deadline_ms: u64,
    pub elapsed_ms: u128,
    pub alive_at_deadline: bool,
    pub reaped: bool,
}

#[cfg(target_os = "macos")]
pub fn leak_gate_iteration() -> Result<LeakProcessEvidence, String> {
    use crate::domain::TeardownReason;
    use crate::fixtures::{
        fixture_state, make_isolated_session, make_isolated_session_with_builder,
        stable_process_identity, wait_for_process_exit, LeakSessionGuard, NoopSink,
        RegisteredLeakSession,
    };
    use crate::term_state::StreamId;
    use portable_pty::CommandBuilder;

    // Partial construction must release its PTY pair without ever returning a
    // child. All fallible session assembly precedes the shared spawn point.
    let missing = CommandBuilder::new("/codesense-leak-gate/missing-executable");
    if let Ok(unexpected) = make_isolated_session_with_builder("partial-spawn", missing, false) {
        unexpected.pty.discard();
        return Err("partial-spawn fixture unexpectedly launched".to_string());
    }
    let state = fixture_state(Arc::new(NoopSink));
    let id = "resource-leak-gate";
    let baseline = registry_cardinalities(&state)?;
    // The shell keeps a descendant in its process group so teardown proves the
    // group oracle, not merely disappearance of the direct child.
    let mut session = LeakSessionGuard::new(make_isolated_session(id, "sleep 30 & wait", true)?);
    let child_pid = session
        .session()
        .pty
        .pid()
        .ok_or_else(|| "PTY child did not expose a process id".to_string())?
        as libc::pid_t;
    let process_group = unsafe { libc::getpgid(child_pid) };
    if process_group <= 0 {
        return Err(format!(
            "getpgid failed for pid {child_pid}: {}",
            std::io::Error::last_os_error()
        ));
    }
    let stable_identity = stable_process_identity(child_pid)?;
    state
        .stream_index
        .lock()
        .map_err(|error| format!("stream index lock: {error}"))?
        .insert(StreamId(9999), session.session().shared.clone());
    state
        .sessions
        .lock()
        .map_err(|error| format!("session lock: {error}"))?
        .insert(id.to_string(), session.take());
    let registered = RegisteredLeakSession { state: &state, id };

    // Exercise detach, including its idempotent missing-id path. The same state
    // mutation backs the host's command adapter.
    state.unsubscribe(StreamId(9999))?;
    state.unsubscribe(StreamId(9999))?;

    drop(registered);
    // Missing-id teardown is intentionally idempotent.
    state.teardown(id, TeardownReason::Expire);
    wait_for_reaper_drain(std::time::Duration::from_secs(2))
        .map_err(|error| format!("explicit teardown reaper: {error}"))?;

    let elapsed = wait_for_process_exit(
        child_pid,
        Some(process_group),
        stable_identity,
        std::time::Duration::from_secs(2),
    )
    .map_err(|error| format!("explicit teardown: {error}"))?;

    exercise_natural_exit_and_timeout(&state)?;

    let final_cardinalities = registry_cardinalities(&state)?;
    if final_cardinalities != baseline {
        return Err(format!(
            "LEAK[registry]: PTY registries did not return to baseline: baseline={baseline:?}, final={final_cardinalities:?}"
        ));
    }
    Ok(LeakProcessEvidence {
        pid: child_pid as u32,
        process_group,
        started_seconds: stable_identity.map(|identity| identity.started_seconds),
        started_microseconds: stable_identity.map(|identity| identity.started_microseconds),
        deadline_ms: 2_000,
        elapsed_ms: elapsed.as_millis(),
        alive_at_deadline: false,
        reaped: true,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn leak_gate_iteration() -> Result<LeakProcessEvidence, String> {
    Err("resource leak gate requires macOS process and fd censuses".to_string())
}

#[cfg(target_os = "macos")]
fn exercise_natural_exit_and_timeout(state: &crate::state::PtyState) -> Result<(), String> {
    use crate::fixtures::{
        make_isolated_session, stable_process_identity, wait_for_process_exit, LeakSessionGuard,
        RegisteredLeakSession,
    };

    let natural_id = "resource-leak-natural-exit";
    let mut natural = LeakSessionGuard::new(make_isolated_session(
        natural_id,
        "sleep 30 & kill -STOP $$; exit 0",
        false,
    )?);
    let natural_pid = natural
        .session()
        .pty
        .pid()
        .ok_or_else(|| "natural-exit child has no pid".to_string())?
        as libc::pid_t;
    let natural_pgid = unsafe { libc::getpgid(natural_pid) };
    if natural_pgid <= 0 {
        return Err("natural-exit process group is unavailable".to_string());
    }
    let natural_identity = stable_process_identity(natural_pid)?;
    state
        .sessions
        .lock()
        .map_err(|error| format!("session lock: {error}"))?
        .insert(natural_id.to_string(), natural.take());
    let natural_registered = RegisteredLeakSession {
        state,
        id: natural_id,
    };
    // Release the child's deterministic stop barrier, then let it exit on its
    // own before the normal teardown adapter schedules wait-handle reaping.
    let natural_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        // Repeated SIGCONT closes the spawn→self-stop race: one signal will
        // necessarily arrive after the shell reaches its deterministic stop.
        unsafe { libc::kill(natural_pid, libc::SIGCONT) };
        // Observe exit directly so this fixture proves teardown itself closes
        // the recorded process group; the liveness probe would pre-clean it.
        let child = state
            .sessions
            .lock()
            .map_err(|error| format!("session lock: {error}"))?
            .get(natural_id)
            .ok_or_else(|| "natural-exit session disappeared".to_string())?
            .pty
            .child();
        let exited = child
            .has_exited()
            .map_err(|error| format!("natural-exit wait census: {error}"))?;
        if exited {
            break;
        }
        if std::time::Instant::now() >= natural_deadline {
            return Err("LEAK[child]: natural-exit fixture did not exit".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    drop(natural_registered);
    wait_for_reaper_drain(std::time::Duration::from_secs(2))
        .map_err(|error| format!("natural-exit reaper: {error}"))?;
    wait_for_process_exit(
        natural_pid,
        Some(natural_pgid),
        natural_identity,
        std::time::Duration::from_secs(2),
    )
    .map_err(|error| format!("natural exit: {error}"))?;

    let timeout_id = "resource-leak-timeout";
    let mut timeout_session =
        LeakSessionGuard::new(make_isolated_session(timeout_id, "sleep 30 & wait", false)?);
    let timeout_pid = timeout_session
        .session()
        .pty
        .pid()
        .ok_or_else(|| "timeout child has no pid".to_string())?
        as libc::pid_t;
    let timeout_pgid = unsafe { libc::getpgid(timeout_pid) };
    if timeout_pgid <= 0 {
        return Err("timeout process group is unavailable".to_string());
    }
    let timeout_identity = stable_process_identity(timeout_pid)?;
    state
        .sessions
        .lock()
        .map_err(|error| format!("session lock: {error}"))?
        .insert(timeout_id.to_string(), timeout_session.take());
    let timeout_registered = RegisteredLeakSession {
        state,
        id: timeout_id,
    };
    let timeout = wait_for_process_exit(
        timeout_pid,
        Some(timeout_pgid),
        timeout_identity,
        std::time::Duration::ZERO,
    )
    .expect_err("live timeout fixture must reach deadline recovery");
    if !timeout.starts_with("LEAK[child]") {
        return Err(format!("timeout path was misclassified: {timeout}"));
    }
    drop(timeout_registered);
    wait_for_reaper_drain(std::time::Duration::from_secs(2))
        .map_err(|error| format!("timeout reaper: {error}"))?;
    wait_for_process_exit(
        timeout_pid,
        Some(timeout_pgid),
        timeout_identity,
        std::time::Duration::from_secs(2),
    )
    .map_err(|error| format!("timeout recovery: {error}"))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn wait_for_reaper_drain(timeout: std::time::Duration) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    while PTY_REAPERS_IN_FLIGHT.load(std::sync::atomic::Ordering::Acquire) != 0 {
        if std::time::Instant::now() >= deadline {
            return Err("LEAK[child]: PTY background reaper did not drain".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    // The counter reaches zero at the end of the blocking closure. Give the
    // executor one scheduling turn to release its task wrapper before DHAT.
    std::thread::sleep(std::time::Duration::from_millis(1));
    Ok(())
}

#[cfg(target_os = "macos")]
fn registry_cardinalities(state: &crate::state::PtyState) -> Result<(usize, usize), String> {
    let sessions = state
        .sessions
        .lock()
        .map_err(|error| format!("session lock: {error}"))?
        .len();
    let streams = state
        .stream_index
        .lock()
        .map_err(|error| format!("stream index lock: {error}"))?
        .len();
    Ok((sessions, streams))
}
