// SPDX-License-Identifier: MIT
//! The backend's notion of "a local guest is using the app".
//!
//! Someone who starts without an account (the no-login first run) works as the
//! local guest ([`Principal::local_guest`]). The frontend owns that choice (it is
//! persisted with the welcome answer) and declares it here, so backend commands
//! that derive the ambient user (`route_to_context::resolve_ambient`) can key the
//! guest's local workspace instead of refusing with "sign in".
//!
//! The declaration is also kept as a marker file in the guest's user home, read
//! once at startup. So after a restart or a webview reload the backend knows a
//! guest is in before the frontend re-declares it: boot-time sweeps and the first
//! background commands of a reloaded guest never see "no user".
//!
//! The guest id is never trusted blindly: it is used only while a guest session
//! is declared AND no code-host account is signed in. A signed-in account always
//! wins, and a signed-out user who never chose the guest start still gets the
//! sign-in message.
//!
//! [`Principal::local_guest`]: crate::domain::Principal::local_guest

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager};

use crate::context_repo::path_to_user_home;
use crate::domain::Principal;

/// The marker file, in the guest's user home, that records a declared guest session.
const GUEST_SESSION_MARKER: &str = ".guest-session";

fn marker_path(codesense_home: &Path) -> PathBuf {
    path_to_user_home(codesense_home, &Principal::local_guest()).join(GUEST_SESSION_MARKER)
}

/// Whether a local guest session is declared.
#[derive(Debug, Default)]
pub struct GuestSessionState(AtomicBool);

impl GuestSessionState {
    /// The state as the last run left it: declared when the marker file exists.
    pub fn load(codesense_home: Option<&Path>) -> Self {
        let active = codesense_home.is_some_and(|home| marker_path(home).is_file());
        Self(AtomicBool::new(active))
    }

    pub fn is_active(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn declare(&self, home: &Path, active: bool) -> Result<(), String> {
        persist(home, active)?;
        self.set(active);
        Ok(())
    }

    fn set(&self, active: bool) {
        self.0.store(active, Ordering::SeqCst);
    }
}

/// Write (or remove) the marker so the declaration survives a restart.
fn persist(codesense_home: &Path, active: bool) -> Result<(), String> {
    let path = marker_path(codesense_home);
    if active {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        std::fs::write(&path, b"guest\n")
            .map_err(|e| format!("could not write {}: {e}", path.display()))
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("could not remove {}: {e}", path.display())),
        }
    }
}

/// Whether a local guest session is declared (`false` when the state is not registered).
pub(crate) fn guest_session_active(app: &AppHandle) -> bool {
    app.try_state::<GuestSessionState>()
        .is_some_and(|state| state.is_active())
}

/// Declare (or end) the local guest session. The frontend calls this with its
/// persisted guest flag on load, when the welcome is answered without an account,
/// and with `false` when a code-host sign-in replaces the guest. The in-memory
/// flag applies at once; the marker only carries it across a restart.
#[tauri::command]
pub async fn set_guest_session(app: AppHandle, active: bool) -> Result<(), String> {
    let state = app
        .try_state::<GuestSessionState>()
        .ok_or_else(|| "GuestSessionState not registered".to_string())?;
    let home = crate::commands::paths::codesense_home_dir()
        .ok_or_else(|| "codesense home directory unavailable".to_string())?;
    crate::commands::await_blocking("set_guest_session", move || persist(&home, active)).await?;
    state.set(active);
    Ok(())
}

/// Whether a local guest session is declared, as the backend knows it: from the
/// marker read at startup, then kept current by [`set_guest_session`]. The frontend
/// reads it when its own (webview) store is empty, so a guest whose web storage was
/// reset still lands in the guest workspace instead of on the welcome screen.
#[tauri::command]
pub async fn get_guest_session(app: AppHandle) -> Result<bool, String> {
    Ok(guest_session_active(&app))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declaration_survives_a_restart_and_ending_it_clears_it() {
        let home = tempfile::tempdir().unwrap();
        assert!(!GuestSessionState::load(Some(home.path())).is_active());

        persist(home.path(), true).unwrap();
        assert!(
            GuestSessionState::load(Some(home.path())).is_active(),
            "a restart after the guest was declared still knows the guest is in"
        );
        assert!(
            marker_path(home.path()).starts_with(home.path().join("users").join("999999999999"))
        );

        persist(home.path(), false).unwrap();
        assert!(!GuestSessionState::load(Some(home.path())).is_active());
        // Ending a session that was never declared is not an error.
        persist(home.path(), false).unwrap();
    }

    #[test]
    fn no_home_means_no_guest() {
        assert!(!GuestSessionState::load(None).is_active());
    }

    #[test]
    fn the_flag_toggles() {
        let state = GuestSessionState::default();
        assert!(!state.is_active());
        state.set(true);
        assert!(state.is_active());
        state.set(false);
        assert!(!state.is_active());
    }
}
