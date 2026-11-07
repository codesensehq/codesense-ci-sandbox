// SPDX-License-Identifier: MIT
//! Test fixtures shared by the attribution tests.

use super::managed::Managed;
use std::path::{Path, PathBuf};

/// Write an executable shell script `body` at `path`.
pub(crate) fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A native entry point preserves argv[0] when handing it to a shell fixture.
/// A shebang alone replaces $0 with the script path before its body runs.
pub(crate) fn argv0_binary(path: &Path) {
    argv0_binary_with_version(path, "1.7.5", "");
}

/// Native fixture with a chosen raw version, and optional CLI behavior after logging.
pub(crate) fn argv0_binary_with_version(path: &Path, version: &str, extra: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let source = path.with_extension("c");
    let body = format!(
        r#"if [ "$(basename "$0")" != git-ai ]; then
if [ "$1" = --version ]; then echo 'git version 2.53.0'; exit 0; else exit 42; fi
fi
if [ "$1" = --version ]; then echo '{version}'; else
 echo "$*" >> "$HOME/argv0-calls"
 {extra}
fi"#
    );
    let source_text = format!(
        r#"
#include <unistd.h>
int main(int argc, char **argv) {{
    char *shell[argc + 4];
    shell[0] = "sh";
    shell[1] = "-c";
    shell[2] = {};
    for (int i = 0; i < argc; ++i) shell[i + 3] = argv[i];
    shell[argc + 3] = 0;
    execv("/bin/sh", shell);
    return 1;
}}
"#,
        serde_json::to_string(&body).unwrap()
    );
    std::fs::write(&source, source_text).unwrap();
    let output = std::process::Command::new("cc")
        .arg(&source)
        .arg("-o")
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

/// A `git-ai` at `path` that appends each call and its `HOME` to `log`, then runs `extra`.
pub(crate) fn logging_binary(path: &Path, log: &Path, extra: &str) {
    script(
        path,
        &format!("echo \"$* HOME=$HOME\" >> \"{}\"\n{extra}", log.display()),
    );
}

/// The calls a [`logging_binary`] logged.
pub(crate) fn logged_calls(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// A managed home under a temp dir, with a short socket dir of its own.
pub(crate) struct Sandbox {
    pub dir: tempfile::TempDir,
    pub managed: Managed,
}

impl Sandbox {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let sockets = tempfile::Builder::new()
            .prefix("csg")
            .tempdir_in("/tmp")
            .unwrap()
            .keep();
        let managed = Managed::at(
            dir.path().join("git-ai"),
            sockets,
            super::managed::UserGit {
                home: PathBuf::from("/real/home"),
                xdg_config_home: None,
                config_global: None,
                git: PathBuf::from("git"),
            },
        );
        Self { dir, managed }
    }

    pub(crate) fn log(&self) -> PathBuf {
        self.dir.path().join("calls.log")
    }

    /// A [`logging_binary`] in the sandbox.
    pub(crate) fn binary(&self, extra: &str) -> PathBuf {
        let path = self.dir.path().join("bin/git-ai");
        logging_binary(&path, &self.log(), extra);
        path
    }

    pub(crate) fn calls(&self) -> Vec<String> {
        logged_calls(&self.log())
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.managed.socket_dir());
    }
}

/// Removes the real socket dir a CodeSense home under test would have used.
pub(crate) struct SocketDirGuard(pub PathBuf);

impl SocketDirGuard {
    pub(crate) fn for_home(codesense_home: &Path) -> Self {
        Self(
            Managed::for_codesense_home(codesense_home)
                .socket_dir()
                .to_owned(),
        )
    }
}

impl Drop for SocketDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A managed worktree, a prepared saved-Agent launch into it, and a fake git-ai under the
/// CodeSense home: everything a launch entry path needs, for real.
pub(crate) struct Launch {
    pub tmp: tempfile::TempDir,
    pub cwd: PathBuf,
    pub launch: crate::agents::launch::AgentLaunch,
    _sockets: SocketDirGuard,
    _git: super::managed::pinned::Guard,
}

pub(crate) const REMOTE: &str = "https://github.com/acme/api.git";

