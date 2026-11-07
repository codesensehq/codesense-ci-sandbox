// SPDX-License-Identifier: MIT
//! SW-LN-35: the Settings → Terminal shell override, and what Settings shows
//! about the shell. The shell itself is resolved by the host a terminal runs on
//! ([`codesense_pty_core::shell`]); the override travels to it in the spawn
//! intent.

use std::path::Path;
use std::sync::RwLock;

use codesense_pty_core::shell::{
    is_executable_file, passwd_login_shell, resolve_shell_from, shell_kind, ShellKind, ShellSource,
};
use serde::Serialize;
use tauri::Manager;

use crate::terminal::warm_pool::WarmPoolState;

/// The Settings → Terminal override, set by the frontend. `None` = detect.
static SHELL_OVERRIDE: RwLock<Option<String>> = RwLock::new(None);

/// The Settings → Terminal override, for a spawn's intent.
pub(crate) fn current_override() -> Option<String> {
    SHELL_OVERRIDE
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// What Settings → Terminal shows about the shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalShellInfo {
    /// The shell new terminals spawn.
    pub path: String,
    pub kind: ShellKind,
    pub source: ShellSource,
    /// The shell detected without the override (`$SHELL` → passwd → zsh).
    pub detected_path: String,
    /// The override currently set, verbatim.
    pub override_path: Option<String>,
    /// The override is set but is not an executable zsh/bash/fish, so it is ignored.
    pub override_rejected: bool,
    /// A login shell (`$SHELL` or passwd) CodeSense can't integrate with, which
    /// is why the fallback is in use.
    pub unsupported_shell: Option<String>,
}

pub(crate) fn shell_info_from(
    override_shell: Option<&str>,
    env_shell: Option<&str>,
    passwd: impl Fn() -> Option<String>,
    usable: impl Fn(&Path) -> bool,
) -> TerminalShellInfo {
    let effective = resolve_shell_from(override_shell, env_shell, &passwd, &usable);
    let detected = resolve_shell_from(None, env_shell, &passwd, &usable);
    let override_path = override_shell
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let override_rejected = override_path.is_some() && effective.source != ShellSource::Override;
    // Only a shell CodeSense has no integration for is "unsupported"; a missing
    // or stale zsh/bash/fish path just falls through to detection.
    let unsupported_shell = if detected.source == ShellSource::Fallback {
        env_shell
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(&passwd)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && shell_kind(Path::new(s)).is_none())
    } else {
        None
    };
    TerminalShellInfo {
        path: effective.path.to_string_lossy().into_owned(),
        kind: effective.kind,
        source: effective.source,
        detected_path: detected.path.to_string_lossy().into_owned(),
        override_path,
        override_rejected,
        unsupported_shell,
    }
}

fn current_shell_info() -> TerminalShellInfo {
    shell_info_from(
        current_override().as_deref(),
        std::env::var("SHELL").ok().as_deref(),
        passwd_login_shell,
        is_executable_file,
    )
}

/// Settings → Terminal: the shell new terminals spawn, and where it came from.
#[tauri::command]
pub async fn terminal_shell_info() -> Result<TerminalShellInfo, String> {
    super::await_blocking("terminal_shell_info", || Ok(current_shell_info())).await
}

/// Settings → Terminal: set (or clear, with `None`/blank) the shell override.
/// Terminals opened afterwards use it, including the next one served from the
/// warm pool: pre-spawned shells of the old shell are never handed out, and the
/// frontend's follow-up `reconfigure` replaces them.
#[tauri::command]
pub async fn set_terminal_shell_override(
    app: tauri::AppHandle,
    shell: Option<String>,
) -> Result<TerminalShellInfo, String> {
    let shell = shell
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    apply_shell_override(app.try_state::<WarmPoolState>().as_deref(), shell);
    super::await_blocking("set_terminal_shell_override", || Ok(current_shell_info())).await
}

/// SW-LN-B31: store the override and hand it to the warm pool in one step, so no
/// terminal opened in between can get a member spawned with the old shell.
fn apply_shell_override(pool: Option<&WarmPoolState>, shell: Option<String>) {
    let mut guard = SHELL_OVERRIDE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(pool) = pool {
        pool.set_shell(shell.clone());
    }
    *guard = shell;
}

/// The shell a spawn runs. SW-LN-B31: a warm-pool member runs the shell its slot
/// was reserved with (`member_shell`), so the pool can tell a stale member from a
/// current one; everything else runs the Settings override.
pub(crate) fn spawn_shell(
    member_shell: Option<Option<String>>,
    current: impl FnOnce() -> Option<String>,
) -> Option<String> {
    member_shell.unwrap_or_else(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usable_set(paths: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |p: &Path| paths.iter().any(|x| Path::new(x) == p)
    }

    #[test]
    fn apply_shell_override_moves_the_pool_to_the_new_shell() {
        let pool = WarmPoolState::default();
        apply_shell_override(Some(&pool), Some("/bin/bash".into()));
        assert_eq!(current_override().as_deref(), Some("/bin/bash"));
        apply_shell_override(Some(&pool), None);
        assert_eq!(current_override(), None);
    }

    #[test]
    fn a_pool_member_spawns_with_its_reserved_shell() {
        assert_eq!(
            spawn_shell(Some(Some("/bin/bash".into())), || Some("/bin/zsh".into())),
            Some("/bin/bash".into())
        );
        assert_eq!(spawn_shell(Some(None), || Some("/bin/zsh".into())), None);
        // Not a pool member: the Settings override.
        assert_eq!(
            spawn_shell(None, || Some("/bin/zsh".into())),
            Some("/bin/zsh".into())
        );
    }

    #[test]
    fn info_reports_rejected_override_and_unsupported_shell() {
        let info = shell_info_from(
            Some("/nope/bash"),
            Some("/bin/tcsh"),
            || Some("/bin/tcsh".into()),
            usable_set(&["/bin/tcsh"]),
        );
        assert!(info.override_rejected);
        assert_eq!(info.override_path.as_deref(), Some("/nope/bash"));
        assert_eq!(info.source, ShellSource::Fallback);
        assert_eq!(info.unsupported_shell.as_deref(), Some("/bin/tcsh"));
    }

    #[test]
    fn info_does_not_call_a_missing_supported_shell_unsupported() {
        // $SHELL names a Homebrew fish that was uninstalled.
        let info = shell_info_from(
            None,
            Some("/opt/homebrew/bin/fish"),
            || None,
            |_: &Path| false,
        );
        assert_eq!(info.source, ShellSource::Fallback);
        assert!(info.unsupported_shell.is_none());
    }

    #[test]
    fn info_reports_an_unsupported_passwd_shell_when_env_is_blank() {
        let info = shell_info_from(
            None,
            Some("  "),
            || Some("/bin/tcsh".into()),
            |_: &Path| true,
        );
        assert_eq!(info.unsupported_shell.as_deref(), Some("/bin/tcsh"));
    }

    #[test]
    fn info_for_a_plain_bash_user() {
        let info = shell_info_from(None, Some("/bin/bash"), || None, usable_set(&["/bin/bash"]));
        assert_eq!(info.path, "/bin/bash");
        assert_eq!(info.detected_path, "/bin/bash");
        assert_eq!(info.kind, ShellKind::Bash);
        assert!(!info.override_rejected);
        assert!(info.unsupported_shell.is_none());
    }
}
