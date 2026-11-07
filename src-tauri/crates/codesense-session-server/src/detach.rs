// SPDX-License-Identifier: MIT
//! Sidecar self-detachment: the classic double fork.
//!
//! For the sidecar to *outlive* the app, it must not sit in the app's process
//! group or session — app teardown (or a shell that started the app) could
//! signal it — and it must not stay the app's child. The launched process
//! forks and exits, so the child is reparented to launchd/init (ppid 1) at
//! once; the child calls `setsid(2)` to leave the app's session (it is never a
//! process-group leader, so this cannot fail with `EPERM`), then forks again
//! and exits, so the server is no session leader and can never acquire a
//! controlling terminal. The process-census tests verify the result.

/// Detach into the server process: every process but the final one exits
/// here. Call before starting any thread.
pub fn detach() -> Result<(), String> {
    fork_and_exit_parent()?;
    if unsafe { libc::setsid() } == -1 {
        return Err(format!(
            "setsid failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    fork_and_exit_parent()
}

/// Fork; the parent exits at once, the child returns.
fn fork_and_exit_parent() -> Result<(), String> {
    match unsafe { libc::fork() } {
        -1 => Err(format!(
            "fork for detach failed: {}",
            std::io::Error::last_os_error()
        )),
        0 => Ok(()),
        _ => unsafe { libc::_exit(0) },
    }
}
