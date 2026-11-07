// SPDX-License-Identifier: MIT
//! Which shell a terminal runs, resolved by the host it runs on.
//!
//! Terminals run the user's own login shell rather than always zsh. The shell is
//! resolved in order: the Settings → Terminal override (carried in the spawn
//! intent), the host's `$SHELL`, the passwd entry for the user the host runs
//! as, then `/bin/zsh`. A candidate is used only when it is an absolute path to
//! an executable file whose basename is a shell CodeSense has integration for
//! (zsh, bash, fish); anything else falls through to the next source, so a
//! stale or unsupported `$SHELL` never leaves the user without a terminal.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// The shell the fallback resolves to when no other source yields a usable shell.
pub const FALLBACK_SHELL: &str = "/bin/zsh";

/// Shells CodeSense has terminal integration for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellKind {
    Zsh,
    Bash,
    Fish,
}

impl ShellKind {
    /// Extension of this shell's bundled integration script
    /// (`resources/shell-integration/codesense-shell-init.<ext>`).
    pub fn init_script_ext(self) -> &'static str {
        match self {
            ShellKind::Zsh => "zsh",
            ShellKind::Bash => "bash",
            ShellKind::Fish => "fish",
        }
    }

    /// Whether `<shell> -l -c <command>` runs CodeSense's POSIX-style command
    /// strings unchanged. fish has its own syntax, so non-interactive commands
    /// for fish users run under the fallback zsh instead.
    pub fn runs_posix_commands(self) -> bool {
        matches!(self, ShellKind::Zsh | ShellKind::Bash)
    }
}

/// Where the resolved shell came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ShellSource {
    Override,
    Env,
    Passwd,
    Fallback,
}

/// The shell a terminal spawns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserShell {
    pub path: PathBuf,
    pub kind: ShellKind,
    pub source: ShellSource,
}

impl UserShell {
    /// The fallback zsh (`/bin/zsh`, or `zsh` on `PATH` when that is missing).
    pub fn fallback() -> Self {
        let path = if is_executable_file(Path::new(FALLBACK_SHELL)) {
            PathBuf::from(FALLBACK_SHELL)
        } else {
            PathBuf::from("zsh")
        };
        UserShell {
            path,
            kind: ShellKind::Zsh,
            source: ShellSource::Fallback,
        }
    }
}

/// The shell kind named by `path`'s basename, if CodeSense supports it.
pub fn shell_kind(path: &Path) -> Option<ShellKind> {
    match path.file_name()?.to_str()? {
        "zsh" => Some(ShellKind::Zsh),
        "bash" => Some(ShellKind::Bash),
        "fish" => Some(ShellKind::Fish),
        _ => None,
    }
}

/// An absolute path to a regular file with an execute bit set.
pub fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_absolute()
        && std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

/// Pure resolution over injected sources, so the precedence is testable without
/// mutating the process environment. `passwd` is only consulted when the
/// override and `$SHELL` both fail.
pub fn resolve_shell_from(
    override_shell: Option<&str>,
    env_shell: Option<&str>,
    passwd: impl FnOnce() -> Option<String>,
    usable: impl Fn(&Path) -> bool,
) -> UserShell {
    let candidate = |raw: Option<&str>| -> Option<(PathBuf, ShellKind)> {
        let raw = raw?.trim();
        if raw.is_empty() {
            return None;
        }
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return None;
        }
        let kind = shell_kind(&path)?;
        usable(&path).then_some((path, kind))
    };
    if let Some((path, kind)) = candidate(override_shell) {
        return UserShell {
            path,
            kind,
            source: ShellSource::Override,
        };
    }
    if let Some((path, kind)) = candidate(env_shell) {
        return UserShell {
            path,
            kind,
            source: ShellSource::Env,
        };
    }
    if let Some((path, kind)) = candidate(passwd().as_deref()) {
        return UserShell {
            path,
            kind,
            source: ShellSource::Passwd,
        };
    }
    UserShell::fallback()
}