impl Launch {
    /// A launch of `runtime`, with attribution switched `on` for the repo and a git-ai 1.7.5
    /// that starts its daemon.
    pub(crate) fn new(runtime: codesense_pty_core::agent_cli::AgentCli) -> Self {
        use crate::agents::resolve::tests::{alice, draft, save, team_content};
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        team_content(&home, &alice());
        let mut definition = draft("builder", runtime);
        if crate::agents::adapter::desktop(runtime)
            .model_endpoint
            .is_some()
        {
            definition.set_startup_settings(Some(std::collections::BTreeMap::from([(
                "model".into(),
                serde_json::json!({"provider": "custom", "base_url": "http://127.0.0.1:11434/v1", "default": "qwen2.5:3b-instruct"}),
            )])));
        }
        let key = save(&home, &alice(), definition);
        let launch = crate::agents::launch::prepare(
            &home,
            &alice(),
            &key,
            crate::agents::launch::PrepareOptions {
                target_team: Some((
                    crate::agents::resolve::tests::ORG.to_string(),
                    crate::agents::resolve::tests::TEAM.to_string(),
                )),
                mode_override: Some(crate::agents::adapter::LaunchMode::Interactive),
                ..Default::default()
            },
        )
        .unwrap();
        let cwd = home.join("worktree");
        let repo = git2::Repository::init(&cwd).unwrap();
        repo.remote("origin", REMOTE).unwrap();
        // A worktree whose `.mcp.json` carries the bridge, as a launch needs.
        std::fs::write(
            cwd.join(".mcp.json"),
            br#"{"mcpServers": {"codesense": {"command": "bridge"}}}"#,
        )
        .unwrap();
        let this = Self {
            _git: super::managed::pinned::pin(super::managed::UserGit {
                home: PathBuf::from("/real/home"),
                xdg_config_home: None,
                config_global: None,
                git: PathBuf::from("git"),
            }),
            _sockets: SocketDirGuard::for_home(&home),
            tmp,
            cwd,
            launch,
        };
        this.register_worktree();
        crate::commands::attachments::upsert_attachment_record(
            this.home(),
            1,
            crate::commands::attachments::existing_checkout_attachment(&this.cwd).unwrap(),
            None,
            false,
        )
        .unwrap();
        this.set_prefs(crate::attribution::prefs::AttributionPrefs {
            mode: Some(crate::attribution::prefs::AttributionMode::On),
            notes: None,
        });
        this.install(&version_script(super::install::PIN.expected_version));
        this
    }

    pub(crate) fn home(&self) -> &Path {
        self.tmp.path()
    }

    pub(crate) fn log(&self) -> PathBuf {
        self.home().join("git-ai-calls.log")
    }

    /// Calls the fake git-ai logged, `--version` probes left out.
    pub(crate) fn calls(&self) -> Vec<String> {
        logged_calls(&self.log())
            .into_iter()
            .filter(|c| !c.starts_with("--version"))
            .collect()
    }

    pub(crate) fn install(&self, body: &str) {
        logging_binary(&self.home().join("bin/git-ai"), &self.log(), body);
    }

    pub(crate) fn uninstall(&self) {
        let _ = std::fs::remove_file(self.home().join("bin/git-ai"));
    }

    fn register_worktree(&self) {
        use crate::agents::resolve::tests::alice;
        let record = crate::install::worktree_index::WorktreeStoryRecord::new(
            &self.cwd,
            "acme",
            crate::domain::SourceRepoSlug::new("acme", "api").unwrap(),
            "SV-1",
            self.home().join("story.md"),
        );
        crate::install::worktree_index::write_record(self.home(), &alice(), &record).unwrap();
    }

    pub(crate) fn set_prefs(&self, prefs: crate::attribution::prefs::AttributionPrefs) {
        use crate::agents::resolve::tests::alice;
        let (key, _) = super::prefs::repo_key_for_checkout(&self.cwd).unwrap();
        super::prefs::update(self.home(), &alice(), &key, |entry| *entry = prefs).unwrap();
    }

    /// Save another checkout `name` of a repo at `remote` through the real attachment
    /// writer (so its attached-at is newer than the fixture's). Returns its path.
    pub(crate) fn attach_checkout(&self, name: &str, remote: &str) -> String {
        let repo_path = self.home().join(name).to_string_lossy().into_owned();
        let repo = git2::Repository::init(&repo_path).unwrap();
        repo.remote("origin", remote).unwrap();
        crate::commands::attachments::upsert_attachment_record(
            self.home(),
            1,
            crate::domain::Attachment {
                repo_path: repo_path.clone(),
                remote_url: Some(remote.into()),
                attached_at: None,
                tracking_prompt_dismissed_at: None,
                credential_ref: None,
                managed: false,
            },
            None,
            false,
        )
        .unwrap();
        repo_path
    }

    /// Reopen the repository store through the checkout's current origin.
    pub(crate) fn prefs_of(&self, repo_path: &str) -> crate::attribution::prefs::AttributionPrefs {
        use crate::agents::resolve::tests::alice;
        let (key, _) = super::prefs::repo_key_for_checkout(Path::new(repo_path)).unwrap();
        super::prefs::read(self.home(), &alice(), &key).unwrap()
    }

    pub(crate) fn prefs(&self) -> crate::attribution::prefs::AttributionPrefs {
        self.prefs_of(&self.cwd.to_string_lossy())
    }

    /// The status the worktree's record holds, read back from disk.
    pub(crate) fn status(&self) -> Option<super::AttributionStatus> {
        use crate::agents::resolve::tests::alice;
        crate::install::worktree_index::read_record(self.home(), &alice(), &self.cwd)
            .unwrap()
            .unwrap()
            .attribution
            .map(|state| state.status)
    }

