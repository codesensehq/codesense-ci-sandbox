// SPDX-License-Identifier: MIT
//! Opt-in lifetime tie: a sidecar launched with [`EXIT_WITH_PID_ENV`] set to a
//! pid exits once that process is gone.
//!
//! The app never sets it — its sidecar must outlive the app. Test runs set it
//! to their own pid, so a sidecar launched by a run that dies before its guards
//! drop (a panic in setup, a timeout, a SIGKILL) still goes away instead of
//! running on under launchd, holding its hosted children and the binary of a
//! worktree that may be deleted.

use std::time::Duration;

/// Names the pid whose death ends this sidecar.
pub const EXIT_WITH_PID_ENV: &str = "CODESENSE_SESSION_SERVER_EXIT_WITH_PID";

/// How often the tied process is checked.
const POLL: Duration = Duration::from_millis(200);

/// Take the tied pid out of the environment, so hosted sessions never inherit
/// it. Call while the process is still single-threaded (before any thread
/// starts); `None` when unset or unparseable.
pub fn take_from_env() -> Option<libc::pid_t> {
    let value = std::env::var(EXIT_WITH_PID_ENV).ok();
    std::env::remove_var(EXIT_WITH_PID_ENV);
    parse(value.as_deref()?)
}

fn parse(value: &str) -> Option<libc::pid_t> {
    value
        .trim()
        .parse()
        .ok()
        .filter(|pid: &libc::pid_t| *pid > 1)
}

/// Whether `pid` still names a process (a live one or one not yet reaped).
/// Pid-based: a pid reused within one poll keeps the sidecar alive until that
/// process exits too.
fn exists(pid: libc::pid_t) -> bool {
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    alive || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Watch `pid` on a background thread and exit this process once it is gone.
/// Exiting closes every PTY master, which hangs up the hosted sessions.
pub fn exit_when_gone(pid: libc::pid_t) {
    let spawned = std::thread::Builder::new()
        .name("pty-server-exit-with".into())
        .spawn(move || {
            while exists(pid) {
                std::thread::sleep(POLL);
            }
            log::info!("tied process {pid} is gone; exiting");
            std::process::exit(0);
        });
    if let Err(err) = spawned {
        // The tie was asked for and cannot be honoured: exit rather than run
        // untied (the leak the tie exists to prevent).
        log::warn!("could not watch tied process {pid}: {err}; exiting");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_real_non_init_pid_ties_the_sidecar() {
        assert_eq!(parse("4242"), Some(4242));
        assert_eq!(parse(" 4242\n"), Some(4242));
        assert_eq!(parse("1"), None, "never tie to launchd/init");
        assert_eq!(parse("0"), None);
        assert_eq!(parse("-5"), None);
        assert_eq!(parse("app"), None);
    }

    #[test]
    fn a_live_process_exists_and_a_reaped_one_does_not() {
        assert!(exists(std::process::id() as libc::pid_t));
        let mut child = std::process::Command::new("/usr/bin/true")
            .spawn()
            .expect("spawn true");
        let pid = child.id() as libc::pid_t;
        child.wait().expect("reap true");
        assert!(!exists(pid), "a reaped pid is gone");
    }
}