/// The login shell recorded for the current user in the passwd database
/// (the `dscl` UserShell on macOS).
pub fn passwd_login_shell() -> Option<String> {
    // SAFETY: `getpwuid_r` writes into our own buffers; `result` is either null
    // or points at `pwd`, whose string fields point into `buf`, which outlives
    // every read below.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut buf = vec![0 as libc::c_char; 4096];
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        loop {
            let rc = libc::getpwuid_r(
                libc::getuid(),
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            );
            // An entry larger than the buffer: grow and retry, within reason.
            if rc == libc::ERANGE && buf.len() < 1 << 20 {
                buf.resize(buf.len() * 2, 0);
                continue;
            }
            if rc != 0 || result.is_null() || pwd.pw_shell.is_null() {
                return None;
            }
            break;
        }
        let shell = std::ffi::CStr::from_ptr(pwd.pw_shell)
            .to_string_lossy()
            .into_owned();
        (!shell.is_empty()).then_some(shell)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usable_set(paths: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |p: &Path| paths.iter().any(|x| Path::new(x) == p)
    }

    #[test]
    fn env_shell_wins_over_passwd() {
        let got = resolve_shell_from(
            None,
            Some("/opt/homebrew/bin/fish"),
            || Some("/bin/bash".into()),
            usable_set(&["/opt/homebrew/bin/fish", "/bin/bash"]),
        );
        assert_eq!(got.path, PathBuf::from("/opt/homebrew/bin/fish"));
        assert_eq!(got.kind, ShellKind::Fish);
        assert_eq!(got.source, ShellSource::Env);
    }

    #[test]
    fn override_wins_over_env() {
        let got = resolve_shell_from(
            Some("/bin/bash"),
            Some("/bin/zsh"),
            || None,
            usable_set(&["/bin/bash", "/bin/zsh"]),
        );
        assert_eq!(got.path, PathBuf::from("/bin/bash"));
        assert_eq!(got.source, ShellSource::Override);
    }

    #[test]
    fn missing_env_falls_back_to_passwd() {
        let got = resolve_shell_from(
            None,
            None,
            || Some("/bin/bash".into()),
            usable_set(&["/bin/bash"]),
        );
        assert_eq!(got.path, PathBuf::from("/bin/bash"));
        assert_eq!(got.source, ShellSource::Passwd);
    }

    #[test]
    fn stale_env_shell_falls_through() {
        // $SHELL names a Homebrew fish that was uninstalled.
        let got = resolve_shell_from(
            None,
            Some("/opt/homebrew/bin/fish"),
            || Some("/bin/bash".into()),
            usable_set(&["/bin/bash"]),
        );
        assert_eq!(got.path, PathBuf::from("/bin/bash"));
        assert_eq!(got.source, ShellSource::Passwd);
    }

    #[test]
    fn unsupported_or_relative_shells_fall_back_to_zsh() {
        let got = resolve_shell_from(
            Some("bash"),
            Some("/bin/tcsh"),
            || Some("/bin/tcsh".into()),
            |_: &Path| true,
        );
        assert_eq!(got.kind, ShellKind::Zsh);
        assert_eq!(got.source, ShellSource::Fallback);
    }

    #[test]
    fn passwd_is_not_consulted_when_env_is_usable() {
        let got = resolve_shell_from(
            None,
            Some("/bin/zsh"),
            || panic!("passwd consulted"),
            usable_set(&["/bin/zsh"]),
        );
        assert_eq!(got.source, ShellSource::Env);
    }

    #[test]
    fn passwd_lookup_returns_this_users_shell() {
        // Every macOS/Linux account has a login shell recorded.
        let shell = passwd_login_shell().expect("passwd shell");
        assert!(shell.starts_with('/'), "{shell}");
    }

    #[test]
    fn executable_check_rejects_dirs_and_relative_paths() {
        assert!(is_executable_file(Path::new("/bin/sh")));
        assert!(!is_executable_file(Path::new("/bin")));
        assert!(!is_executable_file(Path::new("sh")));
        assert!(!is_executable_file(Path::new("/definitely/not/here/zsh")));
    }
}
