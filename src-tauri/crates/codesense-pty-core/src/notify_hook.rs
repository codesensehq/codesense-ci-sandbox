// SPDX-License-Identifier: MIT
//! The Claude status hook CodeSense itself ships.
//!
//! The script is part of CodeSense's own install, never of a workspace or team content:
//! every host writes it once into its activity root, beside the activity helper, when it
//! starts ([`install`]). A Claude session reaches it through its tab's
//! `$CODESENSE_ACTIVITY_HOST`, as its inbox hooks reach the helper ([`status_command`]).
//! The hook posts Claude Code lifecycle events to the running CodeSense app so terminal
//! status dots and the agent list stay live.
//!
//! Lives in `codesense-pty-core` because both the main `codesense` binary and the
//! `codesense-session-server` sidecar run a host.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The reference hook, embedded in the app binary. The repo copy at
/// `scripts/codesense-notify.reference.sh` is the single source of truth;
/// edits there flow in on the next build.
pub const SOURCE: &str = include_str!("../../../../scripts/codesense-notify.reference.sh");

/// The installed hook's file name in a host's activity root.
pub const NAME: &str = "codesense-notify.sh";

/// Mode the installed hook lands with (executable for owner, read + exec
/// for group and other).
pub const MODE: u32 = 0o755;

/// The secret-file guard's template, installed beside the status hook with the protected
/// families filled in ([`guard_source`]). The repo copy at
/// `scripts/codesense-secret-guard.reference.sh` is the single source of truth for its logic.
const GUARD_TEMPLATE: &str =
    include_str!("../../../../scripts/codesense-secret-guard.reference.sh");

/// The installed guard's file name in a host's activity root.
pub const GUARD_NAME: &str = "codesense-secret-guard.sh";

/// The tools the guard is scoped to, as a hook matcher.
pub const GUARD_MATCHER: &str = "Read|Grep|Glob|Bash";

/// One protected family of credential files, named by a basename glob. `except` are the globs
/// the hook lets through (templates, public keys); a native deny rule cannot express an
/// exception, so those are denied to Read, Grep and Glob and stay readable through Bash.
pub struct Family {
    pub glob: &'static str,
    pub except: &'static [&'static str],
}

/// The one list of protected files. The guard's patterns and Claude's `permissions.deny` rules
/// are both generated from it, so they cannot name different things.
pub const FAMILIES: &[Family] = &[
    Family {
        glob: ".env",
        except: &[],
    },
    Family {
        glob: ".env.*",
        except: &[".env.example", ".env.sample", ".env.template", ".env.dist"],
    },
    Family {
        glob: "*.pem",
        except: &[],
    },
    Family {
        glob: "*.key",
        except: &[],
    },
    Family {
        glob: "id_rsa*",
        except: &["*.pub"],
    },
    Family {
        glob: "id_ed25519*",
        except: &["*.pub"],
    },
];

/// Directories under the home folder whose whole contents are protected.
pub const PROTECTED_DIRS: &[&str] = &["~/.config/codesense-mcp"];

/// A basename glob as an anchored regular expression.
fn glob_regex(glob: &str) -> String {
    let mut re = String::from("^");
    for c in glob.chars() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            c if c.is_ascii_alphanumeric() || c == '_' => re.push(c),
            c => {
                re.push('\\');
                re.push(c);
            }
        }
    }
    re.push('$');
    re
}

/// Claude's `permissions.deny` rules for the protected families. The hook sees one path or
/// command; these also keep Grep and Glob from reaching a credential file inside a directory
/// they search.
pub fn deny_rules() -> &'static [String] {
    static RULES: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| {
        FAMILIES
            .iter()
            .map(|f| format!("Read(**/{})", f.glob))
            .chain(PROTECTED_DIRS.iter().map(|d| format!("Read({d}/**)")))
            .collect()
    });
    &RULES
}

