// SPDX-License-Identifier: MIT
//! Launch guard for WebDriver (`e2e-webdriver`) builds.
//!
//! A WebDriver build is only ever meant to run inside the real-app test
//! harness, which launches it with a sandbox HOME, `CODESENSE_E2E=1`, and a
//! per-run `TAURI_WEBDRIVER_PORT`. If the same binary is started any other way
//! — most dangerously when macOS relaunches it after a crash, through a fresh
//! login shell with the user's real HOME and none of the harness environment —
//! it would open the user's live profile and claim the default WebDriver port.
//! The guard makes such a launch exit before anything else runs.

use std::fmt;

/// Why a launch is not a harness launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MissingHarnessEnv {
    /// `CODESENSE_E2E` is absent or not `1`.
    E2eMarker,
    /// `TAURI_WEBDRIVER_PORT` is absent or not a usable TCP port.
    WebDriverPort,
}

impl fmt::Display for MissingHarnessEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::E2eMarker => f.write_str("CODESENSE_E2E=1 is not set"),
            Self::WebDriverPort => f.write_str("TAURI_WEBDRIVER_PORT is not set to a valid port"),
        }
    }
}

/// Checks the launch environment (read through `var`) for the harness markers.
pub(crate) fn check_harness_launch(
    var: impl Fn(&str) -> Option<String>,
) -> Result<(), MissingHarnessEnv> {
    if var("CODESENSE_E2E").as_deref() != Some("1") {
        return Err(MissingHarnessEnv::E2eMarker);
    }
    match var("TAURI_WEBDRIVER_PORT").and_then(|raw| raw.trim().parse::<u16>().ok()) {
        Some(port) if port != 0 => Ok(()),
        _ => Err(MissingHarnessEnv::WebDriverPort),
    }
}

/// Exits the process (status 0, one line on stderr) unless it was launched by
/// the harness. Called first thing in `run()` for WebDriver builds.
#[cfg(feature = "e2e-webdriver")]
pub(crate) fn exit_unless_harness_launch() {
    if let Err(missing) = check_harness_launch(|key| std::env::var(key).ok()) {
        crate::console_log::write_stderr_line(&format!(
            "CodeSense WebDriver build refused to start: {missing}; launch it through the test harness"
        ));
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn harness_launch_with_marker_and_port_is_allowed() {
        assert_eq!(
            check_harness_launch(env(&[
                ("CODESENSE_E2E", "1"),
                ("TAURI_WEBDRIVER_PORT", "53127")
            ])),
            Ok(())
        );
    }

    #[test]
    fn crash_relaunch_with_login_shell_env_is_refused() {
        // A relaunch through Terminal carries the user's own environment only.
        assert_eq!(
            check_harness_launch(env(&[("HOME", "/Users/someone"), ("PATH", "/usr/bin")])),
            Err(MissingHarnessEnv::E2eMarker)
        );
    }

    #[test]
    fn marker_without_port_is_refused_rather_than_using_the_default_port() {
        for port in [None, Some(""), Some("0"), Some("not-a-port"), Some("70000")] {
            let mut pairs = vec![("CODESENSE_E2E", "1")];
            if let Some(port) = port {
                pairs.push(("TAURI_WEBDRIVER_PORT", port));
            }
            assert_eq!(
                check_harness_launch(env(&pairs)),
                Err(MissingHarnessEnv::WebDriverPort),
                "port {port:?}"
            );
        }
    }

    #[test]
    fn marker_must_be_exactly_one() {
        assert_eq!(
            check_harness_launch(env(&[
                ("CODESENSE_E2E", "0"),
                ("TAURI_WEBDRIVER_PORT", "53127")
            ])),
            Err(MissingHarnessEnv::E2eMarker)
        );
    }
}
