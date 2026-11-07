// SPDX-License-Identifier: MIT
//! Finding the git-ai binary and reading its version. The sibling installer supplies a
//! pinned, verified copy when needed; detection itself never mutates the installation.

use super::version::Version;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BINARY: &str = "git-ai";

/// One bound for detection and the bundled version probe. A first-run OS scan
/// can exceed it; the installer retries probe failures. Tests allow extra startup time.
pub(crate) const VERSION_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 30 } else { 3 });

/// What detection found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Detection {
    NotInstalled,
    /// A binary that did not answer `--version` (it failed, hung or is not executable).
    Broken {
        path: PathBuf,
        message: String,
    },
    Found {
        path: PathBuf,
        /// What `--version` printed, trimmed.
        raw: String,
        version: Option<Version>,
    },
}

/// Capture resolves only CodeSense's own regular executable. A user symlink is kept
/// intact and is never followed to run a user's binary.
pub(crate) fn find_binary(
    codesense_home: &Path,
    _path_var: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let managed = codesense_home.join("bin").join(BINARY);
    (!std::fs::symlink_metadata(&managed).is_ok_and(|m| m.file_type().is_symlink())
        && is_executable(&managed))
    .then_some(managed)
}

/// A PATH copy is reported by the doctor without executing it.
pub(crate) fn user_copy_present(codesense_home: &Path, path_var: Option<&std::ffi::OsStr>) -> bool {
    path_var.is_some_and(|paths| {
        std::env::split_paths(paths)
            .map(|dir| dir.join(BINARY))
            .any(|path| path != codesense_home.join("bin/git-ai") && is_executable(&path))
    })
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Locate git-ai and read its version, running it under `env` (the managed home, so it
/// never reads the user's own `~/.git-ai`).
pub(crate) fn detect(
    codesense_home: &Path,
    path_var: Option<&std::ffi::OsStr>,
    env: &[(String, String)],
) -> Detection {
    detect_within(codesense_home, path_var, env, VERSION_TIMEOUT)
}

fn detect_within(
    codesense_home: &Path,
    path_var: Option<&std::ffi::OsStr>,
    env: &[(String, String)],
    timeout: Duration,
) -> Detection {
    let Some(path) = find_binary(codesense_home, path_var) else {
        return Detection::NotInstalled;
    };
    detect_binary(path, env, timeout)
}

/// git-ai dispatches by argv[0]; staged resources and renamed assets must still
/// enter its git-ai CLI rather than its git proxy.
pub(crate) fn git_ai_command(path: &Path) -> Command {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(path);
    command.arg0(BINARY);
    command
}

/// Read one binary's version under the managed environment, including a verified temp asset.
pub(crate) fn detect_binary(
    path: PathBuf,
    env: &[(String, String)],
    timeout: Duration,
) -> Detection {
    let mut command = git_ai_command(&path);
    command
        .arg("--version")
        .envs(env.iter().map(|(k, v)| (k, v)));
    match run_bounded(command, timeout) {
        Ok((status, output)) if status.success() => {
            let raw = output.trim().to_owned();
            Detection::Found {
                version: Version::parse(&raw),
                path,
                raw,
            }
        }
        Ok((status, output)) => Detection::Broken {
            path,
            message: format!("--version exited {status}: {}", output.trim()),
        },
        Err(message) => Detection::Broken { path, message },
    }
}

/// Run `command`, bounded by `timeout`, with its stdout and stderr in one file rather than
/// a pipe: a process it leaves running (git-ai's daemon) must not hold the read open.
pub(crate) fn run_bounded(
    mut command: Command,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, String), String> {
    let mut output = tempfile::tempfile().map_err(|e| e.to_string())?;
    let errors = output.try_clone().map_err(|e| e.to_string())?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(output.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(errors))
        .spawn()
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
    };
    let mut text = std::string::String::new();
    output.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    output
        .take(64 * 1024)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    Ok((status, text))
}

#[cfg(test)]
mod tests {
    use super::super::testing::script;
    use super::*;

    #[test]
    fn a_renamed_binary_is_invoked_as_git_ai() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("git-ai-macos-arm64");
        super::super::testing::argv0_binary(&binary);
        let Detection::Found { raw, version, .. } = detect_binary(binary, &[], VERSION_TIMEOUT)
        else {
            panic!("version probe failed")
        };
        assert_eq!(raw, "1.7.5");
        assert_eq!(version, Some(Version(1, 7, 5)));
    }

    #[test]
    fn the_managed_binary_wins_over_one_on_path() {
        let home = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        script(&home.path().join("bin/git-ai"), "echo 1.7.5");
        script(&elsewhere.path().join("git-ai"), "echo 1.7.6");
        let path = std::env::join_paths([elsewhere.path()]).unwrap();
        let found = find_binary(home.path(), Some(&path)).unwrap();
        assert_eq!(found, home.path().join("bin/git-ai"));
        std::fs::remove_file(&found).unwrap();
        assert_eq!(find_binary(home.path(), Some(&path)), None);
        assert!(user_copy_present(home.path(), Some(&path)));
        assert_eq!(find_binary(home.path(), None), None);
    }

    #[test]
    fn a_non_executable_file_is_not_a_binary() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("bin")).unwrap();
        std::fs::write(home.path().join("bin/git-ai"), "").unwrap();
        assert_eq!(detect(home.path(), None, &[]), Detection::NotInstalled);
    }

    #[test]
    fn each_real_version_output_is_read_and_a_broken_binary_is_reported() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join("bin/git-ai");
        for (body, expected) in [
            ("echo 1.7.5", Some(Version(1, 7, 5))),
            ("echo '1.7.5 (debug)'", Some(Version(1, 7, 5))),
            ("echo 1.8.0", Some(Version(1, 8, 0))),
            ("echo garbage", None),
        ] {
            script(&bin, body);
            let Detection::Found { version, .. } = detect(home.path(), None, &[]) else {
                panic!("{body}");
            };
            assert_eq!(version, expected, "{body}");
        }
        script(&bin, "echo boom >&2; exit 3");
        assert!(matches!(
            detect(home.path(), None, &[]),
            Detection::Broken { message, .. } if message.contains("boom")
        ));
        script(&bin, "exec sleep 30");
        let started = Instant::now();
        let Detection::Broken { message, .. } =
            detect_within(home.path(), None, &[], Duration::from_millis(500))
        else {
            panic!("a hung binary is broken");
        };
        assert!(message.contains("timed out"), "{message}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn the_binary_runs_under_the_given_environment() {
        let home = tempfile::tempdir().unwrap();
        script(&home.path().join("bin/git-ai"), "echo \"1.7.5 $HOME\"");
        let env = [("HOME".to_owned(), "/managed".to_owned())];
        let Detection::Found { raw, .. } = detect(home.path(), None, &env) else {
            panic!()
        };
        assert_eq!(raw, "1.7.5 /managed");
    }
}