/// The guard script as installed: its template with the families and directories written in.
pub fn guard_source() -> String {
    let mut defs = String::from("my @FAMILIES = (\n");
    for f in FAMILIES {
        let except: Vec<String> = f
            .except
            .iter()
            .map(|e| format!("qr/{}/", glob_regex(e)))
            .collect();
        defs.push_str(&format!(
            "  [qr/{}/, [{}]],\n",
            glob_regex(f.glob),
            except.join(", ")
        ));
    }
    defs.push_str(");\nmy @DIRS = (");
    let dirs: Vec<String> = PROTECTED_DIRS
        .iter()
        .map(|d| format!("qr{{{}}}", d.trim_start_matches("~/").replace('.', "\\.")))
        .collect();
    defs.push_str(&dirs.join(", "));
    defs.push_str(");");
    GUARD_TEMPLATE
        .lines()
        .map(|line| {
            if line.starts_with("# @@PROTECTED@@") {
                defs.clone()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// The installed hook as a hook command names it: under the host root its tab exports.
pub fn installed_script() -> String {
    format!("${}/{NAME}", crate::activity::HOST_ENV)
}

/// The installed guard as a hook command names it.
pub fn installed_guard() -> String {
    format!("${}/{GUARD_NAME}", crate::activity::HOST_ENV)
}

/// The command the guard hook runs. A missing script allows the call: the guard never blocks
/// work because it is not installed.
pub fn guard_command() -> String {
    let script = format!("\"{}\"", installed_guard());
    format!("[ -x {script} ] && {script} || true")
}

/// The command a Claude status hook runs. A missing script (a tab outside any host, or a
/// host that could not write it) would fail every hook event, so it is guarded like the
/// inbox hooks; `[` reads no stdin, so the hook's JSON still reaches the script.
pub fn status_command() -> String {
    let script = format!("\"{}\"", installed_script());
    format!("[ -x {script} ] && {script} || true")
}

/// Write the hook into the host activity `root` with mode [`MODE`], unless it is already
/// there. The write is atomic: a tmp file renamed over whatever is at the path (a symlink
/// included, never followed), so a hook firing meanwhile runs one whole script.
pub fn install(root: &Path) -> io::Result<()> {
    let status = install_one(root, NAME, SOURCE);
    let guard = install_one(root, GUARD_NAME, &guard_source());
    status.and(guard)
}

fn install_one(root: &Path, name: &str, source: &str) -> io::Result<()> {
    let dest = root.join(name);
    if is_installed(&dest, source) {
        return Ok(());
    }
    atomic_write_mode(&dest, source.as_bytes(), MODE)
}

/// Whether `dest` is a regular, owner-executable file holding `source`.
fn is_installed(dest: &Path, source: &str) -> bool {
    let Ok(meta) = fs::symlink_metadata(dest) else {
        return false;
    };
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o100 != 0
    };
    #[cfg(not(unix))]
    let executable = true;
    meta.is_file() && executable && fs::read(dest).is_ok_and(|bytes| bytes == source.as_bytes())
}

fn atomic_write_mode(dest: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let file_name = dest.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "notify-hook dest has no file name",
        )
    })?;
    let tmp = tmp_path(parent, file_name);
    // Clean a stale tmp from a previous crashed write.
    let _ = fs::remove_file(&tmp);
    {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
    }
    if let Err(e) = fs::rename(&tmp, dest) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

fn tmp_path(parent: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let mut buf = std::ffi::OsString::from(".");
    buf.push(name);
    buf.push(".tmp.notify-hook");
    parent.join(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn source_has_bash_shebang_and_tab_session_check() {
        assert!(SOURCE.starts_with("#!/bin/bash\n"));
        assert!(SOURCE.contains("CODESENSE_TAB_SESSION_ID"));
    }

    #[test]
    fn the_status_command_runs_the_hosts_installed_script_guarded() {
        assert_eq!(
            status_command(),
            "[ -x \"$CODESENSE_ACTIVITY_HOST/codesense-notify.sh\" ] && \"$CODESENSE_ACTIVITY_HOST/codesense-notify.sh\" || true"
        );
    }

    #[test]
    fn install_writes_an_executable_copy_once() {
        let tmp = tempdir().unwrap();
        install(tmp.path()).unwrap();
        let dest = tmp.path().join(NAME);
        assert_eq!(fs::read_to_string(&dest).unwrap(), SOURCE);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            assert_eq!(
                fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
                0o755
            );
            let first = fs::metadata(&dest).unwrap().ino();
            install(tmp.path()).unwrap();
            assert_eq!(fs::metadata(&dest).unwrap().ino(), first, "left as it was");
        }
    }

    #[cfg(unix)]
    #[test]
    fn install_repairs_drifted_bytes_or_a_missing_exec_bit() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempdir().unwrap();
        let dest = tmp.path().join(NAME);
        for (bytes, mode) in [
            (&b"#!/bin/sh\necho stale\n"[..], 0o755),
            (SOURCE.as_bytes(), 0o644),
        ] {
            fs::write(&dest, bytes).unwrap();
            fs::set_permissions(&dest, fs::Permissions::from_mode(mode)).unwrap();
            install(tmp.path()).unwrap();
            assert_eq!(fs::read_to_string(&dest).unwrap(), SOURCE);
            assert_eq!(
                fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }

    /// A symlink at the path is replaced, never followed, even one whose target holds the
    /// script.
    #[cfg(unix)]
    #[test]
    fn install_replaces_a_symlink_without_following_it() {
        let tmp = tempdir().unwrap();
        let dest = tmp.path().join(NAME);
        for target in ["nowhere.sh", "other.sh"] {
            let target = tmp.path().join(target);
            if target.ends_with("other.sh") {
                fs::write(&target, SOURCE).unwrap();
            }
            let _ = fs::remove_file(&dest);
            std::os::unix::fs::symlink(&target, &dest).unwrap();
            install(tmp.path()).unwrap();
            assert!(fs::symlink_metadata(&dest).unwrap().file_type().is_file());
            assert_eq!(fs::read_to_string(&dest).unwrap(), SOURCE);
        }
        assert!(!tmp.path().join("nowhere.sh").exists(), "never followed");
    }

    #[test]
    fn the_guard_command_is_guarded_like_the_status_command() {
        assert_eq!(
            guard_command(),
            "[ -x \"$CODESENSE_ACTIVITY_HOST/codesense-secret-guard.sh\" ] && \"$CODESENSE_ACTIVITY_HOST/codesense-secret-guard.sh\" || true"
        );
    }

    #[test]
    fn install_writes_the_guard_beside_the_status_hook() {
        let tmp = tempdir().unwrap();
        install(tmp.path()).unwrap();
        assert_eq!(
            fs::read_to_string(tmp.path().join(GUARD_NAME)).unwrap(),
            guard_source()
        );
        assert_eq!(fs::read_to_string(tmp.path().join(NAME)).unwrap(), SOURCE);
    }

    /// The guard's verdict on one hook payload: the deny reason, or `None` when it allows.
    #[cfg(unix)]
    fn verdict(payload: serde_json::Value) -> Option<String> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let tmp = tempdir().unwrap();
        install(tmp.path()).unwrap();
        let mut child = Command::new("/bin/bash")
            .arg(tmp.path().join(GUARD_NAME))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "the guard never fails the call itself"
        );
        if out.stdout.is_empty() {
            return None;
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        Some(
            v["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .to_string(),
        )
    }

    #[cfg(unix)]
    fn call(tool: &str, input: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": tool, "tool_input": input})
    }

    #[cfg(unix)]
    #[test]
    fn the_guard_denies_credential_files_for_every_reading_tool() {
        let denied = [
            call("Read", serde_json::json!({"file_path": "/repo/.env"})),
            call("Read", serde_json::json!({"file_path": "/repo/.env.local"})),
            call(
                "Read",
                serde_json::json!({"file_path": "/repo/certs/server.pem"}),
            ),
            call("Read", serde_json::json!({"file_path": "/repo/tls.key"})),
            call(
                "Read",
                serde_json::json!({"file_path": "/Users/a/.ssh/id_rsa"}),
            ),
            call(
                "Read",
                serde_json::json!({"file_path": "/Users/a/.ssh/id_ed25519"}),
            ),
            call(
                "Read",
                serde_json::json!({"file_path": "/Users/a/.config/codesense-mcp/token.json"}),
            ),
            call(
                "Grep",
                serde_json::json!({"pattern": "KEY", "path": "/repo/.env"}),
            ),
            call(
                "Grep",
                serde_json::json!({"pattern": "KEY", "glob": "**/*.pem"}),
            ),
            call("Glob", serde_json::json!({"pattern": "**/.env*"})),
            call(
                "Glob",
                serde_json::json!({"pattern": "*", "path": "/Users/a/.config/codesense-mcp"}),
            ),
            call("Bash", serde_json::json!({"command": "cat .env"})),
            call(
                "Bash",
                serde_json::json!({"command": "grep -r token ~/.config/codesense-mcp/"}),
            ),
            call(
                "Bash",
                serde_json::json!({"command": "ls && head -3 \"keys/server.key\""}),
            ),
            call(
                "Bash",
                serde_json::json!({"command": "set -a; source .env.production; set +a"}),
            ),
            // Reading it, whatever else the command does with it.
            call("Bash", serde_json::json!({"command": "cp .env /tmp/x"})),
            call(
                "Bash",
                serde_json::json!({"command": "mv server.pem /tmp/x"}),
            ),
            call("Bash", serde_json::json!({"command": "cat < .env"})),
            call(
                "Bash",
                serde_json::json!({"command": "head -n1 .env | wc -c"}),
            ),
            call(
                "Bash",
                serde_json::json!({"command": "FOO=1 sudo cat .env"}),
            ),
            call(
                "Bash",
                serde_json::json!({"command": "docker run --env-file=.env img"}),
            ),
            call(
                "Bash",
                serde_json::json!({"command": "bash -c \"cat .env\""}),
            ),
        ];
        for payload in denied {
            let reason = verdict(payload.clone()).unwrap_or_else(|| panic!("allowed: {payload}"));
            assert!(reason.contains("credential files"), "{reason}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_guard_allows_ordinary_work_and_the_example_files() {
        let allowed = [
            call(
                "Read",
                serde_json::json!({"file_path": "/repo/.env.example"}),
            ),
            call(
                "Read",
                serde_json::json!({"file_path": "/repo/.env.sample"}),
            ),
            call(
                "Read",
                serde_json::json!({"file_path": "/repo/src/main.rs"}),
            ),
            call(
                "Read",
                serde_json::json!({"file_path": "/Users/a/.ssh/id_rsa.pub"}),
            ),
            call(
                "Read",
                serde_json::json!({"file_path": "/repo/environment.md"}),
            ),
            call(
                "Grep",
                serde_json::json!({"pattern": "\\.env", "path": "/repo/src"}),
            ),
            call("Glob", serde_json::json!({"pattern": "src/**/*.rs"})),
            call(
                "Bash",
                serde_json::json!({"command": "cargo test && cat .env.example"}),
            ),
            call("Bash", serde_json::json!({"command": "ls -la"})),
            // Setup and cleanup only write, create or remove the protected file.
            call(
                "Bash",
                serde_json::json!({"command": "cp .env.example .env"}),
            ),
            call("Bash", serde_json::json!({"command": "rm .env"})),
            call(
                "Bash",
                serde_json::json!({"command": "rm -f .env.local server.pem && touch .env"}),
            ),
            call("Bash", serde_json::json!({"command": "echo A=1 > .env"})),
            call(
                "Bash",
                serde_json::json!({"command": "echo A=1 >> .env.local"}),
            ),
            call(
                "Bash",
                serde_json::json!({"command": "mv .env.example .env"}),
            ),
            call(
                "Write",
                serde_json::json!({"file_path": "/repo/.env", "content": "A=1"}),
            ),
            serde_json::json!({"tool_name": "Read"}),
        ];
        for payload in allowed {
            assert_eq!(verdict(payload.clone()), None, "denied: {payload}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_guard_allows_what_it_cannot_parse() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let tmp = tempdir().unwrap();
        install(tmp.path()).unwrap();
        let mut child = Command::new("/bin/bash")
            .arg(tmp.path().join(GUARD_NAME))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"not json").unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success() && out.stdout.is_empty());
    }

    /// Every protected family has both a hook pattern and a native deny rule, and the hook
    /// denies a name in the family while letting its exceptions through.
    #[cfg(unix)]
    #[test]
    fn every_family_has_a_hook_pattern_and_a_deny_rule() {
        let script = guard_source();
        for family in FAMILIES {
            assert!(
                script.contains(&format!("qr/{}/", glob_regex(family.glob))),
                "{} has no hook pattern",
                family.glob
            );
            let rule = format!("Read(**/{})", family.glob);
            assert!(deny_rules().contains(&rule), "{rule} is not a deny rule");
            let name = family.glob.replace('*', "backup");
            let path = format!("/repo/keys/{name}");
            let payload = call("Read", serde_json::json!({"file_path": path}));
            assert!(
                verdict(payload).is_some(),
                "{path} is not denied by the hook"
            );
            for except in family.except {
                let path = format!("/repo/keys/{}", except.replace('*', "x"));
                let payload = call("Read", serde_json::json!({"file_path": path}));
                assert_eq!(verdict(payload), None, "{path} is an exception");
            }
        }
        for dir in PROTECTED_DIRS {
            let rule = format!("Read({dir}/**)");
            assert!(deny_rules().contains(&rule), "{rule} is not a deny rule");
        }
        assert_eq!(
            deny_rules().len(),
            FAMILIES.len() + PROTECTED_DIRS.len(),
            "no rule outside the list"
        );
    }

    /// A variant name inside a directory a search walks is covered by the family rule.
    #[test]
    fn the_deny_rules_are_the_families_not_a_list_of_names() {
        for rule in [
            "Read(**/.env.*)",
            "Read(**/id_rsa*)",
            "Read(**/id_ed25519*)",
            "Read(~/.config/codesense-mcp/**)",
        ] {
            assert!(deny_rules().iter().any(|r| r == rule), "{rule}");
        }
    }
}