    /// The managed environment the fake daemon is "running" in: a listener on its control
    /// socket, held until the returned value drops.
    pub(crate) fn daemon_running(&self) -> std::os::unix::net::UnixListener {
        let managed = Managed::for_codesense_home(self.home());
        std::fs::create_dir_all(managed.socket_dir()).unwrap();
        let _ = std::fs::remove_file(managed.control_socket());
        std::os::unix::net::UnixListener::bind(managed.control_socket()).unwrap()
    }
}

/// A git-ai that reports `version` and starts a daemon without error.
pub(crate) fn version_script(version: &str) -> String {
    format!("case \"$1\" in --version) echo '{version}';; esac")
}

/// Whether a launch's argv, per-launch settings and env carry git-ai capture: Claude's
/// checkpoint hooks are in its settings file, Codex's are trusted `-c hooks.*` flags, and
/// both launch with the trace env.
pub(crate) fn assert_wired(
    runtime: codesense_pty_core::agent_cli::AgentCli,
    args: &[String],
    settings: Option<&serde_json::Value>,
    env: &[(String, String)],
) {
    use codesense_pty_core::agent_cli::{AgentCli, ATTRIBUTION_HOOK_MARKER};
    assert!(
        env.iter()
            .any(|(k, v)| k == "GIT_TRACE2_EVENT" && v.starts_with("af_unix:stream:")),
        "{runtime:?} env lacks the trace socket: {env:?}"
    );
    assert!(env
        .iter()
        .any(|(k, v)| k == "GIT_TRACE2_EVENT_NESTING" && v == "0"));
    // The agent itself keeps the user's HOME.
    assert!(!env.iter().any(|(k, _)| k == "HOME"), "{env:?}");
    match runtime {
        AgentCli::Claude => {
            let hooks = &settings.expect("claude settings file")["hooks"];
            for event in ["PreToolUse", "PostToolUse"] {
                let groups = hooks[event].as_array().expect(event);
                let ours: Vec<_> = groups
                    .iter()
                    .filter(|g| g.to_string().contains(ATTRIBUTION_HOOK_MARKER))
                    .collect();
                assert_eq!(ours.len(), 1, "{event}: {groups:?}");
                assert_eq!(ours[0]["matcher"], "*");
                let command = ours[0]["hooks"][0]["command"].as_str().unwrap();
                assert!(
                    command.contains("checkpoint claude --hook-input stdin"),
                    "{command}"
                );
            }
        }
        AgentCli::Codex => {
            for event in super::launch::CODEX_HOOK_EVENTS {
                let flag = args
                    .iter()
                    .find(|a| {
                        a.starts_with(&format!("hooks.{event}="))
                            && a.contains(ATTRIBUTION_HOOK_MARKER)
                    })
                    .unwrap_or_else(|| panic!("no git-ai hooks.{event} in {args:?}"));
                assert!(
                    flag.contains("checkpoint codex --hook-input stdin"),
                    "{flag}"
                );
            }
        }
        AgentCli::OpenCode | AgentCli::Hermes => panic!("unsupported attribution is never wired"),
    }
}

/// Nothing of git-ai capture reached the launch.
pub(crate) fn assert_unwired(
    args: &[String],
    settings: Option<&serde_json::Value>,
    env: &[(String, String)],
) {
    assert!(
        !env.iter().any(|(k, _)| k.starts_with("GIT_TRACE2")),
        "{env:?}"
    );
    let marker = codesense_pty_core::agent_cli::ATTRIBUTION_HOOK_MARKER;
    assert!(!args.iter().any(|a| a.contains(marker)), "{args:?}");
    assert!(!settings.is_some_and(|s| s.to_string().contains(marker)));
}

/// Fixture git as a session with `env` (a launch's environment) runs it: no reflog action but
/// the one `env` sets, so a suite run from inside an agent session sees the same records.
pub(crate) fn git_as(repo: &Path, env: &[(String, String)], args: &[&str]) -> String {
    let output = crate::test_support::strip_attribution_env(&mut std::process::Command::new("git"))
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
        .envs(env.iter().map(|(key, value)| (key, value)))
        .output()
        .unwrap();
    assert!(output.status.success(), "{args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// The launch keeps its session's commits out of git-ai's untraced-commit fixup as `cli`'s
/// (`Some`), or sets no reflog action at all (`None`).
pub(crate) fn assert_reflog_marker(env: &[(String, String)], cli: Option<&str>) {
    let actions: Vec<&str> = env
        .iter()
        .filter(|(key, _)| key == "GIT_REFLOG_ACTION")
        .map(|(_, value)| value.as_str())
        .collect();
    let expected = cli.map(|cli| format!("{} {cli}", super::unsupported::REFLOG_ACTION));
    assert_eq!(actions, Vec::from_iter(expected.as_deref()), "{env:?}");
}
