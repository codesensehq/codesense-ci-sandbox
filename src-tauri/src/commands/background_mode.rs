// SPDX-License-Identifier: MIT
//! SW-AU-34: background-on-close policy, so running agents keep working.
//!
//! Closing the window keeps the process — and thus the MCP host, the sync engine,
//! and `ActiveTeamState` — alive instead of quitting, the standard macOS
//! dock-resident behavior. "Open" comes to mean *process-alive*, not
//! *window-visible*, which is the contract a leave-it-running agent
//! needs. The window-close handler in `lib.rs` reads a single process-global flag
//! (the user's quit-on-close preference) to decide hide-vs-quit.
//!
//! The flag is mirrored from the frontend `settings.quitOnClose` via
//! [`set_quit_on_close`] — one value, one atomic, no `AppHandle`/`State`. That is what keeps the read on
//! the window-event (main/UI) thread a lock-free `Relaxed` load with no disk I/O
//! and no IPC round-trip, so window close never blocks a frame (the "no beachball"
//! contract). Cmd-Q / menu-Quit fire `RunEvent::ExitRequested`, NOT
//! `WindowEvent::CloseRequested`, so hiding on close can never swallow a real quit
//! and no "quitting" guard flag is needed.

use std::sync::atomic::{AtomicBool, Ordering};

/// Event emitted to the frontend the moment a close is handled as background — the
/// FE renders a one-time explainer toast (gated on a persisted "seen" flag) so the
/// user learns the app is still running. An emitted event, never a native dialog,
/// so the close handler stays non-blocking.
pub const BACKGROUND_MODE_ENTERED_EVENT: &str = "background-mode-entered";

/// The FE→Rust mirror of `settings.quitOnClose`. `false` (default) =
/// background-on-close (hide, keep the host alive); `true` = quit-on-close (a
/// window close exits the process). Defaults to `false` so a fresh profile behaves
/// as a normal dock-resident app until the FE pushes the setting — and, crucially,
/// so the close handler never has to touch disk to learn the preference. `AtomicBool`
/// has a const constructor, so this is a plain `static` with no lazy init (mirrors
/// the `reverse_cadence()` signals the same window handler already writes).
fn quit_on_close_cell() -> &'static AtomicBool {
    static FLAG: AtomicBool = AtomicBool::new(false);
    &FLAG
}

/// Whether the user opted into quit-on-close. Relaxed load — a plain on/off gate
/// with nothing else ordered against it. Called on the window-event (main) thread
/// from the `CloseRequested` handler, so it must stay a bare atomic read.
pub fn quit_on_close() -> bool {
    quit_on_close_cell().load(Ordering::Relaxed)
}

/// FE→Rust push of the `quitOnClose` setting — one value, one atomic, no
/// `AppHandle`/`State`. Logs at `info` only on an actual change (via
/// `swap`) so a re-push on every settings save is not per-call spam.
#[tauri::command]
pub fn set_quit_on_close(quit_on_close: bool) {
    let prev = quit_on_close_cell().swap(quit_on_close, Ordering::Relaxed);
    if prev != quit_on_close {
        log::info!(
            target: "commands::background_mode",
            "quit-on-close set to {quit_on_close} by setting"
        );
    }
}

/// What the window-close handler should do. Factored out as a pure function so the
/// decision is unit-testable without a live Tauri window, and so the closure in
/// `lib.rs` stays a thin read-atomic-then-dispatch (the load-bearing shape for the
/// no-UI-thread-block contract).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum CloseAction {
    /// Hide the app; keep the process (MCP host + engine + state) alive.
    Background,
    /// Quit the process — the user opted into quit-on-close.
    Quit,
}

/// Resolve the close action from the current preference. `quit_on_close = true`
/// ⇒ [`CloseAction::Quit`]; otherwise [`CloseAction::Background`] (the default).
pub fn close_action(quit_on_close: bool) -> CloseAction {
    if quit_on_close {
        CloseAction::Quit
    } else {
        CloseAction::Background
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_background_on_close() {
        // A fresh flag reads false → Background, so a fresh profile is dock-resident
        // (never a surprise quit) until the FE pushes an explicit quit-on-close.
        assert_eq!(close_action(false), CloseAction::Background);
    }

    #[test]
    fn quit_on_close_pref_maps_to_quit() {
        assert_eq!(close_action(true), CloseAction::Quit);
    }

    /// AC-7 guard: the window-close handler must never block the macOS UI thread —
    /// it is a lock-free atomic read plus native calls only. This scans the
    /// `CloseRequested` arm in `lib.rs` and fails if it ever grows a disk read, an
    /// IPC/command invoke, or a blocking dialog — the concrete ways a close could
    /// stall the event loop (a beachball). The `close_action` decision stays pure so
    /// the arm has no reason to reach for any of these.
    #[test]
    fn close_handler_arm_stays_non_blocking() {
        let lib_rs = include_str!("../lib.rs");
        let start = lib_rs
            .find("WindowEvent::CloseRequested")
            .expect("close-to-background arm present in lib.rs");
        // Bound the scan to the arm body: from the marker up to the `.plugin(` that
        // follows the closed `on_window_event` closure.
        let rest = &lib_rs[start..];
        let end = rest.find(".plugin(").unwrap_or(rest.len().min(1600));
        let arm = &rest[..end];
        for forbidden in [
            "std::fs",
            ".invoke",
            "tauri_plugin_dialog",
            "MessageDialog",
            "block_on",
        ] {
            assert!(
                !arm.contains(forbidden),
                "close handler must not block the UI thread; found `{forbidden}` in the CloseRequested arm"
            );
        }
    }

    #[test]
    fn set_quit_on_close_round_trips_through_the_atomic() {
        // Process-global, so set explicitly both ways to stay order-independent.
        set_quit_on_close(true);
        assert!(quit_on_close());
        assert_eq!(close_action(quit_on_close()), CloseAction::Quit);

        set_quit_on_close(false);
        assert!(!quit_on_close());
        assert_eq!(close_action(quit_on_close()), CloseAction::Background);
    }
}
